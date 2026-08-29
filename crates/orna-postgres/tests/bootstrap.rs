mod support;

use std::{collections::BTreeSet, str::FromStr, sync::Arc};

use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, ParameterId, SchemaId,
    SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, function_declaration_digest,
        function_semantic_digest, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, RevisionPair, SourceOrigin,
        StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use sha2::{Digest, Sha256};
use support::{TestDatabase, TestResult, failure, with_test_database};
use tokio_postgres::{Client, Row};

const EXPECTED_KERNEL_TABLES: &[&str] = &[
    "active_revision",
    "application_migrations",
    "catalogue_enum_types",
    "catalogue_expressions",
    "catalogue_fields",
    "catalogue_function_parameters",
    "catalogue_function_return_columns",
    "catalogue_functions",
    "catalogue_object_types",
    "catalogue_record_value_fields",
    "catalogue_record_value_types",
    "catalogue_revisions",
    "catalogue_schemas",
    "definition_references",
    "function_artifacts",
    "function_revisions",
    "inspect_snapshots",
    "inspect_trace_events",
    "invocation_audit_events",
    "invocation_target_authorities",
    "resource_audit_events",
    "resource_request_history",
    "schema_migrations",
    "security_audit_events",
    "security_execute_grants",
    "security_local_peer_credentials",
    "security_principals",
    "security_privilege_grants",
    "security_role_memberships",
    "source_bundle_units",
    "source_bundles",
    "source_revisions",
    "source_units",
    "standard_catalogue_enum_types",
    "standard_catalogue_function_parameters",
    "standard_catalogue_functions",
    "standard_catalogue_schemas",
    "standard_catalogue_type_bindings",
    "standard_catalogue_value_types",
    "standard_definition_references",
    "standard_function_artifacts",
    "standard_function_revisions",
    "standard_library_revisions",
    "user_state_cells",
];

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "private kernel catalogue",
        include_str!("../migrations/0001_kernel.sql"),
    ),
    (
        2,
        "revision catalogue integrity",
        include_str!("../migrations/0002_revisions.sql"),
    ),
    (
        3,
        "definition reference integrity",
        include_str!("../migrations/0003_reference_integrity.sql"),
    ),
    (
        4,
        "canonical hash contract v1",
        include_str!("../migrations/0004_canonical_hash_contract.sql"),
    ),
    (
        5,
        "owner-qualified reference targets",
        include_str!("../migrations/0005_owner_qualified_reference_targets.sql"),
    ),
    (
        6,
        "definition reference write evidence",
        include_str!("../migrations/0006_write_reference_evidence.sql"),
    ),
    (
        7,
        "standard catalogue type storage",
        include_str!("../migrations/0007_catalogue_types.sql"),
    ),
    (
        8,
        "resolved value type storage",
        include_str!("../migrations/0008_resolved_value_types.sql"),
    ),
    (
        9,
        "security decision snapshot",
        include_str!("../migrations/0009_security_snapshot.sql"),
    ),
    (
        10,
        "local peer credentials",
        include_str!("../migrations/0010_local_peer_credentials.sql"),
    ),
    (
        11,
        "protected security audit",
        include_str!("../migrations/0011_security_audit.sql"),
    ),
    (
        12,
        "catalogue enum type storage",
        include_str!("../migrations/0012_catalogue_enum_types.sql"),
    ),
    (
        13,
        "resolved enum type storage",
        include_str!("../migrations/0013_resolved_enum_types.sql"),
    ),
    (
        14,
        "catalogue enum reference targets",
        include_str!("../migrations/0014_enum_reference_targets.sql"),
    ),
    (
        15,
        "catalogue record value storage",
        include_str!("../migrations/0015_catalogue_record_value_types.sql"),
    ),
    (
        16,
        "resolved record value type storage",
        include_str!("../migrations/0016_resolved_record_value_types.sql"),
    ),
    (
        17,
        "record value field reference targets",
        include_str!("../migrations/0017_record_field_reference_targets.sql"),
    ),
    (
        18,
        "disjoint field reference targets",
        include_str!("../migrations/0018_disjoint_field_reference_targets.sql"),
    ),
    (
        19,
        "standard opaque value storage",
        include_str!("../migrations/0019_standard_opaque_value_types.sql"),
    ),
    (
        20,
        "standard enum record field storage",
        include_str!("../migrations/0020_standard_enum_record_fields.sql"),
    ),
    (
        21,
        "nested record field targets",
        include_str!("../migrations/0021_nested_record_field_targets.sql"),
    ),
    (
        22,
        "protected invocation audit",
        include_str!("../migrations/0022_invocation_audit.sql"),
    ),
    (
        23,
        "executable standard relations",
        include_str!("../migrations/0023_executable_standard_snapshots.sql"),
    ),
    (
        24,
        "capability audit decisions",
        include_str!("../migrations/0024_capability_audit.sql"),
    ),
    (
        25,
        "durable user state cells",
        include_str!("../migrations/0025_user_state_cells.sql"),
    ),
    (
        26,
        "user state audit decisions",
        include_str!("../migrations/0026_user_state_audit.sql"),
    ),
    (
        27,
        "inspect snapshots and trace",
        include_str!("../migrations/0027_inspect_snapshots.sql"),
    ),
    (
        28,
        "security admin privilege grants",
        include_str!("../migrations/0028_security_admin.sql"),
    ),
    (
        29,
        "sealed system invocation authorities",
        include_str!("../migrations/0029_sealed_system_invocation_authorities.sql"),
    ),
    (
        30,
        "active roles system invocation authority",
        include_str!("../migrations/0030_active_roles_system_invocation_authority.sql"),
    ),
    (
        31,
        "standard JSON executable format",
        include_str!("../migrations/0031_standard_json_executable_format.sql"),
    ),
    (
        32,
        "protected resource audit",
        include_str!("../migrations/0032_resource_audit.sql"),
    ),
    (
        33,
        "stream function returns",
        include_str!("../migrations/0033_stream_function_returns.sql"),
    ),
    (
        34,
        "resource request identity history",
        include_str!("../migrations/0034_resource_request_history.sql"),
    ),
    (
        35,
        "resource audit target authorities",
        include_str!("../migrations/0035_resource_audit_target_authority.sql"),
    ),
    (
        36,
        "sealed Inspector value types",
        include_str!("../migrations/0036_sealed_inspect_value_types.sql"),
    ),
    (
        37,
        "source apply audit",
        include_str!("../migrations/0037_source_apply_audit.sql"),
    ),
    (
        38,
        "source apply principal binding",
        include_str!("../migrations/0038_source_apply_principal.sql"),
    ),
    (
        39,
        "sealed invocation SECURITY DEFINER denial audit",
        include_str!("../migrations/0039_security_definer_audit_reason.sql"),
    ),
    (
        40,
        "security admin class-wide grant boundary",
        include_str!("../migrations/0040_security_admin_class_wide.sql"),
    ),
    (
        41,
        "nullable resource audit nested invocation",
        include_str!("../migrations/0041_nullable_resource_audit_nested_invocation.sql"),
    ),
    (
        42,
        "non-empty security principal identities",
        include_str!("../migrations/0042_security_principal_non_empty.sql"),
    ),
    (
        43,
        "source bundle unit memberships",
        include_str!("../migrations/0043_source_bundle_units.sql"),
    ),
    (
        44,
        "standard table and CSV executable formats",
        include_str!("../migrations/0044_standard_presenter_executable_formats.sql"),
    ),
    (
        45,
        "inspect snapshot observer context",
        include_str!("../migrations/0045_inspect_snapshot_observer_context.sql"),
    ),
    (
        46,
        "application_migrations",
        include_str!("../migrations/0046_application_migrations.sql"),
    ),
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
const HASH_CONTRACT_TABLES: &[&str] = &[
    "source_units",
    "source_bundles",
    "source_revisions",
    "catalogue_revisions",
    "catalogue_expressions",
    "function_revisions",
    "function_artifacts",
];
const ORIGIN_TABLES: &[&str] = &[
    "catalogue_schemas",
    "catalogue_enum_types",
    "catalogue_object_types",
    "catalogue_fields",
    "catalogue_record_value_fields",
    "catalogue_record_value_types",
    "standard_catalogue_enum_types",
    "catalogue_expressions",
    "catalogue_functions",
    "catalogue_function_parameters",
    "catalogue_function_return_columns",
];
const REGISTERED_V4_SCHEMA_DECLARATION: &str = "schema_decl";
const REGISTERED_V4_FIRST_TYPE_DECLARATION: &str = "first_type_decl";
const REGISTERED_V4_FIELD_DECLARATION: &str = "field_decl";
const REGISTERED_V4_SECOND_TYPE_DECLARATION: &str = "second_type_decl";
const REGISTERED_V4_FIRST_FUNCTION_DECLARATION: &str = "first_function_decl";
const REGISTERED_V4_PARAMETER_DECLARATION: &str = "parameter_decl";
const REGISTERED_V4_FIELD_REFERENCE: &str = "field_reference";
const REGISTERED_V4_PARAMETER_REFERENCE: &str = "parameter_reference";
const REGISTERED_V4_SECOND_FUNCTION_DECLARATION: &str = "second_function_decl";
const REGISTERED_V4_SOURCE: &str = concat!(
    "schema_decl\n",
    "first_type_decl\n",
    "field_decl\n",
    "second_type_decl\n",
    "first_function_decl\n",
    "parameter_decl\n",
    "field_reference\n",
    "parameter_reference\n",
    "second_function_decl\n",
);

#[derive(Debug, Eq, PartialEq)]
struct DefinitionReferenceSnapshot {
    catalogue_revision_id: Vec<u8>,
    source_function_id: Vec<u8>,
    source_function_revision_id: Vec<u8>,
    ordinal: i64,
    target_definition_id: Vec<u8>,
    target_kind: String,
    reference_kind: String,
    source_subobject_id: Option<Vec<u8>>,
    source_unit_id: Vec<u8>,
    source_start: i64,
    source_end: i64,
    target_owner_type_id: Option<Vec<u8>>,
    target_owner_function_id: Option<Vec<u8>>,
    xmin: String,
}

#[derive(Debug, Eq, PartialEq)]
struct UpgradeSnapshot {
    active_pair: (Vec<u8>, Vec<u8>),
    source_unit_count: i64,
    migrations: Vec<(i64, String, Vec<u8>)>,
    references: Vec<DefinitionReferenceSnapshot>,
    catalogue_hashes: Vec<(Vec<u8>, Vec<u8>)>,
    function_hashes: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Debug, Eq, PartialEq)]
struct CatalogueSurfaceSnapshot {
    relations_and_indexes: Vec<(String, String, String)>,
    triggers: Vec<(String, String, String, bool)>,
    relation_acls: Vec<(String, String, String)>,
    schema_acls: Vec<(String, String)>,
}

fn is_later_catalogue_relation(relation: &str) -> bool {
    relation.starts_with("security_")
        || relation.starts_with("catalogue_enum_types")
        || relation.starts_with("catalogue_record_value")
        || relation.starts_with("standard_catalogue_enum_types")
        || relation.starts_with("std_cat_enum_types_")
        || relation == "definition_references_enum_type_target_index"
        || relation.starts_with("definition_references_record_")
        || relation.starts_with("invocation_")
        || relation.starts_with("resource_audit_")
        || relation.starts_with("resource_request_")
        || relation.starts_with("inspect_")
        || relation.starts_with("standard_function_")
        || relation.starts_with("standard_catalogue_function")
        || relation.starts_with("standard_definition_references")
        || relation.starts_with("std_cat_fn_")
        || relation.starts_with("std_cat_functions_")
        || relation.starts_with("std_def_")
        || relation.starts_with("std_fn_")
        || relation.starts_with("user_state_cells")
        || relation.starts_with("source_bundle_units")
}

fn is_later_catalogue_trigger(trigger: &str) -> bool {
    matches!(
        trigger,
        "catalogue_function_parameters_catalogue_revision_id_target_fkey"
            | "catalogue_functions_catalogue_revision_id_return_target_ty_fkey"
            | "catalogue_object_types_function_target_fkey"
    )
}

fn without_later_relations(snapshot: &CatalogueSurfaceSnapshot) -> CatalogueSurfaceSnapshot {
    CatalogueSurfaceSnapshot {
        relations_and_indexes: snapshot
            .relations_and_indexes
            .iter()
            .filter(|(_, relation, _)| !is_later_catalogue_relation(relation))
            .cloned()
            .collect(),
        triggers: snapshot
            .triggers
            .iter()
            .filter(|(_, relation, trigger, _)| {
                !is_later_catalogue_relation(relation) && !is_later_catalogue_trigger(trigger)
            })
            .cloned()
            .collect(),
        relation_acls: snapshot
            .relation_acls
            .iter()
            .filter(|(_, relation, _)| !is_later_catalogue_relation(relation))
            .cloned()
            .collect(),
        schema_acls: snapshot.schema_acls.clone(),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct TargetForeignKeySnapshot(Vec<(String, String, String, bool, bool)>);

#[test]
fn registered_v4_semantic_fixture_is_a_valid_active_database_revision() -> TestResult<()> {
    let fixture = registered_v4_semantic_fixture()?;

    require(
        fixture.catalogue().object_types().len() == 2
            && fixture.catalogue().functions().len() == 2
            && fixture.function_revisions().len() == 2
            && fixture.references().len() == 2,
        "registered v4 fixture lost required semantic rows",
    )
}

#[test]
fn supported_reference_kind_sql_maps_every_legacy_fixture_kind() -> TestResult<()> {
    assert_eq!(
        SUPPORTED_REFERENCE_KINDS,
        &[
            (DefinitionReferenceKind::FunctionCall, "function_call"),
            (DefinitionReferenceKind::NamedType, "named_type"),
            (DefinitionReferenceKind::ObjectReference, "object_reference"),
            (DefinitionReferenceKind::ParameterRead, "parameter_read"),
            (DefinitionReferenceKind::QueryObject, "query_object"),
            (DefinitionReferenceKind::QueryField, "query_field"),
            (DefinitionReferenceKind::Expression, "expression"),
        ]
    );
    for (kind, expected) in SUPPORTED_REFERENCE_KINDS {
        assert_eq!(supported_reference_kind_sql(*kind)?, *expected);
    }
    Ok(())
}

#[test]
fn legacy_migration_epoch_is_order_contiguous() -> TestResult<()> {
    require(
        MIGRATIONS.len() == 46,
        format!(
            "migration registry has {} entries; expected 46",
            MIGRATIONS.len()
        ),
    )?;
    for (index, (version, _, _)) in MIGRATIONS[..45].iter().enumerate() {
        require(
            *version == (index + 1) as i64,
            format!(
                "legacy migration at index {index} is version {version}; expected {}",
                index + 1
            ),
        )?;
    }
    Ok(())
}

#[test]
fn registered_migration_sql_has_no_procedural_language_dependency() -> TestResult<()> {
    require(!MIGRATIONS.is_empty(), "migration registry is empty")?;

    for (version, name, sql) in MIGRATIONS {
        // Migration 23 (executable standard relations, ADR 0055) contains one
        // accepted `DO` block that validates the invocation-target authority
        // backfill inside the append-only migration. The guardrail that
        // remains meaningful is that no migration declares an explicit
        // procedural-language dependency (CREATE EXTENSION plpgsql or
        // LANGUAGE plpgsql); the default `DO $$ ... $$` block relies only on
        // PostgreSQL's built-in default language and ran successfully in the
        // embedded engine.
        require(
            !sql.to_ascii_lowercase().contains("plpgsql"),
            format!("migration {version} ({name}) depends on PL/pgSQL"),
        )?;
    }

    Ok(())
}

#[test]
fn write_reference_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(6, MIGRATIONS[5].2)),
        "e831811c0f42d6f4b3ab2601cf480fabaaed03b5547e2615400b9eec4b6b53bf"
    );
}

#[test]
fn standard_catalogue_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(7, MIGRATIONS[6].2)),
        "da58e39fb08edf1c214f6c041c792adb1446a6acb2939560d9091759a218c90f"
    );
}

#[test]
fn resolved_value_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(8, MIGRATIONS[7].2)),
        "2ef8d844814dafd7d70d40fb39ce7e5e6c52dea3cfc668e84c74c2c5c1dd06e7"
    );
}

#[test]
fn security_snapshot_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(9, MIGRATIONS[8].2)),
        "101413b9478a975b08099cda32bd26e4c41ad0bc00b8c473c5ca281a7e2690ef"
    );
}

#[test]
fn local_peer_credential_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(10, MIGRATIONS[9].2)),
        "0c6d158eb85209c8d0413e3871c5f56840936026f4f80d1325c079d3723e9099"
    );
}

#[test]
fn source_apply_audit_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(37, MIGRATIONS[36].2)),
        "23afc307eefed842ea24b0eab50d21a8108f20983da24454792fe4fc44e2d66b"
    );
}

#[test]
fn source_apply_principal_binding_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(38, MIGRATIONS[37].2)),
        "ada9f1e5b7080ab8955484a3e2c602ba1966f344f6577bf2a48bbc7e444d7179"
    );
}

#[test]
fn protected_security_audit_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(11, MIGRATIONS[10].2)),
        "54288defeebde1621805eed6ac0b2653669a658938e6c707f0665d430d639575"
    );
}

#[test]
fn catalogue_enum_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(12, MIGRATIONS[11].2)),
        "87635d3052423176b969ce860e0c3e0fec665199259c14c1dbf5a0e3e385d3ff"
    );
}

#[test]
fn resolved_enum_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(13, MIGRATIONS[12].2)),
        "850a85e034cc7548c4d70f35763356492af4d2c227506bb79aca0c346b4a3f75"
    );
}

#[test]
fn enum_reference_target_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(14, MIGRATIONS[13].2)),
        "c130918d3a24a386d78c61cae41775df3b57f5a0b070afac19b9fb143088e38d"
    );
}

#[test]
fn catalogue_record_value_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(15, MIGRATIONS[14].2)),
        "31891de1fe93086185d54aa8d995bb5f1f569c8906596e16a007c13ef48385a3"
    );
}

#[test]
fn security_snapshot_migration_is_the_registered_version_nine() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[8];

    require(
        version == 9,
        format!("security snapshot migration is version {version}"),
    )?;
    require(
        name == "security decision snapshot",
        format!("security snapshot migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_principals"),
        "security migration does not create the principal table",
    )
}

#[test]
fn local_peer_credential_migration_is_the_registered_version_ten() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[9];

    require(
        version == 10,
        format!("local peer credential migration is version {version}"),
    )?;
    require(
        name == "local peer credentials",
        format!("local peer credential migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_local_peer_credentials"),
        "local peer credential migration does not create its protected table",
    )
}

#[test]
fn source_apply_audit_is_the_registered_version_thirty_seven() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[36];

    require(
        version == 37,
        format!("source apply audit migration is version {version}"),
    )?;
    require(
        name == "source apply audit",
        format!("source apply audit migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("event_kind = 'source_apply'")
            && sql.contains("denial_reason = 'source_apply:committed'")
            && sql.contains("source_revision_id IS NOT NULL")
            && sql.contains("catalogue_revision_id IS NOT NULL"),
        "source apply audit migration does not constrain the committed candidate shape",
    )
}

#[test]
fn source_apply_principal_binding_is_the_registered_version_thirty_eight() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[37];

    require(
        version == 38,
        format!("source apply principal binding migration is version {version}"),
    )?;
    require(
        name == "source apply principal binding",
        format!("source apply principal binding migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("event_kind <> 'source_apply'")
            && sql.contains(
                "session_principal_id = decode('00000000000000000000000000000001', 'hex')",
            ),
        "source apply principal binding migration does not bind the fixed service principal",
    )
}

#[test]
fn protected_security_audit_is_the_registered_version_eleven() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[10];

    require(
        version == 11,
        format!("last migration is version {version}"),
    )?;
    require(
        name == "protected security audit",
        format!("last migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_audit_events"),
        "protected security audit migration does not create its table",
    )
}

#[test]
fn catalogue_enum_type_storage_is_the_registered_version_twelve() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[11];

    require(
        version == 12,
        format!("catalogue enum migration is version {version}"),
    )?;
    require(
        name == "catalogue enum type storage",
        format!("catalogue enum migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.catalogue_enum_types")
            && sql.contains("labels text[] NOT NULL")
            && sql.contains("cardinality(labels) > 0")
            && sql.contains("REVOKE ALL ON TABLE _orna_kernel.catalogue_enum_types FROM PUBLIC"),
        "catalogue enum migration does not preserve protected ordered label storage",
    )
}

#[test]
fn resolved_enum_type_storage_is_the_registered_version_thirteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[12];

    require(
        version == 13,
        format!("resolved enum migration is version {version}"),
    )?;
    require(
        name == "resolved enum type storage",
        format!("resolved enum migration has unexpected name {name:?}"),
    )?;
    for column in ["enum_type_id", "return_enum_type_id"] {
        require(
            sql.contains(column),
            format!("resolved enum migration omits {column}"),
        )?;
    }
    require(
        sql.matches("REFERENCES _orna_kernel.catalogue_enum_types")
            .count()
            == 4,
        "resolved enum migration does not bind every type position to the enum catalogue",
    )
}

#[test]
fn enum_reference_targets_are_the_registered_version_fourteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[13];

    require(
        version == 14,
        format!("enum reference migration is version {version}"),
    )?;
    require(
        name == "catalogue enum reference targets",
        format!("enum reference migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_enum_catalogue_revision_id")
            && sql.contains("target_kind = 'enum_type'")
            && sql.contains("REFERENCES _orna_kernel.catalogue_enum_types"),
        "enum reference migration does not bind named evidence to its catalogue enum",
    )
}

#[test]
fn record_value_storage_is_the_registered_version_fifteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[14];

    require(
        version == 15,
        format!("last migration is version {version}"),
    )?;
    require(
        name == "catalogue record value storage",
        format!("last migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.catalogue_record_value_types")
            && sql.contains("CREATE TABLE _orna_kernel.catalogue_record_value_fields")
            && sql.contains("CHECK (value_kind = 'record')")
            && sql.contains("CHECK (mutability = 'immutable')")
            && sql.contains("CHECK (persistence = 'persistable')")
            && sql.contains("CHECK (type_kind IN ('value', 'enum'))")
            && sql.contains("REFERENCES _orna_kernel.standard_catalogue_value_types")
            && sql.contains("REFERENCES _orna_kernel.catalogue_enum_types")
            && sql.contains(
                "REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_types FROM PUBLIC",
            )
            && sql.contains(
                "REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_fields FROM PUBLIC",
            ),
        "record value migration does not preserve the complete protected definition contract",
    )
}

#[test]
fn record_field_reference_targets_are_the_registered_version_seventeen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[16];
    require(version == 17, format!("migration is version {version}"))?;
    require(
        name == "record value field reference targets",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_kind = 'record_field'")
            && sql.contains("definition_references_record_field_target_fk")
            && sql.contains("REFERENCES _orna_kernel.catalogue_record_value_fields")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("LANGUAGE plpgsql"),
        "record-field reference migration does not preserve exact relational integrity",
    )
}

#[test]
fn disjoint_field_reference_targets_are_the_registered_version_eighteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[17];
    require(version == 18, format!("migration is version {version}"))?;
    require(
        name == "disjoint field reference targets",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_record_field_owner_type_id")
            && sql.contains("WHERE target_kind = 'record_field'")
            && sql.contains("definition_references_record_field_target_fk")
            && sql.contains("REFERENCES _orna_kernel.catalogue_record_value_fields")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("LANGUAGE plpgsql"),
        "disjoint field-reference migration does not preserve exact relational integrity",
    )
}

#[test]
fn standard_opaque_value_storage_is_the_registered_version_nineteen() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(18).copied() else {
        return Err(failure(
            "standard opaque value storage migration is not registered",
        ));
    };
    require(version == 19, format!("migration is version {version}"))?;
    require(
        name == "standard opaque value storage",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("value_kind IN ('primitive', 'opaque')")
            && sql.contains("value_kind <> 'opaque'")
            && sql.contains("persistence = 'transient'")
            && sql.contains("octet_length(representation_contract) <= 128")
            && sql.contains("representation_contract !~ '[^ -~]'")
            && !sql.contains("CREATE TYPE")
            && !sql.contains("LANGUAGE"),
        "opaque value migration does not preserve the closed definition-only contract",
    )
}

#[test]
fn standard_enum_record_fields_are_the_registered_version_twenty() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(19).copied() else {
        return Err(failure(
            "standard enum record field storage migration is not registered",
        ));
    };
    require(version == 20, format!("migration is version {version}"))?;
    require(
        name == "standard enum record field storage",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.standard_catalogue_enum_types")
            && sql.contains("target_type_kind IN ('value', 'enum')")
            && sql.contains("target_enum_type_id")
            && sql.contains("enum_standard_library_revision_id")
            && sql.contains("standard_enum_type_id")
            && sql.contains("cat_record_value_fields_std_enum_fk")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("CREATE TYPE")
            && !sql.contains("LANGUAGE"),
        "standard enum migration does not preserve its protected relational contract",
    )
}

