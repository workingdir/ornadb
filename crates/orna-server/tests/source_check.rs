#![cfg(unix)]

use nix::{sys::stat::Mode, unistd::mkfifo};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, PermissionsExt, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const USAGE: &[u8] = b"Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna raw-call <canonical-function-id>\n";
const VALID_SOURCE: &[u8] =
    b"CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
const TERMINAL_REQUIRED: &[u8] = b"orna: backend-shell must be run in an interactive terminal\n";
const RAW_CALL_CONNECTION_FAILED: &[u8] = b"local raw-call connection failed\n";
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
            "source-check-test-{}-{}-{label}",
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

fn spawn_source_check(directory: &Path, path: OsString) -> io::Result<Child> {
    spawn_orna(
        directory,
        [OsString::from("source"), OsString::from("check"), path],
        [],
        Stdio::null(),
    )
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

fn run_orna(directory: &Path, arguments: impl IntoIterator<Item = OsString>) -> io::Result<Output> {
    wait_bounded(spawn_orna(directory, arguments, [], Stdio::null())?)
}

fn run_source_check(directory: &Path, path: impl Into<OsString>) -> io::Result<Output> {
    wait_bounded(spawn_source_check(directory, path.into())?)
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
                "orna source check did not exit",
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

fn assert_read_failure(output: &Output, path: &[u8]) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let mut expected = b"orna: could not read source file: ".to_vec();
    expected.extend_from_slice(path);
    expected.push(b'\n');
    assert_eq!(output.stderr, expected);
}

fn assert_usage(output: &Output) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, USAGE);
}

fn assert_success(output: &Output) {
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotEntry {
    kind: &'static str,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    content: Vec<u8>,
}

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, SnapshotEntry>> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<PathBuf, SnapshotEntry>,
    ) -> io::Result<()> {
        let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            let file_type = metadata.file_type();
            let (kind, content) = if file_type.is_dir() {
                visit(root, &path, entries)?;
                ("directory", Vec::new())
            } else if file_type.is_file() {
                ("file", fs::read(&path)?)
            } else if file_type.is_symlink() {
                ("symlink", fs::read_link(&path)?.into_os_string().into_vec())
            } else {
                ("special", Vec::new())
            };
            entries.insert(
                path.strip_prefix(root).expect("snapshot member").to_owned(),
                SnapshotEntry {
                    kind,
                    mode: metadata.mode(),
                    uid: metadata.uid(),
                    gid: metadata.gid(),
                    links: metadata.nlink(),
                    length: metadata.len(),
                    modified_seconds: metadata.mtime(),
                    modified_nanoseconds: metadata.mtime_nsec(),
                    changed_seconds: metadata.ctime(),
                    changed_nanoseconds: metadata.ctime_nsec(),
                    content,
                },
            );
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

#[test]
fn accepts_only_the_exact_command_shape_and_valid_path_tokens() {
    let directory = TestDirectory::new("arguments").expect("test directory");
    for name in ["application.orna", "source with spaces", "-x"] {
        fs::write(directory.0.join(name), VALID_SOURCE).expect("valid source");
    }

    assert_success(
        &run_source_check(&directory.0, "application.orna").expect("valid source check"),
    );
    assert_success(&run_source_check(&directory.0, "source with spaces").expect("space path"));
    assert_success(&run_source_check(&directory.0, "./-x").expect("qualified hyphen path"));

    for arguments in [
        vec![],
        vec![OsString::from("source")],
        vec![OsString::from("source"), OsString::from("check")],
        vec![OsString::from("source"), OsString::from("--check")],
        vec![OsString::from("--source"), OsString::from("check")],
        vec![
            OsString::from("source"),
            OsString::from("check"),
            OsString::new(),
        ],
        vec![
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("-"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("--"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("application.orna"),
            OsString::from("--extra"),
        ],
        vec![
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("first.orna"),
            OsString::from("second.orna"),
        ],
    ] {
        assert_usage(&run_orna(&directory.0, arguments).expect("usage result"));
    }

    for path in [
        "-application.orna",
        "tab\tpath",
        "line\npath",
        "return\rpath",
        "escape\u{001b}path",
        "separator\u{2028}path",
        "paragraph\u{2029}path",
    ] {
        assert_usage(
            &run_orna(
                &directory.0,
                [
                    OsString::from("source"),
                    OsString::from("check"),
                    OsString::from(path),
                ],
            )
            .expect("invalid path result"),
        );
    }
    assert_usage(
        &run_orna(
            &directory.0,
            [
                OsString::from("source"),
                OsString::from("check"),
                OsString::from_vec(b"source-\xff.orna".to_vec()),
            ],
        )
        .expect("non-Unicode path result"),
    );

    assert_read_failure(
        &run_source_check(&directory.0, "*.orna").expect("literal wildcard result"),
        b"*.orna",
    );
}

#[test]
fn resolves_only_the_submitted_regular_file_and_keeps_the_logical_path() {
    let directory = TestDirectory::new("paths").expect("test directory");
    let nested = directory.0.join("nested");
    fs::create_dir(&nested).expect("nested directory");
    fs::write(nested.join("broken.orna"), b"CREATE SCHEMA ;").expect("broken source");
    symlink("broken.orna", nested.join("linked.orna")).expect("regular-file link");
    symlink("missing.orna", nested.join("dangling.orna")).expect("dangling link");
    let socket_path = nested.join("source.socket");
    let _socket = UnixListener::bind(&socket_path).expect("Unix socket");

    let relative = run_source_check(&nested, "broken.orna").expect("relative source");
    assert_eq!(relative.status.code(), Some(1));
    assert!(relative.stdout.is_empty());
    assert_eq!(
        relative.stderr,
        b"broken.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n"
    );

    let absolute_path = nested.join("broken.orna");
    let absolute_text = absolute_path.to_str().expect("UTF-8 test path");
    let absolute = run_source_check(&directory.0, absolute_text).expect("absolute source");
    assert_eq!(absolute.status.code(), Some(1));
    assert!(absolute.stdout.is_empty());
    assert_eq!(
        String::from_utf8(absolute.stderr).expect("UTF-8 diagnostic"),
        format!("{absolute_text}:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n")
    );

    let linked = run_source_check(&nested, "linked.orna").expect("linked source");
    assert_eq!(linked.status.code(), Some(1));
    assert!(linked.stdout.is_empty());
    assert_eq!(
        linked.stderr,
        b"linked.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n"
    );

    for path in [
        "missing.orna",
        "dangling.orna",
        ".",
        "source.socket",
        "/dev/null",
    ] {
        assert_read_failure(
            &run_source_check(&nested, path).expect("non-regular source result"),
            path.as_bytes(),
        );
    }

    let denied = nested.join("denied.orna");
    fs::write(&denied, VALID_SOURCE).expect("denied source");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).expect("denied mode");
    if !nix::unistd::geteuid().is_root() {
        assert_read_failure(
            &run_source_check(&nested, "denied.orna").expect("permission result"),
            b"denied.orna",
        );
    }
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o600)).expect("restore mode");
}

