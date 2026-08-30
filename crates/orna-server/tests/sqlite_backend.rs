//! Focused process-boundary coverage for the local SQLite backend.
//!
//! Every command uses a caller-owned scratch path below `target/`, clears the
//! environment, and drives the compiled `orna` binary. The socket checks use a
//! private Unix socket derived from that scratch database path; they never
//! contact the managed `/run/orna` endpoint.

#![cfg(unix)]

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use orna_core::{
    FunctionId, PrincipalId,
    revision::{ActiveDatabaseRevision, RevisionPair},
    state::{UserStateChange, UserStateWriteOutcome},
    value::RuntimeValue,
};
use orna_protocol::{
    ClientFrame, ServerFrame, decode_catalogue_server_frame, decode_server_frame,
    encode_catalogue_client_frame, encode_client_frame, encode_value,
};
use orna_server::{
    InstalledUserStateChange, InstalledUserStateErrorKind, InstalledUserStateOperation,
    InstalledUserStateRequest, run_sqlite_user_state,
};
use orna_sqlite::{SqliteConfig, SqliteRevisionStore};
use orna_standard::INTEGER_TYPE_ID;
use orna_storage::{ApplicationRevisionStore, MigrationLedgerEntry};
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
const V2_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x02\x00\x00\x00\x00";
const V2_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x02\x00\x00\x00\x00";
const UNSUPPORTED_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x06\x00\x00\x00\x00";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const SERVER_FUNCTION_FIXTURE: &[u8] = include_bytes!("fixtures/server_function_dogfood.orna");

/// A caller-owned scratch directory below the system temporary directory.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "orna-sqlite-test-{}-{}-{label}",
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
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)?;
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)?;
    }
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

    fn stop_with_signal(mut self, signal: Signal) -> io::Result<Output> {
        let mut child = self.child.take().expect("running server child");
        let pid = match i32::try_from(child.id()) {
            Ok(pid) => Pid::from_raw(pid),
            Err(_) => {
                let error = io::Error::other("SQLite server process id overflow");
                let _ = child.kill();
                let _ = wait_bounded(child);
                return Err(error);
            }
        };
        if let Err(error) = kill(pid, signal).map_err(io::Error::other) {
            let _ = child.kill();
            let _ = wait_bounded(child);
            return Err(error);
        }
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
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stderr.take() {
                pipe.read_to_end(&mut stderr)?;
            }
            return Err(io::Error::other(format!(
                "orna SQLite server exited before socket readiness: {status}: {}",
                String::from_utf8_lossy(&stderr)
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
fn complete_v2_handshake(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(&V2_HELLO)?;
    stream.flush()?;
    let mut acknowledgement = [0_u8; V2_ACK.len()];
    stream.read_exact(&mut acknowledgement)?;
    assert_eq!(acknowledgement, V2_ACK, "v2 handshake acknowledgement");
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
fn sqlite_revision_state(database: &Path) -> (RevisionPair, Vec<MigrationLedgerEntry>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("SQLite state runtime");
    runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(database.to_owned()))
            .await
            .expect("open SQLite state");
        let active = store.recover().await.expect("recover SQLite state");
        let ledger = store.read_ledger().await.expect("read SQLite ledger");
        (active.pair(), ledger)
    })
}
fn sqlite_active(database: &Path) -> ActiveDatabaseRevision {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("SQLite active revision runtime");
    runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(database.to_owned()))
            .await
            .expect("open SQLite active revision");
        store
            .recover()
            .await
            .expect("recover SQLite active revision")
    })
}

