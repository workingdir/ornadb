#![allow(dead_code)]

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::EnvironmentProvider;
use tempfile::TempDir;

use crate::format3;
use crate::media_commit::{SourceCaptureRow, commit_source_captures_for_table};

pub struct MountedProcess {
    child: Child,
    mountpoint: PathBuf,
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
    unmount_on_drop: bool,
}

impl MountedProcess {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Returns the mount process's stderr captured so far.
    pub fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The mount process id, so signal tests can stop it.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits up to `timeout` for the mount process to exit. Returns its status,
    /// or `None` while it is still running. Once it has exited, its complete
    /// stderr is available from `stderr_text`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                if let Some(reader) = self.stderr_reader.take() {
                    let _ = reader.join();
                }
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until `path` exists inside the mount, failing if the mount process
    /// exits first.
    pub fn await_path(&mut self, path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.exited(status);
            }
            assert!(
                Instant::now() < deadline,
                "mount did not expose {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Hands the mountpoint to the next command: this guard no longer unmounts
    /// it when dropped. Signal tests use this after the process died so a
    /// recovering `orna mount` or `orna unmount` sees the dead FUSE mount.
    pub fn release_mountpoint(&mut self) {
        self.unmount_on_drop = false;
    }

    fn exited(&mut self, status: std::process::ExitStatus) -> ! {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        panic!("mount exited {status}: {}", self.stderr_text());
    }
}

impl Drop for MountedProcess {
    fn drop(&mut self) {
        if self.unmount_on_drop && mountinfo_contains(&self.mountpoint) {
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

pub fn mountinfo_contains(mountpoint: &Path) -> bool {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(mountpoint))
}

pub fn require_fuse_or_skip() -> bool {
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

pub fn write_and_sync(path: &Path, document: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(document.as_bytes())?;
    file.sync_all()
}

pub struct EditorMount {
    pub _mounted: MountedProcess,
    /// A live writer other than the mount, holding the worktree's runtime owner
    /// lock and writer lease for the fixture's lifetime.
    pub _foreign_writer: Option<RuntimeState>,
    pub repository: Repository,
    pub relation_id: [u8; 16],
    pub row_directory: PathBuf,
    pub data_path: PathBuf,
    pub _mount_parent: TempDir,
    pub _repository_directory: TempDir,
}

/// Starts `mount --cwd` and waits until the Linux mount table lists `mountpoint`.
pub fn spawn_mount_process(repository_path: &Path, mountpoint: &Path) -> MountedProcess {
    std::fs::create_dir(mountpoint).unwrap();
    wait_for_mount(start_mount_process(repository_path, mountpoint))
}

/// Starts `mount --cwd` on an existing mountpoint directory without waiting for
/// the FUSE mount to appear.
pub fn start_mount_process(repository_path: &Path, mountpoint: &Path) -> MountedProcess {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let stderr = Arc::new(Mutex::new(String::new()));
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push_str(&String::from_utf8_lossy(&buffer[..read])),
                }
            }
        })
    });
    MountedProcess {
        child,
        mountpoint: mountpoint.to_path_buf(),
        stderr,
        stderr_reader,
        unmount_on_drop: true,
    }
}

/// Waits until the Linux mount table lists the process's mountpoint.
pub fn wait_for_mount(mut mounted: MountedProcess) -> MountedProcess {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mountinfo_contains(&mounted.mountpoint) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            mounted.exited(status);
        }
        assert!(Instant::now() < deadline, "mount did not appear");
        thread::sleep(Duration::from_millis(20));
    }
    mounted
}

pub fn spawn_editor_mount(repository_path: &Path, mountpoint: &Path) -> MountedProcess {
    let mut mounted = spawn_mount_process(repository_path, mountpoint);
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            mounted.exited(status);
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

/// Runs `orna unmount` for a mount started from `repository`.
pub fn run_orna_unmount(repository: &Path, mountpoint: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--db", repository.to_str().unwrap(), "unmount"])
        .arg(mountpoint)
        .output()
        .expect("run `orna unmount`");
    assert!(
        output.status.success(),
        "orna unmount failed; stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, artist: Str, }\n";

/// The writer owner of the live foreign writer `start_editor_mount(true)` keeps.
pub const FOREIGN_OWNER: [u8; 16] = [0x83; 16];

/// Creates a repository whose `Music` table has one committed row named `key`.
pub async fn repository_with_music_row(
    key: &str,
    ordinal: u8,
    initial: &str,
) -> (TempDir, Repository, [u8; 16]) {
    repository_with_source_row(MUSIC_SOURCE, key, ordinal, initial).await
}

/// Like `repository_with_music_row`, with a caller-supplied table `source`.
pub async fn repository_with_source_row(
    source: &str,
    key: &str,
    ordinal: u8,
    initial: &str,
) -> (TempDir, Repository, [u8; 16]) {
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            &format!("Music/{key}.orna"),
            initial,
        );
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            key,
            blob_fields: &[],
            source_row: Some(initial),
            ordinal,
            insert_only: true,
        }],
    )
    .await;
    drop(state);
    (repository_directory, repository, relation_id)
}

