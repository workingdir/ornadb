//! Behaviour tests for the installed `orna security grant-execute` command
//! boundary.
//!
//! These tests drive the compiled `orna` binary via `env!("CARGO_BIN_EXE_orna")`
//! with a cleared environment and a caller-owned scratch directory below
//! `target/`. They assert observable process output, exit codes, and an
//! exact before/after deep snapshot of the scratch directory. No instance or
//! database is started, so every test stays fail-closed without Docker.
//!
//! What is proved: the listed representative malformed command shapes
//! produce the exact global usage and leave the caller-owned scratch tree
//! unchanged, and one valid canonical identity reaches the wrong-service-
//! identity host boundary with the exact closed diagnostic. What is not
//! proved here: the live grant success path, the absence of database or
//! security-row changes, and hostile endpoint-environment rejection. Those
//! require the installed product with a live embedded instance and are not
//! claimed by this file.

#![cfg(unix)]

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use orna_core::FunctionId;

const USAGE: &[u8] = b"Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna security grant-execute <canonical-function-id>\n  orna raw-call <canonical-function-id>\n";
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
            "security-admin-test-{}-{}-{label}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
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
/// owner, link count, length, mtime and ctime with nanosecond precision, and
/// either the file bytes or the symlink target.
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
struct Snapshot(std::collections::BTreeMap<String, EntrySnapshot>);

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut entries = std::collections::BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(Snapshot(entries))
}

fn walk(
    root: &Path,
    path: &Path,
    entries: &mut std::collections::BTreeMap<String, EntrySnapshot>,
) -> io::Result<()> {
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
    stdin: Stdio,
) -> io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
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
        Stdio::null(),
    )?)
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
                "orna security grant-execute did not exit",
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
/// The guard exists so the valid-identity boundary test can never reach a
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

#[test]
fn malformed_command_shapes_all_fail_closed_with_exact_usage() {
    let directory = TestDirectory::new("usage").expect("scratch directory");
    let before = snapshot(directory.path()).expect("snapshot empty scratch");
    let canonical = FunctionId::from_bytes([0x33; 16]).canonical();
    let cases = [
        vec![],
        vec![OsString::new()],
        vec![OsString::from("security")],
        vec![OsString::from("security"), OsString::from("grant-execute")],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::new(),
        ],
        vec![OsString::from("security"), OsString::from("grant")],
        vec![OsString::from("security"), OsString::from("revoke-execute")],
        vec![OsString::from("security"), OsString::from("grant-executee")],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from("source-revision:0123456789abcdefghjkmnpqrstvwxy"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from("function:0123456789abcdefghjkmnpqrst"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from("function:deadbeef"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from("function:ABCDEFGHIJKLMNOPQRSTUVWXYZ12"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from(format!("{canonical} ")),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from("sys.catalog.health"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from(&canonical),
            OsString::from("extra"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from(&canonical),
            OsString::from("--force"),
        ],
        vec![
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from_vec(b"function:\xff".to_vec()),
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
            output.stderr, USAGE,
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
fn valid_canonical_identity_reaches_the_service_account_boundary() {
    if runs_as_the_orna_account() {
        eprintln!("skipping service-account boundary: suite runs as the orna account");
        return;
    }
    let directory = TestDirectory::new("service-account").expect("scratch directory");
    let before = snapshot(directory.path()).expect("snapshot empty scratch");
    let canonical = FunctionId::from_bytes([0x44; 16]).canonical();
    let output = run_orna(
        directory.path(),
        &[
            OsString::from("security"),
            OsString::from("grant-execute"),
            OsString::from(canonical),
        ],
    )
    .expect("bounded invocation");
    assert_eq!(output.status.code(), Some(1), "status: {output:?}");
    assert!(
        output.stdout.is_empty(),
        "service-account failure must emit no standard output or prompt"
    );
    assert_eq!(
        output.stderr, b"orna: security grant-execute must run as the orna service account\n",
        "service-account failure must print the exact public diagnostic"
    );
    let after = snapshot(directory.path()).expect("snapshot scratch after invocation");
    assert_eq!(
        after, before,
        "the command must leave the scratch directory unchanged"
    );
}