#[test]
fn reads_exact_bytes_and_renders_ordered_byte_spans() {
    let directory = TestDirectory::new("bytes").expect("test directory");
    let path = directory.0.join("diagnostics.orna");

    fs::write(&path, b"CREATE SCHEMA ; CREATE SCHEMA ;").expect("ordered diagnostics source");
    let ordered = run_source_check(&directory.0, "diagnostics.orna").expect("diagnostics");
    assert_eq!(ordered.status.code(), Some(1));
    assert!(ordered.stdout.is_empty());
    assert_eq!(
        ordered.stderr,
        b"diagnostics.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n\
diagnostics.orna:30..31: ORNA0001: expected a schema name after CREATE SCHEMA\n"
    );

    let unicode_prefix = "-- é\r\n";
    fs::write(&path, format!("{unicode_prefix}CREATE SCHEMA ;")).expect("Unicode CRLF source");
    let unicode = run_source_check(&directory.0, "diagnostics.orna").expect("Unicode result");
    let start = unicode_prefix.len() + "CREATE SCHEMA ".len();
    assert_eq!(unicode.status.code(), Some(1));
    assert!(unicode.stdout.is_empty());
    assert_eq!(
        String::from_utf8(unicode.stderr).expect("UTF-8 diagnostic"),
        format!(
            "diagnostics.orna:{start}..{}: ORNA0001: expected a schema name after CREATE SCHEMA\n",
            start + 1
        )
    );

    let name = "a\\b\n\r\t\u{001b}\u{2028}\u{2029}é";
    let first = format!("CREATE SCHEMA \"{name}\";\n");
    let source = format!("{first}CREATE SCHEMA \"{name}\";");
    fs::write(&path, &source).expect("escaped diagnostic source");
    let escaped = run_source_check(&directory.0, "diagnostics.orna").expect("escaped result");
    let start = first.len() + "CREATE SCHEMA ".len();
    let end = start + name.len() + 2;
    assert_eq!(escaped.status.code(), Some(1));
    assert!(escaped.stdout.is_empty());
    assert_eq!(
        String::from_utf8(escaped.stderr).expect("UTF-8 diagnostic"),
        format!(
            "diagnostics.orna:{start}..{end}: ORNA0103: duplicate schema definition a\\\\b\\n\\r\\t\\u{{001B}}\\u{{2028}}\\u{{2029}}é\n"
        )
    );

    fs::write(&path, []).expect("empty source");
    assert_success(&run_source_check(&directory.0, "diagnostics.orna").expect("empty check"));
    fs::write(&path, "CREATE SCHEMA \"é\u{0301}\";").expect("exact Unicode source");
    assert_success(&run_source_check(&directory.0, "diagnostics.orna").expect("Unicode check"));
    let mut large = vec![b' '; 128 * 1024];
    large.extend_from_slice(VALID_SOURCE);
    fs::write(&path, large).expect("large source");
    assert_success(&run_source_check(&directory.0, "diagnostics.orna").expect("large check"));
}