/// Opens a writer that is not the mount and keeps it live: it holds the
/// worktree's runtime owner lock and takes the writer lease for
/// `FOREIGN_OWNER`, so a mount started afterwards is only a secondary
/// participant (ORNA-TURSO-005).
pub async fn live_foreign_writer(repository: &Repository) -> RuntimeState {
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert!(
        state.holds_runtime_owner_lock(),
        "the foreign writer owns the worktree runtime"
    );
    state
        .acquire_lease(FOREIGN_OWNER)
        .await
        .expect("the foreign writer takes the writer lease");
    state
}

/// Creates a repository whose `Music` table has one committed row for each
/// `(key, document)` pair.
pub async fn repository_with_music_rows(
    rows: &[(&str, &str)],
) -> (TempDir, Repository, [u8; 16]) {
    let paths = rows
        .iter()
        .map(|(key, document)| (format!("Music/{key}.orna"), *document))
        .collect::<Vec<_>>();
    let sources = paths
        .iter()
        .map(|(path, document)| (path.as_str(), *document))
        .collect::<Vec<_>>();
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_rows(MUSIC_SOURCE, "Music", &sources);
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    let captures = rows
        .iter()
        .zip(0x90_u8..)
        .map(|(&(key, document), ordinal)| SourceCaptureRow {
            key,
            blob_fields: &[],
            source_row: Some(document),
            ordinal,
            insert_only: true,
        })
        .collect::<Vec<_>>();
    commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &captures,
    )
    .await;
    drop(state);
    (repository_directory, repository, relation_id)
}

/// The contents of every retained candidate record in the repository's VFS
/// recovery storage. Records are the top-level regular files; the private
/// `editor-drafts` subdirectory holds explicit edit drafts, not records.
pub fn retained_drafts(repository: &Repository) -> Vec<String> {
    let directory = repository
        .runtime_paths()
        .root()
        .join("vfs-retained-drafts-v1");
    match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| entry.expect("list retained drafts").path())
            .filter(|path| path.is_file())
            .map(|path| {
                String::from_utf8_lossy(&std::fs::read(&path).expect("read retained draft"))
                    .into_owned()
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("list retained drafts in {}: {error}", directory.display()),
    }
}

/// Mounts a repository whose `Music/london` row is `Original`. With
/// `foreign_owner`, a live foreign writer owns the worktree for the fixture's
/// lifetime, so the mount runs as a secondary participant.
pub async fn start_editor_mount(foreign_owner: bool) -> EditorMount {
    start_editor_mount_with_source(
        "pub table Music(id: Str) { title: Str, artist: Str, }\n",
        "{ title: \"Original\", artist: \"Kieran\", }\n",
        foreign_owner,
    )
    .await
}

pub async fn start_editor_mount_with_source(
    source: &str,
    initial: &str,
    foreign_owner: bool,
) -> EditorMount {
    let (repository_directory, repository, relation_id) =
        repository_with_source_row(source, "london", 0x90, initial).await;
    let foreign_writer = if foreign_owner {
        Some(live_foreign_writer(&repository).await)
    } else {
        None
    };

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mounted = spawn_editor_mount(repository_directory.path(), &mountpoint);
    let row_directory = mountpoint.join("Music/london");
    let data_path = row_directory.join("data.orna");
    EditorMount {
        _mounted: mounted,
        _foreign_writer: foreign_writer,
        repository,
        relation_id,
        row_directory,
        data_path,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
    }
}

pub const EIO: i32 = 5;
pub const ENOSPC: i32 = 28;
pub const EDQUOT: i32 = 122;

/// Opens the mounted `data.orna` read-write without truncating it.
pub fn open_rw(fixture: &EditorMount) -> File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fixture.data_path)
        .expect("open data.orna read-write")
}

/// Reads the accepted row through a new open, never through an editor handle.
pub fn fresh_view(fixture: &EditorMount) -> String {
    std::fs::read_to_string(&fixture.data_path).expect("read accepted state through a new open")
}

/// Asserts that `result` failed with one of `expected` errno values.
pub fn assert_errno(result: std::io::Result<()>, expected: &[i32], context: &str) {
    let error = result.expect_err(context);
    let code = error.raw_os_error();
    assert!(
        code.is_some_and(|code| expected.contains(&code)),
        "{context}: expected one of {expected:?}, got {error}",
    );
}

/// Draft guard value the mount publishes on its root: `1` while any draft is
/// unresolved, `0` when none is.
pub fn draft_guard(fixture: &EditorMount) -> u8 {
    let root = fixture._mount_parent.path().join("library");
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(root.as_path(), "user.orna.unresolved-drafts", &mut value)
        .expect("read the mount draft guard");
    assert_eq!(length, 1, "the draft guard is one byte");
    value[0]
}

/// Waits up to five seconds for the draft guard to reach `expected`. The FUSE
/// release handler can run after close(2) returns, so a guard read right after
/// a drop polls instead of reading once.
pub async fn await_draft_guard(fixture: &EditorMount, expected: u8, context: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let value = draft_guard(fixture);
        if value == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("{context}: guard is {value}, expected {expected} after 5s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
