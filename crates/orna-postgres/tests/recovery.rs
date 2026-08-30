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
    CallSiteId, CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId,
    InvocationId, ParameterId, PrincipalId, SchemaId, SourceBundleId, SourceRevisionId,
    SourceUnitId, TypeId,
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
use orna_protocol::{CallFailure, ResourceArgument, ResourceKind, ResourceRequest};
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
#[path = "recovery/catalogue.rs"]
mod catalogue_recovery;
#[path = "recovery/nested_records.rs"]
mod nested_records;
#[path = "recovery/security.rs"]
mod security;

use security::require_execute_audit;

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
async fn security_admin_grant_rejects_object_scope_and_accepts_class_wide_sentinel()
-> TestResult<()> {
    const GRANTEE: PrincipalId = PrincipalId::from_bytes([0x71; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;

        let session = database.open().await?;
        let grantee = GRANTEE.to_bytes().to_vec();
        let object_id = vec![0xa1_u8; 16];
        let empty_object = Vec::<u8>::new();
        let operation: TestResult<()> = async {
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                     VALUES ($1, 'user', 'active')",
                    &[&grantee],
                )
                .await?;

            let invalid = session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_privilege_grants
                         (grantee_id, privilege_class, object_id)
                     VALUES ($1, $2, $3)",
                    &[&grantee, &"security_admin", &object_id],
                )
                .await;
            let invalid_error = match invalid {
                Ok(_) => {
                    return Err(failure(
                        "object-scoped SecurityAdmin grant unexpectedly succeeded",
                    ));
                }
                Err(error) => error,
            };
            let database_error = invalid_error.as_db_error().ok_or_else(|| {
                failure("object-scoped SecurityAdmin grant returned a non-database error")
            })?;
            require(
                database_error.code().code() == "23514",
                format!(
                    "object-scoped SecurityAdmin grant failed with SQLSTATE {} instead of CHECK violation",
                    database_error.code().code()
                ),
            )?;
            require(
                database_error.constraint()
                    == Some("security_privilege_grants_security_admin_class_wide_check"),
                format!(
                    "object-scoped SecurityAdmin grant failed on unexpected constraint {:?}",
                    database_error.constraint()
                ),
            )?;

            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.security_privilege_grants
                         (grantee_id, privilege_class, object_id)
                     VALUES ($1, $2, $3)",
                    &[&grantee, &"security_admin", &empty_object],
                )
                .await?;

            let stored: i64 = session
                .client()
                .query_one(
                    "SELECT count(*)
                     FROM _orna_kernel.security_privilege_grants
                     WHERE grantee_id = $1
                       AND privilege_class = $2
                       AND object_id = $3",
                    &[&grantee, &"security_admin", &empty_object],
                )
                .await?
                .get(0);
            require(
                stored == 1,
                "class-wide SecurityAdmin sentinel row was not persisted",
            )
        }
        .await;
        finish_session(
            operation,
            session.shutdown().await,
            "SecurityAdmin class-wide grant constraint probe",
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

async fn run_single_row_statement(database: &TestDatabase, statement: &str) -> TestResult<()> {
    let session = database.open().await?;
    let count = session.client().execute(statement, &[]).await?;
    require(
        count == 1,
        format!("tamper statement updated {count} rows; expected exactly one"),
    )?;
    session.shutdown().await
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
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, $3)",
                &[
                    &bundle.to_bytes().to_vec(),
                    &unit.id().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
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
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, $3)",
                &[
                    &source.bundle().to_bytes().to_vec(),
                    &unit.id().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
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
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, $3)",
                &[
                    &bundle.to_bytes().to_vec(),
                    &unit.id().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
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
                "INSERT INTO _orna_kernel.source_bundle_units
                    (bundle_id, source_unit_id, ordinal)
                 VALUES ($1, $2, $3)",
                &[
                    &bundle.to_bytes().to_vec(),
                    &unit.id().to_bytes().to_vec(),
                    &i64::from(unit.ordinal()),
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

async fn data_row_count(database: &TestDatabase, relation: &str) -> TestResult<i64> {
    let session = database.open().await?;
    let operation: TestResult<i64> = async {
        let row = session
            .client()
            .query_one(&format!("SELECT count(*) FROM {relation}"), &[])
            .await?;
        Ok(row.get(0))
    }
    .await;
    finish_session(operation, session.shutdown().await, "data row count")
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
