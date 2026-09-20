//! Stream delivery admission must win the durable Acquire race before the
//! connector performs failure-payload retention work.
//!
//! Normative authority: source/10-execution.md (ORNA-TXN-001/002,
//! ORNA-CONCUR-001), source/11-streams.md (ORNA-CONSUMER-006), and
//! source/12-checkpoints.md (DELIVERY-1 steps 2-5, ORNA-CP-001/005/006,
//! ORNA-SYS-061/062/064).

use std::{
    future::{Future, Ready, ready},
    path::Path,
    pin::Pin,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    RuntimeIdentity, RuntimeState, StreamCheckpoint, StreamHandler, StreamHandlerResult,
    StreamItem, StreamMutationBatch, StreamRunControl, StreamSource, StreamSourceDescriptor,
    StreamSourceKind, StreamSourcePoll, StreamStep, WriterLease,
};
use orna_stream_v1::{
    AsyncCheckpointBackend, CheckpointPrecondition, CommitIntent, CommitResult, Component,
    ConsumerIdentity, DeliveryIdentity, DiagnosticClass, DiagnosticCode, LeasePurpose, Position,
    RejectReason, SafeDiagnostic, StreamFailurePayload,
};
use tempfile::{Builder, TempDir};
use tokio::sync::Notify;

fn repository() -> (TempDir, Repository) {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("create workspace test artifact directory");
    let directory = Builder::new()
        .prefix("orna-v1-stream-admission-")
        .tempdir_in(target)
        .expect("create stream admission repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init for stream admission fixture");
    assert!(initialized.status.success(), "initialize stream admission repository");
    let repository = Repository::discover(directory.path()).expect("discover stream admission repository");
    (directory, repository)
}

async fn runtime() -> (TempDir, RuntimeState, WriterLease) {
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
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer lease");
    (directory, state, writer)
}

fn delivery(position: &str, successor: &str) -> DeliveryIdentity {
    DeliveryIdentity {
        consumer: ConsumerIdentity {
            principal: Component::new("stream-admission-principal").unwrap(),
            root: Component::new("stream-admission-root").unwrap(),
            function: Component::new("stream-admission-consumer").unwrap(),
            binding: Component::new("stream-admission-binding").unwrap(),
        },
        source_format: Component::new("stream-admission-source-format").unwrap(),
        source: Component::new("stream-admission-source").unwrap(),
        partition_format: Component::new("stream-admission-partition-format").unwrap(),
        partition: None,
        position_format: Component::new("stream-admission-position-format").unwrap(),
        position: Position {
            token: Component::new(position).unwrap(),
        },
        successor: Position {
            token: Component::new(successor).unwrap(),
        },
    }
}

fn failure_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    }
}

enum NextMode {
    Immediate,
    Blocked {
        started: Arc<Notify>,
        release: Arc<Notify>,
    },
}

struct CountedSource {
    key: orna_stream_v1::CheckpointKey,
    item: Option<StreamItem>,
    payload_calls: Arc<AtomicUsize>,
    mode: Option<NextMode>,
}

impl CountedSource {
    fn immediate(
        key: orna_stream_v1::CheckpointKey,
        delivery: DeliveryIdentity,
        payload_calls: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            key,
            item: Some(StreamItem {
                delivery,
                payload: vec![9, 8, 7],
            }),
            payload_calls,
            mode: Some(NextMode::Immediate),
        }
    }

    fn blocked(
        key: orna_stream_v1::CheckpointKey,
        delivery: DeliveryIdentity,
        payload_calls: Arc<AtomicUsize>,
        started: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Self {
        Self {
            key,
            item: Some(StreamItem {
                delivery,
                payload: vec![6, 5, 4],
            }),
            payload_calls,
            mode: Some(NextMode::Blocked { started, release }),
        }
    }
}