fn local_principal_and_latest_invocation(
    database: &Path,
) -> (
    orna_core::PrincipalId,
    orna_sqlite::SqliteInvocationAuditEvent,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("SQLite audit runtime");
    runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(database.to_owned()))
            .await
            .expect("open SQLite audit store");
        let active = store.recover().await.expect("recover SQLite audit store");
        let principal = store
            .provision_local_peer(nix::unistd::geteuid().as_raw())
            .await
            .expect("provision local audit principal");
        let _session = store
            .authenticate_local_peer(&active, nix::unistd::geteuid().as_raw())
            .await
            .expect("authenticate local audit principal");
        let audit = store
            .load_latest_invocation_audit(principal)
            .await
            .expect("load latest invocation audit")
            .expect("completed invocation audit");
        (principal, audit)
    })
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
fn local_source_diff_is_read_only_and_rejects_fresh_database() {
    let directory = TestDirectory::new("source-diff").expect("scratch directory");
    let database = directory.path().join("source-diff.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");

    let missing = run_orna(
        directory.path(),
        &source_arguments(&database, "diff", &source),
    )
    .expect("bounded fresh-database diff");
    assert_eq!(missing.status.code(), Some(1), "fresh diff: {missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("could not open SQLite database"),
        "fresh diff must report the missing database: {:?}",
        missing.stderr
    );
    assert!(
        !database.exists(),
        "fresh-file source diff must not create a database"
    );

    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("bounded source apply");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let before = sqlite_revision_state(&database);

    let reduced = directory.path().join("reduced.orna");
    fs::write(&reduced, b"CREATE SCHEMA dogfood;\n").expect("write reduced source");
    let reduced_diff = run_orna(
        directory.path(),
        &source_arguments(&database, "diff", &reduced),
    )
    .expect("bounded SQLite source diff");
    assert_eq!(
        reduced_diff.status.code(),
        Some(1),
        "unsupported physical source diff must fail closed: {reduced_diff:?}"
    );
    assert!(
        String::from_utf8_lossy(&reduced_diff.stderr)
            .contains("durable object drops are not supported"),
        "unsupported physical source diff must report the capability boundary: {:?}",
        reduced_diff.stderr
    );
    assert!(
        reduced_diff.stdout.is_empty(),
        "failed source diff must not render a partial semantic report"
    );
    assert_eq!(
        sqlite_revision_state(&database),
        before,
        "source diff must not mutate active state or migration ledger"
    );
}

#[test]
fn local_source_apply_diagnostics_leave_active_state_and_ledger_unchanged() {
    let directory = TestDirectory::new("source-diagnostics").expect("scratch directory");
    let database = directory.path().join("source-diagnostics.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");

    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("bounded source apply");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let before = sqlite_revision_state(&database);

    let invalid = directory.path().join("invalid.orna");
    fs::write(&invalid, b"CREATE SCHEMA ;\n").expect("write invalid source");
    let failed = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &invalid),
    )
    .expect("bounded diagnostic source apply");
    assert_eq!(
        failed.status.code(),
        Some(1),
        "diagnostic apply: {failed:?}"
    );
    assert!(
        failed.stdout.is_empty(),
        "diagnostic source apply must not emit a success document: {:?}",
        failed.stdout
    );
    assert!(
        !failed.stderr.is_empty(),
        "diagnostic source apply must report diagnostics"
    );
    assert_eq!(
        sqlite_revision_state(&database),
        before,
        "diagnostic source apply must not mutate active state or migration ledger"
    );
}

