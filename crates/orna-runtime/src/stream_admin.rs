//! Administrative stream operations: pause, resume and checkpoint reset with durable receipts.

use crate::RuntimeState;
use crate::codec::{
    bigint_to_i64, capture_tx, cwd_capture_from_parts, decode_bool, decode_position,
    decode_row_ref, decode_u64, fixed, validate_id, validate_observation_text,
};
use crate::error::RuntimeError;
use crate::lease::{WriterLease, validate_writer_lease};
use crate::observation::sync_stream_observation_tx;
use crate::request::RequestIdentity;
use crate::stream_intent::apply_stream_intent_tx;
use crate::stream_store::{load_stream_pause_reason, store_stream_pause_reason, stream_key_id};
use libsql::{Connection, TransactionBehavior, params};
use orna_stream::Checkpoint as StreamCheckpoint;
use orna_stream::{
    CheckpointKey, CheckpointPrecondition, CommitIntent, CommitResult, Position, RejectReason,
    StreamStatus,
};
use orna_value::{CwdCapture, SafeText, Value, validate_stream_reference};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Derives the stable administrative receipt identity for one admitted
/// request. Keeping the session in the digest prevents request IDs that are
/// only unique within a session from aliasing in the runtime-wide receipt log.
pub fn admin_invocation_id(request: RequestIdentity) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(b"orna.admin.invocation.v1\0");
    digest.update(request.session_id);
    digest.update(request.request_id);
    digest.finalize()[..16]
        .try_into()
        .expect("SHA-256 has at least 16 bytes")
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

/// The redaction-safe durable record written with a successful checkpoint
/// reset. Positions remain provider-defined opaque values; the reason is
/// retained only when it passes the safe-text boundary and otherwise becomes
/// the explicit redaction marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointResetAudit {
    pub sequence: u64,
    /// None means the audit predates generation pinning and cannot be assigned
    /// honestly to a historical checkpoint.
    pub observed_generation: Option<u64>,
    pub key: CheckpointKey,
    pub old_version: u64,
    pub old_position: Option<Position>,
    pub new_version: u64,
    pub new_position: Position,
    pub reason: String,
    pub redacted: bool,
}

/// The compare-and-set target and audit reason for one checkpoint reset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointResetRequest {
    pub key: CheckpointKey,
    pub expected: CheckpointPrecondition,
    pub to: Position,
    pub reason: String,
}

/// Durable redaction-safe record for one administrative invocation.
///
/// Successful state-changing calls are committed with their transition in one
/// local transaction. Failed calls are retained after the transition rolls
/// back, so an audit never claims that a requested state change occurred.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminLifecycleEffect {
    CheckpointReset,
    StreamPaused { changed: bool },
    StreamPausePending { changed: bool },
    StreamResumed { changed: bool },
    Busy,
    BlockingFailure,
    Failed,
}

/// The replayable result retained for a successful checkpoint reset receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminCheckpointResetResult {
    pub version: u64,
    pub committed_position: Position,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminInvocationAudit {
    pub sequence: u64,
    pub invocation_id: [u8; 16],
    pub function: String,
    pub safe_arguments: String,
    pub owner: WriterLease,
    /// Admitted generation; `None` when the failed call could not observe the CWD.
    pub observed_generation: Option<u64>,
    pub terminal_outcome: String,
    /// Typed runtime effect decoded from the durable terminal outcome.
    pub effect: AdminLifecycleEffect,
    /// Present for checkpoint-reset receipts created with result retention.
    pub checkpoint_reset_result: Option<AdminCheckpointResetResult>,
    pub succeeded: bool,
    pub redacted: bool,
}

pub(crate) enum AdminInvocationOperation {
    Pause {
        key: CheckpointKey,
        reason: Option<String>,
    },
    Reset {
        key: CheckpointKey,
        expected: CheckpointPrecondition,
        to: Position,
        reason: String,
    },
    Resume {
        key: CheckpointKey,
    },
}

pub(crate) struct AdminInvocationDescriptor {
    invocation_id: [u8; 16],
    function: &'static str,
    safe_arguments: String,
    redacted: bool,
}

/// One admin operation's outcome. The stream outcome carries the full
/// administration record and the checkpoint outcome is a bare pin, so the two
/// variants differ in size. The value is built once per admin call and matched
/// by reference on a cold path, so the enum stays unboxed and readable.
#[allow(clippy::large_enum_variant)]
pub(crate) enum AdminOperationResult {
    Stream(StreamAdministrationOutcome),
    Checkpoint(StreamCheckpoint),
}

fn admin_stream_result(
    result: AdminOperationResult,
) -> Result<StreamAdministrationOutcome, RuntimeError> {
    match result {
        AdminOperationResult::Stream(result) => Ok(result),
        AdminOperationResult::Checkpoint(_) => Err(RuntimeError::RecoveryInvalid),
    }
}

fn admin_checkpoint_result(result: AdminOperationResult) -> Result<StreamCheckpoint, RuntimeError> {
    match result {
        AdminOperationResult::Checkpoint(result) => Ok(result),
        AdminOperationResult::Stream(_) => Err(RuntimeError::RecoveryInvalid),
    }
}

fn admin_invocation_descriptor(operation: &AdminInvocationOperation) -> AdminInvocationDescriptor {
    admin_invocation_descriptor_with_id(operation, Uuid::new_v4().into_bytes())
}

pub(crate) fn admin_invocation_descriptor_with_id(
    operation: &AdminInvocationOperation,
    invocation_id: [u8; 16],
) -> AdminInvocationDescriptor {
    let (function, arguments, redacted) = match operation {
        AdminInvocationOperation::Pause { key, reason } => {
            let has_reason = reason.is_some();
            let redacted = reason
                .as_deref()
                .map(admin_argument_redacted)
                .unwrap_or(false);
            let reason_digest = reason
                .as_deref()
                .map(|value| admin_digest(value.as_bytes()))
                .unwrap_or_else(|| "none".into());
            let reason_marker = if has_reason {
                if redacted { "<redacted>" } else { "<safe>" }
            } else {
                "<none>"
            };
            (
                if has_reason {
                    "sys.admin.pause_stream_with_reason"
                } else {
                    "sys.admin.pause_stream"
                },
                format!(
                    "stream_key_digest={};reason_digest={reason_digest};reason={reason_marker}",
                    admin_digest(stream_key_id(key).as_bytes())
                ),
                redacted,
            )
        }
        AdminInvocationOperation::Reset {
            key,
            expected,
            to,
            reason,
        } => {
            let reason_redacted = admin_argument_redacted(reason);
            let reason_marker = if reason_redacted {
                "<redacted>"
            } else {
                "<safe>"
            };
            let reason_digest = admin_digest(reason.as_bytes());
            let expected_position = expected
                .committed
                .as_ref()
                .map(|position| admin_digest(position.token.as_str().as_bytes()))
                .unwrap_or_else(|| "none".into());
            (
                "sys.admin.reset_checkpoint",
                format!(
                    "stream_key_digest={};expected_version={};expected_position_digest={expected_position};target_digest={};reason_digest={reason_digest};reason={reason_marker}",
                    admin_digest(stream_key_id(key).as_bytes()),
                    expected.version,
                    admin_digest(to.token.as_str().as_bytes()),
                ),
                reason_redacted,
            )
        }
        AdminInvocationOperation::Resume { key } => (
            "sys.admin.resume_stream",
            format!(
                "stream_key_digest={}",
                admin_digest(stream_key_id(key).as_bytes())
            ),
            false,
        ),
    };
    AdminInvocationDescriptor {
        invocation_id,
        function,
        safe_arguments: arguments,
        redacted,
    }
}

