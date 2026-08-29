//! Live process-boundary tests for the raw-call CLI argument row.
//!
//! These tests run the actual compiled `orna` binary with a cleared
//! environment and caller-owned scratch state below `target`. They never
//! start Docker, PostgreSQL, or any external service, and they never call a
//! private API. Tests that execute a syntactically valid raw-call enforce a
//! hard precondition that the child process's fallback socket is absent and
//! fail before spawning if it exists or cannot be inspected, so a regression
//! can never contact a live local daemon. Linux `/proc` inspection is used
//! only to bound readiness polling and to prove that no socket descriptor is
//! opened before the input boundary closes the call.

#![cfg(target_os = "linux")]

mod support;

use nix::{
    sys::signal::{Signal, kill},
    unistd::{Pid, geteuid},
};
use orna_core::{FunctionId, ParameterId};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_INVALID: &[u8] = b"orna: raw-call argument input is invalid\n";
const CONNECTION_FAILED: &[u8] = b"local raw-call connection failed\n";
const ORV1_TRUE: [u8; 26] = [
    0x4f, 0x52, 0x56, 0x31, // ORV1 marker
    0x02, // Boolean tag
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Boolean type identity
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // Boolean type identity
    0x00, 0x00, 0x00, 0x01, // payload length
    0x01, // Boolean true payload
];
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("server crate remains below crates");
        let path = repository.join("target").join(format!(
            "raw-call-test-{}-{}-{label}",
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

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn spawn_orna(directory: &Path, arguments: &[OsString], stdin: Stdio) -> io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .current_dir(directory)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn run_with_stdin(directory: &Path, arguments: &[OsString], input: &[u8]) -> io::Result<Output> {
    let mut child = spawn_orna(directory, arguments, Stdio::piped())?;
    let mut stdin = child.stdin.take().expect("captured stdin");
    stdin.write_all(input)?;
    drop(stdin);
    wait_bounded(child)
}

fn run_without_stdin(directory: &Path, arguments: &[OsString]) -> io::Result<Output> {
    wait_bounded(spawn_orna(directory, arguments, Stdio::null())?)
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
                "orna raw-call did not exit",
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
fn fallback_socket_path() -> PathBuf {
    PathBuf::from("/tmp")
        .join(format!(".orna-{}", geteuid().as_raw()))
        .join("runtime/orna/default/orna.sock")
}

fn require_fixed_socket_absent() {
    let socket = fallback_socket_path();
    match fs::symlink_metadata(&socket) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("fallback socket inspection failed: {error}"),
        Ok(_) => panic!(
            "the fallback socket {} exists; refusing to run against a live local daemon",
            socket.display()
        ),
    }
}

fn assert_input_invalid(output: &Output) {
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, INPUT_INVALID);
}

fn sigint_handler_is_caught(pid: i32) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return false;
    };
    status
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim() != "SigCgt" {
                return None;
            }
            Some(
                u64::from_str_radix(value.trim(), 16)
                    .map(|mask| mask & 0x2 != 0)
                    .unwrap_or(false),
            )
        })
        .unwrap_or(false)
}

fn wait_for_sigint_handler(pid: i32) -> io::Result<()> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while !sigint_handler_is_caught(pid) {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "raw-call child never caught SIGINT",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn socket_descriptors(pid: i32) -> io::Result<Vec<String>> {
    let mut sockets = Vec::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        let target = fs::read_link(entry.path())?;
        let target = target.to_string_lossy().into_owned();
        if target.starts_with("socket:[") {
            sockets.push(target);
        }
    }
    Ok(sockets)
}

fn pair_command(function: &FunctionId, first: &ParameterId, second: &ParameterId) -> Vec<OsString> {
    let function = function.canonical();
    let first = first.canonical();
    let second = second.canonical();
    arguments(&[
        "raw-call",
        function.as_str(),
        first.as_str(),
        second.as_str(),
    ])
}

