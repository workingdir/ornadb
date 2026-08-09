#![cfg(unix)]

use nix::pty::openpty;
use orna_kernel_postgres::PostgresKernel;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read},
    net::TcpListener,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    str,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use tokio_postgres::{Client, config::Host};
use url::Url;

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const USAGE: &[u8] = b"Usage: orna server backend-shell\n";
const TERMINAL_REQUIRED: &[u8] = b"orna: backend-shell must be run in an interactive terminal\n";
const MISSING_CONFIGURATION: &[u8] = b"orna: backend-shell needs ORNA_SERVER_POSTGRES_URL\n";
const INVALID_CONFIGURATION: &[u8] =
    b"orna: ORNA_SERVER_POSTGRES_URL must use postgresql://user[:password]@host:port/database\n";
const PSQL_UNAVAILABLE: &[u8] = b"orna: could not start psql from PATH\n";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const VALID_URL: &str = "postgresql://operator@db.example:5432/catalogue";

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> io::Result<Self> {
        let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "orna-backend-shell-{}-{id}-{label}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct DurableSnapshot {
    relations: Vec<String>,
    columns: Vec<String>,
    indexes: Vec<String>,
    constraints: Vec<String>,
    rows: Vec<(String, Vec<String>)>,
}

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

fn run_without_terminal(arguments: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .output()
        .expect("orna process starts")
}

fn assert_usage(output: &Output) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, USAGE);
}

fn run_in_pty(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    current_directory: &Path,
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
        .current_dir(current_directory);

    command.stdin(match non_terminal {
        Some(NonTerminalStream::Stdin) => Stdio::null(),
        _ => Stdio::from(stdin_slave.try_clone()?),
    });
    command.stdout(match non_terminal {
        Some(NonTerminalStream::Stdout) => Stdio::piped(),
        _ => Stdio::from(stdout_slave.try_clone()?),
    });
    command.stderr(match non_terminal {
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
    let mut normalised = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"\r\n") {
            normalised.push(b'\n');
            index += 2;
        } else {
            normalised.push(bytes[index]);
            index += 1;
        }
    }
    normalised
}

fn base_environment(path: &OsStr, url: Option<&str>) -> Vec<(OsString, OsString)> {
    let mut environment = vec![(OsString::from("PATH"), path.to_owned())];
    if let Some(url) = url {
        environment.push((
            OsString::from("ORNA_SERVER_POSTGRES_URL"),
            OsString::from(url),
        ));
    }
    environment
}

fn write_fake_psql(directory: &Path) -> io::Result<PathBuf> {
    write_fake_psql_with_marker(directory, None)
}

fn write_fake_psql_with_marker(
    directory: &Path,
    marker_environment: Option<&str>,
) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let executable = directory.join("psql");
    let mut script = String::from("#!/bin/bash\nset -euo pipefail\n");
    if let Some(marker_environment) = marker_environment {
        script.push_str(&format!(": > \"${{{marker_environment}}}\"\n"));
    }
    script.push_str(
        "printf '%s\\000' \"$@\" > \"${ORNA_TEST_RECORD}.args\"\n\
         /usr/bin/env -0 > \"${ORNA_TEST_RECORD}.env\"\n\
         case \"${ORNA_TEST_MODE:-exit:0}\" in\n\
           exit:*) exit \"${ORNA_TEST_MODE#exit:}\" ;;\n\
           signal:*) signal=\"${ORNA_TEST_MODE#signal:}\"; ulimit -c 0; exec /usr/bin/env --default-signal=\"$signal\" /bin/kill -\"$signal\" \"$$\" ;;\n\
           *) exit 126 ;;\n\
         esac\n",
    );
    fs::write(&executable, script)?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions)?;
    Ok(executable)
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_nul_values(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path)?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

fn read_environment(path: &Path) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut environment = BTreeMap::new();
    for value in read_nul_values(path)? {
        let Some(separator) = value.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        environment.insert(value[..separator].to_vec(), value[separator + 1..].to_vec());
    }
    Ok(environment)
}

fn require_environment(environment: &BTreeMap<Vec<u8>, Vec<u8>>, name: &[u8], expected: &[u8]) {
    assert!(
        environment
            .get(name)
            .is_some_and(|actual| actual == expected),
        "child environment value was wrong for {}",
        String::from_utf8_lossy(name)
    );
}

