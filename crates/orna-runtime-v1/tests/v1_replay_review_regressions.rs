//! Public RuntimeState replay regressions for ornadb-gov5.17.6 / #786.
//!
//! Normative authority: reference/Orna-1.0.0/source/12-checkpoints.md,
//! "One record per delivery", ORNA-FAIL-004, ORNA-SYS-064, ORNA-SYS-123,
//! and recovery after an interrupted attempt; 34-system-reference.md,
//! sys.admin.replay_failure requires stale observations to fail before work.
//!
//! Ignored tests assert required behavior, not the known implementation gaps.
//! Run them explicitly with --ignored when collecting review evidence.
//! Every fixture uses an isolated workspace-local artifact directory.
//!
//! Scope: cancelled replay grants, replay attempt accounting, and concurrent
//! execution fencing through public runtime APIs. No authenticated
//! administration route, source polling, module loading, or physical process
//! crash is exercised.

use std::{cell::Cell, path::Path, process::Command, sync::Arc};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    RuntimeError, RuntimeIdentity, RuntimeState, StreamFailurePayloadFuture,
    StreamFailurePayloadProvider, StreamHandler, StreamHandlerResult, StreamItem,
    StreamMutationBatch, StreamStepError, WriterLease,
};
use orna_stream_v1::{
    AsyncCheckpointBackend, Checkpoint, CommitIntent, CommitResult, Component, ConsumerIdentity,
    DeliveryIdentity, DiagnosticClass, DiagnosticCode, FailureIdentity, FailureRecord,
    FailureStatus, LeasePurpose, Position, RejectReason, ReplayGrant, SafeDiagnostic,
    StreamFailurePayload,
};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use tokio::sync::Notify;

const PAYLOAD: &[u8] = b"replay-review-payload";
const PROTECTED_REFERENCE: &str = "replay-review-protected-reference";

fn diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    }
}

fn protected_payload() -> StreamFailurePayload {
    StreamFailurePayload::ProtectedReference {
        reference: PROTECTED_REFERENCE.into(),
        digest: Sha256::digest(PAYLOAD).into(),
    }
}

fn repository() -> (TempDir, Repository) {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("create workspace test artifact directory");
    let directory = Builder::new()
        .prefix("orna-v1-replay-review-")
        .tempdir_in(&target)
        .expect("create review repository directory");
    // Matches the neighboring public-API review fixture. No commit is needed.
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init for review fixture");
    assert!(initialized.status.success(), "initialize review repository");
    let repository = Repository::discover(directory.path()).expect("discover review repository");
    (directory, repository)
}

fn delivery() -> DeliveryIdentity {
    DeliveryIdentity {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("replay-review-consumer").unwrap(),
            binding: Component::new("binding").unwrap(),
        },
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("source").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: None,
        position_format: Component::new("position-format").unwrap(),
        position: Position {
            token: Component::new("delivery-position").unwrap(),
        },
        successor: Position {
            token: Component::new("delivery-successor").unwrap(),
        },
    }
}

struct Fixture {
    state: RuntimeState,
    writer: WriterLease,
    skipped: FailureRecord,
    checkpoint: Checkpoint,
    // Drop the runtime before removing its repository and database files.
    _directory: TempDir,
}