impl StreamSource for CountedSource {
    type NextFuture<'a>
        = Pin<Box<dyn Future<Output = Result<StreamSourcePoll, SafeDiagnostic>> + 'a>>
    where
        Self: 'a;
    type WaitFuture<'a>
        = Ready<Result<(), SafeDiagnostic>>
    where
        Self: 'a;

    fn descriptor(&self) -> StreamSourceDescriptor {
        StreamSourceDescriptor {
            kind: StreamSourceKind::Finite,
            replayable: true,
        }
    }

    fn checkpoint_key(&self) -> orna_stream_v1::CheckpointKey {
        self.key.clone()
    }

    fn failure_payload(&self, item: &StreamItem) -> StreamFailurePayload {
        self.payload_calls.fetch_add(1, Ordering::SeqCst);
        StreamFailurePayload::Plaintext(item.payload.clone())
    }

    fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
        let item = self.item.take();
        match self.mode.take().expect("source polled only once") {
            NextMode::Immediate => Box::pin(async move {
                Ok(item.map_or(StreamSourcePoll::Exhausted, |item| {
                    StreamSourcePoll::Item(Box::new(item))
                }))
            }),
            NextMode::Blocked { started, release } => Box::pin(async move {
                started.notify_one();
                release.notified().await;
                Ok(item.map_or(StreamSourcePoll::Exhausted, |item| {
                    StreamSourcePoll::Item(Box::new(item))
                }))
            }),
        }
    }

    fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
        ready(Ok(()))
    }
}

struct Handler {
    calls: usize,
    result: Option<StreamHandlerResult>,
}

impl Handler {
    fn commit() -> Self {
        Self {
            calls: 0,
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: Vec::new(),
                next_digest: [3; 32],
            })),
        }
    }

    fn fail() -> Self {
        Self {
            calls: 0,
            result: Some(StreamHandlerResult::Fail(failure_diagnostic())),
        }
    }
}

impl StreamHandler for Handler {
    fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
        self.calls += 1;
        self.result.take().expect("handler invoked only once")
    }
}

#[tokio::test]
async fn admitted_owner_calls_failure_payload_once_and_persists_failure_cleanup() {
    let (_directory, state, writer) = runtime().await;
    let item = delivery("admitted", "admitted-next");
    let key = item.checkpoint_key();
    let payload_calls = Arc::new(AtomicUsize::new(0));
    let mut source = CountedSource::immediate(key.clone(), item.clone(), payload_calls.clone());
    let mut handler = Handler::fail();
    let failed = match state
        .run_stream_once(writer, &key, &mut source, &mut handler)
        .await
        .expect("admitted failure should be a stream step")
    {
        StreamStep::Failed { failure } => failure,
        other => panic!("expected durable failure, got {other:?}"),
    };

    assert_eq!(payload_calls.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls, 1);
    assert_eq!(failed.attempts, 1);
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .expect("read unchanged checkpoint");
    assert_eq!(checkpoint.version, 0);
    assert_eq!(checkpoint.committed, None);
    assert_eq!(
        state
            .stream_backend(writer)
            .failure_async(&failed.identity)
            .await
            .expect("read durable failure")
            .as_ref()
            .map(|record| record.identity.clone()),
        Some(failed.identity.clone())
    );
}

