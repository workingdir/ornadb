mod support;

use std::{
    error::Error,
    str::FromStr,
    time::{Duration, UNIX_EPOCH},
};

#[cfg(feature = "test-hooks")]
use orna_artifact::server_parameter_echo::ServerParameterEcho;
use orna_client::evaluate_client_function;
#[cfg(feature = "test-hooks")]
use orna_compiler::{
    STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
    STD_INVOKE_SOURCE_UNIT_ID, STD_TYPES_SOURCE_UNIT_ID,
};
use orna_compiler::{
    StandardApplicationCheckContext, check, check_standard_application, prepare,
    prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, InvocationId,
    ParameterId, PrincipalId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        CanonicalHashError, artifact_payload_digest, catalogue_digest,
        catalogue_digest_with_context, function_declaration_digest, function_semantic_digest,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction,
        FunctionVolatility, ObjectTypeDefinition, OnDeleteAction, ParameterDefinition,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition, TypeBinding, TypeLookupName, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        DeployableRevision, DurableCatalogueRevisionRole, EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionInvariantError, RevisionPair, Sha256Digest,
        SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    security::{
        ExecuteDecision, ExecuteDenial, ExecuteGrant, InvocationTarget,
        LocalPeerAuthenticationError, LocalPeerCredential, Principal, PrincipalKind,
        PrincipalStatus, PrivilegeClass, PrivilegeDenial, PrivilegeGrant, RoleMembership,
        SecurityAdminAuditOperation, SecurityAuditDecision, SecurityAuditDenial,
        SecurityAuditEvent, SecurityAuditKind, SecurityAuditOutcome, SecuritySnapshot,
        SecuritySnapshotError, SessionBindingError,
    },
    system::SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    value::RuntimeValue,
};
#[cfg(feature = "test-hooks")]
use orna_core::{
    StandardLibraryRevisionId,
    canonical_hash::verify_standard_library_v2_snapshot,
    catalogue::{PreludeTypeName, ValueTypeDefinition},
    revision::{DeployableRevisionContent, DeployableRevisionInput, StandardExecutable},
    security::SecurityFunctionTarget,
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};

const SCHEMA_SOURCE: &str = "schema café;\n";
const FROZEN_STANDARD_ENUM_DIGEST: [u8; 32] = [
    0xac, 0x5e, 0x03, 0x56, 0xb6, 0xb2, 0x5d, 0xae, 0x93, 0x07, 0x25, 0x3a, 0xba, 0x41, 0x57, 0x26,
    0xd2, 0xa3, 0xc2, 0xb4, 0xa8, 0xe9, 0xe2, 0x9a, 0x71, 0xad, 0xdf, 0xd4, 0xc8, 0xa8, 0x0f, 0xd5,
];
const STANDARD_CLIENT_SCHEMA_SOURCE: &str = "CREATE SCHEMA app;\n";
const STANDARD_CLIENT_TRUE_SOURCE: &str =
    "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
const STANDARD_CLIENT_TRUE_SOURCE_ONLY_EDIT: &str = "-- source-only formatting edit\n\
    CREATE SCHEMA app;\n\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
const STANDARD_CLIENT_FALSE_SOURCE: &str =
    "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;\n";
const STANDARD_ENUM_SOURCE: &str =
    "CREATE SCHEMA app;\nCREATE TYPE app.stage AS ENUM ('lead', 'owner''s', 'customer');\n";
const STANDARD_SECURITY_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;\n";
const TAMPERED_BOOLEAN_CONTRACT: &str = "orna.kernel.value.boolean@tampered";
const OBJECT_SOURCE: &str = "schema café;\nobject Α and object 世界;\nconstant π = 3;\n";
const UNSUPPORTED_FUNCTION_SQL: &str =
    "ALTER TABLE _orna_kernel.catalogue_functions DISABLE TRIGGER ALL;
     INSERT INTO _orna_kernel.catalogue_functions
        (catalogue_revision_id, function_id, schema_id, name_parts, domain,
         security_mode, transaction_mode, volatility, return_shape,
         return_type_kind, return_scalar_type, current_function_revision_id)
     SELECT catalogue_revision_id, decode(repeat('a1', 16), 'hex'),
            decode(repeat('a2', 16), 'hex'), ARRAY['unsupported', 'function'],
            'server', 'invoker', 'atomic', 'immutable', 'single',
            'scalar', 'void', decode(repeat('a3', 16), 'hex')
     FROM _orna_kernel.active_revision";

#[test]
fn supported_reference_kind_sql_maps_every_fixture_kind() -> TestResult<()> {
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
            (DefinitionReferenceKind::WriteObject, "write_object"),
            (DefinitionReferenceKind::WriteField, "write_field"),
        ]
    );
    for (kind, expected) in SUPPORTED_REFERENCE_KINDS {
        assert_eq!(supported_reference_kind_sql(*kind)?, *expected);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_the_exact_bootstrapped_revision_after_reconnecting() -> TestResult<()> {
    with_test_database(|database| async move {
        let seeded = kernel(&database)?.bootstrap().await?;

        let recovered = PostgresKernel::new(database.config()?).recover().await?;

        require(
            recovered.pair().source() == seeded.source()
                && recovered.pair().catalogue() == seeded.catalogue(),
            "recovery returned a different active revision pair",
        )?;
        require(
            recovered.source().units().is_empty()
                && recovered.catalogue().schemas().is_empty()
                && recovered.catalogue().object_types().is_empty()
                && recovered.catalogue().functions().is_empty()
                && recovered.function_revisions().is_empty()
                && recovered.historical_function_revisions().is_empty(),
            "the bootstrapped empty revision recovered invented members",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_and_evaluates_a_standard_boolean_client_function() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;

        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            format!(
                "schema-only compiler diagnostics: {:?}",
                schema_report.diagnostics()
            ),
        )?;
        let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
        let version_one = kernel.apply(&schema_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_client_candidate(STANDARD_CLIENT_TRUE_SOURCE, &version_two, &upgrade)?;
        let active = kernel.apply(&candidate).await?;
        let first = require_standard_client_execution(&active, &upgrade, true)?;

        let source_only_candidate =
            standard_client_candidate(STANDARD_CLIENT_TRUE_SOURCE_ONLY_EDIT, &active, &upgrade)?;
        require(
            source_only_candidate.new_function_revisions().is_empty(),
            "source-only standard CLIENT preparation allocated an immutable function revision",
        )?;
        let source_only = kernel.apply(&source_only_candidate).await?;
        let reused = require_standard_client_execution(&source_only, &upgrade, true)?;
        require(
            source_only.pair().source() != active.pair().source()
                && source_only.source() == source_only_candidate.source()
                && reused.function == first.function
                && reused.revision == first.revision,
            "source-only standard CLIENT apply changed source or immutable function facts",
        )?;

        let false_candidate =
            standard_client_candidate(STANDARD_CLIENT_FALSE_SOURCE, &source_only, &upgrade)?;
        require(
            false_candidate.new_function_revisions().len() == 1,
            "Boolean FALSE preparation did not allocate one immutable function revision",
        )?;
        let changed = kernel.apply(&false_candidate).await?;
        let false_result = require_standard_client_execution(&changed, &upgrade, false)?;
        require(
            false_result.function == first.function
                && false_result.revision.id() != first.revision.id()
                && first.revision.revision_number() == 1
                && false_result.revision.revision_number() == 2
                && false_result.revision.semantic_hash() != first.revision.semantic_hash()
                && changed.historical_function_revisions() == [first.revision.clone()],
            "Boolean FALSE standard CLIENT apply did not retain exact immutable history",
        )?;

        let restarted = PostgresKernel::new(database.config()?).recover().await?;
        let restarted_result = require_standard_client_execution(&restarted, &upgrade, false)?;
        require(
            restarted_result.pair == false_result.pair
                && restarted_result.function == false_result.function
                && restarted_result.revision == false_result.revision
                && restarted.historical_function_revisions()
                    == changed.historical_function_revisions(),
            "reconnect changed standard CLIENT active or historical facts",
        )?;

        let pointer = active_revision_pair(&database).await?;
        require(
            standard_boolean_contract(&database, &upgrade, Some(TAMPERED_BOOLEAN_CONTRACT)).await?
                == TAMPERED_BOOLEAN_CONTRACT,
            "Boolean standard contract tamper did not retain its exact database fact",
        )?;
        let tampered = snapshot_kernel_tables(&database).await?;
        let error = recovery_error(&database).await?;
        require_standard_library_digest_mismatch(
            &error,
            upgrade.verified_standard_snapshot().revision().to_bytes(),
        )?;
        require(
            active_revision_pair(&database).await? == pointer
                && snapshot_kernel_tables(&database).await? == tampered
                && standard_boolean_contract(&database, &upgrade, None).await?
                    == TAMPERED_BOOLEAN_CONTRACT,
            "rejected standard digest tamper repaired durable state",
        )?;
        require_no_session_leaks(&database).await?;
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_nested_record_field_targets_through_the_normal_apply_pipeline() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let empty = kernel_instance.recover().await?;

        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            format!(
                "schema-only compiler diagnostics: {:?}",
                schema_report.diagnostics()
            ),
        )?;
        let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
        let version_one = kernel_instance.apply(&schema_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_client_candidate(NESTED_RECORD_APPLICATION_SOURCE, &version_two, &upgrade)?;

        let records = candidate.candidate().record_value_types();
        if !(records.len() == 2
            && records[0].name().to_string() == "app.outer"
            && records[1].name().to_string() == "app.inner")
        {
            return Err(failure(format!(
                "nested candidate did not preserve source declaration order: {:?}",
                records
                    .iter()
                    .map(|record| record.name().to_string())
                    .collect::<Vec<_>>()
            )));
        }
        let outer = &records[0];
        let inner = &records[1];
        let child = outer
            .fields()
            .iter()
            .find(|field| field.name() == "child")
            .ok_or_else(|| failure("outer record has no child field"))?;
        let TypeDescriptorKind::Named(target) = child.descriptor().kind() else {
            return Err(failure(
                "child field descriptor is not a resolved Named identity",
            ));
        };
        require(
            target == inner.id(),
            "child field does not target the exact inner application record identity",
        )?;

        let applied = kernel_instance.apply(&candidate).await?;
        require_applied_revision_matches_candidate(&applied, &candidate)?;
        let post_apply = snapshot_kernel_tables(&database).await?;

        let first_restart = kernel(&database)?.recover().await?;
        require_applied_revision_matches_candidate(&first_restart, &candidate)?;
        require(
            snapshot_kernel_tables(&database).await? == post_apply,
            "the first fresh recovery wrote kernel rows",
        )?;
        let second_restart = PostgresKernel::new(database.config()?).recover().await?;
        require_applied_revision_matches_candidate(&second_restart, &candidate)?;
        require(
            snapshot_kernel_tables(&database).await? == post_apply,
            "the second fresh recovery wrote kernel rows",
        )?;
        require_applied_revisions_equal(&first_restart, &second_restart)
    })
    .await
}

const NESTED_RECORD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.mode AS ENUM ('lead', 'customer');\n\
    CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE app.inner AS VALUE (flag BOOLEAN, stage app.mode) IMMUTABLE PERSISTABLE;\n";

fn same_members<T>(left: &[T], right: &[T]) -> bool
where
    T: Eq,
{
    if left.len() != right.len() {
        return false;
    }
    let mut unmatched = right.iter().collect::<Vec<_>>();
    for member in left {
        let Some(index) = unmatched.iter().position(|candidate| *candidate == member) else {
            return false;
        };
        unmatched.swap_remove(index);
    }
    unmatched.is_empty()
}

fn require_same_standard_context(
    left: &CatalogueHashContext,
    right: &CatalogueHashContext,
) -> TestResult<()> {
    let (
        CatalogueHashContext::Version2 { standard: left },
        CatalogueHashContext::Version2 { standard: right },
    ) = (left, right)
    else {
        return Err(failure(
            "recovered and candidate standard contexts are not both version two",
        ));
    };
    require(
        left.revision() == right.revision()
            && left.digest() == right.digest()
            && left.source() == right.source()
            && left.catalogue().revision() == right.catalogue().revision()
            && same_members(left.catalogue().schemas(), right.catalogue().schemas())
            && same_members(
                left.catalogue().value_types(),
                right.catalogue().value_types(),
            )
            && same_members(
                left.catalogue().enum_types(),
                right.catalogue().enum_types(),
            )
            && same_members(
                left.catalogue().type_bindings(),
                right.catalogue().type_bindings(),
            ),
        "recovered and candidate pinned standard identity or content differ",
    )
}

fn require_applied_revision_matches_candidate(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    require(
        active.pair() == candidate.candidate_pair() && active.source() == candidate.source(),
        "post-apply recovery changed the candidate source pair",
    )?;
    require(
        active.catalogue().revision() == candidate.candidate().revision(),
        "post-apply recovery changed the candidate catalogue revision",
    )?;
    require(
        active.catalogue().schemas() == candidate.candidate().schemas(),
        format!(
            "post-apply recovery changed ordered schemas: {:?} vs {:?}",
            active.catalogue().schemas(),
            candidate.candidate().schemas(),
        ),
    )?;
    require(
        active.catalogue().object_types() == candidate.candidate().object_types(),
        format!(
            "post-apply recovery changed ordered object types: {:?} vs {:?}",
            active.catalogue().object_types(),
            candidate.candidate().object_types(),
        ),
    )?;
    require(
        active.catalogue().enum_types() == candidate.candidate().enum_types(),
        format!(
            "post-apply recovery changed ordered enum types: {:?} vs {:?}",
            active.catalogue().enum_types(),
            candidate.candidate().enum_types(),
        ),
    )?;
    {
        let active_records = active.catalogue().record_value_types();
        require(
            active_records
                .windows(2)
                .all(|pair| pair[0].id().to_bytes() <= pair[1].id().to_bytes()),
            "post-apply recovery did not emit record value types in canonical identity order",
        )?;
        let mut active_sorted = active_records.to_vec();
        let mut candidate_sorted = candidate.candidate().record_value_types().to_vec();
        active_sorted.sort_by_key(|record| record.id().to_bytes());
        candidate_sorted.sort_by_key(|record| record.id().to_bytes());
        require(
            active_sorted == candidate_sorted,
            format!(
                "post-apply recovery changed record value types in canonical identity order: {:?} vs {:?}",
                active_sorted, candidate_sorted,
            ),
        )?;
    }
    require(
        active.catalogue().value_types() == candidate.candidate().value_types(),
        format!(
            "post-apply recovery changed ordered value types: {:?} vs {:?}",
            active.catalogue().value_types(),
            candidate.candidate().value_types(),
        ),
    )?;
    require(
        active.catalogue().type_bindings() == candidate.candidate().type_bindings(),
        format!(
            "post-apply recovery changed ordered type bindings: {:?} vs {:?}",
            active.catalogue().type_bindings(),
            candidate.candidate().type_bindings(),
        ),
    )?;
    require(
        active.catalogue().functions() == candidate.candidate().functions(),
        format!(
            "post-apply recovery changed ordered functions: {:?} vs {:?}",
            active.catalogue().functions(),
            candidate.candidate().functions(),
        ),
    )?;
    require(
        active.catalogue_hash() == candidate.catalogue_hash()
            && active.expressions() == candidate.expressions()
            && same_members(active.origins(), candidate.origins())
            && same_members(active.references(), candidate.references())
            && active.function_revisions() == candidate.current_function_revisions().unwrap_or(&[]),
        "post-apply recovery changed candidate hashes, evidence, or function revisions",
    )?;
    require_same_standard_context(
        active.catalogue_hash_context(),
        candidate.catalogue_hash_context(),
    )
}

fn require_applied_revisions_equal(
    left: &ActiveDatabaseRevision,
    right: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require(
        left.pair() == right.pair()
            && left.source() == right.source()
            && left.catalogue().revision() == right.catalogue().revision()
            && left.catalogue().schemas() == right.catalogue().schemas()
            && left.catalogue().object_types() == right.catalogue().object_types()
            && left.catalogue().enum_types() == right.catalogue().enum_types()
            && left.catalogue().record_value_types() == right.catalogue().record_value_types()
            && left.catalogue().value_types() == right.catalogue().value_types()
            && left.catalogue().type_bindings() == right.catalogue().type_bindings()
            && left.catalogue().functions() == right.catalogue().functions()
            && left.catalogue_hash() == right.catalogue_hash()
            && left.expressions() == right.expressions()
            && same_members(left.origins(), right.origins())
            && same_members(left.references(), right.references())
            && left.function_revisions() == right.function_revisions(),
        "two fresh kernels recovered different active revisions",
    )?;
    require_same_standard_context(
        left.catalogue_hash_context(),
        right.catalogue_hash_context(),
    )
}

struct NestedRecordPipeline {
    candidate: DeployableRevision,
    standard_revision: Vec<u8>,
}

async fn install_nested_record_pipeline(
    database: &TestDatabase,
    source: &str,
) -> TestResult<NestedRecordPipeline> {
    let kernel_instance = kernel(database)?;
    kernel_instance.bootstrap().await?;
    let empty = kernel_instance.recover().await?;
    let schema_bundle =
        orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
            "main.orna",
            STANDARD_CLIENT_SCHEMA_SOURCE,
        )])?;
    let schema_report = check(&schema_bundle, empty.catalogue());
    require(
        schema_report.diagnostics().is_empty(),
        format!(
            "schema-only compiler diagnostics: {:?}",
            schema_report.diagnostics()
        ),
    )?;
    let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
    let version_one = kernel_instance.apply(&schema_candidate).await?;
    let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
    let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
    let candidate = standard_client_candidate(source, &version_two, &upgrade)?;
    kernel_instance.apply(&candidate).await?;
    Ok(NestedRecordPipeline {
        candidate,
        standard_revision: upgrade
            .verified_standard_snapshot()
            .revision()
            .to_bytes()
            .to_vec(),
    })
}

fn bytea_literal(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    format!(
        "'\\x{}'::bytea",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn field_where(owner_type_id: impl AsRef<[u8]>, field_id: impl AsRef<[u8]>) -> String {
    let owner_type_id = owner_type_id.as_ref();
    let field_id = field_id.as_ref();
    format!(
        "owner_type_id = {} AND field_id = {}",
        bytea_literal(owner_type_id),
        bytea_literal(field_id)
    )
}

fn find_record_field<'a>(
    candidate: &'a DeployableRevision,
    record_name: &str,
    field_name: &str,
) -> TestResult<(
    &'a RecordValueTypeDefinition,
    &'a RecordValueFieldDefinition,
)> {
    let record = candidate
        .candidate()
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == record_name)
        .ok_or_else(|| failure(format!("candidate has no record {record_name}")))?;
    let field = record
        .fields()
        .iter()
        .find(|field| field.name() == field_name)
        .ok_or_else(|| failure(format!("record {record_name} has no field {field_name}")))?;
    Ok((record, field))
}

async fn run_single_row_statement(database: &TestDatabase, statement: &str) -> TestResult<()> {
    let session = database.open().await?;
    let count = session.client().execute(statement, &[]).await?;
    require(
        count == 1,
        format!("tamper statement updated {count} rows; expected exactly one"),
    )?;
    session.shutdown().await
}

async fn drop_record_field_constraints(
    database: &TestDatabase,
    constraints: &[&str],
) -> TestResult<()> {
    let session = database.open().await?;
    for constraint in constraints {
        session
            .client()
            .batch_execute(&format!(
                "ALTER TABLE _orna_kernel.catalogue_record_value_fields
                 DROP CONSTRAINT {constraint}"
            ))
            .await?;
    }
    session.shutdown().await
}

async fn field_row_sans_columns(
    database: &TestDatabase,
    row_where: &str,
    excluded_columns: &[&str],
) -> TestResult<String> {
    let minus = excluded_columns
        .iter()
        .map(|column| format!(" - '{column}'"))
        .collect::<String>();
    let session = database.open().await?;
    let row = session
        .client()
        .query_one(
            &format!(
                "SELECT (to_jsonb(source){minus})::text
                 FROM _orna_kernel.catalogue_record_value_fields AS source
                 WHERE {row_where}"
            ),
            &[],
        )
        .await?;
    let value: String = row.try_get(0)?;
    session.shutdown().await?;
    Ok(value)
}

fn require_durable_rule(error: &PostgresKernelError, record: &str, rule: &str) -> TestResult<()> {
    match error {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_record_value_fields",
            record: actual_record,
            rule: actual_rule,
        } => require(
            actual_record.as_str() == record && *actual_rule == rule,
            format!(
                "unexpected durable record {actual_record:?}/rule {actual_rule:?}; expected {record:?}/{rule:?}"
            ),
        ),
        other => Err(failure(format!(
            "expected catalogue_record_value_fields durable invariant, got {other}"
        ))),
    }
}

fn require_revision_record_field_error(
    error: &PostgresKernelError,
    expected: &RevisionInvariantError,
) -> TestResult<()> {
    let PostgresKernelError::RevisionInvariant(actual) = error else {
        return Err(failure(format!(
            "expected revision invariant record field error, got {error}"
        )));
    };
    require(
        actual == expected,
        format!("revision invariant record field error differs: {actual:?}; expected {expected:?}"),
    )
}

