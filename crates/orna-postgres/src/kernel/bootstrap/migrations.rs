//! Versioned bootstrap migration registry and runner.

use super::*;

pub(super) struct Migration {
    pub(super) version: i64,
    pub(super) name: &'static str,
    pub(super) sql: &'static str,
    pub(super) data_step: Option<MigrationDataStep>,
}

#[derive(Clone, Copy)]
pub(super) enum MigrationDataStep {
    CanonicalHashV1EmptySeed,
}

pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "private kernel catalogue",
        sql: include_str!("../../../migrations/0001_kernel.sql"),
        data_step: None,
    },
    Migration {
        version: 2,
        name: "revision catalogue integrity",
        sql: include_str!("../../../migrations/0002_revisions.sql"),
        data_step: None,
    },
    Migration {
        version: 3,
        name: "definition reference integrity",
        sql: include_str!("../../../migrations/0003_reference_integrity.sql"),
        data_step: None,
    },
    Migration {
        version: 4,
        name: "canonical hash contract v1",
        sql: include_str!("../../../migrations/0004_canonical_hash_contract.sql"),
        data_step: Some(MigrationDataStep::CanonicalHashV1EmptySeed),
    },
    Migration {
        version: 5,
        name: "owner-qualified reference targets",
        sql: include_str!("../../../migrations/0005_owner_qualified_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 6,
        name: "definition reference write evidence",
        sql: include_str!("../../../migrations/0006_write_reference_evidence.sql"),
        data_step: None,
    },
    Migration {
        version: 7,
        name: "standard catalogue type storage",
        sql: include_str!("../../../migrations/0007_catalogue_types.sql"),
        data_step: None,
    },
    Migration {
        version: 8,
        name: "resolved value type storage",
        sql: include_str!("../../../migrations/0008_resolved_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 9,
        name: "security decision snapshot",
        sql: include_str!("../../../migrations/0009_security_snapshot.sql"),
        data_step: None,
    },
    Migration {
        version: 10,
        name: "local peer credentials",
        sql: include_str!("../../../migrations/0010_local_peer_credentials.sql"),
        data_step: None,
    },
    Migration {
        version: 11,
        name: "protected security audit",
        sql: include_str!("../../../migrations/0011_security_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 12,
        name: "catalogue enum type storage",
        sql: include_str!("../../../migrations/0012_catalogue_enum_types.sql"),
        data_step: None,
    },
    Migration {
        version: 13,
        name: "resolved enum type storage",
        sql: include_str!("../../../migrations/0013_resolved_enum_types.sql"),
        data_step: None,
    },
    Migration {
        version: 14,
        name: "catalogue enum reference targets",
        sql: include_str!("../../../migrations/0014_enum_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 15,
        name: "catalogue record value storage",
        sql: include_str!("../../../migrations/0015_catalogue_record_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 16,
        name: "resolved record value type storage",
        sql: include_str!("../../../migrations/0016_resolved_record_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 17,
        name: "record value field reference targets",
        sql: include_str!("../../../migrations/0017_record_field_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 18,
        name: "disjoint field reference targets",
        sql: include_str!("../../../migrations/0018_disjoint_field_reference_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 19,
        name: "standard opaque value storage",
        sql: include_str!("../../../migrations/0019_standard_opaque_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 20,
        name: "standard enum record field storage",
        sql: include_str!("../../../migrations/0020_standard_enum_record_fields.sql"),
        data_step: None,
    },
    Migration {
        version: 21,
        name: "nested record field targets",
        sql: include_str!("../../../migrations/0021_nested_record_field_targets.sql"),
        data_step: None,
    },
    Migration {
        version: 22,
        name: "protected invocation audit",
        sql: include_str!("../../../migrations/0022_invocation_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 23,
        name: "executable standard relations",
        sql: include_str!("../../../migrations/0023_executable_standard_snapshots.sql"),
        data_step: None,
    },
    Migration {
        version: 24,
        name: "capability audit decisions",
        sql: include_str!("../../../migrations/0024_capability_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 25,
        name: "durable user state cells",
        sql: include_str!("../../../migrations/0025_user_state_cells.sql"),
        data_step: None,
    },
    Migration {
        version: 26,
        name: "user state audit decisions",
        sql: include_str!("../../../migrations/0026_user_state_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 27,
        name: "inspect snapshots and trace",
        sql: include_str!("../../../migrations/0027_inspect_snapshots.sql"),
        data_step: None,
    },
    Migration {
        version: 28,
        name: "security admin privilege grants",
        sql: include_str!("../../../migrations/0028_security_admin.sql"),
        data_step: None,
    },
    Migration {
        version: 29,
        name: "sealed system invocation authorities",
        sql: include_str!("../../../migrations/0029_sealed_system_invocation_authorities.sql"),
        data_step: None,
    },
    Migration {
        version: 30,
        name: "active roles system invocation authority",
        sql: include_str!("../../../migrations/0030_active_roles_system_invocation_authority.sql"),
        data_step: None,
    },
    Migration {
        version: 31,
        name: "standard JSON executable format",
        sql: include_str!("../../../migrations/0031_standard_json_executable_format.sql"),
        data_step: None,
    },
    Migration {
        version: 32,
        name: "protected resource audit",
        sql: include_str!("../../../migrations/0032_resource_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 33,
        name: "stream function returns",
        sql: include_str!("../../../migrations/0033_stream_function_returns.sql"),
        data_step: None,
    },
    Migration {
        version: 34,
        name: "resource request identity history",
        sql: include_str!("../../../migrations/0034_resource_request_history.sql"),
        data_step: None,
    },
    Migration {
        version: 35,
        name: "resource audit target authorities",
        sql: include_str!("../../../migrations/0035_resource_audit_target_authority.sql"),
        data_step: None,
    },
    Migration {
        version: 36,
        name: "sealed Inspector value types",
        sql: include_str!("../../../migrations/0036_sealed_inspect_value_types.sql"),
        data_step: None,
    },
    Migration {
        version: 37,
        name: "source apply audit",
        sql: include_str!("../../../migrations/0037_source_apply_audit.sql"),
        data_step: None,
    },
    Migration {
        version: 38,
        name: "source apply principal binding",
        sql: include_str!("../../../migrations/0038_source_apply_principal.sql"),
        data_step: None,
    },
    Migration {
        version: 39,
        name: "sealed invocation SECURITY DEFINER denial audit",
        sql: include_str!("../../../migrations/0039_security_definer_audit_reason.sql"),
        data_step: None,
    },
    Migration {
        version: 40,
        name: "security admin class-wide grant boundary",
        sql: include_str!("../../../migrations/0040_security_admin_class_wide.sql"),
        data_step: None,
    },
    Migration {
        version: 41,
        name: "nullable resource audit nested invocation",
        sql: include_str!("../../../migrations/0041_nullable_resource_audit_nested_invocation.sql"),
        data_step: None,
    },
    Migration {
        version: 42,
        name: "non-empty security principal identities",
        sql: include_str!("../../../migrations/0042_security_principal_non_empty.sql"),
        data_step: None,
    },
    Migration {
        version: 43,
        name: "source bundle unit memberships",
        sql: include_str!("../../../migrations/0043_source_bundle_units.sql"),
        data_step: None,
    },
    Migration {
        version: 44,
        name: "standard table and CSV executable formats",
        sql: include_str!("../../../migrations/0044_standard_presenter_executable_formats.sql"),
        data_step: None,
    },
    Migration {
        version: 45,
        name: "inspect snapshot observer context",
        sql: include_str!("../../../migrations/0045_inspect_snapshot_observer_context.sql"),
        data_step: None,
    },
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
pub(super) const MIGRATION_REGISTRY_SQL: &str = "
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
pub(super) const MIGRATION_LOCK_NAMESPACE: i32 = 0x4f52_4e41;
pub(super) const MIGRATION_LOCK_KEY: i32 = 1;

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

pub(super) fn legacy_migration_checksum(version: i64) -> Option<&'static [u8; 32]> {
    LEGACY_MIGRATION_CHECKSUMS
        .iter()
        .find(|(legacy_version, _)| *legacy_version == version)
        .map(|(_, checksum)| checksum)
}

pub(super) fn migration_checksum_matches(migration: &Migration, applied_checksum: &[u8]) -> bool {
    let expected_checksum = migration_checksum(migration);
    applied_checksum == expected_checksum
        || legacy_migration_checksum(migration.version)
            .is_some_and(|legacy_checksum| applied_checksum == legacy_checksum)
}

pub(super) async fn apply_migrations(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
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

pub(super) fn validated_migration_registry() -> Result<&'static [Migration], PostgresKernelError> {
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

pub(super) fn migration_checksum(migration: &Migration) -> Vec<u8> {
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
