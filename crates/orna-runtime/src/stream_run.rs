//! The stream runner: admission control, polling, handler execution and delivery closing.

use crate::RuntimeState;
use crate::codec::{decode_u32, decode_u64};
use crate::error::{FaultInjector, FaultPoint, NoFault, RuntimeError, StreamTableDeliveryError};
use crate::lease::WriterLease;
use crate::observation::{
    StreamObservationStatus, sync_stream_observation_event_tx, sync_stream_observation_tx,
};
use crate::request::MAX_TERMINAL_OUTCOME_BYTES;
use crate::stream_commit::{StreamDeliveryCommit, StreamValidatedTableDeliveryCommit};
use crate::stream_intent::apply_stream_intent_tx;
use crate::stream_source::{
    MAX_STREAM_PROVIDER_EFFECT_ATTEMPTS, MAX_STREAM_PROVIDER_EFFECTS,
    ProviderEffectDeliveryContract, ProviderEffectDispatchError, ProviderEffectFailure,
    ProviderEffectKey, ProviderEffectPreparation, StreamHandler, StreamHandlerResult, StreamItem,
    StreamProviderEffect, StreamProviderFailure, StreamSource, StreamSourceKind, StreamSourcePoll,
    is_cancellation_diagnostic,
};
use crate::stream_store::{
    FailurePayloadColumns, StoredFailurePayload, encode_class, encode_code,
    ensure_stream_checkpoint, ensure_stream_position_format_compatible, lease_matches,
    load_stream_checkpoint, load_stream_failure, load_stream_lease, load_stream_status,
    stream_identity_id, stream_key_id,
};
use libsql::{Transaction, TransactionBehavior, params};
use orna_stream::Checkpoint as StreamCheckpoint;
use orna_stream::{
    AssertionDiagnosticDetail, AsyncCheckpointBackend, AsyncFailurePayloadBackend, CheckpointKey,
    CheckpointPrecondition, CommitIntent, CommitResult, DeliveryIdentity, DeliveryLease,
    DiagnosticClass, DiagnosticCode, FailureIdentity, FailureRecord, FailureStatus, LeasePurpose,
    RejectReason, SafeDiagnostic, StreamFailurePayload, StreamStatus,
};
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// What one provider poll produced, with the checkpoint it was polled at.
struct PolledStreamItem {
    checkpoint: StreamCheckpoint,
    item: Box<StreamItem>,
}

/// One admitted delivery: the item, the checkpoint precondition it was
/// admitted under, the lease it holds, and the payload retained should the
/// delivery fail.
struct StreamDelivery {
    item: Box<StreamItem>,
    expected: CheckpointPrecondition,
    lease: DeliveryLease,
    failure_payload: StreamFailurePayload,
}

/// How a provider poll or wait failure left the run.
enum ProviderFailureOutcome {
    /// Cancellation won: the run ended as cancelled and no failure is retained.
    Cancelled,
    /// The failure is retained and propagates to the caller.
    Retained,
}

/// The step a delivery's closing commit reports, or `None` when the commit
/// returned a result that closing intent can never produce.
fn committed_step(result: CommitResult) -> Option<StreamStep> {
    match result {
        CommitResult::CheckpointAdvanced { checkpoint } => {
            Some(StreamStep::Committed { checkpoint })
        }
        _ => None,
    }
}

fn failed_step(result: CommitResult) -> Option<StreamStep> {
    match result {
        CommitResult::Failed { failure } => Some(StreamStep::Failed {
            failure: Box::new(failure),
        }),
        _ => None,
    }
}

fn cancelled_step(result: CommitResult) -> Option<StreamStep> {
    match result {
        CommitResult::Cancelled { checkpoint, .. } => Some(StreamStep::Cancelled { checkpoint }),
        _ => None,
    }
}

/// Lets a stream owner stop admission between delivery transactions.
pub trait StreamRunControl {
    fn cancelled(&self) -> bool;
    /// Resolves when a blocked runner must re-check cancellation.
    ///
    /// The default is deliberately inert for controls that never request
    /// asynchronous cancellation. A control used with a provider wait must
    /// override this so the runtime, rather than the provider, owns wakeup on
    /// cancellation.
    fn cancellation_wait<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(std::future::pending())
    }
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

/// Where a [`StreamRunGate`] stands between delivery admissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GateState {
    /// No admission is in flight and nobody asked to cancel.
    Running,
    /// A delivery admission holds the linearization point.
    Admitting,
    /// Cancellation arrived during an admission. It becomes effective when
    /// the admission is released, so the durable acquire attempt that is
    /// already in flight is never torn.
    CancelRequested,
    /// Cancellation is in effect; no further admission is granted.
    Cancelled,
}

/// A cancellation gate which linearizes cancellation against one delivery
/// admission. Cancellation after admission is retained for the next boundary.
#[derive(Clone, Debug)]
pub struct StreamRunGate {
    state: Arc<Mutex<GateState>>,
    cancellation: Arc<Notify>,
}

