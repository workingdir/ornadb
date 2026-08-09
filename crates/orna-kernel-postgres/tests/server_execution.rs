//! Live PostgreSQL tests for the bounded active SERVER SELECT entry point.

mod support;

use std::str::FromStr;

#[cfg(feature = "test-hooks")]
use std::{future::Future, time::Duration};

use orna_compiler::{check, prepare};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
    value::{FunctionArgument, RuntimeFloat, RuntimeValue},
};
use orna_kernel_postgres::{
    PostgresKernel, PostgresKernelError, ServerSelectError, ServerSelectResult,
};
use support::{TestDatabase, TestResult, TestSession, failure, with_test_database};

const EXECUTION_SOURCE: &str = r"CREATE SCHEMA exec;
    CREATE TYPE exec.node AS OBJECT (
      child REF exec.node, active BOOL NOT NULL, value INT NOT NULL,
      amount BIGINT NOT NULL, score FLOAT NOT NULL, label TEXT NOT NULL,
      blob BYTES NOT NULL
    );
    CREATE TYPE exec.other AS OBJECT ();
    CREATE SERVER FUNCTION exec.read()
    RETURNS ROWS (root REF exec.node, active BOOL, value INT, amount BIGINT, score FLOAT, label TEXT, blob BYTES, child_label TEXT)
    AS SELECT REF(n), n.active, n.value, n.amount, n.score, n.label, n.blob, n.child.label
    FROM exec.node n WHERE n.active = TRUE ORDER BY n.value DESC;
    CREATE SERVER FUNCTION exec.none() RETURNS ROWS (value INT)
    AS SELECT n.value FROM exec.node n WHERE n.active = FALSE ORDER BY n.value;
    CREATE SERVER FUNCTION exec.select_node(p_node REF exec.node)
    RETURNS ROWS (selected REF exec.node, value INT, child_label TEXT, same_as_child BOOL)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT REF(selected), selected.value, selected.child.label,
      REF(selected) = selected.child
    FROM exec.node selected WHERE REF(selected) = p_node;
    CREATE SERVER FUNCTION exec.unique_values()
    RETURNS ROWS (active BOOL, value INT, amount BIGINT, blob BYTES, child REF exec.node)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT DISTINCT n.active, n.value, n.amount, n.blob, n.child FROM exec.node n;
    CREATE SERVER FUNCTION exec.all_values()
    RETURNS ROWS (active BOOL, value INT, amount BIGINT, blob BYTES, child REF exec.node)
    AS SELECT n.active, n.value, n.amount, n.blob, n.child FROM exec.node n;
";

#[cfg(feature = "test-hooks")]
const EXECUTION_SOURCE_EDIT: &str = r"-- source-only active edit
    CREATE SCHEMA exec;
    CREATE TYPE exec.node AS OBJECT ( child REF exec.node, active BOOL NOT NULL,
      value INT NOT NULL, amount BIGINT NOT NULL, score FLOAT NOT NULL,
      label TEXT NOT NULL, blob BYTES NOT NULL );
    CREATE TYPE exec.other AS OBJECT ();
    CREATE SERVER FUNCTION exec.read() RETURNS ROWS (root REF exec.node, active BOOL,
      value INT, amount BIGINT, score FLOAT, label TEXT, blob BYTES, child_label TEXT)
    AS SELECT REF(n), n.active, n.value, n.amount, n.score, n.label, n.blob, n.child.label
    FROM exec.node n WHERE n.active = TRUE ORDER BY n.value DESC;
    CREATE SERVER FUNCTION exec.none() RETURNS ROWS (value INT)
    AS SELECT n.value FROM exec.node n WHERE n.active = FALSE ORDER BY n.value;
    CREATE SERVER FUNCTION exec.select_node(p_node REF exec.node)
    RETURNS ROWS (selected REF exec.node, value INT, child_label TEXT, same_as_child BOOL)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT REF(selected), selected.value, selected.child.label,
      REF(selected) = selected.child
    FROM exec.node selected WHERE REF(selected) = p_node;
    CREATE SERVER FUNCTION exec.unique_values()
    RETURNS ROWS (active BOOL, value INT, amount BIGINT, blob BYTES, child REF exec.node)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT DISTINCT n.active, n.value, n.amount, n.blob, n.child FROM exec.node n;
    CREATE SERVER FUNCTION exec.all_values()
    RETURNS ROWS (active BOOL, value INT, amount BIGINT, blob BYTES, child REF exec.node)
    AS SELECT n.active, n.value, n.amount, n.blob, n.child FROM exec.node n;
";

const MANY_SOURCE: &str = "CREATE SCHEMA many;\n\
    CREATE TYPE many.row AS OBJECT (value INT NOT NULL);\n\
    CREATE SERVER FUNCTION many.all_rows() RETURNS ROWS (value INT)\n\
    AS SELECT r.value FROM many.row r ORDER BY r.value;\n";

