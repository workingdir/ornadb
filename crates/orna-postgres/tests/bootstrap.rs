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
#[path = "bootstrap/inspection.rs"]
mod inspection;
use inspection::{
    expected_migration_checksum, inspect_definition_references, inspect_migrations,
    inspect_owner_qualified_catalogue_members, require_constraint,
};

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
    (
        47,
        "application migration ledger baseline",
        include_str!("../migrations/0047_application_migration_ledger_baseline.sql"),
    ),
];
const MIGRATION_DATA_STEP_SEPARATOR: &[u8] = b"\0orna.kernel.migration-step\0";
const CANONICAL_HASH_V1_EMPTY_SEED_STEP: &[u8] = b"canonical-hash-v1-empty-seed/v1";
const APPLICATION_MIGRATION_LEDGER_BASELINE_STEP: &[u8] =
    b"application-migration-ledger-baseline/v1";
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
        || relation.starts_with("application_migrations")
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