async fn reject_nested_record_tamper(
    database: &TestDatabase,
    pipeline: &NestedRecordPipeline,
    row_where: &str,
    excluded_columns: &[&str],
    drops: &[&str],
    tamper: &str,
    expected_error: impl Fn(&PostgresKernelError) -> TestResult<()>,
) -> TestResult<()> {
    require_applied_revision_matches_candidate(
        &kernel(database)?.recover().await?,
        &pipeline.candidate,
    )?;
    let before = field_row_sans_columns(database, row_where, excluded_columns).await?;
    drop_record_field_constraints(database, drops).await?;
    run_single_row_statement(database, tamper).await?;
    let after = field_row_sans_columns(database, row_where, excluded_columns).await?;
    require(
        before == after,
        "tamper changed record field columns beyond the intended ones",
    )?;
    let post_tamper = snapshot_kernel_tables(database).await?;
    expected_error(&recovery_error(database).await?)?;
    expected_error(&recovery_error(database).await?)?;
    require(
        snapshot_kernel_tables(database).await? == post_tamper,
        "rejected record field tamper repaired durable kernel state",
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_target_null_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &["cat_record_value_fields_type_check"],
            &format!("UPDATE _orna_kernel.catalogue_record_value_fields SET record_type_id = NULL WHERE {where_clause}"),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "null record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_mixed_with_application_enum_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let mode = pipeline
            .candidate
            .candidate()
            .enum_types()
            .iter()
            .find(|enum_type| enum_type.name().to_string() == "app.mode")
            .ok_or_else(|| failure("candidate has no app.mode enum"))?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["enum_type_id"],
            &["cat_record_value_fields_type_check"],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET enum_type_id = {}
                 WHERE {where_clause}",
                bytea_literal(mode.id().to_bytes())
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "application-enum record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_mixed_with_standard_enum_pin_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["enum_standard_library_revision_id"],
            &["cat_record_value_fields_type_check"],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET enum_standard_library_revision_id = {}
                 WHERE {where_clause}",
                bytea_literal(&pipeline.standard_revision)
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "standard-enum pin record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_fifteen_byte_nested_record_target_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(outer.id().to_bytes(), child.id().to_bytes());
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "record value field record identity must be null or 16 bytes";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &[
                "cat_record_value_fields_type_check",
                "cat_record_value_fields_record_type_id_length",
                "cat_record_value_fields_record_type_fk",
            ],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET record_type_id = '\\x{}'::bytea
                 WHERE {where_clause}",
                "ab".repeat(15)
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "fifteen-byte record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_unknown_nested_record_target_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let unknown = TypeId::from_bytes([0x7d; 16]).to_bytes().to_vec();
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let expected = RevisionInvariantError::UnsupportedRecordValueFieldType {
            record_value_type: outer.id(),
            field: child.id(),
            descriptor: TypeDescriptor::named(TypeId::from_bytes([0x7d; 16])),
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &[
                "cat_record_value_fields_type_check",
                "cat_record_value_fields_record_type_fk",
            ],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET record_type_id = {}
                 WHERE {where_clause}",
                bytea_literal(&unknown)
            ),
            |error| {
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => {
                        require_revision_record_field_error(error, &expected)?;
                        require(
                            inner.to_string()
                                == "record value field has an unsupported resolved type"
                                && std::error::Error::source(inner).is_none(),
                            format!(
                                "unsupported record target error did not preserve its exact inner display: {error}"
                            ),
                        )
                    }
                    other => Err(failure(format!(
                        "unsupported record target error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_record_cycle_closing_edge_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (inner, flag) = find_record_field(&pipeline.candidate, "app.inner", "flag")?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(inner.id().to_bytes(), flag.id().to_bytes());
        // The two-node cycle closes at the back edge of whichever record the
        // identity-sorted walk visits second. The compiler allocates the
        // fixture identities nondeterministically, so name the closing edge
        // from the actual pair without reproducing the production walk.
        let (closing_owner, closing_field, closing_target) =
            if inner.id().to_bytes() < outer.id().to_bytes() {
                (outer.id(), child.id(), inner.id())
            } else {
                (inner.id(), flag.id(), outer.id())
            };
        let expected = RevisionInvariantError::RecursiveRecordValueField {
            record_value_type: closing_owner,
            field: closing_field,
            nested_record_value_type: closing_target,
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &[
                "type_kind",
                "record_type_id",
                "value_type_id",
                "value_standard_library_revision_id",
            ],
            &[],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET type_kind = 'record',
                     record_type_id = {},
                     value_type_id = NULL,
                     value_standard_library_revision_id = NULL
                 WHERE {where_clause}",
                bytea_literal(outer.id().to_bytes())
            ),
            |error| {
                require_revision_record_field_error(error, &expected)?;
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => require(
                        inner.to_string() == "record value fields must not form a recursive cycle"
                            && std::error::Error::source(inner).is_none(),
                        format!(
                            "record cycle error did not preserve its exact inner display: {error}"
                        ),
                    ),
                    other => Err(failure(format!(
                        "record cycle error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

fn deep_record_source() -> String {
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=31 {
        source.push_str(&format!(
            "CREATE TYPE app.d{index} AS VALUE (next app.d{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.d32 AS VALUE (leaf BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source.push_str("CREATE TYPE app.d33 AS VALUE (leaf BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_record_nesting_depth_33_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, &deep_record_source()).await?;
        let (d32, leaf) = find_record_field(&pipeline.candidate, "app.d32", "leaf")?;
        let d33 = pipeline
            .candidate
            .candidate()
            .record_value_types()
            .iter()
            .find(|record| record.name().to_string() == "app.d33")
            .ok_or_else(|| failure("candidate has no app.d33 record"))?;
        let where_clause = field_where(d32.id().to_bytes(), leaf.id().to_bytes());
        let expected = RevisionInvariantError::RecordValueNestingTooDeep {
            record_value_type: d32.id(),
            field: leaf.id(),
            nested_record_value_type: d33.id(),
            maximum: 32,
            actual: 33,
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &[
                "type_kind",
                "record_type_id",
                "value_type_id",
                "value_standard_library_revision_id",
            ],
            &[],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET type_kind = 'record',
                     record_type_id = {},
                     value_type_id = NULL,
                     value_standard_library_revision_id = NULL
                 WHERE {where_clause}",
                bytea_literal(d33.id().to_bytes())
            ),
            |error| {
                require_revision_record_field_error(error, &expected)?;
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => require(
                        inner.to_string() == "record value nesting exceeds the maximum depth"
                            && std::error::Error::source(inner).is_none(),
                        format!(
                            "record nesting error did not preserve its exact inner display: {error}"
                        ),
                    ),
                    other => Err(failure(format!(
                        "record nesting error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_ambiguous_nested_record_identity_before_the_cycle_without_repair() -> TestResult<()>
{
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        require_applied_revision_matches_candidate(
            &kernel(&database)?.recover().await?,
            &pipeline.candidate,
        )?;
        let (inner, flag) = find_record_field(&pipeline.candidate, "app.inner", "flag")?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let boolean = orna_standard::BOOLEAN_TYPE_ID;
        let revision = pipeline.candidate.candidate().revision().to_bytes().to_vec();
        let inner_id = inner.id().to_bytes().to_vec();
        let outer_id = outer.id().to_bytes().to_vec();
        let flag_id = flag.id().to_bytes().to_vec();
        let child_id = child.id().to_bytes().to_vec();
        let boolean_id = boolean.to_bytes().to_vec();

        let post_apply = snapshot_kernel_tables(&database).await?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            session
                .client()
                .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
                .await?;
            let rename_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_types
                     SET type_id = $1
                     WHERE catalogue_revision_id = $2 AND type_id = $3",
                    &[&boolean_id, &revision, &inner_id],
                )
                .await?;
            require(
                rename_count == 1,
                "inner record type rename must affect exactly one row",
            )?;
            let flag_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET type_kind = 'record',
                         record_type_id = $1,
                         value_type_id = NULL,
                         value_standard_library_revision_id = NULL
                     WHERE catalogue_revision_id = $2
                       AND owner_type_id = $3 AND field_id = $4",
                    &[&outer_id, &revision, &inner_id, &flag_id],
                )
                .await?;
            require(
                flag_count == 1,
                "inner flag tuple conversion must affect exactly one row",
            )?;
            let owner_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET owner_type_id = $1
                     WHERE catalogue_revision_id = $2 AND owner_type_id = $3",
                    &[&boolean_id, &revision, &inner_id],
                )
                .await?;
            require(
                owner_count == 2,
                "inner field owner rename must affect exactly two rows",
            )?;
            let child_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET record_type_id = $1
                     WHERE catalogue_revision_id = $2
                       AND owner_type_id = $3 AND field_id = $4",
                    &[&boolean_id, &revision, &outer_id, &child_id],
                )
                .await?;
            require(
                child_count == 1,
                "outer child target rename must affect exactly one row",
            )?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "ambiguous nested record tamper",
        )?;
        let post_tamper = snapshot_kernel_tables(&database).await?;
        require(
            post_tamper != post_apply,
            "ambiguous nested record tamper did not change the durable kernel state",
        )?;
        let expected = RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: outer.id(),
            field: child.id(),
            type_id: boolean,
        };
        let error = recovery_error(&database).await?;
        require_revision_record_field_error(&error, &expected)?;
        match &error {
            PostgresKernelError::RevisionInvariant(inner) => require(
                inner.to_string()
                    == "record field type is present in both application and standard catalogues"
                    && std::error::Error::source(inner).is_none(),
                format!(
                    "ambiguous record identity error did not preserve its exact inner display: {error}"
                ),
            ),
            other => Err(failure(format!(
                "ambiguous record identity error is not a revision invariant: {other}"
            ))),
        }?;
        require_revision_record_field_error(&recovery_error(&database).await?, &expected)?;
        require(
            snapshot_kernel_tables(&database).await? == post_tamper,
            "rejected ambiguous nested record fixture repaired durable kernel state",
        )?;
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn decodes_an_exact_opaque_standard_row_before_detecting_digest_tamper() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let fixture = install_raw_v2_standard_revision(&database).await?;
        let standard_revision = fixture.standard.revision().to_bytes().to_vec();
        let void_type = orna_standard::VOID_TYPE_ID.to_bytes().to_vec();
        let session = database.open().await?;
        let operation: TestResult<()> = async {
            let invalid_persistence = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque', persistence = 'persistable'
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await
                .expect_err("persistable opaque standard row must be rejected");
            require(
                invalid_persistence.as_db_error().is_some_and(|error| {
                    error.code().code() == "23514"
                        && error.constraint() == Some("std_cat_value_types_opaque_contract_check")
                }),
                "persistable opaque standard row did not fail its exact database constraint",
            )?;
            let invalid_contract = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque', representation_contract = E'opaque\\ncontract'
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await
                .expect_err("non-printable opaque standard contract must be rejected");
            require(
                invalid_contract.as_db_error().is_some_and(|error| {
                    error.code().code() == "23514"
                        && error.constraint() == Some("std_cat_value_types_opaque_contract_check")
                }),
                "non-printable opaque standard contract did not fail its exact database constraint",
            )?;
            let updated = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque'
                     WHERE standard_library_revision_id = $1 AND type_id = $2
                       AND persistence = 'transient'",
                    &[&standard_revision, &void_type],
                )
                .await?;
            require(
                updated == 1,
                format!("opaque kind tamper changed {updated} rows"),
            )
        }
        .await;
        finish_session(
            operation,
            session.shutdown().await,
            "opaque standard row tamper",
        )?;

        let error = recovery_error(&database).await?;
        require_standard_library_digest_mismatch(&error, fixture.standard.revision().to_bytes())?;
        let session = database.open().await?;
        let operation: TestResult<()> = async {
            let row = session
                .client()
                .query_one(
                    "SELECT value_kind, persistence, representation_contract
                     FROM _orna_kernel.standard_catalogue_value_types
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await?;
            require(
                row.try_get::<_, String>(0)? == "opaque"
                    && row.try_get::<_, String>(1)? == "transient"
                    && row.try_get::<_, String>(2)? == "orna.kernel.value.void@1",
                "failed opaque recovery repaired or changed the durable row",
            )
        }
        .await;
        finish_session(
            operation,
            session.shutdown().await,
            "opaque standard row postcondition",
        )
    })
    .await
}

#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn persists_recovers_revokes_and_disables_execute_authority() -> TestResult<()> {
    let handle = std::thread::Builder::new()
        .name("security-recovery-live".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!(
                        "security recovery live runtime could not start: {error}"
                    ))
                })?;
            runtime.block_on(persists_recovers_revokes_and_disables_execute_authority_inner())
        })
        .map_err(|error| {
            failure(format!(
                "security recovery live thread could not start: {error}"
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure("security recovery live thread panicked")),
    }
}

async fn persists_recovers_revokes_and_disables_execute_authority_inner() -> TestResult<()> {
    const USER_UID: u32 = 1_001;
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
    const ROLE: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
    const SERVICE: PrincipalId = PrincipalId::from_bytes([0x33; 16]);
    const OTHER_ROLE: PrincipalId = PrincipalId::from_bytes([0x34; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty_security = kernel.recover_security_snapshot().await?;
        require(
            empty_security.bind_authenticated_session(USER, vec![])
                == Err(SessionBindingError::UnknownSessionPrincipal),
            "empty bootstrap invented a security principal",
        )?;
        let empty = kernel.recover().await?;
        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            "security fixture schema did not compile",
        )?;
        let version_one = kernel
            .apply(&prepare(&schema_report, empty.pair(), &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let active = kernel
            .apply(&standard_client_candidate(
                STANDARD_SECURITY_SOURCE,
                &version_two,
                &upgrade,
            )?)
            .await?;
        let function = require_standard_client_execution(&active, &upgrade, true)?.function;
        let server_function = active
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().parts() == ["app", "read"])
            .ok_or_else(|| failure("security fixture SERVER function was not recovered"))?
            .id();
        let mut functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        functions.sort_unstable();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("security fixture must use a pinned standard snapshot"))?;
        require(
            standard.catalogue().functions().is_empty(),
            "the current verified standard snapshot must contribute no functions",
        )?;
        let recovered_empty = kernel.recover_security_snapshot().await?;
        require(
            recovered_empty.functions().collect::<Vec<_>>() == functions,
            "security recovery did not derive the exact application and empty-standard target union",
        )?;

        let missing_target = SecuritySnapshot::new(
            active.pair(),
            functions[..1].to_vec(),
            vec![],
            vec![],
            vec![],
        )?;
        let missing_error = kernel
            .replace_security_snapshot(&missing_target)
            .await
            .expect_err("security replacement missing an application target must fail");
        require(
            matches!(
                missing_error,
                PostgresKernelError::SecurityFunctionSetMismatch
            ),
            "missing target replacement returned the wrong typed error",
        )?;
        let extra = FunctionId::from_bytes([0x39; 16]);
        let mut extra_targets = functions.clone();
        extra_targets.push(extra);
        extra_targets.sort_unstable();
        let extra_target = SecuritySnapshot::new(
            active.pair(),
            extra_targets,
            vec![],
            vec![],
            vec![],
        )?;
        let extra_error = kernel
            .replace_security_snapshot(&extra_target)
            .await
            .expect_err("security replacement with an extra target must fail");
        require(
            matches!(
                extra_error,
                PostgresKernelError::SecurityFunctionSetMismatch
            ),
            "extra target replacement returned the wrong typed error",
        )?;
        require(
            kernel
                .recover_security_snapshot()
                .await?
                .functions()
                .collect::<Vec<_>>()
                == functions,
            "rejected target-set replacements changed recovered security targets",
        )?;

        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![
                ExecuteGrant::new(ROLE, function),
                ExecuteGrant::new(SERVICE, function),
                ExecuteGrant::new(SERVICE, server_function),
            ],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;

        let recovered = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        let user_session = recovered.bind_authenticated_session(USER, vec![ROLE])?;
        let service_session = recovered.bind_authenticated_session(SERVICE, vec![])?;
        let target = InvocationTarget::new(function, active.pair());
        require(
            recovered.local_peer_credentials().collect::<Vec<_>>()
                == vec![LocalPeerCredential::new(USER_UID, USER)],
            "recovered local peer credential changed",
        )?;
        let local_session = PostgresKernel::new(database.config()?)
            .authenticate_local_peer(USER_UID)
            .await?;
        require(
            local_session.principal() == USER && local_session.active_roles().is_empty(),
            "local peer authentication changed the principal or selected roles",
        )?;
        let unknown_peer_error = kernel
            .authenticate_local_peer(USER_UID + 1)
            .await
            .expect_err("unmapped local peer must fail authentication");
        require(
            matches!(
                unknown_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "unmapped local peer returned the wrong typed error",
        )?;
        require(
            matches!(
                recovered.authorise_execute(&user_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == ROLE
            ) && matches!(
                recovered.authorise_execute(&service_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == SERVICE
            ),
            "recovered direct or selected-role EXECUTE authority changed",
        )?;
        let unselected_role_session = recovered.bind_authenticated_session(USER, vec![])?;
        let missing_error = kernel
            .evaluate_client_function(&unselected_role_session, function)
            .await
            .expect_err("never-granted session must not enter the evaluator");
        require(
            matches!(
                missing_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong never-granted denial",
        )?;
        let evaluated = kernel
            .evaluate_client_function(&user_session, function)
            .await?;
        require(
            evaluated.context().pair() == active.pair()
                && evaluated.context().function() == function
                && evaluated.value() == &RuntimeValue::Boolean(true),
            "kernel CLIENT gate returned the wrong authorised result",
        )?;
        let directly_evaluated = kernel
            .evaluate_client_function(&service_session, function)
            .await?;
        require(
            directly_evaluated.value() == &RuntimeValue::Boolean(true),
            "directly authorised CLIENT evaluation returned the wrong value",
        )?;
        let evaluator_error = kernel
            .evaluate_client_function(&service_session, server_function)
            .await
            .expect_err("SERVER function must be rejected by the CLIENT evaluator");
        require(
            matches!(evaluator_error, PostgresKernelError::ClientExecution(_)),
            "allowed SERVER target returned the wrong CLIENT evaluator error",
        )?;
        let unknown = FunctionId::from_bytes([0x38; 16]);
        let unknown_error = kernel
            .evaluate_client_function(&user_session, unknown)
            .await
            .expect_err("unknown function must be denied before evaluation");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::UnknownFunction,
                } if pair == active.pair() && denied == unknown
            ),
            "kernel CLIENT gate returned the wrong unknown-function denial",
        )?;

        let stale_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x35; 16]),
            CatalogueRevisionId::from_bytes([0x36; 16]),
        );
        let stale = SecuritySnapshot::new(
            stale_pair,
            functions.clone(),
            granted.principals().collect(),
            granted.memberships().collect(),
            granted.execute_grants().collect(),
        )?;
        let stale_error = kernel
            .replace_security_snapshot(&stale)
            .await
            .expect_err("stale security replacement must fail");
        require(
            matches!(
                stale_error,
                PostgresKernelError::SecurityRevisionMismatch {
                    expected,
                    active: locked,
                } if expected == stale_pair && locked == active.pair()
            ),
            "stale security replacement returned the wrong typed error",
        )?;
        let after_stale = kernel.recover_security_snapshot().await?;
        require(
            matches!(
                after_stale.authorise_execute(&service_session, target),
                ExecuteDecision::Allowed(ref evidence)
                    if evidence.authorising_principal() == SERVICE
            ),
            "stale security replacement changed durable grants",
        )?;

        let revoked = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let reconnected = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        require(
            reconnected.authorise_execute(&user_session, target)
                == ExecuteDecision::Denied(ExecuteDenial::MissingExecuteGrant),
            "reconnected snapshot retained a revoked EXECUTE grant",
        )?;
        require(
            reconnected.local_peer_credentials().next().is_none(),
            "reconnected snapshot retained a revoked local peer credential",
        )?;
        let revoked_peer_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("revoked local peer credential must block authentication");
        require(
            matches!(
                revoked_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "revoked local peer credential returned the wrong authentication error",
        )?;
        let revoked_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("revoked EXECUTE grant must block the evaluator");
        require(
            matches!(
                revoked_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong revoked-grant denial",
        )?;

        let stale_session_snapshot = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions.clone(),
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![],
            vec![ExecuteGrant::new(ROLE, function)],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel
            .replace_security_snapshot(&stale_session_snapshot)
            .await?;
        let stale_session_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("stale selected role must block the evaluator");
        require(
            matches!(
                stale_session_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::InvalidSession,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong stale-session denial",
        )?;

        let disabled = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Disabled),
                Principal::new(ROLE, PrincipalKind::Role, PrincipalStatus::Active),
                Principal::new(SERVICE, PrincipalKind::Service, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(ROLE, USER)],
            vec![],
            vec![LocalPeerCredential::new(USER_UID, USER)],
        )?;
        kernel.replace_security_snapshot(&disabled).await?;
        let final_snapshot = PostgresKernel::new(database.config()?)
            .recover_security_snapshot()
            .await?;
        require(
            final_snapshot.bind_authenticated_session(USER, vec![ROLE])
                == Err(SessionBindingError::DisabledSessionPrincipal),
            "reconnected snapshot re-enabled a disabled principal",
        )?;
        let disabled_peer_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("disabled mapped principal must fail authentication");
        require(
            matches!(
                disabled_peer_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::InvalidPrincipal(
                        SessionBindingError::DisabledSessionPrincipal
                    )
                )
            ),
            "disabled mapped principal returned the wrong authentication error",
        )?;
        let disabled_error = kernel
            .evaluate_client_function(&user_session, function)
            .await
            .expect_err("disabled session must block the evaluator");
        require(
            matches!(
                disabled_error,
                PostgresKernelError::ClientExecuteDenied {
                    pair,
                    function: denied,
                    reason: ExecuteDenial::InvalidSession,
                } if pair == active.pair() && denied == function
            ),
            "kernel CLIENT gate returned the wrong disabled-session denial",
        )?;

        let audit = kernel.recover_security_audit_events().await?;
        let execute = audit
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 8,
            format!(
                "security lifecycle appended {} EXECUTE audit records instead of 8",
                execute.len()
            ),
        )?;
        require_execute_audit(
            execute[0],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            InvocationTarget::new(function, active.pair()),
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        require_execute_audit(
            execute[1],
            SecurityAuditOutcome::Allowed,
            USER,
            Some(USER),
            Some(ROLE),
            InvocationTarget::new(function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[2],
            SecurityAuditOutcome::Allowed,
            SERVICE,
            Some(SERVICE),
            Some(SERVICE),
            InvocationTarget::new(function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[3],
            SecurityAuditOutcome::Allowed,
            SERVICE,
            Some(SERVICE),
            Some(SERVICE),
            InvocationTarget::new(server_function, active.pair()),
            None,
        )?;
        require_execute_audit(
            execute[4],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            InvocationTarget::new(unknown, active.pair()),
            Some(ExecuteDenial::UnknownFunction),
        )?;
        for (event, expected) in [
            (execute[5], ExecuteDenial::MissingExecuteGrant),
            (execute[6], ExecuteDenial::InvalidSession),
            (execute[7], ExecuteDenial::InvalidSession),
        ] {
            require_execute_audit(
                event,
                SecurityAuditOutcome::Denied,
                USER,
                None,
                None,
                InvocationTarget::new(function, active.pair()),
                Some(expected),
            )?;
        }

        kernel.replace_security_snapshot(&granted).await?;
        let session = database.open().await?;
        let constraint = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT security_audit_events_test_reject_execute
                 CHECK (false) NOT VALID;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            constraint,
            session.shutdown().await,
            "EXECUTE audit insert failure fixture",
        )?;
        for (session, target) in [(&service_session, function), (&user_session, unknown)] {
            let audit_failure = kernel
                .evaluate_client_function(session, target)
                .await
                .expect_err("EXECUTE audit insertion failure must fail the operation");
            require(
                matches!(audit_failure, PostgresKernelError::Database(_)),
                "EXECUTE audit insertion failure returned the operation result",
            )?;
        }
        require(
            kernel.recover_security_audit_events().await? == audit,
            "failed EXECUTE audit insertion changed prior history",
        )?;
        let session = database.open().await?;
        let removal = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT security_audit_events_test_reject_execute;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            removal,
            session.shutdown().await,
            "EXECUTE audit insert failure fixture cleanup",
        )?;

        let session = database.open().await?;
        let unknown_function = FunctionId::from_bytes([0x37; 16]);
        let tamper_result = session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants
                     (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[
                    &USER.to_bytes().to_vec(),
                    &unknown_function.to_bytes().to_vec(),
                ],
            )
            .await
            .map(|_| ())
            .map_err(Into::into);
        finish_session(
            tamper_result,
            session.shutdown().await,
            "unknown grant tamper",
        )?;
        let unknown_error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("unknown durable function grant must fail recovery");
        require(
            matches!(
                unknown_error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownGrantFunction)
            ),
            "unknown durable function grant returned the wrong typed error",
        )?;
        let session = database.open().await?;
        let retained: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM _orna_kernel.security_execute_grants
                 WHERE function_id = $1",
                &[&unknown_function.to_bytes().to_vec()],
            )
            .await?
            .get(0);
        finish_session(
            require(retained == 1, "rejected unknown grant tamper was repaired"),
            session.shutdown().await,
            "unknown grant retention check",
        )?;

        let session = database.open().await?;
        let cycle_result = async {
            session
                .client()
                .execute(
                    "DELETE FROM _orna_kernel.security_execute_grants
                     WHERE function_id = $1",
                    &[&unknown_function.to_bytes().to_vec()],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                     VALUES ($1, 'role', 'active')",
                    &[&OTHER_ROLE.to_bytes().to_vec()],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_role_memberships
                         (role_id, member_id)
                     VALUES ($1, $2), ($2, $1)",
                    &[&ROLE.to_bytes().to_vec(), &OTHER_ROLE.to_bytes().to_vec()],
                )
                .await?;
            Ok(())
        }
        .await;
        finish_session(cycle_result, session.shutdown().await, "role cycle tamper")?;
        let cycle_error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("durable role cycle must fail recovery");
        require(
            matches!(
                cycle_error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::CyclicRoleMembership)
            ),
            "durable role cycle returned the wrong typed error",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_closed_security_audit_history_under_hostile_search_path_and_rejects_tamper()
-> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x33; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0xf1; 16]);
    const PAIR: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x51; 16]),
        CatalogueRevisionId::from_bytes([0xc1; 16]),
    );

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let active = kernel.bootstrap().await?;
        let active_pair = RevisionPair::new(active.source(), active.catalogue());
        let session = database.open().await?;
        let insertion = session
            .client()
            .batch_execute(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome, session_principal_id)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      TIMESTAMP '1969-12-31 23:59:59',
                      'authentication', 'allowed', decode(repeat('31', 16), 'hex'));

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome, denial_reason)
                 VALUES
                     (decode(repeat('a2', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:00',
                      'authentication', 'denied', 'authentication_unknown_uid');

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, effective_principal_id,
                      authorising_principal_id, function_id, source_revision_id,
                      catalogue_revision_id)
                 VALUES
                     (decode(repeat('a3', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:01',
                      'execute', 'allowed',
                      decode(repeat('31', 16), 'hex'),
                      decode(repeat('32', 16), 'hex'),
                      decode(repeat('33', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      decode(repeat('51', 16), 'hex'),
                      decode(repeat('c1', 16), 'hex'));

                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a4', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:02',
                      'execute', 'denied',
                      decode(repeat('31', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      decode(repeat('51', 16), 'hex'),
                      decode(repeat('c1', 16), 'hex'),
                      'execute_missing_grant');",
            )
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            session.shutdown().await,
            "security audit fixture insertion",
        )?;

        run_batch(
            &database,
            "CREATE TABLE public.active_revision AS
                 SELECT * FROM _orna_kernel.active_revision WITH NO DATA;
             INSERT INTO public.active_revision
                 (singleton, source_revision_id, catalogue_revision_id)
             VALUES
                 (true, decode(repeat('d1', 16), 'hex'),
                        decode(repeat('d2', 16), 'hex'));

             CREATE TABLE public.security_audit_events AS
                 SELECT * FROM _orna_kernel.security_audit_events WITH NO DATA;
             INSERT INTO public.security_audit_events
                 (sequence, event_id, recorded_at, event_kind, outcome,
                  denial_reason)
             VALUES
                 (1, decode(repeat('b1', 16), 'hex'),
                  TIMESTAMP '1970-01-01 00:00:00', 'authentication', 'denied',
                  'authentication_unknown_uid');",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        let hostile_kernel = PostgresKernel::new(hostile_config);
        let recovered_active = hostile_kernel.recover().await?;
        require(
            recovered_active.pair() == active_pair,
            "hostile search_path redirected active revision recovery",
        )?;

        let target = InvocationTarget::new(FUNCTION, PAIR);
        let expected = vec![
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa1; 16]),
                1,
                UNIX_EPOCH - Duration::from_secs(1),
                SecurityAuditDecision::recover_authentication_allowed(USER),
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa2; 16]),
                2,
                UNIX_EPOCH,
                SecurityAuditDecision::authentication_denied(
                    None,
                    LocalPeerAuthenticationError::UnknownUid,
                )?,
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa3; 16]),
                3,
                UNIX_EPOCH + Duration::from_secs(1),
                SecurityAuditDecision::recover_execute_allowed(
                    USER,
                    EFFECTIVE,
                    AUTHORISING,
                    target,
                ),
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa4; 16]),
                4,
                UNIX_EPOCH + Duration::from_secs(2),
                SecurityAuditDecision::recover_execute_denied(
                    USER,
                    target,
                    ExecuteDenial::MissingExecuteGrant,
                ),
            ),
        ];
        let recovered = hostile_kernel.recover_security_audit_events().await?;
        require(
            recovered == expected,
            "security audit recovery changed order, time, identity, or decision evidence",
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                     DROP CONSTRAINT security_audit_events_shape_check,
                     DROP CONSTRAINT security_audit_events_revision_pair_check,
                     DROP CONSTRAINT security_audit_events_denial_reason_check;
                 UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode(repeat('f1', 16), 'hex')
                 WHERE sequence = 1;",
            )
            .await?;

        let error = hostile_kernel
            .recover()
            .await
            .expect_err("malformed durable security audit data must fail full recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "audit event shape is not recognised",
                } if record == "1"
            ),
            "full recovery returned the wrong malformed security audit invariant",
        )?;

        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("invalid durable security audit shape must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "audit event shape is not recognised",
                } if record == "1"
            ),
            "security audit shape tamper returned the wrong durable invariant",
        )?;

        let retained: bool = session
            .client()
            .query_one(
                "SELECT function_id = decode(repeat('f1', 16), 'hex')
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 1",
                &[],
            )
            .await?
            .get(0);
        require(
            retained,
            "rejected security audit shape tamper was repaired",
        )?;

        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = NULL
                 WHERE sequence = 1;
                 UPDATE _orna_kernel.security_audit_events
                 SET catalogue_revision_id = NULL
                 WHERE sequence = 4;",
            )
            .await?;
        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("incomplete durable security audit pair must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "EXECUTE requires a catalogue revision",
                } if record == "4"
            ),
            "security audit pair tamper returned the wrong durable invariant",
        )?;
        let retained_pair: bool = session
            .client()
            .query_one(
                "SELECT catalogue_revision_id IS NULL
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 4",
                &[],
            )
            .await?
            .get(0);
        require(
            retained_pair,
            "rejected security audit pair tamper was repaired",
        )?;

        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET catalogue_revision_id = decode(repeat('c1', 16), 'hex'),
                     denial_reason = 'execute_not_supported'
                 WHERE sequence = 4;",
            )
            .await?;
        let error = hostile_kernel
            .recover_security_audit_events()
            .await
            .expect_err("unknown durable security audit denial must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "EXECUTE denial reason is unsupported",
                } if record == "4"
            ),
            "security audit denial tamper returned the wrong durable invariant",
        )?;
        let retained_reason: String = session
            .client()
            .query_one(
                "SELECT denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 4",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained_reason == "execute_not_supported",
                "rejected security audit denial tamper was repaired",
            ),
            session.shutdown().await,
            "security audit tamper retention checks",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_security_admin_audit_with_exact_redacted_shape() -> TestResult<()> {
    const ADMIN: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const USER: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const CREATED: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let security =
            SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                active.pair(),
                vec![],
                vec![
                    Principal::new(ADMIN, PrincipalKind::User, PrincipalStatus::Active),
                    Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                ],
                vec![],
                vec![],
                vec![],
                vec![PrivilegeGrant::new(
                    ADMIN,
                    PrivilegeClass::SecurityAdmin,
                    None,
                )?],
            )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let admin_session = security.bind_authenticated_session(ADMIN, vec![])?;
        let user_session = security.bind_authenticated_session(USER, vec![])?;

        kernel
            .create_principal(&admin_session, CREATED, PrincipalKind::User)
            .await?;
        let denied = kernel
            .create_principal(&user_session, CREATED, PrincipalKind::User)
            .await
            .expect_err("an unprivileged session must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::SecurityAdminDenied {
                    reason: PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    },
                }
            ),
            "SecurityAdmin denial returned the wrong typed error",
        )?;

        let reopened = PostgresKernel::new(database.config()?);
        let events = reopened.recover_security_audit_events().await?;
        require(
            events.len() == 2,
            format!(
                "fresh recovery returned {} security-admin events instead of 2",
                events.len()
            ),
        )?;
        let allowed = &events[0].decision();
        require(
            allowed.kind() == SecurityAuditKind::SecurityAdmin
                && allowed.outcome() == SecurityAuditOutcome::Allowed
                && allowed.session_principal() == Some(ADMIN)
                && allowed.security_admin_operation()
                    == Some(SecurityAdminAuditOperation::CreatePrincipal)
                && allowed.security_admin_target()
                    == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                && allowed.security_admin_denial().is_none()
                && allowed.effective_principal().is_none()
                && allowed.authorising_principal().is_none()
                && allowed.target().is_none(),
            "fresh recovery changed the allowed SecurityAdmin decision shape",
        )?;
        let denied = &events[1].decision();
        require(
            denied.kind() == SecurityAuditKind::SecurityAdmin
                && denied.outcome() == SecurityAuditOutcome::Denied
                && denied.session_principal() == Some(USER)
                && denied.security_admin_operation()
                    == Some(SecurityAdminAuditOperation::CreatePrincipal)
                && denied.security_admin_target()
                    == Some(SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID)
                && denied.security_admin_denial()
                    == Some(PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    })
                && denied.effective_principal().is_none()
                && denied.authorising_principal().is_none()
                && denied.target().is_none(),
            "fresh recovery changed the denied SecurityAdmin decision shape",
        )?;

        let session = database.open().await?;
        let rows = session
            .client()
            .query(
                "SELECT event_kind, outcome, session_principal_id,
                        effective_principal_id, authorising_principal_id, function_id,
                        source_revision_id, catalogue_revision_id, denial_reason
                 FROM _orna_kernel.security_audit_events
                 ORDER BY sequence",
                &[],
            )
            .await?;
        require(
            rows.len() == 2,
            "durable SecurityAdmin audit row count changed",
        )?;
        for (row, principal, detail) in [
            (&rows[0], ADMIN, "security_admin:create_principal"),
            (
                &rows[1],
                USER,
                "security_admin:create_principal:missing-privilege",
            ),
        ] {
            let event_kind: String = row.try_get(0)?;
            let outcome: String = row.try_get(1)?;
            let session_principal: Vec<u8> = row.try_get(2)?;
            let effective_principal: Option<Vec<u8>> = row.try_get(3)?;
            let authorising_principal: Option<Vec<u8>> = row.try_get(4)?;
            let function: Vec<u8> = row.try_get(5)?;
            let source_revision: Option<Vec<u8>> = row.try_get(6)?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get(7)?;
            let denial_reason: Option<String> = row.try_get(8)?;
            require(
                event_kind == "security_admin"
                    && outcome
                        == if principal == ADMIN {
                            "allowed"
                        } else {
                            "denied"
                        }
                    && session_principal == principal.to_bytes()
                    && function == SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID.to_bytes()
                    && effective_principal.is_none()
                    && authorising_principal.is_none()
                    && source_revision.is_none()
                    && catalogue_revision.is_none()
                    && denial_reason.as_deref() == Some(detail),
                "durable SecurityAdmin audit row contains an unexpected payload",
            )?;
        }
        for (statement, description) in [
            (
                "UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'security_admin:unsupported'
                 WHERE sequence = 1",
                "forged security-admin operation detail",
            ),
            (
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode('00000000000000000000000000000044', 'hex')
                 WHERE sequence = 1",
                "mismatched security-admin target",
            ),
        ] {
            let result = session.client().execute(statement, &[]).await;
            let error = match result {
                Ok(_) => return Err(failure(format!(
                    "{description} unexpectedly bypassed the durable audit boundary"
                ))),
                Err(error) => error,
            };
            let database_error = error.as_db_error().ok_or_else(|| {
                failure(format!("{description} returned a non-database error"))
            })?;
            require(
                database_error.code().code() == "23514",
                format!(
                    "{description} failed with SQLSTATE {} instead of CHECK violation",
                    database_error.code().code()
                ),
            )?;
            require(
                database_error.constraint()
                    == Some("security_audit_events_security_admin_detail_check"),
                format!(
                    "{description} failed on unexpected constraint {:?}",
                    database_error.constraint()
                ),
            )?;
        }
        finish_session(
            Ok(()),
            session.shutdown().await,
            "security-admin audit redaction",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_closed_capability_audit_history_and_rejects_unredacted_tamper() -> TestResult<()>
{
    const USER: PrincipalId = PrincipalId::from_bytes([0x41; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0xf2; 16]);
    const PAIR: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x52; 16]),
        CatalogueRevisionId::from_bytes([0xc2; 16]),
    );

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let session = database.open().await?;
        let insertion = session
            .client()
            .batch_execute(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a5', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:03',
                      'capability', 'allowed',
                      decode(repeat('41', 16), 'hex'),
                      decode(repeat('f2', 16), 'hex'),
                      decode(repeat('52', 16), 'hex'),
                      decode(repeat('c2', 16), 'hex'),
                      'capability:std.fs.read');
                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, recorded_at, event_kind, outcome,
                      session_principal_id, function_id, source_revision_id,
                      catalogue_revision_id, denial_reason)
                 VALUES
                     (decode(repeat('a6', 16), 'hex'),
                      TIMESTAMP '1970-01-01 00:00:04',
                      'capability', 'denied',
                      decode(repeat('41', 16), 'hex'),
                      decode(repeat('f2', 16), 'hex'),
                      decode(repeat('52', 16), 'hex'),
                      decode(repeat('c2', 16), 'hex'),
                      'capability:std.net.connect');",
            )
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            session.shutdown().await,
            "capability audit fixture insertion",
        )?;

        let target = InvocationTarget::new(FUNCTION, PAIR);
        let expected = vec![
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa5; 16]),
                1,
                UNIX_EPOCH + Duration::from_secs(3),
                SecurityAuditDecision::recover_capability_allowed(
                    USER,
                    target,
                    "std.fs.read".to_owned(),
                )?,
            ),
            SecurityAuditEvent::new(
                orna_core::SecurityAuditEventId::from_bytes([0xa6; 16]),
                2,
                UNIX_EPOCH + Duration::from_secs(4),
                SecurityAuditDecision::recover_capability_denied(
                    USER,
                    target,
                    "std.net.connect".to_owned(),
                )?,
            ),
        ];
        let recovered = PostgresKernel::new(database.config()?)
            .recover_security_audit_events()
            .await?;
        require(
            recovered == expected,
            "capability audit recovery changed order, time, identity, or decision evidence",
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'capability:std.fs.read(/home/bob)'
                 WHERE sequence = 1;",
            )
            .await?;
        let error = kernel
            .recover_security_audit_events()
            .await
            .expect_err("unredacted capability audit evidence must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "capability audit name must be a qualified name with no arguments",
                } if record == "1"
            ),
            "unredacted capability tamper returned the wrong durable invariant",
        )?;

        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                     DROP CONSTRAINT security_audit_events_shape_check,
                     DROP CONSTRAINT security_audit_events_denial_reason_check;
                 UPDATE _orna_kernel.security_audit_events
                 SET denial_reason = 'execute_missing_grant'
                 WHERE sequence = 1;",
            )
            .await?;
        let error = kernel
            .recover_security_audit_events()
            .await
            .expect_err("unsupported capability audit evidence must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    ref record,
                    rule: "capability denial reason is unsupported",
                } if record == "1"
            ),
            "unsupported capability tamper returned the wrong durable invariant",
        )?;

        let retained: String = session
            .client()
            .query_one(
                "SELECT denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE sequence = 1",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained == "execute_missing_grant",
                "rejected capability audit tamper was repaired",
            ),
            session.shutdown().await,
            "capability audit tamper retention checks",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_resource_audit_without_its_durable_request_reservation() -> TestResult<()>
{
    const ORPHAN_REQUEST_ID: [u8; 16] = [0x90; 16];
    const REQUEST_ID: [u8; 16] = [0x91; 16];
    const NESTED_INVOCATION_ID: [u8; 16] = [0x92; 16];
    const PARENT_INVOCATION_ID: [u8; 16] = [0x93; 16];
    const CALL_SITE_ID: [u8; 16] = [0x94; 16];
    const SESSION_PRINCIPAL_ID: [u8; 16] = [0x95; 16];
    const RESOURCE_EVENT_ID: [u8; 16] = [0x96; 16];
    const INVOCATION_EVENT_ID: [u8; 16] = [0x97; 16];
    const SECURITY_EVENT_ID: [u8; 16] = [0x98; 16];

    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel_instance.recover().await?;
        let target_function = fixture
            .catalogue
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "café.volatile_single")
            .ok_or_else(|| failure("function fixture is missing café.volatile_single"))?
            .id();
        let target_revision = active.pair();

        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id)
                 VALUES ({orphan_request}), ({request});
                 INSERT INTO _orna_kernel.security_audit_events
                     (event_id, event_kind, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id)
                 VALUES ({security_event}, 'execute', 'allowed', {session_principal},
                         {session_principal}, {session_principal}, {target_function},
                         {source_revision}, {catalogue_revision});
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES ({invocation_event}, {nested_invocation}, 'allowed', {session_principal},
                         {session_principal}, {session_principal}, {target_function},
                         {source_revision}, {catalogue_revision}, {security_event});
                 INSERT INTO _orna_kernel.resource_audit_events
                     (event_id, request_id, nested_invocation_id, parent_invocation_id,
                      call_site_id, target_function_id, source_revision_id,
                      catalogue_revision_id, session_principal_id, decision_outcome,
                      terminal_outcome, item_count, byte_count)
                 VALUES ({resource_event}, {request}, {nested_invocation}, {parent_invocation},
                         {call_site}, {target_function}, {source_revision},
                         {catalogue_revision}, {session_principal}, 'allowed',
                         'completed', 1, 1);",
                orphan_request = bytea_literal(ORPHAN_REQUEST_ID),
                request = bytea_literal(REQUEST_ID),
                security_event = bytea_literal(SECURITY_EVENT_ID),
                session_principal = bytea_literal(SESSION_PRINCIPAL_ID),
                target_function = bytea_literal(target_function.to_bytes()),
                source_revision = bytea_literal(target_revision.source().to_bytes()),
                catalogue_revision = bytea_literal(target_revision.catalogue().to_bytes()),
                invocation_event = bytea_literal(INVOCATION_EVENT_ID),
                nested_invocation = bytea_literal(NESTED_INVOCATION_ID),
                resource_event = bytea_literal(RESOURCE_EVENT_ID),
                parent_invocation = bytea_literal(PARENT_INVOCATION_ID),
                call_site = bytea_literal(CALL_SITE_ID),
            ),
        )
        .await?;

        kernel_instance.recover().await?;

        run_batch(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.resource_request_history WHERE request_id = {}",
                bytea_literal(REQUEST_ID),
            ),
        )
        .await?;
        let request = InvocationId::from_bytes(REQUEST_ID);
        let error = recovery_error(&database).await?;
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.resource_request_history",
                    record,
                    rule: "accepted resource producer must retain its reservation",
                } if record == request.canonical()
            ),
            "missing resource request reservation returned the wrong recovery invariant",
        )?;

        let session = database.open().await?;
        let retention = async {
            let row = session
                .client()
                .query_one(
                    &format!(
                        "SELECT
                             (SELECT count(*) FROM _orna_kernel.resource_audit_events
                               WHERE request_id = {request_id}) AS resource_count,
                             (SELECT count(*) FROM _orna_kernel.invocation_audit_events
                               WHERE invocation_id = {nested_invocation}) AS invocation_count,
                             (SELECT count(*) FROM _orna_kernel.resource_request_history
                               WHERE request_id = {request_id}) AS reservation_count,
                             (SELECT count(*) FROM _orna_kernel.resource_request_history
                               WHERE request_id = {orphan_request}) AS orphan_count",
                        request_id = bytea_literal(REQUEST_ID),
                        nested_invocation = bytea_literal(NESTED_INVOCATION_ID),
                        orphan_request = bytea_literal(ORPHAN_REQUEST_ID),
                    ),
                    &[],
                )
                .await?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let reservation_count: i64 = row.try_get("reservation_count")?;
            let orphan_count: i64 = row.try_get("orphan_count")?;
            require(
                resource_count == 1
                    && invocation_count == 1
                    && reservation_count == 0
                    && orphan_count == 1,
                "failed resource recovery repaired audit or history rows",
            )
        }
        .await;
        finish_session(retention, session.shutdown().await, "resource recovery retention")?;

        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.resource_request_history (request_id) VALUES ({})",
                bytea_literal(REQUEST_ID),
            ),
        )
        .await?;
        kernel_instance.recover().await?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_tampered_protected_invocation_audit_evidence() -> TestResult<()> {
    const SESSION: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel.recover().await?;
        let function_id = fixture.catalogue.functions()[0].id();
        let database_session = database.open().await?;
        let insertion = database_session
            .client()
            .batch_execute(&format!(
                "INSERT INTO _orna_kernel.security_audit_events
                     (event_id, event_kind, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id)
                 VALUES (decode(repeat('a1', 16), 'hex'), 'execute', 'allowed',
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'));
                 INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id,
                      effective_principal_id, authorising_principal_id, function_id,
                      source_revision_id, catalogue_revision_id, security_audit_event_id)
                 VALUES (decode(repeat('b1', 16), 'hex'), decode(repeat('c1', 16), 'hex'),
                         'allowed', decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                         decode(repeat('a1', 16), 'hex'));",
                raw_id_hex(SESSION.to_bytes()),
                raw_id_hex(EFFECTIVE.to_bytes()),
                raw_id_hex(AUTHORISING.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().catalogue().to_bytes()),
                raw_id_hex(SESSION.to_bytes()),
                raw_id_hex(EFFECTIVE.to_bytes()),
                raw_id_hex(AUTHORISING.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().catalogue().to_bytes()),
            ))
            .await
            .map_err(Into::into);
        finish_session(
            insertion,
            database_session.shutdown().await,
            "protected invocation audit fixture insertion",
        )?;
        kernel.recover().await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 DROP CONSTRAINT invocation_audit_events_identity_lengths,
                 DROP CONSTRAINT invocation_audit_events_outcome_check,
                 DROP CONSTRAINT invocation_audit_events_target_evidence_pair_check,
                 DROP CONSTRAINT invocation_audit_events_target_fk,
                 DROP CONSTRAINT invocation_audit_events_revision_pair_fk,
                 DROP CONSTRAINT invocation_audit_events_security_evidence_fk;",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET event_id = decode(repeat('b1', 15), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let malformed = recovery_error(&database).await?;
        require(
            matches!(
                malformed,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "invocation audit identity must be exactly sixteen bytes",
                    ..
                }
            ),
            "malformed invocation audit identity did not fail closed",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET event_id = decode(repeat('b1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET security_audit_event_id = decode(repeat('a2', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let unlinked = recovery_error(&database).await?;
        require(
            matches!(
                unlinked,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "linked security audit evidence is missing",
                    ..
                }
            ),
            "unlinked invocation audit decision did not fail closed",
        )?;
        let database_session = database.open().await?;
        let retained: bool = database_session
            .client()
            .query_one(
                "SELECT security_audit_event_id = decode(repeat('a2', 16), 'hex')
                 FROM _orna_kernel.invocation_audit_events
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
                &[],
            )
            .await?
            .get(0);
        finish_session(
            require(
                retained,
                "rejected invocation audit link tamper was repaired",
            ),
            database_session.shutdown().await,
            "invocation audit tamper retention check",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET security_audit_event_id = decode(repeat('a1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET outcome = 'denied'
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;
        let wrong_outcome = recovery_error(&database).await?;
        require(
            matches!(
                wrong_outcome,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "linked security audit evidence does not match the invocation decision",
                    ..
                }
            ),
            "wrong invocation audit outcome did not fail closed",
        )?;
        run_single_row_statement(
            &database,
            "UPDATE _orna_kernel.invocation_audit_events
             SET outcome = 'allowed'
             WHERE invocation_id = decode(repeat('c1', 16), 'hex')",
        )
        .await?;

        run_batch(
            &database,
            "UPDATE _orna_kernel.security_audit_events
             SET source_revision_id = decode(repeat('f2', 16), 'hex')
             WHERE event_id = decode(repeat('a1', 16), 'hex');
             UPDATE _orna_kernel.invocation_audit_events
             SET source_revision_id = decode(repeat('f2', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
        )
        .await?;
        let invalid_revision = recovery_error(&database).await?;
        require(
            matches!(
                invalid_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "invalid invocation revision did not fail closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET source_revision_id = decode('{}', 'hex')
                 WHERE event_id = decode(repeat('a1', 16), 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET source_revision_id = decode('{}', 'hex')
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
                raw_id_hex(active.pair().source().to_bytes()),
                raw_id_hex(active.pair().source().to_bytes()),
            ),
        )
        .await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT security_audit_events_invocation_evidence_key;
             UPDATE _orna_kernel.security_audit_events
             SET function_id = decode(repeat('f1', 16), 'hex')
             WHERE event_id = decode(repeat('a1', 16), 'hex');
             UPDATE _orna_kernel.invocation_audit_events
             SET function_id = decode(repeat('f1', 16), 'hex')
             WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
        )
        .await?;
        let invalid_target = recovery_error(&database).await?;
        require(
            matches!(
                invalid_target,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "invalid invocation target did not fail closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.security_audit_events
                 SET function_id = decode('{}', 'hex')
                 WHERE event_id = decode(repeat('a1', 16), 'hex');
                 UPDATE _orna_kernel.invocation_audit_events
                 SET function_id = decode('{}', 'hex')
                 WHERE invocation_id = decode(repeat('c1', 16), 'hex');",
                raw_id_hex(function_id.to_bytes()),
                raw_id_hex(function_id.to_bytes()),
            ),
        )
        .await?;

        run_batch(
            &database,
            "ALTER TABLE _orna_kernel.invocation_audit_events
                 ADD COLUMN request_payload bytea;",
        )
        .await?;
        let disclosure = recovery_error(&database).await?;
        require(
            matches!(
                disclosure,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "invocation audit relation has unsupported disclosure-bearing columns",
                    ..
                }
            ),
            "disclosure-bearing invocation audit column did not fail closed",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn local_peer_authentication_appends_one_protected_decision() -> TestResult<()> {
    const USER_UID: u32 = 1_001;
    const DISABLED_UID: u32 = 1_002;
    const UNKNOWN_UID: u32 = 1_003;
    const USER: PrincipalId = PrincipalId::from_bytes([0x61; 16]);
    const DISABLED: PrincipalId = PrincipalId::from_bytes([0x62; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        let active = kernel.bootstrap().await?;
        let active_pair = RevisionPair::new(active.source(), active.catalogue());
        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active_pair,
            vec![],
            vec![
                Principal::new(USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(DISABLED, PrincipalKind::User, PrincipalStatus::Disabled),
            ],
            vec![],
            vec![],
            vec![
                LocalPeerCredential::new(USER_UID, USER),
                LocalPeerCredential::new(DISABLED_UID, DISABLED),
            ],
        )?;
        kernel.replace_security_snapshot(&security).await?;

        let authenticated = kernel.authenticate_local_peer(USER_UID).await?;
        require(
            authenticated.principal() == USER && authenticated.active_roles().is_empty(),
            "allowed local authentication changed its session",
        )?;
        let unknown = kernel
            .authenticate_local_peer(UNKNOWN_UID)
            .await
            .expect_err("unknown local peer must be denied");
        require(
            matches!(
                unknown,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "unknown local peer returned the wrong denial",
        )?;
        let disabled = kernel
            .authenticate_local_peer(DISABLED_UID)
            .await
            .expect_err("disabled mapped principal must be denied");
        require(
            matches!(
                disabled,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::InvalidPrincipal(
                        SessionBindingError::DisabledSessionPrincipal
                    )
                )
            ),
            "disabled mapped principal returned the wrong denial",
        )?;

        let revoked = SecuritySnapshot::new(
            active_pair,
            vec![],
            security.principals().collect(),
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let revoked_error = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("revoked local credential must be denied");
        require(
            matches!(
                revoked_error,
                PostgresKernelError::LocalPeerAuthentication(
                    LocalPeerAuthenticationError::UnknownUid
                )
            ),
            "revoked local credential returned the wrong denial",
        )?;

        let events = PostgresKernel::new(database.config()?)
            .recover_security_audit_events()
            .await?;
        require(
            events.len() == 4
                && events
                    .iter()
                    .enumerate()
                    .all(|(index, event)| event.sequence() == (index + 1) as i64)
                && events.iter().enumerate().all(|(index, event)| {
                    events[..index]
                        .iter()
                        .all(|earlier| earlier.id() != event.id())
                }),
            "authentication audit history changed its exact order or unique identities",
        )?;
        require_authentication_audit(&events[0], SecurityAuditOutcome::Allowed, Some(USER), None)?;
        require_authentication_audit(
            &events[1],
            SecurityAuditOutcome::Denied,
            None,
            Some(LocalPeerAuthenticationError::UnknownUid),
        )?;
        require_authentication_audit(
            &events[2],
            SecurityAuditOutcome::Denied,
            Some(DISABLED),
            Some(LocalPeerAuthenticationError::InvalidPrincipal(
                SessionBindingError::DisabledSessionPrincipal,
            )),
        )?;
        require_authentication_audit(
            &events[3],
            SecurityAuditOutcome::Denied,
            None,
            Some(LocalPeerAuthenticationError::UnknownUid),
        )?;

        let session = database.open().await?;
        let constraint = session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT security_audit_events_test_reject_insert
                 CHECK (false) NOT VALID;",
            )
            .await
            .map_err(Into::into);
        finish_session(
            constraint,
            session.shutdown().await,
            "security audit insert failure fixture",
        )?;
        let audit_failure = kernel
            .authenticate_local_peer(USER_UID)
            .await
            .expect_err("audit insertion failure must fail authentication");
        require(
            matches!(audit_failure, PostgresKernelError::Database(_)),
            "audit insertion failure returned a normal authentication denial",
        )?;
        require(
            kernel.recover_security_audit_events().await? == events,
            "failed authentication audit insertion changed prior history",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

fn require_authentication_audit(
    event: &SecurityAuditEvent,
    outcome: SecurityAuditOutcome,
    principal: Option<PrincipalId>,
    denial: Option<LocalPeerAuthenticationError>,
) -> TestResult<()> {
    require(
        event.decision().kind() == SecurityAuditKind::Authentication
            && event.decision().outcome() == outcome
            && event.decision().session_principal() == principal
            && event.decision().effective_principal().is_none()
            && event.decision().authorising_principal().is_none()
            && event.decision().target().is_none()
            && event.decision().denial() == denial.map(SecurityAuditDenial::Authentication),
        "authentication audit record changed its closed decision evidence",
    )
}

fn require_execute_audit(
    event: &SecurityAuditEvent,
    outcome: SecurityAuditOutcome,
    session: PrincipalId,
    effective: Option<PrincipalId>,
    authorising: Option<PrincipalId>,
    target: InvocationTarget,
    denial: Option<ExecuteDenial>,
) -> TestResult<()> {
    require(
        event.decision().kind() == SecurityAuditKind::Execute
            && event.decision().outcome() == outcome
            && event.decision().session_principal() == Some(session)
            && event.decision().effective_principal() == effective
            && event.decision().authorising_principal() == authorising
            && event.decision().target() == Some(target)
            && event.decision().denial() == denial.map(SecurityAuditDenial::Execute),
        "EXECUTE audit record changed its closed decision evidence",
    )
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_the_two_class_security_target_union_with_standard_targets() -> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let snapshot = kernel.recover_security_snapshot().await?;
        let executable = &fixture.standard.executables()[0];
        let echo = executable.function();
        let echo_target = SecurityFunctionTarget::verified_standard(
            echo,
            fixture.standard.revision(),
            executable.revision().id(),
        );
        let mut targets = snapshot.function_targets().collect::<Vec<_>>();
        require(
            targets
                .iter()
                .filter(|target| {
                    target.class() == orna_core::security::TargetClass::VerifiedStandard
                })
                .count()
                == 1
                && targets.contains(&echo_target),
            "recovered security snapshot lost the verified standard target",
        )?;
        require(
            snapshot
                .function_targets()
                .any(|target| target.function() == fixture.app_function),
            "recovered security snapshot lost the application target",
        )?;
        targets.sort_unstable();
        let mut expected = vec![
            echo_target,
            SecurityFunctionTarget::application(fixture.app_function),
        ];
        expected.sort_unstable();
        require(
            targets == expected,
            "recovered security snapshot returned the wrong two-class target union",
        )?;
        require(
            snapshot
                .functions()
                .eq(expected.iter().map(|target| target.function())),
            "recovered security snapshot changed the canonical identity order",
        )?;

        // An EXECUTE grant on the standard target authorises only through the
        // protected boundary with the exact immutable pins.
        let granted = SecuritySnapshot::new_with_function_targets(
            fixture.active.pair(),
            snapshot.function_targets().collect(),
            vec![Principal::new(
                USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(USER, echo)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;
        let recovered = kernel.recover_security_snapshot().await?;
        let session = recovered.bind_authenticated_session(USER, vec![])?;
        let protected = InvocationTarget::verified_standard(
            echo,
            fixture.active.pair(),
            fixture.standard.revision(),
            executable.revision().id(),
        );
        require(
            matches!(
                recovered.authorise_execute(&session, protected),
                ExecuteDecision::Allowed(evidence)
                    if evidence.authorising_principal() == USER
            ),
            "the protected standard target was not authorised by its exact grant",
        )?;

        // The ordinary raw dispatcher stays closed to the standard target even
        // when its grant exists for the protected gateway.
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, echo)
            .await
            .expect_err("raw dispatch of a standard target must deny");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::UnknownFunction,
                } if pair == fixture.active.pair() && function == echo
            ),
            "raw dispatch of a standard target returned the wrong denial",
        )?;
        let events = kernel.recover_security_audit_events().await?;
        let target = InvocationTarget::new(echo, fixture.active.pair());
        require(
            events.len() == 1,
            "raw standard target denial did not record exactly one EXECUTE decision",
        )?;
        require_execute_audit(
            &events[0],
            SecurityAuditOutcome::Denied,
            USER,
            None,
            None,
            target,
            Some(ExecuteDenial::UnknownFunction),
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_a_standard_authority_target_absent_from_the_pinned_snapshot()
-> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let catalogue = fixture.active.pair().catalogue().to_bytes().to_vec();
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the intact two-class fixture must recover its security snapshot",
        )?;

        // A standard authority row whose function revision is absent from the
        // exact pinned standard executable fails recovery closed.
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode(repeat('77', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(
                    fixture.active.pair().catalogue().to_bytes(),
                ),
                raw_id_hex(
                    fixture.standard.executables()[0].function().to_bytes(),
                ),
            ),
        )
        .await?;
        let wrong_revision = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a standard target with the wrong executable revision must fail");
        require(
            matches!(
                wrong_revision,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "standard invocation target must resolve exactly once in the pinned verified standard snapshot",
                    ..
                }
            ),
            "wrong standard executable revision returned the wrong durable invariant",
        )?;
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;

        // A missing standard authority row fails recovery without repair.
        run_single_row_statement(
            &database,
            &format!(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        let missing = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a missing standard authority target must fail recovery");
        require(
            matches!(
                missing,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "standard invocation targets must exactly match the pinned verified standard executables",
                    ..
                }
            ),
            "missing standard authority target returned the wrong durable invariant",
        )?;
        let retained: i64 = {
            let session = database.open().await?;
            let count = session
                .client()
                .query_one(
                    "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
                     WHERE catalogue_revision_id = $1 AND target_class = 'standard'",
                    &[&catalogue],
                )
                .await?
                .get(0);
            finish_session(
                Ok(count),
                session.shutdown().await,
                "standard authority retention check",
            )?
        };
        require(
            retained == 0,
            "rejected standard authority tamper was repaired",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_an_application_standard_duplicate_target() -> TestResult<()> {
    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let catalogue = fixture.active.pair().catalogue().to_bytes().to_vec();

        // A standard authority row that is re-classified as an application row
        // no longer resolves in the pinned application catalogue: the same
        // function identity cannot belong to both classes.
        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_target_authorities
                     DROP CONSTRAINT invocation_target_authorities_target_class_check,
                     DROP CONSTRAINT invocation_target_authorities_class_shape_check;
                 UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'application', standard_library_revision_id = NULL
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        let ambiguous = kernel
            .recover_security_snapshot()
            .await
            .expect_err("an application-class authority row without a catalogue function must fail");
        require(
            matches!(
                ambiguous,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_target_authorities",
                    rule: "application invocation targets must resolve in the pinned application catalogue",
                    ..
                }
            ),
            "ambiguous application authority row returned the wrong durable invariant",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET target_class = 'standard', standard_library_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');
                 ALTER TABLE _orna_kernel.invocation_target_authorities
                     ADD CONSTRAINT invocation_target_authorities_target_class_check
                     CHECK (target_class IN ('application', 'standard', 'system')),
                     ADD CONSTRAINT invocation_target_authorities_class_shape_check
                     CHECK (
                        (target_class = 'application' AND standard_library_revision_id IS NULL)
                        OR (target_class = 'standard' AND standard_library_revision_id IS NOT NULL)
                        OR (target_class = 'system' AND standard_library_revision_id IS NULL)
                     );",
                raw_id_hex(fixture.standard.revision().to_bytes()),
                raw_id_hex(fixture.active.pair().catalogue().to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].function().to_bytes()),
            ),
        )
        .await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "restored authority rows did not recover the two-class union",
        )?;

        // The same function identity present in both the application catalogue
        // and the standard authority rows is an application-and-standard
        // duplicate. The duplicate changes the catalogue itself, so full
        // recovery fails closed without writing or repairing any row.
        let session = database.open().await?;
        let schema = session
            .client()
            .query_one(
                "SELECT schema_id FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $1 LIMIT 1",
                &[&catalogue],
            )
            .await?
            .get::<_, Vec<u8>>(0);
        let duplicate: TestResult<()> = async {
            let function_id = fixture.standard.executables()[0].function().to_bytes().to_vec();
            let revision = fixture.standard.executables()[0].revision().id().to_bytes().to_vec();
            let content_hash = vec![0x77_u8; 32];
            let unit = vec![0xa1_u8; 16];
            session
                .client()
                .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.function_revisions
                        (id, introduced_catalogue_revision_id, function_id,
                         revision_number, content_hash, semantic_ir_hash,
                         language_version, status)
                     VALUES ($1, $2, $3, 1, $4, $4, 'orna.language/1', 'active')",
                    &[&revision, &catalogue, &function_id, &content_hash],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.function_artifacts
                        (function_revision_id, artifact_kind, format,
                         format_version, payload, content_hash)
                     VALUES ($1, 'server_plan', 'orna.server-parameter-echo', 1,
                             decode('4f524e4150450000000000000001000000000000000000000000000000000000000000000000000000000000000000', 'hex'), $2)",
                    &[&revision, &content_hash],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_functions
                        (catalogue_revision_id, function_id, schema_id, name_parts,
                         domain, security_mode, transaction_mode, volatility,
                         return_shape, return_type_kind, return_scalar_type,
                         current_function_revision_id, source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, ARRAY['std', 'invoke', 'echo'], 'server',
                             'invoker', 'read_only', 'stable', 'single',
                             'scalar', 'integer', $4, $5, 0, 1)",
                    &[&catalogue, &function_id, &schema, &revision, &unit],
                )
                .await?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            duplicate,
            session.shutdown().await,
            "application and standard duplicate fixture",
        )?;
        let error = kernel
            .recover()
            .await
            .expect_err("an application and standard duplicate must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant { .. }
                    | PostgresKernelError::RevisionInvariant(_)
                    | PostgresKernelError::CatalogueSnapshot(_)
            ),
            "application and standard duplicate returned the wrong fail-closed error",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovery_rejects_a_grant_naming_a_removed_standard_function() -> TestResult<()> {
    const USER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let echo = fixture.standard.executables()[0].function();

        // The grant on the standard target is valid while the target exists.
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES (decode('{}', 'hex'), 'user', 'active');
                 INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'));",
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(USER.to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        require(
            kernel.recover_security_snapshot().await.is_ok(),
            "the granted standard target must recover before the upgrade",
        )?;

        // A later standard upgrade removes the granted function from the
        // target union. Recovery must fail closed and must not drop, translate,
        // or keep the unknown grant.
        install_later_standard_upgrade_without_echo(&database, &fixture).await?;
        let error = kernel
            .recover_security_snapshot()
            .await
            .expect_err("a grant naming a removed standard function must fail recovery");
        require(
            matches!(
                error,
                PostgresKernelError::SecuritySnapshot(SecuritySnapshotError::UnknownGrantFunction)
            ),
            "removed standard function grant returned the wrong fail-closed error",
        )?;
        let retained: bool = {
            let session = database.open().await?;
            let exists = session
                .client()
                .query_one(
                    "SELECT count(*) > 0 FROM _orna_kernel.security_execute_grants
                     WHERE grantee_id = $1 AND function_id = $2",
                    &[&USER.to_bytes().to_vec(), &echo.to_bytes().to_vec()],
                )
                .await?
                .get(0);
            finish_session(
                Ok(exists),
                session.shutdown().await,
                "removed standard function grant retention check",
            )?
        };
        require(
            retained,
            "recovery repaired the grant naming the removed standard function",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_invocation_audit_standard_targets_through_the_historical_pin() -> TestResult<()> {
    const SESSION: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
    const EFFECTIVE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
    const AUTHORISING: PrincipalId = PrincipalId::from_bytes([0x73; 16]);

    with_test_database(|database| async move {
        let fixture = install_v2_standard_fixture(&database).await?;
        let kernel = kernel(&database)?;
        let echo = fixture.standard.executables()[0].function();
        let pair = fixture.active.pair();
        {
            let database_session = database.open().await?;
            let insertion = database_session
                .client()
                .batch_execute(&format!(
                    "INSERT INTO _orna_kernel.security_audit_events
                         (event_id, event_kind, outcome, session_principal_id,
                          effective_principal_id, authorising_principal_id, function_id,
                          source_revision_id, catalogue_revision_id)
                     VALUES (decode(repeat('a1', 16), 'hex'), 'execute', 'allowed',
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'));
                     INSERT INTO _orna_kernel.invocation_audit_events
                         (event_id, invocation_id, outcome, session_principal_id,
                          effective_principal_id, authorising_principal_id, function_id,
                          source_revision_id, catalogue_revision_id, security_audit_event_id)
                     VALUES (decode(repeat('b1', 16), 'hex'), decode(repeat('c1', 16), 'hex'),
                             'allowed', decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode('{}', 'hex'), decode('{}', 'hex'), decode('{}', 'hex'),
                             decode(repeat('a1', 16), 'hex'));",
                    raw_id_hex(SESSION.to_bytes()),
                    raw_id_hex(EFFECTIVE.to_bytes()),
                    raw_id_hex(AUTHORISING.to_bytes()),
                    raw_id_hex(echo.to_bytes()),
                    raw_id_hex(pair.source().to_bytes()),
                    raw_id_hex(pair.catalogue().to_bytes()),
                    raw_id_hex(SESSION.to_bytes()),
                    raw_id_hex(EFFECTIVE.to_bytes()),
                    raw_id_hex(AUTHORISING.to_bytes()),
                    raw_id_hex(echo.to_bytes()),
                    raw_id_hex(pair.source().to_bytes()),
                    raw_id_hex(pair.catalogue().to_bytes()),
                ))
                .await
                .map_err(Into::into);
            finish_session(
                insertion,
                database_session.shutdown().await,
                "standard invocation audit fixture insertion",
            )?;
        }
        kernel.recover().await?;

        // The application RevisionPair in the audit row is the durable pin:
        // the standard target must resolve through the authority relation and
        // the historical catalogue revision's exact verified standard snapshot.
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode(repeat('77', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let wrong_executable = recovery_error(&database).await?;
        require(
            matches!(
                wrong_executable,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "wrong standard executable revision did not fail audit recovery closed",
        )?;
        run_single_row_statement(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET function_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex')",
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;

        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_audit_events
                     DROP CONSTRAINT invocation_audit_events_target_fk;
                 DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let absent = recovery_error(&database).await?;
        require(
            matches!(
                absent,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "absent standard authority target did not fail audit recovery closed",
        )?;
        run_batch(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES (decode('{}', 'hex'), decode('{}', 'hex'), 'standard',
                         decode('{}', 'hex'), decode('{}', 'hex'));
                 ALTER TABLE _orna_kernel.invocation_audit_events
                     ADD CONSTRAINT invocation_audit_events_target_fk
                     FOREIGN KEY (catalogue_revision_id, function_id)
                     REFERENCES _orna_kernel.invocation_target_authorities(
                         catalogue_revision_id,
                         function_id
                     );",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
                raw_id_hex(fixture.standard.executables()[0].revision().id().to_bytes()),
                raw_id_hex(fixture.standard.revision().to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;

        run_batch(
            &database,
            &format!(
                "ALTER TABLE _orna_kernel.invocation_target_authorities
                     DROP CONSTRAINT invocation_target_authorities_standard_pin_fk;
                 UPDATE _orna_kernel.invocation_target_authorities
                 SET standard_library_revision_id = decode(repeat('66', 16), 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');",
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        let wrong_pin = recovery_error(&database).await?;
        require(
            matches!(
                wrong_pin,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.invocation_audit_events",
                    rule: "target function and pinned revision must exist together",
                    ..
                }
            ),
            "wrong standard revision pin did not fail audit recovery closed",
        )?;
        run_batch(
            &database,
            &format!(
                "UPDATE _orna_kernel.invocation_target_authorities
                 SET standard_library_revision_id = decode('{}', 'hex')
                 WHERE catalogue_revision_id = decode('{}', 'hex')
                   AND function_id = decode('{}', 'hex');
                 ALTER TABLE _orna_kernel.invocation_target_authorities
                     ADD CONSTRAINT invocation_target_authorities_standard_pin_fk
                     FOREIGN KEY (catalogue_revision_id, standard_library_revision_id)
                     REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id);",
                raw_id_hex(fixture.standard.revision().to_bytes()),
                raw_id_hex(pair.catalogue().to_bytes()),
                raw_id_hex(echo.to_bytes()),
            ),
        )
        .await?;
        kernel.recover().await?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_the_offline_application_catalogue_identity_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        replace_active_catalogue_identity_with_offline_sentinel(&database).await?;

        let before = snapshot_kernel_tables(&database).await?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "fixture did not retain the offline application catalogue identity",
        )?;

        let first = recovery_error(&database).await?;
        require_offline_application_catalogue_error(&first)?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "the first rejected recovery changed the offline application catalogue identity",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "recovery repaired or wrote a table after the first sentinel rejection",
        )?;

        let second = recovery_error(&database).await?;
        require_offline_application_catalogue_error(&second)?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "the repeated rejected recovery changed the offline application catalogue identity",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "recovery repaired or wrote a table after the repeated sentinel rejection",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_a_complete_raw_v2_standard_revision() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_raw_v2_standard_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;
        let standard = recovered
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("version-2 recovery returned no standard context"))?;
        let expected_boolean = expected
            .standard
            .catalogue()
            .value_types()
            .iter()
            .find(|value_type| {
                value_type.representation_contract() == "orna.kernel.value.boolean@1"
            })
            .ok_or_else(|| failure("retained standard fixture has no Boolean value type"))?;
        let application_origin = SourceOrigin::new(
            expected.application.unit.id(),
            0,
            u32::try_from(expected.application.unit.content().len())?,
        )?;

        require_standard_snapshot(standard, &expected.standard)?;
        require(
            standard
                .catalogue()
                .value_type_by_id(expected_boolean.id())
                .is_some_and(|value_type| {
                    value_type.representation_contract() == "orna.kernel.value.boolean@1"
                }),
            "the pinned recovered standard does not contain the Boolean value type",
        )?;
        let recovered_standard_reference = recovered.references().iter().find(|reference| {
            matches!(
                reference.target(),
                DefinitionReferenceTarget::ValueType(id) if id == expected_boolean.id()
            )
        });
        require(
            recovered_standard_reference.is_some_and(|reference| {
                reference.source_function() == expected.application.revisions[0].function()
                    && reference.source_revision() == expected.application.revisions[0].id()
                    && reference.ordinal() == 5
                    && reference.kind() == DefinitionReferenceKind::NamedType
                    && reference.source_origin() == application_origin
            }),
            "version-2 recovery did not return the exact standard ValueType reference",
        )?;
        require(
            recovered.pair().catalogue() == expected.application.catalogue.revision()
                && recovered.source().units() == [expected.application.unit.clone()],
            "version-2 recovery changed the active application pair or source",
        )?;
        require(
            recovered.catalogue().revision() == expected.application.catalogue.revision()
                && recovered.catalogue().schemas() == expected.application.catalogue.schemas()
                && recovered.catalogue().object_types()
                    == expected.application.catalogue.object_types()
                && recovered.catalogue().value_types()
                    == expected.application.catalogue.value_types()
                && recovered.catalogue().type_bindings()
                    == expected.application.catalogue.type_bindings()
                && recovered.catalogue().functions() == expected.application.catalogue.functions()
                && recovered.expressions() == [expected.application.expression.clone()]
                && recovered.function_revisions() == expected.revisions
                && recovered.references() == expected.application.references
                && recovered.origins() == expected.application.origins,
            "version-2 recovery changed application semantic facts",
        )?;
        require_raw_v2_value_slots(&recovered, &expected.standard)?;
        require_raw_v2_value_inventory(&recovered, &expected.standard)?;
        require(
            recovered
                .catalogue()
                .object_types()
                .iter()
                .flat_map(ObjectTypeDefinition::fields)
                .filter(|field| field.name().starts_with("scalar_"))
                .count()
                == 12,
            "version-2 raw fixture did not retain all twelve value fields",
        )
    })
    .await
}

#[test]
fn nonempty_standard_enum_fixture_matches_its_frozen_digest() -> TestResult<()> {
    let standard = verified_standard_enum_fixture()?;

    require(
        standard.digest().to_bytes() == FROZEN_STANDARD_ENUM_DIGEST,
        "verified standard enum fixture changed its frozen digest",
    )?;
    require(
        standard.catalogue().enum_types().len() == 1
            && standard.catalogue().type_bindings().len() == 1,
        "verified standard enum fixture changed its exact definition inventory",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_a_nonempty_standard_enum_and_binding_twice() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let initial = kernel.recover().await?;
        let expected = verified_standard_enum_fixture()?;
        insert_standard_snapshot(&database, &expected).await?;

        let context = CatalogueHashContext::version_two(expected.clone());
        let content_hash = catalogue_digest_with_context(
            &context,
            initial.catalogue(),
            initial.function_revisions(),
            initial.expressions(),
            initial.origins(),
            initial.references(),
        )?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            session.client().batch_execute("BEGIN").await?;
            let updated = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_revisions
                     SET canonical_hash_version = 2,
                         standard_library_revision_id = $2,
                         content_hash = $3
                     WHERE id = $1",
                    &[
                        &initial.pair().catalogue().to_bytes().to_vec(),
                        &expected.revision().to_bytes().to_vec(),
                        &content_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            require(
                updated == 1,
                "standard enum fixture did not update one catalogue",
            )?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "standard enum catalogue pin",
        )?;

        let recovered = kernel.recover().await?;
        let actual = recovered
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("standard enum recovery returned no pinned standard"))?;
        require_standard_snapshot(actual, &expected)?;
        let expected_enum = expected
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no enum"))?;
        let expected_binding = expected
            .catalogue()
            .type_bindings()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no binding"))?;
        let expected_origin =
            standard_origin(&expected, DefinitionIdentity::ValueType(expected_enum.id()))?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            let enum_row = session
                .client()
                .query_one(
                    "SELECT type_id, schema_id, name_parts, labels,
                            source_unit_id, source_start, source_end
                     FROM _orna_kernel.standard_catalogue_enum_types
                     WHERE standard_library_revision_id = $1",
                    &[&expected.revision().to_bytes().to_vec()],
                )
                .await?;
            let binding_row = session
                .client()
                .query_one(
                    "SELECT type_binding_id, kind, name_parts, target_type_kind,
                            target_type_id, target_enum_type_id
                     FROM _orna_kernel.standard_catalogue_type_bindings
                     WHERE standard_library_revision_id = $1",
                    &[&expected.revision().to_bytes().to_vec()],
                )
                .await?;
            require(
                enum_row.try_get::<_, Vec<u8>>(0)? == expected_enum.id().to_bytes().to_vec()
                    && enum_row.try_get::<_, Vec<u8>>(1)?
                        == SchemaId::from_bytes([0xc4; 16]).to_bytes().to_vec()
                    && enum_row.try_get::<_, Vec<String>>(2)? == expected_enum.name().parts()
                    && enum_row.try_get::<_, Vec<String>>(3)? == expected_enum.labels()
                    && enum_row.try_get::<_, Vec<u8>>(4)?
                        == expected_origin.source_unit().to_bytes().to_vec()
                    && enum_row.try_get::<_, i64>(5)? == i64::from(expected_origin.byte_start())
                    && enum_row.try_get::<_, i64>(6)? == i64::from(expected_origin.byte_end()),
                "standard enum recovery fixture did not retain its exact durable row",
            )?;
            require(
                binding_row.try_get::<_, Vec<u8>>(0)? == expected_binding.id().to_bytes().to_vec()
                    && binding_row.try_get::<_, String>(1)? == "qualified"
                    && binding_row.try_get::<_, Vec<String>>(2)?
                        == ["std".to_owned(), "mode_alias".to_owned()]
                    && binding_row.try_get::<_, String>(3)? == "enum"
                    && binding_row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                    && binding_row.try_get::<_, Option<Vec<u8>>>(5)?
                        == Some(expected_enum.id().to_bytes().to_vec()),
                "standard enum recovery fixture did not retain its exact enum binding tuple",
            )
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "standard enum durable rows",
        )?;

        let repeated = kernel.recover().await?;
        let repeated_standard = repeated
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("repeated standard enum recovery returned no pin"))?;
        require_standard_snapshot(repeated_standard, &expected)
    })
    .await
}

fn require_raw_v2_value_inventory(
    revision: &orna_core::revision::ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let mut value_ids = Vec::new();
    let mut legacy_scalar_slots = 0;
    let mut field_value_slots = 0;
    let mut parameter_value_slots = 0;
    let mut return_value_slots = 0;

    for object in revision.catalogue().object_types() {
        for field in object.fields() {
            let resolved = field.resolved_type();
            if let Some(value_type) = resolved.value_type() {
                value_ids.push(value_type);
                field_value_slots += 1;
            } else if resolved.legacy_scalar().is_some() {
                legacy_scalar_slots += 1;
            }
        }
    }
    for function in revision.catalogue().functions() {
        for parameter in function.parameters() {
            let resolved = parameter.resolved_type();
            if let Some(value_type) = resolved.value_type() {
                value_ids.push(value_type);
                parameter_value_slots += 1;
            } else if resolved.legacy_scalar().is_some() {
                legacy_scalar_slots += 1;
            }
        }
        match function.return_type() {
            FunctionReturn::Single(resolved) => {
                if let Some(value_type) = resolved.value_type() {
                    value_ids.push(value_type);
                    return_value_slots += 1;
                } else if resolved.legacy_scalar().is_some() {
                    legacy_scalar_slots += 1;
                }
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let resolved = column.resolved_type();
                    if let Some(value_type) = resolved.value_type() {
                        value_ids.push(value_type);
                        return_value_slots += 1;
                    } else if resolved.legacy_scalar().is_some() {
                        legacy_scalar_slots += 1;
                    }
                }
            }
            FunctionReturn::Stream(_) => {}
        }
    }

    require(
        legacy_scalar_slots == 0,
        format!("raw V2 recovery retained {legacy_scalar_slots} legacy scalar slots"),
    )?;
    require(
        field_value_slots == 12,
        format!("raw V2 recovery returned {field_value_slots} value fields, expected 12"),
    )?;
    require(
        parameter_value_slots == 13,
        format!("raw V2 recovery returned {parameter_value_slots} value parameters, expected 13"),
    )?;
    require(
        return_value_slots == 2,
        format!("raw V2 recovery returned {return_value_slots} value return slots, expected 2"),
    )?;
    require(
        value_ids.len() == 27,
        format!(
            "raw V2 recovery returned {} value slots, expected 27",
            value_ids.len()
        ),
    )?;

    for (local_name, expected_count) in [
        ("boolean", 3),
        ("integer", 3),
        ("bigint", 2),
        ("float", 2),
        ("decimal", 2),
        ("character_large_object", 2),
        ("binary_large_object", 2),
        ("uuid", 2),
        ("date", 2),
        ("time", 2),
        ("timestamp", 2),
        ("duration", 2),
        ("void", 1),
    ] {
        let name = QualifiedSemanticName::new(["std", "types", local_name])?;
        let value_type = standard
            .catalogue()
            .value_type_by_name(&name)
            .ok_or_else(|| failure(format!("retained standard fixture has no {name} value")))?;
        let actual_count = value_ids
            .iter()
            .filter(|value_type_id| **value_type_id == value_type.id())
            .count();
        require(
            actual_count == expected_count,
            format!(
                "raw V2 recovery returned {actual_count} {name} value slots, expected {expected_count}"
            ),
        )?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_raw_v2_value_tuple_pin_and_definition_tampering_without_repair() -> TestResult<()>
{
    let field_record = format!(
        "owner={} field={}",
        TypeId::from_bytes([0x81; 16]).canonical(),
        FieldId::from_bytes([0x90; 16]).canonical(),
    );
    let cases = [
        (
            "ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET value_type_id = decode(repeat('ee', 16), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "resolved value type must identify one value type in the selected pinned standard library",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET value_standard_library_revision_id = decode(repeat('ee', 16), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "resolved value type standard library revision must equal the selected catalogue pin",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check;
             ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET scalar_type = 'boolean'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
        ),
    ];
    for (statement, rule) in cases {
        let expected_record = field_record.clone();
        with_test_database(|database| async move {
            kernel(&database)?.bootstrap().await?;
            install_raw_v2_standard_revision(&database).await?;
            run_batch(&database, statement).await?;

            let before = snapshot_kernel_tables(&database).await?;
            let first = recovery_error(&database).await?;
            require_exact_raw_v2_error(&first, &expected_record, rule)?;
            require(
                snapshot_kernel_tables(&database).await? == before,
                "first raw V2 value recovery rejection changed a durable table",
            )?;

            let second = recovery_error(&database).await?;
            require_exact_raw_v2_error(&second, &expected_record, rule)?;
            require(
                snapshot_kernel_tables(&database).await? == before,
                "repeated raw V2 value recovery rejection changed a durable table",
            )
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_the_raw_standard_catalogue_offline_sentinel_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_raw_v2_standard_revision(&database).await?;
        run_batch(
            &database,
            "UPDATE _orna_kernel.standard_library_revisions
             SET catalogue_revision_id = decode(repeat('00', 16), 'hex')",
        )
        .await?;

        let before = snapshot_kernel_tables(&database).await?;
        let first = recovery_error(&database).await?;
        require_offline_standard_catalogue_error(&first)?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "standard sentinel recovery changed a durable table after the first rejection",
        )?;

        let second = recovery_error(&database).await?;
        require_offline_standard_catalogue_error(&second)?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "standard sentinel recovery changed a durable table after the repeated rejection",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_exact_nonempty_source_for_an_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_source_only_revision(&database, "schema source_only;\n").await?;

        let recovered = kernel(&database)?.recover().await?;
        let units = recovered.source().units();

        require(
            units == [expected],
            "recovery changed exact retained source",
        )?;
        require(
            recovered.catalogue().schemas().is_empty(),
            "source-only fixture recovered semantic definitions",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_an_exact_schema_and_its_unicode_source_origin() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_schema_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "schema recovery changed exact retained source",
        )?;
        require(
            recovered.catalogue().schemas() == [expected.schema],
            "schema recovery changed the exact schema definition",
        )?;
        require(
            recovered.origins() == [expected.origin],
            "schema recovery changed the exact source origin",
        )?;
        require(
            recovered.catalogue().object_types().is_empty()
                && recovered.catalogue().functions().is_empty(),
            "schema recovery invented later catalogue members",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_complete_objects_fields_references_and_expression_origins() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_object_revision(&database, false).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "object recovery changed exact retained Unicode source",
        )?;
        require(
            recovered.catalogue().revision() == expected.catalogue.revision()
                && recovered.catalogue().schemas() == expected.catalogue.schemas()
                && recovered.catalogue().object_types() == expected.catalogue.object_types()
                && recovered.catalogue().functions() == expected.catalogue.functions(),
            "object recovery changed object, owner-qualified field, or reference semantics",
        )?;
        require(
            recovered.expressions() == [expected.expression],
            "object recovery changed the expression artifact",
        )?;
        require(
            recovered.origins() == expected.origins,
            "object recovery changed exact Unicode definition origins",
        )?;
        let objects = recovered.catalogue().object_types();
        require(
            objects.len() == 3
                && objects[0].fields().len() == 17
                && objects[1].fields().len() == 1
                && objects[2].fields().is_empty(),
            "object recovery changed owner-qualified field grouping",
        )?;
        require(
            objects[0].fields()[12].id() == objects[1].fields()[0].id(),
            "duplicate field identities across owners did not remain owner-qualified",
        )?;
        require(
            objects[0].fields()[12].is_required_unique_reference()
                && objects[0].fields()[12].resolved_type()
                    == ResolvedType::reference(objects[1].id()),
            "object recovery changed the required unique reference shape or target",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn reconstructs_shared_expression_defaults_before_physical_rejection() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, true).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_compiler_deployable_server_and_client_function_state() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_function_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.catalogue().functions() == expected.catalogue.functions(),
            "function recovery changed signatures, modifiers, parameters, or returns",
        )?;
        require(
            recovered.function_revisions() == expected.revisions,
            "function recovery changed current immutable revision records",
        )?;
        require(
            recovered.historical_function_revisions().is_empty(),
            "function recovery invented historical revisions",
        )?;
        require(
            recovered.references() == expected.references,
            "function recovery changed ordered owner-qualified references",
        )?;
        require(
            recovered.origins() == expected.origins,
            "function recovery changed exact current definition origins",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_reused_current_revisions_and_retired_function_history() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let introduction = install_function_revision(&database).await?;
        let expected = install_reused_function_catalogue(&database, &introduction).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "reused revision recovery changed the active retained source",
        )?;
        require(
            recovered.catalogue().revision() == expected.catalogue.revision()
                && recovered.catalogue().schemas() == expected.catalogue.schemas()
                && recovered.catalogue().object_types() == expected.catalogue.object_types()
                && recovered.catalogue().functions() == expected.catalogue.functions(),
            "reused revision recovery changed the active semantic catalogue",
        )?;
        require(
            recovered.function_revisions() == expected.current_revisions,
            "reused revision recovery changed current immutable revision records",
        )?;
        require(
            recovered.historical_function_revisions() == [expected.retired_revision],
            "retired function revision was not recovered as immutable history",
        )?;
        require(
            recovered.references() == expected.references,
            "reused revision recovery changed active definition references",
        )?;
        require(
            recovered.origins() == expected.origins,
            "reused revision recovery changed current definition origins",
        )?;
        let current_function_origin = recovered
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Function(recovered.catalogue().functions()[0].id())
            })
            .ok_or_else(|| failure("recovered current function origin is missing"))?;
        require(
            current_function_origin.source()
                != recovered.function_revisions()[0].declaration_origin(),
            "reused revision collapsed current definition origin into historical declaration origin",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_function_signature_revision_artifact_and_reference_tampering() -> TestResult<()> {
    let cases = [
        "ALTER TABLE _orna_kernel.catalogue_function_parameters
             DROP CONSTRAINT catalogue_function_parameters_check;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET scalar_type = NULL
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_function_parameters
         SET ordinal = 99
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_function_parameters DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET function_id = decode(repeat('ff', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET name_parts = ARRAY['wrong', 'server_rows']
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_functions
         SET current_function_revision_id = decode(repeat('e2', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET domain = 'client', transaction_mode = NULL
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions
             DROP CONSTRAINT catalogue_functions_transaction_mode_check;
         UPDATE _orna_kernel.catalogue_functions
         SET transaction_mode = 'manual'
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions
             DROP CONSTRAINT catalogue_functions_check1,
             DROP CONSTRAINT catalogue_functions_return_kind_presence_check;
         UPDATE _orna_kernel.catalogue_functions
         SET return_type_kind = 'scalar', return_scalar_type = 'boolean'
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_function_return_columns
         SET ordinal = 99
         WHERE function_id = decode(repeat('d1', 16), 'hex') AND ordinal = 1",
        "ALTER TABLE _orna_kernel.catalogue_function_parameters DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET default_expression_id = decode(repeat('ff', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_owner_type_id = decode(repeat('82', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 1",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_owner_function_id = decode(repeat('d2', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 2",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET source_function_revision_id = decode(repeat('e2', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET ordinal = ordinal + 10
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET source_end = 999
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.definition_references
             DROP CONSTRAINT definition_references_reference_target_compatibility_check;
         UPDATE _orna_kernel.definition_references
         SET reference_kind = 'function_call'
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_definition_id = decode(repeat('ff', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'retired'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'candidate'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'invalid'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions
             DROP CONSTRAINT function_revisions_revision_number_check;
         UPDATE _orna_kernel.function_revisions
         SET revision_number = 0
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET semantic_ir_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions
             DROP CONSTRAINT function_revisions_hash_contract_version_check;
         UPDATE _orna_kernel.function_revisions
         SET hash_contract_version = 2
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "DELETE FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "INSERT INTO _orna_kernel.function_artifacts
            (function_revision_id, artifact_kind, format, format_version,
             payload, content_hash)
         SELECT function_revision_id, 'client_bytecode', format, format_version,
                payload, content_hash
         FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_artifacts
         SET artifact_kind = 'client_bytecode'
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_format_check;
         UPDATE _orna_kernel.function_artifacts
         SET format = ''
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_format_version_check;
         UPDATE _orna_kernel.function_artifacts
         SET format_version = 0
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_artifacts
         SET payload = payload || decode('00', 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_content_hash_check;
         UPDATE _orna_kernel.function_artifacts
         SET content_hash = decode(repeat('ff', 31), 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_hash_contract_version_check;
         UPDATE _orna_kernel.function_artifacts
         SET hash_contract_version = 2
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_artifacts
         SET function_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_revisions
         SET introduced_catalogue_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE id = decode(repeat('e3', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_revisions
         SET introduced_catalogue_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE id = decode(repeat('e3', 16), 'hex');
         DELETE FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e3', 16), 'hex')",
    ];
    for (index, statement) in cases.into_iter().enumerate() {
        reject_function_tamper(statement).await.map_err(|error| {
            failure(format!(
                "function tamper case {index} failed before recovery rejection: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_crossed_write_reference_updates_at_the_v6_compatibility_check() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_function_revision(&database).await?;

        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            for (ordinal, original_kind, crossed_kind) in [
                (0_i64, "query_object", "write_field"),
                (1_i64, "query_field", "write_object"),
            ] {
                session.client().batch_execute("BEGIN").await?;
                let update_result = session
                    .client()
                    .execute(
                        "UPDATE _orna_kernel.definition_references
                         SET reference_kind = $1
                         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
                           AND ordinal = $2",
                        &[&crossed_kind, &ordinal],
                    )
                    .await;
                let rollback_result = session.client().batch_execute("ROLLBACK").await;
                let constraint = update_result
                    .as_ref()
                    .err()
                    .and_then(|error| error.as_db_error())
                    .and_then(|error| error.constraint());
                require(
                    update_result.is_err(),
                    format!("crossed write reference update {crossed_kind} at ordinal {ordinal} was accepted"),
                )?;
                require(
                    constraint == Some("definition_references_reference_target_compatibility_check"),
                    format!(
                        "crossed write reference update {crossed_kind} at ordinal {ordinal} failed for {constraint:?}"
                    ),
                )?;
                rollback_result?;

                let row = session
                    .client()
                    .query_one(
                        "SELECT reference_kind
                         FROM _orna_kernel.definition_references
                         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
                           AND ordinal = $1",
                        &[&ordinal],
                    )
                    .await?;
                let recovered_kind: String = row.try_get(0)?;
                require(
                    recovered_kind == original_kind,
                    format!(
                        "rolled-back crossed write reference update changed ordinal {ordinal} to {recovered_kind:?}"
                    ),
                )?;
            }
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "crossed write-reference constraint probe",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_crossed_write_reference_kinds_before_catalogue_hash_validation() -> TestResult<()>
{
    for (statement, crossed_kind) in [
        (
            "ALTER TABLE _orna_kernel.definition_references
                 DROP CONSTRAINT definition_references_reference_target_compatibility_check;
             UPDATE _orna_kernel.definition_references
             SET reference_kind = 'write_field'
             WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
               AND ordinal = 0",
            "write_field",
        ),
        (
            "ALTER TABLE _orna_kernel.definition_references
                 DROP CONSTRAINT definition_references_reference_target_compatibility_check;
             UPDATE _orna_kernel.definition_references
             SET reference_kind = 'write_object'
             WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
               AND ordinal = 1",
            "write_object",
        ),
    ] {
        reject_function_tamper_expected(
            statement,
            ExpectedRecoveryError::DurableExact {
                relation: "_orna_kernel.definition_references",
                rule: "reference kind must be compatible with its exact target kind",
            },
        )
        .await
        .map_err(|error| {
            failure(format!(
                "crossed {crossed_kind} recovery case failed: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_unknown_reference_kind_before_catalogue_hash_validation() -> TestResult<()> {
    reject_function_tamper_expected(
        "ALTER TABLE _orna_kernel.definition_references
             DROP CONSTRAINT definition_references_reference_kind_check,
             DROP CONSTRAINT definition_references_reference_target_compatibility_check;
         UPDATE _orna_kernel.definition_references
         SET reference_kind = 'future_reference_kind'
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        ExpectedRecoveryError::DurableExact {
            relation: "_orna_kernel.definition_references",
            rule: "reference kind must be one exact supported semantic relation",
        },
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_void_parameters_and_rows_columns_at_their_decoder_relations() -> TestResult<()> {
    for (statement, relation) in [
        (
            "ALTER TABLE _orna_kernel.catalogue_function_parameters
                 DROP CONSTRAINT catalogue_function_parameters_scalar_type_check;
             UPDATE _orna_kernel.catalogue_function_parameters
             SET scalar_type = 'void'
             WHERE function_id = decode(repeat('d1', 16), 'hex')
               AND parameter_id = decode(repeat('b1', 16), 'hex')",
            "_orna_kernel.catalogue_function_parameters",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_function_return_columns
                 DROP CONSTRAINT catalogue_function_return_columns_scalar_type_check;
             UPDATE _orna_kernel.catalogue_function_return_columns
             SET scalar_type = 'void'
             WHERE function_id = decode(repeat('d1', 16), 'hex') AND ordinal = 0",
            "_orna_kernel.catalogue_function_return_columns",
        ),
    ] {
        reject_function_tamper_expected(statement, ExpectedRecoveryError::Durable(relation))
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_retained_introduction_source_catalogue_and_history_tampering() -> TestResult<()> {
    let cases = [
        "UPDATE _orna_kernel.function_revisions
         SET status = 'active'
         WHERE id = decode(repeat('e3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         )",
        "UPDATE _orna_kernel.source_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = (
             SELECT parent_source_revision_id
             FROM _orna_kernel.source_revisions
             WHERE id = (SELECT source_revision_id FROM _orna_kernel.active_revision)
         )",
        "UPDATE _orna_kernel.catalogue_functions
         SET security_mode = 'definer'
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET target_definition_id = decode(repeat('82', 16), 'hex')
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_start = 1
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_end = 999
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_start = 11
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_unit_id = decode(repeat('f4', 16), 'hex')
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_revisions
         SET parent_catalogue_revision_id = NULL
         WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)",
    ];
    for (index, statement) in cases.into_iter().enumerate() {
        reject_history_tamper(statement).await.map_err(|error| {
            failure(format!(
                "history tamper case {index} failed before recovery rejection: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_valid_function_introduction_from_a_sibling_branch() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let introduction = install_function_revision(&database).await?;
        install_reused_function_catalogue(&database, &introduction).await?;
        install_valid_sibling_introduction(&database, &introduction).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_schema_name_and_incomplete_origin() -> TestResult<()> {
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas
         SET name_parts = ARRAY['tampered']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await?;
    reject_schema_tamper(
        "ALTER TABLE _orna_kernel.catalogue_schemas
             DROP CONSTRAINT catalogue_schemas_source_origin_check;
         UPDATE _orna_kernel.catalogue_schemas SET source_end = NULL",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_schemas"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_schema_origin_from_another_bundle_or_invalid_span() -> TestResult<()> {
    reject_schema_tamper(
        "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
         VALUES (
             decode(repeat('71', 16), 'hex'),
             decode(repeat('00', 32), 'hex')
         );
         INSERT INTO _orna_kernel.source_units
             (id, bundle_id, ordinal, logical_path, content, content_hash)
         VALUES (
             decode(repeat('72', 16), 'hex'),
             decode(repeat('71', 16), 'hex'),
             0,
             'other.orna',
             'other',
             decode(repeat('00', 32), 'hex')
         );
         UPDATE _orna_kernel.catalogue_schemas
         SET source_unit_id = decode(repeat('72', 16), 'hex')",
        ExpectedRecoveryError::Revision,
    )
    .await?;
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas SET source_end = 999",
        ExpectedRecoveryError::Revision,
    )
    .await?;
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas SET source_start = 11",
        ExpectedRecoveryError::Revision,
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_schema_catalogue_hash() -> TestResult<()> {
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_revisions
         SET content_hash = decode(repeat('73', 32), 'hex')
         WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_enum_label_name_and_origin_tampering() -> TestResult<()> {
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET labels = ARRAY['customer', 'owner''s', 'lead']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET labels = ARRAY['lead', 'lead', 'customer']",
        ExpectedRecoveryError::Catalogue,
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET name_parts = ARRAY['wrong', 'stage']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_enum_types"),
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET source_start = source_start + 1",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_object_field_expression_and_origin_tampering() -> TestResult<()> {
    for (statement, expected) in [
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check;
             UPDATE _orna_kernel.catalogue_fields
             SET target_type_id = decode(repeat('82', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_catalogue_revision_id_owner_type_id_fkey;
             UPDATE _orna_kernel.catalogue_fields
             SET owner_type_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_fields SET ordinal = 99
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Catalogue,
        ),
        (
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             SELECT catalogue_revision_id, decode(repeat('62', 16), 'hex'), ARRAY['other'],
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_schemas LIMIT 1;
             UPDATE _orna_kernel.catalogue_object_types
             SET schema_id = decode(repeat('62', 16), 'hex')
             WHERE type_id = decode(repeat('81', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_object_types"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_catalogue_revision_id_target_type_id_fkey;
             UPDATE _orna_kernel.catalogue_fields
             SET target_type_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('a1', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check1;
             UPDATE _orna_kernel.catalogue_fields SET on_delete = 'restrict'
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check2;
             UPDATE _orna_kernel.catalogue_fields SET on_delete = 'set_null'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('a1', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_scalar_type_check;
             UPDATE _orna_kernel.catalogue_fields SET scalar_type = 'void'
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_fields SET type_kind = 'named'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('a0', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "DO $drop$
             DECLARE generated_name name;
             BEGIN
                 SELECT conname INTO STRICT generated_name
                 FROM pg_catalog.pg_constraint
                 WHERE conrelid = '_orna_kernel.catalogue_fields'::regclass
                   AND contype = 'f'
                   AND conname LIKE '%default%';
                 EXECUTE pg_catalog.format(
                     'ALTER TABLE _orna_kernel.catalogue_fields DROP CONSTRAINT %I',
                     generated_name
                 );
             END
             $drop$;
             UPDATE _orna_kernel.catalogue_fields
             SET default_expression_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_expressions SET payload = payload || decode('00', 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_hash_algorithm_check;
             UPDATE _orna_kernel.catalogue_expressions SET hash_algorithm = 'sha512'",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_hash_contract_version_check;
             UPDATE _orna_kernel.catalogue_expressions SET hash_contract_version = 2",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_source_origin_check;
             UPDATE _orna_kernel.catalogue_expressions SET source_end = NULL",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_field_id_check;
             UPDATE _orna_kernel.catalogue_fields
             SET field_id = decode(repeat('ef', 15), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_expression_id_check;
             UPDATE _orna_kernel.catalogue_expressions
             SET expression_id = decode(repeat('ef', 15), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
    ] {
        reject_object_tamper(statement, expected).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_exact_physical_catalogue_tampering() -> TestResult<()> {
    const TABLE: &str = "_orna_data.t_81818181818181818181818181818181";
    const TARGET: &str = "_orna_data.t_82828282828282828282828282828282";
    const UNIQUE: &str = "uq_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const UNIQUE_FIELD: &str = "f_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const OTHER_REFERENCE: &str = "f_a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    let statements = [
        format!("DROP TABLE {TABLE} CASCADE"),
        "CREATE TABLE _orna_data.extra_relation (value integer)".to_owned(),
        format!("ALTER TABLE {TABLE} RENAME TO wrong_name"),
        format!("ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 TYPE bigint"),
        format!(
            "ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 DROP NOT NULL"
        ),
        format!(
            "ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 SET DEFAULT 1"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT ck_81818181818181818181818181818181_object_id"
        ),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT pk_81818181818181818181818181818181 CASCADE"),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0"),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0;
             ALTER TABLE {TABLE} ADD CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0
             FOREIGN KEY (f_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0)
             REFERENCES {TARGET} (_orna_object_id) ON DELETE CASCADE"
        ),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE}"),
        format!("ALTER TABLE {TABLE} RENAME CONSTRAINT {UNIQUE} TO wrong_unique_name"),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE} UNIQUE ({OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE} UNIQUE ({UNIQUE_FIELD})
             DEFERRABLE INITIALLY DEFERRED"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE ({UNIQUE_FIELD}, {OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE NULLS NOT DISTINCT ({UNIQUE_FIELD})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE ({UNIQUE_FIELD}) INCLUDE ({OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             CREATE UNIQUE INDEX {UNIQUE} ON {TABLE} ({UNIQUE_FIELD})
             WHERE {UNIQUE_FIELD} IS NOT NULL"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             CREATE UNIQUE INDEX {UNIQUE} ON {TABLE} ((octet_length({UNIQUE_FIELD})))"
        ),
        format!("CREATE UNIQUE INDEX unexpected_unique_index ON {TABLE} ({UNIQUE_FIELD})"),
        format!("CREATE INDEX unexpected_index ON {TABLE} (f_91919191919191919191919191919191)"),
        format!("ALTER TABLE {TABLE} ENABLE ROW LEVEL SECURITY"),
        format!("GRANT MAINTAIN ON TABLE {TABLE} TO PUBLIC"),
        format!(
            "ALTER TABLE {TABLE} ADD COLUMN dropped integer; ALTER TABLE {TABLE} DROP COLUMN dropped"
        ),
        format!("ALTER TABLE {TABLE} DISABLE TRIGGER ALL"),
        format!("CREATE TABLE public.inbound (value bytea REFERENCES {TABLE} (_orna_object_id))"),
        format!("CREATE TABLE public.inherited () INHERITS ({TABLE})"),
        format!(
            "CREATE FUNCTION public.noop_trigger() RETURNS trigger LANGUAGE plpgsql
             AS 'BEGIN RETURN NEW; END';
             CREATE TRIGGER unexpected_trigger BEFORE INSERT ON {TABLE}
             FOR EACH ROW EXECUTE FUNCTION public.noop_trigger()"
        ),
        format!("CREATE POLICY unexpected_policy ON {TABLE} USING (true)"),
        format!("CREATE RULE unexpected_rule AS ON INSERT TO {TABLE} DO NOTHING"),
    ];
    for statement in statements {
        reject_object_tamper(&statement, ExpectedRecoveryError::AnyDurable).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn trusted_catalogue_search_path_ignores_shadow_privilege_functions() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, false).await?;
        run_batch(
            &database,
            "CREATE FUNCTION public.has_table_privilege(name, oid, text)
             RETURNS boolean LANGUAGE sql IMMUTABLE AS 'SELECT true';
             CREATE FUNCTION public.octet_length(bytea)
             RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 16'",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        PostgresKernel::new(hostile_config).recover().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn trusted_catalogue_rejects_checks_bound_to_a_shadow_function() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, false).await?;
        run_batch(
            &database,
            "CREATE FUNCTION public.octet_length(bytea)
             RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 16'",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        let (client, connection) = hostile_config.connect(tokio_postgres::NoTls).await?;
        let driver = tokio::spawn(connection);
        client
            .batch_execute(
                "ALTER TABLE _orna_data.t_81818181818181818181818181818181
                     DROP CONSTRAINT ck_81818181818181818181818181818181_object_id;
                 ALTER TABLE _orna_data.t_81818181818181818181818181818181
                     ADD CONSTRAINT ck_81818181818181818181818181818181_object_id
                     CHECK (octet_length(_orna_object_id) = 16)",
            )
            .await?;
        drop(client);
        driver.await??;

        let mut hostile_recovery = database.config()?;
        hostile_recovery.options("-c search_path=public,pg_catalog");
        let error = match PostgresKernel::new(hostile_recovery).recover().await {
            Ok(_) => {
                return Err(failure(
                    "shadow-bound physical check recovered successfully",
                ));
            }
            Err(error) => error,
        };
        require_expected_error(error, ExpectedRecoveryError::AnyDurable)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_migration_history_before_durable_state() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(
            &database,
            "UPDATE _orna_kernel.schema_migrations
             SET checksum = decode(repeat('00', 32), 'hex')
             WHERE version = 5;
             UPDATE _orna_kernel.source_bundles
             SET content_hash = decode(repeat('ff', 32), 'hex')",
        )
        .await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(error, PostgresKernelError::MigrationMismatch { version: 5 }),
            format!("tampered migration history produced the wrong failure: {error}"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_source_content_ordinals_encoding_and_contract_tampering() -> TestResult<()> {
    reject_source_tamper(
        "UPDATE _orna_kernel.source_units SET content = content || 'tampered'",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_units SET ordinal = 1",
        ExpectedRecoveryError::Canonical,
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_bundles
         SET content_hash = decode(repeat('fe', 32), 'hex')",
        ExpectedRecoveryError::Durable("_orna_kernel.source_bundles"),
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_revisions
         SET content_hash = decode(repeat('fd', 32), 'hex')",
        ExpectedRecoveryError::Durable("_orna_kernel.source_revisions"),
    )
    .await?;
    reject_source_tamper(
        "ALTER TABLE _orna_kernel.source_units
             DROP CONSTRAINT source_units_encoding_check;
         UPDATE _orna_kernel.source_units SET encoding = 'latin-1'",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await?;
    reject_source_tamper(
        "ALTER TABLE _orna_kernel.source_units
             DROP CONSTRAINT source_units_hash_contract_version_check;
         UPDATE _orna_kernel.source_units SET hash_contract_version = 2",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_functions_and_unexpected_physical_relations() -> TestResult<()> {
    reject_unsupported_state(UNSUPPORTED_FUNCTION_SQL, "_orna_kernel.catalogue_functions").await?;
    reject_unsupported_state(
        "CREATE TABLE _orna_data.unexpected (value integer)",
        "_orna_data",
    )
    .await?;
    reject_unsupported_state("CREATE SEQUENCE _orna_data.unexpected", "_orna_data").await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_multi_revision_catalogue_and_source_ancestry_cycle() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(
            &database,
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash)
             VALUES (
                decode(repeat('b1', 16), 'hex'),
                decode(repeat('00', 32), 'hex')
             );
             INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash)
             SELECT
                decode(repeat('b2', 16), 'hex'),
                source_revision_id,
                decode(repeat('b1', 16), 'hex'),
                decode(repeat('00', 32), 'hex')
             FROM _orna_kernel.active_revision;
             INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash)
             SELECT
                decode(repeat('b3', 16), 'hex'),
                decode(repeat('b2', 16), 'hex'),
                catalogue_revision_id,
                decode(repeat('00', 32), 'hex')
             FROM _orna_kernel.active_revision;
             UPDATE _orna_kernel.source_revisions
             SET parent_source_revision_id = decode(repeat('b2', 16), 'hex')
             WHERE id = (
                SELECT source_revision_id FROM _orna_kernel.active_revision
             );
             UPDATE _orna_kernel.catalogue_revisions
             SET parent_catalogue_revision_id = decode(repeat('b3', 16), 'hex')
             WHERE id = (
                SELECT catalogue_revision_id FROM _orna_kernel.active_revision
             )",
        )
        .await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        )
    })
    .await
}

#[derive(Clone, Copy)]
enum ExpectedRecoveryError {
    AnyInvariant,
    AnyDurable,
    Canonical,
    Catalogue,
    Durable(&'static str),
    DurableExact {
        relation: &'static str,
        rule: &'static str,
    },
    Revision,
}

async fn reject_object_tamper(statement: &str, expected: ExpectedRecoveryError) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, false).await?;
        run_batch(&database, statement).await?;

        require_expected_error(recovery_error(&database).await?, expected)
    })
    .await
}

async fn reject_function_tamper(statement: &str) -> TestResult<()> {
    reject_function_tamper_expected(statement, ExpectedRecoveryError::AnyInvariant).await
}

async fn reject_function_tamper_expected(
    statement: &str,
    expected: ExpectedRecoveryError,
) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_function_revision(&database).await?;
        run_batch(&database, statement).await?;

        require_expected_error(recovery_error(&database).await?, expected)
    })
    .await
}

async fn reject_history_tamper(statement: &str) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let introduction = install_function_revision(&database).await?;
        install_reused_function_catalogue(&database, &introduction).await?;
        run_batch(&database, statement).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::AnyInvariant,
        )
    })
    .await
}

async fn reject_schema_tamper(
    statement: &'static str,
    expected: ExpectedRecoveryError,
) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_schema_revision(&database).await?;
        run_batch(&database, statement).await?;

        require_expected_error(recovery_error(&database).await?, expected)
    })
    .await
}

async fn reject_enum_tamper(
    statement: &'static str,
    expected: ExpectedRecoveryError,
) -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            format!(
                "enum schema compiler diagnostics: {:?}",
                schema_report.diagnostics()
            ),
        )?;
        let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
        let version_one = kernel.apply(&schema_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let enum_candidate =
            standard_client_candidate(STANDARD_ENUM_SOURCE, &version_two, &upgrade)?;
        let installed = kernel.apply(&enum_candidate).await?;
        require(
            installed.catalogue().enum_types() == enum_candidate.candidate().enum_types(),
            "enum tamper fixture did not install its exact semantic enum",
        )?;

        run_batch(&database, statement).await?;
        require_expected_error(recovery_error(&database).await?, expected)
    })
    .await
}

async fn reject_source_tamper(
    statement: &'static str,
    expected: ExpectedRecoveryError,
) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_source_only_revision(&database, "schema source_only;\n").await?;
        run_batch(&database, statement).await?;

        require_expected_error(recovery_error(&database).await?, expected)
    })
    .await
}

async fn reject_unsupported_state(
    statement: &'static str,
    relation: &'static str,
) -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(&database, statement).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable(relation),
        )
    })
    .await
}

async fn install_source_only_revision(
    database: &TestDatabase,
    content: &str,
) -> TestResult<StoredSourceUnit> {
    let session = database.open().await?;
    let operation_result: TestResult<StoredSourceUnit> = async {
        let row = session
            .client()
            .query_one(
                "SELECT source.id, source.bundle_id
                 FROM _orna_kernel.active_revision AS active
                 JOIN _orna_kernel.source_revisions AS source
                   ON source.id = active.source_revision_id",
                &[],
            )
            .await?;
        let source = SourceRevisionId::from_bytes(exact_identity(
            row.try_get("id")?,
            "active source revision identity",
        )?);
        let bundle = SourceBundleId::from_bytes(exact_identity(
            row.try_get("bundle_id")?,
            "active source bundle identity",
        )?);
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x51; 16]),
            0,
            "source-only.orna",
            content,
            source_unit_content_digest(content)?,
        )?;
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
        let source_hash = source_revision_record_digest(bundle, None, bundle_hash)?;

        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &unit.id().to_bytes().to_vec(),
                    &bundle.to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &unit.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.source_bundles
                 SET content_hash = $2
                 WHERE id = $1",
                &[
                    &bundle.to_bytes().to_vec(),
                    &bundle_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.source_revisions
                 SET content_hash = $2
                 WHERE id = $1",
                &[
                    &source.to_bytes().to_vec(),
                    &source_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(unit)
    }
    .await;
    finish_session(operation_result, session.shutdown().await, "source fixture")
}

struct SchemaFixture {
    unit: StoredSourceUnit,
    schema: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct ObjectFixture {
    unit: StoredSourceUnit,
    catalogue: CatalogueSnapshot,
    expression: ExpressionArtifact,
    origins: Vec<DefinitionOrigin>,
}

struct FunctionFixture {
    unit: StoredSourceUnit,
    catalogue: CatalogueSnapshot,
    expression: ExpressionArtifact,
    revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
    origins: Vec<DefinitionOrigin>,
}

struct RawV2Fixture {
    standard: VerifiedStandardLibrarySnapshot,
    application: FunctionFixture,
    revisions: Vec<FunctionRevisionRecord>,
}

fn verified_standard_enum_fixture() -> TestResult<VerifiedStandardLibrarySnapshot> {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xc1; 16]),
        0,
        "std/enum.orna",
        "standard enum fixture",
        source_unit_content_digest("standard enum fixture")?,
    )?;
    let bundle = SourceBundleId::from_bytes([0xc2; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0xc3; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash)?,
    )?;
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes([0xc4; 16]),
        QualifiedSemanticName::new(["std"])?,
    );
    let enum_type = EnumTypeDefinition::new(
        TypeId::from_bytes([0xc5; 16]),
        QualifiedSemanticName::new(["std", "mode"])?,
        vec!["one".to_owned(), "two".to_owned()],
    );
    let binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "mode_alias"])?,
        enum_type.id(),
    )?;
    let source_unit = SourceUnitId::from_bytes([0xc1; 16]);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            SourceOrigin::new(source_unit, 0, 1)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(enum_type.id()),
            SourceOrigin::new(source_unit, 1, 2)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding.id()),
            SourceOrigin::new(source_unit, 2, 3)?,
        ),
    ];
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0xc6; 16]),
        vec![schema],
        vec![],
        vec![],
        vec![enum_type],
        vec![binding],
    )?;
    let snapshot = StandardLibrarySnapshot::new(
        orna_core::StandardLibraryRevisionId::from_bytes([0xc7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes(FROZEN_STANDARD_ENUM_DIGEST),
    )?;
    Ok(verify_standard_library_snapshot(snapshot)?)
}

fn raw_v2_value_type_by_slot(
    standard: &VerifiedStandardLibrarySnapshot,
    slot: &str,
) -> TestResult<Option<TypeId>> {
    let local_name = match slot {
        "scalar_0" | "duplicate_owner_qualified_parameter" => "boolean",
        "scalar_1" | "value" => "integer",
        "scalar_2" => "bigint",
        "scalar_3" => "float",
        "scalar_4" => "decimal",
        "scalar_5" => "character_large_object",
        "scalar_6" => "binary_large_object",
        "scalar_7" => "uuid",
        "scalar_8" => "date",
        "scalar_9" => "time",
        "scalar_10" => "timestamp",
        "scalar_11" => "duration",
        _ => return Ok(None),
    };
    let name = QualifiedSemanticName::new(["std", "types", local_name])?;
    standard
        .catalogue()
        .value_type_by_name(&name)
        .map(|value_type| Some(value_type.id()))
        .ok_or_else(|| failure(format!("raw V2 fixture has no standard value named {name}")))
}

fn raw_v2_single_return_value_type(
    standard: &VerifiedStandardLibrarySnapshot,
    function: &FunctionDefinition,
) -> TestResult<Option<TypeId>> {
    if function.name().parts().last().map(String::as_str) != Some("client_single") {
        return Ok(None);
    }
    let name = QualifiedSemanticName::new(["std", "types", "void"])?;
    standard
        .catalogue()
        .value_type_by_name(&name)
        .map(|value_type| Some(value_type.id()))
        .ok_or_else(|| failure(format!("raw V2 fixture has no standard value named {name}")))
}

fn raw_v2_value_catalogue(
    standard: &VerifiedStandardLibrarySnapshot,
    catalogue: &CatalogueSnapshot,
) -> TestResult<CatalogueSnapshot> {
    let objects = catalogue
        .object_types()
        .iter()
        .map(|object| {
            let fields = object
                .fields()
                .iter()
                .map(|field| {
                    Ok(FieldDefinition::new(
                        field.id(),
                        field.name(),
                        field.ordinal(),
                        raw_v2_value_type_by_slot(standard, field.name())?
                            .map_or(field.resolved_type(), ResolvedType::value),
                        field.nullable(),
                        field.unique(),
                        field.default_expression(),
                        field.on_delete(),
                    ))
                })
                .collect::<TestResult<Vec<_>>>()?;
            Ok(ObjectTypeDefinition::new(
                object.id(),
                object.name().clone(),
                fields,
            ))
        })
        .collect::<TestResult<Vec<_>>>()?;
    let functions = catalogue
        .functions()
        .iter()
        .map(|function| {
            let parameters = function
                .parameters()
                .iter()
                .map(|parameter| {
                    Ok(ParameterDefinition::new(
                        parameter.id(),
                        parameter.name(),
                        parameter.ordinal(),
                        raw_v2_value_type_by_slot(standard, parameter.name())?
                            .map_or(parameter.resolved_type(), ResolvedType::value),
                        parameter.default_expression(),
                    ))
                })
                .collect::<TestResult<Vec<_>>>()?;
            let return_type = match function.return_type() {
                FunctionReturn::Single(resolved_type) => FunctionReturn::Single(
                    raw_v2_single_return_value_type(standard, function)?
                        .map_or(*resolved_type, ResolvedType::value),
                ),
                FunctionReturn::Rows(columns) => FunctionReturn::Rows(
                    columns
                        .iter()
                        .map(|column| {
                            Ok(FunctionReturnColumnDefinition::new(
                                column.name(),
                                column.ordinal(),
                                raw_v2_value_type_by_slot(standard, column.name())?
                                    .map_or(column.resolved_type(), ResolvedType::value),
                            ))
                        })
                        .collect::<TestResult<Vec<_>>>()?,
                ),
                FunctionReturn::Stream(resolved_type) => FunctionReturn::Stream(*resolved_type),
            };
            Ok(FunctionDefinition::new(
                function.id(),
                function.name().clone(),
                function.domain(),
                parameters,
                return_type,
                function.current_revision(),
                function.security(),
                function.transaction(),
                function.volatility(),
            ))
        })
        .collect::<TestResult<Vec<_>>>()?;
    Ok(CatalogueSnapshot::new_with_functions(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        objects,
        functions,
    )?)
}

fn require_raw_v2_value_slots(
    revision: &orna_core::revision::ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    for object in revision.catalogue().object_types() {
        for field in object.fields() {
            let Some(value_type) = raw_v2_value_type_by_slot(standard, field.name())? else {
                continue;
            };
            require(
                field.resolved_type() == ResolvedType::value(value_type),
                format!(
                    "raw V2 field {} did not recover exact value type {}",
                    field.id().canonical(),
                    value_type.canonical()
                ),
            )?;
        }
    }
    for function in revision.catalogue().functions() {
        for parameter in function.parameters() {
            let Some(value_type) = raw_v2_value_type_by_slot(standard, parameter.name())? else {
                continue;
            };
            require(
                parameter.resolved_type() == ResolvedType::value(value_type),
                format!(
                    "raw V2 parameter {} did not recover exact value type {}",
                    parameter.id().canonical(),
                    value_type.canonical()
                ),
            )?;
        }
        match function.return_type() {
            FunctionReturn::Single(resolved_type) => {
                let Some(value_type) = raw_v2_single_return_value_type(standard, function)? else {
                    continue;
                };
                require(
                    *resolved_type == ResolvedType::value(value_type),
                    format!(
                        "raw V2 SINGLE return for {} did not recover exact value type {}",
                        function.id().canonical(),
                        value_type.canonical()
                    ),
                )?;
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let Some(value_type) = raw_v2_value_type_by_slot(standard, column.name())?
                    else {
                        continue;
                    };
                    require(
                        column.resolved_type() == ResolvedType::value(value_type),
                        format!(
                            "raw V2 ROWS column {} did not recover exact value type {}",
                            column.ordinal(),
                            value_type.canonical()
                        ),
                    )?;
                }
            }
            FunctionReturn::Stream(_) => {}
        }
    }
    Ok(())
}

async fn upgrade_raw_v2_value_rows(
    client: &tokio_postgres::Client,
    catalogue: &CatalogueSnapshot,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    for object in catalogue.object_types() {
        for field in object.fields() {
            let Some(value_type) = raw_v2_value_type_by_slot(standard, field.name())? else {
                continue;
            };
            client
                .execute(
                    "UPDATE _orna_kernel.catalogue_fields
                     SET type_kind = 'value', scalar_type = NULL, target_type_id = NULL,
                         value_type_id = $1, value_standard_library_revision_id = $2
                     WHERE catalogue_revision_id = $3
                       AND owner_type_id = $4 AND field_id = $5",
                    &[
                        &value_type.to_bytes().to_vec(),
                        &standard.revision().to_bytes().to_vec(),
                        &catalogue.revision().to_bytes().to_vec(),
                        &object.id().to_bytes().to_vec(),
                        &field.id().to_bytes().to_vec(),
                    ],
                )
                .await?;
        }
    }
    for function in catalogue.functions() {
        for parameter in function.parameters() {
            let Some(value_type) = raw_v2_value_type_by_slot(standard, parameter.name())? else {
                continue;
            };
            client
                .execute(
                    "UPDATE _orna_kernel.catalogue_function_parameters
                     SET type_kind = 'value', scalar_type = NULL, target_type_id = NULL,
                         value_type_id = $1, value_standard_library_revision_id = $2
                     WHERE catalogue_revision_id = $3
                       AND function_id = $4 AND parameter_id = $5",
                    &[
                        &value_type.to_bytes().to_vec(),
                        &standard.revision().to_bytes().to_vec(),
                        &catalogue.revision().to_bytes().to_vec(),
                        &function.id().to_bytes().to_vec(),
                        &parameter.id().to_bytes().to_vec(),
                    ],
                )
                .await?;
        }
        match function.return_type() {
            FunctionReturn::Single(_) => {
                let Some(value_type) = raw_v2_single_return_value_type(standard, function)? else {
                    continue;
                };
                client
                    .execute(
                        "UPDATE _orna_kernel.catalogue_functions
                         SET return_type_kind = 'value', return_scalar_type = NULL,
                             return_target_type_id = NULL, return_value_type_id = $1,
                             return_standard_library_revision_id = $2
                         WHERE catalogue_revision_id = $3 AND function_id = $4",
                        &[
                            &value_type.to_bytes().to_vec(),
                            &standard.revision().to_bytes().to_vec(),
                            &catalogue.revision().to_bytes().to_vec(),
                            &function.id().to_bytes().to_vec(),
                        ],
                    )
                    .await?;
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let Some(value_type) = raw_v2_value_type_by_slot(standard, column.name())?
                    else {
                        continue;
                    };
                    client
                        .execute(
                            "UPDATE _orna_kernel.catalogue_function_return_columns
                             SET type_kind = 'value', scalar_type = NULL, target_type_id = NULL,
                                 value_type_id = $1, value_standard_library_revision_id = $2
                             WHERE catalogue_revision_id = $3
                               AND function_id = $4 AND ordinal = $5",
                            &[
                                &value_type.to_bytes().to_vec(),
                                &standard.revision().to_bytes().to_vec(),
                                &catalogue.revision().to_bytes().to_vec(),
                                &function.id().to_bytes().to_vec(),
                                &i64::from(column.ordinal()),
                            ],
                        )
                        .await?;
                }
            }
            FunctionReturn::Stream(_) => {}
        }
    }
    Ok(())
}

async fn install_function_revision(database: &TestDatabase) -> TestResult<FunctionFixture> {
    let object = install_object_revision(database, false).await?;
    let session = database.open().await?;
    let operation_result: TestResult<FunctionFixture> = async {
        let catalogue_id = object.catalogue.revision();
        let schema = object.catalogue.schemas()[0].clone();
        let left = object.catalogue.object_types()[0].id();
        let right = object.catalogue.object_types()[1].id();
        let source = SourceOrigin::new(
            object.unit.id(),
            0,
            u32::try_from(object.unit.content().len())?,
        )?;
        let server_id = FunctionId::from_bytes([0xd1; 16]);
        let client_id = FunctionId::from_bytes([0xd2; 16]);
        let volatile_id = FunctionId::from_bytes([0xd3; 16]);
        let server_revision = FunctionRevisionId::from_bytes([0xe1; 16]);
        let client_revision = FunctionRevisionId::from_bytes([0xe2; 16]);
        let volatile_revision = FunctionRevisionId::from_bytes([0xe3; 16]);
        let expression = object.expression.clone();
        let scalar_types = [
            StandardScalar::Boolean,
            StandardScalar::Integer,
            StandardScalar::BigInt,
            StandardScalar::Float,
            StandardScalar::Decimal,
            StandardScalar::CharacterLargeObject,
            StandardScalar::BinaryLargeObject,
            StandardScalar::Uuid,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
        ];
        let mut server_parameters = scalar_types
            .into_iter()
            .enumerate()
            .map(|(ordinal, scalar)| {
                ParameterDefinition::new(
                    ParameterId::from_bytes(
                        [0xb0 + u8::try_from(ordinal).expect("twelve parameters"); 16],
                    ),
                    format!("scalar_{ordinal}"),
                    u32::try_from(ordinal).expect("twelve parameters"),
                    ResolvedType::scalar(scalar),
                    (ordinal == 0).then_some(expression.id()),
                )
            })
            .collect::<Vec<_>>();
        server_parameters.push(ParameterDefinition::new(
            ParameterId::from_bytes([0xbc; 16]),
            "named",
            12,
            ResolvedType::named(left),
            None,
        ));
        server_parameters.push(ParameterDefinition::new(
            ParameterId::from_bytes([0xbd; 16]),
            "reference",
            13,
            ResolvedType::reference(right),
            None,
        ));
        let server = FunctionDefinition::new(
            server_id,
            QualifiedSemanticName::new(["café", "server_rows"])?,
            FunctionDomain::Server,
            server_parameters,
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new(
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
                FunctionReturnColumnDefinition::new("owner", 1, ResolvedType::named(left)),
                FunctionReturnColumnDefinition::new("related", 2, ResolvedType::reference(right)),
            ]),
            server_revision,
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let client = FunctionDefinition::new(
            client_id,
            QualifiedSemanticName::new(["café", "client_single"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes([0xb0; 16]),
                "duplicate_owner_qualified_parameter",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            )],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            client_revision,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let volatile = FunctionDefinition::new(
            volatile_id,
            QualifiedSemanticName::new(["café", "volatile_single"])?,
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::reference(left)),
            volatile_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let functions = vec![server.clone(), client.clone(), volatile.clone()];
        let server_artifact = executable_artifact(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            b"ORNASP\0\0\0\0\0\x01fixture".to_vec(),
        )?;
        let client_artifact = executable_artifact(
            ExecutableArtifactKind::Client,
            "orna.client-bytecode",
            b"ORNACB\0\0\0\0\0\x01fixture".to_vec(),
        )?;
        let volatile_artifact = executable_artifact(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            b"ORNASP\0\0\0\0\0\x01volatile".to_vec(),
        )?;
        let references = vec![
            DefinitionReference::new(
                server_id,
                server_revision,
                0,
                DefinitionReferenceTarget::ObjectType(left),
                DefinitionReferenceKind::QueryObject,
                source,
            ),
            DefinitionReference::new(
                server_id,
                server_revision,
                1,
                DefinitionReferenceTarget::Field {
                    owner: left,
                    field: object.catalogue.object_types()[0].fields()[1].id(),
                },
                DefinitionReferenceKind::QueryField,
                source,
            ),
            DefinitionReference::new(
                server_id,
                server_revision,
                2,
                DefinitionReferenceTarget::Parameter {
                    owner: server_id,
                    parameter: server.parameters()[0].id(),
                },
                DefinitionReferenceKind::ParameterRead,
                source,
            ),
            DefinitionReference::new(
                server_id,
                server_revision,
                3,
                DefinitionReferenceTarget::Function(client_id),
                DefinitionReferenceKind::FunctionCall,
                source,
            ),
            DefinitionReference::new(
                server_id,
                server_revision,
                4,
                DefinitionReferenceTarget::Expression(expression.id()),
                DefinitionReferenceKind::Expression,
                source,
            ),
        ];
        let language = "orna-1";
        let declaration_hash = function_declaration_digest(object.unit.content().as_bytes())?;
        let revisions = vec![
            function_revision(
                &server,
                language,
                server_artifact,
                &object,
                &references,
                declaration_hash,
                source,
            )?,
            function_revision(
                &client,
                language,
                client_artifact,
                &object,
                &references,
                declaration_hash,
                source,
            )?,
            function_revision(
                &volatile,
                language,
                volatile_artifact,
                &object,
                &references,
                declaration_hash,
                source,
            )?,
        ];
        let catalogue = CatalogueSnapshot::new_with_functions(
            catalogue_id,
            object.catalogue.schemas().to_vec(),
            object.catalogue.object_types().to_vec(),
            functions.clone(),
        )?;
        let mut origins = object.origins.clone();
        for function in &functions {
            origins.extend(function.parameters().iter().map(|parameter| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Parameter {
                        owner: function.id(),
                        parameter: parameter.id(),
                    },
                    source,
                )
            }));
            if let FunctionReturn::Rows(columns) = function.return_type() {
                origins.extend(columns.iter().map(|column| {
                    DefinitionOrigin::new(
                        DefinitionIdentity::FunctionReturnColumn {
                            owner: function.id(),
                            ordinal: column.ordinal(),
                        },
                        source,
                    )
                }));
            }
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::Function(function.id()),
                source,
            ));
        }
        let catalogue_hash = catalogue_digest(
            &catalogue,
            &revisions,
            std::slice::from_ref(&expression),
            &origins,
            &references,
        )?;

        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        for function in &functions {
            insert_function_record(
                session.client(),
                catalogue_id,
                schema.id(),
                function,
                source,
            )
            .await?;
            for parameter in function.parameters() {
                insert_parameter_record(
                    session.client(),
                    catalogue_id,
                    function.id(),
                    parameter,
                    source,
                )
                .await?;
            }
            if let FunctionReturn::Rows(columns) = function.return_type() {
                for column in columns {
                    insert_return_record(
                        session.client(),
                        catalogue_id,
                        function.id(),
                        column,
                        source,
                    )
                    .await?;
                }
            }
        }
        for revision in &revisions {
            insert_revision_record(session.client(), catalogue_id, revision).await?;
        }
        for reference in &references {
            insert_reference_record(session.client(), catalogue_id, reference).await?;
        }
        for function in &functions {
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.invocation_target_authorities
                        (catalogue_revision_id, function_id, target_class,
                         function_revision_id, standard_library_revision_id)
                     VALUES ($1, $2, 'application', $3, NULL)",
                    &[
                        &catalogue_id.to_bytes().to_vec(),
                        &function.id().to_bytes().to_vec(),
                        &function.current_revision().to_bytes().to_vec(),
                    ],
                )
                .await?;
        }
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions SET content_hash = $2 WHERE id = $1",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(FunctionFixture {
            unit: object.unit.clone(),
            catalogue,
            expression,
            revisions,
            references,
            origins,
        })
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "function fixture",
    )
}

async fn install_raw_v2_standard_revision(database: &TestDatabase) -> TestResult<RawV2Fixture> {
    let mut application = install_function_revision(database).await?;
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()?,
    )?;
    insert_standard_snapshot(database, &standard).await?;
    let legacy_catalogue = application.catalogue.clone();
    application.catalogue = raw_v2_value_catalogue(&standard, &legacy_catalogue)?;
    let standard_boolean = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|value_type| value_type.representation_contract() == "orna.kernel.value.boolean@1")
        .ok_or_else(|| failure("retained standard fixture has no Boolean value type"))?;
    let source_origin = SourceOrigin::new(
        application.unit.id(),
        0,
        u32::try_from(application.unit.content().len())?,
    )?;
    let next_ordinal = application
        .references
        .iter()
        .filter(|reference| reference.source_function() == application.revisions[0].function())
        .map(DefinitionReference::ordinal)
        .max()
        .map_or(0, |ordinal| ordinal + 1);
    let standard_reference = DefinitionReference::new(
        application.revisions[0].function(),
        application.revisions[0].id(),
        next_ordinal,
        DefinitionReferenceTarget::ValueType(standard_boolean.id()),
        DefinitionReferenceKind::NamedType,
        source_origin,
    );
    application.references.push(standard_reference.clone());

    let mut revisions = Vec::with_capacity(application.revisions.len());
    for revision in &application.revisions {
        let function = application
            .catalogue
            .function_by_id(revision.function())
            .ok_or_else(|| failure("standard fixture function revision has no function"))?;
        let references = application
            .references
            .iter()
            .filter(|reference| reference.source_function() == revision.function())
            .cloned()
            .collect::<Vec<_>>();
        let semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            function,
            revision.language_version(),
            revision.artifact(),
            std::slice::from_ref(&application.expression),
            &references,
        )?;
        revisions.push(
            FunctionRevisionRecord::new(
                revision.function(),
                revision.id(),
                revision.revision_number(),
                revision.declaration_origin(),
                revision.declaration_content_hash(),
                semantic_hash,
                revision.language_version(),
                revision.artifact().clone(),
            )?
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        );
    }

    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &application.catalogue,
        &revisions,
        std::slice::from_ref(&application.expression),
        &application.origins,
        &application.references,
    )?;
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET canonical_hash_version = 2,
                     standard_library_revision_id = $2
                 WHERE id = $1",
                &[
                    &application.catalogue.revision().to_bytes().to_vec(),
                    &standard.revision().to_bytes().to_vec(),
                ],
            )
            .await?;
        insert_reference_record_with_standard(
            session.client(),
            application.catalogue.revision(),
            &standard_reference,
            standard.revision(),
        )
        .await?;
        upgrade_raw_v2_value_rows(session.client(), &legacy_catalogue, &standard).await?;
        for revision in &revisions {
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET semantic_ir_hash = $2, semantic_hash_version = 2
                     WHERE id = $1",
                    &[
                        &revision.id().to_bytes().to_vec(),
                        &revision.semantic_hash().to_bytes().to_vec(),
                    ],
                )
                .await?;
        }
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = $2
                 WHERE id = $1",
                &[
                    &application.catalogue.revision().to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "version-2 application fixture",
    )?;

    Ok(RawV2Fixture {
        standard,
        application,
        revisions,
    })
}

async fn insert_standard_snapshot(
    database: &TestDatabase,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        let source = standard.source();
        let unit = source
            .units()
            .first()
            .ok_or_else(|| failure("standard source fixture has no source unit"))?;
        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                 VALUES ($1, $2)",
                &[
                    &source.bundle().to_bytes().to_vec(),
                    &source.bundle_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &unit.id().to_bytes().to_vec(),
                    &source.bundle().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &unit.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        let no_parent: Option<Vec<u8>> = None;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &source.id().to_bytes().to_vec(),
                    &no_parent,
                    &source.bundle().to_bytes().to_vec(),
                    &source.revision_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, $4, $5, 'sha256')",
                &[
                    &standard.revision().to_bytes().to_vec(),
                    &source.id().to_bytes().to_vec(),
                    &standard.catalogue().revision().to_bytes().to_vec(),
                    &standard.language_version(),
                    &standard.digest().to_bytes().to_vec(),
                ],
            )
            .await?;

        for schema in standard.catalogue().schemas() {
            let origin = standard_origin(standard, DefinitionIdentity::Schema(schema.id()))?;
            insert_standard_schema(session.client(), standard.revision(), schema, origin).await?;
        }
        for value_type in standard.catalogue().value_types() {
            let origin = standard_origin(standard, DefinitionIdentity::ValueType(value_type.id()))?;
            insert_standard_value_type(
                session.client(),
                standard.revision(),
                standard.catalogue(),
                value_type,
                origin,
            )
            .await?;
        }
        for enum_type in standard.catalogue().enum_types() {
            let origin = standard_origin(standard, DefinitionIdentity::ValueType(enum_type.id()))?;
            insert_standard_enum_type(
                session.client(),
                standard.revision(),
                standard.catalogue(),
                enum_type,
                origin,
            )
            .await?;
        }
        for binding in standard.catalogue().type_bindings() {
            let origin = standard_origin(standard, DefinitionIdentity::TypeBinding(binding.id()))?;
            insert_standard_binding(
                session.client(),
                standard.revision(),
                standard.catalogue(),
                binding,
                origin,
            )
            .await?;
        }
        session.client().batch_execute("COMMIT").await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "standard snapshot fixture",
    )
}

fn standard_origin(
    standard: &VerifiedStandardLibrarySnapshot,
    identity: DefinitionIdentity,
) -> TestResult<SourceOrigin> {
    standard
        .origins()
        .iter()
        .find(|origin| origin.identity() == identity)
        .map(DefinitionOrigin::source)
        .ok_or_else(|| {
            failure(format!(
                "standard fixture origin is missing for {identity:?}"
            ))
        })
}

// The version-2 standard source constants below are the exact retained
// `std/types.orna` and `std/invoke.orna` shapes from the compiler reconcile
// fixtures. The fixture uses the same fixed identities, source, catalogue,
// executable, and origins, so its canonical digest is the compiled
// STANDARD_V2_CANONICAL_DIGEST golden.
#[cfg(feature = "test-hooks")]
const STD_INVOKE_SOURCE: &str = "CREATE SCHEMA std.invoke;\n\
    CREATE SERVER FUNCTION std.invoke.echo(\n\
    \x20   p_value INTEGER\n\
    )\n\
    RETURNS INTEGER\n\
    SECURITY INVOKER\n\
    TRANSACTION READ ONLY\n\
    VOLATILITY STABLE\n\
    AS\n\
    \x20   SELECT p_value;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_TYPES_SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;\
    CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT \
    'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;\
    EXPORT TYPE std.types.INTEGER AS std.INTEGER;\
    EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";

#[cfg(feature = "test-hooks")]
const STANDARD_V2_CANONICAL_DIGEST: [u8; 32] = [
    115, 202, 159, 209, 255, 174, 218, 69, 195, 114, 168, 108, 210, 7, 50, 127, 176, 149, 134, 145,
    229, 113, 139, 179, 237, 228, 75, 75, 94, 20, 52, 52,
];

/// The complete V2 standard fixture with the exact compiler-reconcile
/// identities and the compiled canonical digest golden. Its source revision
/// parent must exist as a durable source revision before the upgrade applies.
#[cfg(feature = "test-hooks")]
fn verified_standard_v2_fixture() -> TestResult<VerifiedStandardLibrarySnapshot> {
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
        source_unit_content_digest(STANDARD_V2_TYPES_SOURCE)?,
    )?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        STD_INVOKE_SOURCE,
        source_unit_content_digest(STD_INVOKE_SOURCE)?,
    )?;
    let units = vec![types_unit, invoke_unit];
    let bundle = SourceBundleId::from_bytes([0x41; 16]);
    let bundle_hash = source_bundle_digest(&units)?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x42; 16]),
        Some(SourceRevisionId::from_bytes([0x43; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            bundle,
            Some(SourceRevisionId::from_bytes([0x43; 16])),
            bundle_hash,
        )?,
    )?;

    let integer = ValueTypeDefinition::primitive(
        STD_INTEGER_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "integer"])?,
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"])?,
        integer.id(),
    )?;
    let prelude = TypeBinding::prelude(PreludeTypeName::new(["integer"])?, integer.id())?;
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "invoke", "echo"])?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_INVOKE_ECHO_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"])?,
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"])?,
            ),
            SchemaDefinition::new(
                STD_INVOKE_SCHEMA_ID,
                QualifiedSemanticName::new(["std", "invoke"])?,
            ),
        ],
        vec![],
        vec![integer],
        vec![qualified, prelude],
        vec![echo],
    )?;

    let origins = standard_v2_origins(&catalogue, STD_INVOKE_SOURCE)?;
    let executable = standard_v2_executable(&catalogue, &origins)?;

    let provisional = StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes([0x44; 16]),
        StandardLibraryDigestVersion::Version2,
        source,
        "orna.language/1",
        catalogue,
        vec![executable],
        origins,
        Sha256Digest::from_bytes(STANDARD_V2_CANONICAL_DIGEST),
    )?;
    Ok(verify_standard_library_v2_snapshot(provisional)?)
}

