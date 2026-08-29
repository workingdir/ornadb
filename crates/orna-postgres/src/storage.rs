//! Adapter from the neutral revision lifecycle contract to PostgreSQL.

use orna_core::revision::{ActiveDatabaseRevision, DeployableRevision};
use orna_storage::{BootstrapRevision, RevisionStore, StorageError};

use crate::PostgresKernel;

impl RevisionStore for PostgresKernel {
    type Error = crate::PostgresKernelError;

    async fn bootstrap(&self) -> Result<BootstrapRevision, StorageError<Self::Error>> {
        PostgresKernel::bootstrap(self)
            .await
            .map(|revision| BootstrapRevision::new(revision.source(), revision.catalogue()))
            .map_err(StorageError::Backend)
    }

    async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<Self::Error>> {
        PostgresKernel::recover(self).await.map_err(StorageError::Backend)
    }

    async fn apply(&self, candidate: &DeployableRevision) -> Result<ActiveDatabaseRevision, StorageError<Self::Error>> {
        PostgresKernel::apply(self, candidate).await.map_err(StorageError::Backend)
    }
}
