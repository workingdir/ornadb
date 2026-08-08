use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Row, Transaction};

use crate::{PostgresKernel, PostgresKernelError};

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    data_step: Option<MigrationDataStep>,
}

#[derive(Clone, Copy)]
enum MigrationDataStep {
    CanonicalHashV1EmptySeed,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "private kernel catalogue",
        sql: include_str!("../migrations/0001_kernel.sql"),
        data_step: None,
    },
    Migration {
        version: 2,
        name: "revision catalogue integrity",
        sql: include_str!("../migrations/0002_revisions.sql"),
        data_step: None,
    },
    Migration {
        version: 3,
        name: "definition reference integrity",
        sql: include_str!("../migrations/0003_reference_integrity.sql"),
        data_step: None,
    },
    Migration {
        version: 4,
        name: "canonical hash contract v1",
        sql: include_str!("../migrations/0004_canonical_hash_contract.sql"),
        data_step: Some(MigrationDataStep::CanonicalHashV1EmptySeed),
    },
    Migration {
        version: 5,
        name: "owner-qualified reference targets",
        sql: include_str!("../migrations/0005_owner_qualified_reference_targets.sql"),
        data_step: None,
    },
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
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

    apply_migrations(&transaction).await?;
    let active = load_or_seed_active_revision(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

async fn apply_migrations(transaction: &Transaction<'_>) -> Result<(), PostgresKernelError> {
    let migrations = validated_migration_registry()?;
    let applied_count = validated_applied_migration_count(transaction, migrations).await?;

    for migration in migrations.iter().skip(applied_count) {
        transaction
            .batch_execute(migration.sql)
            .await
            .map_err(PostgresKernelError::Database)?;
        apply_migration_data_step(migration, transaction).await?;
        let checksum = migration_checksum(migration);
        transaction
            .execute(
                "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                 VALUES ($1, $2, $3)",
                &[&migration.version, &migration.name, &checksum],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    Ok(())
}

pub(crate) async fn require_current_migrations(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let migrations = validated_migration_registry()?;
    let applied_count = validated_applied_migration_count(transaction, migrations).await?;
    if applied_count == migrations.len() {
        return Ok(());
    }

    Err(PostgresKernelError::MigrationMismatch {
        version: migrations[applied_count].version,
    })
}

async fn validated_applied_migration_count(
    transaction: &Transaction<'_>,
    migrations: &[Migration],
) -> Result<usize, PostgresKernelError> {
    let applied = transaction
        .query(
            "SELECT version, name, checksum
             FROM _orna_kernel.schema_migrations
             ORDER BY version",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for (index, row) in applied.iter().enumerate() {
        let version: i64 = row
            .try_get("version")
            .map_err(PostgresKernelError::Database)?;
        let Some(expected) = migrations.get(index) else {
            return Err(PostgresKernelError::MigrationMismatch { version });
        };
        if version != expected.version {
            return Err(PostgresKernelError::MigrationMismatch { version });
        }

        let applied_name: String = row.try_get("name").map_err(PostgresKernelError::Database)?;
        let applied_checksum: Vec<u8> = row
            .try_get("checksum")
            .map_err(PostgresKernelError::Database)?;
        let expected_checksum = migration_checksum(expected);
        if applied_name != expected.name || applied_checksum != expected_checksum {
            return Err(PostgresKernelError::MigrationMismatch { version });
        }
    }

    Ok(applied.len())
}

fn validated_migration_registry() -> Result<&'static [Migration], PostgresKernelError> {
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let expected_version = i64::try_from(index + 1).map_err(|_| {
            PostgresKernelError::CatalogueInvariant("migration registry exceeds bigint versions")
        })?;
        if migration.version != expected_version {
            return Err(PostgresKernelError::CatalogueInvariant(
                "migration registry versions are not contiguous",
            ));
        }
    }
    Ok(MIGRATIONS)
}

fn migration_checksum(migration: &Migration) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(migration.sql.as_bytes());
    if let Some(data_step) = migration.data_step {
        hash.update(MIGRATION_DATA_STEP_SEPARATOR);
        hash.update(data_step.identity());
    }
    hash.finalize().to_vec()
}

