//! Stream delivery and replay commits, and the replay runner.

use crate::RuntimeState;
use crate::checkpoint_snapshot::StreamCheckpointTransition;
use crate::codec::{capture_tx, validate_id};
use crate::error::{FaultInjector, FaultPoint, NoFault, RuntimeError, StreamTableDeliveryError};
use crate::lease::WriterLease;
use crate::observation::sync_stream_observation_tx;
use crate::stream_intent::apply_stream_intent_tx;
use crate::stream_run::StreamStepError;
use crate::stream_source::{
    NoStreamFailurePayloadProvider, StreamFailurePayloadProvider, StreamHandler,
    StreamHandlerResult, StreamItem, StreamTableCandidateValidator,
};
use crate::stream_store::{
    StoredFailurePayload, encode_status, load_stored_stream_failure_payload, load_stream_failure,
    load_stream_replay_claim, record_stream_checkpoint_watermark_tx, stream_identity_id,
};
use crate::table_store::{
    CommittedTableBase, Mutation, TableMutation, append_mutations_tx, apply_table_mutation_tx,
    mutated_tables, table_rows_tx, validate_stream_mutations, validate_table_candidate_scope,
};
use libsql::{TransactionBehavior, params};
use orna_stream::{
    CheckpointPrecondition, CommitIntent, CommitResult, DeliveryLease, DiagnosticClass,
    DiagnosticCode, FailureStatus, RejectReason, ReplayGrant, SafeDiagnostic,
};
use orna_value::CwdCapture;
use sha2::{Digest, Sha256};
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub struct StreamDeliveryCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [Mutation],
    pub next_digest: [u8; 32],
    pub delivery: DeliveryLease,
    pub expected_stream: CheckpointPrecondition,
    pub faults: &'a dyn FaultInjector,
}

/// A stream-table delivery that validates the exact uncommitted table
/// candidate before its writes and checkpoint become visible.
pub struct StreamValidatedTableDeliveryCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [TableMutation],
    pub next_digest: [u8; 32],
    pub delivery: DeliveryLease,
    pub expected_stream: CheckpointPrecondition,
    pub validator: &'a mut dyn StreamTableCandidateValidator,
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

pub struct StreamValidatedTableReplayCommit<'a> {
    pub writer: WriterLease,
    pub expected_capture: &'a CwdCapture,
    pub mutations: &'a [TableMutation],
    pub next_digest: [u8; 32],
    pub grant: ReplayGrant,
    pub validator: &'a mut dyn StreamTableCandidateValidator,
    pub faults: &'a dyn FaultInjector,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoredStreamReplayClaim {
    pub(crate) version: u64,
    pub(crate) owner: WriterLease,
}

impl RuntimeState {
    /// Commits handler mutations and the corresponding stream checkpoint in
    /// one writer-fenced transaction. A rejected stream precondition rolls
    /// back without exposing any mutation or changing the CWD capture.
    ///
    /// Typed table mutations never take this path: they must use
    /// [`RuntimeState::commit_stream_validated_table_delivery`], which checks
    /// the candidate relation before committing rows or the stream
    /// checkpoint.
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
        if mutations.is_empty() && next_digest != expected_capture.generation_digest() {
            return Err(RuntimeError::InvalidDigest);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let current = capture_tx(&tx).await?;
        if &current != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
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
        let next = if mutations.is_empty() {
            capture_tx(&tx).await?
        } else {
            append_mutations_tx(&tx, expected_capture, mutations, next_digest, faults).await?
        };
        if let CommitResult::CheckpointAdvanced { checkpoint } = &result {
            record_stream_checkpoint_watermark_tx(
                &tx,
                &checkpoint.key,
                checkpoint,
                &next,
                StreamCheckpointTransition::Complete,
            )
            .await?;
        }
        tx.commit().await?;
        Ok((next, result))
    }

