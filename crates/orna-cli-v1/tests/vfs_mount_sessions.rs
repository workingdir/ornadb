#![cfg(target_os = "linux")]

//! Multi-row `mount --cwd` sessions (ORNA-VFS-001/002/004/005/009/011/017/021/024).
//!
//! - Rows first opened after an accepted save stay readable (W15-02).
//! - A stale save refreshes the row instead of trapping the mount, a refused
//!   scratch can be removed, unmount names what is unresolved, and stopping the
//!   mount retains drafts instead of discarding them (W15-03).
//! - Unmount during start-up and recovery after SIGKILL (W15-04, W15-05).
//! - An invalid CWD row is reported instead of showing HEAD (W08-08), and Blob
//!   descriptor edits are refused instead of acknowledged and dropped (A15-04).
//!
//! No product command can create a mountable format-3 store yet (W15-01), so
//! each repository comes from the shared format-3 fixture builder. Every later
//! step uses product commands (`orna mount`, `orna edit`, `orna unmount`,
//! `orna mount status`) and plain filesystem calls.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeState, RuntimeTableIdentity, TableMutation,
    TableObjectId, TerminalOutcome,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{Raw, Value};
use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;
#[path = "support/editor_mount.rs"]
mod editor_mount;

use editor_mount::{
    MountedProcess, clear_retained_draft_store_filler, fill_retained_draft_store,
    mountinfo_contains, repository_with_music_rows, require_fuse_or_skip, retained_drafts,
    spawn_mount_process, start_mount_process, wait_for_mount, write_and_sync,
};

const EIO: i32 = 5;
const EAGAIN: i32 = 11;
const EINVAL: i32 = 22;
const EPERM: i32 = 1;
const EDQUOT: i32 = 122;

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

/// A repository whose `Music` table has one committed row per `(key, title)`.
async fn music_repository(titles: &[(&str, &str)]) -> (TempDir, Repository, [u8; 16]) {
    let documents = titles
        .iter()
        .map(|&(key, title)| (key, music_row(title)))
        .collect::<Vec<_>>();
    let rows = documents
        .iter()
        .map(|(key, document)| (*key, document.as_str()))
        .collect::<Vec<_>>();
    repository_with_music_rows(&rows).await
}

fn assert_errno<T: std::fmt::Debug>(result: std::io::Result<T>, expected: i32, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.raw_os_error(), Some(expected), "{context}: {error}");
}

