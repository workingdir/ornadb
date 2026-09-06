use std::{
    fmt::Write,
    process::{Command, Output},
};

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{CheckpointKey, Component, ConsumerIdentity, RuntimeIdentity, RuntimeState};
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
        source: checkpoint_component("example:sensors:v1"),
        partition_format: checkpoint_component("literal-list"),
        partition: checkpoint_component("null"),
        position_format: checkpoint_component("ordinal"),
    }
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
}

#[tokio::test(flavor = "current_thread")]
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
