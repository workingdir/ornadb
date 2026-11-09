//! Stream checkpoint, lease and failure rows: codecs, loaders and the checkpoint backend.

use crate::RuntimeState;
use crate::checkpoint_snapshot::{StreamCheckpointTransition, StreamCheckpointWatermark};
use crate::codec::{
    decode_assertion_owner_kind, decode_capture, decode_component, decode_nonnegative_bigint,
    decode_optional_component, decode_position, decode_positive_bigint, decode_row_ref, decode_u32,
    decode_u64, encode_assertion_owner_kind, encode_capture, encode_nonnegative_bigint,
    encode_row_ref, fixed, row_text, validate_id,
};
use crate::error::{NoFault, RuntimeError};
use crate::lease::WriterLease;
use crate::stream_commit::StoredStreamReplayClaim;
use crate::stream_intent::apply_stream_intent;
use crate::stream_run::StreamStepError;
use crate::stream_source::{
    StreamFailurePayloadMetadata, StreamFailurePayloadProvider, StreamHandler,
    StreamProviderFailure,
};
use libsql::{Connection, params};
use orna_foundation::{AssertionRef, CwdCapture, ExpressionRef, ObjectRef, SafeText, SourceSpan};
use orna_stream::Checkpoint as StreamCheckpoint;
use orna_stream::{
    AssertionDiagnosticCode, AssertionDiagnosticDetail, AsyncCheckpointBackend,
    AsyncFailurePayloadBackend, CheckpointKey, CommitIntent, CommitResult, Component,
    ConsumerIdentity, DeliveryIdentity, DeliveryLease, DiagnosticClass, DiagnosticCode,
    FailureIdentity, FailureRecord, FailureStatus, LeasePurpose, ReplayGrant, SafeAssertionWitness,
    SafeDiagnostic, StreamFailurePayload, StreamStatus,
};
use std::future::Future;
use std::pin::Pin;

/// Writer-fenced stream administration backed by one runtime state.
pub struct RuntimeStreamBackend<'a> {
    state: &'a RuntimeState,
    lease: WriterLease,
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

    /// Finishes an immediately failed retry using the lease already acquired
    /// by [`CommitIntent::Retry`]. The retry transition and this failed
    /// transition remain separate durable transactions.
    pub async fn fail_retry_async(
        &self,
        delivery: DeliveryIdentity,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        self.state
            .fail_stream_retry_delivery(self.lease, delivery, diagnostic, payload)
            .await
    }

    /// Records a handler failure with optional assertion provenance in the
    /// same writer-fenced transaction as failure payload retention.
    pub async fn fail_with_assertion_detail_async(
        &self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        detail: AssertionDiagnosticDetail,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        self.state
            .fail_stream_delivery_with_faults(
                self.lease,
                lease,
                diagnostic,
                Some(&detail),
                payload,
                &NoFault,
            )
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

    fn fail_with_assertion_detail_and_payload_async<'a>(
        &'a mut self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        detail: AssertionDiagnosticDetail,
        payload: StreamFailurePayload,
    ) -> Pin<Box<dyn Future<Output = Result<CommitResult, RuntimeError>> + 'a>> {
        Box::pin(async move {
            self.state
                .fail_stream_delivery_with_faults(
                    self.lease,
                    lease,
                    diagnostic,
                    Some(&detail),
                    payload,
                    &NoFault,
                )
                .await
        })
    }
}

const STREAM_CHECKPOINT_HISTORY_SELECT: &str =
    "SELECT version, committed_position, snapshot, generation_digest, transition
     FROM stream_checkpoint_history WHERE key_id = ?1 ORDER BY version";

const STREAM_CHECKPOINT_SELECT: &str =
    "SELECT version, committed_position, next_fence FROM stream_checkpoint WHERE key_id = ?1";

const STREAM_FAILURE_SELECT: &str = "SELECT \
    key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, \
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
pub(crate) struct StoredStreamLease {
    pub(crate) delivery_position: String,
    pub(crate) successor_position: String,
    pub(crate) fence: u64,
    pub(crate) purpose: LeasePurpose,
}

