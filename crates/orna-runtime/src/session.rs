//! Session deletion: closing a session to new requests and finalising it.

use crate::RuntimeState;
use crate::codec::{fixed, validate_id};
use crate::error::RuntimeError;
use crate::lease::{WriterLease, validate_writer_lease};
use crate::request::RequestState;
use libsql::{Connection, TransactionBehavior, params};

/// Where one session's deletion stands. The marker is written `Closing` by
/// `begin_session_deletion`, which refuses new reservations for the session,
/// and becomes `Closed` once no request of the session is Reserved or
/// Running. It is retained in `Closed` so the session stays closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionDeletionState {
    Closing,
    Closed,
}

impl SessionDeletionState {
    const fn code(self) -> i64 {
        match self {
            Self::Closing => 1,
            Self::Closed => 2,
        }
    }

    fn from_code(code: i64) -> Result<Self, RuntimeError> {
        match code {
            1 => Ok(Self::Closing),
            2 => Ok(Self::Closed),
            _ => Err(RuntimeError::RecoveryInvalid),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionDeletionRecord {
    owner: WriterLease,
    state: SessionDeletionState,
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
    let state =
        SessionDeletionState::from_code(row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
    Ok(Some(SessionDeletionRecord {
        owner: WriterLease {
            owner_id,
            epoch: owner_epoch,
        },
        state,
    }))
}

pub(crate) async fn ensure_session_admission_open(
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

impl RuntimeState {
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
                            SessionDeletionState::Closing.code(),
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
        if record.state == SessionDeletionState::Closed {
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
                    SessionDeletionState::Closed.code(),
                    session_id.to_vec(),
                    owner.owner_id.to_vec(),
                    i64::try_from(owner.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    SessionDeletionState::Closing.code(),
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

    pub(crate) async fn validate_session_deletions(&self) -> Result<(), RuntimeError> {
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
            SessionDeletionState::from_code(
                row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
        }
        Ok(())
    }
}
