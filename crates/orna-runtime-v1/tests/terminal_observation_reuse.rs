//! Regression for retained terminal `sys.Stream` observations and checkpoint reuse.

use std::{path::PathBuf, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, RequestIdentity, RunObservationRegistration,
    RunObservationStatus, RuntimeIdentity, RuntimeState, StreamObservationRegistration,
    StreamObservationStatus, TerminalOutcome,
};
use tempfile::{Builder, TempDir};

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-terminal-observation-")
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

fn checkpoint() -> CheckpointKey {
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
        position_format: Component::new("position-format").unwrap(),
    }
}

fn request(value: u8) -> RequestIdentity {
    RequestIdentity {
        session_id: [value; 16],
        request_id: [value + 1; 16],
    }
}

#[tokio::test]
async fn terminal_observation_releases_checkpoint_for_a_new_run() {
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
    let key = checkpoint();
    let first_request = request(10);
    state
        .reserve_request(first_request, [11; 32])
        .await
        .expect("reserve first request");
    let first_run = state
        .register_run_observation(RunObservationRegistration {
            request: first_request,
            consumer_identity: key.consumer.clone(),
            function: "pkg.consume".into(),
            source_identity: Some(key.source.as_str().to_owned()),
            invocation_id: [12; 16],
        })
        .await
        .expect("register first run");
    let first_stream = state
        .register_stream_observation(StreamObservationRegistration {
            run: first_run.id,
            producer: "source-object".into(),
            consumer: Some("pkg.consume".into()),
            checkpoint: key.clone(),
        })
        .await
        .expect("register first stream");
    state
        .complete_request(
            first_request,
            [11; 32],
            TerminalOutcome::new(vec![13]).unwrap(),
        )
        .await
        .expect("complete first request");
    let second_request = request(20);
    state
        .reserve_request(second_request, [21; 32])
        .await
        .expect("reserve second request");
    let second_run = state
        .register_run_observation(RunObservationRegistration {
            request: second_request,
            consumer_identity: key.consumer.clone(),
            function: "pkg.consume".into(),
            source_identity: Some(key.source.as_str().to_owned()),
            invocation_id: [22; 16],
        })
        .await
        .expect("register second run");
    let second_stream = state
        .register_stream_observation(StreamObservationRegistration {
            run: second_run.id,
            producer: "source-object".into(),
            consumer: Some("pkg.consume".into()),
            checkpoint: key,
        })
        .await
        .expect("reuse checkpoint after terminal run");

    assert_eq!(
        state
            .run_observation(first_run.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        RunObservationStatus::Completed
    );
    assert_eq!(
        state
            .stream_observation(first_stream.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        StreamObservationStatus::Orphaned
    );
    assert_ne!(first_stream.id, second_stream.id);
    assert_eq!(second_stream.run, second_run.id);
}