/// `renameat2(2)` with explicit flags, through rustix: `std::fs::rename` cannot
/// pass any. The crate forbids `unsafe`, so no libc call is made here.
fn rename_with_flags(
    from: &Path,
    to: &Path,
    flags: rustix::fs::RenameFlags,
) -> std::io::Result<()> {
    rustix::fs::renameat_with(rustix::fs::CWD, from, rustix::fs::CWD, to, flags)
        .map_err(std::io::Error::from)
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn orna(repository: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(repository)
        .args(args)
        .output()
        .expect("run orna")
}

fn signal(process: u32, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(process.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {process} failed");
}

fn mount_status(repository: &Path) -> String {
    let output = orna(repository, &["mount", "status"]);
    assert_success(&output, "orna mount status");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn draft_guard(mountpoint: &Path) -> u8 {
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(mountpoint, "user.orna.unresolved-drafts", &mut value)
        .expect("read the mount draft guard");
    assert_eq!(length, 1, "the draft guard is one byte");
    value[0]
}

/// Waits for the draft guard: FUSE release can run after close(2) returns.
fn await_draft_guard(mountpoint: &Path, expected: u8, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = draft_guard(mountpoint);
        if value == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: guard is {value}, expected {expected} after 5s"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Every text field of the row the runtime stores for `key` now.
async fn stored_fields(repository: &Repository, relation_id: [u8; 16], key: &str) -> Vec<Raw> {
    let (identity, digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, digest).await.unwrap();
    let table = RuntimeTableIdentity::new("Music", TableObjectId::new(relation_id)).unwrap();
    let snapshot = state
        .begin_admitted_table_activation(std::slice::from_ref(&table))
        .await
        .unwrap();
    let row = snapshot
        .query_exact("Music", key.as_bytes())
        .unwrap()
        .expect("the runtime stores the row")
        .to_vec();
    match Value::decode(&row).expect("decode the stored row").raw() {
        Raw::Array(fields) => fields.clone(),
        other => panic!("stored row is not a field array: {other:?}"),
    }
}

async fn assert_stored_title(
    repository: &Repository,
    relation_id: [u8; 16],
    key: &str,
    title: &str,
) {
    let fields = stored_fields(repository, relation_id, key).await;
    assert!(
        fields.contains(&Raw::Text(title.to_owned())),
        "stored {key} row has title {title:?}: {fields:?}"
    );
}

/// Commits `fields` as the CWD row for `key` through a request activation that
/// does not validate rows, as a damaged or out-of-schema runtime row would be.
async fn commit_unvalidated_runtime_row(
    repository: &Repository,
    relation_id: [u8; 16],
    key: &str,
    fields: Raw,
) {
    let (identity, digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let request_identity = RequestIdentity {
        session_id: [0x7b; 16],
        request_id: [0x7c; 16],
    };
    let fingerprint = [0x7d; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();
    let value = Value::new(fields).unwrap().encode().unwrap();
    let mutation = TableMutation::new([0x7e; 16], "Music", key.as_bytes().to_vec(), Some(value))
        .unwrap()
        .with_table_object_id(TableObjectId::new(relation_id));
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [0x7f; 32],
            TerminalOutcome::new(vec![0x80]).unwrap(),
            &NoFault,
            orna_runtime_v1::ActivationEffectReport::declare(orna_runtime_v1::DeclaredEffectSet::None),
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
}

/// One multi-row repository mounted with `mount --cwd`. Fields drop in order,
/// so the mount is released before its directories are removed.
struct Session {
    mounted: MountedProcess,
    repository: Repository,
    relation_id: [u8; 16],
    repository_directory: TempDir,
    mount_parent: TempDir,
    scripts: TempDir,
}

impl Session {
    async fn start(titles: &[(&str, &str)]) -> Self {
        let (repository_directory, repository, relation_id) = music_repository(titles).await;
        Self::mount(repository_directory, repository, relation_id, titles[0].0)
    }

    /// Mounts an existing repository and waits until `Music/<key>` is served.
    fn mount(
        repository_directory: TempDir,
        repository: Repository,
        relation_id: [u8; 16],
        key: &str,
    ) -> Self {
        let mount_parent = TempDir::new().unwrap();
        let mountpoint = mount_parent.path().join("library");
        let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
        mounted.await_path(&mountpoint.join("Music").join(key).join("data.orna"));
        Self {
            mounted,
            repository,
            relation_id,
            repository_directory,
            mount_parent,
            scripts: TempDir::new().unwrap(),
        }
    }

    fn mountpoint(&self) -> PathBuf {
        self.mount_parent.path().join("library")
    }

    fn row(&self, key: &str) -> PathBuf {
        self.mountpoint().join("Music").join(key)
    }

    fn document(&self, key: &str) -> PathBuf {
        self.row(key).join("data.orna")
    }

    fn read(&self, key: &str) -> String {
        fs::read_to_string(self.document(key))
            .unwrap_or_else(|error| panic!("read Music/{key}/data.orna: {error}"))
    }

    fn unmount(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_orna"))
            .arg("--db")
            .arg(self.repository_directory.path())
            .arg("unmount")
            .arg(self.mountpoint())
            .output()
            .expect("run orna unmount")
    }

    /// Replaces `Music/<key>` with `document` through `orna edit` on the
    /// repository path: a writer outside this mount.
    fn edit_elsewhere(&self, key: &str, document: &str) {
        let editor = self.scripts.path().join(format!("edit-{key}"));
        fs::write(
            &editor,
            format!("#!/bin/sh\ncat > \"$1\" <<'ORNA_EDIT'\n{document}ORNA_EDIT\n"),
        )
        .unwrap();
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_orna"))
            .arg("--db")
            .arg(self.repository_directory.path())
            .args(["edit", &format!("Music/{key}/data.orna")])
            .env("EDITOR", &editor)
            .output()
            .expect("run orna edit");
        assert_success(&output, "orna edit outside the mount");
    }

    /// Starts a new `mount --cwd` at the same mountpoint and waits until it
    /// serves `Music/<key>`. The previous guard must already be released.
    fn remount(&mut self, key: &str) {
        self.mounted = start_mount_process(self.repository_directory.path(), &self.mountpoint());
        let document = self.document(key);
        self.mounted.await_path(&document);
    }

    /// Unmounts with `orna unmount` and waits for a clean mount exit.
    fn finish(&mut self) {
        let output = self.unmount();
        assert_success(&output, "orna unmount");
        let status = self
            .mounted
            .wait_for_exit(Duration::from_secs(15))
            .expect("the mount process exits after unmount");
        assert!(
            status.success(),
            "the mount process failed after unmount ({status}): {}",
            self.mounted.stderr_text()
        );
        assert!(!mountinfo_contains(&self.mountpoint()));
    }
}

/// W15-02, ORNA-VFS-001/017: every row stays addressable after accepted saves
/// in the same table, including rows first looked up after those saves and a
/// temp-file save in such a row.
#[tokio::test]
async fn rows_first_opened_after_a_save_stay_readable_and_editable() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[
        ("london", "London"),
        ("paris", "Paris"),
        ("tokyo", "Tokyo"),
    ])
    .await;
    assert!(session.read("london").contains("London"));
    write_and_sync(&session.document("london"), &music_row("London saved"))
        .expect("the first save is accepted");

    let paris_entries = fs::read_dir(session.row("paris"))
        .expect("a row first listed after a save is readable")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(paris_entries, ["data.orna"]);
    assert!(session.read("paris").contains("Paris"));
    write_and_sync(&session.document("paris"), &music_row("Paris saved"))
        .expect("a row first opened after a save can be saved");

    let scratch = session.row("tokyo").join(".data.orna.save");
    fs::write(&scratch, music_row("Tokyo renamed")).expect("write a temp file in a later row");
    fs::rename(&scratch, session.document("tokyo"))
        .expect("a temp-file save in a row first opened after two saves is accepted");
    assert!(session.read("tokyo").contains("Tokyo renamed"));
    assert!(session.read("london").contains("London saved"));

    session.finish();
    for (key, title) in [
        ("london", "London saved"),
        ("paris", "Paris saved"),
        ("tokyo", "Tokyo renamed"),
    ] {
        assert_stored_title(&session.repository, session.relation_id, key, title).await;
    }
}

/// Starts a session whose london row another writer changed, then saves it stale
/// through the mount: the candidate is retained as an unresolved draft.
async fn stale_save_session() -> Session {
    let session = Session::start(&[("london", "London"), ("paris", "Paris")]).await;
    assert!(session.read("london").contains("London"));
    session.edit_elsewhere("london", &music_row("Edited elsewhere"));

    assert_errno(
        write_and_sync(&session.document("london"), &music_row("Stale mount save")),
        EAGAIN,
        "a save against a row another writer changed is stale",
    );
    await_draft_guard(&session.mountpoint(), b'1', "the stale draft is unresolved");
    assert!(
        retained_drafts(&session.repository)
            .iter()
            .any(|draft| draft.contains("Stale mount save")),
        "the stale candidate is retained in recovery storage"
    );
    session
}

/// The draft id `mount status` lists for the rejected london draft.
fn listed_draft_id(status: &str) -> String {
    let prefix = "  unresolved draft Music/london/data.orna code sys.vfs.stale_edit draft ";
    let listing = status
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("the rejected draft lists its cause and draft id: {status}"));
    let id = listing
        .strip_prefix(prefix)
        .and_then(|rest| rest.split_once(": "))
        .map(|(id, _)| id)
        .unwrap_or_default();
    assert!(!id.is_empty() && id != "-", "a retained draft id is listed: {listing}");
    id.to_owned()
}

/// ORNA-VFS-011/024, profiles/vfs-1.md: `mount status` lists the writable `--cwd`
/// mount with its mountpoint, view and writable mode.
#[tokio::test]
async fn stale_save_lists_the_mount_row_as_writable() {
    if !require_fuse_or_skip() {
        return;
    }
    let session = stale_save_session().await;
    let status = mount_status(session.repository_directory.path());
    let mountpoint = fs::canonicalize(session.mountpoint()).expect("canonical mountpoint");
    let mount_row = status
        .lines()
        .find(|line| line.starts_with(&format!("{} snapshot ", mountpoint.display())))
        .unwrap_or_else(|| panic!("the writable mount row names its mountpoint: {status}"));
    assert!(
        mount_row.contains(" view ") && mount_row.ends_with(" writable"),
        "the mount row names its view and writable mode: {mount_row}"
    );
}

/// ORNA-VFS-011/024: `mount status` names the rejected draft with its exact Orna
/// cause and retained draft id.
#[tokio::test]
async fn stale_save_status_names_the_draft_code_and_id() {
    if !require_fuse_or_skip() {
        return;
    }
    let session = stale_save_session().await;
    let status = mount_status(session.repository_directory.path());
    assert!(!listed_draft_id(&status).is_empty());
}

/// ORNA-VFS-011/024: the `user.orna.unresolved-draft-fields` xattr carries the
/// draft's code and id. The buffer is sized by a probe, not a fixed length.
#[tokio::test]
async fn stale_save_fields_xattr_carries_code_and_draft_id() {
    if !require_fuse_or_skip() {
        return;
    }
    let session = stale_save_session().await;
    let mountpoint = fs::canonicalize(session.mountpoint()).expect("canonical mountpoint");
    let name = "user.orna.unresolved-draft-fields";
    let length = rustix::fs::getxattr(&mountpoint, name, &mut [])
        .expect("probe the draft fields xattr size");
    let mut fields = vec![0_u8; length];
    let length = rustix::fs::getxattr(&mountpoint, name, &mut fields)
        .expect("read the draft fields xattr");
    let fields = String::from_utf8_lossy(&fields[..length]).into_owned();
    let line = fields
        .lines()
        .find(|line| line.starts_with("Music/london/data.orna\tsys.vfs.stale_edit\t"))
        .unwrap_or_else(|| panic!("the fields xattr carries the code: {fields}"));
    let draft_id = line.split('\t').nth(2).unwrap_or_default();
    assert!(!draft_id.is_empty() && draft_id != "-", "a draft id is carried: {line}");
}

/// ORNA-VFS-011/024: `mount status --json` carries the writable mount, the draft
/// code and id, the xattr source that listed them, and the omitted count.
#[tokio::test]
async fn stale_save_status_json_flags_the_fields_source() {
    if !require_fuse_or_skip() {
        return;
    }
    let session = stale_save_session().await;
    let draft_id = {
        let json = orna(session.repository_directory.path(), &["mount", "status", "--json"]);
        assert_success(&json, "orna mount status --json");
        let json = String::from_utf8_lossy(&json.stdout).into_owned();
        let after = json
            .split_once("\"path\":\"Music/london/data.orna\",\"code\":\"sys.vfs.stale_edit\",\"draft\":\"")
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_else(|| panic!("the JSON status lists the london draft code: {json}"));
        after.split('"').next().unwrap_or_default().to_owned()
    };
    assert!(!draft_id.is_empty() && draft_id != "-", "a retained draft id is in the JSON");
    let json = orna(session.repository_directory.path(), &["mount", "status", "--json"]);
    assert_success(&json, "orna mount status --json");
    let json = String::from_utf8_lossy(&json.stdout);
    assert!(
        json.contains("\"writable\":true,\"read_only\":false,\"draft_source\":\"fields\",\"drafts_truncated\":false,\"drafts_omitted\":0"),
        "the JSON status flags the fields source and no omitted drafts: {json}"
    );
    assert!(
        json.contains(&format!(
            "{{\"path\":\"Music/london/data.orna\",\"code\":\"sys.vfs.stale_edit\",\"draft\":\"{draft_id}\""
        )),
        "the JSON status carries the draft code and id: {json}"
    );
}

/// W15-03, ORNA-VFS-009/011/024: a save against a row another writer changed
/// is stale; the candidate is retained, the mount then shows the current row,
/// unmount names the unresolved draft, and a save against the current row
/// resolves it.
#[tokio::test]
async fn stale_document_save_refreshes_the_row_instead_of_trapping_unmount() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = stale_save_session().await;

    let refused = session.unmount();
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!refused.status.success(), "unmount must refuse an unresolved draft");
    assert!(
        refusal.contains("Music/london/data.orna") && refusal.contains("sys.vfs.stale_edit"),
        "unmount names the unresolved draft and its cause: {refusal}"
    );
    assert!(mountinfo_contains(&session.mountpoint()));

    assert!(
        session.read("london").contains("Edited elsewhere"),
        "after a stale save the mount shows the current row"
    );
    write_and_sync(&session.document("london"), &music_row("Saved after refresh"))
        .expect("a save against the current row is accepted");
    await_draft_guard(&session.mountpoint(), b'0', "the accepted save resolves the draft");
    assert!(session.read("paris").contains("Paris"));

    session.finish();
    assert_stored_title(
        &session.repository,
        session.relation_id,
        "london",
        "Saved after refresh",
    )
    .await;
    assert_stored_title(&session.repository, session.relation_id, "paris", "Paris").await;
}

/// W15-03, ORNA-VFS-009/011: a temp-file rename made stale by another writer is
/// refused, the scratch can then be removed because its candidate is retained,
/// and the mount shows the current row.
#[tokio::test]
async fn stale_scratch_rename_can_be_removed_and_is_retained() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London"), ("paris", "Paris")]).await;
    let scratch = session.row("paris").join(".data.orna.tmp1");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create a temp file");
    file.write_all(music_row("Stale scratch").as_bytes()).unwrap();
    file.sync_all().expect("fsync persists the scratch");
    drop(file);
    session.edit_elsewhere("paris", &music_row("Paris elsewhere"));

    assert_errno(
        fs::rename(&scratch, session.document("paris")),
        EAGAIN,
        "a temp rename against a row another writer changed is stale",
    );
    assert!(scratch.exists(), "a refused rename keeps the scratch");
    fs::remove_file(&scratch).expect("a refused scratch can be removed");
    assert!(!scratch.exists());
    assert!(
        retained_drafts(&session.repository)
            .iter()
            .any(|draft| draft.contains("Stale scratch")),
        "the removed scratch candidate is retained in recovery storage"
    );
    assert!(
        session.read("paris").contains("Paris elsewhere"),
        "after a stale rename the mount shows the current row"
    );
    await_draft_guard(&session.mountpoint(), b'0', "no draft remains unresolved");

    session.finish();
    assert_stored_title(
        &session.repository,
        session.relation_id,
        "paris",
        "Paris elsewhere",
    )
    .await;
}

