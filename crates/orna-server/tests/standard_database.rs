#![cfg(unix)]

use std::error::Error;

use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use orna_server::{OpenStandardDatabaseError, open_standard_database};
use orna_standard::{
    BOOLEAN_TYPE_ID, retained_standard_library_snapshot, verify_standard_library_snapshot,
};

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

macro_rules! standard_context_facts {
    ($active:expr) => {{
        let active = $active;
        let selected = active.catalogue_hash_context().standard().ok_or_else(|| {
            failure("the public opener did not retain a selected standard context")
        })?;
        (
            active.catalogue_hash_context().version().to_u32(),
            selected.revision().to_bytes(),
            selected.catalogue().revision().to_bytes(),
            selected.digest().to_bytes(),
        )
    }};
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn opens_reopens_and_rejects_tampered_standard_database() -> TestResult<()> {
    let expected =
        retained_standard_library_snapshot().and_then(verify_standard_library_snapshot)?;
    let expected_boolean_contract = expected
        .catalogue()
        .value_type_by_id(BOOLEAN_TYPE_ID)
        .ok_or_else(|| failure("the accepted standard library is missing the Boolean value type"))?
        .representation_contract()
        .to_owned();

    with_test_database(|database| async move {
        let opened = open_standard_database(kernel(&database)?).await?;
        let initial = opened.recover().await?;
        let initial_context = standard_context_facts!(&initial);
        require(
            initial_context.0 == 2 && initial_context.1 == expected.revision().to_bytes()
                && initial_context.2 == expected.catalogue().revision().to_bytes()
                && initial_context.3 == expected.digest().to_bytes(),
            "opening a fresh database did not select the exact accepted version-two standard context",
        )?;
        let initial_pair = initial.pair();
        let initial_pointer = active_pointer(&database).await?;
        require(
            initial_pointer
                == (
                    initial_pair.source().to_bytes().to_vec(),
                    initial_pair.catalogue().to_bytes().to_vec(),
                ),
            "the fresh opener recovery pair does not match the active durable pointer",
        )?;

        let reopened = open_standard_database(kernel(&database)?).await?;
        let reopened_active = reopened.recover().await?;
        require(
            reopened_active.pair() == initial_pair
                && standard_context_facts!(&reopened_active) == initial_context,
            "opening an installed version-two database changed its active pair or accepted context",
        )?;

        let mut reconnect_config = database.config()?;
        reconnect_config.application_name("orna-standard-database-reconnect");
        let reconnected = open_standard_database(PostgresKernel::new(reconnect_config)).await?;
        let reconnected_active = reconnected.recover().await?;
        require(
            reconnected_active.pair() == initial_pair
                && standard_context_facts!(&reconnected_active) == initial_context,
            "reconnecting to an installed version-two database changed its active pair or accepted context",
        )?;

        let tampered_contract = format!("{expected_boolean_contract}.tampered");
        let written_contract = boolean_contract(
            &database,
            expected.revision().to_bytes().to_vec(),
            BOOLEAN_TYPE_ID.to_bytes().to_vec(),
            Some(&tampered_contract),
        )
        .await?;
        require(
            written_contract == tampered_contract,
            "the standard Boolean contract tamper did not commit its exact durable value",
        )?;

        let rejection = match open_standard_database(kernel(&database)?).await {
            Ok(_) => return Err(failure("the public opener repaired or accepted the tampered standard")),
            Err(error) => error,
        };
        require(
            matches!(
                &rejection,
                OpenStandardDatabaseError::Kernel {
                    source: PostgresKernelError::CanonicalHash(_),
                }
            ),
            "the public opener did not expose the canonical standard-tamper rejection",
        )?;
        require(
            rejection.to_string()
                == "canonical durable hash failed: stored standard library digest differs from canonical facts",
            "the public opener changed the standard-tamper Display contract",
        )?;
        let kernel_source = Error::source(&rejection)
            .ok_or_else(|| failure("the public standard-tamper error lost its kernel source"))?;
        require(
            kernel_source.to_string()
                == "canonical durable hash failed: stored standard library digest differs from canonical facts",
            "the public standard-tamper error changed its kernel source",
        )?;
        let canonical_source = Error::source(kernel_source)
            .ok_or_else(|| failure("the public standard-tamper error lost its canonical source"))?;
        require(
            canonical_source.to_string() == "stored standard library digest differs from canonical facts"
                && Error::source(canonical_source).is_none(),
            "the public standard-tamper error changed its canonical source chain",
        )?;
        require(
            boolean_contract(
                &database,
                expected.revision().to_bytes().to_vec(),
                BOOLEAN_TYPE_ID.to_bytes().to_vec(),
                None,
            )
            .await?
                == tampered_contract,
            "the failed public opener repaired the tampered standard contract",
        )?;
        require(
            active_pointer(&database).await? == initial_pointer,
            "the failed public opener changed the active durable pointer",
        )
    })
    .await
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(database.connection_string().parse()?)
}

async fn active_pointer(database: &TestDatabase) -> TestResult<(Vec<u8>, Vec<u8>)> {
    let session = database.open().await?;
    let operation = async {
        let row = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id FROM _orna_kernel.active_revision",
                &[],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "active pointer inspection",
    )
}

async fn boolean_contract(
    database: &TestDatabase,
    standard_revision: Vec<u8>,
    boolean_type: Vec<u8>,
    replacement: Option<&str>,
) -> TestResult<String> {
    let session = database.open().await?;
    let operation = async {
        if let Some(replacement) = replacement {
            let affected = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET representation_contract = $3
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &boolean_type, &replacement],
                )
                .await?;
            require(
                affected == 1,
                "the standard Boolean contract tamper did not select exactly one row",
            )?;
        }
        read_boolean_contract_from_client(session.client(), &standard_revision, &boolean_type).await
    }
    .await;
    finish_session(
        operation,
        session.shutdown().await,
        "standard Boolean contract operation",
    )
}

async fn read_boolean_contract_from_client(
    client: &tokio_postgres::Client,
    standard_revision: &[u8],
    boolean_type: &[u8],
) -> TestResult<String> {
    let row = client
        .query_one(
            "SELECT representation_contract
             FROM _orna_kernel.standard_catalogue_value_types
             WHERE standard_library_revision_id = $1 AND type_id = $2",
            &[&standard_revision, &boolean_type],
        )
        .await?;
    Ok(row.try_get(0)?)
}

fn finish_session<T>(
    operation: TestResult<T>,
    shutdown: TestResult<()>,
    label: &str,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(shutdown_error)) => Err(failure(format!(
            "{label} failed: {operation_error}; connection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
