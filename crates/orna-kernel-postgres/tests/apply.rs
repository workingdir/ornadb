mod support;

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use orna_compiler::{check, prepare};
use orna_core::{
    TypeId,
    catalogue::FunctionReturn,
    revision::{ActiveDatabaseRevision, DeployableRevision, FunctionRevisionRecord},
    source::{SourceBundle, SourceUnit},
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};

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

const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
const RACE_LOCK_KEY: i64 = 0x4f52_4e41_4150_504c;

#[derive(Clone, Copy)]
enum FailurePoint {
    SourceBundle,
    CatalogueSchema,
    FunctionArtifact,
    DefinitionReference,
    DeferredReference,
    StatusSweep,
    ActivePointer,
    PostPointerRecovery,
}

impl FailurePoint {
    const ALL: [Self; 8] = [
        Self::SourceBundle,
        Self::CatalogueSchema,
        Self::FunctionArtifact,
        Self::DefinitionReference,
        Self::DeferredReference,
        Self::StatusSweep,
        Self::ActivePointer,
        Self::PostPointerRecovery,
    ];
}

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
            same_members(
                &foreign_keys,
                &[
                    (relation(left), relation(right), "a".into()),
                    (relation(right), relation(left), "a".into()),
                ],
            ),
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
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator.client().query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY]).await?;
        let left_kernel = named_kernel(&database, "orna-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move { left_kernel.apply(&left_for_task).await });
        wait_for_advisory_wait(&database, "orna-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move { right_kernel.apply(&right_for_task).await });
        wait_for_active_lock_block(&database, "orna-apply-race-a", "orna-apply-race-b").await?;
        coordinator.client().query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY]).await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left").await?;
        let right_result = wait_for_apply_task(right_task, "right").await?;
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
        require_no_candidate_residue(&database, loser_candidate, &empty).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn every_apply_failure_point_rolls_back_to_the_exact_base() -> TestResult<()> {
    for point in FailurePoint::ALL {
        with_test_database(|database| async move {
            let kernel = kernel(&database)?;
            kernel.bootstrap().await?;
            let initial = kernel.recover().await?;
            let (base, candidate) = if matches!(point, FailurePoint::StatusSweep) {
                let committed = kernel.apply(&candidate(BASIC_SOURCE, &initial)?).await?;
                let changed = candidate(BASIC_CHANGED_SOURCE, &committed)?;
                (committed, changed)
            } else {
                let candidate = candidate(BASIC_SOURCE, &initial)?;
                (initial, candidate)
            };
            if matches!(
                point,
                FailurePoint::DefinitionReference | FailurePoint::DeferredReference
            ) {
                require(
                    !candidate.references().is_empty(),
                    "reference trigger fixture must contain references",
                )?;
            }
            let baseline = baseline(&database, &base).await?;
            install_failure_point(&database, point, &candidate).await?;

            let error = kernel
                .apply(&candidate)
                .await
                .expect_err("triggered apply must fail");
            assert_failure_shape(point, &error)?;
            require_baseline(&database, &baseline, &kernel).await?;
            require_no_candidate_residue(&database, &candidate, &base).await
        })
        .await?;
    }
    Ok(())
}

async fn install_race_pause_trigger(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    session
        .client()
        .execute("SELECT pg_advisory_unlock_all()", &[])
        .await?;
    session.client().batch_execute(
        "CREATE FUNCTION _orna_kernel.test_apply_pause_pointer() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(5715716919262203980); RETURN NEW; END $$;
         CREATE TRIGGER pause_active_pointer BEFORE UPDATE ON _orna_kernel.active_revision
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_pause_pointer();",
    ).await?;
    session.shutdown().await
}

async fn wait_for_advisory_wait(database: &TestDatabase, application: &str) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let waiting: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                WHERE application_name = $1
                  AND wait_event_type = 'Lock'
                  AND wait_event = 'advisory'
             )",
                &[&application],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if waiting {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "timed out waiting for {application} to block on the advisory lock"
            )));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_active_lock_block(
    database: &TestDatabase,
    holder: &str,
    waiter: &str,
) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let blocked: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1
                FROM pg_stat_activity AS holder
                JOIN pg_stat_activity AS waiter ON holder.pid = ANY(pg_blocking_pids(waiter.pid))
                WHERE holder.application_name = $1
                  AND waiter.application_name = $2
                  AND waiter.wait_event_type = 'Lock'
             )",
                &[&holder, &waiter],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if blocked {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "timed out waiting for B to block on A's active revision lock",
            ));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_apply_task(
    task: tokio::task::JoinHandle<Result<ActiveDatabaseRevision, PostgresKernelError>>,
    name: &'static str,
) -> TestResult<Result<ActiveDatabaseRevision, PostgresKernelError>> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    while !task.is_finished() {
        if Instant::now() >= deadline {
            task.abort();
            return Err(failure(format!("timed out waiting for {name} apply task")));
        }
        tokio::task::yield_now().await;
    }
    task.await
        .map_err(|error| failure(format!("{name} apply task failed: {error}")))
}

