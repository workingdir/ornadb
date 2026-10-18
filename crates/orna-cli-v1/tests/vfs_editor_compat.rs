#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::EnvironmentProvider;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::{SourceCaptureRow, commit_source_captures_for_table};

struct MountedProcess {
    child: Child,
    mountpoint: PathBuf,
}

impl Drop for MountedProcess {
    fn drop(&mut self) {
        if mountinfo_contains(&self.mountpoint) {
            if let Some(program) = std::env::var_os("FUSERMOUNT_PATH") {
                let _ = Command::new(program)
                    .args(["-u", "--"])
                    .arg(&self.mountpoint)
                    .status();
            } else {
                for program in ["fusermount3", "fusermount"] {
                    if Command::new(program)
                        .args(["-u", "--"])
                        .arg(&self.mountpoint)
                        .status()
                        .is_ok_and(|status| status.success())
                    {
                        break;
                    }
                }
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn mountinfo_contains(mountpoint: &Path) -> bool {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(mountpoint))
}

fn require_fuse_or_skip() -> bool {
    if std::env::var_os("ORNA_SKIP_FUSE_TEST").as_deref() == Some(std::ffi::OsStr::new("1")) {
        assert_ne!(
            std::env::var_os("ORNA_REQUIRE_FUSE").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "ORNA_REQUIRE_FUSE=1 forbids skipping FUSE tests",
        );
        eprintln!("SKIP: ORNA_SKIP_FUSE_TEST=1");
        return false;
    }
    let available = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_ok()
        && (std::env::var_os("FUSERMOUNT_PATH").is_some()
            || ["fusermount3", "fusermount"]
                .into_iter()
                .any(|helper| Command::new(helper).arg("-h").output().is_ok()));
    if !available
        && std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        panic!("required FUSE integration prerequisites are unavailable");
    }
    if !available {
        eprintln!("SKIP: FUSE integration prerequisites are unavailable");
    }
    available
}

fn write_and_sync(path: &Path, document: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(document.as_bytes())?;
    file.sync_all()
}
const IN_PLACE_SAVE_SETTINGS: &str =
    "Vim: -u NONE -n -i NONE; backupcopy=yes, nobackup, nowritebackup, noswapfile, fsync";
const TEMP_RENAME_SAVE_SETTINGS: &str = "Emacs: --batch -Q; file-precious-flag=t, write-region-inhibit-fsync=nil, make-backup-files=nil, auto-save-default=nil, create-lockfiles=nil";

fn command_version(program: &str, argument: &str) -> String {
    let output = Command::new(program)
        .arg(argument)
        .output()
        .unwrap_or_else(|error| panic!("run {program} {argument}: {error}"));
    assert!(
        output.status.success(),
        "{program} {argument} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_else(|| panic!("{program} {argument} returned no version"))
        .to_owned()
}

fn record_editor_environment() {
    let kernel =
        std::fs::read_to_string("/proc/sys/kernel/osrelease").expect("read Linux kernel release");
    let vim = command_version("vi", "--version");
    let emacs = command_version("emacs", "--version");
    let libfuse = command_version("fusermount3", "--version");
    eprintln!(
        "VFS editor compatibility: kernel={}; libfuse={libfuse}; in-place={vim} [{IN_PLACE_SAVE_SETTINGS}]; temp/rename={emacs} [{TEMP_RENAME_SAVE_SETTINGS}]",
        kernel.trim(),
    );
}

fn save_with_vim_in_place(path: &Path) {
    let output = Command::new("vi")
        .args([
            "-Es",
            "-u",
            "NONE",
            "-n",
            "-i",
            "NONE",
            "-c",
            "set backupcopy=yes nobackup nowritebackup noswapfile fsync",
            "-c",
            "%s/Original/Vim in-place/",
            "-c",
            "write",
            "-c",
            "quit",
        ])
        .arg(path)
        .output()
        .expect("start the real Vim editor");
    assert!(
        output.status.success(),
        "Vim in-place save failed ({}): stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn save_with_emacs_temp_rename(path: &Path) {
    let output = Command::new("emacs")
        .args([
            "--batch",
            "-Q",
            "--eval",
            "(setq file-precious-flag t write-region-inhibit-fsync nil make-backup-files nil auto-save-default nil create-lockfiles nil)",
            "--visit",
        ])
        .arg(path)
        .args([
            "--eval",
            "(progn (goto-char (point-min)) (search-forward \"Vim in-place\") (replace-match \"Emacs temp rename\") (save-buffer))",
        ])
        .output()
        .expect("start the real Emacs editor");
    assert!(
        output.status.success(),
        "Emacs temp/rename save failed ({}): stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

struct EditorMount {
    _mounted: MountedProcess,
    repository: orna_repository_v1::Repository,
    relation_id: [u8; 16],
    row_directory: PathBuf,
    data_path: PathBuf,
    _mount_parent: TempDir,
    _repository_directory: TempDir,
}

fn spawn_editor_mount(repository_path: &Path, mountpoint: &Path) -> MountedProcess {
    std::fs::create_dir(mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_path.to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.to_path_buf(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let stderr = mounted
                .child
                .stderr
                .take()
                .map(|mut pipe| {
                    let mut text = String::new();
                    let _ = pipe.read_to_string(&mut text);
                    text
                })
                .unwrap_or_default();
            panic!("mount exited {status}: {stderr}");
        }
        assert!(Instant::now() < deadline, "mount did not expose data.orna");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Original")
    );
    mounted
}
async fn start_editor_mount(foreign_owner: bool) -> EditorMount {
    let source = "pub table Music(id: Str) { title: Str, artist: Str, }\n";
    let initial = "{ title: \"Original\", artist: \"Kieran\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let lease_owner = if foreign_owner {
        [0x83; 16]
    } else {
        runtime_identity.repository_id
    };
    let writer = state.acquire_lease(lease_owner).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x90,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mounted = spawn_editor_mount(repository_directory.path(), &mountpoint);
    let row_directory = mountpoint.join("Music/london");
    let data_path = row_directory.join("data.orna");
    EditorMount {
        _mounted: mounted,
        repository,
        relation_id,
        row_directory,
        data_path,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
    }
}
/// ORNA-VFS-006: edits submitted from the editor path use the normal validated VFS admission path.
#[tokio::test]
async fn editor_saves_use_the_validated_vfs_row_path() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let repository = &fixture.repository;
    let relation_id = fixture.relation_id;
    let row_directory = &fixture.row_directory;
    let data_path = &fixture.data_path;
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Original")
    );
    let mut private_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .unwrap();
    let mut observer = File::open(&data_path).unwrap();
    private_writer
        .write_all(b"{ title: \"Private draft\", artist: \"Kieran\", }\n")
        .unwrap();
    private_writer.seek(SeekFrom::Start(0)).unwrap();
    let mut writer_view = String::new();
    private_writer.read_to_string(&mut writer_view).unwrap();
    assert!(writer_view.contains("Private draft"));
    let mut observer_view = String::new();
    observer.read_to_string(&mut observer_view).unwrap();
    assert!(observer_view.contains("Original"));
    drop(private_writer);
    drop(observer);
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Original")
    );
    write_and_sync(&data_path, "{ title: \"In place\", artist: \"Kieran\", }\n")
        .expect("in-place editor save must validate and sync");
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("In place")
    );

    let arbitrary = row_directory.join("unrelated");
    let mut scratch =
        File::create(&arbitrary).expect("unknown row-directory files are editor scratch");
    scratch.write_all(b"private editor scratch").unwrap();
    scratch
        .sync_all()
        .expect("scratch fsync preserves private bytes only");
    drop(scratch);
    assert!(
        std::fs::read_to_string(data_path)
            .unwrap()
            .contains("In place"),
        "syncing arbitrary scratch must not replace the accepted row",
    );

    let stale_temporary = row_directory.join(".data.orna.stale-tmp");
    let mut stale_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stale_temporary)
        .expect("editor creates a replacement scratch");
    stale_file
        .write_all(b"{ title: \"Stale candidate\", artist: \"Kieran\", }\n")
        .unwrap();
    stale_file.sync_all().unwrap();
    drop(stale_file);
    write_and_sync(
        &data_path,
        "{ title: \"New baseline\", artist: \"Kieran\", }\n",
    )
    .expect("later row save advances the baseline");
    assert!(
        std::fs::rename(&stale_temporary, &data_path).is_err(),
        "rename must reject a scratch whose captured row baseline is stale",
    );
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("New baseline"),
        "stale rename must preserve the newer accepted row",
    );
    let temporary = row_directory.join(".data.orna.editor-tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("editor creates temporary sibling");
    file.write_all(b"{ title: \"Renamed\", artist: \"Kieran\", }\n")
        .unwrap();
    file.sync_all().expect("editor syncs temporary file");
    drop(file);
    std::fs::rename(&temporary, &data_path).expect("editor atomically replaces data.orna");
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Renamed")
    );

    let durable_before_rejection = format3::committed_store_rows(&repository, relation_id);
    let mut invalid = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .expect("open row document for invalid edit");
    invalid.set_len(0).unwrap();
    invalid
        .write_all(b"{ title: 123, artist: \"Kieran\", }\n")
        .unwrap();
    assert!(
        invalid.sync_all().is_err(),
        "invalid row must be rejected at sync"
    );
    invalid.seek(SeekFrom::Start(0)).unwrap();
    let mut rejected_draft = String::new();
    invalid.read_to_string(&mut rejected_draft).unwrap();
    assert!(rejected_draft.contains("title: 123"));
    drop(invalid);
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Renamed")
    );
    let durable_after_rejection = format3::committed_store_rows(&repository, relation_id);
    assert_eq!(
        durable_after_rejection, durable_before_rejection,
        "rejected editor draft must leave the durable row unchanged",
    );
}