/// Builds the exact origin sequence for both retained V2 source units. The
/// byte ranges match the parsed declaration spans of the compiler fixture.
#[cfg(feature = "test-hooks")]
fn standard_v2_origins(
    catalogue: &CatalogueSnapshot,
    invoke_source: &str,
) -> TestResult<Vec<DefinitionOrigin>> {
    let mut origins = Vec::new();
    let types = STANDARD_V2_TYPES_SOURCE;
    let schema_std_end = "CREATE SCHEMA std;".len();
    let schema_types_end = schema_std_end + "CREATE SCHEMA std.types;".len();
    let type_declaration = "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;";
    let type_start = types
        .find("CREATE TYPE")
        .ok_or_else(|| failure("missing type"))?;
    let type_end = type_start + type_declaration.len();
    let qualified_declaration = "EXPORT TYPE std.types.INTEGER AS std.INTEGER;";
    let qualified_start = types
        .find("EXPORT TYPE std.types.INTEGER AS std.INTEGER")
        .ok_or_else(|| failure("missing qualified binding"))?;
    let qualified_end = qualified_start + qualified_declaration.len();
    let prelude_declaration = "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
    let prelude_start = types
        .find("EXPORT TYPE std.INTEGER TO PRELUDE")
        .ok_or_else(|| failure("missing prelude binding"))?;
    let prelude_end = prelude_start + prelude_declaration.len();
    let types_unit = STD_TYPES_SOURCE_UNIT_ID;
    let qualified_binding = catalogue
        .type_bindings()
        .first()
        .ok_or_else(|| failure("missing qualified binding"))?;
    let prelude_binding = catalogue
        .type_bindings()
        .last()
        .ok_or_else(|| failure("missing prelude binding"))?;
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        SourceOrigin::new(types_unit, 0, u32::try_from(schema_std_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
        SourceOrigin::new(
            types_unit,
            u32::try_from(schema_std_end)?,
            u32::try_from(schema_types_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(STD_INTEGER_TYPE_ID),
        SourceOrigin::new(
            types_unit,
            u32::try_from(type_start)?,
            u32::try_from(type_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(qualified_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(qualified_start)?,
            u32::try_from(qualified_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::TypeBinding(prelude_binding.id()),
        SourceOrigin::new(
            types_unit,
            u32::try_from(prelude_start)?,
            u32::try_from(prelude_end)?,
        )?,
    ));

    let function_start = invoke_source
        .find("CREATE SERVER FUNCTION")
        .ok_or_else(|| failure("missing function declaration"))?;
    let function_end = invoke_source.len();
    let parameter_start = invoke_source
        .find("p_value")
        .ok_or_else(|| failure("missing parameter declaration"))?;
    let parameter_end = parameter_start + "p_value INTEGER".len();
    let schema_end = "CREATE SCHEMA std.invoke;".len();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, u32::try_from(schema_end)?)?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(function_start)?,
            u32::try_from(function_end)?,
        )?,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        },
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(parameter_start)?,
            u32::try_from(parameter_end)?,
        )?,
    ));
    Ok(origins)
}