impl Fixture {
    async fn new(payload: StreamFailurePayload) -> Self {
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
        .expect("open review runtime");
        let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
        let delivery = delivery();
        let mut stream = state.stream_backend(writer);
        let initial = stream
            .checkpoint_async(&delivery.checkpoint_key())
            .await
            .expect("read initial checkpoint precondition");
        let lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: delivery.clone(),
                expected: (&initial).into(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .expect("acquire delivery")
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("expected delivery lease, got {other:?}"),
        };
        let failed = match stream
            .fail_async(lease, diagnostic(), payload)
            .await
            .expect("retain failed delivery")
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("expected failed delivery, got {other:?}"),
        };
        assert_eq!(failed.identity, FailureIdentity(delivery.clone()));
        assert_eq!(failed.status, FailureStatus::Failed);
        assert_eq!(failed.attempts, 1, "first admitted processing attempt");
        assert_eq!(
            stream.checkpoint_async(&initial.key).await.unwrap(),
            initial,
            "ORNA-SYS-062: ordinary failure must not move the checkpoint"
        );
        let skip_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: delivery.clone(),
                expected: (&initial).into(),
                purpose: LeasePurpose::Skip,
            })
            .await
            .expect("acquire skip lease")
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("expected skip lease, got {other:?}"),
        };
        let checkpoint = match stream
            .apply_async(CommitIntent::Skip {
                lease: skip_lease,
                expected: (&initial).into(),
                expected_failure_version: failed.version,
            })
            .await
            .expect("skip exact failed delivery")
        {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            other => panic!("expected skipped checkpoint, got {other:?}"),
        };
        assert_eq!(checkpoint.key, initial.key);
        assert_eq!(checkpoint.committed, Some(delivery.successor));
        assert_ne!(checkpoint.version, initial.version);
        let skipped = stream
            .failure_async(&failed.identity)
            .await
            .unwrap()
            .expect("same failure survives skip");
        assert_eq!(skipped.identity, failed.identity);
        assert_eq!(skipped.status, FailureStatus::Skipped);
        assert_ne!(skipped.version, failed.version);
        assert_eq!(skipped.attempts, failed.attempts, "skip is not processing");
        Self {
            state,
            writer,
            skipped,
            checkpoint,
            _directory: directory,
        }
    }

    async fn admit(&self) -> ReplayGrant {
        let grant = match self
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
        };
        let admitted = self.failure(self.writer).await;
        assert_eq!(grant.failure, self.skipped.identity);
        assert_eq!(admitted.identity, grant.failure);
        assert_eq!(admitted.version, grant.version);
        assert_ne!(admitted.version, self.skipped.version);
        assert_eq!(admitted.status, FailureStatus::Replaying);
        self.assert_checkpoint(self.writer).await;
        // Accounting is asserted by individual tests at independent observation
        // points, so a broken admission count cannot mask completion/recovery.
        grant
    }

    async fn cancel(&self, grant: &ReplayGrant) -> FailureRecord {
        match self
            .state
            .stream_backend(self.writer)
            .apply_async(CommitIntent::ReplayCancel {
                failure: grant.failure.clone(),
                expected_version: grant.version,
            })
            .await
            .expect("cancel admitted replay")
        {
            CommitResult::ReplayCancelled { failure } => {
                assert_eq!(failure.identity, grant.failure);
                assert_ne!(failure.version, grant.version);
                assert_eq!(failure.status, FailureStatus::Skipped);
                assert_eq!(self.failure(self.writer).await, failure);
                self.assert_checkpoint(self.writer).await;
                failure
            }
            other => panic!("expected replay cancellation, got {other:?}"),
        }
    }

    async fn failure(&self, writer: WriterLease) -> FailureRecord {
        self.state
            .stream_backend(writer)
            .failure_async(&self.skipped.identity)
            .await
            .expect("read durable failure")
            .expect("same failure remains present")
    }

    async fn assert_checkpoint(&self, writer: WriterLease) {
        assert_eq!(
            self.state
                .stream_backend(writer)
                .checkpoint_async(&self.checkpoint.key)
                .await
                .expect("read live checkpoint"),
            self.checkpoint,
            "ORNA-FAIL-004 / ORNA-SYS-123: replay must not move the live checkpoint"
        );
    }

    fn assert_one_replay_attempt(&self, failure: &FailureRecord) {
        assert_eq!(failure.identity, self.skipped.identity);
        assert_eq!(
            failure.attempts,
            self.skipped.attempts + 1,
            "chapter 12: exactly one admitted replay adds exactly one processing attempt"
        );
    }
}

struct CountingHandler {
    calls: usize,
    result: Option<StreamHandlerResult>,
}

impl CountingHandler {
    fn new(result: StreamHandlerResult) -> Self {
        Self {
            calls: 0,
            result: Some(result),
        }
    }
}

impl StreamHandler for CountingHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        self.calls += 1;
        assert_eq!(
            item.delivery,
            delivery(),
            "replay the exact failed delivery"
        );
        assert_eq!(
            item.payload.as_slice(),
            PAYLOAD,
            "replay retained/refetched bytes"
        );
        self.result
            .take()
            .expect("only one handler call is expected")
    }
}

#[derive(Default)]
struct CountingProvider {
    calls: Cell<usize>,
}

impl StreamFailurePayloadProvider for CountingProvider {
    type Error = ();

    fn refetch<'a>(&'a self, reference: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(reference, PROTECTED_REFERENCE);
        Box::pin(std::future::ready(Ok(PAYLOAD.to_vec())))
    }
}

struct GatedProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl StreamFailurePayloadProvider for GatedProvider {
    type Error = ();