fn distinct_pair_ids() -> (FunctionId, ParameterId, ParameterId) {
    (
        FunctionId::from_bytes([0x11; 16]),
        ParameterId::from_bytes([0x22; 16]),
        ParameterId::from_bytes([0x33; 16]),
    )
}

#[test]
fn invalid_stdin_boundaries_exit_seven_with_exact_stderr() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("invalid-input").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    let mut bad_marker = ORV1_TRUE.to_vec();
    bad_marker[..4].copy_from_slice(b"ORV2");
    let mut malformed_boolean = ORV1_TRUE.to_vec();
    malformed_boolean[25] = 0x02;
    let mut trailing = ORV1_TRUE.to_vec();
    trailing.push(0xaa);
    let cases: [(&str, Vec<u8>); 6] = [
        ("empty", Vec::new()),
        ("truncated header", ORV1_TRUE[..24].to_vec()),
        ("truncated value", ORV1_TRUE[..25].to_vec()),
        ("bad marker", bad_marker),
        ("malformed Boolean", malformed_boolean),
        ("trailing byte", trailing),
    ];
    let command = arguments(&["raw-call", function.as_str(), parameter.as_str()]);
    for (label, input) in cases {
        let output = run_with_stdin(&directory.0, &command, &input)
            .unwrap_or_else(|error| panic!("{label} failed: {error}"));
        assert_input_invalid(&output);
    }
}

#[test]
fn oversized_header_is_rejected_before_any_payload_read() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("oversized").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    let mut header = ORV1_TRUE[..25].to_vec();
    // Declared payload exactly one byte above the ADR raw-argument maximum
    // MAX_FRAME_PAYLOAD_LENGTH - 16, so the total envelope is
    // (MAX_FRAME_PAYLOAD_LENGTH - 16) + 1.
    let declared = u32::try_from(orna_protocol::MAX_FRAME_PAYLOAD_LENGTH - 16 - 25 + 1)
        .expect("bounded length");
    header[21..25].copy_from_slice(&declared.to_be_bytes());
    let mut child = spawn_orna(
        &directory.0,
        &arguments(&["raw-call", function.as_str(), parameter.as_str()]),
        Stdio::piped(),
    )
    .expect("oversized raw-call process");
    let mut stdin = child.stdin.take().expect("captured stdin");
    stdin.write_all(&header).expect("oversized header write");
    // Keep the pipe open. A reader that tried to consume the declared payload
    // would block here instead of rejecting, so the bounded status 7 proves
    // the size check ran before any payload read or allocation.
    let _held_stdin = stdin;
    let output = wait_bounded(child).expect("oversized header rejection exits");
    assert_input_invalid(&output);
}

#[test]
fn exact_orv1_true_passes_validation_then_reaches_the_absent_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("true-input").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    let output = run_with_stdin(
        &directory.0,
        &arguments(&["raw-call", function.as_str(), parameter.as_str()]),
        &ORV1_TRUE,
    )
    .expect("exact TRUE input");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, CONNECTION_FAILED);
}

#[test]
fn exact_orv1_pair_passes_validation_then_reaches_the_absent_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-input").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let mut input = ORV1_TRUE.to_vec();
    input.extend_from_slice(&ORV1_TRUE);
    let output = run_with_stdin(
        &directory.0,
        &pair_command(&function, &first, &second),
        &input,
    )
    .expect("exact pair input");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, CONNECTION_FAILED);
}

#[test]
fn pair_malformed_boundaries_and_missing_second_value_exit_seven() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-invalid-input").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let command = pair_command(&function, &first, &second);
    let mut malformed = ORV1_TRUE.to_vec();
    malformed[..4].copy_from_slice(b"ORV2");
    let mut malformed_first = malformed.clone();
    malformed_first.extend_from_slice(&ORV1_TRUE);
    let mut malformed_second = ORV1_TRUE.to_vec();
    malformed_second.extend_from_slice(&malformed);
    for (label, input) in [
        ("empty", Vec::new()),
        ("malformed first", malformed_first),
        ("missing second", ORV1_TRUE.to_vec()),
        ("malformed second", malformed_second),
    ] {
        let output = run_with_stdin(&directory.0, &command, &input)
            .unwrap_or_else(|error| panic!("{label} failed: {error}"));
        assert_input_invalid(&output);
    }
}