/// Builds the exact V2 executable: the immutable echo revision, the 44-byte
/// server parameter-echo artifact, and the three ordered references.
#[cfg(feature = "test-hooks")]
fn standard_v2_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> TestResult<StandardExecutable> {
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or_else(|| failure("missing echo function"))?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or_else(|| failure("missing echo function origin"))?
        .source();
    let declaration_content_hash = function_declaration_digest(
        &STD_INVOKE_SOURCE.as_bytes()
            [function_origin.byte_start() as usize..function_origin.byte_end() as usize],
    )?;
    let payload =
        ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, STD_INTEGER_TYPE_ID)?.encode()?;
    let content_hash = artifact_payload_digest(&payload)?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-parameter-echo",
        1,
        payload,
        content_hash,
    )?;
    let parameter_integer_start = STD_INVOKE_SOURCE
        .find("INTEGER")
        .ok_or_else(|| failure("missing parameter type"))? as u32;
    let result_integer_start = STD_INVOKE_SOURCE
        .rfind("INTEGER")
        .ok_or_else(|| failure("missing result type"))? as u32;
    let body_p_value_start = STD_INVOKE_SOURCE
        .rfind("p_value")
        .ok_or_else(|| failure("missing body identifier"))? as u32;
    let integer_origin = |start: u32| -> TestResult<SourceOrigin> {
        Ok(SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            start,
            start + 7,
        )?)
    };
    let references = vec![
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            0,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(parameter_integer_start)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            integer_origin(result_integer_start)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            integer_origin(body_p_value_start)?,
        ),
    ];
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        "orna.language/1",
        &artifact,
        &[],
        &references,
    )?;
    let revision = FunctionRevisionRecord::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic,
        "orna.language/1",
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    Ok(StandardExecutable::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        revision,
        references,
    )?)
}

