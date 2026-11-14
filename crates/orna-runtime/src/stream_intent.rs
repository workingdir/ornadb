//! Stream commit intents: one transition function per CommitIntent.

use crate::RuntimeState;
use crate::checkpoint_snapshot::StreamCheckpointTransition;
use crate::codec::capture_tx;
use crate::error::RuntimeError;
use crate::lease::WriterLease;
use crate::observation::sync_stream_observation_tx;
use crate::stream_commit::StoredStreamReplayClaim;
use crate::stream_store::{
    encode_class, encode_code, encode_purpose, encode_status, ensure_stream_checkpoint,
    has_blocking_stream_failure, has_ordered_blocking_stream_failure, lease_matches,
    load_stream_checkpoint, load_stream_checkpoint_state, load_stream_failure, load_stream_lease,
    load_stream_replay_claim, load_stream_status, publish_pending_stream_pause,
    record_stream_checkpoint_watermark_tx, store_stream_failure_assertion_detail,
    store_stream_status, stream_checkpoint_matches, stream_identity_id, stream_key_id,
    stream_pause_pending,
};
use libsql::{Connection, TransactionBehavior, params};
use orna_stream::{
    AssertionDiagnosticDetail, CancellationClassification, CheckpointPrecondition, CommitIntent,
    CommitResult, DeliveryLease, DiagnosticCode, FailureIdentity, FailureRecord, FailureStatus,
    LeasePurpose, RejectReason, SafeDiagnostic, StreamState, StreamStatus,
};
use orna_stream_identity::{CheckpointKey, DeliveryIdentity, Position};
use orna_value::CwdCapture;
use std::ops::ControlFlow;

pub(crate) async fn apply_stream_intent(
    state: &RuntimeState,
    lease: WriterLease,
    expected_capture: Option<&CwdCapture>,
    intent: CommitIntent,
) -> Result<CommitResult, RuntimeError> {
    let transaction = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    state.require_owner(&transaction, lease).await?;
    if let CommitIntent::ReplayComplete {
        failure,
        expected_version,
    } = &intent
    {
        let Some(replay_claim) = load_stream_replay_claim(&transaction, failure).await? else {
            return Err(RuntimeError::RecoveryInvalid);
        };
        if replay_claim.version != *expected_version || replay_claim.owner != lease {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }
    if let Some(expected_capture) = expected_capture {
        let current_capture = capture_tx(&transaction).await?;
        if &current_capture != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current_capture),
            });
        }
    }
    if matches!(
        intent,
        CommitIntent::Fail { .. } | CommitIntent::FailWithAssertion { .. }
    ) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    // A reset must commit with its audit row (old/new position and reason) in
    // the same transaction. Only the administrative reset supplies that reason,
    // and reopen rejects reset-only checkpoints without an audit row, so the
    // stream-intent reset is refused here before it can persist.
    if matches!(intent, CommitIntent::Reset { .. }) {
        return Err(RuntimeError::RecoveryInvalid);
    }
    let transition = match &intent {
        CommitIntent::Complete { .. } => Some(StreamCheckpointTransition::Complete),
        CommitIntent::Skip { .. } => Some(StreamCheckpointTransition::Skip),
        _ => None,
    };
    let result = apply_stream_intent_tx(&transaction, intent).await;
    match result {
        Ok(result) => {
            if let (Some(transition), CommitResult::CheckpointAdvanced { checkpoint }) =
                (transition, &result)
            {
                let capture = capture_tx(&transaction).await?;
                record_stream_checkpoint_watermark_tx(
                    &transaction,
                    &checkpoint.key,
                    checkpoint,
                    &capture,
                    transition,
                )
                .await?;
            }
            sync_stream_observation_tx(&transaction, &result).await?;
            transaction.commit().await?;
            Ok(result)
        }
        Err(error) => Err(error),
    }
}

