//! Durable private local runtime state for one Orna worktree.
//!
//! The database is intentionally below the Git boundary: callers obtain its
//! location only from [`orna_repository_v1::Repository::runtime_paths`].
//! This crate does not publish, project, compact, or contact a remote.

use std::{
    fmt,
    future::{Future, Ready, ready},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::SystemTime,
};

use libsql::{Builder, Connection, TransactionBehavior, params};
use num_bigint::BigInt;
use orna_foundation_v1::{CanonicalSnapshot, CwdCapture, Snapshot};
use orna_repository_v1::Repository;
pub use orna_stream_v1::StreamFailurePayload;
use orna_stream_v1::{
    AsyncCheckpointBackend, AsyncFailurePayloadBackend, CancellationClassification,
    Checkpoint as StreamCheckpoint, CheckpointKey, CheckpointPrecondition, CommitIntent,
    CommitResult, Component, ConsumerIdentity, DeliveryIdentity, DeliveryLease, DiagnosticClass,
    DiagnosticCode, FailureIdentity, FailureRecord, FailureStatus, LeasePurpose, Position,
    RejectReason, ReplayGrant, SafeDiagnostic, StreamState, StreamStatus,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS runtime_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
    repository_id BLOB NOT NULL CHECK (length(repository_id) = 16),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32)
);
CREATE TABLE IF NOT EXISTS runtime_schema_migration (
    migration TEXT PRIMARY KEY CHECK (length(migration) > 0)
);
CREATE TABLE IF NOT EXISTS writer_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    epoch INTEGER NOT NULL CHECK (epoch > 0)
);
CREATE TABLE IF NOT EXISTS pending_mutation (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    mutation_id BLOB NOT NULL UNIQUE CHECK (length(mutation_id) = 16),
    payload BLOB NOT NULL,
    digest BLOB NOT NULL CHECK (length(digest) = 32)
);
CREATE TABLE IF NOT EXISTS checkpoint (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    generation INTEGER NOT NULL UNIQUE CHECK (generation >= 0),
    digest BLOB NOT NULL CHECK (length(digest) = 32),
    mutation_sequence INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS publication_freeze (
    intent_id BLOB PRIMARY KEY CHECK (length(intent_id) = 16),
    checkpoint_generation INTEGER NOT NULL,
    checkpoint_mutation_sequence INTEGER NOT NULL,
    checkpoint_digest BLOB NOT NULL CHECK (length(checkpoint_digest) = 32),
    frozen INTEGER NOT NULL CHECK (frozen = 1)
);
CREATE TABLE IF NOT EXISTS request_ledger (
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    state INTEGER NOT NULL CHECK (state IN (1, 2, 3, 4, 5)),
    terminal_outcome BLOB,
    PRIMARY KEY (session_id, request_id),
    CHECK (
        (state IN (1, 2) AND terminal_outcome IS NULL)
        OR (state IN (3, 4, 5) AND terminal_outcome IS NOT NULL)
    ),
    CHECK (terminal_outcome IS NULL OR length(terminal_outcome) <= 16777216)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
    consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
    consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
    consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
    source_format TEXT NOT NULL CHECK (length(source_format) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
    partition TEXT NOT NULL CHECK (length(partition) > 0),
    position_format TEXT NOT NULL CHECK (length(position_format) > 0),
    version INTEGER NOT NULL CHECK (version >= 0),
    committed_position TEXT,
    next_fence INTEGER NOT NULL CHECK (next_fence >= 0)
);
CREATE TABLE IF NOT EXISTS stream_failure (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
    key_id TEXT NOT NULL CHECK (length(key_id) > 0),
    consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
    consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
    consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
    consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
    source_format TEXT NOT NULL CHECK (length(source_format) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
    partition TEXT NOT NULL CHECK (length(partition) > 0),
    position_format TEXT NOT NULL CHECK (length(position_format) > 0),
    delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
    successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
    version INTEGER NOT NULL CHECK (version >= 0),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 7),
    diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 5),
    diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
);
CREATE TABLE IF NOT EXISTS stream_failure_payload (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
    payload BLOB,
    payload_reference TEXT,
    payload_digest BLOB,
    retention INTEGER NOT NULL CHECK (retention IN (1, 2)),
    CHECK (payload_digest IS NULL OR length(payload_digest) = 32),
    CHECK (
        (
            retention = 1
            AND payload IS NOT NULL
            AND payload_reference IS NULL
            AND payload_digest IS NULL
        )
        OR (
            retention = 2
            AND payload IS NULL
            AND payload_reference IS NOT NULL
            AND length(payload_reference) > 0
            AND payload_digest IS NOT NULL
        )
    )
);
CREATE TABLE IF NOT EXISTS stream_failure_payload_legacy (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0)
);
CREATE TABLE IF NOT EXISTS stream_provider_failure (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    checkpoint_version INTEGER NOT NULL CHECK (checkpoint_version >= 0),
    committed_position TEXT,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 5),
    diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
);
CREATE TABLE IF NOT EXISTS stream_lease (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
    successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
    fence INTEGER NOT NULL CHECK (fence > 0),
    purpose INTEGER NOT NULL CHECK (purpose BETWEEN 1 AND 2)
);
CREATE TABLE IF NOT EXISTS stream_retry_claim (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    identity_id TEXT NOT NULL CHECK (length(identity_id) > 0)
);
CREATE TABLE IF NOT EXISTS stream_control (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    status INTEGER NOT NULL CHECK (status IN (1, 2))
);
"#;

pub const MAX_TERMINAL_OUTCOME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeIdentity {
    pub database_id: [u8; 16],
    pub repository_id: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mutation {
    pub id: [u8; 16],
    pub payload: Vec<u8>,
    pub digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterLease {
    pub owner_id: [u8; 16],
    pub epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub generation: u64,
    pub digest: [u8; 32],
    pub mutation_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationFreeze {
    pub intent_id: [u8; 16],
    pub checkpoint: Checkpoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestIdentity {
    pub session_id: [u8; 16],
    pub request_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    Reserved,
    Running,
    Completed,
    Cancelled,
    Orphaned,
}

impl RequestState {
    const fn code(self) -> i64 {
        match self {
            Self::Reserved => 1,
            Self::Running => 2,
            Self::Completed => 3,
            Self::Cancelled => 4,
            Self::Orphaned => 5,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Orphaned)
    }
}

/// Bounded bytes retained without assigning runtime meaning to their format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalOutcome(Vec<u8>);

impl TerminalOutcome {
    pub fn new(bytes: Vec<u8>) -> Result<Self, RuntimeError> {
        if bytes.len() > MAX_TERMINAL_OUTCOME_BYTES {
            return Err(RuntimeError::TerminalOutcomeTooLarge);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestStatus {
    pub identity: RequestIdentity,
    pub fingerprint: [u8; 32],
    pub state: RequestState,
    pub terminal_outcome: Option<TerminalOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    AfterMutation,
    AfterCheckpoint,
    AfterCapture,
    AfterFailureRecord,
    AfterFailurePayload,
}

/// Deterministic seam for proving transaction rollback. Production callers use
/// [`NoFault`]; the seam never manufactures successful recovery.
pub trait FaultInjector: Send + Sync {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError>;
}

#[derive(Debug, Default)]
pub struct NoFault;
impl FaultInjector for NoFault {
    fn check(&self, _: FaultPoint) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidIdentity,
    InvalidDigest,
    StreamIdentityMismatch,
    StreamCheckpointStale,
    LeaseHeld,
    OwnerLost,
    StaleCapture { current: Box<CwdCapture> },
    InvalidCapture,
    EmptyMutationBatch,
    ConflictingPublicationIntent,
    RequestUnknown,
    RequestFingerprintMismatch,
    RequestStateConflict,
    TerminalOutcomeTooLarge,
    RecoveryInvalid,
    FaultInjected(FaultPoint),
    StorageUnavailable,
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not disclose local paths, SQL, payloads, or native error text.
        f.write_str(match self {
            Self::InvalidIdentity => "invalid runtime identity",
            Self::InvalidDigest => "invalid durable digest",
            Self::StreamIdentityMismatch => "stream source identity mismatch",
            Self::StreamCheckpointStale => "stream checkpoint is stale",
            Self::LeaseHeld => "runtime writer is held",
            Self::OwnerLost => "runtime writer ownership was lost",
            Self::StaleCapture { .. } => "runtime capture is stale",
            Self::InvalidCapture => "invalid runtime capture",
            Self::EmptyMutationBatch => "empty runtime mutation batch",
            Self::ConflictingPublicationIntent => "conflicting publication intent",
            Self::RequestUnknown => "runtime request is unknown",
            Self::RequestFingerprintMismatch => "runtime request fingerprint mismatch",
            Self::RequestStateConflict => "runtime request state conflict",
            Self::TerminalOutcomeTooLarge => "runtime terminal outcome exceeds its bound",
            Self::RecoveryInvalid => "runtime recovery validation failed",
            Self::FaultInjected(_) => "runtime fault injected",
            Self::StorageUnavailable => "runtime state unavailable",
        })
    }
}
impl std::error::Error for RuntimeError {}

pub struct RuntimeState {
    connection: Connection,
}

/// The immutable runtime context captured at activation admission.
///
/// Reads performed by an activation use this CWD capture even if another
/// activation advances the durable runtime generation while this value is
/// retained. The activation time is captured once and never recomputed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeActivationContext {
    capture: CwdCapture,
    activation_time: SystemTime,
}

impl RuntimeActivationContext {
    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }

    pub fn activation_time(&self) -> SystemTime {
        self.activation_time
    }
}

pub struct StreamDeliveryCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [Mutation],
    pub next_digest: [u8; 32],
    pub delivery: DeliveryLease,
    pub expected_stream: CheckpointPrecondition,
    pub faults: &'a dyn FaultInjector,
}

pub struct StreamReplayCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [Mutation],
    pub next_digest: [u8; 32],
    pub grant: ReplayGrant,
    pub faults: &'a dyn FaultInjector,
}

/// Connector-owned refetch for a protected failed-delivery reference.
///
/// The runtime supplies the opaque reference only to this callback. Returned
/// bytes are accepted only after the runtime verifies the durable digest;
/// provider errors are intentionally collapsed into a secret-free retry
/// diagnostic.
pub type StreamFailurePayloadFuture<'a, E> = Pin<Box<dyn Future<Output = Result<Vec<u8>, E>> + 'a>>;

pub trait StreamFailurePayloadProvider {
    type Error;

    fn refetch<'a>(&'a self, reference: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error>;
}

struct NoStreamFailurePayloadProvider;

impl StreamFailurePayloadProvider for NoStreamFailurePayloadProvider {
    type Error = ();

    fn refetch<'a>(&'a self, _: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error> {
        Box::pin(async { Err(()) })
    }
}

/// A provider result for one scheduler turn. Provider-specific positions stay
/// inside [`DeliveryIdentity`]; this boundary only admits opaque values.
pub enum StreamSourcePoll {
    Item(Box<StreamItem>),
    Waiting,
    Exhausted,
}

pub struct StreamItem {
    pub delivery: DeliveryIdentity,
    pub payload: Vec<u8>,
}

/// Redacted public metadata for a retained failure payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamFailurePayloadMetadata {
    pub plaintext_bytes: Option<u64>,
    pub protected_reference: bool,
    pub redacted: bool,
}

pub struct StreamMutationBatch {
    pub mutations: Vec<Mutation>,
    pub next_digest: [u8; 32],
}

pub enum StreamHandlerResult {
    Commit(StreamMutationBatch),
    Fail(SafeDiagnostic),
    Cancelled,
}

/// A provider failure retained against the exact checkpoint that was being
/// polled. It has no delivery identity because no item was admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamProviderFailure {
    pub checkpoint: StreamCheckpoint,
    pub attempts: u32,
    pub diagnostic: SafeDiagnostic,
}

fn is_cancellation_diagnostic(diagnostic: SafeDiagnostic) -> bool {
    diagnostic.code == DiagnosticCode::Cancelled
        || diagnostic.class == DiagnosticClass::Cancellation
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamSourceKind {
    Finite,
    Unbounded,
}

/// Stable connector capabilities used to interpret source closure and retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamSourceDescriptor {
    pub kind: StreamSourceKind,
    pub replayable: bool,
}

pub trait StreamSource {
    type NextFuture<'a>: Future<Output = Result<StreamSourcePoll, SafeDiagnostic>> + 'a
    where
        Self: 'a;
    type WaitFuture<'a>: Future<Output = Result<(), SafeDiagnostic>> + 'a
    where
        Self: 'a;

    fn descriptor(&self) -> StreamSourceDescriptor;
    /// Returns the exact durable stream key served by this connector when it
    /// can declare one. The default preserves compatibility with legacy
    /// connectors; keyed connectors are rejected before provider polling when
    /// their declaration does not match the requested durable stream.
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        None
    }
    /// Selects how a failed item can be preserved for administrative skip or
    /// replay. The default fails closed; every connector that can fail a
    /// delivery must explicitly select plaintext or a protected reference.
    fn failure_payload(&self, _item: &StreamItem) -> StreamFailurePayload {
        StreamFailurePayload::Unavailable
    }
    fn next<'a>(&'a mut self, checkpoint: &'a StreamCheckpoint) -> Self::NextFuture<'a>;
    /// Waits until the source can be polled again or the supplied control is
    /// cancelled. Connectors must wake this future for either event.
    fn wait<'a>(&'a mut self, control: &'a dyn StreamRunControl) -> Self::WaitFuture<'a>;
}

/// A finite, replayable source for the built-in list stream contract.
///
/// The source is stateless between polls: the durable checkpoint selects the
/// next item, so reconstructing the connector after a restart resumes from the
/// same opaque position without relying on process-local cursor state.
pub struct ListStreamSource {
    key: CheckpointKey,
    payloads: Vec<Vec<u8>>,
}

impl ListStreamSource {
    pub fn new(key: CheckpointKey, payloads: Vec<Vec<u8>>) -> Self {
        Self { key, payloads }
    }

    fn next_item(&self, checkpoint: &StreamCheckpoint) -> Result<StreamSourcePoll, SafeDiagnostic> {
        if checkpoint.key != self.key {
            return Err(SafeDiagnostic {
                code: DiagnosticCode::DecodeRejected,
                class: DiagnosticClass::Permanent,
            });
        }
        let index = match &checkpoint.committed {
            None => 0,
            Some(position) => {
                let token = position.token.as_str();
                let canonical = token == "0"
                    || (token.as_bytes().first().is_some_and(|first| *first != b'0')
                        && token.bytes().all(|byte| byte.is_ascii_digit()));
                if !canonical {
                    return Err(SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    });
                }
                token.parse::<usize>().map_err(|_| SafeDiagnostic {
                    code: DiagnosticCode::DecodeRejected,
                    class: DiagnosticClass::Permanent,
                })?
            }
        };
        if index > self.payloads.len() {
            return Err(SafeDiagnostic {
                code: DiagnosticCode::DecodeRejected,
                class: DiagnosticClass::Permanent,
            });
        }
        let Some(payload) = self.payloads.get(index) else {
            return Ok(StreamSourcePoll::Exhausted);
        };
        let position = Position {
            token: Component::new(index.to_string()).map_err(|_| SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            })?,
        };
        let successor_index = index.checked_add(1).ok_or(SafeDiagnostic {
            code: DiagnosticCode::Internal,
            class: DiagnosticClass::Permanent,
        })?;
        let successor = Position {
            token: Component::new(successor_index.to_string()).map_err(|_| SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            })?,
        };
        Ok(StreamSourcePoll::Item(Box::new(StreamItem {
            delivery: DeliveryIdentity {
                consumer: self.key.consumer.clone(),
                source_format: self.key.source_format.clone(),
                source: self.key.source.clone(),
                partition_format: self.key.partition_format.clone(),
                partition: self.key.partition.clone(),
                position_format: self.key.position_format.clone(),
                position,
                successor,
            },
            payload: payload.clone(),
        })))
    }
}

impl StreamSource for ListStreamSource {
    type NextFuture<'a>
        = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
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

    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        Some(self.key.clone())
    }

    fn failure_payload(&self, item: &StreamItem) -> StreamFailurePayload {
        StreamFailurePayload::Plaintext(item.payload.clone())
    }

    fn next<'a>(&'a mut self, checkpoint: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
        ready(self.next_item(checkpoint))
    }

    fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
        ready(Ok(()))
    }
}

pub trait StreamHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult;
}

/// Lets a stream owner stop admission between delivery transactions.
pub trait StreamRunControl {
    fn cancelled(&self) -> bool;
    /// Acquires the linearization point for a new delivery admission.
    fn acquire_admission(&self) -> bool;
    /// Releases the admission point after the durable acquire attempt returns.
    fn release_admission(&self);
}

/// Control for a finite runner that has no external cancellation request.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl StreamRunControl for NeverCancelled {
    fn cancelled(&self) -> bool {
        false
    }

    fn acquire_admission(&self) -> bool {
        true
    }

    fn release_admission(&self) {}
}

/// A cancellation gate which linearizes cancellation against one delivery
/// admission. Cancellation after admission is retained for the next boundary.
#[derive(Clone, Debug)]
pub struct StreamRunGate {
    state: Arc<AtomicU8>,
}

