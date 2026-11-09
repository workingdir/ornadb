//! The worktree's single writer lease: acquisition, takeover, fencing and recovery barrier.

use crate::codec::{fixed, validate_id};
use crate::error::RuntimeError;
use crate::request::RequestState;
use crate::stream_store::{
    encode_status, encode_stream_status, validate_recoverable_stream_attempts,
};
use crate::{RuntimeState, orphan_fenced_running_requests_tx};
use libsql::{Connection, TransactionBehavior, params};
use orna_repository::RuntimeOwnerLock;
use orna_stream::{FailureStatus, StreamStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterLease {
    pub owner_id: [u8; 16],
    pub epoch: u64,
}

/// What a runtime handle may do with its worktree's single writer lease
/// (ORNA-TURSO-004/005).
pub(crate) enum WriterAuthority {
    /// Opened by path without a repository owner lock, which only unit tests
    /// do. The caller is the only writer of that database; a different
    /// recorded owner is never presumed departed.
    #[cfg(test)]
    Unmanaged,
    /// Another live runtime holds the worktree's owner lock. This handle is a
    /// secondary participant: it may neither read, acquire, create nor fence
    /// the writer lease, nor admit requests under it (ORNA-TURSO-005).
    Secondary,
    /// This handle holds the worktree's runtime owner lock under `lock_id`.
    /// Every lease it creates, re-acquires or takes over is bound to that
    /// lock identity.
    Owner {
        lock: RuntimeOwnerLock,
        lock_id: [u8; 16],
    },
}

impl WriterAuthority {
    fn lock_id(&self) -> Option<[u8; 16]> {
        match self {
            Self::Owner { lock_id, .. } => Some(*lock_id),
            Self::Secondary => None,
            #[cfg(test)]
            Self::Unmanaged => None,
        }
    }
}

pub(crate) async fn current_writer_lease_tx(
    connection: &Connection,
) -> Result<Option<WriterLease>, RuntimeError> {
    let mut rows = connection
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
    let epoch = u64::try_from(
        row.get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let lease = WriterLease { owner_id, epoch };
    validate_writer_lease(lease)?;
    Ok(Some(lease))
}

/// The current writer lease and the runtime owner lock identity it is bound
/// to. An unbound lease (null) predates lock binding or was installed by a
/// handle without the lock; no live lock holder vouches for it.
async fn bound_writer_lease_tx(
    connection: &Connection,
) -> Result<Option<(WriterLease, Option<[u8; 16]>)>, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT owner_id, epoch, lock_owner_id FROM writer_lease WHERE singleton = 1",
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
    let epoch = u64::try_from(
        row.get::<i64>(1)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )
    .map_err(|_| RuntimeError::RecoveryInvalid)?;
    let lease = WriterLease { owner_id, epoch };
    validate_writer_lease(lease)?;
    let lock_id = row
        .get::<Option<Vec<u8>>>(2)
        .map_err(|_| RuntimeError::RecoveryInvalid)?
        .map(fixed)
        .transpose()?;
    Ok(Some((lease, lock_id)))
}

/// The takeover fence after an attempt to clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TakeoverBarrier {
    /// No fence is installed, or the matching fence has just been removed.
    Clear,
    /// The matching fence stays because a Running request is not yet terminal.
    Pending,
}

impl TakeoverBarrier {
    fn require_clear(self) -> Result<(), RuntimeError> {
        match self {
            Self::Clear => Ok(()),
            Self::Pending => Err(RuntimeError::RecoveryPending),
        }
    }
}

/// How a writer lease takeover settles the Running requests left by the
/// tokens it fences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TakeoverRecovery {
    /// Explicit takeover: the caller recovers each Running request with its
    /// own outcome payloads, and a durable barrier blocks ordinary writes
    /// until it completes takeover recovery.
    Barrier,
    /// Takeover on lease acquisition: every Running request is fenced into
    /// its conservative orphaned outcome inside the takeover transaction.
    OrphanRunning,
}