#[derive(Clone, Debug)]
struct Baseline {
    active_source: Vec<u8>,
    active_catalogue: Vec<u8>,
    active_updated_at: String,
    counts: Vec<i64>,
    statuses: Vec<(Vec<u8>, String)>,
    recovered: ActiveDatabaseRevision,
}

async fn baseline(
    database: &TestDatabase,
    recovered: &ActiveDatabaseRevision,
) -> TestResult<Baseline> {
    let session = database.open().await?;
    let active = session
        .client()
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id, updated_at::text
         FROM _orna_kernel.active_revision WHERE singleton = true",
            &[],
        )
        .await?;
    let counts = session
        .client()
        .query_one(
            "SELECT
          (SELECT count(*) FROM _orna_kernel.schema_migrations),
          (SELECT count(*) FROM _orna_kernel.source_bundles),
          (SELECT count(*) FROM _orna_kernel.source_units),
          (SELECT count(*) FROM _orna_kernel.source_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_schemas),
          (SELECT count(*) FROM _orna_kernel.catalogue_object_types),
          (SELECT count(*) FROM _orna_kernel.catalogue_expressions),
          (SELECT count(*) FROM _orna_kernel.catalogue_fields),
          (SELECT count(*) FROM _orna_kernel.catalogue_functions),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns),
          (SELECT count(*) FROM _orna_kernel.function_revisions),
          (SELECT count(*) FROM _orna_kernel.function_artifacts),
          (SELECT count(*) FROM _orna_kernel.active_revision),
          (SELECT count(*) FROM _orna_kernel.definition_references)",
            &[],
        )
        .await?;
    let statuses = session
        .client()
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
        .collect::<Result<Vec<(Vec<u8>, String)>, tokio_postgres::Error>>()?;
    let counts = (0..16)
        .map(|index| counts.try_get(index))
        .collect::<Result<Vec<i64>, _>>()?;
    let result = Baseline {
        active_source: active.try_get(0)?,
        active_catalogue: active.try_get(1)?,
        active_updated_at: active.try_get(2)?,
        counts,
        statuses,
        recovered: recovered.clone(),
    };
    session.shutdown().await?;
    Ok(result)
}

async fn require_baseline(
    database: &TestDatabase,
    expected: &Baseline,
    kernel: &PostgresKernel,
) -> TestResult<()> {
    let actual = baseline(database, &kernel.recover().await?).await?;
    require(
        actual.active_source == expected.active_source
            && actual.active_catalogue == expected.active_catalogue
            && actual.active_updated_at == expected.active_updated_at
            && actual.counts == expected.counts
            && actual.statuses == expected.statuses
            && same_recovered(&actual.recovered, &expected.recovered),
        "failed apply changed the exact durable base baseline",
    )
}

fn same_recovered(left: &ActiveDatabaseRevision, right: &ActiveDatabaseRevision) -> bool {
    left.pair() == right.pair()
        && left.source() == right.source()
        && left.catalogue().revision() == right.catalogue().revision()
        && left.catalogue().schemas() == right.catalogue().schemas()
        && left.catalogue().object_types() == right.catalogue().object_types()
        && left.catalogue().functions() == right.catalogue().functions()
        && left.catalogue_hash() == right.catalogue_hash()
        && left.expressions() == right.expressions()
        && same_members(left.origins(), right.origins())
        && left.references() == right.references()
        && left.function_revisions() == right.function_revisions()
        && left.historical_function_revisions() == right.historical_function_revisions()
}