fn require_absent(environment: &BTreeMap<Vec<u8>, Vec<u8>>, name: &[u8]) {
    assert!(
        !environment.contains_key(name),
        "child environment unexpectedly contained {}",
        String::from_utf8_lossy(name)
    );
}

async fn durable_snapshot(database: &TestDatabase) -> TestResult<DurableSnapshot> {
    let session = database.open().await?;
    let operation = async {
        let relations = query_strings(
            session.client(),
            "SELECT format('%s.%s:%s', n.nspname, c.relname, c.relkind::text)
             FROM pg_catalog.pg_class AS c
             JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
             WHERE n.nspname IN ('_orna_kernel', '_orna_data')
             ORDER BY n.nspname, c.relname, c.relkind",
        )
        .await?;
        let columns = query_strings(
            session.client(),
            "SELECT format('%s.%s:%s:%s:%s:%s', n.nspname, c.relname, a.attnum,
                           a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod),
                           a.attnotnull)
             FROM pg_catalog.pg_attribute AS a
             JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid
             JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
             WHERE n.nspname IN ('_orna_kernel', '_orna_data')
               AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY n.nspname, c.relname, a.attnum",
        )
        .await?;
        let indexes = query_strings(
            session.client(),
            "SELECT format('%s.%s:%s', schemaname, indexname, indexdef)
             FROM pg_catalog.pg_indexes
             WHERE schemaname IN ('_orna_kernel', '_orna_data')
             ORDER BY schemaname, indexname",
        )
        .await?;
        let constraints = query_strings(
            session.client(),
            "SELECT format('%s.%s:%s:%s', n.nspname, c.relname, constraint_row.conname,
                           pg_catalog.pg_get_constraintdef(constraint_row.oid))
             FROM pg_catalog.pg_constraint AS constraint_row
             JOIN pg_catalog.pg_class AS c ON c.oid = constraint_row.conrelid
             JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
             WHERE n.nspname IN ('_orna_kernel', '_orna_data')
             ORDER BY n.nspname, c.relname, constraint_row.conname",
        )
        .await?;
        let table_rows = session
            .client()
            .query(
                "SELECT schemaname, tablename
                 FROM pg_catalog.pg_tables
                 WHERE schemaname IN ('_orna_kernel', '_orna_data')
                 ORDER BY schemaname, tablename",
                &[],
            )
            .await?;
        let mut rows = Vec::with_capacity(table_rows.len());
        for table in table_rows {
            let schema = table.get::<_, String>(0);
            let table = table.get::<_, String>(1);
            if !safe_identifier(&schema) || !safe_identifier(&table) {
                return Err(failure(
                    "durable PostgreSQL relation has an unsafe test identifier",
                ));
            }
            let values = query_strings(
                session.client(),
                &format!(
                    "SELECT to_jsonb(snapshot_row)::text
                     FROM \"{schema}\".\"{table}\" AS snapshot_row
                     ORDER BY to_jsonb(snapshot_row)::text"
                ),
            )
            .await?;
            rows.push((format!("{schema}.{table}"), values));
        }

        Ok(DurableSnapshot {
            relations,
            columns,
            indexes,
            constraints,
            rows,
        })
    }
    .await;
    let shutdown = session.shutdown().await;
    finish_live_operation("durable snapshot", operation, shutdown)
}

async fn query_strings(client: &Client, statement: &str) -> TestResult<Vec<String>> {
    Ok(client
        .query(statement, &[])
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect())
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn finish_live_operation<T>(
    operation_name: &str,
    operation: TestResult<T>,
    shutdown: TestResult<()>,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(failure(format!("{operation_name} failed: {error}"))),
        (Ok(_), Err(error)) => Err(failure(format!(
            "{operation_name} connection shutdown failed: {error}"
        ))),
        (Err(operation), Err(shutdown)) => Err(failure(format!(
            "{operation_name} failed: {operation}; connection shutdown failed: {shutdown}"
        ))),
    }
}

