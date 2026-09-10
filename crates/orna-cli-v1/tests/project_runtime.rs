use std::{
    fmt::Write,
    io::Write as _,
    process::{Command, Output, Stdio},
};

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{CheckpointKey, Component, ConsumerIdentity, RuntimeIdentity, RuntimeState};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn reference_project() -> TempDir {
    let directory = tempfile::tempdir().expect("reference project");
    let reference = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../reference/Orna-1.0.0/examples/reference");
    for name in [
        "main.orna",
        "library.orna",
        "warehouse.orna",
        "sensors.orna",
        "values.orna",
    ] {
        std::fs::copy(reference.join(name), directory.path().join(name)).expect("reference source");
    }
    directory
}

fn invoke(directory: &std::path::Path, command: &str, argument: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.to_str().expect("UTF-8 path"),
            command,
            argument,
        ])
        .output()
        .expect("CLI process")
}

fn identity(directory: &std::path::Path) -> (RuntimeIdentity, [u8; 32]) {
    let repository = Repository::discover(directory).expect("repository");
    let metadata = inspect_metadata(&repository)
        .expect("metadata inspection")
        .expect("initialized metadata");
    let database_id = *metadata.database_id().as_bytes();
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        [
            database_id[0],
            database_id[1],
            database_id[2],
            database_id[3],
            database_id[4],
            database_id[5],
            database_id[6],
            database_id[7],
            database_id[8],
            database_id[9],
            database_id[10],
            database_id[11],
            database_id[12],
            database_id[13],
            database_id[14],
            database_id[15],
            repository_id[0],
            repository_id[1],
            repository_id[2],
            repository_id[3],
            repository_id[4],
            repository_id[5],
            repository_id[6],
            repository_id[7],
            repository_id[8],
            repository_id[9],
            repository_id[10],
            repository_id[11],
            repository_id[12],
            repository_id[13],
            repository_id[14],
            repository_id[15],
        ],
    )
}

fn field(row: &[u8], name: &str) -> Option<OvbRaw> {
    let value = Value::decode(row).expect("canonical stored row");
    let OvbRaw::Map(fields) = value.raw() else {
        panic!("stored reference row is not a record");
    };
    fields.iter().find_map(|(key, value)| match key {
        OvbRaw::Text(key) if key == name => Some(value.clone()),
        _ => None,
    })
}

fn text(row: &[u8], name: &str) -> String {
    match field(row, name).expect("row field") {
        OvbRaw::Text(value) => value,
        _ => panic!("row field is not text"),
    }
}

fn integer(row: &[u8], name: &str) -> i64 {
    match field(row, name).expect("row field") {
        OvbRaw::Int(value) => value.try_into().expect("bounded integer"),
        _ => panic!("row field is not integer"),
    }
}

fn decimal(coefficient: i64, exponent10: i64) -> OvbRaw {
    Value::decimal(coefficient.into(), exponent10.into())
        .expect("exact decimal")
        .raw()
        .clone()
}

fn checkpoint_component(value: impl Into<String>) -> Component {
    Component::new(value).expect("checkpoint component")
}

fn sensors_list_payloads() -> Vec<Vec<u8>> {
    [
        (0, decimal(1825, -2)),
        (1, decimal(1850, -2)),
        (2, decimal(1875, -2)),
    ]
    .into_iter()
    .map(|(sequence, value)| {
        let mut fields = vec![
            ("sensor", OvbRaw::Text("greenhouse".into())),
            ("sequence", OvbRaw::Int(sequence.into())),
            ("value", value),
        ];
        fields.sort_by_cached_key(|(name, _)| {
            Value::new(OvbRaw::Text((*name).into()))
                .expect("canonical field name")
                .encode()
                .expect("encoded field name")
        });
        Value::new(OvbRaw::Map(
            fields
                .into_iter()
                .map(|(name, value)| (OvbRaw::Text(name.into()), value))
                .collect(),
        ))
        .expect("canonical Sample payload")
        .encode()
        .expect("encoded Sample payload")
    })
    .collect()
}

fn list_stream_identity(name: &str, payloads: &[Vec<u8>]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in payloads {
        digest.update(
            u64::try_from(payload.len())
                .expect("payload length fits u64")
                .to_be_bytes(),
        );
        digest.update(payload);
    }
    format!("{name}:{:x}", digest.finalize())
}

