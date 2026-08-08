mod support;

use std::{collections::BTreeSet, str::FromStr, sync::Arc};

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
    reject_migration_history(3, "future migration", vec![0; 32]).await
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
            checksum == Sha256::digest(migration_sql.as_bytes()).to_vec(),
            format!("migration {version} checksum does not match its SQL"),
        )?;
        require(
            checksum.len() == 32,
            format!("migration {version} checksum is not 32 bytes"),
        )?;
    }
    Ok(())
}

async fn inspect_empty_aggregate_hashes(client: &Client) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT 'source_bundles' AS relation, content_hash, hash_algorithm
             FROM _orna_kernel.source_bundles
             UNION ALL
             SELECT 'source_revisions' AS relation, content_hash, hash_algorithm
             FROM _orna_kernel.source_revisions
             UNION ALL
             SELECT 'catalogue_revisions' AS relation, content_hash, hash_algorithm
             FROM _orna_kernel.catalogue_revisions
             ORDER BY relation",
            &[],
        )
        .await?;
    let empty_hash = Sha256::digest([]).to_vec();
    require(
        rows.len() == 3,
        "empty revision has incomplete aggregate hashes",
    )?;
    for row in rows {
        let relation: String = value(&row, 0)?;
        let content_hash: Vec<u8> = value(&row, 1)?;
        let hash_algorithm: String = value(&row, 2)?;
        require(
            content_hash == empty_hash,
            format!("{relation} does not store SHA-256 of the empty aggregate"),
        )?;
        require(
            hash_algorithm == "sha256",
            format!("{relation} hash algorithm is {hash_algorithm:?}; expected sha256"),
        )?;
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
        "catalogue_functions",
        "catalogue_functions_current_revision_fk",
        "FOREIGN KEY (function_id, current_function_revision_id) REFERENCES _orna_kernel.function_revisions(function_id, id)",
    )
    .await
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