#[test]
fn pair_third_input_byte_exits_seven_before_the_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-trailing").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let mut input = ORV1_TRUE.to_vec();
    input.extend_from_slice(&ORV1_TRUE);
    input.push(0xaa);
    let output = run_with_stdin(
        &directory.0,
        &pair_command(&function, &first, &second),
        &input,
    )
    .expect("third input byte rejection");
    assert_input_invalid(&output);
}

#[test]
fn pair_individual_and_aggregate_oversize_exit_seven_before_the_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-oversize").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let command = pair_command(&function, &first, &second);
    let declared = u32::try_from(orna_protocol::MAX_FRAME_PAYLOAD_LENGTH - 16 - 25 + 1)
        .expect("bounded length");
    let mut oversized_header = ORV1_TRUE[..25].to_vec();
    oversized_header[21..25].copy_from_slice(&declared.to_be_bytes());

    for (label, input) in [
        ("oversized first", oversized_header.clone()),
        ("oversized second", {
            let mut input = ORV1_TRUE.to_vec();
            input.extend_from_slice(&oversized_header);
            input
        }),
    ] {
        let mut child = spawn_orna(&directory.0, &command, Stdio::piped())
            .unwrap_or_else(|error| panic!("{label} process: {error}"));
        let mut stdin = child.stdin.take().expect("captured stdin");
        stdin
            .write_all(&input)
            .unwrap_or_else(|error| panic!("{label} header: {error}"));
        // Keep the input pipe open. The declared payload must be rejected
        // before a reader can wait for it or allocate it.
        let _held_stdin = stdin;
        let output = wait_bounded(child)
            .unwrap_or_else(|error| panic!("{label} bounded rejection: {error}"));
        assert_input_invalid(&output);
    }

    // The first complete envelope is valid. The second header is also within
    // the individual limit, but its declared value makes the two retained
    // ParameterIds and envelopes exceed the shared frame budget. The held
    // pipe proves that aggregate validation runs before the second payload.
    let aggregate_declared = u32::try_from(orna_protocol::MAX_FRAME_PAYLOAD_LENGTH - 82)
        .expect("bounded aggregate length");
    let mut aggregate_header = ORV1_TRUE[..25].to_vec();
    aggregate_header[21..25].copy_from_slice(&aggregate_declared.to_be_bytes());
    let mut aggregate = ORV1_TRUE.to_vec();
    aggregate.extend_from_slice(&aggregate_header);
    let mut child =
        spawn_orna(&directory.0, &command, Stdio::piped()).expect("aggregate oversize process");
    let mut stdin = child.stdin.take().expect("captured stdin");
    stdin
        .write_all(&aggregate)
        .expect("aggregate oversize header");
    let _held_stdin = stdin;
    let output = wait_bounded(child).expect("aggregate bounded rejection");
    assert_input_invalid(&output);
}