#[tokio::test]
async fn stale_acquire_rejects_before_failure_payload_retention() {
    let (_directory, state, writer) = runtime().await;
    let stale = delivery("stale-position", "stale-successor");
    let key = stale.checkpoint_key();
    let initial = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .expect("read initial checkpoint");
    let stale_calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut source = CountedSource::blocked(
        key.clone(),
        stale,
        stale_calls.clone(),
        started.clone(),
        release.clone(),
    );
    let mut handler = Handler::commit();
    let mut run = Box::pin(state.run_stream_once(writer, &key, &mut source, &mut handler));
    tokio::select! {
        _ = started.notified() => {}
        result = &mut run => panic!("stream completed before poll was released: {result:?}"),
    }

    let racer = delivery("racing-position", "racing-successor");
    let racing_lease = match state
        .stream_backend(writer)
        .apply_async(CommitIntent::Acquire {
            delivery: racer,
            expected: CheckpointPrecondition::from(&initial),
            purpose: LeasePurpose::Deliver,
        })
        .await
        .expect("admit racing delivery")
    {
        CommitResult::Acquired { lease } => lease,
        other => panic!("expected racing delivery lease, got {other:?}"),
    };
    assert!(matches!(
        state
            .stream_backend(writer)
            .apply_async(CommitIntent::Complete {
                lease: racing_lease,
                expected: CheckpointPrecondition::from(&initial),
            })
            .await
            .expect("complete racing delivery"),
        CommitResult::CheckpointAdvanced { .. }
    ));

    release.notify_one();
    assert_eq!(
        run.await
            .expect("stale delivery should return a rejection"),
        StreamStep::Rejected(RejectReason::StaleCheckpoint)
    );
    assert_eq!(stale_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handler.calls, 0);
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .expect("read checkpoint after stale rejection");
    assert_eq!(checkpoint.version, 1);
    assert_eq!(checkpoint.committed.unwrap().token.as_str(), "racing-successor");
}

#[tokio::test]
async fn paused_acquire_rejects_after_poll_without_failure_payload_retention() {
    let (_directory, state, writer) = runtime().await;
    let item = delivery("paused-position", "paused-successor");
    let key = item.checkpoint_key();
    let payload_calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut source = CountedSource::blocked(
        key.clone(),
        item,
        payload_calls.clone(),
        started.clone(),
        release.clone(),
    );
    let mut handler = Handler::commit();
    let mut run = Box::pin(state.run_stream_once(writer, &key, &mut source, &mut handler));
    tokio::select! {
        _ = started.notified() => {}
        result = &mut run => panic!("stream completed before poll was released: {result:?}"),
    }
    assert_eq!(
        state.pause_stream(writer, key.clone()).await,
        Ok(orna_runtime_v1::StreamAdministrationOutcome::Paused { changed: true })
    );
    release.notify_one();
    assert!(matches!(
        run.await.expect("paused acquire should return a stream step"),
        StreamStep::Paused { .. }
    ));
    assert_eq!(payload_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handler.calls, 0);
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .expect("read unchanged paused checkpoint");
    assert_eq!(checkpoint.version, 0);
    assert_eq!(checkpoint.committed, None);
}

#[tokio::test]
async fn busy_acquire_rejects_before_failure_payload_retention_and_cancel_cleans_lease() {
    let (_directory, state, writer) = runtime().await;
    let item = delivery("busy-position", "busy-successor");
    let key = item.checkpoint_key();
    let initial = state
        .stream_backend(writer)
        .checkpoint_async(&key)
        .await
        .expect("read initial checkpoint");
    let held = match state
        .stream_backend(writer)
        .apply_async(CommitIntent::Acquire {
            delivery: item.clone(),
            expected: CheckpointPrecondition::from(&initial),
            purpose: LeasePurpose::Deliver,
        })
        .await
        .expect("hold delivery lease")
    {
        CommitResult::Acquired { lease } => lease,
        other => panic!("expected held delivery lease, got {other:?}"),
    };

    let payload_calls = Arc::new(AtomicUsize::new(0));
    let mut source = CountedSource::immediate(key.clone(), item, payload_calls.clone());
    let mut handler = Handler::commit();
    assert_eq!(
        state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .expect("busy acquire should return a rejection"),
        StreamStep::Rejected(RejectReason::LeaseAlreadyHeld)
    );
    assert_eq!(payload_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handler.calls, 0);

    assert!(matches!(
        state
            .stream_backend(writer)
            .apply_async(CommitIntent::Cancel { lease: held })
            .await
            .expect("cancel held delivery lease"),
        CommitResult::Cancelled { .. }
    ));
}
