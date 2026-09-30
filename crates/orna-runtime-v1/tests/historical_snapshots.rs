use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    HistoricalSnapshot, NoFault, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
};
use tempfile::TempDir;

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("create temporary repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(
        directory.path(),
        &["config", "user.name", "kierandrewett"],
    );
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/historical_snapshot_main.orna"),
    )
    .expect("write fixture root module");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "initial snapshot"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn table_mutation(id: u8, key: u8, value: Option<&[u8]>) -> TableMutation {
    TableMutation::new(
        [id; 16],
        "records",
        vec![key],
        value.map(<[u8]>::to_vec),
    )
    .expect("valid typed table mutation")
}

async fn commit(
    state: &RuntimeState,
    writer: orna_runtime_v1::WriterLease,
    mutations: &[TableMutation],
    digest: u8,
) {
    let context = state.begin_activation().await.expect("capture activation");
    state
        .commit_table_activation(writer, &context, mutations, [digest; 32], &NoFault)
        .await
        .expect("commit table generation");
}

#[tokio::test]
async fn historical_reads_are_pinned_to_checkpoint_generations_and_cover_deletes() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");

    commit(&state, writer, &[table_mutation(5, 1, Some(b"one"))], 6).await;
    commit(
        &state,
        writer,
        &[
            table_mutation(7, 1, Some(b"two")),
            table_mutation(8, 2, Some(b"also two")),
        ],
        9,
    )
    .await;
    commit(&state, writer, &[table_mutation(10, 1, None)], 11).await;

    let generation_zero = state
        .select_historical_snapshot(0)
        .await
        .expect("select initialized generation");
    let generation_one = state
        .select_historical_snapshot(1)
        .await
        .expect("select first checkpoint generation");
    let generation_two = state
        .select_historical_snapshot(2)
        .await
        .expect("select second checkpoint generation");
    let generation_three = state
        .select_historical_snapshot(3)
        .await
        .expect("select third checkpoint generation");

    assert!(state
        .read_table_at(&generation_zero, "records")
        .await
        .expect("read initial generation")
        .rows()
        .is_empty());
    assert_eq!(
        state
            .read_table_at(&generation_one, "records")
            .await
            .expect("read first generation")
            .rows(),
        &[(vec![1], b"one".to_vec())]
    );
    assert_eq!(
        state
            .read_table_at(&generation_two, "records")
            .await
            .expect("read second generation")
            .rows(),
        &[
            (vec![1], b"two".to_vec()),
            (vec![2], b"also two".to_vec()),
        ]
    );
    assert_eq!(
        state
            .read_table_at(&generation_three, "records")
            .await
            .expect("read deletion generation")
            .rows(),
        &[(vec![2], b"also two".to_vec())]
    );
    assert_eq!(
        state
            .capture()
            .await
            .expect("read current capture")
            .generation()
            .to_string(),
        "3"
    );
    assert_eq!(
        generation_one.require_same_context(&generation_two),
        Err(RuntimeError::SnapshotContextMismatch)
    );
    let generation_one_rows = state
        .read_table_at(&generation_one, "records")
        .await
        .expect("read first generation with context");
    let generation_two_rows = state
        .read_table_at(&generation_two, "records")
        .await
        .expect("read second generation with context");
    assert_eq!(
        generation_one_rows.require_same_context(&generation_two_rows),
        Err(RuntimeError::SnapshotContextMismatch)
    );
    assert_eq!(
        state.select_historical_snapshot(4).await.unwrap_err(),
        RuntimeError::SnapshotNotFound
    );
    assert_eq!(
        generation_one.capture().generation().to_string(),
        "1",
        "later commits cannot move an already selected pin"
    );

    drop(state);
    let reopened = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("reopen runtime with retained row history");
    assert_eq!(
        reopened
            .read_table_at(&generation_one, "records")
            .await
            .expect("read old generation after restart")
            .rows(),
        &[(vec![1], b"one".to_vec())]
    );
}

#[tokio::test]
async fn historical_snapshot_cannot_be_rebound_to_another_runtime_identity() {
    let (_directory, first_repository) = repository();
    let (_other_directory, other_repository) = repository();
    let first = RuntimeState::open(
        &first_repository,
        RuntimeIdentity {
            database_id: [12; 16],
            repository_id: [13; 16],
        },
        [14; 32],
    )
    .await
    .expect("open first runtime");
    let first_pin = first
        .select_historical_snapshot(0)
        .await
        .expect("select first runtime generation");

    let second = RuntimeState::open(
        &other_repository,
        RuntimeIdentity {
            database_id: [22; 16],
            repository_id: [23; 16],
        },
        [24; 32],
    )
    .await
    .expect("open second runtime");
    assert_eq!(
        second.read_table_at(&first_pin, "records").await.unwrap_err(),
        RuntimeError::SnapshotContextMismatch
    );

    // The public snapshot type exposes no write operation; it remains a
    // context-bearing selection token for time-scoped reads only.
    let _: &HistoricalSnapshot = &first_pin;
}