#[test]
fn durable_user_state_cells_are_the_registered_version_twenty_five() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(24).copied() else {
        return Err(failure(
            "durable user state cells migration is not registered",
        ));
    };
    require(version == 25, format!("migration is version {version}"))?;
    require(
        name == "durable user state cells",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.user_state_cells")
            && sql.contains("principal_id bytea NOT NULL")
            && sql.contains("root_function_id bytea NOT NULL")
            && sql.contains("root_state_profile text NOT NULL")
            && sql.contains("function_id bytea NOT NULL")
            && sql.contains("function_instance_key text NOT NULL")
            && sql.contains("state_slot_id bytea NOT NULL")
            && sql.contains("value_bytes bytea NOT NULL")
            && sql.contains("value_type_id bytea NOT NULL")
            && sql.contains("revision bigint NOT NULL")
            && sql.contains("updated_at timestamp with time zone NOT NULL")
            && sql.contains("DEFAULT transaction_timestamp()")
            && sql.contains("CONSTRAINT user_state_cells_pkey")
            && sql.contains("CONSTRAINT user_state_cells_identity_lengths CHECK")
            && sql.contains("octet_length(principal_id) = 16")
            && sql.contains("octet_length(value_type_id) = 16")
            && sql.contains("CONSTRAINT user_state_cells_revision_check CHECK (revision > 0)")
            && sql.contains(
                "CREATE INDEX user_state_cells_principal_root_state_profile_idx\n    ON _orna_kernel.user_state_cells (principal_id, root_function_id, root_state_profile)",
            )
            && sql.contains("REVOKE ALL ON TABLE _orna_kernel.user_state_cells FROM PUBLIC"),
        "user state migration does not preserve the complete protected cell contract",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_creates_one_recoverable_empty_revision() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = Arc::new(PostgresKernel::from_str(&database.connection_string())?);

        let first_kernel = Arc::clone(&kernel);
        let second_kernel = Arc::clone(&kernel);
        let (first_result, second_result) =
            tokio::join!(first_kernel.bootstrap(), second_kernel.bootstrap(),);
        let first = first_result?;
        let second = second_result?;
        require(
            first == second,
            "concurrent bootstrap calls returned different revisions",
        )?;

        let reconnected = PostgresKernel::new(database.config()?);
        let recovered = reconnected.bootstrap().await?;
        require(
            recovered == first,
            "a newly constructed kernel did not recover the active revision",
        )?;

        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_seeded_initial_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_initial_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v2_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v2_empty_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v3_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v20_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        let session = database.open().await?;
        let seed_result = async {
            seed_initial_catalogue_client(session.client()).await?;
            apply_and_register_migrations(session.client(), &MIGRATIONS[1..20]).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (seed_result, shutdown_result) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(seed_error), Err(shutdown_error)) => {
                return Err(failure(format!(
                    "registered v20 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
                )))
            }
        }

        let before = snapshot_upgrade_state(&database).await?;
        require(
            before.migrations.len() == 20,
            format!(
                "registered v20 setup produced unexpected migrations: {:?}",
                before.migrations
            ),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len() && after.migrations[..20] == before.migrations[..],
            format!("v21-v45 changed prior migration records: {:?}", after.migrations),
        )?;
        require(
            after.migrations[20]
                == (
                    21,
                    "nested record field targets".to_owned(),
                    expected_migration_checksum(21, MIGRATIONS[20].2),
                ),
            format!("v21 migration record is not exact: {:?}", after.migrations[20]),
        )?;
        require(
            after.migrations[21]
                == (
                    22,
                    "protected invocation audit".to_owned(),
                    expected_migration_checksum(22, MIGRATIONS[21].2),
                ),
            format!("v22 migration record is not exact: {:?}", after.migrations[21]),
        )?;
        require(
            after.migrations[22]
                == (
                    23,
                    "executable standard relations".to_owned(),
                    expected_migration_checksum(23, MIGRATIONS[22].2),
                ),
            format!("v23 migration record is not exact: {:?}", after.migrations[22]),
        )?;
        require(
            after.migrations[23]
                == (
                    24,
                    "capability audit decisions".to_owned(),
                    expected_migration_checksum(24, MIGRATIONS[23].2),
                ),
            format!("v24 migration record is not exact: {:?}", after.migrations[23]),
        )?;
        require(
            after.migrations[24]
                == (
                    25,
                    "durable user state cells".to_owned(),
                    expected_migration_checksum(25, MIGRATIONS[24].2),
                ),
            format!("v25 migration record is not exact: {:?}", after.migrations[24]),
        )?;
        require(
            after.migrations[25]
                == (
                    26,
                    "user state audit decisions".to_owned(),
                    expected_migration_checksum(26, MIGRATIONS[25].2),
                ),
            format!("v26 migration record is not exact: {:?}", after.migrations[25]),
        )?;
        require(
            after.migrations[26]
                == (
                    27,
                    "inspect snapshots and trace".to_owned(),
                    expected_migration_checksum(27, MIGRATIONS[26].2),
                ),
            format!("v27 migration record is not exact: {:?}", after.migrations[26]),
        )?;
        require(
            after.migrations[27]
                == (
                    28,
                    "security admin privilege grants".to_owned(),
                    expected_migration_checksum(28, MIGRATIONS[27].2),
                ),
            format!("v28 migration record is not exact: {:?}", after.migrations[27]),
        )?;
        require(
            after.migrations[28]
                == (
                    29,
                    "sealed system invocation authorities".to_owned(),
                    expected_migration_checksum(29, MIGRATIONS[28].2),
                ),
            format!("v29 migration record is not exact: {:?}", after.migrations[28]),
        )?;
        require(
            after.migrations[29]
                == (
                    30,
                    "active roles system invocation authority".to_owned(),
                    expected_migration_checksum(30, MIGRATIONS[29].2),
                ),
            format!("v30 migration record is not exact: {:?}", after.migrations[29]),
        )?;
        require(
            after.migrations[30]
                == (
                    31,
                    "standard JSON executable format".to_owned(),
                    expected_migration_checksum(31, MIGRATIONS[30].2),
                ),
            format!("v31 migration record is not exact: {:?}", after.migrations[30]),
        )?;
        require(
            after.migrations[31]
                == (
                    32,
                    "protected resource audit".to_owned(),
                    expected_migration_checksum(32, MIGRATIONS[31].2),
                ),
            format!("v32 migration record is not exact: {:?}", after.migrations[31]),
        )?;
        require(
            after.migrations[32]
                == (
                    33,
                    "stream function returns".to_owned(),
                    expected_migration_checksum(33, MIGRATIONS[32].2),
                ),
            format!("v33 migration record is not exact: {:?}", after.migrations[32]),
        )?;
        require(
            after.migrations[33]
                == (
                    34,
                    "resource request identity history".to_owned(),
                    expected_migration_checksum(34, MIGRATIONS[33].2),
                ),
            format!("v34 migration record is not exact: {:?}", after.migrations[33]),
        )?;
        require(
            after.migrations[34]
                == (
                    35,
                    "resource audit target authorities".to_owned(),
                    expected_migration_checksum(35, MIGRATIONS[34].2),
                ),
            format!("v35 migration record is not exact: {:?}", after.migrations[34]),
        )?;
        require(
            after.migrations[35]
                == (
                    36,
                    "sealed Inspector value types".to_owned(),
                    expected_migration_checksum(36, MIGRATIONS[35].2),
                ),
            format!("v36 migration record is not exact: {:?}", after.migrations[35]),
        )?;
        require(
            after.migrations[36]
                == (
                    37,
                    "source apply audit".to_owned(),
                    expected_migration_checksum(37, MIGRATIONS[36].2),
                ),
            format!("v37 migration record is not exact: {:?}", after.migrations[36]),
        )?;
        for (index, (version, name)) in [
            (37, (38, "source apply principal binding")),
            (38, (39, "sealed invocation SECURITY DEFINER denial audit")),
            (39, (40, "security admin class-wide grant boundary")),
            (40, (41, "nullable resource audit nested invocation")),
        ] {
            require(
                after.migrations[index]
                    == (
                        version,
                        name.to_owned(),
                        expected_migration_checksum(version, MIGRATIONS[index].2),
                    ),
                format!("v{version} migration record is not exact: {:?}", after.migrations[index]),
            )?;
        }
        require(
            after.active_pair == before.active_pair,
            "v21-v45 changed the active revision pair",
        )?;

        let recovered = kernel.recover().await?;
        let (source_revision_id, catalogue_revision_id) = after.active_pair;
        require(
            recovered.pair().source().to_bytes().to_vec() == source_revision_id
                && recovered.pair().catalogue().to_bytes().to_vec() == catalogue_revision_id,
            "v21-v45 recovery does not preserve the active revision pair",
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn user_state_cells_migration_applies_cleanly_and_relation_is_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        inspect_user_state_cells_storage(&database).await
    })
    .await
}

async fn inspect_user_state_cells_storage(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let client = session.client();
        inspect_columns(
            client,
            "user_state_cells",
            &[
                ("principal_id", "bytea", "bytea", "NO", Some("")),
                ("root_function_id", "bytea", "bytea", "NO", Some("")),
                ("root_state_profile", "text", "text", "NO", Some("")),
                ("function_id", "bytea", "bytea", "NO", Some("")),
                ("function_instance_key", "text", "text", "NO", Some("")),
                ("state_slot_id", "bytea", "bytea", "NO", Some("")),
                ("value_bytes", "bytea", "bytea", "NO", Some("")),
                ("value_type_id", "bytea", "bytea", "NO", Some("")),
                ("revision", "bigint", "int8", "NO", Some("")),
                (
                    "updated_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ],
        )
        .await?;
        require_exact_constraint(
            client,
            "user_state_cells",
            "user_state_cells_pkey",
            "PRIMARY KEY (principal_id, root_function_id, root_state_profile, function_id, function_instance_key, state_slot_id)",
            false,
            false,
        )
        .await?;
        require_constraint(
            client,
            "user_state_cells",
            "user_state_cells_identity_lengths",
            "octet_length(principal_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "user_state_cells",
            "user_state_cells_revision_check",
            "revision > 0",
        )
        .await?;
        require_index_shape(
            client,
            "user_state_cells_principal_root_state_profile_idx",
            "user_state_cells",
            "(principal_id, root_function_id, root_state_profile)",
            None,
        )
        .await?;
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = "_orna_kernel.user_state_cells";
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            require(
                !value::<bool>(&row, 0)?,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }

        // The closed domains are enforced, not merely declared: a valid cell
        // writes with its default timestamp, a duplicate full key is
        // rejected, a zero revision is rejected, and a short identity is
        // rejected.
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      decode(repeat('a2', 16), 'hex'), '',
                      decode(repeat('a3', 16), 'hex'), '',
                      decode(repeat('a4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('a5', 16), 'hex'), 1);",
            )
            .await?;
        let row = client
            .query_one(
                "SELECT updated_at IS NOT NULL
                 FROM _orna_kernel.user_state_cells
                 WHERE principal_id = decode(repeat('a1', 16), 'hex')",
                &[],
            )
            .await?;
        let stamped: bool = value(&row, 0)?;
        require(
            stamped,
            "user_state_cells write did not stamp updated_at",
        )?;

        let duplicate = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      decode(repeat('a2', 16), 'hex'), '',
                      decode(repeat('a3', 16), 'hex'), '',
                      decode(repeat('a4', 16), 'hex'),
                      decode('00ccdd', 'hex'),
                      decode(repeat('a5', 16), 'hex'), 2);",
            )
            .await
            .err();
        require(
            duplicate
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_pkey"),
            format!("duplicate user state key failed for the wrong reason: {duplicate:?}"),
        )?;

        let zero_revision = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('b1', 16), 'hex'),
                      decode(repeat('b2', 16), 'hex'), '',
                      decode(repeat('b3', 16), 'hex'), '',
                      decode(repeat('b4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('b5', 16), 'hex'), 0);",
            )
            .await
            .err();
        require(
            zero_revision
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_revision_check"),
            format!("zero revision failed for the wrong reason: {zero_revision:?}"),
        )?;

        let short_identity = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('c1', 15), 'hex'),
                      decode(repeat('c2', 16), 'hex'), '',
                      decode(repeat('c3', 16), 'hex'), '',
                      decode(repeat('c4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('c5', 16), 'hex'), 1);",
            )
            .await
            .err();
        require(
            short_identity
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_identity_lengths"),
            format!("short principal identity failed for the wrong reason: {short_identity:?}"),
        )?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "user state storage inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
        ))),
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn inspect_snapshots_migration_applies_cleanly_and_relations_are_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        inspect_inspect_storage(&database).await
    })
    .await
}