pub(crate) fn validate_writer_lease(value: WriterLease) -> Result<(), RuntimeError> {
    validate_id(value.owner_id)?;
    if value.epoch == 0 {
        return Err(RuntimeError::InvalidIdentity);
    }
    Ok(())
}

impl RuntimeState {
    /// Whether this state owns the repository runtime lock used to reclaim
    /// scratch references for its owner identity.
    pub fn holds_runtime_owner_lock(&self) -> bool {
        matches!(self.writer_authority, WriterAuthority::Owner { .. })
    }

    /// Acquires the worktree's single writer lease for `owner_id`.
    ///
    /// The holder re-acquires its own lease unchanged. Only a handle holding
    /// the worktree's runtime owner lock creates the lease or fences a
    /// different holder (ORNA-TURSO-004/005), and every lease such a handle
    /// establishes is bound to its lock identity. That binding is the
    /// liveness proof: while this handle holds the exclusive lock, a lease
    /// bound to another lock identity, or to none, has no live lock holder
    /// behind it. It is taken over with a bumped epoch, so every stale token
    /// fails with `OwnerLost`, and the same transaction fences each Running
    /// request those tokens left into its conservative orphaned outcome
    /// (ORNA-TASK-003, REQUEST-1 step 5). The replacement is therefore
    /// admitted only after recovery. A different owner bound to this
    /// handle's own lock is live and is refused with `LeaseHeld`, as is every
    /// different owner seen by a handle without the lock. A secondary handle
    /// refuses every lease operation, including its own owner's re-acquire
    /// (ORNA-TURSO-005).
    pub async fn acquire_lease(&self, owner_id: [u8; 16]) -> Result<WriterLease, RuntimeError> {
        validate_id(owner_id)?;
        self.require_lease_authority()?;
        let lock_id = self.writer_authority.lock_id();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let lease = match bound_writer_lease_tx(&tx).await? {
            None => {
                tx.execute(
                    "INSERT INTO writer_lease (singleton, owner_id, epoch, lock_owner_id)
                     VALUES (1, ?1, 1, ?2)",
                    params![owner_id.to_vec(), lock_id.map(|id| id.to_vec())],
                )
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
                WriterLease { owner_id, epoch: 1 }
            }
            Some((current, bound_lock)) if current.owner_id == owner_id => {
                if let Some(lock_id) = lock_id
                    && bound_lock != Some(lock_id)
                {
                    tx.execute(
                        "UPDATE writer_lease SET lock_owner_id = ?1 WHERE singleton = 1",
                        params![lock_id.to_vec()],
                    )
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                }
                current
            }
            Some((current, bound_lock)) => {
                let Some(lock_id) = lock_id.filter(|lock_id| bound_lock != Some(*lock_id)) else {
                    return Err(RuntimeError::LeaseHeld);
                };
                Self::replace_writer_lease_tx(
                    &tx,
                    current,
                    owner_id,
                    Some(lock_id),
                    TakeoverRecovery::OrphanRunning,
                )
                .await?
            }
        };
        tx.commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(lease)
    }