    /// Commits typed table mutations and a stream checkpoint only after a
    /// caller validates the rows selected by this transaction after applying
    /// those mutations. A rejected candidate leaves table rows, checkpoint,
    /// capture, and delivery completion uncommitted.
    pub async fn commit_stream_validated_table_delivery(
        &self,
        request: StreamValidatedTableDeliveryCommit<'_>,
    ) -> Result<(CwdCapture, CommitResult), StreamTableDeliveryError> {
        let StreamValidatedTableDeliveryCommit {
            writer,
            expected_capture,
            mutations,
            next_digest,
            delivery,
            expected_stream,
            validator,
            faults,
        } = request;
        let encoded = mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()
            .map_err(StreamTableDeliveryError::Runtime)?;
        validate_id(writer.owner_id).map_err(StreamTableDeliveryError::Runtime)?;
        validate_stream_mutations(&encoded, next_digest)
            .map_err(StreamTableDeliveryError::Runtime)?;
        validate_table_candidate_scope(
            mutations,
            validator.tables(),
            &validator.table_object_ids(),
        )
        .map_err(StreamTableDeliveryError::Runtime)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| StreamTableDeliveryError::Runtime(RuntimeError::StorageUnavailable))?;
        self.require_owner(&tx, writer)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        let current = capture_tx(&tx)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        if &current != expected_capture {
            return Err(StreamTableDeliveryError::Runtime(
                RuntimeError::StaleCapture {
                    current: Box::new(current),
                },
            ));
        }
        let result = apply_stream_intent_tx(
            &tx,
            CommitIntent::Complete {
                lease: delivery,
                expected: expected_stream,
            },
        )
        .await
        .map_err(StreamTableDeliveryError::Runtime)?;
        sync_stream_observation_tx(&tx, &result)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx)
                .await
                .map_err(StreamTableDeliveryError::Runtime)?;
            return Ok((current, result));
        }
        let validator_object_ids = validator.table_object_ids();
        let mut base = CommittedTableBase::load_tx(
            &tx,
            validator.committed_rows(),
            validator
                .tables()
                .iter()
                .map(|table| (table.as_str(), validator_object_ids.get(table).copied())),
        )
        .await
        .map_err(StreamTableDeliveryError::Runtime)?;
        for mutation in mutations {
            apply_table_mutation_tx(&tx, mutation, &mut base)
                .await
                .map_err(StreamTableDeliveryError::Runtime)?;
        }
        let next = append_mutations_tx(&tx, expected_capture, &encoded, next_digest, faults)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        let rows = table_rows_tx(&tx, validator.tables(), &validator_object_ids, &base)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        validator
            .validate_changed(&rows, &mutated_tables(mutations))
            .map_err(StreamTableDeliveryError::ValidationFailed)?;
        if let CommitResult::CheckpointAdvanced { checkpoint } = &result {
            record_stream_checkpoint_watermark_tx(
                &tx,
                &checkpoint.key,
                checkpoint,
                &next,
                StreamCheckpointTransition::Complete,
            )
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        }
        tx.commit()
            .await
            .map_err(|_| StreamTableDeliveryError::Runtime(RuntimeError::StorageUnavailable))?;
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
        if mutations.is_empty() && next_digest != expected_capture.generation_digest() {
            return Err(RuntimeError::InvalidDigest);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let current = capture_tx(&tx).await?;
        if &current != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        let Some(replay_claim) = load_stream_replay_claim(&tx, &grant.failure).await? else {
            return Err(RuntimeError::RecoveryInvalid);
        };
        if replay_claim.version != grant.version || replay_claim.owner != writer {
            return Err(RuntimeError::RecoveryInvalid);
        }
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
        let next = if mutations.is_empty() {
            capture_tx(&tx).await?
        } else {
            append_mutations_tx(&tx, expected_capture, mutations, next_digest, faults).await?
        };
        tx.commit().await?;
        Ok((next, result))
    }

    pub(crate) async fn commit_stream_validated_table_replay(
        &self,
        request: StreamValidatedTableReplayCommit<'_>,
    ) -> Result<(CwdCapture, CommitResult), StreamTableDeliveryError> {
        let StreamValidatedTableReplayCommit {
            writer,
            expected_capture,
            mutations,
            next_digest,
            grant,
            validator,
            faults,
        } = request;
        let encoded = mutations
            .iter()
            .map(TableMutation::runtime_mutation)
            .collect::<Result<Vec<_>, _>>()
            .map_err(StreamTableDeliveryError::Runtime)?;
        validate_id(writer.owner_id).map_err(StreamTableDeliveryError::Runtime)?;
        validate_stream_mutations(&encoded, next_digest)
            .map_err(StreamTableDeliveryError::Runtime)?;
        validate_table_candidate_scope(
            mutations,
            validator.tables(),
            &validator.table_object_ids(),
        )
        .map_err(StreamTableDeliveryError::Runtime)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| StreamTableDeliveryError::Runtime(RuntimeError::StorageUnavailable))?;
        self.require_owner(&tx, writer)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        let current = capture_tx(&tx)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        if &current != expected_capture {
            return Err(StreamTableDeliveryError::Runtime(
                RuntimeError::StaleCapture {
                    current: Box::new(current),
                },
            ));
        }
        let Some(replay_claim) = load_stream_replay_claim(&tx, &grant.failure)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?
        else {
            return Err(StreamTableDeliveryError::Runtime(
                RuntimeError::RecoveryInvalid,
            ));
        };
        if replay_claim.version != grant.version || replay_claim.owner != writer {
            return Err(StreamTableDeliveryError::Runtime(
                RuntimeError::RecoveryInvalid,
            ));
        }
        let result = apply_stream_intent_tx(
            &tx,
            CommitIntent::ReplayComplete {
                failure: grant.failure,
                expected_version: grant.version,
            },
        )
        .await
        .map_err(StreamTableDeliveryError::Runtime)?;
        sync_stream_observation_tx(&tx, &result)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        if matches!(result, CommitResult::Rejected(_)) {
            let current = capture_tx(&tx)
                .await
                .map_err(StreamTableDeliveryError::Runtime)?;
            return Ok((current, result));
        }
        let validator_object_ids = validator.table_object_ids();
        let mut base = CommittedTableBase::load_tx(
            &tx,
            validator.committed_rows(),
            validator
                .tables()
                .iter()
                .map(|table| (table.as_str(), validator_object_ids.get(table).copied())),
        )
        .await
        .map_err(StreamTableDeliveryError::Runtime)?;
        for mutation in mutations {
            apply_table_mutation_tx(&tx, mutation, &mut base)
                .await
                .map_err(StreamTableDeliveryError::Runtime)?;
        }
        let next = append_mutations_tx(&tx, expected_capture, &encoded, next_digest, faults)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        let rows = table_rows_tx(&tx, validator.tables(), &validator_object_ids, &base)
            .await
            .map_err(StreamTableDeliveryError::Runtime)?;
        validator
            .validate_changed(&rows, &mutated_tables(mutations))
            .map_err(StreamTableDeliveryError::ValidationFailed)?;
        tx.commit()
            .await
            .map_err(|_| StreamTableDeliveryError::Runtime(RuntimeError::StorageUnavailable))?;
        Ok((next, result))
    }

    async fn fail_stream_replay(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
        diagnostic: SafeDiagnostic,
    ) -> Result<CommitResult, StreamStepError> {
        self.fail_stream_replay_with_faults(writer, grant, diagnostic, &NoFault)
            .await
            .map_err(StreamStepError::Runtime)
    }

    pub(crate) async fn fail_stream_replay_with_faults(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
        diagnostic: SafeDiagnostic,
        faults: &dyn FaultInjector,
    ) -> Result<CommitResult, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
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
        tx.commit().await?;
        Ok(result)
    }

    /// Cancels a replay only after the handler has been admitted under this
    /// exact owner-fenced claim. Public ReplayCancel intentionally cannot
    /// cancel an in-flight handler.
    async fn cancel_admitted_stream_replay(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
    ) -> Result<CommitResult, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let Some(record) = load_stream_failure(&tx, &grant.failure).await? else {
            return Err(RuntimeError::OwnerLost);
        };
        if record.version != grant.version || record.status != FailureStatus::Replaying {
            return Err(RuntimeError::OwnerLost);
        }
        let Some(claim) = load_stream_replay_claim(&tx, &grant.failure).await? else {
            return Err(RuntimeError::OwnerLost);
        };
        if claim.version != grant.version || claim.owner != writer {
            return Err(RuntimeError::OwnerLost);
        }
        let deleted = tx
            .execute(
                "DELETE FROM stream_replay_claim
                 WHERE identity_id = ?1 AND version = ?2 AND owner_id = ?3 AND owner_epoch = ?4",
                params![
                    stream_identity_id(&grant.failure),
                    i64::try_from(grant.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    writer.owner_id.to_vec(),
                    i64::try_from(writer.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await?;
        if deleted != 1 {
            return Err(RuntimeError::OwnerLost);
        }
        let changed = tx
            .execute(
                "UPDATE stream_failure
                 SET version = version + 1, status = ?4
                 WHERE identity_id = ?1 AND version = ?2 AND status = ?3",
                params![
                    stream_identity_id(&grant.failure),
                    i64::try_from(grant.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    encode_status(FailureStatus::Replaying),
                    encode_status(FailureStatus::Skipped),
                ],
            )
            .await?;
        if changed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let result = CommitResult::ReplayCancelled {
            failure: load_stream_failure(&tx, &grant.failure)
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?,
        };
        sync_stream_observation_tx(&tx, &result).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Claims one admitted replay before reading retained data or contacting
    /// its provider. The claim is intentionally separate from the replay
    /// admission transition: an administrator may cancel a grant that has
    /// not started execution, but cannot invalidate work already in flight.
    pub(crate) async fn claim_stream_replay(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
    ) -> Result<Result<(), RejectReason>, RuntimeError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, writer).await?;
        let Some(record) = load_stream_failure(&tx, &grant.failure).await? else {
            return Ok(Err(RejectReason::FailureMissing));
        };
        if record.version != grant.version {
            return Ok(Err(RejectReason::StaleFailure));
        }
        if record.status != FailureStatus::Replaying {
            return Ok(Err(RejectReason::RetryNotAllowed));
        }
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO stream_replay_claim
                 (identity_id, version, owner_id, owner_epoch)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    stream_identity_id(&grant.failure),
                    i64::try_from(grant.version).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    writer.owner_id.to_vec(),
                    i64::try_from(writer.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await?;
        if changed != 1 {
            return Ok(Err(RejectReason::LeaseAlreadyHeld));
        }
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Rechecks the owner and exact durable replay claim after an awaited
    /// provider operation and before user handler code can run.
    async fn verify_stream_replay_claim(
        &self,
        writer: WriterLease,
        grant: &ReplayGrant,
    ) -> Result<(), RuntimeError> {
        self.require_owner(&self.connection, writer).await?;
        let Some(claim) = load_stream_replay_claim(&self.connection, &grant.failure).await? else {
            return Err(RuntimeError::OwnerLost);
        };
        if claim.version != grant.version || claim.owner != writer {
            return Err(RuntimeError::OwnerLost);
        }
        Ok(())
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
        if let Err(reason) = self
            .claim_stream_replay(writer, &grant)
            .await
            .map_err(StreamStepError::Runtime)?
        {
            return Ok(CommitResult::Rejected(reason));
        }
        let payload = match self
            .replay_payload(&grant, provider)
            .await
            .map_err(StreamStepError::Runtime)?
        {
            ControlFlow::Continue(payload) => payload,
            ControlFlow::Break(diagnostic) => {
                return self.fail_stream_replay(writer, &grant, diagnostic).await;
            }
        };
        self.verify_stream_replay_claim(writer, &grant)
            .await
            .map_err(StreamStepError::Runtime)?;
        let expected_capture = self.capture().await.map_err(StreamStepError::Runtime)?;
        let item = StreamItem {
            delivery: grant.failure.0.clone(),
            payload,
        };
        let handler_result = match catch_unwind(AssertUnwindSafe(|| handler.handle(&item))) {
            Ok(result) => result,
            Err(_) => StreamHandlerResult::Fail(SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            }),
        };
        match handler_result {
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
            StreamHandlerResult::CommitTable(_) => {
                self.fail_stream_replay(
                    writer,
                    &grant,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                )
                .await
            }
            StreamHandlerResult::CommitValidatedTable(mut batch) => {
                match self
                    .commit_stream_validated_table_replay(StreamValidatedTableReplayCommit {
                        writer,
                        expected_capture: &expected_capture,
                        mutations: &batch.mutations,
                        next_digest: batch.next_digest,
                        grant: grant.clone(),
                        validator: batch.validator.as_mut(),
                        faults: &NoFault,
                    })
                    .await
                {
                    Ok((_, result)) => Ok(result),
                    Err(StreamTableDeliveryError::ValidationFailed(diagnostic)) => {
                        self.fail_stream_replay(writer, &grant, diagnostic).await
                    }
                    Err(StreamTableDeliveryError::Runtime(error)) => {
                        Err(StreamStepError::Runtime(error))
                    }
                }
            }
            StreamHandlerResult::FailAfterTable(_, diagnostic)
            | StreamHandlerResult::Fail(diagnostic) => {
                self.fail_stream_replay(writer, &grant, diagnostic).await
            }
            StreamHandlerResult::Cancelled => self
                .cancel_admitted_stream_replay(writer, &grant)
                .await
                .map_err(StreamStepError::Runtime),
        }
    }

    /// The payload a replay hands its handler. A payload the runtime cannot
    /// produce fails the replay with the diagnostic returned in `Break`: a
    /// protected reference without a provider, or whose refetch fails, is a
    /// transient provider failure; a digest mismatch or a missing payload
    /// record is a permanent internal one.
    async fn replay_payload<P>(
        &self,
        grant: &ReplayGrant,
        provider: Option<&P>,
    ) -> Result<ControlFlow<SafeDiagnostic, Vec<u8>>, RuntimeError>
    where
        P: StreamFailurePayloadProvider + ?Sized,
    {
        const PROVIDER_UNAVAILABLE: SafeDiagnostic = SafeDiagnostic {
            code: DiagnosticCode::ProviderUnavailable,
            class: DiagnosticClass::Transient,
        };
        const INTERNAL: SafeDiagnostic = SafeDiagnostic {
            code: DiagnosticCode::Internal,
            class: DiagnosticClass::Permanent,
        };
        let stored = load_stored_stream_failure_payload(&self.connection, &grant.failure).await?;
        Ok(match stored {
            Some(StoredFailurePayload::Plaintext(payload)) => ControlFlow::Continue(payload),
            Some(StoredFailurePayload::ProtectedReference { reference, digest }) => {
                let Some(provider) = provider else {
                    return Ok(ControlFlow::Break(PROVIDER_UNAVAILABLE));
                };
                let Ok(payload) = provider.refetch(&reference).await else {
                    return Ok(ControlFlow::Break(PROVIDER_UNAVAILABLE));
                };
                if <[u8; 32]>::from(Sha256::digest(&payload)) == digest {
                    ControlFlow::Continue(payload)
                } else {
                    ControlFlow::Break(INTERNAL)
                }
            }
            None => ControlFlow::Break(INTERNAL),
        })
    }
}