fn sensors_checkpoint_key(database_id: [u8; 16]) -> CheckpointKey {
    let database = database_id.iter().fold(
        String::with_capacity(database_id.len() * 2),
        |mut database, byte| {
            write!(&mut database, "{byte:02x}").expect("write database identity");
            database
        },
    );
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: checkpoint_component(format!("database:{database}")),
            root: checkpoint_component("public-function"),
            function: checkpoint_component("sensors.ingest"),
            binding: checkpoint_component("arguments:[]"),
        },
        source_format: checkpoint_component("orna-stream-v1"),
        source: checkpoint_component(list_stream_identity(
            "example:sensors:v1",
            &sensors_list_payloads(),
        )),
        partition_format: checkpoint_component("literal-list"),
        partition: None,
        position_format: checkpoint_component("ordinal"),
    }
}

#[test]
fn binary_repl_executes_a_pure_expression_at_the_cli_boundary() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl", "1 + 2"])
        .output()
        .expect("CLI process");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"3 : Int\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_repl_recovers_from_malformed_terminal_input() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(b"2\n\xff\n$_\n:quit\n")
        .expect("REPL input");
    let output = child.wait_with_output().expect("CLI process output");

    assert!(output.status.success());
    assert_eq!(
        output.stdout, b"> 2 : Int\n> error[ORNA-REPL-INPUT-UTF8]\n> 2 : Int\n> ",
        "malformed terminal input must not consume the retained last result"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_managed_local_repl_executes_a_project_standard_import() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    std::fs::write(
        directory.path().join("main.orna"),
        "use std.math; pub fn run(): Int = std.math.increment(41);",
    )
    .expect("project source");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(b"use std.math;\nstd.math.clamp(99, 20, 22)\n:quit\n")
        .expect("REPL input");
    let output = child.wait_with_output().expect("CLI process output");

    assert!(output.status.success());
    assert_eq!(
        output.stdout, b"> > 22 : Int\n> ",
        "managed-local REPL must return the typed bridge result"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_managed_local_repl_executes_integer_list_aggregates() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    std::fs::write(directory.path().join("main.orna"), "pub fn run(): Int = 0;")
        .expect("project source");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(
            b"sum([3, 1, 2])\nmin([3, 1, 2]) ?? 0\nmax([3, 1, 2]) ?? 0\nsum([])\nmin([])\nmax([])\n:quit\n",
        )
        .expect("REPL input");
    let output = child.wait_with_output().expect("CLI process output");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"> 6 : Int\n> 1 : Int\n> 3 : Int\n> 0 : Int\n> null : Null\n> null : Null\n> ",
        "managed-local REPL must preserve typed aggregate results and null extrema"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_status_porcelain_preserves_git_worktree_bytes_and_hides_discovery_paths() {
    let repository = tempfile::tempdir().expect("status repository");
    std::fs::write(repository.path().join("tracked.txt"), "before\n").expect("tracked file");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repository.path())
            .status()
            .expect("git init")
            .success()
    );
    for (key, value) in [
        ("user.name", "Orna Test"),
        ("user.email", "orna@example.invalid"),
    ] {
        assert!(
            Command::new("git")
                .args(["config", key, value])
                .current_dir(repository.path())
                .status()
                .expect("git config")
                .success()
        );
    }
    assert!(
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(repository.path())
            .status()
            .expect("git add")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "initial"
            ])
            .current_dir(repository.path())
            .status()
            .expect("git commit")
            .success()
    );
    std::fs::write(repository.path().join("tracked.txt"), "after\n").expect("modified file");
    std::fs::create_dir(repository.path().join("nested")).expect("untracked directory");
    std::fs::write(repository.path().join("nested/untracked.txt"), "new\n")
        .expect("untracked file");

    let expected = Command::new("git")
        .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .current_dir(repository.path())
        .output()
        .expect("git status");
    let actual = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repository.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("CLI status");
    assert!(actual.status.success());
    assert_eq!(actual.stdout, expected.stdout);
    assert!(actual.stderr.is_empty());

    let outside = tempfile::tempdir().expect("non-repository directory");
    let failure = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(outside.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("CLI status failure");
    assert!(!failure.status.success());
    assert_eq!(
        failure.stderr,
        b"error[E2100]: local Git worktree could not be discovered\nhelp: run the command inside a Git worktree or provide a local project path\n"
    );
    assert!(failure.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&failure.stderr).contains("status repository"));
}