/// Installs the durable source-revision parent of the fixture V2 standard
/// source before the upgrade applies.
#[cfg(feature = "test-hooks")]
async fn install_standard_v2_parent_revision(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let bundle = vec![0x99_u8; 16];
    let parent = vec![0x43_u8; 16];
    let content_hash = vec![0x98_u8; 32];
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', 1)",
            &[&bundle, &content_hash],
        )
        .await?;
    session
        .client()
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, NULL, $2, $3, 'sha256', 1)",
            &[&parent, &bundle, &content_hash],
        )
        .await?;
    session.shutdown().await
}

/// The companion application revision for the V2 standard upgrade: one
/// application CLIENT function under the pinned version-two context. Its
/// identity is distinct from every standard and system function, so the
/// upgrade scan admits it without a collision. The single upgrade installs
/// the application and standard authority rows under one catalogue revision.
#[cfg(feature = "test-hooks")]
fn v2_standard_and_application_candidate(
    active: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let content = "CREATE SCHEMA app;\n";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xa1; 16]),
        0,
        "main.orna",
        content,
        source_unit_content_digest(content)?,
    )?;
    let bundle = SourceBundleId::from_bytes([0xa2; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0xa3; 16]),
        Some(active.pair().source()),
        vec![unit.clone()],
        bundle_hash,
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)?,
    )?;
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes([0xa4; 16]),
        QualifiedSemanticName::new(["app"])?,
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0xa5; 16]),
        QualifiedSemanticName::new(["app", "answer"])?,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::value(STD_INTEGER_TYPE_ID)),
        FunctionRevisionId::from_bytes([0xa6; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0xa7; 16]),
        vec![schema.clone()],
        vec![],
        vec![function.clone()],
    )?;
    let origin = SourceOrigin::new(unit.id(), 0, u32::try_from(content.len())?)?;
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), origin),
        DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), origin),
    ];
    let artifact = executable_artifact(
        ExecutableArtifactKind::Client,
        "orna.client-bytecode",
        b"ORNACB\0\0\0\0\0\x01answer".to_vec(),
    )?;
    let declaration_hash = function_declaration_digest(content.as_bytes())?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        "orna.language/1",
        &artifact,
        &[],
        &[],
    )?;
    let revision = FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        origin,
        declaration_hash,
        semantic_hash,
        "orna.language/1",
        artifact,
    )?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        &[],
        &origins,
        &[],
    )?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(origins, vec![], vec![revision.clone()], vec![])
                .with_current_function_revisions(vec![revision]),
        ),
        context,
    )?)
}