#[cfg(feature = "test-hooks")]
const WAIT: Duration = Duration::from_secs(5);
#[cfg(feature = "test-hooks")]
const ARGUMENT_REJECTION_WAIT: Duration = Duration::from_secs(2);
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const VARIABLE_PAYLOAD_MAXIMUM: usize = 5_592_377;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn executes_the_active_server_select_subset_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;

        let result = kernel.execute_server_select(fixture.read).await?;
        require_result_identity(&result, applied.pair(), fixture.read, fixture.read_revision)?;
        require_exact_columns(&result, fixture)?;
        require_exact_rows(&result, fixture, 20)?;

        let empty = kernel.execute_server_select(fixture.none).await?;
        require_result_identity(&empty, applied.pair(), fixture.none, fixture.none_revision)?;
        require(
            empty.rows().rows().is_empty(),
            "zero-match function returned a row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn identity_selected_server_select_returns_exact_zero_or_one_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        install_public_execution_decoy(&database, fixture).await?;

        let selected = kernel
            .execute_server_select_with_arguments(
                fixture.select_node,
                &selector_argument(fixture, fixture.root)?,
            )
            .await?;
        require_result_identity(
            &selected,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        require_identity_selected_columns(&selected, fixture)?;
        require_identity_selected_root_row(&selected, fixture, 20)?;

        let absent = ObjectId::from_bytes([0x61; 16]);
        let empty = kernel
            .execute_server_select_with_arguments(
                fixture.select_node,
                &selector_argument(fixture, absent)?,
            )
            .await?;
        require_result_identity(
            &empty,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        require_identity_selected_columns(&empty, fixture)?;
        require(
            empty.rows().rows().is_empty(),
            "absent selector returned a row",
        )?;

        let v1 = kernel.execute_server_select(fixture.read).await?;
        require_result_identity(&v1, applied.pair(), fixture.read, fixture.read_revision)?;
        require_exact_columns(&v1, fixture)?;
        require_exact_rows(&v1, fixture, 20)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn distinct_server_select_returns_unique_typed_rows_and_preserves_v1_v2() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        insert_distinct_duplicate_rows(&database, fixture).await?;
        install_public_execution_decoy(&database, fixture).await?;

        let distinct = kernel.execute_server_select(fixture.unique_values).await?;
        require_result_identity(
            &distinct,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&distinct, fixture)?;
        require_distinct_rows(&distinct, fixture, 20)?;

        let preserving = kernel.execute_server_select(fixture.all_values).await?;
        require_result_identity(
            &preserving,
            applied.pair(),
            fixture.all_values,
            fixture.all_values_revision,
        )?;
        require_distinct_columns(&preserving, fixture)?;
        require_version_one_value_multiset(&preserving, fixture, 20)?;

        let selected = kernel
            .execute_server_select_with_arguments(
                fixture.select_node,
                &selector_argument(fixture, fixture.root)?,
            )
            .await?;
        require_result_identity(
            &selected,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        require_identity_selected_columns(&selected, fixture)?;
        require_identity_selected_root_row(&selected, fixture, 20)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn distinct_server_select_deduplicates_before_the_result_limit_and_rejects_arguments()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        insert_distinct_limit_rows(&database, fixture).await?;
        install_public_execution_decoy(&database, fixture).await?;

        let distinct = kernel.execute_server_select(fixture.unique_values).await?;
        require_result_identity(
            &distinct,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&distinct, fixture)?;
        require_distinct_limit_rows(&distinct, fixture, 20)?;

        let before_rows = count_rows(&database, fixture.node).await?;
        require(
            before_rows > 10_000,
            "SELECT DISTINCT limit fixture did not create more than 10,000 physical rows",
        )?;
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0x71; 16]),
            RuntimeValue::Boolean(true),
        )?;
        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let mut execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_server_select_with_arguments_and_test_barrier(
                    fixture.unique_values,
                    &[argument],
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        if tokio::time::timeout(WAIT, reached.wait()).await.is_err() {
            execution.abort_and_wait().await;
            return Err(failure(
                "SELECT DISTINCT argument validation did not recover before the target lock",
            ));
        }
        let holder = match lock_target_relation(&database, fixture.node).await {
            Ok(holder) => holder,
            Err(error) => {
                execution.abort_and_wait().await;
                return Err(error);
            }
        };
        if tokio::time::timeout(ARGUMENT_REJECTION_WAIT, resume.wait())
            .await
            .is_err()
        {
            execution.abort_and_wait().await;
            return match rollback_and_finish_session(
                holder,
                Err(failure(
                    "SELECT DISTINCT argument validation did not resume under the target lock",
                )),
                "SELECT DISTINCT argument-lock holder",
            )
            .await
            {
                Ok(()) => Err(failure(
                    "SELECT DISTINCT argument-lock cleanup lost its resume failure",
                )),
                Err(error) => Err(error),
            };
        }
        let operation = match execution
            .finish_with_timeout(
                "SELECT DISTINCT argument validation",
                ARGUMENT_REJECTION_WAIT,
            )
            .await
        {
            Ok(result) => {
                expect_kernel_error(result, "SELECT DISTINCT accepted an unexpected argument")
            }
            Err(error) => Err(error),
        };
        let error =
            rollback_and_finish_session(holder, operation, "SELECT DISTINCT argument-lock holder")
                .await?;
        require_select_argument_error(
            &error,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
            None,
            "this function does not accept arguments",
        )?;
        require(
            count_rows(&database, fixture.node).await? == before_rows,
            "argument rejection changed SELECT DISTINCT physical rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "argument rejection changed the SELECT DISTINCT active pair",
        )?;

        let unchanged = kernel.execute_server_select(fixture.unique_values).await?;
        require_result_identity(
            &unchanged,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&unchanged, fixture)?;
        require_distinct_limit_rows(&unchanged, fixture, 20)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn identity_selected_arguments_fail_contextually_without_changing_state() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;

        let missing = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(fixture.select_node, &[])
                .await,
            "missing selector argument unexpectedly succeeded",
        )?;
        require_select_argument_error(
            &missing,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
            Some(fixture.select_node_parameter),
            "a required argument is missing",
        )?;

        let duplicate_argument = selector_argument(fixture, fixture.root)?;
        let duplicate = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(
                    fixture.select_node,
                    &[duplicate_argument[0].clone(), duplicate_argument[0].clone()],
                )
                .await,
            "duplicate selector argument unexpectedly succeeded",
        )?;
        require_select_argument_error(
            &duplicate,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
            Some(fixture.select_node_parameter),
            "the same parameter was supplied twice",
        )?;

        let unknown_parameter = ParameterId::from_bytes([0x62; 16]);
        let unknown = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(
                    fixture.select_node,
                    &[FunctionArgument::new(
                        unknown_parameter,
                        RuntimeValue::Reference {
                            target: fixture.node,
                            object: fixture.root,
                        },
                    )?],
                )
                .await,
            "unknown selector parameter unexpectedly succeeded",
        )?;
        require_select_argument_error(
            &unknown,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
            Some(unknown_parameter),
            "an argument was supplied for a parameter that this function does not declare",
        )?;

        let scalar = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(
                    fixture.select_node,
                    &[FunctionArgument::new(
                        fixture.select_node_parameter,
                        RuntimeValue::Integer(1),
                    )?],
                )
                .await,
            "scalar selector argument unexpectedly succeeded",
        )?;
        require_select_argument_error(
            &scalar,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
            Some(fixture.select_node_parameter),
            "the argument type does not match the declared parameter type",
        )?;

        let wrong_target = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(
                    fixture.select_node,
                    &[FunctionArgument::new(
                        fixture.select_node_parameter,
                        RuntimeValue::Reference {
                            target: fixture.other_type,
                            object: fixture.root,
                        },
                    )?],
                )
                .await,
            "wrong active REF selector target unexpectedly succeeded",
        )?;
        require_select_argument_error(
            &wrong_target,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
            Some(fixture.select_node_parameter),
            "the argument type does not match the declared parameter type",
        )?;

        let unchanged = kernel.execute_server_select(fixture.read).await?;
        require_result_identity(
            &unchanged,
            applied.pair(),
            fixture.read,
            fixture.read_revision,
        )?;
        require_exact_columns(&unchanged, fixture)?;
        require_exact_rows(&unchanged, fixture, 20)?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "argument rejection changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn trusted_variable_payload_guard_rejects_before_client_decode() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        install_hostile_octet_length_shadow(&database).await?;

        let oversize = "x".repeat(PAYLOAD_LIMIT / 2 + 1);
        let session = database.open().await?;
        let operation: TestResult<u64> = async {
            Ok(session
                .client()
                .execute(
                    &format!(
                        "UPDATE {} SET {} = $2 WHERE _orna_object_id = $1",
                        relation(fixture.node),
                        field(fixture.label)
                    ),
                    &[&fixture.root.to_bytes().to_vec(), &oversize],
                )
                .await?)
        }
        .await;
        let updated = finish_session(session, operation, "oversize fixture update").await?;
        require(
            updated == 1,
            "oversize fixture update changed the wrong row count",
        )?;

        let before_rows = count_rows(&database, fixture.node).await?;
        let error = expect_kernel_error(
            kernel.execute_server_select(fixture.read).await,
            "oversized TEXT unexpectedly entered RuntimeValue",
        )?;
        require_variable_payload_error(
            &error,
            applied.pair(),
            fixture.read,
            fixture.read_revision,
        )?;
        require(
            count_rows(&database, fixture.node).await? == before_rows,
            "failed execution changed physical rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "failed execution changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn row_limit_is_contextual_and_does_not_mutate_state() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MANY_SOURCE, &active)?).await?;
        let object = applied.catalogue().object_types()[0].id();
        let function = applied.catalogue().functions()[0].id();
        let revision = applied.catalogue().functions()[0].current_revision();
        let value = applied.catalogue().object_types()[0].fields()[0].id();

        let session = database.open().await?;
        let operation: TestResult<()> = async {
            session
                .client()
                .batch_execute(&format!(
                    "INSERT INTO {} (_orna_object_id, {}) \
                 SELECT decode(lpad(to_hex(value), 32, '0'), 'hex'), value \
                 FROM generate_series(1, 10000) AS value",
                    relation(object),
                    field(value),
                ))
                .await?;
            Ok(())
        }
        .await;
        finish_session(session, operation, "row-limit boundary insert").await?;

        let accepted = kernel.execute_server_select(function).await?;
        require_result_identity(&accepted, applied.pair(), function, revision)?;
        require(
            accepted.rows().rows().len() == 10_000,
            "the exact 10,000-row boundary was not accepted",
        )?;
        require(
            accepted.rows().rows()[0].values() == [RuntimeValue::Integer(1)],
            "the accepted boundary lost its first ordered row",
        )?;
        require(
            accepted.rows().rows()[9_999].values() == [RuntimeValue::Integer(10_000)],
            "the accepted boundary lost its final ordered row",
        )?;

        let session = database.open().await?;
        let operation: TestResult<()> = async {
            session
                .client()
                .batch_execute(&format!(
                    "INSERT INTO {} (_orna_object_id, {}) \
                 VALUES (decode(lpad(to_hex(10001), 32, '0'), 'hex'), 10001)",
                    relation(object),
                    field(value),
                ))
                .await?;
            Ok(())
        }
        .await;
        finish_session(session, operation, "row-limit overflow insert").await?;

        let error = expect_kernel_error(
            kernel.execute_server_select(function).await,
            "10,001 rows unexpectedly passed the fixed bound",
        )?;
        require_row_limit_error(&error, applied.pair(), function, revision)?;
        require(
            count_rows(&database, object).await? == 10_001,
            "row-limit execution changed physical rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "row-limit execution changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn execution_pins_one_snapshot_while_source_only_apply_advances() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(EXECUTION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        insert_execution_rows(&database, fixture).await?;
        let source_only_candidate = candidate(EXECUTION_SOURCE_EDIT, &first)?;

        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_server_select_with_test_barrier(
                    fixture.read,
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        let (running, second) = complete_pinned_execution(
            execution,
            reached,
            resume,
            "version-1 SERVER SELECT",
            async {
                update_root_value(&database, fixture, 21).await?;
                kernel
                    .apply(&source_only_candidate)
                    .await
                    .map_err(Into::into)
            },
        )
        .await?;

        require(
            second.pair() != first.pair(),
            "source-only apply did not advance the pair",
        )?;
        require(
            current_revision(&second, fixture.read)? == fixture.read_revision,
            "source-only apply did not reuse the immutable function revision",
        )?;
        require_result_identity(&running, first.pair(), fixture.read, fixture.read_revision)?;
        require_exact_columns(&running, fixture)?;
        require_exact_rows(&running, fixture, 20)?;

        let later = kernel.execute_server_select(fixture.read).await?;
        require_result_identity(&later, second.pair(), fixture.read, fixture.read_revision)?;
        require_exact_columns(&later, fixture)?;
        require_exact_rows(&later, fixture, 21)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn identity_selected_execution_pins_active_revision_and_data_snapshot() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(EXECUTION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        insert_execution_rows(&database, fixture).await?;
        let source_only_candidate = candidate(EXECUTION_SOURCE_EDIT, &first)?;

        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let arguments = selector_argument(fixture, fixture.root)?;
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_server_select_with_arguments_and_test_barrier(
                    fixture.select_node,
                    &arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        let (running, second) = complete_pinned_execution(
            execution,
            reached,
            resume,
            "identity-selected SERVER SELECT",
            async {
                update_root_value(&database, fixture, 21).await?;
                kernel
                    .apply(&source_only_candidate)
                    .await
                    .map_err(Into::into)
            },
        )
        .await?;

        require(
            second.pair() != first.pair(),
            "source-only apply did not advance the identity-selected pair",
        )?;
        require(
            current_revision(&second, fixture.select_node)? == fixture.select_node_revision,
            "source-only apply did not retain the immutable identity-selected revision",
        )?;
        require_result_identity(
            &running,
            first.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        require_identity_selected_columns(&running, fixture)?;
        require_identity_selected_root_row(&running, fixture, 20)?;

        let later = kernel
            .execute_server_select_with_arguments(
                fixture.select_node,
                &selector_argument(fixture, fixture.root)?,
            )
            .await?;
        require_result_identity(
            &later,
            second.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        require_identity_selected_columns(&later, fixture)?;
        require_identity_selected_root_row(&later, fixture, 21)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn distinct_execution_pins_active_revision_and_data_snapshot() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(EXECUTION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        insert_execution_rows(&database, fixture).await?;
        let source_only_candidate = candidate(EXECUTION_SOURCE_EDIT, &first)?;

        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_server_select_with_test_barrier(
                    fixture.unique_values,
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        let (running, second) = complete_pinned_execution(
            execution,
            reached,
            resume,
            "SELECT DISTINCT SERVER SELECT",
            async {
                update_root_value(&database, fixture, 21).await?;
                kernel
                    .apply(&source_only_candidate)
                    .await
                    .map_err(Into::into)
            },
        )
        .await?;

        require(
            second.pair() != first.pair(),
            "source-only apply did not advance the SELECT DISTINCT pair",
        )?;
        require(
            current_revision(&second, fixture.unique_values)? == fixture.unique_values_revision,
            "source-only apply did not reuse the immutable SELECT DISTINCT revision",
        )?;
        require_result_identity(
            &running,
            first.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&running, fixture)?;
        require_distinct_rows(&running, fixture, 20)?;

        let later = kernel.execute_server_select(fixture.unique_values).await?;
        require_result_identity(
            &later,
            second.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&later, fixture)?;
        require_distinct_rows(&later, fixture, 21)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn identity_selected_post_commit_shutdown_is_contextual_and_read_only() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;

        let error = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments_and_forced_post_commit_driver_shutdown(
                    fixture.select_node,
                    &selector_argument(fixture, fixture.root)?,
                )
                .await,
            "forced post-commit shutdown unexpectedly returned a collected result",
        )?;
        require_select_shutdown_error(
            &error,
            applied.pair(),
            fixture.select_node,
            fixture.select_node_revision,
        )?;
        let unchanged = kernel.execute_server_select(fixture.read).await?;
        require_result_identity(
            &unchanged,
            applied.pair(),
            fixture.read,
            fixture.read_revision,
        )?;
        require_exact_columns(&unchanged, fixture)?;
        require_exact_rows(&unchanged, fixture, 20)?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "post-commit select shutdown changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn distinct_post_commit_shutdown_is_contextual_and_read_only() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        insert_execution_rows(&database, fixture).await?;
        let before_rows = count_rows(&database, fixture.node).await?;

        let error = expect_kernel_error(
            kernel
                .execute_server_select_with_forced_post_commit_driver_shutdown(
                    fixture.unique_values,
                )
                .await,
            "forced SELECT DISTINCT post-commit shutdown unexpectedly returned a result",
        )?;
        require_select_shutdown_error(
            &error,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require(
            count_rows(&database, fixture.node).await? == before_rows,
            "SELECT DISTINCT post-commit shutdown changed physical rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "SELECT DISTINCT post-commit shutdown changed the active pair",
        )?;

        let unchanged = kernel.execute_server_select(fixture.unique_values).await?;
        require_result_identity(
            &unchanged,
            applied.pair(),
            fixture.unique_values,
            fixture.unique_values_revision,
        )?;
        require_distinct_columns(&unchanged, fixture)?;
        require_distinct_rows(&unchanged, fixture, 20)?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_artifacts_and_unknown_functions_before_target_execution() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel.apply(&candidate(EXECUTION_SOURCE, &active)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let unknown = FunctionId::from_bytes([0xee; 16]);

        let unknown_error = expect_kernel_error(
            kernel.execute_server_select(unknown).await,
            "unknown function unexpectedly executed",
        )?;
        require(matches!(
            unknown_error,
            PostgresKernelError::ServerSelect(ServerSelectError::FunctionNotActive { pair, function })
                if pair == applied.pair() && function == unknown
        ), "unknown function did not return FunctionNotActive for the recovered pair")?;

        let session = database.open().await?;
        let operation: TestResult<u64> = async {
            Ok(session
                .client()
                .execute(
                "UPDATE _orna_kernel.function_artifacts \
                 SET payload = $1 \
                 WHERE function_revision_id = $2 AND artifact_kind = 'server_plan'",
                &[
                    &vec![0_u8],
                    &fixture.select_node_revision.to_bytes().to_vec(),
                ],
                )
                .await?)
        }
        .await;
        let tampered = finish_session(session, operation, "identity-selected artifact tamper").await?;
        require(tampered == 1, "artifact tamper changed the wrong row count")?;

        let error = expect_kernel_error(
            kernel
                .execute_server_select_with_arguments(
                    fixture.select_node,
                    &selector_argument(fixture, fixture.root)?,
                )
                .await,
            "tampered artifact unexpectedly executed",
        )?;
        require(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.function_artifacts",
                ..
            }
        ), "tampered artifact did not fail as a durable invariant")?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[derive(Clone, Copy)]
struct Fixture {
    node: TypeId,
    other_type: TypeId,
    child: FieldId,
    active: FieldId,
    value: FieldId,
    amount: FieldId,
    score: FieldId,
    label: FieldId,
    blob: FieldId,
    read: FunctionId,
    none: FunctionId,
    select_node: FunctionId,
    unique_values: FunctionId,
    all_values: FunctionId,
    read_revision: FunctionRevisionId,
    none_revision: FunctionRevisionId,
    select_node_revision: FunctionRevisionId,
    unique_values_revision: FunctionRevisionId,
    all_values_revision: FunctionRevisionId,
    select_node_parameter: ParameterId,
    root: ObjectId,
    other: ObjectId,
    duplicate_null: ObjectId,
    duplicate_reference: ObjectId,
}

impl Fixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let node = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["exec", "node"]))
            .ok_or_else(|| failure("execution node type is absent"))?;
        let field = |name| {
            node.field_by_name(name)
                .map(|field| field.id())
                .ok_or_else(|| failure(format!("execution field {name} is absent")))
        };
        let read = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["exec", "read"]))
            .ok_or_else(|| failure("read function is absent"))?;
        let none = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["exec", "none"]))
            .ok_or_else(|| failure("none function is absent"))?;
        let select_node = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["exec", "select_node"]))
            .ok_or_else(|| failure("identity-selected function is absent"))?;
        let unique_values = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["exec", "unique_values"]))
            .ok_or_else(|| failure("SELECT DISTINCT function is absent"))?;
        let all_values = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["exec", "all_values"]))
            .ok_or_else(|| failure("version-1 value tracer function is absent"))?;
        let other_type = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["exec", "other"]))
            .ok_or_else(|| failure("active wrong-reference target type is absent"))?;
        let [selector] = select_node.parameters() else {
            return Err(failure(
                "identity-selected function must have one selector parameter",
            ));
        };
        Ok(Self {
            node: node.id(),
            other_type: other_type.id(),
            child: field("child")?,
            active: field("active")?,
            value: field("value")?,
            amount: field("amount")?,
            score: field("score")?,
            label: field("label")?,
            blob: field("blob")?,
            read: read.id(),
            none: none.id(),
            select_node: select_node.id(),
            unique_values: unique_values.id(),
            all_values: all_values.id(),
            read_revision: read.current_revision(),
            none_revision: none.current_revision(),
            select_node_revision: select_node.current_revision(),
            unique_values_revision: unique_values.current_revision(),
            all_values_revision: all_values.current_revision(),
            select_node_parameter: selector.id(),
            root: ObjectId::from_bytes([1; 16]),
            other: ObjectId::from_bytes([2; 16]),
            duplicate_null: ObjectId::from_bytes([3; 16]),
            duplicate_reference: ObjectId::from_bytes([4; 16]),
        })
    }
}