#[tokio::test(flavor = "current_thread")]
async fn binary_reference_workflow_reopens_durable_rows_and_preserves_duplicate_failure() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());
    assert_eq!(init.stdout, b"initialized Orna repository\n");
    assert!(init.stderr.is_empty());

    let first = invoke(directory.path(), "run", "seed");
    assert!(first.status.success(), "seed stderr: {:?}", first.stderr);
    assert_eq!(first.stdout, b"invocation completed\n");
    assert!(first.stderr.is_empty());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after seed");
    let generation_after_seed = state
        .capture()
        .await
        .expect("seed capture")
        .generation()
        .clone();
    assert_eq!(state.committed_table_rows("Book").await.unwrap().len(), 2);
    assert_eq!(state.committed_table_rows("Loan").await.unwrap().len(), 0);
    let stock = state.committed_table_rows("Stock").await.unwrap();
    assert_eq!(stock.len(), 2);
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "north"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 12
    }));
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "south"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 4
    }));

    let duplicate = invoke(directory.path(), "run", "seed");
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("error[E2200]"));
    assert!(
        !String::from_utf8_lossy(&duplicate.stderr)
            .contains(directory.path().to_string_lossy().as_ref())
    );
    let duplicate_state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after duplicate");
    assert_eq!(
        duplicate_state
            .capture()
            .await
            .expect("duplicate capture")
            .generation(),
        &generation_after_seed
    );
    assert_eq!(
        duplicate_state
            .committed_table_rows("Book")
            .await
            .unwrap()
            .len(),
        2
    );

    let exercise = invoke(directory.path(), "run", "exercise");
    assert!(
        exercise.status.success(),
        "exercise stderr: {:?}",
        exercise.stderr
    );
    assert_eq!(exercise.stdout, b"invocation completed\n");
    assert!(exercise.stderr.is_empty());
    let final_state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after exercise");
    assert!(final_state.capture().await.unwrap().generation() > &generation_after_seed);
    let loans = final_state.committed_table_rows("Loan").await.unwrap();
    assert_eq!(loans.len(), 1);
    assert_eq!(text(&loans[0].1, "book_id"), "book-1");
    assert_eq!(text(&loans[0].1, "borrower"), "reader-1");
    let stock = final_state.committed_table_rows("Stock").await.unwrap();
    assert_eq!(stock.len(), 2);
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "north"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 9
    }));
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "south"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 7
    }));

    let generation_after_exercise = final_state
        .capture()
        .await
        .expect("exercise capture")
        .generation()
        .clone();
    let books_after_exercise = final_state.committed_table_rows("Book").await.unwrap();
    let loans_after_exercise = final_state.committed_table_rows("Loan").await.unwrap();
    let stock_after_exercise = final_state.committed_table_rows("Stock").await.unwrap();
    drop(final_state);

    let duplicate_exercise = invoke(directory.path(), "run", "exercise");
    assert!(!duplicate_exercise.status.success());
    let stderr = String::from_utf8_lossy(&duplicate_exercise.stderr);
    assert!(stderr.contains("error[E2200]"));
    assert!(stderr.contains("failed atomically"));
    assert!(!stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_rejected_exercise = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after rejected duplicate exercise");
    assert_eq!(
        after_rejected_exercise
            .capture()
            .await
            .expect("duplicate exercise capture")
            .generation(),
        &generation_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("Book")
            .await
            .unwrap(),
        books_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("Loan")
            .await
            .unwrap(),
        loans_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("Stock")
            .await
            .unwrap(),
        stock_after_exercise
    );
}

