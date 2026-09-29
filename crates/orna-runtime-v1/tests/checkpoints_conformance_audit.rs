//! Public API coverage for five checkpoint/replay evidence gaps in the
//! chapter audit. The source fixture is used as the retained delivery payload;
//! this suite does not claim that RuntimeState evaluates Orna source.

use std::{path::Path, process::Command, sync::atomic::{AtomicUsize, Ordering}};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    RuntimeIdentity, RuntimeState, StreamFailurePayloadFuture, StreamFailurePayloadProvider,
    StreamHandler, StreamHandlerResult, StreamItem, StreamMutationBatch, WriterLease,
};
use orna_stream_v1::{
    AsyncCheckpointBackend, AsyncFailurePayloadBackend, CheckpointPrecondition, CommitIntent,
    CommitResult, Component,
    ConsumerIdentity, DeliveryIdentity, DiagnosticClass, DiagnosticCode, FailureRecord,
    FailureStatus, LeasePurpose, Position, RejectReason, SafeDiagnostic, StreamFailurePayload,
};
use orna_syntax_v1::parse_module;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

const REPLAY_SOURCE: &str = include_str!("fixtures/checkpoint-audit-replay-handler.orna");

fn repository() -> (TempDir, Repository) {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-checkpoint-audit-")
        .tempdir_in(target)
        .expect("create isolated repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("initialize test repository");
    assert!(initialized.status.success(), "initialize test repository");
    let repository = Repository::discover(directory.path()).expect("discover test repository");
    (directory, repository)
}

fn delivery() -> DeliveryIdentity {
    DeliveryIdentity {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("checkpoint_audit_consumer").unwrap(),
            binding: Component::new("fixture_binding").unwrap(),
        },
        source_format: Component::new("source_format_v1").unwrap(),
        source: Component::new("checkpoint_audit_source").unwrap(),
        partition_format: Component::new("partition_format_v1").unwrap(),
        partition: None,
        position_format: Component::new("position_format_v1").unwrap(),
        position: Position {
            token: Component::new("position_one").unwrap(),
        },
        successor: Position {
            token: Component::new("position_two").unwrap(),
        },
    }
}

fn processing_error() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    }
}

struct CheckpointFixture {
    state: RuntimeState,
    writer: WriterLease,
    skipped: FailureRecord,
    checkpoint: orna_runtime_v1::StreamCheckpoint,
    _directory: TempDir,
}

impl CheckpointFixture {
    async fn new(payload: StreamFailurePayload) -> Self {
        assert!(parse_module(REPLAY_SOURCE).diagnostics.is_empty());
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
        let item = delivery();
        let key = item.checkpoint_key();
        let mut stream = state.stream_backend(writer);
        let initial = stream
            .checkpoint_async(&key)
            .await
            .expect("read initial checkpoint");
        let expected = CheckpointPrecondition::from(&initial);
        let delivery_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: item.clone(),
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .expect("admit delivery")
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("expected delivery lease, got {other:?}"),
        };
        let failed = match stream
            .fail_with_payload_async(delivery_lease, processing_error(), payload)
            .await
            .expect("retain failed delivery")
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("expected failed delivery, got {other:?}"),
        };
        let skip_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: item,
                expected: expected.clone(),
                purpose: LeasePurpose::Skip,
            })
            .await
            .expect("admit explicit skip")
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("expected skip lease, got {other:?}"),
        };
        let checkpoint = match stream
            .apply_async(CommitIntent::Skip {
                lease: skip_lease,
                expected,
                expected_failure_version: failed.version,
            })
            .await
            .expect("skip the exact failed delivery")
        {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            other => panic!("expected skipped checkpoint, got {other:?}"),
        };
        let skipped = stream
            .failure_async(&failed.identity)
            .await
            .expect("read skipped failure")
            .expect("failure record remains present");
        assert_eq!(skipped.identity, failed.identity);
        assert_eq!(skipped.status, FailureStatus::Skipped);
        assert_eq!(skipped.attempts, 1);
        Self {
            state,
            writer,
            skipped,
            checkpoint,
            _directory: directory,
        }
    }

    async fn admit_replay(&self) -> orna_stream_v1::ReplayGrant {
        match self
            .state
            .stream_backend(self.writer)
            .apply_async(CommitIntent::Replay {
                failure: self.skipped.identity.clone(),
                expected_version: self.skipped.version,
            })
            .await
            .expect("admit replay")
        {
            CommitResult::ReplayGranted { grant } => grant,
            other => panic!("expected replay grant, got {other:?}"),
        }
    }

    async fn failure(&self, writer: WriterLease) -> FailureRecord {
        self.state
            .stream_backend(writer)
            .failure_async(&self.skipped.identity)
            .await
            .expect("read failure")
            .expect("failure exists")
    }

    async fn assert_checkpoint(&self, writer: WriterLease) {
        assert_eq!(
            self.state
                .stream_backend(writer)
                .checkpoint_async(&self.checkpoint.key)
                .await
                .expect("read live checkpoint"),
            self.checkpoint
        );
    }
}

