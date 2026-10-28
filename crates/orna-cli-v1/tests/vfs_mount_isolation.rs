#![cfg(target_os = "linux")]

//! Gate E isolation for the writable VFS: private drafts stay invisible to
//! other opens and remounts, read handles pin their generation, kernel IO
//! modes do not leak drafts, and temp-and-rename saves are compared against
//! their captured baseline (including a baseline moved by `orna edit`). A REPL
//! write is refused while the mount is live and is asserted as such; the
//! REPL-concurrency baseline cases are not reachable through this fixture.
//!
//! Spec (Orna-1.1.0, source/41-vfs.md): EDIT-1 ("A writer sees its own draft;
//! other opens see accepted state"; "Read handles ... pin coherent
//! generations"; step 5 close-time flush by the writer), the Temporary-save
//! replacement paragraph ("This does not prove when the editor originally read
//! the row"), ORNA-VFS-009, ORNA-VFS-011, ORNA-VFS-022, ORNA-VFS-024, and
//! ACCEPTANCE Gate E.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

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
    EditorMount, assert_errno, await_draft_guard, fresh_view, open_rw, require_fuse_or_skip,
    start_editor_mount, start_mount_process, wait_for_mount,
};

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn signal(process: u32, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(process.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {process} failed");
}

/// Reads a handle from its start, so each call observes the handle's own
/// generation rather than the position left by the previous read.
fn read_from_start(file: &mut File) -> String {
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read handle");
    text
}

/// Creates a temporary sibling of `data.orna` the way an editor's temp-and-rename
/// save does: create, fill and fsync the scratch, then close it. Creation
/// captures the row's current baseline (Temporary-save replacement).
fn write_temporary_sibling(fixture: &EditorMount, name: &str, document: &str) -> PathBuf {
    let path = fixture.row_directory.join(name);
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create temporary sibling");
    scratch
        .write_all(document.as_bytes())
        .expect("write temporary sibling");
    scratch.sync_all().expect("fsync temporary sibling");
    path
}

/// Commits `title` through the owner-routed `orna edit` while the mount is live,
/// the same path `replace_row_through_owner` uses in `vfs_mount_integration`.
/// The commit advances the row past any baseline captured before it.
fn owner_routed_edit(fixture: &EditorMount, title: &str) {
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("owner-routed-edit");
    std::fs::write(
        &editor,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}' > \"$1\"\n",
            music_row(title).trim_end()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &editor)
        .output()
        .expect("run orna edit through the mount owner");
    assert!(
        output.status.success(),
        "owner-routed orna edit failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Runs one REPL session against the repository at `root`, the way a user's
/// REPL write reaches the store while the mount is live.
fn orna_repl(root: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start orna repl");
    child
        .stdin
        .take()
        .expect("repl stdin")
        .write_all(input.as_bytes())
        .expect("write repl input");
    child.wait_with_output().expect("wait for orna repl")
}

/// Submits one REPL update of row `london` while the mount is live.
fn repl_update_attempt(fixture: &EditorMount, title: &str) -> Output {
    orna_repl(
        fixture._repository_directory.path(),
        &format!("main.Music.update(\"london\", {{ title: \"{title}\" }})"),
    )
}

/// EDIT-1: "A writer sees its own draft; other opens see accepted state." An
/// unsynced private draft stays invisible to a second open even after a child
/// process has been spawned while the writer's descriptor is open. exec(2)
/// closes the inherited CLOEXEC copy of that descriptor in the child, and the
/// kernel then issues a FLUSH under the child's lock owner. That FLUSH is not
/// the writer's close (EDIT-1 step 5), so it must not accept the draft.
/// SIGKILL plus a remount does not turn the draft into accepted state
/// (ORNA-VFS-024).
#[tokio::test]
async fn unsynced_draft_is_invisible_to_second_open_and_after_child_exec_and_sigkill_remount() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mountpoint = fixture._mount_parent.path().join("library");

    let mut draft = open_rw(&fixture);
    draft.set_len(0).unwrap();
    draft.write_all(music_row("Unsynced draft").as_bytes()).unwrap();
    assert!(
        read_from_start(&mut draft).contains("Unsynced draft"),
        "the writer sees its own draft"
    );

    // Spawn a child while the draft descriptor is open: its exec closes the
    // inherited copy, which is the FLUSH path this test pins down.
    let child = Command::new("true").output().expect("spawn a child process");
    assert!(child.status.success(), "the child process exits 0");

    let mut second = File::open(&fixture.data_path).unwrap();
    assert_eq!(
        read_from_start(&mut second),
        original,
        "a second handle sees the accepted row, never the unsynced draft"
    );
    assert_eq!(
        fresh_view(&fixture),
        original,
        "a new open sees the accepted row, never the unsynced draft"
    );

    // Kill with the writer's descriptor still open. Once the daemon is gone the
    // connection is aborted, so closing the descriptor afterwards cannot flush
    // the draft into acceptance.
    signal(fixture._mounted.pid(), "KILL");
    fixture
        ._mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("SIGKILL stops the mount");
    fixture._mounted.release_mountpoint();
    drop(draft);
    drop(second);

    fixture._mounted = wait_for_mount(start_mount_process(
        fixture._repository_directory.path(),
        &mountpoint,
    ));
    assert_eq!(
        fresh_view(&fixture),
        original,
        "a remount after SIGKILL serves only the accepted row"
    );
}

/// EDIT-1 "read handles ... pin coherent generations"; ACCEPTANCE Gate E: "Open
/// reads remain on old content after replacement." A handle opened before a
/// temp rename-over keeps its captured bytes and size, while a new open sees the
/// replacement.
#[tokio::test]
async fn read_handle_opened_before_temp_rename_keeps_old_bytes() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut old_handle = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut old_handle), original);

    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.replace-temp",
        &music_row("Temp replacement"),
    );
    std::fs::rename(&temp, &fixture.data_path).expect("rename admits the temp replacement");

    assert!(
        fresh_view(&fixture).contains("Temp replacement"),
        "a new open sees the admitted replacement"
    );
    assert_eq!(
        read_from_start(&mut old_handle),
        original,
        "a handle opened before the replacement reads its captured bytes"
    );
    assert_eq!(
        old_handle.metadata().unwrap().len(),
        original.len() as u64,
        "fstat on the handle reports the captured generation's size"
    );
    drop(old_handle);
}