fn admin_reference_descriptor(
    invocation_id: [u8; 16],
    function: &'static str,
    reference: &Value,
    reason: Option<&str>,
) -> AdminInvocationDescriptor {
    let reason_redacted = reason.is_some_and(admin_argument_redacted);
    let reason_digest = reason
        .map(|value| admin_digest(value.as_bytes()))
        .unwrap_or_else(|| "none".into());
    let reason_marker = match reason {
        None => "<none>",
        Some(_) if reason_redacted => "<redacted>",
        Some(_) => "<safe>",
    };
    let reference_digest = reference
        .encode()
        .ok()
        .map(|encoded| admin_digest(&encoded))
        .unwrap_or_else(|| "unavailable".into());
    AdminInvocationDescriptor {
        invocation_id,
        function,
        safe_arguments: format!(
            "stream_ref_digest={reference_digest};reason_digest={reason_digest};reason={reason_marker}"
        ),
        redacted: reason_redacted,
    }
}

/// Replays a stored failure or outcome for an admitted invocation.
///
/// A `failure:stale_capture` row replays the capture that rejected the call,
/// stored in `admin_invocation_stale_capture`, so the replayed error equals the
/// recorded one after later commits advance the CWD. Rows written before that
/// table replay the CWD capture current at replay time. A
/// `failure:unobservable:<class>` row (its CWD capture could not be read)
/// replays that class. Its `stale_capture` class replays as `RecoveryInvalid`,
/// because the rejecting capture was never stored. A bare legacy
/// `failure:unobservable` row replays as `RecoveryInvalid`.
pub(crate) async fn replay_admin_stream_result(
    connection: &Connection,
    receipt: &AdminInvocationAudit,
) -> Result<AdminOperationResult, RuntimeError> {
    if let Some(failure) = receipt.terminal_outcome.strip_prefix("failure:") {
        if failure == "stale_capture" {
            let current = match load_admin_stale_capture(connection, receipt.invocation_id).await? {
                Some(current) => current,
                None => capture_tx(connection).await?,
            };
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        if let Some(class) = failure.strip_prefix("unobservable:") {
            return Err(admin_failure_from_class(class));
        }
        return Err(admin_failure_from_class(failure));
    }
    let result = match receipt.terminal_outcome.as_str() {
        "paused" => StreamAdministrationOutcome::Paused { changed: true },
        "paused_noop" => StreamAdministrationOutcome::Paused { changed: false },
        "pause_pending" => StreamAdministrationOutcome::PausePending { changed: true },
        "pause_pending_noop" => StreamAdministrationOutcome::PausePending { changed: false },
        "resumed" => StreamAdministrationOutcome::Running { changed: true },
        "resumed_noop" => StreamAdministrationOutcome::Running { changed: false },
        "busy" => StreamAdministrationOutcome::Busy,
        "blocking_failure" => StreamAdministrationOutcome::BlockingFailure,
        _ => return Err(RuntimeError::AdminInvocationConflict),
    };
    Ok(AdminOperationResult::Stream(result))
}

/// Stable failure class stored in admin audit rows. Exhaustive: every new
/// variant must choose its class here. Payload-carrying variants store only
/// the class; their payload is not persisted (see `admin_failure_from_class`).
pub(crate) fn admin_failure_class(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::InvalidIdentity => "invalid_identity",
        RuntimeError::InvalidDigest => "invalid_digest",
        RuntimeError::InvalidObservationReference => "invalid_observation_reference",
        RuntimeError::ObservationCoordinateMismatch => "observation_coordinate_mismatch",
        RuntimeError::ProcedureMaterializationConflict => "procedure_materialization_conflict",
        RuntimeError::InvocationObservationConflict => "invocation_observation_conflict",
        RuntimeError::InvocationStateConflict => "invocation_state_conflict",
        RuntimeError::QueryObservationConflict => "query_observation_conflict",
        RuntimeError::QueryStateConflict => "query_state_conflict",
        RuntimeError::InvocationChildrenActive => "invocation_children_active",
        RuntimeError::InvocationOwnerInvalid => "invocation_owner_invalid",
        RuntimeError::InvocationTailInvalid => "invocation_tail_invalid",
        RuntimeError::InvocationTailLimit => "invocation_tail_limit",
        RuntimeError::StreamIdentityMismatch => "stream_identity_mismatch",
        RuntimeError::MigrationCarryTargetDiffers => "migration_carry_target_differs",
        RuntimeError::StreamSourceNotReplayable => "stream_source_not_replayable",
        RuntimeError::StreamCheckpointStale => "stream_checkpoint_stale",
        RuntimeError::CheckpointNotReplayable => "checkpoint_not_replayable",
        RuntimeError::SnapshotNotFound => "snapshot_not_found",
        RuntimeError::SnapshotIncomplete => "snapshot_incomplete",
        RuntimeError::SnapshotContextMismatch => "snapshot_context_mismatch",
        RuntimeError::LeaseHeld => "lease_held",
        RuntimeError::AdminBusy => "admin_busy",
        RuntimeError::AdminInvocationConflict => "admin_invocation_conflict",
        RuntimeError::OwnerLost => "owner_lost",
        RuntimeError::RecoveryPending => "recovery_pending",
        RuntimeError::StaleCapture { .. } => "stale_capture",
        RuntimeError::InvalidCapture => "invalid_capture",
        RuntimeError::EmptyMutationBatch => "empty_mutation_batch",
        RuntimeError::StreamFailureStale => "stream_failure_stale",
        RuntimeError::StreamFailureStateConflict => "stream_failure_state_conflict",
        RuntimeError::StreamFailureSkipUnsupported => "stream_failure_skip_unsupported",
        RuntimeError::InvalidTableMutation => "invalid_table_mutation",
        RuntimeError::StaleTableSchema => "stale_table_schema",
        RuntimeError::ConflictingPublicationIntent => "conflicting_publication_intent",
        RuntimeError::ConflictingPublicationCommit => "conflicting_publication_commit",
        RuntimeError::InvalidPublicationCommit => "invalid_publication_commit",
        RuntimeError::InvalidPublicationPolicy => "invalid_publication_policy",
        RuntimeError::CompactPublicationRequired => "compact_publication_required",
        RuntimeError::CompactReceiptKeyMismatch => "compact_receipt_key_mismatch",
        RuntimeError::InvalidCompactReceipt => "invalid_compact_receipt",
        RuntimeError::RequestUnknown => "request_unknown",
        RuntimeError::RequestFingerprintMismatch => "request_fingerprint_mismatch",
        RuntimeError::RequestOwnerConflict => "request_owner_conflict",
        RuntimeError::RequestStateConflict => "request_state_conflict",
        RuntimeError::SessionClosed => "session_closed",
        RuntimeError::SessionWorkActive => "session_work_active",
        RuntimeError::TerminalOutcomeTooLarge => "terminal_outcome_too_large",
        RuntimeError::RecoveryInvalid => "recovery_invalid",
        // Payload-carrying: the injected point is not stored, so it replays as RecoveryInvalid.
        RuntimeError::FaultInjected(_) => "fault_injected",
        RuntimeError::SystemProviderAbiInvalid => "system_provider_abi_invalid",
        RuntimeError::StorageUnavailable => "storage_unavailable",
        // Payload-carrying: the row stores only this class, so it replays as RecoveryInvalid.
        RuntimeError::LegacyHeadReadOnly(_) => "legacy_head_read_only",
        // Payload-carrying: the row stores only this class, so it replays as RecoveryInvalid.
        RuntimeError::UnknownHeadFormat(_) => "unknown_head_format",
    }
}

