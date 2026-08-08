mod support;

use std::{collections::BTreeSet, str::FromStr, sync::Arc};

use orna_kernel_postgres::PostgresKernel;
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
    "function_artifacts",
    "function_revisions",
    "schema_migrations",
    "source_bundles",
    "source_revisions",
    "source_units",
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
    require_count(
        client,
        "_orna_kernel.schema_migrations",
        "SELECT count(*) FROM _orna_kernel.schema_migrations",
        1,
    )
    .await?;
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

    let migration_version = 1_i64;
    let checksum_row = client
        .query_one(
            "SELECT octet_length(checksum) FROM _orna_kernel.schema_migrations WHERE version = $1",
            &[&migration_version],
        )
        .await?;
    let checksum_length: i32 = value(&checksum_row, 0)?;
    require(
        checksum_length == 32,
        "initial migration checksum is not 32 bytes",
    )?;

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
