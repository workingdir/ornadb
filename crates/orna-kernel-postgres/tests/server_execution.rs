//! Live PostgreSQL tests for the bounded active SERVER SELECT entry point.

mod support;

use std::str::FromStr;

#[cfg(feature = "test-hooks")]
use std::time::Duration;

use orna_compiler::{check, prepare};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, TypeId,
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
    value::{RuntimeFloat, RuntimeValue},
};
use orna_kernel_postgres::{
    PostgresKernel, PostgresKernelError, ServerSelectError, ServerSelectResult,
};
use support::{TestDatabase, TestResult, failure, with_test_database};

const EXECUTION_SOURCE: &str = "CREATE SCHEMA exec;\n\
    CREATE TYPE exec.node AS OBJECT (\n\
      child REF exec.node, active BOOL NOT NULL, value INT NOT NULL,\n\
      amount BIGINT NOT NULL, score FLOAT NOT NULL, label TEXT NOT NULL,\n\
      blob BYTES NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION exec.read()\n\
    RETURNS ROWS (root REF exec.node, active BOOL, value INT, amount BIGINT, score FLOAT, label TEXT, blob BYTES, child_label TEXT)\n\
    AS SELECT REF(n), n.active, n.value, n.amount, n.score, n.label, n.blob, n.child.label\n\
    FROM exec.node n WHERE n.active = TRUE ORDER BY n.value DESC;\n\
    CREATE SERVER FUNCTION exec.none() RETURNS ROWS (value INT)\n\
    AS SELECT n.value FROM exec.node n WHERE n.active = FALSE ORDER BY n.value;\n";

#[cfg(feature = "test-hooks")]
const EXECUTION_SOURCE_EDIT: &str = "-- source-only active edit\n\
    CREATE SCHEMA exec;\n\
    CREATE TYPE exec.node AS OBJECT ( child REF exec.node, active BOOL NOT NULL,\n\
      value INT NOT NULL, amount BIGINT NOT NULL, score FLOAT NOT NULL,\n\
      label TEXT NOT NULL, blob BYTES NOT NULL );\n\
    CREATE SERVER FUNCTION exec.read() RETURNS ROWS (root REF exec.node, active BOOL,\n\
      value INT, amount BIGINT, score FLOAT, label TEXT, blob BYTES, child_label TEXT)\n\
    AS SELECT REF(n), n.active, n.value, n.amount, n.score, n.label, n.blob, n.child.label\n\
    FROM exec.node n WHERE n.active = TRUE ORDER BY n.value DESC;\n\
    CREATE SERVER FUNCTION exec.none() RETURNS ROWS (value INT)\n\
    AS SELECT n.value FROM exec.node n WHERE n.active = FALSE ORDER BY n.value;\n";

const MANY_SOURCE: &str = "CREATE SCHEMA many;\n\
    CREATE TYPE many.row AS OBJECT (value INT NOT NULL);\n\
    CREATE SERVER FUNCTION many.all_rows() RETURNS ROWS (value INT)\n\
    AS SELECT r.value FROM many.row r ORDER BY r.value;\n";

#[cfg(feature = "test-hooks")]
const WAIT: Duration = Duration::from_secs(5);
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const VARIABLE_PAYLOAD_MAXIMUM: usize = 5_592_377;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn executes_the_active_server_select_subset_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
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
        require_no_idle_transaction(&database).await
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
        session
            .client()
            .execute(
                &format!(
                    "UPDATE {} SET {} = $2 WHERE _orna_object_id = $1",
                    relation(fixture.node),
                    field(fixture.label)
                ),
                &[&fixture.root.to_bytes().to_vec(), &oversize],
            )
            .await?;
        session.shutdown().await?;

        let before_rows = count_rows(&database, fixture.node).await?;
        let error = kernel
            .execute_server_select(fixture.read)
            .await
            .expect_err("oversized TEXT must not enter RuntimeValue");
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
        require_no_idle_transaction(&database).await
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
        session.shutdown().await?;

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
        session
            .client()
            .batch_execute(&format!(
                "INSERT INTO {} (_orna_object_id, {}) \
                 VALUES (decode(lpad(to_hex(10001), 32, '0'), 'hex'), 10001)",
                relation(object),
                field(value),
            ))
            .await?;
        session.shutdown().await?;

        let error = kernel
            .execute_server_select(function)
            .await
            .expect_err("10,001 rows must exceed the fixed bound");
        require_row_limit_error(&error, applied.pair(), function, revision)?;
        require(
            count_rows(&database, object).await? == 10_001,
            "row-limit execution changed physical rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "row-limit execution changed the active pair",
        )?;
        require_no_idle_transaction(&database).await
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
        let execution = tokio::spawn(async move {
            executor
                .execute_server_select_with_test_barrier(
                    fixture.read,
                    execution_reached,
                    execution_resume,
                )
                .await
        });

        if tokio::time::timeout(WAIT, reached.wait()).await.is_err() {
            execution.abort();
            let _ = execution.await;
            return Err(failure(
                "execution did not recover and pin its initial active snapshot",
            ));
        }

        let advancement: TestResult<_> = async {
            update_root_value(&database, fixture, 21).await?;
            kernel
                .apply(&source_only_candidate)
                .await
                .map_err(Into::into)
        }
        .await;
        resume.wait().await;
        let second = match advancement {
            Ok(second) => second,
            Err(error) => {
                execution.abort();
                let _ = execution.await;
                return Err(error);
            }
        };
        let running = wait_for_execution(execution).await?;

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
        require_no_idle_transaction(&database).await
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
        let applied = kernel.apply(&candidate(MANY_SOURCE, &active)?).await?;
        let function = applied.catalogue().functions()[0].id();
        let revision = applied.catalogue().functions()[0].current_revision();
        let unknown = FunctionId::from_bytes([0xee; 16]);

        let unknown_error = kernel
            .execute_server_select(unknown)
            .await
            .expect_err("unknown function must fail before target execution");
        require(matches!(
            unknown_error,
            PostgresKernelError::ServerSelect(ServerSelectError::FunctionNotActive { pair, function })
                if pair == applied.pair() && function == unknown
        ), "unknown function did not return FunctionNotActive for the recovered pair")?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "UPDATE _orna_kernel.function_artifacts \
                 SET payload = $1 \
                 WHERE function_revision_id = $2 AND artifact_kind = 'server_plan'",
                &[&vec![0_u8], &revision.to_bytes().to_vec()],
            )
            .await?;
        session.shutdown().await?;

        let error = kernel
            .execute_server_select(function)
            .await
            .expect_err("tampered artifact must fail during active recovery");
        require(matches!(
            error,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.function_artifacts",
                ..
            }
        ), "tampered artifact did not fail as a durable invariant")?;
        require_no_idle_transaction(&database).await
    })
    .await
}