#[test]
fn pair_blocks_after_each_envelope_without_opening_a_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-boundaries").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let command = pair_command(&function, &first, &second);

    for (label, input) in [
        ("second envelope", ORV1_TRUE.to_vec()),
        (
            "final eof",
            [ORV1_TRUE.as_slice(), ORV1_TRUE.as_slice()].concat(),
        ),
    ] {
        let mut child = spawn_orna(&directory.0, &command, Stdio::piped())
            .unwrap_or_else(|error| panic!("{label} process: {error}"));
        let mut stdin = child.stdin.take().expect("captured stdin");
        stdin
            .write_all(&input)
            .unwrap_or_else(|error| panic!("{label} input: {error}"));
        let pid = child.id() as i32;
        wait_for_sigint_handler(pid).unwrap_or_else(|error| panic!("{label} handler: {error}"));
        assert!(
            child.try_wait().expect("running check").is_none(),
            "the child must block while waiting for {label}"
        );
        assert!(
            socket_descriptors(pid)
                .expect("descriptor inspection")
                .is_empty(),
            "the child must not open a socket while waiting for {label}"
        );
        drop(stdin);
        let output = wait_bounded(child).unwrap_or_else(|error| panic!("{label} exit: {error}"));
        if label == "second envelope" {
            assert_input_invalid(&output);
        } else {
            assert_eq!(output.status.code(), Some(3), "{label}");
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, CONNECTION_FAILED, "{label}");
        }
    }
}

#[test]
fn pair_sigint_while_waiting_for_second_or_eof_exits_six() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("pair-interrupt").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let command = pair_command(&function, &first, &second);

    for (label, input) in [
        ("first envelope", Vec::new()),
        ("second envelope", ORV1_TRUE.to_vec()),
        (
            "final eof",
            [ORV1_TRUE.as_slice(), ORV1_TRUE.as_slice()].concat(),
        ),
    ] {
        let mut child = spawn_orna(&directory.0, &command, Stdio::piped())
            .unwrap_or_else(|error| panic!("{label} process: {error}"));
        let mut stdin = child.stdin.take().expect("captured stdin");
        stdin
            .write_all(&input)
            .unwrap_or_else(|error| panic!("{label} input: {error}"));
        let pid = child.id() as i32;
        wait_for_sigint_handler(pid).unwrap_or_else(|error| panic!("{label} handler: {error}"));
        assert!(
            child.try_wait().expect("running check").is_none(),
            "the child must block while waiting for {label}"
        );
        assert!(
            socket_descriptors(pid)
                .expect("descriptor inspection")
                .is_empty(),
            "the child must not open a socket while waiting for {label}"
        );
        kill(Pid::from_raw(pid), Signal::SIGINT).expect("SIGINT delivery");
        drop(stdin);
        let output = wait_bounded(child).unwrap_or_else(|error| panic!("{label} exit: {error}"));
        assert_eq!(output.status.code(), Some(6), "{label}");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn zero_argument_call_never_reads_held_open_stdin() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("zero-argument").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let mut child = spawn_orna(
        &directory.0,
        &arguments(&["raw-call", function.as_str()]),
        Stdio::piped(),
    )
    .expect("zero-argument raw-call process");
    // The parent holds the stdin write end open. A child that read stdin
    // would block here, so the bounded wait proves it never reads stdin.
    let _held_stdin = child.stdin.take();
    let output = wait_bounded(child).expect("zero-argument raw-call exits at the socket");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, CONNECTION_FAILED);
}

#[test]
fn one_argument_trailing_byte_is_rejected_while_stdin_stays_open() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("trailing").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    let mut child = spawn_orna(
        &directory.0,
        &arguments(&["raw-call", function.as_str(), parameter.as_str()]),
        Stdio::piped(),
    )
    .expect("one-argument raw-call process");
    let mut stdin = child.stdin.take().expect("captured stdin");
    stdin.write_all(&ORV1_TRUE).expect("exact TRUE input");
    let pid = child.id() as i32;
    wait_for_sigint_handler(pid).expect("child installs its SIGINT handler");
    assert!(
        child.try_wait().expect("running check").is_none(),
        "the child must remain blocked on the trailing EOF probe"
    );
    assert!(
        socket_descriptors(pid)
            .expect("descriptor inspection")
            .is_empty(),
        "the blocked trailing probe must not open a socket descriptor"
    );
    stdin.write_all(&[0xaa]).expect("trailing byte write");
    // Keep the pipe write end open through the bounded wait: the one-byte
    // trailing probe must reject on that byte alone, with no EOF supplied to
    // the child. A reader that waited for EOF would block and time out.
    let _held_stdin = stdin;
    let output = wait_bounded(child).expect("trailing-byte rejection exits");
    assert_input_invalid(&output);
}