/// Applies one stream intent inside the caller's writer transaction. Every
/// intent is its own transition over the checkpoint, lease and failure rows.
/// A refused transition returns `CommitResult::Rejected` without writing.
pub(crate) async fn apply_stream_intent_tx(
    connection: &Connection,
    intent: CommitIntent,
) -> Result<CommitResult, RuntimeError> {
    match intent {
        CommitIntent::Pause { key } => pause_stream_tx(connection, key).await,
        CommitIntent::Resume { key } => resume_stream_tx(connection, key).await,
        CommitIntent::Reset { key, expected, to } => {
            reset_stream_checkpoint_tx(connection, key, &expected, &to).await
        }
        CommitIntent::Acquire {
            delivery,
            expected,
            purpose,
        } => acquire_delivery_tx(connection, delivery, &expected, purpose).await,
        CommitIntent::Fail { lease, diagnostic } => {
            fail_delivery_tx(connection, lease, diagnostic).await
        }
        CommitIntent::FailWithAssertion {
            lease,
            diagnostic,
            detail,
        } => fail_delivery_with_assertion_tx(connection, lease, diagnostic, &detail).await,
        CommitIntent::Retry {
            failure,
            expected_version,
            expected,
        } => retry_failed_delivery_tx(connection, failure, expected_version, &expected).await,
        CommitIntent::Complete { lease, expected } => {
            complete_delivery_tx(connection, lease, &expected).await
        }
        CommitIntent::Skip {
            lease,
            expected,
            expected_failure_version,
        } => skip_failed_delivery_tx(connection, lease, &expected, expected_failure_version).await,
        CommitIntent::Replay {
            failure,
            expected_version,
        } => grant_replay_tx(connection, failure, expected_version).await,
        CommitIntent::ReplayComplete {
            failure,
            expected_version,
        } => complete_replay_tx(connection, failure, expected_version).await,
        CommitIntent::ReplayFail {
            failure,
            expected_version,
            diagnostic,
        } => fail_replay_tx(connection, failure, expected_version, diagnostic).await,
        CommitIntent::ReplayFailWithAssertion {
            failure,
            expected_version,
            diagnostic,
            detail,
        } => {
            fail_replay_with_assertion_tx(
                connection,
                failure,
                expected_version,
                diagnostic,
                &detail,
            )
            .await
        }
        CommitIntent::ReplayCancel {
            failure,
            expected_version,
        } => cancel_replay_tx(connection, failure, expected_version).await,
        CommitIntent::Resolve {
            failure,
            expected_version,
        } => resolve_failure_tx(connection, failure, expected_version).await,
        CommitIntent::Cancel { lease } => cancel_delivery_tx(connection, lease).await,
    }
}

fn stream_state(key: CheckpointKey, status: StreamStatus) -> StreamState {
    StreamState { key, status }
}

async fn pause_stream_tx(
    connection: &Connection,
    key: CheckpointKey,
) -> Result<CommitResult, RuntimeError> {
    ensure_stream_checkpoint(connection, &key).await?;
    if load_stream_status(connection, &key).await? == StreamStatus::Paused {
        return Ok(CommitResult::StreamStatusChanged {
            state: stream_state(key, StreamStatus::Paused),
            changed: false,
        });
    }
    if load_stream_lease(connection, &key).await?.is_some() {
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO stream_pause_pending (key_id) VALUES (?1)",
                params![stream_key_id(&key)],
            )
            .await?
            != 0;
        return Ok(CommitResult::PausePending {
            state: stream_state(key, StreamStatus::Running),
            changed,
        });
    }
    store_stream_status(connection, &key, StreamStatus::Paused).await?;
    Ok(CommitResult::StreamStatusChanged {
        state: stream_state(key, StreamStatus::Paused),
        changed: true,
    })
}