async fn insert_execution_rows(database: &TestDatabase, fixture: Fixture) -> TestResult<()> {
    let statement = format!(
        "INSERT INTO {} (_orna_object_id, {}, {}, {}, {}, {}, {}, {}) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        relation(fixture.node),
        field(fixture.child),
        field(fixture.active),
        field(fixture.value),
        field(fixture.amount),
        field(fixture.score),
        field(fixture.label),
        field(fixture.blob),
    );
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .execute(
                &statement,
                &[
                    &fixture.other.to_bytes().to_vec(),
                    &Option::<Vec<u8>>::None,
                    &true,
                    &10_i32,
                    &100_i64,
                    &1.5_f64,
                    &"other",
                    &vec![1_u8],
                ],
            )
            .await?;
        session
            .client()
            .execute(
                &statement,
                &[
                    &fixture.root.to_bytes().to_vec(),
                    &Some(fixture.other.to_bytes().to_vec()),
                    &true,
                    &20_i32,
                    &200_i64,
                    &2.5_f64,
                    &"root",
                    &vec![2_u8, 0],
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "execution fixture insert").await
}

async fn insert_distinct_duplicate_rows(
    database: &TestDatabase,
    fixture: Fixture,
) -> TestResult<()> {
    let statement = format!(
        "INSERT INTO {} (_orna_object_id, {}, {}, {}, {}, {}, {}, {}) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        relation(fixture.node),
        field(fixture.child),
        field(fixture.active),
        field(fixture.value),
        field(fixture.amount),
        field(fixture.score),
        field(fixture.label),
        field(fixture.blob),
    );
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .execute(
                &statement,
                &[
                    &fixture.duplicate_null.to_bytes().to_vec(),
                    &Option::<Vec<u8>>::None,
                    &true,
                    &10_i32,
                    &100_i64,
                    &1.5_f64,
                    &"other duplicate",
                    &vec![1_u8],
                ],
            )
            .await?;
        session
            .client()
            .execute(
                &statement,
                &[
                    &fixture.duplicate_reference.to_bytes().to_vec(),
                    &Some(fixture.other.to_bytes().to_vec()),
                    &true,
                    &20_i32,
                    &200_i64,
                    &2.5_f64,
                    &"root duplicate",
                    &vec![2_u8, 0],
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(
        session,
        operation,
        "SELECT DISTINCT duplicate fixture insert",
    )
    .await
}

#[cfg(feature = "test-hooks")]
async fn insert_distinct_limit_rows(database: &TestDatabase, fixture: Fixture) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "INSERT INTO {} (_orna_object_id, {}, {}, {}, {}, {}, {}, {}) \
                 SELECT decode(lpad(to_hex(value + 1000), 32, '0'), 'hex'), NULL, FALSE, \
                        30, 300, 3.0, 'limit duplicate', decode('03', 'hex') \
                 FROM generate_series(1, 10001) AS value;",
                relation(fixture.node),
                field(fixture.child),
                field(fixture.active),
                field(fixture.value),
                field(fixture.amount),
                field(fixture.score),
                field(fixture.label),
                field(fixture.blob),
            ))
            .await?;
        session
            .client()
            .execute(
                &format!(
                    "INSERT INTO {} (_orna_object_id, {}, {}, {}, {}, {}, {}, {}) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                    relation(fixture.node),
                    field(fixture.child),
                    field(fixture.active),
                    field(fixture.value),
                    field(fixture.amount),
                    field(fixture.score),
                    field(fixture.label),
                    field(fixture.blob),
                ),
                &[
                    &vec![0x70_u8; 16],
                    &Some(fixture.other.to_bytes().to_vec()),
                    &true,
                    &40_i32,
                    &400_i64,
                    &4_f64,
                    &"limit tail",
                    &vec![4_u8],
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(
        session,
        operation,
        "SELECT DISTINCT result-limit fixture insert",
    )
    .await
}

async fn install_public_execution_decoy(
    database: &TestDatabase,
    fixture: Fixture,
) -> TestResult<()> {
    let session = database.open().await?;
    let statement = format!(
        "CREATE TABLE public.t_{:032x} \
         (_orna_object_id bytea, {} bytea, {} boolean, {} integer, {} bigint, \
          {} double precision, {} text, {} bytea);",
        u128::from_be_bytes(fixture.node.to_bytes()),
        field(fixture.child),
        field(fixture.active),
        field(fixture.value),
        field(fixture.amount),
        field(fixture.score),
        field(fixture.label),
        field(fixture.blob),
    );
    let operation: TestResult<()> = async {
        session.client().batch_execute(&statement).await?;
        let insert = format!(
            "INSERT INTO public.t_{:032x} (_orna_object_id, {}, {}, {}, {}, {}, {}, {}) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            u128::from_be_bytes(fixture.node.to_bytes()),
            field(fixture.child),
            field(fixture.active),
            field(fixture.value),
            field(fixture.amount),
            field(fixture.score),
            field(fixture.label),
            field(fixture.blob),
        );
        session
            .client()
            .execute(
                &insert,
                &[
                    &fixture.other.to_bytes().to_vec(),
                    &Option::<Vec<u8>>::None,
                    &false,
                    &-999_i32,
                    &-999_i64,
                    &-999_f64,
                    &"hostile other",
                    &vec![0_u8],
                ],
            )
            .await?;
        session
            .client()
            .execute(
                &insert,
                &[
                    &fixture.root.to_bytes().to_vec(),
                    &Some(fixture.other.to_bytes().to_vec()),
                    &false,
                    &-998_i32,
                    &-998_i64,
                    &-998_f64,
                    &"hostile root",
                    &vec![0_u8],
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "public execution decoy").await
}

#[cfg(feature = "test-hooks")]
async fn update_root_value(
    database: &TestDatabase,
    fixture: Fixture,
    value: i32,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<u64> = async {
        Ok(session
            .client()
            .execute(
                &format!(
                    "UPDATE {} SET {} = $2 WHERE _orna_object_id = $1",
                    relation(fixture.node),
                    field(fixture.value),
                ),
                &[&fixture.root.to_bytes().to_vec(), &value],
            )
            .await?)
    }
    .await;
    let updated = finish_session(session, operation, "snapshot root update").await?;
    require(
        updated == 1,
        "snapshot advancement did not update the root row",
    )
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn hostile_kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    let mut config = database.config()?;
    config.options("-c search_path=public,pg_catalog");
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

#[cfg(feature = "test-hooks")]
fn current_revision(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> TestResult<FunctionRevisionId> {
    active
        .catalogue()
        .function_by_id(function)
        .map(|definition| definition.current_revision())
        .ok_or_else(|| {
            failure(format!(
                "function {function} is absent from the active catalogue"
            ))
        })
}

fn relation(type_id: TypeId) -> String {
    format!(
        "_orna_data.t_{:032x}",
        u128::from_be_bytes(type_id.to_bytes())
    )
}

fn field(field_id: FieldId) -> String {
    format!("f_{:032x}", u128::from_be_bytes(field_id.to_bytes()))
}

fn name_is(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message.into()))
    }
}

fn expect_kernel_error<T>(
    result: Result<T, PostgresKernelError>,
    success_message: &'static str,
) -> TestResult<PostgresKernelError> {
    match result {
        Ok(_) => Err(failure(success_message)),
        Err(error) => Ok(error),
    }
}

fn require_result_identity(
    result: &ServerSelectResult,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    require(
        result.pair() == pair,
        "result pair differs from the pinned pair",
    )?;
    require(
        result.function() == function,
        "result function differs from the requested function",
    )?;
    require(
        result.function_revision() == revision,
        "result function revision differs from the pinned revision",
    )
}

fn require_exact_columns(result: &ServerSelectResult, fixture: Fixture) -> TestResult<()> {
    let expected = [
        ("root", ResolvedType::reference(fixture.node), false),
        (
            "active",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        ),
        (
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        ),
        (
            "amount",
            ResolvedType::scalar(StandardScalar::BigInt),
            false,
        ),
        ("score", ResolvedType::scalar(StandardScalar::Float), false),
        (
            "label",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        ),
        (
            "blob",
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            false,
        ),
        (
            "child_label",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        ),
    ];
    require(
        result.rows().columns().len() == expected.len(),
        "result column count differs from the declared eight columns",
    )?;
    for (column, (name, resolved_type, nullable)) in result.rows().columns().iter().zip(expected) {
        require(
            column.name() == name,
            format!("result column name is not {name}"),
        )?;
        require(
            column.resolved_type() == resolved_type,
            format!("result column {name} has the wrong resolved type"),
        )?;
        require(
            column.nullable() == nullable,
            format!("result column {name} has the wrong nullability"),
        )?;
    }
    Ok(())
}

fn require_exact_rows(
    result: &ServerSelectResult,
    fixture: Fixture,
    root_value: i32,
) -> TestResult<()> {
    let expected = vec![
        vec![
            RuntimeValue::Reference {
                target: fixture.node,
                object: fixture.root,
            },
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(root_value),
            RuntimeValue::BigInt(200),
            RuntimeValue::Float(RuntimeFloat::new(2.5)?),
            RuntimeValue::Text("root".to_owned()),
            RuntimeValue::Bytes(vec![2, 0]),
            RuntimeValue::Text("other".to_owned()),
        ],
        vec![
            RuntimeValue::Reference {
                target: fixture.node,
                object: fixture.other,
            },
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(10),
            RuntimeValue::BigInt(100),
            RuntimeValue::Float(RuntimeFloat::new(1.5)?),
            RuntimeValue::Text("other".to_owned()),
            RuntimeValue::Bytes(vec![1]),
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))?,
        ],
    ];
    require(
        result.rows().rows().len() == expected.len(),
        "result row count differs from the two-row fixture",
    )?;
    for (index, (actual, expected)) in result.rows().rows().iter().zip(expected).enumerate() {
        require(
            actual.values() == expected,
            format!("result row {index} values differ from the canonical fixture"),
        )?;
    }
    Ok(())
}

fn require_distinct_columns(result: &ServerSelectResult, fixture: Fixture) -> TestResult<()> {
    let expected = [
        (
            "active",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        ),
        (
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        ),
        (
            "amount",
            ResolvedType::scalar(StandardScalar::BigInt),
            false,
        ),
        (
            "blob",
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            false,
        ),
        ("child", ResolvedType::reference(fixture.node), true),
    ];
    require(
        result.rows().columns().len() == expected.len(),
        "SELECT DISTINCT result column count differs from the declared five columns",
    )?;
    for (column, (name, resolved_type, nullable)) in result.rows().columns().iter().zip(expected) {
        require(
            column.name() == name,
            format!("SELECT DISTINCT column name is not {name}"),
        )?;
        require(
            column.resolved_type() == resolved_type,
            format!("SELECT DISTINCT column {name} has the wrong resolved type"),
        )?;
        require(
            column.nullable() == nullable,
            format!("SELECT DISTINCT column {name} has the wrong nullability"),
        )?;
    }
    Ok(())
}

fn require_distinct_rows(
    result: &ServerSelectResult,
    fixture: Fixture,
    root_value: i32,
) -> TestResult<()> {
    require_unordered_rows(
        result,
        distinct_rows(fixture, root_value)?,
        "SELECT DISTINCT base rows",
    )
}

#[cfg(feature = "test-hooks")]
fn require_distinct_limit_rows(
    result: &ServerSelectResult,
    fixture: Fixture,
    root_value: i32,
) -> TestResult<()> {
    let mut expected = distinct_rows(fixture, root_value)?;
    expected.extend([
        vec![
            RuntimeValue::Boolean(false),
            RuntimeValue::Integer(30),
            RuntimeValue::BigInt(300),
            RuntimeValue::Bytes(vec![3]),
            RuntimeValue::null(ResolvedType::reference(fixture.node))?,
        ],
        vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(40),
            RuntimeValue::BigInt(400),
            RuntimeValue::Bytes(vec![4]),
            RuntimeValue::Reference {
                target: fixture.node,
                object: fixture.other,
            },
        ],
    ]);
    require_unordered_rows(result, expected, "SELECT DISTINCT result-limit rows")
}

