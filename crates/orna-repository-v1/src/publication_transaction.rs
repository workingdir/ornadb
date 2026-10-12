//! Caller-owned durable commit seam for a single protected ORP Blob row.
//!
//! This module prepares native OGS-1/ORP-1 objects and keeps their root alive
//! while the caller runs its shared publication/runtime-intent commit. It
//! does not own a runtime dependency, journal, or alternate commit path.

use std::future::Future;

use crate::{
    CoordinationLock, GraphError, IndexGeneration, NativeGraphContext, OrpGraphCandidate,
    ProtectedContentPin, PublicationJournal, PublicationJournalStage, Repository, RepositoryError,
    row_store::TypedKey,
};

/// One typed insert of a Blob reference under an ORP logical key.
///
/// The inserted row is a one-field row containing the canonical format-3
/// Blob value associated with the supplied protected pin. The repository
/// checks that the key is absent from the captured row-map leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedBlobRowInsert {
    key: TypedKey,
}

impl ProtectedBlobRowInsert {
    pub fn new(key: TypedKey) -> Self {
        Self { key }
    }

    pub fn key(&self) -> &TypedKey {
        &self.key
    }
}

/// Failure from native candidate preparation or the caller's durable commit.
#[derive(Debug)]
pub enum PublicationTransactionError<E> {
    Graph(GraphError),
    GraphCleanup {
        graph: GraphError,
        cleanup: Vec<(String, GraphError)>,
    },
    Commit(E),
    CommitCleanup {
        commit: E,
        cleanup: Vec<(String, GraphError)>,
    },
}

impl<E> From<GraphError> for PublicationTransactionError<E> {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

/// Prepares a protected Blob row and delegates durability to the caller's
/// shared commit callback exactly once.
///
/// The callback receives the ORP graph candidate and its protected-content
/// transfer evidence together. It must make the candidate root durable and
/// record the transfer/runtime intent in its existing shared commit boundary
/// before returning `Ok`. A rejected callback attempts to remove both
/// provisional roots and returns no candidate, so no row is admitted by this
/// API. If cleanup fails, the returned error includes the failed ref identities
/// and cleanup errors so any surviving roots can be identified.
///
/// The repository keeps the candidate root protected through the callback.
/// The callback can therefore construct and publish its normal Git commit
/// without a second repository journal or a runtime dependency here.
pub async fn commit_protected_blob_row<F, Fut, T, E>(
    graph: &NativeGraphContext,
    pin: ProtectedContentPin,
    media_type: &str,
    suffix: Option<&str>,
    mutation: ProtectedBlobRowInsert,
    commit: F,
) -> Result<(OrpGraphCandidate, T), PublicationTransactionError<E>>
where
    F: FnOnce(OrpGraphCandidate) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let prepared = match graph.prepare_protected_blob_row(pin, media_type, suffix, mutation) {
        Ok(prepared) => prepared,
        Err(crate::native_graph::ProtectedBlobRowPreparationError::Graph(graph)) => {
            return Err(PublicationTransactionError::Graph(graph));
        }
        Err(crate::native_graph::ProtectedBlobRowPreparationError::Cleanup {
            graph,
            cleanup,
        }) => {
            return Err(PublicationTransactionError::GraphCleanup {
                graph,
                cleanup,
            });
        }
    };
    let receipt = match commit(prepared.candidate().clone()).await {
        Ok(receipt) => receipt,
        Err(commit) => {
            return match prepared.cleanup_rejected_callback() {
                Ok(()) => Err(PublicationTransactionError::Commit(commit)),
                Err(cleanup) => Err(PublicationTransactionError::CommitCleanup { commit, cleanup }),
            };
        }
    };
    Ok((prepared.into_candidate(), receipt))
}

/// Holds the repository coordination lock while a caller validates the
/// persisted runtime receipt for a descendant publication. Completion does
/// not modify the selected ref, index, or worktree.
struct DescendantPublicationRecovery {
    repository: Repository,
    journal: PublicationJournal,
    _coordination_lock: CoordinationLock,
}

impl Repository {
    fn begin_descendant_publication_recovery(
        &self,
        runtime_intent_id: [u8; 16],
        journal: &PublicationJournal,
    ) -> Result<DescendantPublicationRecovery, RepositoryError> {
        if journal.compact_manifest().is_some()
            || !matches!(
                journal.stage(),
                PublicationJournalStage::WorktreeReconciled
                    | PublicationJournalStage::RuntimeCompleted
                    | PublicationJournalStage::Complete
            )
            || journal.runtime_intent_id() != Some(runtime_intent_id)
        {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        let coordination_lock = self.acquire_coordination_lock()?;
        self.require_no_pending_checkout_recovery_locked()?;
        self.ensure_no_git_operation_in_progress()?;
        if self.read_publication_journal_locked()?.as_ref() != Some(journal) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.require_publication_candidate_ancestor(journal)?;
        Ok(DescendantPublicationRecovery {
            repository: self.clone(),
            journal: journal.clone(),
            _coordination_lock: coordination_lock,
        })
    }

    fn require_publication_candidate_ancestor(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let _candidate = self.candidate_from_journal(journal)?;
        let descendant = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &descendant == journal.new_head() {
            return Err(RepositoryError::StaleHead);
        }
        let mut ancestry = self.command();
        ancestry.args([
            "merge-base",
            "--is-ancestor",
            journal.new_head().as_str(),
            descendant.as_str(),
        ]);
        let ancestry = ancestry
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !ancestry.status.success() {
            return match ancestry.status.code() {
                Some(1) => Err(RepositoryError::StaleHead),
                _ => Err(RepositoryError::GitOperationFailed),
            };
        }
        Ok(())
    }

    /// Validates a caller-owned persisted runtime receipt while publication
    /// coordination is fenced, then finalizes only the journal boundary.
    /// A receipt failure leaves the persisted journal unchanged.
    pub async fn complete_descendant_publication<E, F, Fut>(
        &self,
        runtime_intent_id: [u8; 16],
        journal: &PublicationJournal,
        complete_receipt: F,
    ) -> Result<Result<IndexGeneration, E>, RepositoryError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let recovery = self.begin_descendant_publication_recovery(runtime_intent_id, journal)?;
        if let Err(error) = complete_receipt().await {
            return Ok(Err(error));
        }
        recovery.finish().map(Ok)
    }
}

impl DescendantPublicationRecovery {
    /// Finalizes the already receipt-validated publication without rebuilding
    /// a candidate or touching the descendant's source workset.
    fn finish(mut self) -> Result<IndexGeneration, RepositoryError> {
        let repository = &self.repository;
        let journal = &mut self.journal;
        repository.require_no_pending_checkout_recovery_locked()?;
        repository.ensure_no_git_operation_in_progress()?;
        if repository.read_publication_journal_locked()?.as_ref() != Some(journal) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        repository.require_publication_candidate_ancestor(journal)?;
        match journal.stage() {
            PublicationJournalStage::WorktreeReconciled => {
                journal.advance(PublicationJournalStage::RuntimeCompleted)?;
                repository.write_publication_journal_locked(journal)?;
            }
            PublicationJournalStage::RuntimeCompleted => {}
            PublicationJournalStage::Complete => {
                repository.clear_publication_journal_locked()?;
                return repository.index_generation();
            }
            _ => return Err(RepositoryError::RuntimeCompletionRequired),
        }
        journal.advance(PublicationJournalStage::Complete)?;
        repository.write_publication_journal_locked(journal)?;
        repository.clear_publication_journal_locked()?;
        repository.index_generation()
    }
}