async fn resume_stream_tx(
    connection: &Connection,
    key: CheckpointKey,
) -> Result<CommitResult, RuntimeError> {
    ensure_stream_checkpoint(connection, &key).await?;
    if load_stream_status(connection, &key).await? == StreamStatus::Running {
        return Ok(CommitResult::StreamStatusChanged {
            state: stream_state(key, StreamStatus::Running),
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
        state: stream_state(key, StreamStatus::Running),
        changed: true,
    })
}

async fn reset_stream_checkpoint_tx(
    connection: &Connection,
    key: CheckpointKey,
    expected: &CheckpointPrecondition,
    to: &Position,
) -> Result<CommitResult, RuntimeError> {
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
    if !stream_checkpoint_matches(connection, &key, expected).await? {
        return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
    }
    let key_id = stream_key_id(&key);
    connection
        .execute(
            "UPDATE stream_checkpoint
             SET committed_position = ?2, version = version + 1
             WHERE key_id = ?1",
            params![key_id.clone(), to.token.as_str().to_owned()],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM stream_provider_failure WHERE key_id = ?1",
            params![key_id],
        )
        .await?;
    Ok(CommitResult::CheckpointReset {
        checkpoint: load_stream_checkpoint(connection, &key).await?,
    })
}

/// Takes the next fence for `key` and records `delivery` as the stream's one
/// active lease under it.
async fn issue_stream_lease_tx(
    connection: &Connection,
    key: &CheckpointKey,
    delivery: &DeliveryIdentity,
    purpose: LeasePurpose,
) -> Result<u64, RuntimeError> {
    let (_, next_fence) = load_stream_checkpoint_state(connection, key).await?;
    let fence = next_fence
        .checked_add(1)
        .ok_or(RuntimeError::RecoveryInvalid)?;
    let key_id = stream_key_id(key);
    connection
        .execute(
            "UPDATE stream_checkpoint SET next_fence = ?2 WHERE key_id = ?1",
            params![
                key_id.clone(),
                i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?
            ],
        )
        .await?;
    connection
        .execute(
            "INSERT INTO stream_lease
             (key_id, delivery_position, successor_position, fence, purpose)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                key_id,
                delivery.position.token.as_str().to_owned(),
                delivery.successor.token.as_str().to_owned(),
                i64::try_from(fence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                encode_purpose(purpose),
            ],
        )
        .await?;
    Ok(fence)
}

/// Drops the stream's active lease together with the retry claim that was
/// riding on it.
async fn drop_stream_lease_tx(
    connection: &Connection,
    key: &CheckpointKey,
) -> Result<(), RuntimeError> {
    let key_id = stream_key_id(key);
    connection
        .execute(
            "DELETE FROM stream_lease WHERE key_id = ?1",
            params![key_id.clone()],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM stream_retry_claim WHERE key_id = ?1",
            params![key_id],
        )
        .await?;
    Ok(())
}

/// Whether the stream's stored lease is exactly `lease`.
async fn stream_lease_is_held(
    connection: &Connection,
    key: &CheckpointKey,
    lease: &DeliveryLease,
) -> Result<bool, RuntimeError> {
    Ok(load_stream_lease(connection, key)
        .await?
        .is_some_and(|stored| lease_matches(lease, &stored)))
}

/// Moves the failure row of `failure` to `status`, bumping its version.
async fn transition_stream_failure_tx(
    connection: &Connection,
    failure: &FailureIdentity,
    status: FailureStatus,
) -> Result<(), RuntimeError> {
    connection
        .execute(
            "UPDATE stream_failure SET version = version + 1, status = ?2
             WHERE identity_id = ?1",
            params![stream_identity_id(failure), encode_status(status)],
        )
        .await?;
    Ok(())
}

async fn reload_stream_failure(
    connection: &Connection,
    failure: &FailureIdentity,
) -> Result<FailureRecord, RuntimeError> {
    load_stream_failure(connection, failure)
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)
}

async fn acquire_delivery_tx(
    connection: &Connection,
    delivery: DeliveryIdentity,
    expected: &CheckpointPrecondition,
    purpose: LeasePurpose,
) -> Result<CommitResult, RuntimeError> {
    let key = delivery.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    if !stream_checkpoint_matches(connection, &key, expected).await? {
        return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
    }
    let identity = FailureIdentity(delivery.clone());
    let failure_status = load_stream_failure(connection, &identity)
        .await?
        .map(|failure| failure.status);
    let authorised_blocking_delivery = matches!(
        (purpose, failure_status),
        (LeasePurpose::Deliver, Some(FailureStatus::Retrying))
            | (LeasePurpose::Skip, Some(FailureStatus::Failed))
    );
    if has_ordered_blocking_stream_failure(connection, &key).await? && !authorised_blocking_delivery
    {
        return Ok(CommitResult::Rejected(RejectReason::BlockingFailure));
    }
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
                .await?;
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
                    .await?;
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
                | (
                    LeasePurpose::Deliver,
                    FailureStatus::Succeeded | FailureStatus::Resolved
                )
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
    let fence = issue_stream_lease_tx(connection, &key, &delivery, purpose).await?;
    connection
        .execute(
            "DELETE FROM stream_provider_failure WHERE key_id = ?1",
            params![stream_key_id(&key)],
        )
        .await?;
    Ok(CommitResult::Acquired {
        lease: DeliveryLease {
            delivery,
            fence,
            purpose,
        },
    })
}

