#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::{RuntimeState, RuntimeTableIdentity, TableObjectId};
use orna_sys_v1::EnvironmentProvider;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::{commit_source_captures_for_table, SourceCaptureRow};

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
    if !available && std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1")) {
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

async fn current_runtime_row(
    repository: &orna_repository_v1::Repository,
    relation_id: [u8; 16],
    table: &str,
    key: &[u8],
) -> Option<Vec<u8>> {
    let (identity, digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, digest).await.unwrap();
    let table_identity = RuntimeTableIdentity::new(table, TableObjectId::new(relation_id)).unwrap();
    let snapshot = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_identity))
        .await
        .unwrap();
    snapshot
        .query_exact(table_identity.table(), key)
        .unwrap()
        .map(<[u8]>::to_vec)
}

fn editor_script(directory: &TempDir, document: &str) -> PathBuf {
    let path = directory.path().join("editor.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat > \"$1\" <<'ORNADB_TEST'\n{document}ORNADB_TEST\n"),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
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

async fn start_editor_mount(foreign_owner: bool) -> EditorMount {
    start_editor_mount_with_source(
        "pub table Music(id: Str) { title: Str, artist: Str, }\n",
        "{ title: \"Original\", artist: \"Kieran\", }\n",
        foreign_owner,
    )
    .await
}

async fn start_editor_mount_with_source(
    source: &str,
    initial: &str,
    foreign_owner: bool,
) -> EditorMount {
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
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
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
        mountpoint: mountpoint.clone(),
    };

    let row_directory = mountpoint.join("Music/london");
    let data_path = row_directory.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let stderr = mounted
                .child
                .stderr
                .take()
                .map(|mut pipe| {
                    use std::io::Read;
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
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
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

fn run_row_edit(repository_directory: &TempDir, editor: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(repository_directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("EDITOR", editor)
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "edit",
            "--row",
            "Music/london/data.orna",
        ])
        .output()
        .expect("production orna edit --row entrypoint runs")
}

/// ORNA-VFS-014: unsupported namespace and shared-mapping operations fail through FUSE.
#[tokio::test]
async fn editor_mount_rejects_unsupported_links_devices_and_shared_mmap() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;

    let hard_link = fixture.row_directory.join("hard-link");
    assert_eq!(
        std::fs::hard_link(&fixture.data_path, &hard_link)
            .expect_err("hard links are unsupported")
            .raw_os_error(),
        Some(libc::EOPNOTSUPP),
    );

    let symbolic_link = fixture.row_directory.join("symbolic-link");
    assert_eq!(
        std::os::unix::fs::symlink("data.orna", &symbolic_link)
            .expect_err("symbolic links are unsupported")
            .raw_os_error(),
        Some(libc::EOPNOTSUPP),
    );

    let fifo = fixture.row_directory.join("fifo");
    let fifo_output = Command::new("mknod")
        .env("LC_ALL", "C")
        .arg(&fifo)
        .arg("p")
        .output()
        .expect("mknod is available on Linux");
    assert!(
        !fifo_output.status.success(),
        "the FUSE adapter rejects node creation"
    );
    let fifo_error = String::from_utf8_lossy(&fifo_output.stderr);
    assert!(
        fifo_error.contains("Operation not supported"),
        "FIFO mknod should reach the unsupported FUSE operation, got: {fifo_error}"
    );

    let device_node = fixture.row_directory.join("device-node");
    let device_output = Command::new("mknod")
        .env("LC_ALL", "C")
        .arg(&device_node)
        .args(["c", "0", "0"])
        .output()
        .expect("mknod is available on Linux");
    assert!(!device_output.status.success(), "device nodes are unsupported");
    let device_error = String::from_utf8_lossy(&device_output.stderr);
    assert!(
        device_error.contains("Operation not supported")
            || device_error.contains("Operation not permitted"),
        "unexpected device-node mknod error: {device_error}"
    );

    let mmap_output = Command::new("python3")
        .arg("-c")
        .arg(
            r#"import mmap, os, sys
fd = os.open(sys.argv[1], os.O_RDWR)
try:
    mmap.mmap(fd, 1, flags=mmap.MAP_SHARED, prot=mmap.PROT_READ | mmap.PROT_WRITE)
except OSError as error:
    print(error.errno)
else:
    print(0)"#,
        )
        .arg(&fixture.data_path)
        .output()
        .expect("python3 is available on Linux");
    assert!(
        mmap_output.status.success(),
        "mmap probe failed: {}",
        String::from_utf8_lossy(&mmap_output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&mmap_output.stdout).trim(),
        libc::ENODEV.to_string(),
        "writable shared mmap is unsupported"
    );
}

/// ORNA-VFS-020/022: writes remain speculative; admission validates the source-backed candidate and stale handles cannot overwrite a later CLI commit.
#[tokio::test]
async fn speculative_source_writes_validate_on_sync_and_reject_stale_generation() {
    if !require_fuse_or_skip() {
        return;
    }
    let source = "pub table Music(id: Str) {\n    title: Str,\n    artist: Str,\n    assert every(row => row.title != \"Forbidden\");\n}\n";
    let fixture = start_editor_mount_with_source(
        source,
        "{ title: \"Original\", artist: \"Kieran\", }\n",
        false,
    )
    .await;
    let original = current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london")
        .await
        .expect("initial committed row exists");

    let mut open_draft = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fixture.data_path)
        .unwrap();
    open_draft.set_len(0).unwrap();
    open_draft
        .write_all(b"{ title: \"Forbidden\", artist: \"Kieran\", }\n")
        .unwrap();
    assert_eq!(
        current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london").await,
        Some(original.clone()),
        "speculative FUSE writes must not evaluate or commit a source candidate",
    );
    assert_eq!(
        open_draft.sync_all().unwrap_err().raw_os_error(),
        Some(libc::EIO),
        "the production shared-workspace source validator rejects the complete candidate",
    );
    assert_eq!(
        current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london").await,
        Some(original.clone()),
        "rejected candidate leaves the admitted source row unchanged",
    );

    open_draft.set_len(0).unwrap();
    open_draft
        .write_all(b"{ title: \"Stale draft\", artist: \"Kieran\", }\n")
        .unwrap();
    let editor_directory = TempDir::new().unwrap();
    let external_editor = editor_script(
        &editor_directory,
        "{ title: \"External commit\", artist: \"Kieran\", }\n",
    );
    let external = run_row_edit(&fixture._repository_directory, &external_editor);
    assert!(
        external.status.success(),
        "non-VFS orna edit --row failed: {}; initial row={:?}",
        String::from_utf8_lossy(&external.stderr),
        orna_value_v1::decode_canonical_storage_value(&original),
    );
    let externally_committed = current_runtime_row(
        &fixture.repository,
        fixture.relation_id,
        "Music",
        b"london",
    )
    .await
    .expect("the non-VFS writer committed the row");
    assert!(
        String::from_utf8_lossy(&externally_committed).contains("External commit"),
        "the non-VFS writer's row is current",
    );

    assert_eq!(
        open_draft.sync_all().unwrap_err().raw_os_error(),
        Some(libc::EIO),
        "an open draft pinned to the earlier generation must reject its stale save",
    );
    open_draft.seek(SeekFrom::Start(0)).unwrap();
    let mut pinned_draft = String::new();
    open_draft.read_to_string(&mut pinned_draft).unwrap();
    assert!(pinned_draft.contains("Stale draft"), "the open handle remains pinned to its draft");
    assert_eq!(
        current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london").await,
        Some(externally_committed),
        "stale VFS save must preserve the newer non-VFS database row",
    );
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
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
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
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
    write_and_sync(
        &data_path,
        "{ title: \"In place\", artist: \"Kieran\", }\n",
    )
    .expect("in-place editor save must validate and sync");
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("In place"));


    let arbitrary = row_directory.join("unrelated");
    assert!(
        File::create(&arbitrary).is_err(),
        "an arbitrary sibling create must not become a row-save candidate",
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
        std::fs::read_to_string(&data_path).unwrap().contains("New baseline"),
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
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));

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
    assert!(invalid.sync_all().is_err(), "invalid row must be rejected at sync");
    invalid.seek(SeekFrom::Start(0)).unwrap();
    let mut rejected_draft = String::new();
    invalid.read_to_string(&mut rejected_draft).unwrap();
    assert!(rejected_draft.contains("title: 123"));
    drop(invalid);
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));
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
        durable_after,
        durable_before,
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
