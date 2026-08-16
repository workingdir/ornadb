//! Behaviour tests for the installed `orna state get|set` command boundary.
//!
//! These tests drive the compiled `orna` binary via `env!("CARGO_BIN_EXE_orna")`
//! with a cleared environment and a caller-owned scratch directory below
//! `target/`. They assert observable process output and exit codes. No
//! instance or database is started, so every test stays fail-closed without
//! Docker.
//!
//! What is proved: the listed representative malformed command shapes
//! produce the exact global usage, and one valid `get` and one valid `set`
//! shape reach the wrong-service-identity host boundary with the exact
//! closed diagnostic. What is not proved here: the live load/write success
//! path, the principal derivation from an authenticated local peer, and the
//! ORNA0901/ORNA0902 closed outcomes. Those require the installed product
//! with a live embedded instance (ADR 0061 step 6).

#![cfg(unix)]

mod support;

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use orna_core::FunctionId;

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
            "user-state-test-{}-{}-{label}",
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
                "orna state did not exit",
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
fn malformed_state_shapes_all_fail_closed_with_exact_usage() {
    let directory = TestDirectory::new("usage").expect("scratch directory");
    let canonical = FunctionId::from_bytes([0x11; 16]).canonical();
    let cases = [
        vec![OsString::from("state")],
        vec![OsString::from("state"), OsString::from("get")],
        vec![
            OsString::from("state"),
            OsString::from("get"),
            OsString::from("not-an-id"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("get"),
            OsString::from(&canonical),
            OsString::from("extra"),
        ],
        vec![OsString::from("state"), OsString::from("set")],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--function"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--revision"),
            OsString::from("seven"),
        ],
        vec![
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--value-file"),
            OsString::from("missing.bin"),
        ],
        vec![OsString::from("state"), OsString::from("dump")],
        vec![OsString::from_vec(b"state\xff".to_vec())],
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
    }
}

#[test]
fn valid_state_shapes_reach_the_service_account_boundary() {
    if runs_as_the_orna_account() {
        eprintln!("skipping service-account boundary: suite runs as the orna account");
        return;
    }
    let directory = TestDirectory::new("service-account").expect("scratch directory");
    let canonical = FunctionId::from_bytes([0x44; 16]).canonical();
    let slot = orna_core::StateSlotId::from_bytes([0x45; 16]).canonical();
    let value_type = orna_core::TypeId::from_bytes([0x46; 16]).canonical();
    let value_file = directory.path().join("value.bin");
    fs::write(&value_file, [0x0a, 0x0b]).expect("state value file must write");

    let get = run_orna(
        directory.path(),
        &[
            OsString::from("state"),
            OsString::from("get"),
            OsString::from(&canonical),
        ],
    )
    .expect("bounded invocation");
    assert_eq!(get.status.code(), Some(7), "status: {get:?}");
    assert!(
        get.stdout.is_empty(),
        "service-account failure must emit no standard output or prompt"
    );
    assert_eq!(
        get.stderr,
        b"orna state: the installed Orna instance is not available: Orna service identity is invalid\n",
        "service-account failure must print the exact public diagnostic"
    );

    let set = run_orna(
        directory.path(),
        &[
            OsString::from("state"),
            OsString::from("set"),
            OsString::from(&canonical),
            OsString::from("--function"),
            OsString::from(&canonical),
            OsString::from("--slot"),
            OsString::from(&slot),
            OsString::from("--revision"),
            OsString::from("create"),
            OsString::from("--type"),
            OsString::from(&value_type),
            OsString::from("--value-file"),
            value_file.into_os_string(),
        ],
    )
    .expect("bounded invocation");
    assert_eq!(set.status.code(), Some(7), "status: {set:?}");
    assert!(
        set.stdout.is_empty(),
        "service-account failure must emit no standard output or prompt"
    );
    assert_eq!(
        set.stderr,
        b"orna state: the installed Orna instance is not available: Orna service identity is invalid\n",
        "service-account failure must print the exact public diagnostic"
    );
}