/// W15-03, ORNA-VFS-024: SIGTERM with unresolved drafts retains them, reports
/// them, and does not report success; the unsynced scratch is restored by the
/// next mount.
#[tokio::test]
async fn sigterm_retains_unresolved_drafts_instead_of_discarding_them() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London"), ("paris", "Paris")]).await;
    assert_errno(
        write_and_sync(
            &session.document("london"),
            "{ title: \"Rejected before SIGTERM\", artist: 123, }\n",
        ),
        EINVAL,
        "an invalid save is refused",
    );
    let scratch = session.row("paris").join(".data.orna.sigterm");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create a temp file");
    file.write_all(music_row("Scratch kept at SIGTERM").as_bytes())
        .unwrap();
    drop(file);
    await_draft_guard(&session.mountpoint(), b'1', "both drafts are unresolved");

    signal(session.mounted.pid(), "TERM");
    let status = session
        .mounted
        .wait_for_exit(Duration::from_secs(20))
        .expect("the mount stops after SIGTERM");
    let stderr = session.mounted.stderr_text();
    assert!(
        !mountinfo_contains(&session.mountpoint()),
        "SIGTERM unmounts: {stderr}"
    );
    assert!(mount_status(session.repository_directory.path()).contains("no mounts"));
    assert!(
        stderr.contains("sys.vfs.draft_retained")
            && stderr.contains("Music/london/data.orna")
            && stderr.contains("Music/paris/.data.orna.sigterm"),
        "SIGTERM reports every retained draft: {stderr}"
    );
    assert!(
        !status.success(),
        "a mount stopped with unresolved drafts does not report success: {stderr}"
    );
    assert!(
        retained_drafts(&session.repository)
            .iter()
            .any(|draft| draft.contains("Rejected before SIGTERM")),
        "the refused candidate is retained in recovery storage"
    );

    session.remount("paris");
    assert_eq!(
        fs::read_to_string(&scratch).expect("the next mount restores the unsynced scratch"),
        music_row("Scratch kept at SIGTERM")
    );
    assert!(session.read("london").contains("London"));
    fs::remove_file(&scratch).expect("remove the restored scratch");
    session.finish();
}