impl MigrationDataStep {
    const fn identity(self) -> &'static [u8] {
        match self {
            Self::CanonicalHashV1EmptySeed => CANONICAL_HASH_V1_EMPTY_SEED_STEP,
        }
    }
}

async fn apply_migration_data_step(
    migration: &Migration,
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    match migration.data_step {
        None => Ok(()),
        Some(MigrationDataStep::CanonicalHashV1EmptySeed) => {
            rewrite_legacy_empty_hashes(transaction).await
        }
    }
}

struct CanonicalEmptyHashes {
    bundle: Vec<u8>,
    source: Vec<u8>,
    catalogue: Vec<u8>,
}

fn canonical_empty_hashes(
    bundle: SourceBundleId,
    catalogue: CatalogueRevisionId,
) -> Result<CanonicalEmptyHashes, PostgresKernelError> {
    let bundle_hash = source_bundle_digest(&[]).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty source bundle hash",
        )
    })?;
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty source revision hash",
        )
    })?;
    let empty_catalogue =
        CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).map_err(|_| {
            PostgresKernelError::CatalogueInvariant(
                "cannot construct the canonical empty catalogue",
            )
        })?;
    let catalogue_hash = catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).map_err(|_| {
        PostgresKernelError::CatalogueInvariant(
            "cannot calculate the canonical empty catalogue hash",
        )
    })?;

    Ok(CanonicalEmptyHashes {
        bundle: bundle_hash.to_bytes().to_vec(),
        source: source_hash.to_bytes().to_vec(),
        catalogue: catalogue_hash.to_bytes().to_vec(),
    })
}

struct EmptyRevisionState {
    bundle: SourceBundleId,
    source: SourceRevisionId,
    catalogue: CatalogueRevisionId,
    bundle_hash: Vec<u8>,
    source_hash: Vec<u8>,
    catalogue_hash: Vec<u8>,
}

async fn rewrite_legacy_empty_hashes(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let Some(legacy) = strict_empty_revision_state(transaction).await? else {
        return Ok(());
    };
    let legacy_hash = Sha256::digest([]).to_vec();
    require_empty_revision_hashes(
        &legacy,
        &legacy_hash,
        &legacy_hash,
        &legacy_hash,
        "unsupported legacy aggregate hash",
    )?;

    let canonical = canonical_empty_hashes(legacy.bundle, legacy.catalogue)?;
    if canonical.bundle == legacy_hash
        || canonical.source == legacy_hash
        || canonical.catalogue == legacy_hash
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration computed a legacy aggregate hash",
        ));
    }
    let bundle_bytes = legacy.bundle.to_bytes().to_vec();
    let source_bytes = legacy.source.to_bytes().to_vec();
    let catalogue_bytes = legacy.catalogue.to_bytes().to_vec();
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.source_bundles
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&bundle_bytes, &canonical.bundle, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.source_revisions
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&source_bytes, &canonical.source, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions
             SET content_hash = $2
             WHERE id = $1
               AND content_hash = $3
               AND hash_algorithm = 'sha256'
               AND hash_contract_version = 1",
            &[&catalogue_bytes, &canonical.catalogue, &legacy_hash],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one_hash_rewrite(updated)?;

    let postcondition = strict_empty_revision_state(transaction).await?.ok_or(
        PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration lost the active revision",
        ),
    )?;
    require_empty_revision_hashes(
        &postcondition,
        &canonical.bundle,
        &canonical.source,
        &canonical.catalogue,
        "canonical empty hash migration postcondition failed",
    )?;
    if postcondition.bundle_hash == legacy_hash
        || postcondition.source_hash == legacy_hash
        || postcondition.catalogue_hash == legacy_hash
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration retained a legacy aggregate hash",
        ));
    }

    Ok(())
}

fn require_one_hash_rewrite(updated: u64) -> Result<(), PostgresKernelError> {
    if updated == 1 {
        Ok(())
    } else {
        Err(PostgresKernelError::CatalogueInvariant(
            "canonical empty hash migration could not rewrite one exact legacy aggregate",
        ))
    }
}