/// The complete live fixture: the executable V2 standard and one application
/// CLIENT function installed atomically through the production apply path.
/// The active catalogue therefore owns one application target and one standard
/// target under one catalogue revision.
#[cfg(feature = "test-hooks")]
struct V2Fixture {
    standard: VerifiedStandardLibrarySnapshot,
    active: ActiveDatabaseRevision,
    app_function: FunctionId,
}

#[cfg(feature = "test-hooks")]
async fn install_v2_standard_fixture(database: &TestDatabase) -> TestResult<V2Fixture> {
    let kernel = kernel(database)?;
    kernel.bootstrap().await?;
    install_standard_v2_parent_revision(database).await?;
    let active = kernel.recover().await?;
    let standard = verified_standard_v2_fixture()?;
    let candidate = v2_standard_and_application_candidate(&active, &standard)?;
    let applied = kernel
        .apply_test_standard_upgrade(&candidate, &standard)
        .await?;
    let app_function = applied.catalogue().functions()[0].id();
    require(
        applied
            .catalogue_hash_context()
            .standard()
            .is_some_and(|selected| selected.revision() == standard.revision()),
        "fixture active revision must pin the executable standard snapshot",
    )?;
    Ok(V2Fixture {
        standard,
        active: applied,
        app_function,
    })
}

