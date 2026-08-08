use orna_core::{CatalogueRevisionId, SourceBundleId, SourceRevisionId};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Row, Transaction};

use crate::{PostgresKernel, PostgresKernelError};

const MIGRATION_VERSION: i64 = 1;
const MIGRATION_NAME: &str = "private kernel catalogue";
const MIGRATION_SQL: &str = include_str!("../migrations/0001_kernel.sql");
const MIGRATION_REGISTRY_SQL: &str = "
    CREATE SCHEMA IF NOT EXISTS _orna_kernel;
    REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;
    CREATE TABLE IF NOT EXISTS _orna_kernel.schema_migrations (
        version bigint PRIMARY KEY CHECK (version > 0),
        name text NOT NULL CHECK (length(name) > 0),
        checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
        applied_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp()
    );
    REVOKE ALL ON TABLE _orna_kernel.schema_migrations FROM PUBLIC;
";
const MIGRATION_LOCK_NAMESPACE: i32 = 0x4f52_4e41;
const MIGRATION_LOCK_KEY: i32 = 1;

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

    apply_initial_migration(&transaction).await?;
    let active = load_or_seed_active_revision(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

async fn apply_initial_migration(transaction: &Transaction<'_>) -> Result<(), PostgresKernelError> {
    let expected_checksum = Sha256::digest(MIGRATION_SQL.as_bytes()).to_vec();
    let newest_version = transaction
        .query_one(
            "SELECT max(version) FROM _orna_kernel.schema_migrations",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .get::<_, Option<i64>>(0);
    if let Some(version) = newest_version.filter(|version| *version > MIGRATION_VERSION) {
        return Err(PostgresKernelError::MigrationMismatch { version });
    }
    let applied = transaction
        .query_opt(
            "SELECT name, checksum
             FROM _orna_kernel.schema_migrations
             WHERE version = $1",
            &[&MIGRATION_VERSION],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    if let Some(row) = applied {
        let applied_name: String = row.get("name");
        let applied_checksum: Vec<u8> = row.get("checksum");
        if applied_name != MIGRATION_NAME || applied_checksum != expected_checksum {
            return Err(PostgresKernelError::MigrationMismatch {
                version: MIGRATION_VERSION,
            });
        }
        return Ok(());
    }

    transaction
        .batch_execute(MIGRATION_SQL)
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
             VALUES ($1, $2, $3)",
            &[&MIGRATION_VERSION, &MIGRATION_NAME, &expected_checksum],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn load_or_seed_active_revision(
    transaction: &Transaction<'_>,
) -> Result<ActiveRevision, PostgresKernelError> {
    let active = transaction
        .query_opt(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if let Some(row) = active {
        return active_from_row(&row);
    }

    let counts = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.source_bundles),
                (SELECT count(*) FROM _orna_kernel.source_revisions),
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions)",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let durable_rows = counts.get::<_, i64>(0) + counts.get::<_, i64>(1) + counts.get::<_, i64>(2);
    if durable_rows != 0 {
        return Err(PostgresKernelError::CatalogueInvariant(
            "durable revisions exist without an active revision pointer",
        ));
    }

    let bundle = SourceBundleId::new();
    let source = SourceRevisionId::new();
    let catalogue = CatalogueRevisionId::new();
    let bundle_bytes = bundle.to_bytes().to_vec();
    let source_bytes = source.to_bytes().to_vec();
    let catalogue_bytes = catalogue.to_bytes().to_vec();

    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id) VALUES ($1)",
            &[&bundle_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions (id, bundle_id) VALUES ($1, $2)",
            &[&source_bytes, &bundle_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions (id, source_revision_id)
             VALUES ($1, $2)",
            &[&catalogue_bytes, &source_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.active_revision
                (singleton, source_revision_id, catalogue_revision_id)
             VALUES (true, $1, $2)",
            &[&source_bytes, &catalogue_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    Ok(ActiveRevision { source, catalogue })
}

fn active_from_row(row: &Row) -> Result<ActiveRevision, PostgresKernelError> {
    Ok(ActiveRevision {
        source: SourceRevisionId::from_bytes(exact_id_bytes(
            row.get::<_, Vec<u8>>("source_revision_id"),
            "active source revision identity is not 16 bytes",
        )?),
        catalogue: CatalogueRevisionId::from_bytes(exact_id_bytes(
            row.get::<_, Vec<u8>>("catalogue_revision_id"),
            "active catalogue revision identity is not 16 bytes",
        )?),
    })
}

fn exact_id_bytes(bytes: Vec<u8>, message: &'static str) -> Result<[u8; 16], PostgresKernelError> {
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::CatalogueInvariant(message))
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use super::PostgresKernel;

    #[tokio::test]
    #[ignore = "requires an empty private PostgreSQL test database"]
    async fn bootstrap_is_idempotent_under_concurrency() {
        let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
            .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
        let kernel = Arc::new(PostgresKernel::from_str(&connection_string).expect("config parses"));

        let first_kernel = Arc::clone(&kernel);
        let second_kernel = Arc::clone(&kernel);
        let (first, second) = tokio::join!(first_kernel.bootstrap(), second_kernel.bootstrap());
        let first = first.expect("first bootstrap succeeds");
        let second = second.expect("second bootstrap succeeds");

        assert_eq!(first, second);
        assert_eq!(kernel.bootstrap().await.expect("restart succeeds"), first);
    }
}
