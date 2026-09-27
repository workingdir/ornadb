//! Focused provider-gated checkpoint reset coverage.

use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, CheckpointResetAudit, CheckpointResetProvider, CheckpointResetRequest,
    Component, ConsumerIdentity, FailureStatus, RuntimeError, RuntimeIdentity, RuntimeState,
    StreamAdministrationOutcome,
};
use orna_stream_v1::{
    AsyncCheckpointBackend, CheckpointPrecondition, CommitIntent, CommitResult, DeliveryIdentity,
    DiagnosticClass, DiagnosticCode, LeasePurpose, Position, SafeDiagnostic, StreamFailurePayload,
};
use tempfile::{Builder, TempDir};

#[derive(Clone)]
struct TestProvider {
    key: CheckpointKey,
    supports_target: bool,
    validations: Arc<AtomicUsize>,
}

impl CheckpointResetProvider for TestProvider {
    fn checkpoint_key(&self) -> CheckpointKey {
        self.key.clone()
    }

    fn supports_reset_target(&self, _: &Position) -> bool {
        self.validations.fetch_add(1, Ordering::Relaxed);
        self.supports_target
    }
}

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-checkpoint-reset-")
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

async fn fixture() -> (
    TempDir,
    RuntimeState,
    orna_runtime_v1::WriterLease,
    CheckpointKey,
) {
    let (directory, repository) = repository();
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
    (directory, state, writer, key)
}

fn position(token: &str) -> Position {
    Position {
        token: Component::new(token).unwrap(),
    }
}

fn provider(key: CheckpointKey, supports_target: bool) -> TestProvider {
    TestProvider {
        key,
        supports_target,
        validations: Arc::new(AtomicUsize::new(0)),
    }
}

fn expected_initial() -> CheckpointPrecondition {
    CheckpointPrecondition {
        version: 0,
        committed: None,
    }
}

#[tokio::test]
async fn unsupported_target_and_key_format_mismatch_fail_before_mutation() {
    let (_directory, state, writer, key) = fixture().await;
    let before = state.stream_checkpoint(&key).await.unwrap();
    let unsupported = provider(key.clone(), false);

    let rejected = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: position("unsupported"),
                reason: "unsupported target".into(),
            },
            &unsupported,
        )
        .await
        .unwrap_err();
    assert_eq!(
        rejected.public_code(),
        Some("sys.checkpoint.not_replayable")
    );
    assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before);
    assert!(
        state
            .checkpoint_reset_audits(&key)
            .await
            .unwrap()
            .is_empty()
    );
    let audits = state.admin_invocation_audits().await.unwrap();
    assert_eq!(audits.len(), 2);
    assert!(!audits[1].succeeded);
    assert_eq!(audits[1].function, "sys.admin.reset_checkpoint");
    assert!(audits[1].terminal_outcome.contains("not replayable"));

    let mut mismatched_key = key.clone();
    mismatched_key.position_format = Component::new("position-format-v2").unwrap();
    let mismatch_provider = provider(mismatched_key, true);
    let rejected = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: position("format-mismatch"),
                reason: "format mismatch".into(),
            },
            &mismatch_provider,
        )
        .await
        .unwrap_err();
    assert_eq!(
        rejected.public_code(),
        Some("sys.checkpoint.not_replayable")
    );
    assert_eq!(mismatch_provider.validations.load(Ordering::Relaxed), 0);
    assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before);
    let audits = state.admin_invocation_audits().await.unwrap();
    assert_eq!(audits.len(), 3);
    assert!(!audits[2].succeeded);
    assert_eq!(audits[2].function, "sys.admin.reset_checkpoint");
    assert!(audits[2].safe_arguments.contains("expected_version=0"));
}