/// Re-pins the active catalogue revision to the retained version-one standard
/// snapshot and rewrites its target-authority rows without the standard
/// executable, simulating a later standard upgrade that removed the granted
/// function. The application catalogue content is unchanged, so the re-pin is
/// a valid version-two catalogue whose union has no standard target.
#[cfg(feature = "test-hooks")]
async fn install_later_standard_upgrade_without_echo(
    database: &TestDatabase,
    fixture: &V2Fixture,
) -> TestResult<()> {
    let retained = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()?,
    )?;
    insert_standard_snapshot(database, &retained).await?;
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        let active = fixture.active.clone();
        let catalogue_bytes = active.pair().catalogue().to_bytes().to_vec();
        let context = CatalogueHashContext::version_two(retained.clone());
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            active.catalogue(),
            active.function_revisions(),
            active.expressions(),
            active.origins(),
            active.references(),
        )?;
        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        // Remove the standard authority row and re-pin the carried application
        // function's resolved standard value type before re-pinning the
        // catalogue revision so the non-deferrable foreign keys stay valid.
        session
            .client()
            .execute(
                "DELETE FROM _orna_kernel.invocation_target_authorities
                 WHERE catalogue_revision_id = $1 AND function_id = $2",
                &[
                    &catalogue_bytes,
                    &STD_INVOKE_ECHO_FUNCTION_ID.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_functions
                 SET return_standard_library_revision_id = $2
                 WHERE catalogue_revision_id = $1 AND function_id = $3",
                &[
                    &catalogue_bytes,
                    &retained.revision().to_bytes().to_vec(),
                    &fixture.app_function.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET standard_library_revision_id = $2, content_hash = $3
                 WHERE id = $1",
                &[
                    &catalogue_bytes,
                    &retained.revision().to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "later standard upgrade fixture",
    )
}

async fn insert_standard_schema(
    client: &tokio_postgres::Client,
    revision: orna_core::StandardLibraryRevisionId,
    schema: &SchemaDefinition,
    origin: SourceOrigin,
) -> TestResult<()> {
    client
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_schemas
                (standard_library_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &revision.to_bytes().to_vec(),
                &schema.id().to_bytes().to_vec(),
                &schema.name().parts(),
                &origin.source_unit().to_bytes().to_vec(),
                &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_standard_value_type(
    client: &tokio_postgres::Client,
    revision: orna_core::StandardLibraryRevisionId,
    catalogue: &CatalogueSnapshot,
    value_type: &orna_core::catalogue::ValueTypeDefinition,
    origin: SourceOrigin,
) -> TestResult<()> {
    let schema = catalogue
        .schemas()
        .iter()
        .filter(|schema| value_type.name().parts().starts_with(schema.name().parts()))
        .max_by_key(|schema| schema.name().parts().len())
        .ok_or_else(|| failure("standard value type has no owning schema"))?;
    let value_kind = match value_type.kind() {
        orna_core::catalogue::ValueTypeKind::Primitive => "primitive",
        orna_core::catalogue::ValueTypeKind::Opaque => "opaque",
        _ => return Err(failure("standard fixture has an unsupported value kind")),
    };
    let mutability = match value_type.mutability() {
        ValueTypeMutability::Immutable => "immutable",
        _ => return Err(failure("standard fixture has an unsupported mutability")),
    };
    let persistence = match value_type.persistence() {
        ValueTypePersistence::Persistable => "persistable",
        ValueTypePersistence::Transient => "transient",
        _ => return Err(failure("standard fixture has an unsupported persistence")),
    };
    client
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_value_types
                (standard_library_revision_id, type_id, schema_id, name_parts,
                 value_kind, mutability, persistence, representation_contract,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            &[
                &revision.to_bytes().to_vec(),
                &value_type.id().to_bytes().to_vec(),
                &schema.id().to_bytes().to_vec(),
                &value_type.name().parts(),
                &value_kind,
                &mutability,
                &persistence,
                &value_type.representation_contract(),
                &origin.source_unit().to_bytes().to_vec(),
                &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_standard_binding(
    client: &tokio_postgres::Client,
    revision: orna_core::StandardLibraryRevisionId,
    catalogue: &CatalogueSnapshot,
    binding: &orna_core::catalogue::TypeBinding,
    origin: SourceOrigin,
) -> TestResult<()> {
    let (kind, name_parts) = match binding.name() {
        TypeLookupName::Qualified(name) => ("qualified", name.parts()),
        TypeLookupName::Prelude(name) => ("prelude", name.words()),
        _ => {
            return Err(failure(
                "standard fixture has an unsupported binding namespace",
            ));
        }
    };
    let (target_kind, value_target, enum_target) =
        if catalogue.value_type_by_id(binding.target()).is_some() {
            ("value", Some(binding.target().to_bytes().to_vec()), None)
        } else if catalogue.enum_type_by_id(binding.target()).is_some() {
            ("enum", None, Some(binding.target().to_bytes().to_vec()))
        } else {
            return Err(failure(
                "standard fixture binding target is neither a value nor enum",
            ));
        };
    client
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_type_bindings
                (standard_library_revision_id, type_binding_id, kind, name_parts,
                 target_type_kind, target_type_id, target_enum_type_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &revision.to_bytes().to_vec(),
                &binding.id().to_bytes().to_vec(),
                &kind,
                &name_parts,
                &target_kind,
                &value_target,
                &enum_target,
                &origin.source_unit().to_bytes().to_vec(),
                &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_standard_enum_type(
    client: &tokio_postgres::Client,
    revision: orna_core::StandardLibraryRevisionId,
    catalogue: &CatalogueSnapshot,
    enum_type: &EnumTypeDefinition,
    origin: SourceOrigin,
) -> TestResult<()> {
    let schema = catalogue
        .schemas()
        .iter()
        .filter(|schema| enum_type.name().parts().starts_with(schema.name().parts()))
        .max_by_key(|schema| schema.name().parts().len())
        .ok_or_else(|| failure("standard enum has no owning schema"))?;
    client
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_enum_types
                (standard_library_revision_id, type_id, schema_id, name_parts, labels,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &revision.to_bytes().to_vec(),
                &enum_type.id().to_bytes().to_vec(),
                &schema.id().to_bytes().to_vec(),
                &enum_type.name().parts(),
                &enum_type.labels(),
                &origin.source_unit().to_bytes().to_vec(),
                &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

fn require_standard_snapshot(
    actual: &VerifiedStandardLibrarySnapshot,
    expected: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let mut actual_schemas = actual.catalogue().schemas().to_vec();
    let mut expected_schemas = expected.catalogue().schemas().to_vec();
    actual_schemas.sort_by_key(|schema| schema.id().to_bytes());
    expected_schemas.sort_by_key(|schema| schema.id().to_bytes());
    let mut actual_value_types = actual.catalogue().value_types().to_vec();
    let mut expected_value_types = expected.catalogue().value_types().to_vec();
    actual_value_types.sort_by_key(|value_type| value_type.id().to_bytes());
    expected_value_types.sort_by_key(|value_type| value_type.id().to_bytes());
    let mut actual_enum_types = actual.catalogue().enum_types().to_vec();
    let mut expected_enum_types = expected.catalogue().enum_types().to_vec();
    actual_enum_types.sort_by_key(|enum_type| enum_type.id().to_bytes());
    expected_enum_types.sort_by_key(|enum_type| enum_type.id().to_bytes());
    let mut actual_bindings = actual.catalogue().type_bindings().to_vec();
    let mut expected_bindings = expected.catalogue().type_bindings().to_vec();
    actual_bindings.sort_by_key(|binding| binding.id().to_bytes());
    expected_bindings.sort_by_key(|binding| binding.id().to_bytes());
    let mut actual_origins = actual
        .origins()
        .iter()
        .map(|origin| (format!("{:?}", origin.identity()), origin.source()))
        .collect::<Vec<_>>();
    let mut expected_origins = expected
        .origins()
        .iter()
        .map(|origin| (format!("{:?}", origin.identity()), origin.source()))
        .collect::<Vec<_>>();
    actual_origins.sort_by(|left, right| left.0.cmp(&right.0));
    expected_origins.sort_by(|left, right| left.0.cmp(&right.0));
    require(
        actual.revision() == expected.revision()
            && actual.digest_version() == expected.digest_version()
            && actual.language_version() == expected.language_version()
            && actual.digest() == expected.digest()
            && actual.source() == expected.source()
            && actual.catalogue().revision() == expected.catalogue().revision()
            && actual_schemas == expected_schemas
            && actual.catalogue().object_types().is_empty()
            && actual_value_types == expected_value_types
            && actual_enum_types == expected_enum_types
            && actual_bindings == expected_bindings
            && actual.catalogue().functions().is_empty()
            && actual_origins == expected_origins,
        "version-2 recovery changed standard source, catalogue, origins, or digest",
    )
}

struct FunctionHistoryFixture {
    unit: StoredSourceUnit,
    catalogue: CatalogueSnapshot,
    current_revisions: Vec<FunctionRevisionRecord>,
    retired_revision: FunctionRevisionRecord,
    references: Vec<DefinitionReference>,
    origins: Vec<DefinitionOrigin>,
}

async fn install_reused_function_catalogue(
    database: &TestDatabase,
    introduction: &FunctionFixture,
) -> TestResult<FunctionHistoryFixture> {
    let session = database.open().await?;
    let operation_result: TestResult<FunctionHistoryFixture> = async {
        let old_catalogue = introduction.catalogue.revision();
        let row = session
            .client()
            .query_one(
                "SELECT source_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&old_catalogue.to_bytes().to_vec()],
            )
            .await?;
        let old_source = SourceRevisionId::from_bytes(exact_identity(
            row.try_get("source_revision_id")?,
            "introducing source revision identity",
        )?);
        let bundle = SourceBundleId::from_bytes([0xf1; 16]);
        let source = SourceRevisionId::from_bytes([0xf2; 16]);
        let catalogue_id = CatalogueRevisionId::from_bytes([0xf3; 16]);
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xf4; 16]),
            0,
            "reused-functions.orna",
            format!(
                "{}\n// current declarations moved without recompiling\n",
                introduction.unit.content()
            ),
            source_unit_content_digest(&format!(
                "{}\n// current declarations moved without recompiling\n",
                introduction.unit.content()
            ))?,
        )?;
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
        let source_hash = source_revision_record_digest(bundle, Some(old_source), bundle_hash)?;
        let active_functions = introduction.catalogue.functions()[..2].to_vec();
        let retired_function = introduction.catalogue.functions()[2].id();
        let catalogue = CatalogueSnapshot::new_with_functions(
            catalogue_id,
            introduction.catalogue.schemas().to_vec(),
            introduction.catalogue.object_types().to_vec(),
            active_functions,
        )?;
        let current_source = SourceOrigin::new(unit.id(), 0, u32::try_from(unit.content().len())?)?;
        let origins = introduction
            .origins
            .iter()
            .filter(|origin| !definition_owned_by_function(origin.identity(), retired_function))
            .map(|origin| DefinitionOrigin::new(origin.identity(), current_source))
            .collect::<Vec<_>>();
        let references = introduction
            .references
            .iter()
            .map(|reference| {
                DefinitionReference::new(
                    reference.source_function(),
                    reference.source_revision(),
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    current_source,
                )
            })
            .collect::<Vec<_>>();
        let current_revisions = introduction.revisions[..2].to_vec();
        let retired_revision = introduction.revisions[2].clone();
        let catalogue_hash = catalogue_digest(
            &catalogue,
            &current_revisions,
            std::slice::from_ref(&introduction.expression),
            &origins,
            &references,
        )?;

        let full_start = 0_i64;
        let full_end = i64::try_from(unit.content().len())?;
        let old_catalogue_bytes = old_catalogue.to_bytes().to_vec();
        let new_catalogue_bytes = catalogue_id.to_bytes().to_vec();
        let unit_bytes = unit.id().to_bytes().to_vec();
        let retained_function_ids = catalogue
            .functions()
            .iter()
            .map(|function| function.id().to_bytes().to_vec())
            .collect::<Vec<_>>();

        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                 VALUES ($1, $2)",
                &[
                    &bundle.to_bytes().to_vec(),
                    &bundle_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &unit_bytes,
                    &bundle.to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &unit.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &source.to_bytes().to_vec(),
                    &old_source.to_bytes().to_vec(),
                    &bundle.to_bytes().to_vec(),
                    &source_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_revisions
                    (id, source_revision_id, parent_catalogue_revision_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &new_catalogue_bytes,
                    &source.to_bytes().to_vec(),
                    &old_catalogue_bytes,
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_schemas
                    (catalogue_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 SELECT $1, schema_id, name_parts, $2, $3, $4
                 FROM _orna_kernel.catalogue_schemas
                 WHERE catalogue_revision_id = $5",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_object_types
                    (catalogue_revision_id, type_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 SELECT $1, type_id, schema_id, name_parts, $2, $3, $4
                 FROM _orna_kernel.catalogue_object_types
                 WHERE catalogue_revision_id = $5",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                    (catalogue_revision_id, expression_id, format, format_version,
                     payload, content_hash, hash_algorithm, hash_contract_version,
                     source_unit_id, source_start, source_end)
                 SELECT $1, expression_id, format, format_version,
                        payload, content_hash, hash_algorithm, hash_contract_version,
                        $2, $3, $4
                 FROM _orna_kernel.catalogue_expressions
                 WHERE catalogue_revision_id = $5",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_fields
                    (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, nullable, is_unique,
                     default_expression_id, on_delete,
                     source_unit_id, source_start, source_end)
                 SELECT $1, owner_type_id, field_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, nullable, is_unique,
                        default_expression_id, on_delete, $2, $3, $4
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $5",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                    (catalogue_revision_id, function_id, schema_id, name_parts,
                     domain, security_mode, transaction_mode, volatility,
                     return_shape, return_type_kind, return_scalar_type,
                     return_target_type_id, current_function_revision_id,
                     source_unit_id, source_start, source_end)
                 SELECT $1, function_id, schema_id, name_parts,
                        domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind, return_scalar_type,
                        return_target_type_id, current_function_revision_id,
                        $2, $3, $4
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $5
                   AND function_id = ANY($6)",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                    &retained_function_ids,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_function_parameters
                    (catalogue_revision_id, function_id, parameter_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, default_expression_id,
                     source_unit_id, source_start, source_end)
                 SELECT $1, function_id, parameter_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, default_expression_id,
                        $2, $3, $4
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $5
                   AND function_id = ANY($6)",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                    &retained_function_ids,
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
                 SELECT $1, function_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, $2, $3, $4
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $5
                   AND function_id = ANY($6)",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                    &retained_function_ids,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.definition_references
                    (catalogue_revision_id, source_function_id,
                     source_function_revision_id, ordinal, target_definition_id,
                     target_kind, reference_kind, source_subobject_id,
                     target_owner_type_id, target_owner_function_id,
                     source_unit_id, source_start, source_end)
                 SELECT $1, source_function_id, source_function_revision_id,
                        ordinal, target_definition_id, target_kind, reference_kind,
                        source_subobject_id, target_owner_type_id,
                        target_owner_function_id, $2, $3, $4
                 FROM _orna_kernel.definition_references
                 WHERE catalogue_revision_id = $5
                   AND source_function_id = ANY($6)",
                &[
                    &new_catalogue_bytes,
                    &unit_bytes,
                    &full_start,
                    &full_end,
                    &old_catalogue_bytes,
                    &retained_function_ids,
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.function_revisions
                 SET status = 'retired'
                 WHERE id = $1",
                &[&retired_revision.id().to_bytes().to_vec()],
            )
            .await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.active_revision
                 SET source_revision_id = $1, catalogue_revision_id = $2",
                &[&source.to_bytes().to_vec(), &new_catalogue_bytes],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;

        Ok(FunctionHistoryFixture {
            unit,
            catalogue,
            current_revisions,
            retired_revision,
            references,
            origins,
        })
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "reused function history fixture",
    )
}

async fn install_valid_sibling_introduction(
    database: &TestDatabase,
    introduction: &FunctionFixture,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        let old_catalogue = introduction.catalogue.revision();
        let row = session
            .client()
            .query_one(
                "SELECT catalogue.source_revision_id,
                        catalogue.parent_catalogue_revision_id,
                        source.parent_source_revision_id
                 FROM _orna_kernel.catalogue_revisions AS catalogue
                 JOIN _orna_kernel.source_revisions AS source
                   ON source.id = catalogue.source_revision_id
                 WHERE catalogue.id = $1",
                &[&old_catalogue.to_bytes().to_vec()],
            )
            .await?;
        let source_parent = row
            .try_get::<_, Option<Vec<u8>>>("parent_source_revision_id")?
            .map(|value| exact_identity(value, "sibling source parent identity"))
            .transpose()?
            .map(SourceRevisionId::from_bytes);
        let catalogue_parent = row
            .try_get::<_, Option<Vec<u8>>>("parent_catalogue_revision_id")?
            .map(|value| exact_identity(value, "sibling catalogue parent identity"))
            .transpose()?
            .map(CatalogueRevisionId::from_bytes);
        let bundle = SourceBundleId::from_bytes([0xf6; 16]);
        let source = SourceRevisionId::from_bytes([0xf7; 16]);
        let catalogue_id = CatalogueRevisionId::from_bytes([0xf5; 16]);
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xf8; 16]),
            0,
            introduction.unit.logical_path(),
            introduction.unit.content(),
            source_unit_content_digest(introduction.unit.content())?,
        )?;
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
        let source_hash = source_revision_record_digest(bundle, source_parent, bundle_hash)?;
        let catalogue = CatalogueSnapshot::new_with_functions(
            catalogue_id,
            introduction.catalogue.schemas().to_vec(),
            introduction.catalogue.object_types().to_vec(),
            introduction.catalogue.functions().to_vec(),
        )?;
        let sibling_source = SourceOrigin::new(unit.id(), 0, u32::try_from(unit.content().len())?)?;
        let origins = introduction
            .origins
            .iter()
            .map(|origin| DefinitionOrigin::new(origin.identity(), sibling_source))
            .collect::<Vec<_>>();
        let references = introduction
            .references
            .iter()
            .map(|reference| {
                DefinitionReference::new(
                    reference.source_function(),
                    reference.source_revision(),
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    sibling_source,
                )
            })
            .collect::<Vec<_>>();
        let catalogue_hash = catalogue_digest(
            &catalogue,
            &introduction.revisions,
            std::slice::from_ref(&introduction.expression),
            &origins,
            &references,
        )?;
        let source_parent = source_parent.map(|parent| parent.to_bytes().to_vec());
        let catalogue_parent = catalogue_parent.map(|parent| parent.to_bytes().to_vec());

        session
            .client()
            .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                 VALUES ($1, $2)",
                &[
                    &bundle.to_bytes().to_vec(),
                    &bundle_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &unit.id().to_bytes().to_vec(),
                    &bundle.to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &unit.content_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &source.to_bytes().to_vec(),
                    &source_parent,
                    &bundle.to_bytes().to_vec(),
                    &source_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_revisions
                    (id, source_revision_id, parent_catalogue_revision_id, content_hash)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &source.to_bytes().to_vec(),
                    &catalogue_parent,
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        for schema in catalogue.schemas() {
            insert_schema_record(session.client(), catalogue_id, schema, sibling_source).await?;
        }
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                    (catalogue_revision_id, expression_id, format, format_version,
                     payload, content_hash, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &introduction.expression.id().to_bytes().to_vec(),
                    &introduction.expression.format(),
                    &i32::try_from(introduction.expression.version())?,
                    &introduction.expression.payload(),
                    &introduction.expression.content_hash().to_bytes().to_vec(),
                    &sibling_source.source_unit().to_bytes().to_vec(),
                    &i64::from(sibling_source.byte_start()),
                    &i64::from(sibling_source.byte_end()),
                ],
            )
            .await?;
        let schema = catalogue.schemas()[0].id();
        for object in catalogue.object_types() {
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_object_types
                        (catalogue_revision_id, type_id, schema_id, name_parts,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    &[
                        &catalogue_id.to_bytes().to_vec(),
                        &object.id().to_bytes().to_vec(),
                        &schema.to_bytes().to_vec(),
                        &object.name().parts(),
                        &sibling_source.source_unit().to_bytes().to_vec(),
                        &i64::from(sibling_source.byte_start()),
                        &i64::from(sibling_source.byte_end()),
                    ],
                )
                .await?;
            for field in object.fields() {
                insert_field_record(
                    session.client(),
                    catalogue_id,
                    object.id(),
                    field,
                    sibling_source,
                )
                .await?;
            }
        }
        for function in catalogue.functions() {
            insert_function_record(
                session.client(),
                catalogue_id,
                schema,
                function,
                sibling_source,
            )
            .await?;
            for parameter in function.parameters() {
                insert_parameter_record(
                    session.client(),
                    catalogue_id,
                    function.id(),
                    parameter,
                    sibling_source,
                )
                .await?;
            }
            if let FunctionReturn::Rows(columns) = function.return_type() {
                for column in columns {
                    insert_return_record(
                        session.client(),
                        catalogue_id,
                        function.id(),
                        column,
                        sibling_source,
                    )
                    .await?;
                }
            }
        }
        for reference in &references {
            insert_reference_record(session.client(), catalogue_id, reference).await?;
        }
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.function_revisions
                 SET introduced_catalogue_revision_id = $1
                 WHERE id = $2",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &FunctionRevisionId::from_bytes([0xe3; 16])
                        .to_bytes()
                        .to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "valid sibling introduction fixture",
    )
}

fn definition_owned_by_function(identity: DefinitionIdentity, function: FunctionId) -> bool {
    match identity {
        DefinitionIdentity::Function(owner)
        | DefinitionIdentity::Parameter { owner, .. }
        | DefinitionIdentity::FunctionReturnColumn { owner, .. } => owner == function,
        _ => false,
    }
}

fn executable_artifact(
    kind: ExecutableArtifactKind,
    format: &str,
    payload: Vec<u8>,
) -> TestResult<ExecutableArtifact> {
    let hash = artifact_payload_digest(&payload)?;
    Ok(ExecutableArtifact::new(kind, format, 1, payload, hash)?)
}

fn function_revision(
    function: &FunctionDefinition,
    language: &str,
    artifact: ExecutableArtifact,
    object: &ObjectFixture,
    references: &[DefinitionReference],
    declaration_hash: orna_core::revision::Sha256Digest,
    source: SourceOrigin,
) -> TestResult<FunctionRevisionRecord> {
    let function_references = references
        .iter()
        .filter(|reference| reference.source_function() == function.id())
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = function_semantic_digest(
        function,
        language,
        &artifact,
        std::slice::from_ref(&object.expression),
        &function_references,
    )?;
    Ok(FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        source,
        declaration_hash,
        semantic_hash,
        language,
        artifact,
    )?)
}

async fn insert_function_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    schema: SchemaId,
    function: &FunctionDefinition,
    source: SourceOrigin,
) -> TestResult<()> {
    let domain = match function.domain() {
        FunctionDomain::Server => "server",
        FunctionDomain::Client => "client",
    };
    let security = match function.security() {
        FunctionSecurity::Invoker => "invoker",
        FunctionSecurity::Definer => "definer",
    };
    let transaction = function.transaction().map(|transaction| match transaction {
        FunctionTransaction::Atomic => "atomic".to_owned(),
        FunctionTransaction::ReadOnly => "read_only".to_owned(),
        FunctionTransaction::Manual => "manual".to_owned(),
    });
    let volatility = match function.volatility() {
        FunctionVolatility::Immutable => "immutable",
        FunctionVolatility::Stable => "stable",
        FunctionVolatility::Volatile => "volatile",
    };
    let (return_shape, return_kind, return_scalar, return_target) = match function.return_type() {
        FunctionReturn::Single(resolved) => {
            let (kind, scalar, target) = resolved_type_columns(*resolved)?;
            ("single", Some(kind), scalar, target)
        }
        FunctionReturn::Rows(_) => ("rows", None, None, None),
        FunctionReturn::Stream(resolved) => {
            let (kind, scalar, target) = resolved_type_columns(*resolved)?;
            ("stream", Some(kind), scalar, target)
        }
    };
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts,
                 domain, security_mode, transaction_mode, volatility,
                 return_shape, return_type_kind, return_scalar_type,
                 return_target_type_id, current_function_revision_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                     $9, $10, $11, $12, $13, $14, $15, $16)",
            &[
                &catalogue.to_bytes().to_vec(),
                &function.id().to_bytes().to_vec(),
                &schema.to_bytes().to_vec(),
                &function.name().parts(),
                &domain,
                &security,
                &transaction,
                &volatility,
                &return_shape,
                &return_kind,
                &return_scalar,
                &return_target,
                &function.current_revision().to_bytes().to_vec(),
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_parameter_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    owner: FunctionId,
    parameter: &ParameterDefinition,
    source: SourceOrigin,
) -> TestResult<()> {
    let (kind, scalar, target) = resolved_type_columns(parameter.resolved_type())?;
    let default_expression = parameter
        .default_expression()
        .map(|expression| expression.to_bytes().to_vec());
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_function_parameters
                (catalogue_revision_id, function_id, parameter_id, name,
                 ordinal, type_kind, scalar_type, target_type_id,
                 default_expression_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &catalogue.to_bytes().to_vec(),
                &owner.to_bytes().to_vec(),
                &parameter.id().to_bytes().to_vec(),
                &parameter.name(),
                &i64::from(parameter.ordinal()),
                &kind,
                &scalar,
                &target,
                &default_expression,
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_return_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    owner: FunctionId,
    column: &FunctionReturnColumnDefinition,
    source: SourceOrigin,
) -> TestResult<()> {
    let (kind, scalar, target) = resolved_type_columns(column.resolved_type())?;
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_function_return_columns
                (catalogue_revision_id, function_id, name, ordinal,
                 type_kind, scalar_type, target_type_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &catalogue.to_bytes().to_vec(),
                &owner.to_bytes().to_vec(),
                &column.name(),
                &i64::from(column.ordinal()),
                &kind,
                &scalar,
                &target,
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_revision_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    revision: &FunctionRevisionRecord,
) -> TestResult<()> {
    client
        .execute(
            "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id,
                 revision_number, content_hash, semantic_ir_hash,
                 language_version, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'active')",
            &[
                &revision.id().to_bytes().to_vec(),
                &catalogue.to_bytes().to_vec(),
                &revision.function().to_bytes().to_vec(),
                &i64::try_from(revision.revision_number())?,
                &revision.declaration_content_hash().to_bytes().to_vec(),
                &revision.semantic_hash().to_bytes().to_vec(),
                &revision.language_version(),
            ],
        )
        .await?;
    let artifact = revision.artifact();
    let artifact_kind = match artifact.kind() {
        ExecutableArtifactKind::Server => "server_plan",
        ExecutableArtifactKind::Client => "client_bytecode",
    };
    client
        .execute(
            "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format,
                 format_version, payload, content_hash)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &revision.id().to_bytes().to_vec(),
                &artifact_kind,
                &artifact.format(),
                &i32::try_from(artifact.version())?,
                &artifact.payload(),
                &artifact.content_hash().to_bytes().to_vec(),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_reference_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    reference: &DefinitionReference,
) -> TestResult<()> {
    insert_reference_record_with_standard(client, catalogue, reference, None).await
}

async fn insert_reference_record_with_standard(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    reference: &DefinitionReference,
    standard_revision: impl Into<Option<orna_core::StandardLibraryRevisionId>>,
) -> TestResult<()> {
    let standard_revision = standard_revision.into();
    let (target, target_kind, owner_type, owner_function) = match reference.target() {
        DefinitionReferenceTarget::ObjectType(id) => {
            (id.to_bytes().to_vec(), "object_type", None, None)
        }
        DefinitionReferenceTarget::Field { owner, field } => (
            field.to_bytes().to_vec(),
            "field",
            Some(owner.to_bytes().to_vec()),
            None,
        ),
        DefinitionReferenceTarget::Function(id) => (id.to_bytes().to_vec(), "function", None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => (
            parameter.to_bytes().to_vec(),
            "parameter",
            None,
            Some(owner.to_bytes().to_vec()),
        ),
        DefinitionReferenceTarget::ValueType(id) => {
            (id.to_bytes().to_vec(), "value_type", None, None)
        }
        other => {
            let DefinitionReferenceTarget::Expression(id) = other else {
                return Err(failure(
                    "recovery fixture cannot persist this definition reference target",
                ));
            };
            (id.to_bytes().to_vec(), "expression", None, None)
        }
    };
    let target_standard = match reference.target() {
        DefinitionReferenceTarget::ValueType(_) => standard_revision
            .map(|revision| revision.to_bytes().to_vec())
            .ok_or_else(|| failure("ValueType reference requires a standard revision"))?,
        _ => {
            if standard_revision.is_some() {
                return Err(failure(
                    "non-ValueType reference cannot carry a standard revision",
                ));
            }
            Vec::new()
        }
    };
    let target_standard = if target_standard.is_empty() {
        None
    } else {
        Some(target_standard)
    };
    let kind = supported_reference_kind_sql(reference.kind())?;
    let source = reference.source_origin();
    client
        .execute(
            "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id,
                 source_function_revision_id, ordinal, target_definition_id,
                 target_standard_library_revision_id, target_kind, reference_kind,
                 source_subobject_id,
                 target_owner_type_id, target_owner_function_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13)",
            &[
                &catalogue.to_bytes().to_vec(),
                &reference.source_function().to_bytes().to_vec(),
                &reference.source_revision().to_bytes().to_vec(),
                &i64::from(reference.ordinal()),
                &target,
                &target_standard,
                &target_kind,
                &kind,
                &owner_type,
                &owner_function,
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

const SUPPORTED_REFERENCE_KINDS: &[(DefinitionReferenceKind, &str)] = &[
    (DefinitionReferenceKind::FunctionCall, "function_call"),
    (DefinitionReferenceKind::NamedType, "named_type"),
    (DefinitionReferenceKind::ObjectReference, "object_reference"),
    (DefinitionReferenceKind::ParameterRead, "parameter_read"),
    (DefinitionReferenceKind::QueryObject, "query_object"),
    (DefinitionReferenceKind::QueryField, "query_field"),
    (DefinitionReferenceKind::Expression, "expression"),
    (DefinitionReferenceKind::WriteObject, "write_object"),
    (DefinitionReferenceKind::WriteField, "write_field"),
];

fn supported_reference_kind_sql(kind: DefinitionReferenceKind) -> TestResult<&'static str> {
    SUPPORTED_REFERENCE_KINDS
        .iter()
        .find(|(supported, _)| *supported == kind)
        .map(|(_, sql)| *sql)
        .ok_or_else(|| failure("unsupported definition reference kind in recovery fixture"))
}

type ResolvedTypeColumns = (&'static str, Option<String>, Option<Vec<u8>>);

fn resolved_type_columns(resolved: ResolvedType) -> TestResult<ResolvedTypeColumns> {
    if let Some(scalar) = resolved.legacy_scalar() {
        return Ok(("scalar", Some(scalar_storage(scalar).0.to_owned()), None));
    }
    if let Some(target) = resolved.named_type() {
        return Ok(("named", None, Some(target.to_bytes().to_vec())));
    }
    if let Some(target) = resolved.reference_target() {
        return Ok(("reference", None, Some(target.to_bytes().to_vec())));
    }
    if resolved.value_type().is_some() {
        return Err(failure(
            "resolved value types are not supported by legacy recovery fixture encoding",
        ));
    }
    Err(failure(
        "resolved type must expose one supported legacy recovery shape",
    ))
}

async fn install_object_revision(
    database: &TestDatabase,
    shared_defaults: bool,
) -> TestResult<ObjectFixture> {
    let unit = install_source_only_revision(database, OBJECT_SOURCE).await?;
    let session = database.open().await?;
    let operation_result: TestResult<ObjectFixture> = async {
        let row = session
            .client()
            .query_one(
                "SELECT catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        let catalogue_id = CatalogueRevisionId::from_bytes(exact_identity(
            row.try_get("catalogue_revision_id")?,
            "active catalogue revision identity",
        )?);
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes([0x61; 16]),
            QualifiedSemanticName::new(["café"])?,
        );
        let left_id = TypeId::from_bytes([0x81; 16]);
        let right_id = TypeId::from_bytes([0x82; 16]);
        let empty_id = TypeId::from_bytes([0x83; 16]);
        let expression_id = ExpressionId::from_bytes([0xc1; 16]);
        let payload = "π = 3".as_bytes().to_vec();
        let expression = ExpressionArtifact::new(
            expression_id,
            "orna.constant",
            1,
            payload.clone(),
            artifact_payload_digest(&payload)?,
        )?;
        let defaults = shared_defaults.then_some(expression_id);
        let scalar_types = [
            StandardScalar::Boolean,
            StandardScalar::Integer,
            StandardScalar::BigInt,
            StandardScalar::Float,
            StandardScalar::Decimal,
            StandardScalar::CharacterLargeObject,
            StandardScalar::BinaryLargeObject,
            StandardScalar::Uuid,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
        ];
        let mut left_fields = scalar_types
            .into_iter()
            .enumerate()
            .map(|(ordinal, scalar)| {
                FieldDefinition::new(
                    FieldId::from_bytes(
                        [0x90 + u8::try_from(ordinal).expect("twelve scalars"); 16],
                    ),
                    format!("scalar_{ordinal}"),
                    u32::try_from(ordinal).expect("twelve scalars"),
                    ResolvedType::scalar(scalar),
                    ordinal % 2 == 0,
                    false,
                    (ordinal < 2).then_some(defaults).flatten(),
                    None,
                )
            })
            .collect::<Vec<_>>();
        for (offset, (target, nullable, on_delete)) in [
            (right_id, false, None),
            (right_id, false, Some(OnDeleteAction::Restrict)),
            (right_id, true, Some(OnDeleteAction::SetNull)),
            (right_id, false, Some(OnDeleteAction::Cascade)),
            (left_id, false, Some(OnDeleteAction::Cascade)),
        ]
        .into_iter()
        .enumerate()
        {
            left_fields.push(FieldDefinition::new(
                FieldId::from_bytes([0xa0 + u8::try_from(offset).expect("five references"); 16]),
                format!("reference_{offset}"),
                12 + u32::try_from(offset).expect("five references"),
                ResolvedType::reference(target),
                nullable,
                offset == 0,
                None,
                on_delete,
            ));
        }
        let right_fields = vec![FieldDefinition::new(
            FieldId::from_bytes([0xa0; 16]),
            "mutual_reference",
            0,
            ResolvedType::reference(left_id),
            false,
            false,
            None,
            None,
        )];
        let objects = vec![
            ObjectTypeDefinition::new(
                left_id,
                QualifiedSemanticName::new(["café", "Α"])?,
                left_fields,
            ),
            ObjectTypeDefinition::new(
                right_id,
                QualifiedSemanticName::new(["café", "世界"])?,
                right_fields,
            ),
            ObjectTypeDefinition::new(
                empty_id,
                QualifiedSemanticName::new(["café", "empty"])?,
                Vec::new(),
            ),
        ];
        let catalogue =
            CatalogueSnapshot::new(catalogue_id, vec![schema.clone()], objects.clone())?;
        let source = SourceOrigin::new(unit.id(), 0, u32::try_from(OBJECT_SOURCE.len())?)?;
        let mut origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            source,
        )];
        for object in &objects {
            origins.extend(object.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object.id(),
                        field: field.id(),
                    },
                    source,
                )
            }));
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object.id()),
                source,
            ));
        }
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Expression(expression.id()),
            source,
        ));
        let catalogue_hash = catalogue_digest(
            &catalogue,
            &[],
            std::slice::from_ref(&expression),
            &origins,
            &[],
        )?;

        session.client().batch_execute("BEGIN").await?;
        insert_schema_record(session.client(), catalogue_id, &schema, source).await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                    (catalogue_revision_id, expression_id, format, format_version,
                     payload, content_hash, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &expression.id().to_bytes().to_vec(),
                    &expression.format(),
                    &i32::try_from(expression.version())?,
                    &expression.payload(),
                    &expression.content_hash().to_bytes().to_vec(),
                    &source.source_unit().to_bytes().to_vec(),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await?;
        for object in &objects {
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_object_types
                        (catalogue_revision_id, type_id, schema_id, name_parts,
                         source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    &[
                        &catalogue_id.to_bytes().to_vec(),
                        &object.id().to_bytes().to_vec(),
                        &schema.id().to_bytes().to_vec(),
                        &object.name().parts(),
                        &source.source_unit().to_bytes().to_vec(),
                        &i64::from(source.byte_start()),
                        &i64::from(source.byte_end()),
                    ],
                )
                .await?;
            for field in object.fields() {
                insert_field_record(session.client(), catalogue_id, object.id(), field, source)
                    .await?;
            }
        }
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = $2
                 WHERE id = $1",
                &[
                    &catalogue_id.to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session
            .client()
            .batch_execute(&physical_catalogue_sql(&objects)?)
            .await?;
        session.client().batch_execute("COMMIT").await?;

        Ok(ObjectFixture {
            unit,
            catalogue,
            expression,
            origins,
        })
    }
    .await;
    finish_session(operation_result, session.shutdown().await, "object fixture")
}