    fn refetch<'a>(&'a self, reference: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error> {
        assert_eq!(reference, PROTECTED_REFERENCE);
        self.entered.notify_one();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            release.notified().await;
            Ok(PAYLOAD.to_vec())
        })
    }
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #786; run explicitly for review"]
async fn cancelled_plaintext_replay_grant_is_rejected_before_handler() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    let grant = fixture.admit().await;
    let cancelled = fixture.cancel(&grant).await;
    let capture = fixture.state.capture().await.unwrap();
    let mut handler = CountingHandler::new(StreamHandlerResult::Fail(diagnostic()));
    let result = fixture
        .state
        .replay_stream_failure(fixture.writer, grant, &mut handler)
        .await
        .expect("stale grant yields a deterministic rejection");
    assert_eq!(result, CommitResult::Rejected(RejectReason::StaleFailure));
    assert_eq!(fixture.failure(fixture.writer).await, cancelled);
    fixture.assert_checkpoint(fixture.writer).await;
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    assert_eq!(
        handler.calls, 0,
        "ORNA-SYS-064: a cancelled grant's stale version forbids handler execution"
    );
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #786; run explicitly for review"]
async fn cancelled_protected_replay_grant_is_rejected_before_provider_and_handler() {
    let fixture = Fixture::new(protected_payload()).await;
    let grant = fixture.admit().await;
    let cancelled = fixture.cancel(&grant).await;
    let capture = fixture.state.capture().await.unwrap();
    let provider = CountingProvider::default();
    let mut handler = CountingHandler::new(StreamHandlerResult::Fail(diagnostic()));
    let result = fixture
        .state
        .replay_stream_failure_with_provider(fixture.writer, grant, &provider, &mut handler)
        .await
        .expect("stale grant yields a deterministic rejection");
    assert_eq!(result, CommitResult::Rejected(RejectReason::StaleFailure));
    assert_eq!(fixture.failure(fixture.writer).await, cancelled);
    fixture.assert_checkpoint(fixture.writer).await;
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    assert_eq!(
        (provider.calls.get(), handler.calls),
        (0, 0),
        "sys.admin.replay_failure / ORNA-SYS-064: stale grants fail before refetch or handler work"
    );
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #786; run explicitly for review"]
async fn replay_admission_counts_attempt_before_execution() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    fixture.admit().await;
    // No provider or handler is invoked: admission alone must persist N + 1.
    fixture.assert_one_replay_attempt(&fixture.failure(fixture.writer).await);
}

async fn successful_replay(fixture: &Fixture) -> FailureRecord {
    let grant = fixture.admit().await;
    let capture = fixture.state.capture().await.unwrap();
    // A valid no-op activation isolates replay accounting from mutation codecs.
    let mut handler = CountingHandler::new(StreamHandlerResult::Commit(StreamMutationBatch {
        mutations: Vec::new(),
        next_digest: capture.generation_digest(),
    }));
    let result = fixture
        .state
        .replay_stream_failure(fixture.writer, grant.clone(), &mut handler)
        .await
        .expect("execute successful replay");
    let completed = match result {
        CommitResult::ReplayCompleted { failure } => failure,
        other => panic!("expected successful replay, got {other:?}"),
    };
    assert_eq!(handler.calls, 1);
    assert_eq!(completed.identity, grant.failure);
    assert_ne!(completed.version, grant.version);
    assert_eq!(completed.status, FailureStatus::Replayed);
    assert_eq!(fixture.failure(fixture.writer).await, completed);
    fixture.assert_checkpoint(fixture.writer).await;
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    completed
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #786; run explicitly for review"]
async fn successful_replay_counts_exactly_one_attempt() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    let completed = successful_replay(&fixture).await;
    fixture.assert_one_replay_attempt(&completed);
}

#[tokio::test]
#[ignore = "Known Orna 1.0.0 gap: #786; run explicitly for review"]
async fn interrupted_replay_recovery_preserves_admitted_attempt() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    let grant = fixture.admit().await;
    let capture = fixture.state.capture().await.unwrap();
    // No callback/transaction is running or can commit in this fixture. Model
    // interruption after admission, before the external call, with explicit
    // writer takeover; this is not a physical-crash or liveness-detector test.
    let replacement = fixture
        .state
        .takeover_lease(fixture.writer, [5; 16])
        .await
        .expect("fence abandoned writer and recover replay");
    assert_eq!(
        fixture.state.current_lease().await.unwrap(),
        Some(replacement)
    );
    let recovered = fixture.failure(replacement).await;
    assert_eq!(recovered.identity, grant.failure);
    assert_ne!(recovered.version, grant.version);
    assert_eq!(recovered.status, FailureStatus::Skipped);
    fixture.assert_checkpoint(replacement).await;
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_one_replay_attempt(&recovered);
}