async fn inspect_inspect_storage(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let client = session.client();
        inspect_columns(
            client,
            "inspect_snapshots",
            &[
                ("epoch_id", "bytea", "bytea", "NO", Some("")),
                ("invocation_id", "bytea", "bytea", "NO", Some("")),
                (
                    "recorded_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
                ("owner_principal_id", "bytea", "bytea", "NO", Some("")),
                ("source_revision_id", "bytea", "bytea", "NO", Some("")),
                ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
                ("summary_bytes", "bytea", "bytea", "NO", Some("")),
                (
                    "observer_root_invocation_id",
                    "bytea",
                    "bytea",
                    "YES",
                    Some(""),
                ),
                (
                    "observer_parent_invocation_id",
                    "bytea",
                    "bytea",
                    "YES",
                    Some(""),
                ),
                ("observer_purpose", "text", "text", "YES", Some("")),
            ],
        )
        .await?;
        inspect_columns(
            client,
            "inspect_trace_events",
            &[
                ("invocation_id", "bytea", "bytea", "NO", Some("")),
                ("sequence", "bigint", "int8", "NO", Some("")),
                ("kind", "text", "text", "NO", Some("")),
                ("payload_bytes", "bytea", "bytea", "NO", Some("")),
                ("observer_invocation_id", "bytea", "bytea", "YES", Some("")),
                (
                    "recorded_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ],
        )
        .await?;
        require_exact_constraint(
            client,
            "inspect_snapshots",
            "inspect_snapshots_pkey",
            "PRIMARY KEY (epoch_id)",
            false,
            false,
        )
        .await?;
        require_exact_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_pkey",
            "PRIMARY KEY (invocation_id, sequence)",
            false,
            false,
        )
        .await?;
        require_constraint(
            client,
            "inspect_snapshots",
            "inspect_snapshots_identity_lengths",
            "octet_length(epoch_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_identity_lengths",
            "octet_length(invocation_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_sequence_check",
            "sequence >= 0",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_kind_check",
            "'inspect_snapshot'",
        )
        .await?;
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            for relation in [
                "_orna_kernel.inspect_snapshots",
                "_orna_kernel.inspect_trace_events",
            ] {
                let row = client
                    .query_one(
                        "SELECT has_table_privilege('public', $1, $2)",
                        &[&relation, &privilege],
                    )
                    .await?;
                require(
                    !value::<bool>(&row, 0)?,
                    format!("PUBLIC has {privilege} on protected table {relation}"),
                )?;
            }
        }

        // The closed domains are enforced, not merely declared: a valid
        // snapshot writes with its default timestamp, a duplicate epoch id
        // is rejected, a short identity is rejected, an unknown invocation
        // is rejected, and trace rows enforce the composite key, the closed
        // kind set, non-negative sequences, and identity lengths.
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id)
                 VALUES
                     (decode(repeat('e1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'), 'denied',
                      decode(repeat('71', 16), 'hex'));",
            )
            .await?;
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await?;
        let row = client
            .query_one(
                "SELECT recorded_at IS NOT NULL
                 FROM _orna_kernel.inspect_snapshots
                 WHERE epoch_id = decode(repeat('d1', 16), 'hex')",
                &[],
            )
            .await?;
        require(
            value::<bool>(&row, 0)?,
            "inspect_snapshots write did not stamp recorded_at",
        )?;

        let duplicate_epoch = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00ccdd', 'hex'));",
            )
            .await
            .err();
        require(
            duplicate_epoch
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_pkey"),
            format!("duplicate epoch id failed for the wrong reason: {duplicate_epoch:?}"),
        )?;

        let short_epoch = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 15), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await
            .err();
        require(
            short_epoch
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_identity_lengths"),
            format!("short epoch identity failed for the wrong reason: {short_epoch:?}"),
        )?;

        let unknown_invocation = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d4', 16), 'hex'),
                      decode(repeat('f9', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await
            .err();
        require(
            unknown_invocation
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_invocation_fk"),
            format!("unknown invocation failed for the wrong reason: {unknown_invocation:?}"),
        )?;

        client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 0, 'started',
                      decode('00aa', 'hex'), NULL, transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 1, 'value_batch',
                      decode('00bb', 'hex'),
                      decode(repeat('f2', 16), 'hex'), transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 2, 'completed',
                      decode('00cc', 'hex'), NULL, transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 3, 'inspect_snapshot',
                      decode('00dd', 'hex'), NULL, transaction_timestamp());",
            )
            .await?;

        let duplicate_trace = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 0, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            duplicate_trace
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_pkey"),
            format!("duplicate trace key failed for the wrong reason: {duplicate_trace:?}"),
        )?;

        let bad_kind = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 4, 'snapshot',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            bad_kind
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_kind_check"),
            format!("unclosed trace kind failed for the wrong reason: {bad_kind:?}"),
        )?;

        let negative_sequence = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), -1, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            negative_sequence
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_sequence_check"),
            format!("negative trace sequence failed for the wrong reason: {negative_sequence:?}"),
        )?;

        let short_observer = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 4, 'started',
                      decode('00ee', 'hex'),
                      decode(repeat('f3', 15), 'hex'), transaction_timestamp());",
            )
            .await
            .err();
        require(
            short_observer
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_identity_lengths"),
            format!("short observer identity failed for the wrong reason: {short_observer:?}"),
        )?;

        let unknown_trace_invocation = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('fa', 16), 'hex'), 0, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            unknown_trace_invocation
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_invocation_fk"),
            format!(
                "unknown trace invocation failed for the wrong reason: {unknown_trace_invocation:?}"
            ),
        )?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "inspect storage inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
        ))),
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_enforces_nested_record_field_target_storage() -> TestResult<()> {
    with_test_database(|database| async move {
        let session = database.open().await?;
        let result = async {
            let kernel = PostgresKernel::from_str(&database.connection_string())?;
            kernel.bootstrap().await?;
            verify_nested_record_field_target_storage(session.client()).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verify_error), Err(shutdown_error)) => Err(failure(format!(
                "nested record field target storage verification failed: {verify_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_owner_qualifies_registered_v4_semantic_references() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, false).await?;
        seed_registered_v4_physical_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        verify_owner_qualified_reference_backfill(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_v5_write_reference_evidence_without_mutating_semantics()
-> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v5_semantic_catalogue(&database).await?;
        seed_registered_v4_physical_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        let expected_revision = registered_v4_semantic_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;

        require(
            before.migrations.len() == 5
                && before.migrations.last().map(|migration| migration.0) == Some(5),
            format!("manual v5 setup produced unexpected migrations: {:?}", before.migrations),
        )?;
        require(
            before.active_pair
                == (
                    expected_revision.pair().source().to_bytes().to_vec(),
                    expected_revision.pair().catalogue().to_bytes().to_vec(),
                ),
            format!("manual v5 setup changed the active pair: {:?}", before.active_pair),
        )?;

        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len() && after.migrations[..5] == before.migrations[..],
            format!("v6-v45 changed prior migration records: {:?}", after.migrations),
        )?;
        require(
            after.migrations[5]
                == (
                    6,
                    "definition reference write evidence".to_owned(),
                    expected_migration_checksum(6, MIGRATIONS[5].2),
                ),
            format!("v6 migration record is not exact: {:?}", after.migrations[5]),
        )?;
        require(
            after.migrations[6]
                == (
                    7,
                    "standard catalogue type storage".to_owned(),
                    expected_migration_checksum(7, MIGRATIONS[6].2),
                ),
            format!("v7 migration record is not exact: {:?}", after.migrations[6]),
        )?;
        require(
            after.migrations[7]
                == (
                    8,
                    "resolved value type storage".to_owned(),
                    expected_migration_checksum(8, MIGRATIONS[7].2),
                ),
            format!("v8 migration record is not exact: {:?}", after.migrations[7]),
        )?;
        require(
            after.migrations[8]
                == (
                    9,
                    "security decision snapshot".to_owned(),
                    expected_migration_checksum(9, MIGRATIONS[8].2),
                ),
            format!("v9 migration record is not exact: {:?}", after.migrations[8]),
        )?;
        require(
            after.migrations[9]
                == (
                    10,
                    "local peer credentials".to_owned(),
                    expected_migration_checksum(10, MIGRATIONS[9].2),
                ),
            format!("v10 migration record is not exact: {:?}", after.migrations[9]),
        )?;
        require(
            after.migrations[10]
                == (
                    11,
                    "protected security audit".to_owned(),
                    expected_migration_checksum(11, MIGRATIONS[10].2),
                ),
            format!("v11 migration record is not exact: {:?}", after.migrations[10]),
        )?;
        require(
            after.migrations[11]
                == (
                    12,
                    "catalogue enum type storage".to_owned(),
                    expected_migration_checksum(12, MIGRATIONS[11].2),
                ),
            format!("v12 migration record is not exact: {:?}", after.migrations[11]),
        )?;
        require(
            after.migrations[12]
                == (
                    13,
                    "resolved enum type storage".to_owned(),
                    expected_migration_checksum(13, MIGRATIONS[12].2),
                ),
            format!("v13 migration record is not exact: {:?}", after.migrations[12]),
        )?;
        require(
            after.migrations[13]
                == (
                    14,
                    "catalogue enum reference targets".to_owned(),
                    expected_migration_checksum(14, MIGRATIONS[13].2),
                ),
            format!("v14 migration record is not exact: {:?}", after.migrations[13]),
        )?;
        require(
            after.migrations[14]
                == (
                    15,
                    "catalogue record value storage".to_owned(),
                    expected_migration_checksum(15, MIGRATIONS[14].2),
                ),
            format!("v15 migration record is not exact: {:?}", after.migrations[14]),
        )?;
        require(
            after.migrations[15]
                == (
                    16,
                    "resolved record value type storage".to_owned(),
                    expected_migration_checksum(16, MIGRATIONS[15].2),
                ),
            format!("v16 migration record is not exact: {:?}", after.migrations[15]),
        )?;
        require(
            after.migrations[16]
                == (
                    17,
                    "record value field reference targets".to_owned(),
                    expected_migration_checksum(17, MIGRATIONS[16].2),
                ),
            format!("v17 migration record is not exact: {:?}", after.migrations[16]),
        )?;
        require(
            after.migrations[17]
                == (
                    18,
                    "disjoint field reference targets".to_owned(),
                    expected_migration_checksum(18, MIGRATIONS[17].2),
                ),
            format!("v18 migration record is not exact: {:?}", after.migrations[17]),
        )?;
        require(
            after.migrations[18]
                == (
                    19,
                    "standard opaque value storage".to_owned(),
                    expected_migration_checksum(19, MIGRATIONS[18].2),
                ),
            format!("v19 migration record is not exact: {:?}", after.migrations[18]),
        )?;
        require(
            after.migrations[19]
                == (
                    20,
                    "standard enum record field storage".to_owned(),
                    expected_migration_checksum(20, MIGRATIONS[19].2),
                ),
            format!("v20 migration record is not exact: {:?}", after.migrations[19]),
        )?;
        require(
            after.migrations[20]
                == (
                    21,
                    "nested record field targets".to_owned(),
                    expected_migration_checksum(21, MIGRATIONS[20].2),
                ),
            format!("v21 migration record is not exact: {:?}", after.migrations[20]),
        )?;
        require(
            after.migrations[21]
                == (
                    22,
                    "protected invocation audit".to_owned(),
                    expected_migration_checksum(22, MIGRATIONS[21].2),
                ),
            format!("v22 migration record is not exact: {:?}", after.migrations[21]),
        )?;
        require(
            after.migrations[22]
                == (
                    23,
                    "executable standard relations".to_owned(),
                    expected_migration_checksum(23, MIGRATIONS[22].2),
                ),
            format!("v23 migration record is not exact: {:?}", after.migrations[22]),
        )?;
        require(
            after.migrations[23]
                == (
                    24,
                    "capability audit decisions".to_owned(),
                    expected_migration_checksum(24, MIGRATIONS[23].2),
                ),
            format!("v24 migration record is not exact: {:?}", after.migrations[23]),
        )?;
        require(
            after.migrations[24]
                == (
                    25,
                    "durable user state cells".to_owned(),
                    expected_migration_checksum(25, MIGRATIONS[24].2),
                ),
            format!("v25 migration record is not exact: {:?}", after.migrations[24]),
        )?;
        require(
            after.migrations[25]
                == (
                    26,
                    "user state audit decisions".to_owned(),
                    expected_migration_checksum(26, MIGRATIONS[25].2),
                ),
            format!("v26 migration record is not exact: {:?}", after.migrations[25]),
        )?;
        require(
            after.migrations[26]
                == (
                    27,
                    "inspect snapshots and trace".to_owned(),
                    expected_migration_checksum(27, MIGRATIONS[26].2),
                ),
            format!("v27 migration record is not exact: {:?}", after.migrations[26]),
        )?;
        require(
            after.migrations[27]
                == (
                    28,
                    "security admin privilege grants".to_owned(),
                    expected_migration_checksum(28, MIGRATIONS[27].2),
                ),
            format!("v28 migration record is not exact: {:?}", after.migrations[27]),
        )?;
        require(
            after.migrations[28]
                == (
                    29,
                    "sealed system invocation authorities".to_owned(),
                    expected_migration_checksum(29, MIGRATIONS[28].2),
                ),
            format!("v29 migration record is not exact: {:?}", after.migrations[28]),
        )?;
        require(
            after.migrations[29]
                == (
                    30,
                    "active roles system invocation authority".to_owned(),
                    expected_migration_checksum(30, MIGRATIONS[29].2),
                ),
            format!("v30 migration record is not exact: {:?}", after.migrations[29]),
        )?;
        require(
            after.migrations[30]
                == (
                    31,
                    "standard JSON executable format".to_owned(),
                    expected_migration_checksum(31, MIGRATIONS[30].2),
                ),
            format!("v31 migration record is not exact: {:?}", after.migrations[30]),
        )?;
        require(
            after.migrations[31]
                == (
                    32,
                    "protected resource audit".to_owned(),
                    expected_migration_checksum(32, MIGRATIONS[31].2),
                ),
            format!("v32 migration record is not exact: {:?}", after.migrations[31]),
        )?;
        require(
            after.migrations[32]
                == (
                    33,
                    "stream function returns".to_owned(),
                    expected_migration_checksum(33, MIGRATIONS[32].2),
                ),
            format!("v33 migration record is not exact: {:?}", after.migrations[32]),
        )?;
        require(
            after.active_pair == before.active_pair,
            "v6 changed the active revision pair",
        )?;
        require(
            after.references == before.references,
            "v6 changed existing definition-reference rows or xmin values",
        )?;
        require(
            after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "v6 changed catalogue or function semantic hash bytes",
        )?;
        let after_revision = kernel.recover().await?;
        let pair_matches = expected_revision.pair() == after_revision.pair();
        let source_matches = expected_revision.source() == after_revision.source();
        let catalogue_hash_matches =
            expected_revision.catalogue_hash() == after_revision.catalogue_hash();
        let catalogue_revision_matches =
            expected_revision.catalogue().revision() == after_revision.catalogue().revision();
        let schemas_match =
            expected_revision.catalogue().schemas() == after_revision.catalogue().schemas();
        let object_types_match = expected_revision.catalogue().object_types()
            == after_revision.catalogue().object_types();
        let functions_match =
            expected_revision.catalogue().functions() == after_revision.catalogue().functions();
        let expressions_match = expected_revision.expressions() == after_revision.expressions();
        let function_revisions_match =
            expected_revision.function_revisions() == after_revision.function_revisions();
        let historical_revisions_match = expected_revision.historical_function_revisions()
            == after_revision.historical_function_revisions();
        let origins_match = same_members(expected_revision.origins(), after_revision.origins());
        let references_match = expected_revision.references() == after_revision.references();
        require(
            pair_matches
                && source_matches
                && catalogue_hash_matches
                && catalogue_revision_matches
                && schemas_match
                && object_types_match
                && functions_match
                && expressions_match
                && function_revisions_match
                && historical_revisions_match
                && origins_match
                && references_match,
            format!(
                "v6 recovery differs: pair={pair_matches}, source={source_matches}, catalogue_hash={catalogue_hash_matches}, catalogue_revision={catalogue_revision_matches}, schemas={schemas_match}, object_types={object_types_match}, functions={functions_match}, expressions={expressions_match}, function_revisions={function_revisions_match}, historical={historical_revisions_match}, origins={origins_match}, references={references_match}"
            ),
        )?;

        let session = database.open().await?;
        let verification_result = verify_write_reference_compatibility(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (verification_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verification_error), Err(shutdown_error)) => Err(failure(format!(
                "write-reference compatibility verification failed: {verification_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_registered_v6_without_standard_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v6_catalogue(&database).await?;
        let expected_revision = registered_v4_semantic_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;
        require(
            before.migrations.len() == 6
                && before.migrations.last().map(|migration| migration.0) == Some(6),
            format!(
                "manual v6 setup produced unexpected migrations: {:?}",
                before.migrations
            ),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len()
                && after.migrations[..6] == before.migrations[..]
                && after.migrations[6]
                    == (
                        7,
                        "standard catalogue type storage".to_owned(),
                        expected_migration_checksum(7, MIGRATIONS[6].2),
                    )
                && after.migrations[20]
                    == (
                        21,
                        "nested record field targets".to_owned(),
                        expected_migration_checksum(21, MIGRATIONS[20].2),
                    )
                && after.migrations[21]
                    == (
                        22,
                        "protected invocation audit".to_owned(),
                        expected_migration_checksum(22, MIGRATIONS[21].2),
                    )
                && after.migrations[22]
                    == (
                        23,
                        "executable standard relations".to_owned(),
                        expected_migration_checksum(23, MIGRATIONS[22].2),
                    )
                && after.migrations[23]
                    == (
                        24,
                        "capability audit decisions".to_owned(),
                        expected_migration_checksum(24, MIGRATIONS[23].2),
                    )
                && after.migrations[24]
                    == (
                        25,
                        "durable user state cells".to_owned(),
                        expected_migration_checksum(25, MIGRATIONS[24].2),
                    )
                && after.migrations[25]
                    == (
                        26,
                        "user state audit decisions".to_owned(),
                        expected_migration_checksum(26, MIGRATIONS[25].2),
                    )
                && after.migrations[26]
                    == (
                        27,
                        "inspect snapshots and trace".to_owned(),
                        expected_migration_checksum(27, MIGRATIONS[26].2),
                    )
                && after.migrations[27]
                    == (
                        28,
                        "security admin privilege grants".to_owned(),
                        expected_migration_checksum(28, MIGRATIONS[27].2),
                    )
                && after.migrations[28]
                    == (
                        29,
                        "sealed system invocation authorities".to_owned(),
                        expected_migration_checksum(29, MIGRATIONS[28].2),
                    )
                && after.migrations[29]
                    == (
                        30,
                        "active roles system invocation authority".to_owned(),
                        expected_migration_checksum(30, MIGRATIONS[29].2),
                    )
                && after.migrations[30]
                    == (
                        31,
                        "standard JSON executable format".to_owned(),
                        expected_migration_checksum(31, MIGRATIONS[30].2),
                    )
                && after.migrations[31]
                    == (
                        32,
                        "protected resource audit".to_owned(),
                        expected_migration_checksum(32, MIGRATIONS[31].2),
                    )
                && after.migrations[32]
                    == (
                        33,
                        "stream function returns".to_owned(),
                        expected_migration_checksum(33, MIGRATIONS[32].2),
                    ),
            format!("v6 upgrade produced unexpected migrations: {:?}", after.migrations),
        )?;
        require(
            after.migrations[7]
                == (
                    8,
                    "resolved value type storage".to_owned(),
                    expected_migration_checksum(8, MIGRATIONS[7].2),
                ),
            format!("v8 migration record is not exact: {:?}", after.migrations[7]),
        )?;
        require(
            after.migrations[8]
                == (
                    9,
                    "security decision snapshot".to_owned(),
                    expected_migration_checksum(9, MIGRATIONS[8].2),
                ),
            format!("v9 migration record is not exact: {:?}", after.migrations[8]),
        )?;
        require(
            after.migrations[9]
                == (
                    10,
                    "local peer credentials".to_owned(),
                    expected_migration_checksum(10, MIGRATIONS[9].2),
                ),
            format!("v10 migration record is not exact: {:?}", after.migrations[9]),
        )?;
        require(
            after.migrations[10]
                == (
                    11,
                    "protected security audit".to_owned(),
                    expected_migration_checksum(11, MIGRATIONS[10].2),
                ),
            format!("v11 migration record is not exact: {:?}", after.migrations[10]),
        )?;
        require(
            after.migrations[11]
                == (
                    12,
                    "catalogue enum type storage".to_owned(),
                    expected_migration_checksum(12, MIGRATIONS[11].2),
                ),
            format!("v12 migration record is not exact: {:?}", after.migrations[11]),
        )?;
        require(
            after.migrations[12]
                == (
                    13,
                    "resolved enum type storage".to_owned(),
                    expected_migration_checksum(13, MIGRATIONS[12].2),
                ),
            format!("v13 migration record is not exact: {:?}", after.migrations[12]),
        )?;
        require(
            after.migrations[13]
                == (
                    14,
                    "catalogue enum reference targets".to_owned(),
                    expected_migration_checksum(14, MIGRATIONS[13].2),
                ),
            format!("v14 migration record is not exact: {:?}", after.migrations[13]),
        )?;
        require(
            after.migrations[14]
                == (
                    15,
                    "catalogue record value storage".to_owned(),
                    expected_migration_checksum(15, MIGRATIONS[14].2),
                ),
            format!("v15 migration record is not exact: {:?}", after.migrations[14]),
        )?;
        require(
            after.migrations[15]
                == (
                    16,
                    "resolved record value type storage".to_owned(),
                    expected_migration_checksum(16, MIGRATIONS[15].2),
                ),
            format!("v16 migration record is not exact: {:?}", after.migrations[15]),
        )?;
        require(
            after.migrations[16]
                == (
                    17,
                    "record value field reference targets".to_owned(),
                    expected_migration_checksum(17, MIGRATIONS[16].2),
                ),
            format!("v17 migration record is not exact: {:?}", after.migrations[16]),
        )?;
        require(
            after.migrations[17]
                == (
                    18,
                    "disjoint field reference targets".to_owned(),
                    expected_migration_checksum(18, MIGRATIONS[17].2),
                ),
            format!("v18 migration record is not exact: {:?}", after.migrations[17]),
        )?;
        require(
            after.migrations[18]
                == (
                    19,
                    "standard opaque value storage".to_owned(),
                    expected_migration_checksum(19, MIGRATIONS[18].2),
                ),
            format!("v19 migration record is not exact: {:?}", after.migrations[18]),
        )?;
        require(
            after.migrations[19]
                == (
                    20,
                    "standard enum record field storage".to_owned(),
                    expected_migration_checksum(20, MIGRATIONS[19].2),
                ),
            format!("v20 migration record is not exact: {:?}", after.migrations[19]),
        )?;
        require(
            after.active_pair == before.active_pair
                && after.source_unit_count == before.source_unit_count
                && after.references == before.references
                && after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "migration 0007 changed the active pair, references, or semantic hashes",
        )?;
        let recovered = kernel.recover().await?;
        let catalogue_matches = recovered.catalogue().revision()
            == expected_revision.catalogue().revision()
            && recovered.catalogue().schemas() == expected_revision.catalogue().schemas()
            && recovered.catalogue().object_types() == expected_revision.catalogue().object_types()
            && recovered.catalogue().functions() == expected_revision.catalogue().functions();
        require(
            recovered.pair() == expected_revision.pair()
                && recovered.source() == expected_revision.source()
                && recovered.catalogue_hash() == expected_revision.catalogue_hash()
                && catalogue_matches
                && recovered.expressions() == expected_revision.expressions()
                && recovered.function_revisions() == expected_revision.function_revisions()
                && same_members(recovered.origins(), expected_revision.origins())
                && recovered.references() == expected_revision.references(),
            "migration 0007 changed recoverable application revision facts",
        )?;
        let session = database.open().await?;
        let inspection_result = inspect_standard_catalogue_schema(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (inspection_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
                "v6 standard schema inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_registered_v7_without_resolved_value_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v7_catalogue(&database).await?;
        let expected_revision = registered_v7_rows_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;
        let before_surface = snapshot_catalogue_surface(&database).await?;
        let before_target_fks = snapshot_application_target_foreign_keys(&database).await?;
        let expected_target_fks = expected_application_target_foreign_keys();
        require(
            before_target_fks == expected_target_fks,
            format!("v7 application target foreign keys are not exact: {before_target_fks:?}"),
        )?;
        require(
            before.migrations.len() == 7
                && before.migrations.last().map(|migration| migration.0) == Some(7),
            format!("manual v7 setup produced unexpected migrations: {:?}", before.migrations),
        )?;
        require(
            before.active_pair
                == (
                    expected_revision.pair().source().to_bytes().to_vec(),
                    expected_revision.pair().catalogue().to_bytes().to_vec(),
                ),
            format!("manual v7 setup changed the active pair: {:?}", before.active_pair),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        let after_surface = snapshot_catalogue_surface(&database).await?;
        let after_target_fks = snapshot_application_target_foreign_keys(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len()
                && after.migrations[..7] == before.migrations[..]
                && after.migrations[7]
                    == (
                        8,
                        "resolved value type storage".to_owned(),
                        expected_migration_checksum(8, MIGRATIONS[7].2),
                    )
                && after.migrations[8]
                    == (
                        9,
                        "security decision snapshot".to_owned(),
                        expected_migration_checksum(9, MIGRATIONS[8].2),
                    )
                && after.migrations[9]
                    == (
                        10,
                        "local peer credentials".to_owned(),
                        expected_migration_checksum(10, MIGRATIONS[9].2),
                    )
                && after.migrations[10]
                    == (
                        11,
                        "protected security audit".to_owned(),
                        expected_migration_checksum(11, MIGRATIONS[10].2),
                    )
                && after.migrations[11]
                    == (
                        12,
                        "catalogue enum type storage".to_owned(),
                        expected_migration_checksum(12, MIGRATIONS[11].2),
                    )
                && after.migrations[12]
                    == (
                        13,
                        "resolved enum type storage".to_owned(),
                        expected_migration_checksum(13, MIGRATIONS[12].2),
                    )
                && after.migrations[13]
                    == (
                        14,
                        "catalogue enum reference targets".to_owned(),
                        expected_migration_checksum(14, MIGRATIONS[13].2),
                    )
                && after.migrations[14]
                    == (
                        15,
                        "catalogue record value storage".to_owned(),
                        expected_migration_checksum(15, MIGRATIONS[14].2),
                    )
                && after.migrations[15]
                    == (
                        16,
                        "resolved record value type storage".to_owned(),
                        expected_migration_checksum(16, MIGRATIONS[15].2),
                    )
                && after.migrations[16]
                    == (
                        17,
                        "record value field reference targets".to_owned(),
                        expected_migration_checksum(17, MIGRATIONS[16].2),
                    )
                && after.migrations[17]
                    == (
                        18,
                        "disjoint field reference targets".to_owned(),
                        expected_migration_checksum(18, MIGRATIONS[17].2),
                    )
                && after.migrations[18]
                    == (
                        19,
                        "standard opaque value storage".to_owned(),
                        expected_migration_checksum(19, MIGRATIONS[18].2),
                    )
                && after.migrations[19]
                    == (
                        20,
                        "standard enum record field storage".to_owned(),
                        expected_migration_checksum(20, MIGRATIONS[19].2),
                    )
                && after.migrations[20]
                    == (
                        21,
                        "nested record field targets".to_owned(),
                        expected_migration_checksum(21, MIGRATIONS[20].2),
                    )
                && after.migrations[21]
                    == (
                        22,
                        "protected invocation audit".to_owned(),
                        expected_migration_checksum(22, MIGRATIONS[21].2),
                    )
                && after.migrations[22]
                    == (
                        23,
                        "executable standard relations".to_owned(),
                        expected_migration_checksum(23, MIGRATIONS[22].2),
                    )
                && after.migrations[23]
                    == (
                        24,
                        "capability audit decisions".to_owned(),
                        expected_migration_checksum(24, MIGRATIONS[23].2),
                    )
                && after.migrations[24]
                    == (
                        25,
                        "durable user state cells".to_owned(),
                        expected_migration_checksum(25, MIGRATIONS[24].2),
                    )
                && after.migrations[25]
                    == (
                        26,
                        "user state audit decisions".to_owned(),
                        expected_migration_checksum(26, MIGRATIONS[25].2),
                    )
                && after.migrations[26]
                    == (
                        27,
                        "inspect snapshots and trace".to_owned(),
                        expected_migration_checksum(27, MIGRATIONS[26].2),
                    )
                && after.migrations[27]
                    == (
                        28,
                        "security admin privilege grants".to_owned(),
                        expected_migration_checksum(28, MIGRATIONS[27].2),
                    )
                && after.migrations[28]
                    == (
                        29,
                        "sealed system invocation authorities".to_owned(),
                        expected_migration_checksum(29, MIGRATIONS[28].2),
                    )
                && after.migrations[29]
                    == (
                        30,
                        "active roles system invocation authority".to_owned(),
                        expected_migration_checksum(30, MIGRATIONS[29].2),
                    )
                && after.migrations[30]
                    == (
                        31,
                        "standard JSON executable format".to_owned(),
                        expected_migration_checksum(31, MIGRATIONS[30].2),
                    )
                && after.migrations[31]
                    == (
                        32,
                        "protected resource audit".to_owned(),
                        expected_migration_checksum(32, MIGRATIONS[31].2),
                    )
                && after.migrations[32]
                    == (
                        33,
                        "stream function returns".to_owned(),
                        expected_migration_checksum(33, MIGRATIONS[32].2),
                    ),
            format!("v7-v45 upgrade produced unexpected migrations: {:?}", after.migrations),
        )?;
        require(
            after.active_pair == before.active_pair
                && after.source_unit_count == before.source_unit_count
                && after.references == before.references
                && after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "migration 0008 changed the active pair, references, or semantic hashes",
        )?;
        require(
            before_surface == without_later_relations(&after_surface),
            format!(
                "migration 0008 changed a relation, index, trigger, or ACL: before={before_surface:?}, after={after_surface:?}"
            ),
        )?;
        require(
            after_target_fks == expected_application_target_foreign_keys_after_sealed_inspector(),
            format!(
                "migration 0036 changed application target foreign keys unexpectedly: before={before_target_fks:?}, after={after_target_fks:?}"
            ),
        )?;

        let recovered = kernel.recover().await?;
        let catalogue_matches = recovered.catalogue().revision()
            == expected_revision.catalogue().revision()
            && recovered.catalogue().schemas() == expected_revision.catalogue().schemas()
            && recovered.catalogue().object_types() == expected_revision.catalogue().object_types()
            && recovered.catalogue().functions() == expected_revision.catalogue().functions();
        require(
            recovered.pair() == expected_revision.pair()
                && recovered.source() == expected_revision.source()
                && recovered.catalogue_hash() == expected_revision.catalogue_hash()
                && catalogue_matches
                && recovered.expressions() == expected_revision.expressions()
                && recovered.function_revisions() == expected_revision.function_revisions()
                && same_members(recovered.origins(), expected_revision.origins())
                && recovered.references() == expected_revision.references(),
            "migration 0008 changed recoverable application revision facts",
        )?;

        let session = database.open().await?;
        let inspection_result = async {
            inspect_resolved_value_storage(session.client(), true).await?;
            inspect_resolved_enum_storage(session.client(), true).await?;
            inspect_standard_catalogue_schema(session.client()).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (inspection_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
                "v7 resolved-value inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_catalogue_zero_catalogue_id_is_schema_valid_without_activation() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        let session = database.open().await?;
        let result = async {
            let source_revision_id = session
                .client()
                .query_one(
                    "SELECT source_revision_id
                     FROM _orna_kernel.active_revision
                     WHERE singleton = true",
                    &[],
                )
                .await?
                .get::<_, Vec<u8>>(0);
            let standard_library_revision_id = vec![0x71_u8; 16];
            let zero_catalogue_revision_id = vec![0_u8; 16];
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.standard_library_revisions
                        (id, source_revision_id, catalogue_revision_id,
                         language_version, content_hash)
                     VALUES ($1, $2, $3, 'standard-v1', $4)",
                    &[
                        &standard_library_revision_id,
                        &source_revision_id,
                        &zero_catalogue_revision_id,
                        &vec![0x72_u8; 32],
                    ],
                )
                .await?;
            let row = session
                .client()
                .query_one(
                    "SELECT catalogue_revision_id, digest_version, hash_algorithm
                     FROM _orna_kernel.standard_library_revisions
                     WHERE id = $1",
                    &[&standard_library_revision_id],
                )
                .await?;
            let catalogue_revision_id: Vec<u8> = row.get(0);
            let digest_version: i16 = row.get(1);
            let hash_algorithm: String = row.get(2);
            require(
                catalogue_revision_id == zero_catalogue_revision_id
                    && digest_version == 1
                    && hash_algorithm == "sha256",
                "the all-zero standard catalogue ID did not remain schema-valid",
            )?;
            let active_pin: Option<Vec<u8>> = session
                .client()
                .query_one(
                    "SELECT standard_library_revision_id
                     FROM _orna_kernel.catalogue_revisions",
                    &[],
                )
                .await?
                .get(0);
            require(
                active_pin.is_none(),
                "the raw sentinel fixture changed the application catalogue pin",
            )?;
            session
                .client()
                .execute(
                    "DELETE FROM _orna_kernel.standard_library_revisions WHERE id = $1",
                    &[&standard_library_revision_id],
                )
                .await?;
            Ok(())
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(insert_error), Err(shutdown_error)) => Err(failure(format!(
                "all-zero standard catalogue fixture failed: {insert_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v5_for_a_dangling_legacy_reference() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, true).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v5 must reject a dangling legacy field reference");
        require_database_constraint(
            &error,
            "23514",
            Some("definition_references_target_owner_shape_check"),
            "dangling legacy field reference",
        )?;
        inspect_v5_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v5_for_an_ambiguous_legacy_reference() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, false).await?;
        insert_ambiguous_legacy_field_target(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v5 must reject an ambiguous legacy field reference");
        require_database_constraint(&error, "21000", None, "ambiguous legacy field reference")?;
        inspect_v5_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v4_when_legacy_empty_hashes_are_tampered() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        let session = database.open().await?;
        let tamper_result = session
            .client()
            .execute(
                "UPDATE _orna_kernel.source_bundles SET content_hash = $1",
                &[&vec![0_u8; 32]],
            )
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as _);
        let shutdown_result = session.shutdown().await;
        match (tamper_result, shutdown_result) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(tamper_error), Err(shutdown_error)) => {
                return Err(failure(format!(
                    "legacy hash tamper failed: {tamper_error}; tamper driver shutdown failed: {shutdown_error}"
                )));
            }
        }

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        let error = kernel
            .bootstrap()
            .await
            .expect_err("a tampered legacy hash must fail closed");
        require(
            matches!(error, PostgresKernelError::CatalogueInvariant(_)),
            format!("tampered legacy hash produced the wrong failure: {error}"),
        )?;
        inspect_v4_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_registered_v3_semantic_rows_and_rolls_back_v4() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        insert_unsupported_initial_schema(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v4 must reject a registered legacy catalogue with semantic rows");
        require(
            matches!(error, PostgresKernelError::CatalogueInvariant(_)),
            format!("registered v3 semantic row produced the wrong failure: {error}"),
        )?;
        inspect_v4_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn function_revisions_allow_distinct_semantics_for_one_declaration() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let session = database.open().await?;
        let verification_result = verify_function_revision_semantic_hash_uniqueness(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (verification_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verification_error), Err(shutdown_error)) => Err(failure(format!(
                "function revision uniqueness verification failed: {verification_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_a_seeded_initial_catalogue_with_semantic_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_initial_catalogue(&database).await?;
        insert_unsupported_initial_schema(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("migration 0002 must reject an unhashable initial catalogue");
        require_database_constraint(
            &error,
            "23514",
            Some("migration_0002_legacy_state_valid_check"),
            "non-empty migration 0001 catalogue",
        )?;
        inspect_v2_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_tampered_gapped_and_newer_migration_history() -> TestResult<()> {
    reject_migration_history(
        1,
        "renamed migration",
        Sha256::digest(MIGRATIONS[0].2.as_bytes()).to_vec(),
    )
    .await?;
    reject_migration_history(1, MIGRATIONS[0].1, vec![0; 32]).await?;
    reject_migration_history(
        2,
        MIGRATIONS[1].1,
        Sha256::digest(MIGRATIONS[1].2.as_bytes()).to_vec(),
    )
    .await?;
    reject_migration_history(26, "future migration", vec![0; 32]).await
}

async fn inspect_bootstrap_state(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = inspect_client(session.client()).await;
    let shutdown_result = session.shutdown().await;

    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "bootstrap inspection failed: {inspection_error}; inspection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn inspect_client(client: &Client) -> TestResult<()> {
    inspect_migrations(client).await?;
    require_count(
        client,
        "_orna_kernel.source_bundles",
        "SELECT count(*) FROM _orna_kernel.source_bundles",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.source_revisions",
        "SELECT count(*) FROM _orna_kernel.source_revisions",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.catalogue_revisions",
        "SELECT count(*) FROM _orna_kernel.catalogue_revisions",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.active_revision",
        "SELECT count(*) FROM _orna_kernel.active_revision",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.source_units",
        "SELECT count(*) FROM _orna_kernel.source_units",
        0,
    )
    .await?;

    inspect_empty_aggregate_hashes(client).await?;
    inspect_hash_contract_columns(client).await?;
    inspect_origin_columns(client).await?;
    inspect_owner_qualified_catalogue_members(client).await?;
    inspect_definition_references(client).await?;
    inspect_function_revision_constraints(client).await?;
    inspect_standard_catalogue_schema(client).await?;
    inspect_resolved_value_storage(client, true).await?;
    inspect_resolved_enum_storage(client, true).await?;
    inspect_record_value_storage(client).await?;
    inspect_security_snapshot_schema(client).await?;
    inspect_resource_audit_schema(client).await?;

    for schema in ["_orna_kernel", "_orna_data"] {
        let role = "public";
        let privilege = "USAGE";
        let row = client
            .query_one(
                "SELECT has_schema_privilege($1, $2, $3)",
                &[&role, &schema, &privilege],
            )
            .await?;
        let has_public_usage: bool = value(&row, 0)?;
        require(
            !has_public_usage,
            format!("PUBLIC has USAGE on protected schema {schema}"),
        )?;
    }

    let table_schema = "_orna_kernel";
    let table_type = "BASE TABLE";
    let rows = client
        .query(
            "SELECT table_name
             FROM information_schema.tables
             WHERE table_schema = $1 AND table_type = $2
             ORDER BY table_name",
            &[&table_schema, &table_type],
        )
        .await?;
    let actual_tables = rows
        .iter()
        .map(|row| value::<String>(row, 0))
        .collect::<TestResult<BTreeSet<_>>>()?;
    let expected_tables = EXPECTED_KERNEL_TABLES
        .iter()
        .map(|table| (*table).to_owned())
        .collect::<BTreeSet<_>>();
    require(
        actual_tables == expected_tables,
        format!(
            "protected table set differs; expected {expected_tables:?}, found {actual_tables:?}"
        ),
    )
}

async fn inspect_security_snapshot_schema(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "security_principals",
        &[
            ("id", "bytea", "bytea", "NO", Some("")),
            ("kind", "text", "text", "NO", Some("")),
            ("status", "text", "text", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_role_memberships",
        &[
            ("role_id", "bytea", "bytea", "NO", Some("")),
            ("member_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_execute_grants",
        &[
            ("grantee_id", "bytea", "bytea", "NO", Some("")),
            ("function_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_local_peer_credentials",
        &[
            ("uid", "bigint", "int8", "NO", Some("")),
            ("principal_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_audit_events",
        &[
            ("sequence", "bigint", "int8", "NO", None),
            ("event_id", "bytea", "bytea", "NO", Some("")),
            (
                "recorded_at",
                "timestamp without time zone",
                "timestamp",
                "NO",
                None,
            ),
            ("event_kind", "text", "text", "NO", Some("")),
            ("outcome", "text", "text", "NO", Some("")),
            ("session_principal_id", "bytea", "bytea", "YES", Some("")),
            ("effective_principal_id", "bytea", "bytea", "YES", Some("")),
            (
                "authorising_principal_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("function_id", "bytea", "bytea", "YES", Some("")),
            ("source_revision_id", "bytea", "bytea", "YES", Some("")),
            ("catalogue_revision_id", "bytea", "bytea", "YES", Some("")),
            ("denial_reason", "text", "text", "YES", Some("")),
        ],
    )
    .await?;

    for (table, constraint, expected) in [
        (
            "security_principals",
            "security_principals_id_length",
            "octet_length(id) = 16",
        ),
        (
            "security_principals",
            "security_principals_kind_check",
            "kind = ANY",
        ),
        (
            "security_principals",
            "security_principals_status_check",
            "status = ANY",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_not_self",
            "role_id <> member_id",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_role_fk",
            "FOREIGN KEY (role_id)",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_member_fk",
            "FOREIGN KEY (member_id)",
        ),
        (
            "security_execute_grants",
            "security_execute_grants_function_id_length",
            "octet_length(function_id) = 16",
        ),
        (
            "security_execute_grants",
            "security_execute_grants_grantee_fk",
            "FOREIGN KEY (grantee_id)",
        ),
        (
            "security_local_peer_credentials",
            "security_local_peer_credentials_principal_key",
            "UNIQUE (principal_id)",
        ),
        (
            "security_local_peer_credentials",
            "security_local_peer_credentials_principal_fk",
            "FOREIGN KEY (principal_id)",
        ),
        (
            "security_audit_events",
            "security_audit_events_event_id_key",
            "UNIQUE (event_id)",
        ),
        (
            "security_audit_events",
            "security_audit_events_identity_lengths",
            "octet_length(event_id) = 16",
        ),
        (
            "security_audit_events",
            "security_audit_events_kind_check",
            "event_kind = ANY",
        ),
        (
            "security_audit_events",
            "security_audit_events_outcome_check",
            "outcome = ANY",
        ),
        (
            "security_audit_events",
            "security_audit_events_revision_pair_check",
            "(source_revision_id IS NULL) = (catalogue_revision_id IS NULL)",
        ),
        (
            "security_audit_events",
            "security_audit_events_denial_reason_check",
            "source_apply:committed",
        ),
        (
            "security_audit_events",
            "security_audit_events_shape_check",
            "event_kind = 'source_apply'::text",
        ),
    ] {
        require_constraint(client, table, constraint, expected).await?;
    }
    let uid_range = constraint_definition(
        client,
        "security_local_peer_credentials",
        "security_local_peer_credentials_uid_range",
    )
    .await?;
    require(
        uid_range.contains("uid >= 0") && uid_range.contains("uid <= '4294967295'::bigint"),
        format!("local peer UID range is not exact: {uid_range:?}"),
    )?;
    require_index(
        client,
        "security_role_memberships_member_index",
        "(member_id, role_id)",
    )
    .await?;
    require_index(
        client,
        "security_execute_grants_function_index",
        "(function_id, grantee_id)",
    )
    .await?;

    let identity = client
        .query_one(
            "SELECT is_identity, identity_generation
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'security_audit_events'
               AND column_name = 'sequence'",
            &[],
        )
        .await?;
    require(
        value::<String>(&identity, 0)? == "YES" && value::<String>(&identity, 1)? == "ALWAYS",
        "security audit sequence is not an always-generated identity",
    )?;
    require_count(
        client,
        "_orna_kernel.security_audit_events",
        "SELECT count(*) FROM _orna_kernel.security_audit_events",
        0,
    )
    .await?;

    for table in [
        "security_audit_events",
        "security_principals",
        "security_role_memberships",
        "security_execute_grants",
        "security_local_peer_credentials",
    ] {
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }

    for privilege in ["USAGE", "SELECT", "UPDATE"] {
        let row = client
            .query_one(
                "SELECT has_sequence_privilege(
                    'public',
                    '_orna_kernel.security_audit_events_sequence_seq',
                    $1
                 )",
                &[&privilege],
            )
            .await?;
        require(
            !value::<bool>(&row, 0)?,
            format!("PUBLIC has {privilege} on the protected audit sequence"),
        )?;
    }

    Ok(())
}

async fn inspect_resource_audit_schema(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "resource_audit_events",
        &[
            ("sequence", "bigint", "int8", "NO", None),
            ("event_id", "bytea", "bytea", "NO", Some("")),
            (
                "recorded_at",
                "timestamp without time zone",
                "timestamp",
                "NO",
                None,
            ),
            ("request_id", "bytea", "bytea", "NO", Some("")),
            ("nested_invocation_id", "bytea", "bytea", "YES", Some("")),
            ("parent_invocation_id", "bytea", "bytea", "NO", Some("")),
            ("call_site_id", "bytea", "bytea", "NO", Some("")),
            ("target_function_id", "bytea", "bytea", "YES", Some("")),
            ("source_revision_id", "bytea", "bytea", "YES", Some("")),
            ("catalogue_revision_id", "bytea", "bytea", "YES", Some("")),
            ("session_principal_id", "bytea", "bytea", "NO", Some("")),
            ("decision_outcome", "text", "text", "NO", Some("")),
            ("terminal_outcome", "text", "text", "NO", Some("")),
            ("item_count", "bigint", "int8", "YES", Some("")),
            ("byte_count", "bigint", "int8", "YES", Some("")),
        ],
    )
    .await?;

    for (constraint, expected) in [
        ("resource_audit_events_pkey", "PRIMARY KEY (sequence)"),
        ("resource_audit_events_event_id_key", "UNIQUE (event_id)"),
        (
            "resource_audit_events_request_id_key",
            "UNIQUE (request_id)",
        ),
        (
            "resource_audit_events_nested_invocation_id_key",
            "UNIQUE (nested_invocation_id)",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(event_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(request_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "nested_invocation_id IS NULL",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(nested_invocation_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(parent_invocation_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(call_site_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(session_principal_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(target_function_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(source_revision_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(catalogue_revision_id) = 16",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "nested_invocation_id IS NOT NULL",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "decision_outcome = 'denied'",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "terminal_outcome = ANY",
        ),
        (
            "resource_audit_events_target_pair_check",
            "(target_function_id IS NULL) = (source_revision_id IS NULL)",
        ),
        (
            "resource_audit_events_target_pair_check",
            "(target_function_id IS NULL) = (catalogue_revision_id IS NULL)",
        ),
        (
            "resource_audit_events_decision_outcome_check",
            "decision_outcome = ANY",
        ),
        (
            "resource_audit_events_terminal_outcome_check",
            "terminal_outcome = ANY",
        ),
        ("resource_audit_events_counts_check", "item_count >= 0"),
        ("resource_audit_events_counts_check", "byte_count >= 0"),
        (
            "resource_audit_events_target_fk",
            "FOREIGN KEY (catalogue_revision_id, target_function_id) REFERENCES _orna_kernel.invocation_target_authorities(catalogue_revision_id, function_id)",
        ),
        (
            "resource_audit_events_revision_pair_fk",
            "FOREIGN KEY (catalogue_revision_id, source_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id)",
        ),
        (
            "resource_audit_events_nested_invocation_fk",
            "FOREIGN KEY (nested_invocation_id) REFERENCES _orna_kernel.invocation_audit_events(invocation_id)",
        ),
    ] {
        require_constraint(client, "resource_audit_events", constraint, expected).await?;
    }

    let identity = client
        .query_one(
            "SELECT is_identity, identity_generation
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'resource_audit_events'
               AND column_name = 'sequence'",
            &[],
        )
        .await?;
    require(
        value::<String>(&identity, 0)? == "YES" && value::<String>(&identity, 1)? == "ALWAYS",
        "resource audit sequence is not an always-generated identity",
    )?;
    require_count(
        client,
        "_orna_kernel.resource_audit_events",
        "SELECT count(*) FROM _orna_kernel.resource_audit_events",
        0,
    )
    .await?;

    for privilege in [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
        "MAINTAIN",
    ] {
        let relation = "_orna_kernel.resource_audit_events";
        let row = client
            .query_one(
                "SELECT has_table_privilege('public', $1, $2)",
                &[&relation, &privilege],
            )
            .await?;
        let granted: bool = value(&row, 0)?;
        require(
            !granted,
            format!("PUBLIC has {privilege} on protected table {relation}"),
        )?;
    }

    for privilege in ["USAGE", "SELECT", "UPDATE"] {
        let row = client
            .query_one(
                "SELECT has_sequence_privilege(
                    'public',
                    '_orna_kernel.resource_audit_events_sequence_seq',
                    $1
                 )",
                &[&privilege],
            )
            .await?;
        require(
            !value::<bool>(&row, 0)?,
            format!("PUBLIC has {privilege} on the protected resource audit sequence"),
        )?;
    }

    Ok(())
}

async fn inspect_standard_catalogue_schema(client: &Client) -> TestResult<()> {
    for (table, expected_columns) in [
        (
            "standard_library_revisions",
            &[
                ("id", "bytea", "bytea", "NO", Some("")),
                ("source_revision_id", "bytea", "bytea", "NO", Some("")),
                ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
                ("digest_version", "smallint", "int2", "NO", Some("1")),
                ("language_version", "text", "text", "NO", Some("")),
                ("content_hash", "bytea", "bytea", "NO", Some("")),
                (
                    "hash_algorithm",
                    "text",
                    "text",
                    "NO",
                    Some("'sha256'::text"),
                ),
                (
                    "created_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ][..],
        ),
        (
            "standard_catalogue_schemas",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_value_types",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_id", "bytea", "bytea", "NO", Some("")),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("value_kind", "text", "text", "NO", Some("")),
                ("mutability", "text", "text", "NO", Some("")),
                ("persistence", "text", "text", "NO", Some("")),
                ("representation_contract", "text", "text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_enum_types",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_id", "bytea", "bytea", "NO", Some("")),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("labels", "ARRAY", "_text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_type_bindings",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_binding_id", "bytea", "bytea", "NO", Some("")),
                ("kind", "text", "text", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("target_type_id", "bytea", "bytea", "YES", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
                (
                    "target_type_kind",
                    "text",
                    "text",
                    "NO",
                    Some("'value'::text"),
                ),
                ("target_enum_type_id", "bytea", "bytea", "YES", Some("")),
            ][..],
        ),
    ] {
        inspect_columns(client, table, expected_columns).await?;
        require_count(
            client,
            table,
            &format!("SELECT count(*) FROM _orna_kernel.{table}"),
            0,
        )
        .await?;
    }

    inspect_column_contract(
        client,
        "catalogue_revisions",
        &[
            (
                "canonical_hash_version",
                "smallint",
                "int2",
                "NO",
                Some("1"),
            ),
            (
                "standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
        ],
    )
    .await?;
    inspect_column_contract(
        client,
        "function_revisions",
        &[("semantic_hash_version", "smallint", "int2", "NO", Some("1"))],
    )
    .await?;
    inspect_column_contract(
        client,
        "definition_references",
        &[(
            "target_standard_library_revision_id",
            "bytea",
            "bytea",
            "YES",
            Some(""),
        )],
    )
    .await?;

    let catalogue_version = client
        .query_one(
            "SELECT canonical_hash_version, standard_library_revision_id
             FROM _orna_kernel.catalogue_revisions",
            &[],
        )
        .await?;
    let canonical_hash_version: i16 = value(&catalogue_version, 0)?;
    let standard_library_revision_id: Option<Vec<u8>> = value(&catalogue_version, 1)?;
    require(
        canonical_hash_version == 1 && standard_library_revision_id.is_none(),
        format!(
            "application catalogue standard context is ({canonical_hash_version}, {standard_library_revision_id:?}); expected (1, NULL)"
        ),
    )?;
    let semantic_versions = client
        .query(
            "SELECT semantic_hash_version
             FROM _orna_kernel.function_revisions
             ORDER BY id",
            &[],
        )
        .await?;
    for row in semantic_versions {
        let semantic_hash_version: i16 = value(&row, 0)?;
        require(
            semantic_hash_version == 1,
            format!("function semantic hash version is {semantic_hash_version}; expected 1"),
        )?;
    }

    inspect_standard_catalogue_constraints(client).await?;
    inspect_standard_catalogue_indexes(client).await?;
    inspect_standard_catalogue_privileges(client).await
}

async fn inspect_resolved_value_storage(
    client: &Client,
    require_null_values: bool,
) -> TestResult<()> {
    for (table, columns) in [
        (
            "catalogue_fields",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_function_parameters",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_function_return_columns",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_functions",
            [
                "return_value_type_id",
                "return_standard_library_revision_id",
            ],
        ),
    ] {
        for column in columns {
            inspect_column_contract(
                client,
                table,
                &[(column, "bytea", "bytea", "YES", Some(""))],
            )
            .await?;
        }
        if require_null_values {
            let row = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM _orna_kernel.{table}
                         WHERE {} IS NOT NULL OR {} IS NOT NULL",
                        columns[0], columns[1]
                    ),
                    &[],
                )
                .await?;
            let non_null_rows: i64 = value(&row, 0)?;
            require(
                non_null_rows == 0,
                format!("{table} contains {non_null_rows} resolved value rows"),
            )?;
        }
    }

    for (table, constraint, expected_deferrable, expected_deferred) in [
        ("catalogue_fields", "cat_fields_val_pin_fk", true, true),
        (
            "catalogue_fields",
            "cat_fields_val_std_rev_len",
            false,
            false,
        ),
        ("catalogue_fields", "cat_fields_val_type_fk", true, true),
        ("catalogue_fields", "cat_fields_val_type_len", false, false),
        ("catalogue_fields", "catalogue_fields_check", false, false),
        (
            "catalogue_fields",
            "catalogue_fields_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_value_pin_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "catalogue_function_return_columns_check",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "catalogue_function_return_columns_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_check1",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_shape_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_value_pin_check",
            false,
            false,
        ),
    ] {
        if let Some((value_type_column, standard_revision_column, require_shape)) = match constraint
        {
            "catalogue_function_parameters_check"
            | "catalogue_function_parameters_value_pin_check" => Some((
                "value_type_id",
                "value_standard_library_revision_id",
                constraint == "catalogue_function_parameters_check",
            )),
            "catalogue_functions_check1" | "catalogue_functions_return_value_pin_check" => Some((
                "return_value_type_id",
                "return_standard_library_revision_id",
                constraint == "catalogue_functions_check1",
            )),
            _ => None,
        } {
            inspect_sealed_value_type_constraint(
                client,
                table,
                constraint,
                value_type_column,
                standard_revision_column,
                require_shape,
            )
            .await?;
            continue;
        }

        let definition =
            exact_resolved_type_constraint_definition(constraint).ok_or_else(|| {
                failure(format!(
                    "missing exact resolved-type contract for {constraint}"
                ))
            })?;
        require_exact_constraint(
            client,
            table,
            constraint,
            definition,
            expected_deferrable,
            expected_deferred,
        )
        .await?;
    }
    inspect_resolved_value_public_privileges(client).await
}

async fn inspect_sealed_value_type_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    value_type_column: &str,
    standard_revision_column: &str,
    require_shape: bool,
) -> TestResult<()> {
    let definition = constraint_definition(client, table, constraint).await?;
    let value_type_not_null = format!("{value_type_column} IS NOT NULL");
    let value_type_null = format!("{value_type_column} IS NULL");
    let value_type_exclusion = format!("{value_type_column} <> ALL");
    let value_type_inclusion = format!("{value_type_column} = ANY");
    let standard_revision_not_null = format!("{standard_revision_column} IS NOT NULL");
    let standard_revision_null = format!("{standard_revision_column} IS NULL");
    let shape_is_valid = !require_shape
        || (value_type_column == "value_type_id"
            && definition.contains("type_kind = 'scalar'::text")
            && definition.contains("type_kind = 'value'::text")
            && definition.contains("type_kind = 'enum'::text")
            && definition.contains("type_kind = 'record'::text"))
        || (value_type_column == "return_value_type_id"
            && definition.contains("return_shape = 'rows'::text")
            && definition.contains("return_shape = 'single'::text")
            && definition.contains("return_shape = 'stream'::text"));
    require(
        definition.contains(&value_type_not_null)
            && (!require_shape || definition.contains(&value_type_null))
            && definition.contains(&value_type_exclusion)
            && definition.contains(&value_type_inclusion)
            && definition.contains(&standard_revision_not_null)
            && definition.contains(&standard_revision_null)
            && definition.contains("decode('000000000000000000000000000000f3'::text, 'hex'::text)")
            && definition.contains("decode('000000000000000000000000000000ff'::text, 'hex'::text)")
            && shape_is_valid,
        format!(
            "{table} constraint {constraint} has an incomplete sealed value-type contract: {definition:?}"
        ),
    )
}

fn exact_resolved_type_constraint_definition(constraint: &str) -> Option<&'static str> {
    Some(match constraint {
        "catalogue_fields_type_kind_check"
        | "catalogue_function_parameters_type_kind_check"
        | "catalogue_function_return_columns_type_kind_check" => {
            "CHECK ((type_kind = ANY (ARRAY['scalar'::text, 'named'::text, 'reference'::text, 'value'::text, 'enum'::text, 'record'::text])))"
        }
        "catalogue_fields_check" | "catalogue_function_return_columns_check" => {
            "CHECK ((((type_kind = 'scalar'::text) AND (scalar_type IS NOT NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = ANY (ARRAY['named'::text, 'reference'::text])) AND (scalar_type IS NULL) AND (target_type_id IS NOT NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'value'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NOT NULL) AND (value_standard_library_revision_id IS NOT NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'enum'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NOT NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'record'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NOT NULL))))"
        }
        "catalogue_functions_return_type_kind_check" => {
            "CHECK ((return_type_kind = ANY (ARRAY['scalar'::text, 'named'::text, 'reference'::text, 'value'::text, 'enum'::text, 'record'::text])))"
        }
        "catalogue_functions_return_shape_check" => {
            "CHECK ((return_shape = ANY (ARRAY['single'::text, 'rows'::text, 'stream'::text])))"
        }
        "cat_fields_val_type_len"
        | "cat_fn_params_val_type_len"
        | "cat_fn_ret_cols_val_type_len" => {
            "CHECK (((value_type_id IS NULL) OR (octet_length(value_type_id) = 16)))"
        }
        "cat_fields_val_std_rev_len"
        | "cat_fn_params_val_std_rev_len"
        | "cat_fn_ret_cols_val_std_rev_len" => {
            "CHECK (((value_standard_library_revision_id IS NULL) OR (octet_length(value_standard_library_revision_id) = 16)))"
        }
        "cat_funcs_ret_val_type_len" => {
            "CHECK (((return_value_type_id IS NULL) OR (octet_length(return_value_type_id) = 16)))"
        }
        "cat_funcs_ret_val_std_rev_len" => {
            "CHECK (((return_standard_library_revision_id IS NULL) OR (octet_length(return_standard_library_revision_id) = 16)))"
        }
        "cat_fields_val_pin_fk" | "cat_fn_params_val_pin_fk" | "cat_fn_ret_cols_val_pin_fk" => {
            "FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_fields_val_type_fk" | "cat_fn_params_val_type_fk" | "cat_fn_ret_cols_val_type_fk" => {
            "FOREIGN KEY (value_standard_library_revision_id, value_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_funcs_ret_val_type_fk" => {
            "FOREIGN KEY (return_standard_library_revision_id, return_value_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_funcs_ret_val_pin_fk" => {
            "FOREIGN KEY (catalogue_revision_id, return_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED"
        }
        _ => return None,
    })
}

async fn inspect_resolved_enum_storage(
    client: &Client,
    require_null_values: bool,
) -> TestResult<()> {
    for (table, column, length_constraint, foreign_key) in [
        (
            "catalogue_fields",
            "enum_type_id",
            "cat_fields_enum_type_len",
            "cat_fields_enum_type_fk",
        ),
        (
            "catalogue_function_parameters",
            "enum_type_id",
            "cat_fn_params_enum_type_len",
            "cat_fn_params_enum_type_fk",
        ),
        (
            "catalogue_function_return_columns",
            "enum_type_id",
            "cat_fn_ret_cols_enum_type_len",
            "cat_fn_ret_cols_enum_type_fk",
        ),
        (
            "catalogue_functions",
            "return_enum_type_id",
            "cat_funcs_ret_enum_type_len",
            "cat_funcs_ret_enum_type_fk",
        ),
    ] {
        inspect_column_contract(
            client,
            table,
            &[(column, "bytea", "bytea", "YES", Some(""))],
        )
        .await?;
        if require_null_values {
            let row = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM _orna_kernel.{table} WHERE {column} IS NOT NULL"
                    ),
                    &[],
                )
                .await?;
            require(
                value::<i64>(&row, 0)? == 0,
                format!("{table} contains a resolved enum tuple"),
            )?;
        }
        require_exact_constraint(
            client,
            table,
            length_constraint,
            &format!("CHECK ((({column} IS NULL) OR (octet_length({column}) = 16)))"),
            false,
            false,
        )
        .await?;
        require_exact_constraint(
            client,
            table,
            foreign_key,
            &format!(
                "FOREIGN KEY (catalogue_revision_id, {column}) REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
            ),
            true,
            true,
        )
        .await?;
    }
    Ok(())
}

async fn inspect_record_value_storage(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "catalogue_record_value_types",
        &[
            ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
            ("type_id", "bytea", "bytea", "NO", Some("")),
            ("schema_id", "bytea", "bytea", "NO", Some("")),
            ("name_parts", "ARRAY", "_text", "NO", Some("")),
            ("value_kind", "text", "text", "NO", Some("")),
            ("mutability", "text", "text", "NO", Some("")),
            ("persistence", "text", "text", "NO", Some("")),
            ("source_unit_id", "bytea", "bytea", "NO", Some("")),
            ("source_start", "bigint", "int8", "NO", Some("")),
            ("source_end", "bigint", "int8", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "catalogue_record_value_fields",
        &[
            ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
            ("owner_type_id", "bytea", "bytea", "NO", Some("")),
            ("field_id", "bytea", "bytea", "NO", Some("")),
            ("name", "text", "text", "NO", Some("")),
            ("ordinal", "bigint", "int8", "NO", Some("")),
            ("type_kind", "text", "text", "NO", Some("")),
            ("value_type_id", "bytea", "bytea", "YES", Some("")),
            (
                "value_standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("enum_type_id", "bytea", "bytea", "YES", Some("")),
            ("source_unit_id", "bytea", "bytea", "NO", Some("")),
            ("source_start", "bigint", "int8", "NO", Some("")),
            ("source_end", "bigint", "int8", "NO", Some("")),
            (
                "enum_standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("standard_enum_type_id", "bytea", "bytea", "YES", Some("")),
            ("record_type_id", "bytea", "bytea", "YES", Some("")),
        ],
    )
    .await?;

    for (table, constraint, fragment) in [
        (
            "catalogue_record_value_types",
            "cat_record_value_types_value_kind_check",
            "value_kind = 'record'::text",
        ),
        (
            "catalogue_record_value_types",
            "cat_record_value_types_mutability_check",
            "mutability = 'immutable'::text",
        ),
        (
            "catalogue_record_value_types",
            "cat_record_value_types_persistence_check",
            "persistence = 'persistable'::text",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_kind_check",
            "type_kind = ANY (ARRAY['value'::text, 'enum'::text, 'record'::text])",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_check",
            "enum_standard_library_revision_id IS NOT NULL",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_check",
            "type_kind = 'record'::text) AND (value_type_id IS NULL)",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_std_rev_length",
            "octet_length(enum_standard_library_revision_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_std_enum_id_length",
            "octet_length(standard_enum_type_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_record_type_id_length",
            "octet_length(record_type_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_owner_fk",
            "REFERENCES _orna_kernel.catalogue_record_value_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_value_pin_fk",
            "REFERENCES _orna_kernel.catalogue_revisions",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_value_type_fk",
            "REFERENCES _orna_kernel.standard_catalogue_value_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_type_fk",
            "REFERENCES _orna_kernel.catalogue_enum_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_pin_fk",
            "REFERENCES _orna_kernel.catalogue_revisions",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_std_enum_fk",
            "REFERENCES _orna_kernel.standard_catalogue_enum_types",
        ),
    ] {
        require_constraint(client, table, constraint, fragment).await?;
    }

    require_exact_constraint(
        client,
        "catalogue_record_value_fields",
        "cat_record_value_fields_record_type_fk",
        "FOREIGN KEY (catalogue_revision_id, record_type_id) REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;

    for table in [
        "catalogue_record_value_types",
        "catalogue_record_value_fields",
    ] {
        require_count(
            client,
            &format!("_orna_kernel.{table}"),
            &format!("SELECT count(*) FROM _orna_kernel.{table}"),
            0,
        )
        .await?;
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            require(
                !value::<bool>(&row, 0)?,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }

    Ok(())
}

async fn inspect_resolved_value_public_privileges(client: &Client) -> TestResult<()> {
    for table in [
        "catalogue_fields",
        "catalogue_function_parameters",
        "catalogue_function_return_columns",
        "catalogue_functions",
    ] {
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }
    Ok(())
}

async fn inspect_columns(
    client: &Client,
    table: &str,
    expected_columns: &[(&str, &str, &str, &str, Option<&str>)],
) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT column_name, data_type, udt_name, is_nullable, column_default
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel' AND table_name = $1
             ORDER BY ordinal_position",
            &[&table],
        )
        .await?;
    let expected_names = expected_columns
        .iter()
        .map(|column| column.0)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let actual_names = rows
        .iter()
        .map(|row| value::<String>(row, 0))
        .collect::<TestResult<Vec<_>>>()?;
    for expected in expected_columns {
        let column = expected.0;
        let row = rows
            .iter()
            .find(|row| row.get::<_, String>(0) == column)
            .ok_or_else(|| failure(format!("missing {table}.{column}")))?;
        let actual = (
            value::<String>(row, 1)?,
            value::<String>(row, 2)?,
            value::<String>(row, 3)?,
            value::<Option<String>>(row, 4)?,
        );
        require(
            actual.0 == expected.1
                && actual.1 == expected.2
                && actual.2 == expected.3
                && match expected.4 {
                    Some("") => actual.3.is_none(),
                    Some(default) => actual.3.as_deref() == Some(default),
                    None => true,
                },
            format!(
                "{table}.{column} is ({:?}, {:?}, {:?}, {:?}); expected ({:?}, {:?}, {:?}, {:?})",
                actual.0,
                actual.1,
                actual.2,
                actual.3,
                expected.1,
                expected.2,
                expected.3,
                expected.4,
            ),
        )?;
    }
    require(
        actual_names == expected_names,
        format!("{table} columns differ: {actual_names:?}"),
    )
}

async fn inspect_column_contract(
    client: &Client,
    table: &str,
    expected_columns: &[(&str, &str, &str, &str, Option<&str>)],
) -> TestResult<()> {
    for expected in expected_columns {
        let row = client
            .query_opt(
                "SELECT data_type, udt_name, is_nullable, column_default
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = $1
                   AND column_name = $2",
                &[&table, &expected.0],
            )
            .await?
            .ok_or_else(|| failure(format!("missing {table}.{}", expected.0)))?;
        let actual = (
            value::<String>(&row, 0)?,
            value::<String>(&row, 1)?,
            value::<String>(&row, 2)?,
            value::<Option<String>>(&row, 3)?,
        );
        require(
            actual.0 == expected.1
                && actual.1 == expected.2
                && actual.2 == expected.3
                && match expected.4 {
                    Some("") => actual.3.is_none(),
                    Some(default) => actual.3.as_deref() == Some(default),
                    None => true,
                },
            format!(
                "{table}.{} is ({:?}, {:?}, {:?}, {:?}); expected ({:?}, {:?}, {:?}, {:?})",
                expected.0,
                actual.0,
                actual.1,
                actual.2,
                actual.3,
                expected.1,
                expected.2,
                expected.3,
                expected.4,
            ),
        )?;
    }
    Ok(())
}

fn exact_standard_catalogue_constraint_definition(constraint: &str) -> Option<&'static str> {
    Some(match constraint {
        "std_lib_rev_pkey" => "PRIMARY KEY (id)",
        "std_lib_rev_id_length" => "CHECK ((octet_length(id) = 16))",
        "std_lib_rev_source_revision_id_length" => {
            "CHECK ((octet_length(source_revision_id) = 16))"
        }
        "std_lib_rev_source_revision_key" => "UNIQUE (source_revision_id)",
        "std_lib_rev_source_revision_fk" => {
            "FOREIGN KEY (source_revision_id) REFERENCES _orna_kernel.source_revisions(id)"
        }
        "std_lib_rev_catalogue_revision_id_length" => {
            "CHECK ((octet_length(catalogue_revision_id) = 16))"
        }
        "std_lib_rev_catalogue_revision_key" => "UNIQUE (catalogue_revision_id)",
        "std_lib_rev_digest_version_check" => "CHECK ((digest_version = ANY (ARRAY[1, 2])))",
        "std_lib_rev_language_version_check" => "CHECK ((length(language_version) > 0))",
        "std_lib_rev_content_hash_length" => "CHECK ((octet_length(content_hash) = 32))",
        "std_lib_rev_hash_algorithm_check" => "CHECK ((hash_algorithm = 'sha256'::text))",
        "std_cat_schemas_pkey" => "PRIMARY KEY (standard_library_revision_id, schema_id)",
        "std_cat_schemas_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_schemas_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_schemas_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_schemas_name_parts_check" => {
            "CHECK (((cardinality(name_parts) > 0) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_schemas_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_schemas_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_schemas_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_value_types_pkey" => "PRIMARY KEY (standard_library_revision_id, type_id)",
        "std_cat_value_types_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_value_types_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_value_types_type_id_length" => "CHECK ((octet_length(type_id) = 16))",
        "std_cat_value_types_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_value_types_schema_fk" => {
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)"
        }
        "std_cat_value_types_name_parts_check" => {
            "CHECK (((cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_value_types_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_value_types_value_kind_check" => {
            "CHECK ((value_kind = ANY (ARRAY['primitive'::text, 'opaque'::text])))"
        }
        "std_cat_value_types_opaque_contract_check" => {
            "CHECK (((value_kind <> 'opaque'::text) OR ((persistence = 'transient'::text) AND (octet_length(representation_contract) <= 128) AND (representation_contract !~ '[^ -~]'::text))))"
        }
        "std_cat_value_types_mutability_check" => "CHECK ((mutability = 'immutable'::text))",
        "std_cat_value_types_persistence_check" => {
            "CHECK ((persistence = ANY (ARRAY['persistable'::text, 'transient'::text])))"
        }
        "std_cat_value_types_representation_contract_check" => {
            "CHECK ((length(representation_contract) > 0))"
        }
        "std_cat_value_types_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_value_types_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_enum_types_pkey" => "PRIMARY KEY (standard_library_revision_id, type_id)",
        "std_cat_enum_types_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_enum_types_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_enum_types_type_id_length" => "CHECK ((octet_length(type_id) = 16))",
        "std_cat_enum_types_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_enum_types_schema_fk" => {
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)"
        }
        "std_cat_enum_types_name_parts_check" => {
            "CHECK (((cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_enum_types_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_enum_types_labels_check" => {
            "CHECK (((cardinality(labels) > 0) AND (array_position(labels, NULL::text) IS NULL)))"
        }
        "standard_catalogue_enum_types_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "standard_catalogue_enum_types_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_type_bindings_pkey" => {
            "PRIMARY KEY (standard_library_revision_id, type_binding_id)"
        }
        "std_cat_type_bindings_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_type_bindings_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_type_bindings_type_binding_id_length" => {
            "CHECK ((octet_length(type_binding_id) = 16))"
        }
        "std_cat_type_bindings_kind_check" => {
            "CHECK ((kind = ANY (ARRAY['qualified'::text, 'prelude'::text])))"
        }
        "std_cat_type_bindings_name_parts_check" => {
            "CHECK ((((kind = 'qualified'::text) AND (cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)) OR ((kind = 'prelude'::text) AND (cardinality(name_parts) >= 1) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL))))"
        }
        "std_cat_type_bindings_name_key" => {
            "UNIQUE (standard_library_revision_id, kind, name_parts)"
        }
        "std_cat_type_bindings_target_type_id_length" => {
            "CHECK ((octet_length(target_type_id) = 16))"
        }
        "std_cat_type_bindings_target_type_fk" => {
            "FOREIGN KEY (standard_library_revision_id, target_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id)"
        }
        "std_cat_type_bindings_target_type_kind_check" => {
            "CHECK ((target_type_kind = ANY (ARRAY['value'::text, 'enum'::text])))"
        }
        "std_cat_type_bindings_target_shape_check" => {
            "CHECK ((((target_type_kind = 'value'::text) AND (target_type_id IS NOT NULL) AND (target_enum_type_id IS NULL)) OR ((target_type_kind = 'enum'::text) AND (target_type_id IS NULL) AND (target_enum_type_id IS NOT NULL))))"
        }
        "std_cat_type_bindings_target_enum_id_length" => {
            "CHECK (((target_enum_type_id IS NULL) OR (octet_length(target_enum_type_id) = 16)))"
        }
        "std_cat_type_bindings_target_enum_fk" => {
            "FOREIGN KEY (standard_library_revision_id, target_enum_type_id) REFERENCES _orna_kernel.standard_catalogue_enum_types(standard_library_revision_id, type_id)"
        }
        "std_cat_type_bindings_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_type_bindings_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "catalogue_revisions_canonical_hash_version_check" => {
            "CHECK ((canonical_hash_version = ANY (ARRAY[1, 2])))"
        }
        "catalogue_revisions_std_lib_rev_id_length" => {
            "CHECK (((standard_library_revision_id IS NULL) OR (octet_length(standard_library_revision_id) = 16)))"
        }
        "catalogue_revisions_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "catalogue_revisions_standard_context_check" => {
            "CHECK ((((canonical_hash_version = 1) AND (standard_library_revision_id IS NULL)) OR ((canonical_hash_version = 2) AND (standard_library_revision_id IS NOT NULL))))"
        }
        "catalogue_revisions_id_std_lib_rev_key" => "UNIQUE (id, standard_library_revision_id)",
        "function_revisions_semantic_hash_version_check" => {
            "CHECK ((semantic_hash_version = ANY (ARRAY[1, 2])))"
        }
        _ => return None,
    })
}

async fn inspect_standard_catalogue_constraints(client: &Client) -> TestResult<()> {
    for (table, constraint, _fragment) in [
        (
            "standard_library_revisions",
            "std_lib_rev_pkey",
            "PRIMARY KEY (id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_id_length",
            "octet_length(id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_id_length",
            "octet_length(source_revision_id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_key",
            "UNIQUE (source_revision_id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_fk",
            "FOREIGN KEY (source_revision_id) REFERENCES _orna_kernel.source_revisions(id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_catalogue_revision_id_length",
            "octet_length(catalogue_revision_id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_catalogue_revision_key",
            "UNIQUE (catalogue_revision_id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_digest_version_check",
            "digest_version = ANY (ARRAY[1, 2])",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_language_version_check",
            "length(language_version) > 0",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_content_hash_length",
            "octet_length(content_hash) = 32",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_hash_algorithm_check",
            "hash_algorithm = 'sha256'::text",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_pkey",
            "PRIMARY KEY (standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_name_parts_check",
            "cardinality(name_parts) > 0",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_type_id_length",
            "octet_length(type_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_schema_fk",
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_name_parts_check",
            "cardinality(name_parts) >= 2",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_value_kind_check",
            "value_kind = ANY (ARRAY['primitive'::text, 'opaque'::text])",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_opaque_contract_check",
            "value_kind <> 'opaque'::text",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_mutability_check",
            "mutability = 'immutable'::text",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_persistence_check",
            "persistence = ANY (ARRAY['persistable'::text, 'transient'::text])",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_representation_contract_check",
            "length(representation_contract) > 0",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_type_id_length",
            "octet_length(type_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_schema_fk",
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_name_parts_check",
            "cardinality(name_parts) >= 2",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_labels_check",
            "cardinality(labels) > 0",
        ),
        (
            "standard_catalogue_enum_types",
            "standard_catalogue_enum_types_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_enum_types",
            "standard_catalogue_enum_types_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_binding_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_type_binding_id_length",
            "octet_length(type_binding_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_kind_check",
            "kind = ANY (ARRAY['qualified'::text, 'prelude'::text])",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_name_parts_check",
            "kind = 'qualified'::text",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_name_key",
            "UNIQUE (standard_library_revision_id, kind, name_parts)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_id_length",
            "octet_length(target_type_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_fk",
            "FOREIGN KEY (standard_library_revision_id, target_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_kind_check",
            "target_type_kind = ANY (ARRAY['value'::text, 'enum'::text])",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_shape_check",
            "target_enum_type_id IS NOT NULL",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_enum_id_length",
            "octet_length(target_enum_type_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_enum_fk",
            "FOREIGN KEY (standard_library_revision_id, target_enum_type_id) REFERENCES _orna_kernel.standard_catalogue_enum_types(standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_canonical_hash_version_check",
            "canonical_hash_version = ANY (ARRAY[1, 2])",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_standard_context_check",
            "canonical_hash_version = 1",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_id_std_lib_rev_key",
            "UNIQUE (id, standard_library_revision_id)",
        ),
        (
            "function_revisions",
            "function_revisions_semantic_hash_version_check",
            "semantic_hash_version = ANY (ARRAY[1, 2])",
        ),
    ] {
        let expected_definition = exact_standard_catalogue_constraint_definition(constraint)
            .ok_or_else(|| failure(format!("missing exact standard contract for {constraint}")))?;
        require_exact_constraint(client, table, constraint, expected_definition, false, false)
            .await?;
    }

    require_no_foreign_key_to(
        client,
        "standard_library_revisions",
        "_orna_kernel.catalogue_revisions",
    )
    .await?;
    Ok(())
}

async fn inspect_standard_catalogue_indexes(client: &Client) -> TestResult<()> {
    for (index, relation, columns) in [
        (
            "catalogue_schemas_identity_index",
            "catalogue_schemas",
            "(schema_id, catalogue_revision_id)",
        ),
        (
            "catalogue_object_types_identity_index",
            "catalogue_object_types",
            "(type_id, catalogue_revision_id)",
        ),
        (
            "standard_catalogue_schemas_identity_index",
            "standard_catalogue_schemas",
            "(schema_id, standard_library_revision_id)",
        ),
        (
            "standard_catalogue_value_types_identity_index",
            "standard_catalogue_value_types",
            "(type_id, standard_library_revision_id)",
        ),
        (
            "standard_catalogue_type_bindings_identity_index",
            "standard_catalogue_type_bindings",
            "(type_binding_id, standard_library_revision_id)",
        ),
    ] {
        require_index_shape(client, index, relation, columns, None).await?;
    }
    require_index_shape(
        client,
        "definition_references_value_type_target_index",
        "definition_references",
        "(target_standard_library_revision_id, target_definition_id, catalogue_revision_id)",
        Some("(target_kind = 'value_type'::text)"),
    )
    .await
}

async fn inspect_standard_catalogue_privileges(client: &Client) -> TestResult<()> {
    for table in [
        "standard_library_revisions",
        "standard_catalogue_enum_types",
        "standard_catalogue_schemas",
        "standard_catalogue_value_types",
        "standard_catalogue_type_bindings",
    ] {
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }
    Ok(())
}

async fn inspect_migrations(client: &Client) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT version, name, checksum
             FROM _orna_kernel.schema_migrations
             ORDER BY version",
            &[],
        )
        .await?;
    require(
        rows.len() == MIGRATIONS.len(),
        format!(
            "migration count is {}; expected {}",
            rows.len(),
            MIGRATIONS.len()
        ),
    )?;

    for (row, (expected_version, expected_name, migration_sql)) in rows.iter().zip(MIGRATIONS) {
        let version: i64 = value(row, 0)?;
        let name: String = value(row, 1)?;
        let checksum: Vec<u8> = value(row, 2)?;
        require(
            version == *expected_version,
            format!("migration version is {version}; expected {expected_version}"),
        )?;
        require(
            name == *expected_name,
            format!("migration {version} name is {name:?}; expected {expected_name:?}"),
        )?;
        require(
            checksum == expected_migration_checksum(*expected_version, migration_sql),
            format!("migration {version} checksum does not match its registered contract"),
        )?;
        require(
            checksum.len() == 32,
            format!("migration {version} checksum is not 32 bytes"),
        )?;
    }
    Ok(())
}

fn expected_migration_checksum(version: i64, sql: &str) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(sql.as_bytes());
    if version == 4 {
        hash.update(MIGRATION_DATA_STEP_SEPARATOR);
        hash.update(CANONICAL_HASH_V1_EMPTY_SEED_STEP);
    }
    hash.finalize().to_vec()
}

fn require_database_constraint(
    error: &PostgresKernelError,
    expected_sqlstate: &str,
    expected_constraint: Option<&str>,
    context: &str,
) -> TestResult<()> {
    let PostgresKernelError::Database(error) = error else {
        return Err(failure(format!(
            "{context} produced a non-database failure: {error}"
        )));
    };
    let database_error = error
        .as_db_error()
        .ok_or_else(|| failure(format!("{context} has no PostgreSQL error fields: {error}")))?;
    require(
        database_error.code().code() == expected_sqlstate
            && database_error.constraint() == expected_constraint,
        format!(
            "{context} failed with SQLSTATE {} and constraint {:?}; expected {expected_sqlstate} and {expected_constraint:?}",
            database_error.code().code(),
            database_error.constraint(),
        ),
    )
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn snapshot_upgrade_state(database: &TestDatabase) -> TestResult<UpgradeSnapshot> {
    let session = database.open().await?;
    let snapshot_result = async {
        let active = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        let active_pair = (value(&active, 0)?, value(&active, 1)?);
        let source_unit_count = session
            .client()
            .query_one("SELECT count(*) FROM _orna_kernel.source_units", &[])
            .await?
            .get(0);
        let migrations = session
            .client()
            .query(
                "SELECT version, name, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(i64, String, Vec<u8>)>>>()?;
        let references = session
            .client()
            .query(
                "SELECT catalogue_revision_id, source_function_id,
                        source_function_revision_id, ordinal,
                        target_definition_id, target_kind, reference_kind,
                        source_subobject_id, source_unit_id, source_start,
                        source_end, target_owner_type_id,
                        target_owner_function_id, xmin::text
                 FROM _orna_kernel.definition_references
                 ORDER BY ordinal",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(DefinitionReferenceSnapshot {
                    catalogue_revision_id: value(row, 0)?,
                    source_function_id: value(row, 1)?,
                    source_function_revision_id: value(row, 2)?,
                    ordinal: value(row, 3)?,
                    target_definition_id: value(row, 4)?,
                    target_kind: value(row, 5)?,
                    reference_kind: value(row, 6)?,
                    source_subobject_id: value(row, 7)?,
                    source_unit_id: value(row, 8)?,
                    source_start: value(row, 9)?,
                    source_end: value(row, 10)?,
                    target_owner_type_id: value(row, 11)?,
                    target_owner_function_id: value(row, 12)?,
                    xmin: value(row, 13)?,
                })
            })
            .collect::<TestResult<Vec<DefinitionReferenceSnapshot>>>()?;
        let catalogue_hashes = session
            .client()
            .query(
                "SELECT id, content_hash
                 FROM _orna_kernel.catalogue_revisions
                 ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(Vec<u8>, Vec<u8>)>>>()?;
        let function_hashes = session
            .client()
            .query(
                "SELECT id, semantic_ir_hash
                 FROM _orna_kernel.function_revisions
                 ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(Vec<u8>, Vec<u8>)>>>()?;
        Ok(UpgradeSnapshot {
            active_pair,
            source_unit_count,
            migrations,
            references,
            catalogue_hashes,
            function_hashes,
        })
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "upgrade snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn snapshot_catalogue_surface(
    database: &TestDatabase,
) -> TestResult<CatalogueSurfaceSnapshot> {
    let session = database.open().await?;
    let snapshot_result = async {
        let relations_and_indexes = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname, relation.relkind::text
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname, relation.relname, relation.relkind",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(String, String, String)>>>()?;
        let triggers = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname, trigger_row.tgname,
                        trigger_row.tgisinternal
                 FROM pg_trigger AS trigger_row
                 JOIN pg_class AS relation ON relation.oid = trigger_row.tgrelid
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND NOT trigger_row.tgisinternal
                 ORDER BY namespace.nspname, relation.relname, trigger_row.tgname",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok((
                    value(row, 0)?,
                    value(row, 1)?,
                    value(row, 2)?,
                    value(row, 3)?,
                ))
            })
            .collect::<TestResult<Vec<(String, String, String, bool)>>>()?;
        let relation_acls = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname,
                        COALESCE(relation.relacl::text, '')
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname, relation.relname",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(String, String, String)>>>()?;
        let schema_acls = session
            .client()
            .query(
                "SELECT namespace.nspname, COALESCE(namespace.nspacl::text, '')
                 FROM pg_namespace AS namespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(String, String)>>>()?;
        Ok(CatalogueSurfaceSnapshot {
            relations_and_indexes,
            triggers,
            relation_acls,
            schema_acls,
        })
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "catalogue surface snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn snapshot_application_target_foreign_keys(
    database: &TestDatabase,
) -> TestResult<TargetForeignKeySnapshot> {
    let session = database.open().await?;
    let snapshot_result = session
        .client()
        .query(
            "SELECT relation.relname, constraint_row.conname,
                    pg_get_constraintdef(constraint_row.oid),
                    constraint_row.condeferrable,
                    constraint_row.condeferred
             FROM pg_constraint AS constraint_row
             JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
             JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
             WHERE namespace.nspname = '_orna_kernel'
               AND constraint_row.contype = 'f'
               AND (
                   (relation.relname = 'catalogue_fields'
                    AND constraint_row.conname = 'catalogue_fields_catalogue_revision_id_target_type_id_fkey')
                   OR (relation.relname = 'catalogue_function_parameters'
                    AND constraint_row.conname = 'catalogue_function_parameters_catalogue_revision_id_target_fkey')
                   OR (relation.relname = 'catalogue_function_return_columns'
                    AND constraint_row.conname = 'catalogue_function_return_col_catalogue_revision_id_target_fkey')
                   OR (relation.relname = 'catalogue_functions'
                    AND constraint_row.conname = 'catalogue_functions_catalogue_revision_id_return_target_ty_fkey')
               )
             ORDER BY relation.relname, constraint_row.conname",
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    Ok((
                        value(row, 0)?,
                        value(row, 1)?,
                        value(row, 2)?,
                        value(row, 3)?,
                        value(row, 4)?,
                    ))
                })
                .collect::<TestResult<Vec<(String, String, String, bool, bool)>>>()
                .map(TargetForeignKeySnapshot)
        })?;
    let shutdown_result = session.shutdown().await;
    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "target foreign-key snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

fn expected_application_target_foreign_keys() -> TargetForeignKeySnapshot {
    TargetForeignKeySnapshot(vec![
        (
            "catalogue_fields".to_owned(),
            "catalogue_fields_catalogue_revision_id_target_type_id_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_parameters".to_owned(),
            "catalogue_function_parameters_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_return_columns".to_owned(),
            "catalogue_function_return_col_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_functions".to_owned(),
            "catalogue_functions_catalogue_revision_id_return_target_ty_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, return_target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
    ])
}

fn expected_application_target_foreign_keys_after_sealed_inspector() -> TargetForeignKeySnapshot {
    TargetForeignKeySnapshot(vec![
        (
            "catalogue_fields".to_owned(),
            "catalogue_fields_catalogue_revision_id_target_type_id_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_parameters".to_owned(),
            "catalogue_function_parameters_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id_fk) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_return_columns".to_owned(),
            "catalogue_function_return_col_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_functions".to_owned(),
            "catalogue_functions_catalogue_revision_id_return_target_ty_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, return_target_type_id_fk) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
    ])
}

async fn inspect_empty_aggregate_hashes(client: &Client) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT
                bundle.id,
                bundle.content_hash,
                bundle.hash_algorithm,
                bundle.hash_contract_version,
                source.content_hash,
                source.hash_algorithm,
                source.hash_contract_version,
                catalogue.id,
                catalogue.content_hash,
                catalogue.hash_algorithm,
                catalogue.hash_contract_version
             FROM _orna_kernel.source_bundles AS bundle
             CROSS JOIN _orna_kernel.source_revisions AS source
             CROSS JOIN _orna_kernel.catalogue_revisions AS catalogue",
            &[],
        )
        .await?;
    let bundle = SourceBundleId::from_bytes(exact_id(value(&row, 0)?, "source bundle")?);
    let catalogue = CatalogueRevisionId::from_bytes(exact_id(value(&row, 7)?, "catalogue")?);
    let bundle_hash = source_bundle_digest(&[])?;
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash)?;
    let snapshot = CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new())?;
    let catalogue_hash = catalogue_digest(&snapshot, &[], &[], &[], &[])?;

    require(
        value::<Vec<u8>>(&row, 1)? == bundle_hash.to_bytes(),
        "source bundle does not store the canonical empty bundle hash",
    )?;
    require(
        value::<Vec<u8>>(&row, 4)? == source_hash.to_bytes(),
        "source revision does not store the canonical empty source revision hash",
    )?;
    require(
        value::<Vec<u8>>(&row, 8)? == catalogue_hash.to_bytes(),
        "catalogue revision does not store the canonical empty catalogue hash",
    )?;
    for (relation, algorithm_index, contract_version_index) in [
        ("source bundle", 2, 3),
        ("source revision", 5, 6),
        ("catalogue revision", 9, 10),
    ] {
        let hash_algorithm: String = value(&row, algorithm_index)?;
        let contract_version: i16 = value(&row, contract_version_index)?;
        require(
            hash_algorithm == "sha256",
            format!("{relation} hash algorithm is {hash_algorithm:?}; expected sha256"),
        )?;
        require(
            contract_version == 1,
            format!("{relation} hash contract version is {contract_version}; expected 1"),
        )?;
    }
    Ok(())
}

async fn inspect_hash_contract_columns(client: &Client) -> TestResult<()> {
    for table in HASH_CONTRACT_TABLES {
        let row = client
            .query_opt(
                "SELECT data_type, is_nullable, column_default
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = $1
                   AND column_name = 'hash_contract_version'",
                &[table],
            )
            .await?
            .ok_or_else(|| failure(format!("missing {table}.hash_contract_version")))?;
        let data_type: String = value(&row, 0)?;
        let is_nullable: String = value(&row, 1)?;
        let default: Option<String> = value(&row, 2)?;
        require(
            data_type == "smallint" && is_nullable == "NO" && default.as_deref() == Some("1"),
            format!(
                "{table}.hash_contract_version contract is ({data_type:?}, {is_nullable:?}, {default:?})"
            ),
        )?;
        require_constraint(
            client,
            table,
            &format!("{table}_hash_contract_version_check"),
            "hash_contract_version = 1",
        )
        .await?;
    }
    Ok(())
}

async fn inspect_origin_columns(client: &Client) -> TestResult<()> {
    let schema = "_orna_kernel";

    for table in ORIGIN_TABLES {
        let nullability = match *table {
            "catalogue_enum_types"
            | "catalogue_record_value_fields"
            | "catalogue_record_value_types"
            | "standard_catalogue_enum_types" => "NO",
            _ => "YES",
        };
        let expected_columns = BTreeSet::from([
            ("source_end".to_owned(), nullability.to_owned()),
            ("source_start".to_owned(), nullability.to_owned()),
            ("source_unit_id".to_owned(), nullability.to_owned()),
        ]);
        let rows = client
            .query(
                "SELECT column_name, is_nullable
                 FROM information_schema.columns
                 WHERE table_schema = $1
                   AND table_name = $2
                   AND column_name IN ('source_unit_id', 'source_start', 'source_end')",
                &[&schema, table],
            )
            .await?;
        let actual_columns = rows
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<BTreeSet<(String, String)>>>()?;
        require(
            actual_columns == expected_columns,
            format!("{table} source-origin columns differ: {actual_columns:?}"),
        )?;
        require_constraint(
            client,
            table,
            &format!("{table}_source_origin_check"),
            "CHECK",
        )
        .await?;
        require_constraint(
            client,
            table,
            &format!("{table}_source_unit_fk"),
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        )
        .await?;
    }
    Ok(())
}

async fn inspect_owner_qualified_catalogue_members(client: &Client) -> TestResult<()> {
    require_constraint(
        client,
        "catalogue_fields",
        "catalogue_fields_pkey",
        "PRIMARY KEY (catalogue_revision_id, owner_type_id, field_id)",
    )
    .await?;
    require_constraint(
        client,
        "catalogue_function_parameters",
        "catalogue_function_parameters_pkey",
        "PRIMARY KEY (catalogue_revision_id, function_id, parameter_id)",
    )
    .await
}

async fn inspect_definition_references(client: &Client) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT column_name, is_nullable
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'definition_references'
             ORDER BY ordinal_position",
            &[],
        )
        .await?;
    let actual_columns = rows
        .iter()
        .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
        .collect::<TestResult<Vec<(String, String)>>>()?;
    let expected_columns = vec![
        ("catalogue_revision_id".to_owned(), "NO".to_owned()),
        ("source_function_id".to_owned(), "NO".to_owned()),
        ("source_function_revision_id".to_owned(), "NO".to_owned()),
        ("ordinal".to_owned(), "NO".to_owned()),
        ("target_definition_id".to_owned(), "NO".to_owned()),
        ("target_kind".to_owned(), "NO".to_owned()),
        ("reference_kind".to_owned(), "NO".to_owned()),
        ("source_subobject_id".to_owned(), "YES".to_owned()),
        ("source_unit_id".to_owned(), "NO".to_owned()),
        ("source_start".to_owned(), "NO".to_owned()),
        ("source_end".to_owned(), "NO".to_owned()),
        ("target_owner_type_id".to_owned(), "YES".to_owned()),
        ("target_owner_function_id".to_owned(), "YES".to_owned()),
        (
            "target_standard_library_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_enum_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_field_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_field_owner_type_id".to_owned(),
            "YES".to_owned(),
        ),
    ];
    require(
        actual_columns == expected_columns,
        format!("definition_references columns differ: {actual_columns:?}"),
    )?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_catalogue_function_revision_fk",
        "FOREIGN KEY (catalogue_revision_id, source_function_id, source_function_revision_id) REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id, current_function_revision_id)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_function_revision_fk",
        "FOREIGN KEY (source_function_id, source_function_revision_id) REFERENCES _orna_kernel.function_revisions(function_id, id)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_source_unit_fk",
        "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_kind_check",
        "CHECK ((target_kind = ANY (ARRAY['object_type'::text, 'field'::text, 'record_field'::text, 'function'::text, 'parameter'::text, 'expression'::text, 'value_type'::text, 'enum_type'::text, 'record_type'::text])))",
        false,
        false,
    )
    .await?;
    let reference_kind_constraint = constraint_definition(
        client,
        "definition_references",
        "definition_references_reference_kind_check",
    )
    .await?;
    for reference_kind in [
        "function_call",
        "named_type",
        "object_reference",
        "parameter_read",
        "query_object",
        "query_field",
        "expression",
        "write_object",
        "write_field",
    ] {
        require(
            reference_kind_constraint.contains(&format!("'{reference_kind}'::text")),
            format!(
                "definition_references reference kind constraint omits {reference_kind:?}: {reference_kind_constraint:?}"
            ),
        )?;
    }

    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_type_id_check",
        "octet_length(target_owner_type_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_function_id_check",
        "octet_length(target_owner_function_id) = 16",
    )
    .await?;

    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_shape_check",
        "(target_kind = 'record_field'::text) AND (target_owner_type_id IS NULL) AND (target_record_field_owner_type_id IS NOT NULL)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_reference_target_compatibility_check",
        "(reference_kind = 'write_field'::text) AND (target_kind = ANY (ARRAY['field'::text, 'record_field'::text]))",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_field_target_fk",
        "FOREIGN KEY (catalogue_revision_id, target_owner_type_id, target_definition_id) REFERENCES _orna_kernel.catalogue_fields(catalogue_revision_id, owner_type_id, field_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_record_field_target_fk",
        "FOREIGN KEY (target_record_field_catalogue_revision_id, target_record_field_owner_type_id, target_definition_id) REFERENCES _orna_kernel.catalogue_record_value_fields(catalogue_revision_id, owner_type_id, field_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_parameter_target_fk",
        "FOREIGN KEY (catalogue_revision_id, target_owner_function_id, target_definition_id) REFERENCES _orna_kernel.catalogue_function_parameters(catalogue_revision_id, function_id, parameter_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_std_lib_rev_id_length",
        "CHECK (((target_standard_library_revision_id IS NULL) OR (octet_length(target_standard_library_revision_id) = 16)))",
        false,
        false,
    )
    .await?;
    let target_std_lib_rev_shape = constraint_definition(
        client,
        "definition_references",
        "definition_references_target_std_lib_rev_shape_check",
    )
    .await?;
    require(
        target_std_lib_rev_shape.contains("target_kind = 'value_type'::text")
            && target_std_lib_rev_shape.contains("target_standard_library_revision_id IS NOT NULL")
            && target_std_lib_rev_shape.contains("target_standard_library_revision_id IS NULL")
            && target_std_lib_rev_shape.contains("target_definition_id <> ALL")
            && target_std_lib_rev_shape.contains("target_definition_id = ANY")
            && target_std_lib_rev_shape
                .contains("decode('000000000000000000000000000000f3'::text, 'hex'::text)")
            && target_std_lib_rev_shape
                .contains("decode('000000000000000000000000000000ff'::text, 'hex'::text)"),
        format!(
            "definition_references sealed value-type shape is not closed: {target_std_lib_rev_shape:?}"
        ),
    )?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_catalogue_std_lib_rev_fk",
        "FOREIGN KEY (catalogue_revision_id, target_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_std_value_type_target_fk",
        "FOREIGN KEY (target_standard_library_revision_id, target_definition_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_enum_revision_length",
        "CHECK (((target_enum_catalogue_revision_id IS NULL) OR (octet_length(target_enum_catalogue_revision_id) = 16)))",
        false,
        false,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_enum_revision_shape",
        "CHECK ((((target_kind = 'enum_type'::text) AND (target_enum_catalogue_revision_id = catalogue_revision_id)) OR ((target_kind <> 'enum_type'::text) AND (target_enum_catalogue_revision_id IS NULL))))",
        false,
        false,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_enum_type_target_fk",
        "FOREIGN KEY (target_enum_catalogue_revision_id, target_definition_id) REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_revision_length",
        "octet_length(target_record_catalogue_revision_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_revision_shape",
        "target_kind = 'record_type'::text",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_record_type_target_fk",
        "FOREIGN KEY (target_record_catalogue_revision_id, target_definition_id) REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_field_revision_length",
        "octet_length(target_record_field_catalogue_revision_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_field_owner_type_id_check",
        "octet_length(target_record_field_owner_type_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_field_revision_shape",
        "target_kind = 'record_field'::text",
    )
    .await?;
    require_index(
        client,
        "definition_references_field_target_index",
        "(target_owner_type_id, target_definition_id, catalogue_revision_id) WHERE (target_kind = 'field'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_record_field_target_index",
        "(target_record_field_catalogue_revision_id, target_record_field_owner_type_id, target_definition_id) WHERE (target_kind = 'record_field'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_parameter_target_index",
        "(target_owner_function_id, target_definition_id, catalogue_revision_id) WHERE (target_kind = 'parameter'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_direct_target_index",
        "(target_kind, target_definition_id, catalogue_revision_id) WHERE (target_kind <> ALL (ARRAY['field'::text, 'record_field'::text, 'parameter'::text]))",
    )
    .await?;
    require_index(
        client,
        "definition_references_enum_type_target_index",
        "(target_enum_catalogue_revision_id, target_definition_id) WHERE (target_kind = 'enum_type'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_record_type_target_index",
        "(target_record_catalogue_revision_id, target_definition_id) WHERE (target_kind = 'record_type'::text)",
    )
    .await?;
    require_index_absent(client, "definition_references_target_index").await?;
    require_index_absent(client, "definition_references_owner_qualified_target_index").await?;
    Ok(())
}

async fn inspect_function_revision_constraints(client: &Client) -> TestResult<()> {
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_introduced_catalogue_revision_fk",
        "FOREIGN KEY (introduced_catalogue_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_introduced_function_fk",
        "FOREIGN KEY (introduced_catalogue_revision_id, function_id) REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_function_id_id_key",
        "UNIQUE (function_id, id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_function_content_semantic_key",
        "UNIQUE (function_id, content_hash, semantic_ir_hash)",
    )
    .await?;
    require_constraint_absent(
        client,
        "function_revisions",
        "function_revisions_function_id_content_hash_key",
    )
    .await?;
    require_constraint(
        client,
        "catalogue_functions",
        "catalogue_functions_current_revision_fk",
        "FOREIGN KEY (function_id, current_function_revision_id) REFERENCES _orna_kernel.function_revisions(function_id, id)",
    )
    .await
}

async fn require_constraint_absent(
    client: &Client,
    table: &str,
    constraint: &str,
) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT count(*)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?;
    let count: i64 = value(&row, 0)?;
    require(
        count == 0,
        format!("unexpected {table} constraint {constraint}"),
    )
}

async fn require_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    expected_fragment: &str,
) -> TestResult<()> {
    let definition = constraint_definition(client, table, constraint).await?;
    require(
        definition.contains(expected_fragment),
        format!(
            "{table} constraint {constraint} is {definition:?}; expected {expected_fragment:?}"
        ),
    )
}

async fn require_exact_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    expected_definition: &str,
    expected_deferrable: bool,
    expected_deferred: bool,
) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT pg_get_constraintdef(constraint_row.oid),
                    constraint_row.condeferrable,
                    constraint_row.condeferred
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?
        .ok_or_else(|| failure(format!("missing {table} constraint {constraint}")))?;
    let definition: String = value(&row, 0)?;
    let deferrable: bool = value(&row, 1)?;
    let deferred: bool = value(&row, 2)?;
    require(
        definition == expected_definition
            && deferrable == expected_deferrable
            && deferred == expected_deferred,
        format!(
            "{table} constraint {constraint} is ({definition:?}, deferrable={deferrable}, deferred={deferred}); expected ({expected_definition:?}, deferrable={expected_deferrable}, deferred={expected_deferred})"
        ),
    )
}

async fn require_index(client: &Client, index: &str, expected_fragment: &str) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT pg_get_indexdef(to_regclass($1))",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?
        .ok_or_else(|| failure(format!("missing index {index}")))?;
    let definition: Option<String> = value(&row, 0)?;
    let definition = definition.ok_or_else(|| failure(format!("missing index {index}")))?;
    require(
        definition.contains(expected_fragment),
        format!("index {index} is {definition:?}; expected {expected_fragment:?}"),
    )
}

async fn require_index_shape(
    client: &Client,
    index: &str,
    relation: &str,
    expected_columns: &str,
    expected_predicate: Option<&str>,
) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT index_class.relname,
                    index_namespace.nspname,
                    table_class.relname,
                    table_namespace.nspname,
                    pg_get_indexdef(index_row.indexrelid),
                    pg_get_expr(index_row.indpred, index_row.indrelid),
                    index_row.indisunique
             FROM pg_index AS index_row
             JOIN pg_class AS index_class
               ON index_class.oid = index_row.indexrelid
             JOIN pg_namespace AS index_namespace
               ON index_namespace.oid = index_class.relnamespace
             JOIN pg_class AS table_class
               ON table_class.oid = index_row.indrelid
             JOIN pg_namespace AS table_namespace
               ON table_namespace.oid = table_class.relnamespace
             WHERE index_row.indexrelid = to_regclass($1)",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?
        .ok_or_else(|| failure(format!("missing index {index}")))?;
    let actual_index: String = value(&row, 0)?;
    let actual_index_schema: String = value(&row, 1)?;
    let actual_relation: String = value(&row, 2)?;
    let actual_relation_schema: String = value(&row, 3)?;
    let definition: String = value(&row, 4)?;
    let predicate: Option<String> = value(&row, 5)?;
    let unique: bool = value(&row, 6)?;
    let expected_definition = format!(
        "CREATE INDEX {index} ON _orna_kernel.{relation} USING btree {expected_columns}{}",
        expected_predicate
            .map(|predicate| format!(" WHERE {predicate}"))
            .unwrap_or_default()
    );
    require(
        actual_index == index
            && actual_index_schema == "_orna_kernel"
            && actual_relation == relation
            && actual_relation_schema == "_orna_kernel"
            && !unique
            && definition == expected_definition,
        format!(
            "index {index} is ({actual_index_schema}.{actual_index} on {actual_relation_schema}.{actual_relation}, unique={unique}, definition={definition:?}); expected {expected_definition:?}"
        ),
    )?;
    require(
        predicate.as_deref() == expected_predicate,
        format!("index {index} predicate is {predicate:?}; expected {expected_predicate:?}"),
    )
}

async fn require_no_foreign_key_to(
    client: &Client,
    table: &str,
    target_table: &str,
) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT count(*)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.confrelid = to_regclass($2)
               AND constraint_row.contype = 'f'",
            &[&format!("_orna_kernel.{table}"), &target_table.to_owned()],
        )
        .await?;
    let count: i64 = value(&row, 0)?;
    require(
        count == 0,
        format!("{table} has {count} foreign keys to {target_table}; expected none"),
    )
}

async fn require_index_absent(client: &Client, index: &str) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT to_regclass($1)::text",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?;
    let relation: Option<String> = value(&row, 0)?;
    require(relation.is_none(), format!("unexpected index {index}"))
}

async fn constraint_definition(
    client: &Client,
    table: &str,
    constraint: &str,
) -> TestResult<String> {
    let row = client
        .query_opt(
            "SELECT pg_get_constraintdef(constraint_row.oid)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?
        .ok_or_else(|| failure(format!("missing {table} constraint {constraint}")))?;
    value(&row, 0)
}

async fn seed_initial_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let seed_result = seed_initial_catalogue_client(session.client()).await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "initial catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

struct RecordFieldTarget<'a> {
    name: &'a str,
    ordinal: u32,
    type_kind: &'a str,
    value_type_id: Option<&'a [u8]>,
    value_standard_library_revision_id: Option<&'a [u8]>,
    enum_type_id: Option<&'a [u8]>,
    enum_standard_library_revision_id: Option<&'a [u8]>,
    standard_enum_type_id: Option<&'a [u8]>,
    record_type_id: Option<&'a [u8]>,
}

impl<'a> RecordFieldTarget<'a> {
    fn record(name: &'a str, ordinal: u32, record_type_id: Option<&'a [u8]>) -> Self {
        Self {
            name,
            ordinal,
            type_kind: "record",
            value_type_id: None,
            value_standard_library_revision_id: None,
            enum_type_id: None,
            enum_standard_library_revision_id: None,
            standard_enum_type_id: None,
            record_type_id,
        }
    }

    fn value(
        name: &'a str,
        ordinal: u32,
        value_type_id: &'a [u8],
        standard_library_revision_id: &'a [u8],
    ) -> Self {
        Self {
            name,
            ordinal,
            type_kind: "value",
            value_type_id: Some(value_type_id),
            value_standard_library_revision_id: Some(standard_library_revision_id),
            enum_type_id: None,
            enum_standard_library_revision_id: None,
            standard_enum_type_id: None,
            record_type_id: None,
        }
    }

    fn application_enum(name: &'a str, ordinal: u32, enum_type_id: &'a [u8]) -> Self {
        Self {
            name,
            ordinal,
            type_kind: "enum",
            value_type_id: None,
            value_standard_library_revision_id: None,
            enum_type_id: Some(enum_type_id),
            enum_standard_library_revision_id: None,
            standard_enum_type_id: None,
            record_type_id: None,
        }
    }

    fn standard_enum(
        name: &'a str,
        ordinal: u32,
        standard_library_revision_id: &'a [u8],
        standard_enum_type_id: &'a [u8],
    ) -> Self {
        Self {
            name,
            ordinal,
            type_kind: "enum",
            value_type_id: None,
            value_standard_library_revision_id: None,
            enum_type_id: None,
            enum_standard_library_revision_id: Some(standard_library_revision_id),
            standard_enum_type_id: Some(standard_enum_type_id),
            record_type_id: None,
        }
    }
}

async fn verify_nested_record_field_target_storage(client: &Client) -> TestResult<()> {
    let bundle_id = SourceBundleId::from_bytes([0x21; 16]);
    let unit_id = SourceUnitId::from_bytes([0x22; 16]);
    let schema_id = SchemaId::from_bytes([0x23; 16]);
    let inner_type_id = TypeId::from_bytes([0x24; 16]);
    let outer_type_id = TypeId::from_bytes([0x25; 16]);
    let field_id = FieldId::from_bytes([0x26; 16]);
    let bundle = bundle_id.to_bytes().to_vec();
    let unit = unit_id.to_bytes().to_vec();
    let schema = schema_id.to_bytes().to_vec();
    let inner = inner_type_id.to_bytes().to_vec();
    let outer = outer_type_id.to_bytes().to_vec();
    let field = field_id.to_bytes().to_vec();
    let catalogue_revision_id: Vec<u8> = value(
        &client
            .query_one(
                "SELECT id FROM _orna_kernel.catalogue_revisions LIMIT 1",
                &[],
            )
            .await?,
        0,
    )?;
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.inner AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n";
    let stored_unit = StoredSourceUnit::new(
        unit_id,
        0,
        "records.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .map_err(|error| failure(format!("cannot seed the source unit: {error}")))?;
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&stored_unit))?;
    let bundle_hash = bundle_hash.to_bytes().to_vec();
    let unit_hash = source_unit_content_digest(source)
        .unwrap()
        .to_bytes()
        .to_vec();
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id, content_hash) VALUES ($1, $2)",
            &[&bundle, &bundle_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_units
                (id, bundle_id, ordinal, logical_path, content, content_hash)
             VALUES ($1, $2, 0, 'records.orna', $3, $4)",
            &[&unit, &bundle, &source, &unit_hash],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundle_units
                (bundle_id, source_unit_id, ordinal)
             VALUES ($1, $2, $3)",
            &[&bundle, &unit, &0_i64],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, ARRAY['app'], $3, 0, 12)",
            &[&catalogue_revision_id, &schema, &unit],
        )
        .await?;
    insert_record_value_type(
        client,
        &catalogue_revision_id,
        &inner,
        &schema,
        "inner",
        &unit,
    )
    .await?;
    insert_record_value_type(
        client,
        &catalogue_revision_id,
        &outer,
        &schema,
        "outer",
        &unit,
    )
    .await?;

    insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &field,
        &RecordFieldTarget {
            name: "child",
            ordinal: 0,
            type_kind: "record",
            value_type_id: None,
            value_standard_library_revision_id: None,
            enum_type_id: None,
            enum_standard_library_revision_id: None,
            standard_enum_type_id: None,
            record_type_id: Some(&inner),
        },
        &unit,
    )
    .await?;
    let row = client
        .query_one(
            "SELECT type_kind, record_type_id, value_type_id, enum_type_id
             FROM _orna_kernel.catalogue_record_value_fields
             WHERE catalogue_revision_id = $1 AND owner_type_id = $2 AND field_id = $3",
            &[&catalogue_revision_id, &outer, &field],
        )
        .await?;
    let kind: String = value(&row, 0)?;
    let record_type: Vec<u8> = value(&row, 1)?;
    let value_type: Option<Vec<u8>> = value(&row, 2)?;
    let enum_type: Option<Vec<u8>> = value(&row, 3)?;
    require(
        kind == "record" && record_type == inner && value_type.is_none() && enum_type.is_none(),
        format!(
            "record field did not round trip exactly: kind={kind} record_type={record_type:?} value={value_type:?} enum={enum_type:?}"
        ),
    )?;

    let legacy_value_field = FieldId::from_bytes([0x6b; 16]).to_bytes().to_vec();
    let legacy_enum_field = FieldId::from_bytes([0x6c; 16]).to_bytes().to_vec();
    let legacy_std_enum_field = FieldId::from_bytes([0x6d; 16]).to_bytes().to_vec();
    client.batch_execute("BEGIN").await?;
    let value_arm = insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &legacy_value_field,
        &RecordFieldTarget::value("legacy_value", 1, &[0xaa; 16], &[0xab; 16]),
        &unit,
    )
    .await;
    require(
        value_arm.is_ok(),
        "the replacement tuple check must accept the exact legacy value arm",
    )?;
    let enum_arm = insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &legacy_enum_field,
        &RecordFieldTarget::application_enum("legacy_enum", 2, &[0xac; 16]),
        &unit,
    )
    .await;
    require(
        enum_arm.is_ok(),
        "the replacement tuple check must accept the exact legacy application-enum arm",
    )?;
    let std_enum_arm = insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &legacy_std_enum_field,
        &RecordFieldTarget::standard_enum("legacy_std_enum", 3, &[0xab; 16], &[0xad; 16]),
        &unit,
    )
    .await;
    require(
        std_enum_arm.is_ok(),
        "the replacement tuple check must accept the exact legacy standard-enum arm",
    )?;
    client.batch_execute("ROLLBACK").await?;

    let mixed_fields = (0..9)
        .map(|index| {
            FieldId::from_bytes([0x30 + index as u8; 16])
                .to_bytes()
                .to_vec()
        })
        .collect::<Vec<_>>();
    for (index, (case, type_kind, value, value_std, enum_type, enum_std, std_enum, record_type)) in
        [
            (
                "record with value provenance",
                "record",
                Some(&inner[..]),
                None,
                None,
                None,
                None,
                Some(&inner[..]),
            ),
            (
                "record with value standard revision provenance",
                "record",
                None,
                Some(&inner[..]),
                None,
                None,
                None,
                Some(&inner[..]),
            ),
            (
                "record with application enum provenance",
                "record",
                None,
                None,
                Some(&inner[..]),
                None,
                None,
                Some(&inner[..]),
            ),
            (
                "record with standard enum revision provenance",
                "record",
                None,
                None,
                None,
                Some(&inner[..]),
                None,
                Some(&inner[..]),
            ),
            (
                "record with standard enum provenance",
                "record",
                None,
                None,
                None,
                None,
                Some(&inner[..]),
                Some(&inner[..]),
            ),
            (
                "record with no target provenance",
                "record",
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            (
                "value arm mixed with record provenance",
                "value",
                Some(&inner[..]),
                Some(&inner[..]),
                None,
                None,
                None,
                Some(&inner[..]),
            ),
            (
                "enum arm mixed with record provenance",
                "enum",
                None,
                None,
                Some(&inner[..]),
                None,
                None,
                Some(&inner[..]),
            ),
            (
                "standard enum arm mixed with record provenance",
                "enum",
                None,
                None,
                None,
                Some(&inner[..]),
                Some(&inner[..]),
                Some(&inner[..]),
            ),
        ]
        .into_iter()
        .enumerate()
    {
        let violation = insert_record_field(
            client,
            &catalogue_revision_id,
            &outer,
            &mixed_fields[index],
            &RecordFieldTarget {
                name: "mixed_child",
                ordinal: 0,
                type_kind,
                value_type_id: value,
                value_standard_library_revision_id: value_std,
                enum_type_id: enum_type,
                enum_standard_library_revision_id: enum_std,
                standard_enum_type_id: std_enum,
                record_type_id: record_type,
            },
            &unit,
        )
        .await;
        require_record_field_insert_violation(
            violation,
            "23514",
            "cat_record_value_fields_type_check",
            &format!("{case} must be rejected"),
        )?;
    }

    for (case, size) in [("fifteen byte", 15_usize), ("seventeen byte", 17_usize)] {
        let short_target = vec![0x41; size];
        let field_id = FieldId::from_bytes([0x40 + size as u8; 16])
            .to_bytes()
            .to_vec();
        let violation = insert_record_field(
            client,
            &catalogue_revision_id,
            &outer,
            &field_id,
            &RecordFieldTarget::record("length_child", 0, Some(&short_target)),
            &unit,
        )
        .await;
        require_record_field_insert_violation(
            violation,
            "23514",
            "cat_record_value_fields_record_type_id_length",
            &format!("{case} record_type_id must be rejected"),
        )?;
    }

    let dangling = vec![0x51; 16];
    let dangling_field = FieldId::from_bytes([0x52; 16]).to_bytes().to_vec();
    let violation = insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &dangling_field,
        &RecordFieldTarget::record("dangling_child", 10, Some(&dangling)),
        &unit,
    )
    .await;
    require_record_field_insert_violation(
        violation,
        "23503",
        "cat_record_value_fields_record_type_fk",
        "dangling record_type_id must fail at the implicit statement commit",
    )?;

    let deferred_dangling_field = FieldId::from_bytes([0x53; 16]).to_bytes().to_vec();
    client.batch_execute("BEGIN").await?;
    let deferred = insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &deferred_dangling_field,
        &RecordFieldTarget::record("dangling_child", 11, Some(&dangling)),
        &unit,
    )
    .await;
    require(
        deferred.is_ok(),
        "deferred dangling record field insert must defer its foreign key",
    )?;
    let commit = client.batch_execute("COMMIT").await;
    match commit {
        Ok(()) => Err(failure(
            "deferred dangling record field commit unexpectedly succeeded",
        )),
        Err(error) => {
            let database_error = error.as_db_error().ok_or_else(|| {
                failure(format!(
                    "deferred dangling record field commit produced a non-database failure: {error}"
                ))
            })?;
            require(
                database_error.code().code() == "23503"
                    && database_error.constraint()
                        == Some("cat_record_value_fields_record_type_fk"),
                format!(
                    "deferred dangling commit failed with SQLSTATE {} and constraint {:?}; expected 23503 and cat_record_value_fields_record_type_fk",
                    database_error.code().code(),
                    database_error.constraint(),
                ),
            )?;
            Ok(())
        }
    }?;
    client.batch_execute("ROLLBACK").await?;

    let second_bundle_id = SourceBundleId::from_bytes([0x61; 16]);
    let second_source_revision_id = SourceRevisionId::from_bytes([0x62; 16]);
    let second_revision_id = CatalogueRevisionId::from_bytes([0x63; 16]);
    let second_type_id = TypeId::from_bytes([0x64; 16]);
    let second_bundle = second_bundle_id.to_bytes().to_vec();
    let second_source_revision = second_source_revision_id.to_bytes().to_vec();
    let second_revision = second_revision_id.to_bytes().to_vec();
    let second_type = second_type_id.to_bytes().to_vec();
    let active_source_revision_id: Vec<u8> = value(
        &client
            .query_one(
                "SELECT source_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&catalogue_revision_id],
            )
            .await?,
        0,
    )?;
    let (bundle_content_hash, bundle_hash_algorithm): (Vec<u8>, String) = {
        let row = client
            .query_one(
                "SELECT bundle.content_hash, bundle.hash_algorithm
                 FROM _orna_kernel.source_revisions AS revision
                 JOIN _orna_kernel.source_bundles AS bundle ON bundle.id = revision.bundle_id
                 WHERE revision.id = $1",
                &[&active_source_revision_id],
            )
            .await?;
        (value(&row, 0)?, value(&row, 1)?)
    };
    let (source_revision_content_hash, source_revision_hash_algorithm): (Vec<u8>, String) = {
        let row = client
            .query_one(
                "SELECT content_hash, hash_algorithm
                 FROM _orna_kernel.source_revisions
                 WHERE id = $1",
                &[&active_source_revision_id],
            )
            .await?;
        (value(&row, 0)?, value(&row, 1)?)
    };
    let catalogue_revision_row = client
        .query_one(
            "SELECT content_hash, hash_algorithm, canonical_hash_version,
                    standard_library_revision_id, parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = $1",
            &[&catalogue_revision_id],
        )
        .await?;
    let catalogue_revision_content_hash: Vec<u8> = value(&catalogue_revision_row, 0)?;
    let catalogue_revision_hash_algorithm: String = value(&catalogue_revision_row, 1)?;
    let canonical_hash_version: i16 = value(&catalogue_revision_row, 2)?;
    let standard_library_revision_id: Option<Vec<u8>> = value(&catalogue_revision_row, 3)?;
    let parent_catalogue_revision_id: Option<Vec<u8>> = value(&catalogue_revision_row, 4)?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm)
             VALUES ($1, $2, $3)",
            &[&second_bundle, &bundle_content_hash, &bundle_hash_algorithm],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, bundle_id, content_hash, hash_algorithm)
             VALUES ($1, $2, $3, $4)",
            &[
                &second_source_revision,
                &second_bundle,
                &source_revision_content_hash,
                &source_revision_hash_algorithm,
            ],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, content_hash, hash_algorithm,
                 canonical_hash_version, standard_library_revision_id,
                 parent_catalogue_revision_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &second_revision,
                &second_source_revision,
                &catalogue_revision_content_hash,
                &catalogue_revision_hash_algorithm,
                &canonical_hash_version,
                &standard_library_revision_id,
                &parent_catalogue_revision_id,
            ],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, ARRAY['app'], $3, 0, 12)",
            &[&second_revision, &schema, &unit],
        )
        .await?;
    insert_record_value_type(
        client,
        &second_revision,
        &second_type,
        &schema,
        "second",
        &unit,
    )
    .await?;

    let cross_revision_field = FieldId::from_bytes([0x69; 16]).to_bytes().to_vec();
    client.batch_execute("BEGIN").await?;
    insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &cross_revision_field,
        &RecordFieldTarget::record("cross_revision", 4, Some(&second_type)),
        &unit,
    )
    .await?;
    let immediate = client
        .batch_execute(
            "SET CONSTRAINTS _orna_kernel.cat_record_value_fields_record_type_fk IMMEDIATE",
        )
        .await;
    match immediate {
        Ok(()) => Err(failure(
            "cross-revision record target unexpectedly passed SET CONSTRAINTS IMMEDIATE",
        )),
        Err(error) => {
            let database_error = error.as_db_error().ok_or_else(|| {
                failure(format!(
                    "cross-revision record target produced a non-database failure: {error}"
                ))
            })?;
            require(
                database_error.code().code() == "23503"
                    && database_error.constraint()
                        == Some("cat_record_value_fields_record_type_fk"),
                format!(
                    "cross-revision SET CONSTRAINTS IMMEDIATE failed with SQLSTATE {} and constraint {:?}; expected 23503 and cat_record_value_fields_record_type_fk",
                    database_error.code().code(),
                    database_error.constraint(),
                ),
            )?;
            Ok(())
        }
    }?;
    client.batch_execute("ROLLBACK").await?;

    let forward_field = FieldId::from_bytes([0x6a; 16]).to_bytes().to_vec();
    let forward_type = TypeId::from_bytes([0x6b; 16]).to_bytes().to_vec();
    client.batch_execute("BEGIN").await?;
    insert_record_field(
        client,
        &catalogue_revision_id,
        &outer,
        &forward_field,
        &RecordFieldTarget::record("forward_child", 5, Some(&forward_type)),
        &unit,
    )
    .await?;
    insert_record_value_type(
        client,
        &catalogue_revision_id,
        &forward_type,
        &schema,
        "forward",
        &unit,
    )
    .await?;
    client
        .batch_execute(
            "SET CONSTRAINTS _orna_kernel.cat_record_value_fields_record_type_fk IMMEDIATE",
        )
        .await?;
    client.batch_execute("COMMIT").await?;
    Ok(())
}