impl StreamRunGate {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(GateState::Running)),
            cancellation: Arc::new(Notify::new()),
        }
    }

    /// Runs one transition under the gate lock. The guarded value is a `Copy`
    /// enum that every transition leaves valid, so a poisoned lock carries no
    /// torn state and is recovered rather than propagated.
    fn transition<T>(&self, step: impl FnOnce(&mut GateState) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        step(&mut state)
    }

    /// Requests cancellation. `true` means this call first recorded it.
    pub fn cancel(&self) -> bool {
        let recorded = self.transition(|state| match *state {
            GateState::Running => {
                *state = GateState::Cancelled;
                true
            }
            GateState::Admitting => {
                *state = GateState::CancelRequested;
                true
            }
            GateState::CancelRequested | GateState::Cancelled => false,
        });
        if recorded {
            self.cancellation.notify_one();
        }
        recorded
    }
}

impl Default for StreamRunGate {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRunControl for StreamRunGate {
    fn cancelled(&self) -> bool {
        self.transition(|state| matches!(*state, GateState::CancelRequested | GateState::Cancelled))
    }

    fn cancellation_wait<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(async move {
            let notified = self.cancellation.notified();
            if self.cancelled() {
                return;
            }
            notified.await;
        })
    }

    fn acquire_admission(&self) -> bool {
        self.transition(|state| match *state {
            GateState::Running => {
                *state = GateState::Admitting;
                true
            }
            GateState::Admitting | GateState::CancelRequested | GateState::Cancelled => false,
        })
    }

    fn release_admission(&self) {
        self.transition(|state| match *state {
            GateState::Admitting => *state = GateState::Running,
            GateState::CancelRequested => *state = GateState::Cancelled,
            GateState::Running | GateState::Cancelled => {}
        });
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
    Paused {
        delivered: usize,
        checkpoint: StreamCheckpoint,
    },
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
    Paused { checkpoint: StreamCheckpoint },
    Waiting,
    Exhausted,
    Committed { checkpoint: StreamCheckpoint },
    Failed { failure: Box<FailureRecord> },
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

impl RuntimeState {
    async fn record_stream_provider_failure(
        &self,
        writer: WriterLease,
        expected: &StreamCheckpoint,
        diagnostic: SafeDiagnostic,
    ) -> Result<StreamProviderFailure, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
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
            .await?;
        let previous = rows
            .next()
            .await?
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
            .await?;
        sync_stream_observation_event_tx(
            &transaction,
            &expected.key,
            StreamObservationStatus::BackingOff,
            Some(diagnostic),
        )
        .await?;
        transaction.commit().await?;
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
            .await?;
        self.require_owner(&transaction, writer).await?;
        transaction
            .execute(
                "DELETE FROM stream_provider_failure WHERE key_id = ?1",
                params![stream_key_id(key)],
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    pub(crate) async fn fail_stream_delivery(
        &self,
        writer: WriterLease,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        self.fail_stream_delivery_with_faults(writer, lease, diagnostic, None, payload, &NoFault)
            .await
    }

    pub(crate) async fn fail_stream_delivery_with_faults(
        &self,
        writer: WriterLease,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        assertion_detail: Option<&AssertionDiagnosticDetail>,
        payload: StreamFailurePayload,
        faults: &dyn FaultInjector,
    ) -> Result<CommitResult, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, writer).await?;
        let result = self
            .fail_stream_delivery_tx(
                &transaction,
                lease,
                diagnostic,
                assertion_detail,
                payload,
                faults,
            )
            .await?;
        transaction.commit().await?;
        Ok(result)
    }

    /// Finishes a retry whose `Retry` transition already acquired the
    /// delivery lease. The retry admission remains a separately committed
    /// durable boundary before handler code; this helper only avoids a
    /// redundant second admission transaction when a bounded witness models
    /// the handler's immediate failure.
    pub(crate) async fn fail_stream_retry_delivery(
        &self,
        writer: WriterLease,
        delivery: DeliveryIdentity,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Result<CommitResult, RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&transaction, writer).await?;
        let identity = FailureIdentity(delivery.clone());
        let Some(failure) = load_stream_failure(&transaction, &identity).await? else {
            return Ok(CommitResult::Rejected(RejectReason::FailureMissing));
        };
        if failure.status != FailureStatus::Retrying {
            return Ok(CommitResult::Rejected(RejectReason::RetryNotAllowed));
        }
        let key = delivery.checkpoint_key();
        let Some(stored) = load_stream_lease(&transaction, &key).await? else {
            return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
        };
        let lease = DeliveryLease {
            delivery,
            fence: stored.fence,
            purpose: stored.purpose,
        };
        if lease.purpose != LeasePurpose::Deliver || !lease_matches(&lease, &stored) {
            return Ok(CommitResult::Rejected(RejectReason::LeaseFenced));
        }
        let result = self
            .fail_stream_delivery_tx(&transaction, lease, diagnostic, None, payload, &NoFault)
            .await?;
        transaction.commit().await?;
        Ok(result)
    }

