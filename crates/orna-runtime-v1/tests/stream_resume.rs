//! Public stream resume results preserve the durable running state.

use std::{path::PathBuf, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, CheckpointResetRequest, Component, ConsumerIdentity, RuntimeIdentity,
    RuntimeState, StreamAdministrationOutcome,
};
use orna_stream_v1::{CheckpointPrecondition, Position};
use tempfile::{Builder, TempDir};

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-stream-resume-")
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

#[tokio::test]
async fn resume_after_checkpoint_reset_reports_running_and_noop() {
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
    let key = CheckpointKey {
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
    };

    assert_eq!(
        state.pause_stream(writer, key.clone()).await.unwrap(),
        StreamAdministrationOutcome::Paused { changed: true }
    );
    let checkpoint = state
        .reset_checkpoint(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: 0,
                    committed: None,
                },
                to: Position {
                    token: Component::new("opaque-41").unwrap(),
                },
                reason: "scenario setup".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(checkpoint.version, 1);

    assert_eq!(
        state.resume_stream(writer, key.clone()).await.unwrap(),
        StreamAdministrationOutcome::Running { changed: true }
    );
    assert_eq!(
        state.resume_stream(writer, key).await.unwrap(),
        StreamAdministrationOutcome::Running { changed: false }
    );
}