#[derive(Clone, Copy)]
struct Fixture {
    node: TypeId,
    child: FieldId,
    active: FieldId,
    value: FieldId,
    amount: FieldId,
    score: FieldId,
    label: FieldId,
    blob: FieldId,
    read: FunctionId,
    none: FunctionId,
    read_revision: FunctionRevisionId,
    none_revision: FunctionRevisionId,
    root: ObjectId,
    other: ObjectId,
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
        Ok(Self {
            node: node.id(),
            child: field("child")?,
            active: field("active")?,
            value: field("value")?,
            amount: field("amount")?,
            score: field("score")?,
            label: field("label")?,
            blob: field("blob")?,
            read: read.id(),
            none: none.id(),
            read_revision: read.current_revision(),
            none_revision: none.current_revision(),
            root: ObjectId::from_bytes([1; 16]),
            other: ObjectId::from_bytes([2; 16]),
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
    session.shutdown().await
}

#[cfg(feature = "test-hooks")]
async fn update_root_value(
    database: &TestDatabase,
    fixture: Fixture,
    value: i32,
) -> TestResult<()> {
    let session = database.open().await?;
    let updated = session
        .client()
        .execute(
            &format!(
                "UPDATE {} SET {} = $2 WHERE _orna_object_id = $1",
                relation(fixture.node),
                field(fixture.value),
            ),
            &[&fixture.root.to_bytes().to_vec(), &value],
        )
        .await?;
    session.shutdown().await?;
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

async fn install_hostile_octet_length_shadow(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let result = session
        .client()
        .batch_execute(
            "CREATE FUNCTION public.octet_length(text) RETURNS integer
             LANGUAGE plpgsql IMMUTABLE AS $$
             BEGIN
               RAISE EXCEPTION 'hostile public.octet_length(text) invoked';
             END;
             $$",
        )
        .await;
    let shutdown = session.shutdown().await;
    result?;
    shutdown
}

async fn count_rows(database: &TestDatabase, object: TypeId) -> TestResult<i64> {
    let session = database.open().await?;
    let result = session
        .client()
        .query_one(&format!("SELECT count(*) FROM {}", relation(object)), &[])
        .await?
        .try_get(0)?;
    session.shutdown().await?;
    Ok(result)
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

async fn require_no_idle_transaction(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let idle: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_stat_activity
             WHERE datname = pg_catalog.current_database()
               AND state = 'idle in transaction'",
            &[],
        )
        .await?
        .try_get(0)?;
    session.shutdown().await?;
    require(idle == 0, format!("found {idle} idle transaction(s)"))
}

#[cfg(feature = "test-hooks")]
async fn wait_for_execution(
    mut execution: tokio::task::JoinHandle<Result<ServerSelectResult, PostgresKernelError>>,
) -> TestResult<ServerSelectResult> {
    match tokio::time::timeout(WAIT, &mut execution).await {
        Ok(result) => result
            .map_err(|error| failure(format!("snapshot execution task failed: {error}")))?
            .map_err(|error| failure(format!("snapshot execution failed: {error}"))),
        Err(_) => {
            execution.abort();
            let _ = execution.await;
            Err(failure("snapshot execution exceeded the bounded wait"))
        }
    }
}
