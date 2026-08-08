mod support;

use std::{str::FromStr, sync::Arc};

use orna_compiler::{check, prepare};
use orna_core::{
    TypeId,
    revision::{ActiveDatabaseRevision, DeployableRevision, FunctionRevisionRecord},
    source::{SourceBundle, SourceUnit},
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};
use tokio::sync::Barrier;

const BASIC_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

const BASIC_SOURCE_ONLY_EDIT: &str = "-- source-only formatting edit\n\
    CREATE SCHEMA app;\n\n\
    CREATE TYPE app.widget AS OBJECT ( name TEXT NOT NULL, active BOOL NOT NULL );\n\
    CREATE SERVER FUNCTION app.list_widgets() RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

const BASIC_CHANGED_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = TRUE;\n";

const MUTUAL_REFERENCE_SOURCE: &str = "CREATE SCHEMA graph;\n\
    CREATE TYPE graph.left AS OBJECT (right REF graph.right);\n\
    CREATE TYPE graph.right AS OBJECT (left REF graph.left);\n";

const RACE_LEFT_SOURCE: &str = "CREATE SCHEMA race_left;\n\
    CREATE TYPE race_left.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_left.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_left.item item;\n";

const RACE_RIGHT_SOURCE: &str = "CREATE SCHEMA race_right;\n\
    CREATE TYPE race_right.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_right.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_right.item item;\n";

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_a_compiler_candidate_and_recovers_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &active)?;

        let applied = kernel.apply(&candidate).await?;
        let recovered = kernel.recover().await?;

        require_recovered_new_candidate(&candidate, &applied)?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        require(
            recovered.catalogue().schemas().len() == 1
                && recovered.catalogue().object_types().len() == 1
                && recovered.catalogue().functions().len() == 1
                && recovered.function_revisions().len() == 1,
            "basic apply did not recover one schema, object, function, and immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_mutual_references_with_real_postgres_foreign_keys() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(MUTUAL_REFERENCE_SOURCE, &active)?;
        let applied = kernel.apply(&candidate).await?;

        let left = applied.catalogue().object_types()[0].id();
        let right = applied.catalogue().object_types()[1].id();
        let session = database.open().await?;
        let foreign_keys = session
            .client()
            .query(
                "SELECT conrelid::regclass::text, confrelid::regclass::text, confdeltype::text\n                 FROM pg_constraint\n                 WHERE contype = 'f'\n                   AND conrelid::regclass::text = ANY($1::text[])\n                 ORDER BY conrelid::regclass::text",
                &[&vec![relation(left), relation(right)]],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<(String, String, String)>, tokio_postgres::Error>>()?;
        session.shutdown().await?;
        require(
            foreign_keys
                == [
                    (relation(left), relation(right), "a".into()),
                    (relation(right), relation(left), "a".into()),
                ],
            "mutual REF apply did not install exact left/right NO ACTION foreign keys",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_only_edit_reuses_the_immutable_function_revision_and_artifact() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let first_revision = only_revision(&first)?.clone();
        let before = immutable_rows(&database, &first_revision).await?;
        let candidate = candidate(BASIC_SOURCE_ONLY_EDIT, &first)?;
        require(
            candidate.new_function_revisions().is_empty(),
            "source-only compiler preparation allocated an immutable function revision",
        )?;

        let applied = kernel.apply(&candidate).await?;
        let reused = only_revision(&applied)?;
        require_recovered_snapshot(&candidate, &applied)?;
        require(
            reused == &first_revision,
            "source-only apply changed the complete immutable function revision record",
        )?;
        let after = immutable_rows(&database, reused).await?;
        require(
            before == after,
            "source-only apply rewrote or added immutable function revision or artifact rows",
        )?;
        require(
            applied.historical_function_revisions().is_empty(),
            "source-only apply invented function revision history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn changed_function_history_is_retained_and_revert_reactivates_it() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let original = only_revision(&first)?.clone();
        let changed_candidate = candidate(BASIC_CHANGED_SOURCE, &first)?;
        let changed = kernel.apply(&changed_candidate).await?;
        let changed_revision = only_revision(&changed)?.clone();
        require(
            changed_revision.id() != original.id()
                && changed.historical_function_revisions() == [original.clone()],
            "changed function apply did not retain the previous immutable revision",
        )?;
        require_recovered_snapshot(&changed_candidate, &changed)?;
        require(
            changed.function_revisions() == changed_candidate.new_function_revisions(),
            "changed function apply did not activate its newly prepared immutable revision",
        )?;

        let revert_candidate = candidate(BASIC_SOURCE, &changed)?;
        require(
            revert_candidate.new_function_revisions().is_empty(),
            "revert preparation allocated a new immutable function revision",
        )?;
        let reverted = kernel.apply(&revert_candidate).await?;
        require_recovered_snapshot(&revert_candidate, &reverted)?;
        require(
            only_revision(&reverted)?.id() == original.id(),
            "revert did not reactivate the retained matching immutable revision",
        )?;
        require(
            reverted.historical_function_revisions() == [changed_revision],
            "revert did not retire the changed immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_apply_has_one_winner_and_no_loser_residue() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        let start = Arc::new(Barrier::new(3));
        let left_kernel = kernel.clone();
        let left_start = Arc::clone(&start);
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move {
            left_start.wait().await;
            left_kernel.apply(&left_for_task).await
        });
        let right_kernel = kernel.clone();
        let right_start = Arc::clone(&start);
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move {
            right_start.wait().await;
            right_kernel.apply(&right_for_task).await
        });
        start.wait().await;
        let left_result = left_task
            .await
            .map_err(|error| failure(format!("left apply task failed: {error}")))?;
        let right_result = right_task
            .await
            .map_err(|error| failure(format!("right apply task failed: {error}")))?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate).await
    })
    .await
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn candidate(source: &str, active: &ActiveDatabaseRevision) -> TestResult<DeployableRevision> {
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check(&bundle, active.catalogue());
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "compiler diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare(&report, active.pair(), active)?)
}

