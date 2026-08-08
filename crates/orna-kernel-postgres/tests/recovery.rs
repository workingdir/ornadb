mod support;

use std::str::FromStr;

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
    SourceUnitId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, OnDeleteAction,
        QualifiedSemanticName, SchemaDefinition,
    },
    revision::{
        DefinitionIdentity, DefinitionOrigin, ExpressionArtifact, SourceOrigin, StoredSourceUnit,
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
async fn trusted_recovery_rejects_rows_hidden_by_a_shadow_count_aggregate() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(
            &database,
            "CREATE FUNCTION public.zero_count_state(bigint)
             RETURNS bigint LANGUAGE sql IMMUTABLE AS 'SELECT 0';
             CREATE AGGREGATE public.count(*) (
                 SFUNC = public.zero_count_state,
                 STYPE = bigint,
                 INITCOND = '0'
             )",
        )
        .await?;
        run_batch(&database, UNSUPPORTED_FUNCTION_SQL).await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        let error = match PostgresKernel::new(hostile_config).recover().await {
            Ok(_) => return Err(failure("shadowed unsupported-state count recovered rows")),
            Err(error) => error,
        };
        require_expected_error(
            error,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_functions"),
        )
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
    AnyDurable,
    Canonical,
    Catalogue,
    Durable(&'static str),
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
                false,
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
            .batch_execute(&physical_catalogue_sql(&objects))
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
    let (kind, scalar, target) = match field.resolved_type() {
        ResolvedType::Scalar(scalar) => ("scalar", Some(scalar_storage(scalar).0.to_owned()), None),
        ResolvedType::Named(target) => ("named", None, Some(target.to_bytes().to_vec())),
        ResolvedType::Reference { target } => ("reference", None, Some(target.to_bytes().to_vec())),
    };
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

fn physical_catalogue_sql(objects: &[ObjectTypeDefinition]) -> String {
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
            let sql_type = match field.resolved_type() {
                ResolvedType::Scalar(scalar) => scalar_storage(scalar).1,
                ResolvedType::Reference { .. } => "bytea",
                ResolvedType::Named(_) => "bytea",
            };
            let nullability = if field.nullable() { "" } else { " NOT NULL" };
            definitions.push(format!("{column} {sql_type}{nullability}"));
            if let ResolvedType::Reference { target } = field.resolved_type() {
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
    statements.join("\n")
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
