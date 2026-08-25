//! Offline process-boundary tests for the installed `orna source diff` command.
//!
//! These tests invoke the real binary, but never start the installed host or
//! connect to PostgreSQL. The source reader must reject every invalid input
//! before host inspection; a valid V1 source must then reach the service
//! account guard. Every invocation is bounded and checks that its scratch
//! directory remains unchanged.

#![cfg(unix)]

use nix::{sys::stat::Mode, unistd::mkfifo};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const VALID_V1_SOURCE: &[u8] =
    b"CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "source-diff-test-{}-{}-{label}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write scratch file");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    File,
    Directory,
    Symlink,
    Fifo,
    Other,
}

#[derive(Debug, Eq, PartialEq)]
struct EntrySnapshot {
    kind: Kind,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    length: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    content: Option<Vec<u8>>,
    target: Option<Vec<u8>>,
}

#[derive(Debug, Eq, PartialEq)]
struct Snapshot(BTreeMap<String, EntrySnapshot>);

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(Snapshot(entries))
}

fn walk(root: &Path, path: &Path, entries: &mut BTreeMap<String, EntrySnapshot>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let relative = path
        .strip_prefix(root)
        .expect("walked path remains below scratch root")
        .to_string_lossy()
        .into_owned();
    let kind = classify(metadata.file_type());
    let content = (kind == Kind::File).then(|| fs::read(path)).transpose()?;
    let target = (kind == Kind::Symlink)
        .then(|| fs::read_link(path).map(|target| target.into_os_string().into_vec()))
        .transpose()?;
    entries.insert(
        relative,
        EntrySnapshot {
            kind,
            mode: metadata.permissions().mode() & 0o7777,
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.size(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            content,
            target,
        },
    );
    if kind == Kind::Directory {
        let mut names = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        for name in names {
            walk(root, &path.join(name), entries)?;
        }
    }
    Ok(())
}

fn classify(file_type: fs::FileType) -> Kind {
    if file_type.is_file() {
        Kind::File
    } else if file_type.is_dir() {
        Kind::Directory
    } else if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_fifo() {
        Kind::Fifo
    } else {
        Kind::Other
    }
}

fn run_source_diff(directory: &Path, path: &Path) -> io::Result<Output> {
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["source", "diff"])
        .arg(path)
        .env_clear()
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_bounded(child)
}

fn wait_bounded(mut child: Child) -> io::Result<Output> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "orna source diff did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("captured stdout")
        .read_to_end(&mut stdout)?;
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_end(&mut stderr)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn runs_as_the_orna_account() -> bool {
    let Ok(Some(orna)) = nix::unistd::User::from_name("orna") else {
        return false;
    };
    nix::unistd::getuid() == orna.uid
        && nix::unistd::geteuid() == orna.uid
        && nix::unistd::getgid() == orna.gid
        && nix::unistd::getegid() == orna.gid
}

fn assert_read_failure(output: &Output, expected_stderr: &[u8]) {
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "source diff must emit no stdout: {output:?}"
    );
    assert_eq!(output.stderr, expected_stderr);
}

fn assert_snapshot_unchanged(root: &Path, before: &Snapshot) {
    assert_eq!(
        &snapshot(root).expect("snapshot scratch after invocation"),
        before,
        "source diff invocation must leave the scratch directory unchanged"
    );
}

#[test]
fn source_read_rejections_fail_closed_before_host() {
    let directory = TestDirectory::new("read").expect("scratch directory");
    let missing = directory.path().join("missing.orna");
    let as_directory = directory.path().join("directory.orna");
    fs::create_dir(&as_directory).expect("create directory source");
    let dangling = directory.path().join("dangling.orna");
    symlink(directory.path().join("gone.orna"), &dangling).expect("create dangling symlink");
    let fifo = directory.path().join("pipe.orna");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("create source fifo");
    let invalid = directory.write("invalid.orna", b"CREATE SCHEMA app;\xff");
    let linked_invalid = directory.path().join("linked-invalid.orna");
    symlink(&invalid, &linked_invalid).expect("create regular-file symlink");
    let before = snapshot(directory.path()).expect("snapshot arranged scratch");

    let expected_read_error = b"orna: could not read source file\n";
    assert_read_failure(
        &run_source_diff(directory.path(), &missing).expect("bounded missing source"),
        expected_read_error,
    );
    assert_snapshot_unchanged(directory.path(), &before);
    assert_read_failure(
        &run_source_diff(directory.path(), &dangling).expect("bounded dangling source"),
        expected_read_error,
    );
    assert_snapshot_unchanged(directory.path(), &before);
    assert_read_failure(
        &run_source_diff(directory.path(), &as_directory).expect("bounded directory source"),
        expected_read_error,
    );
    assert_snapshot_unchanged(directory.path(), &before);
    assert_read_failure(
        &run_source_diff(directory.path(), &fifo).expect("bounded fifo source"),
        expected_read_error,
    );
    assert_snapshot_unchanged(directory.path(), &before);

    let expected_utf8_error = b"orna: source file is not valid UTF-8\n";
    assert_read_failure(
        &run_source_diff(directory.path(), &invalid).expect("bounded invalid UTF-8 source"),
        expected_utf8_error,
    );
    assert_read_failure(
        &run_source_diff(directory.path(), &linked_invalid).expect("bounded linked invalid source"),
        expected_utf8_error,
    );
    assert_snapshot_unchanged(directory.path(), &before);
    assert_eq!(
        snapshot(directory.path()).expect("snapshot scratch after read failures"),
        before,
        "source read failures must leave the scratch directory unchanged"
    );
}

#[test]
fn valid_v1_source_reaches_service_account_boundary() {
    if runs_as_the_orna_account() {
        eprintln!("skipping service-account boundary: suite runs as the orna account");
        return;
    }
    let directory = TestDirectory::new("service-account").expect("scratch directory");
    let valid = directory.write("valid.orna", VALID_V1_SOURCE);
    let before = snapshot(directory.path()).expect("snapshot valid source scratch");
    let output = run_source_diff(directory.path(), &valid).expect("bounded valid source");
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(output.stdout.is_empty(), "host failure must emit no stdout");
    assert_eq!(
        output.stderr,
        b"orna: source diff must run as the orna service account\n"
    );
    assert_eq!(
        snapshot(directory.path()).expect("snapshot scratch after host rejection"),
        before,
        "service-account rejection must leave the scratch directory unchanged"
    );
}