fn distinct_rows(fixture: Fixture, root_value: i32) -> TestResult<Vec<Vec<RuntimeValue>>> {
    Ok(vec![
        vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(10),
            RuntimeValue::BigInt(100),
            RuntimeValue::Bytes(vec![1]),
            RuntimeValue::null(ResolvedType::reference(fixture.node))?,
        ],
        vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(root_value),
            RuntimeValue::BigInt(200),
            RuntimeValue::Bytes(vec![2, 0]),
            RuntimeValue::Reference {
                target: fixture.node,
                object: fixture.other,
            },
        ],
    ])
}

fn require_unordered_rows(
    result: &ServerSelectResult,
    expected: Vec<Vec<RuntimeValue>>,
    name: &'static str,
) -> TestResult<()> {
    require(
        result.rows().rows().len() == expected.len(),
        format!("{name} returned the wrong row count"),
    )?;
    for expected_row in expected {
        require(
            result
                .rows()
                .rows()
                .iter()
                .any(|actual| actual.values() == expected_row),
            format!("{name} is missing one exact typed row"),
        )?;
    }
    Ok(())
}

fn require_version_one_value_multiset(
    result: &ServerSelectResult,
    fixture: Fixture,
    root_value: i32,
) -> TestResult<()> {
    let expected = distinct_rows(fixture, root_value)?;
    require(
        result.rows().rows().len() == expected.len() * 2,
        "version-1 value tracer did not return the four duplicate source values",
    )?;
    for expected_row in expected {
        let count = result
            .rows()
            .rows()
            .iter()
            .filter(|actual| actual.values() == expected_row)
            .count();
        require(
            count == 2,
            "version-1 value tracer did not preserve one exact typed duplicate pair",
        )?;
    }
    Ok(())
}

