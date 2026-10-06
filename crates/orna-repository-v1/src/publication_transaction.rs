//! Caller-owned durable commit seam for a single protected ORP Blob row.
//!
//! This module prepares native OGS-1/ORP-1 objects and keeps their root alive
//! while the caller runs its shared publication/runtime-intent commit. It
//! does not own a runtime dependency, journal, or alternate commit path.

use std::future::Future;

use crate::{
    GraphError, NativeGraphContext, OrpGraphCandidate, ProtectedContentPin, row_store::TypedKey,
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
/// before returning `Ok`. A rejected callback removes both provisional roots
/// and returns no candidate, so no row is admitted by this API.
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
