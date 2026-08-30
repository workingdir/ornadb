//! Adapter from the neutral revision lifecycle contract to PostgreSQL.

use orna_core::{
    physical::PhysicalMigrationArtifact,
    revision::{ActiveDatabaseRevision, DeployableRevision},
};
use orna_storage::{
    ApplicationRevisionStore, BootstrapRevision, MigrationLedgerEntry, MigrationLedgerEntryError,
    StorageError,
};

use crate::{PostgresKernel, PostgresKernelError};

impl ApplicationRevisionStore for PostgresKernel {
    type Error = PostgresKernelError;

    async fn bootstrap(&self) -> Result<BootstrapRevision, StorageError<Self::Error>> {
        PostgresKernel::bootstrap(self)
            .await
            .map(|revision| BootstrapRevision::new(revision.source(), revision.catalogue()))
            .map_err(StorageError::Backend)
    }

    async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<Self::Error>> {
        PostgresKernel::recover(self)
            .await
            .map_err(StorageError::Backend)
    }

    async fn apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> Result<ActiveDatabaseRevision, StorageError<Self::Error>> {
        match PostgresKernel::apply_with_artifact(self, candidate, artifact).await {
            Ok(active) => Ok(active),
            Err(PostgresKernelError::InvalidLedgerRequest(error)) => {
                Err(StorageError::InvalidRequest(error))
            }
            Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }) => Err(
                StorageError::InvalidRequest(MigrationLedgerEntryError::ActiveBaseMismatch {
                    expected,
                    actual: active,
                }),
            ),
            Err(error) => Err(StorageError::Backend(error)),
        }
    }

    async fn apply_source_apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> Result<ActiveDatabaseRevision, StorageError<Self::Error>> {
        match PostgresKernel::apply_source_apply_with_artifact(self, candidate, artifact).await {
            Ok(active) => Ok(active),
            Err(PostgresKernelError::InvalidLedgerRequest(error)) => {
                Err(StorageError::InvalidRequest(error))
            }
            Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }) => Err(
                StorageError::InvalidRequest(MigrationLedgerEntryError::ActiveBaseMismatch {
                    expected,
                    actual: active,
                }),
            ),
            Err(error) => Err(StorageError::Backend(error)),
        }
    }

    async fn read_ledger(&self) -> Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>> {
        PostgresKernel::read_ledger(self)
            .await
            .map_err(StorageError::Backend)
    }
}