async fn insert_schema_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    schema: &SchemaDefinition,
    source: SourceOrigin,
) -> TestResult<()> {
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &catalogue.to_bytes().to_vec(),
                &schema.id().to_bytes().to_vec(),
                &schema.name().parts(),
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

async fn insert_field_record(
    client: &tokio_postgres::Client,
    catalogue: CatalogueRevisionId,
    owner: TypeId,
    field: &FieldDefinition,
    source: SourceOrigin,
) -> TestResult<()> {
    let (kind, scalar, target) = resolved_type_columns(field.resolved_type())?;
    let default_expression = field
        .default_expression()
        .map(|expression| expression.to_bytes().to_vec());
    let on_delete = field.on_delete().map(|action| match action {
        OnDeleteAction::Restrict => "restrict".to_owned(),
        OnDeleteAction::SetNull => "set_null".to_owned(),
        OnDeleteAction::Cascade => "cascade".to_owned(),
    });
    client
        .execute(
            "INSERT INTO _orna_kernel.catalogue_fields
                (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                 type_kind, scalar_type, target_type_id, nullable, is_unique,
                 default_expression_id, on_delete,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                     $11, $12, $13, $14, $15)",
            &[
                &catalogue.to_bytes().to_vec(),
                &owner.to_bytes().to_vec(),
                &field.id().to_bytes().to_vec(),
                &field.name(),
                &i64::from(field.ordinal()),
                &kind,
                &scalar,
                &target,
                &field.nullable(),
                &field.unique(),
                &default_expression,
                &on_delete,
                &source.source_unit().to_bytes().to_vec(),
                &i64::from(source.byte_start()),
                &i64::from(source.byte_end()),
            ],
        )
        .await?;
    Ok(())
}

fn scalar_storage(scalar: StandardScalar) -> (&'static str, &'static str) {
    match scalar {
        StandardScalar::Boolean => ("boolean", "boolean"),
        StandardScalar::Integer => ("integer", "integer"),
        StandardScalar::BigInt => ("bigint", "bigint"),
        StandardScalar::Float => ("float", "double precision"),
        StandardScalar::Decimal => ("decimal", "numeric"),
        StandardScalar::CharacterLargeObject => ("character_large_object", "text"),
        StandardScalar::BinaryLargeObject => ("binary_large_object", "bytea"),
        StandardScalar::Uuid => ("uuid", "uuid"),
        StandardScalar::Date => ("date", "date"),
        StandardScalar::Time => ("time", "time without time zone"),
        StandardScalar::Timestamp => ("timestamp", "timestamp with time zone"),
        StandardScalar::Duration => ("duration", "interval"),
        StandardScalar::Void => ("void", "void"),
    }
}

fn physical_catalogue_sql(objects: &[ObjectTypeDefinition]) -> TestResult<String> {
    let mut statements = Vec::new();
    let mut references = Vec::new();
    for object in objects {
        let type_hex = raw_id_hex(object.id().to_bytes());
        let table = format!("t_{type_hex}");
        let mut definitions = vec![
            "_orna_object_id bytea NOT NULL".to_owned(),
            format!("CONSTRAINT pk_{type_hex} PRIMARY KEY (_orna_object_id)"),
            format!(
                "CONSTRAINT ck_{type_hex}_object_id CHECK (octet_length(_orna_object_id) = 16)"
            ),
        ];
        for field in object.fields() {
            let field_hex = raw_id_hex(field.id().to_bytes());
            let column = format!("f_{field_hex}");
            let resolved = field.resolved_type();
            let sql_type = if let Some(scalar) = resolved.legacy_scalar() {
                scalar_storage(scalar).1
            } else if resolved.named_type().is_some() || resolved.reference_target().is_some() {
                "bytea"
            } else if resolved.value_type().is_some() {
                return Err(failure(
                    "resolved value types are not supported by legacy physical fixture encoding",
                ));
            } else {
                return Err(failure(
                    "resolved type must expose one supported legacy physical shape",
                ));
            };
            let nullability = if field.nullable() { "" } else { " NOT NULL" };
            definitions.push(format!("{column} {sql_type}{nullability}"));
            if let Some(target) = resolved.reference_target() {
                definitions.push(format!(
                    "CONSTRAINT ck_{field_hex}_object_id CHECK (octet_length({column}) = 16)"
                ));
                let target_table = format!("t_{}", raw_id_hex(target.to_bytes()));
                let delete_action = match field.on_delete() {
                    None => "NO ACTION",
                    Some(OnDeleteAction::Restrict) => "RESTRICT",
                    Some(OnDeleteAction::SetNull) => "SET NULL",
                    Some(OnDeleteAction::Cascade) => "CASCADE",
                };
                references.push(format!(
                    "ALTER TABLE _orna_data.{table} ADD CONSTRAINT fk_{field_hex} \
                     FOREIGN KEY ({column}) REFERENCES _orna_data.{target_table} \
                    (_orna_object_id) ON DELETE {delete_action};"
                ));
            }
            if field.unique() {
                definitions.push(format!("CONSTRAINT uq_{field_hex} UNIQUE ({column})"));
            }
        }
        statements.push(format!(
            "CREATE TABLE _orna_data.{table} ({});",
            definitions.join(", ")
        ));
        statements.push(format!(
            "REVOKE ALL ON TABLE _orna_data.{table} FROM PUBLIC;"
        ));
    }
    statements.extend(references);
    Ok(statements.join("\n"))
}

fn raw_id_hex(bytes: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(bytes))
}

async fn install_schema_revision(database: &TestDatabase) -> TestResult<SchemaFixture> {
    let unit = install_source_only_revision(database, SCHEMA_SOURCE).await?;
    let session = database.open().await?;
    let operation_result: TestResult<SchemaFixture> = async {
        let row = session
            .client()
            .query_one(
                "SELECT catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        let catalogue = CatalogueRevisionId::from_bytes(exact_identity(
            row.try_get("catalogue_revision_id")?,
            "active catalogue revision identity",
        )?);
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes([0x61; 16]),
            QualifiedSemanticName::new(["café"])?,
        );
        let origin = DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            SourceOrigin::new(
                unit.id(),
                0,
                u32::try_from(SCHEMA_SOURCE.trim_end_matches('\n').len())?,
            )?,
        );
        let snapshot = CatalogueSnapshot::new(catalogue, vec![schema.clone()], Vec::new())?;
        let catalogue_hash =
            catalogue_digest(&snapshot, &[], &[], std::slice::from_ref(&origin), &[])?;
        let name_parts = schema.name().parts().to_vec();
        let source = origin.source();

        session.client().batch_execute("BEGIN").await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.catalogue_schemas
                    (catalogue_revision_id, schema_id, name_parts,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &catalogue.to_bytes().to_vec(),
                    &schema.id().to_bytes().to_vec(),
                    &name_parts,
                    &source.source_unit().to_bytes().to_vec(),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
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
                    &catalogue.to_bytes().to_vec(),
                    &catalogue_hash.to_bytes().to_vec(),
                ],
            )
            .await?;
        session.client().batch_execute("COMMIT").await?;

        Ok(SchemaFixture {
            unit,
            schema,
            origin,
        })
    }
    .await;
    finish_session(operation_result, session.shutdown().await, "schema fixture")
}

#[derive(Debug, Eq, PartialEq)]
struct KernelTableSnapshot {
    name: String,
    rows: String,
}

async fn replace_active_catalogue_identity_with_offline_sentinel(
    database: &TestDatabase,
) -> TestResult<()> {
    let active = active_catalogue_identity(database).await?;
    require(
        active != EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        "bootstrap unexpectedly created the offline application catalogue identity",
    )?;
    let session = database.open().await?;
    let operation_result: TestResult<()> = async {
        session
            .client()
            .batch_execute(
                "BEGIN;
                 ALTER TABLE _orna_kernel.active_revision DISABLE TRIGGER ALL;
                 ALTER TABLE _orna_kernel.catalogue_revisions DISABLE TRIGGER ALL;
                 UPDATE _orna_kernel.active_revision
                 SET catalogue_revision_id = decode(repeat('00', 16), 'hex')
                 WHERE singleton = true;
                 UPDATE _orna_kernel.catalogue_revisions
                 SET id = decode(repeat('00', 16), 'hex')
                 WHERE source_revision_id = (
                     SELECT source_revision_id
                     FROM _orna_kernel.active_revision
                     WHERE singleton = true
                 );
                 ALTER TABLE _orna_kernel.catalogue_revisions ENABLE TRIGGER ALL;
                 ALTER TABLE _orna_kernel.active_revision ENABLE TRIGGER ALL;
                 COMMIT;",
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "offline catalogue identity fixture",
    )
}

async fn active_catalogue_identity(database: &TestDatabase) -> TestResult<CatalogueRevisionId> {
    let session = database.open().await?;
    let operation_result = async {
        let row = session
            .client()
            .query_one(
                "SELECT catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        Ok(CatalogueRevisionId::from_bytes(exact_identity(
            row.try_get("catalogue_revision_id")?,
            "active catalogue revision identity",
        )?))
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "active catalogue identity inspection",
    )
}

async fn snapshot_kernel_tables(database: &TestDatabase) -> TestResult<Vec<KernelTableSnapshot>> {
    let session = database.open().await?;
    let operation_result: TestResult<Vec<KernelTableSnapshot>> = async {
        let tables = session
            .client()
            .query(
                "SELECT table_name
                 FROM information_schema.tables
                 WHERE table_schema = '_orna_kernel'
                   AND table_type = 'BASE TABLE'
                 ORDER BY table_name",
                &[],
            )
            .await?;
        let mut snapshot = Vec::with_capacity(tables.len());
        for table in tables {
            let name: String = table.try_get("table_name")?;
            require(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
                format!("unexpected kernel table name in snapshot: {name}"),
            )?;
            let row = session
                .client()
                .query_one(
                    &format!(
                        "SELECT COALESCE(
                            jsonb_agg(snapshot.payload ORDER BY snapshot.payload::text),
                            '[]'::jsonb
                         )::text
                         FROM (
                            SELECT to_jsonb(source) AS payload
                            FROM _orna_kernel.\"{name}\" AS source
                         ) AS snapshot"
                    ),
                    &[],
                )
                .await?;
            snapshot.push(KernelTableSnapshot {
                name,
                rows: row.try_get(0)?,
            });
        }
        Ok(snapshot)
    }
    .await;
    finish_session(
        operation_result,
        session.shutdown().await,
        "kernel table snapshot",
    )
}

fn require_offline_application_catalogue_error(error: &PostgresKernelError) -> TestResult<()> {
    let PostgresKernelError::RevisionInvariant(core) = error else {
        return Err(failure(format!(
            "offline application catalogue identity produced the wrong wrapper: {error}"
        )));
    };
    require(
        matches!(
            core,
            RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { revision, role }
                if *revision == EMPTY_APPLICATION_CATALOGUE_REVISION_ID
                    && *role == DurableCatalogueRevisionRole::ActiveOrRecoveredApplication
        ),
        format!("offline application catalogue identity produced the wrong core error: {core}"),
    )?;
    require(
        core.to_string()
            == "the reserved offline-check catalogue identity cannot be used in a durable revision",
        "offline application catalogue identity changed the core error display",
    )?;
    require(
        error.to_string()
            == "recovered revision invariant failed: the reserved offline-check catalogue identity cannot be used in a durable revision",
        "offline application catalogue identity changed the wrapper error display",
    )?;
    require(
        Error::source(core).is_none(),
        "offline application catalogue core error unexpectedly has a source",
    )?;
    require(
        Error::source(error).map(ToString::to_string)
            == Some(
                "the reserved offline-check catalogue identity cannot be used in a durable revision"
                    .to_owned(),
            ),
        "offline application catalogue wrapper did not retain the exact core source",
    )
}

fn require_offline_standard_catalogue_error(error: &PostgresKernelError) -> TestResult<()> {
    let PostgresKernelError::RevisionInvariant(core) = error else {
        return Err(failure(format!(
            "offline standard catalogue identity produced the wrong wrapper: {error}"
        )));
    };
    require(
        matches!(
            core,
            RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { revision, role }
                if *revision == EMPTY_APPLICATION_CATALOGUE_REVISION_ID
                    && *role == DurableCatalogueRevisionRole::ActiveOrRecoveredStandard
        ),
        format!("offline standard catalogue identity produced the wrong core error: {core}"),
    )?;
    require(
        core.to_string()
            == "the reserved offline-check catalogue identity cannot be used in a durable revision",
        "offline standard catalogue identity changed the core error display",
    )?;
    require(
        error.to_string()
            == "recovered revision invariant failed: the reserved offline-check catalogue identity cannot be used in a durable revision",
        "offline standard catalogue identity changed the wrapper error display",
    )?;
    require(
        Error::source(core).is_none(),
        "offline standard catalogue core error unexpectedly has a source",
    )?;
    require(
        Error::source(error).map(ToString::to_string)
            == Some(
                "the reserved offline-check catalogue identity cannot be used in a durable revision"
                    .to_owned(),
            ),
        "offline standard catalogue wrapper did not retain the exact core source",
    )
}

async fn run_batch(database: &TestDatabase, statement: &str) -> TestResult<()> {
    let session = database.open().await?;
    let operation_result = session
        .client()
        .batch_execute(statement)
        .await
        .map_err(|error| Box::new(error) as _);
    finish_session(
        operation_result,
        session.shutdown().await,
        "database mutation",
    )
}

async fn recovery_error(database: &TestDatabase) -> TestResult<PostgresKernelError> {
    match kernel(database)?.recover().await {
        Ok(_) => Err(failure("tampered durable state recovered successfully")),
        Err(error) => Ok(error),
    }
}

fn require_expected_error(
    error: PostgresKernelError,
    expected: ExpectedRecoveryError,
) -> TestResult<()> {
    let matches = match expected {
        ExpectedRecoveryError::AnyInvariant => matches!(
            error,
            PostgresKernelError::DurableInvariant { .. }
                | PostgresKernelError::CanonicalHash(_)
                | PostgresKernelError::CatalogueSnapshot(_)
                | PostgresKernelError::RevisionInvariant(_)
                | PostgresKernelError::RowDecode { .. }
        ),
        ExpectedRecoveryError::AnyDurable => {
            matches!(error, PostgresKernelError::DurableInvariant { .. })
        }
        ExpectedRecoveryError::Canonical => matches!(error, PostgresKernelError::CanonicalHash(_)),
        ExpectedRecoveryError::Catalogue => {
            matches!(error, PostgresKernelError::CatalogueSnapshot(_))
        }
        ExpectedRecoveryError::Durable(expected_relation) => matches!(
            error,
            PostgresKernelError::DurableInvariant { relation, .. }
                if relation == expected_relation
        ),
        ExpectedRecoveryError::DurableExact {
            relation: expected_relation,
            rule: expected_rule,
        } => matches!(
            error,
            PostgresKernelError::DurableInvariant { relation, rule, .. }
                if relation == expected_relation && rule == expected_rule
        ),
        ExpectedRecoveryError::Revision => {
            matches!(error, PostgresKernelError::RevisionInvariant(_))
        }
    };
    require(
        matches,
        format!("durable tamper produced the wrong recovery failure: {error}"),
    )
}

fn require_exact_raw_v2_error(
    error: &PostgresKernelError,
    expected_record: &str,
    expected_rule: &str,
) -> TestResult<()> {
    let PostgresKernelError::DurableInvariant {
        relation,
        record,
        rule,
    } = error
    else {
        return Err(failure(format!(
            "raw V2 value tuple produced the wrong wrapper: {error}"
        )));
    };
    require(
        *relation == "_orna_kernel.catalogue_fields"
            && record == expected_record
            && *rule == expected_rule,
        format!(
            "raw V2 value tuple produced relation={relation:?}, record={record:?}, rule={rule:?}"
        ),
    )?;
    require(
        error.to_string()
            == format!(
                "durable invariant failed for _orna_kernel.catalogue_fields record {expected_record}: {expected_rule}"
            ),
        "raw V2 value tuple changed the durable error display",
    )?;
    require(
        error.source().is_none(),
        "raw V2 value tuple durable error unexpectedly has a source",
    )
}

fn finish_session<T>(
    operation: TestResult<T>,
    shutdown: TestResult<()>,
    context: &str,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(shutdown_error)) => Err(failure(format!(
            "{context} failed: {operation_error}; session shutdown failed: {shutdown_error}"
        ))),
    }
}

fn exact_identity(value: Vec<u8>, description: &str) -> TestResult<[u8; 16]> {
    value
        .try_into()
        .map_err(|_| failure(format!("{description} was not exactly 16 bytes")))
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn standard_client_candidate(
    source: &str,
    active: &orna_core::revision::ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<orna_core::revision::DeployableRevision> {
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )?;
    let bundle = orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
        "main.orna",
        source,
    )])?;
    let report = check_standard_application(&bundle, &context);
    require(
        report.diagnostics().is_empty(),
        format!(
            "standard CLIENT compiler diagnostics: {:?}",
            report.diagnostics()
        ),
    )?;
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
}

#[derive(Clone)]
struct StandardClientExecutionFacts {
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionRecord,
}

fn require_standard_client_execution(
    active: &orna_core::revision::ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
    expected_value: bool,
) -> TestResult<StandardClientExecutionFacts> {
    let expected_standard = upgrade.verified_standard_snapshot();
    let selected_standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("standard CLIENT active revision has no selected standard"))?;
    require(
        matches!(
            active.catalogue_hash_context(),
            CatalogueHashContext::Version2 { .. }
        ) && selected_standard.revision() == expected_standard.revision()
            && selected_standard.catalogue().revision() == expected_standard.catalogue().revision()
            && selected_standard.digest() == expected_standard.digest()
            && selected_standard.digest_version() == expected_standard.digest_version()
            && selected_standard.language_version() == expected_standard.language_version()
            && selected_standard.source().bundle() == expected_standard.source().bundle()
            && selected_standard.source().id() == expected_standard.source().id()
            && selected_standard.source().bundle_hash() == expected_standard.source().bundle_hash()
            && selected_standard.source().revision_hash()
                == expected_standard.source().revision_hash(),
        "standard CLIENT active revision changed the selected standard identity",
    )?;
    require(
        selected_standard
            .catalogue()
            .value_type_by_id(orna_standard::BOOLEAN_TYPE_ID)
            .is_some_and(|value_type| {
                value_type.representation_contract() == "orna.kernel.value.boolean@1"
            }),
        "selected standard does not retain the exact Boolean value contract",
    )?;

    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app".to_owned(), "enabled".to_owned()])
        .ok_or_else(|| failure("standard CLIENT Boolean function was not recovered"))?;
    require(
        matches!(
            function.return_type(),
            orna_core::catalogue::FunctionReturn::Single(ResolvedType::Value(id))
                if *id == orna_standard::BOOLEAN_TYPE_ID
        ),
        "standard CLIENT return did not retain the Boolean Value identity",
    )?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| failure("standard CLIENT function has no current immutable revision"))?
        .clone();
    require(
        revision.semantic_hash_version() == FunctionSemanticHashVersion::Version2,
        "standard CLIENT function did not retain the version-two semantic hash contract",
    )?;
    let references = active
        .references()
        .iter()
        .filter(|reference| {
            reference.source_function() == function.id()
                && reference.source_revision() == revision.id()
        })
        .collect::<Vec<_>>();
    require(
        references.len() == 1
            && references[0].ordinal() == 0
            && references[0].kind() == DefinitionReferenceKind::NamedType
            && matches!(
                references[0].target(),
                DefinitionReferenceTarget::ValueType(id) if id == orna_standard::BOOLEAN_TYPE_ID
            ),
        "standard CLIENT function changed its exact NamedType ValueType reference",
    )?;

    let evaluator_principal = PrincipalId::from_bytes([0x79; 16]);
    let evaluator_security = SecuritySnapshot::new(
        active.pair(),
        vec![function.id()],
        vec![Principal::new(
            evaluator_principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(evaluator_principal, function.id())],
    )?;
    let evaluator_session =
        evaluator_security.bind_authenticated_session(evaluator_principal, vec![])?;
    let ExecuteDecision::Allowed(authorisation) = evaluator_security.authorise_execute(
        &evaluator_session,
        InvocationTarget::new(function.id(), active.pair()),
    ) else {
        return Err(failure(
            "standard CLIENT test grant did not authorise evaluation",
        ));
    };
    let result = evaluate_client_function(active, &authorisation)?;
    require(
        result.context().pair() == active.pair()
            && result.context().function() == function.id()
            && result.context().function_revision() == revision.id()
            && result.value() == &RuntimeValue::Boolean(expected_value),
        "recovered standard CLIENT evaluation returned the wrong context or value",
    )?;
    Ok(StandardClientExecutionFacts {
        pair: active.pair(),
        function: function.id(),
        revision,
    })
}

async fn active_revision_pair(database: &TestDatabase) -> TestResult<RevisionPair> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        Ok(RevisionPair::new(
            SourceRevisionId::from_bytes(exact_identity(
                row.try_get("source_revision_id")?,
                "active source revision identity",
            )?),
            CatalogueRevisionId::from_bytes(exact_identity(
                row.try_get("catalogue_revision_id")?,
                "active catalogue revision identity",
            )?),
        ))
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "active revision pointer inspection",
    )
}

async fn standard_boolean_contract(
    database: &TestDatabase,
    upgrade: &orna_standard::StandardUpgrade,
    replacement: Option<&str>,
) -> TestResult<String> {
    let standard = upgrade.verified_standard_snapshot();
    let revision = standard.revision().to_bytes().to_vec();
    let boolean = orna_standard::BOOLEAN_TYPE_ID.to_bytes().to_vec();
    let replacement = replacement.map(str::to_owned);
    let session = database.open().await?;
    let operation = async {
        if let Some(replacement) = replacement.as_deref() {
            let affected = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET representation_contract = $1
                     WHERE standard_library_revision_id = $2 AND type_id = $3",
                    &[&replacement, &revision, &boolean],
                )
                .await?;
            require(
                affected == 1,
                format!("Boolean standard contract update changed {affected} rows"),
            )?;
        }
        let row = session
            .client()
            .query_one(
                "SELECT representation_contract
                 FROM _orna_kernel.standard_catalogue_value_types
                 WHERE standard_library_revision_id = $1 AND type_id = $2",
                &[&revision, &boolean],
            )
            .await?;
        Ok(row.try_get("representation_contract")?)
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "Boolean standard contract inspection or update",
    )
}

fn require_standard_library_digest_mismatch(
    error: &PostgresKernelError,
    expected_revision: [u8; 16],
) -> TestResult<()> {
    let PostgresKernelError::CanonicalHash(CanonicalHashError::StandardLibraryDigestMismatch {
        revision,
    }) = error
    else {
        return Err(failure(format!(
            "Boolean standard contract tamper produced the wrong recovery error: {error}"
        )));
    };
    require(
        revision.to_bytes() == expected_revision,
        "Boolean standard contract tamper reported the wrong standard revision",
    )?;
    require(
        error.to_string()
            == "canonical durable hash failed: stored standard library digest differs from canonical facts",
        "Boolean standard contract tamper changed the kernel error display",
    )?;
    let source = Error::source(error).ok_or_else(|| {
        failure("Boolean standard contract tamper lost the canonical error source")
    })?;
    require(
        source.to_string() == "stored standard library digest differs from canonical facts"
            && Error::source(source).is_none(),
        "Boolean standard contract tamper changed the canonical error source chain",
    )
}

async fn require_no_session_leaks(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(i64, i64)> = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FILTER (WHERE state = 'idle in transaction'),
                        count(*) FILTER (WHERE pid <> pg_catalog.pg_backend_pid())
                 FROM pg_catalog.pg_stat_activity
                 WHERE datname = pg_catalog.current_database()
                   AND backend_type = 'client backend'",
                &[],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    let (idle, others) = finish_session(
        operation,
        session.shutdown().await,
        "session leak inspection",
    )?;
    require(idle == 0, format!("found {idle} idle transaction(s)"))?;
    require(others == 0, format!("found {others} leaked session(s)"))
}

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
