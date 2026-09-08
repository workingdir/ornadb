//! Durable private local runtime state for one Orna worktree.
//!
//! The database is intentionally below the Git boundary: callers obtain its
//! location only from [`orna_repository_v1::Repository::runtime_paths`].
//! This crate does not publish, project, compact, or contact a remote.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, OpenOptions},
    future::{Future, Ready, ready},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use libsql::{Builder, Connection, TransactionBehavior, params};
use num_bigint::BigInt;
use orna_foundation_v1::{
    CanonicalSnapshot, CheckpointRef, CwdCapture, FailureRef, OvbRaw, RowRef, RunRef,
    SYS_RUN_TABLE_ID, SYS_STREAM_TABLE_ID, Snapshot, StreamRef, Value, checkpoint_reference,
    failure_reference, validate_checkpoint_reference, validate_failure_reference,
    validate_run_reference, validate_stream_reference,
};
#[cfg(test)]
use orna_foundation_v1::{SYS_CHECKPOINT_TABLE_ID, SYS_FAILURE_TABLE_ID};
use orna_repository_v1::{CompactPublicationPending, CompactRuntimeReceipt, Repository};
pub use orna_stream_v1::{
    AsyncCheckpointBackend, Checkpoint as StreamCheckpoint, CheckpointKey, Component,
    ConsumerIdentity, StreamFailurePayload,
};
use orna_stream_v1::{
    AsyncFailurePayloadBackend, CancellationClassification, CheckpointPrecondition, CommitIntent,
    CommitResult, DeliveryIdentity, DeliveryLease, DiagnosticClass, DiagnosticCode,
    FailureIdentity, FailureRecord, FailureStatus, LeasePurpose, Position, RejectReason,
    ReplayGrant, SafeDiagnostic, StreamState, StreamStatus,
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
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    compact_receipt_seed BLOB CHECK (compact_receipt_seed IS NULL OR length(compact_receipt_seed) = 32),
    compact_receipt_public_key BLOB CHECK (compact_receipt_public_key IS NULL OR length(compact_receipt_public_key) = 32)
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
CREATE TABLE IF NOT EXISTS table_row (
    table_id TEXT NOT NULL CHECK (length(table_id) > 0),
    row_key BLOB NOT NULL CHECK (length(row_key) > 0),
    row_value BLOB NOT NULL,
    row_digest BLOB NOT NULL CHECK (length(row_digest) = 32),
    PRIMARY KEY (table_id, row_key)
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
    compact_watermark BLOB CHECK (compact_watermark IS NULL OR length(compact_watermark) = 32),
    compact_commit_id BLOB CHECK (compact_commit_id IS NULL OR length(compact_commit_id) IN (40, 64)),
    compact_journal_verifier BLOB CHECK (compact_journal_verifier IS NULL OR length(compact_journal_verifier) = 32),
    frozen INTEGER NOT NULL CHECK (frozen = 1),
    CHECK (
        (compact_watermark IS NULL AND compact_commit_id IS NULL AND compact_journal_verifier IS NULL)
        OR (compact_watermark IS NOT NULL AND compact_commit_id IS NOT NULL AND compact_journal_verifier IS NOT NULL)
    )
);
CREATE TABLE IF NOT EXISTS publication_commit (
    intent_id BLOB PRIMARY KEY REFERENCES publication_freeze(intent_id),
    commit_id BLOB NOT NULL CHECK (length(commit_id) IN (40, 64)),
    compact_receipt BLOB
);
CREATE TABLE IF NOT EXISTS request_ledger (
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    state INTEGER NOT NULL CHECK (state IN (1, 2, 3, 4, 5)),
    terminal_outcome BLOB,
    owner_id BLOB,
    owner_epoch INTEGER,
    effect_evidence INTEGER NOT NULL DEFAULT 0 CHECK (effect_evidence IN (0, 1, 2)),
    recovery_disposition INTEGER NOT NULL DEFAULT 0 CHECK (recovery_disposition IN (0, 1, 2)),
    controlled_transaction_proof BLOB,
    controlled_rollback_proof BLOB,
    PRIMARY KEY (session_id, request_id),
    CHECK (
        (state IN (1, 2) AND terminal_outcome IS NULL)
        OR (state IN (3, 4, 5) AND terminal_outcome IS NOT NULL)
    ),
    CHECK (terminal_outcome IS NULL OR length(terminal_outcome) <= 16777216),
    CHECK ((owner_id IS NULL) = (owner_epoch IS NULL)),
    CHECK (owner_id IS NULL OR length(owner_id) = 16),
    CHECK (owner_epoch IS NULL OR owner_epoch > 0)
);
CREATE TABLE IF NOT EXISTS session_deletion (
    session_id BLOB PRIMARY KEY CHECK (length(session_id) = 16),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    state INTEGER NOT NULL CHECK (state IN (1, 2))
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
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
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
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
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
CREATE TABLE IF NOT EXISTS stream_pause_pending (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0)
);
CREATE TABLE IF NOT EXISTS stream_pause_reason (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    reason TEXT NOT NULL CHECK (length(reason) <= 16777216)
);
CREATE TABLE IF NOT EXISTS sys_run_observation (
    run_id BLOB PRIMARY KEY CHECK (length(run_id) = 16),
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    consumer_identity TEXT NOT NULL CHECK (length(consumer_identity) > 0),
    function_name TEXT NOT NULL CHECK (length(function_name) > 0),
    source_identity TEXT,
    invocation_id BLOB NOT NULL CHECK (length(invocation_id) = 16),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation >= 0),
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    observed_ms INTEGER NOT NULL,
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 6),
    checkpoint_count INTEGER NOT NULL DEFAULT 0 CHECK (checkpoint_count >= 0),
    diagnostic_code INTEGER,
    diagnostic_class INTEGER,
    CHECK ((diagnostic_code IS NULL) = (diagnostic_class IS NULL))
);
CREATE TABLE IF NOT EXISTS sys_stream_observation (
    stream_id BLOB PRIMARY KEY CHECK (length(stream_id) = 16),
    run_id BLOB NOT NULL REFERENCES sys_run_observation(run_id),
    checkpoint_key_id TEXT NOT NULL UNIQUE CHECK (length(checkpoint_key_id) > 0),
    producer TEXT NOT NULL CHECK (length(producer) > 0),
    consumer_name TEXT,
    consumer_identity TEXT NOT NULL CHECK (length(consumer_identity) > 0),
    source_identity TEXT NOT NULL CHECK (length(source_identity) > 0),
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 8),
    items_seen INTEGER NOT NULL DEFAULT 0 CHECK (items_seen >= 0),
    items_committed INTEGER NOT NULL DEFAULT 0 CHECK (items_committed >= 0),
    items_failed INTEGER NOT NULL DEFAULT 0 CHECK (items_failed >= 0),
    checkpoint_version INTEGER,
    last_failure_identity TEXT,
    last_item_ms INTEGER,
    diagnostic_code INTEGER,
    diagnostic_class INTEGER,
    observed_ms INTEGER NOT NULL,
    UNIQUE(run_id, source_identity, partition),
    CHECK ((diagnostic_code IS NULL) = (diagnostic_class IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_null_natural_key
    ON sys_stream_observation (run_id, source_identity)
    WHERE partition IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_present_natural_key
    ON sys_stream_observation (run_id, source_identity, partition)
    WHERE partition IS NOT NULL;
"#;

pub const MAX_TERMINAL_OUTCOME_BYTES: usize = 16 * 1024 * 1024;

const SESSION_DELETION_CLOSING: i64 = 1;
const SESSION_DELETION_CLOSED: i64 = 2;

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

const MAX_TABLE_MUTATION_BYTES: usize = 16 * 1024 * 1024;

/// A schema-independent durable table change.
///
/// This is the first typed boundary between table execution and the durable
/// runtime.  The runtime persists the canonical table/key/value operation in
/// the same transaction as its mutation ledger and CWD checkpoint; schema
/// admission and evaluator integration remain owned by their respective
/// layers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableMutation {
    id: [u8; 16],
    table: String,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
}

impl TableMutation {
    pub fn new(
        id: [u8; 16],
        table: impl Into<String>,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    ) -> Result<Self, RuntimeError> {
        let table = table.into();
        validate_table_mutation(id, &table, &key, value.as_deref())?;
        Ok(Self {
            id,
            table,
            key,
            value,
        })
    }

    pub fn id(&self) -> [u8; 16] {
        self.id
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Decodes a mutation previously produced by this typed boundary.
    /// Generic runtime mutations are rejected unless their payload, digest,
    /// and canonical table operation all validate together.
    pub fn decode(mutation: &Mutation) -> Result<Self, RuntimeError> {
        validate_id(mutation.id)?;
        let digest: [u8; 32] = Sha256::digest(&mutation.payload).into();
        if digest != mutation.digest {
            return Err(RuntimeError::InvalidTableMutation);
        }
        let mut cursor = 0;
        let prefix = b"ORNA-TABLE-MUTATION\0";
        if mutation.payload.get(..prefix.len()) != Some(prefix) {
            return Err(RuntimeError::InvalidTableMutation);
        }
        cursor += prefix.len();
        let table =
            String::from_utf8(read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec())
                .map_err(|_| RuntimeError::InvalidTableMutation)?;
        let key = read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec();
        let value = match mutation.payload.get(cursor).copied() {
            Some(0) => {
                cursor += 1;
                None
            }
            Some(1) => {
                cursor += 1;
                Some(read_length_prefixed(&mutation.payload, &mut cursor)?.to_vec())
            }
            _ => return Err(RuntimeError::InvalidTableMutation),
        };
        if cursor != mutation.payload.len() {
            return Err(RuntimeError::InvalidTableMutation);
        }
        Self::new(mutation.id, table, key, value)
    }

    fn runtime_mutation(&self) -> Result<Mutation, RuntimeError> {
        let payload = encode_table_mutation(self)?;
        Ok(Mutation {
            id: self.id,
            digest: Sha256::digest(&payload).into(),
            payload,
        })
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCommitId(Vec<u8>);

impl PublicationCommitId {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, RuntimeError> {
        let value = value.into();
        if !matches!(value.len(), 40 | 64) || !value.iter().all(u8::is_ascii_hexdigit) {
            return Err(RuntimeError::InvalidPublicationCommit);
        }
        Ok(Self(value))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestIdentity {
    pub session_id: [u8; 16],
    pub request_id: [u8; 16],
}

/// Durable identity of the activation that owns a running request.
///
/// The epoch is a fence: recovery is permitted only after a different writer
/// lease has replaced this exact owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestOwner {
    pub owner_id: [u8; 16],
    pub epoch: u64,
}

impl From<WriterLease> for RequestOwner {
    fn from(value: WriterLease) -> Self {
        Self {
            owner_id: value.owner_id,
            epoch: value.epoch,
        }
    }
}

/// The retained meaning of a recovered orphaned request. Protocol state stays
/// `orphaned`; the serving layer maps this durable distinction to diagnostics.
/// `RollbackProven` requires a validated durable rollback receipt; otherwise
/// recovery retains `ExternalEffectsUncertain`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    RollbackProven,
    ExternalEffectsUncertain,
}

impl RecoveryDisposition {
    const fn code(self) -> i64 {
        match self {
            Self::RollbackProven => 1,
            Self::ExternalEffectsUncertain => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredRequest {
    pub status: RequestStatus,
    pub disposition: RecoveryDisposition,
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

/// Runtime-owned identity for one durable `sys.Run` observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RunObservationId([u8; 16]);

impl RunObservationId {
    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Runtime-owned identity for one durable `sys.Stream` observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StreamObservationId([u8; 16]);

impl StreamObservationId {
    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunObservationStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
    Orphaned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamObservationStatus {
    Starting,
    Running,
    Paused,
    BackingOff,
    Completed,
    Failed,
    Cancelled,
    Orphaned,
}

/// All caller-supplied metadata required before an activation may execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunObservationRegistration {
    pub request: RequestIdentity,
    pub consumer_identity: ConsumerIdentity,
    pub function: String,
    pub source_identity: Option<String>,
    pub invocation_id: [u8; 16],
}

/// The result of atomically admitting an observed request before evaluator
/// code may run. A matching terminal request is replayed without requiring a
/// writer lease or creating a second run observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedRequestStart {
    pub request: RequestStatus,
    pub run: Option<RunObservation>,
    pub admitted: bool,
}

/// All caller-supplied metadata required before a stream may consume an item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamObservationRegistration {
    pub run: RunObservationId,
    pub producer: String,
    pub consumer: Option<String>,
    pub checkpoint: CheckpointKey,
}

/// Durable `sys.Run` projection. `live` is derived at read time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunObservation {
    pub id: RunObservationId,
    pub request: RequestIdentity,
    pub consumer_identity: ConsumerIdentity,
    pub function: String,
    pub source_identity: Option<String>,
    pub snapshot: CwdCapture,
    pub runtime_id: [u8; 16],
    pub invocation_id: [u8; 16],
    /// Durable Unix milliseconds at which this run was admitted.
    pub started_ms: i64,
    /// Durable Unix milliseconds at which this run became terminal.
    pub ended_ms: Option<i64>,
    /// Durable Unix milliseconds at which this projection was last updated.
    pub observed_ms: i64,
    pub status: RunObservationStatus,
    /// Runtime generation stored with the durable observation. This must
    /// agree with the generation in [`Self::snapshot`] before a reference is
    /// projected.
    pub runtime_generation: i64,
    pub checkpoint_count: u64,
    pub diagnostic: Option<SafeDiagnostic>,
    pub live: bool,
}

impl RunObservation {
    /// Reconstructs this retained observation's checked `sys.RunRef` using
    /// its own pinned snapshot and database identity.
    ///
    /// This only proves that the durable coordinates have the canonical
    /// `sys.Run` shape. It does not prove row existence, retention, or a
    /// caller's authority to observe the row; an authenticated public query
    /// surface remains a separate boundary.
    pub fn reference(&self) -> Result<RunRef, RuntimeError> {
        let generation = bigint_to_i64(self.snapshot.generation())
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        if self.runtime_id != self.snapshot.runtime_id() || self.runtime_generation != generation {
            return Err(RuntimeError::InvalidObservationReference);
        }
        let reference = RowRef::new(
            self.snapshot.database_id(),
            SYS_RUN_TABLE_ID,
            opaque_reference_key(self.id.0),
            self.snapshot.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        validate_run_reference(reference, &self.snapshot)
            .map_err(|_| RuntimeError::InvalidObservationReference)
    }
}

/// Durable `sys.Stream` projection. `live` is derived from its parent run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamObservation {
    pub id: StreamObservationId,
    pub run: RunObservationId,
    pub producer: String,
    pub consumer: Option<String>,
    /// Durable consumer identity copied from the stream natural key.
    pub consumer_identity: ConsumerIdentity,
    /// Durable source identity copied from the stream natural key.
    pub source_identity: String,
    /// Durable optional partition copied from the stream natural key.
    pub partition: Option<String>,
    // Loader-owned copies of the durable observation and checkpoint-key
    // partition values. Keeping these separate from the public projection
    // prevents a caller from changing `partition` on a cloned observation and
    // manufacturing a different `sys.Stream` natural key.
    partition_evidence: StreamPartitionEvidence,
    /// The full pinned capture copied from the stream's durable parent run.
    /// It prevents reference projection from accepting a same-ID parent from
    /// another snapshot or runtime generation.
    pub parent_capture: CwdCapture,
    /// Checked reference to the retained durable checkpoint bound to this
    /// stream. This is causal metadata, not stream-control authority.
    pub checkpoint_reference: CheckpointRef,
    pub checkpoint: CheckpointKey,
    /// The most recent retained delivery failure for this stream, when one
    /// exists. Cancellation and activation failures do not manufacture it.
    pub last_failure: Option<FailureRef>,
    pub status: StreamObservationStatus,
    pub items_seen: u64,
    pub items_committed: u64,
    pub items_failed: u64,
    /// Durable Unix milliseconds of the most recently observed stream item.
    pub last_item_ms: Option<i64>,
    pub diagnostic: Option<SafeDiagnostic>,
    /// Durable Unix milliseconds at which this projection was last updated.
    pub observed_ms: i64,
    pub live: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamPartitionEvidence {
    observation: Option<String>,
    checkpoint: Option<String>,
}

impl StreamObservation {
    /// Reconstructs this retained observation's checked `sys.StreamRef` from
    /// the exact `run + source_identity + partition` natural key.
    ///
    /// The caller must supply the retained parent run so this boundary can
    /// reject mismatched observation coordinates and keep the stream pinned
    /// to the run's admission snapshot. As with [`RunObservation::reference`],
    /// a valid reference is not evidence of row existence or observation
    /// authority.
    pub fn reference(&self, run: &RunObservation) -> Result<StreamRef, RuntimeError> {
        if self.run != run.id
            || self.parent_capture != run.snapshot
            || self.consumer_identity != self.checkpoint.consumer
            || self.source_identity != self.checkpoint.source.as_str()
            || self.partition != self.partition_evidence.observation
            || self.partition != self.partition_evidence.checkpoint
            || self.partition_evidence.checkpoint
                != self
                    .checkpoint
                    .partition
                    .as_ref()
                    .map(|partition| partition.as_str().to_owned())
        {
            return Err(RuntimeError::ObservationCoordinateMismatch);
        }
        let run_reference = run.reference()?;
        let reference = RowRef::new(
            run_reference.as_row_ref().database_id,
            SYS_STREAM_TABLE_ID,
            OvbRaw::Array(vec![
                row_reference_raw(run_reference.as_row_ref()),
                OvbRaw::Text(self.source_identity.clone()),
                self.partition
                    .clone()
                    .map(OvbRaw::Text)
                    .unwrap_or(OvbRaw::Null),
            ]),
            run_reference.as_row_ref().snapshot.clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        validate_stream_reference(reference, &run.snapshot)
            .map_err(|_| RuntimeError::InvalidObservationReference)
    }
}

/// The durable result of finalizing one owner-fenced table activation.
///
/// `capture` and `request` are returned from the same committed transaction;
/// a matching terminal replay returns the retained pair without applying the
/// supplied activation again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestActivationCommit {
    pub capture: CwdCapture,
    pub request: RequestStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    AfterMutation,
    AfterCheckpoint,
    AfterCapture,
    AfterFailureRecord,
    AfterFailurePayload,
    AfterReplayFailureRecord,
    BeforeTableWrite,
    AfterTableWrite,
    BeforeTerminalClaim,
    AfterTerminalClaim,
}

/// Deterministic test seam that selects a failure. A rollback receipt still
/// requires the runtime to observe an explicit rollback of its own
/// transaction.
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
    InvalidObservationReference,
    ObservationCoordinateMismatch,
    StreamIdentityMismatch,
    StreamCheckpointStale,
    LeaseHeld,
    OwnerLost,
    StaleCapture { current: Box<CwdCapture> },
    InvalidCapture,
    EmptyMutationBatch,
    InvalidTableMutation,
    ConflictingPublicationIntent,
    ConflictingPublicationCommit,
    InvalidPublicationCommit,
    CompactPublicationRequired,
    CompactReceiptKeyMismatch,
    InvalidCompactReceipt,
    RequestUnknown,
    RequestFingerprintMismatch,
    RequestOwnerConflict,
    RequestStateConflict,
    SessionClosed,
    SessionWorkActive,
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
            Self::InvalidObservationReference => "invalid runtime observation reference",
            Self::ObservationCoordinateMismatch => "runtime observation coordinates do not match",
            Self::StreamIdentityMismatch => "stream source identity mismatch",
            Self::StreamCheckpointStale => "stream checkpoint is stale",
            Self::LeaseHeld => "runtime writer is held",
            Self::OwnerLost => "runtime writer ownership was lost",
            Self::StaleCapture { .. } => "runtime capture is stale",
            Self::InvalidCapture => "invalid runtime capture",
            Self::EmptyMutationBatch => "empty runtime mutation batch",
            Self::InvalidTableMutation => "invalid durable table mutation",
            Self::ConflictingPublicationIntent => "conflicting publication intent",
            Self::ConflictingPublicationCommit => "conflicting publication commit",
            Self::InvalidPublicationCommit => "invalid publication commit",
            Self::CompactPublicationRequired => {
                "compact-bound publication requires a runtime receipt"
            }
            Self::CompactReceiptKeyMismatch => {
                "runtime compact receipt key does not match initialization"
            }
            Self::InvalidCompactReceipt => "invalid compact runtime receipt",
            Self::RequestUnknown => "runtime request is unknown",
            Self::RequestFingerprintMismatch => "runtime request fingerprint mismatch",
            Self::RequestOwnerConflict => "runtime request owner cannot be recovered",
            Self::RequestStateConflict => "runtime request state conflict",
            Self::SessionClosed => "runtime session is closed",
            Self::SessionWorkActive => "runtime session still has active work",
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
    compact_receipt_signing_key: SigningKey,
}

enum RequestActivationTransactionError {
    Runtime(RuntimeError),
    RolledBack(RuntimeError),
}

impl From<RuntimeError> for RequestActivationTransactionError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionDeletionRecord {
    owner: WriterLease,
    state: i64,
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

/// The immutable admission view for a table-backed activation.
///
/// The context and every requested relation are read from one database
/// transaction, so evaluation cannot combine a generation from one durable
/// state with rows from another.
pub type RuntimeTableRows = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTableActivationSnapshot {
    context: RuntimeActivationContext,
    table_rows: RuntimeTableRows,
}

impl RuntimeTableActivationSnapshot {
    pub fn context(&self) -> &RuntimeActivationContext {
        &self.context
    }

    pub fn table_rows(&self) -> &RuntimeTableRows {
        &self.table_rows
    }
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

pub struct StreamTableDeliveryCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [TableMutation],
    pub next_digest: [u8; 32],
    pub delivery: DeliveryLease,
    pub expected_stream: CheckpointPrecondition,
    pub faults: &'a dyn FaultInjector,
}

struct StreamDeliveryParts<'a> {
    writer: WriterLease,
    expected_capture: &'a CwdCapture,
    mutations: &'a [Mutation],
    table_mutations: &'a [TableMutation],
    next_digest: [u8; 32],
    delivery: DeliveryLease,
    expected_stream: CheckpointPrecondition,
    faults: &'a dyn FaultInjector,
}

pub struct StreamReplayCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [Mutation],
    pub table_mutations: &'a [TableMutation],
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

pub struct StreamTableMutationBatch {
    pub mutations: Vec<TableMutation>,
    pub next_digest: [u8; 32],
}

pub enum StreamHandlerResult {
    Commit(StreamMutationBatch),
    CommitTable(StreamTableMutationBatch),
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
    /// Returns the exact durable stream key served by this connector.
    ///
    /// A connector cannot be admitted without declaring the stable source,
    /// partition, and position-format identities represented by its durable
    /// checkpoint. The runner compares this key before it loads a checkpoint
    /// or polls the provider.
    fn checkpoint_key(&self) -> CheckpointKey;
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

    fn checkpoint_key(&self) -> CheckpointKey {
        self.key.clone()
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

/// The durable outcome of one stream-administration transition.
///
/// This is deliberately narrower than the public `sys.admin` result: a
/// pending pause has stopped new delivery admission, but its active delivery
/// has not reached the transaction boundary that publishes `Paused` yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamAdministrationOutcome {
    /// The stream is durably paused; `changed` is false for an existing pause.
    Paused { changed: bool },
    /// A pause is durably pending the active delivery boundary.
    PausePending { changed: bool },
    /// The stream is durably running; `changed` is false for an existing run.
    Running { changed: bool },
    /// A live delivery lease prevents the requested transition.
    Busy,
    /// A blocking delivery failure prevents resume.
    BlockingFailure,
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
            Some(
                repository
                    .runtime_paths()
                    .compact_runtime_receipt_public_key(),
            ),
        )
        .await
    }

    async fn open_path(
        path: &Path,
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        public_key_path: Option<PathBuf>,
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
        Self::initialize_runtime_meta(&connection, identity, initial_digest).await?;
        migrate_compact_receipt_schema(&connection).await?;
        let (compact_receipt_signing_key, compact_receipt_public_key) =
            initialize_compact_receipt_key(&connection).await?;
        if let Some(path) = public_key_path {
            initialize_compact_receipt_public_key(&path, compact_receipt_public_key)?;
        }
        let state = Self {
            connection,
            compact_receipt_signing_key,
        };
        state.migrate_observation_projection_schema().await?;
        state.migrate_stream_observation_failure_identity().await?;
        state.migrate_request_recovery_evidence().await?;
        state.migrate_stream_failure_payloads().await?;
        state.migrate_nullable_stream_partitions().await?;
        state.validate_recovery().await?;
        Ok(state)
    }

    /// Creates a stream backend whose mutations are fenced by this writer lease.
    pub fn stream_backend(&self, lease: WriterLease) -> RuntimeStreamBackend<'_> {
        RuntimeStreamBackend { state: self, lease }
    }

    /// Registers a durable `sys.Run` observation before user code is allowed
    /// to execute. Registration is bound to an already-admitted request and
    /// the exact CWD capture observed in the same transaction.
    pub async fn register_run_observation(
        &self,
        registration: RunObservationRegistration,
    ) -> Result<RunObservation, RuntimeError> {
        validate_request_identity(registration.request)?;
        validate_id(registration.invocation_id)?;
        validate_observation_text(&registration.function)?;
        if let Some(source) = &registration.source_identity {
            validate_observation_text(source)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let request = request_status_tx(&tx, registration.request)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        if request.state != RequestState::Reserved {
            return Err(RuntimeError::RequestStateConflict);
        }
        let capture = capture_tx(&tx).await?;
        let id = RunObservationId(*Uuid::new_v4().as_bytes());
        tx.execute(
            "INSERT INTO sys_run_observation (run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?12, ?13, 0, NULL, NULL)",
            params![
                id.0.to_vec(), registration.request.session_id.to_vec(), registration.request.request_id.to_vec(),
                registration.consumer_identity.canonical(), registration.function, registration.source_identity,
                registration.invocation_id.to_vec(), encode_capture(&capture)?, capture.generation_digest().to_vec(), capture.runtime_id().to_vec(),
                bigint_to_i64(capture.generation())?, now_ms()?, run_status_code(RunObservationStatus::Starting),
            ],
        ).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.run_observation(id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)
    }

    /// Atomically admits a request, pins its capture, creates its `sys.Run`
    /// observation, and records the owner-fenced running state. Callers must
    /// invoke this before evaluator code. A matching terminal request is
    /// replayed without acquiring or checking a new writer lease.
    pub async fn begin_observed_request(
        &self,
        registration: RunObservationRegistration,
        fingerprint: [u8; 32],
        owner: WriterLease,
    ) -> Result<ObservedRequestStart, RuntimeError> {
        validate_request_identity(registration.request)?;
        validate_id(registration.invocation_id)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        validate_observation_text(&registration.function)?;
        if let Some(source) = &registration.source_identity {
            validate_observation_text(source)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if let Some(status) = request_status_tx(&tx, registration.request).await? {
            require_fingerprint(&status, fingerprint)?;
            if status.state.is_terminal() {
                tx.commit()
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                return Ok(ObservedRequestStart {
                    request: status,
                    run: None,
                    admitted: false,
                });
            }
            return Err(RuntimeError::RequestStateConflict);
        }
        ensure_session_admission_open(&tx, registration.request.session_id).await?;
        self.require_owner(&tx, owner).await?;
        let capture = capture_tx(&tx).await?;
        tx.execute(
            "INSERT INTO request_ledger (session_id, request_id, fingerprint, state, terminal_outcome, owner_id, owner_epoch) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                registration.request.session_id.to_vec(),
                registration.request.request_id.to_vec(),
                fingerprint.to_vec(),
                RequestState::Running.code(),
                owner.owner_id.to_vec(),
                i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        let id = RunObservationId(*Uuid::new_v4().as_bytes());
        tx.execute(
            "INSERT INTO sys_run_observation (run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?12, ?13, 0, NULL, NULL)",
            params![
                id.0.to_vec(), registration.request.session_id.to_vec(), registration.request.request_id.to_vec(),
                registration.consumer_identity.canonical(), registration.function, registration.source_identity,
                registration.invocation_id.to_vec(), encode_capture(&capture)?, capture.generation_digest().to_vec(), capture.runtime_id().to_vec(),
                bigint_to_i64(capture.generation())?, now_ms()?, run_status_code(RunObservationStatus::Running),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let run = self
            .run_observation(id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        Ok(ObservedRequestStart {
            request: RequestStatus {
                identity: registration.request,
                fingerprint,
                state: RequestState::Running,
                terminal_outcome: None,
            },
            run: Some(run),
            admitted: true,
        })
    }

    /// Registers exactly one durable `sys.Stream` observation for a runtime
    /// checkpoint key. The unique checkpoint binding prevents a stream row
    /// from being paired with a different consumer/source/partition later.
    pub async fn register_stream_observation(
        &self,
        registration: StreamObservationRegistration,
    ) -> Result<StreamObservation, RuntimeError> {
        validate_observation_text(&registration.producer)?;
        if let Some(consumer) = &registration.consumer {
            validate_observation_text(consumer)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let capture = capture_tx(&tx).await?;
        let run = load_run_observation_tx(&tx, registration.run, &capture)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if run.status.is_terminal() || run.runtime_id != capture.runtime_id() {
            return Err(RuntimeError::RequestStateConflict);
        }
        ensure_stream_checkpoint(&tx, &registration.checkpoint).await?;
        let id = StreamObservationId(*Uuid::new_v4().as_bytes());
        let key_id = stream_key_id(&registration.checkpoint);
        let partition = registration
            .checkpoint
            .partition
            .as_ref()
            .map(|value| value.as_str().to_owned());
        tx.execute(
            "INSERT INTO sys_stream_observation (stream_id, run_id, checkpoint_key_id, producer, consumer_name, consumer_identity, source_identity, partition, status, items_seen, items_committed, items_failed, checkpoint_version, last_failure_identity, last_item_ms, diagnostic_code, diagnostic_class, observed_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, 0, NULL, NULL, NULL, NULL, NULL, ?10)",
            params![id.0.to_vec(), registration.run.0.to_vec(), key_id, registration.producer, registration.consumer,
                registration.checkpoint.consumer.canonical(), registration.checkpoint.source.as_str().to_owned(), partition,
                stream_observation_status_code(StreamObservationStatus::Starting), now_ms()?],
        ).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.stream_observation(id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)
    }

    /// Reads retained `sys.Run` observations. This method never starts,
    /// resumes, recovers, or otherwise mutates an activation.
    pub async fn run_observations(&self) -> Result<Vec<RunObservation>, RuntimeError> {
        let capture = self.capture().await?;
        load_run_observations(&self.connection, &capture).await
    }

    /// Reads the current-generation live subset for `sys.rt.runs`.
    pub async fn runtime_run_observations(&self) -> Result<Vec<RunObservation>, RuntimeError> {
        Ok(self
            .run_observations()
            .await?
            .into_iter()
            .filter(|row| row.live)
            .collect())
    }

    /// Reads retained `sys.Stream` observations without mutating checkpoint or lease state.
    pub async fn stream_observations(&self) -> Result<Vec<StreamObservation>, RuntimeError> {
        let capture = self.capture().await?;
        load_stream_observations(&self.connection, &capture).await
    }

    /// Reads the current-generation live subset for `sys.rt.streams`.
    pub async fn runtime_stream_observations(
        &self,
    ) -> Result<Vec<StreamObservation>, RuntimeError> {
        Ok(self
            .stream_observations()
            .await?
            .into_iter()
            .filter(|row| row.live)
            .collect())
    }

    pub async fn run_observation(
        &self,
        id: RunObservationId,
    ) -> Result<Option<RunObservation>, RuntimeError> {
        let capture = self.capture().await?;
        load_run_observation_tx(&self.connection, id, &capture).await
    }

    pub async fn stream_observation(
        &self,
        id: StreamObservationId,
    ) -> Result<Option<StreamObservation>, RuntimeError> {
        let capture = self.capture().await?;
        load_stream_observation_tx(&self.connection, id, &capture).await
    }

    /// Atomically closes durable admission for one session under the current
    /// writer lease. Repeating the operation with the same owner is safe;
    /// another owner cannot close or finalize the session.
    pub async fn begin_session_deletion(
        &self,
        session_id: [u8; 16],
        owner: WriterLease,
    ) -> Result<(), RuntimeError> {
        validate_id(session_id)?;
        validate_writer_lease(owner)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, owner).await?;
        match session_deletion_record(&transaction, session_id).await? {
            Some(record) if record.owner != owner => return Err(RuntimeError::OwnerLost),
            Some(record)
                if !matches!(
                    record.state,
                    SESSION_DELETION_CLOSING | SESSION_DELETION_CLOSED
                ) =>
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            Some(_) => {}
            None => {
                transaction
                    .execute(
                        "INSERT INTO session_deletion (session_id, owner_id, owner_epoch, state)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            session_id.to_vec(),
                            owner.owner_id.to_vec(),
                            i64::try_from(owner.epoch)
                                .map_err(|_| RuntimeError::RecoveryInvalid)?,
                            SESSION_DELETION_CLOSING,
                        ],
                    )
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Finalizes a session deletion only after the writer-owned marker still
    /// matches and no Reserved or Running durable request remains. The marker
    /// is retained in its terminal state so future reservations stay closed.
    pub async fn finish_session_deletion(
        &self,
        session_id: [u8; 16],
        owner: WriterLease,
    ) -> Result<(), RuntimeError> {
        validate_id(session_id)?;
        validate_writer_lease(owner)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, owner).await?;
        let Some(record) = session_deletion_record(&transaction, session_id).await? else {
            return Err(RuntimeError::RecoveryInvalid);
        };
        if record.owner != owner {
            return Err(RuntimeError::OwnerLost);
        }
        if !matches!(
            record.state,
            SESSION_DELETION_CLOSING | SESSION_DELETION_CLOSED
        ) {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let mut active = transaction
            .query(
                "SELECT 1 FROM request_ledger
                 WHERE session_id = ?1 AND state IN (?2, ?3)
                 LIMIT 1",
                params![
                    session_id.to_vec(),
                    RequestState::Reserved.code(),
                    RequestState::Running.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if active
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some()
        {
            return Err(RuntimeError::SessionWorkActive);
        }
        if record.state == SESSION_DELETION_CLOSED {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(());
        }
        let changed = transaction
            .execute(
                "UPDATE session_deletion SET state = ?1
                 WHERE session_id = ?2 AND owner_id = ?3 AND owner_epoch = ?4
                   AND state = ?5",
                params![
                    SESSION_DELETION_CLOSED,
                    session_id.to_vec(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    SESSION_DELETION_CLOSING,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Applies a writer-fenced durable pause transition for one resolved
    /// stream. A caller that needs the public administration result must wait
    /// for `PausePending` to publish `Paused` at the delivery boundary.
    pub async fn pause_stream(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.pause_stream_at_capture(lease, key, None).await
    }

    /// Applies a pause only if the writer-fenced transaction still observes
    /// the supplied CWD capture. Hosts resolving a snapshot-pinned system row
    /// use this boundary so a capture cannot change between reference checks
    /// and the durable transition.
    pub async fn pause_stream_at_capture(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        expected_capture: Option<&CwdCapture>,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        match apply_stream_intent(self, lease, expected_capture, CommitIntent::Pause { key })
            .await?
        {
            CommitResult::StreamStatusChanged { state, changed }
                if state.status == StreamStatus::Paused =>
            {
                Ok(StreamAdministrationOutcome::Paused { changed })
            }
            CommitResult::PausePending { changed, .. } => {
                Ok(StreamAdministrationOutcome::PausePending { changed })
            }
            CommitResult::Rejected(RejectReason::StreamBusy) => {
                Ok(StreamAdministrationOutcome::Busy)
            }
            _ => Err(RuntimeError::RecoveryInvalid),
        }
    }

    /// Applies a writer-fenced pause while retaining its supplied safe reason
    /// in the same local transaction that admits the pause. A no-op pause
    /// never overwrites the reason already attached to the existing pause.
    pub async fn pause_stream_with_reason(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        reason: String,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.pause_stream_with_reason_at_capture(lease, key, reason, None)
            .await
    }

    /// Equivalent to [`Self::pause_stream_with_reason`], additionally
    /// requiring the CWD capture observed while resolving the stream.
    pub async fn pause_stream_with_reason_at_capture(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        reason: String,
        expected_capture: Option<&CwdCapture>,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        if reason.len() > 16_777_216 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, lease).await?;
        if let Some(expected_capture) = expected_capture {
            let current_capture = capture_tx(&transaction).await?;
            if &current_capture != expected_capture {
                return Err(RuntimeError::StaleCapture {
                    current: Box::new(current_capture),
                });
            }
        }
        let result = apply_stream_intent_tx(&transaction, CommitIntent::Pause { key }).await?;
        if stream_pause_changed(&result) {
            store_stream_pause_reason(&transaction, stream_pause_key(&result)?, &reason).await?;
        }
        sync_stream_observation_tx(&transaction, &result).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        stream_administration_outcome(result)
    }

    /// Reads the most recently admitted pause reason retained for a stream.
    /// This private runtime value is not a substitute for the public audit
    /// projection.
    pub async fn stream_pause_reason(
        &self,
        key: &CheckpointKey,
    ) -> Result<Option<String>, RuntimeError> {
        load_stream_pause_reason(&self.connection, key).await
    }

    /// Applies a writer-fenced durable resume transition for one resolved
    /// stream. Source availability and public stream-reference resolution are
    /// owned by the host above this runtime boundary.
    pub async fn resume_stream(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.resume_stream_at_capture(lease, key, None).await
    }

    /// Applies a resume only if the writer-fenced transaction still observes
    /// the supplied CWD capture.
    pub async fn resume_stream_at_capture(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        expected_capture: Option<&CwdCapture>,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        match apply_stream_intent(self, lease, expected_capture, CommitIntent::Resume { key })
            .await?
        {
            CommitResult::StreamStatusChanged { state, changed }
                if state.status == StreamStatus::Running =>
            {
                Ok(StreamAdministrationOutcome::Running { changed })
            }
            CommitResult::Rejected(RejectReason::StreamBusy) => {
                Ok(StreamAdministrationOutcome::Busy)
            }
            CommitResult::Rejected(RejectReason::BlockingFailure) => {
                Ok(StreamAdministrationOutcome::BlockingFailure)
            }
            _ => Err(RuntimeError::RecoveryInvalid),
        }
    }

    /// Captures the fixed CWD and activation time for one root activation.
    pub async fn begin_activation(&self) -> Result<RuntimeActivationContext, RuntimeError> {
        Ok(RuntimeActivationContext {
            capture: self.capture().await?,
            activation_time: SystemTime::now(),
        })
    }

    /// Captures one activation context and its declared committed relations
    /// from the same durable read transaction.
    pub async fn begin_table_activation(
        &self,
        tables: &[&str],
    ) -> Result<RuntimeTableActivationSnapshot, RuntimeError> {
        for table in tables {
            validate_table_name(table)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let context = RuntimeActivationContext {
            capture: capture_tx(&transaction).await?,
            activation_time: SystemTime::now(),
        };
        let mut table_rows = BTreeMap::new();
        for table in tables {
            if table_rows.contains_key(*table) {
                continue;
            }
            let mut rows = transaction
                .query(
                    "SELECT row_key, row_value FROM table_row
                     WHERE table_id = ?1 ORDER BY row_key",
                    params![*table],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let mut values = Vec::new();
            while let Some(row) = rows
                .next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
            {
                values.push((
                    row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ));
            }
            table_rows.insert((*table).to_owned(), values);
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RuntimeTableActivationSnapshot {
            context,
            table_rows,
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

    /// Atomically publishes typed table changes, their durable mutation
    /// records, one checkpoint, and the next CWD capture.
    pub async fn commit_table_activation(
        &self,
        lease: WriterLease,
        context: &RuntimeActivationContext,
        mutations: &[TableMutation],
        next_digest: [u8; 32],
        faults: &dyn FaultInjector,
    ) -> Result<CwdCapture, RuntimeError> {
        if mutations.is_empty() {
            return Err(RuntimeError::EmptyMutationBatch);
        }
        let encoded = mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()?;
        validate_mutations(&encoded, next_digest)?;
        let current = self.capture().await?;
        if &current != context.capture() {
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
        for mutation in mutations {
            apply_table_mutation_tx(&tx, mutation).await?;
        }
        let next =
            append_mutations_tx(&tx, context.capture(), &encoded, next_digest, faults).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(next)
    }

    /// Atomically finalizes one Orna-controlled table activation and its
    /// durable request claim.
    ///
    /// The request must already be `Running` under `lease`; admission and
    /// owner assignment remain separate durable steps. On a successful first
    /// call, typed table rows, the mutation/checkpoint capture, and the
    /// validated terminal outcome commit together. If the controlled table
    /// transaction faults before commit, this method records a runtime-owned
    /// rollback receipt. That proof is deliberately narrower than an entry
    /// marker: only this method observes the transaction unwind after the
    /// controlled table boundary. An externally marked activation remains
    /// conservatively uncertain.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_table_request_activation(
        &self,
        lease: WriterLease,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        context: &RuntimeActivationContext,
        mutations: &[TableMutation],
        next_digest: [u8; 32],
        outcome: TerminalOutcome,
        faults: &dyn FaultInjector,
    ) -> Result<RequestActivationCommit, RuntimeError> {
        match self
            .commit_table_request_activation_tx(
                lease,
                identity,
                fingerprint,
                context,
                mutations,
                next_digest,
                outcome,
                faults,
            )
            .await
        {
            Err(RequestActivationTransactionError::RolledBack(error)) => {
                // A receipt follows only the explicit rollback observed at
                // the runtime-owned boundary. Commit failures never enter
                // this branch because their durable outcome is unknowable.
                self.record_controlled_rollback_proof(identity, fingerprint, lease)
                    .await?;
                Err(error)
            }
            Ok(committed) => Ok(committed),
            Err(RequestActivationTransactionError::Runtime(error)) => Err(error),
        }
    }

    /// Performs the validated bounded terminal outcome commit in one
    /// writer-fenced transaction. Any validation, cancellation, fault, or
    /// owner-loss error
    /// rolls the complete transaction back. A matching terminal request is
    /// replayed from durable state without applying `mutations` again.
    ///
    /// This is the strongest boundary available while the evaluator owns its
    /// own execution and cannot borrow this libSQL connection for the whole
    /// activation. Callers therefore stage canonical mutations in memory and
    /// must use the existing owner-fenced cancellation/failure APIs when an
    /// activation does not successfully finalize. External effects remain
    /// outside this transaction and are never implied reversible.
    #[allow(clippy::too_many_arguments)]
    async fn commit_table_request_activation_tx(
        &self,
        lease: WriterLease,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        context: &RuntimeActivationContext,
        mutations: &[TableMutation],
        next_digest: [u8; 32],
        outcome: TerminalOutcome,
        faults: &dyn FaultInjector,
    ) -> Result<RequestActivationCommit, RequestActivationTransactionError> {
        validate_request_identity(identity)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let current = request_status_tx(&transaction, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        let evidence = request_execution_evidence_tx(&transaction, identity).await?;
        validate_request_execution_evidence(&current, evidence)?;

        if current.state.is_terminal() {
            let capture = capture_tx(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(RequestActivationCommit {
                capture,
                request: current,
            });
        }

        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict.into());
        }
        validate_id(lease.owner_id)?;
        if lease.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity.into());
        }
        self.require_owner(&transaction, lease).await?;
        if evidence.owner != Some(RequestOwner::from(lease)) {
            return Err(RuntimeError::RequestOwnerConflict.into());
        }
        if mutations.is_empty() {
            return Err(RuntimeError::EmptyMutationBatch.into());
        }
        let encoded = mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()?;
        validate_mutations(&encoded, next_digest)?;
        let current_capture = capture_tx(&transaction).await?;
        if &current_capture != context.capture() {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current_capture),
            }
            .into());
        }

        if let Err(error) = faults.check(FaultPoint::BeforeTableWrite) {
            return Err(request_activation_rollback(transaction, error).await?);
        }
        for mutation in mutations {
            if let Err(error) = apply_table_mutation_tx(&transaction, mutation).await {
                return Err(request_activation_rollback(transaction, error).await?);
            }
        }
        if let Err(error) = faults.check(FaultPoint::AfterTableWrite) {
            return Err(request_activation_rollback(transaction, error).await?);
        }
        let capture = match append_mutations_tx(
            &transaction,
            context.capture(),
            &encoded,
            next_digest,
            faults,
        )
        .await
        {
            Ok(capture) => capture,
            Err(error) => return Err(request_activation_rollback(transaction, error).await?),
        };
        if let Err(error) = faults.check(FaultPoint::BeforeTerminalClaim) {
            return Err(request_activation_rollback(transaction, error).await?);
        }
        let changed = match transaction
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, owner_id = NULL,
                     owner_epoch = NULL, effect_evidence = 0,
                     recovery_disposition = 0, controlled_transaction_proof = NULL,
                     controlled_rollback_proof = NULL
                 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5
                   AND state = ?6 AND owner_id = ?7 AND owner_epoch = ?8
                   AND terminal_outcome IS NULL",
                params![
                    RequestState::Completed.code(),
                    outcome.as_bytes().to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    lease.owner_id.to_vec(),
                    i64::try_from(lease.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
        {
            Ok(changed) => changed,
            Err(_) => {
                return Err(request_activation_rollback(
                    transaction,
                    RuntimeError::StorageUnavailable,
                )
                .await?);
            }
        };
        if changed != 1 {
            return Err(request_activation_rollback(
                transaction,
                RuntimeError::RequestOwnerConflict,
            )
            .await?);
        }
        if let Err(error) =
            sync_run_request_state_tx(&transaction, identity, RunObservationStatus::Completed).await
        {
            return Err(request_activation_rollback(transaction, error).await?);
        }
        if let Err(error) = faults.check(FaultPoint::AfterTerminalClaim) {
            return Err(request_activation_rollback(transaction, error).await?);
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RequestActivationCommit {
            capture,
            request: RequestStatus {
                identity,
                fingerprint,
                state: RequestState::Completed,
                terminal_outcome: Some(outcome),
            },
        })
    }

    /// Reads a committed table row after reopening the durable runtime.
    pub async fn committed_table_row(
        &self,
        table: &str,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        validate_table_identity(table, key)?;
        let mut rows = self
            .connection
            .query(
                "SELECT row_value FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                params![table, key.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        rows.next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .map(|row| row.get(0).map_err(|_| RuntimeError::RecoveryInvalid))
            .transpose()
    }

    /// Reads one complete committed relation in canonical key order.
    pub async fn committed_table_rows(
        &self,
        table: &str,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, RuntimeError> {
        validate_table_name(table)?;
        let mut rows = self
            .connection
            .query(
                "SELECT row_key, row_value FROM table_row
                 WHERE table_id = ?1 ORDER BY row_key",
                params![table],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut result = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            result.push((
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            ));
        }
        Ok(result)
    }

    /// Reads one durable stream checkpoint without exposing the runtime
    /// database connection or weakening the stream identity boundary.
    pub async fn stream_checkpoint(
        &self,
        key: &CheckpointKey,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        load_stream_checkpoint(&self.connection, key).await
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
        sync_stream_observation_event_tx(
            &transaction,
            &expected.key,
            StreamObservationStatus::BackingOff,
            Some(diagnostic),
        )
        .await?;
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

    /// Records a terminal runner outcome that has no delivery lease (for
    /// example finite exhaustion or cancellation before admission). The
    /// observation update is still writer-fenced and committed atomically.
    async fn complete_stream_observation(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        status: StreamObservationStatus,
    ) -> Result<(), RuntimeError> {
        debug_assert!(matches!(
            status,
            StreamObservationStatus::Completed | StreamObservationStatus::Cancelled
        ));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, writer).await?;
        sync_stream_observation_event_tx(&transaction, key, status, None).await?;
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
            sync_stream_observation_tx(&transaction, &result).await?;
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
        if source.checkpoint_key() != *key {
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
                    self.complete_stream_observation(
                        writer,
                        key,
                        StreamObservationStatus::Cancelled,
                    )
                    .await
                    .map_err(StreamStepError::Runtime)?;
                    return Ok(StreamStep::Cancelled { checkpoint });
                }
                self.record_stream_provider_failure(writer, &checkpoint, diagnostic)
                    .await
                    .map_err(StreamStepError::Runtime)?;
                if control.cancelled() {
                    self.clear_stream_provider_failure(writer, key)
                        .await
                        .map_err(StreamStepError::Runtime)?;
                    self.complete_stream_observation(
                        writer,
                        key,
                        StreamObservationStatus::Cancelled,
                    )
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
            self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
                .await
                .map_err(StreamStepError::Runtime)?;
            return Ok(StreamStep::Cancelled { checkpoint });
        }
        let expected = CheckpointPrecondition::from(&checkpoint);
        let lease = {
            if !control.acquire_admission() {
                self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
                    .await
                    .map_err(StreamStepError::Runtime)?;
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
            StreamHandlerResult::CommitTable(batch) => {
                let capture = self.capture().await.map_err(StreamStepError::Runtime)?;
                let faults = NoFault;
                let lease_for_cleanup = lease.clone();
                let result = self
                    .commit_stream_table_delivery(StreamTableDeliveryCommit {
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
                if is_cancellation_diagnostic(diagnostic) {
                    let result = self
                        .stream_backend(writer)
                        .apply_async(CommitIntent::Cancel { lease })
                        .await
                        .map_err(StreamStepError::Runtime)?;
                    return match result {
                        CommitResult::Cancelled { checkpoint, .. } => {
                            Ok(StreamStep::Cancelled { checkpoint })
                        }
                        CommitResult::Rejected(reason) => Ok(StreamStep::Rejected(reason)),
                        _ => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
                    };
                }
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
        if source.checkpoint_key() != *key {
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
                self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
                    .await
                    .map_err(StreamStepError::Runtime)?;
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
                            self.complete_stream_observation(
                                writer,
                                key,
                                StreamObservationStatus::Cancelled,
                            )
                            .await
                            .map_err(StreamStepError::Runtime)?;
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
                            self.complete_stream_observation(
                                writer,
                                key,
                                StreamObservationStatus::Cancelled,
                            )
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
                    self.complete_stream_observation(
                        writer,
                        key,
                        StreamObservationStatus::Completed,
                    )
                    .await
                    .map_err(StreamStepError::Runtime)?;
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

    async fn initialize_runtime_meta(
        connection: &Connection,
        identity: RuntimeIdentity,
        digest: [u8; 32],
    ) -> Result<(), RuntimeError> {
        let runtime_id = *Uuid::new_v4().as_bytes();
        connection.execute(
            "INSERT INTO runtime_meta (singleton, database_id, repository_id, runtime_id, generation, generation_digest)
             VALUES (1, ?1, ?2, ?3, 0, ?4) ON CONFLICT(singleton) DO NOTHING",
            params![identity.database_id.to_vec(), identity.repository_id.to_vec(), runtime_id.to_vec(), digest.to_vec()],
        ).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = connection
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
        let stored = RuntimeIdentity {
            database_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            repository_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        };
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

    /// Upgrades the private stream identity store so its partition component
    /// has the same nullable semantics as the public checkpoint and stream
    /// natural keys. Existing non-null rows retain their byte-for-byte key
    /// identifiers; only the SQL column constraint changes.
    async fn migrate_nullable_stream_partitions(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let checkpoint_partition_required =
            stream_partition_required(&transaction, "stream_checkpoint").await?;
        let failure_partition_required =
            stream_partition_required(&transaction, "stream_failure").await?;
        if checkpoint_partition_required || failure_partition_required {
            transaction
                .execute_batch(
                    "CREATE TABLE stream_checkpoint_nullable_partition (
                        key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
                        consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
                        consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
                        consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
                        consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
                        source_format TEXT NOT NULL CHECK (length(source_format) > 0),
                        source TEXT NOT NULL CHECK (length(source) > 0),
                        partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
                        partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
                        position_format TEXT NOT NULL CHECK (length(position_format) > 0),
                        version INTEGER NOT NULL CHECK (version >= 0),
                        committed_position TEXT,
                        next_fence INTEGER NOT NULL CHECK (next_fence >= 0)
                    );
                    CREATE TABLE stream_failure_nullable_partition (
                        identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
                        key_id TEXT NOT NULL CHECK (length(key_id) > 0),
                        consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
                        consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
                        consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
                        consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
                        source_format TEXT NOT NULL CHECK (length(source_format) > 0),
                        source TEXT NOT NULL CHECK (length(source) > 0),
                        partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
                        partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
                        position_format TEXT NOT NULL CHECK (length(position_format) > 0),
                        delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
                        successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
                        version INTEGER NOT NULL CHECK (version >= 0),
                        attempts INTEGER NOT NULL CHECK (attempts >= 0),
                        status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 7),
                        diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 5),
                        diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
                    );
                    INSERT INTO stream_checkpoint_nullable_partition
                        SELECT * FROM stream_checkpoint;
                    INSERT INTO stream_failure_nullable_partition
                        SELECT * FROM stream_failure;
                    DROP TABLE stream_checkpoint;
                    DROP TABLE stream_failure;
                    ALTER TABLE stream_checkpoint_nullable_partition RENAME TO stream_checkpoint;
                    ALTER TABLE stream_failure_nullable_partition RENAME TO stream_failure;",
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        let mut empty_partitions = transaction
            .query(
                "SELECT COUNT(*) FROM sys_stream_observation WHERE partition = ''",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let empty_partition_count = empty_partitions
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        if empty_partition_count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS sys_stream_observation_natural_key;
                 CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_null_natural_key
                   ON sys_stream_observation (run_id, source_identity)
                   WHERE partition IS NULL;
                 CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_present_natural_key
                   ON sys_stream_observation (run_id, source_identity, partition)
                   WHERE partition IS NOT NULL;",
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('nullable-stream-partition-v1')",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Adds the durable run observation timestamp without reinterpreting any
    /// retained request, lease, or recovery state. Older rows use their
    /// terminal instant when present, otherwise their admission instant.
    async fn migrate_observation_projection_schema(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = transaction
            .query("PRAGMA table_info(sys_run_observation)", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut columns = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            columns.insert(
                row.get::<String>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
                (),
            );
        }
        if !columns.contains_key("observed_ms") {
            transaction
                .execute(
                    "ALTER TABLE sys_run_observation
                     ADD COLUMN observed_ms INTEGER NOT NULL DEFAULT 0",
                    (),
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            transaction
                .execute(
                    "UPDATE sys_run_observation
                     SET observed_ms = COALESCE(ended_ms, started_ms)",
                    (),
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('observation-projection-v1')",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Adds the causal link from a retained stream observation to its most
    /// recent durable delivery-failure identity. Existing rows intentionally
    /// remain null: no historical failure is inferred without exact evidence.
    async fn migrate_stream_observation_failure_identity(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = transaction
            .query("PRAGMA table_info(sys_stream_observation)", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut found = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            if row
                .get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                == "last_failure_identity"
            {
                found = true;
            }
        }
        if !found {
            transaction
                .execute(
                    "ALTER TABLE sys_stream_observation ADD COLUMN last_failure_identity TEXT",
                    (),
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-observation-failure-identity-v1')",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Adds only backward-compatible ledger columns. The migration marker is
    /// transactional so an interrupted upgrade cannot look complete.
    async fn migrate_request_recovery_evidence(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let applied = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('request-recovery-evidence-v2')",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if applied == 1 {
            let mut rows = transaction
                .query("PRAGMA table_info(request_ledger)", ())
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            let mut columns = BTreeMap::new();
            while let Some(row) = rows
                .next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
            {
                let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
                columns.insert(name, ());
            }
            for (column, sql) in [
                (
                    "owner_id",
                    "ALTER TABLE request_ledger ADD COLUMN owner_id BLOB",
                ),
                (
                    "owner_epoch",
                    "ALTER TABLE request_ledger ADD COLUMN owner_epoch INTEGER",
                ),
                (
                    "effect_evidence",
                    "ALTER TABLE request_ledger ADD COLUMN effect_evidence INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "recovery_disposition",
                    "ALTER TABLE request_ledger ADD COLUMN recovery_disposition INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "controlled_transaction_proof",
                    "ALTER TABLE request_ledger ADD COLUMN controlled_transaction_proof BLOB",
                ),
                (
                    "controlled_rollback_proof",
                    "ALTER TABLE request_ledger ADD COLUMN controlled_rollback_proof BLOB",
                ),
            ] {
                if !columns.contains_key(column) {
                    transaction
                        .execute(sql, ())
                        .await
                        .map_err(|_| RuntimeError::StorageUnavailable)?;
                }
            }
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

    /// Reads the current writer lease without changing runtime state.
    ///
    /// This read does not establish that the owner is abandoned. The caller
    /// must supply that liveness proof to [`Self::takeover_lease`], whose
    /// owner-and-epoch compare-and-swap closes the race with a concurrent
    /// takeover.
    pub async fn current_lease(&self) -> Result<Option<WriterLease>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT owner_id, epoch FROM writer_lease WHERE singleton = 1",
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
        let owner_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let epoch: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let epoch = u64::try_from(epoch).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_id(owner_id)?;
        if epoch == 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(Some(WriterLease { owner_id, epoch }))
    }

    /// Atomically replaces exactly the expected abandoned writer lease.
    ///
    /// This is a compare-and-swap fence, not an abandonment detector: the
    /// caller owns the liveness proof. A changed owner or epoch returns
    /// `OwnerLost`; no blind or arbitrary steal is permitted by this method.
    pub async fn takeover_lease(
        &self,
        abandoned: WriterLease,
        replacement: [u8; 16],
    ) -> Result<WriterLease, RuntimeError> {
        validate_id(abandoned.owner_id)?;
        validate_id(replacement)?;
        if abandoned.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let changed = transaction
            .execute(
                "UPDATE writer_lease
                 SET owner_id = ?1, epoch = epoch + 1
                 WHERE singleton = 1 AND owner_id = ?2 AND epoch = ?3",
                params![
                    replacement.to_vec(),
                    abandoned.owner_id.to_vec(),
                    i64::try_from(abandoned.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
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
            .execute(
                "UPDATE stream_control
                 SET status = ?1
                 WHERE key_id IN (SELECT key_id FROM stream_pause_pending)",
                params![encode_stream_status(StreamStatus::Paused)],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO stream_control (key_id, status)
                 SELECT key_id, ?1 FROM stream_pause_pending",
                params![encode_stream_status(StreamStatus::Paused)],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute("DELETE FROM stream_pause_pending", ())
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
        transaction
            .execute(
                "UPDATE stream_failure
                 SET version = version + 1, status = ?1
                 WHERE status = ?2",
                params![
                    encode_status(FailureStatus::Skipped),
                    encode_status(FailureStatus::Replaying),
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
        let epoch = u64::try_from(epoch).map_err(|_| RuntimeError::RecoveryInvalid)?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(WriterLease {
            owner_id: replacement,
            epoch,
        })
    }

    /// Compatibility handover for callers that only have the prior owner ID.
    /// It reads the current lease, then delegates to the owner-and-epoch
    /// compare-and-swap. The caller still owns the liveness proof.
    pub async fn recover_abandoned(
        &self,
        abandoned: [u8; 16],
        replacement: [u8; 16],
    ) -> Result<WriterLease, RuntimeError> {
        validate_id(abandoned)?;
        validate_id(replacement)?;
        let expected = self.current_lease().await?;
        let Some(expected) = expected.filter(|lease| lease.owner_id == abandoned) else {
            return Err(RuntimeError::OwnerLost);
        };
        self.takeover_lease(expected, replacement).await
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
        self.commit_stream_delivery_inner(StreamDeliveryParts {
            writer,
            expected_capture,
            mutations,
            table_mutations: &[],
            next_digest,
            delivery,
            expected_stream,
            faults,
        })
        .await
    }

    /// Commits typed table mutations and the corresponding stream checkpoint
    /// in one writer-fenced transaction. Rejected or faulted deliveries leave
    /// both the table rows and stream checkpoint unchanged.
    pub async fn commit_stream_table_delivery(
        &self,
        request: StreamTableDeliveryCommit<'_>,
    ) -> Result<(CwdCapture, CommitResult), RuntimeError> {
        let StreamTableDeliveryCommit {
            writer,
            expected_capture,
            mutations,
            next_digest,
            delivery,
            expected_stream,
            faults,
        } = request;
        let encoded = mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()?;
        self.commit_stream_delivery_inner(StreamDeliveryParts {
            writer,
            expected_capture,
            mutations: &encoded,
            table_mutations: mutations,
            next_digest,
            delivery,
            expected_stream,
            faults,
        })
        .await
    }

    async fn commit_stream_delivery_inner(
        &self,
        parts: StreamDeliveryParts<'_>,
    ) -> Result<(CwdCapture, CommitResult), RuntimeError> {
        let StreamDeliveryParts {
            writer,
            expected_capture,
            mutations,
            table_mutations,
            next_digest,
            delivery,
            expected_stream,
            faults,
        } = parts;
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
        sync_stream_observation_tx(&tx, &result).await?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx).await?;
            return Ok((current, result));
        }
        for mutation in table_mutations {
            apply_table_mutation_tx(&tx, mutation).await?;
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
            table_mutations,
            next_digest,
            grant,
            faults,
        } = request;
        validate_id(writer.owner_id)?;
        validate_stream_mutations(mutations, next_digest)?;
        let encoded_table_mutations = table_mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()?;
        if !table_mutations.is_empty() && encoded_table_mutations.as_slice() != mutations {
            return Err(RuntimeError::InvalidTableMutation);
        }
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
        sync_stream_observation_tx(&tx, &result).await?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx).await?;
            return Ok((current, result));
        }
        for mutation in table_mutations {
            apply_table_mutation_tx(&tx, mutation).await?;
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
        self.fail_stream_replay_with_faults(writer, grant, diagnostic, &NoFault)
            .await
    }

    async fn fail_stream_replay_with_faults(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
        diagnostic: SafeDiagnostic,
        faults: &dyn FaultInjector,
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
        sync_stream_observation_tx(&tx, &result).await?;
        if matches!(result, CommitResult::ReplayFailed { .. }) {
            faults.check(FaultPoint::AfterReplayFailureRecord)?;
        }
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
                    table_mutations: &[],
                    next_digest: batch.next_digest,
                    grant,
                    faults: &NoFault,
                })
                .await
                .map(|(_, result)| result)
                .map_err(StreamStepError::Runtime),
            StreamHandlerResult::CommitTable(batch) => {
                let mutations = batch
                    .mutations
                    .iter()
                    .map(TableMutation::runtime_mutation)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StreamStepError::Runtime)?;
                self.commit_stream_replay(StreamReplayCommit {
                    writer,
                    expected_capture: &expected_capture,
                    mutations: &mutations,
                    table_mutations: &batch.mutations,
                    next_digest: batch.next_digest,
                    grant,
                    faults: &NoFault,
                })
                .await
                .map(|(_, result)| result)
                .map_err(StreamStepError::Runtime)
            }
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

    /// Returns only the pending mutation prefix covered by a persisted freeze.
    /// Mutations appended after the freeze are intentionally excluded.
    pub async fn pending_through(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<Vec<Mutation>, RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        let stored = self
            .frozen_intent(freeze.intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let mut rows = self
            .connection
            .query(
                "SELECT mutation_id, payload, digest FROM pending_mutation WHERE sequence <= ?1 ORDER BY sequence",
                params![upper],
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

    /// Returns the typed table operations covered by a persisted freeze.
    /// Non-table mutations fail closed; newer pending tail mutations remain
    /// outside the returned range.
    pub async fn pending_table_mutations_through(
        &self,
        freeze: &PublicationFreeze,
    ) -> Result<Vec<TableMutation>, RuntimeError> {
        self.pending_through(freeze)
            .await?
            .iter()
            .map(TableMutation::decode)
            .collect()
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
        self.connection
            .execute(
                "INSERT INTO publication_freeze \
                 (intent_id, checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest, frozen) \
                 VALUES (?1, ?2, ?3, ?4, 1) ON CONFLICT(intent_id) DO NOTHING",
                params![
                    intent_id.to_vec(),
                    i64::try_from(checkpoint.generation)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    i64::try_from(checkpoint.mutation_sequence)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    checkpoint.digest.to_vec(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
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

    /// Durably binds a freeze to one compact candidate before receipt
    /// completion. Once bound, [`Self::complete_publication`] rejects the
    /// freeze; only signed compact receipt completion may consume its prefix.
    /// Repeating the exact binding is safe after restart.
    pub async fn bind_compact_publication(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<(), RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        if pending.runtime_intent_id() != freeze.intent_id
            || pending.cleanup_watermark() != freeze.checkpoint.digest
        {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = transaction
            .query(
                "SELECT checkpoint_generation, checkpoint_mutation_sequence, checkpoint_digest, \
                 compact_watermark, compact_commit_id, compact_journal_verifier \
                 FROM publication_freeze WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let stored = Checkpoint {
            generation: u64::try_from(
                row.get::<i64>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
            mutation_sequence: u64::try_from(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
            digest: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        };
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let watermark: Option<Vec<u8>> = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let commit: Option<Vec<u8>> = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let verifier: Option<Vec<u8>> = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
        match (watermark, commit, verifier) {
            (None, None, None) => {
                transaction
                    .execute(
                        "UPDATE publication_freeze SET compact_watermark = ?1, \
                         compact_commit_id = ?2, compact_journal_verifier = ?3 \
                         WHERE intent_id = ?4",
                        params![
                            pending.cleanup_watermark().to_vec(),
                            pending.commit().as_str().as_bytes().to_vec(),
                            pending.journal_verifier().to_vec(),
                            freeze.intent_id.to_vec(),
                        ],
                    )
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
            }
            (Some(watermark), Some(commit), Some(verifier)) => {
                if fixed::<32>(watermark)? != pending.cleanup_watermark()
                    || commit != pending.commit().as_str().as_bytes()
                    || fixed::<32>(verifier)? != pending.journal_verifier()
                {
                    return Err(RuntimeError::ConflictingPublicationIntent);
                }
            }
            _ => return Err(RuntimeError::ConflictingPublicationIntent),
        }
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Atomically signs and persists a compact publication receipt with the
    /// candidate commit and deletion of only the frozen pending prefix.
    ///
    /// The returned receipt is stable across restart for one intent and
    /// candidate. The signing key is runtime-owned durable state; callers
    /// cannot supply signing or verification material.
    pub async fn complete_compact_publication(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<CompactRuntimeReceipt, RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        if pending.runtime_intent_id() != freeze.intent_id
            || pending.cleanup_watermark() != freeze.checkpoint.digest
        {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        self.bind_compact_publication(pending, freeze).await?;
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let signing_bytes = CompactRuntimeReceipt::signing_bytes(
            pending.runtime_intent_id(),
            pending.cleanup_watermark(),
            pending.commit(),
            pending.journal_verifier(),
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let signature = self
            .compact_receipt_signing_key
            .sign(&signing_bytes)
            .to_bytes();
        let receipt = CompactRuntimeReceipt::new(
            pending.runtime_intent_id(),
            pending.cleanup_watermark(),
            pending.commit().clone(),
            pending.journal_verifier(),
            signature,
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let encoded = receipt
            .encode()
            .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = tx
            .query(
                "SELECT commit_id, compact_receipt FROM publication_commit WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let commit: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if commit != pending.commit().as_str().as_bytes() {
                return Err(RuntimeError::ConflictingPublicationCommit);
            }
            let encoded: Option<Vec<u8>> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let existing = CompactRuntimeReceipt::decode(
                encoded
                    .as_deref()
                    .ok_or(RuntimeError::InvalidCompactReceipt)?,
            )
            .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
            self.verify_compact_receipt(&existing)?;
            if existing != receipt {
                return Err(RuntimeError::ConflictingPublicationIntent);
            }
            tx.commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(existing);
        }
        tx.execute(
            "INSERT INTO publication_commit (intent_id, commit_id, compact_receipt)
             VALUES (?1, ?2, ?3)",
            params![
                freeze.intent_id.to_vec(),
                pending.commit().as_str().as_bytes().to_vec(),
                encoded,
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.execute(
            "DELETE FROM pending_mutation WHERE sequence <= ?1",
            params![upper],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(receipt)
    }

    /// Records a verified ordinary publication and consumes only the frozen
    /// pending prefix. Compact publication must use
    /// [`Self::complete_compact_publication`] so its signed receipt is
    /// persisted in the same transaction.
    /// prefix. Repeating the same intent and commit is safe after restart;
    /// another commit for that intent is rejected.
    pub async fn complete_publication(
        &self,
        freeze: &PublicationFreeze,
        commit_id: &PublicationCommitId,
    ) -> Result<(), RuntimeError> {
        validate_id(freeze.intent_id)?;
        validate_digest(freeze.checkpoint.digest)?;
        let stored = self
            .frozen_intent(freeze.intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if stored != freeze.checkpoint {
            return Err(RuntimeError::ConflictingPublicationIntent);
        }
        let mut binding = self
            .connection
            .query(
                "SELECT compact_watermark, compact_commit_id, compact_journal_verifier \
                 FROM publication_freeze WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let binding = binding
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let watermark: Option<Vec<u8>> =
            binding.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let commit: Option<Vec<u8>> = binding.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let verifier: Option<Vec<u8>> =
            binding.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if watermark.is_some() || commit.is_some() || verifier.is_some() {
            return Err(RuntimeError::CompactPublicationRequired);
        }
        let upper = i64::try_from(freeze.checkpoint.mutation_sequence)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut rows = tx
            .query(
                "SELECT commit_id FROM publication_commit WHERE intent_id = ?1",
                params![freeze.intent_id.to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let existing = row
                .get::<Vec<u8>>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?;
            if existing != commit_id.as_bytes() {
                return Err(RuntimeError::ConflictingPublicationCommit);
            }
        } else {
            tx.execute(
                "INSERT INTO publication_commit (intent_id, commit_id) VALUES (?1, ?2)",
                params![freeze.intent_id.to_vec(), commit_id.as_bytes().to_vec()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        tx.execute(
            "DELETE FROM pending_mutation WHERE sequence <= ?1",
            params![upper],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
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
        ensure_session_admission_open(&tx, identity.session_id).await?;
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

    /// Starts a legacy request without durable ownership.
    ///
    /// This compatibility path is only valid before a writer lease exists;
    /// live writers must use [`Self::start_request_with_owner`]. A legacy
    /// Running row that already exists before lease acquisition remains
    /// recoverable through [`Self::recover_legacy_running_request`].
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

    /// Starts a request with durable ownership. The runtime records effect
    /// evidence separately at the transaction/effect boundary so restart
    /// recovery can fence the old activation before changing its record.
    pub async fn start_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
    ) -> Result<RequestStatus, RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Reserved {
            return Err(RuntimeError::RequestStateConflict);
        }
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, owner_id = ?2, owner_epoch = ?3,
                     effect_evidence = 0, recovery_disposition = 0,
                     controlled_transaction_proof = NULL, controlled_rollback_proof = NULL
                 WHERE session_id = ?4 AND request_id = ?5 AND fingerprint = ?6 AND state = ?7",
                params![
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Reserved.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        sync_run_request_state_tx(&tx, identity, RunObservationStatus::Running).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RequestStatus {
            identity,
            fingerprint,
            state: RequestState::Running,
            terminal_outcome: None,
        })
    }

    /// Durably records that this owner entered a request transaction that the
    /// runtime expected to be Orna-controlled. The marker is tied to the exact
    /// request fingerprint and owner, but it is not a rollback receipt: crash
    /// recovery remains uncertain until a durable transaction-boundary proof
    /// exists.
    pub async fn record_controlled_transaction(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
    ) -> Result<(), RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let marker = controlled_transaction_marker(identity, fingerprint, owner);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        if evidence.owner != Some(RequestOwner::from(owner)) {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        if evidence.effects.is_some() || evidence.controlled_marker.is_some() {
            return Err(RuntimeError::RequestStateConflict);
        }
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET effect_evidence = 1, controlled_transaction_proof = ?1
                 WHERE session_id = ?2 AND request_id = ?3 AND fingerprint = ?4
                   AND state = ?5 AND owner_id = ?6 AND owner_epoch = ?7
                   AND effect_evidence = 0 AND controlled_transaction_proof IS NULL",
                params![
                    marker.to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Records that an effect outside Orna's transaction boundary may have
    /// occurred. This is intentionally one-way and can only make recovery
    /// more conservative.
    pub async fn record_external_effect(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
    ) -> Result<(), RuntimeError> {
        self.record_effect_evidence(identity, fingerprint, owner, 2)
            .await
    }

    /// Persists evidence only after this runtime observed its own controlled
    /// table transaction fail before commit. An entry marker alone never
    /// reaches this transition, and a possible external effect keeps recovery
    /// conservative.
    async fn record_controlled_rollback_proof(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
    ) -> Result<(), RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        if evidence.owner != Some(RequestOwner::from(owner)) {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        if evidence.effects == Some(EffectEvidence::ExternalPossible) {
            tx.commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(());
        }
        if evidence.rollback_proof.is_some()
            || !matches!(
                evidence.effects,
                None | Some(EffectEvidence::ControlledTransaction)
            )
        {
            return Err(RuntimeError::RequestStateConflict);
        }
        let marker = controlled_transaction_marker(identity, fingerprint, owner);
        if evidence
            .controlled_marker
            .is_some_and(|existing| existing != marker)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let proof = controlled_rollback_proof(marker);
        let changed = tx
            .execute(
                "UPDATE request_ledger
             SET effect_evidence = 1, controlled_transaction_proof = ?1,
                 controlled_rollback_proof = ?2
             WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5
               AND state = ?6 AND owner_id = ?7 AND owner_epoch = ?8
               AND effect_evidence IN (0, 1)
               AND (controlled_transaction_proof IS NULL OR controlled_transaction_proof = ?1)
               AND controlled_rollback_proof IS NULL",
                params![
                    marker.to_vec(),
                    proof.to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    async fn record_effect_evidence(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        evidence_code: i64,
    ) -> Result<(), RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 || evidence_code != 2 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        if evidence.owner != Some(RequestOwner::from(owner)) {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        match (evidence.effects, evidence.controlled_marker) {
            (Some(EffectEvidence::ExternalPossible), None) => return Ok(()),
            (Some(_), _) => return Err(RuntimeError::RequestStateConflict),
            (None, None) => {}
            (None, Some(_)) => return Err(RuntimeError::RequestStateConflict),
        }
        let changed = tx
            .execute(
                "UPDATE request_ledger SET effect_evidence = 2
                 WHERE session_id = ?1 AND request_id = ?2 AND fingerprint = ?3
                   AND state = ?4 AND owner_id = ?5 AND owner_epoch = ?6
                   AND effect_evidence = 0 AND controlled_transaction_proof IS NULL",
                params![
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Completes a request only while the activation still owns its writer
    /// epoch. A stale activation cannot publish a terminal result.
    pub async fn complete_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request_with_owner(
            identity,
            fingerprint,
            owner,
            RequestState::Completed,
            outcome,
        )
        .await
    }

    /// Marks an observed request successful while the owner fence is current.
    pub async fn complete_observed_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.complete_request_with_owner(identity, fingerprint, owner, outcome)
            .await
    }

    /// Retains an ordinary evaluator failure as a completed request terminal
    /// while keeping the observed run distinctly `Failed`. The runtime makes
    /// no rollback assertion here; callers may only record such proof through
    /// runtime-owned transaction handling.
    pub async fn fail_observed_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        outcome: TerminalOutcome,
        diagnostic: SafeDiagnostic,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request_with_owner_and_run_status(
            identity,
            fingerprint,
            owner,
            RequestState::Completed,
            outcome,
            RunObservationStatus::Failed,
            Some(diagnostic),
        )
        .await
    }

    /// Cancels a request only while the activation still owns its writer
    /// epoch. A stale activation cannot publish a terminal cancellation.
    pub async fn cancel_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.transition_request_with_owner(
            identity,
            fingerprint,
            owner,
            RequestState::Cancelled,
            outcome,
        )
        .await
    }

    /// Marks an observed request cancelled while preserving cancellation as a
    /// distinct run terminal state.
    pub async fn cancel_observed_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        self.cancel_request_with_owner(identity, fingerprint, owner, outcome)
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
        _outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        // This compatibility wrapper has no durable owner fence, so it must
        // not turn a running reservation into an orphaned recovery outcome.
        // Callers that have proved owner loss use `recover_running_request`.
        let status = self.request_status(identity, fingerprint).await?;
        match status {
            Some(status) if status.state == RequestState::Running => {
                Err(RuntimeError::RequestOwnerConflict)
            }
            Some(_) => Err(RuntimeError::RequestStateConflict),
            None => Err(RuntimeError::RequestUnknown),
        }
    }

    /// Fences a known-lost activation and retains a non-replayable orphaned
    /// result. A caller must first replace the old writer lease with `fence`;
    /// the comparison with `lost_owner` prevents recovery from changing a
    /// request still owned by a different activation.
    pub async fn recover_running_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        lost_owner: RequestOwner,
        fence: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RecoveredRequest, RuntimeError> {
        self.recover_running_request_with_outcomes(
            identity,
            fingerprint,
            lost_owner,
            fence,
            outcome.clone(),
            outcome,
        )
        .await
    }

    /// Fences a known-lost activation and atomically retains the terminal
    /// payload matching its durable recovery disposition. The caller supplies
    /// opaque payloads for the two distinct recovery cases; the fenced owner
    /// transaction chooses and stores exactly one after validating evidence.
    pub async fn recover_running_request_with_outcomes(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        lost_owner: RequestOwner,
        fence: WriterLease,
        rollback_proven_outcome: TerminalOutcome,
        external_effects_uncertain_outcome: TerminalOutcome,
    ) -> Result<RecoveredRequest, RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(lost_owner.owner_id)?;
        validate_id(fence.owner_id)?;
        if lost_owner.epoch == 0 || fence.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, fence).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        validate_request_execution_evidence(&current, evidence)?;
        if evidence.owner != Some(lost_owner) || lost_owner == RequestOwner::from(fence) {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        let disposition = if evidence.effects == Some(EffectEvidence::ControlledTransaction)
            && evidence.controlled_marker.is_some()
            && evidence.rollback_proof.is_some()
        {
            RecoveryDisposition::RollbackProven
        } else {
            RecoveryDisposition::ExternalEffectsUncertain
        };
        let outcome = match disposition {
            RecoveryDisposition::RollbackProven => rollback_proven_outcome,
            RecoveryDisposition::ExternalEffectsUncertain => external_effects_uncertain_outcome,
        };
        let retained_effect_evidence = if disposition == RecoveryDisposition::RollbackProven {
            1
        } else {
            0
        };
        let retained_marker = if disposition == RecoveryDisposition::RollbackProven {
            evidence.controlled_marker.map(|value| value.to_vec())
        } else {
            None
        };
        let retained_rollback_proof = if disposition == RecoveryDisposition::RollbackProven {
            evidence.rollback_proof.map(|value| value.to_vec())
        } else {
            None
        };
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, owner_id = NULL,
                     owner_epoch = NULL, effect_evidence = ?3,
                     recovery_disposition = ?4, controlled_transaction_proof = ?5,
                     controlled_rollback_proof = ?6
                 WHERE session_id = ?7 AND request_id = ?8 AND fingerprint = ?9
                   AND state = ?10 AND owner_id = ?11 AND owner_epoch = ?12",
                params![
                    RequestState::Orphaned.code(),
                    outcome.as_bytes().to_vec(),
                    retained_effect_evidence,
                    disposition.code(),
                    retained_marker,
                    retained_rollback_proof,
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    lost_owner.owner_id.to_vec(),
                    i64::try_from(lost_owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        sync_run_request_state_tx(&tx, identity, RunObservationStatus::Orphaned).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RecoveredRequest {
            status: RequestStatus {
                identity,
                fingerprint,
                state: RequestState::Orphaned,
                terminal_outcome: Some(outcome),
            },
            disposition,
        })
    }

    /// Recovers a pre-evidence Running row after a writer-lease takeover.
    /// Because the old row has no owner or effect proof, the result is always
    /// retained as externally uncertain. Epoch one is rejected: an initial
    /// lease is not evidence that the legacy owner was lost.
    pub async fn recover_legacy_running_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        fence: WriterLease,
        outcome: TerminalOutcome,
    ) -> Result<RecoveredRequest, RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(fence.owner_id)?;
        if fence.epoch <= 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, fence).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Running {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        validate_request_execution_evidence(&current, evidence)?;
        if evidence.owner.is_some()
            || evidence.effects.is_some()
            || evidence.controlled_marker.is_some()
            || evidence.disposition.is_some()
        {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, recovery_disposition = 2
                 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5
                   AND state = ?6 AND owner_id IS NULL AND owner_epoch IS NULL
                   AND effect_evidence = 0 AND controlled_transaction_proof IS NULL
                   AND controlled_rollback_proof IS NULL
                   AND recovery_disposition = 0",
                params![
                    RequestState::Orphaned.code(),
                    outcome.as_bytes().to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        sync_run_request_state_tx(&tx, identity, RunObservationStatus::Orphaned).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RecoveredRequest {
            status: RequestStatus {
                identity,
                fingerprint,
                state: RequestState::Orphaned,
                terminal_outcome: Some(outcome),
            },
            disposition: RecoveryDisposition::ExternalEffectsUncertain,
        })
    }

    /// Returns the recovery distinction retained for an orphaned request.
    /// This is runtime metadata; protocol state remains unchanged until the
    /// live adapter maps it to its existing diagnostics/result representation.
    pub async fn request_recovery_disposition(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<Option<RecoveryDisposition>, RuntimeError> {
        let Some(status) = self.request_status(identity, fingerprint).await? else {
            return Ok(None);
        };
        let evidence = request_execution_evidence_tx(&self.connection, identity).await?;
        validate_request_execution_evidence(&status, evidence)?;
        Ok(evidence.disposition)
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

    /// Enumerates durable Running requests for a recovery worker. This is a
    /// read-only operation: it never schedules, replays, or changes a
    /// request. Pair each returned identity with [`Self::request_owner`] and
    /// one of the fenced recovery methods.
    pub async fn running_requests(&self) -> Result<Vec<RequestStatus>, RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, state, terminal_outcome,
                        owner_id, owner_epoch, effect_evidence, recovery_disposition,
                        controlled_transaction_proof, controlled_rollback_proof
                 FROM request_ledger
                 WHERE state = ?1
                 ORDER BY session_id, request_id",
                params![RequestState::Running.code()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut requests = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let status = decode_request_status(&row).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let evidence = decode_request_execution_evidence(&row, 5)?;
            validate_request_execution_evidence(&status, evidence)?;
            requests.push(status);
        }
        Ok(requests)
    }

    /// Enumerates every non-terminal request owned by one durable session.
    ///
    /// This is intentionally broader than [`Self::running_requests`]: a
    /// `Reserved` record is unfinished work too. A session-ending owner must
    /// account for it before reporting orderly termination, even though a
    /// recovered reservation is never authorization to execute it.
    pub async fn unfinished_session_requests(
        &self,
        session_id: [u8; 16],
    ) -> Result<Vec<RequestStatus>, RuntimeError> {
        validate_id(session_id)?;
        let mut rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, state, terminal_outcome,
                        owner_id, owner_epoch, effect_evidence, recovery_disposition,
                        controlled_transaction_proof, controlled_rollback_proof
                 FROM request_ledger
                 WHERE session_id = ?1 AND state IN (?2, ?3)
                 ORDER BY request_id",
                params![
                    session_id.to_vec(),
                    RequestState::Reserved.code(),
                    RequestState::Running.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut requests = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let status = decode_request_status(&row).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let evidence = decode_request_execution_evidence(&row, 5)?;
            validate_request_execution_evidence(&status, evidence)?;
            requests.push(status);
        }
        Ok(requests)
    }

    /// Returns the active owner recorded for a Running request. `None` means
    /// a validated legacy Running row or a non-running request; it never
    /// authorizes recovery by itself.
    pub async fn request_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<Option<RequestOwner>, RuntimeError> {
        let Some(status) = self.request_status(identity, fingerprint).await? else {
            return Ok(None);
        };
        let evidence = request_execution_evidence_tx(&self.connection, identity).await?;
        validate_request_execution_evidence(&status, evidence)?;
        Ok((status.state == RequestState::Running)
            .then_some(evidence.owner)
            .flatten())
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
        if next.is_terminal()
            && matches!(
                current.state,
                RequestState::Reserved | RequestState::Running
            )
        {
            let evidence = request_execution_evidence_tx(&tx, identity).await?;
            if evidence.owner.is_some()
                || tx
                    .query("SELECT 1 FROM writer_lease WHERE singleton = 1", ())
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?
                    .next()
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?
                    .is_some()
            {
                return Err(RuntimeError::RequestOwnerConflict);
            }
        }
        if !allowed.contains(&current.state) || current.state.is_terminal() {
            return Err(RuntimeError::RequestStateConflict);
        }
        if next == RequestState::Running
            && tx
                .query("SELECT 1 FROM writer_lease WHERE singleton = 1", ())
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .next()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?
                .is_some()
        {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        let outcome_bytes = terminal_outcome
            .as_ref()
            .map(|outcome| outcome.as_bytes().to_vec());
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, owner_id = NULL,
                     owner_epoch = NULL, effect_evidence = 0,
                     recovery_disposition = 0, controlled_transaction_proof = NULL,
                     controlled_rollback_proof = NULL
                 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5 AND state = ?6",
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
        let observation_status = match next {
            RequestState::Running => RunObservationStatus::Running,
            RequestState::Completed => RunObservationStatus::Completed,
            RequestState::Cancelled => RunObservationStatus::Cancelled,
            RequestState::Orphaned => RunObservationStatus::Orphaned,
            RequestState::Reserved => return Err(RuntimeError::RecoveryInvalid),
        };
        sync_run_request_state_tx(&tx, identity, observation_status).await?;
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

    async fn transition_request_with_owner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        next: RequestState,
        terminal_outcome: TerminalOutcome,
    ) -> Result<RequestStatus, RuntimeError> {
        let observation_status = match next {
            RequestState::Completed => RunObservationStatus::Completed,
            RequestState::Cancelled => RunObservationStatus::Cancelled,
            _ => return Err(RuntimeError::RecoveryInvalid),
        };
        self.transition_request_with_owner_and_run_status(
            identity,
            fingerprint,
            owner,
            next,
            terminal_outcome,
            observation_status,
            None,
        )
        .await
    }

    async fn transition_request_with_owner_and_run_status(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        next: RequestState,
        terminal_outcome: TerminalOutcome,
        observation_status: RunObservationStatus,
        diagnostic: Option<SafeDiagnostic>,
    ) -> Result<RequestStatus, RuntimeError> {
        validate_request_identity(identity)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 || !next.is_terminal() {
            return Err(RuntimeError::RequestStateConflict);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if !matches!(
            current.state,
            RequestState::Reserved | RequestState::Running
        ) {
            return Err(RuntimeError::RequestStateConflict);
        }
        let evidence = request_execution_evidence_tx(&tx, identity).await?;
        validate_request_execution_evidence(&current, evidence)?;
        let owner_request = match current.state {
            RequestState::Running if evidence.owner == Some(RequestOwner::from(owner)) => true,
            RequestState::Reserved if evidence.owner.is_none() => false,
            _ => return Err(RuntimeError::RequestOwnerConflict),
        };
        let changed = if owner_request {
            tx.execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, owner_id = NULL,
                     owner_epoch = NULL, effect_evidence = 0,
                     recovery_disposition = 0, controlled_transaction_proof = NULL,
                     controlled_rollback_proof = NULL
                 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5
                   AND state = ?6 AND owner_id = ?7 AND owner_epoch = ?8",
                params![
                    next.code(),
                    terminal_outcome.as_bytes().to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        } else {
            tx.execute(
                "UPDATE request_ledger
                 SET state = ?1, terminal_outcome = ?2, owner_id = NULL,
                     owner_epoch = NULL, effect_evidence = 0,
                     recovery_disposition = 0, controlled_transaction_proof = NULL,
                     controlled_rollback_proof = NULL
                 WHERE session_id = ?3 AND request_id = ?4 AND fingerprint = ?5
                   AND state = ?6 AND owner_id IS NULL AND owner_epoch IS NULL
                   AND effect_evidence = 0 AND recovery_disposition = 0
                   AND controlled_transaction_proof IS NULL
                   AND controlled_rollback_proof IS NULL",
                params![
                    next.code(),
                    terminal_outcome.as_bytes().to_vec(),
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Reserved.code(),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        };
        if changed != 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        sync_run_request_state_with_diagnostic_tx(&tx, identity, observation_status, diagnostic)
            .await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(RequestStatus {
            identity,
            fingerprint,
            state: next,
            terminal_outcome: Some(terminal_outcome),
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

    /// Reconstructs a persisted publication freeze for restart recovery.
    /// Unknown intents are reported as recovery-invalid rather than inferred
    /// from the current checkpoint or pending mutation tail.
    pub async fn publication_freeze(
        &self,
        intent_id: [u8; 16],
    ) -> Result<PublicationFreeze, RuntimeError> {
        validate_id(intent_id)?;
        let checkpoint = self
            .frozen_intent(intent_id)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        Ok(PublicationFreeze {
            intent_id,
            checkpoint,
        })
    }

    fn verify_compact_receipt(&self, receipt: &CompactRuntimeReceipt) -> Result<(), RuntimeError> {
        let payload = CompactRuntimeReceipt::signing_bytes(
            receipt.runtime_intent_id(),
            receipt.cleanup_watermark(),
            receipt.commit(),
            receipt.journal_verifier(),
        )
        .map_err(|_| RuntimeError::InvalidCompactReceipt)?;
        let key =
            VerifyingKey::from_bytes(&self.compact_receipt_signing_key.verifying_key().to_bytes())
                .map_err(|_| RuntimeError::CompactReceiptKeyMismatch)?;
        key.verify(&payload, &Signature::from_bytes(receipt.signature()))
            .map_err(|_| RuntimeError::InvalidCompactReceipt)
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
        self.validate_session_deletions().await?;
        let mut request_rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, state, terminal_outcome,
                        owner_id, owner_epoch, effect_evidence, recovery_disposition,
                        controlled_transaction_proof, controlled_rollback_proof
                 FROM request_ledger",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = request_rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let status = decode_request_status(&row).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let evidence = decode_request_execution_evidence(&row, 5)?;
            validate_request_execution_evidence(&status, evidence)?;
        }
        Ok(())
    }

    async fn validate_session_deletions(&self) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT session_id, owner_id, owner_epoch, state FROM session_deletion",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            validate_id(fixed(
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)?;
            validate_id(fixed(
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)?;
            let epoch: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if epoch <= 0 {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let state: i64 = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if !matches!(state, SESSION_DELETION_CLOSING | SESSION_DELETION_CLOSED) {
                return Err(RuntimeError::RecoveryInvalid);
            }
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
                "SELECT checkpoint.generation, checkpoint.digest, checkpoint.mutation_sequence, pending_mutation.sequence, publication_commit.intent_id \
                 FROM checkpoint LEFT JOIN pending_mutation ON pending_mutation.sequence = checkpoint.mutation_sequence \
                 LEFT JOIN publication_freeze ON publication_freeze.checkpoint_generation = checkpoint.generation \
                 AND publication_freeze.checkpoint_mutation_sequence = checkpoint.mutation_sequence \
                 AND publication_freeze.checkpoint_digest = checkpoint.digest \
                 LEFT JOIN publication_commit ON publication_commit.intent_id = publication_freeze.intent_id \
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
            let completed: Option<Vec<u8>> =
                row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if sequence == 0
                || sequence <= previous_sequence
                || (referenced.and_then(|value| u64::try_from(value).ok()) != Some(sequence)
                    && completed.is_none())
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
                 publication_freeze.frozen, checkpoint.generation, \
                 publication_freeze.compact_watermark, publication_freeze.compact_commit_id, \
                 publication_freeze.compact_journal_verifier \
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
            let watermark: Option<Vec<u8>> =
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let commit: Option<Vec<u8>> = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let verifier: Option<Vec<u8>> =
                row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if generation == 0 || sequence == 0 || frozen != 1 || referenced.is_none() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            match (watermark, commit, verifier) {
                (None, None, None) => {}
                (Some(watermark), Some(commit), Some(verifier)) => {
                    if fixed::<32>(watermark)?
                        != fixed::<32>(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?
                        || PublicationCommitId::new(commit).is_err()
                        || fixed::<32>(verifier).is_err()
                    {
                        return Err(RuntimeError::RecoveryInvalid);
                    }
                }
                _ => return Err(RuntimeError::RecoveryInvalid),
            }
        }

        let mut publications = self
            .connection
            .query(
                "SELECT publication_commit.intent_id, publication_commit.commit_id, publication_commit.compact_receipt, publication_freeze.intent_id, \
                 publication_freeze.compact_watermark, publication_freeze.compact_commit_id, \
                 publication_freeze.compact_journal_verifier, publication_freeze.checkpoint_digest \
                 FROM publication_commit LEFT JOIN publication_freeze \
                 ON publication_freeze.intent_id = publication_commit.intent_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = publications
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let intent_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            validate_id(intent_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let commit = PublicationCommitId::new(
                row.get::<Vec<u8>>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
            let receipt: Option<Vec<u8>> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let watermark: Option<Vec<u8>> =
                row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let bound_commit: Option<Vec<u8>> =
                row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let verifier: Option<Vec<u8>> =
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let freeze_digest: [u8; 32] =
                fixed(row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            match (receipt, watermark, bound_commit, verifier) {
                (Some(receipt), Some(watermark), Some(bound_commit), Some(verifier)) => {
                    let receipt = CompactRuntimeReceipt::decode(&receipt)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?;
                    if receipt.runtime_intent_id() != intent_id
                        || receipt.commit().as_str().as_bytes() != commit.as_bytes()
                        || receipt.cleanup_watermark() != fixed::<32>(watermark)?
                        || receipt.cleanup_watermark() != freeze_digest
                        || receipt.commit().as_str().as_bytes() != bound_commit
                        || receipt.journal_verifier() != fixed::<32>(verifier)?
                    {
                        return Err(RuntimeError::RecoveryInvalid);
                    }
                    self.verify_compact_receipt(&receipt)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?;
                }
                (None, None, None, None) => {}
                _ => return Err(RuntimeError::RecoveryInvalid),
            }
            validate_id(fixed(
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
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
        let mut pending = self
            .connection
            .query(
                "SELECT pause.key_id, checkpoint.key_id
                 FROM stream_pause_pending AS pause
                 LEFT JOIN stream_checkpoint AS checkpoint
                   ON checkpoint.key_id = pause.key_id",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        while let Some(row) = pending
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        {
            let key_id: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let referenced: Option<String> =
                row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if key_id.is_empty() || referenced.as_deref() != Some(key_id.as_str()) {
                return Err(RuntimeError::RecoveryInvalid);
            }
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
        Box::pin(async move { apply_stream_intent(self.state, self.lease, None, intent).await })
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
    let prefix = if key.partition.is_some() {
        "checkpoint/v1"
    } else {
        "checkpoint/v1/null"
    };
    format!(
        "{prefix}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        key.consumer.canonical(),
        key.source_format.as_str(),
        key.source.as_str(),
        key.partition_format.as_str(),
        key.partition.as_ref().map(Component::as_str).unwrap_or(""),
        key.position_format.as_str(),
        key.consumer.root.as_str(),
        key.consumer.function.as_str(),
        key.consumer.binding.as_str(),
    )
}

fn stream_identity_id(identity: &FailureIdentity) -> String {
    identity.0.canonical()
}

impl RunObservationStatus {
    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Orphaned
        )
    }
}

const fn run_status_code(status: RunObservationStatus) -> i64 {
    match status {
        RunObservationStatus::Starting => 1,
        RunObservationStatus::Running => 2,
        RunObservationStatus::Completed => 3,
        RunObservationStatus::Failed => 4,
        RunObservationStatus::Cancelled => 5,
        RunObservationStatus::Orphaned => 6,
    }
}

fn decode_run_status(value: i64) -> Result<RunObservationStatus, RuntimeError> {
    match value {
        1 => Ok(RunObservationStatus::Starting),
        2 => Ok(RunObservationStatus::Running),
        3 => Ok(RunObservationStatus::Completed),
        4 => Ok(RunObservationStatus::Failed),
        5 => Ok(RunObservationStatus::Cancelled),
        6 => Ok(RunObservationStatus::Orphaned),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

const fn stream_observation_status_code(status: StreamObservationStatus) -> i64 {
    match status {
        StreamObservationStatus::Starting => 1,
        StreamObservationStatus::Running => 2,
        StreamObservationStatus::Paused => 3,
        StreamObservationStatus::BackingOff => 4,
        StreamObservationStatus::Completed => 5,
        StreamObservationStatus::Failed => 6,
        StreamObservationStatus::Cancelled => 7,
        StreamObservationStatus::Orphaned => 8,
    }
}

fn decode_stream_observation_status(value: i64) -> Result<StreamObservationStatus, RuntimeError> {
    match value {
        1 => Ok(StreamObservationStatus::Starting),
        2 => Ok(StreamObservationStatus::Running),
        3 => Ok(StreamObservationStatus::Paused),
        4 => Ok(StreamObservationStatus::BackingOff),
        5 => Ok(StreamObservationStatus::Completed),
        6 => Ok(StreamObservationStatus::Failed),
        7 => Ok(StreamObservationStatus::Cancelled),
        8 => Ok(StreamObservationStatus::Orphaned),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn validate_observation_text(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty() || value.len() > 16 * 1024 || value.bytes().any(|byte| byte == 0) {
        return Err(RuntimeError::InvalidIdentity);
    }
    Ok(())
}

fn now_ms() -> Result<i64, RuntimeError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
            .as_millis(),
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)
}

fn bigint_to_i64(value: &BigInt) -> Result<i64, RuntimeError> {
    value
        .to_string()
        .parse()
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

fn encode_capture(capture: &CwdCapture) -> Result<Vec<u8>, RuntimeError> {
    Value::new(capture.snapshot().raw())
        .and_then(|value| value.encode())
        .map_err(|_| RuntimeError::RecoveryInvalid)
}

fn decode_capture(bytes: Vec<u8>, digest: [u8; 32]) -> Result<CwdCapture, RuntimeError> {
    let value = Value::decode(&bytes).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let snapshot = Snapshot::decode(value.raw()).map_err(|_| RuntimeError::RecoveryInvalid)?;
    CwdCapture::new(snapshot, digest).map_err(|_| RuntimeError::RecoveryInvalid)
}

fn decode_consumer_identity(value: &str) -> Result<ConsumerIdentity, RuntimeError> {
    let parts = value.split('|').collect::<Vec<_>>();
    if parts.len() != 5 || parts[0] != "consumer/v1" {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(ConsumerIdentity {
        principal: decode_component(parts[1].to_owned())?,
        root: decode_component(parts[2].to_owned())?,
        function: decode_component(parts[3].to_owned())?,
        binding: decode_component(parts[4].to_owned())?,
    })
}

async fn load_run_observations(
    connection: &Connection,
    capture: &CwdCapture,
) -> Result<Vec<RunObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT run_id FROM sys_run_observation ORDER BY started_ms, run_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut observations = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let id = RunObservationId(fixed(
            row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?);
        observations.push(
            load_run_observation_tx(connection, id, capture)
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?,
        );
    }
    Ok(observations)
}

async fn load_run_observation_tx(
    connection: &Connection,
    id: RunObservationId,
    capture: &CwdCapture,
) -> Result<Option<RunObservation>, RuntimeError> {
    let mut rows = connection.query("SELECT session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class FROM sys_run_observation WHERE run_id = ?1", params![id.0.to_vec()]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let snapshot_bytes: Vec<u8> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let snapshot_digest = fixed(row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation = row
        .get::<i64>(9)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let snapshot = decode_capture(snapshot_bytes, snapshot_digest)?;
    if snapshot.runtime_id() != runtime_id || bigint_to_i64(snapshot.generation())? != generation {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let code: Option<i64> = row.get(15).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let class: Option<i64> = row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let diagnostic = match (code, class) {
        (Some(code), Some(class)) => Some(SafeDiagnostic {
            code: decode_code(code)?,
            class: decode_class(class)?,
        }),
        (None, None) => None,
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let status = decode_run_status(row.get(13).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    Ok(Some(RunObservation {
        id,
        request: RequestIdentity {
            session_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            request_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        },
        consumer_identity: decode_consumer_identity(&row_text(&row, 2)?)?,
        function: row_text(&row, 3)?,
        source_identity: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
        snapshot,
        runtime_id,
        invocation_id: fixed(row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        started_ms: row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
        ended_ms: row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
        observed_ms: row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?,
        status,
        runtime_generation: generation,
        checkpoint_count: decode_u64(row.get(14).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        diagnostic,
        live: runtime_id == capture.runtime_id()
            && generation == bigint_to_i64(capture.generation())?
            && !status.is_terminal(),
    }))
}

async fn load_stream_observations(
    connection: &Connection,
    capture: &CwdCapture,
) -> Result<Vec<StreamObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT stream_id FROM sys_stream_observation ORDER BY observed_ms, stream_id",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut observations = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let id = StreamObservationId(fixed(
            row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?);
        observations.push(
            load_stream_observation_tx(connection, id, capture)
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?,
        );
    }
    Ok(observations)
}

async fn stream_partition_required(
    connection: &libsql::Transaction,
    table: &str,
) -> Result<bool, RuntimeError> {
    let statement = match table {
        "stream_checkpoint" => "PRAGMA table_info(stream_checkpoint)",
        "stream_failure" => "PRAGMA table_info(stream_failure)",
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let mut rows = connection
        .query(statement, ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if name == "partition" {
            let required: i64 = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            return Ok(required != 0);
        }
    }
    Err(RuntimeError::RecoveryInvalid)
}

async fn load_stream_observation_tx(
    connection: &Connection,
    id: StreamObservationId,
    capture: &CwdCapture,
) -> Result<Option<StreamObservation>, RuntimeError> {
    let mut rows = connection.query("SELECT observation.run_id, observation.producer, observation.consumer_name, observation.status, observation.items_seen, observation.items_committed, observation.items_failed, observation.diagnostic_code, observation.diagnostic_class, checkpoint.consumer_principal, checkpoint.consumer_root, checkpoint.consumer_function, checkpoint.consumer_binding, checkpoint.source_format, checkpoint.source, checkpoint.partition_format, checkpoint.partition, checkpoint.position_format, run.runtime_id, run.runtime_generation, run.status, observation.consumer_identity, observation.source_identity, observation.partition, observation.last_item_ms, observation.observed_ms, observation.last_failure_identity FROM sys_stream_observation AS observation JOIN stream_checkpoint AS checkpoint ON checkpoint.key_id = observation.checkpoint_key_id JOIN sys_run_observation AS run ON run.run_id = observation.run_id WHERE observation.stream_id = ?1", params![id.0.to_vec()]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    let parent_run_id = RunObservationId(fixed(
        row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    let parent_run = load_run_observation_tx(connection, parent_run_id, capture)
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let parent_capture = parent_run.snapshot.clone();
    let checkpoint = CheckpointKey {
        consumer: ConsumerIdentity {
            principal: decode_component(row_text(&row, 9)?)?,
            root: decode_component(row_text(&row, 10)?)?,
            function: decode_component(row_text(&row, 11)?)?,
            binding: decode_component(row_text(&row, 12)?)?,
        },
        source_format: decode_component(row_text(&row, 13)?)?,
        source: decode_component(row_text(&row, 14)?)?,
        partition_format: decode_component(row_text(&row, 15)?)?,
        partition: decode_optional_component(
            row.get(16).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        position_format: decode_component(row_text(&row, 17)?)?,
    };
    let code: Option<i64> = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let class: Option<i64> = row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let diagnostic = match (code, class) {
        (Some(code), Some(class)) => Some(SafeDiagnostic {
            code: decode_code(code)?,
            class: decode_class(class)?,
        }),
        (None, None) => None,
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let status =
        decode_stream_observation_status(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let run_status = decode_run_status(row.get(20).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(18).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation: i64 = row.get(19).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if runtime_id != parent_run.runtime_id || generation != parent_run.runtime_generation {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let partition: Option<String> = row.get(23).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let checkpoint_partition = checkpoint
        .partition
        .as_ref()
        .map(|value| value.as_str().to_owned());
    if partition != checkpoint_partition {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let checkpoint_reference = checkpoint_reference(
        parent_capture.database_id(),
        parent_capture.snapshot().clone(),
        checkpoint.consumer.canonical(),
        checkpoint.source.as_str().to_owned(),
        checkpoint_partition.clone(),
    )
    .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_checkpoint_reference(checkpoint_reference.as_row_ref().clone(), &parent_capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let last_failure_identity: Option<String> =
        row.get(26).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let last_failure = match last_failure_identity {
        Some(identity) => Some(
            load_stream_observation_failure_reference(
                connection,
                &checkpoint,
                &parent_capture,
                identity,
            )
            .await?,
        ),
        None => None,
    };
    Ok(Some(StreamObservation {
        id,
        run: parent_run_id,
        producer: row_text(&row, 1)?,
        consumer: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
        consumer_identity: decode_consumer_identity(&row_text(&row, 21)?)?,
        source_identity: row_text(&row, 22)?,
        partition: partition.clone(),
        partition_evidence: StreamPartitionEvidence {
            observation: partition,
            checkpoint: checkpoint_partition,
        },
        parent_capture,
        checkpoint_reference,
        checkpoint,
        last_failure,
        status,
        items_seen: decode_u64(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        items_committed: decode_u64(row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        items_failed: decode_u64(row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        last_item_ms: row.get(24).map_err(|_| RuntimeError::RecoveryInvalid)?,
        diagnostic,
        observed_ms: row.get(25).map_err(|_| RuntimeError::RecoveryInvalid)?,
        live: runtime_id == capture.runtime_id()
            && generation == bigint_to_i64(capture.generation())?
            && !run_status.is_terminal()
            && !matches!(
                status,
                StreamObservationStatus::Completed
                    | StreamObservationStatus::Failed
                    | StreamObservationStatus::Cancelled
                    | StreamObservationStatus::Orphaned
            ),
    }))
}

async fn load_stream_observation_failure_reference(
    connection: &Connection,
    checkpoint: &CheckpointKey,
    capture: &CwdCapture,
    identity: String,
) -> Result<FailureRef, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, delivery_position, successor_position FROM stream_failure WHERE identity_id = ?1",
            params![identity.clone()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    if row_text(&row, 0)? != stream_key_id(checkpoint)
        || row_text(&row, 1)? != checkpoint.consumer.principal.as_str()
        || row_text(&row, 2)? != checkpoint.consumer.root.as_str()
        || row_text(&row, 3)? != checkpoint.consumer.function.as_str()
        || row_text(&row, 4)? != checkpoint.consumer.binding.as_str()
        || row_text(&row, 5)? != checkpoint.source_format.as_str()
        || row_text(&row, 6)? != checkpoint.source.as_str()
        || row_text(&row, 7)? != checkpoint.partition_format.as_str()
        || row
            .get::<Option<String>>(8)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
            != checkpoint
                .partition
                .as_ref()
                .map(|value| value.as_str().to_owned())
        || row_text(&row, 9)? != checkpoint.position_format.as_str()
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let failure = FailureIdentity(DeliveryIdentity {
        consumer: checkpoint.consumer.clone(),
        source_format: checkpoint.source_format.clone(),
        source: checkpoint.source.clone(),
        partition_format: checkpoint.partition_format.clone(),
        partition: checkpoint.partition.clone(),
        position_format: checkpoint.position_format.clone(),
        position: decode_position(row_text(&row, 10)?)?,
        successor: decode_position(row_text(&row, 11)?)?,
    });
    if stream_identity_id(&failure) != identity {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let reference = failure_reference(
        capture.database_id(),
        capture.snapshot().clone(),
        checkpoint.consumer.canonical(),
        checkpoint.source.as_str().to_owned(),
        checkpoint
            .partition
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        checkpoint.position_format.as_str().to_owned(),
        failure.0.position.token.as_str().to_owned(),
    )
    .map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_failure_reference(reference.as_row_ref().clone(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)
}

async fn sync_stream_observation_tx(
    connection: &Connection,
    result: &CommitResult,
) -> Result<(), RuntimeError> {
    let (
        key,
        status,
        seen,
        committed,
        failed,
        diagnostic,
        checkpoint,
        last_failure,
        advances_checkpoint,
    ) = match result {
        CommitResult::Acquired { lease } => (
            lease.delivery.checkpoint_key(),
            Some(StreamObservationStatus::Running),
            1_i64,
            0,
            0,
            None,
            None,
            None,
            false,
        ),
        CommitResult::CheckpointAdvanced { checkpoint } => (
            checkpoint.key.clone(),
            Some(StreamObservationStatus::Running),
            0,
            1,
            0,
            None,
            Some(checkpoint.version),
            None,
            true,
        ),
        CommitResult::Failed { failure } => (
            failure.identity.0.checkpoint_key(),
            Some(StreamObservationStatus::Failed),
            0,
            0,
            1,
            Some(failure.diagnostic),
            None,
            Some(stream_identity_id(&failure.identity)),
            false,
        ),
        CommitResult::RetryScheduled { failure } => (
            failure.identity.0.checkpoint_key(),
            Some(StreamObservationStatus::BackingOff),
            0,
            0,
            0,
            Some(failure.diagnostic),
            None,
            Some(stream_identity_id(&failure.identity)),
            false,
        ),
        CommitResult::ReplayGranted { grant } => (
            grant.failure.0.checkpoint_key(),
            Some(StreamObservationStatus::BackingOff),
            0,
            0,
            0,
            None,
            None,
            Some(stream_identity_id(&grant.failure)),
            false,
        ),
        CommitResult::ReplayCompleted { failure } | CommitResult::Resolved { failure } => (
            failure.identity.0.checkpoint_key(),
            Some(StreamObservationStatus::Running),
            0,
            0,
            0,
            None,
            None,
            Some(stream_identity_id(&failure.identity)),
            false,
        ),
        CommitResult::ReplayFailed { failure } => (
            failure.identity.0.checkpoint_key(),
            Some(StreamObservationStatus::Failed),
            0,
            0,
            0,
            Some(failure.diagnostic),
            None,
            Some(stream_identity_id(&failure.identity)),
            false,
        ),
        CommitResult::Cancelled { checkpoint, .. } => (
            checkpoint.key.clone(),
            Some(StreamObservationStatus::Cancelled),
            0,
            0,
            0,
            None,
            Some(checkpoint.version),
            None,
            false,
        ),
        CommitResult::CheckpointReset { checkpoint } => (
            checkpoint.key.clone(),
            Some(StreamObservationStatus::Running),
            0,
            0,
            0,
            None,
            Some(checkpoint.version),
            None,
            false,
        ),
        CommitResult::StreamStatusChanged { state, .. } => (
            state.key.clone(),
            Some(match state.status {
                StreamStatus::Running => StreamObservationStatus::Running,
                StreamStatus::Paused => StreamObservationStatus::Paused,
            }),
            0,
            0,
            0,
            None,
            None,
            None,
            false,
        ),
        CommitResult::PausePending { state, .. } => (
            state.key.clone(),
            Some(StreamObservationStatus::Running),
            0,
            0,
            0,
            None,
            None,
            None,
            false,
        ),
        _ => return Ok(()),
    };
    let changed = sync_stream_observation_update_tx(
        connection,
        &key,
        StreamObservationUpdate {
            status,
            seen,
            committed,
            failed,
            diagnostic,
            checkpoint,
            last_failure,
        },
    )
    .await?;
    if advances_checkpoint && changed != 0 {
        connection
            .execute(
                "UPDATE sys_run_observation SET checkpoint_count = checkpoint_count + 1
                 WHERE run_id = (SELECT run_id FROM sys_stream_observation WHERE checkpoint_key_id = ?1)",
                params![stream_key_id(&key)],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
}

async fn sync_stream_observation_event_tx(
    connection: &Connection,
    key: &CheckpointKey,
    status: StreamObservationStatus,
    diagnostic: Option<SafeDiagnostic>,
) -> Result<(), RuntimeError> {
    sync_stream_observation_update_tx(
        connection,
        key,
        StreamObservationUpdate {
            status: Some(status),
            seen: 0,
            committed: 0,
            failed: 0,
            diagnostic,
            checkpoint: None,
            last_failure: None,
        },
    )
    .await?;
    Ok(())
}

struct StreamObservationUpdate {
    status: Option<StreamObservationStatus>,
    seen: i64,
    committed: i64,
    failed: i64,
    diagnostic: Option<SafeDiagnostic>,
    checkpoint: Option<u64>,
    last_failure: Option<String>,
}

async fn sync_stream_observation_update_tx(
    connection: &Connection,
    key: &CheckpointKey,
    update: StreamObservationUpdate,
) -> Result<u64, RuntimeError> {
    let code = update.diagnostic.map(|value| encode_code(value.code));
    let class = update.diagnostic.map(|value| encode_class(value.class));
    let changed = connection.execute("UPDATE sys_stream_observation SET status = COALESCE(?2, status), items_seen = items_seen + ?3, items_committed = items_committed + ?4, items_failed = items_failed + ?5, checkpoint_version = COALESCE(?6, checkpoint_version), last_failure_identity = COALESCE(?7, last_failure_identity), last_item_ms = CASE WHEN ?3 + ?4 + ?5 > 0 THEN ?8 ELSE last_item_ms END, diagnostic_code = COALESCE(?9, diagnostic_code), diagnostic_class = COALESCE(?10, diagnostic_class), observed_ms = ?8 WHERE checkpoint_key_id = ?1", params![stream_key_id(key), update.status.map(stream_observation_status_code), update.seen, update.committed, update.failed, update.checkpoint.map(|value| i64::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)).transpose()?, update.last_failure, now_ms()?, code, class]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(changed)
}

async fn sync_run_request_state_tx(
    connection: &Connection,
    identity: RequestIdentity,
    status: RunObservationStatus,
) -> Result<(), RuntimeError> {
    sync_run_request_state_with_diagnostic_tx(connection, identity, status, None).await
}

async fn sync_run_request_state_with_diagnostic_tx(
    connection: &Connection,
    identity: RequestIdentity,
    status: RunObservationStatus,
    diagnostic: Option<SafeDiagnostic>,
) -> Result<(), RuntimeError> {
    let now = now_ms()?;
    let code = diagnostic.map(|value| encode_code(value.code));
    let class = diagnostic.map(|value| encode_class(value.class));
    connection.execute("UPDATE sys_run_observation SET status = ?1, ended_ms = CASE WHEN ?2 THEN ?3 ELSE ended_ms END, observed_ms = ?3, diagnostic_code = COALESCE(?4, diagnostic_code), diagnostic_class = COALESCE(?5, diagnostic_class) WHERE session_id = ?6 AND request_id = ?7 AND status IN (?8, ?9)", params![run_status_code(status), status.is_terminal(), now, code, class, identity.session_id.to_vec(), identity.request_id.to_vec(), run_status_code(RunObservationStatus::Starting), run_status_code(RunObservationStatus::Running)]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    if status.is_terminal() {
        connection.execute("UPDATE sys_stream_observation SET status = ?1, observed_ms = ?2 WHERE run_id IN (SELECT run_id FROM sys_run_observation WHERE session_id = ?3 AND request_id = ?4) AND status IN (?5, ?6, ?7, ?8)", params![stream_observation_status_code(StreamObservationStatus::Orphaned), now, identity.session_id.to_vec(), identity.request_id.to_vec(), stream_observation_status_code(StreamObservationStatus::Starting), stream_observation_status_code(StreamObservationStatus::Running), stream_observation_status_code(StreamObservationStatus::Paused), stream_observation_status_code(StreamObservationStatus::BackingOff)]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
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

fn decode_optional_component(value: Option<String>) -> Result<Option<Component>, RuntimeError> {
    value.map(decode_component).transpose()
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
                key.partition
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
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

async fn load_stream_pause_reason(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<Option<String>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT reason FROM stream_pause_reason WHERE key_id = ?1",
            params![stream_key_id(key)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    rows.next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .map(|row| row.get(0).map_err(|_| RuntimeError::RecoveryInvalid))
        .transpose()
}

async fn store_stream_pause_reason(
    connection: &Connection,
    key: &CheckpointKey,
    reason: &str,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO stream_pause_reason (key_id, reason) VALUES (?1, ?2)
             ON CONFLICT(key_id) DO UPDATE SET reason = excluded.reason",
            params![stream_key_id(key), reason],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

fn stream_pause_changed(result: &CommitResult) -> bool {
    matches!(
        result,
        CommitResult::StreamStatusChanged { changed: true, .. }
            | CommitResult::PausePending { changed: true, .. }
    )
}

fn stream_pause_key(result: &CommitResult) -> Result<&CheckpointKey, RuntimeError> {
    match result {
        CommitResult::StreamStatusChanged { state, .. }
        | CommitResult::PausePending { state, .. } => Ok(&state.key),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn stream_administration_outcome(
    result: CommitResult,
) -> Result<StreamAdministrationOutcome, RuntimeError> {
    match result {
        CommitResult::StreamStatusChanged { state, changed }
            if state.status == StreamStatus::Paused =>
        {
            Ok(StreamAdministrationOutcome::Paused { changed })
        }
        CommitResult::PausePending { changed, .. } => {
            Ok(StreamAdministrationOutcome::PausePending { changed })
        }
        CommitResult::Rejected(RejectReason::StreamBusy) => Ok(StreamAdministrationOutcome::Busy),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

async fn stream_pause_pending(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM stream_pause_pending WHERE key_id = ?1",
            params![stream_key_id(key)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some())
}

/// Completes a previously admitted pause once its sole delivery lease has
/// reached a terminal transaction boundary. The pending record and visible
/// status change share that transaction with the lease release.
async fn publish_pending_stream_pause(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<(), RuntimeError> {
    let changed = connection
        .execute(
            "DELETE FROM stream_pause_pending WHERE key_id = ?1",
            params![stream_key_id(key)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if changed != 0 {
        store_stream_status(connection, key, StreamStatus::Paused).await?;
    }
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
        partition: decode_optional_component(
            row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
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
    expected_capture: Option<&CwdCapture>,
    intent: CommitIntent,
) -> Result<CommitResult, RuntimeError> {
    let transaction = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    state.require_owner(&transaction, lease).await?;
    if let Some(expected_capture) = expected_capture {
        let current_capture = capture_tx(&transaction).await?;
        if &current_capture != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current_capture),
            });
        }
    }
    if matches!(intent, CommitIntent::Fail { .. }) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let result = apply_stream_intent_tx(&transaction, intent).await;
    match result {
        Ok(result) => {
            sync_stream_observation_tx(&transaction, &result).await?;
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
            if load_stream_status(connection, &key).await? == StreamStatus::Paused {
                return Ok(CommitResult::StreamStatusChanged {
                    state: StreamState {
                        key,
                        status: StreamStatus::Paused,
                    },
                    changed: false,
                });
            }
            if load_stream_lease(connection, &key).await?.is_some() {
                let changed = connection
                    .execute(
                        "INSERT OR IGNORE INTO stream_pause_pending (key_id) VALUES (?1)",
                        params![stream_key_id(&key)],
                    )
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?
                    != 0;
                return Ok(CommitResult::PausePending {
                    state: StreamState {
                        key,
                        status: StreamStatus::Running,
                    },
                    changed,
                });
            }
            let changed = true;
            store_stream_status(connection, &key, StreamStatus::Paused).await?;
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
                        connection
                            .execute(
                                "UPDATE stream_lease
                                 SET successor_position = ?3
                                 WHERE key_id = ?1 AND fence = ?2",
                                params![
                                    stream_key_id(&key),
                                    i64::try_from(stored.fence)
                                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                                    delivery.successor.token.as_str().to_owned(),
                                ],
                            )
                            .await
                            .map_err(|_| RuntimeError::StorageUnavailable)?;
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
                && (load_stream_status(connection, &key).await? == StreamStatus::Paused
                    || stream_pause_pending(connection, &key).await?)
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
                        attempts = stream_failure.attempts
                            + CASE WHEN stream_failure.status = ?17 THEN 0 ELSE 1 END,
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
                        delivery
                            .partition
                            .as_ref()
                            .map(|value| value.as_str().to_owned()),
                        delivery.position_format.as_str().to_owned(),
                        delivery.position.token.as_str().to_owned(),
                        delivery.successor.token.as_str().to_owned(),
                        encode_status(FailureStatus::Failed),
                        encode_code(diagnostic.code),
                        encode_class(diagnostic.class),
                        encode_status(FailureStatus::Retrying),
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            publish_pending_stream_pause(connection, &key).await?;
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
                    "UPDATE stream_failure SET version = version + 1, attempts = attempts + 1, status = ?2
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
            publish_pending_stream_pause(connection, &key).await?;
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
            publish_pending_stream_pause(connection, &key).await?;
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
            publish_pending_stream_pause(connection, &key).await?;
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

async fn session_deletion_record(
    connection: &Connection,
    session_id: [u8; 16],
) -> Result<Option<SessionDeletionRecord>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT owner_id, owner_epoch, state
             FROM session_deletion WHERE session_id = ?1",
            params![session_id.to_vec()],
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
    let owner_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let owner_epoch = u64::try_from(
        row.get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let state = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if !matches!(state, SESSION_DELETION_CLOSING | SESSION_DELETION_CLOSED) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(SessionDeletionRecord {
        owner: WriterLease {
            owner_id,
            epoch: owner_epoch,
        },
        state,
    }))
}

async fn ensure_session_admission_open(
    connection: &Connection,
    session_id: [u8; 16],
) -> Result<(), RuntimeError> {
    if session_deletion_record(connection, session_id)
        .await?
        .is_some()
    {
        return Err(RuntimeError::SessionClosed);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectEvidence {
    ControlledTransaction,
    ExternalPossible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequestExecutionEvidence {
    owner: Option<RequestOwner>,
    effects: Option<EffectEvidence>,
    controlled_marker: Option<[u8; 32]>,
    rollback_proof: Option<[u8; 32]>,
    disposition: Option<RecoveryDisposition>,
}

async fn request_execution_evidence_tx(
    connection: &Connection,
    identity: RequestIdentity,
) -> Result<RequestExecutionEvidence, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT owner_id, owner_epoch, effect_evidence, recovery_disposition,
                    controlled_transaction_proof, controlled_rollback_proof
             FROM request_ledger WHERE session_id = ?1 AND request_id = ?2",
            params![identity.session_id.to_vec(), identity.request_id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RequestUnknown)?;
    decode_request_execution_evidence(&row, 0)
}

fn decode_request_execution_evidence(
    row: &libsql::Row,
    offset: i32,
) -> Result<RequestExecutionEvidence, RuntimeError> {
    let owner_id: Option<Vec<u8>> = row.get(offset).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let owner_epoch: Option<i64> = row
        .get(offset + 1)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let owner = match (owner_id, owner_epoch) {
        (Some(id), Some(epoch)) => Some(RequestOwner {
            owner_id: fixed(id)?,
            epoch: u64::try_from(epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
        }),
        (None, None) => None,
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let effects = match row
        .get::<i64>(offset + 2)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
    {
        0 => None,
        1 => Some(EffectEvidence::ControlledTransaction),
        2 => Some(EffectEvidence::ExternalPossible),
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let controlled_marker = row
        .get::<Option<Vec<u8>>>(offset + 4)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(fixed)
        .transpose()?;
    let rollback_proof = row
        .get::<Option<Vec<u8>>>(offset + 5)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(fixed)
        .transpose()?;
    let disposition = match row
        .get::<i64>(offset + 3)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
    {
        0 => None,
        1 => Some(RecoveryDisposition::RollbackProven),
        2 => Some(RecoveryDisposition::ExternalEffectsUncertain),
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    Ok(RequestExecutionEvidence {
        owner,
        effects,
        controlled_marker,
        rollback_proof,
        disposition,
    })
}

fn validate_request_execution_evidence(
    status: &RequestStatus,
    evidence: RequestExecutionEvidence,
) -> Result<(), RuntimeError> {
    if let Some(owner) = evidence.owner {
        validate_id(owner.owner_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if owner.epoch == 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    let marker_matches_owner = match evidence.controlled_marker {
        Some(marker) => match evidence.owner {
            Some(owner) => {
                marker
                    == controlled_transaction_marker(
                        status.identity,
                        status.fingerprint,
                        WriterLease {
                            owner_id: owner.owner_id,
                            epoch: owner.epoch,
                        },
                    )
            }
            // A terminal rollback receipt deliberately retains the paired
            // marker/proof after the active owner has been fenced away.
            None => {
                status.state == RequestState::Orphaned
                    && evidence.disposition == Some(RecoveryDisposition::RollbackProven)
            }
        },
        None => true,
    };
    if !marker_matches_owner {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if evidence.controlled_marker.is_some()
        != (evidence.effects == Some(EffectEvidence::ControlledTransaction))
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if evidence.disposition.is_some() && status.state != RequestState::Orphaned {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let rollback_proof_matches = evidence.controlled_marker.is_some_and(|marker| {
        evidence.rollback_proof == Some(controlled_rollback_proof(marker))
            || [
                FaultPoint::BeforeTableWrite,
                FaultPoint::AfterTableWrite,
                FaultPoint::AfterMutation,
                FaultPoint::AfterCheckpoint,
                FaultPoint::AfterCapture,
                FaultPoint::BeforeTerminalClaim,
                FaultPoint::AfterTerminalClaim,
            ]
            .into_iter()
            .any(|point| {
                evidence.rollback_proof == Some(legacy_controlled_rollback_proof(marker, point))
            })
    });
    if evidence.rollback_proof.is_some() && !rollback_proof_matches {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if evidence.rollback_proof.is_some()
        && evidence.effects != Some(EffectEvidence::ControlledTransaction)
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if evidence.disposition == Some(RecoveryDisposition::RollbackProven)
        && !(status.state == RequestState::Orphaned
            && evidence.owner.is_none()
            && evidence.effects == Some(EffectEvidence::ControlledTransaction)
            && evidence.controlled_marker.is_some()
            && evidence.rollback_proof.is_some()
            && rollback_proof_matches)
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if evidence.disposition == Some(RecoveryDisposition::ExternalEffectsUncertain)
        && status.state != RequestState::Orphaned
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let legacy_running = status.state == RequestState::Running
        && evidence.owner.is_none()
        && evidence.effects.is_none()
        && evidence.controlled_marker.is_none()
        && evidence.rollback_proof.is_none()
        && evidence.disposition.is_none();
    let owned_running = status.state == RequestState::Running && evidence.owner.is_some();
    if status.state == RequestState::Reserved
        && (evidence.owner.is_some()
            || evidence.effects.is_some()
            || evidence.controlled_marker.is_some()
            || evidence.rollback_proof.is_some()
            || evidence.disposition.is_some())
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if owned_running
        && evidence.effects == Some(EffectEvidence::ControlledTransaction)
        && evidence.controlled_marker.is_none()
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if owned_running && evidence.disposition.is_some() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if status.state == RequestState::Running && !legacy_running && !owned_running {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if matches!(
        status.state,
        RequestState::Completed | RequestState::Cancelled
    ) && (evidence.owner.is_some()
        || evidence.effects.is_some()
        || evidence.controlled_marker.is_some()
        || evidence.rollback_proof.is_some()
        || evidence.disposition.is_some())
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    if status.state == RequestState::Orphaned {
        let documented_legacy = evidence.owner.is_none()
            && evidence.effects.is_none()
            && evidence.controlled_marker.is_none()
            && evidence.rollback_proof.is_none()
            && evidence.disposition.is_none();
        let recovered_uncertain = evidence.disposition
            == Some(RecoveryDisposition::ExternalEffectsUncertain)
            && evidence.owner.is_none()
            && evidence.effects.is_none()
            && evidence.controlled_marker.is_none()
            && evidence.rollback_proof.is_none();
        let recovered_rollback = evidence.disposition == Some(RecoveryDisposition::RollbackProven)
            && evidence.owner.is_none()
            && evidence.effects == Some(EffectEvidence::ControlledTransaction)
            && evidence.controlled_marker.is_some()
            && evidence.rollback_proof.is_some()
            && rollback_proof_matches;
        if !(documented_legacy || recovered_uncertain || recovered_rollback) {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    Ok(())
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

fn opaque_reference_key(id: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(id.to_vec())))
}

fn row_reference_raw(reference: &RowRef) -> OvbRaw {
    OvbRaw::Tag(
        60010,
        Box::new(OvbRaw::Array(vec![
            opaque_reference_key(reference.database_id),
            opaque_reference_key(reference.table_id),
            reference.key.clone(),
            reference.snapshot.raw(),
        ])),
    )
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

fn validate_table_identity(table: &str, key: &[u8]) -> Result<(), RuntimeError> {
    validate_table_name(table)?;
    if key.is_empty() || key.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

fn validate_table_name(table: &str) -> Result<(), RuntimeError> {
    if table.is_empty() || table.len() > MAX_TABLE_MUTATION_BYTES || table.contains('\0') {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

fn validate_table_mutation(
    id: [u8; 16],
    table: &str,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<(), RuntimeError> {
    validate_id(id)?;
    validate_table_identity(table, key)?;
    if value.is_some_and(|value| value.len() > MAX_TABLE_MUTATION_BYTES) {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(())
}

fn encode_table_mutation(mutation: &TableMutation) -> Result<Vec<u8>, RuntimeError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"ORNA-TABLE-MUTATION\0");
    append_length_prefixed(&mut payload, mutation.table.as_bytes())?;
    append_length_prefixed(&mut payload, &mutation.key)?;
    match &mutation.value {
        Some(value) => {
            payload.push(1);
            append_length_prefixed(&mut payload, value)?;
        }
        None => payload.push(0),
    }
    if payload.len() > MAX_TABLE_MUTATION_BYTES {
        return Err(RuntimeError::InvalidTableMutation);
    }
    Ok(payload)
}

fn append_length_prefixed(target: &mut Vec<u8>, value: &[u8]) -> Result<(), RuntimeError> {
    target.extend_from_slice(
        &u32::try_from(value.len())
            .map_err(|_| RuntimeError::InvalidTableMutation)?
            .to_be_bytes(),
    );
    target.extend_from_slice(value);
    Ok(())
}

fn read_length_prefixed<'a>(
    payload: &'a [u8],
    cursor: &mut usize,
) -> Result<&'a [u8], RuntimeError> {
    let length = payload
        .get(*cursor..(*cursor).saturating_add(4))
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    *cursor = cursor
        .checked_add(4)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let end = (*cursor)
        .checked_add(usize::try_from(length).map_err(|_| RuntimeError::InvalidTableMutation)?)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    let bytes = payload
        .get(*cursor..end)
        .ok_or(RuntimeError::InvalidTableMutation)?;
    *cursor = end;
    Ok(bytes)
}

async fn apply_table_mutation_tx(
    connection: &Connection,
    mutation: &TableMutation,
) -> Result<(), RuntimeError> {
    match &mutation.value {
        Some(value) => {
            let digest: [u8; 32] = Sha256::digest(value).into();
            connection
                .execute(
                    "INSERT INTO table_row (table_id, row_key, row_value, row_digest)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(table_id, row_key) DO UPDATE SET
                       row_value = excluded.row_value,
                       row_digest = excluded.row_digest",
                    params![
                        mutation.table.clone(),
                        mutation.key.clone(),
                        value.clone(),
                        digest.to_vec()
                    ],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        None => {
            connection
                .execute(
                    "DELETE FROM table_row WHERE table_id = ?1 AND row_key = ?2",
                    params![mutation.table.clone(), mutation.key.clone()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
    }
    Ok(())
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

async fn migrate_compact_receipt_schema(connection: &Connection) -> Result<(), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut rows = transaction
        .query("PRAGMA table_info(runtime_meta)", ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut runtime_meta_columns = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        runtime_meta_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    for (column, statement) in [
        (
            "compact_receipt_seed",
            "ALTER TABLE runtime_meta ADD COLUMN compact_receipt_seed BLOB",
        ),
        (
            "compact_receipt_public_key",
            "ALTER TABLE runtime_meta ADD COLUMN compact_receipt_public_key BLOB",
        ),
    ] {
        if !runtime_meta_columns.contains_key(column) {
            transaction
                .execute(statement, ())
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
    }
    let mut rows = transaction
        .query("PRAGMA table_info(publication_commit)", ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut publication_columns = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        publication_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    if !publication_columns.contains_key("compact_receipt") {
        transaction
            .execute(
                "ALTER TABLE publication_commit ADD COLUMN compact_receipt BLOB",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    let mut rows = transaction
        .query("PRAGMA table_info(publication_freeze)", ())
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut freeze_columns = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        freeze_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    for (column, statement) in [
        (
            "compact_watermark",
            "ALTER TABLE publication_freeze ADD COLUMN compact_watermark BLOB",
        ),
        (
            "compact_commit_id",
            "ALTER TABLE publication_freeze ADD COLUMN compact_commit_id BLOB",
        ),
        (
            "compact_journal_verifier",
            "ALTER TABLE publication_freeze ADD COLUMN compact_journal_verifier BLOB",
        ),
    ] {
        if !freeze_columns.contains_key(column) {
            transaction
                .execute(statement, ())
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_schema_migration (migration)
             VALUES ('compact-runtime-receipt-v1')",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    transaction
        .commit()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)
}

async fn initialize_compact_receipt_key(
    connection: &Connection,
) -> Result<(SigningKey, [u8; 32]), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut rows = transaction
        .query(
            "SELECT compact_receipt_seed, compact_receipt_public_key
             FROM runtime_meta WHERE singleton = 1",
            (),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let seed: Option<Vec<u8>> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let public_key: Option<Vec<u8>> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let (key, public_key) = match (seed, public_key) {
        (Some(seed), Some(public_key)) => {
            let seed = fixed(seed)?;
            let public_key = fixed(public_key)?;
            let key = SigningKey::from_bytes(&seed);
            if key.verifying_key().to_bytes() != public_key {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            (key, public_key)
        }
        (None, None) => {
            let seed = compact_receipt_seed();
            let key = SigningKey::from_bytes(&seed);
            let public_key = key.verifying_key().to_bytes();
            let changed = transaction
                .execute(
                    "UPDATE runtime_meta
                     SET compact_receipt_seed = ?1, compact_receipt_public_key = ?2
                     WHERE singleton = 1
                       AND compact_receipt_seed IS NULL
                       AND compact_receipt_public_key IS NULL",
                    params![seed.to_vec(), public_key.to_vec()],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            if changed != 1 {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            (key, public_key)
        }
        _ => return Err(RuntimeError::CompactReceiptKeyMismatch),
    };
    transaction
        .commit()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok((key, public_key))
}

fn compact_receipt_seed() -> [u8; 32] {
    let mut seed = [0_u8; 32];
    seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    seed
}

fn initialize_compact_receipt_public_key(
    path: &Path,
    expected: [u8; 32],
) -> Result<(), RuntimeError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(&expected)
                .and_then(|()| file.sync_all())
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| RuntimeError::StorageUnavailable)?;
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != 32
            {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            let actual = fs::read(path).map_err(|_| RuntimeError::StorageUnavailable)?;
            if actual.as_slice() != expected {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            Ok(())
        }
        Err(_) => Err(RuntimeError::StorageUnavailable),
    }
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
fn validate_writer_lease(value: WriterLease) -> Result<(), RuntimeError> {
    validate_id(value.owner_id)?;
    if value.epoch == 0 {
        return Err(RuntimeError::InvalidIdentity);
    }
    Ok(())
}
fn controlled_transaction_marker(
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    owner: WriterLease,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.runtime.controlled-transaction.v1");
    digest.update(identity.session_id);
    digest.update(identity.request_id);
    digest.update(fingerprint);
    digest.update(owner.owner_id);
    digest.update(owner.epoch.to_be_bytes());
    digest.finalize().into()
}
async fn request_activation_rollback(
    transaction: libsql::Transaction,
    error: RuntimeError,
) -> Result<RequestActivationTransactionError, RuntimeError> {
    transaction
        .rollback()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(RequestActivationTransactionError::RolledBack(error))
}

fn controlled_rollback_proof(marker: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.runtime.controlled-rollback.v2");
    digest.update(marker);
    digest.finalize().into()
}

fn legacy_controlled_rollback_proof(marker: [u8; 32], point: FaultPoint) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.runtime.controlled-rollback.v1");
    digest.update(marker);
    digest.update([match point {
        FaultPoint::BeforeTableWrite => 1,
        FaultPoint::AfterTableWrite => 2,
        FaultPoint::AfterMutation => 3,
        FaultPoint::AfterCheckpoint => 4,
        FaultPoint::AfterCapture => 5,
        FaultPoint::AfterFailureRecord => 6,
        FaultPoint::AfterFailurePayload => 7,
        FaultPoint::AfterReplayFailureRecord => 8,
        FaultPoint::BeforeTerminalClaim => 9,
        FaultPoint::AfterTerminalClaim => 10,
    }]);
    digest.finalize().into()
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
    fn table_mutation(id_value: u8, key: u8, value: Option<u8>) -> TableMutation {
        TableMutation::new(
            id(id_value),
            "books",
            vec![key],
            value.map(|value| vec![value]),
        )
        .unwrap()
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
        git(temp.path(), &["config", "commit.gpgsign", "false"]);
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

    #[tokio::test]
    async fn typed_table_activation_is_reopenable_and_deletes_atomically() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &context,
                &[table_mutation(5, 1, Some(9)), table_mutation(6, 2, Some(8))],
                digest(6),
                &NoFault,
            )
            .await
            .unwrap();
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        assert_eq!(
            state.committed_table_rows("books").await.unwrap(),
            vec![(vec![1], vec![9]), (vec![2], vec![8])]
        );
        drop(state);

        let state = open_state(&repo).await;
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        assert_eq!(
            state.committed_table_rows("books").await.unwrap(),
            vec![(vec![1], vec![9]), (vec![2], vec![8])]
        );
        let lease = state.recover_abandoned(id(4), id(7)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &context,
                &[table_mutation(8, 1, None)],
                digest(10),
                &NoFault,
            )
            .await
            .unwrap();
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn table_activation_snapshot_pins_context_and_declared_rows() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &context,
                &[table_mutation(5, 2, Some(8)), table_mutation(6, 1, Some(9))],
                digest(6),
                &NoFault,
            )
            .await
            .unwrap();

        let snapshot = state
            .begin_table_activation(&["books", "unused", "books"])
            .await
            .unwrap();
        assert_eq!(snapshot.context().capture().generation(), &BigInt::from(1));
        assert_eq!(
            snapshot.table_rows().get("books"),
            Some(&vec![(vec![1], vec![9]), (vec![2], vec![8])])
        );
        assert_eq!(snapshot.table_rows().get("unused"), Some(&Vec::new()));
        assert_eq!(snapshot.table_rows().len(), 2);
    }

    #[tokio::test]
    async fn table_activation_snapshot_remains_immutable_after_later_commit() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &context,
                &[table_mutation(5, 2, Some(8)), table_mutation(6, 1, Some(9))],
                digest(6),
                &NoFault,
            )
            .await
            .unwrap();

        let snapshot = state.begin_table_activation(&["books"]).await.unwrap();
        let later_context = state.begin_activation().await.unwrap();
        state
            .commit_table_activation(
                lease,
                &later_context,
                &[
                    table_mutation(7, 1, Some(10)),
                    table_mutation(8, 3, Some(11)),
                ],
                digest(9),
                &NoFault,
            )
            .await
            .unwrap();

        assert_eq!(snapshot.context().capture().generation(), &BigInt::from(1));
        assert_eq!(
            snapshot.table_rows().get("books"),
            Some(&vec![(vec![1], vec![9]), (vec![2], vec![8])])
        );

        let fresh = state.begin_table_activation(&["books"]).await.unwrap();
        assert_eq!(fresh.context().capture().generation(), &BigInt::from(2));
        assert_eq!(
            fresh.table_rows().get("books"),
            Some(&vec![
                (vec![1], vec![10]),
                (vec![2], vec![8]),
                (vec![3], vec![11]),
            ])
        );
    }

    #[tokio::test]
    async fn typed_pending_mutations_round_trip_through_the_public_decoder() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        let expected = table_mutation(5, 1, Some(9));
        state
            .commit_table_activation(
                lease,
                &context,
                std::slice::from_ref(&expected),
                digest(6),
                &NoFault,
            )
            .await
            .unwrap();
        let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
        let freeze = state.freeze(id(7), &checkpoint).await.unwrap();
        assert_eq!(
            state
                .pending_table_mutations_through(&freeze)
                .await
                .unwrap(),
            vec![expected]
        );
    }

    #[tokio::test]
    async fn typed_table_activation_rolls_back_rows_with_runtime_faults() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let context = state.begin_activation().await.unwrap();
        for point in [
            FaultPoint::AfterMutation,
            FaultPoint::AfterCheckpoint,
            FaultPoint::AfterCapture,
        ] {
            assert_eq!(
                state
                    .commit_table_activation(
                        lease,
                        &context,
                        &[table_mutation(point as u8 + 10, point as u8 + 1, Some(7))],
                        digest(11),
                        &Fail(point),
                    )
                    .await,
                Err(RuntimeError::FaultInjected(point))
            );
            assert_eq!(
                state
                    .committed_table_row("books", &[point as u8 + 1])
                    .await
                    .unwrap(),
                None
            );
            assert!(state.pending().await.unwrap().is_empty());
            assert_eq!(state.latest_checkpoint().await.unwrap(), None);
            assert_eq!(state.capture().await.unwrap(), context.capture().clone());
        }
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
            partition: Some(Component::new("partition").unwrap()),
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

        fn checkpoint_key(&self) -> CheckpointKey {
            self.key.clone()
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

        fn checkpoint_key(&self) -> CheckpointKey {
            self.key.clone()
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

    struct DefaultPayloadSource {
        key: CheckpointKey,
        item: Option<StreamItem>,
        polls: usize,
    }

    impl StreamSource for DefaultPayloadSource {
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

        fn checkpoint_key(&self) -> CheckpointKey {
            self.key.clone()
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

        fn checkpoint_key(&self) -> CheckpointKey {
            self.key.clone()
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

        fn checkpoint_key(&self) -> CheckpointKey {
            self.key.clone()
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

    struct TableCommitHandler {
        calls: usize,
    }

    impl StreamHandler for TableCommitHandler {
        fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
            self.calls += 1;
            StreamHandlerResult::CommitTable(StreamTableMutationBatch {
                mutations: vec![table_mutation(5, 1, Some(9))],
                next_digest: digest(9),
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
        assert_eq!(mismatch_handler.calls, 0);
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);
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
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);

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
    async fn stream_runner_routes_typed_table_delivery_atomically() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("typed:one", "typed:two");
        let key = delivery.checkpoint_key();
        let mut source = TestSource {
            key: key.clone(),
            item: Some(StreamItem {
                delivery,
                payload: vec![1, 2, 3],
            }),
            polls: 0,
        };
        let mut handler = TableCommitHandler { calls: 0 };

        assert!(matches!(
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await
                .unwrap(),
            StreamStep::Committed {
                checkpoint: StreamCheckpoint { version: 1, .. }
            }
        ));
        assert_eq!(handler.calls, 1);
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("typed delivery checkpoint")
                .digest,
            digest(9)
        );
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
    async fn durable_stream_backend_reopens_with_nullable_checkpoint_and_failure_state() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let mut delivery = stream_delivery("one", "two");
        delivery.partition = None;
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
            assert_eq!(retry.attempts, 2);
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
        assert_eq!(failure.attempts, 2);
    }

    #[tokio::test]
    async fn retry_admission_accepts_a_provider_successor_revision() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let original = stream_delivery("retry-revision", "retry-original-next");
        let revised = stream_delivery("retry-revision", "retry-revised-next");
        let expected = orna_stream_v1::CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let mut stream = state.stream_backend(writer);
        let original_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: original.clone(),
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
                original_lease,
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
        let revised_lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: revised,
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected retry admission result: {other:?}"),
        };
        let advanced = match stream
            .apply_async(CommitIntent::Complete {
                lease: revised_lease,
                expected,
            })
            .await
            .unwrap()
        {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            other => panic!("unexpected retry completion result: {other:?}"),
        };
        assert_eq!(
            advanced.committed,
            Some(Position {
                token: Component::new("retry-revised-next").unwrap(),
            })
        );
        let succeeded = stream
            .failure_async(&failed.identity)
            .await
            .unwrap()
            .expect("stable failure");
        assert_eq!(succeeded.identity, failed.identity);
        assert_eq!(succeeded.attempts, retry.attempts);
        assert_eq!(succeeded.status, FailureStatus::Succeeded);
    }

    #[tokio::test]
    async fn durable_pause_waits_for_delivery_and_survives_owner_recovery() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let first = stream_delivery("pause-first", "pause-after-first");
        let key = first.checkpoint_key();
        let first_checkpoint;
        {
            let mut stream = state.stream_backend(writer);
            first_checkpoint = stream.checkpoint_async(&key).await.unwrap();
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: first.clone(),
                    expected: CheckpointPrecondition::from(&first_checkpoint),
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
                    .apply_async(CommitIntent::Pause { key: key.clone() })
                    .await
                    .unwrap(),
                CommitResult::PausePending {
                    state: StreamState {
                        status: StreamStatus::Running,
                        ..
                    },
                    changed: true,
                }
            ));
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Complete {
                        lease,
                        expected: CheckpointPrecondition::from(&first_checkpoint),
                    })
                    .await
                    .unwrap(),
                CommitResult::CheckpointAdvanced { .. }
            ));
            let checkpoint = stream.checkpoint_async(&key).await.unwrap();
            assert_eq!(
                stream
                    .apply_async(CommitIntent::Acquire {
                        delivery: stream_delivery("pause-second", "pause-after-second"),
                        expected: CheckpointPrecondition::from(&checkpoint),
                        purpose: LeasePurpose::Deliver,
                    })
                    .await
                    .unwrap(),
                CommitResult::Rejected(RejectReason::StreamPaused)
            );
            assert!(matches!(
                stream
                    .apply_async(CommitIntent::Resume { key: key.clone() })
                    .await
                    .unwrap(),
                CommitResult::StreamStatusChanged { changed: true, .. }
            ));
        }

        let active = {
            let mut stream = state.stream_backend(writer);
            let checkpoint = stream.checkpoint_async(&key).await.unwrap();
            let lease = match stream
                .apply_async(CommitIntent::Acquire {
                    delivery: stream_delivery("pause-recovery", "pause-after-recovery"),
                    expected: CheckpointPrecondition::from(&checkpoint),
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
                    .apply_async(CommitIntent::Pause { key: key.clone() })
                    .await
                    .unwrap(),
                CommitResult::PausePending { changed: true, .. }
            ));
            lease
        };
        let replacement = state.takeover_lease(writer, id(5)).await.unwrap();
        let mut stream = state.stream_backend(replacement);
        let checkpoint = stream.checkpoint_async(&key).await.unwrap();
        assert_eq!(checkpoint.committed, Some(first.successor));
        assert_eq!(
            stream
                .apply_async(CommitIntent::Acquire {
                    delivery: active.delivery,
                    expected: CheckpointPrecondition::from(&checkpoint),
                    purpose: LeasePurpose::Deliver,
                })
                .await
                .unwrap(),
            CommitResult::Rejected(RejectReason::StreamPaused)
        );
    }

    #[tokio::test]
    async fn durable_pause_reason_survives_reopen_and_noop_pause_preserves_it() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let key = stream_delivery("pause-reason", "pause-reason-next").checkpoint_key();
        assert_eq!(
            state
                .pause_stream_with_reason(writer, key.clone(), "maintenance boundary".into())
                .await,
            Ok(StreamAdministrationOutcome::Paused { changed: true }),
        );
        assert_eq!(
            state.stream_pause_reason(&key).await,
            Ok(Some("maintenance boundary".into())),
        );
        assert_eq!(
            state
                .pause_stream_with_reason(writer, key.clone(), "replacement".into())
                .await,
            Ok(StreamAdministrationOutcome::Paused { changed: false }),
        );
        assert_eq!(
            state.stream_pause_reason(&key).await,
            Ok(Some("maintenance boundary".into())),
        );
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened.stream_pause_reason(&key).await,
            Ok(Some("maintenance boundary".into())),
        );
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
            result: Some(StreamHandlerResult::Fail(SafeDiagnostic {
                code: DiagnosticCode::Cancelled,
                class: DiagnosticClass::Cancellation,
            })),
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
    async fn keyed_source_without_payload_policy_fails_closed_and_releases_lease() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("legacy:one", "legacy:two");
        let key = delivery.checkpoint_key();
        let mut source = DefaultPayloadSource {
            key: key.clone(),
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
        assert_eq!(retrying.attempts, 2);

        let replacement = state.recover_abandoned(id(4), id(5)).await.unwrap();
        let mut stream = state.stream_backend(replacement);
        let recovered = stream
            .failure_async(&retrying.identity)
            .await
            .unwrap()
            .expect("recovered failure");
        assert_eq!(recovered.status, FailureStatus::Failed);
        assert_eq!(recovered.version, retrying.version + 1);
        assert_eq!(recovered.attempts, 2);
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
    async fn replay_typed_table_delivery_publishes_rows_without_advancing_checkpoint() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let delivery = stream_delivery("typed-replay", "typed-replay-next");
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
            result: Some(StreamHandlerResult::CommitTable(StreamTableMutationBatch {
                mutations: vec![table_mutation(31, 4, Some(12))],
                next_digest: digest(99),
            })),
        };

        let result = state
            .stream_backend(writer)
            .replay_async(grant, &mut handler)
            .await
            .unwrap();
        assert!(matches!(result, CommitResult::ReplayCompleted { .. }));
        assert_eq!(
            state.committed_table_row("books", &[4]).await.unwrap(),
            Some(vec![12])
        );
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
        let pending = state.pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0],
            table_mutation(31, 4, Some(12)).runtime_mutation().unwrap()
        );
    }

    #[tokio::test]
    async fn replay_typed_table_delivery_rolls_back_rows_and_terminal_transition_on_fault() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let (grant, checkpoint, delivery) =
            protected_replay_fixture(&state, writer, "typed-replay-fault", digest(5)).await;
        let capture = state.capture().await.unwrap();
        let table = table_mutation(32, 5, Some(13));
        let encoded = table.runtime_mutation().unwrap();

        assert_eq!(
            state
                .commit_stream_replay(StreamReplayCommit {
                    writer,
                    expected_capture: &capture,
                    mutations: std::slice::from_ref(&encoded),
                    table_mutations: std::slice::from_ref(&table),
                    next_digest: digest(101),
                    grant: grant.clone(),
                    faults: &Fail(FaultPoint::AfterMutation),
                })
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterMutation))
        );
        assert_eq!(
            state.committed_table_row("books", &[5]).await.unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
        assert_eq!(
            state
                .stream_backend(writer)
                .failure_async(&grant.failure)
                .await
                .unwrap()
                .expect("replay remains retryable")
                .status,
            FailureStatus::Replaying
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
    async fn request_activation_faults_roll_back_writes_and_terminal_claim() {
        for (key, point) in [
            (1, FaultPoint::BeforeTableWrite),
            (2, FaultPoint::AfterTableWrite),
            (3, FaultPoint::AfterMutation),
            (4, FaultPoint::AfterCheckpoint),
            (5, FaultPoint::AfterCapture),
            (6, FaultPoint::BeforeTerminalClaim),
            (7, FaultPoint::AfterTerminalClaim),
        ] {
            let (_temp, repo) = repository();
            let state = open_state(&repo).await;
            let owner = state.acquire_lease(id(4)).await.unwrap();
            let identity = request(4, 5);
            let fingerprint = digest(6);
            state.reserve_request(identity, fingerprint).await.unwrap();
            state
                .start_request_with_owner(identity, fingerprint, owner)
                .await
                .unwrap();
            let context = state.begin_activation().await.unwrap();
            let mutation = table_mutation(20 + key, key, Some(9));

            assert_eq!(
                state
                    .commit_table_request_activation(
                        owner,
                        identity,
                        fingerprint,
                        &context,
                        std::slice::from_ref(&mutation),
                        digest(10),
                        outcome(11),
                        &Fail(point),
                    )
                    .await,
                Err(RuntimeError::FaultInjected(point))
            );
            assert_eq!(
                state.committed_table_row("books", &[key]).await.unwrap(),
                None
            );
            assert!(state.pending().await.unwrap().is_empty());
            assert_eq!(state.latest_checkpoint().await.unwrap(), None);
            assert_eq!(state.capture().await.unwrap(), *context.capture());
            let status = state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.state, RequestState::Running);
            assert_eq!(status.terminal_outcome, None);

            drop(state);
            let reopened = open_state(&repo).await;
            assert_eq!(
                reopened.committed_table_row("books", &[key]).await.unwrap(),
                None
            );
            assert!(reopened.pending().await.unwrap().is_empty());
            assert_eq!(reopened.latest_checkpoint().await.unwrap(), None);
            assert_eq!(reopened.capture().await.unwrap(), context.capture().clone());
            let reopened_status = reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(reopened_status.state, RequestState::Running);
            assert_eq!(reopened_status.terminal_outcome, None);
        }
    }

    #[tokio::test]
    async fn controlled_fault_receipt_recovers_as_proven_after_owner_takeover_and_reopen() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        assert_eq!(
            state
                .commit_table_request_activation(
                    owner,
                    identity,
                    fingerprint,
                    &context,
                    &[table_mutation(8, 1, Some(9))],
                    digest(10),
                    outcome(11),
                    &Fail(FaultPoint::AfterTerminalClaim),
                )
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
        );
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        // The current owner cannot convert its own still-live activation into
        // a recovered terminal result.
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner::from(owner),
                    owner,
                    outcome(12),
                )
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        let fence = state.recover_abandoned(id(4), id(7)).await.unwrap();
        let recovered = state
            .recover_running_request_with_outcomes(
                identity,
                fingerprint,
                RequestOwner::from(owner),
                fence,
                outcome(12),
                outcome(13),
            )
            .await
            .unwrap();
        assert_eq!(recovered.disposition, RecoveryDisposition::RollbackProven);
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(recovered.status.terminal_outcome, Some(outcome(12)));
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner::from(owner),
                    fence,
                    outcome(13),
                )
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state.reserve_request(identity, fingerprint).await.unwrap(),
            recovered.status
        );
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::RollbackProven)
        );
        assert_eq!(
            reopened.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .terminal_outcome,
            Some(outcome(12))
        );
        assert_eq!(
            reopened
                .reserve_request(identity, fingerprint)
                .await
                .unwrap(),
            recovered.status
        );
    }

    #[tokio::test]
    async fn runtime_owned_activation_failure_rolls_back_and_recovers_as_proven_after_reopen() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();

        assert_eq!(
            state
                .commit_table_request_activation(
                    owner,
                    identity,
                    fingerprint,
                    &context,
                    &[table_mutation(8, 1, Some(9))],
                    digest(10),
                    outcome(11),
                    &Fail(FaultPoint::BeforeTerminalClaim),
                )
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::BeforeTerminalClaim))
        );
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);
        assert_eq!(state.capture().await.unwrap(), context.capture().clone());

        let recovery_owner = state.recover_abandoned(id(4), id(7)).await.unwrap();
        let recovered = state
            .recover_running_request_with_outcomes(
                identity,
                fingerprint,
                RequestOwner::from(owner),
                recovery_owner,
                outcome(12),
                outcome(13),
            )
            .await
            .unwrap();
        assert_eq!(recovered.disposition, RecoveryDisposition::RollbackProven);
        assert_eq!(recovered.status.terminal_outcome, Some(outcome(12)));
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert_eq!(
            reopened
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::RollbackProven)
        );
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .terminal_outcome,
            Some(outcome(12))
        );
    }

    #[tokio::test]
    async fn external_effect_marker_keeps_fault_recovery_uncertain() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        state
            .record_external_effect(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        assert!(matches!(
            state
                .commit_table_request_activation(
                    owner,
                    identity,
                    fingerprint,
                    &context,
                    &[table_mutation(8, 1, Some(9))],
                    digest(10),
                    outcome(11),
                    &Fail(FaultPoint::AfterTableWrite),
                )
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterTableWrite))
        ));
        let fence = state.recover_abandoned(id(4), id(7)).await.unwrap();
        assert_eq!(
            state
                .recover_running_request_with_outcomes(
                    identity,
                    fingerprint,
                    RequestOwner::from(owner),
                    fence,
                    outcome(12),
                    outcome(13),
                )
                .await
                .unwrap()
                .disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(
            state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .terminal_outcome,
            Some(outcome(13))
        );
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .terminal_outcome,
            Some(outcome(13))
        );
    }

    #[tokio::test]
    async fn request_activation_rejects_a_stale_owner_before_any_write() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        let replacement = state.takeover_lease(owner, id(7)).await.unwrap();

        assert_eq!(
            state
                .commit_table_request_activation(
                    owner,
                    identity,
                    fingerprint,
                    &context,
                    &[table_mutation(8, 1, Some(9))],
                    digest(10),
                    outcome(11),
                    &NoFault,
                )
                .await,
            Err(RuntimeError::OwnerLost)
        );
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(state.capture().await.unwrap(), context.capture().clone());
        assert_eq!(state.current_lease().await.unwrap(), Some(replacement));
        assert_eq!(
            state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Running
        );
    }

    #[tokio::test]
    async fn request_activation_replays_an_acknowledged_cancellation_without_writes() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        let cancelled = state
            .cancel_request_with_owner(identity, fingerprint, owner, outcome(7))
            .await
            .unwrap();

        let replay = state
            .commit_table_request_activation(
                owner,
                identity,
                fingerprint,
                &context,
                &[table_mutation(8, 1, Some(9))],
                digest(10),
                outcome(11),
                &NoFault,
            )
            .await
            .unwrap();
        assert_eq!(replay.request, cancelled);
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(state.latest_checkpoint().await.unwrap(), None);

        drop(state);
        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap(),
            Some(cancelled)
        );
        assert_eq!(
            reopened.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn request_activation_commits_and_replays_without_reexecution() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        let mutation = table_mutation(7, 1, Some(9));
        let committed = state
            .commit_table_request_activation(
                owner,
                identity,
                fingerprint,
                &context,
                std::slice::from_ref(&mutation),
                digest(10),
                outcome(11),
                &NoFault,
            )
            .await
            .unwrap();
        assert_eq!(committed.request.state, RequestState::Completed);
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        assert_eq!(
            state.pending().await.unwrap(),
            vec![mutation.runtime_mutation().unwrap()]
        );
        assert_eq!(
            state.latest_checkpoint().await.unwrap().unwrap().digest,
            digest(10)
        );

        drop(state);
        let reopened = open_state(&repo).await;
        let replacement = reopened.takeover_lease(owner, id(7)).await.unwrap();
        let replay_context = reopened.begin_activation().await.unwrap();
        let replay = reopened
            .commit_table_request_activation(
                replacement,
                identity,
                fingerprint,
                &replay_context,
                &[table_mutation(12, 2, Some(13))],
                digest(14),
                outcome(15),
                &NoFault,
            )
            .await
            .unwrap();
        assert_eq!(replay, committed);
        assert_eq!(
            reopened.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        assert_eq!(
            reopened.committed_table_row("books", &[2]).await.unwrap(),
            None
        );
        assert_eq!(
            reopened.pending().await.unwrap(),
            vec![mutation.runtime_mutation().unwrap()]
        );
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
    async fn typed_stream_delivery_publishes_rows_and_checkpoint_atomically() {
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

        let (next, result) = state
            .commit_stream_table_delivery(StreamTableDeliveryCommit {
                writer,
                expected_capture: &capture,
                mutations: &[table_mutation(5, 1, Some(9))],
                next_digest: digest(9),
                delivery: delivery_lease,
                expected_stream: expected,
                faults: &NoFault,
            })
            .await
            .unwrap();
        assert!(matches!(result, CommitResult::CheckpointAdvanced { .. }));
        assert_eq!(
            state.committed_table_row("books", &[1]).await.unwrap(),
            Some(vec![9])
        );
        let pending = state.pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, [5; 16]);
        assert!(pending[0].payload.starts_with(b"ORNA-TABLE-MUTATION\0"));
        assert_eq!(next.generation_digest(), digest(9));
    }

    #[tokio::test]
    async fn typed_stream_delivery_rolls_back_staged_row_and_checkpoint_on_fault_or_stale_cas() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        let delivery = stream_delivery("typed-fault:one", "typed-fault:two");
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
        let key = delivery_lease.delivery.checkpoint_key();

        assert_eq!(
            state
                .commit_stream_table_delivery(StreamTableDeliveryCommit {
                    writer,
                    expected_capture: &capture,
                    mutations: &[table_mutation(7, 2, Some(8))],
                    next_digest: digest(8),
                    delivery: delivery_lease.clone(),
                    expected_stream: expected.clone(),
                    faults: &Fail(FaultPoint::AfterCheckpoint),
                })
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterCheckpoint))
        );
        assert_eq!(
            state.committed_table_row("books", &[2]).await.unwrap(),
            None
        );
        assert_eq!(state.stream_checkpoint(&key).await.unwrap().version, 0);
        assert_eq!(state.stream_checkpoint(&key).await.unwrap().committed, None);

        let (unchanged, result) = state
            .commit_stream_table_delivery(StreamTableDeliveryCommit {
                writer,
                expected_capture: &capture,
                mutations: &[table_mutation(8, 3, Some(9))],
                next_digest: digest(9),
                delivery: delivery_lease,
                expected_stream: CheckpointPrecondition {
                    version: 1,
                    committed: None,
                },
                faults: &NoFault,
            })
            .await
            .unwrap();
        assert_eq!(
            result,
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
        assert_eq!(unchanged, capture);
        assert_eq!(
            state.committed_table_row("books", &[3]).await.unwrap(),
            None
        );
        assert_eq!(state.stream_checkpoint(&key).await.unwrap().version, 0);
        assert_eq!(state.stream_checkpoint(&key).await.unwrap().committed, None);
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
    async fn replay_failure_rolls_back_before_publishing_terminal_failure() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let (grant, checkpoint, delivery) =
            protected_replay_fixture(&state, writer, "replay-fault", digest(5)).await;

        assert_eq!(
            state
                .fail_stream_replay_with_faults(
                    writer,
                    &grant,
                    SafeDiagnostic {
                        code: DiagnosticCode::Internal,
                        class: DiagnosticClass::Permanent,
                    },
                    &Fail(FaultPoint::AfterReplayFailureRecord),
                )
                .await,
            Err(RuntimeError::FaultInjected(
                FaultPoint::AfterReplayFailureRecord
            ))
        );
        assert_eq!(state.pending().await.unwrap(), Vec::<Mutation>::new());
        assert_eq!(
            state
                .stream_backend(writer)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
        assert_eq!(
            state
                .stream_backend(writer)
                .failure_async(&grant.failure)
                .await
                .unwrap()
                .expect("replay failure remains durable")
                .status,
            FailureStatus::Replaying
        );
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
        assert_eq!(state.current_lease().await.unwrap(), Some(old));
        assert_eq!(
            state
                .takeover_lease(
                    WriterLease {
                        owner_id: old.owner_id,
                        epoch: old.epoch + 1,
                    },
                    id(5),
                )
                .await,
            Err(RuntimeError::OwnerLost)
        );
        let new = state.takeover_lease(old, id(5)).await.unwrap();
        assert_eq!(state.current_lease().await.unwrap(), Some(new));
        assert_eq!(
            state.takeover_lease(old, id(6)).await,
            Err(RuntimeError::OwnerLost)
        );
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
    async fn recovery_worker_can_enumerate_owned_and_legacy_requests() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owned = request(4, 5);
        let legacy = request(4, 6);
        let fingerprint = digest(8);
        // This is the deliberate compatibility fixture: the legacy row was
        // Running before any writer lease existed.
        state.reserve_request(legacy, fingerprint).await.unwrap();
        state.start_request(legacy, fingerprint).await.unwrap();
        let old = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(owned, fingerprint).await.unwrap();
        state
            .start_request_with_owner(owned, fingerprint, old)
            .await
            .unwrap();
        assert_eq!(state.running_requests().await.unwrap().len(), 2);
        assert_eq!(
            state.request_owner(owned, fingerprint).await.unwrap(),
            Some(RequestOwner::from(old))
        );
        assert_eq!(
            state.request_owner(legacy, fingerprint).await.unwrap(),
            None
        );

        let fence = state.takeover_lease(old, id(9)).await.unwrap();
        state
            .recover_running_request(
                owned,
                fingerprint,
                RequestOwner::from(old),
                fence,
                outcome(10),
            )
            .await
            .unwrap();
        state
            .recover_legacy_running_request(legacy, fingerprint, fence, outcome(11))
            .await
            .unwrap();
        assert!(state.running_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn recovery_reopens_an_interrupted_replay() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(4)).await.unwrap();
        let (grant, checkpoint, delivery) =
            protected_replay_fixture(&state, writer, "replay-recovery", digest(5)).await;
        let before_capture = state.capture().await.unwrap();

        let replacement = state.recover_abandoned(id(4), id(5)).await.unwrap();
        let recovered = state
            .stream_backend(replacement)
            .failure_async(&grant.failure)
            .await
            .unwrap()
            .expect("interrupted replay remains durable");
        assert_eq!(recovered.status, FailureStatus::Skipped);
        assert_eq!(recovered.version, grant.version + 1);
        assert_eq!(
            state
                .stream_backend(replacement)
                .checkpoint_async(&delivery.checkpoint_key())
                .await
                .unwrap(),
            checkpoint
        );
        assert_eq!(state.capture().await.unwrap(), before_capture);

        assert!(matches!(
            state
                .stream_backend(replacement)
                .apply_async(CommitIntent::Replay {
                    failure: grant.failure,
                    expected_version: recovered.version,
                })
                .await
                .unwrap(),
            CommitResult::ReplayGranted { .. }
        ));
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
    async fn compact_receipt_key_is_create_once_and_rejects_replacement() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let path = repo.runtime_paths().compact_runtime_receipt_public_key();
        let first = fs::read(&path).unwrap();
        assert_eq!(first.len(), 32);
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(fs::read(&path).unwrap(), first);
        drop(reopened);

        fs::write(&path, [9_u8; 32]).unwrap();
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
            Err(RuntimeError::CompactReceiptKeyMismatch)
        ));
    }

    #[tokio::test]
    async fn compact_receipt_encoding_round_trips_the_runtime_signature() {
        let (_temp, repo) = repository();
        git(repo.worktree(), &["commit", "--allow-empty", "-m", "root"]);
        let state = open_state(&repo).await;
        let commit = repo.head().unwrap().unwrap();
        let signing_bytes =
            CompactRuntimeReceipt::signing_bytes(id(7), digest(8), &commit, digest(9)).unwrap();
        let receipt = CompactRuntimeReceipt::new(
            id(7),
            digest(8),
            commit,
            digest(9),
            state
                .compact_receipt_signing_key
                .sign(&signing_bytes)
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(
            CompactRuntimeReceipt::decode(&receipt.encode().unwrap()).unwrap(),
            receipt
        );
    }

    #[tokio::test]
    async fn publication_completion_consumes_only_the_frozen_pending_prefix() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        let freeze = state
            .freeze(
                id(7),
                &Checkpoint {
                    generation: 1,
                    digest: digest(6),
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();

        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(8), digest(9), &NoFault)
            .await
            .unwrap();
        assert_eq!(
            state.pending_through(&freeze).await.unwrap(),
            vec![mutation(5)]
        );
        assert_eq!(
            state.pending().await.unwrap(),
            vec![mutation(5), mutation(8)]
        );

        let published = PublicationCommitId::new(vec![b'a'; 40]).unwrap();
        state
            .complete_publication(&freeze, &published)
            .await
            .unwrap();
        assert_eq!(state.pending_through(&freeze).await.unwrap(), Vec::new());
        assert_eq!(state.pending().await.unwrap(), vec![mutation(8)]);
        state
            .complete_publication(&freeze, &published)
            .await
            .unwrap();
        let different = PublicationCommitId::new(vec![b'b'; 40]).unwrap();
        assert_eq!(
            state.complete_publication(&freeze, &different).await,
            Err(RuntimeError::ConflictingPublicationCommit)
        );

        drop(state);
        let reopened = open_state(&repo).await;
        reopened.validate_recovery().await.unwrap();
        assert_eq!(reopened.pending().await.unwrap(), vec![mutation(8)]);
    }

    #[tokio::test]
    async fn ordinary_completion_rejects_a_compact_bound_freeze_without_consuming_its_tail() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let first_capture = state.capture().await.unwrap();
        state
            .commit(lease, &first_capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        let freeze = state
            .freeze(
                id(7),
                &Checkpoint {
                    generation: 1,
                    digest: digest(6),
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let second_capture = state.capture().await.unwrap();
        state
            .commit(lease, &second_capture, &mutation(8), digest(9), &NoFault)
            .await
            .unwrap();
        state
            .connection
            .execute(
                "UPDATE publication_freeze SET compact_watermark = ?1, compact_commit_id = ?2, \
                 compact_journal_verifier = ?3 WHERE intent_id = ?4",
                params![
                    freeze.checkpoint.digest.to_vec(),
                    vec![b'a'; 40],
                    digest(10).to_vec(),
                    freeze.intent_id.to_vec(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(
            state
                .complete_publication(&freeze, &PublicationCommitId::new(vec![b'a'; 40]).unwrap(),)
                .await,
            Err(RuntimeError::CompactPublicationRequired)
        );
        assert_eq!(
            state.pending().await.unwrap(),
            vec![mutation(5), mutation(8)]
        );
    }

    #[tokio::test]
    async fn recovery_rejects_malformed_or_incorrectly_signed_compact_receipts() {
        let (_temp, repo) = repository();
        git(repo.worktree(), &["commit", "--allow-empty", "-m", "root"]);
        let state = open_state(&repo).await;
        let lease = state.acquire_lease(id(4)).await.unwrap();
        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(5), digest(6), &NoFault)
            .await
            .unwrap();
        let freeze = state
            .freeze(
                id(7),
                &Checkpoint {
                    generation: 1,
                    digest: digest(6),
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let commit = repo.head().unwrap().unwrap();
        let receipt = CompactRuntimeReceipt::new(
            freeze.intent_id,
            freeze.checkpoint.digest,
            commit.clone(),
            digest(8),
            [0; 64],
        )
        .unwrap();
        state
            .connection
            .execute(
                "UPDATE publication_freeze SET compact_watermark = ?1, compact_commit_id = ?2, \
                 compact_journal_verifier = ?3 WHERE intent_id = ?4",
                params![
                    freeze.checkpoint.digest.to_vec(),
                    commit.as_str().as_bytes().to_vec(),
                    digest(8).to_vec(),
                    freeze.intent_id.to_vec(),
                ],
            )
            .await
            .unwrap();
        state
            .connection
            .execute(
                "INSERT INTO publication_commit (intent_id, commit_id, compact_receipt) \
                 VALUES (?1, ?2, ?3)",
                params![
                    freeze.intent_id.to_vec(),
                    commit.as_str().as_bytes().to_vec(),
                    receipt.encode().unwrap(),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
        state
            .connection
            .execute(
                "UPDATE publication_commit SET compact_receipt = ?1 WHERE intent_id = ?2",
                params![vec![0_u8], freeze.intent_id.to_vec()],
            )
            .await
            .unwrap();
        assert_eq!(
            state.validate_recovery().await,
            Err(RuntimeError::RecoveryInvalid)
        );
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
    async fn session_deletion_fence_blocks_external_reservation_until_terminal() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let external = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let active = request(5, 6);
        let active_fingerprint = digest(7);
        state
            .reserve_request(active, active_fingerprint)
            .await
            .unwrap();

        state.begin_session_deletion(id(5), owner).await.unwrap();
        state.begin_session_deletion(id(5), owner).await.unwrap();
        assert_eq!(
            external.reserve_request(request(5, 8), digest(9)).await,
            Err(RuntimeError::SessionClosed)
        );
        assert_eq!(
            state.finish_session_deletion(id(5), owner).await,
            Err(RuntimeError::SessionWorkActive)
        );
        assert_eq!(
            state
                .finish_session_deletion(
                    id(5),
                    WriterLease {
                        owner_id: id(9),
                        epoch: owner.epoch,
                    },
                )
                .await,
            Err(RuntimeError::OwnerLost)
        );

        state
            .cancel_request_with_owner(active, active_fingerprint, owner, outcome(10))
            .await
            .unwrap();
        state.finish_session_deletion(id(5), owner).await.unwrap();
        state.finish_session_deletion(id(5), owner).await.unwrap();
        assert_eq!(
            external.reserve_request(request(5, 11), digest(12)).await,
            Err(RuntimeError::SessionClosed)
        );

        drop(external);
        drop(state);
        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened.reserve_request(request(5, 13), digest(14)).await,
            Err(RuntimeError::SessionClosed)
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
    async fn legacy_orphan_wrapper_fails_closed_without_an_owner_fence() {
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
        assert_eq!(
            state
                .orphan_request(identity, fingerprint, outcome(7))
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        let fence = state.acquire_lease(id(8)).await.unwrap();
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner {
                        owner_id: id(9),
                        epoch: 1,
                    },
                    fence,
                    outcome(10),
                )
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Running
        );
    }
    #[tokio::test]
    async fn recovery_does_not_transition_a_request_owned_by_the_active_writer() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let active = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, active)
            .await
            .unwrap();
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner {
                        owner_id: id(8),
                        epoch: 1,
                    },
                    active,
                    outcome(10),
                )
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner::from(active),
                    active,
                    outcome(10),
                )
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Running
        );
    }
    #[tokio::test]
    async fn terminal_request_transitions_are_fenced_by_owner_epoch() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let old = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(identity, fingerprint).await.unwrap();
        let reserved_identity = request(4, 7);
        state
            .reserve_request(reserved_identity, fingerprint)
            .await
            .unwrap();
        state
            .start_request_with_owner(identity, fingerprint, old)
            .await
            .unwrap();
        let current = state.recover_abandoned(id(7), id(8)).await.unwrap();
        assert_eq!(
            state
                .complete_request_with_owner(identity, fingerprint, old, outcome(9))
                .await,
            Err(RuntimeError::OwnerLost)
        );
        assert_eq!(
            state
                .cancel_request_with_owner(identity, fingerprint, old, outcome(9))
                .await,
            Err(RuntimeError::OwnerLost)
        );
        assert_eq!(
            state
                .complete_request(identity, fingerprint, outcome(9))
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        let recovered = state
            .recover_running_request(
                identity,
                fingerprint,
                RequestOwner::from(old),
                current,
                outcome(10),
            )
            .await
            .unwrap();
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            state
                .complete_request(reserved_identity, fingerprint, outcome(12))
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .cancel_request(reserved_identity, fingerprint, outcome(12))
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .cancel_request_with_owner(reserved_identity, fingerprint, current, outcome(13))
                .await
                .unwrap()
                .state,
            RequestState::Cancelled
        );

        let next_identity = request(4, 6);
        state
            .reserve_request(next_identity, fingerprint)
            .await
            .unwrap();
        state
            .start_request_with_owner(next_identity, fingerprint, current)
            .await
            .unwrap();
        assert_eq!(
            state
                .complete_request_with_owner(next_identity, fingerprint, current, outcome(11))
                .await
                .unwrap()
                .state,
            RequestState::Completed
        );
    }
    #[tokio::test]
    async fn request_recovery_evidence_migration_is_idempotent() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        state.migrate_request_recovery_evidence().await.unwrap();
        state.migrate_request_recovery_evidence().await.unwrap();
        state.validate_recovery().await.unwrap();
    }

    #[tokio::test]
    async fn nullable_stream_partition_migration_preserves_legacy_checkpoint_keys() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(183)).await.unwrap();
        let key = stream_delivery("migration:one", "migration:two").checkpoint_key();
        assert!(matches!(
            state
                .stream_backend(writer)
                .apply_async(CommitIntent::Pause { key: key.clone() })
                .await
                .unwrap(),
            CommitResult::StreamStatusChanged { .. }
        ));
        drop(state);

        let database = Builder::new_local(repo.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "ALTER TABLE stream_checkpoint RENAME TO stream_checkpoint_current;
                 ALTER TABLE stream_failure RENAME TO stream_failure_current;
                 CREATE TABLE stream_checkpoint (
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
                 CREATE TABLE stream_failure (
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
                 INSERT INTO stream_checkpoint SELECT * FROM stream_checkpoint_current;
                 INSERT INTO stream_failure SELECT * FROM stream_failure_current;
                 DROP TABLE stream_checkpoint_current;
                 DROP TABLE stream_failure_current;
                 DELETE FROM runtime_schema_migration
                   WHERE migration = 'nullable-stream-partition-v1';",
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);

        let reopened = open_state(&repo).await;
        assert_eq!(reopened.stream_checkpoint(&key).await.unwrap().key, key);
        reopened.migrate_nullable_stream_partitions().await.unwrap();
    }

    #[tokio::test]
    async fn pre_evidence_ledger_migrates_and_reopens_as_legacy_uncertain() {
        let (_temp, repo) = repository();
        repo.runtime_paths().ensure_exists().unwrap();
        let database = Builder::new_local(repo.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE request_ledger (
                     session_id BLOB NOT NULL,
                     request_id BLOB NOT NULL,
                     fingerprint BLOB NOT NULL,
                     state INTEGER NOT NULL,
                     terminal_outcome BLOB,
                     PRIMARY KEY (session_id, request_id)
                 );
                 INSERT INTO request_ledger
                     (session_id, request_id, fingerprint, state, terminal_outcome)
                 VALUES (x'04040404040404040404040404040404',
                         x'05050505050505050505050505050505',
                         x'0606060606060606060606060606060606060606060606060606060606060606',
                         2, NULL);",
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);

        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        assert_eq!(
            state
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            None
        );
        let old = state.acquire_lease(id(7)).await.unwrap();
        assert_eq!(
            state
                .recover_legacy_running_request(identity, fingerprint, old, outcome(9),)
                .await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        let fence = state.recover_abandoned(id(7), id(8)).await.unwrap();
        assert_eq!(old.epoch, 1);
        let recovered = state
            .recover_legacy_running_request(identity, fingerprint, fence, outcome(9))
            .await
            .unwrap();
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
    }

    #[tokio::test]
    async fn populated_pre_rollback_receipt_ledger_migrates_and_reopens_uncertain() {
        let (_temp, repo) = repository();
        repo.runtime_paths().ensure_exists().unwrap();
        let database = Builder::new_local(repo.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let owner = WriterLease {
            owner_id: id(4),
            epoch: 1,
        };
        let marker = controlled_transaction_marker(identity, fingerprint, owner);
        connection
            .execute_batch(
                "CREATE TABLE request_ledger (
                     session_id BLOB NOT NULL,
                     request_id BLOB NOT NULL,
                     fingerprint BLOB NOT NULL,
                     state INTEGER NOT NULL,
                     terminal_outcome BLOB,
                     owner_id BLOB,
                     owner_epoch INTEGER,
                     effect_evidence INTEGER NOT NULL DEFAULT 0,
                     recovery_disposition INTEGER NOT NULL DEFAULT 0,
                     controlled_transaction_proof BLOB,
                     PRIMARY KEY (session_id, request_id)
                 );",
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO request_ledger
                     (session_id, request_id, fingerprint, state, terminal_outcome,
                      owner_id, owner_epoch, effect_evidence, recovery_disposition,
                      controlled_transaction_proof)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 1, 0, ?7)",
                params![
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).unwrap(),
                    marker.to_vec(),
                ],
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);

        let state = open_state(&repo).await;
        assert_eq!(
            state
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            None
        );
        let active = state.acquire_lease(owner.owner_id).await.unwrap();
        assert_eq!(active, owner);
        let recovery_owner = state.recover_abandoned(id(4), id(7)).await.unwrap();
        let rollback_proven = outcome(8);
        let external_effects_uncertain = outcome(9);
        let recovered = state
            .recover_running_request_with_outcomes(
                identity,
                fingerprint,
                RequestOwner::from(owner),
                recovery_owner,
                rollback_proven,
                external_effects_uncertain.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(
            recovered.status.terminal_outcome,
            Some(external_effects_uncertain)
        );
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
        assert_eq!(
            reopened
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .terminal_outcome,
            Some(outcome(9))
        );
    }

    #[tokio::test]
    async fn migrated_ledger_retains_controlled_rollback_recovery_after_reopen() {
        let (_temp, repo) = repository();
        repo.runtime_paths().ensure_exists().unwrap();
        let database = Builder::new_local(repo.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE request_ledger (
                     session_id BLOB NOT NULL,
                     request_id BLOB NOT NULL,
                     fingerprint BLOB NOT NULL,
                     state INTEGER NOT NULL,
                     terminal_outcome BLOB,
                     PRIMARY KEY (session_id, request_id)
                 );",
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);

        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(4)).await.unwrap();
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let context = state.begin_activation().await.unwrap();
        assert_eq!(
            state
                .commit_table_request_activation(
                    owner,
                    identity,
                    fingerprint,
                    &context,
                    &[table_mutation(8, 1, Some(9))],
                    digest(10),
                    outcome(11),
                    &Fail(FaultPoint::AfterTerminalClaim),
                )
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
        );
        let recovery_owner = state.recover_abandoned(id(4), id(7)).await.unwrap();
        let recovered = state
            .recover_running_request(
                identity,
                fingerprint,
                RequestOwner::from(owner),
                recovery_owner,
                outcome(12),
            )
            .await
            .unwrap();
        assert_eq!(recovered.disposition, RecoveryDisposition::RollbackProven);
        drop(state);

        let reopened = open_state(&repo).await;
        assert_eq!(
            reopened
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::RollbackProven)
        );
        assert_eq!(
            reopened.committed_table_row("books", &[1]).await.unwrap(),
            None
        );
        assert_eq!(
            reopened
                .reserve_request(identity, fingerprint)
                .await
                .unwrap(),
            recovered.status
        );
    }

    #[tokio::test]
    async fn recovery_enforces_the_request_metadata_state_matrix() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        state.reserve_request(identity, fingerprint).await.unwrap();
        for (invalid, reset) in [
            (
                "UPDATE request_ledger SET owner_id = x'07070707070707070707070707070707', owner_epoch = 1",
                "UPDATE request_ledger SET owner_id = NULL, owner_epoch = NULL",
            ),
            (
                "UPDATE request_ledger SET recovery_disposition = 2",
                "UPDATE request_ledger SET recovery_disposition = 0",
            ),
            (
                "UPDATE request_ledger SET state = 2, owner_id = x'07070707070707070707070707070707', owner_epoch = 1, effect_evidence = 1",
                "UPDATE request_ledger SET state = 1, terminal_outcome = NULL, owner_id = NULL, owner_epoch = NULL, effect_evidence = 0",
            ),
            (
                "UPDATE request_ledger SET state = 3, terminal_outcome = x'01', effect_evidence = 2",
                "UPDATE request_ledger SET state = 1, terminal_outcome = NULL, effect_evidence = 0",
            ),
            (
                "UPDATE request_ledger SET state = 5, terminal_outcome = x'01', effect_evidence = 1, recovery_disposition = 1, controlled_transaction_proof = x'0000000000000000000000000000000000000000000000000000000000000000'",
                "UPDATE request_ledger SET state = 1, terminal_outcome = NULL, effect_evidence = 0, recovery_disposition = 0, controlled_transaction_proof = NULL",
            ),
            (
                "UPDATE request_ledger SET state = 5, terminal_outcome = x'01', recovery_disposition = 1",
                "UPDATE request_ledger SET state = 1, terminal_outcome = NULL, recovery_disposition = 0",
            ),
        ] {
            state
                .connection
                .execute_batch(&format!(
                    "PRAGMA ignore_check_constraints = ON; {invalid}; PRAGMA ignore_check_constraints = OFF;"
                ))
                .await
                .unwrap();
            assert_eq!(
                state.validate_recovery().await,
                Err(RuntimeError::RecoveryInvalid),
                "invalid metadata combination was accepted"
            );
            state.connection.execute(reset, ()).await.unwrap();
        }
    }
    #[tokio::test]
    async fn legacy_running_request_recovers_as_uncertain_after_takeover() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        // Preserve the migration path for a legacy row written before the
        // owner-fenced runtime had acquired a lease.
        state.reserve_request(identity, fingerprint).await.unwrap();
        state.start_request(identity, fingerprint).await.unwrap();
        state.acquire_lease(id(7)).await.unwrap();
        let fence = state.recover_abandoned(id(7), id(8)).await.unwrap();
        let recovered = state
            .recover_legacy_running_request(identity, fingerprint, fence, outcome(9))
            .await
            .unwrap();
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(
            state
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
    }
    #[tokio::test]
    async fn legacy_start_request_is_fenced_when_a_writer_lease_is_present() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let owner = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(identity, fingerprint).await.unwrap();

        assert_eq!(
            state.start_request(identity, fingerprint).await,
            Err(RuntimeError::RequestOwnerConflict)
        );
        assert_eq!(
            state
                .request_status(identity, fingerprint)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Reserved
        );
        assert_eq!(
            state
                .start_request_with_owner(identity, fingerprint, owner)
                .await
                .unwrap()
                .state,
            RequestState::Running
        );
    }
    #[tokio::test]
    async fn recovery_retains_uncertainty_and_clears_old_owner_evidence() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(7)).await.unwrap();
        let controlled = request(4, 5);
        let external = request(4, 6);
        for identity in [controlled, external] {
            state.reserve_request(identity, digest(8)).await.unwrap();
            state
                .start_request_with_owner(identity, digest(8), owner)
                .await
                .unwrap();
        }
        state
            .record_controlled_transaction(controlled, digest(8), owner)
            .await
            .unwrap();
        state
            .record_external_effect(external, digest(8), owner)
            .await
            .unwrap();
        let fence = state.recover_abandoned(id(7), id(9)).await.unwrap();
        let lost = RequestOwner::from(owner);
        let recovered = state
            .recover_running_request(controlled, digest(8), lost, fence, outcome(10))
            .await
            .unwrap();
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(
            state
                .request_recovery_disposition(controlled, digest(8))
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
        let mut rows = state
            .connection
            .query(
                "SELECT owner_id, owner_epoch, effect_evidence,
                        recovery_disposition, controlled_transaction_proof
                 FROM request_ledger WHERE session_id = ?1 AND request_id = ?2",
                params![
                    controlled.session_id.to_vec(),
                    controlled.request_id.to_vec()
                ],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert!(row.get::<Option<Vec<u8>>>(0).unwrap().is_none());
        assert!(row.get::<Option<i64>>(1).unwrap().is_none());
        assert_eq!(row.get::<i64>(2).unwrap(), 0);
        assert_eq!(row.get::<i64>(3).unwrap(), 2);
        assert!(row.get::<Option<Vec<u8>>>(4).unwrap().is_none());
        let recovered = state
            .recover_running_request(external, digest(8), lost, fence, outcome(11))
            .await
            .unwrap();
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(
            state
                .request_recovery_disposition(external, digest(8))
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
        let mut rows = state
            .connection
            .query(
                "SELECT owner_id, owner_epoch, effect_evidence,
                        recovery_disposition, controlled_transaction_proof
                 FROM request_ledger WHERE session_id = ?1 AND request_id = ?2",
                params![external.session_id.to_vec(), external.request_id.to_vec()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert!(row.get::<Option<Vec<u8>>>(0).unwrap().is_none());
        assert!(row.get::<Option<i64>>(1).unwrap().is_none());
        assert_eq!(row.get::<i64>(2).unwrap(), 0);
        assert_eq!(row.get::<i64>(3).unwrap(), 2);
        assert!(row.get::<Option<Vec<u8>>>(4).unwrap().is_none());
    }
    #[tokio::test]
    async fn recovery_is_fingerprinted_terminal_and_never_reexecutes() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let owner = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        let fence = state.recover_abandoned(id(7), id(8)).await.unwrap();
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    digest(9),
                    RequestOwner::from(owner),
                    fence,
                    outcome(10),
                )
                .await,
            Err(RuntimeError::RequestFingerprintMismatch)
        );
        let first = state
            .recover_running_request(
                identity,
                fingerprint,
                RequestOwner::from(owner),
                fence,
                outcome(10),
            )
            .await
            .unwrap();
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner::from(owner),
                    fence,
                    outcome(11),
                )
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state.reserve_request(identity, fingerprint).await.unwrap(),
            first.status
        );
        assert_eq!(
            state.start_request(identity, fingerprint).await,
            Err(RuntimeError::RequestStateConflict)
        );
    }
    #[tokio::test]
    async fn recovery_does_not_change_existing_terminal_replay() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = request(4, 5);
        let fingerprint = digest(6);
        let owner = state.acquire_lease(id(7)).await.unwrap();
        state.reserve_request(identity, fingerprint).await.unwrap();
        state
            .start_request_with_owner(identity, fingerprint, owner)
            .await
            .unwrap();
        state
            .record_controlled_transaction(identity, fingerprint, owner)
            .await
            .unwrap();
        let completed = state
            .complete_request_with_owner(identity, fingerprint, owner, outcome(8))
            .await
            .unwrap();
        let fence = state.recover_abandoned(id(7), id(9)).await.unwrap();
        assert_eq!(
            state
                .recover_running_request(
                    identity,
                    fingerprint,
                    RequestOwner::from(owner),
                    fence,
                    outcome(10),
                )
                .await,
            Err(RuntimeError::RequestStateConflict)
        );
        assert_eq!(
            state.reserve_request(identity, fingerprint).await.unwrap(),
            completed
        );
        assert_eq!(
            state
                .request_recovery_disposition(identity, fingerprint)
                .await
                .unwrap(),
            None
        );
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

    #[tokio::test]
    async fn run_observation_reopens_with_its_pinned_snapshot_and_identity() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let request = request(31, 32);
        state.reserve_request(request, digest(33)).await.unwrap();
        let consumer = stream_delivery("one", "two").consumer;
        let registered = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: consumer,
                function: "pkg.main".into(),
                source_identity: Some("source-a".into()),
                invocation_id: id(34),
            })
            .await
            .unwrap();
        let id = registered.id;
        let pin = registered.snapshot.clone();
        assert!(registered.ended_ms.is_none());
        assert!(registered.observed_ms >= registered.started_ms);
        state
            .complete_request(request, digest(33), outcome(35))
            .await
            .unwrap();
        drop(state);
        let reopened = open_state(&repo).await;
        let restored = reopened.run_observation(id).await.unwrap().unwrap();
        assert_eq!(restored.id, id);
        assert_eq!(restored.snapshot, pin);
        assert_eq!(restored.status, RunObservationStatus::Completed);
        assert!(restored.ended_ms.is_some());
        assert_eq!(restored.ended_ms, Some(restored.observed_ms));
        assert!(restored.observed_ms >= restored.started_ms);
        assert!(!restored.live);
    }

    #[tokio::test]
    async fn observed_request_lifecycle_admits_before_work_and_keeps_terminals_distinct() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(151)).await.unwrap();
        let consumer = stream_delivery("observed", "request").consumer;

        let completed_request = request(170, 171);
        let completed = state
            .begin_observed_request(
                RunObservationRegistration {
                    request: completed_request,
                    consumer_identity: consumer.clone(),
                    function: "pkg.completed".into(),
                    source_identity: Some("source-completed".into()),
                    invocation_id: id(172),
                },
                digest(173),
                owner,
            )
            .await
            .unwrap();
        let completed_run = completed.run.unwrap();
        let context = state.begin_activation().await.unwrap();
        state
            .commit_table_request_activation(
                owner,
                completed_request,
                digest(173),
                &context,
                &[table_mutation(174, 1, Some(175))],
                digest(176),
                outcome(177),
                &NoFault,
            )
            .await
            .unwrap();
        let completed_observation = state
            .run_observation(completed_run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            completed_observation.status,
            RunObservationStatus::Completed
        );
        assert!(!completed_observation.live);

        let failed_request = request(152, 153);
        let failed = state
            .begin_observed_request(
                RunObservationRegistration {
                    request: failed_request,
                    consumer_identity: consumer.clone(),
                    function: "pkg.failed".into(),
                    source_identity: Some("source-failed".into()),
                    invocation_id: id(154),
                },
                digest(155),
                owner,
            )
            .await
            .unwrap();
        let failed_run = failed.run.unwrap();
        assert!(failed.admitted);
        assert_eq!(failed.request.state, RequestState::Running);
        assert_eq!(failed_run.status, RunObservationStatus::Running);
        assert_eq!(failed_run.invocation_id, id(154));
        let failure = SafeDiagnostic {
            code: DiagnosticCode::ExecutionRejected,
            class: DiagnosticClass::Permanent,
        };
        state
            .fail_observed_request_with_owner(
                failed_request,
                digest(155),
                owner,
                outcome(156),
                failure,
            )
            .await
            .unwrap();
        let failed_observation = state.run_observation(failed_run.id).await.unwrap().unwrap();
        assert_eq!(failed_observation.status, RunObservationStatus::Failed);
        assert_eq!(failed_observation.diagnostic, Some(failure));

        let replay = state
            .begin_observed_request(
                RunObservationRegistration {
                    request: failed_request,
                    consumer_identity: consumer.clone(),
                    function: "pkg.never-runs".into(),
                    source_identity: None,
                    invocation_id: id(157),
                },
                digest(155),
                WriterLease {
                    owner_id: id(158),
                    epoch: 1,
                },
            )
            .await
            .unwrap();
        assert!(!replay.admitted);
        assert_eq!(replay.request.terminal_outcome, Some(outcome(156)));
        assert_eq!(replay.run, None);

        let cancelled_request = request(159, 160);
        let cancelled = state
            .begin_observed_request(
                RunObservationRegistration {
                    request: cancelled_request,
                    consumer_identity: consumer.clone(),
                    function: "pkg.cancelled".into(),
                    source_identity: None,
                    invocation_id: id(161),
                },
                digest(162),
                owner,
            )
            .await
            .unwrap();
        let cancelled_run = cancelled.run.unwrap();
        state
            .cancel_observed_request_with_owner(cancelled_request, digest(162), owner, outcome(163))
            .await
            .unwrap();
        assert_eq!(
            state
                .run_observation(cancelled_run.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RunObservationStatus::Cancelled
        );

        let orphaned_request = request(164, 165);
        let orphaned = state
            .begin_observed_request(
                RunObservationRegistration {
                    request: orphaned_request,
                    consumer_identity: consumer,
                    function: "pkg.orphaned".into(),
                    source_identity: None,
                    invocation_id: id(166),
                },
                digest(167),
                owner,
            )
            .await
            .unwrap();
        let orphaned_run = orphaned.run.unwrap();
        let fence = state.recover_abandoned(id(151), id(168)).await.unwrap();
        state
            .recover_running_request(
                orphaned_request,
                digest(167),
                RequestOwner::from(owner),
                fence,
                outcome(169),
            )
            .await
            .unwrap();
        let retained = state
            .run_observation(orphaned_run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.status, RunObservationStatus::Orphaned);
        assert!(!retained.live);

        // The successful activation must only transition its own observed
        // run. Other terminal observations retain their distinct status and
        // diagnostic evidence.
        assert_eq!(
            state
                .run_observation(failed_run.id)
                .await
                .unwrap()
                .unwrap()
                .diagnostic,
            Some(failure)
        );
        assert_eq!(
            state
                .run_observation(cancelled_run.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RunObservationStatus::Cancelled
        );
        assert_eq!(
            state
                .run_observation(orphaned_run.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RunObservationStatus::Orphaned
        );
    }

    #[tokio::test]
    async fn stream_observation_rejects_cross_run_checkpoint_rebinding_and_projects_checked_references()
     {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let key = stream_delivery("one", "two").checkpoint_key();
        let mut runs = Vec::new();
        for (session, request_id, invocation) in [(35, 36, 37), (38, 39, 40)] {
            let request = request(session, request_id);
            state
                .reserve_request(request, digest(request_id))
                .await
                .unwrap();
            runs.push(
                state
                    .register_run_observation(RunObservationRegistration {
                        request,
                        consumer_identity: key.consumer.clone(),
                        function: "pkg.consume".into(),
                        source_identity: None,
                        invocation_id: id(invocation),
                    })
                    .await
                    .unwrap(),
            );
        }
        let first = state
            .register_stream_observation(StreamObservationRegistration {
                run: runs[0].id,
                producer: "source-object".into(),
                consumer: Some("pkg.consume".into()),
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        let run_reference = runs[0].reference().unwrap();
        assert_eq!(
            run_reference.as_row_ref().database_id,
            runs[0].snapshot.database_id()
        );
        assert_eq!(
            run_reference.as_row_ref().snapshot,
            *runs[0].snapshot.snapshot()
        );
        let mut mismatched_generation = runs[0].clone();
        mismatched_generation.runtime_generation += 1;
        assert_eq!(
            mismatched_generation.reference(),
            Err(RuntimeError::InvalidObservationReference)
        );
        let stream_reference = first.reference(&runs[0]).unwrap();
        assert_eq!(first.parent_capture, runs[0].snapshot);
        assert_eq!(
            stream_reference.as_row_ref().snapshot,
            *runs[0].snapshot.snapshot()
        );
        let OvbRaw::Array(natural_key) = &stream_reference.as_row_ref().key else {
            panic!("stream reference did not retain its natural key");
        };
        assert_eq!(natural_key.len(), 3);
        assert_eq!(
            Value::new(natural_key[0].clone())
                .unwrap()
                .encode()
                .unwrap(),
            run_reference.as_row_ref().encode().unwrap()
        );
        let OvbRaw::Tag(60010, nested_body) = &natural_key[0] else {
            panic!("stream natural key did not contain a canonical run RowRef");
        };
        let OvbRaw::Array(nested_fields) = nested_body.as_ref() else {
            panic!("nested run RowRef did not contain its canonical fields");
        };
        assert_eq!(nested_fields.len(), 4);
        assert_eq!(
            nested_fields[0],
            OvbRaw::Tag(
                37,
                Box::new(OvbRaw::Bytes(runs[0].snapshot.database_id().to_vec()))
            )
        );
        assert_eq!(
            nested_fields[1],
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(SYS_RUN_TABLE_ID.to_vec())))
        );
        assert_eq!(nested_fields[2], run_reference.as_row_ref().key);
        assert_eq!(nested_fields[3], runs[0].snapshot.snapshot().raw().clone());
        assert_eq!(natural_key[1], OvbRaw::Text(first.source_identity.clone()));
        assert_eq!(
            natural_key[2],
            OvbRaw::Text(first.partition.clone().unwrap())
        );
        let mut unpartitioned = first.clone();
        unpartitioned.partition = None;
        assert_eq!(
            unpartitioned.reference(&runs[0]),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        let mut mismatched_partition = first.clone();
        mismatched_partition.partition = Some("other-partition".into());
        assert_eq!(
            mismatched_partition.reference(&runs[0]),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        assert_eq!(
            first.reference(&runs[1]),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        let mut same_id_different_capture = runs[0].clone();
        same_id_different_capture.snapshot = CwdCapture::new(
            Snapshot::cwd(
                runs[0].snapshot.database_id(),
                runs[0].snapshot.runtime_id(),
                BigInt::from(1),
            )
            .unwrap(),
            digest(178),
        )
        .unwrap();
        assert_eq!(
            first.reference(&same_id_different_capture),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        let mut mismatched_source = first.clone();
        mismatched_source.source_identity = "other-source".into();
        assert_eq!(
            mismatched_source.reference(&runs[0]),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        let mut mismatched_consumer = first.clone();
        mismatched_consumer.consumer_identity.principal =
            Component::new("other-principal").unwrap();
        assert_eq!(
            mismatched_consumer.reference(&runs[0]),
            Err(RuntimeError::ObservationCoordinateMismatch)
        );
        let mut mismatched_parent = runs[0].clone();
        mismatched_parent.runtime_id = id(177);
        assert_eq!(
            first.reference(&mismatched_parent),
            Err(RuntimeError::InvalidObservationReference)
        );
        assert!(matches!(
            state
                .register_stream_observation(StreamObservationRegistration {
                    run: runs[1].id,
                    producer: "source-object".into(),
                    consumer: Some("pkg.consume".into()),
                    checkpoint: key.clone(),
                })
                .await,
            Err(RuntimeError::StorageUnavailable)
        ));
        let before = state.stream_checkpoint(&key).await.unwrap();
        assert_eq!(
            state
                .stream_observation(first.id)
                .await
                .unwrap()
                .unwrap()
                .items_seen,
            0
        );
        assert_eq!(state.runtime_stream_observations().await.unwrap().len(), 1);
        assert_eq!(state.stream_checkpoint(&key).await.unwrap(), before);
    }

    #[tokio::test]
    async fn nullable_stream_partition_persists_reopens_and_projects_canonical_null() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let request = request(179, 180);
        state.reserve_request(request, digest(181)).await.unwrap();
        let mut key = stream_delivery("nullable:one", "nullable:two").checkpoint_key();
        key.partition = None;
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: key.consumer.clone(),
                function: "pkg.consume-nullable".into(),
                source_identity: None,
                invocation_id: id(182),
            })
            .await
            .unwrap();
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: None,
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        assert_eq!(stream.partition, None);
        assert_eq!(state.stream_checkpoint(&key).await.unwrap().key, key);
        let reference = stream.reference(&run).unwrap();
        let OvbRaw::Array(natural_key) = &reference.as_row_ref().key else {
            panic!("nullable stream reference did not retain its natural key");
        };
        assert_eq!(natural_key[2], OvbRaw::Null);

        let mut duplicate_key = key.clone();
        duplicate_key.consumer.principal = Component::new("other-principal").unwrap();
        assert!(matches!(
            state
                .register_stream_observation(StreamObservationRegistration {
                    run: run.id,
                    producer: "other-source-object".into(),
                    consumer: None,
                    checkpoint: duplicate_key,
                })
                .await,
            Err(RuntimeError::StorageUnavailable)
        ));

        drop(state);
        let reopened = open_state(&repo).await;
        let restored_run = reopened.run_observation(run.id).await.unwrap().unwrap();
        let restored = reopened
            .stream_observation(stream.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.checkpoint, key);
        assert_eq!(restored.partition, None);
        assert_eq!(restored.reference(&restored_run).unwrap(), reference);
        assert_eq!(reopened.stream_checkpoint(&key).await.unwrap().key, key);
    }

    #[tokio::test]
    async fn old_generation_run_is_retained_but_not_live() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let request = request(48, 49);
        state.reserve_request(request, digest(50)).await.unwrap();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: stream_delivery("one", "two").consumer,
                function: "pkg.main".into(),
                source_identity: None,
                invocation_id: id(51),
            })
            .await
            .unwrap();
        let lease = state.acquire_lease(id(52)).await.unwrap();
        let capture = state.capture().await.unwrap();
        state
            .commit(lease, &capture, &mutation(53), digest(54), &NoFault)
            .await
            .unwrap();
        let retained = state.run_observation(run.id).await.unwrap().unwrap();
        assert!(!retained.live);
        assert_eq!(state.run_observations().await.unwrap().len(), 1);
        assert!(state.runtime_run_observations().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn observation_counters_follow_checkpoint_and_owner_loss_orphans_the_run() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let request = request(41, 42);
        let fingerprint = digest(43);
        state.reserve_request(request, fingerprint).await.unwrap();
        let key = stream_delivery("one", "two").checkpoint_key();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: key.consumer.clone(),
                function: "pkg.consume".into(),
                source_identity: None,
                invocation_id: id(44),
            })
            .await
            .unwrap();
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: None,
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        let owner = state.acquire_lease(id(45)).await.unwrap();
        state
            .start_request_with_owner(request, fingerprint, owner)
            .await
            .unwrap();
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let mut backend = state.stream_backend(owner);
        let lease = match backend
            .apply_async(CommitIntent::Acquire {
                delivery: stream_delivery("one", "two"),
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected result: {other:?}"),
        };
        assert!(matches!(
            backend
                .apply_async(CommitIntent::Complete { lease, expected })
                .await
                .unwrap(),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let observed = state.stream_observation(stream.id).await.unwrap().unwrap();
        assert_eq!(
            (
                observed.items_seen,
                observed.items_committed,
                observed.items_failed
            ),
            (1, 1, 0)
        );
        assert_eq!(observed.consumer_identity, key.consumer);
        assert_eq!(observed.source_identity, key.source.as_str());
        assert_eq!(
            observed.partition,
            key.partition
                .as_ref()
                .map(|value| value.as_str().to_owned())
        );
        assert!(observed.last_item_ms.is_some());
        assert!(observed.observed_ms >= observed.last_item_ms.unwrap());
        assert_eq!(
            state
                .run_observation(run.id)
                .await
                .unwrap()
                .unwrap()
                .checkpoint_count,
            1
        );
        let fence = state.recover_abandoned(id(45), id(46)).await.unwrap();
        state
            .recover_running_request(
                request,
                fingerprint,
                RequestOwner::from(owner),
                fence,
                outcome(47),
            )
            .await
            .unwrap();
        let orphaned = state.run_observation(run.id).await.unwrap().unwrap();
        assert_eq!(orphaned.status, RunObservationStatus::Orphaned);
        assert!(!orphaned.live);
        assert_eq!(state.runtime_run_observations().await.unwrap().len(), 0);
        drop(state);
        let reopened = open_state(&repo).await;
        let restored = reopened
            .stream_observation(stream.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.run, run.id);
        assert_eq!(restored.consumer_identity, key.consumer);
        assert_eq!(restored.source_identity, key.source.as_str());
        assert_eq!(
            restored.partition,
            key.partition
                .as_ref()
                .map(|value| value.as_str().to_owned())
        );
        assert_eq!(
            (
                restored.items_seen,
                restored.items_committed,
                restored.items_failed
            ),
            (1, 1, 0)
        );
        assert!(restored.last_item_ms.is_some());
        assert!(!restored.live);
    }

    #[tokio::test]
    async fn observation_checkpoint_counter_rolls_back_and_cancellation_is_not_failure() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(61)).await.unwrap();
        let request = request(62, 63);
        state.reserve_request(request, digest(64)).await.unwrap();
        let key =
            stream_delivery("observation-fault:one", "observation-fault:two").checkpoint_key();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: key.consumer.clone(),
                function: "pkg.consume".into(),
                source_identity: None,
                invocation_id: id(65),
            })
            .await
            .unwrap();
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: None,
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let delivery = stream_delivery("observation-fault:one", "observation-fault:two");
        let lease = match state
            .stream_backend(writer)
            .apply_async(CommitIntent::Acquire {
                delivery,
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected acquisition: {other:?}"),
        };
        let capture = state.capture().await.unwrap();
        assert_eq!(
            state
                .commit_stream_delivery(StreamDeliveryCommit {
                    writer,
                    expected_capture: &capture,
                    mutations: &[mutation(66)],
                    next_digest: digest(67),
                    delivery: lease.clone(),
                    expected_stream: expected,
                    faults: &Fail(FaultPoint::AfterCheckpoint),
                })
                .await,
            Err(RuntimeError::FaultInjected(FaultPoint::AfterCheckpoint))
        );
        let after_fault = state.stream_observation(stream.id).await.unwrap().unwrap();
        assert_eq!(
            (after_fault.items_seen, after_fault.items_committed),
            (1, 0)
        );
        assert_eq!(
            state
                .run_observation(run.id)
                .await
                .unwrap()
                .unwrap()
                .checkpoint_count,
            0
        );

        assert!(matches!(
            state
                .stream_backend(writer)
                .apply_async(CommitIntent::Cancel { lease })
                .await
                .unwrap(),
            CommitResult::Cancelled { .. }
        ));
        let cancelled = state.stream_observation(stream.id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, StreamObservationStatus::Cancelled);
        assert_eq!(cancelled.items_failed, 0);
        assert_eq!(cancelled.diagnostic, None);
    }

    #[tokio::test]
    async fn finite_stream_exhaustion_completes_its_durable_observation() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(71)).await.unwrap();
        let request = request(72, 73);
        state.reserve_request(request, digest(74)).await.unwrap();
        let key =
            stream_delivery("observation-finite:one", "observation-finite:two").checkpoint_key();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: key.consumer.clone(),
                function: "pkg.consume".into(),
                source_identity: None,
                invocation_id: id(75),
            })
            .await
            .unwrap();
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: None,
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        let mut source = SequenceSource {
            key,
            descriptor: StreamSourceDescriptor {
                kind: StreamSourceKind::Finite,
                replayable: true,
            },
            polls: 0,
            waits: 0,
            steps: VecDeque::from([StreamSourcePoll::Exhausted]),
        };
        let mut handler = CommitHandler { calls: 0 };
        assert!(matches!(
            state
                .run_stream(
                    writer,
                    &source.key.clone(),
                    &mut source,
                    &mut handler,
                    &NeverCancelled
                )
                .await
                .unwrap(),
            StreamRunOutcome::Exhausted { delivered: 0, .. }
        ));
        let completed = state.stream_observation(stream.id).await.unwrap().unwrap();
        assert_eq!(completed.status, StreamObservationStatus::Completed);
        assert!(!completed.live);
    }

    #[tokio::test]
    async fn stream_observation_retains_checked_failure_reference_across_retry_and_reopen() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let writer = state.acquire_lease(id(201)).await.unwrap();
        let request = request(202, 203);
        state.reserve_request(request, digest(204)).await.unwrap();
        let mut delivery = stream_delivery("failure-reference:one", "failure-reference:two");
        delivery.partition = None;
        let key = delivery.checkpoint_key();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: key.consumer.clone(),
                function: "pkg.failure-reference".into(),
                source_identity: None,
                invocation_id: id(205),
            })
            .await
            .unwrap();
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: None,
                checkpoint: key.clone(),
            })
            .await
            .unwrap();
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let lease = match state
            .stream_backend(writer)
            .apply_async(CommitIntent::Acquire {
                delivery,
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected acquisition: {other:?}"),
        };
        let failure = match state
            .fail_stream_delivery(
                writer,
                lease,
                SafeDiagnostic {
                    code: DiagnosticCode::ExecutionRejected,
                    class: DiagnosticClass::Permanent,
                },
                StreamFailurePayload::Plaintext(vec![1]),
            )
            .await
            .unwrap()
        {
            CommitResult::Failed { failure } => failure,
            other => panic!("unexpected failure: {other:?}"),
        };
        let failed = state.stream_observation(stream.id).await.unwrap().unwrap();
        let failure_reference = failed
            .last_failure
            .clone()
            .expect("retained failure reference");
        assert_eq!(
            failed.checkpoint_reference.as_row_ref().table_id,
            SYS_CHECKPOINT_TABLE_ID
        );
        assert_eq!(
            failure_reference.as_row_ref().table_id,
            SYS_FAILURE_TABLE_ID
        );
        let retry = match state
            .stream_backend(writer)
            .apply_async(CommitIntent::Retry {
                failure: failure.identity,
                expected_version: failure.version,
                expected,
            })
            .await
            .unwrap()
        {
            CommitResult::RetryScheduled { failure } => failure,
            other => panic!("unexpected retry: {other:?}"),
        };
        assert_eq!(retry.attempts, 2);
        assert_eq!(
            state
                .stream_observation(stream.id)
                .await
                .unwrap()
                .unwrap()
                .last_failure,
            Some(failure_reference.clone())
        );
        drop(state);
        let reopened = open_state(&repo).await;
        let restored = reopened
            .stream_observation(stream.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.partition, None);
        assert_eq!(restored.last_failure, Some(failure_reference));
        let mut rows = reopened
            .connection
            .query(
                "SELECT last_failure_identity FROM sys_stream_observation WHERE stream_id = ?1",
                params![stream.id.0.to_vec()],
            )
            .await
            .unwrap();
        let identity: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        reopened
            .connection
            .execute(
                "UPDATE stream_failure SET key_id = 'cross-stream-evidence' WHERE identity_id = ?1",
                params![identity],
            )
            .await
            .unwrap();
        assert_eq!(
            reopened.stream_observation(stream.id).await,
            Err(RuntimeError::RecoveryInvalid)
        );
    }
}