#[test]
fn one_argument_call_sigint_cancels_before_the_socket() {
    require_fixed_socket_absent();
    let directory = TestDirectory::new("interrupt").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    let mut child = spawn_orna(
        &directory.0,
        &arguments(&["raw-call", function.as_str(), parameter.as_str()]),
        Stdio::piped(),
    )
    .expect("one-argument raw-call process");
    // The empty pipe write end stays open, so the child blocks on the input
    // read before any socket work.
    let _held_stdin = child.stdin.take();
    let pid = child.id() as i32;
    wait_for_sigint_handler(pid).expect("child installs its SIGINT handler");
    assert!(
        socket_descriptors(pid)
            .expect("descriptor inspection")
            .is_empty(),
        "the blocked input read must not open a socket descriptor"
    );
    kill(Pid::from_raw(pid), Signal::SIGINT).expect("SIGINT delivery");
    let output = wait_bounded(child).expect("interrupted raw-call exits cleanly");
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn malformed_parameters_and_extra_tokens_print_the_usage() {
    let directory = TestDirectory::new("usage").expect("test directory");
    let function = FunctionId::from_bytes([0x11; 16]).canonical();
    let parameter = ParameterId::from_bytes([0x22; 16]).canonical();
    for values in [
        vec!["raw-call"],
        vec!["raw-call", function.as_str(), "parameter:not-an-id"],
        vec!["raw-call", function.as_str(), parameter.as_str(), "extra"],
        vec!["raw-call", "sys.catalog.health", parameter.as_str()],
    ] {
        let output =
            run_without_stdin(&directory.0, &arguments(&values)).expect("usage rejection exits");
        assert_eq!(output.status.code(), Some(2), "{values:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, support::EXPECTED_USAGE, "{values:?}");
    }

    let non_unicode_parameter = OsString::from_vec(b"parameter:\xff".to_vec());
    let output = run_without_stdin(
        &directory.0,
        &[
            OsString::from("raw-call"),
            OsString::from(function.as_str()),
            non_unicode_parameter,
        ],
    )
    .expect("non-Unicode parameter rejection exits");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, support::EXPECTED_USAGE);
}

#[test]
fn pair_command_shape_errors_do_not_read_held_open_stdin() {
    let directory = TestDirectory::new("pair-usage").expect("test directory");
    let (function, first, second) = distinct_pair_ids();
    let third = ParameterId::from_bytes([0x44; 16]);
    let function = function.canonical();
    let first = first.canonical();
    let second = second.canonical();
    let third = third.canonical();
    let cases = [
        vec![
            "raw-call",
            function.as_str(),
            first.as_str(),
            first.as_str(),
        ],
        vec!["raw-call", function.as_str(), "parameter:not-an-id"],
        vec![
            "raw-call",
            function.as_str(),
            first.as_str(),
            "parameter:not-an-id",
        ],
        vec![
            "raw-call",
            function.as_str(),
            first.as_str(),
            second.as_str(),
            third.as_str(),
        ],
    ];
    for values in cases {
        let mut child = spawn_orna(&directory.0, &arguments(&values), Stdio::piped())
            .unwrap_or_else(|error| panic!("{values:?} process: {error}"));
        // Keep the write end open. Any command-shape path that reads stdin
        // blocks here and violates the CLI authority boundary.
        let _held_stdin = child.stdin.take();
        let output =
            wait_bounded(child).unwrap_or_else(|error| panic!("{values:?} usage exit: {error}"));
        assert_eq!(output.status.code(), Some(2), "{values:?}");
        assert!(output.stdout.is_empty(), "{values:?}");
        assert_eq!(output.stderr, support::EXPECTED_USAGE, "{values:?}");
    }
}
