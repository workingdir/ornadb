mod support;

use std::{error::Error, str::FromStr};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
        function_declaration_digest, function_semantic_digest,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, OnDeleteAction, ParameterDefinition, QualifiedSemanticName,
        SchemaDefinition, TypeLookupName, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DurableCatalogueRevisionRole,
        EMPTY_APPLICATION_CATALOGUE_REVISION_ID, ExecutableArtifact, ExecutableArtifactKind,
        ExpressionArtifact, FunctionRevisionRecord, FunctionSemanticHashVersion,
        RevisionInvariantError, SourceOrigin, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};

const SCHEMA_SOURCE: &str = "schema café;\n";
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
        )
    })
    .await
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
             DROP CONSTRAINT catalogue_functions_check1;
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
        for binding in standard.catalogue().type_bindings() {
            let origin = standard_origin(standard, DefinitionIdentity::TypeBinding(binding.id()))?;
            insert_standard_binding(session.client(), standard.revision(), binding, origin).await?;
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
    client
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_type_bindings
                (standard_library_revision_id, type_binding_id, kind, name_parts,
                 target_type_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &revision.to_bytes().to_vec(),
                &binding.id().to_bytes().to_vec(),
                &kind,
                &name_parts,
                &binding.target().to_bytes().to_vec(),
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

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