fn require_live(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn record_environment(
    directory: &TestDirectory,
    label: &str,
    url: &str,
    mode: &str,
) -> io::Result<(PtyOutput, PathBuf)> {
    let bin = directory.path().join(format!("bin-{label}"));
    write_fake_psql(&bin)?;
    let record = directory.path().join(format!("record-{label}"));
    let mut environment = base_environment(bin.as_os_str(), Some(url));
    environment.extend([
        (
            OsString::from("ORNA_TEST_RECORD"),
            record.as_os_str().to_owned(),
        ),
        (OsString::from("ORNA_TEST_MODE"), OsString::from(mode)),
    ]);
    let output = run_in_pty(environment, directory.path(), None)?;
    Ok((output, record))
}

fn server_url(database: &TestDatabase) -> TestResult<String> {
    let config = database.config()?;
    let [Host::Tcp(host)] = config.get_hosts() else {
        return Err(failure(
            "live backend-shell test requires one TCP PostgreSQL host",
        ));
    };
    let [port] = config.get_ports() else {
        return Err(failure(
            "live backend-shell test requires one explicit PostgreSQL port",
        ));
    };
    let user = config
        .get_user()
        .ok_or_else(|| failure("live backend-shell test requires an explicit PostgreSQL user"))?;
    let password = config.get_password().map(str::from_utf8).transpose()?;

    let mut url = Url::parse("postgresql://placeholder@localhost:1/database")?;
    url.set_host(Some(host))
        .map_err(|_| failure("live PostgreSQL host cannot be represented as a URL"))?;
    url.set_port(Some(*port))
        .map_err(|_| failure("live PostgreSQL port cannot be represented as a URL"))?;
    url.set_username(user)
        .map_err(|_| failure("live PostgreSQL user cannot be represented as a URL"))?;
    url.set_password(password)
        .map_err(|_| failure("live PostgreSQL password cannot be represented as a URL"))?;
    let database_name = config
        .get_dbname()
        .ok_or_else(|| failure("live backend-shell test requires an explicit database"))?;
    url.set_path(&format!("/{database_name}"));
    Ok(url.into())
}

async fn prove_attach_only_launch(database: &TestDatabase) -> TestResult<()> {
    let directory = TestDirectory::new("live-storage")?;
    let server_url = server_url(database)?;

    let fresh_before = durable_snapshot(database).await?;
    let (fresh_launch, fresh_record) =
        record_environment(&directory, "fresh", &server_url, "exit:0")?;
    require_live(
        fresh_launch.status.code() == Some(0)
            && fresh_launch.stdout.is_empty()
            && fresh_launch.stderr.is_empty(),
        "backend-shell did not replace itself cleanly against fresh storage",
    )?;
    require_live(
        read_nul_values(&suffixed_path(&fresh_record, ".args"))? == vec![b"--no-psqlrc".to_vec()],
        "fresh-storage fake psql received unexpected arguments",
    )?;
    require_live(
        durable_snapshot(database).await? == fresh_before,
        "backend-shell launch changed fresh PostgreSQL storage",
    )?;

    database
        .connection_string()
        .parse::<PostgresKernel>()?
        .bootstrap()
        .await?;
    let existing_before = durable_snapshot(database).await?;
    let serving_session = database.open().await?;
    let operation = async {
        let (successful_launch, successful_record) =
            record_environment(&directory, "existing", &server_url, "exit:0")?;
        require_live(
            successful_launch.status.code() == Some(0)
                && successful_launch.stdout.is_empty()
                && successful_launch.stderr.is_empty(),
            "backend-shell did not replace itself cleanly against existing storage",
        )?;
        require_live(
            read_nul_values(&suffixed_path(&successful_record, ".args"))?
                == vec![b"--no-psqlrc".to_vec()],
            "existing-storage fake psql received unexpected arguments",
        )?;
        require_live(
            durable_snapshot(database).await? == existing_before,
            "successful backend-shell launch changed durable Orna storage",
        )?;

        let missing_path = directory.path().join("missing-psql");
        fs::create_dir(&missing_path)?;
        let failed_launch = run_in_pty(
            base_environment(missing_path.as_os_str(), Some(&server_url)),
            directory.path(),
            None,
        )?;
        require_live(
            failed_launch.status.code() == Some(1)
                && failed_launch.stdout.is_empty()
                && failed_launch.stderr == PSQL_UNAVAILABLE,
            "missing psql did not return the exact pre-launch failure",
        )?;
        require_live(
            durable_snapshot(database).await? == existing_before,
            "failed backend-shell launch changed durable Orna storage",
        )
    }
    .await;
    let shutdown = serving_session.shutdown().await;
    finish_live_operation("attach-only launch proof", operation, shutdown)
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn launch_does_not_change_fresh_or_existing_durable_storage() -> TestResult<()> {
    with_test_database(|database| async move { prove_attach_only_launch(&database).await }).await
}

#[test]
fn command_shape_failures_have_exact_process_results() {
    for arguments in [
        vec![],
        vec![OsString::from("server")],
        vec![OsString::from("backend-shell")],
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
fn each_standard_stream_must_be_a_terminal_before_configuration_or_launch() {
    let directory = TestDirectory::new("terminal").expect("temporary directory");
    let bin = directory.path().join("bin");
    write_fake_psql(&bin).expect("fake psql");
    let record = directory.path().join("must-not-exist");

    for stream in [
        NonTerminalStream::Stdin,
        NonTerminalStream::Stdout,
        NonTerminalStream::Stderr,
    ] {
        let environment = vec![
            (OsString::from("PATH"), bin.as_os_str().to_owned()),
            (
                OsString::from("ORNA_SERVER_POSTGRES_URL"),
                OsString::from("invalid-configuration-containing-super-secret"),
            ),
            (
                OsString::from("ORNA_TEST_RECORD"),
                record.as_os_str().to_owned(),
            ),
        ];
        let output = run_in_pty(environment, directory.path(), Some(stream)).expect("orna run");

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, TERMINAL_REQUIRED);
        assert!(!suffixed_path(&record, ".args").exists());
        assert!(!suffixed_path(&record, ".env").exists());
    }
}

#[test]
fn configuration_failures_are_exact_and_redacted_in_a_terminal() {
    let directory = TestDirectory::new("configuration").expect("temporary directory");
    let path = directory.path().join("unused-bin");
    for (url, expected) in [
        (None, MISSING_CONFIGURATION),
        (Some(""), MISSING_CONFIGURATION),
        (
            Some("not-a-valid-URL-containing-super-secret"),
            INVALID_CONFIGURATION,
        ),
    ] {
        let output = run_in_pty(
            base_environment(path.as_os_str(), url),
            directory.path(),
            None,
        )
        .expect("orna run");

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, expected);
        assert!(
            !output
                .stderr
                .windows(12)
                .any(|part| part == b"super-secret")
        );
    }
}

#[test]
fn child_receives_only_the_selected_connection_inputs_and_exact_argument() {
    let directory = TestDirectory::new("environment").expect("temporary directory");
    let bin = directory.path().join("bin with ;$ metacharacters");
    write_fake_psql(&bin).expect("fake psql");
    let record = directory.path().join("record");
    let home = directory.path().join("hostile-home");
    fs::create_dir_all(home.join(".postgresql")).expect("hostile TLS directory");
    fs::write(home.join(".pgpass"), b"hostile-password").expect("hostile passfile");
    fs::write(home.join(".psqlrc"), b"\\set hostile on\n").expect("hostile psqlrc");
    fs::write(home.join(".postgresql/postgresql.crt"), b"hostile-cert")
        .expect("hostile certificate");
    fs::write(directory.path().join("compose.yaml"), b"hostile-compose")
        .expect("hostile compose file");

    let url = "postgresql://user%3B%24%28touch%20PWNED%29:p%24%28touch%20PWNED%29@db.example:5433/catalogue%3B%24%28touch%20PWNED%29";
    let mut environment = base_environment(bin.as_os_str(), Some(url));
    environment.extend([
        (
            OsString::from("ORNA_TEST_RECORD"),
            record.as_os_str().to_owned(),
        ),
        (OsString::from("ORNA_TEST_MODE"), OsString::from("exit:0")),
        (OsString::from("HOME"), home.as_os_str().to_owned()),
        (OsString::from("KEEP_ME"), OsString::from("retained")),
        (
            OsString::from("DATABASE_URL"),
            OsString::from("postgresql://hostile"),
        ),
        (OsString::from("PGHOST"), OsString::from("hostile")),
        (OsString::from("PGPORT"), OsString::from("1")),
        (OsString::from("PGUSER"), OsString::from("hostile")),
        (OsString::from("PGDATABASE"), OsString::from("hostile")),
        (OsString::from("PGPASSWORD"), OsString::from("hostile")),
        (OsString::from("PGSERVICE"), OsString::from("hostile")),
        (OsString::from("PGPASSFILE"), OsString::from("hostile")),
        (OsString::from("PGOPTIONS"), OsString::from("hostile")),
        (OsString::from("PGSSLMODE"), OsString::from("require")),
        (OsString::from("PGGSSENCMODE"), OsString::from("prefer")),
        (
            OsString::from_vec(b"PG\xffHOST".to_vec()),
            OsString::from("hostile"),
        ),
    ]);

    let output = run_in_pty(environment, directory.path(), None).expect("orna run");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!directory.path().join("PWNED").exists());

    let arguments = read_nul_values(&suffixed_path(&record, ".args")).expect("arguments record");
    assert_eq!(arguments, vec![b"--no-psqlrc".to_vec()]);
    let environment = read_environment(&suffixed_path(&record, ".env")).expect("environment");
    require_environment(&environment, b"PGHOST", b"db.example");
    require_environment(&environment, b"PGPORT", b"5433");
    require_environment(&environment, b"PGUSER", b"user;$(touch PWNED)");
    require_environment(&environment, b"PGDATABASE", b"catalogue;$(touch PWNED)");
    require_environment(&environment, b"PGPASSWORD", b"p$(touch PWNED)");
    require_environment(&environment, b"PGPASSFILE", b"/dev/null");
    require_environment(&environment, b"PGSSLMODE", b"disable");
    require_environment(&environment, b"PGGSSENCMODE", b"disable");
    require_environment(&environment, b"KEEP_ME", b"retained");
    require_environment(&environment, b"DATABASE_URL", b"postgresql://hostile");
    require_absent(&environment, b"ORNA_SERVER_POSTGRES_URL");
    require_absent(&environment, b"PGSERVICE");
    require_absent(&environment, b"PGOPTIONS");
    require_absent(&environment, b"PG\xffHOST");
}

#[test]
fn absent_empty_and_present_password_states_reach_psql_exactly() {
    let directory = TestDirectory::new("passwords").expect("temporary directory");
    for (label, url, expected) in [
        ("absent", VALID_URL, None),
        (
            "empty",
            "postgresql://operator:@db.example:5432/catalogue",
            Some(&b""[..]),
        ),
        (
            "present",
            "postgresql://operator:secret@db.example:5432/catalogue",
            Some(&b"secret"[..]),
        ),
    ] {
        let (output, record) =
            record_environment(&directory, label, url, "exit:0").expect("orna run");
        assert_eq!(output.status.code(), Some(0));
        let environment = read_environment(&suffixed_path(&record, ".env")).expect("environment");
        match expected {
            Some(password) => require_environment(&environment, b"PGPASSWORD", password),
            None => require_absent(&environment, b"PGPASSWORD"),
        }
    }
}

#[test]
fn path_search_is_absolute_ordered_and_never_falls_back_after_exec_failure() {
    let directory = TestDirectory::new("path").expect("temporary directory");
    let relative = directory.path().join("relative");
    let non_executable = directory.path().join("non-executable");
    let selected = directory.path().join("selected");
    let later = directory.path().join("later");
    write_fake_psql(&relative).expect("relative fake");
    let non_executable_path = write_fake_psql(&non_executable).expect("non-executable fake");
    fs::set_permissions(&non_executable_path, fs::Permissions::from_mode(0o600))
        .expect("remove execute bits");
    write_fake_psql_with_marker(&selected, Some("ORNA_TEST_SELECTED_MARKER"))
        .expect("selected fake");
    write_fake_psql_with_marker(&later, Some("ORNA_TEST_LATER_MARKER")).expect("later fake");

    let record = directory.path().join("selected-record");
    let later_record = directory.path().join("later-record");
    let path = OsString::from(format!(
        ":relative:/missing:{}:{}:{}",
        non_executable.display(),
        selected.display(),
        later.display()
    ));
    let mut environment = base_environment(&path, Some(VALID_URL));
    environment.extend([
        (
            OsString::from("ORNA_TEST_RECORD"),
            record.as_os_str().to_owned(),
        ),
        (
            OsString::from("ORNA_TEST_SELECTED_MARKER"),
            record.as_os_str().to_owned(),
        ),
        (
            OsString::from("ORNA_TEST_LATER_MARKER"),
            later_record.as_os_str().to_owned(),
        ),
        (OsString::from("ORNA_TEST_MODE"), OsString::from("exit:0")),
    ]);
    let output = run_in_pty(environment, directory.path(), None).expect("orna run");
    assert_eq!(output.status.code(), Some(0));
    assert!(suffixed_path(&record, ".args").exists());
    assert!(record.exists());
    assert!(!later_record.exists());

    let broken = directory.path().join("broken");
    fs::create_dir(&broken).expect("broken directory");
    let broken_psql = broken.join("psql");
    fs::write(&broken_psql, b"not an executable image").expect("broken executable");
    fs::set_permissions(&broken_psql, fs::Permissions::from_mode(0o700))
        .expect("broken execute bits");
    let fallback_record = directory.path().join("fallback-record");
    let path = OsString::from(format!("{}:{}", broken.display(), later.display()));
    let mut environment = base_environment(&path, Some(VALID_URL));
    environment.extend([
        (
            OsString::from("ORNA_TEST_RECORD"),
            fallback_record.as_os_str().to_owned(),
        ),
        (
            OsString::from("ORNA_TEST_LATER_MARKER"),
            later_record.as_os_str().to_owned(),
        ),
        (OsString::from("ORNA_TEST_MODE"), OsString::from("exit:0")),
    ]);
    let output = run_in_pty(environment, directory.path(), None).expect("orna run");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, PSQL_UNAVAILABLE);
    assert!(!suffixed_path(&fallback_record, ".args").exists());
    assert!(!later_record.exists());
}