/// ORNA-VFS-024, profiles/vfs-1 errno table: SIGTERM while recovery storage is
/// full does not unmount over a draft that is still unkept. The mount stays up
/// and names the full store; once storage has room the stop completes and keeps
/// the draft.
#[tokio::test]
async fn full_recovery_store_keeps_mount_up_on_sigterm_until_the_draft_is_kept() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London")]).await;
    fill_retained_draft_store(&session.repository);
    assert_errno(
        write_and_sync(
            &session.document("london"),
            "{ title: \"Blocked stop\", artist: 123, }\n",
        ),
        EDQUOT,
        "a refused save that a full recovery store cannot keep fails with EDQUOT",
    );
    await_draft_guard(&session.mountpoint(), b'1', "the unkept draft holds the guard");

    signal(session.mounted.pid(), "TERM");
    assert!(
        session.mounted.wait_for_exit(Duration::from_secs(2)).is_none(),
        "SIGTERM keeps the mount running while its draft cannot be kept: {}",
        session.mounted.stderr_text(),
    );
    assert!(
        mountinfo_contains(&session.mountpoint()),
        "SIGTERM does not unmount over an unkept draft",
    );
    assert!(
        session.mounted.stderr_text().contains("already holds 256 drafts"),
        "the stop names the full recovery store: {}",
        session.mounted.stderr_text(),
    );

    clear_retained_draft_store_filler(&session.repository);
    let status = session
        .mounted
        .wait_for_exit(Duration::from_secs(20))
        .expect("the stop completes once the draft can be kept");
    let stderr = session.mounted.stderr_text();
    assert!(
        !mountinfo_contains(&session.mountpoint()),
        "the stop unmounts once the draft is kept: {stderr}"
    );
    assert!(
        !status.success(),
        "a stop with an unresolved draft does not report success: {stderr}"
    );
    assert!(
        retained_drafts(&session.repository)
            .iter()
            .any(|draft| draft.contains("Blocked stop")),
        "the refused candidate is retained in recovery storage"
    );
}