fn selector_argument(fixture: Fixture, object: ObjectId) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![FunctionArgument::new(
        fixture.select_node_parameter,
        RuntimeValue::Reference {
            target: fixture.node,
            object,
        },
    )?])
}

fn require_identity_selected_columns(
    result: &ServerSelectResult,
    fixture: Fixture,
) -> TestResult<()> {
    let expected = [
        ("selected", ResolvedType::reference(fixture.node), false),
        (
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        ),
        (
            "child_label",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        ),
        (
            "same_as_child",
            ResolvedType::scalar(StandardScalar::Boolean),
            true,
        ),
    ];
    require(
        result.rows().columns().len() == expected.len(),
        "identity-selected result column count differs",
    )?;
    for (column, (name, resolved_type, nullable)) in result.rows().columns().iter().zip(expected) {
        require(
            column.name() == name,
            format!("identity-selected result column name is not {name}"),
        )?;
        require(
            column.resolved_type() == resolved_type,
            format!("identity-selected result column {name} type differs"),
        )?;
        require(
            column.nullable() == nullable,
            format!("identity-selected result column {name} nullability differs"),
        )?;
    }
    Ok(())
}

fn require_identity_selected_root_row(
    result: &ServerSelectResult,
    fixture: Fixture,
    value: i32,
) -> TestResult<()> {
    require(
        result.rows().rows().len() == 1,
        "identity-selected root query did not return exactly one row",
    )?;
    require(
        result.rows().rows()[0].values()
            == [
                RuntimeValue::Reference {
                    target: fixture.node,
                    object: fixture.root,
                },
                RuntimeValue::Integer(value),
                RuntimeValue::Text(String::from("other")),
                RuntimeValue::Boolean(false),
            ],
        "identity-selected root row differs from the exact durable values",
    )
}