    async fn fail_stream_delivery_tx(
        &self,
        transaction: &Transaction,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        assertion_detail: Option<&AssertionDiagnosticDetail>,
        payload: StreamFailurePayload,
        faults: &dyn FaultInjector,
    ) -> Result<CommitResult, RuntimeError> {
        let FailurePayloadColumns {
            payload: plaintext,
            reference,
            digest,
            retention,
        } = StoredFailurePayload::from_submitted(payload)?.into_columns();
        let result = match assertion_detail {
            Some(detail) => {
                apply_stream_intent_tx(
                    transaction,
                    CommitIntent::FailWithAssertion {
                        lease,
                        diagnostic,
                        detail: detail.clone(),
                    },
                )
                .await?
            }
            None => {
                apply_stream_intent_tx(transaction, CommitIntent::Fail { lease, diagnostic })
                    .await?
            }
        };
        if let CommitResult::Failed { failure } = &result {
            sync_stream_observation_tx(transaction, &result).await?;
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
                .await?;
            faults.check(FaultPoint::AfterFailurePayload)?;
        }
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

    /// Cleanup after admission must never hide the error that caused the
    /// delivery to stop. The lease is a recovery resource, so release it on
    /// a best-effort basis while returning the original runtime failure.
    async fn release_stream_lease_best_effort(&self, writer: WriterLease, lease: DeliveryLease) {
        let _ = self.release_stream_lease(writer, lease).await;
    }

    async fn dispatch_stream_provider_effects<H: StreamHandler>(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        handler: &mut H,
        effects: &[StreamProviderEffect],
    ) -> Result<(), ProviderEffectDispatchError> {
        if effects.len() > MAX_STREAM_PROVIDER_EFFECTS {
            return Err(ProviderEffectDispatchError::Provider(
                ProviderEffectFailure::Permanent,
            ));
        }
        for (ordinal, effect) in effects.iter().enumerate() {
            if handler.provider_effect_contract(effect.provider())
                != ProviderEffectDeliveryContract::IdempotentByKey
            {
                return Err(ProviderEffectDispatchError::Provider(
                    ProviderEffectFailure::Unsupported,
                ));
            }

            let delivery = lease.delivery.canonical();
            let mut identity = b"ORNA-STREAM-PROVIDER-EFFECT/v1\0".to_vec();
            for part in [
                delivery.as_bytes(),
                effect.provider().as_bytes(),
                effect.operation().as_bytes(),
            ] {
                identity.extend_from_slice(&(part.len() as u64).to_be_bytes());
                identity.extend_from_slice(part);
            }
            identity.extend_from_slice(&(ordinal as u64).to_be_bytes());

            let request_digest: [u8; 32] = Sha256::digest(effect.request()).into();
            let mut key_hash = Sha256::new();
            key_hash.update(b"ORNA-STREAM-PROVIDER-EFFECT-KEY/v1\0");
            key_hash.update((identity.len() as u64).to_be_bytes());
            key_hash.update(&identity);
            key_hash.update(request_digest);
            let effect_key = ProviderEffectKey(key_hash.finalize().into());

            loop {
                let preparation = self
                    .prepare_stream_provider_effect_attempt(
                        writer,
                        lease,
                        &identity,
                        request_digest,
                        effect_key,
                    )
                    .await
                    .map_err(ProviderEffectDispatchError::Runtime)?;
                let attempt = match preparation {
                    ProviderEffectPreparation::Cached => break,
                    ProviderEffectPreparation::Started { attempt } => attempt,
                    ProviderEffectPreparation::Rejected(failure) => {
                        return Err(ProviderEffectDispatchError::Provider(failure));
                    }
                };

                match handler.apply_provider_effect_once(effect_key, effect).await {
                    Ok(receipt) if receipt.len() <= MAX_TERMINAL_OUTCOME_BYTES => {
                        self.complete_stream_provider_effect(
                            writer,
                            lease,
                            &identity,
                            request_digest,
                            effect_key,
                            receipt,
                        )
                        .await
                        .map_err(ProviderEffectDispatchError::Runtime)?;
                        break;
                    }
                    Ok(_) => {
                        self.fail_stream_provider_effect(writer, lease, &identity, false)
                            .await
                            .map_err(ProviderEffectDispatchError::Runtime)?;
                        return Err(ProviderEffectDispatchError::Provider(
                            ProviderEffectFailure::Permanent,
                        ));
                    }
                    Err(failure) => {
                        let retryable = matches!(
                            failure,
                            ProviderEffectFailure::Retryable
                                | ProviderEffectFailure::OutcomeUnknown
                        );
                        self.fail_stream_provider_effect(writer, lease, &identity, retryable)
                            .await
                            .map_err(ProviderEffectDispatchError::Runtime)?;
                        if retryable && attempt < MAX_STREAM_PROVIDER_EFFECT_ATTEMPTS {
                            continue;
                        }
                        return Err(ProviderEffectDispatchError::Provider(failure));
                    }
                }
            }
        }
        Ok(())
    }

    async fn prepare_stream_provider_effect_attempt(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        identity: &[u8],
        request_digest: [u8; 32],
        effect_key: ProviderEffectKey,
    ) -> Result<ProviderEffectPreparation, RuntimeError> {
        if lease.purpose != LeasePurpose::Deliver {
            return Ok(ProviderEffectPreparation::Rejected(
                ProviderEffectFailure::Permanent,
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let stream_key = stream_key_id(&lease.delivery.checkpoint_key());
        let mut leases = tx
            .query(
                "SELECT delivery_position, successor_position, fence, purpose \
                 FROM stream_lease WHERE key_id = ?1",
                params![stream_key],
            )
            .await?;
        let Some(active_lease) = leases.next().await? else {
            return Err(RuntimeError::RequestStateConflict);
        };
        let position: String = active_lease
            .get(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let successor: String = active_lease
            .get(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let fence: i64 = active_lease
            .get(2)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        let purpose: i64 = active_lease
            .get(3)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        if position != lease.delivery.position.token.as_str()
            || successor != lease.delivery.successor.token.as_str()
            || fence != i64::try_from(lease.fence).map_err(|_| RuntimeError::RecoveryInvalid)?
            || purpose != 1
        {
            return Err(RuntimeError::RequestStateConflict);
        }

        let mut rows = tx
            .query(
                "SELECT request_digest, idempotency_key, lease_fence, \
                        attempts_in_fence, total_attempts, status, receipt \
                 FROM stream_provider_effect_receipt WHERE effect_identity = ?1",
                params![identity],
            )
            .await?;
        let current_fence =
            i64::try_from(lease.fence).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let effect_key = effect_key.as_bytes().to_vec();
        let preparation = if let Some(row) = rows.next().await? {
            let stored_digest: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let stored_key: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let stored_fence: i64 = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let attempts: i64 = row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let total_attempts: i64 = row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let status: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let receipt: Option<Vec<u8>> = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if stored_digest != request_digest || stored_key != effect_key {
                ProviderEffectPreparation::Rejected(ProviderEffectFailure::Permanent)
            } else if status == 2 {
                if receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.len() > MAX_TERMINAL_OUTCOME_BYTES)
                {
                    return Err(RuntimeError::RecoveryInvalid);
                }
                tx.commit().await?;
                return Ok(ProviderEffectPreparation::Cached);
            } else if receipt.is_some() || !(1..=4).contains(&status) {
                return Err(RuntimeError::RecoveryInvalid);
            } else if stored_fence == current_fence && status == 1 {
                ProviderEffectPreparation::Rejected(ProviderEffectFailure::OutcomeUnknown)
            } else if stored_fence == current_fence
                && (attempts >= i64::from(MAX_STREAM_PROVIDER_EFFECT_ATTEMPTS) || status == 4)
            {
                ProviderEffectPreparation::Rejected(if status == 4 {
                    ProviderEffectFailure::Permanent
                } else {
                    ProviderEffectFailure::Retryable
                })
            } else {
                let attempts_in_fence = if stored_fence == current_fence {
                    attempts + 1
                } else {
                    1
                };
                let total_attempts = total_attempts
                    .checked_add(1)
                    .ok_or(RuntimeError::RecoveryInvalid)?;
                tx.execute(
                    "UPDATE stream_provider_effect_receipt \
                     SET lease_fence = ?1, attempts_in_fence = ?2, \
                         total_attempts = ?3, status = 1, receipt = NULL \
                     WHERE effect_identity = ?4",
                    params![current_fence, attempts_in_fence, total_attempts, identity],
                )
                .await?;
                ProviderEffectPreparation::Started {
                    attempt: u32::try_from(attempts_in_fence)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                }
            }
        } else {
            tx.execute(
                "INSERT INTO stream_provider_effect_receipt \
                 (effect_identity, request_digest, idempotency_key, lease_fence, \
                  attempts_in_fence, total_attempts, status, receipt) \
                 VALUES (?1, ?2, ?3, ?4, 1, 1, 1, NULL)",
                params![identity, request_digest.to_vec(), effect_key, current_fence],
            )
            .await?;
            ProviderEffectPreparation::Started { attempt: 1 }
        };
        tx.commit().await?;
        Ok(preparation)
    }

    async fn complete_stream_provider_effect(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        identity: &[u8],
        request_digest: [u8; 32],
        effect_key: ProviderEffectKey,
        receipt: Vec<u8>,
    ) -> Result<(), RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let changed = tx
            .execute(
                "UPDATE stream_provider_effect_receipt \
                 SET status = 2, receipt = ?1 \
                 WHERE effect_identity = ?2 AND request_digest = ?3 \
                   AND idempotency_key = ?4 AND lease_fence = ?5 AND status = 1",
                params![
                    receipt,
                    identity,
                    request_digest.to_vec(),
                    effect_key.as_bytes().to_vec(),
                    i64::try_from(lease.fence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit().await.map_err(RuntimeError::from)
    }

    async fn fail_stream_provider_effect(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        identity: &[u8],
        retryable: bool,
    ) -> Result<(), RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let changed = tx
            .execute(
                "UPDATE stream_provider_effect_receipt \
                 SET status = ?1, receipt = NULL \
                 WHERE effect_identity = ?2 AND lease_fence = ?3 AND status = 1",
                params![
                    if retryable { 3 } else { 4 },
                    identity,
                    i64::try_from(lease.fence).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit().await.map_err(RuntimeError::from)
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

    /// Runs one provider poll and, when it yields an item, that item's
    /// delivery. Each phase either ends the step early (`Break`) or hands the
    /// next phase what it needs (`Continue`).
    pub(crate) async fn run_stream_once_controlled<S, H, C>(
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
        let polled = match self.poll_stream_item(writer, key, source, control).await? {
            ControlFlow::Continue(polled) => polled,
            ControlFlow::Break(step) => return Ok(step),
        };
        let delivery = match self
            .admit_stream_delivery(writer, key, source, control, polled)
            .await?
        {
            ControlFlow::Continue(delivery) => delivery,
            ControlFlow::Break(step) => return Ok(step),
        };
        self.deliver_stream_item(writer, handler, control, delivery)
            .await
    }

    /// Polls the provider once at the stream's current checkpoint. The step
    /// ends early when the stream is paused, the poll yields no item, or the
    /// owner cancelled; otherwise the polled item is handed on.
    async fn poll_stream_item<S, C>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        source: &mut S,
        control: &C,
    ) -> Result<ControlFlow<StreamStep, PolledStreamItem>, StreamStepError>
    where
        S: StreamSource,
        C: StreamRunControl,
    {
        if source.checkpoint_key() != *key {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch,
            ));
        }
        if !source.descriptor().replayable {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamSourceNotReplayable,
            ));
        }
        self.require_owner(&self.connection, writer)
            .await
            .map_err(StreamStepError::Runtime)?;
        ensure_stream_position_format_compatible(&self.connection, key)
            .await
            .map_err(StreamStepError::Runtime)?;
        let checkpoint = self
            .stream_backend(writer)
            .checkpoint_async(key)
            .await
            .map_err(StreamStepError::Runtime)?;
        if load_stream_status(&self.connection, key)
            .await
            .map_err(StreamStepError::Runtime)?
            == StreamStatus::Paused
        {
            return Ok(ControlFlow::Break(StreamStep::Paused { checkpoint }));
        }
        self.require_owner(&self.connection, writer)
            .await
            .map_err(StreamStepError::Runtime)?;
        let next = source.next(&checkpoint);
        let poll = match tokio::select! {
            _ = control.cancellation_wait() => {
                return self.break_stream_cancelled(writer, key, checkpoint).await;
            }
            result = next => result,
        } {
            Ok(poll) => poll,
            Err(diagnostic) => {
                return match self
                    .settle_provider_failure(writer, key, &checkpoint, control, diagnostic)
                    .await?
                {
                    ProviderFailureOutcome::Cancelled => {
                        Ok(ControlFlow::Break(StreamStep::Cancelled { checkpoint }))
                    }
                    ProviderFailureOutcome::Retained => Err(StreamStepError::Provider(diagnostic)),
                };
            }
        };
        let item = match poll {
            StreamSourcePoll::Item(item) => item,
            // A provider poll is outside the activation transaction and may
            // finish after the owner requests cancellation. Re-check at that
            // boundary before reporting a finite source as exhausted (or an
            // unbounded one as closed): cancellation owns the terminal run
            // classification and must not be turned into source completion.
            StreamSourcePoll::Waiting | StreamSourcePoll::Exhausted if control.cancelled() => {
                return self.break_stream_cancelled(writer, key, checkpoint).await;
            }
            StreamSourcePoll::Waiting => return Ok(ControlFlow::Break(StreamStep::Waiting)),
            StreamSourcePoll::Exhausted => return Ok(ControlFlow::Break(StreamStep::Exhausted)),
        };
        if item.delivery.checkpoint_key() != *key {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamIdentityMismatch,
            ));
        }
        if control.cancelled() {
            return self.break_stream_cancelled(writer, key, checkpoint).await;
        }
        Ok(ControlFlow::Continue(PolledStreamItem { checkpoint, item }))
    }

    /// Admits one polled item as a delivery: takes the admission point,
    /// acquires the delivery lease under the polled checkpoint, and asks the
    /// source for the payload retained should the delivery fail. The step
    /// ends early when cancellation wins admission or the acquire is refused.
    async fn admit_stream_delivery<S, C>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        source: &S,
        control: &C,
        polled: PolledStreamItem,
    ) -> Result<ControlFlow<StreamStep, StreamDelivery>, StreamStepError>
    where
        S: StreamSource,
        C: StreamRunControl,
    {
        let PolledStreamItem { checkpoint, item } = polled;
        let expected = CheckpointPrecondition::from(&checkpoint);
        if !control.acquire_admission() {
            return self.break_stream_cancelled(writer, key, checkpoint).await;
        }
        let _permit = AdmissionPermit(control);
        let mut stream = self.stream_backend(writer);
        let lease = match stream
            .apply_async(CommitIntent::Acquire {
                delivery: item.delivery.clone(),
                expected: expected.clone(),
                purpose: LeasePurpose::Deliver,
            })
            .await
            .map_err(StreamStepError::Runtime)?
        {
            CommitResult::Acquired { lease } => lease,
            CommitResult::Rejected(RejectReason::StreamPaused) => {
                return Ok(ControlFlow::Break(StreamStep::Paused { checkpoint }));
            }
            CommitResult::Rejected(reason) => {
                return Ok(ControlFlow::Break(StreamStep::Rejected(reason)));
            }
            _ => return Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
        };
        // Delivery admission is the inter-item cancellation boundary. Once
        // admitted, start this activation and let the bounded execution check
        // in `deliver_stream_item` roll it back if cancellation interrupts its
        // open work.
        let failure_payload = source.failure_payload(&item);
        if matches!(&failure_payload, StreamFailurePayload::Unavailable) {
            self.release_stream_lease_best_effort(writer, lease).await;
            return Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid));
        }
        Ok(ControlFlow::Continue(StreamDelivery {
            item,
            expected,
            lease,
            failure_payload,
        }))
    }

    /// Runs the handler for one admitted delivery and closes the delivery
    /// lease with the commit, failure or cancellation its result selects.
    async fn deliver_stream_item<H, C>(
        &self,
        writer: WriterLease,
        handler: &mut H,
        control: &C,
        delivery: StreamDelivery,
    ) -> Result<StreamStep, StreamStepError>
    where
        H: StreamHandler,
        C: StreamRunControl,
    {
        let StreamDelivery {
            item,
            expected,
            lease,
            failure_payload,
        } = delivery;
        // DELIVERY-1 admits each item as one activation. Capture that
        // activation's CWD generation before user code runs, then use the
        // same fence for either delivery commit path. A concurrent activation
        // that advances the generation while the handler executes must make
        // this delivery fail closed rather than rebasing its writes or
        // checkpoint movement onto the newer CWD.
        let capture = match self.capture().await {
            Ok(capture) => capture,
            Err(error) => {
                self.release_stream_lease_best_effort(writer, lease).await;
                return Err(StreamStepError::Runtime(error));
            }
        };

        let (mut handler_result, handler_panicked) =
            match catch_unwind(AssertUnwindSafe(|| handler.handle(&item))) {
                Ok(result) => (result, false),
                Err(_) => (
                    StreamHandlerResult::Fail(SafeDiagnostic {
                        code: DiagnosticCode::Internal,
                        class: DiagnosticClass::Permanent,
                    }),
                    true,
                ),
            };
        if !handler_panicked && control.cancelled() {
            // A cancelled delivery never reaches the provider, so the effects
            // the handler staged are dropped unsent.
            drop(handler.take_provider_effects());
            return self.cancel_delivery(writer, &lease).await;
        }

        let effects = if handler_panicked {
            Vec::new()
        } else {
            handler.take_provider_effects()
        };
        if matches!(
            &handler_result,
            StreamHandlerResult::Commit(_) | StreamHandlerResult::CommitValidatedTable(_)
        ) && !effects.is_empty()
        {
            match self
                .dispatch_stream_provider_effects(writer, &lease, handler, &effects)
                .await
            {
                Ok(()) => {}
                Err(ProviderEffectDispatchError::Runtime(error)) => {
                    self.release_stream_lease_best_effort(writer, lease).await;
                    return Err(StreamStepError::Runtime(error));
                }
                Err(ProviderEffectDispatchError::Provider(failure)) => {
                    let class = match failure {
                        ProviderEffectFailure::Permanent | ProviderEffectFailure::Unsupported => {
                            DiagnosticClass::Permanent
                        }
                        ProviderEffectFailure::Retryable
                        | ProviderEffectFailure::OutcomeUnknown => DiagnosticClass::Transient,
                    };
                    handler_result = StreamHandlerResult::Fail(SafeDiagnostic {
                        code: DiagnosticCode::ProviderUnavailable,
                        class,
                    });
                }
            }
        }

        match handler_result {
            StreamHandlerResult::Commit(batch) => {
                let result = self
                    .commit_stream_delivery(StreamDeliveryCommit {
                        writer,
                        expected_capture: &capture,
                        mutations: &batch.mutations,
                        next_digest: batch.next_digest,
                        delivery: lease.clone(),
                        expected_stream: expected,
                        faults: &NoFault,
                    })
                    .await
                    .map(|(_, result)| result);
                self.settle_delivery(writer, &lease, result, committed_step)
                    .await
            }
            // A typed table result without a validator cannot cross the
            // durable boundary. Callers must use CommitValidatedTable so the
            // candidate is checked in the same transaction as its writes.
            StreamHandlerResult::CommitTable(_) => {
                // Treat the missing validator as a handler failure so the
                // acquired delivery is durably released and retryable. No
                // table mutation, checkpoint, or capture is committed.
                self.fail_delivery(
                    writer,
                    &lease,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                    failure_payload,
                )
                .await
            }
            StreamHandlerResult::CommitValidatedTable(mut batch) => {
                let committed = self
                    .commit_stream_validated_table_delivery(StreamValidatedTableDeliveryCommit {
                        writer,
                        expected_capture: &capture,
                        mutations: &batch.mutations,
                        next_digest: batch.next_digest,
                        delivery: lease.clone(),
                        expected_stream: expected,
                        validator: batch.validator.as_mut(),
                        faults: &NoFault,
                    })
                    .await;
                match committed {
                    Ok((_, result)) => {
                        self.settle_delivery(writer, &lease, Ok(result), committed_step)
                            .await
                    }
                    Err(StreamTableDeliveryError::Runtime(error)) => {
                        self.settle_delivery(writer, &lease, Err(error), committed_step)
                            .await
                    }
                    Err(StreamTableDeliveryError::ValidationFailed(diagnostic)) => {
                        self.reject_delivery(writer, &lease, diagnostic, failure_payload)
                            .await
                    }
                }
            }
            // A handler may have staged table inserts before returning an
            // ordinary error. Those inserts are activation-local and must be
            // dropped before the separate durable failure-record transaction.
            // Keep this path identical to `Fail`: the delivery remains at its
            // current checkpoint and can be retried under the same identity.
            StreamHandlerResult::FailAfterTable(_, diagnostic)
            | StreamHandlerResult::Fail(diagnostic) => {
                self.reject_delivery(writer, &lease, diagnostic, failure_payload)
                    .await
            }
            StreamHandlerResult::Cancelled => self.cancel_delivery(writer, &lease).await,
        }
    }

    /// Closes a delivery attempt whose closing commit returned `result`.
    /// `step_for` turns the one result that commit must produce into its
    /// step. A rejection, any other result, or a commit error leaves the
    /// delivery lease held, so it is released before the outcome is reported
    /// and the delivery stays retryable.
    async fn settle_delivery(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        result: Result<CommitResult, RuntimeError>,
        step_for: fn(CommitResult) -> Option<StreamStep>,
    ) -> Result<StreamStep, StreamStepError> {
        let unsettled = match result {
            Ok(CommitResult::Rejected(reason)) => Ok(StreamStep::Rejected(reason)),
            Ok(result) => match step_for(result) {
                Some(step) => return Ok(step),
                None => Err(StreamStepError::Runtime(RuntimeError::RecoveryInvalid)),
            },
            Err(error) => Err(StreamStepError::Runtime(error)),
        };
        self.release_stream_lease_best_effort(writer, lease.clone())
            .await;
        unsettled
    }

    /// Closes the delivery as cancelled at its checkpoint.
    async fn cancel_delivery(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
    ) -> Result<StreamStep, StreamStepError> {
        let result = self
            .stream_backend(writer)
            .apply_async(CommitIntent::Cancel {
                lease: lease.clone(),
            })
            .await;
        self.settle_delivery(writer, lease, result, cancelled_step)
            .await
    }

    /// Closes the delivery by recording `diagnostic` as its retained failure.
    async fn fail_delivery(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        diagnostic: SafeDiagnostic,
        failure_payload: StreamFailurePayload,
    ) -> Result<StreamStep, StreamStepError> {
        if let Err(error) = self.require_owner(&self.connection, writer).await {
            self.release_stream_lease_best_effort(writer, lease.clone())
                .await;
            return Err(StreamStepError::Runtime(error));
        }
        let result = self
            .stream_backend(writer)
            .fail_with_payload_async(lease.clone(), diagnostic, failure_payload)
            .await;
        self.settle_delivery(writer, lease, result, failed_step)
            .await
    }

    /// Closes the delivery for a handler or validator that reported
    /// `diagnostic`: a cancellation diagnostic cancels the delivery, any
    /// other diagnostic becomes its retained failure.
    async fn reject_delivery(
        &self,
        writer: WriterLease,
        lease: &DeliveryLease,
        diagnostic: SafeDiagnostic,
        failure_payload: StreamFailurePayload,
    ) -> Result<StreamStep, StreamStepError> {
        if is_cancellation_diagnostic(diagnostic) {
            return self.cancel_delivery(writer, lease).await;
        }
        self.fail_delivery(writer, lease, diagnostic, failure_payload)
            .await
    }

    /// Records the stream's run as cancelled and ends the step at
    /// `checkpoint`.
    async fn break_stream_cancelled<T>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        checkpoint: StreamCheckpoint,
    ) -> Result<ControlFlow<StreamStep, T>, StreamStepError> {
        self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
            .await
            .map_err(StreamStepError::Runtime)?;
        Ok(ControlFlow::Break(StreamStep::Cancelled { checkpoint }))
    }

    /// Records the stream's run as cancelled after `delivered` deliveries.
    async fn cancel_run(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        delivered: usize,
        checkpoint: StreamCheckpoint,
    ) -> Result<StreamRunOutcome, StreamStepError> {
        self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
            .await
            .map_err(StreamStepError::Runtime)?;
        Ok(StreamRunOutcome::Cancelled {
            delivered,
            checkpoint,
        })
    }

    /// Classifies a provider poll or wait failure at `checkpoint`. A failure
    /// raised by cancellation, or one that races a cancellation request, ends
    /// the run as cancelled and retains nothing. Any other failure is
    /// retained for the owner, unless cancellation arrives while it is being
    /// retained, in which case the retained record is withdrawn again.
    async fn settle_provider_failure<C: StreamRunControl>(
        &self,
        writer: WriterLease,
        key: &CheckpointKey,
        checkpoint: &StreamCheckpoint,
        control: &C,
        diagnostic: SafeDiagnostic,
    ) -> Result<ProviderFailureOutcome, StreamStepError> {
        if control.cancelled() || is_cancellation_diagnostic(diagnostic) {
            self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
                .await
                .map_err(StreamStepError::Runtime)?;
            return Ok(ProviderFailureOutcome::Cancelled);
        }
        self.record_stream_provider_failure(writer, checkpoint, diagnostic)
            .await
            .map_err(StreamStepError::Runtime)?;
        if control.cancelled() {
            self.clear_stream_provider_failure(writer, key)
                .await
                .map_err(StreamStepError::Runtime)?;
            self.complete_stream_observation(writer, key, StreamObservationStatus::Cancelled)
                .await
                .map_err(StreamStepError::Runtime)?;
            return Ok(ProviderFailureOutcome::Cancelled);
        }
        Ok(ProviderFailureOutcome::Retained)
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
        if !source.descriptor().replayable {
            return Err(StreamStepError::Runtime(
                RuntimeError::StreamSourceNotReplayable,
            ));
        }
        ensure_stream_position_format_compatible(&self.connection, key)
            .await
            .map_err(StreamStepError::Runtime)?;
        let mut checkpoint = self
            .stream_backend(writer)
            .checkpoint_async(key)
            .await
            .map_err(StreamStepError::Runtime)?;
        let source_descriptor = source.descriptor();
        let mut delivered = 0;
        loop {
            if control.cancelled() {
                return self.cancel_run(writer, key, delivered, checkpoint).await;
            }
            match self
                .run_stream_once_controlled(writer, key, source, handler, control)
                .await?
            {
                StreamStep::Paused { checkpoint } => {
                    self.complete_stream_observation(writer, key, StreamObservationStatus::Paused)
                        .await
                        .map_err(StreamStepError::Runtime)?;
                    return Ok(StreamRunOutcome::Paused {
                        delivered,
                        checkpoint,
                    });
                }
                StreamStep::Waiting => {
                    if control.cancelled() {
                        return self.cancel_run(writer, key, delivered, checkpoint).await;
                    }
                    let wait = source.wait(control as &dyn StreamRunControl);
                    let wait_result = tokio::select! {
                        _ = control.cancellation_wait() => Err(SafeDiagnostic {
                            code: DiagnosticCode::Cancelled,
                            class: DiagnosticClass::Cancellation,
                        }),
                        result = wait => result,
                    };
                    if let Err(diagnostic) = wait_result {
                        return match self
                            .settle_provider_failure(writer, key, &checkpoint, control, diagnostic)
                            .await?
                        {
                            ProviderFailureOutcome::Cancelled => Ok(StreamRunOutcome::Cancelled {
                                delivered,
                                checkpoint,
                            }),
                            ProviderFailureOutcome::Retained => {
                                Err(StreamStepError::Provider(diagnostic))
                            }
                        };
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
                        failure,
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
}