/// ORNA-VFS-024, profiles/vfs-1 errno table: while a full recovery store keeps
/// SIGTERM waiting on an unkept draft, a second stop signal ends the wait. The
/// mount is not unmounted over the unkept draft; the stop names the draft that
/// will be lost and exits non-zero.
#[tokio::test]
async fn full_recovery_store_second_stop_signal_ends_the_wait_and_names_lost_drafts() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London")]).await;
    fill_retained_draft_store(&session.repository);
    assert_errno(
        write_and_sync(
            &session.document("london"),
            "{ title: \"Escaped stop\", artist: 123, }\n",
        ),
        EDQUOT,
        "a refused save that a full recovery store cannot keep fails with EDQUOT",
    );
    await_draft_guard(&session.mountpoint(), b'1', "the unkept draft holds the guard");

    signal(session.mounted.pid(), "TERM");
    assert!(
        session.mounted.wait_for_exit(Duration::from_secs(1)).is_none(),
        "the first SIGTERM waits on the unkept draft: {}",
        session.mounted.stderr_text(),
    );
    signal(session.mounted.pid(), "TERM");
    let status = session
        .mounted
        .wait_for_exit(Duration::from_secs(5))
        .expect("a second stop signal ends the wait");
    let stderr = session.mounted.stderr_text();
    assert!(!status.success(), "the escaped stop is not a success: {stderr}");
    assert!(
        stderr.contains("will be lost"),
        "the escaped stop names the drafts that will be lost: {stderr}"
    );
    assert!(
        stderr.contains("london"),
        "the escaped stop lists the unretained draft path: {stderr}"
    );
    clear_retained_draft_store_filler(&session.repository);
}

/// ORNA-VFS-009, profiles/vfs-1 errno table: a temp-file rename whose refused
/// candidate a full recovery store cannot keep fails with EDQUOT, and so does
/// removing that scratch. The scratch stays in place until storage has room.
#[tokio::test]
async fn full_recovery_store_replies_quota_to_refused_rename_and_removal() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London")]).await;
    let scratch = session.row("london").join(".data.orna.quota");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create a temp file");
    file.write_all(b"{ title: \"Quota scratch\", artist: 123, }\n")
        .unwrap();
    file.sync_all().expect("fsync persists the scratch");
    drop(file);
    fill_retained_draft_store(&session.repository);

    assert_errno(
        fs::rename(&scratch, session.document("london")),
        EDQUOT,
        "a refused rename whose candidate a full recovery store cannot keep fails with EDQUOT",
    );
    assert!(scratch.exists(), "a refused rename keeps the scratch");
    assert_errno(
        fs::remove_file(&scratch),
        EDQUOT,
        "removing a refused scratch a full recovery store cannot keep fails with EDQUOT",
    );
    assert!(scratch.exists(), "a scratch that could not be kept is not removed");

    clear_retained_draft_store_filler(&session.repository);
    fs::remove_file(&scratch).expect("the refused scratch is removed once it can be kept");
    assert!(
        retained_drafts(&session.repository)
            .iter()
            .any(|draft| draft.contains("Quota scratch")),
        "the removed scratch candidate is retained in recovery storage",
    );
    await_draft_guard(&session.mountpoint(), b'0', "no draft remains unresolved");
    session.finish();
}

