mod support;

use std::{collections::BTreeSet, str::FromStr, sync::Arc};

use orna_core::{
    CatalogueRevisionId, SourceBundleId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
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
    reject_migration_history(5, "future migration", vec![0; 32]).await
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
    ] {
        require(
            reference_kind_constraint.contains(&format!("'{reference_kind}'::text")),
            format!(
                "definition_references reference kind constraint omits {reference_kind:?}: {reference_kind_constraint:?}"
            ),
        )?;
    }
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
