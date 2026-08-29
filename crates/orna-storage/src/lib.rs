//! Backend-neutral storage contracts for Orna revision lifecycle operations.
//!
//! This crate deliberately exposes only Orna domain values. Backend adapters
//! own connection pools, transactions, SQL, and driver-specific errors.

use std::{error::Error, fmt, future::Future};

use orna_core::{
    revision::{ActiveDatabaseRevision, DeployableRevision},
    CatalogueRevisionId, SourceRevisionId,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootstrapRevision {
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
}

impl BootstrapRevision {
    /// Creates a bootstrap revision pair.
    pub const fn new(source: SourceRevisionId, catalogue: CatalogueRevisionId) -> Self {
        Self { source, catalogue }
    }

    /// Returns the active source revision identity.
    pub const fn source(&self) -> SourceRevisionId { self.source }

    /// Returns the active catalogue revision identity.
    pub const fn catalogue(&self) -> CatalogueRevisionId { self.catalogue }
}

/// Backend-neutral revision lifecycle failure.
#[derive(Debug)]
pub enum StorageError<E> {
    /// The backend reported an error while executing the operation.
    Backend(E),
}

impl<E: fmt::Display> fmt::Display for StorageError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::Backend(error) => write!(f, "storage backend error: {error}") }
    }
}

impl<E: Error + 'static> Error for StorageError<E> {}

/// Minimal revision lifecycle contract implemented by each storage backend.
pub trait RevisionStore {
    /// Backend-specific error type.
    type Error: Error + Send + Sync + 'static;

    /// Bootstraps durable state and returns the seeded active pair.
    fn bootstrap(&self) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send;

    /// Recovers the complete active durable revision.
    fn recover(&self) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send;

    /// Atomically applies one compiler-produced candidate revision.
    fn apply(&self, candidate: &DeployableRevision) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{CatalogueRevisionId, SourceRevisionId};

    #[test]
    fn bootstrap_revision_keeps_domain_identities() {
        let source = SourceRevisionId::new();
        let catalogue = CatalogueRevisionId::new();
        let pair = BootstrapRevision::new(source, catalogue);
        assert_eq!(pair.source(), source);
        assert_eq!(pair.catalogue(), catalogue);
    }
}