struct FixtureHandler {
    calls: usize,
    generation_digest: [u8; 32],
}

impl StreamHandler for FixtureHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        self.calls += 1;
        assert!(parse_module(REPLAY_SOURCE).diagnostics.is_empty());
        assert_eq!(item.payload, REPLAY_SOURCE.as_bytes());
        StreamHandlerResult::Commit(StreamMutationBatch {
            mutations: Vec::new(),
            next_digest: self.generation_digest,
        })
    }
}

#[derive(Default)]
struct CountingProvider {
    calls: AtomicUsize,
}

impl StreamFailurePayloadProvider for CountingProvider {
    type Error = ();

    fn refetch<'a>(&'a self, _: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(REPLAY_SOURCE.as_bytes().to_vec()) })
    }
}

fn protected_payload() -> StreamFailurePayload {
    StreamFailurePayload::ProtectedReference {
        reference: "checkpoint-audit-payload".into(),
        digest: Sha256::digest(REPLAY_SOURCE.as_bytes()).into(),
    }
}

// ORNA-SYS-064 / ORNA-SYS-123, source/12-checkpoints.md:72,82.
#[tokio::test]
async fn cancelled_plaintext_replay_grant_is_rejected_before_handler() {
    let fixture = CheckpointFixture::new(StreamFailurePayload::Plaintext(
        REPLAY_SOURCE.as_bytes().to_vec(),
    ))
    .await;
    let grant = fixture.admit_replay().await;
    let cancelled = match fixture
        .state
        .stream_backend(fixture.writer)
        .apply_async(CommitIntent::ReplayCancel {
            failure: grant.failure.clone(),
            expected_version: grant.version,
        })
        .await
        .expect("cancel replay grant")
    {
        CommitResult::ReplayCancelled { failure } => failure,
        other => panic!("expected replay cancellation, got {other:?}"),
    };
    let capture = fixture.state.capture().await.expect("capture runtime");
    let mut handler = FixtureHandler {
        calls: 0,
        generation_digest: capture.generation_digest(),
    };
    assert_eq!(
        fixture
            .state
            .replay_stream_failure(fixture.writer, grant, &mut handler)
            .await
            .expect("stale replay grant is a stable rejection"),
        CommitResult::Rejected(RejectReason::StaleFailure)
    );
    assert_eq!(fixture.failure(fixture.writer).await, cancelled);
    assert_eq!(handler.calls, 0);
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_checkpoint(fixture.writer).await;
}