fn require_select_argument_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    parameter: Option<ParameterId>,
    rule: &'static str,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "argument error is not contextual SERVER SELECT execution",
        ));
    };
    require(context.pair() == pair, "argument context pair differs")?;
    require(
        context.function() == function,
        "argument context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "argument context revision differs",
    )?;
    let ServerSelectError::Argument {
        parameter: actual,
        rule: actual_rule,
    } = source.as_ref()
    else {
        return Err(failure("argument execution source is not Argument"));
    };
    require(*actual == parameter, "argument error parameter differs")?;
    require(*actual_rule == rule, "argument error rule differs")
}

#[cfg(feature = "test-hooks")]
fn require_select_shutdown_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "post-commit shutdown is not contextual SERVER SELECT execution",
        ));
    };
    require(context.pair() == pair, "shutdown context pair differs")?;
    require(
        context.function() == function,
        "shutdown context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "shutdown context revision differs",
    )?;
    let ServerSelectError::Kernel { source } = source.as_ref() else {
        return Err(failure(
            "post-commit shutdown source is not a contextual kernel failure",
        ));
    };
    require(
        matches!(source.as_ref(), PostgresKernelError::DriverTask(error) if error.is_cancelled()),
        "post-commit shutdown source is not the forced driver-task cancellation",
    )
}