async fn fail_delivery_tx(
    connection: &Connection,
    lease: DeliveryLease,
    diagnostic: SafeDiagnostic,
) -> Result<CommitResult, RuntimeError> {
    let key = lease.delivery.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    if lease.purpose != LeasePurpose::Deliver
        || !stream_lease_is_held(connection, &key, &lease).await?
    {
        return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
    }
    drop_stream_lease_tx(connection, &key).await?;
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
                successor_position = excluded.successor_position,
                status = ?14,
                diagnostic_code = ?15,
                diagnostic_class = ?16",
            params![
                identity_id.clone(),
                stream_key_id(&key),
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
        .await?;
    connection
        .execute(
            "DELETE FROM stream_failure_assertion_detail WHERE identity_id = ?1",
            params![identity_id],
        )
        .await?;
    publish_pending_stream_pause(connection, &key).await?;
    Ok(CommitResult::Failed {
        failure: reload_stream_failure(connection, &identity).await?,
    })
}

async fn fail_delivery_with_assertion_tx(
    connection: &Connection,
    lease: DeliveryLease,
    supplied_diagnostic: SafeDiagnostic,
    detail: &AssertionDiagnosticDetail,
) -> Result<CommitResult, RuntimeError> {
    let diagnostic = SafeDiagnostic {
        code: DiagnosticCode::TableAssertionFalse,
        class: supplied_diagnostic.class,
    };
    let result = fail_delivery_tx(connection, lease, diagnostic).await?;
    if let CommitResult::Failed { failure } = &result {
        store_stream_failure_assertion_detail(connection, &failure.identity, detail).await?;
        return Ok(CommitResult::Failed {
            failure: reload_stream_failure(connection, &failure.identity).await?,
        });
    }
    Ok(result)
}

async fn retry_failed_delivery_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
    expected: &CheckpointPrecondition,
) -> Result<CommitResult, RuntimeError> {
    let key = failure.0.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    if !stream_checkpoint_matches(connection, &key, expected).await? {
        return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
    }
    let Some(record) = load_stream_failure(connection, &failure).await? else {
        return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
    };
    if record.version != expected_version || record.identity.0.successor != failure.0.successor {
        return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
    }
    if record.status != FailureStatus::Failed {
        return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
    }
    if load_stream_lease(connection, &key).await?.is_some() {
        return Ok(CommitResult::Rejected(RejectReason::LeaseAlreadyHeld));
    }
    issue_stream_lease_tx(connection, &key, &failure.0, LeasePurpose::Deliver).await?;
    let identity_id = stream_identity_id(&failure);
    connection
        .execute(
            "INSERT INTO stream_retry_claim (key_id, identity_id)
             VALUES (?1, ?2)",
            params![stream_key_id(&key), identity_id.clone()],
        )
        .await?;
    connection
        .execute(
            "UPDATE stream_failure SET version = version + 1, attempts = attempts + 1, status = ?2
             WHERE identity_id = ?1",
            params![identity_id, encode_status(FailureStatus::Retrying)],
        )
        .await?;
    Ok(CommitResult::RetryScheduled {
        failure: reload_stream_failure(connection, &failure).await?,
    })
}

/// Moves the checkpoint past the leased delivery, drops the lease, and
/// settles the delivery's failure row as `status`.
async fn advance_checkpoint_past_delivery_tx(
    connection: &Connection,
    key: &CheckpointKey,
    delivery: DeliveryIdentity,
    status: FailureStatus,
) -> Result<CommitResult, RuntimeError> {
    connection
        .execute(
            "UPDATE stream_checkpoint
             SET committed_position = ?2, version = version + 1
             WHERE key_id = ?1",
            params![
                stream_key_id(key),
                delivery.successor.token.as_str().to_owned()
            ],
        )
        .await?;
    drop_stream_lease_tx(connection, key).await?;
    transition_stream_failure_tx(connection, &FailureIdentity(delivery), status).await?;
    publish_pending_stream_pause(connection, key).await?;
    Ok(CommitResult::CheckpointAdvanced {
        checkpoint: load_stream_checkpoint(connection, key).await?,
    })
}

async fn complete_delivery_tx(
    connection: &Connection,
    lease: DeliveryLease,
    expected: &CheckpointPrecondition,
) -> Result<CommitResult, RuntimeError> {
    let key = lease.delivery.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    if !stream_checkpoint_matches(connection, &key, expected).await? {
        return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
    }
    if lease.purpose != LeasePurpose::Deliver
        || !stream_lease_is_held(connection, &key, &lease).await?
    {
        return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
    }
    advance_checkpoint_past_delivery_tx(connection, &key, lease.delivery, FailureStatus::Succeeded)
        .await
}

