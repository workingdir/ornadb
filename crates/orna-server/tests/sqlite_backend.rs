//! Focused process-boundary coverage for the local SQLite backend.
//!
//! Every command uses a caller-owned scratch path below `target/`, clears the
//! environment, and drives the compiled `orna` binary. The socket checks use a
//! private Unix socket derived from that scratch database path; they never
//! contact the managed `/run/orna` endpoint.

#![cfg(unix)]

use orna_protocol::{ClientFrame, ServerFrame, decode_server_frame, encode_client_frame};
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_HEADER_LENGTH: usize = 18;
const V1_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x01\x00\x00\x00\x00";
const V1_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x01\x00\x00\x00\x00";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const SERVER_FUNCTION_FIXTURE: &[u8] = include_bytes!("fixtures/server_function_dogfood.orna");

/// A caller-owned scratch directory below the repository `target/` directory.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "sqlite-backend-test-{}-{}-{label}",
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

fn socket_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(".orna.sock");
    PathBuf::from(path)
}

fn spawn_orna(directory: &Path, arguments: &[OsString]) -> io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_orna(directory: &Path, arguments: &[OsString]) -> io::Result<Output> {
    wait_bounded(spawn_orna(directory, arguments)?)
}

fn wait_bounded(mut child: Child) -> io::Result<Output> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.try_wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "orna SQLite process did not exit",
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

/// A foreground SQLite server whose drop path always kills the child boundedly.
struct RunningServer {
    child: Option<Child>,
}

impl RunningServer {
    fn spawn(directory: &Path, database: &Path) -> io::Result<Self> {
        let arguments = vec![
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("server"),
            OsString::from("run"),
        ];
        Ok(Self {
            child: Some(spawn_orna(directory, &arguments)?),
        })
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("running server child")
    }

    fn wait_for_socket(&mut self, path: &Path) -> io::Result<()> {
        wait_for_socket(self.child_mut(), path)
    }

    fn stop(mut self) -> io::Result<Output> {
        let mut child = self.child.take().expect("running server child");
        let _ = child.kill();
        wait_bounded(child)
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.kill();
        let _ = wait_bounded(child);
    }
}

fn wait_for_socket(child: &mut Child, path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "orna SQLite server exited before socket readiness: {status}"
            )));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                if UnixStream::connect(path).is_ok() {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "orna SQLite server socket did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn connect_socket(path: &Path) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT))?;
    Ok(stream)
}

fn complete_v1_handshake(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(&V1_HELLO)?;
    stream.flush()?;
    let mut acknowledgement = [0_u8; V1_ACK.len()];
    stream.read_exact(&mut acknowledgement)?;
    assert_eq!(acknowledgement, V1_ACK, "v1 handshake acknowledgement");
    Ok(())
}

fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut header = [0_u8; FRAME_HEADER_LENGTH];
    stream.read_exact(&mut header)?;
    let payload_length = u32::from_be_bytes(
        header[14..18]
            .try_into()
            .expect("frame header length is fixed"),
    ) as usize;
    if payload_length > orna_protocol::MAX_FRAME_PAYLOAD_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server frame payload exceeds protocol limit",
        ));
    }
    let mut frame = header.to_vec();
    frame.resize(FRAME_HEADER_LENGTH + payload_length, 0);
    stream.read_exact(&mut frame[FRAME_HEADER_LENGTH..])?;
    Ok(frame)
}

fn source_arguments(database: &Path, command: &str, source: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--db"),
        database.as_os_str().to_os_string(),
        OsString::from("source"),
        OsString::from(command),
        source.as_os_str().to_os_string(),
    ]
}

#[test]
fn local_source_apply_emits_json_and_same_source_diff_is_empty() {
    let directory = TestDirectory::new("source").expect("scratch directory");
    let database = directory.path().join("server-functions.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy server function fixture");

    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("bounded source apply");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    assert!(
        applied.stderr.is_empty(),
        "successful source apply must not write stderr: {:?}",
        applied.stderr
    );
    assert!(
        applied.stdout.ends_with(b"\n"),
        "source apply must emit one newline-terminated JSON document"
    );
    let document: Value =
        serde_json::from_slice(&applied.stdout).expect("source apply output must be JSON");
    let object = document
        .as_object()
        .expect("source apply JSON document must be an object");
    assert!(
        object
            .get("source_revision")
            .and_then(Value::as_str)
            .is_some_and(|revision| !revision.is_empty()),
        "source apply JSON must identify its source revision"
    );
    assert!(
        object
            .get("catalogue_revision")
            .and_then(Value::as_str)
            .is_some_and(|revision| !revision.is_empty()),
        "source apply JSON must identify its catalogue revision"
    );
    let functions = object
        .get("functions")
        .and_then(Value::as_array)
        .expect("source apply JSON must contain a functions array");
    for function in ["read", "distinct_values", "stream", "read_item", "update"] {
        assert!(
            functions.iter().any(|entry| {
                entry.get("qualified_name")
                    == Some(&Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String(function.to_owned()),
                    ]))
            }),
            "source apply JSON must include dogfood.{function}"
        );
    }

    let diff = run_orna(
        directory.path(),
        &source_arguments(&database, "diff", &source),
    )
    .expect("bounded source diff");
    assert_eq!(diff.status.code(), Some(0), "source diff: {diff:?}");
    assert!(
        diff.stderr.is_empty(),
        "successful source diff must not write stderr: {:?}",
        diff.stderr
    );
    let diff_text = String::from_utf8(diff.stdout).expect("source diff output is UTF-8");
    assert!(
        diff_text.starts_with("semantic diff "),
        "source diff must identify the compared revisions: {diff_text:?}"
    );
    assert_eq!(
        diff_text.lines().last(),
        Some("no semantic changes"),
        "same source after apply must report no semantic changes"
    );
    assert_eq!(
        diff_text.lines().count(),
        2,
        "empty semantic diff has two lines"
    );
}