async fn install_failure_point(
    database: &TestDatabase,
    point: FailurePoint,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let statement = match point {
        FailurePoint::SourceBundle => source_bundle_failure_trigger(candidate)?,
        FailurePoint::CatalogueSchema => catalogue_schema_failure_trigger(candidate),
        FailurePoint::FunctionArtifact => function_artifact_failure_trigger(candidate),
        FailurePoint::DefinitionReference => definition_reference_failure_trigger(candidate),
        FailurePoint::DeferredReference => "
            CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred definition reference' USING ERRCODE = 'P0001'; END $$;
            CREATE CONSTRAINT TRIGGER deferred_definition_reference
            AFTER INSERT ON _orna_kernel.definition_references
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();
            CREATE FUNCTION _orna_kernel.test_status_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached status transition' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_status_sentinel BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            EXECUTE FUNCTION _orna_kernel.test_status_sentinel();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::StatusSweep => "
            CREATE FUNCTION _orna_kernel.test_apply_status_invalid() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN NEW.status := 'invalid'; RETURN NEW; END $$;
            CREATE TRIGGER rewrite_active_status BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            WHEN (NEW.status = 'active') EXECUTE FUNCTION _orna_kernel.test_apply_status_invalid();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'status sweep reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER status_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::ActivePointer => fail_trigger("active_revision", "before_active_pointer", "BEFORE", "UPDATE", "before active pointer"),
        FailurePoint::PostPointerRecovery => "
            CREATE FUNCTION _orna_kernel.test_apply_tamper_source() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              UPDATE _orna_kernel.source_revisions
              SET content_hash = decode(repeat('00', 32), 'hex')
              WHERE id = NEW.source_revision_id;
              RETURN NEW;
            END $$;
            CREATE TRIGGER tamper_after_active_pointer AFTER UPDATE
            ON _orna_kernel.active_revision FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_tamper_source();".into(),
    };
    session.client().batch_execute(&statement).await?;
    session.shutdown().await
}

fn source_bundle_failure_trigger(candidate: &DeployableRevision) -> TestResult<String> {
    let object = candidate
        .candidate()
        .object_types()
        .first()
        .ok_or_else(|| failure("source-bundle rollback fixture requires an object type"))?;
    let expected_relation = relation(object.id());
    Ok(prerequisite_trigger(
        "source_bundles",
        "before_source_bundle",
        format!("pg_catalog.to_regclass('{expected_relation}') IS NOT NULL"),
        "physical plan missing before source bundle",
        "before source bundle",
    ))
}

fn catalogue_schema_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let source_complete = state.source_complete_condition();
    let catalogue_present = state.catalogue_revision_present_condition();
    prerequisite_trigger(
        "catalogue_schemas",
        "before_catalogue_schema",
        format!("{source_complete} AND {catalogue_present}"),
        "source or catalogue state missing before catalogue schema",
        "before catalogue schema",
    )
}

fn function_artifact_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let semantics_complete = state.semantics_complete_condition();
    let revisions_complete = state.function_revisions_complete_condition();
    prerequisite_trigger(
        "function_artifacts",
        "before_function_artifact",
        format!("{semantics_complete} AND {revisions_complete}"),
        "candidate semantics or revision missing before function artifact",
        "before function artifact",
    )
}

fn definition_reference_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let artifacts_complete = state.artifacts_complete_condition();
    prerequisite_trigger(
        "definition_references",
        "before_definition_reference",
        artifacts_complete,
        "candidate artifact missing before definition reference",
        "before definition reference",
    )
}

fn prerequisite_trigger(
    table: &str,
    trigger: &str,
    prerequisite: String,
    missing_marker: &str,
    expected_marker: &str,
) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT ({prerequisite}) THEN
             RAISE EXCEPTION '{missing_marker}' USING ERRCODE = 'P0001';
           END IF;
           RAISE EXCEPTION '{expected_marker}' USING ERRCODE = 'P0001';
         END $$;
         CREATE TRIGGER {trigger} BEFORE INSERT ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

#[derive(Debug)]
struct CandidateSqlState {
    source_bundle_id: String,
    source_revision_id: String,
    catalogue_revision_id: String,
    source_unit_count: usize,
    schema_count: usize,
    object_type_count: usize,
    expression_count: usize,
    field_count: usize,
    function_count: usize,
    parameter_count: usize,
    return_column_count: usize,
    new_function_revision_ids: Vec<String>,
}

impl CandidateSqlState {
    fn from_candidate(candidate: &DeployableRevision) -> Self {
        let catalogue = candidate.candidate();
        let functions = catalogue.functions();
        Self {
            source_bundle_id: hex_bytes(candidate.source().bundle().to_bytes()),
            source_revision_id: hex_bytes(candidate.source().id().to_bytes()),
            catalogue_revision_id: hex_bytes(catalogue.revision().to_bytes()),
            source_unit_count: candidate.source().units().len(),
            schema_count: catalogue.schemas().len(),
            object_type_count: catalogue.object_types().len(),
            expression_count: candidate.expressions().len(),
            field_count: catalogue
                .object_types()
                .iter()
                .map(|object| object.fields().len())
                .sum(),
            function_count: functions.len(),
            parameter_count: functions
                .iter()
                .map(|function| function.parameters().len())
                .sum(),
            return_column_count: functions
                .iter()
                .map(|function| match function.return_type() {
                    FunctionReturn::Single(_) => 0,
                    FunctionReturn::Rows(columns) => columns.len(),
                })
                .sum(),
            new_function_revision_ids: candidate
                .new_function_revisions()
                .iter()
                .map(|revision| hex_bytes(revision.id().to_bytes()))
                .collect(),
        }
    }

