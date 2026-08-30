#![cfg(unix)]

mod support;

use nix::pty::openpty;
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read},
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const TERMINAL_REQUIRED: &[u8] = b"orna: backend-shell must be run in an interactive terminal\n";
const INSTANCE_INVALID: &[u8] = b"orna: the default Orna instance is invalid\n";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum NonTerminalStream {
    Stdin,
    Stdout,
    Stderr,
}

struct PtyOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "backend-shell-test-{}-{}-{label}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_without_terminal(arguments: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .output()
        .expect("orna process starts")
}

fn run_in_pty(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    directory: &Path,
    non_terminal: Option<NonTerminalStream>,
) -> io::Result<PtyOutput> {
    let stdin_pty = openpty(None, None).map_err(io::Error::other)?;
    let stdout_pty = openpty(None, None).map_err(io::Error::other)?;
    let stderr_pty = openpty(None, None).map_err(io::Error::other)?;
    let stdin_master = File::from(stdin_pty.master);
    let mut stdout_master = File::from(stdout_pty.master);
    let mut stderr_master = File::from(stderr_pty.master);
    let stdin_slave = File::from(stdin_pty.slave);
    let stdout_slave = File::from(stdout_pty.slave);
    let stderr_slave = File::from(stderr_pty.slave);

    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .args(["server", "backend-shell"])
        .env_clear()
        .envs(environment)
        .current_dir(directory)
        .stdin(match non_terminal {
            Some(NonTerminalStream::Stdin) => Stdio::null(),
            _ => Stdio::from(stdin_slave.try_clone()?),
        })
        .stdout(match non_terminal {
            Some(NonTerminalStream::Stdout) => Stdio::piped(),
            _ => Stdio::from(stdout_slave.try_clone()?),
        })
        .stderr(match non_terminal {
            Some(NonTerminalStream::Stderr) => Stdio::piped(),
            _ => Stdio::from(stderr_slave.try_clone()?),
        });
    let mut child = command.spawn()?;
    drop(command);
    drop(stdin_slave);
    drop(stdout_slave);
    drop(stderr_slave);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let status = wait_bounded(&mut child)?;
    let stdout = match stdout {
        Some(stdout) => read_pipe(stdout)?,
        None => normalise_terminal_output(read_pty(&mut stdout_master)?),
    };
    let stderr = match stderr {
        Some(stderr) => read_pipe(stderr)?,
        None => normalise_terminal_output(read_pty(&mut stderr_master)?),
    };
    drop(stdin_master);
    Ok(PtyOutput {
        status,
        stdout,
        stderr,
    })
}

fn wait_bounded(child: &mut Child) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "orna test process did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_pty(master: &mut File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => return Ok(bytes),
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) if error.raw_os_error() == Some(5) => return Ok(bytes),
            Err(error) => return Err(error),
        }
    }
}

fn normalise_terminal_output(bytes: Vec<u8>) -> Vec<u8> {
    bytes
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            if pair == b"\r\n" {
                None
            } else {
                Some(bytes[index])
            }
        })
        .chain(bytes.last().copied())
        .collect()
}

fn assert_usage(output: &Output) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, support::EXPECTED_USAGE);
}

#[test]
fn invalid_server_command_shapes_have_exact_process_results() {
    for arguments in [
        vec![OsString::from("server")],
        vec![
            OsString::from("server"),
            OsString::from("backend-shell"),
            OsString::from("--command"),
        ],
        vec![
            OsString::from("server"),
            OsString::from("backend-shell"),
            OsString::from("select 1"),
        ],
    ] {
        assert_usage(&run_without_terminal(arguments));
    }
}

#[test]
fn non_unicode_command_tokens_are_usage_errors() {
    assert_usage(&run_without_terminal([
        OsString::from("server"),
        OsString::from_vec(b"backend-shell\xff".to_vec()),
    ]));
}

#[test]
fn each_standard_stream_must_be_a_terminal_first() {
    let directory = TestDirectory::new("terminal").expect("test directory");
    for stream in [
        NonTerminalStream::Stdin,
        NonTerminalStream::Stdout,
        NonTerminalStream::Stderr,
    ] {
        let output = run_in_pty([], &directory.0, Some(stream)).expect("shell process");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, TERMINAL_REQUIRED);
    }
}

#[test]
fn hostile_client_environment_cannot_select_a_program_or_connection() {
    let directory = TestDirectory::new("hostile-environment").expect("test directory");
    let bin = directory.0.join("bin");
    let marker = directory.0.join("psql-was-run");
    fs::create_dir(&bin).expect("fake bin");
    let fake_psql = bin.join("psql");
    fs::write(
        &fake_psql,
        format!("#!/bin/sh\n: > '{}'\n", marker.display()),
    )
    .expect("fake psql");
    fs::set_permissions(&fake_psql, fs::Permissions::from_mode(0o700)).expect("fake psql mode");
    let output = run_in_pty(
        [
            (OsString::from("PATH"), bin.into_os_string()),
            (
                OsString::from("ORNA_SERVER_POSTGRES_URL"),
                OsString::from("postgresql://hostile@elsewhere:5432/wrong"),
            ),
            (OsString::from("PGHOST"), OsString::from("elsewhere")),
            (OsString::from("PGPASSWORD"), OsString::from("secret")),
            (OsString::from("HOME"), directory.0.clone().into_os_string()),
        ],
        &directory.0,
        None,
    )
    .expect("shell process");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, INSTANCE_INVALID);
    assert!(!marker.exists());
}