fn only_revision(active: &ActiveDatabaseRevision) -> TestResult<&FunctionRevisionRecord> {
    active
        .function_revisions()
        .first()
        .filter(|_| active.function_revisions().len() == 1)
        .ok_or_else(|| failure("expected exactly one active function revision"))
}

#[derive(Debug, Eq, PartialEq)]
struct ImmutableRevisionRows {
    revision_count: i64,
    revision_xmin: String,
    introduced_catalogue_revision_id: Vec<u8>,
    function_id: Vec<u8>,
    revision_number: i64,
    declaration_hash: Vec<u8>,
    semantic_hash: Vec<u8>,
    hash_algorithm: String,
    language_version: String,
    status: String,
    hash_contract_version: i16,
    artifact_count: i64,
    artifact_xmin: String,
    artifact_function_revision_id: Vec<u8>,
    artifact_kind: String,
    artifact_format: String,
    artifact_version: i32,
    artifact_payload: Vec<u8>,
    artifact_hash: Vec<u8>,
    artifact_hash_algorithm: String,
    artifact_hash_contract_version: i16,
}

async fn immutable_rows(
    database: &TestDatabase,
    revision: &FunctionRevisionRecord,
) -> TestResult<ImmutableRevisionRows> {
    let session = database.open().await?;
    let revision_id = revision.id().to_bytes().to_vec();
    let revision_count: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
            &[&revision_id],
        )
        .await?
        .try_get(0)?;
    let revision_row = session
        .client()
        .query_one(
            "SELECT xmin::text, introduced_catalogue_revision_id, function_id,
                    revision_number, content_hash, semantic_ir_hash, hash_algorithm,
                    language_version, status, hash_contract_version
             FROM _orna_kernel.function_revisions
             WHERE id = $1",
            &[&revision_id],
        )
        .await?;
    let artifact_count: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
            &[&revision_id],
        )
        .await?
        .try_get(0)?;
    let artifact_row = session
        .client()
        .query_one(
            "SELECT xmin::text, function_revision_id, artifact_kind, format, format_version,
                    payload, content_hash, hash_algorithm, hash_contract_version
             FROM _orna_kernel.function_artifacts
             WHERE function_revision_id = $1",
            &[&revision_id],
        )
        .await?;
    session.shutdown().await?;
    Ok(ImmutableRevisionRows {
        revision_count,
        revision_xmin: revision_row.try_get(0)?,
        introduced_catalogue_revision_id: revision_row.try_get(1)?,
        function_id: revision_row.try_get(2)?,
        revision_number: revision_row.try_get(3)?,
        declaration_hash: revision_row.try_get(4)?,
        semantic_hash: revision_row.try_get(5)?,
        hash_algorithm: revision_row.try_get(6)?,
        language_version: revision_row.try_get(7)?,
        status: revision_row.try_get(8)?,
        hash_contract_version: revision_row.try_get(9)?,
        artifact_count,
        artifact_xmin: artifact_row.try_get(0)?,
        artifact_function_revision_id: artifact_row.try_get(1)?,
        artifact_kind: artifact_row.try_get(2)?,
        artifact_format: artifact_row.try_get(3)?,
        artifact_version: artifact_row.try_get(4)?,
        artifact_payload: artifact_row.try_get(5)?,
        artifact_hash: artifact_row.try_get(6)?,
        artifact_hash_algorithm: artifact_row.try_get(7)?,
        artifact_hash_contract_version: artifact_row.try_get(8)?,
    })
}