#[test]
fn local_sqlite_invoke_formats_results_and_persists_inspectable_evidence() {
    let directory = TestDirectory::new("invoke-inspect").expect("scratch directory");
    let database = directory.path().join("invoke-inspect.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for local invoke");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let applied_document: Value =
        serde_json::from_slice(&applied.stdout).expect("source apply JSON");
    let function = applied_document["functions"]
        .as_array()
        .and_then(|functions| {
            functions.iter().find(|entry| {
                entry["qualified_name"]
                    == Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String("read".to_owned()),
                    ])
            })
        })
        .and_then(|entry| entry["function_id"].as_str())
        .and_then(|canonical| FunctionId::from_canonical(canonical).ok())
        .expect("dogfood.read function identity");

    for (presenter, expected) in [
        ("json", b"[]\n".as_slice()),
        ("table", b"value\n".as_slice()),
        ("csv", b"value\n".as_slice()),
    ] {
        let result = run_orna(
            directory.path(),
            &[
                OsString::from("--db"),
                database.as_os_str().to_os_string(),
                OsString::from("invoke"),
                OsString::from("dogfood.read"),
                OsString::from("--output"),
                OsString::from(presenter),
                OsString::from("--no-progress"),
            ],
        )
        .expect("local SQLite invoke");
        assert_eq!(
            result.status.code(),
            Some(0),
            "invoke {presenter}: {result:?}"
        );
        assert_eq!(result.stdout, expected, "invoke {presenter} output");
        assert!(result.stderr.is_empty(), "no-progress invoke stderr");
    }

    let (_, audit) = local_principal_and_latest_invocation(&database);
    assert_eq!(audit.outcome, "completed");
    assert_eq!(audit.function, Some(function));
    let inspect = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("inspect"),
            OsString::from(audit.invocation.canonical()),
        ],
    )
    .expect("local SQLite inspect summary");
    assert_eq!(
        inspect.status.code(),
        Some(0),
        "inspect summary: {inspect:?}"
    );
    let summary: Value = serde_json::from_slice(&inspect.stdout).expect("inspect summary JSON");
    assert_eq!(summary["record"], "inspect_summary");
    assert_eq!(summary["function_id"], function.canonical());
    assert!(
        summary.get("results").is_none(),
        "inspect must not persist results"
    );

    let trace = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("inspect"),
            OsString::from(audit.invocation.canonical()),
            OsString::from("--trace"),
        ],
    )
    .expect("local SQLite inspect trace");
    assert_eq!(trace.status.code(), Some(0), "inspect trace: {trace:?}");
    let trace_record: Value = serde_json::from_slice(&trace.stdout).expect("inspect trace JSON");
    assert_eq!(trace_record["record"], "inspect_trace");
    assert_eq!(trace_record["function_id"], function.canonical());
}

#[test]
fn local_sqlite_user_state_writes_conflicts_and_reopens() {
    let directory = TestDirectory::new("user-state").expect("scratch directory");
    let database = directory.path().join("user-state.sqlite");
    let root = FunctionId::from_bytes([0x11; 16]);
    let function = FunctionId::from_bytes([0x22; 16]);
    let state_slot = orna_core::StateSlotId::from_bytes([0x33; 16]);
    let value = RuntimeValue::Integer(42);
    let value_bytes = encode_value(&value).expect("encode ORV5 integer");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("SQLite USER state runtime");
    let (first, conflict, cells, audit) = runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&database))
            .await
            .expect("open SQLite USER state store");
        ApplicationRevisionStore::bootstrap(&store)
            .await
            .expect("bootstrap SQLite USER state store");
        let principal = store
            .provision_local_peer(nix::unistd::geteuid().as_raw())
            .await
            .expect("provision USER state principal");
        let active = store.recover().await.expect("recover USER state revision");
        let security = store
            .security_snapshot(&active)
            .await
            .expect("recover USER state security");
        let session = security
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .expect("authenticate USER state principal");
        let change = UserStateChange::new(
            root,
            "profile".to_owned(),
            function,
            String::new(),
            state_slot,
            None,
            value.clone(),
            INTEGER_TYPE_ID,
        )
        .expect("construct USER state change");
        let first = store
            .write_user_state(&active, &security, &session, &change)
            .await
            .expect("first USER state write");
        let conflict = store
            .write_user_state(&active, &security, &session, &change)
            .await
            .expect("conflicting USER state write is a result");
        drop(store);
        let reopened = SqliteRevisionStore::open(&SqliteConfig::new(&database))
            .await
            .expect("reopen SQLite USER state store");
        let active = reopened
            .recover()
            .await
            .expect("recover reopened USER state revision");
        let security = reopened
            .security_snapshot(&active)
            .await
            .expect("recover reopened USER state security");
        let session = security
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .expect("authenticate reopened USER state principal");
        let cells = reopened
            .load_user_state(&active, &security, &session, root, "profile")
            .await
            .expect("load USER state after reopen");
        let audit = reopened
            .load_user_state_audit(principal, root, "profile")
            .await
            .expect("load USER state audit");
        (first, conflict, cells, audit)
    });
    assert!(matches!(
        first.outcome(),
        UserStateWriteOutcome::Written { revision: 1 }
    ));
    assert!(matches!(
        conflict.outcome(),
        UserStateWriteOutcome::Conflict {
            current_revision: 1
        }
    ));
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].value(), &value);
    assert_eq!(cells[0].value_type(), INTEGER_TYPE_ID);
    assert_eq!(cells[0].revision(), 1);
    assert_eq!(audit.len(), 3, "write, conflict, and load audits");
    assert_eq!(audit[0].operation, "write");
    assert_eq!(audit[0].outcome, "completed");
    assert_eq!(audit[1].operation, "write");
    assert_eq!(audit[1].outcome, "conflict");
    assert_eq!(audit[2].operation, "load");
    assert_eq!(audit[2].outcome, "completed");
    assert_eq!(
        encode_value(cells[0].value()).expect("re-encode ORV5"),
        value_bytes
    );
}

