//! Admin failure audit rows record the admitted generation and a stable class,
//! and replaying a stored failure reproduces the original `RuntimeError`.
use std::{path::PathBuf, process::Command};

use orna_foundation_v1::CwdCapture;
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, Mutation, NoFault, RuntimeError, RuntimeIdentity,
    RuntimeState,
};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-admin-failure-audit-")
        .tempdir_in(target)
        .expect("create repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize repository");
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

async fn open(repository: &Repository, repository_id: [u8; 16]) -> RuntimeState {
    RuntimeState::open(
        repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id,
        },
        [3; 32],
    )
    .await
    .expect("open runtime")
}

fn key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("consume").unwrap(),
            binding: Component::new("binding").unwrap(),
        },
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("source").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: Some(Component::new("partition").unwrap()),
        position_format: Component::new("position-format-v1").unwrap(),
    }
}

fn mutation(id: u8, payload: &[u8]) -> Mutation {
    Mutation {
        id: [id; 16],
        payload: payload.to_vec(),
        digest: Sha256::digest(payload).into(),
    }
}

fn generation_of(capture: &CwdCapture) -> u64 {
    u64::try_from(
        capture
            .generation()
            .to_string()
            .parse::<i64>()
            .expect("generation fits i64"),
    )
    .expect("generation is non-negative")
}

#[tokio::test]
async fn stale_capture_failure_records_admitted_generation_and_replays_same_error() {
    let (_directory, repository) = repository();
    let state = open(&repository, [2; 16]).await;
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");

    // The caller holds a capture that a committed activation then makes stale.
    let stale = state.capture().await.expect("capture before commit");
    let advanced = state
        .commit(
            writer,
            &stale,
            &mutation(5, b"advance generation"),
            [6; 32],
            &NoFault,
        )
        .await
        .expect("commit advances the CWD generation");
    assert_ne!(stale, advanced, "commit must advance the CWD capture");
    assert!(generation_of(&advanced) > generation_of(&stale));

    // The generation the failing call will observe, read immediately before it.
    let before_call = state.capture().await.expect("capture before failing call");
    assert_eq!(before_call, advanced);
    let observed = generation_of(&before_call);
    assert!(observed > 0, "advanced generation must be non-zero");

    let error = state
        .pause_stream_at_capture_with_invocation_id(writer, key(), Some(&stale), [8; 16])
        .await
        .expect_err("stale capture must fail");
    assert!(
        matches!(error, RuntimeError::StaleCapture { .. }),
        "{error:?}"
    );

    let audits = state.admin_invocation_audits().await.unwrap();
    let row = audits
        .iter()
        .find(|row| row.invocation_id == [8; 16])
        .expect("failure row");
    assert_eq!(row.terminal_outcome, "failure:stale_capture");
    assert_eq!(row.observed_generation, observed);
    assert!(!row.succeeded);

    // Replay of the same invocation must reproduce the stored class.
    let replay = state
        .pause_stream_at_capture_with_invocation_id(writer, key(), Some(&stale), [8; 16])
        .await
        .expect_err("replayed stale capture must fail");
    assert!(
        matches!(replay, RuntimeError::StaleCapture { .. }),
        "{replay:?}"
    );
}