async fn skip_failed_delivery_tx(
    connection: &Connection,
    lease: DeliveryLease,
    expected: &CheckpointPrecondition,
    expected_failure_version: u64,
) -> Result<CommitResult, RuntimeError> {
    let key = lease.delivery.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    let identity = FailureIdentity(lease.delivery.clone());
    let Some(record) = load_stream_failure(connection, &identity).await? else {
        return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
    };
    if record.version != expected_failure_version
        || record.status != FailureStatus::Failed
        || record.identity.0.successor != lease.delivery.successor
    {
        return Ok(CommitResult::Rejected(RejectReason::StaleFailure));
    }
    if !stream_checkpoint_matches(connection, &key, expected).await? {
        return Ok(CommitResult::Rejected(RejectReason::StaleCheckpoint));
    }
    if lease.purpose != LeasePurpose::Skip
        || !stream_lease_is_held(connection, &key, &lease).await?
    {
        return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
    }
    advance_checkpoint_past_delivery_tx(connection, &key, lease.delivery, FailureStatus::Skipped)
        .await
}

/// Loads the failure a replay-family intent targets after the checks every
/// one of them shares: the failure exists and still has `expected_version`.
async fn load_replay_target(
    connection: &Connection,
    failure: &FailureIdentity,
    expected_version: u64,
) -> Result<ControlFlow<RejectReason, FailureRecord>, RuntimeError> {
    ensure_stream_checkpoint(connection, &failure.0.checkpoint_key()).await?;
    let Some(record) = load_stream_failure(connection, failure).await? else {
        return Ok(ControlFlow::Break(RejectReason::FailureMissing));
    };
    if record.version != expected_version {
        return Ok(ControlFlow::Break(RejectReason::StaleFailure));
    }
    Ok(ControlFlow::Continue(record))
}

/// Loads the replay claim of a failure that is mid-replay. A claim recorded
/// for a different version means the replay this intent names is stale.
async fn load_replaying_claim(
    connection: &Connection,
    failure: &FailureIdentity,
    expected_version: u64,
) -> Result<ControlFlow<RejectReason, Option<StoredStreamReplayClaim>>, RuntimeError> {
    let record = match load_replay_target(connection, failure, expected_version).await? {
        ControlFlow::Continue(record) => record,
        ControlFlow::Break(reason) => return Ok(ControlFlow::Break(reason)),
    };
    if record.status != FailureStatus::Replaying {
        return Ok(ControlFlow::Break(RejectReason::RetryNotAllowed));
    }
    let claim = load_stream_replay_claim(connection, failure).await?;
    if claim.is_some_and(|claim| claim.version != expected_version) {
        return Ok(ControlFlow::Break(RejectReason::StaleFailure));
    }
    Ok(ControlFlow::Continue(claim))
}

/// Removes the replay claim a finished replay held. The claim was read in
/// this transaction, so failing to delete exactly one row means the durable
/// state is not what it was read as.
async fn delete_replay_claim_tx(
    connection: &Connection,
    failure: &FailureIdentity,
    version: u64,
) -> Result<(), RuntimeError> {
    let deleted = connection
        .execute(
            "DELETE FROM stream_replay_claim
             WHERE identity_id = ?1 AND version = ?2",
            params![
                stream_identity_id(failure),
                i64::try_from(version).map_err(|_| RuntimeError::RecoveryInvalid)?,
            ],
        )
        .await?;
    if deleted != 1 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok(())
}

async fn grant_replay_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
) -> Result<CommitResult, RuntimeError> {
    let record = match load_replay_target(connection, &failure, expected_version).await? {
        ControlFlow::Continue(record) => record,
        ControlFlow::Break(reason) => return Ok(CommitResult::Rejected(reason)),
    };
    if record.status != FailureStatus::Skipped {
        return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
    }
    if record.attempts == u32::MAX {
        return Err(RuntimeError::RecoveryInvalid);
    }
    connection
        .execute(
            "UPDATE stream_failure SET version = version + 1, attempts = attempts + 1, status = ?2
             WHERE identity_id = ?1",
            params![
                stream_identity_id(&failure),
                encode_status(FailureStatus::Replaying)
            ],
        )
        .await?;
    Ok(CommitResult::ReplayGranted {
        grant: orna_stream::ReplayGrant {
            failure,
            version: record.version + 1,
        },
    })
}