/// Rebuilds the runtime error for a stored failure class.
///
/// Classes whose variant carries a payload that the row does not store
/// (`fault_injected`, `legacy_head_read_only`) and unknown names surface as
/// `RecoveryInvalid`. `stale_capture` is handled in `replay_admin_stream_result`
/// because its payload is captured at replay. Outcomes stored before stable
/// classes keep their original variants: `unavailable`, and the `AdminBusy`
/// message that busy rows stored before durable admin receipts.
pub(crate) fn admin_failure_from_class(class: &str) -> RuntimeError {
    match class {
        "admin_busy" | "runtime administration callback is busy" => RuntimeError::AdminBusy,
        "invalid_identity" => RuntimeError::InvalidIdentity,
        "invalid_digest" => RuntimeError::InvalidDigest,
        "invalid_observation_reference" => RuntimeError::InvalidObservationReference,
        "observation_coordinate_mismatch" => RuntimeError::ObservationCoordinateMismatch,
        "procedure_materialization_conflict" => RuntimeError::ProcedureMaterializationConflict,
        "invocation_observation_conflict" => RuntimeError::InvocationObservationConflict,
        "invocation_state_conflict" => RuntimeError::InvocationStateConflict,
        "query_observation_conflict" => RuntimeError::QueryObservationConflict,
        "query_state_conflict" => RuntimeError::QueryStateConflict,
        "invocation_children_active" => RuntimeError::InvocationChildrenActive,
        "invocation_owner_invalid" => RuntimeError::InvocationOwnerInvalid,
        "invocation_tail_invalid" => RuntimeError::InvocationTailInvalid,
        "invocation_tail_limit" => RuntimeError::InvocationTailLimit,
        "stream_identity_mismatch" => RuntimeError::StreamIdentityMismatch,
        "stream_source_not_replayable" => RuntimeError::StreamSourceNotReplayable,
        "stream_checkpoint_stale" => RuntimeError::StreamCheckpointStale,
        "checkpoint_not_replayable" => RuntimeError::CheckpointNotReplayable,
        "snapshot_not_found" => RuntimeError::SnapshotNotFound,
        "snapshot_incomplete" => RuntimeError::SnapshotIncomplete,
        "snapshot_context_mismatch" => RuntimeError::SnapshotContextMismatch,
        "lease_held" => RuntimeError::LeaseHeld,
        "admin_invocation_conflict" => RuntimeError::AdminInvocationConflict,
        "owner_lost" => RuntimeError::OwnerLost,
        "recovery_pending" => RuntimeError::RecoveryPending,
        "invalid_capture" => RuntimeError::InvalidCapture,
        "stream_failure_stale" => RuntimeError::StreamFailureStale,
        "stream_failure_state_conflict" => RuntimeError::StreamFailureStateConflict,
        "stream_failure_skip_unsupported" => RuntimeError::StreamFailureSkipUnsupported,
        "empty_mutation_batch" => RuntimeError::EmptyMutationBatch,
        "invalid_table_mutation" => RuntimeError::InvalidTableMutation,
        "stale_table_schema" => RuntimeError::StaleTableSchema,
        "conflicting_publication_intent" => RuntimeError::ConflictingPublicationIntent,
        "conflicting_publication_commit" => RuntimeError::ConflictingPublicationCommit,
        "invalid_publication_commit" => RuntimeError::InvalidPublicationCommit,
        "invalid_publication_policy" => RuntimeError::InvalidPublicationPolicy,
        "compact_publication_required" => RuntimeError::CompactPublicationRequired,
        "compact_receipt_key_mismatch" => RuntimeError::CompactReceiptKeyMismatch,
        "invalid_compact_receipt" => RuntimeError::InvalidCompactReceipt,
        "request_unknown" => RuntimeError::RequestUnknown,
        "request_fingerprint_mismatch" => RuntimeError::RequestFingerprintMismatch,
        "request_owner_conflict" => RuntimeError::RequestOwnerConflict,
        "request_state_conflict" => RuntimeError::RequestStateConflict,
        "session_closed" => RuntimeError::SessionClosed,
        "session_work_active" => RuntimeError::SessionWorkActive,
        "terminal_outcome_too_large" => RuntimeError::TerminalOutcomeTooLarge,
        "system_provider_abi_invalid" => RuntimeError::SystemProviderAbiInvalid,
        "storage_unavailable" | "unavailable" => RuntimeError::StorageUnavailable,
        "unobservable" => RuntimeError::RecoveryInvalid,
        _ => RuntimeError::RecoveryInvalid,
    }
}

pub(crate) fn admin_observed_generation(capture: &CwdCapture) -> Result<u64, RuntimeError> {
    u64::try_from(bigint_to_i64(capture.generation())?).map_err(|_| RuntimeError::RecoveryInvalid)
}

async fn admin_result_from_receipt(
    connection: &Connection,
    operation: Option<&AdminInvocationOperation>,
    descriptor: &AdminInvocationDescriptor,
    receipt: &AdminInvocationAudit,
) -> Result<AdminOperationResult, RuntimeError> {
    if receipt.function != descriptor.function
        || receipt.safe_arguments != descriptor.safe_arguments
        || receipt.redacted != descriptor.redacted
    {
        return Err(RuntimeError::AdminInvocationConflict);
    }
    match operation {
        Some(AdminInvocationOperation::Reset { key, .. }) => {
            if receipt.terminal_outcome.starts_with("failure:") {
                return replay_admin_stream_result(connection, receipt).await;
            }
            let result = receipt
                .checkpoint_reset_result
                .as_ref()
                .ok_or(RuntimeError::AdminInvocationConflict)?;
            Ok(AdminOperationResult::Checkpoint(StreamCheckpoint {
                key: key.clone(),
                version: result.version,
                committed: Some(result.committed_position.clone()),
            }))
        }
        Some(AdminInvocationOperation::Pause { .. })
        | Some(AdminInvocationOperation::Resume { .. })
        | None => replay_admin_stream_result(connection, receipt).await,
    }
}

fn admin_argument_redacted(value: &str) -> bool {
    value.len() > 16_777_216 || SafeText::new(value.to_owned()).is_err()
}

pub(crate) fn admin_digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Host-supplied capability for one provider-backed checkpoint reset.
///
/// The runtime compares the provider's complete key before asking whether the
/// opaque target is supported. The provider owns target semantics; this API
/// does not infer ordering or provide a generic typed-position codec.
pub trait CheckpointResetProvider {
    /// The exact stream key served by this provider, including its position
    /// format identity.
    fn checkpoint_key(&self) -> CheckpointKey;

    /// Returns whether this provider can resume/reset to the opaque target.
    /// Returning false fails closed before a transaction or audit mutation.
    fn supports_reset_target(&self, target: &Position) -> bool;
}

