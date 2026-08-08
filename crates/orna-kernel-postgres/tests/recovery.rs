mod support;

use std::str::FromStr;

use orna_core::{
    SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{
        source_bundle_digest, source_revision_record_digest, source_unit_content_digest,
    },
    revision::StoredSourceUnit,
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};

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
async fn rejects_semantic_and_physical_state_that_this_slice_cannot_recover() -> TestResult<()> {
    reject_unsupported_state(
        "INSERT INTO _orna_kernel.catalogue_schemas
            (catalogue_revision_id, schema_id, name_parts)
         SELECT catalogue_revision_id, decode(repeat('a1', 16), 'hex'), ARRAY['unexpected']
         FROM _orna_kernel.active_revision",
        "_orna_kernel.catalogue_schemas",
    )
    .await?;
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
    Canonical,
    Durable(&'static str),
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
        ExpectedRecoveryError::Canonical => matches!(error, PostgresKernelError::CanonicalHash(_)),
        ExpectedRecoveryError::Durable(expected_relation) => matches!(
            error,
            PostgresKernelError::DurableInvariant { relation, .. }
                if relation == expected_relation
        ),
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
