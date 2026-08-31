//! Versioned bootstrap migration registry and runner.

use super::*;

use orna_core::{
    physical::{PhysicalMigrationArtifact, PhysicalPlan},
    revision::RevisionPair,
};
use orna_storage::MigrationLedgerEntry;
use std::collections::HashSet;

pub(super) struct Migration {
    pub(super) version: i64,
    pub(super) name: &'static str,
    pub(super) sql: &'static str,
    pub(super) data_step: Option<MigrationDataStep>,
}

#[derive(Clone, Copy)]
pub(super) enum MigrationDataStep {
    CanonicalHashV1EmptySeed,
    BackfillApplicationMigrationLedger,
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
    Migration {
        version: 46,
        name: "application_migrations",
        sql: include_str!("../../../migrations/0046_application_migrations.sql"),
        data_step: None,
    },
    Migration {
        version: 47,
        name: "application migration ledger baseline",
        sql: include_str!("../../../migrations/0047_application_migration_ledger_baseline.sql"),
        data_step: Some(MigrationDataStep::BackfillApplicationMigrationLedger),
    },
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
const APPLICATION_MIGRATION_LEDGER_BASELINE_STEP: &[u8] =
    b"application-migration-ledger-baseline/v1";
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
    (
        43,
        [
            0x02, 0x26, 0x8f, 0x8d, 0x50, 0xe4, 0x46, 0xba, 0x5e, 0x21, 0xce, 0x24, 0x18, 0x6c,
            0xc8, 0xcc, 0x0c, 0xa7, 0x6c, 0x9b, 0x45, 0xa7, 0x48, 0xb1, 0x7b, 0x25, 0x35, 0xaf,
            0xa4, 0xce, 0x04, 0x20,
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
    // Pin the lexical mode before registry validation as well as before each
    // batch, so this transaction is deterministic even if its session starts
    // with a non-default standard_conforming_strings value.
    transaction
        .batch_execute("SET LOCAL standard_conforming_strings = on;")
        .await
        .map_err(PostgresKernelError::Database)?;
    let migrations = validated_migration_registry()?;
    let applied_count = validated_applied_migration_count(transaction, migrations).await?;

    for migration in migrations.iter().skip(applied_count) {
        // `batch_execute` parses the complete input before executing it, so a
        // SET inside a migration cannot change how later strings are lexed.
        // Pin the server's lexical mode in a separate command to match the
        // scanner's fixed PostgreSQL default.
        transaction
            .batch_execute("SET LOCAL standard_conforming_strings = on;")
            .await
            .map_err(PostgresKernelError::Database)?;
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
        if migration_sql_contains_anonymous_do_block(migration.sql) {
            return Err(PostgresKernelError::CatalogueInvariant(
                "migration registry contains an anonymous DO block",
            ));
        }
    }
    Ok(MIGRATIONS)
}

/// Returns whether SQL contains a top-level anonymous PostgreSQL `DO` statement.
///
/// The scanner follows PostgreSQL's lexical boundaries so occurrences in
/// comments, quoted literals, and quoted identifiers are not treated as
/// executable statements.
///
/// `apply_migrations` pins `standard_conforming_strings` to PostgreSQL's
/// default (`on`) in a separate command before each batch. The scanner uses
/// that fixed mode rather than interpreting SET statements in a batch, since
/// `batch_execute` parses the complete batch before executing any statement.
pub(super) fn migration_sql_contains_anonymous_do_block(sql: &str) -> bool {
    migration_sql_contains_anonymous_do_block_with_mode(sql, true)
}

#[cfg(test)]
pub(super) fn migration_sql_contains_anonymous_do_block_with_standard_conforming_strings(
    sql: &str,
    standard_conforming_strings: bool,
) -> bool {
    migration_sql_contains_anonymous_do_block_with_mode(sql, standard_conforming_strings)
}

fn migration_sql_contains_anonymous_do_block_with_mode(
    sql: &str,
    standard_conforming_strings: bool,
) -> bool {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut statement_start = true;

    while index < bytes.len() {
        index = skip_sql_trivia(bytes, index);
        if index >= bytes.len() {
            break;
        }

        if bytes[index] == b';' {
            statement_start = true;
            index += 1;
            continue;
        }

        if let Some(next) = skip_sql_literal(bytes, index, standard_conforming_strings) {
            statement_start = false;
            index = next;
            continue;
        }

        let Some((word_start, word_end)) = sql_identifier(bytes, index) else {
            statement_start = false;
            index += 1;
            continue;
        };

        if statement_start
            && sql_identifier_is(bytes, word_start, word_end, b"do")
            && do_statement_has_code(bytes, word_end, standard_conforming_strings)
        {
            return true;
        }

        statement_start = false;
        index = word_end;
    }

    false
}

fn do_statement_has_code(bytes: &[u8], do_end: usize, standard_conforming_strings: bool) -> bool {
    let mut index = skip_sql_trivia(bytes, do_end);
    if let Some((word_start, word_end)) = sql_identifier(bytes, index)
        && sql_identifier_is(bytes, word_start, word_end, b"language")
    {
        index = skip_sql_trivia(bytes, word_end);
        index = skip_sql_literal(bytes, index, standard_conforming_strings)
            .or_else(|| sql_identifier(bytes, index).map(|(_, word_end)| word_end))
            .unwrap_or(index);
        index = skip_sql_trivia(bytes, index);
    }

    is_sql_code_literal_start(bytes, index)
}

fn is_sql_code_literal_start(bytes: &[u8], index: usize) -> bool {
    if index >= bytes.len() {
        return false;
    }
    matches!(bytes[index], b'\'' | b'e' | b'E' | b'u' | b'U' | b'$')
        && (bytes[index] == b'\''
            || (matches!(bytes[index], b'e' | b'E') && bytes.get(index + 1) == Some(&b'\''))
            || (matches!(bytes[index], b'u' | b'U')
                && bytes.get(index + 1) == Some(&b'&')
                && bytes.get(index + 2) == Some(&b'\''))
            || (bytes[index] == b'$' && dollar_quote_end(bytes, index).is_some()))
}

fn sql_identifier(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    if !is_sql_identifier_start(bytes.get(index).copied()?) {
        return None;
    }

    let mut end = index + 1;
    while end < bytes.len() && is_sql_identifier_continue(bytes[end]) {
        end += 1;
    }
    Some((index, end))
}

fn sql_identifier_is(bytes: &[u8], start: usize, end: usize, expected: &[u8]) -> bool {
    end - start == expected.len()
        && bytes[start..end]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
}

fn is_sql_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_sql_identifier_continue(byte: u8) -> bool {
    is_sql_identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

fn skip_sql_trivia(bytes: &[u8], mut index: usize) -> usize {
    loop {
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            index += 1;
        }

        if bytes.get(index..index + 2) == Some(b"--") {
            index += 2;
            while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                index += 1;
            }
            continue;
        }

        if bytes.get(index..index + 2) != Some(b"/*") {
            return index;
        }

        let mut depth = 1;
        index += 2;
        while index < bytes.len() && depth > 0 {
            if bytes.get(index..index + 2) == Some(b"/*") {
                depth += 1;
                index += 2;
            } else if bytes.get(index..index + 2) == Some(b"*/") {
                depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
        }
    }
}

fn skip_sql_literal(
    bytes: &[u8],
    index: usize,
    standard_conforming_strings: bool,
) -> Option<usize> {
    if bytes.get(index) == Some(&b'\'') {
        return Some(skip_quoted_sql_literal(
            bytes,
            index,
            b'\'',
            !standard_conforming_strings,
        ));
    }
    if bytes.get(index) == Some(&b'"') {
        return Some(skip_quoted_sql_literal(bytes, index, b'"', false));
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) && bytes.get(index + 1) == Some(&b'\'') {
        return Some(skip_quoted_sql_literal(bytes, index + 1, b'\'', true));
    }
    if matches!(bytes.get(index), Some(b'u' | b'U'))
        && bytes.get(index + 1) == Some(&b'&')
        && matches!(bytes.get(index + 2), Some(b'\'' | b'"'))
    {
        // U& escapes encode code points; neither the default backslash nor
        // an alternate UESCAPE character can escape a quote delimiter.
        return Some(skip_unicode_quoted_sql_literal(bytes, index + 2));
    }
    if bytes.get(index) == Some(&b'$') {
        return dollar_quote_end(bytes, index);
    }
    None
}

fn skip_quoted_sql_literal(
    bytes: &[u8],
    quote_start: usize,
    quote: u8,
    backslash_escapes: bool,
) -> usize {
    let mut index = quote_start + 1;
    while index < bytes.len() {
        if backslash_escapes && bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_unicode_quoted_sql_literal(bytes: &[u8], quote_start: usize) -> usize {
    let quote = bytes[quote_start];
    let mut index = quote_start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn dollar_quote_end(bytes: &[u8], start: usize) -> Option<usize> {
    let delimiter_end = dollar_quote_delimiter_end(bytes, start)?;
    let delimiter = &bytes[start..delimiter_end];
    let mut index = delimiter_end;
    while index <= bytes.len().saturating_sub(delimiter.len()) {
        if bytes[index..index + delimiter.len()] == *delimiter {
            return Some(index + delimiter.len());
        }
        index += 1;
    }
    Some(bytes.len())
}

fn dollar_quote_delimiter_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'$') {
        return None;
    }

    let mut index = start + 1;
    if bytes.get(index) == Some(&b'$') {
        return Some(index + 1);
    }
    if !is_sql_identifier_start(bytes.get(index).copied()?) {
        return None;
    }
    index += 1;
    while index < bytes.len() && (is_sql_identifier_continue(bytes[index]) && bytes[index] != b'$')
    {
        index += 1;
    }
    (bytes.get(index) == Some(&b'$')).then_some(index + 1)
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
            let expected_index =
                start
                    .checked_add(offset)
                    .ok_or(PostgresKernelError::CatalogueInvariant(
                        "legacy revision path index exceeds platform limits",
                    ))?;
            let expected_base =
                reverse_path
                    .get(expected_index)
                    .ok_or(PostgresKernelError::CatalogueInvariant(
                        "existing migration ledger starts beyond legacy revision ancestry",
                    ))?;
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

fn exact_id_bytes(bytes: Vec<u8>, message: &'static str) -> Result<[u8; 16], PostgresKernelError> {
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::CatalogueInvariant(message))
}

fn optional_id_bytes(
    bytes: Option<Vec<u8>>,
    message: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    bytes
        .map(|bytes| exact_id_bytes(bytes, message))
        .transpose()
}