// ORNA-SYS-064 / ORNA-SYS-123, source/12-checkpoints.md:72,82.
#[tokio::test]
async fn cancelled_protected_replay_grant_is_rejected_before_refetch() {
    let fixture = CheckpointFixture::new(protected_payload()).await;
    let grant = fixture.admit_replay().await;
    let cancelled = match fixture
        .state
        .stream_backend(fixture.writer)
        .apply_async(CommitIntent::ReplayCancel {
            failure: grant.failure.clone(),
            expected_version: grant.version,
        })
        .await
        .expect("cancel protected replay grant")
    {
        CommitResult::ReplayCancelled { failure } => failure,
        other => panic!("expected replay cancellation, got {other:?}"),
    };
    let capture = fixture.state.capture().await.expect("capture runtime");
    let provider = CountingProvider::default();
    let mut handler = FixtureHandler {
        calls: 0,
        generation_digest: capture.generation_digest(),
    };
    assert_eq!(
        fixture
            .state
            .replay_stream_failure_with_provider(
                fixture.writer,
                grant,
                &provider,
                &mut handler,
            )
            .await
            .expect("stale replay grant is rejected before provider access"),
        CommitResult::Rejected(RejectReason::StaleFailure)
    );
    assert_eq!(fixture.failure(fixture.writer).await, cancelled);
    assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    assert_eq!(handler.calls, 0);
    fixture.assert_checkpoint(fixture.writer).await;
}

// Attempt-count rule at source/12-checkpoints.md:46,59-61.
#[tokio::test]
async fn replay_admission_persists_attempt_before_any_callback() {
    let fixture = CheckpointFixture::new(StreamFailurePayload::Plaintext(
        REPLAY_SOURCE.as_bytes().to_vec(),
    ))
    .await;
    let grant = fixture.admit_replay().await;
    let admitted = fixture.failure(fixture.writer).await;
    assert_eq!(admitted.identity, fixture.skipped.identity);
    assert_eq!(admitted.status, FailureStatus::Replaying);
    assert_eq!(admitted.attempts, fixture.skipped.attempts + 1);
    assert_eq!(admitted.version, grant.version);
    fixture.assert_checkpoint(fixture.writer).await;
}

// ORNA-SYS-123, source/12-checkpoints.md:82; attempt count at :46.
#[tokio::test]
async fn successful_replay_counts_one_attempt_and_preserves_live_checkpoint() {
    let fixture = CheckpointFixture::new(StreamFailurePayload::Plaintext(
        REPLAY_SOURCE.as_bytes().to_vec(),
    ))
    .await;
    let grant = fixture.admit_replay().await;
    let capture = fixture.state.capture().await.expect("capture before replay");
    let mut handler = FixtureHandler {
        calls: 0,
        generation_digest: capture.generation_digest(),
    };
    let replayed = match fixture
        .state
        .replay_stream_failure(fixture.writer, grant.clone(), &mut handler)
        .await
        .expect("complete fixture replay")
    {
        CommitResult::ReplayCompleted { failure } => failure,
        other => panic!("expected replay completion, got {other:?}"),
    };
    assert_eq!(handler.calls, 1);
    assert_eq!(replayed.identity, fixture.skipped.identity);
    assert_eq!(replayed.status, FailureStatus::Replayed);
    assert_eq!(replayed.attempts, fixture.skipped.attempts + 1);
    assert_ne!(replayed.version, grant.version);
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_checkpoint(fixture.writer).await;
}

// ORNA-SYS-123 plus crash recovery paragraph, source/12-checkpoints.md:82,94.
#[tokio::test]
async fn interrupted_replay_recovery_keeps_the_admitted_attempt() {
    let fixture = CheckpointFixture::new(StreamFailurePayload::Plaintext(
        REPLAY_SOURCE.as_bytes().to_vec(),
    ))
    .await;
    let grant = fixture.admit_replay().await;
    let capture = fixture.state.capture().await.expect("capture after admission");
    let replacement = fixture
        .state
        .takeover_lease(fixture.writer, [5; 16])
        .await
        .expect("fence interrupted replay owner and recover its state");
    let recovered = fixture.failure(replacement).await;
    assert_eq!(recovered.identity, grant.failure);
    assert_eq!(recovered.status, FailureStatus::Skipped);
    assert_eq!(recovered.attempts, fixture.skipped.attempts + 1);
    assert_ne!(recovered.version, grant.version);
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_checkpoint(replacement).await;
}
