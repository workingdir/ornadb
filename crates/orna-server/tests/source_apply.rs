//! Behaviour tests for the installed `orna source apply` command boundary.
//!
//! These tests drive the compiled `orna` binary via `env!("CARGO_BIN_EXE_orna")`
//! with a cleared environment and a caller-owned scratch directory below
//! `target/`. They assert observable process output, exit codes, and an
//! exact before/after deep snapshot of the scratch directory. No instance or
//! database is started, so every test stays fail-closed without Docker.
//!
//! Hostile endpoint-environment authority is not tested here. The installed
//! product scenario exercises the real service identity under the packaged
//! executable.

#![cfg(unix)]

mod support;

use nix::{sys::stat::Mode, unistd::mkfifo};
use std::{
    collections::BTreeMap,
    ffi::OsString,
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

const VALID_SOURCE: &[u8] =
    b"CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A caller-owned scratch directory below the repository `target/` directory.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "source-apply-test-{}-{}-{label}",
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

/// The entry kind of one snapshot entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    File,
    Directory,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
    Other,
}

/// One deep snapshot entry for one path below the scratch root.
///
/// The snapshot captures the metadata fields that detect content
/// modification, metadata changes, and same-name replacement: kind, mode,
/// owner, link count, length, mtime, ctime, and either the file bytes or the
/// symlink target.
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

/// A recursive, sorted deep snapshot of one scratch directory tree.
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
        .expect("walked path remains below the scratch root")
        .to_string_lossy()
        .into_owned();
    let kind = classify(metadata.file_type());
    let mut content = None;
    let mut target = None;
    if kind == Kind::File {
        content = Some(fs::read(path)?);
    } else if kind == Kind::Symlink {
        target = Some(fs::read_link(path)?.into_os_string().into_vec());
    }
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
    } else if file_type.is_socket() {
        Kind::Socket
    } else if file_type.is_block_device() {
        Kind::BlockDevice
    } else if file_type.is_char_device() {
        Kind::CharDevice
    } else {
        Kind::Other
    }
}

fn spawn_orna(
    directory: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    stdin: Stdio,
) -> io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .envs(environment)
        .current_dir(directory)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_orna(directory: &Path, arguments: &[OsString]) -> io::Result<Output> {
    wait_bounded(spawn_orna(
        directory,
        arguments.iter().cloned(),
        [],
        Stdio::null(),
    )?)
}

fn run_source_apply(directory: &Path, path: impl Into<OsString>) -> io::Result<Output> {
    run_orna(
        directory,
        &[
            OsString::from("source"),
            OsString::from("apply"),
            path.into(),
        ],
    )
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
                "orna source apply did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = read_pipe(child.stdout.take().expect("captured stdout"))?;
    let stderr = read_pipe(child.stderr.take().expect("captured stderr"))?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether this test process runs with the exact installed `orna` account.
///
/// The guard exists so the wrong-identity boundary test can never apply to a
/// real default database when the suite itself runs as the service account.
/// If the account is absent or the lookup fails, the boundary may proceed.
fn runs_as_the_orna_account() -> bool {
    let Ok(Some(orna)) = nix::unistd::User::from_name("orna") else {
        return false;
    };
    nix::unistd::getuid() == orna.uid
        && nix::unistd::geteuid() == orna.uid
        && nix::unistd::getgid() == orna.gid
        && nix::unistd::getegid() == orna.gid
}

fn require_read_rejection(output: &Output, path: &Path) {
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "rejected source apply must emit no standard output, got {:?}",
        output.stdout
    );
    let expected = format!("orna: could not read source file: {}\n", path.display());
    assert_eq!(
        output.stderr,
        expected.as_bytes(),
        "rejected source apply must print the exact public read line"
    );
}

