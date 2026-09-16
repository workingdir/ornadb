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
    CheckpointKey, CheckpointResetAudit, CheckpointResetProvider, Component, ConsumerIdentity,
    RuntimeError, RuntimeIdentity, RuntimeState, StreamAdministrationOutcome,
};
use orna_stream_v1::{CheckpointPrecondition, Position};
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
            key.clone(),
            expected_initial(),
            position("unsupported"),
            "unsupported target".into(),
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

    let mut mismatched_key = key.clone();
    mismatched_key.position_format = Component::new("position-format-v2").unwrap();
    let mismatch_provider = provider(mismatched_key, true);
    let rejected = state
        .reset_checkpoint_with_provider(
            writer,
            key.clone(),
            expected_initial(),
            position("format-mismatch"),
            "format mismatch".into(),
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
            key.clone(),
            expected_initial(),
            target.clone(),
            "operator rewind".into(),
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
            key.clone(),
            expected_initial(),
            first.clone(),
            "first reset".into(),
            &provider,
        )
        .await
        .unwrap();
    let before = state.stream_checkpoint(&key).await.unwrap();

    let stale_version = state
        .reset_checkpoint_with_provider(
            writer,
            key.clone(),
            expected_initial(),
            position("stale-version"),
            "stale version".into(),
            &provider,
        )
        .await
        .unwrap_err();
    assert_eq!(stale_version, RuntimeError::StreamCheckpointStale);
    assert_eq!(stale_version.public_code(), Some("sys.checkpoint.conflict"));

    let stale_position = state
        .reset_checkpoint_with_provider(
            writer,
            key.clone(),
            CheckpointPrecondition {
                version: before.version,
                committed: Some(position("wrong-position")),
            },
            position("stale-position"),
            "stale position".into(),
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