#[test]
fn missing_empty_relative_and_unusable_paths_fail_without_platform_fallback() {
    let directory = TestDirectory::new("bad-path").expect("temporary directory");
    let relative = directory.path().join("relative");
    write_fake_psql(&relative).expect("relative fake");
    for path in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("relative:/missing")),
    ] {
        let environment = match path {
            Some(path) => base_environment(path, Some(VALID_URL)),
            None => vec![(
                OsString::from("ORNA_SERVER_POSTGRES_URL"),
                OsString::from(VALID_URL),
            )],
        };
        let output = run_in_pty(environment, directory.path(), None).expect("orna run");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, PSQL_UNAVAILABLE);
    }
}

#[test]
fn unavailable_backend_is_left_to_psql_after_process_replacement() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind non-PostgreSQL endpoint");
    let address = listener.local_addr().expect("reserved TCP address");
    listener
        .set_nonblocking(true)
        .expect("make non-PostgreSQL endpoint observable");

    let directory = TestDirectory::new("unavailable-backend").expect("temporary directory");
    let url = format!(
        "postgresql://operator@127.0.0.1:{}/catalogue",
        address.port()
    );
    let (output, record) =
        record_environment(&directory, "unavailable", &url, "exit:37").expect("orna run");

    assert_eq!(output.status.code(), Some(37));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        read_nul_values(&suffixed_path(&record, ".args")).expect("arguments record"),
        vec![b"--no-psqlrc".to_vec()]
    );
    assert!(
        listener
            .accept()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "Orna must not contact the configured endpoint before replacing itself"
    );
}

#[test]
fn replacement_preserves_exit_codes_and_terminating_signals() {
    let directory = TestDirectory::new("process-results").expect("temporary directory");
    for (label, mode, expected) in [("zero", "exit:0", 0), ("nonzero", "exit:37", 37)] {
        let (output, _) = record_environment(&directory, label, VALID_URL, mode).expect("orna run");
        assert_eq!(output.status.code(), Some(expected));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    for (name, signal) in [("HUP", 1), ("INT", 2), ("QUIT", 3), ("TERM", 15)] {
        let mode = format!("signal:{name}");
        let (output, _) =
            record_environment(&directory, &format!("signal-{name}"), VALID_URL, &mode)
                .expect("orna run");
        assert_eq!(output.status.signal(), Some(signal));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
