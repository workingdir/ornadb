//! Durable sys.Run and sys.Stream observations and their projections.

use crate::codec::{
    bigint_to_i64, capture_tx, decode_capture, decode_component, decode_consumer_identity,
    decode_optional_component, decode_position, decode_u64, encode_capture, fixed, now_ms,
    row_text, validate_id, validate_observation_text,
};
use crate::error::RuntimeError;
use crate::lease::{WriterLease, validate_writer_lease};
use crate::query::{QueryObservation, SysQueryProjection};
use crate::request::{
    RequestAdmissionCapability, RequestIdentity, RequestOwner, RequestState, RequestStatus,
    admission_capability_digest, request_status_tx, require_fingerprint, validate_request_identity,
};
use crate::session::ensure_session_admission_open;
use crate::stream_store::{
    decode_class, decode_code, encode_class, encode_code, ensure_stream_checkpoint,
    load_stream_failure_by_id, stream_identity_id, stream_key_id,
};
use crate::{RuntimeState, opaque_reference_key, row_reference_raw};
use libsql::{Connection, TransactionBehavior, params};
use orna_foundation::{
    CheckpointRef, CwdCapture, FailureRef, InvocationRef, OvbRaw, RowRef, RunRef, SYS_RUN_TABLE_ID,
    SYS_STREAM_TABLE_ID, SnapshotRef, StreamRef, Value, checkpoint_reference, failure_reference,
    invocation_reference, snapshot_reference, validate_checkpoint_reference,
    validate_failure_reference, validate_invocation_reference, validate_run_reference,
    validate_stream_reference,
};
use orna_stream::{
    CheckpointKey, CommitResult, ConsumerIdentity, DeliveryIdentity, FailureIdentity,
    FailureRecord, SafeDiagnostic, StreamStatus,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// An owner-fenced observation instant for the current runtime generation.
///
/// Values of this type can only be obtained from [`RuntimeState`]. Reusing a
/// fence after its writer lease is replaced or its captured generation changes
/// fails closed rather than silently treating retained rows as current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeObservationFence {
    lease: WriterLease,
    capture: CwdCapture,
}

impl RuntimeObservationFence {
    pub fn writer_lease(&self) -> WriterLease {
        self.lease
    }

    pub fn capture(&self) -> &CwdCapture {
        &self.capture
    }
}

/// One coherent, owner-fenced view of the current-runtime Run and Stream
/// subsets. The contained rows remain the durable retained projections; this
/// type only establishes their membership in this observation instant.
///
/// Invocation, function, and snapshot relation handles are intentionally not
/// added here: the runtime does not retain their physical coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeObservationView {
    pub capture: CwdCapture,
    pub queries: Vec<QueryObservation>,
    pub runs: Vec<RunObservation>,
    pub streams: Vec<StreamObservation>,
}

/// Checked, read-only current-runtime projections for the `sys.rt.runs` and
/// `sys.rt.streams` subsets.
///
/// This type preserves the one coherent observation instant established by
/// [`RuntimeObservationFence`]. It intentionally represents only the Run and
/// Stream fields for which the runtime has durable evidence; it does not
/// manufacture physical FunctionRef, ObjectRef, or InvocationRef links.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysCurrentRuntimeProjection {
    pub capture: CwdCapture,
    pub queries: Vec<SysQueryProjection>,
    pub runs: Vec<SysRunProjection>,
    pub streams: Vec<SysStreamProjection>,
}

/// Checked, read-only retained projections for the durable `sys.Run` and
/// `sys.Stream` relations.
///
/// The rows are read from one database snapshot and every Stream is bound to
/// its retained parent Run before it is exposed. This establishes only the
/// coherent durable-observation query boundary: fields requiring physical
/// FunctionRef, ObjectRef, or InvocationRef coordinates remain unavailable
/// until the runtime durably owns those coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysRetainedObservationProjection {
    pub capture: CwdCapture,
    pub queries: Vec<SysQueryProjection>,
    pub runs: Vec<SysRunProjection>,
    pub streams: Vec<SysStreamProjection>,
}

/// Runtime-owned identity for one durable `sys.Run` observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RunObservationId(pub(crate) [u8; 16]);

impl RunObservationId {
    pub fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Runtime-owned identity for one durable `sys.Stream` observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StreamObservationId(pub(crate) [u8; 16]);

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