fn require_empty_revision_hashes(
    state: &EmptyRevisionState,
    expected_bundle: &[u8],
    expected_source: &[u8],
    expected_catalogue: &[u8],
    message: &'static str,
) -> Result<(), PostgresKernelError> {
    if state.bundle_hash == expected_bundle
        && state.source_hash == expected_source
        && state.catalogue_hash == expected_catalogue
    {
        Ok(())
    } else {
        Err(PostgresKernelError::CatalogueInvariant(message))
    }
}

async fn strict_empty_revision_state(
    transaction: &Transaction<'_>,
) -> Result<Option<EmptyRevisionState>, PostgresKernelError> {
    let counts = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.source_bundles) AS bundles,
                (SELECT count(*) FROM _orna_kernel.source_units) AS source_units,
                (SELECT count(*) FROM _orna_kernel.source_revisions) AS source_revisions,
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions) AS catalogue_revisions,
                (SELECT count(*) FROM _orna_kernel.active_revision) AS active_revisions,
                (SELECT count(*) FROM _orna_kernel.catalogue_schemas) AS schemas,
                (SELECT count(*) FROM _orna_kernel.catalogue_object_types) AS object_types,
                (SELECT count(*) FROM _orna_kernel.catalogue_fields) AS fields,
                (SELECT count(*) FROM _orna_kernel.catalogue_expressions) AS expressions,
                (SELECT count(*) FROM _orna_kernel.catalogue_functions) AS functions,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters) AS parameters,
                (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns) AS return_columns,
                (SELECT count(*) FROM _orna_kernel.function_revisions) AS function_revisions,
                (SELECT count(*) FROM _orna_kernel.function_artifacts) AS function_artifacts,
                (SELECT count(*) FROM _orna_kernel.definition_references) AS references,
                (SELECT count(*)
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname = '_orna_data'
                   AND relation.relkind IN ('r', 'p')) AS data_relations",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let count = |column| counts.get::<_, i64>(column);
    let fresh = [
        "bundles",
        "source_units",
        "source_revisions",
        "catalogue_revisions",
        "active_revisions",
        "schemas",
        "object_types",
        "fields",
        "expressions",
        "functions",
        "parameters",
        "return_columns",
        "function_revisions",
        "function_artifacts",
        "references",
        "data_relations",
    ]
    .iter()
    .all(|column| count(*column) == 0);
    if fresh {
        return Ok(None);
    }

    let supported_legacy_empty = count("bundles") == 1
        && count("source_units") == 0
        && count("source_revisions") == 1
        && count("catalogue_revisions") == 1
        && count("active_revisions") == 1
        && count("schemas") == 0
        && count("object_types") == 0
        && count("fields") == 0
        && count("expressions") == 0
        && count("functions") == 0
        && count("parameters") == 0
        && count("return_columns") == 0
        && count("function_revisions") == 0
        && count("function_artifacts") == 0
        && count("references") == 0
        && count("data_relations") == 0;
    if !supported_legacy_empty {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration only supports a fresh or empty legacy catalogue",
        ));
    }

    let row = transaction
        .query_opt(
            "SELECT
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version,
                source.id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                catalogue.id AS catalogue_id,
                catalogue.source_revision_id AS catalogue_source_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                catalogue.content_hash AS catalogue_hash,
                catalogue.hash_algorithm AS catalogue_algorithm,
                catalogue.hash_contract_version AS catalogue_contract_version,
                active.source_revision_id AS active_source_id,
                active.catalogue_revision_id AS active_catalogue_id
             FROM _orna_kernel.source_bundles AS bundle
             JOIN _orna_kernel.source_revisions AS source ON source.bundle_id = bundle.id
             JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.source_revision_id = source.id
             JOIN _orna_kernel.active_revision AS active
               ON active.source_revision_id = source.id
              AND active.catalogue_revision_id = catalogue.id
             FOR UPDATE OF bundle, source, catalogue, active",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .ok_or(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration found an unsupported legacy revision graph",
        ))?;

    let bundle_bytes: Vec<u8> = row
        .try_get("bundle_id")
        .map_err(PostgresKernelError::Database)?;
    let source_bytes: Vec<u8> = row
        .try_get("source_id")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_bytes: Vec<u8> = row
        .try_get("catalogue_id")
        .map_err(PostgresKernelError::Database)?;
    let bundle = SourceBundleId::from_bytes(exact_id_bytes(
        bundle_bytes.clone(),
        "canonical hash migration found a non-16-byte source bundle identity",
    )?);
    let source = SourceRevisionId::from_bytes(exact_id_bytes(
        source_bytes.clone(),
        "canonical hash migration found a non-16-byte source revision identity",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(exact_id_bytes(
        catalogue_bytes.clone(),
        "canonical hash migration found a non-16-byte catalogue revision identity",
    )?);
    let no_parent: Option<Vec<u8>> = row
        .try_get("source_parent_id")
        .map_err(PostgresKernelError::Database)?;
    let no_catalogue_parent: Option<Vec<u8>> = row
        .try_get("catalogue_parent_id")
        .map_err(PostgresKernelError::Database)?;
    let source_bundle: Vec<u8> = row
        .try_get("source_bundle_id")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_source: Vec<u8> = row
        .try_get("catalogue_source_id")
        .map_err(PostgresKernelError::Database)?;
    let active_source: Vec<u8> = row
        .try_get("active_source_id")
        .map_err(PostgresKernelError::Database)?;
    let active_catalogue: Vec<u8> = row
        .try_get("active_catalogue_id")
        .map_err(PostgresKernelError::Database)?;
    let bundle_algorithm: String = row
        .try_get("bundle_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let source_algorithm: String = row
        .try_get("source_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_algorithm: String = row
        .try_get("catalogue_algorithm")
        .map_err(PostgresKernelError::Database)?;
    let bundle_contract_version: i16 = row
        .try_get("bundle_contract_version")
        .map_err(PostgresKernelError::Database)?;
    let source_contract_version: i16 = row
        .try_get("source_contract_version")
        .map_err(PostgresKernelError::Database)?;
    let catalogue_contract_version: i16 = row
        .try_get("catalogue_contract_version")
        .map_err(PostgresKernelError::Database)?;
    if no_parent.is_some()
        || no_catalogue_parent.is_some()
        || source_bundle != bundle_bytes
        || catalogue_source != source_bytes
        || active_source != source_bytes
        || active_catalogue != catalogue_bytes
        || bundle_algorithm != "sha256"
        || source_algorithm != "sha256"
        || catalogue_algorithm != "sha256"
        || bundle_contract_version != 1
        || source_contract_version != 1
        || catalogue_contract_version != 1
    {
        return Err(PostgresKernelError::CatalogueInvariant(
            "canonical hash migration found an unsupported legacy revision graph",
        ));
    }

    Ok(Some(EmptyRevisionState {
        bundle,
        source,
        catalogue,
        bundle_hash: row
            .try_get("bundle_hash")
            .map_err(PostgresKernelError::Database)?,
        source_hash: row
            .try_get("source_hash")
            .map_err(PostgresKernelError::Database)?,
        catalogue_hash: row
            .try_get("catalogue_hash")
            .map_err(PostgresKernelError::Database)?,
    }))
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
    let canonical_hashes = canonical_empty_hashes(bundle, catalogue)?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id, content_hash) VALUES ($1, $2)",
            &[&bundle_bytes, &canonical_hashes.bundle],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions (id, bundle_id, content_hash)
             VALUES ($1, $2, $3)",
            &[&source_bytes, &bundle_bytes, &canonical_hashes.source],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, content_hash)
             VALUES ($1, $2, $3)",
            &[&catalogue_bytes, &source_bytes, &canonical_hashes.catalogue],
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

    use super::{MIGRATIONS, PostgresKernel, validated_migration_registry};

    #[test]
    fn migration_registry_is_a_strict_contiguous_sequence() {
        assert_eq!(
            validated_migration_registry()
                .expect("registry is valid")
                .len(),
            5
        );
        assert_eq!(MIGRATIONS[0].version, 1);
        assert_eq!(MIGRATIONS[1].version, 2);
        assert_eq!(MIGRATIONS[2].version, 3);
        assert_eq!(MIGRATIONS[3].version, 4);
        assert_eq!(MIGRATIONS[4].version, 5);
    }

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