/// W15-04, ORNA-VFS-005/024: `orna unmount` while `orna mount` is still
/// starting leaves no writable FUSE mount without a registered view.
#[tokio::test]
async fn unmount_during_mount_start_up_leaves_no_unregistered_mount() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, repository, _relation_id) =
        music_repository(&[("london", "London"), ("paris", "Paris")]).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    fs::create_dir(&mountpoint).unwrap();
    let registry = repository.runtime_paths().root().join("mounts");
    let mut mounted = start_mount_process(repository_directory.path(), &mountpoint);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let recorded = fs::read_dir(&registry).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("mount-") && name.ends_with(".orna")
            })
        });
        if recorded {
            break;
        }
        if let Some(status) = mounted.wait_for_exit(Duration::ZERO) {
            panic!(
                "mount exited {status} before recording its view: {}",
                mounted.stderr_text()
            );
        }
        assert!(Instant::now() < deadline, "the mount never recorded its view");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !mountinfo_contains(&mountpoint),
        "the view record must exist before the FUSE mount appears"
    );

    let unmount = orna(
        repository_directory.path(),
        &["unmount", mountpoint.to_str().unwrap()],
    );
    assert_success(&unmount, "orna unmount during mount start-up");
    let status = mounted.wait_for_exit(Duration::from_secs(20));
    assert!(
        status.is_some(),
        "a mount whose view was unmounted during start-up must stop: {}",
        mounted.stderr_text()
    );
    assert!(
        !mountinfo_contains(&mountpoint),
        "no FUSE mount remains without a registered view"
    );
    assert!(mount_status(repository_directory.path()).contains("no mounts"));
    assert_eq!(
        fs::read_dir(&mountpoint).unwrap().count(),
        0,
        "nothing is served at the mountpoint"
    );
}

/// W15-05, ORNA-VFS-021: after SIGKILL of the mount, a new `orna mount` and
/// `orna unmount` both recover the dead FUSE mount without a manual
/// `fusermount`; the acknowledged save and the fsynced scratch survive.
#[tokio::test]
async fn unmount_and_remount_recover_after_sigkill() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London"), ("paris", "Paris")]).await;
    write_and_sync(
        &session.document("london"),
        &music_row("Acknowledged before SIGKILL"),
    )
    .expect("the save is acknowledged");
    let scratch = session.row("paris").join(".data.orna.k1");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create a temp file");
    file.write_all(music_row("Scratch before SIGKILL").as_bytes())
        .unwrap();
    file.sync_all().expect("fsync persists the scratch");
    drop(file);

    signal(session.mounted.pid(), "KILL");
    session
        .mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("SIGKILL stops the mount");
    assert!(
        mountinfo_contains(&session.mountpoint()),
        "a killed FUSE daemon leaves its dead mount behind"
    );
    session.mounted.release_mountpoint();

    session.remount("london");
    assert!(session.read("london").contains("Acknowledged before SIGKILL"));
    assert_eq!(
        fs::read_to_string(&scratch).expect("the fsynced scratch survives SIGKILL"),
        music_row("Scratch before SIGKILL")
    );
    fs::remove_file(&scratch).expect("remove the restored scratch");
    session.finish();

    // The finished process no longer owns the mountpoint: `finish` removed its
    // mount, and this guard must not unmount the successor's mount on assignment.
    session.mounted.release_mountpoint();
    session.mounted = wait_for_mount(start_mount_process(
        session.repository_directory.path(),
        &session.mountpoint(),
    ));
    let document = session.document("london");
    session.mounted.await_path(&document);
    signal(session.mounted.pid(), "KILL");
    session
        .mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("SIGKILL stops the mount");
    assert!(mountinfo_contains(&session.mountpoint()));
    session.mounted.release_mountpoint();
    let unmount = session.unmount();
    assert_success(&unmount, "orna unmount after SIGKILL");
    assert!(
        !mountinfo_contains(&session.mountpoint()),
        "orna unmount releases the dead FUSE mount"
    );
    assert!(mount_status(session.repository_directory.path()).contains("no mounts"));
    assert_stored_title(
        &session.repository,
        session.relation_id,
        "london",
        "Acknowledged before SIGKILL",
    )
    .await;
}

/// W08-08, ORNA-VFS-002/005: a CWD row that fails validation is reported as
/// EIO; the mount never shows the committed HEAD row in its place, and other
/// rows stay readable.
#[tokio::test]
async fn invalid_cwd_row_is_reported_instead_of_showing_head() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, repository, relation_id) =
        music_repository(&[("london", "London at HEAD"), ("paris", "Paris")]).await;
    commit_unvalidated_runtime_row(
        &repository,
        relation_id,
        "london",
        Raw::Array(vec![Raw::Int(5.into())]),
    )
    .await;
    let mut session = Session::mount(repository_directory, repository, relation_id, "paris");
    assert_errno(
        fs::read_to_string(session.document("london")),
        EIO,
        "an invalid CWD row must not be projected as HEAD",
    );
    assert!(session.read("paris").contains("Paris"));
    session.finish();
}

const MEDIA_SOURCE: &str = "pub table Music(id: Str) { title: Str, content: Blob, }\n";