#[test]
fn local_sqlite_server_has_private_v1_socket_and_ping_pong() {
    let directory = TestDirectory::new("socket").expect("scratch directory");
    let database = directory.path().join("socket.sqlite");
    let socket = socket_path(&database);
    let mut server = RunningServer::spawn(directory.path(), &database).expect("SQLite server");
    server
        .wait_for_socket(&socket)
        .expect("bounded socket readiness");

    let metadata = fs::symlink_metadata(&socket).expect("socket metadata");
    assert!(
        metadata.file_type().is_socket(),
        "socket path must be a Unix socket"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o7777,
        0o600,
        "SQLite socket must be owner-only"
    );

    let mut stream = connect_socket(&socket).expect("connect SQLite socket");
    complete_v1_handshake(&mut stream).expect("complete v1 handshake");
    let token = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];
    let ping = encode_client_frame(&ClientFrame::Ping { token }).expect("encode v1 Ping");
    stream.write_all(&ping).expect("write v1 Ping");
    stream.flush().expect("flush v1 Ping");
    let pong = read_frame(&mut stream).expect("read v1 Pong");
    assert_eq!(
        decode_server_frame(&pong).expect("decode v1 Pong"),
        ServerFrame::Pong { token },
        "SQLite socket must echo one valid v1 Ping token as Pong"
    );
    drop(stream);
}

#[test]
fn sqlite_socket_preserves_regular_occupants_stale_socket_replacement_and_live_server() {
    let directory = TestDirectory::new("socket-safety").expect("scratch directory");
    let database = directory.path().join("safety.sqlite");
    let socket = socket_path(&database);
    let sentinel = b"caller-owned socket path";
    fs::write(&socket, sentinel).expect("create regular socket occupant");

    let occupied = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("server"),
            OsString::from("run"),
        ],
    )
    .expect("bounded occupied-path server");
    assert_eq!(
        occupied.status.code(),
        Some(1),
        "occupied path: {occupied:?}"
    );
    assert!(
        String::from_utf8_lossy(&occupied.stderr)
            .contains("socket path is occupied by a non-socket"),
        "occupied path must report a closed non-socket error: {:?}",
        occupied.stderr
    );
    assert_eq!(
        fs::read(&socket).expect("regular occupant remains"),
        sentinel
    );
    fs::remove_file(&socket).expect("remove regular occupant");

    let stale_listener = UnixListener::bind(&socket).expect("create stale Unix socket");
    drop(stale_listener);
    assert!(
        fs::symlink_metadata(&socket)
            .expect("stale socket metadata")
            .file_type()
            .is_socket(),
        "stale path must be a socket before replacement"
    );
    let mut stale_server =
        RunningServer::spawn(directory.path(), &database).expect("replace stale socket");
    stale_server
        .wait_for_socket(&socket)
        .expect("bounded stale socket replacement readiness");
    let mut stale_stream = connect_socket(&socket).expect("connect replacement server");
    complete_v1_handshake(&mut stale_stream).expect("replacement server v1 handshake");
    drop(stale_stream);
    let _ = stale_server.stop().expect("bounded stale server shutdown");

    let mut live_server = RunningServer::spawn(directory.path(), &database).expect("live server");
    live_server
        .wait_for_socket(&socket)
        .expect("bounded live socket readiness");
    let contender =
        run_orna_async_child(directory.path(), &database).expect("spawn live-server contender");
    let contender_output = wait_bounded(contender).expect("bounded live-server contender");
    assert_eq!(
        contender_output.status.code(),
        Some(1),
        "a second server must reject the live socket: {contender_output:?}"
    );
    assert!(
        String::from_utf8_lossy(&contender_output.stderr)
            .contains("socket already has a live server"),
        "live socket rejection must identify the live server: {:?}",
        contender_output.stderr
    );
    assert!(
        socket.exists(),
        "rejecting a second server must not unlink the live socket"
    );

    let mut live_stream = connect_socket(&socket).expect("live socket remains connectable");
    complete_v1_handshake(&mut live_stream).expect("live server remains responsive");
    drop(live_stream);
    let _ = live_server.stop().expect("bounded live server shutdown");
}

fn run_orna_async_child(directory: &Path, database: &Path) -> io::Result<Child> {
    spawn_orna(
        directory,
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("server"),
            OsString::from("run"),
        ],
    )
}