/// F2 finding: a read handle opened before an owner-routed `orna edit`, then a
/// temp created after that edit. The temp captures the mount's recorded row
/// baseline, which `fuse_mount` leaves at the pre-edit value after an owner-routed
/// edit, so the rename is refused with EAGAIN even though the temp was created
/// after the commit. Spec-silent on whether temp creation must capture the
/// durable row rather than the mounted baseline, so this asserts the observed
/// conservative refusal and flags it. The pre-edit read handle keeps its
/// generation (EDIT-1 read-handle pinning) regardless.
#[tokio::test]
async fn read_handle_across_owner_edit_temp_created_after_it_is_refused_f2_finding() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut pinned = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut pinned), original);

    owner_routed_edit(&fixture, "Owner change");
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.after-owner",
        &music_row("After owner"),
    );
    assert_errno(
        std::fs::rename(&temp, &fixture.data_path),
        &[libc::EAGAIN],
        "F2: a temp created after the owner edit is refused by the mounted baseline",
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Owner change") && !view.contains("After owner"),
        "the refused temp leaves the owner's row in place, got {view}"
    );
    assert_eq!(
        read_from_start(&mut pinned),
        original,
        "the read handle opened before the owner edit keeps its generation"
    );
    drop(pinned);
    std::fs::remove_file(&temp).expect("remove the refused scratch");
}

/// ORNA-VFS-011 CAS against the captured temp-create baseline; ORNA-VFS-009
/// (stale candidate preserves the accepted row and retains the draft bytes).
/// A temp created before another writer's commit captures the older baseline,
/// so its rename is refused with EAGAIN. The owner's row stays, and the scratch
/// keeps its bytes and holds the draft guard until it is removed.
#[tokio::test]
async fn temp_created_before_owner_change_is_refused_with_eagain_and_keeps_scratch() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.stale-temp",
        &music_row("Stale temp"),
    );
    owner_routed_edit(&fixture, "Owner change");

    assert_errno(
        std::fs::rename(&temp, &fixture.data_path),
        &[libc::EAGAIN],
        "a temp created before the owner change must be refused",
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Owner change") && !view.contains("Stale temp"),
        "the refused temp leaves the owner's row in place, got {view}"
    );
    assert_eq!(
        std::fs::read_to_string(&temp).expect("the refused scratch is kept"),
        music_row("Stale temp"),
    );
    await_draft_guard(&fixture, b'1', "the refused scratch is an unresolved draft").await;

    std::fs::remove_file(&temp).expect("remove the refused scratch");
    await_draft_guard(&fixture, b'0', "removing the refused scratch clears the guard").await;
}

/// A REPL update of the mounted row while the mount is live is refused with
/// `ORNA-REPL-EFFECT` and leaves the row unchanged. This fixture cannot exercise
/// the REPL-concurrency baseline cases (temp captured before or after a REPL
/// write), because the non-mount REPL write never reaches the store here. This
/// test asserts only the refusal observed in this configuration; the reason it
/// is refused is not established by this test.
#[tokio::test]
async fn repl_update_while_mount_is_live_is_refused_and_leaves_row_unchanged() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let output = repl_update_attempt(&fixture, "Repl change");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        text.contains("error[ORNA-REPL-EFFECT]"),
        "expected the REPL write to be refused, got {text}"
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Original") && !view.contains("Repl change"),
        "the refused REPL write leaves the row unchanged, got {view}"
    );
}

/// ORNA-VFS-022: "supported caching/IO modes MUST preserve per-open generation
/// semantics despite non-VFS database writers"; ACCEPTANCE Gate E: "Kernel
/// caching must not leak private drafts into another handle/generation."
/// Repeated observer opens and a read handle opened before the write never show
/// the private draft. After acceptance, the earlier handle keeps its generation
/// and a new open sees the accepted bytes.
#[tokio::test]
async fn repeated_observer_reads_never_show_private_draft_and_read_handles_pin_generation() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut pinned = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut pinned), original);

    let mut writer = open_rw(&fixture);
    writer.set_len(0).unwrap();
    writer.write_all(music_row("Observer draft").as_bytes()).unwrap();
    assert!(
        read_from_start(&mut writer).contains("Observer draft"),
        "the writer sees its own draft"
    );

    for _ in 0..5 {
        assert_eq!(
            fresh_view(&fixture),
            original,
            "a repeated observer open never reads the private draft"
        );
        assert_eq!(
            read_from_start(&mut pinned),
            original,
            "the read handle keeps its generation while the draft is private"
        );
    }

    writer.sync_all().expect("fsync accepts the private draft");
    assert!(
        fresh_view(&fixture).contains("Observer draft"),
        "a new open sees the accepted draft"
    );
    assert_eq!(
        read_from_start(&mut pinned),
        original,
        "a read handle opened before acceptance keeps its captured generation"
    );
    drop(writer);
    drop(pinned);
}
