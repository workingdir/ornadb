//! The REQUEST-1 ledger: reservation, admission capability, ownership and terminal transitions.

use crate::codec::{bigint_to_i64, capture_tx, fixed, validate_id};
use crate::error::RuntimeError;
use crate::lease::{WriterLease, current_writer_lease_tx, validate_writer_lease};
use crate::observation::{
    RunObservationStatus, sync_run_request_state_tx, sync_run_request_state_with_diagnostic_tx,
};
use crate::session::ensure_session_admission_open;
use crate::{
    RuntimeState, decode_request_execution_evidence, request_execution_evidence_tx,
    validate_request_execution_evidence,
};
use libsql::{Connection, TransactionBehavior, params};
use orna_stream::SafeDiagnostic;
use sha2::{Digest, Sha256};
use std::fmt;
use uuid::Uuid;

pub const MAX_TERMINAL_OUTCOME_BYTES: usize = 16 * 1024 * 1024;

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
    pub(crate) const fn code(self) -> i64 {
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
    pub(crate) const fn code(self) -> i64 {
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

/// An opaque, single-use bearer capability for starting one fresh request
/// reservation. The raw token is returned only to the caller that inserted
/// the Reserved row; durable state retains only its digest and bindings.
///
/// The capability is intentionally not serializable or constructible by
/// callers. It is invalid after one successful owner-start, or whenever its
/// request, fingerprint, owner/epoch, or admission generation differs.
#[derive(Clone, Eq, PartialEq)]
pub struct RequestAdmissionCapability([u8; 32]);

impl fmt::Debug for RequestAdmissionCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestAdmissionCapability(REDACTED)")
    }
}

impl RequestAdmissionCapability {
    fn new() -> Self {
        let mut token = [0_u8; 32];
        token[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        token[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self(token)
    }
}

struct RequestTerminalTransition {
    next: RequestState,
    terminal_outcome: TerminalOutcome,
    observation_status: RunObservationStatus,
    diagnostic: Option<SafeDiagnostic>,
}

pub(crate) async fn request_status_tx(
    connection: &Connection,
    identity: RequestIdentity,
) -> Result<Option<RequestStatus>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT session_id, request_id, fingerprint, state, terminal_outcome FROM request_ledger WHERE session_id = ?1 AND request_id = ?2",
            params![identity.session_id.to_vec(), identity.request_id.to_vec()],
        )
        .await
        ?;
    rows.next()
        .await?
        .map(|row| decode_request_status(&row))
        .transpose()
}