pub(crate) fn stream_key_id(key: &CheckpointKey) -> String {
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

pub(crate) fn stream_identity_id(identity: &FailureIdentity) -> String {
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

pub(crate) fn encode_status(status: FailureStatus) -> i64 {
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

pub(crate) fn encode_code(code: DiagnosticCode) -> i64 {
    match code {
        DiagnosticCode::ProviderUnavailable => 1,
        DiagnosticCode::DecodeRejected => 2,
        DiagnosticCode::ExecutionRejected => 3,
        DiagnosticCode::Cancelled => 4,
        DiagnosticCode::Internal => 5,
        DiagnosticCode::TableAssertionFalse => 6,
        DiagnosticCode::StaleBaseline => 7,
    }
}

pub(crate) fn decode_code(value: i64) -> Result<DiagnosticCode, RuntimeError> {
    match value {
        1 => Ok(DiagnosticCode::ProviderUnavailable),
        2 => Ok(DiagnosticCode::DecodeRejected),
        3 => Ok(DiagnosticCode::ExecutionRejected),
        4 => Ok(DiagnosticCode::Cancelled),
        5 => Ok(DiagnosticCode::Internal),
        6 => Ok(DiagnosticCode::TableAssertionFalse),
        7 => Ok(DiagnosticCode::StaleBaseline),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

pub(crate) fn encode_class(class: DiagnosticClass) -> i64 {
    match class {
        DiagnosticClass::Transient => 1,
        DiagnosticClass::Permanent => 2,
        DiagnosticClass::Cancellation => 3,
    }
}

pub(crate) fn decode_class(value: i64) -> Result<DiagnosticClass, RuntimeError> {
    match value {
        1 => Ok(DiagnosticClass::Transient),
        2 => Ok(DiagnosticClass::Permanent),
        3 => Ok(DiagnosticClass::Cancellation),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

pub(crate) fn encode_purpose(purpose: LeasePurpose) -> i64 {
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

pub(crate) fn encode_stream_status(status: StreamStatus) -> i64 {
    match status {
        StreamStatus::Running => 1,
        StreamStatus::Paused => 2,
    }
}

pub(crate) fn decode_stream_status(value: i64) -> Result<StreamStatus, RuntimeError> {
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

pub(crate) async fn ensure_stream_checkpoint(
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

/// Position formats are part of the checkpoint encoding, while the natural
/// stream identity is consumer/source/partition. Reusing that stream under a
/// different position format would otherwise create a second empty row and
/// silently restart provider progress. Require an explicit migration before
/// the runner can poll such a provider.
pub(crate) async fn ensure_stream_position_format_compatible(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<(), RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM stream_checkpoint
             WHERE consumer_principal = ?1 AND consumer_root = ?2
               AND consumer_function = ?3 AND consumer_binding = ?4
               AND source_format = ?5 AND source = ?6
               AND partition_format = ?7 AND partition IS ?8
               AND position_format != ?9
             LIMIT 1",
            params![
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
    if rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some()
    {
        return Err(RuntimeError::StreamIdentityMismatch);
    }
    Ok(())
}

pub(crate) async fn load_stream_checkpoint_state(
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

pub(crate) async fn load_stream_checkpoint(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<StreamCheckpoint, RuntimeError> {
    Ok(load_stream_checkpoint_state(connection, key).await?.0)
}

pub(crate) async fn load_stream_checkpoint_history(
    connection: &Connection,
    key: &CheckpointKey,
    selected: &CwdCapture,
) -> Result<Vec<StreamCheckpointWatermark>, RuntimeError> {
    let mut rows = connection
        .query(
            STREAM_CHECKPOINT_HISTORY_SELECT,
            params![stream_key_id(key)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut history = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let version = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let committed = decode_position(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let snapshot = row
            .get::<Vec<u8>>(2)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let generation_digest = fixed(
            row.get::<Vec<u8>>(3)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let capture = decode_capture(snapshot, generation_digest)?;
        if capture.database_id() != selected.database_id()
            || capture.runtime_id() != selected.runtime_id()
            || capture.generation() > selected.generation()
        {
            continue;
        }
        let transition = match row
            .get::<i64>(4)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
        {
            1 => StreamCheckpointTransition::Complete,
            2 => StreamCheckpointTransition::Skip,
            _ => return Err(RuntimeError::RecoveryInvalid),
        };
        history.push(StreamCheckpointWatermark {
            checkpoint: StreamCheckpoint {
                key: key.clone(),
                version,
                committed: Some(committed),
            },
            capture,
            transition,
        });
    }
    Ok(history)
}

pub(crate) async fn record_stream_checkpoint_watermark_tx(
    connection: &Connection,
    key: &CheckpointKey,
    checkpoint: &StreamCheckpoint,
    capture: &CwdCapture,
    transition: StreamCheckpointTransition,
) -> Result<(), RuntimeError> {
    let Some(committed) = &checkpoint.committed else {
        return Err(RuntimeError::RecoveryInvalid);
    };
    let transition_code = match transition {
        StreamCheckpointTransition::Complete => 1_i64,
        StreamCheckpointTransition::Skip => 2_i64,
    };
    connection
        .execute(
            "INSERT INTO stream_checkpoint_history
             (key_id, version, committed_position, snapshot, generation_digest, transition)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                stream_key_id(key),
                i64::try_from(checkpoint.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                committed.token.as_str().to_owned(),
                encode_capture(capture)?,
                capture.generation_digest().to_vec(),
                transition_code,
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

pub(crate) async fn load_stream_status(
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

pub(crate) async fn store_stream_status(
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

pub(crate) async fn load_stream_pause_reason(
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

pub(crate) async fn store_stream_pause_reason(
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

pub(crate) async fn stream_pause_pending(
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
pub(crate) async fn publish_pending_stream_pause(
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

pub(crate) async fn has_blocking_stream_failure(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM stream_failure
             WHERE key_id = ?1 AND status IN (?2, ?3)
             LIMIT 1",
            params![
                stream_key_id(key),
                encode_status(FailureStatus::Failed),
                encode_status(FailureStatus::Retrying),
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

pub(crate) async fn has_ordered_blocking_stream_failure(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM stream_failure
             WHERE key_id = ?1 AND status IN (?2, ?3)
             LIMIT 1",
            params![
                stream_key_id(key),
                encode_status(FailureStatus::Failed),
                encode_status(FailureStatus::Retrying),
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

/// Verifies the durable evidence needed to reopen abandoned stream attempts.
///
/// A writer takeover fences the former runtime owner, which is the authority
/// that prevents a displaced callback from committing.  For a `Retrying`
/// delivery, the matching delivery lease is the durable evidence that the row
/// was the admitted callback. A scheduled retry retains its matching claim;
/// an acquired retry consumes that claim while retaining the lease. Without
/// either valid shape, recovery must leave all state untouched rather than
/// guessing which delivery an administrative retry represented.
/// `Replaying` has no live-checkpoint lease by design, so it is recovered only
/// when it has not been confused with a retry claim for that same failure.
pub(crate) async fn validate_recoverable_stream_attempts(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT failure.identity_id, failure.key_id, failure.delivery_position,
                    failure.successor_position, failure.status,
                    lease.delivery_position, lease.successor_position, lease.purpose,
                    claim.identity_id
             FROM stream_failure AS failure
             LEFT JOIN stream_lease AS lease ON lease.key_id = failure.key_id
             LEFT JOIN stream_retry_claim AS claim
               ON claim.key_id = failure.key_id
             WHERE failure.status IN (?1, ?2)
             ORDER BY failure.identity_id",
            params![
                encode_status(FailureStatus::Retrying),
                encode_status(FailureStatus::Replaying),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let identity: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let key: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let delivery: String = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let successor: String = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if identity.is_empty() || key.is_empty() || delivery.is_empty() || successor.is_empty() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let status = decode_status(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let leased_delivery: Option<String> =
            row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let leased_successor: Option<String> =
            row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let purpose: Option<i64> = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let claimed: Option<String> = row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?;
        match status {
            FailureStatus::Retrying => {
                if leased_delivery.as_deref() != Some(delivery.as_str())
                    || leased_successor.as_deref() != Some(successor.as_str())
                    || purpose.map(decode_purpose).transpose()? != Some(LeasePurpose::Deliver)
                    || claimed.is_some_and(|claim| claim != identity)
                {
                    return Err(RuntimeError::RecoveryInvalid);
                }
            }
            FailureStatus::Replaying => {
                if claimed.as_deref() == Some(identity.as_str()) {
                    return Err(RuntimeError::RecoveryInvalid);
                }
            }
            _ => return Err(RuntimeError::RecoveryInvalid),
        }
    }
    let mut replay_claims = connection
        .query(
            "SELECT 1
             FROM stream_replay_claim AS claim
             LEFT JOIN stream_failure AS failure
               ON failure.identity_id = claim.identity_id
             WHERE failure.identity_id IS NULL
                OR failure.status != ?1
                OR failure.version != claim.version
             LIMIT 1",
            params![encode_status(FailureStatus::Replaying)],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if replay_claims
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some()
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(())
}

pub(crate) async fn load_stream_failure(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<FailureRecord>, RuntimeError> {
    let record = load_stream_failure_by_id(connection, stream_identity_id(identity)).await?;
    match record {
        Some(record) if record.identity == *identity => Ok(Some(record)),
        Some(_) => Err(RuntimeError::RecoveryInvalid),
        None => Ok(None),
    }
}

pub(crate) async fn load_stream_failure_by_id(
    connection: &Connection,
    identity_id: String,
) -> Result<Option<FailureRecord>, RuntimeError> {
    let mut rows = connection
        .query(STREAM_FAILURE_SELECT, params![identity_id.clone()])
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
            principal: decode_component(row_text(&row, 1)?)?,
            root: decode_component(row_text(&row, 2)?)?,
            function: decode_component(row_text(&row, 3)?)?,
            binding: decode_component(row_text(&row, 4)?)?,
        },
        source_format: decode_component(row_text(&row, 5)?)?,
        source: decode_component(row_text(&row, 6)?)?,
        partition_format: decode_component(row_text(&row, 7)?)?,
        partition: decode_optional_component(
            row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        position_format: decode_component(row_text(&row, 9)?)?,
        position: decode_position(row_text(&row, 10)?)?,
        successor: decode_position(row_text(&row, 11)?)?,
    };
    let reconstructed = FailureIdentity(delivery);
    if stream_identity_id(&reconstructed) != identity_id
        || row_text(&row, 0)? != stream_key_id(&reconstructed.0.checkpoint_key())
    {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(FailureRecord {
        identity: reconstructed.clone(),
        version: decode_u64(
            row.get::<i64>(12)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        attempts: decode_u32(
            row.get::<i64>(13)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        status: decode_status(
            row.get::<i64>(14)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        diagnostic: decode_diagnostic(&row, 15)?,
        assertion_detail: load_stream_failure_assertion_detail(connection, &reconstructed).await?,
    }))
}

async fn load_stream_failure_assertion_detail(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<AssertionDiagnosticDetail>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT code, assertion_ref, owner_ref, owner_kind, predicate_ref,
                    source_file_ref, start_byte, end_byte, start_line,
                    start_column, end_line, end_column, safe_witness
             FROM stream_failure_assertion_detail WHERE identity_id = ?1",
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
    if row_text(&row, 0)? != orna_stream::ORNA_A091_009 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let assertion = AssertionRef::from_row_ref(decode_row_ref(
        row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    let owner = ObjectRef::from_row_ref(decode_row_ref(
        row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    let owner_kind = decode_assertion_owner_kind(
        row.get::<i64>(3)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let predicate = ExpressionRef::from_row_ref(decode_row_ref(
        row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    let file = orna_foundation::FileRef::from_row_ref(decode_row_ref(
        row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?);
    let source_span = SourceSpan::new(
        file,
        orna_foundation::SourceSpanExtent {
            start_byte: decode_nonnegative_bigint(
                row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            end_byte: decode_nonnegative_bigint(
                row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            start_line: decode_positive_bigint(
                row.get(8).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            start_column: decode_positive_bigint(
                row.get(9).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            end_line: decode_positive_bigint(
                row.get(10).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            end_column: decode_positive_bigint(
                row.get(11).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
        },
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let witness = row
        .get::<Option<String>>(12)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(SafeText::new)
        .transpose()
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(SafeAssertionWitness::from_safe_presentation);
    Ok(Some(AssertionDiagnosticDetail {
        code: AssertionDiagnosticCode::TableOrCrossTableFalse,
        assertion,
        owner,
        owner_kind,
        predicate,
        source_span,
        witness,
    }))
}

pub(crate) async fn store_stream_failure_assertion_detail(
    connection: &Connection,
    identity: &FailureIdentity,
    detail: &AssertionDiagnosticDetail,
) -> Result<(), RuntimeError> {
    if detail.code != AssertionDiagnosticCode::TableOrCrossTableFalse {
        return Err(RuntimeError::RecoveryInvalid);
    }
    connection
        .execute(
            "INSERT INTO stream_failure_assertion_detail (
                identity_id, code, assertion_ref, owner_ref, owner_kind,
                predicate_ref, source_file_ref, start_byte, end_byte,
                start_line, start_column, end_line, end_column, safe_witness
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(identity_id) DO UPDATE SET
                code = excluded.code, assertion_ref = excluded.assertion_ref,
                owner_ref = excluded.owner_ref, owner_kind = excluded.owner_kind,
                predicate_ref = excluded.predicate_ref, source_file_ref = excluded.source_file_ref,
                start_byte = excluded.start_byte, end_byte = excluded.end_byte,
                start_line = excluded.start_line, start_column = excluded.start_column,
                end_line = excluded.end_line, end_column = excluded.end_column,
                safe_witness = excluded.safe_witness",
            params![
                stream_identity_id(identity),
                detail.code.as_str(),
                encode_row_ref(detail.assertion.as_row_ref())?,
                encode_row_ref(detail.owner.as_row_ref())?,
                encode_assertion_owner_kind(detail.owner_kind),
                encode_row_ref(detail.predicate.as_row_ref())?,
                encode_row_ref(detail.source_span.file.as_row_ref())?,
                encode_nonnegative_bigint(&detail.source_span.start_byte)?,
                encode_nonnegative_bigint(&detail.source_span.end_byte)?,
                encode_nonnegative_bigint(&detail.source_span.start_line)?,
                encode_nonnegative_bigint(&detail.source_span.start_column)?,
                encode_nonnegative_bigint(&detail.source_span.end_line)?,
                encode_nonnegative_bigint(&detail.source_span.end_column)?,
                detail
                    .witness
                    .as_ref()
                    .map(|value| value.as_safe_presentation().as_str().to_owned()),
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

pub(crate) async fn load_stream_replay_claim(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<StoredStreamReplayClaim>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT version, owner_id, owner_epoch
             FROM stream_replay_claim WHERE identity_id = ?1",
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
    let version = decode_u64(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let owner_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let owner_epoch = decode_u64(
        row.get::<i64>(2)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    validate_id(owner_id)?;
    if owner_epoch == 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(StoredStreamReplayClaim {
        version,
        owner: WriterLease {
            owner_id,
            epoch: owner_epoch,
        },
    }))
}

/// A failed delivery's retained payload as the runtime stores it: the source
/// bytes, or a protected connector reference with the digest its refetch must
/// match. The table spreads this over four nullable columns plus a
/// `retention` discriminant, and only two shapes of them are valid;
/// [`Self::from_columns`] checks that shape once so no consumer handles a
/// half-populated payload.
pub(crate) enum StoredFailurePayload {
    Plaintext(Vec<u8>),
    ProtectedReference { reference: String, digest: [u8; 32] },
}

impl StoredFailurePayload {
    const PLAINTEXT_RETENTION: i64 = 1;
    const PROTECTED_REFERENCE_RETENTION: i64 = 2;

    /// Decodes the `payload`, `payload_reference`, `payload_digest` and
    /// `retention` columns. Any other combination is a damaged row.
    pub(crate) fn from_columns(
        payload: Option<Vec<u8>>,
        reference: Option<String>,
        digest: Option<Vec<u8>>,
        retention: i64,
    ) -> Result<Self, RuntimeError> {
        match (retention, payload, reference, digest) {
            (Self::PLAINTEXT_RETENTION, Some(bytes), None, None) => Ok(Self::Plaintext(bytes)),
            (Self::PROTECTED_REFERENCE_RETENTION, None, Some(reference), Some(digest))
                if !reference.is_empty() =>
            {
                Ok(Self::ProtectedReference {
                    reference,
                    digest: fixed(digest)?,
                })
            }
            _ => Err(RuntimeError::RecoveryInvalid),
        }
    }

    /// Encodes a connector-selected payload for storage. A connector that
    /// declared no safe retention has nothing to store, and a protected
    /// reference must name something.
    pub(crate) fn from_submitted(payload: StreamFailurePayload) -> Result<Self, RuntimeError> {
        match payload {
            StreamFailurePayload::Unavailable => Err(RuntimeError::RecoveryInvalid),
            StreamFailurePayload::Plaintext(bytes) => Ok(Self::Plaintext(bytes)),
            StreamFailurePayload::ProtectedReference { reference, digest } => {
                if reference.is_empty() {
                    return Err(RuntimeError::InvalidIdentity);
                }
                Ok(Self::ProtectedReference { reference, digest })
            }
        }
    }

    pub(crate) fn into_columns(self) -> FailurePayloadColumns {
        match self {
            Self::Plaintext(bytes) => FailurePayloadColumns {
                payload: Some(bytes),
                reference: None,
                digest: None,
                retention: Self::PLAINTEXT_RETENTION,
            },
            Self::ProtectedReference { reference, digest } => FailurePayloadColumns {
                payload: None,
                reference: Some(reference),
                digest: Some(digest.to_vec()),
                retention: Self::PROTECTED_REFERENCE_RETENTION,
            },
        }
    }
}

/// The `payload`, `payload_reference`, `payload_digest` and `retention`
/// columns of `stream_failure_payload`.
pub(crate) struct FailurePayloadColumns {
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) reference: Option<String>,
    pub(crate) digest: Option<Vec<u8>>,
    pub(crate) retention: i64,
}

pub(crate) async fn load_stored_stream_failure_payload(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<StoredFailurePayload>, RuntimeError> {
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
    StoredFailurePayload::from_columns(
        row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
        row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?,
        row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map(Some)
}

async fn load_stream_failure_payload_metadata(
    connection: &Connection,
    identity: &FailureIdentity,
) -> Result<Option<StreamFailurePayloadMetadata>, RuntimeError> {
    let Some(payload) = load_stored_stream_failure_payload(connection, identity).await? else {
        return Ok(None);
    };
    let (plaintext_bytes, protected_reference) = match &payload {
        StoredFailurePayload::Plaintext(bytes) => {
            (Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)), false)
        }
        StoredFailurePayload::ProtectedReference { .. } => (None, true),
    };
    Ok(Some(StreamFailurePayloadMetadata {
        plaintext_bytes,
        protected_reference,
        redacted: true,
    }))
}

pub(crate) async fn load_stream_lease(
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

pub(crate) fn lease_matches(lease: &DeliveryLease, stored: &StoredStreamLease) -> bool {
    lease.fence == stored.fence
        && lease.purpose == stored.purpose
        && lease.delivery.position.token.as_str() == stored.delivery_position
        && lease.delivery.successor.token.as_str() == stored.successor_position
}

pub(crate) async fn stream_checkpoint_matches(
    connection: &Connection,
    key: &CheckpointKey,
    expected: &orna_stream::CheckpointPrecondition,
) -> Result<bool, RuntimeError> {
    let (checkpoint, _) = load_stream_checkpoint_state(connection, key).await?;
    Ok(checkpoint.version == expected.version && checkpoint.committed == expected.committed)
}

impl RuntimeState {
    /// Creates a stream backend whose mutations are fenced by this writer lease.
    pub fn stream_backend(&self, lease: WriterLease) -> RuntimeStreamBackend<'_> {
        RuntimeStreamBackend { state: self, lease }
    }

    /// Reads one durable stream checkpoint without exposing the runtime
    /// database connection or weakening the stream identity boundary.
    pub async fn stream_checkpoint(
        &self,
        key: &CheckpointKey,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        load_stream_checkpoint(&self.connection, key).await
    }

    /// Counts private durable stream-failure rows for bounded implementation
    /// evidence only.
    ///
    /// This is not a `sys.Failure` relation, is not authorized observation
    /// surface, and must not be used to construct or publish public system
    /// rows. It exists so isolated conformance witnesses can prove that a
    /// retry sequence coalesces one delivery identity into one stored row.
    pub async fn stream_failure_row_count_for_evidence(&self) -> Result<u64, RuntimeError> {
        let mut rows = self
            .connection
            .query("SELECT COUNT(*) FROM stream_failure", ())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )
    }
}