async fn insert_record_value_type(
    client: &Client,
    catalogue_revision_id: &[u8],
    type_id: &[u8],
    schema_id: &[u8],
    name: &str,
    source_unit_id: &[u8],
) -> TestResult<()> {
    let name_parts = vec!["app".to_owned(), name.to_owned()];
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_record_value_types
                (catalogue_revision_id, type_id, schema_id, name_parts,
                 value_kind, mutability, persistence,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, 'record', 'immutable', 'persistable', $5, 14, 30)",
            &[
                &catalogue_revision_id,
                &type_id,
                &schema_id,
                &name_parts,
                &source_unit_id,
            ],
        )
        .await?;
    Ok(())
}

async fn insert_record_field(
    client: &Client,
    catalogue_revision_id: &[u8],
    owner_type_id: &[u8],
    field_id: &[u8],
    target: &RecordFieldTarget<'_>,
    source_unit_id: &[u8],
) -> Result<u64, tokio_postgres::Error> {
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_record_value_fields
                (catalogue_revision_id, owner_type_id, field_id, name, ordinal, type_kind,
                 value_type_id, value_standard_library_revision_id, enum_type_id,
                 enum_standard_library_revision_id, standard_enum_type_id,
                 record_type_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 18, 30)",
            &[
                &catalogue_revision_id,
                &owner_type_id,
                &field_id,
                &target.name,
                &i64::from(target.ordinal),
                &target.type_kind,
                &target.value_type_id,
                &target.value_standard_library_revision_id,
                &target.enum_type_id,
                &target.enum_standard_library_revision_id,
                &target.standard_enum_type_id,
                &target.record_type_id,
                &source_unit_id,
            ],
        )
        .await
}