async fn apply_admin_operation_tx(
    connection: &Connection,
    operation: &AdminInvocationOperation,
    observed_generation: u64,
) -> Result<AdminOperationResult, RuntimeError> {
    match operation {
        AdminInvocationOperation::Pause { key, reason } => {
            if reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 16_777_216)
            {
                return Err(RuntimeError::InvalidIdentity);
            }
            let result =
                apply_stream_intent_tx(connection, CommitIntent::Pause { key: key.clone() })
                    .await?;
            if stream_pause_changed(&result)
                && let Some(reason) = reason
            {
                store_stream_pause_reason(connection, stream_pause_key(&result)?, reason).await?;
            }
            sync_stream_observation_tx(connection, &result).await?;
            Ok(AdminOperationResult::Stream(stream_administration_outcome(
                result,
            )?))
        }
        AdminInvocationOperation::Reset {
            key,
            expected,
            to,
            reason,
        } => {
            let audit_reason = redact_reset_reason(reason.clone())?;
            let result = apply_stream_intent_tx(
                connection,
                CommitIntent::Reset {
                    key: key.clone(),
                    expected: expected.clone(),
                    to: to.clone(),
                },
            )
            .await?;
            let checkpoint = match result {
                CommitResult::CheckpointReset { checkpoint } => checkpoint,
                CommitResult::Rejected(RejectReason::StreamNotPaused)
                | CommitResult::Rejected(RejectReason::StreamBusy)
                | CommitResult::Rejected(RejectReason::BlockingFailure) => {
                    return Err(RuntimeError::RecoveryInvalid);
                }
                CommitResult::Rejected(RejectReason::StaleCheckpoint) => {
                    return Err(RuntimeError::StreamCheckpointStale);
                }
                _ => return Err(RuntimeError::RecoveryInvalid),
            };
            store_stream_checkpoint_reset_audit(
                connection,
                key,
                expected,
                &checkpoint,
                &audit_reason,
                observed_generation,
            )
            .await?;
            sync_stream_observation_tx(
                connection,
                &CommitResult::CheckpointReset {
                    checkpoint: checkpoint.clone(),
                },
            )
            .await?;
            Ok(AdminOperationResult::Checkpoint(checkpoint))
        }
        AdminInvocationOperation::Resume { key } => {
            let result =
                apply_stream_intent_tx(connection, CommitIntent::Resume { key: key.clone() })
                    .await?;
            sync_stream_observation_tx(connection, &result).await?;
            Ok(AdminOperationResult::Stream(stream_administration_outcome(
                result,
            )?))
        }
    }
}

fn admin_operation_outcome(result: &AdminOperationResult) -> String {
    match result {
        AdminOperationResult::Checkpoint(_) => "checkpoint_reset".into(),
        AdminOperationResult::Stream(StreamAdministrationOutcome::Paused { changed: true }) => {
            "paused".into()
        }
        AdminOperationResult::Stream(StreamAdministrationOutcome::Paused { changed: false }) => {
            "paused_noop".into()
        }
        AdminOperationResult::Stream(StreamAdministrationOutcome::PausePending {
            changed: true,
        }) => "pause_pending".into(),
        AdminOperationResult::Stream(StreamAdministrationOutcome::PausePending {
            changed: false,
        }) => "pause_pending_noop".into(),
        AdminOperationResult::Stream(StreamAdministrationOutcome::Running { changed: true }) => {
            "resumed".into()
        }
        AdminOperationResult::Stream(StreamAdministrationOutcome::Running { changed: false }) => {
            "resumed_noop".into()
        }
        AdminOperationResult::Stream(StreamAdministrationOutcome::Busy) => "busy".into(),
        AdminOperationResult::Stream(StreamAdministrationOutcome::BlockingFailure) => {
            "blocking_failure".into()
        }
    }
}

fn admin_operation_succeeded(result: &AdminOperationResult) -> bool {
    !matches!(
        result,
        AdminOperationResult::Stream(
            StreamAdministrationOutcome::Busy | StreamAdministrationOutcome::BlockingFailure
        )
    )
}

pub(crate) fn decode_admin_lifecycle_effect(
    value: &str,
) -> Result<AdminLifecycleEffect, RuntimeError> {
    Ok(match value {
        "checkpoint_reset" => AdminLifecycleEffect::CheckpointReset,
        "paused" => AdminLifecycleEffect::StreamPaused { changed: true },
        "paused_noop" => AdminLifecycleEffect::StreamPaused { changed: false },
        "pause_pending" => AdminLifecycleEffect::StreamPausePending { changed: true },
        "pause_pending_noop" => AdminLifecycleEffect::StreamPausePending { changed: false },
        "resumed" => AdminLifecycleEffect::StreamResumed { changed: true },
        "resumed_noop" => AdminLifecycleEffect::StreamResumed { changed: false },
        "busy" => AdminLifecycleEffect::Busy,
        "blocking_failure" => AdminLifecycleEffect::BlockingFailure,
        value if value.starts_with("failure:") => AdminLifecycleEffect::Failed,
        _ => return Err(RuntimeError::RecoveryInvalid),
    })
}