/// A `Music` table whose rows each hold a title and one captured MP3 Blob.
async fn media_session(rows: &[(&str, &str, &[u8])]) -> (Session, TempDir) {
    let media_root = TempDir::new().unwrap();
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let mut documents = Vec::with_capacity(rows.len());
    let mut captures = Vec::with_capacity(rows.len());
    for (key, title, bytes) in rows {
        fs::write(media_root.path().join(format!("{key}.mp3")), bytes).unwrap();
        let capture = format!(
            "sys.blob.annotate(sys.blob.capture_file({root}, \"{key}.mp3\", 65536), \"audio/mpeg\")"
        );
        documents.push(format!("{{\n    title: \"{title}\",\n    content: {capture},\n}}\n"));
        captures.push(capture);
    }
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(MEDIA_SOURCE, "Music");
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let blob_fields = captures
        .iter()
        .map(|capture| [("content", capture.as_str())])
        .collect::<Vec<_>>();
    let capture_rows = rows
        .iter()
        .zip(&blob_fields)
        .zip(&documents)
        .zip(0xa0_u8..)
        .map(|(((&(key, _, _), fields), document), ordinal)| media_commit::SourceCaptureRow {
            key,
            blob_fields: fields,
            source_row: document,
            ordinal,
            insert_only: true,
        })
        .collect::<Vec<_>>();
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;
    drop(state);
    let session = Session::mount(repository_directory, repository, relation_id, rows[0].0);
    (session, media_root)
}

fn media_row(title: &str, path: &str, media_type: &str) -> String {
    format!("{{\n    title: \"{title}\",\n    content: {{ path: \"{path}\", media_type: \"{media_type}\" }},\n}}\n")
}

/// A15-04, ORNA-VFS-004/012 and "missing stored fields fail": Blob descriptor
/// edits that the mount cannot apply are refused with EINVAL at fsync instead
/// of being acknowledged and dropped. W15-02 for Blob rows: after a save, a
/// row first opened later can replace its content.
#[tokio::test]
async fn blob_descriptor_edits_are_refused_instead_of_dropped() {
    if !require_fuse_or_skip() {
        return;
    }
    let (mut session, _media_root) = media_session(&[
        ("london", "London", b"london-mp3-bytes".as_slice()),
        ("paris", "Paris", b"paris-mp3-bytes".as_slice()),
    ])
    .await;
    let descriptor = "./content.mp3";
    let original = session.read("london");
    assert!(
        original.contains("path: \"./content.mp3\"") && original.contains("audio/mpeg"),
        "the row document carries the captured descriptor: {original}"
    );
    write_and_sync(
        &session.document("london"),
        &media_row("London edited", descriptor, "audio/mpeg"),
    )
    .expect("a metadata save with the captured descriptor is accepted");

    assert_eq!(
        fs::read(session.row("paris").join("content.mp3"))
            .expect("a Blob row first opened after a save is readable"),
        b"paris-mp3-bytes"
    );
    let paris_blob = session.row("paris").join("content.mp3");
    let paris_scratch = session.row("paris").join(".content.mp3.swap");
    fs::write(&paris_scratch, b"paris-replacement-bytes").expect("write a Blob temp file");
    fs::rename(&paris_scratch, &paris_blob)
        .expect("a Blob replacement in a row first opened after a save is accepted");
    assert_eq!(fs::read(&paris_blob).unwrap(), b"paris-replacement-bytes");

    for (document, context) in [
        (
            media_row("Absolute", "/etc/passwd", "audio/mpeg"),
            "an absolute Blob path is refused",
        ),
        (
            media_row("Traversal", "../paris/content.mp3", "audio/mpeg"),
            "a cross-row Blob path is refused",
        ),
        (
            media_row("Other file", "./other.mp3", "audio/mpeg"),
            "a Blob path outside the field projection is refused",
        ),
        (
            media_row("Retyped", descriptor, "audio/wav"),
            "a media type that does not describe the captured content is refused",
        ),
        (
            "{ title: \"No content\", }\n".to_owned(),
            "a missing required Blob field is refused",
        ),
        (
            "{ title: \"Not a descriptor\", content: \"./content.mp3\", }\n".to_owned(),
            "a Blob field that is not a descriptor is refused",
        ),
        (
            "{ title: \"Number\", content: 123, }\n".to_owned(),
            "a Blob field holding a number is refused",
        ),
    ] {
        assert_errno(
            write_and_sync(&session.document("london"), &document),
            EINVAL,
            context,
        );
        let current = session.read("london");
        assert!(
            current.contains("London edited") && current.contains("path: \"./content.mp3\""),
            "{context}: the accepted row is unchanged: {current}"
        );
    }
    write_and_sync(
        &session.document("london"),
        &media_row("London final", descriptor, "audio/mpeg"),
    )
    .expect("a corrected save is accepted");
    await_draft_guard(&session.mountpoint(), b'0', "the corrected save resolves london");
    assert_eq!(
        fs::read(session.row("london").join("content.mp3")).unwrap(),
        b"london-mp3-bytes",
        "metadata saves keep the captured content"
    );
    session.finish();
}

/// A Blob write-intent open, including `O_TRUNC`, is accepted and gets a private
/// candidate (ORNA-VFS-011, 41-vfs.md EDIT-1 step 1; profiles/vfs-1.md). Opening
/// does not change the accepted Blob: the stored bytes stay unchanged until a write
/// is accepted by fsync or close.
#[tokio::test]
async fn blob_write_open_is_accepted_and_leaves_accepted_bytes_unchanged() {
    if !require_fuse_or_skip() {
        return;
    }
    let (mut session, _media_root) =
        media_session(&[("london", "London", b"london-mp3-bytes".as_slice())]).await;
    let blob = session.row("london").join("content.mp3");
    let write_open = OpenOptions::new().write(true).open(&blob);
    assert!(write_open.is_ok(), "a Blob write-open is accepted: {write_open:?}");
    drop(write_open);
    let mut trunc_open = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&blob)
        .expect("a Blob O_TRUNC open is accepted");
    trunc_open.write_all(b"london-mp3-bytes").unwrap();
    trunc_open.sync_all().unwrap();
    drop(trunc_open);
    // The accepted bytes are restored by the fsync, so the session unmounts clean.
    assert_eq!(
        fs::read(&blob).unwrap(),
        b"london-mp3-bytes",
        "opening a Blob for write does not change the accepted content",
    );
    session.finish();
}