fn require_record_field_insert_violation(
    result: Result<u64, tokio_postgres::Error>,
    expected_sqlstate: &str,
    expected_constraint: &str,
    context: &str,
) -> TestResult<()> {
    match result {
        Ok(_) => Err(failure(format!("{context} unexpectedly accepted a row"))),
        Err(error) => {
            let database_error = error.as_db_error().ok_or_else(|| {
                failure(format!(
                    "{context} produced a non-database failure: {error}"
                ))
            })?;
            require(
                database_error.code().code() == expected_sqlstate
                    && database_error.constraint() == Some(expected_constraint),
                format!(
                    "{context} failed with SQLSTATE {} and constraint {:?}; expected {expected_sqlstate} and {expected_constraint:?}",
                    database_error.code().code(),
                    database_error.constraint(),
                ),
            )
        }
    }
}

async fn seed_registered_v2_empty_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let seed_result = async {
        seed_initial_catalogue_client(session.client()).await?;
        apply_and_register_migrations(session.client(), &MIGRATIONS[1..2]).await
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v2 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v3_empty_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let seed_result = async {
        seed_initial_catalogue_client(session.client()).await?;
        apply_and_register_migrations(session.client(), &MIGRATIONS[1..3]).await
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v3 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v4_semantic_catalogue(
    database: &TestDatabase,
    dangling_field_reference: bool,
) -> TestResult<()> {
    let fixture = registered_v4_semantic_fixture()?;
    let session = database.open().await?;
    let seed_result = async {
        seed_registered_v4_empty_catalogue_client(session.client()).await?;
        insert_registered_v4_semantic_rows(session.client(), &fixture).await?;
        if dangling_field_reference {
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.definition_references
                     SET target_definition_id = $1
                     WHERE ordinal = 0",
                    &[&vec![99_u8; 16]],
                )
                .await?;
        }
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v4 semantic catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v5_semantic_catalogue(database: &TestDatabase) -> TestResult<()> {
    seed_registered_v4_semantic_catalogue(database, false).await?;
    let session = database.open().await?;
    let migration = &MIGRATIONS[4];
    let seed_result = async {
        session.client().batch_execute(migration.2).await?;
        let checksum = expected_migration_checksum(migration.0, migration.2);
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                 VALUES ($1, $2, $3)",
                &[&migration.0, &migration.1, &checksum],
            )
            .await?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v5 semantic catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v6_catalogue(database: &TestDatabase) -> TestResult<()> {
    seed_registered_v5_semantic_catalogue(database).await?;
    seed_registered_v4_physical_catalogue(database).await?;
    let session = database.open().await?;
    let migration = &MIGRATIONS[5];
    let seed_result = async {
        session.client().batch_execute(migration.2).await?;
        let checksum = expected_migration_checksum(migration.0, migration.2);
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                 VALUES ($1, $2, $3)",
                &[&migration.0, &migration.1, &checksum],
            )
            .await?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v6 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v7_catalogue(database: &TestDatabase) -> TestResult<()> {
    seed_registered_v6_catalogue(database).await?;
    let fixture = registered_v7_rows_fixture()?;
    let function = fixture
        .catalogue()
        .functions()
        .get(1)
        .ok_or_else(|| failure("registered v7 fixture has no rows function"))?;
    let revision = fixture
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function.id())
        .ok_or_else(|| failure("registered v7 fixture has no rows revision"))?;
    let return_origin = fixture_origin(
        &fixture,
        DefinitionIdentity::FunctionReturnColumn {
            owner: function.id(),
            ordinal: 0,
        },
    )?;
    let session = database.open().await?;
    let migration = &MIGRATIONS[6];
    let seed_result = async {
        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED;")
            .await?;
        let update_result: TestResult<()> = async {
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_functions
                     SET return_shape = 'rows',
                         return_type_kind = NULL,
                         return_scalar_type = NULL,
                         return_target_type_id = NULL
                     WHERE catalogue_revision_id = $1 AND function_id = $2",
                    &[
                        &fixture.catalogue().revision().to_bytes().to_vec(),
                        &function.id().to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_function_return_columns
                        (catalogue_revision_id, function_id, name, ordinal,
                         type_kind, scalar_type, target_type_id,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, 'result', 0, 'scalar', 'boolean', NULL, $3, $4, $5)",
                    &[
                        &fixture.catalogue().revision().to_bytes().to_vec(),
                        &function.id().to_bytes().to_vec(),
                        &return_origin.source_unit().to_bytes().to_vec(),
                        &i64::from(return_origin.byte_start()),
                        &i64::from(return_origin.byte_end()),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET semantic_ir_hash = $2
                     WHERE id = $1",
                    &[
                        &revision.id().to_bytes().to_vec(),
                        &revision.semantic_hash().to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_revisions
                     SET content_hash = $2
                     WHERE id = $1",
                    &[
                        &fixture.catalogue().revision().to_bytes().to_vec(),
                        &fixture.catalogue_hash().to_bytes().to_vec(),
                    ],
                )
                .await?;
            Ok(())
        }
        .await;
        match update_result {
            Ok(()) => session.client().batch_execute("COMMIT").await?,
            Err(error) => {
                session.client().batch_execute("ROLLBACK").await?;
                return Err(error);
            }
        }
        session.client().batch_execute(migration.2).await?;
        let checksum = expected_migration_checksum(migration.0, migration.2);
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                 VALUES ($1, $2, $3)",
                &[&migration.0, &migration.1, &checksum],
            )
            .await?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v7 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v4_physical_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let result = session
        .client()
        .batch_execute(
            "CREATE SCHEMA IF NOT EXISTS _orna_data;
             CREATE TABLE _orna_data.t_06060606060606060606060606060606 (
                 _orna_object_id bytea NOT NULL,
                 CONSTRAINT pk_06060606060606060606060606060606
                     PRIMARY KEY (_orna_object_id),
                 CONSTRAINT ck_06060606060606060606060606060606_object_id
                     CHECK (octet_length(_orna_object_id) = 16),
                 f_08080808080808080808080808080808 boolean NOT NULL
             );
             REVOKE ALL ON TABLE _orna_data.t_06060606060606060606060606060606 FROM PUBLIC;
             CREATE TABLE _orna_data.t_07070707070707070707070707070707 (
                 _orna_object_id bytea NOT NULL,
                 CONSTRAINT pk_07070707070707070707070707070707
                     PRIMARY KEY (_orna_object_id),
                 CONSTRAINT ck_07070707070707070707070707070707_object_id
                     CHECK (octet_length(_orna_object_id) = 16)
             );
             REVOKE ALL ON TABLE _orna_data.t_07070707070707070707070707070707 FROM PUBLIC;",
        )
        .await;
    let shutdown_result = session.shutdown().await;

    match (result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(Box::new(error)),
        (Ok(()), Err(error)) => Err(error),
        (Err(create_error), Err(shutdown_error)) => Err(failure(format!(
            "registered v4 physical catalogue setup failed: {create_error}; setup driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_registered_v4_empty_catalogue_client(client: &Client) -> TestResult<()> {
    seed_initial_catalogue_client(client).await?;
    apply_and_register_migrations(client, &MIGRATIONS[1..4]).await
}

async fn apply_and_register_migrations(
    client: &Client,
    migrations: &[(i64, &str, &str)],
) -> TestResult<()> {
    client.batch_execute("BEGIN").await?;
    let apply_result: TestResult<()> = async {
        for (version, name, sql) in migrations {
            client.batch_execute(sql).await?;
            if *version == 4 {
                rewrite_registered_v4_empty_hashes(client).await?;
            }
            let checksum = expected_migration_checksum(*version, sql);
            client
                .execute(
                    "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                     VALUES ($1, $2, $3)",
                    &[version, name, &checksum],
                )
                .await?;
        }
        Ok(())
    }
    .await;

    match apply_result {
        Ok(()) => client.batch_execute("COMMIT").await.map_err(Into::into),
        Err(error) => {
            let rollback_result = client.batch_execute("ROLLBACK").await;
            match rollback_result {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(failure(format!(
                    "registered migration setup failed: {error}; rollback failed: {rollback_error}"
                ))),
            }
        }
    }
}

async fn rewrite_registered_v4_empty_hashes(client: &Client) -> TestResult<()> {
    let bundle = SourceBundleId::from_bytes([1_u8; 16]);
    let catalogue = CatalogueRevisionId::from_bytes([3_u8; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash)?;
    let snapshot = CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new())?;
    let catalogue_hash = catalogue_digest(&snapshot, &[], &[], &[], &[])?;

    client
        .execute(
            "UPDATE _orna_kernel.source_bundles SET content_hash = $1",
            &[&bundle_hash.to_bytes().to_vec()],
        )
        .await?;
    client
        .execute(
            "UPDATE _orna_kernel.source_revisions SET content_hash = $1",
            &[&source_hash.to_bytes().to_vec()],
        )
        .await?;
    client
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions SET content_hash = $1",
            &[&catalogue_hash.to_bytes().to_vec()],
        )
        .await?;
    Ok(())
}

fn registered_v4_semantic_fixture() -> TestResult<ActiveDatabaseRevision> {
    let bundle_id = SourceBundleId::from_bytes([1_u8; 16]);
    let source_revision_id = SourceRevisionId::from_bytes([2_u8; 16]);
    let catalogue_revision_id = CatalogueRevisionId::from_bytes([3_u8; 16]);
    let source_unit_id = SourceUnitId::from_bytes([4_u8; 16]);
    let schema_id = SchemaId::from_bytes([5_u8; 16]);
    let first_type_id = TypeId::from_bytes([6_u8; 16]);
    let second_type_id = TypeId::from_bytes([7_u8; 16]);
    let field_id = FieldId::from_bytes([8_u8; 16]);
    let first_function_id = FunctionId::from_bytes([9_u8; 16]);
    let second_function_id = FunctionId::from_bytes([10_u8; 16]);
    let first_revision_id = FunctionRevisionId::from_bytes([11_u8; 16]);
    let second_revision_id = FunctionRevisionId::from_bytes([12_u8; 16]);
    let parameter_id = ParameterId::from_bytes([13_u8; 16]);

    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "semantic.orna",
        REGISTERED_V4_SOURCE,
        source_unit_content_digest(REGISTERED_V4_SOURCE)?,
    )?;
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit))?;
    let source = StoredSourceRevision::new(
        bundle_id,
        source_revision_id,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(bundle_id, None, bundle_hash)?,
    )?;

    let schema = SchemaDefinition::new(schema_id, QualifiedSemanticName::new(["semantic"])?);
    let first_type = ObjectTypeDefinition::new(
        first_type_id,
        QualifiedSemanticName::new(["semantic", "first_type"])?,
        vec![FieldDefinition::new(
            field_id,
            "shared_field",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
            false,
            None,
            None,
        )],
    );
    let second_type = ObjectTypeDefinition::new(
        second_type_id,
        QualifiedSemanticName::new(["semantic", "second_type"])?,
        Vec::new(),
    );
    let first_function = FunctionDefinition::new(
        first_function_id,
        QualifiedSemanticName::new(["semantic", "first_function"])?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "shared_parameter",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
        first_revision_id,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Immutable,
    );
    let second_function = FunctionDefinition::new(
        second_function_id,
        QualifiedSemanticName::new(["semantic", "second_function"])?,
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
        second_revision_id,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        catalogue_revision_id,
        vec![schema],
        vec![first_type, second_type],
        vec![first_function, second_function],
    )?;

    let references = vec![
        DefinitionReference::new(
            first_function_id,
            first_revision_id,
            0,
            DefinitionReferenceTarget::Field {
                owner: first_type_id,
                field: field_id,
            },
            DefinitionReferenceKind::QueryField,
            fixture_source_origin(REGISTERED_V4_FIELD_REFERENCE)?,
        ),
        DefinitionReference::new(
            first_function_id,
            first_revision_id,
            1,
            DefinitionReferenceTarget::Parameter {
                owner: first_function_id,
                parameter: parameter_id,
            },
            DefinitionReferenceKind::ParameterRead,
            fixture_source_origin(REGISTERED_V4_PARAMETER_REFERENCE)?,
        ),
    ];
    let first_artifact_payload = b"first-server-plan-v1".to_vec();
    let first_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        first_artifact_payload.clone(),
        artifact_payload_digest(&first_artifact_payload)?,
    )?;
    let second_artifact_payload = b"second-server-plan-v1".to_vec();
    let second_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        second_artifact_payload.clone(),
        artifact_payload_digest(&second_artifact_payload)?,
    )?;
    let first_function = catalogue
        .function_by_id(first_function_id)
        .ok_or_else(|| failure("registered v4 fixture lost its first function"))?;
    let second_function = catalogue
        .function_by_id(second_function_id)
        .ok_or_else(|| failure("registered v4 fixture lost its second function"))?;
    let function_revisions = vec![
        FunctionRevisionRecord::new(
            first_function_id,
            first_revision_id,
            1,
            fixture_source_origin(REGISTERED_V4_FIRST_FUNCTION_DECLARATION)?,
            function_declaration_digest(REGISTERED_V4_FIRST_FUNCTION_DECLARATION.as_bytes())?,
            function_semantic_digest(first_function, "orna-1", &first_artifact, &[], &references)?,
            "orna-1",
            first_artifact,
        )?,
        FunctionRevisionRecord::new(
            second_function_id,
            second_revision_id,
            1,
            fixture_source_origin(REGISTERED_V4_SECOND_FUNCTION_DECLARATION)?,
            function_declaration_digest(REGISTERED_V4_SECOND_FUNCTION_DECLARATION.as_bytes())?,
            function_semantic_digest(second_function, "orna-1", &second_artifact, &[], &[])?,
            "orna-1",
            second_artifact,
        )?,
    ];
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            fixture_source_origin(REGISTERED_V4_SCHEMA_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(first_type_id),
            fixture_source_origin(REGISTERED_V4_FIRST_TYPE_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: first_type_id,
                field: field_id,
            },
            fixture_source_origin(REGISTERED_V4_FIELD_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(second_type_id),
            fixture_source_origin(REGISTERED_V4_SECOND_TYPE_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(first_function_id),
            fixture_source_origin(REGISTERED_V4_FIRST_FUNCTION_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: first_function_id,
                parameter: parameter_id,
            },
            fixture_source_origin(REGISTERED_V4_PARAMETER_DECLARATION)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(second_function_id),
            fixture_source_origin(REGISTERED_V4_SECOND_FUNCTION_DECLARATION)?,
        ),
    ];
    let catalogue_hash =
        catalogue_digest(&catalogue, &function_revisions, &[], &origins, &references)?;
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    Ok(ActiveDatabaseRevision::new_with_history(
        pair,
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        function_revisions,
        Vec::new(),
        origins,
        references,
    )?)
}

fn registered_v7_rows_fixture() -> TestResult<ActiveDatabaseRevision> {
    let base = registered_v4_semantic_fixture()?;
    let first_function = base
        .catalogue()
        .functions()
        .first()
        .ok_or_else(|| failure("registered v4 fixture has no first function"))?
        .clone();
    let second_function = base
        .catalogue()
        .functions()
        .get(1)
        .ok_or_else(|| failure("registered v4 fixture has no second function"))?;
    let second_rows_function = FunctionDefinition::new(
        second_function.id(),
        second_function.name().clone(),
        second_function.domain(),
        second_function.parameters().to_vec(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "result",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
        second_function.current_revision(),
        second_function.security(),
        second_function.transaction(),
        second_function.volatility(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        vec![first_function, second_rows_function.clone()],
    )?;
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::FunctionReturnColumn {
            owner: second_rows_function.id(),
            ordinal: 0,
        },
        fixture_source_origin(REGISTERED_V4_SECOND_FUNCTION_DECLARATION)?,
    ));
    let function_revisions = base
        .function_revisions()
        .iter()
        .map(|revision| {
            if revision.function() == second_rows_function.id() {
                let semantic_hash = function_semantic_digest(
                    &second_rows_function,
                    revision.language_version(),
                    revision.artifact(),
                    &[],
                    &[],
                )?;
                Ok(FunctionRevisionRecord::new(
                    revision.function(),
                    revision.id(),
                    revision.revision_number(),
                    revision.declaration_origin(),
                    revision.declaration_content_hash(),
                    semantic_hash,
                    revision.language_version(),
                    revision.artifact().clone(),
                )?)
            } else {
                Ok(revision.clone())
            }
        })
        .collect::<TestResult<Vec<_>>>()?;
    let catalogue_hash = catalogue_digest(
        &catalogue,
        &function_revisions,
        &[],
        &origins,
        base.references(),
    )?;
    Ok(ActiveDatabaseRevision::new_with_history(
        base.pair(),
        base.source().clone(),
        catalogue,
        catalogue_hash,
        base.expressions().to_vec(),
        function_revisions,
        base.historical_function_revisions().to_vec(),
        origins,
        base.references().to_vec(),
    )?)
}

fn fixture_source_origin(token: &str) -> TestResult<SourceOrigin> {
    let start = REGISTERED_V4_SOURCE
        .find(token)
        .ok_or_else(|| failure(format!("registered v4 source omits {token:?}")))?;
    let end = start + token.len();
    Ok(SourceOrigin::new(
        SourceUnitId::from_bytes([4_u8; 16]),
        u32::try_from(start)?,
        u32::try_from(end)?,
    )?)
}

fn fixture_origin(
    fixture: &ActiveDatabaseRevision,
    identity: DefinitionIdentity,
) -> TestResult<SourceOrigin> {
    fixture
        .origins()
        .iter()
        .find(|origin| origin.identity() == identity)
        .map(DefinitionOrigin::source)
        .ok_or_else(|| failure(format!("registered v4 fixture omits origin {identity:?}")))
}

fn legacy_reference_target(
    target: DefinitionReferenceTarget,
) -> TestResult<(Vec<u8>, &'static str)> {
    Ok(match target {
        DefinitionReferenceTarget::ObjectType(id) => (id.to_bytes().to_vec(), "object_type"),
        DefinitionReferenceTarget::Field { field, .. } => (field.to_bytes().to_vec(), "field"),
        DefinitionReferenceTarget::Function(id) => (id.to_bytes().to_vec(), "function"),
        DefinitionReferenceTarget::Parameter { parameter, .. } => {
            (parameter.to_bytes().to_vec(), "parameter")
        }
        other => {
            let DefinitionReferenceTarget::Expression(id) = other else {
                return Err(failure(
                    "registered v4 fixture cannot persist this definition reference target",
                ));
            };
            (id.to_bytes().to_vec(), "expression")
        }
    })
}

const SUPPORTED_REFERENCE_KINDS: &[(DefinitionReferenceKind, &str)] = &[
    (DefinitionReferenceKind::FunctionCall, "function_call"),
    (DefinitionReferenceKind::NamedType, "named_type"),
    (DefinitionReferenceKind::ObjectReference, "object_reference"),
    (DefinitionReferenceKind::ParameterRead, "parameter_read"),
    (DefinitionReferenceKind::QueryObject, "query_object"),
    (DefinitionReferenceKind::QueryField, "query_field"),
    (DefinitionReferenceKind::Expression, "expression"),
];

fn supported_reference_kind_sql(kind: DefinitionReferenceKind) -> TestResult<&'static str> {
    SUPPORTED_REFERENCE_KINDS
        .iter()
        .find(|(supported, _)| *supported == kind)
        .map(|(_, sql)| *sql)
        .ok_or_else(|| failure("unsupported definition reference kind in bootstrap fixture"))
}

async fn insert_registered_v4_semantic_rows(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
) -> TestResult<()> {
    client
        .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED;")
        .await?;
    let insert_result: TestResult<()> = async {
        persist_registered_v4_source(client, fixture).await?;
        persist_registered_v4_catalogue(client, fixture).await?;
        persist_registered_v4_function_revisions(client, fixture).await?;
        persist_registered_v4_references(client, fixture).await
    }
    .await;

    match insert_result {
        Ok(()) => client.batch_execute("COMMIT").await?,
        Err(error) => {
            let rollback_result = client.batch_execute("ROLLBACK").await;
            return match rollback_result {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(failure(format!(
                    "registered v4 semantic row setup failed: {error}; rollback failed: {rollback_error}"
                ))),
            };
        }
    }
    Ok(())
}

async fn persist_registered_v4_source(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let source = fixture.source();
    client
        .execute(
            "UPDATE _orna_kernel.source_bundles SET content_hash = $2 WHERE id = $1",
            &[
                &source.bundle().to_bytes().to_vec(),
                &source.bundle_hash().to_bytes().to_vec(),
            ],
        )
        .await?;
    client
        .execute(
            "UPDATE _orna_kernel.source_revisions SET content_hash = $2 WHERE id = $1",
            &[
                &source.id().to_bytes().to_vec(),
                &source.revision_hash().to_bytes().to_vec(),
            ],
        )
        .await?;
    for unit in source.units() {
        let logical_path = unit.logical_path();
        let content = unit.content();
        client
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &unit.id().to_bytes().to_vec(),
                    &source.bundle().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
                    &logical_path,
                    &content,
                    &unit.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn persist_registered_v4_catalogue(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let catalogue = fixture.catalogue();
    let catalogue_revision_id = catalogue.revision().to_bytes().to_vec();
    client
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions SET content_hash = $2 WHERE id = $1",
            &[
                &catalogue_revision_id,
                &fixture.catalogue_hash().to_bytes().to_vec(),
            ],
        )
        .await?;

    for schema in catalogue.schemas() {
        let origin = fixture_origin(fixture, DefinitionIdentity::Schema(schema.id()))?;
        let name_parts = schema.name().parts().to_vec();
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_schemas
                    (catalogue_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &catalogue_revision_id,
                    &schema.id().to_bytes().to_vec(),
                    &name_parts,
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
    }

    let schema_id = catalogue
        .schemas()
        .first()
        .ok_or_else(|| failure("registered v4 fixture has no schema"))?
        .id()
        .to_bytes()
        .to_vec();
    for object_type in catalogue.object_types() {
        let origin = fixture_origin(fixture, DefinitionIdentity::ObjectType(object_type.id()))?;
        let name_parts = object_type.name().parts().to_vec();
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_object_types
                    (catalogue_revision_id, type_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &catalogue_revision_id,
                    &object_type.id().to_bytes().to_vec(),
                    &schema_id,
                    &name_parts,
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
        persist_registered_v4_fields(client, fixture, object_type.id()).await?;
    }

    for function in catalogue.functions() {
        require(
            function.domain() == FunctionDomain::Server
                && function.security() == FunctionSecurity::Invoker
                && function.transaction() == Some(FunctionTransaction::Atomic)
                && function.volatility() == FunctionVolatility::Immutable
                && function.return_type()
                    == &FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            "registered v4 function differs from its persisted execution contract",
        )?;
        let origin = fixture_origin(fixture, DefinitionIdentity::Function(function.id()))?;
        let name_parts = function.name().parts().to_vec();
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                    (catalogue_revision_id, function_id, schema_id, name_parts,
                     domain, security_mode, transaction_mode, volatility,
                     return_shape, return_type_kind, return_scalar_type,
                     current_function_revision_id, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, 'server', 'invoker', 'atomic',
                         'immutable', 'single', 'scalar', 'void', $5, $6, $7, $8)",
                &[
                    &catalogue_revision_id,
                    &function.id().to_bytes().to_vec(),
                    &schema_id,
                    &name_parts,
                    &function.current_revision().to_bytes().to_vec(),
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
        persist_registered_v4_parameters(client, fixture, function.id()).await?;
    }
    Ok(())
}

async fn persist_registered_v4_fields(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
    owner: TypeId,
) -> TestResult<()> {
    let catalogue = fixture.catalogue();
    let object_type = catalogue
        .object_type_by_id(owner)
        .ok_or_else(|| failure("registered v4 fixture lost an object type"))?;
    for field in object_type.fields() {
        require(
            field.resolved_type() == ResolvedType::scalar(StandardScalar::Boolean)
                && field.default_expression().is_none()
                && field.on_delete().is_none(),
            "registered v4 field differs from its persisted scalar contract",
        )?;
        let origin = fixture_origin(
            fixture,
            DefinitionIdentity::Field {
                owner,
                field: field.id(),
            },
        )?;
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_fields
                    (catalogue_revision_id, owner_type_id, field_id, name,
                     ordinal, type_kind, scalar_type, nullable, is_unique,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, 'scalar', 'boolean', $6, $7,
                         $8, $9, $10)",
                &[
                    &catalogue.revision().to_bytes().to_vec(),
                    &owner.to_bytes().to_vec(),
                    &field.id().to_bytes().to_vec(),
                    &field.name(),
                    &i64::from(field.ordinal()),
                    &field.nullable(),
                    &field.unique(),
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn persist_registered_v4_parameters(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
    owner: FunctionId,
) -> TestResult<()> {
    let catalogue = fixture.catalogue();
    let function = catalogue
        .function_by_id(owner)
        .ok_or_else(|| failure("registered v4 fixture lost a function"))?;
    for parameter in function.parameters() {
        require(
            parameter.resolved_type() == ResolvedType::scalar(StandardScalar::Boolean)
                && parameter.default_expression().is_none(),
            "registered v4 parameter differs from its persisted scalar contract",
        )?;
        let origin = fixture_origin(
            fixture,
            DefinitionIdentity::Parameter {
                owner,
                parameter: parameter.id(),
            },
        )?;
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_function_parameters
                    (catalogue_revision_id, function_id, parameter_id, name,
                     ordinal, type_kind, scalar_type, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, 'scalar', 'boolean', $6, $7, $8)",
                &[
                    &catalogue.revision().to_bytes().to_vec(),
                    &owner.to_bytes().to_vec(),
                    &parameter.id().to_bytes().to_vec(),
                    &parameter.name(),
                    &i64::from(parameter.ordinal()),
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn persist_registered_v4_function_revisions(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let catalogue_revision_id = fixture.catalogue().revision().to_bytes().to_vec();
    for revision in fixture.function_revisions() {
        let language_version = revision.language_version();
        client
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                    (id, introduced_catalogue_revision_id, function_id,
                     revision_number, content_hash, semantic_ir_hash,
                     language_version, status)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
                &[
                    &revision.id().to_bytes().to_vec(),
                    &catalogue_revision_id,
                    &revision.function().to_bytes().to_vec(),
                    &i64::try_from(revision.revision_number())?,
                    &revision.declaration_content_hash().to_bytes().to_vec(),
                    &revision.semantic_hash().to_bytes().to_vec(),
                    &language_version,
                ],
            )
            .await?;
        let artifact = revision.artifact();
        require(
            artifact.kind() == ExecutableArtifactKind::Server,
            "registered v4 fixture has a non-server function artifact",
        )?;
        let format = artifact.format();
        let payload = artifact.payload().to_vec();
        client
            .execute(
                "INSERT INTO _orna_kernel.function_artifacts
                    (function_revision_id, artifact_kind, format,
                     format_version, payload, content_hash)
                 VALUES ($1, 'server_plan', $2, $3, $4, $5)",
                &[
                    &revision.id().to_bytes().to_vec(),
                    &format,
                    &i32::try_from(artifact.version())?,
                    &payload,
                    &artifact.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn persist_registered_v4_references(
    client: &Client,
    fixture: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let catalogue_revision_id = fixture.catalogue().revision().to_bytes().to_vec();
    for reference in fixture.references() {
        let (target_definition_id, target_kind) = legacy_reference_target(reference.target())?;
        let reference_kind = supported_reference_kind_sql(reference.kind())?;
        let origin = reference.source_origin();
        client
            .execute(
                "INSERT INTO _orna_kernel.definition_references
                    (catalogue_revision_id, source_function_id,
                     source_function_revision_id, ordinal,
                     target_definition_id, target_kind, reference_kind,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &catalogue_revision_id,
                    &reference.source_function().to_bytes().to_vec(),
                    &reference.source_revision().to_bytes().to_vec(),
                    &i64::from(reference.ordinal()),
                    &target_definition_id,
                    &target_kind,
                    &reference_kind,
                    &origin.source_unit().to_bytes().to_vec(),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await?;
    }
    Ok(())
}

async fn verify_owner_qualified_reference_backfill(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let verification_result =
        verify_owner_qualified_reference_backfill_client(session.client()).await;
    let shutdown_result = session.shutdown().await;

    match (verification_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(verification_error), Err(shutdown_error)) => Err(failure(format!(
            "owner-qualified reference verification failed: {verification_error}; verification driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn verify_owner_qualified_reference_backfill_client(client: &Client) -> TestResult<()> {
    inspect_migrations(client).await?;
    inspect_owner_qualified_catalogue_members(client).await?;
    inspect_definition_references(client).await?;

    let rows = client
        .query(
            "SELECT target_kind, target_definition_id,
                    target_owner_type_id, target_owner_function_id
             FROM _orna_kernel.definition_references
             ORDER BY ordinal",
            &[],
        )
        .await?;
    require(
        rows.len() == 2,
        format!("legacy reference count is {}; expected 2", rows.len()),
    )?;
    let field_kind: String = value(&rows[0], 0)?;
    let field_target: Vec<u8> = value(&rows[0], 1)?;
    let field_owner: Option<Vec<u8>> = value(&rows[0], 2)?;
    let field_function_owner: Option<Vec<u8>> = value(&rows[0], 3)?;
    require(
        field_kind == "field"
            && field_target == vec![8_u8; 16]
            && field_owner == Some(vec![6_u8; 16])
            && field_function_owner.is_none(),
        "legacy field reference did not receive its exact object-type owner",
    )?;
    let parameter_kind: String = value(&rows[1], 0)?;
    let parameter_target: Vec<u8> = value(&rows[1], 1)?;
    let parameter_type_owner: Option<Vec<u8>> = value(&rows[1], 2)?;
    let parameter_owner: Option<Vec<u8>> = value(&rows[1], 3)?;
    require(
        parameter_kind == "parameter"
            && parameter_target == vec![13_u8; 16]
            && parameter_type_owner.is_none()
            && parameter_owner == Some(vec![9_u8; 16]),
        "legacy parameter reference did not receive its exact function owner",
    )?;

    let catalogue_revision_id = vec![3_u8; 16];
    let second_type_id = vec![7_u8; 16];
    let field_id = vec![8_u8; 16];
    let source_function_id = vec![9_u8; 16];
    let second_function_id = vec![10_u8; 16];
    let source_function_revision_id = vec![11_u8; 16];
    let parameter_id = vec![13_u8; 16];
    let source_unit_id = vec![4_u8; 16];
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_fields
                (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                 type_kind, scalar_type, nullable, is_unique)
             VALUES ($1, $2, $3, 'duplicate_field_id', 0,
                     'scalar', 'uuid', false, false)",
            &[&catalogue_revision_id, &second_type_id, &field_id],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_function_parameters
                (catalogue_revision_id, function_id, parameter_id, name,
                 ordinal, type_kind, scalar_type)
             VALUES ($1, $2, $3, 'duplicate_parameter_id', 0,
                     'scalar', 'uuid')",
            &[&catalogue_revision_id, &second_function_id, &parameter_id],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id,
                 source_function_revision_id, ordinal, target_definition_id,
                 target_kind, reference_kind, target_owner_type_id,
                 target_owner_function_id, source_unit_id, source_start, source_end)
             VALUES
                ($1, $2, $3, 2, $4, 'field', 'query_field', $5, NULL, $6, 2, 3),
                ($1, $2, $3, 3, $7, 'parameter', 'parameter_read', NULL, $8, $6, 3, 4)",
            &[
                &catalogue_revision_id,
                &source_function_id,
                &source_function_revision_id,
                &field_id,
                &second_type_id,
                &source_unit_id,
                &parameter_id,
                &second_function_id,
            ],
        )
        .await?;

    require_count(
        client,
        "owner-qualified catalogue fields",
        "SELECT count(*) FROM _orna_kernel.catalogue_fields WHERE field_id = decode(repeat('08', 16), 'hex')",
        2,
    )
    .await?;
    require_count(
        client,
        "owner-qualified function parameters",
        "SELECT count(*) FROM _orna_kernel.catalogue_function_parameters WHERE parameter_id = decode(repeat('0d', 16), 'hex')",
        2,
    )
    .await?;
    require_count(
        client,
        "owner-qualified definition references",
        "SELECT count(*) FROM _orna_kernel.definition_references",
        4,
    )
    .await
}

async fn verify_write_reference_compatibility(client: &Client) -> TestResult<()> {
    let first_type_id = vec![6_u8; 16];
    let field_id = vec![8_u8; 16];

    client
        .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED;")
        .await?;
    let valid_result: TestResult<()> = async {
        insert_reference_probe(
            client,
            2,
            first_type_id.clone(),
            "object_type",
            "write_object",
            None,
            None,
        )
        .await?;
        insert_reference_probe(
            client,
            3,
            field_id.clone(),
            "field",
            "write_field",
            Some(first_type_id.clone()),
            None,
        )
        .await?;
        Ok(())
    }
    .await;
    let valid_rollback = client.batch_execute("ROLLBACK").await;
    match (valid_result, valid_rollback) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) => return Err(error),
        (Ok(()), Err(error)) => return Err(Box::new(error)),
        (Err(insert_error), Err(rollback_error)) => {
            return Err(failure(format!(
                "valid write-reference probe failed: {insert_error}; rollback failed: {rollback_error}"
            )));
        }
    }

    for (ordinal, target_id, target_kind, reference_kind, owner_type_id) in [
        (
            2,
            field_id.clone(),
            "field",
            "write_object",
            Some(first_type_id.clone()),
        ),
        (3, first_type_id.clone(), "object_type", "write_field", None),
    ] {
        client.batch_execute("BEGIN").await?;
        let insert_result = insert_reference_probe(
            client,
            ordinal,
            target_id,
            target_kind,
            reference_kind,
            owner_type_id,
            None,
        )
        .await;
        let rollback_result = client.batch_execute("ROLLBACK").await;
        let constraint = insert_result
            .as_ref()
            .err()
            .and_then(|error| error.as_db_error())
            .and_then(|error| error.constraint());
        require(
            insert_result.is_err(),
            format!("crossed {reference_kind}->{target_kind} write reference was accepted"),
        )?;
        require(
            constraint == Some("definition_references_reference_target_compatibility_check"),
            format!(
                "crossed {reference_kind}->{target_kind} write reference failed for {constraint:?}"
            ),
        )?;
        rollback_result?;
    }

    Ok(())
}

async fn insert_reference_probe(
    client: &Client,
    ordinal: i64,
    target_definition_id: Vec<u8>,
    target_kind: &str,
    reference_kind: &str,
    target_owner_type_id: Option<Vec<u8>>,
    target_owner_function_id: Option<Vec<u8>>,
) -> Result<u64, tokio_postgres::Error> {
    client
        .execute(
            "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id,
                 source_function_revision_id, ordinal, target_definition_id,
                 target_kind, reference_kind, target_owner_type_id,
                 target_owner_function_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, 1)",
            &[
                &vec![3_u8; 16],
                &vec![9_u8; 16],
                &vec![11_u8; 16],
                &ordinal,
                &target_definition_id,
                &target_kind,
                &reference_kind,
                &target_owner_type_id,
                &target_owner_function_id,
                &vec![4_u8; 16],
            ],
        )
        .await
}

async fn insert_unsupported_initial_schema(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let insert_result = session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts)
             VALUES ($1, $2, $3)",
            &[&vec![3_u8; 16], &vec![4_u8; 16], &vec!["manual".to_owned()]],
        )
        .await
        .map(|_| ())
        .map_err(|error| Box::new(error) as _);
    let shutdown_result = session.shutdown().await;

    match (insert_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(insert_error), Err(shutdown_error)) => Err(failure(format!(
            "unsupported initial schema insert failed: {insert_error}; insert driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn insert_ambiguous_legacy_field_target(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let insert_result = session
        .client()
        .batch_execute(
            "CREATE TABLE _orna_kernel.ambiguous_catalogue_fields ()
                 INHERITS (_orna_kernel.catalogue_fields);
             REVOKE ALL ON TABLE _orna_kernel.ambiguous_catalogue_fields FROM PUBLIC;
             INSERT INTO _orna_kernel.ambiguous_catalogue_fields
                 (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                  type_kind, scalar_type, nullable, is_unique)
             VALUES (
                 decode(repeat('03', 16), 'hex'),
                 decode(repeat('07', 16), 'hex'),
                 decode(repeat('08', 16), 'hex'),
                 'ambiguous_field', 0, 'scalar', 'boolean', false, false
             );",
        )
        .await
        .map_err(|error| Box::new(error) as _);
    let shutdown_result = session.shutdown().await;

    match (insert_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(insert_error), Err(shutdown_error)) => Err(failure(format!(
            "ambiguous legacy field setup failed: {insert_error}; setup driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn seed_initial_catalogue_client(client: &Client) -> TestResult<()> {
    create_migration_registry(client).await?;
    client.batch_execute(MIGRATIONS[0].2).await?;

    let checksum = Sha256::digest(MIGRATIONS[0].2.as_bytes()).to_vec();
    let version = MIGRATIONS[0].0;
    let name = MIGRATIONS[0].1;
    client
        .execute(
            "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
             VALUES ($1, $2, $3)",
            &[&version, &name, &checksum],
        )
        .await?;

    let bundle_id = vec![1_u8; 16];
    let source_revision_id = vec![2_u8; 16];
    let catalogue_revision_id = vec![3_u8; 16];
    client
        .execute(
            "INSERT INTO _orna_kernel.source_bundles (id) VALUES ($1)",
            &[&bundle_id],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.source_revisions (id, bundle_id) VALUES ($1, $2)",
            &[&source_revision_id, &bundle_id],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions (id, source_revision_id)
             VALUES ($1, $2)",
            &[&catalogue_revision_id, &source_revision_id],
        )
        .await?;
    client
        .execute(
            "INSERT INTO _orna_kernel.active_revision
                (singleton, source_revision_id, catalogue_revision_id)
             VALUES (true, $1, $2)",
            &[&source_revision_id, &catalogue_revision_id],
        )
        .await?;
    Ok(())
}

async fn reject_migration_history(
    version: i64,
    name: &'static str,
    checksum: Vec<u8>,
) -> TestResult<()> {
    with_test_database(|database| async move {
        seed_migration_record(&database, version, name, checksum).await?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        let error = kernel
            .bootstrap()
            .await
            .expect_err("invalid migration history must fail closed");
        require(
            matches!(
                error,
                PostgresKernelError::MigrationMismatch {
                    version: rejected_version
                } if rejected_version == version
            ),
            format!("migration {version} produced the wrong failure: {error}"),
        )
    })
    .await
}

async fn seed_migration_record(
    database: &TestDatabase,
    version: i64,
    name: &'static str,
    checksum: Vec<u8>,
) -> TestResult<()> {
    let session = database.open().await?;
    let seed_result = async {
        create_migration_registry(session.client()).await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.schema_migrations (version, name, checksum)
                 VALUES ($1, $2, $3)",
                &[&version, &name, &checksum],
            )
            .await?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (seed_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(seed_error), Err(shutdown_error)) => Err(failure(format!(
            "migration history seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn create_migration_registry(client: &Client) -> TestResult<()> {
    client
        .batch_execute(
            "CREATE SCHEMA _orna_kernel;
             REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;
             CREATE TABLE _orna_kernel.schema_migrations (
                 version bigint PRIMARY KEY CHECK (version > 0),
                 name text NOT NULL CHECK (length(name) > 0),
                 checksum bytea NOT NULL CHECK (octet_length(checksum) = 32),
                 applied_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp()
             );
             REVOKE ALL ON TABLE _orna_kernel.schema_migrations FROM PUBLIC;",
        )
        .await?;
    Ok(())
}

async fn inspect_v2_rollback(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let state = session
            .client()
            .query_one(
                "SELECT
                    (SELECT count(*) FROM _orna_kernel.schema_migrations),
                    (SELECT count(*) FROM _orna_kernel.source_bundles),
                    (SELECT count(*) FROM _orna_kernel.source_revisions),
                    (SELECT count(*) FROM _orna_kernel.catalogue_revisions),
                    (SELECT count(*) FROM _orna_kernel.active_revision),
                    (SELECT count(*) FROM _orna_kernel.catalogue_schemas)",
                &[],
            )
            .await?;
        let counts = (
            value::<i64>(&state, 0)?,
            value::<i64>(&state, 1)?,
            value::<i64>(&state, 2)?,
            value::<i64>(&state, 3)?,
            value::<i64>(&state, 4)?,
            value::<i64>(&state, 5)?,
        );
        require(
            counts == (1, 1, 1, 1, 1, 1),
            format!("v2 failure changed legacy row counts: {counts:?}"),
        )?;

        let columns = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name IN (
                       'source_bundles',
                       'source_revisions',
                       'catalogue_revisions'
                   )
                   AND column_name IN ('content_hash', 'hash_algorithm')",
                &[],
            )
            .await?;
        require(
            value::<i64>(&columns, 0)? == 0,
            "v2 schema changes survived a rejected legacy state",
        )
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "v2 rollback inspection failed: {inspection_error}; inspection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn inspect_v4_rollback(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let migration_row = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.schema_migrations WHERE version = 4",
                &[],
            )
            .await?;
        require(
            value::<i64>(&migration_row, 0)? == 0,
            "v4 migration record survived a failed data step",
        )?;
        let column_row = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = 'source_bundles'
                   AND column_name = 'hash_contract_version'",
                &[],
            )
            .await?;
        require(
            value::<i64>(&column_row, 0)? == 0,
            "v4 schema changes survived a failed data step",
        )
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "v4 rollback inspection failed: {inspection_error}; inspection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn inspect_v5_rollback(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let migration_row = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.schema_migrations WHERE version = 5",
                &[],
            )
            .await?;
        require(
            value::<i64>(&migration_row, 0)? == 0,
            "v5 migration record survived a failed owner backfill",
        )?;
        let column_row = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = 'definition_references'
                   AND column_name IN (
                       'target_owner_type_id',
                       'target_owner_function_id'
                   )",
                &[],
            )
            .await?;
        require(
            value::<i64>(&column_row, 0)? == 0,
            "v5 owner columns survived a failed owner backfill",
        )?;
        require_constraint(
            session.client(),
            "catalogue_fields",
            "catalogue_fields_pkey",
            "PRIMARY KEY (catalogue_revision_id, field_id)",
        )
        .await?;
        require_constraint(
            session.client(),
            "catalogue_function_parameters",
            "catalogue_function_parameters_pkey",
            "PRIMARY KEY (catalogue_revision_id, parameter_id)",
        )
        .await
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "v5 rollback inspection failed: {inspection_error}; inspection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn verify_function_revision_semantic_hash_uniqueness(client: &Client) -> TestResult<()> {
    let active = client
        .query_one(
            "SELECT catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true",
            &[],
        )
        .await?;
    let catalogue_revision_id: Vec<u8> = value(&active, 0)?;
    let schema_id = vec![4_u8; 16];
    let function_id = vec![5_u8; 16];
    let first_revision_id = vec![6_u8; 16];
    let second_revision_id = vec![7_u8; 16];
    let duplicate_revision_id = vec![8_u8; 16];
    let declaration_hash = vec![9_u8; 32];
    let first_semantic_hash = vec![10_u8; 32];
    let second_semantic_hash = vec![11_u8; 32];

    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts)
             VALUES ($1, $2, $3)",
            &[
                &catalogue_revision_id,
                &schema_id,
                &vec!["semantic".to_owned()],
            ],
        )
        .await?;
    client
        .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED;")
        .await?;
    let insert_result: TestResult<()> = async {
        client
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                    (catalogue_revision_id, function_id, schema_id, name_parts,
                     domain, security_mode, transaction_mode, volatility,
                     return_shape, current_function_revision_id)
                 VALUES ($1, $2, $3, $4, 'server', 'invoker', 'atomic', 'immutable', 'rows', $5)",
                &[
                    &catalogue_revision_id,
                    &function_id,
                    &schema_id,
                    &vec!["semantic".to_owned(), "work".to_owned()],
                    &first_revision_id,
                ],
            )
            .await?;
        insert_function_revision(
            client,
            &catalogue_revision_id,
            &function_id,
            &first_revision_id,
            1,
            &declaration_hash,
            &first_semantic_hash,
        )
        .await?;
        insert_function_revision(
            client,
            &catalogue_revision_id,
            &function_id,
            &second_revision_id,
            2,
            &declaration_hash,
            &second_semantic_hash,
        )
        .await?;
        Ok(())
    }
    .await;
    match insert_result {
        Ok(()) => client.batch_execute("COMMIT").await?,
        Err(error) => {
            let rollback_result = client.batch_execute("ROLLBACK").await;
            return match rollback_result {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(failure(format!(
                    "function revision setup failed: {error}; rollback failed: {rollback_error}"
                ))),
            };
        }
    }

    let duplicate_error = insert_function_revision(
        client,
        &catalogue_revision_id,
        &function_id,
        &duplicate_revision_id,
        3,
        &declaration_hash,
        &first_semantic_hash,
    )
    .await
    .expect_err("an exact function revision content-and-semantic tuple must be unique");
    require(
        duplicate_error
            .as_db_error()
            .and_then(|error| error.constraint())
            == Some("function_revisions_function_content_semantic_key"),
        format!("duplicate function revision tuple failed for the wrong reason: {duplicate_error}"),
    )?;
    let revisions = client
        .query_one(
            "SELECT count(*) FROM _orna_kernel.function_revisions WHERE function_id = $1",
            &[&function_id],
        )
        .await?;
    require(
        value::<i64>(&revisions, 0)? == 2,
        "function revisions with distinct semantic hashes were not both retained",
    )
}

async fn insert_function_revision(
    client: &Client,
    catalogue_revision_id: &[u8],
    function_id: &[u8],
    revision_id: &[u8],
    revision_number: i64,
    declaration_hash: &[u8],
    semantic_hash: &[u8],
) -> Result<u64, tokio_postgres::Error> {
    client
        .execute(
            "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, language_version, status)
             VALUES ($1, $2, $3, $4, $5, $6, 'test-v1', 'active')",
            &[
                &revision_id,
                &catalogue_revision_id,
                &function_id,
                &revision_number,
                &declaration_hash,
                &semantic_hash,
            ],
        )
        .await
}

async fn require_count(
    client: &Client,
    table: &str,
    statement: &str,
    expected: i64,
) -> TestResult<()> {
    let count_row = client.query_one(statement, &[]).await?;
    let count: i64 = value(&count_row, 0)?;
    require(
        count == expected,
        format!("{table} count is {count}; expected {expected}"),
    )
}

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn same_members<T: Eq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len() && left.iter().all(|member| right.contains(member))
}

fn value<T>(row: &Row, index: usize) -> TestResult<T>
where
    T: tokio_postgres::types::FromSqlOwned,
{
    Ok(row.try_get(index)?)
}

fn exact_id(bytes: Vec<u8>, identity: &str) -> TestResult<[u8; 16]> {
    let length = bytes.len();
    bytes.try_into().map_err(|_| {
        failure(format!(
            "{identity} identity is {length} bytes; expected 16"
        ))
    })
}