pub(crate) async fn store_admin_invocation_audit(
    connection: &Connection,
    descriptor: &AdminInvocationDescriptor,
    owner: WriterLease,
    observed_generation: Option<u64>,
    terminal_outcome: String,
    succeeded: bool,
    checkpoint_result: Option<&StreamCheckpoint>,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO admin_invocation_audit
             (invocation_id, function_name, safe_arguments, owner_id, owner_epoch,
              observed_generation, terminal_outcome, succeeded, redacted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                descriptor.invocation_id.to_vec(),
                descriptor.function,
                descriptor.safe_arguments.clone(),
                owner.owner_id.to_vec(),
                i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                observed_generation
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
                terminal_outcome,
                if succeeded { 1_i64 } else { 0_i64 },
                if descriptor.redacted { 1_i64 } else { 0_i64 },
            ],
        )
        .await?;
    if let Some(checkpoint) = checkpoint_result {
        let position = checkpoint
            .committed
            .as_ref()
            .ok_or(RuntimeError::RecoveryInvalid)?;
        connection
            .execute(
                "INSERT INTO admin_invocation_checkpoint_result
                 (invocation_id, version, committed_position) VALUES (?1, ?2, ?3)",
                params![
                    descriptor.invocation_id.to_vec(),
                    i64::try_from(checkpoint.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    position.token.as_str(),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn store_admin_stale_capture(
    connection: &Connection,
    invocation_id: [u8; 16],
    capture: &CwdCapture,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO admin_invocation_stale_capture
             (invocation_id, database_id, runtime_id, generation, generation_digest)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                invocation_id.to_vec(),
                capture.database_id().to_vec(),
                capture.runtime_id().to_vec(),
                bigint_to_i64(capture.generation())?,
                capture.generation_digest().to_vec(),
            ],
        )
        .await?;
    Ok(())
}

async fn load_admin_stale_capture(
    connection: &Connection,
    invocation_id: [u8; 16],
) -> Result<Option<CwdCapture>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT database_id, runtime_id, generation, generation_digest
             FROM admin_invocation_stale_capture WHERE invocation_id = ?1",
            params![invocation_id.to_vec()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let database_id = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let runtime_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    let generation: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let digest = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    cwd_capture_from_parts(database_id, runtime_id, generation, digest).map(Some)
}

async fn load_admin_invocation_audits(
    connection: &Connection,
) -> Result<Vec<AdminInvocationAudit>, RuntimeError> {
    load_admin_invocation_audits_through(connection, i64::MAX as u64).await
}

pub(crate) async fn load_admin_invocation_audits_through(
    connection: &Connection,
    through_sequence: u64,
) -> Result<Vec<AdminInvocationAudit>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT sequence, invocation_id, function_name, safe_arguments,
                    owner_id, owner_epoch, observed_generation,
                    terminal_outcome, succeeded, redacted,
                    (SELECT version FROM admin_invocation_checkpoint_result r
                     WHERE r.invocation_id = a.invocation_id),
                    (SELECT committed_position FROM admin_invocation_checkpoint_result r
                     WHERE r.invocation_id = a.invocation_id)
             FROM admin_invocation_audit a WHERE sequence <= ?1 ORDER BY sequence",
            params![i64::try_from(through_sequence).map_err(|_| RuntimeError::RecoveryInvalid)?],
        )
        .await?;
    let mut audits = Vec::new();
    while let Some(row) = rows.next().await? {
        let sequence = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let invocation_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        validate_id(invocation_id)?;
        let function: String = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&function)?;
        let safe_arguments: String = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&safe_arguments)?;
        let owner = WriterLease {
            owner_id: fixed(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            epoch: decode_u64(
                row.get::<i64>(5)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
        };
        validate_writer_lease(owner)?;
        let observed_generation = row
            .get::<Option<i64>>(6)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
            .map(decode_u64)
            .transpose()?;
        let terminal_outcome: String = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
        validate_observation_text(&terminal_outcome)?;
        let effect = decode_admin_lifecycle_effect(&terminal_outcome)?;
        let checkpoint_reset_result = decode_admin_checkpoint_reset_result(&row, 10, 11)?;
        if checkpoint_reset_result.is_some() && effect != AdminLifecycleEffect::CheckpointReset {
            return Err(RuntimeError::RecoveryInvalid);
        }
        audits.push(AdminInvocationAudit {
            sequence,
            invocation_id,
            function,
            safe_arguments,
            owner,
            observed_generation,
            terminal_outcome,
            effect,
            checkpoint_reset_result,
            succeeded: decode_bool(
                row.get::<i64>(8)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            redacted: decode_bool(
                row.get::<i64>(9)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
        });
    }
    Ok(audits)
}

async fn load_admin_invocation_receipt(
    connection: &Connection,
    invocation_id: [u8; 16],
) -> Result<Option<AdminInvocationAudit>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT sequence, invocation_id, function_name, safe_arguments,
                    owner_id, owner_epoch, observed_generation,
                    terminal_outcome, succeeded, redacted,
                    (SELECT version FROM admin_invocation_checkpoint_result r
                     WHERE r.invocation_id = a.invocation_id),
                    (SELECT committed_position FROM admin_invocation_checkpoint_result r
                     WHERE r.invocation_id = a.invocation_id)
             FROM admin_invocation_audit a WHERE a.invocation_id = ?1",
            params![invocation_id.to_vec()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let sequence = decode_u64(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let invocation_id = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    validate_id(invocation_id)?;
    let function: String = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
    validate_observation_text(&function)?;
    let safe_arguments: String = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
    validate_observation_text(&safe_arguments)?;
    let owner = WriterLease {
        owner_id: fixed(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        epoch: decode_u64(
            row.get::<i64>(5)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
    };
    validate_writer_lease(owner)?;
    let observed_generation = row
        .get::<Option<i64>>(6)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(decode_u64)
        .transpose()?;
    let terminal_outcome: String = row.get(7).map_err(|_| RuntimeError::RecoveryInvalid)?;
    validate_observation_text(&terminal_outcome)?;
    let effect = decode_admin_lifecycle_effect(&terminal_outcome)?;
    let checkpoint_reset_result = decode_admin_checkpoint_reset_result(&row, 10, 11)?;
    if checkpoint_reset_result.is_some() && effect != AdminLifecycleEffect::CheckpointReset {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(Some(AdminInvocationAudit {
        sequence,
        invocation_id,
        function,
        safe_arguments,
        owner,
        observed_generation,
        terminal_outcome,
        effect,
        checkpoint_reset_result,
        succeeded: decode_bool(
            row.get::<i64>(8)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        redacted: decode_bool(
            row.get::<i64>(9)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
    }))
}

fn decode_admin_checkpoint_reset_result(
    row: &libsql::Row,
    version_index: i32,
    position_index: i32,
) -> Result<Option<AdminCheckpointResetResult>, RuntimeError> {
    let version: Option<i64> = row
        .get(version_index)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let position: Option<String> = row
        .get(position_index)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    match (version, position) {
        (None, None) => Ok(None),
        (Some(version), Some(position)) => Ok(Some(AdminCheckpointResetResult {
            version: decode_u64(version)?,
            committed_position: decode_position(position)?,
        })),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

fn redact_reset_reason(reason: String) -> Result<(String, bool), RuntimeError> {
    if reason.len() > 16_777_216 {
        return Err(RuntimeError::InvalidIdentity);
    }
    match SafeText::new(reason) {
        Ok(reason) => Ok((reason.as_str().to_owned(), false)),
        Err(_) => Ok((SafeText::redacted().as_str().to_owned(), true)),
    }
}

async fn store_stream_checkpoint_reset_audit(
    connection: &Connection,
    key: &CheckpointKey,
    expected: &CheckpointPrecondition,
    checkpoint: &StreamCheckpoint,
    audit_reason: &(String, bool),
    observed_generation: u64,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "INSERT INTO stream_checkpoint_reset_audit
             (key_id, observed_generation, old_version, old_position, new_version, new_position,
              reason, redacted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                stream_key_id(key),
                i64::try_from(observed_generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                i64::try_from(expected.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                expected
                    .committed
                    .as_ref()
                    .map(|position| position.token.as_str().to_owned()),
                i64::try_from(checkpoint.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                checkpoint
                    .committed
                    .as_ref()
                    .ok_or(RuntimeError::RecoveryInvalid)?
                    .token
                    .as_str()
                    .to_owned(),
                audit_reason.0.clone(),
                if audit_reason.1 { 1 } else { 0 },
            ],
        )
        .await?;
    Ok(())
}

async fn load_stream_checkpoint_reset_audits(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<Vec<CheckpointResetAudit>, RuntimeError> {
    load_stream_checkpoint_reset_audits_through(connection, key, i64::MAX as u64, true).await
}

pub(crate) async fn load_stream_checkpoint_reset_audits_through(
    connection: &Connection,
    key: &CheckpointKey,
    through_sequence: u64,
    include_unpinned_legacy: bool,
) -> Result<Vec<CheckpointResetAudit>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT sequence, observed_generation, old_version, old_position,
                    new_version, new_position, reason, redacted
             FROM stream_checkpoint_reset_audit
             WHERE key_id = ?1 AND sequence <= ?2
               AND (?3 = 1 OR observed_generation >= 0)
             ORDER BY sequence",
            params![
                stream_key_id(key),
                i64::try_from(through_sequence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                if include_unpinned_legacy {
                    1_i64
                } else {
                    0_i64
                }
            ],
        )
        .await?;
    let mut audits = Vec::new();
    while let Some(row) = rows.next().await? {
        let sequence = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let raw_observed_generation = row
            .get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let observed_generation = if raw_observed_generation < 0 {
            None
        } else {
            Some(decode_u64(raw_observed_generation)?)
        };
        let old_position = row
            .get::<Option<String>>(3)
            .map_err(|_| RuntimeError::RecoveryInvalid)?
            .map(decode_position)
            .transpose()?;
        let new_position = decode_position(
            row.get::<String>(5)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        audits.push(CheckpointResetAudit {
            sequence,
            observed_generation,
            key: key.clone(),
            old_version: decode_u64(
                row.get::<i64>(2)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            old_position,
            new_version: decode_u64(
                row.get::<i64>(4)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
            new_position,
            reason: row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?,
            redacted: decode_bool(
                row.get::<i64>(7)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?,
        });
    }
    Ok(audits)
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
        CommitResult::StreamStatusChanged { state, changed }
            if state.status == StreamStatus::Running =>
        {
            Ok(StreamAdministrationOutcome::Running { changed })
        }
        CommitResult::PausePending { changed, .. } => {
            Ok(StreamAdministrationOutcome::PausePending { changed })
        }
        CommitResult::Rejected(RejectReason::StreamBusy) => Ok(StreamAdministrationOutcome::Busy),
        _ => Err(RuntimeError::RecoveryInvalid),
    }
}

/// The closed set of administrative stream effects that can reach the busy
/// receipt path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminStreamFunction {
    /// `sys.admin.pause_stream`.
    PauseStream,
    /// `sys.admin.pause_stream_with_reason` (engine vocabulary).
    PauseStreamWithReason,
    /// `sys.admin.resume_stream`.
    ResumeStream,
}

impl AdminStreamFunction {
    /// The exact durable receipt name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PauseStream => "sys.admin.pause_stream",
            Self::PauseStreamWithReason => "sys.admin.pause_stream_with_reason",
            Self::ResumeStream => "sys.admin.resume_stream",
        }
    }

    /// Whether this effect requires a reason argument.
    #[must_use]
    pub const fn requires_reason(self) -> bool {
        matches!(self, Self::PauseStreamWithReason)
    }

    /// The exact inverse of [`Self::name`], at the caller/durable boundary.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "sys.admin.pause_stream" => Some(Self::PauseStream),
            "sys.admin.pause_stream_with_reason" => Some(Self::PauseStreamWithReason),
            "sys.admin.resume_stream" => Some(Self::ResumeStream),
            _ => None,
        }
    }
}

impl RuntimeState {
    /// Applies a writer-fenced durable pause transition for one resolved
    /// stream. A caller that needs the public administration result must wait
    /// for `PausePending` to publish `Paused` at the delivery boundary.
    pub async fn pause_stream(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.apply_admin_invocation(
            lease,
            None,
            AdminInvocationOperation::Pause { key, reason: None },
        )
        .await
        .and_then(admin_stream_result)
    }

    /// Reason-bearing stable-receipt form of a stream pause.
    pub async fn pause_stream_with_reason_and_invocation_id(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        reason: String,
        invocation_id: [u8; 16],
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        let operation = AdminInvocationOperation::Pause {
            key,
            reason: Some(reason),
        };
        let descriptor = admin_invocation_descriptor_with_id(&operation, invocation_id);
        self.apply_admin_invocation_with_descriptor(lease, None, operation, descriptor)
            .await
            .and_then(admin_stream_result)
    }

    /// Resolves an admitted portable `sys.StreamRef` against the live writer's
    /// owner-fenced current-runtime observations, then pauses that exact stream
    /// only while its activation capture remains current.
    pub async fn pause_stream_reference_at_capture(
        &self,
        lease: WriterLease,
        reference: Value,
        reason: Option<String>,
        expected_capture: &CwdCapture,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.pause_stream_reference_at_capture_with_invocation_id(
            lease,
            reference,
            reason,
            expected_capture,
            Uuid::new_v4().into_bytes(),
        )
        .await
    }

    /// Stable-receipt form of [`Self::pause_stream_reference_at_capture`].
    /// The public reference digest is the argument identity, so an exact
    /// request replay can recover its result before resolving a now-stale
    /// observation reference.
    pub async fn pause_stream_reference_at_capture_with_invocation_id(
        &self,
        lease: WriterLease,
        reference: Value,
        reason: Option<String>,
        expected_capture: &CwdCapture,
        invocation_id: [u8; 16],
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        let function = if reason.is_some() {
            "sys.admin.pause_stream_with_reason"
        } else {
            "sys.admin.pause_stream"
        };
        let descriptor =
            admin_reference_descriptor(invocation_id, function, &reference, reason.as_deref());
        if let Some(result) = self
            .replay_admin_invocation_receipt(lease, None, &descriptor)
            .await?
        {
            return admin_stream_result(result);
        }
        if crate::activation::runtime_activation_active(self) {
            return Err(self.reentrant_admin_busy_error(&descriptor, lease).await);
        }
        let key = self
            .resolve_admin_stream_reference(lease, &descriptor, &reference, expected_capture)
            .await?;
        let operation = AdminInvocationOperation::Pause { key, reason };
        self.apply_admin_invocation_with_descriptor(
            lease,
            Some(expected_capture),
            operation,
            descriptor,
        )
        .await
        .and_then(admin_stream_result)
    }

    /// Applies a writer-fenced pause while retaining its supplied reason in
    /// the same local transaction that admits the pause. A no-op pause never
    /// overwrites the reason already attached to the existing pause.
    pub async fn pause_stream_with_reason(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
        reason: String,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.apply_admin_invocation(
            lease,
            None,
            AdminInvocationOperation::Pause {
                key,
                reason: Some(reason),
            },
        )
        .await
        .and_then(admin_stream_result)
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

    /// Performs the public runtime boundary for
    /// `sys.admin.reset_checkpoint`.
    ///
    /// The checkpoint, failure cleanup, specialized reset audit, and generic
    /// invocation audit are admitted in one writer-fenced transaction.
    pub async fn reset_checkpoint(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        self.reset_checkpoint_with_invocation_id(lease, request, Uuid::new_v4().into_bytes())
            .await
    }

    /// Replays a checkpoint reset receipt for a caller-stable invocation ID.
    /// This lets a caller reconcile an admitted reset after losing its result.
    pub async fn reset_checkpoint_with_invocation_id(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
        invocation_id: [u8; 16],
    ) -> Result<StreamCheckpoint, RuntimeError> {
        self.reset_checkpoint_at_capture_with_invocation_id(lease, request, None, invocation_id)
            .await
    }

    /// Provider-gated form of [`Self::reset_checkpoint`]. The host supplies
    /// the provider capability for this exact stream; a key mismatch or an
    /// unsupported opaque target is rejected before transition admission but
    /// still retains a failed invocation audit.
    pub async fn reset_checkpoint_with_provider<P: CheckpointResetProvider>(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
        provider: &P,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        self.reset_checkpoint_at_capture_with_provider_and_invocation_id(
            lease,
            request,
            None,
            provider,
            Uuid::new_v4().into_bytes(),
        )
        .await
    }

    /// Provider-gated reset with a stable receipt identity for reconciliation.
    pub async fn reset_checkpoint_at_capture_with_provider_and_invocation_id<
        P: CheckpointResetProvider,
    >(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
        expected_capture: Option<&CwdCapture>,
        provider: &P,
        invocation_id: [u8; 16],
    ) -> Result<StreamCheckpoint, RuntimeError> {
        let operation = AdminInvocationOperation::Reset {
            key: request.key.clone(),
            expected: request.expected.clone(),
            to: request.to.clone(),
            reason: request.reason.clone(),
        };
        let descriptor = admin_invocation_descriptor_with_id(&operation, invocation_id);
        if let Some(result) = self
            .replay_admin_invocation_receipt(lease, Some(&operation), &descriptor)
            .await?
        {
            return admin_checkpoint_result(result);
        }
        if provider.checkpoint_key() != request.key || !provider.supports_reset_target(&request.to)
        {
            let error = RuntimeError::CheckpointNotReplayable;
            return match self
                .record_failed_admin_descriptor(&descriptor, lease, None, &error)
                .await
            {
                Ok(()) | Err(RuntimeError::OwnerLost) => Err(error),
                Err(audit_error) => Err(audit_error),
            };
        }
        self.apply_admin_invocation_with_descriptor(lease, expected_capture, operation, descriptor)
            .await
            .and_then(admin_checkpoint_result)
    }

    /// Capture-fenced form of [`Self::reset_checkpoint`]. Hosts use this when
    /// the checkpoint was resolved from a pinned CWD observation.
    pub async fn reset_checkpoint_at_capture(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
        expected_capture: Option<&CwdCapture>,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        self.reset_checkpoint_at_capture_with_invocation_id(
            lease,
            request,
            expected_capture,
            Uuid::new_v4().into_bytes(),
        )
        .await
    }

    /// Capture-fenced reset with a stable receipt identity for reconciliation.
    pub async fn reset_checkpoint_at_capture_with_invocation_id(
        &self,
        lease: WriterLease,
        request: CheckpointResetRequest,
        expected_capture: Option<&CwdCapture>,
        invocation_id: [u8; 16],
    ) -> Result<StreamCheckpoint, RuntimeError> {
        let operation = AdminInvocationOperation::Reset {
            key: request.key,
            expected: request.expected,
            to: request.to,
            reason: request.reason,
        };
        let descriptor = admin_invocation_descriptor_with_id(&operation, invocation_id);
        self.apply_admin_invocation_with_descriptor(lease, expected_capture, operation, descriptor)
            .await
            .and_then(admin_checkpoint_result)
    }

    /// Reads redaction-safe generic administrative invocation audits in
    /// admission order. Specialized checkpoint-reset audits remain available
    /// through [`Self::checkpoint_reset_audits`].
    pub async fn admin_invocation_audits(&self) -> Result<Vec<AdminInvocationAudit>, RuntimeError> {
        load_admin_invocation_audits(&self.connection).await
    }

    /// Reads one durable administrative receipt by its stable invocation ID.
    pub async fn admin_invocation_receipt(
        &self,
        invocation_id: [u8; 16],
    ) -> Result<Option<AdminInvocationAudit>, RuntimeError> {
        load_admin_invocation_receipt(&self.connection, invocation_id).await
    }

    /// Retains the failed receipt for an admin stream effect that is rejected
    /// because its source activation still owns staged table writes.
    pub async fn reject_admin_stream_effect_as_busy(
        &self,
        lease: WriterLease,
        function: AdminStreamFunction,
        reference: Value,
        reason: Option<String>,
        invocation_id: [u8; 16],
    ) -> Result<(), RuntimeError> {
        if function.requires_reason() != reason.is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let descriptor = admin_reference_descriptor(
            invocation_id,
            function.name(),
            &reference,
            reason.as_deref(),
        );
        match self
            .record_failed_admin_descriptor(&descriptor, lease, None, &RuntimeError::AdminBusy)
            .await
        {
            Ok(()) | Err(RuntimeError::OwnerLost) => Err(RuntimeError::AdminBusy),
            Err(error) => Err(error),
        }
    }

    /// Reads reset audits in admission order. This is intentionally a
    /// redaction-safe projection and never exposes the runtime connection.
    pub async fn checkpoint_reset_audits(
        &self,
        key: &CheckpointKey,
    ) -> Result<Vec<CheckpointResetAudit>, RuntimeError> {
        load_stream_checkpoint_reset_audits(&self.connection, key).await
    }

    /// Applies a writer-fenced durable resume transition for one resolved
    /// stream. Source availability and public stream-reference resolution are
    /// owned by the host above this runtime boundary.
    pub async fn resume_stream(
        &self,
        lease: WriterLease,
        key: CheckpointKey,
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        self.apply_admin_invocation(lease, None, AdminInvocationOperation::Resume { key })
            .await
            .and_then(admin_stream_result)
    }

    /// Resolves and resumes one public stream reference under the captured
    /// writer generation. Its durable receipt is keyed by the request ID.
    pub async fn resume_stream_reference_at_capture_with_invocation_id(
        &self,
        lease: WriterLease,
        reference: Value,
        expected_capture: &CwdCapture,
        invocation_id: [u8; 16],
    ) -> Result<StreamAdministrationOutcome, RuntimeError> {
        let descriptor =
            admin_reference_descriptor(invocation_id, "sys.admin.resume_stream", &reference, None);
        if let Some(result) = self
            .replay_admin_invocation_receipt(lease, None, &descriptor)
            .await?
        {
            return admin_stream_result(result);
        }
        if crate::activation::runtime_activation_active(self) {
            return Err(self.reentrant_admin_busy_error(&descriptor, lease).await);
        }
        let key = self
            .resolve_admin_stream_reference(lease, &descriptor, &reference, expected_capture)
            .await?;
        let operation = AdminInvocationOperation::Resume { key };
        self.apply_admin_invocation_with_descriptor(
            lease,
            Some(expected_capture),
            operation,
            descriptor,
        )
        .await
        .and_then(admin_stream_result)
    }

    async fn apply_admin_invocation(
        &self,
        lease: WriterLease,
        expected_capture: Option<&CwdCapture>,
        operation: AdminInvocationOperation,
    ) -> Result<AdminOperationResult, RuntimeError> {
        let descriptor = admin_invocation_descriptor(&operation);
        self.apply_admin_invocation_with_descriptor(lease, expected_capture, operation, descriptor)
            .await
    }

    async fn apply_admin_invocation_with_descriptor(
        &self,
        lease: WriterLease,
        expected_capture: Option<&CwdCapture>,
        operation: AdminInvocationOperation,
        descriptor: AdminInvocationDescriptor,
    ) -> Result<AdminOperationResult, RuntimeError> {
        if let Some(result) = self
            .replay_admin_invocation_receipt(lease, Some(&operation), &descriptor)
            .await?
        {
            return Ok(result);
        }
        if crate::activation::runtime_activation_active(self) {
            return Err(self.reentrant_admin_busy_error(&descriptor, lease).await);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut admitted_generation: Option<u64> = None;
        let attempt = async {
            self.require_owner(&transaction, lease).await?;
            if let Some(receipt) =
                load_admin_invocation_receipt(&transaction, descriptor.invocation_id).await?
            {
                return admin_result_from_receipt(
                    &transaction,
                    Some(&operation),
                    &descriptor,
                    &receipt,
                )
                .await;
            }
            let current_capture = capture_tx(&transaction).await?;
            let observed_generation = admin_observed_generation(&current_capture)?;
            admitted_generation = Some(observed_generation);
            if let Some(expected_capture) = expected_capture
                && &current_capture != expected_capture
            {
                return Err(RuntimeError::StaleCapture {
                    current: Box::new(current_capture),
                });
            }
            let result =
                apply_admin_operation_tx(&transaction, &operation, observed_generation).await?;
            store_admin_invocation_audit(
                &transaction,
                &descriptor,
                lease,
                Some(observed_generation),
                admin_operation_outcome(&result),
                admin_operation_succeeded(&result),
                match &result {
                    AdminOperationResult::Checkpoint(checkpoint) => Some(checkpoint),
                    AdminOperationResult::Stream(_) => None,
                },
            )
            .await?;
            Ok::<_, RuntimeError>(result)
        }
        .await;
        match attempt {
            Ok(result) => {
                transaction.commit().await?;
                Ok(result)
            }
            Err(error) => {
                drop(transaction);
                if let Some(receipt) =
                    load_admin_invocation_receipt(&self.connection, descriptor.invocation_id)
                        .await?
                {
                    return admin_result_from_receipt(
                        &self.connection,
                        Some(&operation),
                        &descriptor,
                        &receipt,
                    )
                    .await;
                }
                match self
                    .record_failed_admin_descriptor(&descriptor, lease, admitted_generation, &error)
                    .await
                {
                    Ok(()) | Err(RuntimeError::OwnerLost) => Err(error),
                    Err(audit_error) => Err(audit_error),
                }
            }
        }
    }

    async fn replay_admin_invocation_receipt(
        &self,
        lease: WriterLease,
        operation: Option<&AdminInvocationOperation>,
        descriptor: &AdminInvocationDescriptor,
    ) -> Result<Option<AdminOperationResult>, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, lease).await?;
        let receipt = load_admin_invocation_receipt(&transaction, descriptor.invocation_id).await?;
        let result = match receipt.as_ref() {
            Some(receipt) => {
                Some(admin_result_from_receipt(&transaction, operation, descriptor, receipt).await?)
            }
            None => None,
        };
        transaction.commit().await?;
        Ok(result)
    }

    async fn reentrant_admin_busy_error(
        &self,
        descriptor: &AdminInvocationDescriptor,
        lease: WriterLease,
    ) -> RuntimeError {
        let error = RuntimeError::AdminBusy;
        match self
            .record_failed_admin_descriptor(descriptor, lease, None, &error)
            .await
        {
            Ok(()) | Err(RuntimeError::OwnerLost) => error,
            Err(audit_error) => audit_error,
        }
    }

    /// Records a failed admin call with its observed generation and stable
    /// failure class (ORNA-ADMIN-001). `admitted_generation` is the generation
    /// captured under the lock that admitted the call; a call rejected before
    /// any capture records the generation current when its failure is
    /// recorded. A `failure:stale_capture` row also stores the rejecting
    /// capture. If that capture cannot be read, the row records a NULL
    /// generation with `failure:unobservable:<class>`, where `<class>` is the
    /// stable class of the original error; the caller still returns that error.
    async fn record_failed_admin_descriptor(
        &self,
        descriptor: &AdminInvocationDescriptor,
        lease: WriterLease,
        admitted_generation: Option<u64>,
        error: &RuntimeError,
    ) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, lease).await?;
        if let Some(receipt) =
            load_admin_invocation_receipt(&transaction, descriptor.invocation_id).await?
        {
            let same_invocation = receipt.function == descriptor.function
                && receipt.safe_arguments == descriptor.safe_arguments
                && receipt.redacted == descriptor.redacted;
            transaction.commit().await?;
            return if same_invocation {
                Ok(())
            } else {
                Err(RuntimeError::AdminInvocationConflict)
            };
        }
        let observed_generation = match admitted_generation {
            Some(generation) => Some(generation),
            None => capture_tx(&transaction)
                .await
                .and_then(|capture| admin_observed_generation(&capture))
                .ok(),
        };
        let (terminal_outcome, stale_capture) = match (observed_generation, error) {
            (None, error) => (
                format!("failure:unobservable:{}", admin_failure_class(error)),
                None,
            ),
            (Some(_), RuntimeError::StaleCapture { current }) => {
                ("failure:stale_capture".to_owned(), Some(&**current))
            }
            (Some(_), error) => (format!("failure:{}", admin_failure_class(error)), None),
        };
        store_admin_invocation_audit(
            &transaction,
            descriptor,
            lease,
            observed_generation,
            terminal_outcome,
            false,
            None,
        )
        .await?;
        if let Some(current) = stale_capture {
            store_admin_stale_capture(&transaction, descriptor.invocation_id, current).await?;
        }
        transaction.commit().await.map_err(RuntimeError::from)
    }

    async fn record_failed_admin_and_return(
        &self,
        descriptor: &AdminInvocationDescriptor,
        lease: WriterLease,
        admitted_generation: Option<u64>,
        error: RuntimeError,
    ) -> RuntimeError {
        match self
            .record_failed_admin_descriptor(descriptor, lease, admitted_generation, &error)
            .await
        {
            Ok(()) | Err(RuntimeError::OwnerLost) => error,
            Err(audit_error) => audit_error,
        }
    }

    /// Resolves an admitted `sys.StreamRef` to the checkpoint of the live
    /// stream it names, under the writer fence and only while
    /// `expected_capture` is current. Each rejection is recorded under
    /// `descriptor` as a failed invocation (ORNA-ADMIN-001); a rejection
    /// decided under the fence records the generation the fence observed.
    async fn resolve_admin_stream_reference(
        &self,
        lease: WriterLease,
        descriptor: &AdminInvocationDescriptor,
        reference: &Value,
        expected_capture: &CwdCapture,
    ) -> Result<CheckpointKey, RuntimeError> {
        let fenced = async {
            let row = decode_row_ref(
                reference
                    .encode()
                    .map_err(|_| RuntimeError::InvalidObservationReference)?,
            )?;
            let requested = validate_stream_reference(row, expected_capture)
                .map_err(|_| RuntimeError::InvalidObservationReference)?;
            let fence = self.runtime_observation_fence(lease).await?;
            let generation = admin_observed_generation(fence.capture())?;
            Ok::<_, RuntimeError>((requested, fence, generation))
        }
        .await;
        let (requested, fence, generation) = match fenced {
            Ok(fenced) => fenced,
            Err(error) => {
                return Err(self
                    .record_failed_admin_and_return(descriptor, lease, None, error)
                    .await);
            }
        };
        let resolved = async {
            if fence.capture() != expected_capture {
                return Err(RuntimeError::StaleCapture {
                    current: Box::new(fence.capture().clone()),
                });
            }
            let view = self.current_runtime_observations(&fence).await?;
            view.streams
                .iter()
                .find_map(|stream| {
                    let run = view.runs.iter().find(|run| run.id == stream.run)?;
                    (stream.reference(run).ok().as_ref() == Some(&requested))
                        .then(|| stream.checkpoint.clone())
                })
                .ok_or(RuntimeError::InvalidObservationReference)
        }
        .await;
        match resolved {
            Ok(key) => Ok(key),
            Err(error) => Err(self
                .record_failed_admin_and_return(descriptor, lease, Some(generation), error)
                .await),
        }
    }
}