#[test]
fn rejects_each_invalid_utf8_shape_without_compiler_output() {
    let directory = TestDirectory::new("utf8").expect("test directory");
    let path = directory.0.join("invalid.orna");
    for bytes in [
        vec![0xff],
        vec![0xe2, 0x82],
        b"CREATE SCHEMA app;\xff".to_vec(),
    ] {
        fs::write(&path, bytes).expect("invalid UTF-8 source");
        let output = run_source_check(&directory.0, "invalid.orna").expect("UTF-8 result");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"orna: source file is not valid UTF-8: invalid.orna\n"
        );
    }
}

#[test]
fn never_reads_neighbouring_source_units() {
    let directory = TestDirectory::new("one-unit").expect("test directory");
    fs::write(directory.0.join("schema.orna"), b"CREATE SCHEMA app;").expect("neighbouring schema");
    fs::write(
        directory.0.join("type.orna"),
        b"CREATE TYPE app.task AS OBJECT (done BOOLEAN);",
    )
    .expect("single checked unit");

    let output = run_source_check(&directory.0, "type.orna").expect("one-unit check");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"type.orna:12..20: ORNA0101: unknown schema app for object type app.task\n"
    );
    assert_read_failure(
        &run_source_check(&directory.0, ".").expect("directory result"),
        b".",
    );
}

#[test]
fn piped_input_and_hostile_environment_have_no_authority() {
    let directory = TestDirectory::new("offline").expect("test directory");
    fs::write(directory.0.join("application.orna"), VALID_SOURCE).expect("valid source");
    let child = spawn_orna(
        &directory.0,
        [
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("application.orna"),
        ],
        [
            (
                OsString::from("ORNA_PACKAGE_MAINTENANCE"),
                OsString::from("begin"),
            ),
            (
                OsString::from("ORNA_SERVER_POSTGRES_URL"),
                OsString::from("postgresql://hostile.invalid/wrong"),
            ),
            (
                OsString::from("DATABASE_URL"),
                OsString::from("postgresql://hostile.invalid/wrong"),
            ),
            (OsString::from("PGHOST"), OsString::from("hostile.invalid")),
            (OsString::from("PGPORT"), OsString::from("1")),
            (OsString::from("PGPASSWORD"), OsString::from("secret")),
            (OsString::from("HOME"), directory.0.clone().into_os_string()),
            (OsString::from("PATH"), OsString::from("/nonexistent")),
        ],
        Stdio::piped(),
    )
    .expect("offline source-check process");
    assert_success(&wait_bounded(child).expect("source check ignores open stdin"));
}