#[test]
fn local_sqlite_user_state_rejects_uninstalled_schema() {
    let directory = TestDirectory::new("user-state-schema").expect("scratch directory");
    let database = directory.path().join("user-state.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy server function fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for USER state schema rejection");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let request = InstalledUserStateRequest::new(InstalledUserStateOperation::Write {
        root_function: FunctionId::from_bytes([0x11; 16]),
        state_profile: "profile".to_owned(),
        change: InstalledUserStateChange {
            function: FunctionId::from_bytes([0x22; 16]),
            instance_key: String::new(),
            state_slot: orna_core::StateSlotId::from_bytes([0x33; 16]),
            expected_revision: None,
            value_type: INTEGER_TYPE_ID,
            value_bytes: encode_value(&RuntimeValue::Integer(42)).expect("encode ORV5 integer"),
        },
    });
    let error = run_sqlite_user_state(database, request, &mut Vec::new())
        .expect_err("uninstalled USER state schema must be rejected");
    assert_eq!(error.kind(), InstalledUserStateErrorKind::State);
    assert!(error.message().contains("not installed"));
}

#[test]
fn local_sqlite_security_admin_mutations_persist() {
    let directory = TestDirectory::new("security-admin").expect("scratch directory");
    let database = directory.path().join("security-admin.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy server function fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for security admin");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let document: Value = serde_json::from_slice(&applied.stdout).expect("security apply JSON");
    let health_target = document["functions"]
        .as_array()
        .and_then(|functions| {
            functions.iter().find(|entry| {
                entry["qualified_name"]
                    == Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String("read".to_owned()),
                    ])
            })
        })
        .and_then(|entry| entry["function_id"].as_str())
        .expect("dogfood.read function identity")
        .to_owned();
    for _ in 0..2 {
        let granted = run_orna(
            directory.path(),
            &[
                OsString::from("--db"),
                database.as_os_str().to_os_string(),
                OsString::from("security"),
                OsString::from("grant-execute"),
                OsString::from(&health_target),
            ],
        )
        .expect("grant fixed catalogue-health execute");
        assert_eq!(granted.status.code(), Some(0), "grant-execute: {granted:?}");
    }
    let active = sqlite_active(&database);
    let health_function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["dogfood", "read"])
        .expect("dogfood.read active function")
        .id();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("security snapshot runtime");
    let fixed_grant_count = runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&database))
            .await
            .expect("open security snapshot store");
        let snapshot = store
            .security_snapshot(&active)
            .await
            .expect("load security snapshot");
        snapshot
            .execute_grants()
            .filter(|grant| {
                grant.grantee() == orna_core::security::CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
                    && grant.function() == health_function
            })
            .count()
    });
    assert_eq!(fixed_grant_count, 1, "fixed execute grant is idempotent");
    let whoami = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("whoami"),
        ],
    )
    .expect("local SQLite security whoami");
    assert_eq!(whoami.status.code(), Some(0), "whoami: {whoami:?}");
    let principal =
        serde_json::from_slice::<Value>(&whoami.stdout).expect("whoami JSON")["principal"]
            .as_str()
            .expect("whoami principal")
            .to_owned();
    let created = PrincipalId::from_bytes([0x44; 16]).canonical();
    let create = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("user"),
            OsString::from("create"),
            OsString::from(&created),
        ],
    )
    .expect("create local SQLite principal");
    assert_eq!(
        create.status.code(),
        Some(0),
        "create principal: {create:?}"
    );
    let grants = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("grants"),
            OsString::from("grant"),
            OsString::from(&created),
            OsString::from("execute"),
        ],
    )
    .expect("grant local SQLite privilege");
    assert_eq!(grants.status.code(), Some(0), "grant privilege: {grants:?}");
    let object_grantee = PrincipalId::from_bytes([0x45; 16]).canonical();
    let object_create = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("user"),
            OsString::from("create"),
            OsString::from(&object_grantee),
        ],
    )
    .expect("create object-scoped SQLite principal");
    assert_eq!(
        object_create.status.code(),
        Some(0),
        "create object-scoped principal: {object_create:?}"
    );
    let object_grant = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("grants"),
            OsString::from("grant"),
            OsString::from(&object_grantee),
            OsString::from("execute"),
            OsString::from(&health_target),
        ],
    )
    .expect("grant object-scoped SQLite privilege");
    assert_eq!(
        object_grant.status.code(),
        Some(0),
        "grant object-scoped privilege: {object_grant:?}"
    );
    let object_check = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("check"),
            OsString::from("has-privilege"),
            OsString::from(&object_grantee),
            OsString::from("execute"),
            OsString::from(&health_target),
        ],
    )
    .expect("check object-scoped SQLite privilege");
    assert_eq!(
        object_check.status.code(),
        Some(0),
        "object-scoped privilege check: {object_check:?}"
    );
    let object_check: Value =
        serde_json::from_slice(&object_check.stdout).expect("object privilege JSON");
    assert_eq!(object_check["result"], true);
    let listed = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("security"),
            OsString::from("grants"),
            OsString::from("list"),
            OsString::from(&created),
        ],
    )
    .expect("list local SQLite grants");
    assert_eq!(listed.status.code(), Some(0), "list grants: {listed:?}");
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("listed grants JSON");
    assert_eq!(listed["principal"], created);
    assert_eq!(listed["privileges"][0]["class"], "execute");
    assert!(!principal.is_empty(), "whoami principal remains present");
}