/// ORNA-VFS-006: a writer without the selected worktree lease cannot submit an edit.
#[tokio::test]
async fn editor_saves_reject_a_foreign_writer_lease() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(true).await;
    let durable_before = format3::committed_store_rows(&fixture.repository, fixture.relation_id);

    let error = write_and_sync(
        &fixture.data_path,
        "{ title: \"Rejected\", artist: \"Kieran\", }\n",
    )
    .expect_err("a foreign writer lease must reject an editor save");
    assert_eq!(error.raw_os_error(), Some(5));
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Original")
    );
    let durable_after = format3::committed_store_rows(&fixture.repository, fixture.relation_id);
    assert_eq!(
        durable_after, durable_before,
        "rejected save must preserve the durable row",
    );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&fixture.repository);
    let state = RuntimeState::open(&fixture.repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert!(matches!(
        state.acquire_lease(runtime_identity.repository_id).await,
        Err(orna_runtime_v1::RuntimeError::LeaseHeld),
    ));
}
#[tokio::test]
async fn real_editors_save_through_the_mounted_linux_vfs() {
    assert!(
        require_fuse_or_skip(),
        "real editor compatibility requires a mounted Linux FUSE view",
    );
    let fixture = start_editor_mount(false).await;
    assert!(mountinfo_contains(&fixture._mounted.mountpoint));
    record_editor_environment();

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&fixture.repository);
    let engine_state = RuntimeState::open(&fixture.repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let pending_before = engine_state.pending_count().await.unwrap();
    save_with_vim_in_place(&fixture.data_path);
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Vim in-place"),
        "Vim's in-place save must be visible through the mounted view",
    );
    let vim_pending = engine_state.pending_count().await.unwrap();
    assert_eq!(
        vim_pending,
        pending_before + 1,
        "Vim's accepted save must add one durable engine mutation",
    );

    save_with_emacs_temp_rename(&fixture.data_path);
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Emacs temp rename"),
        "Emacs's temp/rename save must be visible through the mounted view",
    );
    let emacs_pending = engine_state.pending_count().await.unwrap();
    assert_eq!(
        emacs_pending,
        vim_pending + 1,
        "Emacs's accepted save must add one durable engine mutation",
    );
}
#[tokio::test]
async fn fsynced_temporary_scratch_survives_mount_restart() {
    assert!(
        require_fuse_or_skip(),
        "temporary scratch durability requires a mounted Linux FUSE view",
    );
    let fixture = start_editor_mount(false).await;
    let head_before_restart = fixture.repository.head().unwrap().unwrap();
    let repository = fixture.repository.clone();
    let relation_id = fixture.relation_id;
    assert!(mountinfo_contains(&fixture._mounted.mountpoint));

    let scratch_path = fixture.row_directory.join(".data.orna.persisted-scratch");
    let expected = b"durable editor scratch, not an accepted row";
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_path)
        .expect("create temporary scratch through FUSE");
    scratch.write_all(expected).unwrap();
    scratch
        .sync_all()
        .expect("fsync persists the private scratch");
    drop(scratch);

    let EditorMount {
        _mounted: previous_mount,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
        ..
    } = fixture;
    drop(previous_mount);
    assert_eq!(
        repository.head().unwrap().unwrap().as_str(),
        head_before_restart.as_str(),
        "fsync of temporary scratch must not advance workspace HEAD",
    );
    let context = repository.open_format_context_at_selector("HEAD").unwrap();
    let row_map = context.load_row_map(relation_id).unwrap();
    assert_eq!(
        row_map.version().row_count(),
        Some(1),
        "fsync of temporary scratch must leave the committed row present",
    );
    let restarted_mountpoint = mount_parent.path().join("restarted");
    let _restarted_mount = spawn_editor_mount(repository_directory.path(), &restarted_mountpoint);
    assert!(mountinfo_contains(&restarted_mountpoint));
    let restarted_row = restarted_mountpoint.join("Music/london");
    assert_eq!(
        std::fs::read(restarted_row.join(".data.orna.persisted-scratch")).unwrap(),
        expected,
        "synced scratch survives the mount process restart",
    );
    assert!(
        std::fs::read_to_string(restarted_row.join("data.orna"))
            .unwrap()
            .contains("Original"),
        "fsync of temporary scratch must not accept the row",
    );
}
