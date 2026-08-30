// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
    system::{
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
    },
};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Row, Transaction};

use crate::{PostgresKernel, PostgresKernelError, recovery::recover_active_revision};
#[path = "bootstrap/empty_seed.rs"]
mod empty_seed;
#[path = "bootstrap/migrations.rs"]
mod migrations;

use empty_seed::{load_or_seed_active_revision, rewrite_legacy_empty_hashes};
pub(crate) use migrations::require_current_migrations;
use migrations::{
    MIGRATION_LOCK_KEY, MIGRATION_LOCK_NAMESPACE, MIGRATION_REGISTRY_SQL, apply_migrations,
};
#[cfg(test)]
use migrations::{
    MIGRATIONS, legacy_migration_checksum, migration_checksum, migration_checksum_matches,
    validated_migration_registry,
};

/// The consistent empty or active durable revision pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveRevision {
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
}

impl ActiveRevision {
    /// Returns the active source revision identity.
    pub const fn source(self) -> SourceRevisionId {
        self.source
    }

    /// Returns the active semantic catalogue revision identity.
    pub const fn catalogue(self) -> CatalogueRevisionId {
        self.catalogue
    }
}

impl PostgresKernel {
    /// Installs the protected catalogue and returns its active revision pair.
    ///
    /// Repeated and concurrent calls return the same seeded empty revision.
    pub async fn bootstrap(&self) -> Result<ActiveRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let bootstrap_result = bootstrap_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;

        match (bootstrap_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn bootstrap_client(client: &mut Client) -> Result<ActiveRevision, PostgresKernelError> {
    let transaction = client
        .transaction()
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock($1, $2)",
            &[&MIGRATION_LOCK_NAMESPACE, &MIGRATION_LOCK_KEY],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .batch_execute(MIGRATION_REGISTRY_SQL)
        .await
        .map_err(PostgresKernelError::Database)?;

    apply_migrations(&transaction).await?;
    let active = load_or_seed_active_revision(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

#[cfg(test)]
#[path = "bootstrap/tests.rs"]
mod tests;