    /// Reads the current writer lease without changing runtime state.
    /// A secondary handle is refused with `LeaseHeld`: only the owner reads the
    /// lease, so a second participant cannot obtain a bearer lease.
    ///
    /// This read does not establish that the owner is abandoned. The caller
    /// must supply that liveness proof to [`Self::takeover_lease`], whose
    /// owner-and-epoch compare-and-swap closes the race with a concurrent
    /// takeover.
    pub async fn current_lease(&self) -> Result<Option<WriterLease>, RuntimeError> {
        self.require_lease_authority()?;
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
    /// The replacement is bound to this handle's runtime owner lock when it
    /// holds one. Running requests stay with the caller: a durable recovery
    /// barrier blocks ordinary writes until the caller recovers them with
    /// its own outcomes and completes takeover recovery.
    pub async fn takeover_lease(
        &self,
        abandoned: WriterLease,
        replacement: [u8; 16],
    ) -> Result<WriterLease, RuntimeError> {
        validate_id(abandoned.owner_id)?;
        validate_id(replacement)?;
        self.require_lease_authority()?;
        if abandoned.epoch == 0 {
            return Err(RuntimeError::InvalidIdentity);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let lease = Self::replace_writer_lease_tx(
            &transaction,
            abandoned,
            replacement,
            self.writer_authority.lock_id(),
            TakeoverRecovery::Barrier,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(lease)
    }

    /// Replaces exactly the expected abandoned writer lease inside
    /// `transaction`, binds the replacement to `lock_id`, settles the Running
    /// requests the fenced tokens left as `recovery` selects, and resets the
    /// abandoned owner's stream work. A changed owner or epoch returns
    /// `OwnerLost`.
    async fn replace_writer_lease_tx(
        transaction: &Connection,
        abandoned: WriterLease,
        replacement: [u8; 16],
        lock_id: Option<[u8; 16]>,
        recovery: TakeoverRecovery,
    ) -> Result<WriterLease, RuntimeError> {
        let changed = transaction
            .execute(
                "UPDATE writer_lease
                 SET owner_id = ?1, epoch = epoch + 1, lock_owner_id = ?4
                 WHERE singleton = 1 AND owner_id = ?2 AND epoch = ?3",
                params![
                    replacement.to_vec(),
                    abandoned.owner_id.to_vec(),
                    i64::try_from(abandoned.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    lock_id.map(|id| id.to_vec()),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if changed != 1 {
            return Err(RuntimeError::OwnerLost);
        }
        let replacement_epoch = abandoned
            .epoch
            .checked_add(1)
            .ok_or(RuntimeError::RecoveryInvalid)?;
        match recovery {
            TakeoverRecovery::Barrier => {
                let mut running = transaction
                    .query(
                        "SELECT 1 FROM request_ledger WHERE state = ?1 LIMIT 1",
                        params![RequestState::Running.code()],
                    )
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
                if running
                    .next()
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?
                    .is_some()
                {
                    transaction
                        .execute(
                            "INSERT INTO takeover_recovery_pending (singleton, owner_id, owner_epoch)
                             VALUES (1, ?1, ?2)
                             ON CONFLICT(singleton) DO UPDATE SET
                               owner_id = excluded.owner_id, owner_epoch = excluded.owner_epoch",
                            params![
                                replacement.to_vec(),
                                i64::try_from(replacement_epoch)
                                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
                            ],
                        )
                        .await
                        .map_err(|_| RuntimeError::StorageUnavailable)?;
                } else {
                    transaction
                        .execute("DELETE FROM takeover_recovery_pending", ())
                        .await
                        .map_err(|_| RuntimeError::StorageUnavailable)?;
                }
            }
            TakeoverRecovery::OrphanRunning => {
                orphan_fenced_running_requests_tx(transaction).await?;
                transaction
                    .execute("DELETE FROM takeover_recovery_pending", ())
                    .await
                    .map_err(|_| RuntimeError::StorageUnavailable)?;
            }
        }
        // A retrying delivery can be returned to `Failed` only when the
        // durable execution lease and its retry claim still describe the same
        // retained failure.  The writer fence prevents the old owner from
        // committing after this point, but it is not authority to reinterpret
        // malformed attempt state as an interrupted callback.
        validate_recoverable_stream_attempts(transaction).await?;
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
            .execute("DELETE FROM stream_replay_claim", ())
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
        Ok(WriterLease {
            owner_id: replacement,
            epoch,
        })
    }

    /// Clears the durable takeover fence after the replacement writer has
    /// recovered every abandoned Running request. The fence and the
    /// no-Running check are one immediate transaction, so no ordinary writer
    /// can slip an admission between the check and reopening the runtime.
    pub async fn complete_takeover_recovery(
        &self,
        replacement: WriterLease,
    ) -> Result<(), RuntimeError> {
        validate_writer_lease(replacement)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_recovery_owner(&transaction, replacement)
            .await?;
        self.settle_takeover_barrier(&transaction, replacement)
            .await?
            .require_clear()?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    /// Removes the matching takeover fence only after every Running request
    /// is terminal, and reports which side of that line the runtime is on.
    /// Request recovery ignores a still-pending result so the final row
    /// recovery and the fence removal commit together; explicit completion
    /// and ordinary writers turn it into `RecoveryPending`.
    pub(crate) async fn settle_takeover_barrier(
        &self,
        transaction: &Connection,
        replacement: WriterLease,
    ) -> Result<TakeoverBarrier, RuntimeError> {
        let mut fence = transaction
            .query(
                "SELECT owner_id, owner_epoch FROM takeover_recovery_pending
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let Some(row) = fence
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        else {
            return Ok(TakeoverBarrier::Clear);
        };
        let owner = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let epoch: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if owner != replacement.owner_id || u64::try_from(epoch).ok() != Some(replacement.epoch) {
            return Err(RuntimeError::RecoveryPending);
        }
        let mut running = transaction
            .query(
                "SELECT 1 FROM request_ledger WHERE state = ?1 LIMIT 1",
                params![RequestState::Running.code()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if running
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some()
        {
            return Ok(TakeoverBarrier::Pending);
        }
        let cleared = transaction
            .execute(
                "DELETE FROM takeover_recovery_pending
                 WHERE singleton = 1 AND owner_id = ?1 AND owner_epoch = ?2",
                params![
                    replacement.owner_id.to_vec(),
                    i64::try_from(replacement.epoch).map_err(|_| RuntimeError::RecoveryInvalid)?,
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if cleared != 1 {
            return Err(RuntimeError::RecoveryPending);
        }
        Ok(TakeoverBarrier::Clear)
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
        self.require_lease_authority()?;
        let expected = self.current_lease().await?;
        let Some(expected) = expected.filter(|lease| lease.owner_id == abandoned) else {
            return Err(RuntimeError::OwnerLost);
        };
        self.takeover_lease(expected, replacement).await
    }

    /// Refuses a secondary handle before it touches the writer lease. Only the
    /// owner-lock holder coordinates lease transitions; any other local writer
    /// fails rather than reopening the writable state (ORNA-TURSO-005).
    pub(crate) fn require_lease_authority(&self) -> Result<(), RuntimeError> {
        if matches!(self.writer_authority, WriterAuthority::Secondary) {
            return Err(RuntimeError::LeaseHeld);
        }
        Ok(())
    }

    pub(crate) async fn require_owner(
        &self,
        tx: &Connection,
        lease: WriterLease,
    ) -> Result<(), RuntimeError> {
        self.require_current_owner(tx, lease).await?;
        // A takeover with no durable Running request has nothing to recover.
        // Resolve that already-satisfied barrier in this same writer
        // transaction; otherwise the strict check keeps every ordinary write
        // unavailable until recovery has made every Running row terminal.
        self.settle_takeover_barrier(tx, lease)
            .await?
            .require_clear()
    }

    /// Permits a recovery write only for the exact replacement lease that
    /// installed the takeover fence. In the normal (no-fence) case this keeps
    /// the existing direct recovery APIs compatible; while fenced, a current
    /// but different writer is never recovery authority.
    pub(crate) async fn require_recovery_owner(
        &self,
        tx: &Connection,
        lease: WriterLease,
    ) -> Result<(), RuntimeError> {
        self.require_current_owner(tx, lease).await?;
        let mut rows = tx
            .query(
                "SELECT owner_id, owner_epoch FROM takeover_recovery_pending
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
        else {
            return Ok(());
        };
        let owner = fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let epoch: i64 = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if owner != lease.owner_id || u64::try_from(epoch).ok() != Some(lease.epoch) {
            return Err(RuntimeError::RecoveryPending);
        }
        Ok(())
    }

    pub(crate) async fn require_recovery_barrier_clear(
        &self,
        tx: &Connection,
    ) -> Result<(), RuntimeError> {
        let mut rows = tx
            .query(
                "SELECT 1 FROM takeover_recovery_pending WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .is_some()
        {
            return Err(RuntimeError::RecoveryPending);
        }
        Ok(())
    }

    async fn require_current_owner(
        &self,
        tx: &Connection,
        lease: WriterLease,
    ) -> Result<(), RuntimeError> {
        self.require_lease_authority()?;
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