#[tokio::test]
async fn stale_owner_provider_mismatch_preserves_error_without_failed_audit() {
    let (_directory, state, stale_writer, key) = fixture().await;
    let before_audits = state.admin_invocation_audits().await.unwrap();
    let current_writer = state.takeover_lease(stale_writer, [5; 16]).await.unwrap();
    assert_ne!(stale_writer, current_writer);

    let mut mismatched_key = key.clone();
    mismatched_key.position_format = Component::new("position-format-v2").unwrap();
    let provider = provider(mismatched_key, true);
    let rejected = state
        .reset_checkpoint_with_provider(
            stale_writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: position("format-mismatch"),
                reason: "format mismatch".into(),
            },
            &provider,
        )
        .await
        .unwrap_err();

    assert_eq!(rejected, RuntimeError::CheckpointNotReplayable);
    assert_eq!(provider.validations.load(Ordering::Relaxed), 0);
    assert_eq!(
        state.admin_invocation_audits().await.unwrap(),
        before_audits
    );
    assert!(
        state
            .checkpoint_reset_audits(&key)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn supported_target_advances_once_and_audits_once() {
    let (_directory, state, writer, key) = fixture().await;
    let target = position("provider-target");
    let provider = provider(key.clone(), true);

    let checkpoint = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: target.clone(),
                reason: "operator rewind".into(),
            },
            &provider,
        )
        .await
        .unwrap();
    assert_eq!(checkpoint.version, 1);
    assert_eq!(checkpoint.committed, Some(target.clone()));
    assert_eq!(provider.validations.load(Ordering::Relaxed), 1);
    assert_eq!(
        state.checkpoint_reset_audits(&key).await.unwrap(),
        vec![CheckpointResetAudit {
            key: key.clone(),
            old_version: 0,
            old_position: None,
            new_version: 1,
            new_position: target,
            reason: "operator rewind".into(),
            redacted: false,
        }]
    );
}

#[tokio::test]
async fn stale_version_and_position_map_to_conflict_without_mutation() {
    let (_directory, state, writer, key) = fixture().await;
    let provider = provider(key.clone(), true);
    let first = position("first");
    state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: first.clone(),
                reason: "first reset".into(),
            },
            &provider,
        )
        .await
        .unwrap();
    let before = state.stream_checkpoint(&key).await.unwrap();

    let stale_version = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: position("stale-version"),
                reason: "stale version".into(),
            },
            &provider,
        )
        .await
        .unwrap_err();
    assert_eq!(stale_version, RuntimeError::StreamCheckpointStale);
    assert_eq!(stale_version.public_code(), Some("sys.checkpoint.conflict"));

    let stale_position = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: before.version,
                    committed: Some(position("wrong-position")),
                },
                to: position("stale-position"),
                reason: "stale position".into(),
            },
            &provider,
        )
        .await
        .unwrap_err();
    assert_eq!(
        stale_position.public_code(),
        Some("sys.checkpoint.conflict")
    );
    assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before);
    assert_eq!(state.checkpoint_reset_audits(&key).await.unwrap().len(), 1);
}

#[tokio::test]
async fn blocking_failure_rejects_provider_supported_reset_without_mutation() {
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
    let before = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .unwrap();

    let delivery = DeliveryIdentity {
        consumer: key.consumer.clone(),
        source_format: key.source_format.clone(),
        source: key.source.clone(),
        partition_format: key.partition_format.clone(),
        partition: key.partition.clone(),
        position_format: key.position_format.clone(),
        position: position("failed-delivery"),
        successor: position("after-failure"),
    };
    let mut stream = state.stream_backend(writer);
    let lease = match stream
        .apply_async(CommitIntent::Acquire {
            delivery: delivery.clone(),
            expected: expected_initial(),
            purpose: LeasePurpose::Deliver,
        })
        .await
        .unwrap()
    {
        CommitResult::Acquired { lease } => lease,
        other => panic!("expected delivery lease, got {other:?}"),
    };
    let failure = match stream
        .fail_async(
            lease,
            SafeDiagnostic {
                code: DiagnosticCode::ExecutionRejected,
                class: DiagnosticClass::Permanent,
            },
            StreamFailurePayload::Plaintext(vec![1, 2, 3]),
        )
        .await
        .unwrap()
    {
        CommitResult::Failed { failure } => failure,
        other => panic!("expected blocking failure, got {other:?}"),
    };
    assert_eq!(failure.status, FailureStatus::Failed);
    assert_eq!(
        state.pause_stream(writer, key.clone()).await.unwrap(),
        StreamAdministrationOutcome::Paused { changed: true }
    );

    let provider = provider(key.clone(), true);
    let rejected = state
        .reset_checkpoint_with_provider(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: before.version,
                    committed: before.committed.clone(),
                },
                to: position("provider-supported-target"),
                reason: "reset must not discard blocking failure".into(),
            },
            &provider,
        )
        .await;

    assert!(
        rejected.is_err(),
        "reset must reject a current blocking delivery failure"
    );

    assert_eq!(provider.validations.load(Ordering::Relaxed), 1);
    assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before);
    assert!(
        state
            .checkpoint_reset_audits(&key)
            .await
            .unwrap()
            .is_empty()
    );
    let invocations = state.admin_invocation_audits().await.unwrap();
    let reset = invocations.last().unwrap();
    assert_eq!(reset.function, "sys.admin.reset_checkpoint");
    assert!(!reset.succeeded);
}