async fn complete_replay_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
) -> Result<CommitResult, RuntimeError> {
    let claim = match load_replaying_claim(connection, &failure, expected_version).await? {
        ControlFlow::Continue(claim) => claim,
        ControlFlow::Break(reason) => return Ok(CommitResult::Rejected(reason)),
    };
    transition_stream_failure_tx(connection, &failure, FailureStatus::Replayed).await?;
    if claim.is_some() {
        delete_replay_claim_tx(connection, &failure, expected_version).await?;
    }
    Ok(CommitResult::ReplayCompleted {
        failure: reload_stream_failure(connection, &failure).await?,
    })
}

async fn fail_replay_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
    diagnostic: SafeDiagnostic,
) -> Result<CommitResult, RuntimeError> {
    let claim = match load_replaying_claim(connection, &failure, expected_version).await? {
        ControlFlow::Continue(claim) => claim,
        ControlFlow::Break(reason) => return Ok(CommitResult::Rejected(reason)),
    };
    connection
        .execute(
            "UPDATE stream_failure
             SET version = version + 1,
                 status = ?2, diagnostic_code = ?3, diagnostic_class = ?4
             WHERE identity_id = ?1",
            params![
                stream_identity_id(&failure),
                encode_status(FailureStatus::Skipped),
                encode_code(diagnostic.code),
                encode_class(diagnostic.class),
            ],
        )
        .await?;
    connection
        .execute(
            "DELETE FROM stream_failure_assertion_detail WHERE identity_id = ?1",
            params![stream_identity_id(&failure)],
        )
        .await?;
    if claim.is_some() {
        delete_replay_claim_tx(connection, &failure, expected_version).await?;
    }
    Ok(CommitResult::ReplayFailed {
        failure: reload_stream_failure(connection, &failure).await?,
    })
}

async fn fail_replay_with_assertion_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
    supplied_diagnostic: SafeDiagnostic,
    detail: &AssertionDiagnosticDetail,
) -> Result<CommitResult, RuntimeError> {
    let diagnostic = SafeDiagnostic {
        code: DiagnosticCode::TableAssertionFalse,
        class: supplied_diagnostic.class,
    };
    let result = fail_replay_tx(connection, failure.clone(), expected_version, diagnostic).await?;
    if let CommitResult::ReplayFailed { .. } = result {
        store_stream_failure_assertion_detail(connection, &failure, detail).await?;
        return Ok(CommitResult::ReplayFailed {
            failure: reload_stream_failure(connection, &failure).await?,
        });
    }
    Ok(result)
}

async fn cancel_replay_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
) -> Result<CommitResult, RuntimeError> {
    let claim = match load_replaying_claim(connection, &failure, expected_version).await? {
        ControlFlow::Continue(claim) => claim,
        ControlFlow::Break(reason) => return Ok(CommitResult::Rejected(reason)),
    };
    if claim.is_some() {
        // A durable claim means a replay handler has begun work. Only its
        // owner-fenced completion path may end that work.
        return Ok(CommitResult::Rejected(RejectReason::LeaseAlreadyHeld));
    }
    transition_stream_failure_tx(connection, &failure, FailureStatus::Skipped).await?;
    Ok(CommitResult::ReplayCancelled {
        failure: reload_stream_failure(connection, &failure).await?,
    })
}

async fn resolve_failure_tx(
    connection: &Connection,
    failure: FailureIdentity,
    expected_version: u64,
) -> Result<CommitResult, RuntimeError> {
    let record = match load_replay_target(connection, &failure, expected_version).await? {
        ControlFlow::Continue(record) => record,
        ControlFlow::Break(reason) => return Ok(CommitResult::Rejected(reason)),
    };
    if !matches!(
        record.status,
        FailureStatus::Succeeded | FailureStatus::Skipped | FailureStatus::Replayed
    ) {
        return Ok(CommitResult::Rejected(RejectReason::ResolveBlocked));
    }
    transition_stream_failure_tx(connection, &failure, FailureStatus::Resolved).await?;
    Ok(CommitResult::Resolved {
        failure: reload_stream_failure(connection, &failure).await?,
    })
}

async fn cancel_delivery_tx(
    connection: &Connection,
    lease: DeliveryLease,
) -> Result<CommitResult, RuntimeError> {
    let key = lease.delivery.checkpoint_key();
    ensure_stream_checkpoint(connection, &key).await?;
    if !stream_lease_is_held(connection, &key, &lease).await? {
        return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
    }
    drop_stream_lease_tx(connection, &key).await?;
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
        .await?;
    publish_pending_stream_pause(connection, &key).await?;
    Ok(CommitResult::Cancelled {
        checkpoint: load_stream_checkpoint(connection, &key).await?,
        classification: CancellationClassification::RollbackShaped,
    })
}