pub(crate) fn decode_request_status(row: &libsql::Row) -> Result<RequestStatus, RuntimeError> {
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

pub(crate) fn require_fingerprint(
    status: &RequestStatus,
    fingerprint: [u8; 32],
) -> Result<(), RuntimeError> {
    if status.fingerprint == fingerprint {
        Ok(())
    } else {
        Err(RuntimeError::RequestFingerprintMismatch)
    }
}

async fn request_admission_exists_tx(
    connection: &Connection,
    identity: RequestIdentity,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM request_admission
             WHERE session_id = ?1 AND request_id = ?2",
            params![identity.session_id.to_vec(), identity.request_id.to_vec()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

pub(crate) fn validate_request_identity(value: RequestIdentity) -> Result<(), RuntimeError> {
    validate_id(value.session_id)?;
    validate_id(value.request_id)
}

pub(crate) fn admission_capability_digest(capability: &RequestAdmissionCapability) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.runtime.request-admission.v1");
    digest.update(capability.0);
    digest.finalize().into()
}

impl RuntimeState {
    /// Atomically reserves a REQUEST-1 identity. Repeating the same identity
    /// and fingerprint returns the durable record, including terminal replay.
    pub async fn reserve_request(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<RequestStatus, RuntimeError> {
        self.reserve_request_inner(identity, fingerprint, false)
            .await
            .map(|(status, _)| status)
    }

    /// Atomically reserves a REQUEST-1 identity and reports whether this call
    /// inserted the reservation. A newly inserted row also returns an opaque
    /// bearer capability when a current writer owner is available; an
    /// existing row never reissues its capability.
    pub async fn reserve_request_with_admission(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
    ) -> Result<(RequestStatus, Option<RequestAdmissionCapability>), RuntimeError> {
        self.reserve_request_inner(identity, fingerprint, true)
            .await
    }

    async fn reserve_request_inner(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        issue_capability: bool,
    ) -> Result<(RequestStatus, Option<RequestAdmissionCapability>), RuntimeError> {
        validate_request_identity(identity)?;
        // ORNA-TURSO-005: a handle that does not own the worktree lock must
        // coordinate with the owner or fail. A reservation is a durable write,
        // so a secondary handle is refused before it can insert a row.
        self.require_lease_authority()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_recovery_barrier_clear(&tx).await?;
        ensure_session_admission_open(&tx, identity.session_id).await?;
        if let Some(status) = request_status_tx(&tx, identity).await? {
            require_fingerprint(&status, fingerprint)?;
            tx.commit().await?;
            return Ok((status, None));
        }
        let admission = if issue_capability {
            current_writer_lease_tx(&tx).await?
        } else {
            None
        };
        let (capability, owner, generation) = if let Some(owner) = admission {
            let capture = capture_tx(&tx).await?;
            let capability = RequestAdmissionCapability::new();
            (
                Some(capability.clone()),
                Some(owner),
                Some(bigint_to_i64(capture.generation())?),
            )
        } else {
            (None, None, None)
        };
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
        ?;
        if let (Some(capability), Some(owner), Some(generation)) = (&capability, owner, generation)
        {
            tx.execute(
                "INSERT INTO request_admission
                 (session_id, request_id, fingerprint, capability_digest,
                  owner_id, owner_epoch, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
                    fingerprint.to_vec(),
                    admission_capability_digest(capability).to_vec(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    generation,
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok((
            RequestStatus {
                identity,
                fingerprint,
                state: RequestState::Reserved,
                terminal_outcome: None,
            },
            capability,
        ))
    }

    /// Starts a legacy request without durable ownership.
    ///
    /// This compatibility path is only valid before a writer lease exists;
    /// live writers must use [`Self::start_request_with_owner_and_admission`].
    /// A legacy Running row that already exists before lease acquisition
    /// remains recoverable through [`Self::recover_legacy_running_request`].
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

    /// Starts a newly admitted request with the exact capability issued by
    /// [`Self::reserve_request_with_admission`]. The capability, request
    /// identity, fingerprint, writer lease, and current runtime generation
    /// are checked and consumed in the same immediate transaction.
    pub async fn start_request_with_owner_and_admission(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        capability: RequestAdmissionCapability,
    ) -> Result<RequestStatus, RuntimeError> {
        validate_request_identity(identity)?;
        validate_writer_lease(owner)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_owner(&tx, owner).await?;
        let current = request_status_tx(&tx, identity)
            .await?
            .ok_or(RuntimeError::RequestUnknown)?;
        require_fingerprint(&current, fingerprint)?;
        if current.state != RequestState::Reserved {
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
                params![identity.session_id.to_vec(), identity.request_id.to_vec()],
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
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
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
                    identity.session_id.to_vec(),
                    identity.request_id.to_vec(),
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
        sync_run_request_state_tx(&tx, identity, RunObservationStatus::Running).await?;
        tx.commit().await?;
        Ok(RequestStatus {
            identity,
            fingerprint,
            state: RequestState::Running,
            terminal_outcome: None,
        })
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
        self.transition_request_with_owner_and_run_status(
            identity,
            fingerprint,
            owner,
            RequestTerminalTransition {
                next: RequestState::Completed,
                terminal_outcome: outcome,
                observation_status: RunObservationStatus::Completed,
                diagnostic: None,
            },
            true,
        )
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
            RequestTerminalTransition {
                next: RequestState::Completed,
                terminal_outcome: outcome,
                observation_status: RunObservationStatus::Failed,
                diagnostic: Some(diagnostic),
            },
            true,
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
        self.transition_request_with_owner_and_run_status(
            identity,
            fingerprint,
            owner,
            RequestTerminalTransition {
                next: RequestState::Cancelled,
                terminal_outcome: outcome,
                observation_status: RunObservationStatus::Cancelled,
                diagnostic: None,
            },
            true,
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
            .await?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next().await? {
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
            .await?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next().await? {
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
        self.require_lease_authority()?;
        validate_request_identity(identity)?;
        debug_assert_eq!(next.is_terminal(), terminal_outcome.is_some());
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        self.require_recovery_barrier_clear(&tx).await?;
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
                    .await?
                    .next()
                    .await?
                    .is_some()
            {
                return Err(RuntimeError::RequestOwnerConflict);
            }
        }
        if !allowed.contains(&current.state) || current.state.is_terminal() {
            return Err(RuntimeError::RequestStateConflict);
        }
        if next == RequestState::Running && request_admission_exists_tx(&tx, identity).await? {
            return Err(RuntimeError::RequestStateConflict);
        }
        if next == RequestState::Running
            && tx
                .query("SELECT 1 FROM writer_lease WHERE singleton = 1", ())
                .await?
                .next()
                .await?
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
            .await?;
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
        tx.commit().await?;
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
            RequestTerminalTransition {
                next,
                terminal_outcome,
                observation_status,
                diagnostic: None,
            },
            false,
        )
        .await
    }

    async fn transition_request_with_owner_and_run_status(
        &self,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        owner: WriterLease,
        transition: RequestTerminalTransition,
        require_observation: bool,
    ) -> Result<RequestStatus, RuntimeError> {
        let RequestTerminalTransition {
            next,
            terminal_outcome,
            observation_status,
            diagnostic,
        } = transition;
        validate_request_identity(identity)?;
        validate_id(owner.owner_id)?;
        if owner.epoch == 0 || !next.is_terminal() {
            return Err(RuntimeError::RequestStateConflict);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
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
            .await?
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
            .await?
        };
        if changed != 1 {
            return Err(RuntimeError::RequestOwnerConflict);
        }
        let observed = sync_run_request_state_with_diagnostic_tx(
            &tx,
            identity,
            observation_status,
            diagnostic,
        )
        .await?;
        if require_observation && observed != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        tx.commit().await?;
        Ok(RequestStatus {
            identity,
            fingerprint,
            state: next,
            terminal_outcome: Some(terminal_outcome),
        })
    }

    pub(crate) async fn validate_request_admissions(
        &self,
        current_generation: u64,
    ) -> Result<(), RuntimeError> {
        let mut rows = self
            .connection
            .query(
                "SELECT session_id, request_id, fingerprint, capability_digest,
                        owner_id, owner_epoch, generation
                 FROM request_admission",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let identity = RequestIdentity {
                session_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
                request_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            };
            let fingerprint = fixed(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let _capability_digest: [u8; 32] =
                fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let owner_id = fixed(row.get(4).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            let owner_epoch: i64 = row.get(5).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let generation: i64 = row.get(6).map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_id(identity.session_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_id(identity.request_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            validate_id(owner_id).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if owner_epoch <= 0
                || u64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?
                    > current_generation
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
            let Some(status) = request_status_tx(&self.connection, identity).await? else {
                return Err(RuntimeError::RecoveryInvalid);
            };
            if status.state != RequestState::Reserved
                || status.fingerprint != fingerprint
                || status.terminal_outcome.is_some()
            {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        Ok(())
    }
}
