// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
use std::collections::HashSet;

use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
    physical::{PhysicalMigrationArtifact, PhysicalPlan},
    revision::RevisionPair,
    system::{
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
    },
};
use orna_storage::MigrationLedgerEntry;
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Row, Transaction};

use crate::{PostgresKernel, PostgresKernelError, recovery::recover_active_revision};

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    data_step: Option<MigrationDataStep>,
}

#[derive(Clone, Copy)]
enum MigrationDataStep {
    CanonicalHashV1EmptySeed,
    BackfillApplicationMigrationLedger,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "private kernel catalogue",
        sql: include_str!("../../migrations/0001_kernel.sql"),
        data_step: None,
    },
    Migration {
        version: 2,
        name: "revision catalogue integrity",
        sql: include_str!("../../migrations/0002_revisions.sql"),
        data_step: None,
    },
    Migration {
        version: 3,
        name: "definition reference integrity",
        sql: include_str!("../../migrations/0003_reference_integrity.sql"),
        data_step: None,
    },
    Migration {
        version: 4,
        name: "canonical hash contract v1",
        sql: include_str!("../../migrations/0004_canonical_hash_contract.sql"),
        data_step: Some(MigrationDataStep::CanonicalHashV1EmptySeed),
    },
    Migration {
        version: 5,
        name: "owner-qualified reference targets",
        sql: include_str!("../../migrations/0005_owner_qualified_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 6,
        name: "definition reference write evidence",
        sql: include_str!("../../migrations/0006_write_reference_evidence.sql"),
        data_step: None,
    },
    Migration {
        version: 7,
        name: "standard catalogue type storage",
        sql: include_str!("../../migrations/0007_catalogue_types.sql"),
        data_step: None,
    },
    Migration {
        version: 8,
        name: "resolved value type storage",
        sql: include_str!("../../migrations/0008_resolved_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 9,
        name: "security decision snapshot",
        sql: include_str!("../../migrations/0009_security_snapshot.sql"),
        data_step: None,
    },
    Migration {
        version: 10,
        name: "local peer credentials",
        sql: include_str!("../../migrations/0010_local_peer_credentials.sql"),
        data_step: None,
    },
    Migration {
        version: 11,
        name: "protected security audit",
        sql: include_str!("../../migrations/0011_security_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 12,
        name: "catalogue enum type storage",
        sql: include_str!("../../migrations/0012_catalogue_enum_types.sql"),
        data_step: None,
    },
    Migration {
        version: 13,
        name: "resolved enum type storage",
        sql: include_str!("../../migrations/0013_resolved_enum_types.sql"),
        data_step: None,
    },
    Migration {
        version: 14,
        name: "catalogue enum reference targets",
        sql: include_str!("../../migrations/0014_enum_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 15,
        name: "catalogue record value storage",
        sql: include_str!("../../migrations/0015_catalogue_record_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 16,
        name: "resolved record value type storage",
        sql: include_str!("../../migrations/0016_resolved_record_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 17,
        name: "record value field reference targets",
        sql: include_str!("../../migrations/0017_record_field_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 18,
        name: "disjoint field reference targets",
        sql: include_str!("../../migrations/0018_disjoint_field_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 19,
        name: "standard opaque value storage",
        sql: include_str!("../../migrations/0019_standard_opaque_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 20,
        name: "standard enum record field storage",
        sql: include_str!("../../migrations/0020_standard_enum_record_fields.sql"),
        data_step: None,
    },
    Migration {
        version: 21,
        name: "nested record field targets",
        sql: include_str!("../../migrations/0021_nested_record_field_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 22,
        name: "protected invocation audit",
        sql: include_str!("../../migrations/0022_invocation_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 23,
        name: "executable standard relations",
        sql: include_str!("../../migrations/0023_executable_standard_snapshots.sql"),
        data_step: None,
    },
    Migration {
        version: 24,
        name: "capability audit decisions",
        sql: include_str!("../../migrations/0024_capability_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 25,
        name: "durable user state cells",
        sql: include_str!("../../migrations/0025_user_state_cells.sql"),
        data_step: None,
    },
    Migration {
        version: 26,
        name: "user state audit decisions",
        sql: include_str!("../../migrations/0026_user_state_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 27,
        name: "inspect snapshots and trace",
        sql: include_str!("../../migrations/0027_inspect_snapshots.sql"),
        data_step: None,
    },
    Migration {
        version: 28,
        name: "security admin privilege grants",
        sql: include_str!("../../migrations/0028_security_admin.sql"),
        data_step: None,
    },
    Migration {
        version: 29,
        name: "sealed system invocation authorities",
        sql: include_str!("../../migrations/0029_sealed_system_invocation_authorities.sql"),
        data_step: None,
    },
    Migration {
        version: 30,
        name: "active roles system invocation authority",
        sql: include_str!("../../migrations/0030_active_roles_system_invocation_authority.sql"),
        data_step: None,
    },
    Migration {
        version: 31,
        name: "standard JSON executable format",
        sql: include_str!("../../migrations/0031_standard_json_executable_format.sql"),
        data_step: None,
    },
    Migration {
        version: 32,
        name: "protected resource audit",
        sql: include_str!("../../migrations/0032_resource_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 33,
        name: "stream function returns",
        sql: include_str!("../../migrations/0033_stream_function_returns.sql"),
        data_step: None,
    },
    Migration {
        version: 34,
        name: "resource request identity history",
        sql: include_str!("../../migrations/0034_resource_request_history.sql"),
        data_step: None,
    },
    Migration {
        version: 35,
        name: "resource audit target authorities",
        sql: include_str!("../../migrations/0035_resource_audit_target_authority.sql"),
        data_step: None,
    },
    Migration {
        version: 36,
        name: "sealed Inspector value types",
        sql: include_str!("../../migrations/0036_sealed_inspect_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 37,
        name: "source apply audit",
        sql: include_str!("../../migrations/0037_source_apply_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 38,
        name: "source apply principal binding",
        sql: include_str!("../../migrations/0038_source_apply_principal.sql"),
        data_step: None,
    },
    Migration {
        version: 39,
        name: "sealed invocation SECURITY DEFINER denial audit",
        sql: include_str!("../../migrations/0039_security_definer_audit_reason.sql"),
        data_step: None,
    },
    Migration {
        version: 40,
        name: "security admin class-wide grant boundary",
        sql: include_str!("../../migrations/0040_security_admin_class_wide.sql"),
        data_step: None,
    },
    Migration {
        version: 41,
        name: "nullable resource audit nested invocation",
        sql: include_str!("../../migrations/0041_nullable_resource_audit_nested_invocation.sql"),
        data_step: None,
    },
    Migration {
        version: 42,
        name: "non-empty security principal identities",
        sql: include_str!("../../migrations/0042_security_principal_non_empty.sql"),
        data_step: None,
    },
    Migration {
        version: 43,
        name: "source bundle unit memberships",
        sql: include_str!("../../migrations/0043_source_bundle_units.sql"),
        data_step: None,
    },
    Migration {
        version: 44,
        name: "standard table and CSV executable formats",
        sql: include_str!("../../migrations/0044_standard_presenter_executable_formats.sql"),
        data_step: None,
    },
    Migration {
        version: 45,
        name: "inspect snapshot observer context",
        sql: include_str!("../../migrations/0045_inspect_snapshot_observer_context.sql"),
        data_step: None,
    },
    Migration {
        version: 46,
        name: "application_migrations",
        sql: include_str!("../../migrations/0046_application_migrations.sql"),
        data_step: None,
    },
    Migration {
        version: 47,
        name: "application migration ledger baseline",
        sql: include_str!("../../migrations/0047_application_migration_ledger_baseline.sql"),
        data_step: Some(MigrationDataStep::BackfillApplicationMigrationLedger),
    },
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
const APPLICATION_MIGRATION_LEDGER_BASELINE_STEP: &[u8] =
    b"application-migration-ledger-baseline/v1";
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

const LEGACY_MIGRATION_CHECKSUMS: &[(i64, [u8; 32])] = &[
    (
        23,
        [
            0x3c, 0xa6, 0x3b, 0x0c, 0xc4, 0xf2, 0x6d, 0x91, 0x30, 0x5d, 0xcc, 0xd7, 0xda, 0xdc,
            0x50, 0x64, 0xfe, 0xfc, 0xfc, 0xf0, 0x7c, 0x5b, 0x2b, 0x22, 0x6e, 0x92, 0x0b, 0xbf,
            0x88, 0xd0, 0xed, 0x89,
        ],
    ),
    (
        29,
        [
            0xc4, 0x08, 0xd9, 0xfa, 0xeb, 0x56, 0x22, 0x76, 0xb9, 0x19, 0x1d, 0xbd, 0x9d, 0xc1,
            0xe4, 0xce, 0xea, 0x02, 0xd7, 0x94, 0xb9, 0x4e, 0x48, 0x14, 0xf4, 0xfa, 0xb4, 0x0e,
            0x62, 0x96, 0x76, 0xef,
        ],
    ),
    (
        30,
        [
            0x90, 0x36, 0x2d, 0x04, 0x93, 0xf2, 0xbd, 0xd7, 0x9b, 0xcb, 0xf8, 0x6c, 0x23, 0x66,
            0x1d, 0xdc, 0xe5, 0xc0, 0xa4, 0x06, 0x6a, 0x79, 0xe8, 0xed, 0xcc, 0xd7, 0x1b, 0x19,
            0x3e, 0xfe, 0x81, 0x01,
        ],
    ),
];

fn legacy_migration_checksum(version: i64) -> Option<&'static [u8; 32]> {
    LEGACY_MIGRATION_CHECKSUMS
        .iter()
        .find(|(legacy_version, _)| *legacy_version == version)
        .map(|(_, checksum)| checksum)
}

fn migration_checksum_matches(migration: &Migration, applied_checksum: &[u8]) -> bool {
    let expected_checksum = migration_checksum(migration);
    applied_checksum == expected_checksum
        || legacy_migration_checksum(migration.version)
            .is_some_and(|legacy_checksum| applied_checksum == legacy_checksum)
}

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
        if applied_name != expected.name || !migration_checksum_matches(expected, &applied_checksum)
        {
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
            Self::BackfillApplicationMigrationLedger => APPLICATION_MIGRATION_LEDGER_BASELINE_STEP,
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
        Some(MigrationDataStep::BackfillApplicationMigrationLedger) => {
            backfill_application_migration_ledger(transaction).await
        }
    }
}
/// Records the pre-ledger revision path as an explicit historical baseline.
///
/// Before migration 46, physical changes were already applied atomically with
/// revision persistence but no replayable artifact was retained. The baseline
/// therefore binds each existing source/catalogue edge to an empty artifact:
/// it preserves lineage without pretending that old physical operations can be
/// reconstructed.
async fn backfill_application_migration_ledger(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let existing_ledger =
        crate::kernel::apply::load_and_validate_migration_ledger_suffix(transaction).await?;
    let Some(active_row) = transaction
        .query_opt(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
    else {
        if existing_ledger.is_empty() {
            return Ok(());
        }
        return Err(PostgresKernelError::CatalogueInvariant(
            "application migration ledger exists without an active revision",
        ));
    };

    let mut source = SourceRevisionId::from_bytes(exact_id_bytes(
        active_row
            .try_get("source_revision_id")
            .map_err(PostgresKernelError::Database)?,
        "active source revision identity is not 16 bytes",
    )?);
    let mut catalogue = CatalogueRevisionId::from_bytes(exact_id_bytes(
        active_row
            .try_get("catalogue_revision_id")
            .map_err(PostgresKernelError::Database)?,
        "active catalogue revision identity is not 16 bytes",
    )?);
    let mut reverse_path = Vec::new();
    let mut seen_pairs = HashSet::new();

    loop {
        let pair = RevisionPair::new(source, catalogue);
        if !seen_pairs.insert(pair) {
            return Err(PostgresKernelError::CatalogueInvariant(
                "legacy revision ancestry contains a cycle",
            ));
        }

        let source_bytes = source.to_bytes().to_vec();
        let source_row = transaction
            .query_opt(
                "SELECT parent_source_revision_id
                 FROM _orna_kernel.source_revisions
                 WHERE id = $1",
                &[&source_bytes],
            )
            .await
            .map_err(PostgresKernelError::Database)?
            .ok_or(PostgresKernelError::CatalogueInvariant(
                "legacy active source revision is missing",
            ))?;
        let source_parent = optional_id_bytes(
            source_row
                .try_get("parent_source_revision_id")
                .map_err(PostgresKernelError::Database)?,
            "legacy parent source revision identity is not 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);

        let catalogue_bytes = catalogue.to_bytes().to_vec();
        let catalogue_row = transaction
            .query_opt(
                "SELECT source_revision_id, parent_catalogue_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&catalogue_bytes],
            )
            .await
            .map_err(PostgresKernelError::Database)?
            .ok_or(PostgresKernelError::CatalogueInvariant(
                "legacy active catalogue revision is missing",
            ))?;
        let catalogue_source = SourceRevisionId::from_bytes(exact_id_bytes(
            catalogue_row
                .try_get::<_, Vec<u8>>("source_revision_id")
                .map_err(PostgresKernelError::Database)?,
            "legacy catalogue source revision identity is not 16 bytes",
        )?);
        if catalogue_source != source {
            return Err(PostgresKernelError::CatalogueInvariant(
                "legacy catalogue revision is bound to another source revision",
            ));
        }
        let catalogue_parent = optional_id_bytes(
            catalogue_row
                .try_get("parent_catalogue_revision_id")
                .map_err(PostgresKernelError::Database)?,
            "legacy parent catalogue revision identity is not 16 bytes",
        )?
        .map(CatalogueRevisionId::from_bytes);

        reverse_path.push(pair);
        match (source_parent, catalogue_parent) {
            (None, None) => break,
            (Some(next_source), Some(next_catalogue)) => {
                source = next_source;
                catalogue = next_catalogue;
            }
            _ => {
                return Err(PostgresKernelError::CatalogueInvariant(
                    "legacy source and catalogue ancestry are not aligned",
                ));
            }
        }
    }

    reverse_path.reverse();
    let baseline_edge_count = if let Some(first) = existing_ledger.first() {
        let Some(start) = reverse_path
            .iter()
            .position(|pair| *pair == first.expected_base())
        else {
            return Err(PostgresKernelError::CatalogueInvariant(
                "existing migration ledger does not follow legacy revision ancestry",
            ));
        };
        for (offset, entry) in existing_ledger.iter().enumerate() {
            let expected_index = start.checked_add(offset).ok_or(
                PostgresKernelError::CatalogueInvariant(
                    "legacy revision path index exceeds platform limits",
                ),
            )?;
            let expected_base = reverse_path.get(expected_index).ok_or(
                PostgresKernelError::CatalogueInvariant(
                    "existing migration ledger starts beyond legacy revision ancestry",
                ),
            )?;
            let candidate = reverse_path.get(expected_index + 1).ok_or(
                PostgresKernelError::CatalogueInvariant(
                    "existing migration ledger extends beyond legacy revision ancestry",
                ),
            )?;
            if entry.expected_base() != *expected_base || entry.candidate_pair() != *candidate {
                return Err(PostgresKernelError::CatalogueInvariant(
                    "existing migration ledger does not follow legacy revision ancestry",
                ));
            }
        }
        if start.checked_add(existing_ledger.len()) != Some(reverse_path.len() - 1) {
            return Err(PostgresKernelError::CatalogueInvariant(
                "existing migration ledger does not cover the active revision ancestry",
            ));
        }
        start
    } else {
        reverse_path.len() - 1
    };

    if baseline_edge_count > 0 && !existing_ledger.is_empty() {
        let baseline_count = i64::try_from(baseline_edge_count).map_err(|_| {
            PostgresKernelError::CatalogueInvariant("legacy revision path exceeds bigint ordinals")
        })?;
        let existing_count = i64::try_from(existing_ledger.len()).map_err(|_| {
            PostgresKernelError::CatalogueInvariant("existing ledger exceeds bigint ordinals")
        })?;
        let temporary_offset = existing_count
            .checked_add(baseline_count)
            .and_then(|value| value.checked_add(1))
            .ok_or(PostgresKernelError::CatalogueInvariant(
                "legacy ledger ordinal shift exceeds bigint",
            ))?;
        for ordinal in (0..existing_ledger.len()).rev() {
            let ordinal = i64::try_from(ordinal).map_err(|_| {
                PostgresKernelError::CatalogueInvariant("existing ledger ordinal exceeds bigint")
            })?;
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.application_migrations
                     SET ordinal = ordinal + $1
                     WHERE ordinal = $2",
                    &[&temporary_offset, &ordinal],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            if updated != 1 {
                return Err(PostgresKernelError::CatalogueInvariant(
                    "existing ledger ordinal disappeared during baseline shift",
                ));
            }
        }
        let final_shift = temporary_offset
            .checked_sub(baseline_count)
            .expect("temporary ledger offset includes the baseline count");
        for ordinal in 0..existing_ledger.len() {
            let ordinal = i64::try_from(ordinal).map_err(|_| {
                PostgresKernelError::CatalogueInvariant("existing ledger ordinal exceeds bigint")
            })?;
            let shifted_ordinal = ordinal.checked_add(temporary_offset).ok_or(
                PostgresKernelError::CatalogueInvariant(
                    "existing ledger ordinal shift exceeds bigint",
                ),
            )?;
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.application_migrations
                     SET ordinal = ordinal - $1
                     WHERE ordinal = $2",
                    &[&final_shift, &shifted_ordinal],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            if updated != 1 {
                return Err(PostgresKernelError::CatalogueInvariant(
                    "existing ledger ordinal disappeared during baseline shift",
                ));
            }
        }
    }

    for (ordinal, window) in reverse_path
        .windows(2)
        .take(baseline_edge_count)
        .enumerate()
    {
        let [expected_base, candidate_pair] = window else {
            unreachable!("windows(2) always yields pairs");
        };
        let artifact = PhysicalMigrationArtifact::from_plan(
            *expected_base,
            *candidate_pair,
            &PhysicalPlan::empty(),
        )
        .map_err(|_| {
            PostgresKernelError::CatalogueInvariant("legacy revision baseline could not be encoded")
        })?;
        let entry = MigrationLedgerEntry::from_artifact(&artifact);
        let ordinal = i64::try_from(ordinal).map_err(|_| {
            PostgresKernelError::CatalogueInvariant("legacy revision path exceeds bigint ordinals")
        })?;
        let expected_source = entry.expected_base().source().to_bytes().to_vec();
        let expected_catalogue = entry.expected_base().catalogue().to_bytes().to_vec();
        let candidate_source = entry.candidate_pair().source().to_bytes().to_vec();
        let candidate_catalogue = entry.candidate_pair().catalogue().to_bytes().to_vec();
        let version = i64::from(entry.version());
        let digest = entry.digest().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.application_migrations
                    (ordinal, format, version,
                     expected_source_revision_id, expected_catalogue_revision_id,
                     candidate_source_revision_id, candidate_catalogue_revision_id,
                     canonical_bytes, digest)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &ordinal,
                    &entry.format(),
                    &version,
                    &expected_source,
                    &expected_catalogue,
                    &candidate_source,
                    &candidate_catalogue,
                    &entry.canonical_bytes(),
                    &digest,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    Ok(())
}

fn optional_id_bytes(
    bytes: Option<Vec<u8>>,
    message: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    bytes
        .map(|bytes| exact_id_bytes(bytes, message))
        .transpose()
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
    // Migration 4 runs before migration 43 creates the membership table, so
    // account for the relation only when it exists.
    let source_bundle_units: i64 = if transaction
        .query_one(
            "SELECT to_regclass('_orna_kernel.source_bundle_units') IS NOT NULL",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .get(0)
    {
        transaction
            .query_one("SELECT count(*) FROM _orna_kernel.source_bundle_units", &[])
            .await
            .map_err(PostgresKernelError::Database)?
            .get(0)
    } else {
        0
    };

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
    .all(|column| count(*column) == 0)
        && source_bundle_units == 0;
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
        && count("data_relations") == 0
        && source_bundle_units == 0;
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
        let active = active_from_row(&row)?;
        let recovered = recover_active_revision(transaction).await?;
        let pair = recovered.pair();
        if pair.source() != active.source || pair.catalogue() != active.catalogue {
            return Err(PostgresKernelError::CatalogueInvariant(
                "recovered active revision does not match the active revision pointer",
            ));
        }
        crate::kernel::apply::load_and_validate_migration_ledger(transaction, Some(&recovered))
            .await?;
        return Ok(active);
    }

    let counts = transaction
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.source_bundles),
                (SELECT count(*) FROM _orna_kernel.source_revisions),
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions),
                (SELECT count(*) FROM _orna_kernel.application_migrations)",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let durable_rows = counts.get::<_, i64>(0)
        + counts.get::<_, i64>(1)
        + counts.get::<_, i64>(2)
        + counts.get::<_, i64>(3);
    if durable_rows != 0 {
        return Err(PostgresKernelError::CatalogueInvariant(
            "durable revisions or migration ledger rows exist without an active revision pointer",
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
    for function in [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ] {
        let function_bytes = function.to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'system', $2, NULL)",
                &[&catalogue_bytes, &function_bytes],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

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

    use super::{
        MIGRATIONS, PostgresKernel, legacy_migration_checksum, migration_checksum,
        migration_checksum_matches, validated_migration_registry,
    };

    #[test]
    fn migration_registry_is_a_strict_contiguous_sequence() {
        assert_eq!(
            validated_migration_registry()
                .expect("registry is valid")
                .len(),
            47
        );
        assert_eq!(MIGRATIONS[0].version, 1);
        assert_eq!(MIGRATIONS[1].version, 2);
        assert_eq!(MIGRATIONS[2].version, 3);
        assert_eq!(MIGRATIONS[3].version, 4);
        assert_eq!(MIGRATIONS[4].version, 5);
        assert_eq!(MIGRATIONS[5].version, 6);
        assert_eq!(MIGRATIONS[6].version, 7);
        assert_eq!(MIGRATIONS[7].version, 8);
        assert_eq!(MIGRATIONS[8].version, 9);
        assert_eq!(MIGRATIONS[9].version, 10);
        assert_eq!(MIGRATIONS[10].version, 11);
        assert_eq!(MIGRATIONS[11].version, 12);
        assert_eq!(MIGRATIONS[12].version, 13);
        assert_eq!(MIGRATIONS[13].version, 14);
        assert_eq!(MIGRATIONS[14].version, 15);
        assert_eq!(MIGRATIONS[15].version, 16);
        assert_eq!(MIGRATIONS[16].version, 17);
        assert_eq!(MIGRATIONS[17].version, 18);
        assert_eq!(MIGRATIONS[18].version, 19);
        assert_eq!(MIGRATIONS[19].version, 20);
        assert_eq!(MIGRATIONS[20].version, 21);
        assert_eq!(MIGRATIONS[21].version, 22);
        assert_eq!(MIGRATIONS[22].version, 23);
        assert_eq!(MIGRATIONS[23].version, 24);
        assert_eq!(MIGRATIONS[24].version, 25);
        assert_eq!(MIGRATIONS[25].version, 26);
        assert_eq!(MIGRATIONS[26].version, 27);
        assert_eq!(MIGRATIONS[27].version, 28);
        assert_eq!(MIGRATIONS[28].version, 29);
        assert_eq!(MIGRATIONS[29].version, 30);
        assert_eq!(MIGRATIONS[30].version, 31);
        assert_eq!(MIGRATIONS[31].version, 32);
        assert_eq!(MIGRATIONS[32].version, 33);
        assert_eq!(MIGRATIONS[33].version, 34);
        assert_eq!(MIGRATIONS[34].version, 35);
        assert_eq!(MIGRATIONS[35].version, 36);
        assert_eq!(MIGRATIONS[36].version, 37);
        assert_eq!(MIGRATIONS[37].version, 38);
        assert_eq!(MIGRATIONS[38].version, 39);
        assert_eq!(MIGRATIONS[39].version, 40);
        assert_eq!(MIGRATIONS[40].version, 41);
        assert_eq!(MIGRATIONS[41].version, 42);
        assert_eq!(MIGRATIONS[42].version, 43);
        assert_eq!(MIGRATIONS[43].version, 44);
        assert_eq!(MIGRATIONS[44].version, 45);
        assert_eq!(MIGRATIONS[45].version, 46);
        assert_eq!(MIGRATIONS[46].version, 47);
        assert_eq!(MIGRATIONS[33].name, "resource request identity history");
        assert_eq!(MIGRATIONS[34].name, "resource audit target authorities");
        assert_eq!(MIGRATIONS[35].name, "sealed Inspector value types");
        assert_eq!(MIGRATIONS[36].name, "source apply audit");
        assert_eq!(MIGRATIONS[37].name, "source apply principal binding");
        assert_eq!(
            MIGRATIONS[38].name,
            "sealed invocation SECURITY DEFINER denial audit"
        );
        assert_eq!(
            MIGRATIONS[39].name,
            "security admin class-wide grant boundary"
        );
        assert_eq!(
            MIGRATIONS[40].name,
            "nullable resource audit nested invocation"
        );
        assert_eq!(
            MIGRATIONS[41].name,
            "non-empty security principal identities"
        );
        assert_eq!(MIGRATIONS[42].name, "source bundle unit memberships");
        assert_eq!(
            MIGRATIONS[43].name,
            "standard table and CSV executable formats"
        );
        assert_eq!(MIGRATIONS[44].name, "inspect snapshot observer context");
        assert_eq!(MIGRATIONS[45].name, "application_migrations");
        assert_eq!(
            MIGRATIONS[46].name,
            "application migration ledger baseline"
        );
        assert_eq!(MIGRATIONS[5].name, "definition reference write evidence");
        assert_eq!(MIGRATIONS[6].name, "standard catalogue type storage");
        assert_eq!(MIGRATIONS[7].name, "resolved value type storage");
        assert_eq!(MIGRATIONS[8].name, "security decision snapshot");
        assert_eq!(MIGRATIONS[9].name, "local peer credentials");
        assert_eq!(MIGRATIONS[10].name, "protected security audit");
        assert_eq!(MIGRATIONS[11].name, "catalogue enum type storage");
        assert_eq!(MIGRATIONS[12].name, "resolved enum type storage");
        assert_eq!(MIGRATIONS[13].name, "catalogue enum reference targets");
        assert_eq!(MIGRATIONS[14].name, "catalogue record value storage");
        assert_eq!(MIGRATIONS[15].name, "resolved record value type storage");
        assert_eq!(MIGRATIONS[16].name, "record value field reference targets");
        assert_eq!(MIGRATIONS[17].name, "disjoint field reference targets");
        assert_eq!(MIGRATIONS[18].name, "standard opaque value storage");
        assert_eq!(MIGRATIONS[19].name, "standard enum record field storage");
        assert_eq!(MIGRATIONS[20].name, "nested record field targets");
        assert_eq!(MIGRATIONS[21].name, "protected invocation audit");
        assert_eq!(MIGRATIONS[22].name, "executable standard relations");
        assert_eq!(MIGRATIONS[23].name, "capability audit decisions");
        assert_eq!(MIGRATIONS[24].name, "durable user state cells");
        assert_eq!(MIGRATIONS[25].name, "user state audit decisions");
        assert_eq!(MIGRATIONS[26].name, "inspect snapshots and trace");
        assert_eq!(MIGRATIONS[27].name, "security admin privilege grants");
        assert_eq!(MIGRATIONS[28].name, "sealed system invocation authorities");
        assert_eq!(
            MIGRATIONS[29].name,
            "active roles system invocation authority"
        );
        assert_eq!(MIGRATIONS[30].name, "standard JSON executable format");
        assert_eq!(MIGRATIONS[31].name, "protected resource audit");
        assert!(MIGRATIONS[6].data_step.is_none());
        assert!(MIGRATIONS[7].data_step.is_none());
        assert!(MIGRATIONS[8].data_step.is_none());
        assert!(MIGRATIONS[9].data_step.is_none());
        assert!(MIGRATIONS[10].data_step.is_none());
        assert!(MIGRATIONS[11].data_step.is_none());
        assert!(MIGRATIONS[12].data_step.is_none());
        assert!(MIGRATIONS[13].data_step.is_none());
        assert!(MIGRATIONS[14].data_step.is_none());
        assert!(MIGRATIONS[15].data_step.is_none());
        assert!(MIGRATIONS[16].data_step.is_none());
        assert!(MIGRATIONS[17].data_step.is_none());
        assert!(MIGRATIONS[18].data_step.is_none());
        assert!(MIGRATIONS[19].data_step.is_none());
        assert!(MIGRATIONS[20].data_step.is_none());
        assert!(MIGRATIONS[21].data_step.is_none());
        assert!(MIGRATIONS[22].data_step.is_none());
        assert!(MIGRATIONS[23].data_step.is_none());
        assert!(MIGRATIONS[24].data_step.is_none());
        assert!(MIGRATIONS[25].data_step.is_none());
        assert!(MIGRATIONS[26].data_step.is_none());
        assert!(MIGRATIONS[27].data_step.is_none());
        assert!(MIGRATIONS[28].data_step.is_none());
        assert!(MIGRATIONS[29].data_step.is_none());
        assert!(MIGRATIONS[30].data_step.is_none());
        assert!(MIGRATIONS[31].data_step.is_none());
        assert!(MIGRATIONS[32].data_step.is_none());
        assert!(MIGRATIONS[33].data_step.is_none());
        assert!(MIGRATIONS[34].data_step.is_none());
        assert!(MIGRATIONS[35].data_step.is_none());
        assert!(MIGRATIONS[36].data_step.is_none());
        assert!(MIGRATIONS[39].data_step.is_none());
        assert!(MIGRATIONS[40].data_step.is_none());
        assert!(MIGRATIONS[41].data_step.is_none());
        assert!(MIGRATIONS[42].data_step.is_none());
        assert!(MIGRATIONS[43].data_step.is_none());
    }

    #[test]
    fn legacy_migration_checksums_are_scoped_to_versions_23_29_and_30() {
        let expected_legacy_checksums = [
            (
                23_i64,
                [
                    0x3c, 0xa6, 0x3b, 0x0c, 0xc4, 0xf2, 0x6d, 0x91, 0x30, 0x5d, 0xcc, 0xd7, 0xda,
                    0xdc, 0x50, 0x64, 0xfe, 0xfc, 0xfc, 0xf0, 0x7c, 0x5b, 0x2b, 0x22, 0x6e, 0x92,
                    0x0b, 0xbf, 0x88, 0xd0, 0xed, 0x89,
                ],
            ),
            (
                29_i64,
                [
                    0xc4, 0x08, 0xd9, 0xfa, 0xeb, 0x56, 0x22, 0x76, 0xb9, 0x19, 0x1d, 0xbd, 0x9d,
                    0xc1, 0xe4, 0xce, 0xea, 0x02, 0xd7, 0x94, 0xb9, 0x4e, 0x48, 0x14, 0xf4, 0xfa,
                    0xb4, 0x0e, 0x62, 0x96, 0x76, 0xef,
                ],
            ),
            (
                30_i64,
                [
                    0x90, 0x36, 0x2d, 0x04, 0x93, 0xf2, 0xbd, 0xd7, 0x9b, 0xcb, 0xf8, 0x6c, 0x23,
                    0x66, 0x1d, 0xdc, 0xe5, 0xc0, 0xa4, 0x06, 0x6a, 0x79, 0xe8, 0xed, 0xcc, 0xd7,
                    0x1b, 0x19, 0x3e, 0xfe, 0x81, 0x01,
                ],
            ),
        ];

        for (version, expected_checksum) in expected_legacy_checksums {
            let migration = &MIGRATIONS[usize::try_from(version - 1).expect("valid version")];
            assert_eq!(legacy_migration_checksum(version), Some(&expected_checksum));
            assert!(migration_checksum_matches(migration, &expected_checksum));
            assert!(migration_checksum_matches(
                migration,
                &migration_checksum(migration)
            ));

            let mut drifted_legacy_checksum = expected_checksum;
            drifted_legacy_checksum[0] ^= 0xff;
            assert!(!migration_checksum_matches(
                migration,
                &drifted_legacy_checksum
            ));
        }

        assert!(legacy_migration_checksum(22).is_none());
        assert!(legacy_migration_checksum(24).is_none());
        assert!(legacy_migration_checksum(28).is_none());
        assert!(legacy_migration_checksum(31).is_none());
    }

    #[test]
    fn unrelated_migration_checksum_drift_is_rejected() {
        for migration in MIGRATIONS {
            if matches!(migration.version, 23 | 29 | 30) {
                continue;
            }

            let mut drifted_checksum = migration_checksum(migration);
            drifted_checksum[0] ^= 0xff;
            assert!(!migration_checksum_matches(migration, &drifted_checksum));
        }

        let legacy_23_checksum = legacy_migration_checksum(23).expect("version 23 compatibility");
        assert!(!migration_checksum_matches(
            &MIGRATIONS[28],
            legacy_23_checksum
        ));
    }

    #[test]
    fn source_bundle_unit_memberships_is_the_registered_version_forty_three() {
        let migration = &MIGRATIONS[42];

        assert_eq!(migration.version, 43);
        assert_eq!(migration.name, "source bundle unit memberships");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.source_bundle_units")
        );
        assert!(
            migration
                .sql
                .contains("bundle_id bytea NOT NULL REFERENCES _orna_kernel.source_bundles(id)")
        );
        assert!(
            migration
                .sql
                .contains("source_unit_id bytea NOT NULL REFERENCES _orna_kernel.source_units(id)")
        );
        assert!(
            migration
                .sql
                .contains("ordinal bigint NOT NULL CHECK (ordinal >= 0)")
        );
        assert!(
            migration
                .sql
                .contains("PRIMARY KEY (bundle_id, source_unit_id)")
        );
        assert!(migration.sql.contains("UNIQUE (bundle_id, ordinal)"));
        assert!(migration.sql.contains(
            "INSERT INTO _orna_kernel.source_bundle_units (bundle_id, source_unit_id, ordinal)"
        ));
        assert!(migration.sql.contains("SELECT bundle_id, id, ordinal"));
        assert!(
            migration
                .sql
                .contains("REVOKE ALL ON TABLE _orna_kernel.source_bundle_units FROM PUBLIC")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TEMP TABLE _orna_migration_0043_membership_guard")
        );
        assert!(
            migration
                .sql
                .contains("valid boolean NOT NULL CHECK (valid)")
        );
    }

    #[test]
    fn non_empty_security_principal_identity_is_the_registered_version_forty_two() {
        let migration = &MIGRATIONS[41];

        assert_eq!(migration.version, 42);
        assert_eq!(migration.name, "non-empty security principal identities");
        assert!(migration.data_step.is_none());
        assert!(migration.sql.contains("security_principals_id_not_empty"));
        assert!(
            migration
                .sql
                .contains("decode('00000000000000000000000000000000', 'hex')")
        );
    }

    #[test]
    fn nullable_resource_audit_nested_invocation_is_the_registered_version_forty_one() {
        let migration = &MIGRATIONS[40];

        assert_eq!(migration.version, 41);
        assert_eq!(migration.name, "nullable resource audit nested invocation");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("ALTER COLUMN nested_invocation_id DROP NOT NULL")
        );
        assert!(
            migration.sql.contains(
                "nested_invocation_id IS NULL OR octet_length(nested_invocation_id) = 16"
            )
        );
        assert!(
            migration
                .sql
                .contains("resource_audit_events_nested_invocation_presence_check")
        );
        assert!(
            migration
                .sql
                .contains("terminal_outcome IN ('failed', 'cancelled')")
        );
    }

    #[test]
    fn security_admin_class_wide_grant_boundary_is_the_registered_version_forty() {
        let migration = &MIGRATIONS[39];

        assert_eq!(migration.version, 40);
        assert_eq!(migration.name, "security admin class-wide grant boundary");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("security_privilege_grants_security_admin_class_wide_check")
        );
        assert!(
            migration
                .sql
                .contains("CHECK (privilege_class <> 'security_admin' OR object_id = '')")
        );
    }

    #[test]
    fn source_apply_audit_migration_admits_only_committed_candidates() {
        let migration = &MIGRATIONS[36];

        assert_eq!(migration.version, 37);
        assert_eq!(migration.name, "source apply audit");
        assert!(migration.sql.contains("event_kind = 'source_apply'"));
        assert!(
            migration
                .sql
                .contains("denial_reason = 'source_apply:committed'")
        );
        assert!(migration.sql.contains("source_revision_id IS NOT NULL"));
        assert!(migration.sql.contains("catalogue_revision_id IS NOT NULL"));
    }

    #[test]
    fn sealed_inspect_value_migration_preserves_strict_ref_targets() {
        let migration = &MIGRATIONS[35];

        assert_eq!(migration.version, 36);
        assert_eq!(migration.name, "sealed Inspector value types");
        assert!(migration.sql.contains(
            "DROP CONSTRAINT catalogue_function_parameters_catalogue_revision_id_target_fkey"
        ));
        assert!(migration.sql.contains(
            "DROP CONSTRAINT catalogue_functions_catalogue_revision_id_return_target_ty_fkey"
        ));
        assert!(
            migration
                .sql
                .contains("ADD COLUMN target_type_id_fk bytea\n        GENERATED ALWAYS AS")
        );
        assert!(
            migration
                .sql
                .contains("ADD COLUMN return_target_type_id_fk bytea\n        GENERATED ALWAYS AS")
        );
        assert!(
            migration
                .sql
                .contains("FOREIGN KEY (catalogue_revision_id, target_type_id_fk)")
        );
        assert!(
            migration
                .sql
                .contains("FOREIGN KEY (catalogue_revision_id, return_target_type_id_fk)")
        );
        assert!(
            migration
                .sql
                .contains("target_type_id = decode('000000000000000000000000000000f3', 'hex')")
        );
        assert!(
            migration.sql.contains(
                "return_target_type_id = decode('000000000000000000000000000000f3', 'hex')"
            )
        );
        assert!(!migration.sql.to_ascii_lowercase().contains("plpgsql"));
        assert!(!migration.sql.contains("CREATE CONSTRAINT TRIGGER"));
        assert!(!migration.sql.contains("FOR KEY SHARE"));
    }

    #[test]
    fn executable_standard_relations_is_the_registered_version_twenty_three() {
        let migration = &MIGRATIONS[22];

        assert_eq!(migration.version, 23);
        assert_eq!(migration.name, "executable standard relations");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.standard_catalogue_functions")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.standard_catalogue_function_parameters")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.standard_function_revisions")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.standard_function_artifacts")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.standard_definition_references")
        );
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.invocation_target_authorities")
        );
        assert!(
            migration
                .sql
                .contains("INSERT INTO _orna_kernel.invocation_target_authorities")
        );
        assert!(
            migration
                .sql
                .contains("DROP CONSTRAINT invocation_audit_events_target_fk")
        );
        assert!(migration.sql.contains(
            "REFERENCES _orna_kernel.invocation_target_authorities(
        catalogue_revision_id,
        function_id
    )"
        ));
    }

    #[test]
    fn protected_invocation_audit_is_the_registered_version_twenty_two() {
        let migration = &MIGRATIONS[21];

        assert_eq!(migration.version, 22);
        assert_eq!(migration.name, "protected invocation audit");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.invocation_audit_events")
        );
        assert!(migration.sql.contains("UNIQUE (invocation_id)"));
        assert!(
            migration
                .sql
                .contains("invocation_audit_events_target_evidence_pair_check")
        );
        assert!(
            migration
                .sql
                .contains("security_audit_events_invocation_evidence_key")
        );
        assert!(
            migration
                .sql
                .contains("invocation_audit_events_security_evidence_fk")
        );
    }

    #[test]
    fn capability_audit_decisions_is_the_registered_version_twenty_four() {
        let migration = &MIGRATIONS[23];

        assert_eq!(migration.version, 24);
        assert_eq!(migration.name, "capability audit decisions");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("ALTER TABLE _orna_kernel.security_audit_events")
        );
        assert!(
            migration
                .sql
                .contains("event_kind IN ('authentication', 'execute', 'capability')")
        );
        assert!(migration.sql.contains("denial_reason LIKE 'capability:%'"));
        assert!(migration.sql.contains("event_kind = 'capability'"));
        assert!(
            migration
                .sql
                .contains("DROP CONSTRAINT security_audit_events_shape_check")
        );
    }

    #[test]
    fn durable_user_state_cells_is_the_registered_version_twenty_five() {
        let migration = &MIGRATIONS[24];

        assert_eq!(migration.version, 25);
        assert_eq!(migration.name, "durable user state cells");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.user_state_cells")
        );
        assert!(
            migration.sql.contains("PRIMARY KEY (")
                && migration.sql.contains("principal_id")
                && migration.sql.contains("root_function_id")
                && migration.sql.contains("root_state_profile")
                && migration.sql.contains("function_id")
                && migration.sql.contains("function_instance_key")
                && migration.sql.contains("state_slot_id")
        );
        assert!(migration.sql.contains("user_state_cells_identity_lengths"));
        assert!(migration.sql.contains("octet_length(value_type_id) = 16"));
        assert!(migration.sql.contains("user_state_cells_revision_check"));
        assert!(migration.sql.contains("revision > 0"));
        assert!(
            migration
                .sql
                .contains("updated_at timestamp with time zone")
        );
        assert!(
            migration
                .sql
                .contains("REVOKE ALL ON TABLE _orna_kernel.user_state_cells FROM PUBLIC")
        );
    }

    #[test]
    fn security_admin_privilege_grants_is_the_registered_version_twenty_eight() {
        let migration = &MIGRATIONS[27];

        assert_eq!(migration.version, 28);
        assert_eq!(migration.name, "security admin privilege grants");
        assert!(migration.data_step.is_none());
        assert!(
            migration
                .sql
                .contains("CREATE TABLE _orna_kernel.security_privilege_grants")
        );
        assert!(
            migration
                .sql
                .contains("PRIMARY KEY (grantee_id, privilege_class, object_id)")
                && migration.sql.contains("grantee_id bytea NOT NULL")
                && migration.sql.contains("privilege_class text NOT NULL")
                && migration.sql.contains("object_id bytea NOT NULL")
        );
        // The class-wide sentinel keeps the composite key total: PostgreSQL
        // treats NULLs as distinct in unique keys, so a nullable object_id
        // would admit duplicate class-wide grants.
        assert!(
            migration
                .sql
                .contains("object_id = '' OR octet_length(object_id) = 16")
        );
        assert!(
            migration
                .sql
                .contains("privilege_class IN ('execute', 'security_admin')")
        );
        assert!(migration.sql.contains("'inspect:own-invocation'"));
        assert!(migration.sql.contains("'inspect:runtime-internals'"));
        assert!(
            migration
                .sql
                .contains("REFERENCES _orna_kernel.security_principals(id)")
        );
        assert!(
            migration
                .sql
                .contains("REVOKE ALL ON TABLE _orna_kernel.security_privilege_grants FROM PUBLIC")
        );

        // The audit extension admits the closed security_admin kind and both
        // allowed/denied shape rows.
        assert!(
            migration.sql.contains(
                "event_kind IN (\n            'authentication',\n            'execute',\n            'capability',\n            'user_state',\n            'inspect',\n            'security_admin'\n        )"
            )
        );
        assert!(
            migration
                .sql
                .contains("denial_reason LIKE 'security_admin:%'")
        );
        assert!(migration.sql.contains("event_kind = 'security_admin'"));
        assert!(
            migration
                .sql
                .contains("denial_reason NOT LIKE '%:missing-privilege'")
        );
        assert!(
            migration
                .sql
                .contains("denial_reason LIKE 'security_admin:%:missing-privilege'")
        );
        assert!(
            migration
                .sql
                .contains("DROP CONSTRAINT security_audit_events_shape_check")
        );
        assert!(
            migration
                .sql
                .contains("DROP CONSTRAINT security_audit_events_kind_check")
        );
        assert!(
            migration
                .sql
                .contains("DROP CONSTRAINT security_audit_events_denial_reason_check")
        );
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

    #[tokio::test]
    #[ignore = "requires the Compose PostgreSQL development service"]
    async fn bootstrap_rejects_tampered_current_catalogue_hash_without_mutation() {
        let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
            .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
        let kernel = PostgresKernel::from_str(&connection_string).expect("config parses");
        let active = kernel
            .bootstrap()
            .await
            .expect("initial bootstrap succeeds");
        let catalogue_id = active.catalogue().to_bytes().to_vec();

        let session = kernel.open().await.expect("snapshot session opens");
        let active_before = session
            .client
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await
            .expect("active pointer is readable");
        let active_before = (
            active_before.get::<_, Vec<u8>>(0),
            active_before.get::<_, Vec<u8>>(1),
        );
        let migrations_before: Vec<(i64, Vec<u8>)> = session
            .client
            .query(
                "SELECT version, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
                &[],
            )
            .await
            .expect("migration state is readable")
            .into_iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Vec<u8>>(1)))
            .collect();
        let original_hash: Vec<u8> = session
            .client
            .query_one(
                "SELECT content_hash
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&catalogue_id],
            )
            .await
            .expect("active catalogue hash is readable")
            .get(0);
        let updated = session
            .client
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = decode(repeat('00', 32), 'hex')
                 WHERE id = $1",
                &[&catalogue_id],
            )
            .await
            .expect("catalogue hash tamper succeeds");
        assert_eq!(updated, 1);
        session.shutdown().await.expect("tamper session shuts down");

        assert!(
            kernel.bootstrap().await.is_err(),
            "bootstrap must fail closed when the current catalogue hash is tampered"
        );

        let verification = kernel.open().await.expect("verification session opens");
        let active_after = verification
            .client
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await
            .expect("active pointer remains readable");
        let active_after = (
            active_after.get::<_, Vec<u8>>(0),
            active_after.get::<_, Vec<u8>>(1),
        );
        let migrations_after: Vec<(i64, Vec<u8>)> = verification
            .client
            .query(
                "SELECT version, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
                &[],
            )
            .await
            .expect("migration state remains readable")
            .into_iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Vec<u8>>(1)))
            .collect();
        assert_eq!(active_after, active_before);
        assert_eq!(migrations_after, migrations_before);

        let restored = verification
            .client
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = $1
                 WHERE id = $2",
                &[&original_hash, &catalogue_id],
            )
            .await
            .expect("catalogue hash restoration succeeds");
        assert_eq!(restored, 1);
        verification
            .shutdown()
            .await
            .expect("verification session shuts down");
    }
}
