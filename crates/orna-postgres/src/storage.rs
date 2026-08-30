//! Adapter from the neutral revision lifecycle contract to PostgreSQL.

use std::future::Future;

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

    fn bootstrap(
        &self,
    ) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send {
        async move {
            PostgresKernel::bootstrap(self)
                .await
                .map(|revision| BootstrapRevision::new(revision.source(), revision.catalogue()))
                .map_err(StorageError::Backend)
        }
    }

    fn recover(
        &self,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        async move {
            PostgresKernel::recover(self)
                .await
                .map_err(StorageError::Backend)
        }
    }

    fn apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        async move {
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
    }

    fn apply_source_apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        async move {
            match PostgresKernel::apply_source_apply_with_artifact(self, candidate, artifact).await
            {
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
    }

    fn read_ledger(
        &self,
    ) -> impl Future<Output = Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>>> + Send
    {
        async move {
            PostgresKernel::read_ledger(self)
                .await
                .map_err(StorageError::Backend)
        }
    }
}