#[tokio::test(flavor = "current_thread")]
async fn binary_reference_library_lend_rejects_invalid_and_duplicate_rows_without_publication() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());

    let seed = invoke(directory.path(), "run", "seed");
    assert!(seed.status.success(), "seed stderr: {:?}", seed.stderr);

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let seeded = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after seed");
    let generation_after_seed = seeded
        .capture()
        .await
        .expect("seed capture")
        .generation()
        .clone();
    let books_after_seed = seeded.committed_table_rows("Book").await.unwrap();
    drop(seeded);

    let missing = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "missing-book",
            "reader-2",
        ])
        .output()
        .expect("CLI process");
    assert!(!missing.status.success());
    let missing_stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(missing_stderr.contains("error[E2200]"));
    assert!(missing_stderr.contains("failed atomically"));
    assert!(!missing_stderr.contains("missing-book"));
    assert!(!missing_stderr.contains("reader-2"));
    assert!(!missing_stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_missing = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after missing-book loan");
    assert_eq!(
        after_missing
            .capture()
            .await
            .expect("missing capture")
            .generation(),
        &generation_after_seed
    );
    assert_eq!(
        after_missing.committed_table_rows("Book").await.unwrap(),
        books_after_seed
    );
    assert!(
        after_missing
            .committed_table_rows("Loan")
            .await
            .unwrap()
            .is_empty()
    );
    drop(after_missing);

    let first_lend = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "book-1",
            "reader-1",
        ])
        .output()
        .expect("CLI process");
    assert!(
        first_lend.status.success(),
        "lend stderr: {:?}",
        first_lend.stderr
    );

    let lent = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after first loan");
    let generation_after_lend = lent
        .capture()
        .await
        .expect("lend capture")
        .generation()
        .clone();
    let loans_after_lend = lent.committed_table_rows("Loan").await.unwrap();
    assert_eq!(loans_after_lend.len(), 1);
    assert_eq!(text(&loans_after_lend[0].1, "book_id"), "book-1");
    assert_eq!(text(&loans_after_lend[0].1, "borrower"), "reader-1");
    drop(lent);

    let duplicate = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "book-1",
            "reader-2",
        ])
        .output()
        .expect("CLI process");
    assert!(!duplicate.status.success());
    let duplicate_stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(duplicate_stderr.contains("error[E2200]"));
    assert!(duplicate_stderr.contains("failed atomically"));
    assert!(!duplicate_stderr.contains("book-1"));
    assert!(!duplicate_stderr.contains("reader-2"));
    assert!(!duplicate_stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_duplicate = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after duplicate loan");
    assert_eq!(
        after_duplicate
            .capture()
            .await
            .expect("duplicate capture")
            .generation(),
        &generation_after_lend
    );
    assert_eq!(
        after_duplicate.committed_table_rows("Loan").await.unwrap(),
        loans_after_lend
    );
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn binary_sensors_ingest_reopens_typed_rows_and_checkpoint() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());
    assert_eq!(init.stdout, b"initialized Orna repository\n");
    assert!(init.stderr.is_empty());

    let first = invoke(directory.path(), "run", "sensors.ingest");
    assert!(first.status.success(), "sensors invocation failed");
    assert_eq!(first.stdout, b"invocation completed\n");
    assert!(first.stderr.is_empty());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after sensors invocation");
    let readings = state
        .committed_table_rows("Reading")
        .await
        .expect("read durable readings");
    assert_eq!(readings.len(), 3);
    for (sequence, value) in [
        (0, decimal(1825, -2)),
        (1, decimal(1850, -2)),
        (2, decimal(1875, -2)),
    ] {
        assert!(readings.iter().any(|(_, row)| {
            text(row, "sensor") == "greenhouse"
                && integer(row, "sequence") == sequence
                && field(row, "value") == Some(value.clone())
        }));
    }

    let key = sensors_checkpoint_key(runtime_identity.database_id);
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("read sensors checkpoint");
    assert_eq!(
        checkpoint
            .committed
            .as_ref()
            .map(|position| position.token.as_str()),
        Some("3")
    );
    drop(state);

    let second = invoke(directory.path(), "run", "sensors.ingest");
    assert!(second.status.success(), "second sensors invocation failed");
    assert_eq!(second.stdout, b"invocation completed\n");
    assert!(second.stderr.is_empty());

    let reopened = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after second sensors invocation");
    assert_eq!(
        reopened
            .committed_table_rows("Reading")
            .await
            .expect("read durable readings after restart"),
        readings
    );
    assert_eq!(
        reopened
            .stream_checkpoint(&key)
            .await
            .expect("read sensors checkpoint after restart"),
        checkpoint
    );
}