#[test]
fn usage_shape_failures_all_fail_closed_with_exact_usage() {
    let directory = TestDirectory::new("usage").expect("scratch directory");
    let before = snapshot(directory.path()).expect("snapshot empty scratch");
    let cases = [
        vec![],
        vec![OsString::new()],
        vec![OsString::from("source")],
        vec![OsString::from("source"), OsString::from("apply")],
        vec![
            OsString::from("source"),
            OsString::from("chek"),
            OsString::from("app.orna"),
        ],
        vec![
            OsString::from("--source"),
            OsString::from("apply"),
            OsString::from("app.orna"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from(""),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("-"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("--"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("app.orna"),
            OsString::from("second.orna"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("app.orna"),
            OsString::from("--extra"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("-leading.orna"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("line\nbreak.orna"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from("line\u{2028}break.orna"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("apply"),
            OsString::from_vec(b"app\xff.orna".to_vec()),
        ],
    ];
    for arguments in cases {
        let output = run_orna(directory.path(), &arguments).expect("bounded invocation");
        assert_eq!(
            output.status.code(),
            Some(2),
            "arguments {arguments:?} must exit 2, got {output:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "arguments {arguments:?} must emit no standard output"
        );
        assert_eq!(
            output.stderr,
            support::EXPECTED_USAGE,
            "arguments {arguments:?} must print the exact global usage"
        );
        let after = snapshot(directory.path()).expect("snapshot scratch after invocation");
        assert_eq!(
            after, before,
            "arguments {arguments:?} must leave the scratch directory unchanged"
        );
    }
}

#[test]
fn missing_and_non_regular_sources_fail_closed() {
    let directory = TestDirectory::new("read").expect("scratch directory");

    let missing = directory.path().join("missing.orna");
    let as_directory = directory.path().join("directory.orna");
    fs::create_dir(&as_directory).expect("create scratch directory entry");
    let dangling = directory.path().join("dangling.orna");
    symlink(directory.path().join("gone.orna"), &dangling).expect("create dangling symlink");
    let fifo = directory.path().join("pipe.orna");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("create scratch fifo");

    let before = snapshot(directory.path()).expect("snapshot arranged scratch");

    require_read_rejection(
        &run_source_apply(directory.path(), &missing).expect("bounded missing source"),
        &missing,
    );
    require_read_rejection(
        &run_source_apply(directory.path(), &as_directory).expect("bounded directory source"),
        &as_directory,
    );
    require_read_rejection(
        &run_source_apply(directory.path(), &dangling).expect("bounded symlink source"),
        &dangling,
    );
    require_read_rejection(
        &run_source_apply(directory.path(), &fifo).expect("bounded fifo source"),
        &fifo,
    );

    let after = snapshot(directory.path()).expect("snapshot scratch after invocations");
    assert_eq!(
        after, before,
        "read failures must leave the scratch directory unchanged"
    );
}

#[test]
fn invalid_utf8_source_fails_closed() {
    let directory = TestDirectory::new("utf8").expect("scratch directory");
    let invalid = directory.write("invalid.orna", b"CREATE SCHEMA \xff;");
    let before = snapshot(directory.path()).expect("snapshot arranged scratch");
    let output =
        run_source_apply(directory.path(), &invalid).expect("bounded invalid UTF-8 source");
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "invalid UTF-8 must emit no standard output"
    );
    let expected = format!(
        "orna: source file is not valid UTF-8: {}\n",
        invalid.display()
    );
    assert_eq!(
        output.stderr,
        expected.as_bytes(),
        "invalid UTF-8 must print the exact public diagnostic"
    );
    let after = snapshot(directory.path()).expect("snapshot scratch after invocation");
    assert_eq!(
        after, before,
        "invalid UTF-8 must leave the scratch directory unchanged"
    );
}

#[test]
fn valid_source_is_read_then_fails_at_the_service_account_boundary() {
    if runs_as_the_orna_account() {
        eprintln!("skipping service-account boundary: suite runs as the orna account");
        return;
    }
    let directory = TestDirectory::new("service-account").expect("scratch directory");
    let valid = directory.write("valid.orna", VALID_SOURCE);
    let before = snapshot(directory.path()).expect("snapshot arranged scratch");
    let output = run_source_apply(directory.path(), &valid).expect("bounded invocation");
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "service-account failure must emit no standard output"
    );
    assert_eq!(
        output.stderr, b"orna: source apply must run as the orna service account\n",
        "service-account failure must print the exact public diagnostic"
    );
    let after = snapshot(directory.path()).expect("snapshot scratch after invocation");
    assert_eq!(
        after, before,
        "the command must leave the scratch directory unchanged"
    );
}