async fn install_hostile_octet_length_shadow(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(
                "CREATE FUNCTION public.octet_length(text) RETURNS integer
             LANGUAGE plpgsql IMMUTABLE AS $$
             BEGIN
               RAISE EXCEPTION 'hostile public.octet_length(text) invoked';
             END;
             $$",
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "hostile octet_length fixture").await
}

#[cfg(feature = "test-hooks")]
async fn lock_target_relation(database: &TestDatabase, object: TypeId) -> TestResult<TestSession> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "BEGIN; LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
                relation(object),
            ))
            .await?;
        Ok(())
    }
    .await;
    match operation {
        Ok(()) => Ok(session),
        Err(error) => {
            match rollback_and_finish_session(session, Err(error), "target relation lock").await {
                Ok(()) => Err(failure("target relation lock failed without an error")),
                Err(error) => Err(error),
            }
        }
    }
}

async fn count_rows(database: &TestDatabase, object: TypeId) -> TestResult<i64> {
    let session = database.open().await?;
    let operation: TestResult<i64> = async {
        Ok(session
            .client()
            .query_one(&format!("SELECT count(*) FROM {}", relation(object)), &[])
            .await?
            .try_get(0)?)
    }
    .await;
    finish_session(session, operation, "private row count").await
}

fn require_variable_payload_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "oversize error is not the contextual Execution variant",
        ));
    };
    require(context.pair() == pair, "oversize context pair differs")?;
    require(
        context.function() == function,
        "oversize context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "oversize context function revision differs",
    )?;
    match source.as_ref() {
        ServerSelectError::VariablePayload {
            row,
            column,
            maximum,
        } => {
            require(*row == 0, "oversize row index differs")?;
            require(*column == 5, "oversize column index differs")?;
            require(
                *maximum == VARIABLE_PAYLOAD_MAXIMUM,
                "oversize maximum differs",
            )
        }
        _ => Err(failure("oversize source is not VariablePayload")),
    }
}