    fn source_complete_condition(&self) -> String {
        let source_bundle = self.source_bundle();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.source_bundles
                      WHERE id = {source_bundle})
             AND (SELECT count(*) FROM _orna_kernel.source_units
                  WHERE bundle_id = {source_bundle}) = {source_unit_count}
             AND EXISTS (SELECT 1 FROM _orna_kernel.source_revisions
                         WHERE id = {source_revision} AND bundle_id = {source_bundle})",
            source_unit_count = self.source_unit_count,
        )
    }

    fn catalogue_revision_present_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.catalogue_revisions
                      WHERE id = {catalogue_revision}
                        AND source_revision_id = {source_revision})"
        )
    }

    fn semantics_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        format!(
            "{catalogue_present}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_schemas
                  WHERE catalogue_revision_id = {catalogue_revision}) = {schema_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_object_types
                  WHERE catalogue_revision_id = {catalogue_revision}) = {object_type_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_expressions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {expression_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_fields
                  WHERE catalogue_revision_id = {catalogue_revision}) = {field_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_functions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {function_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters
                  WHERE catalogue_revision_id = {catalogue_revision}) = {parameter_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns
                  WHERE catalogue_revision_id = {catalogue_revision}) = {return_column_count}",
            catalogue_present = self.catalogue_revision_present_condition(),
            schema_count = self.schema_count,
            object_type_count = self.object_type_count,
            expression_count = self.expression_count,
            field_count = self.field_count,
            function_count = self.function_count,
            parameter_count = self.parameter_count,
            return_column_count = self.return_column_count,
        )
    }

    fn function_revisions_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1 FROM _orna_kernel.function_revisions
                          WHERE id = {} AND introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn artifacts_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1
                          FROM _orna_kernel.function_artifacts AS artifact
                          JOIN _orna_kernel.function_revisions AS revision
                            ON revision.id = artifact.function_revision_id
                          WHERE revision.id = {}
                            AND revision.introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn source_bundle(&self) -> String {
        bytea(&self.source_bundle_id)
    }

    fn source_revision(&self) -> String {
        bytea(&self.source_revision_id)
    }

    fn catalogue_revision(&self) -> String {
        bytea(&self.catalogue_revision_id)
    }
}

fn conjunction(conditions: impl IntoIterator<Item = String>) -> String {
    let conditions = conditions.into_iter().collect::<Vec<_>>();
    if conditions.is_empty() {
        "TRUE".into()
    } else {
        conditions.join(" AND ")
    }
}

fn bytea(hex: &str) -> String {
    format!("decode('{hex}', 'hex')")
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fail_trigger(table: &str, trigger: &str, timing: &str, event: &str, marker: &str) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION '{marker}' USING ERRCODE = 'P0001'; END $$;
         CREATE TRIGGER {trigger} {timing} {event} ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

fn assert_failure_shape(point: FailurePoint, error: &PostgresKernelError) -> TestResult<()> {
    match point {
        FailurePoint::StatusSweep => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.function_revisions",
                    ..
                }
            ),
            "status rewrite must fail the global status sweep with a function revision invariant",
        ),
        FailurePoint::PostPointerRecovery => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.source_revisions",
                    ..
                }
            ),
            "post-pointer source tampering must fail recovery with a source invariant",
        ),
        _ => match error {
            PostgresKernelError::Database(source) => {
                let marker = source
                    .as_db_error()
                    .map(|detail| detail.message())
                    .unwrap_or("");
                require(
                    source.code().is_some_and(|code| code.code() == "P0001")
                        && marker == trigger_marker(point).expect("trigger point has a marker"),
                    "trigger error did not preserve SQLSTATE P0001 and its exact marker",
                )
            }
            _ => Err(failure(format!(
                "expected PostgreSQL P0001 trigger failure, got {error}"
            ))),
        },
    }
}

fn trigger_marker(point: FailurePoint) -> Option<&'static str> {
    match point {
        FailurePoint::SourceBundle => Some("before source bundle"),
        FailurePoint::CatalogueSchema => Some("before catalogue schema"),
        FailurePoint::FunctionArtifact => Some("before function artifact"),
        FailurePoint::DefinitionReference => Some("before definition reference"),
        FailurePoint::DeferredReference => Some("deferred definition reference"),
        FailurePoint::ActivePointer => Some("before active pointer"),
        FailurePoint::StatusSweep | FailurePoint::PostPointerRecovery => None,
    }
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn named_kernel(database: &TestDatabase, application_name: &str) -> TestResult<PostgresKernel> {
    let mut config = database.config()?;
    config.application_name(application_name);
    Ok(PostgresKernel::new(config))
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
    base: &ActiveDatabaseRevision,
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
        .filter(|object| base.catalogue().object_type_by_id(object.id()).is_none())
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