#[test]
fn raw_call_uses_only_the_fixed_endpoint_under_hostile_process_state() {
    assert!(!Path::new("/run/orna/default/orna.sock").exists());
    let directory = TestDirectory::new("raw-call").expect("test directory");
    let child = spawn_orna(
        &directory.0,
        [
            OsString::from("raw-call"),
            OsString::from("function:00000000000000000000000000"),
        ],
        [
            (
                OsString::from("ORNA_SOCKET"),
                directory.0.join("hostile.sock").into_os_string(),
            ),
            (OsString::from("HOME"), directory.0.clone().into_os_string()),
            (OsString::from("PATH"), OsString::from("/nonexistent")),
        ],
        Stdio::piped(),
    )
    .expect("raw-call process");
    let output = wait_bounded(child).expect("raw-call ignores open stdin");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, RAW_CALL_CONNECTION_FAILED);
}

#[test]
fn issues_no_filesystem_mutation_in_writable_or_read_only_directories() {
    let directory = TestDirectory::new("no-writes").expect("test directory");
    fs::write(directory.0.join("valid.orna"), VALID_SOURCE).expect("valid source");
    fs::write(directory.0.join("invalid.orna"), b"CREATE SCHEMA ;").expect("invalid source");
    let before = snapshot(&directory.0).expect("before snapshot");
    assert_success(&run_source_check(&directory.0, "valid.orna").expect("valid check"));
    let failed = run_source_check(&directory.0, "invalid.orna").expect("invalid check");
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    assert_eq!(
        failed.stderr,
        b"invalid.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n"
    );
    assert_eq!(snapshot(&directory.0).expect("after snapshot"), before);

    fs::set_permissions(
        directory.0.join("valid.orna"),
        fs::Permissions::from_mode(0o444),
    )
    .expect("read-only file");
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o555))
        .expect("read-only directory");
    assert_success(&run_source_check(&directory.0, "valid.orna").expect("read-only check"));
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700))
        .expect("restore directory mode");
}

#[test]
fn trace_has_no_child_network_or_write_authority_when_strace_is_available() {
    let strace = Path::new("/usr/bin/strace");
    if !strace.is_file() {
        return;
    }
    let directory = TestDirectory::new("trace").expect("test directory");
    fs::write(directory.0.join("application.orna"), VALID_SOURCE).expect("valid source");
    let trace = directory.0.join("source-check.strace");
    let output = Command::new(strace)
        .args([
            OsString::from("-f"),
            OsString::from("-qq"),
            OsString::from("-e"),
            OsString::from("trace=%process,%network,%file"),
            OsString::from("-o"),
            trace.clone().into_os_string(),
            OsString::from(env!("CARGO_BIN_EXE_orna")),
            OsString::from("source"),
            OsString::from("check"),
            OsString::from("application.orna"),
        ])
        .env_clear()
        .current_dir(&directory.0)
        .output()
        .expect("straced source check");
    assert_success(&output);
    let trace = fs::read_to_string(trace).expect("UTF-8 strace");
    for forbidden in [
        "clone(",
        "clone3(",
        "fork(",
        "vfork(",
        "socket(",
        "connect(",
        "bind(",
        "listen(",
        "accept(",
        "creat(",
        "mkdir(",
        "mkdirat(",
        "unlink(",
        "unlinkat(",
        "rename(",
        "renameat(",
        "truncate(",
        "ftruncate(",
        "chmod(",
        "fchmod(",
        "chown(",
        "fchown(",
        "O_WRONLY",
        "O_RDWR",
        "O_CREAT",
        "O_TRUNC",
        "O_APPEND",
        "/etc/orna",
        "/run/orna",
        "/var/lib/orna",
        "/usr/lib/orna",
    ] {
        assert!(
            !trace.contains(forbidden),
            "forbidden trace token {forbidden}"
        );
    }
    assert!(trace.contains("application.orna"));
}

#[test]
fn backend_shell_dispatch_still_uses_its_terminal_boundary() {
    let directory = TestDirectory::new("backend-shell").expect("test directory");
    let output = run_orna(
        &directory.0,
        [OsString::from("server"), OsString::from("backend-shell")],
    )
    .expect("backend-shell result");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, TERMINAL_REQUIRED);
}

#[test]
fn rejects_a_fifo_without_waiting_for_a_writer() {
    let directory = TestDirectory::new("fifo").expect("test directory");
    let fifo = directory.0.join("source.pipe");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("source fifo");

    let output = wait_bounded(
        spawn_source_check(&directory.0, OsString::from("source.pipe"))
            .expect("source-check process"),
    )
    .expect("bounded source-check result");
    assert_read_failure(&output, b"source.pipe");
}