#[tokio::test]
async fn replay_execution_claim_rejects_concurrent_cancellation() {
    let fixture = Fixture::new(protected_payload()).await;
    let grant = fixture.admit().await;
    let capture = fixture.state.capture().await.unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = GatedProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let mut handler = CountingHandler::new(StreamHandlerResult::Commit(StreamMutationBatch {
        mutations: Vec::new(),
        next_digest: capture.generation_digest(),
    }));
    let mut replay = Box::pin(fixture.state.replay_stream_failure_with_provider(
        fixture.writer,
        grant.clone(),
        &provider,
        &mut handler,
    ));
    tokio::select! {
        result = &mut replay => panic!("replay completed before provider gate: {result:?}"),
        _ = entered.notified() => {}
    }

    let cancellation = fixture
        .state
        .stream_backend(fixture.writer)
        .apply_async(CommitIntent::ReplayCancel {
            failure: grant.failure.clone(),
            expected_version: grant.version,
        })
        .await
        .expect("cancel request should reach the durable claim");
    assert_eq!(
        cancellation,
        CommitResult::Rejected(RejectReason::LeaseAlreadyHeld),
        "an in-flight replay claim cannot be invalidated by cancellation"
    );
    assert_eq!(
        fixture.failure(fixture.writer).await.status,
        FailureStatus::Replaying
    );

    release.notify_one();
    let result = replay.await.expect("replay should finish after release");
    assert!(matches!(result, CommitResult::ReplayCompleted { .. }));
    assert_eq!(handler.calls, 1);
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_checkpoint(fixture.writer).await;
}

#[tokio::test]
async fn replay_execution_claim_is_fenced_by_owner_takeover() {
    let fixture = Fixture::new(protected_payload()).await;
    let grant = fixture.admit().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = GatedProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let mut handler = CountingHandler::new(StreamHandlerResult::Fail(diagnostic()));
    let mut replay = Box::pin(fixture.state.replay_stream_failure_with_provider(
        fixture.writer,
        grant.clone(),
        &provider,
        &mut handler,
    ));
    tokio::select! {
        result = &mut replay => panic!("replay completed before provider gate: {result:?}"),
        _ = entered.notified() => {}
    }

    let replacement = fixture
        .state
        .takeover_lease(fixture.writer, [5; 16])
        .await
        .expect("takeover should fence the in-flight replay claim");
    let recovered = fixture.failure(replacement).await;
    assert_eq!(recovered.status, FailureStatus::Skipped);
    fixture.assert_one_replay_attempt(&recovered);
    fixture.assert_checkpoint(replacement).await;

    release.notify_one();
    assert_eq!(
        replay.await.expect_err("the old owner must be fenced"),
        StreamStepError::Runtime(RuntimeError::OwnerLost)
    );
    assert_eq!(handler.calls, 0, "fenced replay must not reach the handler");
}

// Ordinary controls assert only their stated invariants. They do not assert
// an unchanged admitted count or label late accounting as correct admission.
#[tokio::test]
async fn valid_plaintext_replay_executes_handler_without_moving_checkpoint() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    successful_replay(&fixture).await;
}

#[tokio::test]
async fn failed_protected_replay_counts_one_attempt_without_moving_checkpoint() {
    let fixture = Fixture::new(protected_payload()).await;
    let grant = fixture.admit().await;
    let capture = fixture.state.capture().await.unwrap();
    let provider = CountingProvider::default();
    let mut handler = CountingHandler::new(StreamHandlerResult::Fail(diagnostic()));
    let result = fixture
        .state
        .replay_stream_failure_with_provider(fixture.writer, grant.clone(), &provider, &mut handler)
        .await
        .expect("execute failing replay");
    let failed = match result {
        CommitResult::ReplayFailed { failure } => failure,
        other => panic!("expected failed replay, got {other:?}"),
    };
    assert_eq!((provider.calls.get(), handler.calls), (1, 1));
    assert_eq!(failed.status, FailureStatus::Skipped);
    assert_ne!(failed.version, grant.version);
    assert_eq!(failed.diagnostic, diagnostic());
    assert_eq!(fixture.failure(fixture.writer).await, failed);
    fixture.assert_checkpoint(fixture.writer).await;
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_one_replay_attempt(&failed);
}

#[tokio::test]
async fn replay_cancel_counts_one_attempt_without_moving_checkpoint() {
    let fixture = Fixture::new(StreamFailurePayload::Plaintext(PAYLOAD.to_vec())).await;
    let grant = fixture.admit().await;
    let capture = fixture.state.capture().await.unwrap();
    let cancelled = fixture.cancel(&grant).await;
    assert_eq!(cancelled.diagnostic, fixture.skipped.diagnostic);
    assert_eq!(fixture.state.capture().await.unwrap(), capture);
    fixture.assert_one_replay_attempt(&cancelled);
}