fn require_row_limit_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "row-limit error is not the contextual Execution variant",
        ));
    };
    require(context.pair() == pair, "row-limit context pair differs")?;
    require(
        context.function() == function,
        "row-limit context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "row-limit context function revision differs",
    )?;
    match source.as_ref() {
        ServerSelectError::RowLimit { maximum } => require(
            *maximum == 10_000,
            "row-limit maximum differs from the exact contextual bound",
        ),
        _ => Err(failure("row-limit source is not RowLimit")),
    }
}

async fn require_no_session_leaks(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(i64, i64)> = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FILTER (WHERE state = 'idle in transaction'),
                        count(*) FILTER (WHERE pid <> pg_catalog.pg_backend_pid())
                 FROM pg_catalog.pg_stat_activity
                 WHERE datname = pg_catalog.current_database()",
                &[],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    let (idle, others) = finish_session(session, operation, "session leak inspection").await?;
    require(idle == 0, format!("found {idle} idle transaction(s)"))?;
    require(others == 0, format!("found {others} leaked session(s)"))
}

async fn finish_session<T>(
    session: TestSession,
    operation: TestResult<T>,
    name: &str,
) -> TestResult<T> {
    let shutdown = session.shutdown().await;
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(failure(format!("{name} failed: {error}"))),
        (Ok(_), Err(error)) => Err(failure(format!("{name} shutdown failed: {error}"))),
        (Err(operation), Err(shutdown)) => Err(failure(format!(
            "{name} failed: {operation}; test session shutdown failed: {shutdown}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
async fn rollback_and_finish_session<T>(
    session: TestSession,
    operation: TestResult<T>,
    name: &str,
) -> TestResult<T> {
    let rollback = session.client().batch_execute("ROLLBACK").await;
    let shutdown = session.shutdown().await;
    match (operation, rollback, shutdown) {
        (Ok(value), Ok(()), Ok(())) => Ok(value),
        (Err(error), Ok(()), Ok(())) => Err(failure(format!("{name} failed: {error}"))),
        (Ok(_), Err(error), Ok(())) => Err(failure(format!("{name} rollback failed: {error}"))),
        (Ok(_), Ok(()), Err(error)) => Err(failure(format!("{name} shutdown failed: {error}"))),
        (Err(operation), Err(rollback), Ok(())) => Err(failure(format!(
            "{name} failed: {operation}; rollback failed: {rollback}"
        ))),
        (Err(operation), Ok(()), Err(shutdown)) => Err(failure(format!(
            "{name} failed: {operation}; shutdown failed: {shutdown}"
        ))),
        (Ok(_), Err(rollback), Err(shutdown)) => Err(failure(format!(
            "{name} rollback failed: {rollback}; shutdown failed: {shutdown}"
        ))),
        (Err(operation), Err(rollback), Err(shutdown)) => Err(failure(format!(
            "{name} failed: {operation}; rollback failed: {rollback}; shutdown failed: {shutdown}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
struct ExecutionTask {
    handle: Option<tokio::task::JoinHandle<Result<ServerSelectResult, PostgresKernelError>>>,
}

#[cfg(feature = "test-hooks")]
impl ExecutionTask {
    fn new(
        handle: tokio::task::JoinHandle<Result<ServerSelectResult, PostgresKernelError>>,
    ) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn abort_and_wait(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn finish_with_timeout(
        mut self,
        name: &str,
        wait: Duration,
    ) -> TestResult<Result<ServerSelectResult, PostgresKernelError>> {
        let Some(mut handle) = self.handle.take() else {
            return Err(failure(format!("{name} task was already consumed")));
        };
        match tokio::time::timeout(wait, &mut handle).await {
            Ok(result) => result.map_err(|error| failure(format!("{name} task failed: {error}"))),
            Err(_) => {
                handle.abort();
                let _ = handle.await;
                Err(failure(format!("{name} exceeded the bounded wait")))
            }
        }
    }

    async fn finish(self, name: &str) -> TestResult<ServerSelectResult> {
        self.finish_with_timeout(name, WAIT)
            .await?
            .map_err(|error| failure(format!("{name} failed: {error}")))
    }
}

#[cfg(feature = "test-hooks")]
impl Drop for ExecutionTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[cfg(feature = "test-hooks")]
async fn complete_pinned_execution<F>(
    mut execution: ExecutionTask,
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
    name: &'static str,
    advancement: F,
) -> TestResult<(ServerSelectResult, ActiveDatabaseRevision)>
where
    F: Future<Output = TestResult<ActiveDatabaseRevision>>,
{
    if tokio::time::timeout(WAIT, reached.wait()).await.is_err() {
        execution.abort_and_wait().await;
        return Err(failure(format!(
            "{name} did not recover and pin its initial snapshot"
        )));
    }
    let advanced = match tokio::time::timeout(WAIT, advancement).await {
        Ok(Ok(advanced)) => advanced,
        Ok(Err(error)) => {
            execution.abort_and_wait().await;
            return Err(error);
        }
        Err(_) => {
            execution.abort_and_wait().await;
            return Err(failure(format!(
                "{name} active-state advancement exceeded the bounded wait"
            )));
        }
    };
    if tokio::time::timeout(WAIT, resume.wait()).await.is_err() {
        execution.abort_and_wait().await;
        return Err(failure(format!(
            "{name} did not resume after active-state advancement"
        )));
    }
    let result = execution.finish(name).await?;
    Ok((result, advanced))
}