async fn require_no_candidate_residue(
    database: &TestDatabase,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let source_bundle = candidate.source().bundle().to_bytes().to_vec();
    let source_revision = candidate.source().id().to_bytes().to_vec();
    let catalogue = candidate.candidate().revision().to_bytes().to_vec();
    let source_bundle_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_bundles WHERE id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_unit_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_units WHERE bundle_id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_revision_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&source_revision],
        )
        .await?
        .try_get(0)?;
    let catalogue_and_semantic_rows: i64 = session
        .client()
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions WHERE id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_schemas WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_object_types WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_expressions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_fields WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_functions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.definition_references WHERE catalogue_revision_id = $1)",
            &[&catalogue],
        )
        .await?
        .try_get(0)?;
    let mut immutable_rows = 0_i64;
    for revision in candidate.new_function_revisions() {
        let revision_id = revision.id().to_bytes().to_vec();
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
    }
    let names = candidate
        .candidate()
        .object_types()
        .iter()
        .map(|object| relation(object.id()))
        .collect::<Vec<_>>();
    let physical_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*)
             FROM unnest($1::text[]) AS expected(name)
             WHERE to_regclass(expected.name) IS NOT NULL",
            &[&names],
        )
        .await?
        .try_get(0)?;
    session.shutdown().await?;
    require(
        source_bundle_rows == 0
            && source_unit_rows == 0
            && source_revision_rows == 0
            && catalogue_and_semantic_rows == 0
            && immutable_rows == 0
            && physical_rows == 0,
        "losing apply left source, catalogue, semantic, immutable, artifact, or physical residue",
    )
}

fn require_recovered_new_candidate(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require_recovered_snapshot(candidate, active)?;
    require(
        active.function_revisions() == candidate.new_function_revisions(),
        "recovered candidate current function revisions differ",
    )?;
    require(
        active.historical_function_revisions().is_empty(),
        "new candidate apply unexpectedly recovered function history",
    )
}

fn require_recovered_snapshot(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require(
        active.pair() == candidate.candidate_pair(),
        "recovered candidate pair differs",
    )?;
    require(
        active.source() == candidate.source(),
        "recovered candidate source differs",
    )?;
    require(
        active.catalogue().revision() == candidate.candidate().revision()
            && active.catalogue().schemas() == candidate.candidate().schemas()
            && active.catalogue().object_types() == candidate.candidate().object_types()
            && active.catalogue().functions() == candidate.candidate().functions(),
        "recovered candidate catalogue differs",
    )?;
    require(
        active.catalogue_hash() == candidate.catalogue_hash(),
        "recovered candidate catalogue hash differs",
    )?;
    require(
        active.expressions() == candidate.expressions(),
        "recovered candidate expressions differ",
    )?;
    require(
        same_members(active.origins(), candidate.origins()),
        "recovered candidate origins differ",
    )?;
    require(
        active.references() == candidate.references(),
        "recovered candidate references differ",
    )?;
    Ok(())
}

fn same_members<T>(left: &[T], right: &[T]) -> bool
where
    T: Eq,
{
    left.len() == right.len() && left.iter().all(|member| right.contains(member))
}

fn relation(type_id: TypeId) -> String {
    format!(
        "_orna_data.t_{:032x}",
        u128::from_be_bytes(type_id.to_bytes())
    )
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