#[tokio::test]
async fn generic_admin_audit_is_redacted_atomic_and_reopenable() {
    let (_directory, state, writer, key) = fixture().await;
    assert_eq!(
        state
            .pause_stream_with_reason(writer, key.clone(), "operator\0secret".into())
            .await,
        Ok(StreamAdministrationOutcome::Paused { changed: false })
    );
    state.validate_recovery().await.unwrap();
    let paused = state.admin_invocation_audits().await.unwrap();
    assert_eq!(paused.len(), 2);
    assert_eq!(paused[1].sequence, 2);
    assert_eq!(paused[1].function, "sys.admin.pause_stream_with_reason");
    assert!(paused[1].succeeded);
    assert!(paused[1].redacted);
    assert_eq!(paused[1].terminal_outcome, "paused_noop");
    assert!(paused[1].safe_arguments.contains("stream_key_digest="));
    assert!(paused[1].safe_arguments.contains("reason=<redacted>"));
    assert!(!paused[1].safe_arguments.contains("operator"));
    assert_eq!(paused[1].owner, writer);
    assert_eq!(paused[1].observed_generation, 0);

    assert_eq!(paused[0].function, "sys.admin.pause_stream");
    assert!(paused[0].succeeded);
    drop(state);
    let state = {
        let repository = _directory.path();
        let repository = orna_repository_v1::Repository::discover(repository).unwrap();
        RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            [3; 32],
        )
        .await
        .unwrap()
    };
    let writer = state.acquire_lease([4; 16]).await.unwrap();
    assert_eq!(state.admin_invocation_audits().await.unwrap(), paused);

    let reset = position("reset-target");
    state
        .reset_checkpoint(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: expected_initial(),
                to: reset.clone(),
                reason: "operator reset".into(),
            },
        )
        .await
        .unwrap();
    let before_stale = state.stream_checkpoint(&key).await.unwrap();
    assert_eq!(
        state
            .reset_checkpoint(
                writer,
                CheckpointResetRequest {
                    key: key.clone(),
                    expected: expected_initial(),
                    to: position("stale-target"),
                    reason: "stale\0secret".into(),
                },
            )
            .await,
        Err(RuntimeError::StreamCheckpointStale)
    );
    assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before_stale);
    assert_eq!(state.checkpoint_reset_audits(&key).await.unwrap().len(), 1);
    let audits = state.admin_invocation_audits().await.unwrap();
    assert_eq!(audits.len(), 4);
    assert_eq!(audits[2].function, "sys.admin.reset_checkpoint");
    assert!(audits[2].succeeded);
    assert_eq!(audits[2].terminal_outcome, "checkpoint_reset");
    assert_eq!(audits[3].function, "sys.admin.reset_checkpoint");
    assert!(!audits[3].succeeded);
    assert!(audits[3].terminal_outcome.starts_with("failure:"));
    assert!(audits[3].redacted);
    assert!(!audits[3].safe_arguments.contains("secret"));
}