impl StreamRunGate {
    const RUNNING: u8 = 0;
    const ADMITTING: u8 = 1;
    const CANCEL_REQUESTED: u8 = 2;
    const CANCELLED: u8 = 3;

    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(Self::RUNNING)),
        }
    }

    /// Requests cancellation. `true` means this call first recorded it.
    pub fn cancel(&self) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            match state {
                Self::RUNNING => {
                    if self
                        .state
                        .compare_exchange(
                            state,
                            Self::CANCELLED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                Self::ADMITTING => {
                    if self
                        .state
                        .compare_exchange(
                            state,
                            Self::CANCEL_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                Self::CANCEL_REQUESTED | Self::CANCELLED => return false,
                _ => return false,
            }
        }
    }
}

impl Default for StreamRunGate {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRunControl for StreamRunGate {
    fn cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) >= Self::CANCEL_REQUESTED
    }

    fn acquire_admission(&self) -> bool {
        self.state
            .compare_exchange(
                Self::RUNNING,
                Self::ADMITTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn release_admission(&self) {
        let _ = self.state.compare_exchange(
            Self::ADMITTING,
            Self::RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.state.compare_exchange(
            Self::CANCEL_REQUESTED,
            Self::CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

struct AdmissionPermit<'a, C: StreamRunControl>(&'a C);

impl<C: StreamRunControl> Drop for AdmissionPermit<'_, C> {
    fn drop(&mut self) {
        self.0.release_admission();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamRunOutcome {
    Exhausted {
        delivered: usize,
        checkpoint: StreamCheckpoint,
    },
    Closed {
        delivered: usize,
        checkpoint: StreamCheckpoint,
    },
    Failed {
        delivered: usize,
        checkpoint: StreamCheckpoint,
        failure: Box<FailureRecord>,
    },
    Cancelled {
        delivered: usize,
        checkpoint: StreamCheckpoint,
    },
    Rejected {
        delivered: usize,
        checkpoint: StreamCheckpoint,
        reason: RejectReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamStep {
    Waiting,
    Exhausted,
    Committed { checkpoint: StreamCheckpoint },
    Failed { failure: FailureRecord },
    Cancelled { checkpoint: StreamCheckpoint },
    Rejected(RejectReason),
}

#[derive(Debug, Eq, PartialEq)]
pub enum StreamStepError {
    Provider(SafeDiagnostic),
    Runtime(RuntimeError),
}

impl fmt::Display for StreamStepError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(_) => formatter.write_str("stream provider failed"),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StreamStepError {}

/// Writer-fenced stream administration backed by one runtime state.
pub struct RuntimeStreamBackend<'a> {
    state: &'a RuntimeState,
    lease: WriterLease,
}

impl RuntimeState {
    /// Opens the `state.db` path resolved by Git for this exact worktree.
    pub async fn open(
        repository: &Repository,
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
    ) -> Result<Self, RuntimeError> {
        repository
            .runtime_paths()
            .ensure_exists()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Self::open_path(
            &repository.runtime_paths().state_db(),
            identity,
            initial_digest,
        )
        .await
    }

    async fn open_path(
        path: &Path,
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
    ) -> Result<Self, RuntimeError> {
        validate_identity(identity)?;
        validate_digest(initial_digest)?;
        let database = Builder::new_local(path)
            .build()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let connection = database
            .connect()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        connection
            .execute_batch(SCHEMA)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let state = Self { connection };
        state.initialize(identity, initial_digest).await?;
        state.migrate_stream_failure_payloads().await?;
        state.validate_recovery().await?;
        Ok(state)
    }

    /// Creates a stream backend whose mutations are fenced by this writer lease.
    pub fn stream_backend(&self, lease: WriterLease) -> RuntimeStreamBackend<'_> {
        RuntimeStreamBackend { state: self, lease }
    }

    /// Captures the fixed CWD and activation time for one root activation.
    pub async fn begin_activation(&self) -> Result<RuntimeActivationContext, RuntimeError> {
        Ok(RuntimeActivationContext {
            capture: self.capture().await?,
            activation_time: SystemTime::now(),
        })
    }

    /// Publishes one activation against the CWD capture admitted at its start.
    pub async fn commit_activation(
        &self,
        lease: WriterLease,
        context: &RuntimeActivationContext,
        mutations: &[Mutation],
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        self.commit_batch(lease, context.capture(), mutations, next_digest, faults)
            .await
    }

    async fn record_stream_provider_failure(
        &self,
        writer: WriterLease,
        expected: &StreamCheckpoint,
        diagnostic: SafeDiagnostic,
    ) -> Result<StreamProviderFailure, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, writer).await?;
        ensure_stream_checkpoint(&transaction, &expected.key).await?;
        let current = load_stream_checkpoint(&transaction, &expected.key).await?;
        if current != *expected {
            return Err(RuntimeError::StreamCheckpointStale);
        }
        let key_id = stream_key_id(&expected.key);
        let mut rows = transaction
            .query(
                "SELECT checkpoint_version, committed_position, attempts
                 FROM stream_provider_failure WHERE key_id = ?1",
                params![key_id.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let previous = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .map(|row| {
                let version = decode_u64(
                    row.get::<i64>(0)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?;
                let committed: Option<String> =
                    row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
                let attempts = decode_u32(
                    row.get::<i64>(2)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                )?;
                Ok::<_, RuntimeError>((version, committed, attempts))
            })
            .transpose()?;
        let attempts = match previous {
            Some((version, committed, attempts))
                if version == expected.version
                    && committed.as_deref()
                        == expected
                            .committed
                            .as_ref()
                            .map(|position| position.token.as_str()) =>
            {
                attempts
                    .checked_add(1)
                    .ok_or(RuntimeError::RecoveryInvalid)?
            }
            _ => 1,
        };
        transaction
            .execute(
                "INSERT INTO stream_provider_failure
                 (key_id, checkpoint_version, committed_position, attempts,
                  diagnostic_code, diagnostic_class)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(key_id) DO UPDATE SET
                    checkpoint_version = excluded.checkpoint_version,
                    committed_position = excluded.committed_position,
                    attempts = excluded.attempts,
                    diagnostic_code = excluded.diagnostic_code,
                    diagnostic_class = excluded.diagnostic_class",
                params![
                    key_id,
                    i64::try_from(expected.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    expected
                        .committed
                        .as_ref()
                        .map(|position| position.token.as_str().to_owned()),
                    i64::from(attempts),
                    encode_code(diagnostic.code),
                    encode_class(diagnostic.class),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(StreamProviderFailure {
            checkpoint: expected.clone(),
            attempts,
            diagnostic,
        })
    }

    async fn clear_stream_provider_failure(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
    ) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, writer).await?;
        transaction
            .execute(
                "DELETE FROM stream_provider_failure WHERE key_id = ?1",
                params![stream_key_id(key)],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    async fn fail_stream_delivery(
        &self,
        writer: WriterLease,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        self.fail_stream_delivery_with_faults(writer, lease, diagnostic, payload, &NoFault)
            .await
    }

    async fn fail_stream_delivery_with_faults(
        &self,
        writer: WriterLease,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
        faults: &dyn FaultInjector,
    ) -> Result<CommitResult, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, writer).await?;
        let (plaintext, reference, digest, retention) = match payload {
            StreamFailurePayload::Unavailable => {
                return Err(RuntimeError::RecoveryInvalid);
            }
            StreamFailurePayload::Plaintext(bytes) => (Some(bytes), None, None, 1_i64),
            StreamFailurePayload::ProtectedReference { reference, digest } => {
                if reference.is_empty() {
                    return Err(RuntimeError::InvalidIdentity);
                }
                (None, Some(reference), Some(digest.to_vec()), 2_i64)
            }
        };
        let result =
            apply_stream_intent_tx(&transaction, CommitIntent::Fail { lease, diagnostic }).await?;
        if let CommitResult::Failed { failure } = &result {
            faults.check(FaultPoint::AfterFailureRecord)?;
            transaction
                .execute(
                    "INSERT INTO stream_failure_payload
                     (identity_id, payload, payload_reference, payload_digest, retention)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(identity_id) DO NOTHING",
                    params![
                        stream_identity_id(&failure.identity),
                        plaintext,
                        reference,
                        digest,
                        retention,
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            faults.check(FaultPoint::AfterFailurePayload)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(result)
    }

    async fn release_stream_lease(
        &self,
        writer: WriterLease,
        lease: DeliveryLease,
    ) -> Result<(), StreamStepError> {
        let result = self
            .stream_backend(writer)
            .apply_async(CommitIntent::Cancel { lease })
            .await
            .map_err(StreamStepError::Runtime)?;
        match result {
            CommitResult::Cancelled { .. } | CommitResult::Rejected(RejectReason::LeaseFenced) => {
                Ok(())
            }
            _ => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
        }
    }

    /// Runs at most one provider delivery. Polling, handler execution and
    /// durable publication stay separated: only a successful handler result
    /// reaches the atomic mutation/checkpoint boundary.
    pub async fn run_stream_once<S, H>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        source: &mut S,
        handler: &mut H,
    ) -> Result<StreamStep, StreamStepError>
    where
        S: StreamSource,
        H: StreamHandler,
    {
        self.run_stream_once_controlled(writer, key, source, handler, &NeverCancelled)
            .await
    }

    async fn run_stream_once_controlled<S, H, C>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        source: &mut S,
        handler: &mut H,
        control: &C,
    ) -> Result<StreamStep, StreamStepError>
    where
        S: StreamSource,
        H: StreamHandler,
        C: StreamRunControl,
    {
        if source
            .checkpoint_key()
            .as_ref()
            .is_some_and(|source_key| source_key != key)
        {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch,
            ));
        }
        let checkpoint = self
            .stream_backend(writer)
            .checkpoint_async(key)
            .await
            .map_err(StreamStepError::Runtime)?;
        let poll = match source.next(&checkpoint).await {
            Ok(poll) => poll,
            Err(diagnostic) => {
                if control.cancelled() || is_cancellation_diagnostic(diagnostic) {
                    return Ok(StreamStep::Cancelled { checkpoint });
                }
                self.record_stream_provider_failure(writer, &checkpoint, diagnostic)
                    .await
                    .map_err(StreamStepError::Runtime)?;
                if control.cancelled() {
                    self.clear_stream_provider_failure(writer, key)
                        .await
                        .map_err(StreamStepError::Runtime)?;
                    return Ok(StreamStep::Cancelled { checkpoint });
                }
                return Err(StreamStepError::Provider(diagnostic));
            }
        };
        let StreamSourcePoll::Item(item) = poll else {
            return Ok(match poll {
                StreamSourcePoll::Waiting => StreamStep::Waiting,
                StreamSourcePoll::Exhausted => StreamStep::Exhausted,
                StreamSourcePoll::Item(_) => unreachable!(),
            });
        };
        if item.delivery.checkpoint_key() != *key {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch,
            ));
        }
        if control.cancelled() {
            return Ok(StreamStep::Cancelled { checkpoint });
        }
        let expected = CheckpointPrecondition::from(&checkpoint);
        let lease = {
            if !control.acquire_admission() {
                return Ok(StreamStep::Cancelled { checkpoint });
            }
            let _permit = AdmissionPermit(control);
            let mut stream = self.stream_backend(writer);
            match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: item.delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .map_err(StreamStepError::Runtime)?
            {
                CommitResult::Acquired { lease } => lease,
                CommitResult::Rejected(reason) => return Ok(StreamStep::Rejected(reason)),
                _ => {
                    return Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid));
                }
            }
        };

        match handler.handle(&item) {
            StreamHandlerResult::Commit(batch) => {
                let capture = self.capture().await.map_err(StreamStepError::Runtime)?;
                let faults = NoFault;
                let lease_for_cleanup = lease.clone();
                let result = self
                    .commit_stream_delivery(StreamDeliveryCommit {
                        writer,
                        expected_capture: &capture,
                        mutations: &batch.mutations,
                        next_digest: batch.next_digest,
                        delivery: lease_for_cleanup.clone(),
                        expected_stream: expected,
                        faults: &faults,
                    })
                    .await
                    .map_err(StreamStepError::Runtime)?
                    .1;
                match result {
                    CommitResult::CheckpointAdvanced { checkpoint } => {
                        Ok(StreamStep::Committed { checkpoint })
                    }
                    CommitResult::Rejected(reason) => {
                        self.release_stream_lease(writer, lease_for_cleanup).await?;
                        Ok(StreamStep::Rejected(reason))
                    }
                    _ => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
                }
            }
            StreamHandlerResult::Fail(diagnostic) => {
                self.require_owner(&self.connection, writer)
                    .await
                    .map_err(StreamStepError::Runtime)?;
                let failure_payload = source.failure_payload(&item);
                let lease_for_cleanup = lease.clone();
                let mut stream = self.stream_backend(writer);
                let result = stream
                    .fail_with_payload_async(lease, diagnostic, failure_payload)
                    .await;
                let result = match result {
                    Ok(result) => result,
                    Err(RuntimeError::OwnerLost) => {
                        return Err(StreamStepError::Runtime(RuntimeError::OwnerLost));
                    }
                    Err(error) => {
                        self.release_stream_lease(writer, lease_for_cleanup).await?;
                        return Err(StreamStepError::Runtime(error));
                    }
                };
                match result {
                    CommitResult::Failed { failure } => Ok(StreamStep::Failed { failure }),
                    CommitResult::Rejected(reason) => Ok(StreamStep::Rejected(reason)),
                    _ => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
                }
            }
            StreamHandlerResult::Cancelled => {
                let result = self
                    .stream_backend(writer)
                    .apply_async(CommitIntent::Cancel { lease })
                    .await
                    .map_err(StreamStepError::Runtime)?;
                match result {
                    CommitResult::Cancelled { checkpoint, .. } => {
                        Ok(StreamStep::Cancelled { checkpoint })
                    }
                    CommitResult::Rejected(reason) => Ok(StreamStep::Rejected(reason)),
                    _ => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
                }
            }
        }
    }

    /// Runs delivery transactions until the source exhausts, fails, rejects
    /// admission, or the owner requests cancellation. A provider `Waiting`
    /// result is re-armed through its async wait hook, so an unbounded source
    /// remains live without a runtime busy loop.
    pub async fn run_stream<S, H, C>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        source: &mut S,
        handler: &mut H,
        control: &C,
    ) -> Result<StreamRunOutcome, StreamStepError>
    where
        S: StreamSource,
        H: StreamHandler,
        C: StreamRunControl,
    {
        if source
            .checkpoint_key()
            .as_ref()
            .is_some_and(|source_key| source_key != key)
        {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch,
            ));
        }
        let mut checkpoint = self
            .stream_backend(writer)
            .checkpoint_async(key)
            .await
            .map_err(StreamStepError::Runtime)?;
        let source_descriptor = source.descriptor();
        let mut delivered = 0;
        loop {
            if control.cancelled() {
                return Ok(StreamRunOutcome::Cancelled {
                    delivered,
                    checkpoint,
                });
            }
            match self
                .run_stream_once_controlled(writer, key, source, handler, control)
                .await?
            {
                StreamStep::Waiting => {
                    if let Err(diagnostic) = source.wait(control as &dyn StreamRunControl).await {
                        if control.cancelled() || is_cancellation_diagnostic(diagnostic) {
                            return Ok(StreamRunOutcome::Cancelled {
                                delivered,
                                checkpoint,
                            });
                        }
                        self.record_stream_provider_failure(writer, &checkpoint, diagnostic)
                            .await
                            .map_err(StreamStepError::Runtime)?;
                        if control.cancelled() {
                            self.clear_stream_provider_failure(writer, key)
                                .await
                                .map_err(StreamStepError::Runtime)?;
                            return Ok(StreamRunOutcome::Cancelled {
                                delivered,
                                checkpoint,
                            });
                        }
                        return Err(StreamStepError::Provider(diagnostic));
                    }
                }
                StreamStep::Exhausted => {
                    return Ok(match source_descriptor.kind {
                        StreamSourceKind::Finite => StreamRunOutcome::Exhausted {
                            delivered,
                            checkpoint,
                        },
                        StreamSourceKind::Unbounded => StreamRunOutcome::Closed {
                            delivered,
                            checkpoint,
                        },
                    });
                }
                StreamStep::Committed { checkpoint: next } => {
                    delivered += 1;
                    checkpoint = next;
                }
                StreamStep::Failed { failure } => {
                    return Ok(StreamRunOutcome::Failed {
                        delivered,
                        checkpoint,
                        failure: Box::new(failure),
                    });
                }
                StreamStep::Cancelled { checkpoint: next } => {
                    return Ok(StreamRunOutcome::Cancelled {
                        delivered,
                        checkpoint: next,
                    });
                }
                StreamStep::Rejected(reason) => {
                    return Ok(StreamRunOutcome::Rejected {
                        delivered,
                        checkpoint,
                        reason,
                    });
                }
            }
        }
    }

    async fn initialize(
        &self,
        identity: RuntimeIdentity,
        digest: [u8; 32],
    ) -> Result<(), RuntimeError> {
        let runtime_id = *Uuid::new_v4().as_bytes();
        self.connection.execute(
            "INSERT INTO runtime_meta VALUES (1, ?1, ?2, ?3, 0, ?4) ON CONFLICT(singleton) DO NOTHING",
            params![identity.database_id.to_vec(), identity.repository_id.to_vec(), runtime_id.to_vec(), digest.to_vec()],
        ).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        let stored = self.identity().await?;
        if stored != identity {
            return Err(RuntimeError::InvalidIdentity);
        }
        Ok(())
    }

    async fn migrate_stream_failure_payloads(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let applied = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-failure-payload-v1')",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if applied == 1 {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO stream_failure_payload_legacy (identity_id)
                     SELECT failure.identity_id
                     FROM stream_failure AS failure
                     LEFT JOIN stream_failure_payload AS payload
                       ON payload.identity_id = failure.identity_id
                     WHERE payload.identity_id IS NULL",
                    (),
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    pub async fn identity(&self) -> Result<RuntimeIdentity, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT database_id, repository_id FROM runtime_meta WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        Ok(RuntimeIdentity {
            database_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            repository_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        })
    }

    pub async fn capture(&self) -> Result<CwdCapture, RuntimeError> {
        let mut rows = self.connection.query("SELECT database_id, runtime_id, generation, generation_digest FROM runtime_meta WHERE singleton = 1", ()).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let generation: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let digest = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        if generation < 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let snapshot = CanonicalSnapshot::cwd(database_id, runtime_id, BigInt::from(generation))
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        CwdCapture::new(snapshot, digest).map_err(|_| RuntimeError::RecoveryInvalid)
    }

    pub async fn acquire_lease(&self, owner_id: [u8; 16]) -> Result<WriterLease, RuntimeError> {
        validate_id(owner_id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = tx
            .query(
                "SELECT owner_id, epoch FROM writer_lease WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let owner = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let epoch: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if owner != owner_id {
                return Err(RuntimeError::LeaseHeld);
            }
            tx.commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(WriterLease {
                owner_id,
                epoch: u64::try_from(epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
            });
        }
        tx.execute(
            "INSERT INTO writer_lease VALUES (1, ?1, 1)",
            params![owner_id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(WriterLease { owner_id, epoch: 1 })
    }

    /// Explicit abandoned-owner handover. A caller cannot steal an unknown or
    /// changed lease: the recorded prior owner is part of the operation.
    pub async fn recover_abandoned(
        &self,
        abandoned: [u8; 16],
        replacement: [u8; 16],
    ) -> Result<WriterLease, RuntimeError> {
        validate_id(abandoned)?;
        validate_id(replacement)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let changed = transaction
            .execute(
                "UPDATE writer_lease
                 SET owner_id = ?1, epoch = epoch + 1
                 WHERE singleton = 1 AND owner_id = ?2",
                params![replacement.to_vec(), abandoned.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::OwnerLost);
        }
        transaction
            .execute("DELETE FROM stream_lease", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute("DELETE FROM stream_retry_claim", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "UPDATE stream_failure
                 SET version = version + 1, status = ?1
                 WHERE status = ?2",
                params![
                    encode_status(FailureStatus::Failed),
                    encode_status(FailureStatus::Retrying),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = transaction
            .query("SELECT epoch FROM writer_lease WHERE singleton = 1", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let epoch: i64 = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(WriterLease {
            owner_id: replacement,
            epoch: u64::try_from(epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
        })
    }

    pub async fn commit(
        &self,
        lease: WriterLease,
        expected: &CwdCapture,
        mutation: &Mutation,
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        self.commit_batch(
            lease,
            expected,
            std::slice::from_ref(mutation),
            next_digest,
            faults,
        )
        .await
    }

    /// Atomically append a non-empty activation batch, its one corresponding
    /// checkpoint, and the next CWD capture. A fault or validation failure
    /// rolls back the complete batch rather than exposing a partial prefix.
    pub async fn commit_batch(
        &self,
        lease: WriterLease,
        expected: &CwdCapture,
        mutations: &[Mutation],
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        validate_id(lease.owner_id)?;
        validate_mutations(mutations, next_digest)?;
        let current = self.capture().await?;
        if &current != expected {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, lease).await?;
        let next = append_mutations_tx(&tx, expected, mutations, next_digest, faults).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(next)
    }

    /// Commits handler mutations and the corresponding stream checkpoint in
    /// one writer-fenced transaction. A rejected stream precondition rolls
    /// back without exposing any mutation or changing the CWD capture.
    pub async fn commit_stream_delivery(
        &self,
        request: StreamDeliveryCommit<'_>,
    ) -> Result<(CwdCapture, CommitResult), RuntimeError> {
        let StreamDeliveryCommit {
            writer,
            expected_capture,
            mutations,
            next_digest,
            delivery,
            expected_stream,
            faults,
        } = request;
        validate_id(writer.owner_id)?;
        validate_stream_mutations(mutations, next_digest)?;
        let current = self.capture().await?;
        if &current != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        if mutations.is_empty() && next_digest != expected_capture.generation_digest() {
            return Err(RuntimeError::InvalidDigest);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let result = apply_stream_intent_tx(
            &tx,
            CommitIntent::Complete {
                lease: delivery,
                expected: expected_stream,
            },
        )
        .await?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx).await?;
            return Ok((current, result));
        }
        let next = if mutations.is_empty() {
            capture_tx(&tx).await?
        } else {
            append_mutations_tx(&tx, expected_capture, mutations, next_digest, faults).await?
        };
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok((next, result))
    }

    /// Commits replay mutations and the terminal replay transition without
    /// moving the live stream checkpoint. A rejected replay precondition
    /// leaves both the activation capture and failure row unchanged.
    pub async fn commit_stream_replay(
        &self,
        request: StreamReplayCommit<'_>,
    ) -> Result<(CwdCapture, CommitResult), RuntimeError> {
        let StreamReplayCommit {
            writer,
            expected_capture,
            mutations,
            next_digest,
            grant,
            faults,
        } = request;
        validate_id(writer.owner_id)?;
        validate_stream_mutations(mutations, next_digest)?;
        let current = self.capture().await?;
        if &current != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        if mutations.is_empty() && next_digest != expected_capture.generation_digest() {
            return Err(RuntimeError::InvalidDigest);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let result = apply_stream_intent_tx(
            &tx,
            CommitIntent::ReplayComplete {
                failure: grant.failure,
                expected_version: grant.version,
            },
        )
        .await?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx).await?;
            return Ok((current, result));
        }
        let next = if mutations.is_empty() {
            capture_tx(&tx).await?
        } else {
            append_mutations_tx(&tx, expected_capture, mutations, next_digest, faults).await?
        };
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok((next, result))
    }

    async fn fail_stream_replay(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
        diagnostic: SafeDiagnostic,
    ) -> Result<CommitResult, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, writer).await?;
        let result = apply_stream_intent_tx(
            &tx,
            CommitIntent::ReplayFail {
                failure: grant.failure.clone(),
                expected_version: grant.version,
                diagnostic,
            },
        )
        .await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(result)
    }

    /// Executes one replay grant against its retained plaintext payload. A
    /// protected reference remains explicitly unavailable until its connector
    /// supplies a refetch implementation; it is returned to `Skipped` rather
    /// than exposing the reference or leaving the row stuck in `Replaying`.
    pub async fn replay_stream_failure<H>(
        &self,
        writer: WriterLease,
        grant: ReplayGrant,
        handler: &mut H,
    ) -> Result<CommitResult, StreamStepError>
    where
        H: StreamHandler,
    {
        self.replay_stream_failure_inner(
            writer,
            grant,
            None::<&NoStreamFailurePayloadProvider>,
            handler,
        )
        .await
    }

    /// Executes one replay grant with connector-owned refetch for protected
    /// payload references. The runtime verifies the fetched bytes before the
    /// handler sees them and leaves a failed refetch explicitly skipped.
    pub async fn replay_stream_failure_with_provider<P, H>(
        &self,
        writer: WriterLease,
        grant: ReplayGrant,
        provider: &P,
        handler: &mut H,
    ) -> Result<CommitResult, StreamStepError>
    where
        P: StreamFailurePayloadProvider + ?Sized,
        H: StreamHandler,
    {
        self.replay_stream_failure_inner(writer, grant, Some(provider), handler)
            .await
    }

    async fn replay_stream_failure_inner<P, H>(
        &self,
        writer: WriterLease,
        grant: ReplayGrant,
        provider: Option<&P>,
        handler: &mut H,
    ) -> Result<CommitResult, StreamStepError>
    where
        P: StreamFailurePayloadProvider + ?Sized,
        H: StreamHandler,
    {
        self.require_owner(&self.connection, writer)
            .await
            .map_err(StreamStepError::Runtime)?;
        let payload = match load_stored_stream_failure_payload(&self.connection, &grant.failure)
            .await
            .map_err(StreamStepError::Runtime)?
        {
            Some(StoredStreamFailurePayload {
                payload: Some(payload),
                retention: 1,
                ..
            }) => payload,
            Some(StoredStreamFailurePayload {
                payload: None,
                reference: Some(reference),
                digest: Some(digest),
                retention: 2,
            }) => {
                let Some(provider) = provider else {
                    return self
                        .fail_stream_replay(
                            writer,
                            &grant,
                            SafeDiagnostic {
                                code: DiagnosticCode::ProviderUnavailable,
                                class: DiagnosticClass::Transient,
                            },
                        )
                        .await
                        .map_err(StreamStepError::Runtime);
                };
                let expected_digest: [u8; 32] = digest
                    .try_into()
                    .map_err(|_| StreamStepError::Runtime(RuntimeError::RecoveryInvalid))?;
                let payload = match provider.refetch(&reference).await {
                    Ok(payload) => payload,
                    Err(_) => {
                        return self
                            .fail_stream_replay(
                                writer,
                                &grant,
                                SafeDiagnostic {
                                    code: DiagnosticCode::ProviderUnavailable,
                                    class: DiagnosticClass::Transient,
                                },
                            )
                            .await
                            .map_err(StreamStepError::Runtime);
                    }
                };
                let actual_digest: [u8; 32] = Sha256::digest(&payload).into();
                if actual_digest != expected_digest {
                    return self
                        .fail_stream_replay(
                            writer,
                            &grant,
                            SafeDiagnostic {
                                code: DiagnosticCode::Internal,
                                class: DiagnosticClass::Permanent,
                            },
                        )
                        .await
                        .map_err(StreamStepError::Runtime);
                }
                payload
            }
            Some(_) | None => {
                return self
                    .fail_stream_replay(
                        writer,
                        &grant,
                        SafeDiagnostic {
                            code: DiagnosticCode::Internal,
                            class: DiagnosticClass::Permanent,
                        },
                    )
                    .await
                    .map_err(StreamStepError::Runtime);
            }
        };
        let expected_capture = self.capture().await.map_err(StreamStepError::Runtime)?;
        let item = StreamItem {
            delivery: grant.failure.0.clone(),
            payload,
        };
        match handler.handle(&item) {
            StreamHandlerResult::Commit(batch) => self
                .commit_stream_replay(StreamReplayCommit {
                    writer,
                    expected_capture: &expected_capture,
                    mutations: &batch.mutations,
                    next_digest: batch.next_digest,
                    grant,
                    faults: &NoFault,
                })
                .await
                .map(|(_, result)| result)
                .map_err(StreamStepError::Runtime),
            StreamHandlerResult::Fail(diagnostic) => self
                .fail_stream_replay(writer, &grant, diagnostic)
                .await
                .map_err(StreamStepError::Runtime),
            StreamHandlerResult::Cancelled => self
                .fail_stream_replay(
                    writer,
                    &grant,
                    SafeDiagnostic {
                        code: DiagnosticCode::Cancelled,
                        class: DiagnosticClass::Cancellation,
                    },
                )
                .await
                .map_err(StreamStepError::Runtime),
        }
    }

    pub async fn pending(&self) -> Result<Vec<Mutation>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT mutation_id, payload, digest FROM pending_mutation ORDER BY sequence",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            out.push(Mutation {
                id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                payload: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
                digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            });
        }
        Ok(out)
    }

    /// Returns the newest durable checkpoint, if this runtime has committed
    /// one. Checkpoints are never inferred from a pending mutation.
    pub async fn latest_checkpoint(&self) -> Result<Option<Checkpoint>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT generation, digest, mutation_sequence FROM checkpoint ORDER BY generation DESC LIMIT 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        else {
            return Ok(None);
        };
        let generation: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let mutation_sequence: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        Ok(Some(Checkpoint {
            generation: u64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
            digest: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            mutation_sequence: u64::try_from(mutation_sequence)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        }))
    }

    pub async fn freeze(
        &self,
        intent_id: [u8; 16],
        checkpoint: &Checkpoint,
    ) -> Result<PublicationFreeze, RuntimeError> {
        validate_id(intent_id)?;
        validate_digest(checkpoint.digest)?;
        if let Some(stored) = self.frozen_intent(intent_id).await? {
            if &stored != checkpoint {
                return Err(RuntimeError::ConflictingPublicationIntent);
            }
            return Ok(PublicationFreeze {
                intent_id,
                checkpoint: stored,
            });
        }
        if self.latest_checkpoint().await?.as_ref() != Some(checkpoint) {
            return Err(RuntimeError::RecoveryInvalid);
        }
        self.connection.execute("INSERT INTO publication_freeze VALUES (?1, ?2, ?3, ?4, 1) ON CONFLICT(intent_id) DO NOTHING", params![intent_id.to_vec(), i64::try_from(checkpoint.generation).map_err(|_| RuntimeError::RecoveryInvalid)?, i64::try_from(checkpoint.mutation_sequence).map_err(|_| RuntimeError::RecoveryInvalid)?, checkpoint.digest.to_vec()]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        let stored = self
            .frozen_intent(intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if &stored != checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        Ok(PublicationFreeze {
            intent_id,
            checkpoint: stored,
        })
    }

    /// Atomically reserves a REQUEST-1 identity. Repeating the same identity
    /// and fingerprint returns the durable record, including terminal replay.
    pub async fn reserve_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<RequestStatus, RuntimeError> {
        self.reserve_request_with_admission(identity, fingerprint)
            .await
            .map(|(status, _)| status)
    }

    /// Atomically reserves a REQUEST-1 identity and reports whether this call
    /// inserted the reservation. The boolean prevents a caller from treating
    /// an existing `Reserved` row as permission to execute after a restart.
    pub async fn reserve_request_with_admission(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<(RequestStatus, bool), RuntimeError> {
        validate_request_identity(identity)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if let Some(status) = request_status_tx(&tx, identity).await? {
            require_fingerprint(&status, fingerprint)?;
            tx.commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok((status, false));
        }
        tx.execute(
            "INSERT INTO request_ledger (session_id, request_id, fingerprint, state, terminal_outcome) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                identity.session_id.to_vec(),
                identity.request_id.to_vec(),
                fingerprint.to_vec(),
                RequestState::Reserved.code()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok((
            RequestStatus {
                identity,
                fingerprint,
                state: RequestState::Reserved,
                terminal_outcome: None,
            },
            true,
        ))
    }

    pub async fn start_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request(
            identity,
            fingerprint,
            &[RequestState::Reserved],
            RequestState::Running,
            None,
        )
        .await
    }

    pub async fn complete_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request(
            identity,
            fingerprint,
            &[RequestState::Reserved, RequestState::Running],
            RequestState::Completed,
            Some(outcome),
        )
        .await
    }

    pub async fn cancel_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request(
            identity,
            fingerprint,
            &[RequestState::Reserved, RequestState::Running],
            RequestState::Cancelled,
            Some(outcome),
        )
        .await
    }

    pub async fn orphan_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request(
            identity,
            fingerprint,
            &[RequestState::Running],
            RequestState::Orphaned,
            Some(outcome),
        )
        .await
    }

    pub async fn request_status(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<Option<RequestStatus>, RuntimeError> {
        let status = self.request_status_for_identity(identity).await?;
        if let Some(status) = &status {
            require_fingerprint(status, fingerprint)?;
        }
        Ok(status)
    }

    /// Returns a request record without a fingerprint precondition.
    ///
    /// Trusted runtime owners use this only after authenticating the session
    /// and request identity at their own boundary, for example to recover a
    /// cancellation target whose fingerprint is not part of the wire command.
    pub async fn request_status_for_identity(
        &self,
        identity: RequestIdentity,
    ) -> Result<Option<RequestStatus>, RuntimeError> {
        validate_request_identity(identity)?;
        request_status_tx(&self.connection, identity).await
    }

    async fn transition_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        allowed: &[RequestState],
        next: RequestState,
        terminal_outcome: Option<TerminalOutcome>,
    ) -> Result<RequestStatus, RuntimeError> {
        validate_request_identity(identity)?;
        debug_assert_eq!(next.is_terminal(), terminal_outcome.is_some());
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if !allowed.contains(&current.state) || current.state.is_terminal() {
            return Err(RuntimeError::RequestStateConflict);
        }
        let outcome_bytes = terminal_outcome
            .as_ref()
            .map(|outcome| outcome.as_bytes().to_vec());
        let changed = tx
            .execute(
                "UPDATE request_ledger SET state = ?1, terminal_outcome = ?2 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5 AND state = ?6",
                params![
                    next.code(),
                    outcome_bytes,
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    current.state.code()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RequestStatus {
            identity,
            fingerprint,
            state: next,
            terminal_outcome,
        })
    }

    async fn frozen_intent(&self, intent_id: [u8; 16]) -> Result<Option<Checkpoint>, RuntimeError> {
        let mut rows = self.connection.query("SELECT checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest FROM publication_freeze WHERE intent_id = ?1", params![intent_id.to_vec()]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        else {
            return Ok(None);
        };
        let generation: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let mutation_sequence: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        Ok(Some(Checkpoint {
            generation: u64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
            digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            mutation_sequence: u64::try_from(mutation_sequence)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        }))
    }

    pub async fn validate_recovery(&self) -> Result<(), RuntimeError> {
        let capture = self.capture().await?;
        let generation = capture
            .generation()
            .to_u64_digits()
            .1
            .first()
            .copied()
            .unwrap_or(0);
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*), COALESCE(MIN(generation), 0), COALESCE(MAX(generation), 0) FROM checkpoint",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let count: i64 = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let minimum: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let maximum: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_count = u64::try_from(count).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_minimum =
            u64::try_from(minimum).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_maximum =
            u64::try_from(maximum).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if (generation == 0 && checkpoint_count != 0)
            || (generation > 0
                && (checkpoint_count != generation
                    || checkpoint_minimum != 1
                    || checkpoint_maximum != generation))
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        match (generation, self.latest_checkpoint().await?) {
            (0, None) => {}
            (value, Some(checkpoint))
                if checkpoint.generation == value
                    && checkpoint.digest == capture.generation_digest() => {}
            _ => return Err(RuntimeError::RecoveryInvalid),
        }
        self.validate_checkpoint_anchors().await?;
        self.validate_stream_controls().await?;
        self.validate_stream_provider_failures().await?;
        self.validate_stream_failure_payloads().await?;
        let mut request_rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, state, terminal_outcome FROM request_ledger",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = request_rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            decode_request_status(&row).map_err(|_| RuntimeError::RecoveryInvalid)?;
        }
        Ok(())
    }

    /// A checkpoint is an anchor into the durable mutation ledger. Recovery
    /// must not accept a contiguous generation history whose anchors have
    /// been corrupted, removed, or reordered.
    async fn validate_checkpoint_anchors(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT checkpoint.generation, checkpoint.digest, checkpoint.mutation_sequence, pending_mutation.sequence \
                 FROM checkpoint LEFT JOIN pending_mutation ON pending_mutation.sequence = checkpoint.mutation_sequence \
                 ORDER BY checkpoint.generation",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut previous_sequence = 0;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let _: u64 = u64::try_from(
                row.get::<i64>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_digest(fixed(
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let sequence = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<i64> = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if sequence == 0
                || sequence <= previous_sequence
                || referenced.and_then(|value| u64::try_from(value).ok()) != Some(sequence)
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            previous_sequence = sequence;
        }

        let mut freezes = self
            .connection
            .query(
                "SELECT publication_freeze.intent_id, publication_freeze.checkpoint_generation, \
                 publication_freeze.checkpoint_mutation_sequence, publication_freeze.checkpoint_digest, \
                 publication_freeze.frozen, checkpoint.generation \
                 FROM publication_freeze LEFT JOIN checkpoint ON checkpoint.generation = publication_freeze.checkpoint_generation \
                 AND checkpoint.mutation_sequence = publication_freeze.checkpoint_mutation_sequence \
                 AND checkpoint.digest = publication_freeze.checkpoint_digest",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = freezes
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            validate_id(fixed(
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let generation = u64::try_from(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let sequence = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_digest(fixed(
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let frozen: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<i64> = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if generation == 0 || sequence == 0 || frozen != 1 || referenced.is_none() {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }

    async fn validate_stream_controls(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT stream_control.key_id, stream_control.status,
                        stream_checkpoint.key_id
                 FROM stream_control
                 LEFT JOIN stream_checkpoint
                   ON stream_checkpoint.key_id = stream_control.key_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let referenced: Option<String> =
                row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if referenced.as_deref() != Some(key_id.as_str()) {
                return Err(RuntimeError::RecoveryInvalid);
            }
            decode_stream_status(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
        }
        Ok(())
    }

    async fn validate_stream_provider_failures(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT failure.key_id, failure.checkpoint_version,
                        failure.committed_position, failure.attempts,
                        failure.diagnostic_code, failure.diagnostic_class,
                        checkpoint.version, checkpoint.committed_position
                 FROM stream_provider_failure AS failure
                 LEFT JOIN stream_checkpoint AS checkpoint
                   ON checkpoint.key_id = failure.key_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let failure_version = decode_u64(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let failure_position: Option<String> =
                row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let attempts = decode_u32(
                row.get::<i64>(3)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            if attempts == 0 {
                return Err(RuntimeError::RecoveryInvalid);
            }
            decode_code(
                row.get::<i64>(4)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            decode_class(
                row.get::<i64>(5)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let checkpoint_version = row
                .get::<Option<i64>>(6)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                .ok_or(RuntimeError::RecoveryInvalid)
                .and_then(decode_u64)?;
            let checkpoint_position: Option<String> =
                row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if failure_version != checkpoint_version || failure_position != checkpoint_position {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }

    async fn validate_stream_failure_payloads(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT failure.identity_id, payload.identity_id, payload.payload,
                        payload.payload_reference, payload.payload_digest, payload.retention,
                        legacy.identity_id
                 FROM stream_failure AS failure
                 LEFT JOIN stream_failure_payload AS payload
                   ON payload.identity_id = failure.identity_id
                 LEFT JOIN stream_failure_payload_legacy AS legacy
                   ON legacy.identity_id = failure.identity_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let identity_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if identity_id.is_empty() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let referenced: Option<String> =
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let legacy: Option<String> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if referenced.is_none() {
                if legacy.as_deref() != Some(identity_id.as_str()) {
                    return Err(RuntimeError::RecoveryInvalid);
                }
                continue;
            }
            if referenced.as_deref() != Some(identity_id.as_str()) || legacy.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let payload = StoredStreamFailurePayload {
                payload: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
                reference: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
                digest: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
                retention: row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
            };
            validate_stream_failure_payload(&payload)?;
        }

        let mut orphans = self
            .connection
            .query(
                "SELECT payload.identity_id
                 FROM stream_failure_payload AS payload
                 LEFT JOIN stream_failure AS failure
                   ON failure.identity_id = payload.identity_id
                 WHERE failure.identity_id IS NULL",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if orphans
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some()
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(())
    }

    async fn require_owner(&self, tx: &Connection, lease: WriterLease) -> Result<(), RuntimeError> {
        let mut rows = tx
            .query(
                "SELECT owner_id, epoch FROM writer_lease WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::OwnerLost)?;
        let owner = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let epoch: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if owner != lease.owner_id || u64::try_from(epoch).ok() != Some(lease.epoch) {
            return Err(RuntimeError::OwnerLost);
        }
        Ok(())
    }
}

impl RuntimeStreamBackend<'_> {
    /// Records a handler failure and its connector-selected retention record
    /// in one writer-fenced metadata transaction.
    pub async fn fail_async(
        &self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        self.state
            .fail_stream_delivery(self.lease, lease, diagnostic, payload)
            .await
    }

    /// Replays one granted failure without changing the live checkpoint.
    pub async fn replay_async<H>(
        &self,
        grant: ReplayGrant,
        handler: &mut H,
    ) -> Result<CommitResult, StreamStepError>
    where
        H: StreamHandler,
    {
        self.state
            .replay_stream_failure(self.lease, grant, handler)
            .await
    }

    /// Replays one granted failure, allowing its connector to refetch a
    /// protected payload reference after runtime digest verification.
    pub async fn replay_async_with_provider<P, H>(
        &self,
        grant: ReplayGrant,
        provider: &P,
        handler: &mut H,
    ) -> Result<CommitResult, StreamStepError>
    where
        P: StreamFailurePayloadProvider + ?Sized,
        H: StreamHandler,
    {
        self.state
            .replay_stream_failure_with_provider(self.lease, grant, provider, handler)
            .await
    }

    /// Returns redacted metadata for a retained failure payload.
    pub async fn failure_payload_metadata_async(
        &self,
        identity: &FailureIdentity,
    ) -> Result<Option<StreamFailurePayloadMetadata>, RuntimeError> {
        load_stream_failure_payload_metadata(&self.state.connection, identity).await
    }

    /// Returns the durable provider failure for a stream checkpoint, if one is
    /// retained. The row is only valid when its checkpoint still matches the
    /// current stream position.
    pub async fn provider_failure_async(
        &self,
        key: &CheckpointKey,
    ) -> Result<Option<StreamProviderFailure>, RuntimeError> {
        load_stream_provider_failure(&self.state.connection, key).await
    }
}

impl AsyncCheckpointBackend for RuntimeStreamBackend<'_> {
    type Error = RuntimeError;
    type ApplyFuture<'a>
        = Pin<Box<dyn Future<Output = Result<CommitResult, RuntimeError>> + 'a>>
    where
        Self: 'a;
    type CheckpointFuture<'a>
        = Pin<Box<dyn Future<Output = Result<StreamCheckpoint, RuntimeError>> + 'a>>
    where
        Self: 'a;
    type FailureFuture<'a>
        = Pin<Box<dyn Future<Output = Result<Option<FailureRecord>, RuntimeError>> + 'a>>
    where
        Self: 'a;

    fn apply_async<'a>(&'a mut self, intent: CommitIntent) -> Self::ApplyFuture<'a> {
        Box::pin(async move { apply_stream_intent(self.state, self.lease, intent).await })
    }

    fn checkpoint_async<'a>(&'a self, key: &'a CheckpointKey) -> Self::CheckpointFuture<'a> {
        Box::pin(async move { load_stream_checkpoint(&self.state.connection, key).await })
    }

    fn failure_async<'a>(&'a self, identity: &'a FailureIdentity) -> Self::FailureFuture<'a> {
        Box::pin(async move { load_stream_failure(&self.state.connection, identity).await })
    }
}

impl AsyncFailurePayloadBackend for RuntimeStreamBackend<'_> {
    type Error = RuntimeError;

    fn fail_with_payload_async<'a>(
        &'a mut self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Pin<Box<dyn Future<Output = Result<CommitResult, RuntimeError>> + 'a>> {
        Box::pin(async move {
            self.state
                .fail_stream_delivery(self.lease, lease, diagnostic, payload)
                .await
        })
    }
}

const STREAM_CHECKPOINT_SELECT: &str =
    "SELECT version, committed_position, next_fence FROM stream_checkpoint WHERE key_id = ?1";
const STREAM_FAILURE_SELECT: &str = "SELECT \
    consumer_principal, consumer_root, consumer_function, consumer_binding, \
    source_format, source, partition_format, partition, position_format, \
    delivery_position, successor_position, version, attempts, status, \
    diagnostic_code, diagnostic_class \
    FROM stream_failure WHERE identity_id = ?1";
const STREAM_FAILURE_PAYLOAD_SELECT: &str =
    "SELECT payload, payload_reference, payload_digest, retention
     FROM stream_failure_payload WHERE identity_id = ?1";
const STREAM_PROVIDER_FAILURE_SELECT: &str = "SELECT \
    checkpoint_version, committed_position, attempts, diagnostic_code, diagnostic_class \
    FROM stream_provider_failure WHERE key_id = ?1";

#[derive(Clone, Debug)]
struct StoredStreamLease {
    delivery_position: String,
    successor_position: String,
    fence: u64,
    purpose: LeasePurpose,
}

fn stream_key_id(key: &CheckpointKey) -> String {
    format!(
        "checkpoint/v1|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        key.consumer.canonical(),
        key.source_format.as_str(),
        key.source.as_str(),
        key.partition_format.as_str(),
        key.partition.as_str(),
        key.position_format.as_str(),
        key.consumer.root.as_str(),
        key.consumer.function.as_str(),
        key.consumer.binding.as_str(),
    )
}

fn stream_identity_id(identity: &FailureIdentity) -> String {
    identity.0.canonical()
}

async fn load_stream_provider_failure(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<Option<StreamProviderFailure>, RuntimeError> {
    let key_id = stream_key_id(key);
    let mut rows = connection
        .query(STREAM_PROVIDER_FAILURE_SELECT, params![key_id])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let checkpoint = load_stream_checkpoint(connection, key).await?;
    let version = decode_u64(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let committed: Option<String> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if version != checkpoint.version
        || committed.as_deref()
            != checkpoint
                .committed
                .as_ref()
                .map(|position| position.token.as_str())
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(StreamProviderFailure {
        checkpoint,
        attempts: decode_u32(
            row.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        diagnostic: decode_diagnostic(&row, 3)?,
    }))
}

fn row_text(row: &libsql::Row, index: i32) -> Result<String, RuntimeError> {
    row.get(index).map_err(|_| RuntimeError::RecoveryInvalid)
}

fn decode_component(value: String) -> Result<Component, RuntimeError> {
    Component::new(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

fn decode_position(value: String) -> Result<Position, RuntimeError> {
    Ok(Position {
        token: decode_component(value)?,
    })
}

fn decode_u64(value: i64) -> Result<u64, RuntimeError> {
    u64::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

fn decode_u32(value: i64) -> Result<u32, RuntimeError> {
    u32::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)
}

fn encode_status(status: FailureStatus) -> i64 {
    match status {
        FailureStatus::Failed => 1,
        FailureStatus::Retrying => 2,
        FailureStatus::Succeeded => 3,
        FailureStatus::Skipped => 4,
        FailureStatus::Replaying => 5,
        FailureStatus::Replayed => 6,
        FailureStatus::Resolved => 7,
    }
}

fn decode_status(value: i64) -> Result<FailureStatus, RuntimeError> {
    match value {
        1 => Ok(FailureStatus::Failed),
        2 => Ok(FailureStatus::Retrying),
        3 => Ok(FailureStatus::Succeeded),
        4 => Ok(FailureStatus::Skipped),
        5 => Ok(FailureStatus::Replaying),
        6 => Ok(FailureStatus::Replayed),
        7 => Ok(FailureStatus::Resolved),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn encode_code(code: DiagnosticCode) -> i64 {
    match code {
        DiagnosticCode::ProviderUnavailable => 1,
        DiagnosticCode::DecodeRejected => 2,
        DiagnosticCode::ExecutionRejected => 3,
        DiagnosticCode::Cancelled => 4,
        DiagnosticCode::Internal => 5,
    }
}

fn decode_code(value: i64) -> Result<DiagnosticCode, RuntimeError> {
    match value {
        1 => Ok(DiagnosticCode::ProviderUnavailable),
        2 => Ok(DiagnosticCode::DecodeRejected),
        3 => Ok(DiagnosticCode::ExecutionRejected),
        4 => Ok(DiagnosticCode::Cancelled),
        5 => Ok(DiagnosticCode::Internal),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn encode_class(class: DiagnosticClass) -> i64 {
    match class {
        DiagnosticClass::Transient => 1,
        DiagnosticClass::Permanent => 2,
        DiagnosticClass::Cancellation => 3,
    }
}

fn decode_class(value: i64) -> Result<DiagnosticClass, RuntimeError> {
    match value {
        1 => Ok(DiagnosticClass::Transient),
        2 => Ok(DiagnosticClass::Permanent),
        3 => Ok(DiagnosticClass::Cancellation),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn encode_purpose(purpose: LeasePurpose) -> i64 {
    match purpose {
        LeasePurpose::Deliver => 1,
        LeasePurpose::Skip => 2,
    }
}

fn decode_purpose(value: i64) -> Result<LeasePurpose, RuntimeError> {
    match value {
        1 => Ok(LeasePurpose::Deliver),
        2 => Ok(LeasePurpose::Skip),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn encode_stream_status(status: StreamStatus) -> i64 {
    match status {
        StreamStatus::Running => 1,
        StreamStatus::Paused => 2,
    }
}

fn decode_stream_status(value: i64) -> Result<StreamStatus, RuntimeError> {
    match value {
        1 => Ok(StreamStatus::Running),
        2 => Ok(StreamStatus::Paused),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn decode_diagnostic(row: &libsql::Row, code_index: i32) -> Result<SafeDiagnostic, RuntimeError> {
    Ok(SafeDiagnostic {
        code: decode_code(
            row.get::<i64>(code_index)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        class: decode_class(
            row.get::<i64>(code_index + 1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
    })
}

async fn ensure_stream_checkpoint(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO stream_checkpoint (
                key_id, consumer_principal, consumer_root, consumer_function,
                consumer_binding, source_format, source, partition_format,
                partition, position_format, version, committed_position, next_fence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, 0)
             ON CONFLICT(key_id) DO NOTHING",
            params![
                stream_key_id(key),
                key.consumer.principal.as_str().to_owned(),
                key.consumer.root.as_str().to_owned(),
                key.consumer.function.as_str().to_owned(),
                key.consumer.binding.as_str().to_owned(),
                key.source_format.as_str().to_owned(),
                key.source.as_str().to_owned(),
                key.partition_format.as_str().to_owned(),
                key.partition.as_str().to_owned(),
                key.position_format.as_str().to_owned(),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

async fn load_stream_checkpoint_state(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<(StreamCheckpoint, u64), RuntimeError> {
    let key_id = stream_key_id(key);
    let mut rows = connection
        .query(STREAM_CHECKPOINT_SELECT, params![key_id])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok((
            StreamCheckpoint {
                key: key.clone(),
                version: 0,
                committed: None,
            },
            0,
        ));
    };
    let committed: Option<String> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    Ok((
        StreamCheckpoint {
            key: key.clone(),
            version: decode_u64(
                row.get::<i64>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            committed: committed.map(decode_position).transpose()?,
        },
        decode_u64(
            row.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
    ))
}

async fn load_stream_checkpoint(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<StreamCheckpoint, RuntimeError> {
    Ok(load_stream_checkpoint_state(connection, key).await?.0)
}

async fn load_stream_status(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<StreamStatus, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT status FROM stream_control WHERE key_id = ?1",
            params![stream_key_id(key)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(StreamStatus::Running);
    };
    decode_stream_status(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)
}

async fn store_stream_status(
    connection: &Connection,
    key: &CheckpointKey,
    status: StreamStatus,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO stream_control (key_id, status) VALUES (?1, ?2)
             ON CONFLICT(key_id) DO UPDATE SET status = excluded.status",
            params![stream_key_id(key), encode_stream_status(status)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

async fn has_blocking_stream_failure(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM stream_failure
             WHERE key_id = ?1 AND status IN (?2, ?3, ?4)
             LIMIT 1",
            params![
                stream_key_id(key),
                encode_status(FailureStatus::Failed),
                encode_status(FailureStatus::Retrying),
                encode_status(FailureStatus::Replaying),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some())
}

async fn load_stream_failure(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<FailureRecord>, RuntimeError> {
    let identity_id = stream_identity_id(identity);
    let mut rows = connection
        .query(STREAM_FAILURE_SELECT, params![identity_id])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let delivery = DeliveryIdentity {
        consumer: ConsumerIdentity {
            principal: decode_component(row_text(&row, 0)?)?,
            root: decode_component(row_text(&row, 1)?)?,
            function: decode_component(row_text(&row, 2)?)?,
            binding: decode_component(row_text(&row, 3)?)?,
        },
        source_format: decode_component(row_text(&row, 4)?)?,
        source: decode_component(row_text(&row, 5)?)?,
        partition_format: decode_component(row_text(&row, 6)?)?,
        partition: decode_component(row_text(&row, 7)?)?,
        position_format: decode_component(row_text(&row, 8)?)?,
        position: decode_position(row_text(&row, 9)?)?,
        successor: decode_position(row_text(&row, 10)?)?,
    };
    Ok(Some(FailureRecord {
        identity: FailureIdentity(delivery),
        version: decode_u64(
            row.get::<i64>(11)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        attempts: decode_u32(
            row.get::<i64>(12)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        status: decode_status(
            row.get::<i64>(13)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        diagnostic: decode_diagnostic(&row, 14)?,
    }))
}

struct StoredStreamFailurePayload {
    payload: Option<Vec<u8>>,
    reference: Option<String>,
    digest: Option<Vec<u8>>,
    retention: i64,
}

fn validate_stream_failure_payload(
    payload: &StoredStreamFailurePayload,
) -> Result<(), RuntimeError> {
    if let Some(digest) = &payload.digest
        && digest.len() != 32
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    match payload.retention {
        1 if payload.payload.is_some()
            && payload.reference.is_none()
            && payload.digest.is_none() =>
        {
            Ok(())
        }
        2 if payload.payload.is_none()
            && payload
                .reference
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            && payload.digest.is_some() =>
        {
            Ok(())
        }
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

async fn load_stored_stream_failure_payload(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<StoredStreamFailurePayload>, RuntimeError> {
    let mut rows = connection
        .query(
            STREAM_FAILURE_PAYLOAD_SELECT,
            params![stream_identity_id(identity)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let payload = StoredStreamFailurePayload {
        payload: row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
        reference: row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        digest: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
        retention: row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
    };
    validate_stream_failure_payload(&payload)?;
    Ok(Some(payload))
}

async fn load_stream_failure_payload_metadata(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<StreamFailurePayloadMetadata>, RuntimeError> {
    let Some(payload) = load_stored_stream_failure_payload(connection, identity).await? else {
        return Ok(None);
    };
    Ok(Some(StreamFailurePayloadMetadata {
        plaintext_bytes: payload
            .payload
            .as_ref()
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
        protected_reference: payload.retention == 2,
        redacted: true,
    }))
}

async fn load_stream_lease(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<Option<StoredStreamLease>, RuntimeError> {
    let key_id = stream_key_id(key);
    let mut rows = connection
        .query(
            "SELECT delivery_position, successor_position, fence, purpose
             FROM stream_lease WHERE key_id = ?1",
            params![key_id],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    Ok(Some(StoredStreamLease {
        delivery_position: row_text(&row, 0)?,
        successor_position: row_text(&row, 1)?,
        fence: decode_u64(
            row.get::<i64>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        purpose: decode_purpose(
            row.get::<i64>(3)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
    }))
}

fn lease_matches(lease: &DeliveryLease, stored: &StoredStreamLease) -> bool {
    lease.fence == stored.fence
        && lease.purpose == stored.purpose
        && lease.delivery.position.token.as_str() == stored.delivery_position
        && lease.delivery.successor.token.as_str() == stored.successor_position
}

async fn stream_checkpoint_matches(
    connection: &Connection,
    key: &CheckpointKey,
    expected: &orna_stream_v1::CheckpointPrecondition,
) -> Result<bool, RuntimeError> {
    let (checkpoint, _) = load_stream_checkpoint_state(connection, key).await?;
    Ok(checkpoint.version == expected.version && checkpoint.committed == expected.committed)
}

async fn apply_stream_intent(
    state: &RuntimeState,
    lease: WriterLease,
    intent: CommitIntent,
) -> Result<CommitResult, RuntimeError> {
    let transaction = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    state.require_owner(&transaction, lease).await?;
    if matches!(intent, CommitIntent::Fail { .. }) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let result = apply_stream_intent_tx(&transaction, intent).await;
    match result {
        Ok(result) => {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(result)
        }
        Err(error) => Err(error),
    }
}

async fn apply_stream_intent_tx(
    connection: &Connection,
    intent: CommitIntent,
) -> Result<CommitResult, RuntimeError> {
    match intent {
        CommitIntent::Pause { key } => {
            ensure_stream_checkpoint(connection, &key).await?;
            if load_stream_lease(connection, &key).await?.is_some() {
                return Ok(CommitResult::Rejected(RejectReason::StreamBusy));
            }
            let changed = load_stream_status(connection, &key).await? != StreamStatus::Paused;
            if changed {
                store_stream_status(connection, &key, StreamStatus::Paused).await?;
            }
            Ok(CommitResult::StreamStatusChanged {
                state: StreamState {
                    key,
                    status: StreamStatus::Paused,
                },
                changed,
            })
        }
        CommitIntent::Resume { key } => {
            ensure_stream_checkpoint(connection, &key).await?;
            if load_stream_status(connection, &key).await? == StreamStatus::Running {
                return Ok(CommitResult::StreamStatusChanged {
                    state: StreamState {
                        key,
                        status: StreamStatus::Running,
                    },
                    changed: false,
                });
            }
            if load_stream_lease(connection, &key).await?.is_some() {
                return Ok(CommitResult::Rejected(RejectReason::StreamBusy));
            }
            if has_blocking_stream_failure(connection, &key).await? {
                return Ok(CommitResult::Rejected(RejectReason::BlockingFailure));
            }
            store_stream_status(connection, &key, StreamStatus::Running).await?;
            Ok(CommitResult::StreamStatusChanged {
                state: StreamState {
                    key,
                    status: StreamStatus::Running,
                },
                changed: true,
            })
        }
        CommitIntent::Reset { key, expected, to } => {
            ensure_stream_checkpoint(connection, &key).await?;
            if load_stream_status(connection, &key).await? != StreamStatus::Paused {
                return Ok(CommitResult::Rejected(RejectReason::StreamNotPaused));
            }
            if load_stream_lease(connection, &key).await?.is_some() {
                return Ok(CommitResult::Rejected(RejectReason::StreamBusy));
            }
            if has_blocking_stream_failure(connection, &key).await? {
                return Ok(CommitResult::Rejected(RejectReason::BlockingFailure));
            }
            if !stream_checkpoint_matches(connection, &key, &expected).await? {
                return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
            }
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "UPDATE stream_checkpoint
                     SET committed_position = ?2, version = version + 1
                     WHERE key_id = ?1",
                    params![key_id, to.token.as_str().to_owned()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_provider_failure WHERE key_id = ?1",
                    params![stream_key_id(&key)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::CheckpointReset {
                checkpoint: load_stream_checkpoint(connection, &key).await?,
            })
        }
        CommitIntent::Acquire {
            delivery,
            expected,
            purpose,
        } => {
            let key = delivery.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            if !stream_checkpoint_matches(connection, &key, &expected).await? {
                return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
            }
            let identity = FailureIdentity(delivery.clone());
            let failure_status = load_stream_failure(connection, &identity)
                .await?
                .map(|failure| failure.status);
            if let Some(stored) = load_stream_lease(connection, &key).await? {
                if purpose == LeasePurpose::Deliver
                    && failure_status == Some(FailureStatus::Retrying)
                    && stored.purpose == LeasePurpose::Deliver
                {
                    let claimed = connection
                        .execute(
                            "DELETE FROM stream_retry_claim
                             WHERE key_id = ?1 AND identity_id = ?2",
                            params![stream_key_id(&key), stream_identity_id(&identity)],
                        )
                        .await
                        .map_err(|_| RuntimeError::StorageUnavailable)?;
                    if claimed == 1 {
                        return Ok(CommitResult::Acquired {
                            lease: DeliveryLease {
                                delivery,
                                fence: stored.fence,
                                purpose: stored.purpose,
                            },
                        });
                    }
                }
                return Ok(CommitResult::Rejected(RejectReason::LeaseAlreadyHeld));
            }
            if let Some(status) = failure_status {
                let allowed = matches!(
                    (purpose, status),
                    (LeasePurpose::Deliver, FailureStatus::Retrying)
                        | (LeasePurpose::Skip, FailureStatus::Failed)
                );
                if !allowed {
                    return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
                }
            } else if purpose == LeasePurpose::Skip {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            }
            if purpose == LeasePurpose::Deliver
                && load_stream_status(connection, &key).await? == StreamStatus::Paused
                && failure_status != Some(FailureStatus::Retrying)
            {
                return Ok(CommitResult::Rejected(RejectReason::StreamPaused));
            }
            let (_, next_fence) = load_stream_checkpoint_state(connection, &key).await?;
            let fence = next_fence
                .checked_add(1)
                .ok_or(RuntimeError::RecoveryInvalid)?;
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "UPDATE stream_checkpoint SET next_fence = ?2 WHERE key_id = ?1",
                    params![
                        key_id.clone(),
                        i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "INSERT INTO stream_lease
                     (key_id, delivery_position, successor_position, fence, purpose)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        key_id.clone(),
                        delivery.position.token.as_str().to_owned(),
                        delivery.successor.token.as_str().to_owned(),
                        i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                        encode_purpose(purpose),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_provider_failure WHERE key_id = ?1",
                    params![key_id],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::Acquired {
                lease: DeliveryLease {
                    delivery,
                    fence,
                    purpose,
                },
            })
        }
        CommitIntent::Fail { lease, diagnostic } => {
            let key = lease.delivery.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(stored) = load_stream_lease(connection, &key).await? else {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            };
            if lease.purpose != LeasePurpose::Deliver || !lease_matches(&lease, &stored) {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            }
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "DELETE FROM stream_lease WHERE key_id = ?1",
                    params![key_id.clone()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_retry_claim WHERE key_id = ?1",
                    params![key_id.clone()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let identity = FailureIdentity(lease.delivery.clone());
            let identity_id = stream_identity_id(&identity);
            let delivery = lease.delivery;
            connection
                .execute(
                    "INSERT INTO stream_failure (
                        identity_id, key_id, consumer_principal, consumer_root,
                        consumer_function, consumer_binding, source_format, source,
                        partition_format, partition, position_format,
                        delivery_position, successor_position, version, attempts,
                        status, diagnostic_code, diagnostic_class
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                               ?12, ?13, 1, 1, ?14, ?15, ?16)
                     ON CONFLICT(identity_id) DO UPDATE SET
                        version = stream_failure.version + 1,
                        attempts = stream_failure.attempts + 1,
                        status = ?14,
                        diagnostic_code = ?15,
                        diagnostic_class = ?16",
                    params![
                        identity_id,
                        key_id,
                        delivery.consumer.principal.as_str().to_owned(),
                        delivery.consumer.root.as_str().to_owned(),
                        delivery.consumer.function.as_str().to_owned(),
                        delivery.consumer.binding.as_str().to_owned(),
                        delivery.source_format.as_str().to_owned(),
                        delivery.source.as_str().to_owned(),
                        delivery.partition_format.as_str().to_owned(),
                        delivery.partition.as_str().to_owned(),
                        delivery.position_format.as_str().to_owned(),
                        delivery.position.token.as_str().to_owned(),
                        delivery.successor.token.as_str().to_owned(),
                        encode_status(FailureStatus::Failed),
                        encode_code(diagnostic.code),
                        encode_class(diagnostic.class),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::Failed {
                failure: load_stream_failure(connection, &identity)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            })
        }
        CommitIntent::Retry {
            failure,
            expected_version,
            expected,
        } => {
            let key = failure.0.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            if !stream_checkpoint_matches(connection, &key, &expected).await? {
                return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
            }
            let Some(record) = load_stream_failure(connection, &failure).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_version {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if record.status != FailureStatus::Failed {
                return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
            }
            if load_stream_lease(connection, &key).await?.is_some() {
                return Ok(CommitResult::Rejected(RejectReason::LeaseAlreadyHeld));
            }
            let (_, next_fence) = load_stream_checkpoint_state(connection, &key).await?;
            let fence = next_fence
                .checked_add(1)
                .ok_or(RuntimeError::RecoveryInvalid)?;
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "UPDATE stream_checkpoint SET next_fence = ?2 WHERE key_id = ?1",
                    params![
                        key_id.clone(),
                        i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "INSERT INTO stream_lease
                     (key_id, delivery_position, successor_position, fence, purpose)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        key_id.clone(),
                        failure.0.position.token.as_str().to_owned(),
                        failure.0.successor.token.as_str().to_owned(),
                        i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                        encode_purpose(LeasePurpose::Deliver),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let identity_id = stream_identity_id(&failure);
            connection
                .execute(
                    "INSERT INTO stream_retry_claim (key_id, identity_id)
                     VALUES (?1, ?2)",
                    params![key_id, identity_id.clone()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![identity_id, encode_status(FailureStatus::Retrying)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::RetryScheduled {
                failure: load_stream_failure(connection, &failure)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            })
        }
        CommitIntent::Complete { lease, expected } => {
            let key = lease.delivery.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            if !stream_checkpoint_matches(connection, &key, &expected).await? {
                return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
            }
            let Some(stored) = load_stream_lease(connection, &key).await? else {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            };
            if lease.purpose != LeasePurpose::Deliver || !lease_matches(&lease, &stored) {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            }
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "UPDATE stream_checkpoint
                     SET committed_position = ?2, version = version + 1
                     WHERE key_id = ?1",
                    params![
                        key_id.clone(),
                        lease.delivery.successor.token.as_str().to_owned()
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_lease WHERE key_id = ?1",
                    params![key_id],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_retry_claim WHERE key_id = ?1",
                    params![stream_key_id(&key)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let identity_id = stream_identity_id(&FailureIdentity(lease.delivery));
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![identity_id, encode_status(FailureStatus::Succeeded)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::CheckpointAdvanced {
                checkpoint: load_stream_checkpoint(connection, &key).await?,
            })
        }
        CommitIntent::Skip {
            lease,
            expected,
            expected_failure_version,
        } => {
            let key = lease.delivery.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let identity = FailureIdentity(lease.delivery.clone());
            let Some(record) = load_stream_failure(connection, &identity).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_failure_version || record.status != FailureStatus::Failed
            {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if !stream_checkpoint_matches(connection, &key, &expected).await? {
                return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
            }
            let Some(stored) = load_stream_lease(connection, &key).await? else {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            };
            if lease.purpose != LeasePurpose::Skip || !lease_matches(&lease, &stored) {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            }
            let key_id = stream_key_id(&key);
            connection
                .execute(
                    "UPDATE stream_checkpoint
                     SET committed_position = ?2, version = version + 1
                     WHERE key_id = ?1",
                    params![
                        key_id.clone(),
                        lease.delivery.successor.token.as_str().to_owned()
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_lease WHERE key_id = ?1",
                    params![key_id],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_retry_claim WHERE key_id = ?1",
                    params![stream_key_id(&key)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![
                        stream_identity_id(&identity),
                        encode_status(FailureStatus::Skipped)
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::CheckpointAdvanced {
                checkpoint: load_stream_checkpoint(connection, &key).await?,
            })
        }
        CommitIntent::Replay {
            failure,
            expected_version,
        } => {
            let key = failure.0.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(record) = load_stream_failure(connection, &failure).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_version {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if record.status != FailureStatus::Skipped {
                return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
            }
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![
                        stream_identity_id(&failure),
                        encode_status(FailureStatus::Replaying)
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::ReplayGranted {
                grant: orna_stream_v1::ReplayGrant {
                    failure,
                    version: record.version + 1,
                },
            })
        }
        CommitIntent::ReplayComplete {
            failure,
            expected_version,
        } => {
            let key = failure.0.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(record) = load_stream_failure(connection, &failure).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_version {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if record.status != FailureStatus::Replaying {
                return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
            }
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![
                        stream_identity_id(&failure),
                        encode_status(FailureStatus::Replayed)
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::ReplayCompleted {
                failure: load_stream_failure(connection, &failure)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            })
        }
        CommitIntent::ReplayFail {
            failure,
            expected_version,
            diagnostic,
        } => {
            let key = failure.0.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(record) = load_stream_failure(connection, &failure).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_version {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if record.status != FailureStatus::Replaying {
                return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
            }
            connection
                .execute(
                    "UPDATE stream_failure
                     SET version = version + 1, attempts = attempts + 1,
                         status = ?2, diagnostic_code = ?3, diagnostic_class = ?4
                     WHERE identity_id = ?1",
                    params![
                        stream_identity_id(&failure),
                        encode_status(FailureStatus::Skipped),
                        encode_code(diagnostic.code),
                        encode_class(diagnostic.class),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::ReplayFailed {
                failure: load_stream_failure(connection, &failure)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            })
        }
        CommitIntent::Resolve {
            failure,
            expected_version,
        } => {
            let key = failure.0.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(record) = load_stream_failure(connection, &failure).await? else {
                return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
            };
            if record.version != expected_version {
                return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
            }
            if !matches!(
                record.status,
                FailureStatus::Succeeded | FailureStatus::Skipped | FailureStatus::Replayed
            ) {
                return Ok(CommitResult::Rejected(RejectReason::ResolveBlocked));
            }
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1",
                    params![
                        stream_identity_id(&failure),
                        encode_status(FailureStatus::Resolved)
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::Resolved {
                failure: load_stream_failure(connection, &failure)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?,
            })
        }
        CommitIntent::Cancel { lease } => {
            let key = lease.delivery.checkpoint_key();
            ensure_stream_checkpoint(connection, &key).await?;
            let Some(stored) = load_stream_lease(connection, &key).await? else {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            };
            if !lease_matches(&lease, &stored) {
                return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
            }
            connection
                .execute(
                    "DELETE FROM stream_lease WHERE key_id = ?1",
                    params![stream_key_id(&key)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "DELETE FROM stream_retry_claim WHERE key_id = ?1",
                    params![stream_key_id(&key)],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            connection
                .execute(
                    "UPDATE stream_failure SET version = version + 1, status = ?2
                     WHERE identity_id = ?1 AND status = ?3",
                    params![
                        stream_identity_id(&FailureIdentity(lease.delivery)),
                        encode_status(FailureStatus::Failed),
                        encode_status(FailureStatus::Retrying),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(CommitResult::Cancelled {
                checkpoint: load_stream_checkpoint(connection, &key).await?,
                classification: CancellationClassification::RollbackShaped,
            })
        }
    }
}

async fn request_status_tx(
    connection: &Connection,
    identity: RequestIdentity,
) -> Result<Option<RequestStatus>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT session_id, request_id, fingerprint, state, terminal_outcome FROM request_ledger WHERE session_id = ?1 AND request_id = ?2",
            params![identity.session_id.to_vec(), identity.request_id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    rows.next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .map(|row| decode_request_status(&row))
        .transpose()
}

fn decode_request_status(row: &libsql::Row) -> Result<RequestStatus, RuntimeError> {
    let identity = RequestIdentity {
        session_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        request_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
    };
    validate_request_identity(identity).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let fingerprint = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let state = match row
        .get::<i64>(3)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
    {
        1 => RequestState::Reserved,
        2 => RequestState::Running,
        3 => RequestState::Completed,
        4 => RequestState::Cancelled,
        5 => RequestState::Orphaned,
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let outcome: Option<Vec<u8>> = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let terminal_outcome = outcome
        .map(TerminalOutcome::new)
        .transpose()
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    if state.is_terminal() != terminal_outcome.is_some() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(RequestStatus {
        identity,
        fingerprint,
        state,
        terminal_outcome,
    })
}

fn require_fingerprint(status: &RequestStatus, fingerprint: [u8; 32]) -> Result<(), RuntimeError> {
    if status.fingerprint == fingerprint {
        Ok(())
    } else {
        Err(RuntimeError::RequestFingerprintMismatch)
    }
}

async fn capture_tx(connection: &Connection) -> Result<CwdCapture, RuntimeError> {
    let mut rows = connection.query("SELECT database_id, runtime_id, generation, generation_digest FROM runtime_meta WHERE singleton = 1", ()).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let digest = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    if generation < 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    CwdCapture::new(
        Snapshot::cwd(database_id, runtime_id, BigInt::from(generation))
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
        digest,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

fn validate_mutations(mutations: &[Mutation], next_digest: [u8; 32]) -> Result<(), RuntimeError> {
    if mutations.is_empty() {
        return Err(RuntimeError::EmptyMutationBatch);
    }
    validate_stream_mutations(mutations, next_digest)
}

fn validate_stream_mutations(
    mutations: &[Mutation],
    next_digest: [u8; 32],
) -> Result<(), RuntimeError> {
    for mutation in mutations {
        validate_id(mutation.id)?;
        validate_digest(mutation.digest)?;
    }
    validate_digest(next_digest)
}

async fn append_mutations_tx(
    connection: &Connection,
    expected: &CwdCapture,
    mutations: &[Mutation],
    next_digest: [u8; 32],
    faults: &dyn FaultInjector,
) -> Result<CwdCapture, RuntimeError> {
    let current = capture_tx(connection).await?;
    if &current != expected {
        return Err(RuntimeError::StaleCapture {
            current: Box::new(current),
        });
    }
    let mut sequence: Option<i64> = None;
    for mutation in mutations {
        connection
            .execute(
                "INSERT INTO pending_mutation (mutation_id, payload, digest) VALUES (?1, ?2, ?3)",
                params![
                    mutation.id.to_vec(),
                    mutation.payload.clone(),
                    mutation.digest.to_vec()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = connection
            .query("SELECT last_insert_rowid()", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        sequence = Some(
            rows.next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .ok_or(RuntimeError::RecoveryInvalid)?
                .get(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        );
    }
    faults.check(FaultPoint::AfterMutation)?;
    let sequence = sequence.ok_or(RuntimeError::RecoveryInvalid)?;
    let generation = current
        .generation()
        .to_u64_digits()
        .1
        .first()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(RuntimeError::RecoveryInvalid)?;
    connection
        .execute(
            "INSERT INTO checkpoint (generation, digest, mutation_sequence) VALUES (?1, ?2, ?3)",
            params![
                i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                next_digest.to_vec(),
                sequence
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    faults.check(FaultPoint::AfterCheckpoint)?;
    connection
        .execute(
            "UPDATE runtime_meta SET generation = ?1, generation_digest = ?2 WHERE singleton = 1",
            params![
                i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                next_digest.to_vec()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    faults.check(FaultPoint::AfterCapture)?;
    capture_tx(connection).await
}

fn fixed<const N: usize>(value: Vec<u8>) -> Result<[u8; N], RuntimeError> {
    value.try_into().map_err(|_| RuntimeError::RecoveryInvalid)
}
fn validate_id(value: [u8; 16]) -> Result<(), RuntimeError> {
    if value == [0; 16] {
        Err(RuntimeError::InvalidIdentity)
    } else {
        Ok(())
    }
}
fn validate_identity(value: RuntimeIdentity) -> Result<(), RuntimeError> {
    validate_id(value.database_id)?;
    validate_id(value.repository_id)
}
fn validate_request_identity(value: RequestIdentity) -> Result<(), RuntimeError> {
    validate_id(value.session_id)?;
    validate_id(value.request_id)
}
fn validate_digest(value: [u8; 32]) -> Result<(), RuntimeError> {
    if value == [0; 32] {
        Err(RuntimeError::InvalidDigest)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        collections::VecDeque,
        future::{Ready, ready},
        path::Path,
        process::Command,
    };
    use tempfile::TempDir;

    struct Fail(FaultPoint);
    impl FaultInjector for Fail {
        fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
            if point == self.0 {
                Err(RuntimeError::FaultInjected(point))
            } else {
                Ok(())
            }
        }
    }
    fn id(value: u8) -> [u8; 16] {
        [value; 16]
    }
    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }
    fn mutation(value: u8) -> Mutation {
        Mutation {
            id: id(value),
            payload: vec![value],
            digest: digest(value),
        }
    }
    fn request(session: u8, request: u8) -> RequestIdentity {
        RequestIdentity {
            session_id: id(session),
            request_id: id(request),
        }
    }
    fn outcome(value: u8) -> TerminalOutcome {
        TerminalOutcome::new(vec![value]).unwrap()
    }
    fn git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .unwrap()
                .success()
        );
    }
    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().unwrap();
        git(temp.path(), &["init"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "test"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }
    async fn open_state(repo: &Repository) -> RuntimeState {
        RuntimeState::open(
            repo,
            RuntimeIdentity {
                database_id: id(1),
                repository_id: id(2),
            },
            digest(3),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn activation_context_pins_capture_and_time_through_commit() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        let captured = context.capture().clone();
        let started = context.activation_time();

        let next = state
            .commit_activation(lease, &context, &[mutation(5)], digest(6), &NoFault)
            .await
            .unwrap();

        assert_eq!(context.capture(), &captured);
        assert_eq!(context.activation_time(), started);
        assert_ne!(next, captured);
        assert_eq!(state.capture().await.unwrap(), next);
    }

    fn stream_delivery(position: &str, successor: &str) -> DeliveryIdentity {
        DeliveryIdentity {
            consumer: ConsumerIdentity {
                principal: Component::new("principal").unwrap(),
                root: Component::new("root").unwrap(),
                function: Component::new("consume").unwrap(),
                binding: Component::new("binding").unwrap(),
            },
            source_format: Component::new("source-format").unwrap(),
            source: Component::new("source").unwrap(),
            partition_format: Component::new("partition-format").unwrap(),
            partition: Component::new("partition").unwrap(),
            position_format: Component::new("position-format").unwrap(),
            position: Position {
                token: Component::new(position).unwrap(),
            },
            successor: Position {
                token: Component::new(successor).unwrap(),
            },
        }
    }

    async fn protected_replay_fixture(
        state: &RuntimeState,
        writer: WriterLease,
        label: &str,
        payload_digest: [u8; 32],
    ) -> (ReplayGrant, StreamCheckpoint, DeliveryIdentity) {
        let delivery = stream_delivery(label, &format!("{label}-next"));
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let mut stream = state.stream_backend(writer);
        let lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: delivery.clone(),
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected delivery lease: {other:?}"),
        };
        let failure = match stream
            .fail_async(
                lease,
                SafeDiagnostic {
                    code: DiagnosticCode::ExecutionRejected,
                    class: DiagnosticClass::Permanent,
                },
                StreamFailurePayload::ProtectedReference {
                    reference: "opaque-ref".into(),
                    digest: payload_digest,
                },
            )
            .await
            .unwrap()
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("unexpected failure result: {other:?}"),
        };
        let skip_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: delivery.clone(),
                expected: expected.clone(),
                purpose: LeasePurpose::Skip,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected skip lease: {other:?}"),
        };
        let checkpoint = match stream
            .apply_async(CommitIntent::Skip {
                lease: skip_lease,
                expected,
                expected_failure_version: failure.version,
            })
            .await
            .unwrap()
        {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            other => panic!("unexpected skip result: {other:?}"),
        };
        let skipped = stream
            .failure_async(&failure.identity)
            .await
            .unwrap()
            .expect("skipped failure");
        let grant = match stream
            .apply_async(CommitIntent::Replay {
                failure: failure.identity,
                expected_version: skipped.version,
            })
            .await
            .unwrap()
        {
            CommitResult::ReplayGranted { grant } => grant,
            other => panic!("unexpected replay grant: {other:?}"),
        };
        (grant, checkpoint, delivery)
    }

    struct TestSource {
        key: CheckpointKey,
        item: Option<StreamItem>,
        polls: usize,
    }

    impl StreamSource for TestSource {
        type NextFuture<'a>
            = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
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

        fn checkpoint_key(&self) -> Option<CheckpointKey> {
            Some(self.key.clone())
        }

        fn failure_payload(&self, item: &StreamItem) -> StreamFailurePayload {
            StreamFailurePayload::Plaintext(item.payload.clone())
        }

        fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
            self.polls += 1;
            ready(Ok(self
                .item
                .take()
                .map_or(StreamSourcePoll::Exhausted, |item| {
                    StreamSourcePoll::Item(Box::new(item))
                })))
        }

        fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
            ready(Ok(()))
        }
    }

    struct TestHandler {
        result: Option<StreamHandlerResult>,
        calls: usize,
    }

    impl StreamHandler for TestHandler {
        fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
            self.calls += 1;
            self.result.take().unwrap_or(StreamHandlerResult::Cancelled)
        }
    }

    struct SequenceSource {
        key: CheckpointKey,
        descriptor: StreamSourceDescriptor,
        polls: usize,
        waits: usize,
        steps: VecDeque<StreamSourcePoll>,
    }

    impl StreamSource for SequenceSource {
        type NextFuture<'a>
            = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
        where
            Self: 'a;
        type WaitFuture<'a>
            = Ready<Result<(), SafeDiagnostic>>
        where
            Self: 'a;

        fn descriptor(&self) -> StreamSourceDescriptor {
            self.descriptor
        }

        fn checkpoint_key(&self) -> Option<CheckpointKey> {
            Some(self.key.clone())
        }

        fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
            self.polls += 1;
            ready(Ok(self
                .steps
                .pop_front()
                .unwrap_or(StreamSourcePoll::Exhausted)))
        }

        fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
            self.waits += 1;
            ready(Ok(()))
        }
    }

    struct LegacySource {
        item: Option<StreamItem>,
        polls: usize,
    }

    impl StreamSource for LegacySource {
        type NextFuture<'a>
            = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
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

        fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
            self.polls += 1;
            ready(Ok(self
                .item
                .take()
                .map_or(StreamSourcePoll::Exhausted, |item| {
                    StreamSourcePoll::Item(Box::new(item))
                })))
        }

        fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
            ready(Ok(()))
        }
    }

    struct FailingSource {
        key: CheckpointKey,
        diagnostic: Option<SafeDiagnostic>,
    }

    impl StreamSource for FailingSource {
        type NextFuture<'a>
            = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
        where
            Self: 'a;
        type WaitFuture<'a>
            = Ready<Result<(), SafeDiagnostic>>
        where
            Self: 'a;

        fn descriptor(&self) -> StreamSourceDescriptor {
            StreamSourceDescriptor {
                kind: StreamSourceKind::Unbounded,
                replayable: true,
            }
        }

        fn checkpoint_key(&self) -> Option<CheckpointKey> {
            Some(self.key.clone())
        }

        fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
            ready(
                self.diagnostic
                    .take()
                    .map_or(Ok(StreamSourcePoll::Exhausted), Err),
            )
        }

        fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
            ready(Ok(()))
        }
    }

    struct WaitingFailureSource {
        key: CheckpointKey,
        diagnostic: Option<SafeDiagnostic>,
    }

    impl StreamSource for WaitingFailureSource {
        type NextFuture<'a>
            = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
        where
            Self: 'a;
        type WaitFuture<'a>
            = Ready<Result<(), SafeDiagnostic>>
        where
            Self: 'a;

        fn descriptor(&self) -> StreamSourceDescriptor {
            StreamSourceDescriptor {
                kind: StreamSourceKind::Unbounded,
                replayable: true,
            }
        }

        fn checkpoint_key(&self) -> Option<CheckpointKey> {
            Some(self.key.clone())
        }

        fn next<'a>(&'a mut self, _: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
            ready(Ok(StreamSourcePoll::Waiting))
        }

        fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
            ready(self.diagnostic.take().map_or(Ok(()), Err))
        }
    }

    struct CommitHandler {
        calls: usize,
    }

    impl StreamHandler for CommitHandler {
        fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
            self.calls += 1;
            StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: Vec::new(),
                next_digest: digest(3),
            })
        }
    }

    struct ReplayHandler {
        payload: Vec<u8>,
        result: Option<StreamHandlerResult>,
    }

    impl StreamHandler for ReplayHandler {
        fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
            self.payload = item.payload.clone();
            self.result
                .take()
                .expect("replay handler result is configured")
        }
    }

    struct ReplayProvider {
        payload: Vec<u8>,
    }

    impl StreamFailurePayloadProvider for ReplayProvider {
        type Error = ();

        fn refetch<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>> {
            Box::pin(ready(Ok(self.payload.clone())))
        }
    }

    #[derive(Clone, Copy)]
    struct CancelImmediately;

    impl StreamRunControl for CancelImmediately {
        fn cancelled(&self) -> bool {
            true
        }

        fn acquire_admission(&self) -> bool {
            false
        }

        fn release_admission(&self) {}
    }

    struct CancelAfterPoll(Cell<usize>);

    impl StreamRunControl for CancelAfterPoll {
        fn cancelled(&self) -> bool {
            let calls = self.0.get();
            self.0.set(calls + 1);
            calls > 0
        }

        fn acquire_admission(&self) -> bool {
            !self.cancelled()
        }

        fn release_admission(&self) {}
    }

    #[derive(Clone, Copy)]
    struct CancelledBeforeProviderFailure;

    impl StreamRunControl for CancelledBeforeProviderFailure {
        fn cancelled(&self) -> bool {
            true
        }

        fn acquire_admission(&self) -> bool {
            false
        }

        fn release_admission(&self) {}
    }

    struct CancelAfterWait(Cell<usize>);

    impl StreamRunControl for CancelAfterWait {
        fn cancelled(&self) -> bool {
            let calls = self.0.get();
            self.0.set(calls + 1);
            calls > 0
        }

        fn acquire_admission(&self) -> bool {
            false
        }

        fn release_admission(&self) {}
    }

    #[test]
    fn stream_run_gate_linearizes_cancellation_with_admission() {
        let gate = StreamRunGate::new();
        assert!(!gate.cancelled());
        assert!(gate.acquire_admission());
        assert!(gate.cancel());
        assert!(gate.cancelled());
        gate.release_admission();
        assert!(!gate.acquire_admission());
        assert!(!gate.cancel());
    }

    #[tokio::test]
    async fn runtime_stream_runner_repeats_until_exhaustion_and_fences_cancellation() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let first = stream_delivery("runner:one", "runner:two");
        let second = stream_delivery("runner:two", "runner:three");
        let key = first.checkpoint_key();
        let mut mismatch_source = SequenceSource {
            key: CheckpointKey {
                source: Component::new("other-source").unwrap(),
                ..key.clone()
            },
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Finite,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::new(),
        };
        let mut mismatch_handler = CommitHandler { calls: 0 };
        assert_eq!(
            state
                .run_stream_once(writer, &key, &mut mismatch_source, &mut mismatch_handler,)
                .await,
            Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch
            ))
        );
        assert_eq!(mismatch_source.polls, 0);
        let mut long_mismatch_handler = CommitHandler { calls: 0 };
        assert_eq!(
            state
                .run_stream(
                    writer,
                    &key,
                    &mut mismatch_source,
                    &mut long_mismatch_handler,
                    &NeverCancelled,
                )
                .await,
            Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch
            ))
        );
        assert_eq!(mismatch_source.polls, 0);
        assert_eq!(mismatch_source.waits, 0);
        assert_eq!(long_mismatch_handler.calls, 0);

        let mut legacy_delivery = stream_delivery("legacy:one", "legacy:two");
        legacy_delivery.source = Component::new("legacy-source").unwrap();
        let legacy_key = legacy_delivery.checkpoint_key();
        let mut legacy_source = LegacySource {
            item: Some(StreamItem {
                delivery: legacy_delivery,
                payload: vec![8],
            }),
            polls: 0,
        };
        let mut legacy_handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream_once(writer, &legacy_key, &mut legacy_source, &mut legacy_handler,)
                .await
                .unwrap(),
            StreamStep::Committed { .. }
        ));
        assert_eq!(legacy_source.polls, 1);
        assert_eq!(legacy_handler.calls, 1);

        let mut source = SequenceSource {
            key: key.clone(),
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Finite,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::from([
                StreamSourcePoll::Waiting,
                StreamSourcePoll::Item(Box::new(StreamItem {
                    delivery: first,
                    payload: vec![1],
                })),
                StreamSourcePoll::Item(Box::new(StreamItem {
                    delivery: second,
                    payload: vec![2],
                })),
            ]),
        };
        let mut handler = CommitHandler { calls: 0 };
        let outcome = state
            .run_stream(writer, &key, &mut source, &mut handler, &NeverCancelled)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            StreamRunOutcome::Exhausted {
                delivered: 2,
                checkpoint: StreamCheckpoint {
                    version: 2,
                    committed: Some(Position { .. }),
                    ..
                }
            }
        ));
        assert_eq!(source.polls, 4);
        assert_eq!(source.waits, 1);
        assert_eq!(handler.calls, 2);

        let mut closed_source = SequenceSource {
            key: key.clone(),
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Unbounded,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::new(),
        };
        let mut closed_handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream(
                    writer,
                    &key,
                    &mut closed_source,
                    &mut closed_handler,
                    &NeverCancelled,
                )
                .await
                .unwrap(),
            StreamRunOutcome::Closed {
                delivered: 0,
                checkpoint: StreamCheckpoint { version: 2, .. }
            }
        ));

        let mut cancelled_source = SequenceSource {
            key: key.clone(),
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Finite,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::from([StreamSourcePoll::Item(Box::new(StreamItem {
                delivery: stream_delivery("runner:three", "runner:four"),
                payload: vec![3],
            }))]),
        };
        let mut cancelled_handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream(
                    writer,
                    &key,
                    &mut cancelled_source,
                    &mut cancelled_handler,
                    &CancelImmediately,
                )
                .await
                .unwrap(),
            StreamRunOutcome::Cancelled {
                delivered: 0,
                checkpoint: StreamCheckpoint { version: 2, .. }
            }
        ));
        assert_eq!(cancelled_source.polls, 0);
        assert_eq!(cancelled_handler.calls, 0);

        let mut raced_source = SequenceSource {
            key: key.clone(),
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Finite,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::from([StreamSourcePoll::Item(Box::new(StreamItem {
                delivery: stream_delivery("runner:three", "runner:four"),
                payload: vec![4],
            }))]),
        };
        let mut raced_handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream(
                    writer,
                    &key,
                    &mut raced_source,
                    &mut raced_handler,
                    &CancelAfterPoll(Cell::new(0)),
                )
                .await
                .unwrap(),
            StreamRunOutcome::Cancelled {
                delivered: 0,
                checkpoint: StreamCheckpoint { version: 2, .. }
            }
        ));
        assert_eq!(raced_source.polls, 1);
        assert_eq!(raced_handler.calls, 0);
    }

    #[tokio::test]
    async fn list_stream_source_reopens_from_the_durable_successor() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let key = stream_delivery("list:one", "list:two").checkpoint_key();
        let payloads = vec![vec![1], vec![2], vec![3]];
        let mut source = ListStreamSource::new(key.clone(), payloads.clone());
        let mut handler = CommitHandler { calls: 0 };
        let outcome = state
            .run_stream(writer, &key, &mut source, &mut handler, &NeverCancelled)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            StreamRunOutcome::Exhausted {
                delivered: 3,
                checkpoint: StreamCheckpoint {
                    version: 3,
                    committed: Some(Position { .. }),
                    ..
                }
            }
        ));
        assert_eq!(handler.calls, 3);
        assert!(source.descriptor().replayable);
        drop(state);

        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        let mut source = ListStreamSource::new(key.clone(), payloads);
        let mut handler = CommitHandler { calls: 0 };
        assert!(matches!(
            reopened
                .run_stream(writer, &key, &mut source, &mut handler, &NeverCancelled)
                .await
                .unwrap(),
            StreamRunOutcome::Exhausted {
                delivered: 0,
                checkpoint: StreamCheckpoint { version: 3, .. }
            }
        ));
        assert_eq!(handler.calls, 0);
    }

    #[tokio::test]
    async fn list_stream_source_rejects_noncanonical_and_future_positions() {
        let delivery = stream_delivery("list:one", "list:two");
        let key = delivery.checkpoint_key();
        let mut source = ListStreamSource::new(key.clone(), vec![vec![1], vec![2], vec![3]]);
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        for token in ["01", "+1", "4"] {
            let checkpoint = StreamCheckpoint {
                key: key.clone(),
                version: 1,
                committed: Some(Position {
                    token: Component::new(token).unwrap(),
                }),
            };
            assert!(matches!(
                source.next(&checkpoint).await,
                Err(actual) if actual == diagnostic
            ));
        }

        let wrong_key = CheckpointKey {
            source: Component::new("other-source").unwrap(),
            ..key.clone()
        };
        assert!(matches!(
            source
                .next(&StreamCheckpoint {
                    key: wrong_key,
                    version: 0,
                    committed: None,
                })
                .await,
            Err(actual) if actual == diagnostic
        ));
    }

    #[tokio::test]
    async fn provider_failures_are_durable_until_the_checkpoint_is_admitted() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("provider:one", "provider:two");
        let key = delivery.checkpoint_key();
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::ProviderUnavailable,
            class: DiagnosticClass::Transient,
        };

        for expected_attempts in 1..=2 {
            let mut source = FailingSource {
                key: key.clone(),
                diagnostic: Some(diagnostic),
            };
            let mut handler = TestHandler {
                result: None,
                calls: 0,
            };
            assert_eq!(
                state
                    .run_stream_once(writer, &key, &mut source, &mut handler)
                    .await,
                Err(StreamStepError::Provider(diagnostic))
            );
            let failure = state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(failure.attempts, expected_attempts);
            assert_eq!(failure.checkpoint.version, 0);
            assert_eq!(failure.diagnostic, diagnostic);
        }

        drop(state);
        let state = open_state(&repo).await;
        let mut source = TestSource {
            key: key.clone(),
            item: Some(StreamItem {
                delivery,
                payload: vec![1],
            }),
            polls: 0,
        };
        let mut handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await,
            Ok(StreamStep::Committed { .. })
        ));
        assert!(
            state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancellation_does_not_become_a_durable_provider_failure() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("cancel:one", "cancel:two");
        let key = delivery.checkpoint_key();
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::ProviderUnavailable,
            class: DiagnosticClass::Transient,
        };
        let mut source = FailingSource {
            key: key.clone(),
            diagnostic: Some(diagnostic),
        };
        let mut handler = TestHandler {
            result: None,
            calls: 0,
        };
        assert!(matches!(
            state
                .run_stream_once_controlled(
                    writer,
                    &key,
                    &mut source,
                    &mut handler,
                    &CancelledBeforeProviderFailure,
                )
                .await,
            Ok(StreamStep::Cancelled { .. })
        ));
        assert!(
            state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .is_none()
        );

        let cancellation = SafeDiagnostic {
            code: DiagnosticCode::Cancelled,
            class: DiagnosticClass::Cancellation,
        };
        let mut source = FailingSource {
            key: key.clone(),
            diagnostic: Some(cancellation),
        };
        let mut handler = TestHandler {
            result: None,
            calls: 0,
        };
        assert!(matches!(
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await,
            Ok(StreamStep::Cancelled { .. })
        ));
        assert!(
            state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .is_none()
        );

        let mut source = WaitingFailureSource {
            key: key.clone(),
            diagnostic: Some(cancellation),
        };
        let mut handler = TestHandler {
            result: None,
            calls: 0,
        };
        assert!(matches!(
            state
                .run_stream(writer, &key, &mut source, &mut handler, &NeverCancelled,)
                .await,
            Ok(StreamRunOutcome::Cancelled { delivered: 0, .. })
        ));
        assert!(
            state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .is_none()
        );

        let mut source = WaitingFailureSource {
            key: key.clone(),
            diagnostic: Some(diagnostic),
        };
        let mut handler = TestHandler {
            result: None,
            calls: 0,
        };
        let outcome = state
            .run_stream(
                writer,
                &key,
                &mut source,
                &mut handler,
                &CancelAfterWait(Cell::new(0)),
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            StreamRunOutcome::Cancelled {
                delivered: 0,
                checkpoint: StreamCheckpoint { version: 0, .. },
            }
        ));
        assert!(
            state
                .stream_backend(writer)
                .provider_failure_async(&key)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn recovery_rejects_provider_failure_with_oversized_attempts() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("corrupt:one", "corrupt:two");
        let key = delivery.checkpoint_key();
        let mut source = FailingSource {
            key: key.clone(),
            diagnostic: Some(SafeDiagnostic {
                code: DiagnosticCode::ProviderUnavailable,
                class: DiagnosticClass::Transient,
            }),
        };
        let mut handler = TestHandler {
            result: None,
            calls: 0,
        };
        assert!(matches!(
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await,
            Err(StreamStepError::Provider(_))
        ));
        state
            .connection
            .execute_batch(
                "PRAGMA ignore_check_constraints = ON;
                 UPDATE stream_provider_failure SET attempts = 4294967296;
                 PRAGMA ignore_check_constraints = OFF;",
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
        drop(state);
        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: id(1),
                    repository_id: id(2),
                },
                digest(3),
            )
            .await,
            Err(RuntimeError::RecoveryInvalid)
        ));
    }

    #[tokio::test]
    async fn durable_stream_backend_reopens_with_checkpoint_and_failure_state() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("one", "two");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let (failed, advanced) = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            };
            let failed = match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(Vec::new()),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected stream failure result: {other:?}"),
            };
            assert_eq!(failed.attempts, 1);
            let retry = match stream
                .apply_async(CommitIntent::Retry {
                    failure: failed.identity.clone(),
                    expected_version: failed.version,
                    expected: expected.clone(),
                })
                .await
                .unwrap()
            {
                CommitResult::RetryScheduled { failure } => failure,
                other => panic!("unexpected stream retry result: {other:?}"),
            };
            assert_eq!(retry.status, FailureStatus::Retrying);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream retry acquire result: {other:?}"),
            };
            let advanced = match stream
                .apply_async(CommitIntent::Complete { lease, expected })
                .await
                .unwrap()
            {
                CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
                other => panic!("unexpected stream completion result: {other:?}"),
            };
            assert_eq!(
                advanced.committed,
                Some(Position {
                    token: Component::new("two").unwrap(),
                })
            );
            (failed, advanced)
        };
        drop(state);

        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        let stream = reopened.stream_backend(writer);
        let checkpoint = stream.checkpoint_async(&advanced.key).await.unwrap();
        assert_eq!(checkpoint, advanced);
        let failure = stream
            .failure_async(&failed.identity)
            .await
            .unwrap()
            .expect("durable failure");
        assert_eq!(failure.status, FailureStatus::Succeeded);
        assert_eq!(failure.attempts, 1);
    }

    #[tokio::test]
    async fn durable_stream_control_reopens_paused_and_resets_opaque_position() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("receipt:zero", "resume:one");
        let key = delivery.checkpoint_key();
        {
            let mut stream = state.stream_backend(writer);
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Pause { key: key.clone() })
                    .await
                    .unwrap(),
                CommitResult::StreamStatusChanged {
                    state: StreamState {
                        status: StreamStatus::Paused,
                        ..
                    },
                    changed: true,
                }
            ));
        }
        drop(state);

        let reset_position = Position {
            token: Component::new("provider/opaque-rewind").unwrap(),
        };
        {
            let reopened = open_state(&repo).await;
            let writer = reopened.acquire_lease(id(4)).await.unwrap();
            let mut stream = reopened.stream_backend(writer);
            let before = stream.checkpoint_async(&key).await.unwrap();
            assert_eq!(
                stream
                    .apply_async(CommitIntent::Acquire {
                        delivery: delivery.clone(),
                        expected: CheckpointPrecondition::from(&before),
                        purpose: LeasePurpose::Deliver,
                    })
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::StreamPaused)
            );
            let reset = match stream
                .apply_async(CommitIntent::Reset {
                    key: key.clone(),
                    expected: CheckpointPrecondition::from(&before),
                    to: reset_position.clone(),
                })
                .await
                .unwrap()
            {
                CommitResult::CheckpointReset { checkpoint } => checkpoint,
                other => panic!("unexpected checkpoint reset result: {other:?}"),
            };
            assert_eq!(reset.committed, Some(reset_position.clone()));
            assert_eq!(reset.version, 1);
            assert_eq!(
                stream
                    .apply_async(CommitIntent::Reset {
                        key: key.clone(),
                        expected: CheckpointPrecondition::from(&before),
                        to: Position {
                            token: Component::new("stale").unwrap(),
                        },
                    })
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::StaleCheckpoint)
            );
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Resume { key: key.clone() })
                    .await
                    .unwrap(),
                CommitResult::StreamStatusChanged {
                    state: StreamState {
                        status: StreamStatus::Running,
                        ..
                    },
                    changed: true,
                }
            ));
        }

        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        let mut stream = reopened.stream_backend(writer);
        let current = stream.checkpoint_async(&key).await.unwrap();
        assert_eq!(current.committed, Some(reset_position));
        assert!(matches!(
            stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: CheckpointPrecondition::from(&current),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap(),
            CommitResult::Acquired { .. }
        ));
    }

    #[tokio::test]
    async fn runtime_stream_scheduler_commits_noop_and_keeps_failures_unadvanced() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();

        let committed_delivery = stream_delivery("scheduler:commit", "scheduler:next");
        let committed_key = committed_delivery.checkpoint_key();
        let mut source = TestSource {
            key: committed_key.clone(),
            item: Some(StreamItem {
                delivery: committed_delivery,
                payload: vec![1, 2, 3],
            }),
            polls: 0,
        };
        let mut handler = TestHandler {
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: Vec::new(),
                next_digest: digest(3),
            })),
            calls: 0,
        };
        let committed = state
            .run_stream_once(writer, &committed_key, &mut source, &mut handler)
            .await
            .unwrap();
        assert!(matches!(
            committed,
            StreamStep::Committed {
                checkpoint: StreamCheckpoint {
                    version: 1,
                    committed: Some(Position { .. }),
                    ..
                }
            }
        ));
        assert_eq!(source.polls, 1);
        assert_eq!(handler.calls, 1);
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            state
                .run_stream_once(writer, &committed_key, &mut source, &mut handler)
                .await
                .unwrap(),
            StreamStep::Exhausted
        );

        let failed_delivery = stream_delivery("scheduler:fail", "scheduler:after-fail");
        let failed_key = failed_delivery.checkpoint_key();
        let mut failing_source = TestSource {
            key: failed_key.clone(),
            item: Some(StreamItem {
                delivery: failed_delivery.clone(),
                payload: vec![9],
            }),
            polls: 0,
        };
        let mut failing_handler = TestHandler {
            result: Some(StreamHandlerResult::Fail(SafeDiagnostic {
                code: DiagnosticCode::ExecutionRejected,
                class: DiagnosticClass::Permanent,
            })),
            calls: 0,
        };
        let before_failure = state
            .stream_backend(writer)
            .checkpoint_async(&failed_key)
            .await
            .unwrap();
        let failed = state
            .run_stream_once(
                writer,
                &failed_key,
                &mut failing_source,
                &mut failing_handler,
            )
            .await
            .unwrap();
        assert!(matches!(failed, StreamStep::Failed { .. }));
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&failed_key)
                .await
                .unwrap(),
            before_failure
        );
        assert_eq!(failing_handler.calls, 1);

        let cancelled_delivery = stream_delivery("scheduler:cancel", "scheduler:after-cancel");
        let cancelled_key = cancelled_delivery.checkpoint_key();
        let mut cancelled_source = TestSource {
            key: cancelled_key.clone(),
            item: Some(StreamItem {
                delivery: cancelled_delivery,
                payload: vec![7],
            }),
            polls: 0,
        };
        let mut cancelled_handler = TestHandler {
            result: Some(StreamHandlerResult::Cancelled),
            calls: 0,
        };
        let before_cancel = state
            .stream_backend(writer)
            .checkpoint_async(&cancelled_key)
            .await
            .unwrap();
        assert!(matches!(
            state
                .run_stream_once(
                    writer,
                    &cancelled_key,
                    &mut cancelled_source,
                    &mut cancelled_handler,
                )
                .await
                .unwrap(),
            StreamStep::Cancelled { .. }
        ));
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&cancelled_key)
                .await
                .unwrap(),
            before_cancel
        );
        assert!(
            state
                .stream_backend(writer)
                .failure_async(&FailureIdentity(stream_delivery(
                    "scheduler:cancel",
                    "scheduler:after-cancel"
                )))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_delivery_payload_survives_runtime_reopen() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("payload:one", "payload:two");
        let key = delivery.checkpoint_key();
        let payload = vec![9, 8, 7, 6];
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        let mut source = TestSource {
            key: key.clone(),
            item: Some(StreamItem {
                delivery,
                payload: payload.clone(),
            }),
            polls: 0,
        };
        let mut handler = TestHandler {
            result: Some(StreamHandlerResult::Fail(diagnostic)),
            calls: 0,
        };
        let failure = match state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .unwrap()
        {
            StreamStep::Failed { failure } => failure,
            other => panic!("unexpected stream result: {other:?}"),
        };
        assert_eq!(
            state
                .stream_backend(writer)
                .failure_payload_metadata_async(&failure.identity)
                .await
                .unwrap(),
            Some(StreamFailurePayloadMetadata {
                plaintext_bytes: Some(payload.len() as u64),
                protected_reference: false,
                redacted: true,
            })
        );
        let mut rows = state
            .connection
            .query(
                "SELECT payload FROM stream_failure_payload WHERE identity_id = ?1",
                params![stream_identity_id(&failure.identity)],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("retained payload");
        let retained: Vec<u8> = row.get(0).unwrap();
        assert_eq!(retained, payload);
        drop(state);

        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        assert_eq!(
            reopened
                .stream_backend(writer)
                .failure_payload_metadata_async(&failure.identity)
                .await
                .unwrap(),
            Some(StreamFailurePayloadMetadata {
                plaintext_bytes: Some(payload.len() as u64),
                protected_reference: false,
                redacted: true,
            })
        );
        let mut rows = reopened
            .connection
            .query(
                "SELECT payload FROM stream_failure_payload WHERE identity_id = ?1",
                params![stream_identity_id(&failure.identity)],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("retained payload");
        let retained: Vec<u8> = row.get(0).unwrap();
        assert_eq!(retained, payload);
    }

    #[tokio::test]
    async fn list_source_resumes_from_durable_successor_after_reopen() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(9)).await.unwrap();
        let key = stream_delivery("0", "1").checkpoint_key();
        let mut source = ListStreamSource::new(key.clone(), vec![vec![10], vec![11]]);
        let mut first_handler = TestHandler {
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(7)],
                next_digest: digest(7),
            })),
            calls: 0,
        };

        let first = state
            .run_stream_once(writer, &key, &mut source, &mut first_handler)
            .await
            .unwrap();
        assert!(matches!(first, StreamStep::Committed { .. }));
        assert_eq!(state.pending().await.unwrap().len(), 1);
        drop(state);

        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(9)).await.unwrap();
        let mut source = ListStreamSource::new(key.clone(), vec![vec![10], vec![11]]);
        let mut second_handler = TestHandler {
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(8)],
                next_digest: digest(8),
            })),
            calls: 0,
        };
        assert!(matches!(
            reopened
                .run_stream_once(writer, &key, &mut source, &mut second_handler)
                .await
                .unwrap(),
            StreamStep::Committed { .. }
        ));
        assert_eq!(reopened.pending().await.unwrap().len(), 2);
        assert!(matches!(
            reopened
                .run_stream_once(writer, &key, &mut source, &mut second_handler)
                .await
                .unwrap(),
            StreamStep::Exhausted
        ));
    }

    #[tokio::test]
    async fn legacy_failure_rows_are_migrated_without_retained_payload() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("legacy-payload", "legacy-next");
        let failure = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: orna_stream_v1::CheckpointPrecondition {
                        version: 0,
                        committed: None,
                    },
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            };
            match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(vec![8, 8]),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected stream failure result: {other:?}"),
            }
        };
        state
            .connection
            .execute("DELETE FROM stream_failure_payload", ())
            .await
            .unwrap();
        state
            .connection
            .execute("DELETE FROM runtime_schema_migration", ())
            .await
            .unwrap();
        drop(state);

        let reopened = open_state(&repo).await;
        let stream = reopened.stream_backend(writer);
        assert!(
            stream
                .failure_async(&failure.identity)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            stream
                .failure_payload_metadata_async(&failure.identity)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn retry_keeps_the_first_retained_payload_for_a_stable_failure() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("retry-payload", "retry-next");
        let key = delivery.checkpoint_key();
        let mut source = TestSource {
            key: key.clone(),
            item: Some(StreamItem {
                delivery: delivery.clone(),
                payload: vec![1, 2],
            }),
            polls: 0,
        };
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        let mut handler = TestHandler {
            result: Some(StreamHandlerResult::Fail(diagnostic)),
            calls: 0,
        };
        let failure = match state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .unwrap()
        {
            StreamStep::Failed { failure } => failure,
            other => panic!("unexpected stream result: {other:?}"),
        };
        let checkpoint = state
            .stream_backend(writer)
            .checkpoint_async(&key)
            .await
            .unwrap();
        let retrying = match state
            .stream_backend(writer)
            .apply_async(CommitIntent::Retry {
                failure: failure.identity.clone(),
                expected_version: failure.version,
                expected: orna_stream_v1::CheckpointPrecondition {
                    version: checkpoint.version,
                    committed: checkpoint.committed.clone(),
                },
            })
            .await
            .unwrap()
        {
            CommitResult::RetryScheduled { failure, .. } => failure,
            other => panic!("unexpected retry result: {other:?}"),
        };
        let retry_lease = match state
            .stream_backend(writer)
            .apply_async(CommitIntent::Acquire {
                delivery: retrying.identity.0.clone(),
                expected: orna_stream_v1::CheckpointPrecondition {
                    version: checkpoint.version,
                    committed: checkpoint.committed.clone(),
                },
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected retry acquire result: {other:?}"),
        };
        let retried_failure = match state
            .stream_backend(writer)
            .fail_async(
                retry_lease,
                diagnostic,
                StreamFailurePayload::Plaintext(vec![9, 9, 9]),
            )
            .await
            .unwrap()
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("unexpected retry failure result: {other:?}"),
        };
        assert_eq!(retried_failure.attempts, 2);
        let mut rows = state
            .connection
            .query(
                "SELECT payload FROM stream_failure_payload WHERE identity_id = ?1",
                params![stream_identity_id(&retried_failure.identity)],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("retained payload");
        let retained: Vec<u8> = row.get(0).unwrap();
        assert_eq!(retained, vec![1, 2]);
    }

    #[tokio::test]
    async fn protected_failure_payload_is_redacted_and_raw_fail_intents_are_rejected() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("protected:one", "protected:two");
        let key = delivery.checkpoint_key();
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let lease = {
            let mut stream = state.stream_backend(writer);
            match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected,
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            }
        };
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        let mut stream = state.stream_backend(writer);
        assert_eq!(
            stream
                .apply_async(CommitIntent::Fail {
                    lease: lease.clone(),
                    diagnostic,
                })
                .await,
            Err(RuntimeError::RecoveryInvalid)
        );
        let failure = match stream
            .fail_async(
                lease,
                diagnostic,
                StreamFailurePayload::ProtectedReference {
                    reference: "protected-reference".to_owned(),
                    digest: [7; 32],
                },
            )
            .await
            .unwrap()
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("unexpected stream failure result: {other:?}"),
        };
        assert_eq!(
            stream
                .failure_payload_metadata_async(&failure.identity)
                .await
                .unwrap(),
            Some(StreamFailurePayloadMetadata {
                plaintext_bytes: None,
                protected_reference: true,
                redacted: true,
            })
        );
        assert!(
            stream
                .failure_async(&failure.identity)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(stream.checkpoint_async(&key).await.unwrap().committed, None);
    }

    #[tokio::test]
    async fn legacy_source_without_payload_policy_fails_closed_and_releases_lease() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("legacy:one", "legacy:two");
        let key = delivery.checkpoint_key();
        let mut source = LegacySource {
            item: Some(StreamItem {
                delivery: delivery.clone(),
                payload: vec![1, 2, 3],
            }),
            polls: 0,
        };
        let mut handler = TestHandler {
            result: Some(StreamHandlerResult::Fail(SafeDiagnostic {
                code: DiagnosticCode::DecodeRejected,
                class: DiagnosticClass::Permanent,
            })),
            calls: 0,
        };
        assert_eq!(
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await,
            Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid))
        );
        let mut stream = state.stream_backend(writer);
        assert!(
            stream
                .failure_async(&FailureIdentity(delivery.clone()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: orna_stream_v1::CheckpointPrecondition {
                        version: 0,
                        committed: None,
                    },
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap(),
            CommitResult::Acquired { .. }
        ));
    }

    #[tokio::test]
    async fn invalid_failure_payload_is_owner_fenced_before_validation() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let old = state.acquire_lease(id(4)).await.unwrap();
        let mut old_backend = state.stream_backend(old);
        let delivery = stream_delivery("invalid-owner", "invalid-next");
        let lease = match old_backend
            .apply_async(CommitIntent::Acquire {
                delivery,
                expected: orna_stream_v1::CheckpointPrecondition {
                    version: 0,
                    committed: None,
                },
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected stream acquire result: {other:?}"),
        };
        let replacement = state.recover_abandoned(id(4), id(5)).await.unwrap();
        assert_eq!(
            old_backend
                .fail_async(
                    lease.clone(),
                    SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::ProtectedReference {
                        reference: String::new(),
                        digest: [3; 32],
                    },
                )
                .await,
            Err(RuntimeError::OwnerLost)
        );
        let mut replacement_backend = state.stream_backend(replacement);
        assert!(matches!(
            replacement_backend
                .apply_async(CommitIntent::Acquire {
                    delivery: lease.delivery,
                    expected: orna_stream_v1::CheckpointPrecondition {
                        version: 0,
                        committed: None,
                    },
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap(),
            CommitResult::Acquired { .. }
        ));
    }

    #[tokio::test]
    async fn recovery_reopens_an_interrupted_retry_after_releasing_its_lease() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("retry-recovery", "retry-recovery-next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        let retrying = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            };
            let failed = match stream
                .fail_async(
                    lease,
                    diagnostic,
                    StreamFailurePayload::Plaintext(vec![1, 2, 3]),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected stream failure result: {other:?}"),
            };
            match stream
                .apply_async(CommitIntent::Retry {
                    failure: failed.identity,
                    expected_version: failed.version,
                    expected: expected.clone(),
                })
                .await
                .unwrap()
            {
                CommitResult::RetryScheduled { failure, .. } => failure,
                other => panic!("unexpected stream retry result: {other:?}"),
            }
        };
        assert_eq!(retrying.status, FailureStatus::Retrying);

        let replacement = state.recover_abandoned(id(4), id(5)).await.unwrap();
        let mut stream = state.stream_backend(replacement);
        let recovered = stream
            .failure_async(&retrying.identity)
            .await
            .unwrap()
            .expect("recovered failure");
        assert_eq!(recovered.status, FailureStatus::Failed);
        assert_eq!(recovered.version, retrying.version + 1);
        assert!(matches!(
            stream
                .apply_async(CommitIntent::Retry {
                    failure: recovered.identity,
                    expected_version: recovered.version,
                    expected,
                })
                .await
                .unwrap(),
            CommitResult::RetryScheduled { .. }
        ));
    }

    #[tokio::test]
    async fn recovery_rejects_missing_orphaned_and_malformed_failure_payloads() {
        {
            let (_temp, repo) = repository();
            let state = open_state(&repo).await;
            let writer = state.acquire_lease(id(4)).await.unwrap();
            let delivery = stream_delivery("missing-payload", "missing-next");
            let failure = {
                let mut stream = state.stream_backend(writer);
                let lease = match stream
                    .apply_async(CommitIntent::Acquire {
                        delivery: delivery.clone(),
                        expected: orna_stream_v1::CheckpointPrecondition {
                            version: 0,
                            committed: None,
                        },
                        purpose: LeasePurpose::Deliver,
                    })
                    .await
                    .unwrap()
                {
                    CommitResult::Acquired { lease } => lease,
                    other => panic!("unexpected stream acquire result: {other:?}"),
                };
                match stream
                    .fail_async(
                        lease,
                        SafeDiagnostic {
                            code: DiagnosticCode::DecodeRejected,
                            class: DiagnosticClass::Permanent,
                        },
                        StreamFailurePayload::Plaintext(vec![4]),
                    )
                    .await
                    .unwrap()
                {
                    CommitResult::Failed { failure } => failure,
                    other => panic!("unexpected stream failure result: {other:?}"),
                }
            };
            state
                .connection
                .execute(
                    "DELETE FROM stream_failure_payload WHERE identity_id = ?1",
                    params![stream_identity_id(&failure.identity)],
                )
                .await
                .unwrap();
            drop(state);
            assert!(matches!(
                RuntimeState::open(
                    &repo,
                    RuntimeIdentity {
                        database_id: id(1),
                        repository_id: id(2),
                    },
                    digest(3),
                )
                .await,
                Err(RuntimeError::RecoveryInvalid)
            ));
        }

        {
            let (_temp, repo) = repository();
            let state = open_state(&repo).await;
            state
                .connection
                .execute(
                    "INSERT INTO stream_failure_payload
                     (identity_id, payload, payload_reference, payload_digest, retention)
                     VALUES (?1, ?2, NULL, NULL, 1)",
                    params!["orphan", vec![8_u8]],
                )
                .await
                .unwrap();
            drop(state);
            assert!(matches!(
                RuntimeState::open(
                    &repo,
                    RuntimeIdentity {
                        database_id: id(1),
                        repository_id: id(2),
                    },
                    digest(3),
                )
                .await,
                Err(RuntimeError::RecoveryInvalid)
            ));
        }

        {
            let (_temp, repo) = repository();
            let state = open_state(&repo).await;
            let writer = state.acquire_lease(id(4)).await.unwrap();
            let delivery = stream_delivery("malformed-payload", "malformed-next");
            let failure = {
                let mut stream = state.stream_backend(writer);
                let lease = match stream
                    .apply_async(CommitIntent::Acquire {
                        delivery,
                        expected: orna_stream_v1::CheckpointPrecondition {
                            version: 0,
                            committed: None,
                        },
                        purpose: LeasePurpose::Deliver,
                    })
                    .await
                    .unwrap()
                {
                    CommitResult::Acquired { lease } => lease,
                    other => panic!("unexpected stream acquire result: {other:?}"),
                };
                match stream
                    .fail_async(
                        lease,
                        SafeDiagnostic {
                            code: DiagnosticCode::DecodeRejected,
                            class: DiagnosticClass::Permanent,
                        },
                        StreamFailurePayload::Plaintext(vec![9]),
                    )
                    .await
                    .unwrap()
                {
                    CommitResult::Failed { failure } => failure,
                    other => panic!("unexpected stream failure result: {other:?}"),
                }
            };
            state
                .connection
                .execute("PRAGMA ignore_check_constraints = ON", ())
                .await
                .unwrap();
            state
                .connection
                .execute(
                    "UPDATE stream_failure_payload
                     SET payload_digest = ?2 WHERE identity_id = ?1",
                    params![stream_identity_id(&failure.identity), vec![0_u8; 32]],
                )
                .await
                .unwrap();
            state
                .connection
                .execute("PRAGMA ignore_check_constraints = OFF", ())
                .await
                .unwrap();
            drop(state);
            assert!(matches!(
                RuntimeState::open(
                    &repo,
                    RuntimeIdentity {
                        database_id: id(1),
                        repository_id: id(2),
                    },
                    digest(3),
                )
                .await,
                Err(RuntimeError::RecoveryInvalid)
            ));
        }
    }

    #[tokio::test]
    async fn owner_fenced_stream_backend_rejects_stale_writer_without_mutation() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let old = state.acquire_lease(id(4)).await.unwrap();
        let mut old_backend = state.stream_backend(old);
        let replacement = state.recover_abandoned(id(4), id(5)).await.unwrap();
        let delivery = stream_delivery("stale-owner", "next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };

        assert_eq!(
            old_backend
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected,
                    purpose: LeasePurpose::Deliver,
                })
                .await,
            Err(RuntimeError::OwnerLost)
        );
        assert_eq!(
            old_backend
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap()
                .version,
            0
        );

        let mut replacement_backend = state.stream_backend(replacement);
        assert!(matches!(
            replacement_backend
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: orna_stream_v1::CheckpointPrecondition {
                        version: 0,
                        committed: None,
                    },
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap(),
            CommitResult::Acquired { .. }
        ));
    }

    #[tokio::test]
    async fn durable_stream_backend_fences_stale_leases_and_reopens_active_lease() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("lease-one", "lease-two");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let second = {
            let mut stream = state.stream_backend(writer);
            let first = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            };
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Cancel {
                        lease: first.clone(),
                    })
                    .await
                    .unwrap(),
                CommitResult::Cancelled {
                    classification: CancellationClassification::RollbackShaped,
                    checkpoint: StreamCheckpoint {
                        version: 0,
                        committed: None,
                        ..
                    },
                }
            ));
            assert!(
                stream
                    .failure_async(&FailureIdentity(delivery.clone()))
                    .await
                    .unwrap()
                    .is_none()
            );

            let second = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected replacement acquire result: {other:?}"),
            };
            assert!(second.fence > first.fence);
            assert_eq!(
                stream
                    .fail_async(
                        first,
                        SafeDiagnostic {
                            code: DiagnosticCode::ExecutionRejected,
                            class: DiagnosticClass::Permanent,
                        },
                        StreamFailurePayload::Plaintext(Vec::new()),
                    )
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::LeaseFenced)
            );
            second
        };
        drop(state);
        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        let mut stream = reopened.stream_backend(writer);
        let advanced = match stream
            .apply_async(CommitIntent::Complete {
                lease: second,
                expected,
            })
            .await
            .unwrap()
        {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            other => panic!("unexpected reopened completion result: {other:?}"),
        };
        assert_eq!(
            advanced.committed,
            Some(Position {
                token: Component::new("lease-two").unwrap(),
            })
        );
    }

    #[tokio::test]
    async fn durable_stream_backend_persists_skip_replay_resolution_and_stale_cas() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("failed", "after-failed");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let resolved = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            };
            let failed = match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ProviderUnavailable,
                        class: DiagnosticClass::Transient,
                    },
                    StreamFailurePayload::Plaintext(Vec::new()),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected stream failure result: {other:?}"),
            };
            let skip_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Skip,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected skip acquire result: {other:?}"),
            };
            let skipped = match stream
                .apply_async(CommitIntent::Skip {
                    lease: skip_lease,
                    expected: expected.clone(),
                    expected_failure_version: failed.version,
                })
                .await
                .unwrap()
            {
                CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
                other => panic!("unexpected skip result: {other:?}"),
            };
            let skipped_failure = stream
                .failure_async(&failed.identity)
                .await
                .unwrap()
                .expect("skipped failure");
            assert_eq!(skipped_failure.status, FailureStatus::Skipped);

            let replay = match stream
                .apply_async(CommitIntent::Replay {
                    failure: failed.identity.clone(),
                    expected_version: skipped_failure.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayGranted { grant } => grant,
                other => panic!("unexpected replay result: {other:?}"),
            };
            assert_eq!(
                stream.checkpoint_async(&skipped.key).await.unwrap(),
                skipped
            );
            let replay_failed = match stream
                .apply_async(CommitIntent::ReplayFail {
                    failure: replay.failure.clone(),
                    expected_version: replay.version,
                    diagnostic: SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    },
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayFailed { failure } => failure,
                other => panic!("unexpected replay failure result: {other:?}"),
            };
            assert_eq!(replay_failed.attempts, 2);
            let replay = match stream
                .apply_async(CommitIntent::Replay {
                    failure: replay_failed.identity.clone(),
                    expected_version: replay_failed.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayGranted { grant } => grant,
                other => panic!("unexpected second replay result: {other:?}"),
            };
            let replayed = match stream
                .apply_async(CommitIntent::ReplayComplete {
                    failure: replay.failure,
                    expected_version: replay.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayCompleted { failure } => failure,
                other => panic!("unexpected replay completion result: {other:?}"),
            };
            let resolved = match stream
                .apply_async(CommitIntent::Resolve {
                    failure: replayed.identity.clone(),
                    expected_version: replayed.version,
                })
                .await
                .unwrap()
            {
                CommitResult::Resolved { failure } => failure,
                other => panic!("unexpected resolve result: {other:?}"),
            };
            assert_eq!(resolved.status, FailureStatus::Resolved);

            let stale_delivery = stream_delivery("stale", "stale-next");
            let stale_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: stale_delivery.clone(),
                    expected: (&skipped).into(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stale acquire result: {other:?}"),
            };
            let stale_failure = match stream
                .fail_async(
                    stale_lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(Vec::new()),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected stale failure result: {other:?}"),
            };
            let current = stream
                .checkpoint_async(&stale_delivery.checkpoint_key())
                .await
                .unwrap();
            let later = stream_delivery("later", "later-next");
            let later_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: later.clone(),
                    expected: orna_stream_v1::CheckpointPrecondition::from(&current),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected later acquire result: {other:?}"),
            };
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Complete {
                        lease: later_lease,
                        expected: orna_stream_v1::CheckpointPrecondition::from(&current),
                    })
                    .await
                    .unwrap(),
                CommitResult::CheckpointAdvanced { .. }
            ));
            assert_eq!(
                stream
                    .apply_async(CommitIntent::Retry {
                        failure: stale_failure.identity.clone(),
                        expected_version: stale_failure.version,
                        expected: orna_stream_v1::CheckpointPrecondition::from(&current),
                    })
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::StaleCheckpoint)
            );
            assert_eq!(
                stream
                    .failure_async(&stale_failure.identity)
                    .await
                    .unwrap()
                    .expect("stale failure")
                    .status,
                FailureStatus::Failed
            );
            resolved
        };
        drop(state);
        let reopened = open_state(&repo).await;
        let writer = reopened.acquire_lease(id(4)).await.unwrap();
        let stream = reopened.stream_backend(writer);
        assert_eq!(
            stream
                .failure_async(&resolved.identity)
                .await
                .unwrap()
                .expect("resolved failure")
                .status,
            FailureStatus::Resolved
        );
    }

    #[tokio::test]
    async fn replay_executes_retained_payload_without_advancing_live_checkpoint() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("replay", "replay-next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let (grant, checkpoint) = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected delivery lease: {other:?}"),
            };
            let failure = match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(vec![7, 8]),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected failure result: {other:?}"),
            };
            let skip_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Skip,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected skip lease: {other:?}"),
            };
            let checkpoint = match stream
                .apply_async(CommitIntent::Skip {
                    lease: skip_lease,
                    expected,
                    expected_failure_version: failure.version,
                })
                .await
                .unwrap()
            {
                CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
                other => panic!("unexpected skip result: {other:?}"),
            };
            let skipped = stream
                .failure_async(&failure.identity)
                .await
                .unwrap()
                .expect("skipped failure");
            let grant = match stream
                .apply_async(CommitIntent::Replay {
                    failure: failure.identity,
                    expected_version: skipped.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayGranted { grant } => grant,
                other => panic!("unexpected replay grant: {other:?}"),
            };
            (grant, checkpoint)
        };
        let mut handler = ReplayHandler {
            payload: Vec::new(),
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(9)],
                next_digest: digest(10),
            })),
        };
        let result = state
            .stream_backend(writer)
            .replay_async(grant, &mut handler)
            .await
            .unwrap();
        let replayed = match result {
            CommitResult::ReplayCompleted { failure } => failure,
            other => panic!("unexpected replay result: {other:?}"),
        };
        assert_eq!(replayed.status, FailureStatus::Replayed);
        assert_eq!(handler.payload, vec![7, 8]);
        assert_eq!(state.pending().await.unwrap(), vec![mutation(9)]);
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn replay_handler_failure_returns_to_skipped_without_checkpoint_movement() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("replay-fail", "replay-fail-next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let (grant, checkpoint) = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected delivery lease: {other:?}"),
            };
            let failure = match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(vec![4, 5]),
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected failure result: {other:?}"),
            };
            let skip_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Skip,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected skip lease: {other:?}"),
            };
            let checkpoint = match stream
                .apply_async(CommitIntent::Skip {
                    lease: skip_lease,
                    expected,
                    expected_failure_version: failure.version,
                })
                .await
                .unwrap()
            {
                CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
                other => panic!("unexpected skip result: {other:?}"),
            };
            let skipped = stream
                .failure_async(&failure.identity)
                .await
                .unwrap()
                .expect("skipped failure");
            let grant = match stream
                .apply_async(CommitIntent::Replay {
                    failure: failure.identity,
                    expected_version: skipped.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayGranted { grant } => grant,
                other => panic!("unexpected replay grant: {other:?}"),
            };
            (grant, checkpoint)
        };
        let diagnostic = SafeDiagnostic {
            code: DiagnosticCode::DecodeRejected,
            class: DiagnosticClass::Permanent,
        };
        let mut handler = ReplayHandler {
            payload: Vec::new(),
            result: Some(StreamHandlerResult::Fail(diagnostic)),
        };
        let result = state
            .stream_backend(writer)
            .replay_async(grant, &mut handler)
            .await
            .unwrap();
        let failed = match result {
            CommitResult::ReplayFailed { failure } => failure,
            other => panic!("unexpected replay failure result: {other:?}"),
        };
        assert_eq!(failed.status, FailureStatus::Skipped);
        assert_eq!(failed.attempts, 2);
        assert_eq!(failed.diagnostic, diagnostic);
        assert_eq!(handler.payload, vec![4, 5]);
        assert_eq!(state.pending().await.unwrap(), Vec::<Mutation>::new());
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn protected_reference_replay_fails_closed_without_invoking_handler() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("replay-ref", "replay-ref-next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let (grant, checkpoint) = {
            let mut stream = state.stream_backend(writer);
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected delivery lease: {other:?}"),
            };
            let failure = match stream
                .fail_async(
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::ProtectedReference {
                        reference: "opaque-ref".into(),
                        digest: digest(8),
                    },
                )
                .await
                .unwrap()
            {
                CommitResult::Failed { failure } => failure,
                other => panic!("unexpected failure result: {other:?}"),
            };
            let skip_lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Skip,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected skip lease: {other:?}"),
            };
            let checkpoint = match stream
                .apply_async(CommitIntent::Skip {
                    lease: skip_lease,
                    expected,
                    expected_failure_version: failure.version,
                })
                .await
                .unwrap()
            {
                CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
                other => panic!("unexpected skip result: {other:?}"),
            };
            let skipped = stream
                .failure_async(&failure.identity)
                .await
                .unwrap()
                .expect("skipped failure");
            let grant = match stream
                .apply_async(CommitIntent::Replay {
                    failure: failure.identity,
                    expected_version: skipped.version,
                })
                .await
                .unwrap()
            {
                CommitResult::ReplayGranted { grant } => grant,
                other => panic!("unexpected replay grant: {other:?}"),
            };
            (grant, checkpoint)
        };
        let mut handler = ReplayHandler {
            payload: Vec::new(),
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(11)],
                next_digest: digest(12),
            })),
        };
        let result = state
            .stream_backend(writer)
            .replay_async(grant, &mut handler)
            .await
            .unwrap();
        let failed = match result {
            CommitResult::ReplayFailed { failure } => failure,
            other => panic!("unexpected protected replay result: {other:?}"),
        };
        assert_eq!(failed.status, FailureStatus::Skipped);
        assert_eq!(failed.attempts, 2);
        assert_eq!(
            failed.diagnostic,
            SafeDiagnostic {
                code: DiagnosticCode::ProviderUnavailable,
                class: DiagnosticClass::Transient,
            }
        );
        assert!(handler.payload.is_empty());
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn protected_reference_provider_replay_verifies_and_executes_payload() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let payload = b"refetched-payload".to_vec();
        let payload_digest: [u8; 32] = Sha256::digest(&payload).into();
        let (grant, checkpoint, delivery) =
            protected_replay_fixture(&state, writer, "replay-provider", payload_digest).await;
        let provider = ReplayProvider {
            payload: payload.clone(),
        };
        let mut handler = ReplayHandler {
            payload: Vec::new(),
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(13)],
                next_digest: digest(14),
            })),
        };
        let result = state
            .stream_backend(writer)
            .replay_async_with_provider(grant, &provider, &mut handler)
            .await
            .unwrap();
        let replayed = match result {
            CommitResult::ReplayCompleted { failure } => failure,
            other => panic!("unexpected provider replay result: {other:?}"),
        };
        assert_eq!(replayed.status, FailureStatus::Replayed);
        assert_eq!(handler.payload, payload);
        assert_eq!(state.pending().await.unwrap(), vec![mutation(13)]);
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn protected_reference_provider_digest_mismatch_fails_closed() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let expected_payload = b"expected-payload".to_vec();
        let payload_digest: [u8; 32] = Sha256::digest(&expected_payload).into();
        let (grant, checkpoint, delivery) =
            protected_replay_fixture(&state, writer, "replay-tampered", payload_digest).await;
        let mut handler = ReplayHandler {
            payload: Vec::new(),
            result: Some(StreamHandlerResult::Commit(StreamMutationBatch {
                mutations: vec![mutation(15)],
                next_digest: digest(16),
            })),
        };
        let result = state
            .stream_backend(writer)
            .replay_async_with_provider(
                grant,
                &ReplayProvider {
                    payload: b"tampered-payload".to_vec(),
                },
                &mut handler,
            )
            .await
            .unwrap();
        let failed = match result {
            CommitResult::ReplayFailed { failure } => failure,
            other => panic!("unexpected tampered replay result: {other:?}"),
        };
        assert_eq!(failed.status, FailureStatus::Skipped);
        assert_eq!(failed.attempts, 2);
        assert_eq!(
            failed.diagnostic,
            SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            }
        );
        assert!(handler.payload.is_empty());
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn atomic_commit_has_no_partial_visibility_under_each_injected_fault() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        for point in [
            FaultPoint::AfterMutation,
            FaultPoint::AfterCheckpoint,
            FaultPoint::AfterCapture,
        ] {
            assert_eq!(
                state
                    .commit(
                        lease,
                        &capture,
                        &mutation(point as u8 + 5),
                        digest(9),
                        &Fail(point)
                    )
                    .await,
                Err(RuntimeError::FaultInjected(point))
            );
            assert!(state.pending().await.unwrap().is_empty());
            assert_eq!(state.latest_checkpoint().await.unwrap(), None);
            assert_eq!(state.capture().await.unwrap(), capture);
        }
    }

    #[tokio::test]
    async fn batch_commit_publishes_all_mutations_under_one_checkpoint() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let batch = vec![mutation(5), mutation(6)];
        let next = state
            .commit_batch(lease, &capture, &batch, digest(9), &NoFault)
            .await
            .unwrap();

        assert_eq!(state.pending().await.unwrap(), batch);
        assert_eq!(
            state.latest_checkpoint().await.unwrap(),
            Some(Checkpoint {
                generation: 1,
                digest: digest(9),
                mutation_sequence: 2,
            })
        );
        assert_eq!(next.generation_digest(), digest(9));
        assert_eq!(
            state
                .commit_batch(lease, &next, &[], digest(10), &NoFault)
                .await,
            Err(RuntimeError::EmptyMutationBatch)
        );
    }

    #[tokio::test]
    async fn stream_delivery_commit_publishes_mutations_and_checkpoint_atomically() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let delivery = stream_delivery("one", "two");
        let stream_expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let delivery_lease = {
            let mut stream = state.stream_backend(writer);
            match stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: stream_expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            }
        };

        let (next, result) = state
            .commit_stream_delivery(StreamDeliveryCommit {
                writer,
                expected_capture: &capture,
                mutations: &[mutation(5), mutation(6)],
                next_digest: digest(9),
                delivery: delivery_lease,
                expected_stream: stream_expected,
                faults: &NoFault,
            })
            .await
            .unwrap();
        let CommitResult::CheckpointAdvanced { checkpoint } = result else {
            panic!("stream completion must advance its checkpoint");
        };
        assert_eq!(checkpoint.version, 1);
        assert_eq!(
            checkpoint
                .committed
                .as_ref()
                .map(|position| position.token.as_str()),
            Some("two")
        );
        assert_eq!(
            state.pending().await.unwrap(),
            vec![mutation(5), mutation(6)]
        );
        assert_eq!(
            state.latest_checkpoint().await.unwrap(),
            Some(Checkpoint {
                generation: 1,
                digest: digest(9),
                mutation_sequence: 2,
            })
        );
        assert_eq!(next.generation_digest(), digest(9));
    }

    #[tokio::test]
    async fn stream_delivery_commit_rolls_back_both_sides_on_fault_or_stale_cas() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let delivery = stream_delivery("one", "two");
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let delivery_lease = {
            let mut stream = state.stream_backend(writer);
            match stream
                .apply_async(CommitIntent::Acquire {
                    delivery,
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected stream acquire result: {other:?}"),
            }
        };

        assert_eq!(
            state
                .commit_stream_delivery(StreamDeliveryCommit {
                    writer,
                    expected_capture: &capture,
                    mutations: &[mutation(7)],
                    next_digest: digest(8),
                    delivery: delivery_lease.clone(),
                    expected_stream: expected.clone(),
                    faults: &Fail(FaultPoint::AfterCheckpoint),
                },)
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterCheckpoint))
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);
        assert_eq!(state.capture().await.unwrap(), capture);
        let checkpoint = {
            let stream = state.stream_backend(writer);
            stream
                .checkpoint_async(&delivery_lease.delivery.checkpoint_key())
                .await
                .unwrap()
        };
        assert_eq!(checkpoint.version, 0);
        assert_eq!(checkpoint.committed, None);

        let stale = CheckpointPrecondition {
            version: 1,
            committed: None,
        };
        let (unchanged, result) = state
            .commit_stream_delivery(StreamDeliveryCommit {
                writer,
                expected_capture: &capture,
                mutations: &[mutation(8)],
                next_digest: digest(9),
                delivery: delivery_lease,
                expected_stream: stale,
                faults: &NoFault,
            })
            .await
            .unwrap();
        assert_eq!(
            result,
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
        assert_eq!(unchanged, capture);
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);
    }

    #[tokio::test]
    async fn batch_faults_roll_back_every_mutation_and_checkpoint() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let batch = vec![mutation(5), mutation(6)];
        for point in [
            FaultPoint::AfterMutation,
            FaultPoint::AfterCheckpoint,
            FaultPoint::AfterCapture,
        ] {
            assert_eq!(
                state
                    .commit_batch(lease, &capture, &batch, digest(9), &Fail(point))
                    .await,
                Err(RuntimeError::FaultInjected(point))
            );
            assert!(state.pending().await.unwrap().is_empty());
            assert_eq!(state.latest_checkpoint().await.unwrap(), None);
            assert_eq!(state.capture().await.unwrap(), capture);
        }
    }

    #[tokio::test]
    async fn failed_delivery_metadata_rolls_back_at_each_metadata_fault_boundary() {
        for (value, point) in [
            (5, FaultPoint::AfterFailureRecord),
            (6, FaultPoint::AfterFailurePayload),
        ] {
            let (_temp, repo) = repository();
            let state = open_state(&repo).await;
            let writer = state.acquire_lease(id(4)).await.unwrap();
            let delivery = stream_delivery(&format!("failure:{value}"), "failure:next");
            let key = delivery.checkpoint_key();
            let expected = CheckpointPrecondition {
                version: 0,
                committed: None,
            };
            let lease = match state
                .stream_backend(writer)
                .apply_async(CommitIntent::Acquire {
                    delivery: delivery.clone(),
                    expected: expected.clone(),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap()
            {
                CommitResult::Acquired { lease } => lease,
                other => panic!("unexpected delivery lease: {other:?}"),
            };
            let before = state
                .stream_backend(writer)
                .checkpoint_async(&key)
                .await
                .unwrap();
            let before_capture = state.capture().await.unwrap();
            let result = state
                .fail_stream_delivery_with_faults(
                    writer,
                    lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    StreamFailurePayload::Plaintext(vec![value]),
                    &Fail(point),
                )
                .await;

            assert_eq!(result, Err(RuntimeError::FaultInjected(point)));
            assert_eq!(state.pending().await.unwrap(), Vec::<Mutation>::new());
            assert_eq!(state.capture().await.unwrap(), before_capture);
            assert_eq!(
                state
                    .stream_backend(writer)
                    .checkpoint_async(&key)
                    .await
                    .unwrap(),
                before
            );
            let stream = state.stream_backend(writer);
            assert!(
                stream
                    .failure_async(&FailureIdentity(delivery.clone()))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                stream
                    .failure_payload_metadata_async(&FailureIdentity(delivery.clone()))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                state
                    .stream_backend(writer)
                    .apply_async(CommitIntent::Acquire {
                        delivery,
                        expected,
                        purpose: LeasePurpose::Deliver,
                    })
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::LeaseAlreadyHeld)
            );
        }
    }

    #[tokio::test]
    async fn stale_capture_and_competing_owner_are_distinct() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        assert_eq!(
            state.acquire_lease(id(5)).await,
            Err(RuntimeError::LeaseHeld)
        );
        let old = state.capture().await.unwrap();
        let next = state
            .commit(owner, &old, &mutation(6), digest(7), &NoFault)
            .await
            .unwrap();
        assert_ne!(next, old);
        assert!(matches!(
            state
                .commit(owner, &old, &mutation(8), digest(9), &NoFault)
                .await,
            Err(RuntimeError::StaleCapture { .. })
        ));
    }
    #[tokio::test]
    async fn abandoned_owner_recovery_fences_the_old_lease() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let old = state.acquire_lease(id(4)).await.unwrap();
        let new = state.recover_abandoned(id(4), id(5)).await.unwrap();
        assert!(new.epoch > old.epoch);
        let capture = state.capture().await.unwrap();
        assert_eq!(
            state
                .commit(old, &capture, &mutation(6), digest(7), &NoFault)
                .await,
            Err(RuntimeError::OwnerLost)
        );
        assert!(
            state
                .commit(new, &capture, &mutation(6), digest(7), &NoFault)
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn malformed_identity_and_digest_fail_closed_without_diagnostics() {
        let (_temp, repo) = repository();
        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: [0; 16],
                    repository_id: id(2)
                },
                digest(3)
            )
            .await,
            Err(RuntimeError::InvalidIdentity)
        ));
        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: id(1),
                    repository_id: id(2)
                },
                [0; 32]
            )
            .await,
            Err(RuntimeError::InvalidDigest)
        ));
        assert!(!RuntimeError::StorageUnavailable.to_string().contains('/'));
    }
    #[tokio::test]
    async fn freeze_is_idempotent_and_reopen_validates_recovery() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        let checkpoint = Checkpoint {
            generation: 1,
            digest: digest(6),
            mutation_sequence: 1,
        };
        let first = state.freeze(id(7), &checkpoint).await.unwrap();
        assert_eq!(state.freeze(id(7), &checkpoint).await.unwrap(), first);
        assert_eq!(
            state
                .freeze(
                    id(7),
                    &Checkpoint {
                        digest: digest(8),
                        ..checkpoint
                    }
                )
                .await,
            Err(RuntimeError::ConflictingPublicationIntent)
        );
        drop(state);
        let reopened = open_state(&repo).await;
        reopened.validate_recovery().await.unwrap();
        assert_eq!(reopened.pending().await.unwrap(), vec![mutation(5)]);
    }
    #[tokio::test]
    async fn duplicate_request_reservation_is_idempotent() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let (first, inserted) = state
            .reserve_request_with_admission(identity, digest(6))
            .await
            .unwrap();
        assert!(inserted);

        let (second, inserted) = state
            .reserve_request_with_admission(identity, digest(6))
            .await
            .unwrap();
        assert!(!inserted);
        assert_eq!(second, first);
        assert_eq!(
            state.request_status(identity, digest(6)).await.unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            state.request_status_for_identity(identity).await.unwrap(),
            Some(first)
        );
    }
    #[tokio::test]
    async fn request_fingerprint_mismatch_is_stable() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        state.reserve_request(identity, digest(6)).await.unwrap();

        for error in [
            state
                .reserve_request(identity, digest(7))
                .await
                .unwrap_err(),
            state.start_request(identity, digest(7)).await.unwrap_err(),
            state.request_status(identity, digest(7)).await.unwrap_err(),
        ] {
            assert_eq!(error, RuntimeError::RequestFingerprintMismatch);
            assert_eq!(error.to_string(), "runtime request fingerprint mismatch");
        }
        assert_eq!(
            state
                .request_status(identity, digest(6))
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Reserved
        );
    }
    #[tokio::test]
    async fn terminal_request_replays_retained_outcome_after_reopen() {
        let (_temp, repo) = repository();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let state = open_state(&repo).await;
        state.reserve_request(identity, fingerprint).await.unwrap();
        state.start_request(identity, fingerprint).await.unwrap();
        let completed = state
            .complete_request(identity, fingerprint, outcome(7))
            .await
            .unwrap();
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .reserve_request(identity, fingerprint)
                .await
                .unwrap(),
            completed
        );
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap(),
            Some(completed)
        );
    }
    #[tokio::test]
    async fn stale_and_terminal_request_mutations_are_rejected() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state.start_request(identity, fingerprint).await.unwrap();
        assert_eq!(
            state.start_request(identity, fingerprint).await,
            Err(RuntimeError::RequestStateConflict)
        );
        let completed = state
            .complete_request(identity, fingerprint, outcome(7))
            .await
            .unwrap();

        assert_eq!(
            state
                .complete_request(identity, fingerprint, outcome(8))
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state
                .cancel_request(identity, fingerprint, outcome(8))
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state
                .orphan_request(identity, fingerprint, outcome(8))
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state.request_status(identity, fingerprint).await.unwrap(),
            Some(completed)
        );
    }
    #[tokio::test]
    async fn request_cancellation_is_terminal_and_bounded() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();

        let cancelled = state
            .cancel_request(identity, fingerprint, outcome(7))
            .await
            .unwrap();
        assert_eq!(cancelled.state, RequestState::Cancelled);
        assert_eq!(cancelled.terminal_outcome, Some(outcome(7)));
        assert_eq!(
            TerminalOutcome::new(vec![0; MAX_TERMINAL_OUTCOME_BYTES + 1]),
            Err(RuntimeError::TerminalOutcomeTooLarge)
        );
    }
    #[tokio::test]
    async fn only_running_requests_can_be_orphaned() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        assert_eq!(
            state
                .orphan_request(identity, fingerprint, outcome(7))
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        state.start_request(identity, fingerprint).await.unwrap();

        let orphaned = state
            .orphan_request(identity, fingerprint, outcome(7))
            .await
            .unwrap();
        assert_eq!(orphaned.state, RequestState::Orphaned);
        assert_eq!(orphaned.terminal_outcome, Some(outcome(7)));
    }
    #[tokio::test]
    async fn recovery_rejects_malformed_request_ledger_state() {
        let (_temp, repo) = repository();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let state = open_state(&repo).await;
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .connection
            .execute_batch(
                "PRAGMA ignore_check_constraints = ON;
                 UPDATE request_ledger SET state = 3, terminal_outcome = NULL;
                 PRAGMA ignore_check_constraints = OFF;",
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
        drop(state);

        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: id(1),
                    repository_id: id(2),
                },
                digest(3),
            )
            .await,
            Err(RuntimeError::RecoveryInvalid)
        ));
    }
    #[tokio::test]
    async fn recovery_rejects_noncontiguous_checkpoint_generations() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let next = state
            .commit(lease, &capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        state
            .commit(lease, &next, &mutation(7), digest(8), &NoFault)
            .await
            .unwrap();
        state
            .connection
            .execute(
                "UPDATE checkpoint SET generation = 0 WHERE generation = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
        drop(state);

        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: id(1),
                    repository_id: id(2),
                },
                digest(3),
            )
            .await,
            Err(RuntimeError::RecoveryInvalid)
        ));
    }
    #[tokio::test]
    async fn recovery_rejects_dangling_checkpoint_or_freeze_anchors() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
        state.freeze(id(7), &checkpoint).await.unwrap();
        state
            .connection
            .execute(
                "UPDATE checkpoint SET mutation_sequence = 2 WHERE generation = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
        drop(state);

        assert!(matches!(
            RuntimeState::open(
                &repo,
                RuntimeIdentity {
                    database_id: id(1),
                    repository_id: id(2),
                },
                digest(3),
            )
            .await,
            Err(RuntimeError::RecoveryInvalid)
        ));
    }
    #[tokio::test]
    async fn worktrees_resolve_isolated_runtime_databases() {
        let (temp, primary) = repository();
        let linked = temp.path().join("linked");
        git(temp.path(), &["commit", "--allow-empty", "-m", "root"]);
        git(
            temp.path(),
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        let secondary = Repository::discover(&linked).unwrap();
        assert_ne!(
            primary.runtime_paths().state_db(),
            secondary.runtime_paths().state_db()
        );
        let first = open_state(&primary).await;
        let second = open_state(&secondary).await;
        assert_ne!(
            first.capture().await.unwrap().runtime_id(),
            second.capture().await.unwrap().runtime_id()
        );
    }
}