/// A Blob projection has no candidate, so unlink and a same-row rename of it are
/// refused with EPERM (profiles/vfs-1.md:110, :139; 41-vfs.md:103-105). The stored
/// Blob is unchanged afterwards.
#[tokio::test]
async fn blob_unlink_and_rename_are_refused_with_eperm() {
    if !require_fuse_or_skip() {
        return;
    }
    let (mut session, _media_root) =
        media_session(&[("london", "London", b"london-mp3-bytes".as_slice())]).await;
    let blob = session.row("london").join("content.mp3");
    assert_errno(fs::remove_file(&blob), EPERM, "a Blob unlink is refused");
    assert_errno(
        fs::rename(&blob, session.row("london").join("moved.mp3")),
        EPERM,
        "a same-row Blob rename is refused",
    );
    assert_eq!(
        fs::read(&blob).unwrap(),
        b"london-mp3-bytes",
        "a refused Blob unlink or rename leaves the stored content unchanged",
    );
    session.finish();
}

/// Structural mutations refused on the live writable mount are EPERM, not EROFS:
/// EROFS is reserved for historical and read-only views (profiles/vfs-1.md:139;
/// 41-vfs.md:103-105). A cross-row rename is also EPERM, not EXDEV: EXDEV would
/// let `mv` and `cp` fall back to copy-then-unlink, which the refused unlink of
/// the source would leave half-applied. Every refusal leaves the stored rows
/// unchanged.
#[tokio::test]
async fn live_mount_structural_refusals_are_eperm() {
    if !require_fuse_or_skip() {
        return;
    }
    let (mut session, _media_root) = media_session(&[
        ("london", "London", b"london-mp3-bytes".as_slice()),
        ("paris", "Paris", b"paris-mp3-bytes".as_slice()),
    ])
    .await;
    let london_before = session.read("london");
    assert_errno(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(session.mountpoint().join("Music").join("notes.txt")),
        EPERM,
        "a create directly in the table directory is refused",
    );
    assert_errno(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(session.row("london").join("notes.txt")),
        EPERM,
        "a create of a non-blob name inside a row is refused",
    );
    assert_errno(
        fs::remove_file(session.document("london")),
        EPERM,
        "unlinking a row's data.orna is refused",
    );
    assert_errno(
        fs::rename(
            session.row("london").join("content.mp3"),
            session.row("paris").join("moved.mp3"),
        ),
        EPERM,
        "a Blob rename across rows is refused with EPERM, not EXDEV",
    );
    assert_errno(
        fs::rename(
            session.document("london"),
            session.row("london").join("other.orna"),
        ),
        EPERM,
        "renaming a row's data.orna to another name is refused",
    );
    assert_errno(
        rename_with_flags(
            &session.row("london").join("content.mp3"),
            &session.row("london").join("renamed.mp3"),
            rustix::fs::RenameFlags::NOREPLACE,
        ),
        EPERM,
        "a same-row rename with flags is refused",
    );
    let truncate = std::process::Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], 0)"])
        .arg(session.row("london").join("content.mp3"))
        .env("LC_ALL", "C")
        .output()
        .expect("run python3 os.truncate on a Blob");
    assert!(
        !truncate.status.success(),
        "a path-based truncate of a Blob is refused",
    );
    assert!(
        String::from_utf8_lossy(&truncate.stderr).contains("Operation not permitted"),
        "a path-based truncate of a Blob fails with EPERM, got: {}",
        String::from_utf8_lossy(&truncate.stderr),
    );
    assert_eq!(
        session.read("london"),
        london_before,
        "refused structural mutations leave the row document unchanged",
    );
    assert_eq!(
        fs::read(session.row("london").join("content.mp3")).unwrap(),
        b"london-mp3-bytes",
        "a refused cross-row rename leaves the Blob in its row",
    );
    session.finish();
}

/// A temp-file save renamed to any name other than the row's `data.orna` is
/// refused with EPERM (41-vfs.md:103-105); the same temp file still completes
/// the save to `data.orna` afterwards.
#[tokio::test]
async fn temporary_rename_to_another_name_is_eperm_and_still_saves() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut session = Session::start(&[("london", "London")]).await;
    let before = session.read("london");
    let scratch = session.row("london").join(".data.orna.save");
    fs::write(&scratch, music_row("London renamed")).expect("write a temp file in a row");
    assert_errno(
        fs::rename(&scratch, session.row("london").join("other.txt")),
        EPERM,
        "a temp-file rename to a name other than data.orna is refused",
    );
    assert_eq!(
        session.read("london"),
        before,
        "the refused rename leaves the row document unchanged",
    );
    fs::rename(&scratch, session.document("london"))
        .expect("the refused temp file still saves to data.orna");
    assert!(session.read("london").contains("London renamed"));
    session.finish();
}