#[test]
fn local_sqlite_raw_call_route_executes_server_function() {
    let directory = TestDirectory::new("raw-call").expect("scratch directory");
    let database = directory.path().join("raw-call.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for raw call");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let document: Value = serde_json::from_slice(&applied.stdout).expect("source apply JSON");
    let function = document["functions"]
        .as_array()
        .and_then(|functions| {
            functions.iter().find(|entry| {
                entry["qualified_name"]
                    == Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String("read".to_owned()),
                    ])
            })
        })
        .and_then(|entry| entry["function_id"].as_str())
        .expect("dogfood.read function identity")
        .to_owned();
    let result = run_orna(
        directory.path(),
        &[
            OsString::from("--db"),
            database.as_os_str().to_os_string(),
            OsString::from("raw-call"),
            OsString::from(function),
        ],
    )
    .expect("local SQLite raw call");
    assert_eq!(result.status.code(), Some(0), "raw call: {result:?}");
    assert!(
        result.stdout.is_empty(),
        "empty dogfood table has no values"
    );
    assert!(
        result.stderr.is_empty(),
        "raw call keeps stdout/stderr clean"
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
fn local_sqlite_socket_dispatches_raw_calls_and_rejects_unknown_targets() {
    let directory = TestDirectory::new("socket-call").expect("scratch directory");
    let database = directory.path().join("socket-call.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for raw socket call");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let document: Value = serde_json::from_slice(&applied.stdout).expect("source apply JSON");
    let function = document["functions"]
        .as_array()
        .and_then(|functions| {
            functions.iter().find(|entry| {
                entry["qualified_name"]
                    == Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String("read".to_owned()),
                    ])
            })
        })
        .and_then(|entry| entry["function_id"].as_str())
        .and_then(|canonical| FunctionId::from_canonical(canonical).ok())
        .expect("dogfood.read function identity");

    let socket = socket_path(&database);
    let mut server = RunningServer::spawn(directory.path(), &database).expect("SQLite server");
    server
        .wait_for_socket(&socket)
        .expect("bounded socket readiness");
    let mut stream = connect_socket(&socket).expect("connect raw-call socket");
    complete_v1_handshake(&mut stream).expect("raw-call v1 handshake");

    for frame in [
        ClientFrame::CallRawStart {
            stream: 1,
            function,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
    ] {
        stream
            .write_all(&encode_client_frame(&frame).expect("encode raw-call frame"))
            .expect("write raw-call frame");
    }
    stream.flush().expect("flush raw-call frames");
    let mut accepted_invocation = None;
    let mut completed = false;
    for _ in 0..3 {
        match decode_server_frame(&read_frame(&mut stream).expect("read raw-call response"))
            .expect("decode raw-call response")
        {
            ServerFrame::CallAccepted {
                stream: response_stream,
                invocation,
            } => {
                assert_eq!(response_stream, 1);
                accepted_invocation = Some(invocation);
            }
            ServerFrame::EventBatch {
                stream: response_stream,
                ..
            } => assert_eq!(response_stream, 1),
            ServerFrame::CallCompleted {
                stream: response_stream,
            } => {
                assert_eq!(response_stream, 1);
                completed = true;
                break;
            }
            response => panic!("unexpected raw-call response: {response:?}"),
        }
    }

    for frame in [
        ClientFrame::CallRawStart {
            stream: 2,
            function: FunctionId::from_bytes([0xff; 16]),
        },
        ClientFrame::CallArgumentsComplete { stream: 2 },
    ] {
        stream
            .write_all(&encode_client_frame(&frame).expect("encode unknown-target frame"))
            .expect("write unknown-target frame");
    }
    stream.flush().expect("flush unknown-target frames");
    assert_eq!(
        decode_server_frame(&read_frame(&mut stream).expect("read unknown-target response"))
            .expect("decode unknown-target response"),
        ServerFrame::CallFailed {
            stream: 2,
            failure: orna_protocol::CallFailure::ExecuteDenied,
        }
    );
    drop(stream);
    let _ = server
        .stop_with_signal(Signal::SIGTERM)
        .expect("stop SQLite socket server");
    let invocation = accepted_invocation.expect("raw call accepted invocation id");
    assert!(completed, "raw call must complete");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("socket evidence runtime");
    let (audit, snapshot, trace) = runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&database))
            .await
            .expect("open socket evidence store");
        let active = store
            .recover()
            .await
            .expect("recover socket evidence revision");
        let security = store
            .security_snapshot(&active)
            .await
            .expect("recover socket evidence security");
        let session = security
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .expect("authenticate socket evidence principal");
        (
            store
                .load_invocation_audit(invocation)
                .await
                .expect("load socket invocation audit")
                .expect("socket invocation audit"),
            store
                .load_inspect_snapshot(&active, &security, &session, invocation, None)
                .await
                .expect("load socket inspect snapshot")
                .expect("socket inspect snapshot"),
            store
                .load_inspect_trace_events(&active, &security, &session, invocation, 0)
                .await
                .expect("load socket inspect trace"),
        )
    });
    assert_eq!(audit.invocation, invocation);
    assert_eq!(audit.outcome, "completed");
    assert_eq!(snapshot.invocation, invocation);
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].invocation, invocation);
}
#[test]
fn local_sqlite_socket_supports_v2_catalogue_calls() {
    let directory = TestDirectory::new("socket-v2").expect("scratch directory");
    let database = directory.path().join("socket-v2.sqlite");
    let source = directory.path().join("server_function_dogfood.orna");
    fs::write(&source, SERVER_FUNCTION_FIXTURE).expect("copy source fixture");
    let applied = run_orna(
        directory.path(),
        &source_arguments(&database, "apply", &source),
    )
    .expect("source apply for v2 socket call");
    assert_eq!(applied.status.code(), Some(0), "source apply: {applied:?}");
    let document: Value = serde_json::from_slice(&applied.stdout).expect("source apply JSON");
    let function = document["functions"]
        .as_array()
        .and_then(|functions| {
            functions.iter().find(|entry| {
                entry["qualified_name"]
                    == Value::Array(vec![
                        Value::String("dogfood".to_owned()),
                        Value::String("read".to_owned()),
                    ])
            })
        })
        .and_then(|entry| entry["function_id"].as_str())
        .and_then(|canonical| FunctionId::from_canonical(canonical).ok())
        .expect("dogfood.read function identity");
    let active = sqlite_active(&database);

    let socket = socket_path(&database);
    let mut server = RunningServer::spawn(directory.path(), &database).expect("SQLite server");
    server
        .wait_for_socket(&socket)
        .expect("bounded socket readiness");
    let mut stream = connect_socket(&socket).expect("connect v2 socket");
    complete_v2_handshake(&mut stream).expect("v2 handshake");
    for frame in [
        ClientFrame::CallRawStart {
            stream: 1,
            function,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
    ] {
        stream
            .write_all(
                &encode_catalogue_client_frame(active.catalogue(), &frame)
                    .expect("encode v2 call frame"),
            )
            .expect("write v2 call frame");
    }
    stream.flush().expect("flush v2 call frames");
    let mut accepted = false;
    let mut completed = false;
    for _ in 0..3 {
        match decode_catalogue_server_frame(
            active.catalogue(),
            &read_frame(&mut stream).expect("read v2 call response"),
        )
        .expect("decode v2 call response")
        {
            ServerFrame::CallAccepted { stream, .. } => {
                assert_eq!(stream, 1);
                accepted = true;
            }
            ServerFrame::EventBatch { stream, .. } => assert_eq!(stream, 1),
            ServerFrame::CallCompleted { stream } => {
                assert_eq!(stream, 1);
                completed = true;
                break;
            }
            response => panic!("unexpected v2 call response: {response:?}"),
        }
    }
    assert!(accepted, "v2 raw call must be accepted");
    assert!(completed, "v2 raw call must complete");
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
    let stale_output = stale_server
        .stop_with_signal(Signal::SIGINT)
        .expect("bounded stale server shutdown");
    assert!(stale_output.status.success());
    assert!(
        !socket.exists(),
        "graceful stale-server shutdown removes socket"
    );

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
    let live_output = live_server
        .stop_with_signal(Signal::SIGTERM)
        .expect("bounded live server shutdown");
    assert!(live_output.status.success());
    assert!(
        !socket.exists(),
        "graceful live-server shutdown removes socket"
    );
}
#[test]
fn local_sqlite_socket_rejects_unknown_versions_and_handles_concurrent_clients() {
    let directory = TestDirectory::new("socket-concurrency").expect("scratch directory");
    let database = directory.path().join("concurrency.sqlite");
    let socket = socket_path(&database);
    let mut server = RunningServer::spawn(directory.path(), &database).expect("SQLite server");
    server
        .wait_for_socket(&socket)
        .expect("bounded socket readiness");

    let mut unsupported = connect_socket(&socket).expect("connect unsupported-version socket");
    unsupported
        .write_all(&UNSUPPORTED_HELLO)
        .expect("write unsupported hello");
    unsupported.flush().expect("flush unsupported hello");
    let mut acknowledgement = [0_u8; 12];
    let bytes_read = unsupported
        .read(&mut acknowledgement)
        .expect("read unsupported-version response");
    assert_eq!(
        bytes_read, 0,
        "unsupported protocol versions must receive no acknowledgement"
    );

    let mut clients = Vec::new();
    for index in 0_u8..8 {
        let socket = socket.clone();
        clients.push(thread::spawn(move || -> io::Result<()> {
            let mut stream = connect_socket(&socket)?;
            complete_v1_handshake(&mut stream)?;
            let token = [index; 8];
            stream.write_all(
                &encode_client_frame(&ClientFrame::Ping { token }).map_err(io::Error::other)?,
            )?;
            stream.flush()?;
            let frame = read_frame(&mut stream)?;
            assert_eq!(
                decode_server_frame(&frame).map_err(io::Error::other)?,
                ServerFrame::Pong { token }
            );
            Ok(())
        }));
    }
    for client in clients {
        client
            .join()
            .expect("concurrent socket client thread")
            .expect("concurrent socket client");
    }
    let output = server
        .stop_with_signal(Signal::SIGTERM)
        .expect("bounded concurrent server shutdown");
    assert!(output.status.success());
    assert!(
        !socket.exists(),
        "graceful shutdown removes concurrent socket"
    );
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
