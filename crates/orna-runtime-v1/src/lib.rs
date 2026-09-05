//! Durable private local runtime state for one Orna worktree.
//!
//! The database is intentionally below the Git boundary: callers obtain its
//! location only from [`orna_repository_v1::Repository::runtime_paths`].
//! This crate does not publish, project, compact, or contact a remote.

use std::{fmt, path::Path};

use libsql::{Builder, Connection, TransactionBehavior, params};
use num_bigint::BigInt;
use orna_foundation_v1::{CanonicalSnapshot, CwdCapture, Snapshot};
use orna_repository_v1::Repository;
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
        state.validate_recovery().await?;
        Ok(state)
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
        let changed = self.connection.execute("UPDATE writer_lease SET owner_id = ?1, epoch = epoch + 1 WHERE singleton = 1 AND owner_id = ?2", params![replacement.to_vec(), abandoned.to_vec()]).await.map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::OwnerLost);
        }
        let mut rows = self
            .connection
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
        if mutations.is_empty() {
            return Err(RuntimeError::EmptyMutationBatch);
        }
        for mutation in mutations {
            validate_id(mutation.id)?;
            validate_digest(mutation.digest)?;
        }
        validate_digest(next_digest)?;
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
        let current = capture_tx(&tx).await?;
        if &current != expected {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(current),
            });
        }
        let mut sequence: Option<i64> = None;
        for mutation in mutations {
            tx.execute(
                "INSERT INTO pending_mutation (mutation_id, payload, digest) VALUES (?1, ?2, ?3)",
                params![
                    mutation.id.to_vec(),
                    mutation.payload.clone(),
                    mutation.digest.to_vec()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
            let mut rows = tx
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
        tx.execute(
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
        tx.execute(
            "UPDATE runtime_meta SET generation = ?1, generation_digest = ?2 WHERE singleton = 1",
            params![
                i64::try_from(generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                next_digest.to_vec()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        faults.check(FaultPoint::AfterCapture)?;
        let next = capture_tx(&tx).await?;
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(next)
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
                "SELECT COUNT(*), COALESCE(MAX(generation), 0) FROM checkpoint",
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
        let maximum: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_count = u64::try_from(count).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let checkpoint_maximum =
            u64::try_from(maximum).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if checkpoint_count != generation || checkpoint_maximum != generation {
            return Err(RuntimeError::RecoveryInvalid);
        }
        match (generation, self.latest_checkpoint().await?) {
            (0, None) => {}
            (value, Some(checkpoint))
                if checkpoint.generation == value
                    && checkpoint.digest == capture.generation_digest() => {}
            _ => return Err(RuntimeError::RecoveryInvalid),
        }
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
    use std::{path::Path, process::Command};
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
