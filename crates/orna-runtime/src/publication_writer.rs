//! The typed handle that gates publication-state writes below the CLI.

use crate::RuntimeState;
use crate::error::RuntimeError;
use crate::publication::{
    Checkpoint, PublicationCommitId, PublicationFreeze, PublicationTableIdentity,
};
use orna_repository::{
    CompactPublicationPending, CompactRuntimeReceipt, PublicationLease, Repository, RepositoryError,
};

/// A publication-state writer bound to the worktree publication lock.
///
/// Freezing, compact binding/completion and ordinary completion change the
/// runtime database after durable rows exist. ORNA-TURSO-005 requires another
/// local writer to coordinate with the worktree's owner or fail, and PUB-3
/// steps 2 and 7 name the publication lock as that coordination primitive.
/// This handle is the only way to reach those methods: obtaining it takes the
/// worktree's publication lease, so no caller below the CLI can freeze or
/// complete a publication outside the publication boundary. The lease is
/// released on drop and by the kernel when the holding process dies, so an
/// abandoned publisher cannot keep writing publication state
/// (ORNA-TURSO-006).
pub struct PublicationWriter<'a> {
    state: &'a RuntimeState,
    lease: PublicationLease,
}

impl<'a> PublicationWriter<'a> {
    /// The publication lease this writer holds.
    pub fn lease(&self) -> &PublicationLease {
        &self.lease
    }

    /// Persists the effective compressed publication target. Validation and
    /// storage errors keep the ordinary [`RuntimeError`] surface.
    pub async fn set_publication_compressed_target_bytes(
        &self,
        compressed_target_bytes: u64,
    ) -> Result<(), RuntimeError> {
        self.state
            .set_publication_compressed_target_bytes_inner(compressed_target_bytes)
            .await
    }

    /// Freezes the contiguous committed range named by `checkpoint`.
    pub async fn freeze(
        &self,
        intent_id: [u8; 16],
        checkpoint: &Checkpoint,
    ) -> Result<PublicationFreeze, RuntimeError> {
        self.state.freeze_inner(intent_id, checkpoint).await
    }

    /// Freezes a compact publication using schema admission's authoritative
    /// table/profile identities.
    pub async fn freeze_compact(
        &self,
        intent_id: [u8; 16],
        checkpoint: &Checkpoint,
        identities: &[PublicationTableIdentity],
    ) -> Result<PublicationFreeze, RuntimeError> {
        self.state
            .freeze_compact_inner(intent_id, checkpoint, identities)
            .await
    }

    /// Durably binds a freeze to one compact candidate before receipt
    /// completion.
    pub async fn bind_compact_publication(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<(), RuntimeError> {
        self.state
            .bind_compact_publication_inner(pending, freeze)
            .await
    }

    /// Atomically signs and persists a compact publication receipt.
    pub async fn complete_compact_publication(
        &self,
        pending: &CompactPublicationPending,
        freeze: &PublicationFreeze,
    ) -> Result<CompactRuntimeReceipt, RuntimeError> {
        self.state
            .complete_compact_publication_inner(pending, freeze)
            .await
    }

    /// Records a verified ordinary publication and consumes only the frozen
    /// pending prefix.
    pub async fn complete_publication(
        &self,
        freeze: &PublicationFreeze,
        commit_id: &PublicationCommitId,
    ) -> Result<(), RuntimeError> {
        self.state
            .complete_publication_inner(freeze, commit_id)
            .await
    }
}

impl RuntimeState {
    /// Takes the worktree's publication lease and returns the handle that
    /// gates publication-state writes below the CLI.
    ///
    /// The lease must cover this runtime's own root: a lease taken in another
    /// worktree never authorises this state. A held lease fails with
    /// [`RuntimeError::LeaseHeld`], and an unavailable lock with
    /// [`RuntimeError::StorageUnavailable`].
    pub fn publication_writer(
        &self,
        repository: &Repository,
    ) -> Result<PublicationWriter<'_>, RuntimeError> {
        if repository.runtime_paths().root() != self.runtime_root {
            return Err(RuntimeError::OwnerLost);
        }
        let lease = repository
            .acquire_publication_lease()
            .map_err(|error| match error {
                RepositoryError::RepositoryBusy => RuntimeError::LeaseHeld,
                _ => RuntimeError::StorageUnavailable,
            })?;
        Ok(PublicationWriter { state: self, lease })
    }
}