    /// Reconstructs this retained observation's checked `sys.InvocationRef`
    /// from the invocation identity admitted with this run and that same
    /// admission-pinned CWD capture.
    ///
    /// This is coordinate validation only. It neither proves that the
    /// invocation observation is retained nor grants authority to observe it.
    pub fn invocation_reference(&self) -> Result<InvocationRef, RuntimeError> {
        validate_id(self.invocation_id)?;
        let reference = invocation_reference(
            self.snapshot.database_id(),
            self.snapshot.snapshot().clone(),
            self.invocation_id,
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        validate_invocation_reference(reference.into_row_ref(), &self.snapshot)
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
    // Loader-owned causal evidence for `last_failure`. A valid FailureRef
    // shape alone does not establish that it belongs to this stream's durable
    // delivery identity, so public field mutation must not rebind the
    // projection to another failure in the same snapshot.
    last_failure_evidence: Option<FailureRef>,
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

/// Checked, durable-field-only shape of one `sys.Run` observation.
///
/// This is deliberately a partial relation DTO: the runtime retains no
/// physical `FunctionRef` coordinates, so that schema field is not represented
/// here. `snapshot` and `invocation` are reconstructed only from the durable
/// run capture and admission identity; they cannot be rebound to the caller's
/// later CWD.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysRunProjection {
    pub reference: RunRef,
    pub id: [u8; 16],
    pub consumer_identity: ConsumerIdentity,
    pub source_identity: Option<String>,
    pub snapshot: SnapshotRef,
    pub invocation: InvocationRef,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub status_at_snapshot: RunObservationStatus,
    pub observed_at: SystemTime,
    pub runtime_id: Option<[u8; 16]>,
    /// Derived from the retained observation. Current-runtime projection
    /// rejects rows for which this is false.
    pub live: bool,
    pub checkpoint_count: u64,
    pub failure: Option<SafeDiagnostic>,
}

impl TryFrom<&RunObservation> for SysRunProjection {
    type Error = RuntimeError;

    fn try_from(observation: &RunObservation) -> Result<Self, Self::Error> {
        let reference = observation.reference()?;
        let invocation = observation.invocation_reference()?;
        let snapshot = snapshot_reference(
            observation.snapshot.database_id(),
            observation.snapshot.snapshot().clone(),
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let started = observation_instant(observation.started_ms)?;
        let observed_at = observation_instant(observation.observed_ms)?;
        if observed_at < started {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let ended = observation.ended_ms.map(observation_instant).transpose()?;
        if observation.status.is_terminal() != ended.is_some()
            || ended.is_some_and(|ended| ended < started || observed_at < ended)
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        if observation.live && observation.status.is_terminal() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        if observation.status == RunObservationStatus::Failed && observation.diagnostic.is_none() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(Self {
            reference,
            id: observation.id.0,
            consumer_identity: observation.consumer_identity.clone(),
            source_identity: observation.source_identity.clone(),
            snapshot,
            invocation,
            started,
            ended,
            status_at_snapshot: observation.status,
            observed_at,
            runtime_id: Some(observation.runtime_id),
            live: observation.live,
            checkpoint_count: observation.checkpoint_count,
            failure: observation.diagnostic,
        })
    }
}

/// Checked, durable-field-only shape of one `sys.Stream` observation.
///
/// `producer` and `consumer` are omitted because the runtime only retains
/// names, not the physical `ObjectRef`/`FunctionRef` coordinates required by
/// the contract. This DTO is retained-observation data and does not establish
/// membership in `sys.rt.streams`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysStreamProjection {
    pub reference: StreamRef,
    pub run: RunRef,
    pub consumer_identity: ConsumerIdentity,
    pub source_identity: String,
    pub partition: Option<String>,
    pub status_at_snapshot: StreamObservationStatus,
    pub items_seen: u64,
    pub items_committed: u64,
    pub items_failed: u64,
    pub checkpoint: Option<CheckpointRef>,
    pub last_item_at: Option<SystemTime>,
    pub last_failure: Option<FailureRef>,
    pub last_diagnostic: Option<SafeDiagnostic>,
    pub observed_at: SystemTime,
    /// Derived from the retained observation. Current-runtime projection
    /// rejects rows for which this is false.
    pub live: bool,
}

impl SysStreamProjection {
    pub fn try_from_observation(
        observation: &StreamObservation,
        run: &RunObservation,
    ) -> Result<Self, RuntimeError> {
        // A closed stream has no further delivery, retry, skip or replay
        // transition. A failed delivery is not closed: it stays open for an
        // authorised retry, skip or replay, so it is live until acknowledged.
        let stream_is_closed = matches!(
            observation.status,
            StreamObservationStatus::Completed
                | StreamObservationStatus::Cancelled
                | StreamObservationStatus::Orphaned
        );
        // Run end orphans in-flight streams but leaves a failed delivery
        // open, so a terminal run may still retain a failed stream.
        let stream_is_settled_by_run_end =
            stream_is_closed || observation.status == StreamObservationStatus::Failed;
        if (run.status.is_terminal() && !stream_is_settled_by_run_end)
            || (observation.live && (!run.live || stream_is_closed))
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        // A terminal delivery failure is written atomically with both the
        // causal failure identity and a redacted diagnostic. Do not expose a
        // retained row that claims the terminal state without that evidence.
        if observation.status == StreamObservationStatus::Failed
            && (observation.last_failure.is_none() || observation.diagnostic.is_none())
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        if observation.last_failure != observation.last_failure_evidence {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let run_reference = run.reference()?;
        let reference = observation.reference(run)?;
        validate_checkpoint_reference(
            observation.checkpoint_reference.as_row_ref().clone(),
            &run.snapshot,
        )
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
        if let Some(failure) = &observation.last_failure {
            validate_failure_reference(failure.as_row_ref().clone(), &run.snapshot)
                .map_err(|_| RuntimeError::InvalidObservationReference)?;
        }
        if observation
            .items_committed
            .saturating_add(observation.items_failed)
            > observation.items_seen
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let observed_at = observation_instant(observation.observed_ms)?;
        let last_item_at = observation
            .last_item_ms
            .map(observation_instant)
            .transpose()?;
        if last_item_at.is_some_and(|last_item_at| last_item_at > observed_at) {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(Self {
            reference,
            run: run_reference,
            consumer_identity: observation.consumer_identity.clone(),
            source_identity: observation.source_identity.clone(),
            partition: observation.partition.clone(),
            status_at_snapshot: observation.status,
            items_seen: observation.items_seen,
            items_committed: observation.items_committed,
            items_failed: observation.items_failed,
            checkpoint: Some(observation.checkpoint_reference.clone()),
            last_item_at,
            last_failure: observation.last_failure.clone(),
            last_diagnostic: observation.diagnostic,
            observed_at,
            live: observation.live,
        })
    }
}

pub(crate) fn project_current_runtime_observations(
    view: RuntimeObservationView,
) -> Result<SysCurrentRuntimeProjection, RuntimeError> {
    if view.runs.iter().any(|run| !run.live) || view.streams.iter().any(|stream| !stream.live) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let retained = project_retained_observations(view)?;
    Ok(SysCurrentRuntimeProjection {
        capture: retained.capture,
        queries: retained.queries,
        runs: retained.runs,
        streams: retained.streams,
    })
}

pub(crate) fn project_retained_observations(
    view: RuntimeObservationView,
) -> Result<SysRetainedObservationProjection, RuntimeError> {
    let queries = crate::query::project_query_observations(&view.queries)?;
    let runs = view
        .runs
        .iter()
        .map(SysRunProjection::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let streams = view
        .streams
        .iter()
        .map(|stream| {
            let run = view
                .runs
                .iter()
                .find(|run| run.id == stream.run)
                .ok_or(RuntimeError::ObservationCoordinateMismatch)?;
            SysStreamProjection::try_from_observation(stream, run)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SysRetainedObservationProjection {
        capture: view.capture,
        queries,
        runs,
        streams,
    })
}

pub(crate) fn observation_instant(milliseconds: i64) -> Result<SystemTime, RuntimeError> {
    let duration = Duration::from_millis(milliseconds.unsigned_abs());
    if milliseconds >= 0 {
        UNIX_EPOCH
            .checked_add(duration)
            .ok_or(RuntimeError::RecoveryInvalid)
    } else {
        UNIX_EPOCH
            .checked_sub(duration)
            .ok_or(RuntimeError::RecoveryInvalid)
    }
}

impl RunObservationStatus {
    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Orphaned
        )
    }
}

pub(crate) const fn run_status_code(status: RunObservationStatus) -> i64 {
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

pub(crate) const fn stream_observation_status_code(status: StreamObservationStatus) -> i64 {
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

/// Recovers the exact retained CWD capture named by a `sys.FailureRef`.
///
/// A failure is local-durable, so its reference remains bound to the CWD
/// observation that produced it even after the runtime advances. The current
/// capture supplies this evidence only when it names the same snapshot;
/// otherwise the retained-capture ledger must supply the database, runtime,
/// generation, and digest together. A missing or inconsistent retained pin is
/// not authority to reinterpret the reference at the current CWD.
async fn retained_failure_reference_capture(
    connection: &Connection,
    reference: &FailureRef,
    current_capture: &CwdCapture,
) -> Result<CwdCapture, RuntimeError> {
    let snapshot = reference.as_row_ref().snapshot.clone();
    if snapshot == *current_capture.snapshot() {
        return Ok(current_capture.clone());
    }
    let snapshot_bytes = Value::new(snapshot.raw())
        .and_then(|value| value.encode())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let mut rows = connection
        .query(
            "SELECT database_id, runtime_id, generation, generation_digest
             FROM runtime_catalogue_capture WHERE snapshot = ?1",
            params![snapshot_bytes],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RuntimeError::RecoveryInvalid);
    };
    let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let generation_digest = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    if rows.next().await?.is_some() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let capture = CwdCapture::new(snapshot, generation_digest)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    if capture.database_id() != database_id
        || capture.runtime_id() != runtime_id
        || bigint_to_i64(capture.generation())? != generation
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(capture)
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
        .await?;
    let mut observations = Vec::new();
    while let Some(row) = rows.next().await? {
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

/// Loads the one durable Run that proves a request's admission capture.
///
/// A Running request without exactly one matching observation is not safe to
/// continue: the runtime cannot tell which capture should pin evaluation.
pub(crate) async fn load_run_observation_for_request_tx(
    connection: &Connection,
    identity: RequestIdentity,
    capture: &CwdCapture,
) -> Result<Option<RunObservation>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT run_id FROM sys_run_observation
             WHERE session_id = ?1 AND request_id = ?2
             ORDER BY started_ms, run_id",
            params![identity.session_id.to_vec(), identity.request_id.to_vec()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let id = RunObservationId(fixed(
        row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    if rows.next().await?.is_some() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    drop(rows);
    load_run_observation_tx(connection, id, capture).await
}

pub(crate) async fn load_run_observation_tx(
    connection: &Connection,
    id: RunObservationId,
    capture: &CwdCapture,
) -> Result<Option<RunObservation>, RuntimeError> {
    let mut rows = connection.query("SELECT session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class FROM sys_run_observation WHERE run_id = ?1", params![id.0.to_vec()]).await?;
    let Some(row) = rows.next().await? else {
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
    let started_ms: i64 = row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let ended_ms: Option<i64> = row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let observed_ms: i64 = row.get(12).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if observed_ms < started_ms
        || status.is_terminal() != ended_ms.is_some()
        || ended_ms.is_some_and(|ended| ended < started_ms || observed_ms < ended)
        || (status == RunObservationStatus::Failed && diagnostic.is_none())
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let request = RequestIdentity {
        session_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        request_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
    };
    // A retained run is lifecycle evidence for exactly one request, rather
    // than an independently mutable status record.  Keep the ordinary
    // evaluator-failure mapping explicit: its request is completed while the
    // observation retains the distinct Failed status and safe diagnostic.
    let request_status = request_status_tx(connection, request)
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let lifecycle_matches = match request_status.state {
        RequestState::Reserved => status == RunObservationStatus::Starting,
        RequestState::Running => status == RunObservationStatus::Running,
        RequestState::Completed => matches!(
            status,
            RunObservationStatus::Completed | RunObservationStatus::Failed
        ),
        RequestState::Cancelled => status == RunObservationStatus::Cancelled,
        RequestState::Orphaned => status == RunObservationStatus::Orphaned,
    };
    if !lifecycle_matches {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let live = snapshot == *capture && !status.is_terminal();
    Ok(Some(RunObservation {
        id,
        request,
        consumer_identity: decode_consumer_identity(&row_text(&row, 2)?)?,
        function: row_text(&row, 3)?,
        source_identity: row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
        snapshot,
        runtime_id,
        invocation_id: fixed(row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        started_ms,
        ended_ms,
        observed_ms,
        status,
        runtime_generation: generation,
        checkpoint_count: decode_u64(row.get(14).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        diagnostic,
        live,
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
        .await?;
    let mut observations = Vec::new();
    while let Some(row) = rows.next().await? {
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

pub(crate) async fn stream_partition_required(
    connection: &libsql::Transaction,
    table: &str,
) -> Result<bool, RuntimeError> {
    let statement = match table {
        "stream_checkpoint" => "PRAGMA table_info(stream_checkpoint)",
        "stream_failure" => "PRAGMA table_info(stream_failure)",
        _ => return Err(RuntimeError::RecoveryInvalid),
    };
    let mut rows = connection.query(statement, ()).await?;
    while let Some(row) = rows.next().await? {
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
    let mut rows = connection.query("SELECT observation.run_id, observation.producer, observation.consumer_name, observation.status, observation.items_seen, observation.items_committed, observation.items_failed, observation.diagnostic_code, observation.diagnostic_class, checkpoint.consumer_principal, checkpoint.consumer_root, checkpoint.consumer_function, checkpoint.consumer_binding, checkpoint.source_format, checkpoint.source, checkpoint.partition_format, checkpoint.partition, checkpoint.position_format, run.runtime_id, run.runtime_generation, run.status, observation.consumer_identity, observation.source_identity, observation.partition, observation.last_item_ms, observation.observed_ms, observation.last_failure_identity, observation.checkpoint_key_id FROM sys_stream_observation AS observation JOIN stream_checkpoint AS checkpoint ON checkpoint.key_id = observation.checkpoint_key_id JOIN sys_run_observation AS run ON run.run_id = observation.run_id WHERE observation.stream_id = ?1", params![id.0.to_vec()]).await?;
    let Some(row) = rows.next().await? else {
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
    validate_stream_parent_run_evidence(run_status, runtime_id, generation, &parent_run)?;
    let partition: Option<String> = row.get(23).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let checkpoint_partition = checkpoint
        .partition
        .as_ref()
        .map(|value| value.as_str().to_owned());
    let observation_consumer = decode_consumer_identity(&row_text(&row, 21)?)?;
    let observation_source = row_text(&row, 22)?;
    if observation_consumer != checkpoint.consumer
        || observation_source != checkpoint.source.as_str()
        || row_text(&row, 27)? != stream_key_id(&checkpoint)
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
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
    let items_seen = decode_u64(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let items_committed = decode_u64(row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let items_failed = decode_u64(row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let last_item_ms: Option<i64> = row.get(24).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let observed_ms: i64 = row.get(25).map_err(|_| RuntimeError::RecoveryInvalid)?;
    if items_committed.saturating_add(items_failed) > items_seen
        || last_item_ms.is_some_and(|last_item| last_item > observed_ms)
        || (status == StreamObservationStatus::Failed
            && (last_failure.is_none() || diagnostic.is_none()))
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let live = parent_capture == *capture
        && !run_status.is_terminal()
        && !matches!(
            status,
            StreamObservationStatus::Completed
                | StreamObservationStatus::Cancelled
                | StreamObservationStatus::Orphaned
        );
    Ok(Some(StreamObservation {
        id,
        run: parent_run_id,
        producer: row_text(&row, 1)?,
        consumer: row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
        consumer_identity: observation_consumer,
        source_identity: observation_source,
        partition: partition.clone(),
        partition_evidence: StreamPartitionEvidence {
            observation: partition,
            checkpoint: checkpoint_partition,
        },
        parent_capture,
        checkpoint_reference,
        checkpoint,
        last_failure: last_failure.clone(),
        last_failure_evidence: last_failure,
        status,
        items_seen,
        items_committed,
        items_failed,
        last_item_ms,
        diagnostic,
        observed_ms,
        live,
    }))
}

/// A stream's joined parent fields are evidence for the same retained Run that
/// was independently decoded above.  Do not mix an older joined lifecycle
/// state with a newer Run observation when a concurrent recovery update races
/// this read.
pub(crate) fn validate_stream_parent_run_evidence(
    joined_status: RunObservationStatus,
    joined_runtime_id: [u8; 16],
    joined_generation: i64,
    parent_run: &RunObservation,
) -> Result<(), RuntimeError> {
    if joined_status != parent_run.status
        || joined_runtime_id != parent_run.runtime_id
        || joined_generation != parent_run.runtime_generation
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(())
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
        ?;
    let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
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

pub(crate) async fn sync_stream_observation_tx(
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
            None,
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
            None,
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
            None,
            0,
            0,
            0,
            Some(failure.diagnostic),
            None,
            Some(stream_identity_id(&failure.identity)),
            false,
        ),
        CommitResult::ReplayCancelled { failure } => (
            failure.identity.0.checkpoint_key(),
            None,
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
                 WHERE run_id IN (
                     SELECT observation.run_id
                     FROM sys_stream_observation AS observation
                     JOIN sys_run_observation AS run ON run.run_id = observation.run_id
                     WHERE observation.checkpoint_key_id = ?1
                       AND run.status IN (?2, ?3) AND run.ended_ms IS NULL
                 )",
                params![
                    stream_key_id(&key),
                    run_status_code(RunObservationStatus::Starting),
                    run_status_code(RunObservationStatus::Running),
                ],
            )
            .await?;
    }
    Ok(())
}

pub(crate) async fn sync_stream_observation_event_tx(
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
    let key_id = stream_key_id(key);
    let changed = connection.execute("UPDATE sys_stream_observation SET status = COALESCE(?2, status), items_seen = items_seen + ?3, items_committed = items_committed + ?4, items_failed = items_failed + ?5, checkpoint_version = COALESCE(?6, checkpoint_version), last_failure_identity = COALESCE(?7, last_failure_identity), last_item_ms = CASE WHEN ?3 + ?4 + ?5 > 0 THEN ?8 ELSE last_item_ms END, diagnostic_code = COALESCE(?9, diagnostic_code), diagnostic_class = COALESCE(?10, diagnostic_class), observed_ms = ?8 WHERE checkpoint_key_id = ?1 AND status NOT IN (?11, ?12, ?13) AND run_id IN (SELECT run_id FROM sys_run_observation WHERE status IN (?14, ?15) AND ended_ms IS NULL)", params![key_id.clone(), update.status.map(stream_observation_status_code), update.seen, update.committed, update.failed, update.checkpoint.map(|value| i64::try_from(value).map_err(|_| RuntimeError::RecoveryInvalid)).transpose()?, update.last_failure, now_ms()?, code, class, stream_observation_status_code(StreamObservationStatus::Completed), stream_observation_status_code(StreamObservationStatus::Cancelled), stream_observation_status_code(StreamObservationStatus::Orphaned), run_status_code(RunObservationStatus::Starting), run_status_code(RunObservationStatus::Running)]).await?;
    if changed != 0 {
        return Ok(changed);
    }

    let mut retained = connection
        .query(
            "SELECT 1 FROM sys_stream_observation WHERE checkpoint_key_id = ?1",
            params![key_id],
        )
        .await?;
    if retained.next().await?.is_some() {
        // A retained observation must never turn an orphaned/completed
        // stream back into a live state, and no delivery may advance after
        // its parent run has reached a terminal boundary. The enclosing
        // transaction rolls back the checkpoint and control mutation too.
        return Err(RuntimeError::RequestStateConflict);
    }
    Ok(0)
}

pub(crate) async fn sync_run_request_state_tx(
    connection: &Connection,
    identity: RequestIdentity,
    status: RunObservationStatus,
) -> Result<u64, RuntimeError> {
    sync_run_request_state_with_diagnostic_tx(connection, identity, status, None).await
}

pub(crate) async fn sync_run_request_state_with_diagnostic_tx(
    connection: &Connection,
    identity: RequestIdentity,
    status: RunObservationStatus,
    diagnostic: Option<SafeDiagnostic>,
) -> Result<u64, RuntimeError> {
    let now = now_ms()?;
    let code = diagnostic.map(|value| encode_code(value.code));
    let class = diagnostic.map(|value| encode_class(value.class));
    let changed = connection.execute("UPDATE sys_run_observation SET status = ?1, ended_ms = CASE WHEN ?2 THEN ?3 ELSE ended_ms END, observed_ms = ?3, diagnostic_code = COALESCE(?4, diagnostic_code), diagnostic_class = COALESCE(?5, diagnostic_class) WHERE session_id = ?6 AND request_id = ?7 AND status IN (?8, ?9)", params![run_status_code(status), status.is_terminal(), now, code, class, identity.session_id.to_vec(), identity.request_id.to_vec(), run_status_code(RunObservationStatus::Starting), run_status_code(RunObservationStatus::Running)]).await?;
    if status.is_terminal() {
        connection.execute("UPDATE sys_stream_observation SET status = ?1, observed_ms = ?2 WHERE run_id IN (SELECT run_id FROM sys_run_observation WHERE session_id = ?3 AND request_id = ?4) AND status IN (?5, ?6, ?7, ?8)", params![stream_observation_status_code(StreamObservationStatus::Orphaned), now, identity.session_id.to_vec(), identity.request_id.to_vec(), stream_observation_status_code(StreamObservationStatus::Starting), stream_observation_status_code(StreamObservationStatus::Running), stream_observation_status_code(StreamObservationStatus::Paused), stream_observation_status_code(StreamObservationStatus::BackingOff)]).await?;
    }
    Ok(changed)
}

impl RuntimeState {
    /// Resolves one public `sys.FailureRef` to the complete retained delivery
    /// identity.  The reference supplies only its public natural key; source
    /// and partition formats plus the successor position are recovered from
    /// the durable failure row and are never reconstructed by a caller.
    ///
    /// A public reference that is stale, absent, or ambiguous is not an
    /// authority to select a row.  In particular, if two retained physical
    /// rows share the public key but differ in their hidden provider formats,
    /// this returns `RecoveryInvalid` rather than choosing either row.
    pub async fn resolve_failure_reference(
        &self,
        reference: &FailureRef,
    ) -> Result<Option<FailureRecord>, RuntimeError> {
        let current_capture = self.capture().await?;
        let capture =
            retained_failure_reference_capture(&self.connection, reference, &current_capture)
                .await?;
        let reference = validate_failure_reference(reference.as_row_ref().clone(), &capture)
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let OvbRaw::Array(key) = &reference.as_row_ref().key else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let [consumer, source, partition, position_format, position] = key.as_slice() else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let OvbRaw::Text(consumer) = consumer else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let consumer = decode_consumer_identity(consumer)?;
        let OvbRaw::Text(source) = source else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let partition = match partition {
            OvbRaw::Null => None,
            OvbRaw::Text(partition) => Some(partition.as_str()),
            _ => return Err(RuntimeError::InvalidObservationReference),
        };
        let OvbRaw::Text(position_format) = position_format else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let OvbRaw::Text(position) = position else {
            return Err(RuntimeError::InvalidObservationReference);
        };
        let mut rows = self
            .connection
            .query(
                "SELECT identity_id FROM stream_failure
                 WHERE consumer_principal = ?1 AND consumer_root = ?2
                   AND consumer_function = ?3 AND consumer_binding = ?4
                   AND source = ?5 AND partition IS ?6
                   AND position_format = ?7 AND delivery_position = ?8",
                params![
                    consumer.principal.as_str(),
                    consumer.root.as_str(),
                    consumer.function.as_str(),
                    consumer.binding.as_str(),
                    source.as_str(),
                    partition,
                    position_format.as_str(),
                    position.as_str(),
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let identity = row_text(&row, 0)?;
        if rows.next().await?.is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let record = load_stream_failure_by_id(&self.connection, identity).await?;
        let Some(record) = record else {
            return Err(RuntimeError::RecoveryInvalid);
        };
        if record.identity.0.consumer != consumer
            || record.identity.0.source.as_str() != source.as_str()
            || record
                .identity
                .0
                .partition
                .as_ref()
                .map(|value| value.as_str())
                != partition
            || record.identity.0.position_format.as_str() != position_format.as_str()
            || record.identity.0.position.token.as_str() != position.as_str()
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(Some(record))
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
            .await?;
        self.require_recovery_barrier_clear(&tx).await?;
        let request = request_status_tx(&tx, registration.request)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        if request.state != RequestState::Reserved {
            return Err(RuntimeError::RequestStateConflict);
        }
        let mut existing_rows = tx
            .query(
                "SELECT 1 FROM sys_run_observation \
                 WHERE session_id = ?1 AND request_id = ?2",
                params![
                    registration.request.session_id.to_vec(),
                    registration.request.request_id.to_vec(),
                ],
            )
            .await?;
        if existing_rows.next().await?.is_some() {
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
        ).await?;
        tx.commit().await?;
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
        self.require_lease_authority()?;
        validate_observation_text(&registration.function)?;
        if let Some(source) = &registration.source_identity {
            validate_observation_text(source)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        if let Some(status) = request_status_tx(&tx, registration.request).await? {
            require_fingerprint(&status, fingerprint)?;
            if status.state.is_terminal() {
                // This is the observed lifecycle entrypoint. A terminal replay
                // without its retained Run witness is not evidence that this
                // request was ever admitted through the observed boundary.
                // Validate the witness in the same read transaction before
                // returning the replay and never manufacture one from the
                // caller's new registration metadata.
                let capture = capture_tx(&tx).await?;
                load_run_observation_for_request_tx(&tx, registration.request, &capture)
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                tx.commit().await?;
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
        ?;
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
        ?;
        tx.commit().await?;
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

    /// Atomically consumes a fresh request admission capability, starts the
    /// owner-fenced ledger row, and creates its `sys.Run` observation. This is
    /// the live durable admission boundary: splitting these operations would
    /// let a capability start succeed while the existing observation API
    /// rejects the already-Running request.
    pub async fn begin_observed_request_with_admission(
        &self,
        registration: RunObservationRegistration,
        fingerprint: [u8; 32],
        owner: WriterLease,
        capability: RequestAdmissionCapability,
    ) -> Result<ObservedRequestStart, RuntimeError> {
        validate_request_identity(registration.request)?;
        validate_id(registration.invocation_id)?;
        validate_writer_lease(owner)?;
        self.require_lease_authority()?;
        validate_observation_text(&registration.function)?;
        if let Some(source) = &registration.source_identity {
            validate_observation_text(source)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        ensure_session_admission_open(&tx, registration.request.session_id).await?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, registration.request)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Reserved {
            return Err(RuntimeError::RequestStateConflict);
        }
        if tx
            .query(
                "SELECT 1 FROM sys_run_observation
                 WHERE session_id = ?1 AND request_id = ?2",
                params![
                    registration.request.session_id.to_vec(),
                    registration.request.request_id.to_vec()
                ],
            )
            .await?
            .next()
            .await?
            .is_some()
        {
            return Err(RuntimeError::RequestStateConflict);
        }
        let capture = capture_tx(&tx).await?;
        let generation = bigint_to_i64(capture.generation())?;
        let token_digest = admission_capability_digest(&capability);
        let mut admissions = tx
            .query(
                "SELECT fingerprint, capability_digest, owner_id, owner_epoch,
                        generation
                 FROM request_admission
                 WHERE session_id = ?1 AND request_id = ?2",
                params![
                    registration.request.session_id.to_vec(),
                    registration.request.request_id.to_vec()
                ],
            )
            .await?;
        let Some(row) = admissions.next().await? else {
            return Err(RuntimeError::RequestStateConflict);
        };
        let admitted_fingerprint = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let admitted_digest = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let admitted_owner = RequestOwner {
            owner_id: fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            epoch: u64::try_from(
                row.get::<i64>(3)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
        };
        let admitted_generation: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if admitted_fingerprint != fingerprint {
            return Err(RuntimeError::RequestFingerprintMismatch);
        }
        if admitted_owner != RequestOwner::from(owner) {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        if admitted_generation != generation || admitted_digest != token_digest {
            return Err(RuntimeError::RequestStateConflict);
        }
        let changed = tx
            .execute(
                "UPDATE request_ledger
                 SET state = ?1, owner_id = ?2, owner_epoch = ?3,
                     effect_evidence = 0, recovery_disposition = 0,
                     controlled_transaction_proof = NULL,
                     controlled_rollback_proof = NULL
                 WHERE session_id = ?4 AND request_id = ?5
                   AND fingerprint = ?6 AND state = ?7",
                params![
                    RequestState::Running.code(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    registration.request.session_id.to_vec(),
                    registration.request.request_id.to_vec(),
                    fingerprint.to_vec(),
                    RequestState::Reserved.code(),
                ],
            )
            .await?;
        if changed != 1 {
            return Err(RuntimeError::RequestStateConflict);
        }
        let consumed = tx
            .execute(
                "DELETE FROM request_admission
                 WHERE session_id = ?1 AND request_id = ?2 AND fingerprint = ?3
                   AND capability_digest = ?4 AND owner_id = ?5
                   AND owner_epoch = ?6 AND generation = ?7",
                params![
                    registration.request.session_id.to_vec(),
                    registration.request.request_id.to_vec(),
                    fingerprint.to_vec(),
                    token_digest.to_vec(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    generation,
                ],
            )
            .await?;
        if consumed != 1 {
            return Err(RuntimeError::RequestStateConflict);
        }
        let id = RunObservationId(*Uuid::new_v4().as_bytes());
        tx.execute(
            "INSERT INTO sys_run_observation (run_id, session_id, request_id, consumer_identity, function_name, source_identity, invocation_id, snapshot, generation_digest, runtime_id, runtime_generation, started_ms, ended_ms, observed_ms, status, checkpoint_count, diagnostic_code, diagnostic_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?12, ?13, 0, NULL, NULL)",
            params![
                id.0.to_vec(), registration.request.session_id.to_vec(), registration.request.request_id.to_vec(),
                registration.consumer_identity.canonical(), registration.function, registration.source_identity,
                registration.invocation_id.to_vec(), encode_capture(&capture)?, capture.generation_digest().to_vec(), capture.runtime_id().to_vec(),
                bigint_to_i64(capture.generation())?, now_ms()?, run_status_code(RunObservationStatus::Running),
            ],
        ).await?;
        tx.commit().await?;
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

    /// Registers exactly one durable `sys.Stream` observation for this run.
    /// A terminal predecessor may retain the same durable checkpoint, but a
    /// live predecessor remains an identity conflict.
    pub async fn register_stream_observation(
        &self,
        registration: StreamObservationRegistration,
    ) -> Result<StreamObservation, RuntimeError> {
        self.register_stream_observation_inner(None, registration)
            .await
    }

    /// Registers a durable `sys.Stream` observation while retaining the
    /// caller's writer fence through the registration transaction.
    ///
    /// New activation paths must use this boundary. The compatibility
    /// wrapper above remains available for existing administrative callers,
    /// but it cannot protect a registration against owner replacement
    /// between lease acquisition and insertion.
    pub async fn register_stream_observation_with_owner(
        &self,
        writer: WriterLease,
        registration: StreamObservationRegistration,
    ) -> Result<StreamObservation, RuntimeError> {
        self.register_stream_observation_inner(Some(writer), registration)
            .await
    }

    async fn register_stream_observation_inner(
        &self,
        writer: Option<WriterLease>,
        registration: StreamObservationRegistration,
    ) -> Result<StreamObservation, RuntimeError> {
        self.require_lease_authority()?;
        validate_observation_text(&registration.producer)?;
        if let Some(consumer) = &registration.consumer {
            validate_observation_text(consumer)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_recovery_barrier_clear(&tx).await?;
        if let Some(writer) = writer {
            self.require_owner(&tx, writer).await?;
        }
        let capture = capture_tx(&tx).await?;
        let run = load_run_observation_tx(&tx, registration.run, &capture)
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        if run.status.is_terminal() || run.runtime_id != capture.runtime_id() {
            return Err(RuntimeError::RequestStateConflict);
        }
        ensure_stream_checkpoint(&tx, &registration.checkpoint).await?;
        let key_id = stream_key_id(&registration.checkpoint);
        let mut existing = tx
            .query(
                "SELECT observation.run_id, run.status, run.ended_ms
                 FROM sys_stream_observation AS observation
                 JOIN sys_run_observation AS run ON run.run_id = observation.run_id
                 WHERE observation.checkpoint_key_id = ?1",
                params![key_id.clone()],
            )
            .await?;
        while let Some(row) = existing.next().await? {
            let existing_run = RunObservationId(fixed(
                row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?);
            let status = decode_run_status(
                row.get::<i64>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            let ended_ms: Option<i64> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if existing_run == registration.run {
                return Err(RuntimeError::RequestStateConflict);
            }
            if !status.is_terminal() || ended_ms.is_none() {
                return Err(RuntimeError::StreamIdentityMismatch);
            }
        }
        let partition = registration
            .checkpoint
            .partition
            .as_ref()
            .map(|value| value.as_str().to_owned());
        let mut natural_key = tx
            .query(
                "SELECT checkpoint_key_id FROM sys_stream_observation
                 WHERE run_id = ?1 AND source_identity = ?2 AND partition IS ?3",
                params![
                    registration.run.0.to_vec(),
                    registration.checkpoint.source.as_str(),
                    partition.clone(),
                ],
            )
            .await?;
        if natural_key.next().await?.is_some() {
            return Err(RuntimeError::StreamIdentityMismatch);
        }
        let id = StreamObservationId(*Uuid::new_v4().as_bytes());
        tx.execute(
            "INSERT INTO sys_stream_observation (stream_id, run_id, checkpoint_key_id, producer, consumer_name, consumer_identity, source_identity, partition, status, items_seen, items_committed, items_failed, checkpoint_version, last_failure_identity, last_item_ms, diagnostic_code, diagnostic_class, observed_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, 0, NULL, NULL, NULL, NULL, NULL, ?10)",
            params![id.0.to_vec(), registration.run.0.to_vec(), key_id, registration.producer, registration.consumer,
                registration.checkpoint.consumer.canonical(), registration.checkpoint.source.as_str().to_owned(), partition,
                stream_observation_status_code(StreamObservationStatus::Starting), now_ms()?],
        ).await?;
        tx.commit().await?;
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

    /// Reads one coherent, checked, retained projection of the durable
    /// `sys.Run` and `sys.Stream` relations without mutating runtime state.
    ///
    /// The captured database state and both relation reads occur in one
    /// transaction. A Stream whose durable parent Run is absent or whose
    /// checked natural-key reference does not agree with that parent fails
    /// closed rather than being exposed as an orphaned relation row.
    pub async fn retained_sys_observation_projections(
        &self,
    ) -> Result<SysRetainedObservationProjection, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let capture = capture_tx(&tx).await?;
        let queries = crate::query::load_query_observations(&tx).await?;
        let runs = load_run_observations(&tx, &capture).await?;
        let streams = load_stream_observations(&tx, &capture).await?;
        tx.commit().await?;
        project_retained_observations(RuntimeObservationView {
            capture,
            queries,
            runs,
            streams,
        })
    }

    /// Verifies `lease` and captures the current runtime generation in one
    /// immediate transaction. Pass the returned fence to
    /// [`Self::current_runtime_observations`] to prevent an owner or
    /// generation change from being observed as a current-runtime row.
    pub async fn runtime_observation_fence(
        &self,
        lease: WriterLease,
    ) -> Result<RuntimeObservationFence, RuntimeError> {
        validate_writer_lease(lease)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, lease).await?;
        let capture = capture_tx(&tx).await?;
        tx.commit().await?;
        Ok(RuntimeObservationFence { lease, capture })
    }

    /// Reads one owner-fenced current-runtime view over the same retained
    /// `sys.Run` and `sys.Stream` projections.
    ///
    /// The writer lease and generation captured in `fence` are rechecked in
    /// the same immediate transaction as both projection reads. Rows from an
    /// earlier generation remain available through the retained APIs but are
    /// excluded here.
    pub async fn current_runtime_observations(
        &self,
        fence: &RuntimeObservationFence,
    ) -> Result<RuntimeObservationView, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, fence.lease).await?;
        let capture = capture_tx(&tx).await?;
        if capture != fence.capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(capture),
            });
        }
        let queries = crate::query::load_query_observations(&tx)
            .await?
            .into_iter()
            .filter(|query| query.runtime_id == capture.runtime_id())
            .collect();
        let runs = load_run_observations(&tx, &capture)
            .await?
            .into_iter()
            .filter(|row| row.live)
            .collect();
        let streams = load_stream_observations(&tx, &capture)
            .await?
            .into_iter()
            .filter(|row| row.live)
            .collect();
        tx.commit().await?;
        Ok(RuntimeObservationView {
            capture,
            queries,
            runs,
            streams,
        })
    }

    /// Reads checked, durable-field-only projections for the current runtime
    /// Run and Stream subsets without starting, resuming, recovering, or
    /// otherwise mutating an activation.
    ///
    /// The supplied fence is rechecked through
    /// [`Self::current_runtime_observations`] before any projection occurs.
    /// Every current Stream must have its retained parent Run in that same
    /// fenced view; missing or malformed retained coordinates fail closed.
    pub async fn current_runtime_sys_projections(
        &self,
        fence: &RuntimeObservationFence,
    ) -> Result<SysCurrentRuntimeProjection, RuntimeError> {
        project_current_runtime_observations(self.current_runtime_observations(fence).await?)
    }

    /// Reads the owner-fenced current-runtime Run subset.
    pub async fn runtime_run_observations(
        &self,
        fence: &RuntimeObservationFence,
    ) -> Result<Vec<RunObservation>, RuntimeError> {
        Ok(self.current_runtime_observations(fence).await?.runs)
    }

    /// Reads retained `sys.Stream` observations without mutating checkpoint or lease state.
    pub async fn stream_observations(&self) -> Result<Vec<StreamObservation>, RuntimeError> {
        let capture = self.capture().await?;
        load_stream_observations(&self.connection, &capture).await
    }

    /// Reads the owner-fenced current-runtime Stream subset.
    pub async fn runtime_stream_observations(
        &self,
        fence: &RuntimeObservationFence,
    ) -> Result<Vec<StreamObservation>, RuntimeError> {
        Ok(self.current_runtime_observations(fence).await?.streams)
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

    /// Retains a writer-fenced terminal stream failure for an admitted
    /// observation whose runner cannot produce a delivery failure record.
    /// This is used only for unexpected runner boundaries; ordinary delivery
    /// failures continue through the failure-record path, which retains the
    /// exact FailureRef and payload metadata.
    pub async fn fail_stream_observation_with_owner(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        diagnostic: SafeDiagnostic,
    ) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, writer).await?;
        sync_stream_observation_event_tx(
            &transaction,
            key,
            StreamObservationStatus::Failed,
            Some(diagnostic),
        )
        .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Records a completed or paused runner boundary that has no delivery
    /// lease (for example finite exhaustion, cancellation before admission,
    /// or a pause before the next item). The observation update is still
    /// writer-fenced and committed atomically.
    pub(crate) async fn complete_stream_observation(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        status: StreamObservationStatus,
    ) -> Result<(), RuntimeError> {
        debug_assert!(matches!(
            status,
            StreamObservationStatus::Completed
                | StreamObservationStatus::Cancelled
                | StreamObservationStatus::Paused
        ));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, writer).await?;
        sync_stream_observation_event_tx(&transaction, key, status, None).await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }
}
