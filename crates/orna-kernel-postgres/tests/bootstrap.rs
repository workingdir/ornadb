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
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, RevisionPair, SourceOrigin,
        StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use sha2::{Digest, Sha256};
use support::{TestDatabase, TestResult, failure, with_test_database};
use tokio_postgres::{Client, Row};

const EXPECTED_KERNEL_TABLES: &[&str] = &[
    "active_revision",
    "catalogue_expressions",
    "catalogue_fields",
    "catalogue_function_parameters",
    "catalogue_function_return_columns",
    "catalogue_functions",
    "catalogue_object_types",
    "catalogue_revisions",
    "catalogue_schemas",
    "definition_references",
    "function_artifacts",
    "function_revisions",
    "schema_migrations",
    "source_bundles",
    "source_revisions",
    "source_units",
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
    "catalogue_object_types",
    "catalogue_fields",
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
    migrations: Vec<(i64, String, Vec<u8>)>,
    references: Vec<DefinitionReferenceSnapshot>,
    catalogue_hashes: Vec<(Vec<u8>, Vec<u8>)>,
    function_hashes: Vec<(Vec<u8>, Vec<u8>)>,
}

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
fn write_reference_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(6, MIGRATIONS[5].2)),
        "e831811c0f42d6f4b3ab2601cf480fabaaed03b5547e2615400b9eec4b6b53bf"
    );
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
async fn bootstrap_owner_qualifies_registered_v4_semantic_references() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, false).await?;
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
            after.migrations.len() == 6 && after.migrations[..5] == before.migrations[..],
            format!("v6 changed prior migration records: {:?}", after.migrations),
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
async fn bootstrap_rolls_back_v5_for_a_dangling_legacy_reference() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, true).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v5 must reject a dangling legacy field reference");
        require(
            matches!(error, PostgresKernelError::Database(_)),
            format!("dangling legacy reference produced the wrong failure: {error}"),
        )?;
        let database_message = match &error {
            PostgresKernelError::Database(error) => error
                .as_db_error()
                .map(tokio_postgres::error::DbError::message),
            _ => None,
        };
        require(
            database_message
                == Some("cannot owner-qualify a dangling or ambiguous legacy field reference"),
            format!("dangling legacy reference produced an unexpected error: {error}"),
        )?;
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
        require(
            matches!(error, PostgresKernelError::Database(_)),
            format!("unexpected error for an unhashable initial catalogue: {error}"),
        )
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
    reject_migration_history(7, "future migration", vec![0; 32]).await
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
    let expected_columns = BTreeSet::from([
        ("source_end".to_owned(), "YES".to_owned()),
        ("source_start".to_owned(), "YES".to_owned()),
        ("source_unit_id".to_owned(), "YES".to_owned()),
    ]);

    for table in ORIGIN_TABLES {
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
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_kind_check",
        "target_kind = ANY",
    )
    .await?;
    let target_kind_constraint = constraint_definition(
        client,
        "definition_references",
        "definition_references_target_kind_check",
    )
    .await?;
    for target_kind in [
        "object_type",
        "field",
        "function",
        "parameter",
        "expression",
    ] {
        require(
            target_kind_constraint.contains(&format!("'{target_kind}'::text")),
            format!(
                "definition_references target kind constraint omits {target_kind:?}: {target_kind_constraint:?}"
            ),
        )?;
    }
    for unsupported_kind in ["schema", "return_column"] {
        require(
            !target_kind_constraint.contains(&format!("'{unsupported_kind}'::text")),
            format!(
                "definition_references incorrectly accepts {unsupported_kind:?} as a stable target: {target_kind_constraint:?}"
            ),
        )?;
    }

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

    let owner_shape_constraint = constraint_definition(
        client,
        "definition_references",
        "definition_references_target_owner_shape_check",
    )
    .await?;
    for expected_fragment in [
        "target_kind = 'field'::text",
        "target_kind = 'parameter'::text",
        "target_owner_type_id IS NOT NULL",
        "target_owner_function_id IS NOT NULL",
        "target_owner_type_id IS NULL",
        "target_owner_function_id IS NULL",
        "target_kind <> ALL",
    ] {
        require(
            owner_shape_constraint.contains(expected_fragment),
            format!(
                "definition reference owner-shape constraint omits {expected_fragment:?}: {owner_shape_constraint:?}"
            ),
        )?;
    }

    let compatibility_constraint = constraint_definition(
        client,
        "definition_references",
        "definition_references_reference_target_compatibility_check",
    )
    .await?;
    for expected_fragment in [
        "reference_kind = 'function_call'::text",
        "target_kind = 'function'::text",
        "'named_type'::text",
        "'object_reference'::text",
        "'query_object'::text",
        "target_kind = 'object_type'::text",
        "reference_kind = 'parameter_read'::text",
        "target_kind = 'parameter'::text",
        "reference_kind = 'query_field'::text",
        "target_kind = 'field'::text",
        "reference_kind = 'expression'::text",
        "target_kind = 'expression'::text",
        "reference_kind = 'write_object'::text",
        "reference_kind = 'write_field'::text",
    ] {
        require(
            compatibility_constraint.contains(expected_fragment),
            format!(
                "definition reference compatibility constraint omits {expected_fragment:?}: {compatibility_constraint:?}"
            ),
        )?;
    }

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
        "definition_references_parameter_target_fk",
        "FOREIGN KEY (catalogue_revision_id, target_owner_function_id, target_definition_id) REFERENCES _orna_kernel.catalogue_function_parameters(catalogue_revision_id, function_id, parameter_id) DEFERRABLE INITIALLY DEFERRED",
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
        "definition_references_parameter_target_index",
        "(target_owner_function_id, target_definition_id, catalogue_revision_id) WHERE (target_kind = 'parameter'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_direct_target_index",
        "(target_kind, target_definition_id, catalogue_revision_id) WHERE (target_kind <> ALL (ARRAY['field'::text, 'parameter'::text]))",
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

async fn seed_registered_v3_empty_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let seed_result = async {
        seed_initial_catalogue_client(session.client()).await?;
        for (version, name, sql) in &MIGRATIONS[1..3] {
            session.client().batch_execute(sql).await?;
            let checksum = expected_migration_checksum(*version, sql);
            session
                .client()
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

async fn seed_registered_v4_physical_catalogue(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let result = session
        .client()
        .batch_execute(
            "CREATE TABLE _orna_data.t_06060606060606060606060606060606 (
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
    for (version, name, sql) in &MIGRATIONS[1..4] {
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

fn legacy_reference_target(target: DefinitionReferenceTarget) -> (Vec<u8>, &'static str) {
    match target {
        DefinitionReferenceTarget::ObjectType(id) => (id.to_bytes().to_vec(), "object_type"),
        DefinitionReferenceTarget::Field { field, .. } => (field.to_bytes().to_vec(), "field"),
        DefinitionReferenceTarget::Function(id) => (id.to_bytes().to_vec(), "function"),
        DefinitionReferenceTarget::Parameter { parameter, .. } => {
            (parameter.to_bytes().to_vec(), "parameter")
        }
        DefinitionReferenceTarget::Expression(id) => (id.to_bytes().to_vec(), "expression"),
    }
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
        let (target_definition_id, target_kind) = legacy_reference_target(reference.target());
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
