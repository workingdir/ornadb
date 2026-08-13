//! Live PostgreSQL tests for atomic single-row SERVER mutation execution.

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod support;

use std::{collections::BTreeSet, str::FromStr};

#[cfg(feature = "test-hooks")]
use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle as ThreadJoinHandle,
    time::{Duration, Instant},
};

use orna_compiler::{
    StandardApplicationCheckContext, check, check_standard_application, prepare,
    prepare_standard_application,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    revision::{
        ActiveDatabaseRevision, DeployableRevision, RevisionPair, VerifiedStandardLibrarySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    types::ResolvedType,
    value::{EnumValue, FunctionArgument, RecordValue, RuntimeFloat, RuntimeValue},
};
use orna_postgres::{
    PostgresKernel, PostgresKernelError, ServerDeleteCommitState, ServerDeleteError,
    ServerDeleteResult, ServerInsertCommitState, ServerInsertError, ServerInsertResult,
    ServerMutationError, ServerUpdateCommitState, ServerUpdateError, ServerUpdateResult,
};
use orna_protocol::encode_active_value;
use support::{TestDatabase, TestResult, TestSession, failure, with_test_database};
use tokio_postgres::error::SqlState;
#[cfg(feature = "test-hooks")]
use tokio_postgres::{
    Config,
    config::{Host, SslMode},
};

const MUTATION_SOURCE: &str = "CREATE SCHEMA tasks;\n\
    CREATE TYPE tasks.owner AS OBJECT (name TEXT NOT NULL);\n\
    CREATE TYPE tasks.task AS OBJECT (\n\
      active BOOL NOT NULL, count INT NOT NULL, amount BIGINT NOT NULL,\n\
      score FLOAT NOT NULL, title TEXT NOT NULL, payload BYTES NOT NULL,\n\
      owner REF tasks.owner NOT NULL, note TEXT\n\
    );\n\
    CREATE TYPE tasks.task_restrict AS OBJECT (\n\
      task REF tasks.task NOT NULL ON DELETE RESTRICT\n\
    );\n\
    CREATE TYPE tasks.task_set_null AS OBJECT (\n\
      task REF tasks.task ON DELETE SET NULL\n\
    );\n\
    CREATE TYPE tasks.task_cascade AS OBJECT (\n\
      task REF tasks.task NOT NULL ON DELETE CASCADE\n\
    );\n\
    CREATE SERVER FUNCTION tasks.create_owner(p_name TEXT)\n\
    RETURNS ROWS (created_owner REF tasks.owner)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO tasks.owner AS made_owner (name)\n\
    VALUES (p_name) RETURNING REF(made_owner);\n\
    CREATE SERVER FUNCTION tasks.create_task(\n\
      p_active BOOL, p_count INT, p_amount BIGINT, p_score FLOAT,\n\
      p_title TEXT, p_payload BYTES, p_owner REF tasks.owner\n\
    )\n\
    RETURNS ROWS (created_task REF tasks.task)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO tasks.task AS made_task\n\
    (active, count, amount, score, title, payload, owner)\n\
    VALUES (p_active, p_count, p_amount, p_score, p_title, p_payload, p_owner)\n\
    RETURNING REF(made_task);\n\
    CREATE SERVER FUNCTION tasks.update_task(\n\
      p_task REF tasks.task, p_active BOOL, p_count INT,\n\
      p_title TEXT, p_owner REF tasks.owner\n\
    )\n\
    RETURNS ROWS (updated_task REF tasks.task)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE tasks.task AS updated_task\n\
    SET active = p_active, count = p_count, title = p_title,\n\
        owner = p_owner, note = NULL\n\
    WHERE REF(updated_task) = p_task\n\
    RETURNING REF(updated_task);\n\
    CREATE SERVER FUNCTION tasks.delete_task(p_task REF tasks.task)\n\
    RETURNS ROWS (deleted BOOL)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS DELETE FROM tasks.task AS deleted_task\n\
    WHERE REF(deleted_task) = p_task RETURNING TRUE;\n\
    CREATE SERVER FUNCTION tasks.delete_owner(p_owner REF tasks.owner)\n\
    RETURNS ROWS (deleted BOOL)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS DELETE FROM tasks.owner AS deleted_owner\n\
    WHERE REF(deleted_owner) = p_owner RETURNING TRUE;\n";

const RECORD_MUTATION_SOURCE: &str = "CREATE SCHEMA record_mutation;\n\
    CREATE TYPE record_mutation.stage AS ENUM ('lead', 'qualified');\n\
    CREATE TYPE record_mutation.status AS VALUE (\n\
      enabled BOOLEAN, stage record_mutation.stage\n\
    ) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE record_mutation.case AS OBJECT (\n\
      status record_mutation.status NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION record_mutation.create(\n\
      p_enabled BOOLEAN, p_stage record_mutation.stage\n\
    )\n\
    RETURNS ROWS (created REF record_mutation.case)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO record_mutation.case AS made (status)\n\
    VALUES (record_mutation.status{stage: p_stage, enabled: p_enabled})\n\
    RETURNING REF(made);\n\
    CREATE SERVER FUNCTION record_mutation.read()\n\
    RETURNS ROWS (status record_mutation.status)\n\
    AS SELECT item.status FROM record_mutation.case item;\n";

// This stays separate from `tasks.owner`: the main fixture deliberately uses
// that type for a high-volume allocation regression and cannot make it unique.
#[cfg(feature = "test-hooks")]
const UNIQUE_REFERENCE_SOURCE: &str = "CREATE SCHEMA assignments;\n\
    CREATE TYPE assignments.owner AS OBJECT (name TEXT NOT NULL);\n\
    CREATE TYPE assignments.assignment AS OBJECT (\n\
      owner REF assignments.owner NOT NULL UNIQUE, label TEXT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION assignments.create_owner(p_name TEXT)\n\
    RETURNS ROWS (created_owner REF assignments.owner)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO assignments.owner AS made_owner (name)\n\
    VALUES (p_name) RETURNING REF(made_owner);\n\
    CREATE SERVER FUNCTION assignments.create_assignment(\n\
      p_owner REF assignments.owner, p_label TEXT\n\
    ) RETURNS ROWS (created_assignment REF assignments.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO assignments.assignment AS made_assignment (owner, label)\n\
    VALUES (p_owner, p_label) RETURNING REF(made_assignment);\n\
    CREATE SERVER FUNCTION assignments.update_assignment(\n\
      p_assignment REF assignments.assignment, p_owner REF assignments.owner, p_label TEXT\n\
    ) RETURNS ROWS (updated_assignment REF assignments.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE assignments.assignment AS changed_assignment\n\
    SET owner = p_owner, label = p_label\n\
    WHERE REF(changed_assignment) = p_assignment\n\
    RETURNING REF(changed_assignment);\n";

#[cfg(feature = "test-hooks")]
const MUTATION_SOURCE_EDIT: &str = "-- source-only edit\n\
    CREATE SCHEMA tasks;\n\
    CREATE TYPE tasks.owner AS OBJECT ( name TEXT NOT NULL );\n\
    CREATE TYPE tasks.task AS OBJECT ( active BOOL NOT NULL, count INT NOT NULL,\n\
      amount BIGINT NOT NULL, score FLOAT NOT NULL, title TEXT NOT NULL,\n\
      payload BYTES NOT NULL, owner REF tasks.owner NOT NULL, note TEXT );\n\
    CREATE TYPE tasks.task_restrict AS OBJECT (\n\
      task REF tasks.task NOT NULL ON DELETE RESTRICT );\n\
    CREATE TYPE tasks.task_set_null AS OBJECT (\n\
      task REF tasks.task ON DELETE SET NULL );\n\
    CREATE TYPE tasks.task_cascade AS OBJECT (\n\
      task REF tasks.task NOT NULL ON DELETE CASCADE );\n\
    CREATE SERVER FUNCTION tasks.create_owner( p_name TEXT )\n\
    RETURNS ROWS ( created_owner REF tasks.owner ) SECURITY INVOKER\n\
    TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO tasks.owner AS made_owner ( name )\n\
    VALUES ( p_name ) RETURNING REF(made_owner);\n\
    CREATE SERVER FUNCTION tasks.create_task( p_active BOOL, p_count INT,\n\
      p_amount BIGINT, p_score FLOAT, p_title TEXT, p_payload BYTES,\n\
      p_owner REF tasks.owner )\n\
    RETURNS ROWS ( created_task REF tasks.task ) SECURITY INVOKER\n\
    TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO tasks.task AS made_task\n\
    ( active, count, amount, score, title, payload, owner )\n\
    VALUES ( p_active, p_count, p_amount, p_score, p_title, p_payload, p_owner )\n\
    RETURNING REF(made_task);\n\
    CREATE SERVER FUNCTION tasks.update_task( p_task REF tasks.task,\n\
      p_active BOOL, p_count INT, p_title TEXT, p_owner REF tasks.owner )\n\
    RETURNS ROWS ( updated_task REF tasks.task ) SECURITY INVOKER\n\
    TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE tasks.task AS updated_task\n\
    SET active = p_active, count = p_count, title = p_title,\n\
      owner = p_owner, note = NULL\n\
    WHERE REF(updated_task) = p_task RETURNING REF(updated_task);\n\
    CREATE SERVER FUNCTION tasks.delete_task( p_task REF tasks.task )\n\
    RETURNS ROWS ( deleted BOOL ) SECURITY INVOKER TRANSACTION ATOMIC\n\
    VOLATILITY VOLATILE AS DELETE FROM tasks.task AS deleted_task\n\
    WHERE REF(deleted_task) = p_task RETURNING TRUE;\n\
    CREATE SERVER FUNCTION tasks.delete_owner( p_owner REF tasks.owner )\n\
    RETURNS ROWS ( deleted BOOL ) SECURITY INVOKER TRANSACTION ATOMIC\n\
    VOLATILITY VOLATILE AS DELETE FROM tasks.owner AS deleted_owner\n\
    WHERE REF(deleted_owner) = p_owner RETURNING TRUE;\n";

#[cfg(feature = "test-hooks")]
const WAIT: Duration = Duration::from_secs(10);

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn commits_exact_typed_rows_uses_private_ids_and_allocates_unique_ids() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let owner = kernel
            .execute_server_insert(
                fixture.create_owner,
                &[FunctionArgument::new(
                    fixture.owner_name_parameter,
                    RuntimeValue::Text(String::from("Ada")),
                )?],
            )
            .await?;
        require_insert_result(
            &owner,
            applied.pair(),
            fixture.create_owner,
            fixture.create_owner_revision,
            fixture.owner,
            "created_owner",
        )?;
        require_owner_row(&database, fixture, owner.object(), "Ada").await?;

        let exact = ExactTask {
            active: true,
            count: -17,
            amount: 9_000_000_001,
            score: 3.25,
            title: String::from("exact task"),
            payload: vec![0, 1, 255],
            owner: owner.object(),
        };
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &exact)?)
            .await?;
        require_insert_result(
            &task,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let mut identities = BTreeSet::from([task.object()]);
        for index in 1..100_i32 {
            let value = ExactTask {
                active: index % 2 == 0,
                count: index,
                amount: i64::from(index) * 10_000,
                score: f64::from(index) / 4.0,
                title: format!("task {index}"),
                payload: index.to_be_bytes().to_vec(),
                owner: owner.object(),
            };
            let inserted = kernel
                .execute_server_insert(fixture.create_task, &task_arguments(fixture, &value)?)
                .await?;
            require_insert_result(
                &inserted,
                applied.pair(),
                fixture.create_task,
                fixture.create_task_revision,
                fixture.task,
                "created_task",
            )?;
            require(
                identities.insert(inserted.object()),
                "SERVER INSERT allocated a duplicate object identity",
            )?;
        }
        require(
            identities.len() == 100,
            "the 100 committed inserts did not return 100 unique identities",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 100,
            "the private task relation does not contain all 100 committed rows",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private INSERT",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "row execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_value_mutations_preserve_legacy_bind_and_result_behaviour() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let version_one = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let version_two_candidate =
            standard_application_candidate(MUTATION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&version_two_candidate).await?;

        let fixture = Fixture::from_active(&applied)?;
        require_standard_mutation_catalogue(
            &applied,
            fixture,
            upgrade.verified_standard_snapshot(),
        )?;
        let owner = insert_owner(&kernel, fixture, "Ada").await?;
        let original = ExactTask::new(owner.object());
        let inserted = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &original)?)
            .await?;
        require_insert_result(
            &inserted,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, inserted.object(), &original).await?;

        let changed = ExactTask {
            active: true,
            count: -73,
            title: String::from("updated task"),
            ..original.clone()
        };
        let updated = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, inserted.object(), &changed)?,
            )
            .await?;
        require_update_result(&updated, applied.pair(), fixture, inserted.object(), true)?;
        require_task_row(&database, fixture, inserted.object(), &changed).await?;

        let deleted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    inserted.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &deleted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            inserted.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "standard-backed DELETE left the inserted task row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn constructs_stores_and_reads_one_canonical_named_record() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let version_one = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(RECORD_MUTATION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;

        let enum_type = applied
            .catalogue()
            .enum_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.stage")
            .ok_or_else(|| failure("record mutation enum is absent"))?;
        let record = applied
            .catalogue()
            .record_value_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.status")
            .ok_or_else(|| failure("record mutation type is absent"))?;
        let object = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.case")
            .ok_or_else(|| failure("record mutation object is absent"))?;
        let object_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("record mutation object field is absent"))?;
        let create = applied
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.create")
            .ok_or_else(|| failure("record mutation INSERT function is absent"))?;
        let read = applied
            .catalogue()
            .functions()
            .iter()
            .find(|definition| definition.name().to_string() == "record_mutation.read")
            .ok_or_else(|| failure("record mutation SELECT function is absent"))?;
        let enabled_parameter = create
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == "p_enabled")
            .ok_or_else(|| failure("record mutation Boolean parameter is absent"))?;
        let stage_parameter = create
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == "p_stage")
            .ok_or_else(|| failure("record mutation enum parameter is absent"))?;
        let stage = EnumValue::new(applied.catalogue(), enum_type.id(), "qualified")?;
        let expected = RuntimeValue::Record(RecordValue::new(
            &applied,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("stage"), RuntimeValue::Enum(stage.clone())),
            ],
        )?);
        let expected_bytes = encode_active_value(&applied, &expected)?;

        let inserted = kernel
            .execute_server_insert(
                create.id(),
                &[
                    FunctionArgument::new(stage_parameter.id(), RuntimeValue::Enum(stage))?,
                    FunctionArgument::new(enabled_parameter.id(), RuntimeValue::Boolean(true))?,
                ],
            )
            .await?;
        require_insert_result(
            &inserted,
            applied.pair(),
            create.id(),
            create.current_revision(),
            object.id(),
            "created",
        )?;

        let session = database.open().await?;
        let stored = session
            .client()
            .query_one(
                &format!(
                    "SELECT {} FROM {} WHERE _orna_object_id = $1",
                    field(object_field.id()),
                    relation(object.id()),
                ),
                &[&inserted.object().to_bytes().to_vec()],
            )
            .await?
            .try_get::<_, Vec<u8>>(0)?;
        session.shutdown().await?;
        require(
            stored == expected_bytes,
            "record INSERT did not store the exact canonical ORV3 bytes",
        )?;

        let selected = kernel.execute_server_select(read.id()).await?;
        let [row] = selected.rows().rows() else {
            return Err(failure("record SELECT did not return exactly one row"));
        };
        let [actual] = row.values() else {
            return Err(failure("record SELECT did not return exactly one value"));
        };
        require(
            selected.pair() == applied.pair()
                && selected.function() == read.id()
                && selected.function_revision() == read.current_revision()
                && actual == &expected,
            "record INSERT and SELECT did not preserve the active nominal value",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn update_returns_zero_or_one_row_and_rolls_back_reference_failures() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let first_owner = insert_owner(&kernel, fixture, "Ada").await?;
        let second_owner = insert_owner(&kernel, fixture, "Grace").await?;
        let original = ExactTask::new(first_owner.object());
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &original)?)
            .await?;

        let changed = ExactTask {
            active: true,
            count: -73,
            amount: original.amount,
            score: original.score,
            title: String::from("updated task"),
            payload: original.payload.clone(),
            owner: second_owner.object(),
        };
        let updated = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, task.object(), &changed)?,
            )
            .await?;
        require_update_result(&updated, applied.pair(), fixture, task.object(), true)?;
        require_task_row(&database, fixture, task.object(), &changed).await?;

        let absent = ObjectId::from_bytes([0xb1; 16]);
        let missing = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, absent, &changed)?,
            )
            .await?;
        require_update_result(&missing, applied.pair(), fixture, absent, false)?;
        require(
            count_rows(&database, fixture.task).await? == 1,
            "updating an absent object changed the target relation",
        )?;

        let invalid_reference = ExactTask {
            owner: ObjectId::from_bytes([0xb2; 16]),
            ..changed.clone()
        };
        let error = kernel
            .execute_server_update(
                fixture.update_task,
                &update_arguments(fixture, task.object(), &invalid_reference)?,
            )
            .await
            .expect_err("a missing referenced owner must reject the update");
        require_update_database_failure(&error, applied.pair(), fixture)?;
        require_task_row(&database, fixture, task.object(), &changed).await?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private UPDATE",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "SERVER UPDATE execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn delete_returns_zero_or_one_boolean_and_hides_reference_timing() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.task).await?;

        let unknown_function = FunctionId::from_bytes([0xdd; 16]);
        let unknown = kernel.execute_server_delete(unknown_function, &[]).await;
        let unknown = match unknown {
            Ok(_) => return Err(failure("unknown DELETE function unexpectedly executed")),
            Err(error) => error,
        };
        require(
            matches!(
                &unknown,
                PostgresKernelError::ServerDelete(ServerDeleteError::FunctionNotActive {
                    pair,
                    function,
                }) if *pair == applied.pair() && *function == unknown_function
            ),
            "unknown DELETE function lost its active pair or typed identity",
        )?;

        let owner = insert_owner(&kernel, fixture, "Ada").await?;
        let exact = ExactTask::new(owner.object());
        let task = kernel
            .execute_server_insert(fixture.create_task, &task_arguments(fixture, &exact)?)
            .await?;

        let wrong_target = [FunctionArgument::new(
            fixture.delete_task_selector_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: owner.object(),
            },
        )?];
        let wrong_target = kernel
            .execute_server_delete(fixture.delete_task, &wrong_target)
            .await;
        let wrong_target = match wrong_target {
            Ok(_) => {
                return Err(failure(
                    "wrong-target DELETE argument unexpectedly executed",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(ServerDeleteError::NotCommitted { context, source }) =
            &wrong_target
        else {
            return Err(failure(
                "wrong-target DELETE argument did not fail before execution",
            ));
        };
        require_context(
            *context,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
        )?;
        require(
            matches!(source.as_ref(), ServerMutationError::Argument { .. }),
            "wrong-target DELETE argument did not fail typed argument validation",
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let restricted = kernel
            .execute_server_delete(
                fixture.delete_owner,
                &delete_argument(
                    fixture.delete_owner_selector_parameter,
                    fixture.owner,
                    owner.object(),
                )?,
            )
            .await;
        let restricted = match restricted {
            Ok(_) => return Err(failure("a referenced owner was unexpectedly deleted")),
            Err(error) => error,
        };
        require_delete_restricted(
            &restricted,
            applied.pair(),
            fixture.delete_owner,
            fixture.delete_owner_revision,
            fixture.owner,
            owner.object(),
            &SqlState::FOREIGN_KEY_VIOLATION,
        )?;
        require_owner_row(&database, fixture, owner.object(), "Ada").await?;
        require_task_row(&database, fixture, task.object(), &exact).await?;

        let restrict_object = ObjectId::from_bytes([0xc1; 16]);
        let set_null_object = ObjectId::from_bytes([0xc2; 16]);
        let cascade_object = ObjectId::from_bytes([0xc3; 16]);
        insert_reference_fixture_row(
            &database,
            fixture.task_restrict,
            fixture.task_restrict_field,
            restrict_object,
            task.object(),
        )
        .await?;
        insert_reference_fixture_row(
            &database,
            fixture.task_set_null,
            fixture.task_set_null_field,
            set_null_object,
            task.object(),
        )
        .await?;
        insert_reference_fixture_row(
            &database,
            fixture.task_cascade,
            fixture.task_cascade_field,
            cascade_object,
            task.object(),
        )
        .await?;

        let task_restricted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await;
        let task_restricted = match task_restricted {
            Ok(_) => return Err(failure("RESTRICT unexpectedly allowed task deletion")),
            Err(error) => error,
        };
        require_delete_restricted(
            &task_restricted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            &SqlState::RESTRICT_VIOLATION,
        )?;
        require_task_row(&database, fixture, task.object(), &exact).await?;
        require(
            reference_fixture_value(
                &database,
                fixture.task_set_null,
                fixture.task_set_null_field,
                set_null_object,
            )
            .await?
                == Some(task.object().to_bytes().to_vec()),
            "failed restricted DELETE changed the SET NULL row",
        )?;
        require(
            count_rows(&database, fixture.task_cascade).await? == 1,
            "failed restricted DELETE changed the CASCADE row",
        )?;
        delete_fixture_row(&database, fixture.task_restrict, restrict_object).await?;

        let deleted = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &deleted,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "matched DELETE left the selected task row",
        )?;
        require(
            reference_fixture_value(
                &database,
                fixture.task_set_null,
                fixture.task_set_null_field,
                set_null_object,
            )
            .await?
            .is_none(),
            "SET NULL did not clear the dependent reference",
        )?;
        require(
            count_rows(&database, fixture.task_cascade).await? == 0,
            "CASCADE did not remove the dependent object",
        )?;

        let absent = kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    task.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &absent,
            applied.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            task.object(),
            false,
        )?;

        let owner_deleted = kernel
            .execute_server_delete(
                fixture.delete_owner,
                &delete_argument(
                    fixture.delete_owner_selector_parameter,
                    fixture.owner,
                    owner.object(),
                )?,
            )
            .await?;
        require_delete_result(
            &owner_deleted,
            applied.pair(),
            fixture.delete_owner,
            fixture.delete_owner_revision,
            fixture.owner,
            owner.object(),
            true,
        )?;
        require(
            count_rows(&database, fixture.owner).await? == 0,
            "owner remained after its dependent task was deleted",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.task).await? == 0,
            "hostile public search_path redirected the private DELETE",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "SERVER DELETE execution changed the active revision pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn reference_failures_are_preflight_or_database_integrity_rejections() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let base = ExactTask::new(owner.object());

        let mut wrong_target = task_arguments(fixture, &base)?;
        replace_owner_argument(
            &mut wrong_target,
            fixture,
            RuntimeValue::Reference {
                target: fixture.task,
                object: ObjectId::from_bytes([0x91; 16]),
            },
        )?;
        let wrong_error = kernel
            .execute_server_insert(fixture.create_task, &wrong_target)
            .await
            .expect_err("wrong-target REF must fail before the INSERT");
        require_not_committed_argument_error(
            &wrong_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "wrong-target REF left a task row",
        )?;

        let missing_owner = ObjectId::from_bytes([0x92; 16]);
        let mut nonexistent = task_arguments(fixture, &base)?;
        replace_owner_argument(
            &mut nonexistent,
            fixture,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: missing_owner,
            },
        )?;
        let missing_error = kernel
            .execute_server_insert(fixture.create_task, &nonexistent)
            .await
            .expect_err("missing same-target REF must fail the physical foreign key");
        require_wrapped_database_failure(
            &missing_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            &SqlState::FOREIGN_KEY_VIOLATION,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "foreign-key rejection left a task row",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "reference failures changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn unknown_and_tampered_functions_fail_before_the_target_insert() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let unknown = FunctionId::from_bytes([0xa1; 16]);
        let error = kernel
            .execute_server_insert(unknown, &[])
            .await
            .expect_err("unknown function must fail before target INSERT");
        require(
            matches!(
                error,
                PostgresKernelError::ServerInsert(ServerInsertError::FunctionNotActive {
                    pair,
                    function,
                }) if pair == applied.pair() && function == unknown
            ),
            "unknown function did not retain the recovered pair and function identity",
        )?;
        require_unchanged_state(&database, fixture.task, applied.pair(), 0).await?;
        require_no_session_leaks(&database).await
    })
    .await?;

    assert_tamper_rejected_before_insert(Tamper::Artifact).await?;
    assert_tamper_rejected_before_insert(Tamper::Reference).await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn row_and_deferred_trigger_failures_roll_back_with_not_committed_state() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let arguments = task_arguments(fixture, &ExactTask::new(owner.object()))?;

        let after_error = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_task,
            fixture.task,
            &arguments,
            TriggerKind::AfterRow,
            "triggered insert",
        )
        .await?;
        require_wrapped_database_failure(
            &after_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            &SqlState::RAISE_EXCEPTION,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "AFTER INSERT failure left a task row",
        )?;

        let deferred_error = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_task,
            fixture.task,
            &arguments,
            TriggerKind::DeferredConstraint,
            "triggered insert",
        )
        .await?;
        require_commit_rejected(
            &deferred_error,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "deferred constraint-trigger failure left a task row",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "trigger failures changed the active pair",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn insert_pins_snapshot_while_source_only_apply_advances() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let arguments = task_arguments(fixture, &ExactTask::new(owner.object()))?;
        let source_only = candidate(MUTATION_SOURCE_EDIT, &first)?;
        require(
            source_only.new_function_revisions().is_empty(),
            "source-only edit unexpectedly created a function revision",
        )?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let mut execution = tokio::spawn(async move {
            executor
                .execute_server_insert_with_test_barrier(
                    fixture.create_task,
                    &arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        wait_for_barrier(&mut execution, reached, "snapshot insert", "recovery").await?;

        let advancement = kernel.apply(&source_only).await;
        resume.wait().await;
        let second = match advancement {
            Ok(active) => active,
            Err(error) => {
                abort_and_wait(execution).await;
                return Err(error.into());
            }
        };
        let running = wait_for_success(execution, "snapshot insert").await?;

        require(
            first.pair() != second.pair(),
            "source-only apply did not advance the pair",
        )?;
        require(
            fixture.create_task_revision == function_revision(&second, fixture.create_task)?,
            "source-only apply did not reuse the immutable INSERT function revision",
        )?;
        require_insert_result(
            &running,
            first.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;

        let later = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        require_insert_result(
            &later,
            second.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 2,
            "snapshot test did not commit both task rows",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn delete_pins_snapshot_and_preserves_uncertain_and_committed_outcomes() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let first = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&first)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let first_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let source_only = candidate(MUTATION_SOURCE_EDIT, &first)?;
        require(
            source_only.new_function_revisions().is_empty(),
            "source-only edit unexpectedly created a function revision",
        )?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let arguments = delete_argument(
            fixture.delete_task_selector_parameter,
            fixture.task,
            first_task.object(),
        )?;
        let mut execution = tokio::spawn(async move {
            executor
                .execute_server_delete_with_test_barrier(
                    fixture.delete_task,
                    &arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        wait_for_barrier(&mut execution, reached, "snapshot delete", "recovery").await?;

        let advancement = kernel.apply(&source_only).await;
        resume.wait().await;
        let second = match advancement {
            Ok(active) => active,
            Err(error) => {
                abort_and_wait(execution).await;
                return Err(error.into());
            }
        };
        let running = wait_for_success(execution, "snapshot delete").await?;
        require(
            first.pair() != second.pair(),
            "source-only apply did not advance the pair",
        )?;
        require(
            fixture.delete_task_revision == function_revision(&second, fixture.delete_task)?,
            "source-only apply did not reuse the immutable DELETE function revision",
        )?;
        require_delete_result(
            &running,
            first.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            first_task.object(),
            true,
        )?;

        let second_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let committed_shutdown = kernel
            .execute_server_delete_with_forced_post_commit_driver_shutdown(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    second_task.object(),
                )?,
            )
            .await;
        let committed_shutdown = match committed_shutdown {
            Ok(_) => {
                return Err(failure(
                    "forced post-commit shutdown unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(
            ServerDeleteError::CommittedButShutdownFailed { result, .. },
        ) = committed_shutdown
        else {
            return Err(failure(
                "post-commit shutdown did not retain the confirmed DELETE result",
            ));
        };
        require_delete_result(
            &result,
            second.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
            fixture.task,
            second_task.object(),
            true,
        )?;

        let rejected_task_value = ExactTask::new(owner.object());
        let rejected_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &rejected_task_value)?,
            )
            .await?;
        let rejected = execute_delete_with_installed_trigger(
            &database,
            &kernel,
            fixture,
            rejected_task.object(),
        )
        .await?;
        require_delete_commit_rejected(
            &rejected,
            second.pair(),
            fixture,
            rejected_task.object(),
        )?;
        require_task_row(
            &database,
            fixture,
            rejected_task.object(),
            &rejected_task_value,
        )
        .await?;
        kernel
            .execute_server_delete(
                fixture.delete_task,
                &delete_argument(
                    fixture.delete_task_selector_parameter,
                    fixture.task,
                    rejected_task.object(),
                )?,
            )
            .await?;

        let third_task = kernel
            .execute_server_insert(
                fixture.create_task,
                &task_arguments(fixture, &ExactTask::new(owner.object()))?,
            )
            .await?;
        let arguments = delete_argument(
            fixture.delete_task_selector_parameter,
            fixture.task,
            third_task.object(),
        )?;
        let (proxy_config, proxy) = start_commit_drop_proxy(&database).await?;
        let proxy_kernel = PostgresKernel::new(proxy_config);
        let uncertain = proxy_kernel
            .execute_server_delete(fixture.delete_task, &arguments)
            .await;
        wait_for_proxy(proxy).await?;
        let uncertain = match uncertain {
            Ok(_) => {
                return Err(failure(
                    "withheld DELETE commit confirmation unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        let PostgresKernelError::ServerDelete(ServerDeleteError::CommitOutcomeUnknown {
            context,
            target,
            selector,
            matched,
            ..
        }) = &uncertain
        else {
            return Err(failure(
                "withheld DELETE confirmation did not retain its uncertain outcome",
            ));
        };
        require_context(
            *context,
            second.pair(),
            fixture.delete_task,
            fixture.delete_task_revision,
        )?;
        require(*target == fixture.task, "uncertain delete target differs")?;
        require(
            *selector == third_task.object(),
            "uncertain delete selector differs",
        )?;
        require(*matched, "uncertain delete lost its match state")?;
        require(
            uncertain.to_string()
                == format!(
                    "object deletion failed: the connection failed while deleting object {}; it is not known whether the delete committed; do not retry automatically",
                    third_task.object().canonical(),
                ),
            "uncertain delete lost its no-retry warning",
        )?;
        require(
            count_rows(&database, fixture.task).await? == 0,
            "DELETE outcome tests left an unexpected task row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn confirmed_commit_retains_full_result_when_driver_shutdown_fails() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&kernel, fixture, "owner").await?;
        let exact = ExactTask::new(owner.object());

        let error = kernel
            .execute_server_insert_with_forced_post_commit_driver_shutdown(
                fixture.create_task,
                &task_arguments(fixture, &exact)?,
            )
            .await
            .expect_err("forced post-commit shutdown must retain committed outcome");
        require(
            matches!(
                &error,
                PostgresKernelError::ServerInsert(insert)
                    if insert.commit_state() == ServerInsertCommitState::Committed
            ),
            "post-confirmed-commit shutdown failure has the wrong commit state",
        )?;
        let PostgresKernelError::ServerInsert(ServerInsertError::CommittedButShutdownFailed {
            result,
            ..
        }) = error
        else {
            return Err(failure(
                "post-confirmed-commit error did not retain the committed result",
            ));
        };
        require_insert_result(
            &result,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
            fixture.task,
            "created_task",
        )?;
        require_task_row(&database, fixture, result.object(), &exact).await?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn withheld_commit_confirmation_is_unknown_but_the_row_exists_once() -> TestResult<()> {
    with_test_database(|database| async move {
        let direct_kernel = kernel(&database)?;
        direct_kernel.bootstrap().await?;
        let empty = direct_kernel.recover().await?;
        let applied = direct_kernel
            .apply(&candidate(MUTATION_SOURCE, &empty)?)
            .await?;
        let fixture = Fixture::from_active(&applied)?;
        let owner = insert_owner(&direct_kernel, fixture, "owner").await?;
        let exact = ExactTask::new(owner.object());
        let arguments = task_arguments(fixture, &exact)?;
        let (proxy_config, proxy) = start_commit_drop_proxy(&database).await?;
        let proxy_kernel = PostgresKernel::new(proxy_config);

        let outcome = proxy_kernel
            .execute_server_insert(fixture.create_task, &arguments)
            .await;
        wait_for_proxy(proxy).await?;
        let error = match outcome {
            Ok(_) => {
                return Err(failure(
                    "withheld COMMIT confirmation unexpectedly returned success",
                ));
            }
            Err(error) => error,
        };
        require(
            matches!(
                &error,
                PostgresKernelError::ServerInsert(insert)
                    if insert.commit_state() == ServerInsertCommitState::Unknown
            ),
            "withheld COMMIT confirmation has the wrong commit state",
        )?;
        let PostgresKernelError::ServerInsert(ServerInsertError::CommitOutcomeUnknown {
            context,
            target,
            candidate,
            ..
        }) = &error
        else {
            return Err(failure(
                "withheld COMMIT confirmation did not retain the unknown candidate",
            ));
        };
        require_context(
            *context,
            applied.pair(),
            fixture.create_task,
            fixture.create_task_revision,
        )?;
        require(*target == fixture.task, "unknown commit target differs")?;
        require(
            error.to_string()
                == format!(
                    "row creation failed: the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                    candidate.canonical(),
                ),
            "unknown commit error lost its no-retry warning",
        )?;
        require_task_row(&database, fixture, *candidate, &exact).await?;
        require(
            count_rows(&database, fixture.task).await? == 1,
            "unknown commit outcome did not leave exactly one durable row",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn required_unique_reference_conflicts_are_typed_and_transactional() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel
            .apply(&candidate(UNIQUE_REFERENCE_SOURCE, &empty)?)
            .await?;
        let fixture = UniqueReferenceFixture::from_active(&applied)?;
        install_public_decoy(&database, fixture.assignment).await?;

        let claimed_owner = insert_unique_owner(&kernel, fixture, "claimed").await?;
        let other_owner = insert_unique_owner(&kernel, fixture, "other").await?;
        let concurrent_owner = insert_unique_owner(&kernel, fixture, "concurrent").await?;
        let claimed =
            insert_assignment(&kernel, fixture, claimed_owner.object(), "claimed").await?;
        require_unique_insert_result(
            &claimed,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
            "created_assignment",
        )?;

        let duplicate_insert = match kernel
            .execute_server_insert(
                fixture.create_assignment,
                &assignment_arguments(fixture, claimed_owner.object(), "duplicate")?,
            )
            .await
        {
            Ok(_) => {
                return Err(failure(
                    "a second required unique reference INSERT unexpectedly committed",
                ));
            }
            Err(error) => error,
        };
        require_unique_insert_conflict(
            &duplicate_insert,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed",
        )
        .await?;

        let other = insert_assignment(&kernel, fixture, other_owner.object(), "other").await?;
        require_unique_insert_result(
            &other,
            applied.pair(),
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
            "created_assignment",
        )?;
        let duplicate_update = match kernel
            .execute_server_update(
                fixture.update_assignment,
                &assignment_update_arguments(
                    fixture,
                    other.object(),
                    claimed_owner.object(),
                    "duplicate update",
                )?,
            )
            .await
        {
            Ok(_) => {
                return Err(failure(
                    "an UPDATE assigning an already used reference unexpectedly committed",
                ));
            }
            Err(error) => error,
        };
        require_unique_update_conflict(&duplicate_update, applied.pair(), fixture)?;
        require_assignment_row(
            &database,
            fixture,
            other.object(),
            other_owner.object(),
            "other",
        )
        .await?;

        let self_update = kernel
            .execute_server_update(
                fixture.update_assignment,
                &assignment_update_arguments(
                    fixture,
                    claimed.object(),
                    claimed_owner.object(),
                    "claimed again",
                )?,
            )
            .await?;
        require_unique_update_result(
            &self_update,
            applied.pair(),
            fixture,
            claimed.object(),
            true,
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed again",
        )
        .await?;

        let unrelated = execute_insert_with_installed_trigger(
            &database,
            &kernel,
            fixture.create_assignment,
            fixture.assignment,
            &assignment_arguments(fixture, concurrent_owner.object(), "unrelated")?,
            TriggerKind::UnrelatedUniqueViolation,
            "unrelated unique INSERT",
        )
        .await?;
        require_unrelated_unique_insert_failure(
            &unrelated,
            applied.pair(),
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        require(
            count_rows(&database, fixture.assignment).await? == 2,
            "the unrelated unique violation changed the persisted assignment set",
        )?;

        let first_reached = Arc::new(tokio::sync::Barrier::new(2));
        let first_resume = Arc::new(tokio::sync::Barrier::new(2));
        let second_reached = Arc::new(tokio::sync::Barrier::new(2));
        let second_resume = Arc::new(tokio::sync::Barrier::new(2));
        let first_kernel = kernel.clone();
        let first_arguments = assignment_arguments(fixture, concurrent_owner.object(), "first")?;
        let first_execution_reached = first_reached.clone();
        let first_execution_resume = first_resume.clone();
        let mut first = tokio::spawn(async move {
            first_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create_assignment,
                    &first_arguments,
                    first_execution_reached,
                    first_execution_resume,
                )
                .await
        });
        let second_kernel = kernel.clone();
        let second_arguments = assignment_arguments(fixture, concurrent_owner.object(), "second")?;
        let second_execution_reached = second_reached.clone();
        let second_execution_resume = second_resume.clone();
        let mut second = tokio::spawn(async move {
            second_kernel
                .execute_server_insert_with_test_barrier(
                    fixture.create_assignment,
                    &second_arguments,
                    second_execution_reached,
                    second_execution_resume,
                )
                .await
        });
        if let Err(error) =
            wait_for_barrier(&mut first, first_reached, "first unique claim", "recovery").await
        {
            abort_and_wait(second).await;
            return Err(error);
        }
        if let Err(error) = wait_for_barrier(
            &mut second,
            second_reached,
            "second unique claim",
            "recovery",
        )
        .await
        {
            abort_and_wait(first).await;
            return Err(error);
        }
        let (first_release, second_release) = tokio::join!(
            wait_for_barrier(&mut first, first_resume, "first unique claim", "resume"),
            wait_for_barrier(&mut second, second_resume, "second unique claim", "resume",),
        );
        match (first_release, second_release) {
            (Ok(()), Ok(())) => {}
            (Err(first_error), Ok(())) => {
                abort_and_wait(second).await;
                return Err(first_error);
            }
            (Ok(()), Err(second_error)) => {
                abort_and_wait(first).await;
                return Err(second_error);
            }
            (Err(first_error), Err(second_error)) => {
                return Err(failure(format!(
                    "both unique claim releases failed: {first_error}; {second_error}"
                )));
            }
        }
        let (first_outcome, second_outcome) = tokio::join!(
            wait_for_outcome(first, "first unique claim"),
            wait_for_outcome(second, "second unique claim"),
        );
        let first_outcome = first_outcome?;
        let second_outcome = second_outcome?;
        let outcomes = [first_outcome, second_outcome];
        let successes = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        require(
            successes == 1,
            "concurrent claims did not yield exactly one success",
        )?;
        for error in outcomes.iter().filter_map(|outcome| outcome.as_ref().err()) {
            require_unique_insert_conflict(
                error,
                applied.pair(),
                fixture,
                fixture.create_assignment,
                fixture.create_assignment_revision,
            )?;
        }
        let concurrent_label =
            assignment_label_for_owner(&database, fixture, concurrent_owner.object()).await?;
        require(
            matches!(concurrent_label.as_str(), "first" | "second"),
            "the concurrent winner stored an unexpected assignment value",
        )?;
        require_assignment_row(
            &database,
            fixture,
            claimed.object(),
            claimed_owner.object(),
            "claimed again",
        )
        .await?;
        require_assignment_row(
            &database,
            fixture,
            other.object(),
            other_owner.object(),
            "other",
        )
        .await?;

        require(
            count_rows(&database, fixture.assignment).await? == 3,
            "unique conflicts changed the persisted assignment set",
        )?;
        require(
            count_public_decoy_rows(&database, fixture.assignment).await? == 0,
            "hostile public search_path redirected a unique-reference mutation",
        )?;
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == applied.pair(),
            "unique conflicts changed the active pair",
        )?;
        require(
            function_revision(&recovered, fixture.create_assignment)?
                == fixture.create_assignment_revision
                && function_revision(&recovered, fixture.update_assignment)?
                    == fixture.update_assignment_revision
                && function_revision(&recovered, fixture.create_owner)?
                    == fixture.create_owner_revision,
            "unique conflicts changed immutable function revisions",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[derive(Clone, Copy)]
struct Fixture {
    owner: TypeId,
    owner_name: FieldId,
    task: TypeId,
    active: FieldId,
    count: FieldId,
    amount: FieldId,
    score: FieldId,
    title: FieldId,
    payload: FieldId,
    owner_field: FieldId,
    note: FieldId,
    task_restrict: TypeId,
    task_restrict_field: FieldId,
    task_set_null: TypeId,
    task_set_null_field: FieldId,
    task_cascade: TypeId,
    task_cascade_field: FieldId,
    create_owner: FunctionId,
    create_owner_revision: FunctionRevisionId,
    owner_name_parameter: ParameterId,
    create_task: FunctionId,
    create_task_revision: FunctionRevisionId,
    task_active_parameter: ParameterId,
    task_count_parameter: ParameterId,
    task_amount_parameter: ParameterId,
    task_score_parameter: ParameterId,
    task_title_parameter: ParameterId,
    task_payload_parameter: ParameterId,
    task_owner_parameter: ParameterId,
    update_task: FunctionId,
    update_task_revision: FunctionRevisionId,
    update_selector_parameter: ParameterId,
    update_active_parameter: ParameterId,
    update_count_parameter: ParameterId,
    update_title_parameter: ParameterId,
    update_owner_parameter: ParameterId,
    delete_task: FunctionId,
    delete_task_revision: FunctionRevisionId,
    delete_task_selector_parameter: ParameterId,
    delete_owner: FunctionId,
    delete_owner_revision: FunctionRevisionId,
    delete_owner_selector_parameter: ParameterId,
}

impl Fixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let owner = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["tasks", "owner"]))
            .ok_or_else(|| failure("owner type is absent"))?;
        let task = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["tasks", "task"]))
            .ok_or_else(|| failure("task type is absent"))?;
        let task_restrict = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["tasks", "task_restrict"]))
            .ok_or_else(|| failure("task_restrict type is absent"))?;
        let task_set_null = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["tasks", "task_set_null"]))
            .ok_or_else(|| failure("task_set_null type is absent"))?;
        let task_cascade = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["tasks", "task_cascade"]))
            .ok_or_else(|| failure("task_cascade type is absent"))?;
        let create_owner = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["tasks", "create_owner"]))
            .ok_or_else(|| failure("create_owner function is absent"))?;
        let create_task = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["tasks", "create_task"]))
            .ok_or_else(|| failure("create_task function is absent"))?;
        let update_task = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["tasks", "update_task"]))
            .ok_or_else(|| failure("update_task function is absent"))?;
        let delete_task = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["tasks", "delete_task"]))
            .ok_or_else(|| failure("delete_task function is absent"))?;
        let delete_owner = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| name_is(function.name().parts(), &["tasks", "delete_owner"]))
            .ok_or_else(|| failure("delete_owner function is absent"))?;
        let object_field = |object: &orna_core::catalogue::ObjectTypeDefinition, name| {
            object
                .field_by_name(name)
                .map(|field| field.id())
                .ok_or_else(|| failure(format!("field {name} is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("parameter {name} is absent")))
        };
        Ok(Self {
            owner: owner.id(),
            owner_name: object_field(owner, "name")?,
            task: task.id(),
            active: object_field(task, "active")?,
            count: object_field(task, "count")?,
            amount: object_field(task, "amount")?,
            score: object_field(task, "score")?,
            title: object_field(task, "title")?,
            payload: object_field(task, "payload")?,
            owner_field: object_field(task, "owner")?,
            note: object_field(task, "note")?,
            task_restrict: task_restrict.id(),
            task_restrict_field: object_field(task_restrict, "task")?,
            task_set_null: task_set_null.id(),
            task_set_null_field: object_field(task_set_null, "task")?,
            task_cascade: task_cascade.id(),
            task_cascade_field: object_field(task_cascade, "task")?,
            create_owner: create_owner.id(),
            create_owner_revision: create_owner.current_revision(),
            owner_name_parameter: parameter(create_owner, "p_name")?,
            create_task: create_task.id(),
            create_task_revision: create_task.current_revision(),
            task_active_parameter: parameter(create_task, "p_active")?,
            task_count_parameter: parameter(create_task, "p_count")?,
            task_amount_parameter: parameter(create_task, "p_amount")?,
            task_score_parameter: parameter(create_task, "p_score")?,
            task_title_parameter: parameter(create_task, "p_title")?,
            task_payload_parameter: parameter(create_task, "p_payload")?,
            task_owner_parameter: parameter(create_task, "p_owner")?,
            update_task: update_task.id(),
            update_task_revision: update_task.current_revision(),
            update_selector_parameter: parameter(update_task, "p_task")?,
            update_active_parameter: parameter(update_task, "p_active")?,
            update_count_parameter: parameter(update_task, "p_count")?,
            update_title_parameter: parameter(update_task, "p_title")?,
            update_owner_parameter: parameter(update_task, "p_owner")?,
            delete_task: delete_task.id(),
            delete_task_revision: delete_task.current_revision(),
            delete_task_selector_parameter: parameter(delete_task, "p_task")?,
            delete_owner: delete_owner.id(),
            delete_owner_revision: delete_owner.current_revision(),
            delete_owner_selector_parameter: parameter(delete_owner, "p_owner")?,
        })
    }
}

#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct UniqueReferenceFixture {
    owner: TypeId,
    assignment: TypeId,
    assignment_owner_field: FieldId,
    assignment_label_field: FieldId,
    create_owner: FunctionId,
    create_owner_revision: FunctionRevisionId,
    owner_name_parameter: ParameterId,
    create_assignment: FunctionId,
    create_assignment_revision: FunctionRevisionId,
    create_assignment_owner_parameter: ParameterId,
    create_assignment_label_parameter: ParameterId,
    update_assignment: FunctionId,
    update_assignment_revision: FunctionRevisionId,
    update_assignment_selector_parameter: ParameterId,
    update_assignment_owner_parameter: ParameterId,
    update_assignment_label_parameter: ParameterId,
}

#[cfg(feature = "test-hooks")]
impl UniqueReferenceFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = |name| {
            active
                .catalogue()
                .object_types()
                .iter()
                .find(|object| name_is(object.name().parts(), &["assignments", name]))
                .ok_or_else(|| failure(format!("assignments.{name} type is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["assignments", name]))
                .ok_or_else(|| failure(format!("assignments.{name} function is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("parameter {name} is absent")))
        };
        let owner = object("owner")?;
        let assignment = object("assignment")?;
        let create_owner = function("create_owner")?;
        let create_assignment = function("create_assignment")?;
        let update_assignment = function("update_assignment")?;
        Ok(Self {
            owner: owner.id(),
            assignment: assignment.id(),
            assignment_owner_field: assignment
                .field_by_name("owner")
                .map(|field| field.id())
                .ok_or_else(|| failure("assignments.assignment.owner field is absent"))?,
            assignment_label_field: assignment
                .field_by_name("label")
                .map(|field| field.id())
                .ok_or_else(|| failure("assignments.assignment.label field is absent"))?,
            create_owner: create_owner.id(),
            create_owner_revision: create_owner.current_revision(),
            owner_name_parameter: parameter(create_owner, "p_name")?,
            create_assignment: create_assignment.id(),
            create_assignment_revision: create_assignment.current_revision(),
            create_assignment_owner_parameter: parameter(create_assignment, "p_owner")?,
            create_assignment_label_parameter: parameter(create_assignment, "p_label")?,
            update_assignment: update_assignment.id(),
            update_assignment_revision: update_assignment.current_revision(),
            update_assignment_selector_parameter: parameter(update_assignment, "p_assignment")?,
            update_assignment_owner_parameter: parameter(update_assignment, "p_owner")?,
            update_assignment_label_parameter: parameter(update_assignment, "p_label")?,
        })
    }
}

#[derive(Clone)]
struct ExactTask {
    active: bool,
    count: i32,
    amount: i64,
    score: f64,
    title: String,
    payload: Vec<u8>,
    owner: ObjectId,
}

struct StoredTaskRow {
    object: Vec<u8>,
    active: bool,
    count: i32,
    amount: i64,
    score: f64,
    title: String,
    payload: Vec<u8>,
    owner: Vec<u8>,
    note: Option<String>,
}

impl ExactTask {
    fn new(owner: ObjectId) -> Self {
        Self {
            active: false,
            count: 42,
            amount: 420_000,
            score: 1.5,
            title: String::from("task"),
            payload: vec![4, 2],
            owner,
        }
    }
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

fn standard_application_candidate(
    source: &str,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )
    .map_err(|error| failure(format!("standard application context failed: {error}")))?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check_standard_application(&bundle, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "standard application diagnostics prevented mutation preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
}

fn require_standard_mutation_catalogue(
    active: &ActiveDatabaseRevision,
    fixture: Fixture,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let context_standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("standard-backed mutation active revision has no standard pin"))?;
    require(
        context_standard.revision() == standard.revision()
            && context_standard.catalogue().revision() == standard.catalogue().revision()
            && context_standard.digest_version() == standard.digest_version()
            && context_standard.digest() == standard.digest(),
        "standard-backed mutation selected an unexpected standard revision",
    )?;
    let text = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let boolean = orna_standard::BOOLEAN_TYPE_ID;
    let integer = orna_standard::INTEGER_TYPE_ID;
    let bigint = orna_standard::BIGINT_TYPE_ID;
    let float = orna_standard::FLOAT_TYPE_ID;
    let bytes = orna_standard::BINARY_LARGE_OBJECT_TYPE_ID;

    for (object, field, expected, description) in [
        (fixture.owner, fixture.owner_name, text, "tasks.owner.name"),
        (fixture.task, fixture.active, boolean, "tasks.task.active"),
        (fixture.task, fixture.count, integer, "tasks.task.count"),
        (fixture.task, fixture.amount, bigint, "tasks.task.amount"),
        (fixture.task, fixture.score, float, "tasks.task.score"),
        (fixture.task, fixture.title, text, "tasks.task.title"),
        (fixture.task, fixture.payload, bytes, "tasks.task.payload"),
        (fixture.task, fixture.note, text, "tasks.task.note"),
    ] {
        require_value_type(
            object_field_type(active, object, field)?,
            expected,
            standard,
            description,
        )?;
    }
    require_reference_type(
        object_field_type(active, fixture.task, fixture.owner_field)?,
        fixture.owner,
        "tasks.task.owner",
    )?;

    for (function, parameter, expected, description) in [
        (
            fixture.create_owner,
            fixture.owner_name_parameter,
            text,
            "tasks.create_owner.p_name",
        ),
        (
            fixture.create_task,
            fixture.task_active_parameter,
            boolean,
            "tasks.create_task.p_active",
        ),
        (
            fixture.create_task,
            fixture.task_count_parameter,
            integer,
            "tasks.create_task.p_count",
        ),
        (
            fixture.create_task,
            fixture.task_amount_parameter,
            bigint,
            "tasks.create_task.p_amount",
        ),
        (
            fixture.create_task,
            fixture.task_score_parameter,
            float,
            "tasks.create_task.p_score",
        ),
        (
            fixture.create_task,
            fixture.task_title_parameter,
            text,
            "tasks.create_task.p_title",
        ),
        (
            fixture.create_task,
            fixture.task_payload_parameter,
            bytes,
            "tasks.create_task.p_payload",
        ),
        (
            fixture.update_task,
            fixture.update_active_parameter,
            boolean,
            "tasks.update_task.p_active",
        ),
        (
            fixture.update_task,
            fixture.update_count_parameter,
            integer,
            "tasks.update_task.p_count",
        ),
        (
            fixture.update_task,
            fixture.update_title_parameter,
            text,
            "tasks.update_task.p_title",
        ),
    ] {
        require_value_type(
            parameter_type(active, function, parameter)?,
            expected,
            standard,
            description,
        )?;
    }
    for (function, parameter, target, description) in [
        (
            fixture.create_task,
            fixture.task_owner_parameter,
            fixture.owner,
            "tasks.create_task.p_owner",
        ),
        (
            fixture.update_task,
            fixture.update_selector_parameter,
            fixture.task,
            "tasks.update_task.p_task",
        ),
        (
            fixture.update_task,
            fixture.update_owner_parameter,
            fixture.owner,
            "tasks.update_task.p_owner",
        ),
        (
            fixture.delete_task,
            fixture.delete_task_selector_parameter,
            fixture.task,
            "tasks.delete_task.p_task",
        ),
    ] {
        require_reference_type(
            parameter_type(active, function, parameter)?,
            target,
            description,
        )?;
    }
    for (function, target, description) in [
        (
            fixture.create_owner,
            fixture.owner,
            "tasks.create_owner.created_owner",
        ),
        (
            fixture.create_task,
            fixture.task,
            "tasks.create_task.created_task",
        ),
        (
            fixture.update_task,
            fixture.task,
            "tasks.update_task.updated_task",
        ),
    ] {
        require_reference_type(rows_return_type(active, function, 0)?, target, description)?;
    }
    require_value_type(
        rows_return_type(active, fixture.delete_task, 0)?,
        boolean,
        standard,
        "tasks.delete_task.deleted",
    )?;
    Ok(())
}

fn object_field_type(
    active: &ActiveDatabaseRevision,
    object_id: TypeId,
    field_id: FieldId,
) -> TestResult<ResolvedType> {
    active
        .catalogue()
        .object_types()
        .iter()
        .find(|object| object.id() == object_id)
        .and_then(|object| object.field_by_id(field_id))
        .map(|field| field.resolved_type())
        .ok_or_else(|| failure("standard mutation field is absent"))
}

fn parameter_type(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
    parameter_id: ParameterId,
) -> TestResult<ResolvedType> {
    active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.id() == function_id)
        .and_then(|function| function.parameter_by_id(parameter_id))
        .map(|parameter| parameter.resolved_type())
        .ok_or_else(|| failure("standard mutation parameter is absent"))
}

fn rows_return_type(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
    ordinal: u32,
) -> TestResult<ResolvedType> {
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.id() == function_id)
        .ok_or_else(|| failure("standard mutation function is absent"))?;
    let orna_core::catalogue::FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(failure("standard mutation function does not return ROWS"));
    };
    columns
        .iter()
        .find(|column| column.ordinal() == ordinal)
        .map(|column| column.resolved_type())
        .ok_or_else(|| failure("standard mutation ROWS column is absent"))
}

fn require_value_type(
    resolved: ResolvedType,
    expected: TypeId,
    standard: &VerifiedStandardLibrarySnapshot,
    description: &str,
) -> TestResult<()> {
    require(
        resolved == ResolvedType::value(expected)
            && standard.catalogue().value_type_by_id(expected).is_some(),
        format!("{description} did not retain the exact standard Value identity"),
    )
}

fn require_reference_type(
    resolved: ResolvedType,
    expected: TypeId,
    description: &str,
) -> TestResult<()> {
    require(
        resolved == ResolvedType::reference(expected),
        format!("{description} did not retain the exact REF identity"),
    )
}

async fn insert_owner(
    kernel: &PostgresKernel,
    fixture: Fixture,
    name: &str,
) -> TestResult<ServerInsertResult> {
    Ok(kernel
        .execute_server_insert(
            fixture.create_owner,
            &[FunctionArgument::new(
                fixture.owner_name_parameter,
                RuntimeValue::Text(name.to_owned()),
            )?],
        )
        .await?)
}

#[cfg(feature = "test-hooks")]
async fn insert_unique_owner(
    kernel: &PostgresKernel,
    fixture: UniqueReferenceFixture,
    name: &str,
) -> TestResult<ServerInsertResult> {
    Ok(kernel
        .execute_server_insert(
            fixture.create_owner,
            &[FunctionArgument::new(
                fixture.owner_name_parameter,
                RuntimeValue::Text(name.to_owned()),
            )?],
        )
        .await?)
}

#[cfg(feature = "test-hooks")]
async fn insert_assignment(
    kernel: &PostgresKernel,
    fixture: UniqueReferenceFixture,
    owner: ObjectId,
    label: &str,
) -> TestResult<ServerInsertResult> {
    Ok(kernel
        .execute_server_insert(
            fixture.create_assignment,
            &assignment_arguments(fixture, owner, label)?,
        )
        .await?)
}

#[cfg(feature = "test-hooks")]
fn assignment_arguments(
    fixture: UniqueReferenceFixture,
    owner: ObjectId,
    label: &str,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.create_assignment_label_parameter,
            RuntimeValue::Text(label.to_owned()),
        )?,
        FunctionArgument::new(
            fixture.create_assignment_owner_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: owner,
            },
        )?,
    ])
}

#[cfg(feature = "test-hooks")]
fn assignment_update_arguments(
    fixture: UniqueReferenceFixture,
    selector: ObjectId,
    owner: ObjectId,
    label: &str,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.update_assignment_label_parameter,
            RuntimeValue::Text(label.to_owned()),
        )?,
        FunctionArgument::new(
            fixture.update_assignment_selector_parameter,
            RuntimeValue::Reference {
                target: fixture.assignment,
                object: selector,
            },
        )?,
        FunctionArgument::new(
            fixture.update_assignment_owner_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: owner,
            },
        )?,
    ])
}

fn task_arguments(fixture: Fixture, task: &ExactTask) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.task_payload_parameter,
            RuntimeValue::Bytes(task.payload.clone()),
        )?,
        FunctionArgument::new(
            fixture.task_owner_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: task.owner,
            },
        )?,
        FunctionArgument::new(
            fixture.task_title_parameter,
            RuntimeValue::Text(task.title.clone()),
        )?,
        FunctionArgument::new(
            fixture.task_score_parameter,
            RuntimeValue::Float(RuntimeFloat::new(task.score)?),
        )?,
        FunctionArgument::new(
            fixture.task_amount_parameter,
            RuntimeValue::BigInt(task.amount),
        )?,
        FunctionArgument::new(
            fixture.task_count_parameter,
            RuntimeValue::Integer(task.count),
        )?,
        FunctionArgument::new(
            fixture.task_active_parameter,
            RuntimeValue::Boolean(task.active),
        )?,
    ])
}

fn update_arguments(
    fixture: Fixture,
    selector: ObjectId,
    task: &ExactTask,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.update_owner_parameter,
            RuntimeValue::Reference {
                target: fixture.owner,
                object: task.owner,
            },
        )?,
        FunctionArgument::new(
            fixture.update_title_parameter,
            RuntimeValue::Text(task.title.clone()),
        )?,
        FunctionArgument::new(
            fixture.update_selector_parameter,
            RuntimeValue::Reference {
                target: fixture.task,
                object: selector,
            },
        )?,
        FunctionArgument::new(
            fixture.update_count_parameter,
            RuntimeValue::Integer(task.count),
        )?,
        FunctionArgument::new(
            fixture.update_active_parameter,
            RuntimeValue::Boolean(task.active),
        )?,
    ])
}

fn delete_argument(
    parameter: ParameterId,
    target: TypeId,
    selector: ObjectId,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target,
            object: selector,
        },
    )?])
}

fn replace_owner_argument(
    arguments: &mut [FunctionArgument],
    fixture: Fixture,
    value: RuntimeValue,
) -> TestResult<()> {
    let slot = arguments
        .iter_mut()
        .find(|argument| argument.parameter() == fixture.task_owner_parameter)
        .ok_or_else(|| failure("task owner argument is absent"))?;
    *slot = FunctionArgument::new(fixture.task_owner_parameter, value)?;
    Ok(())
}

fn require_insert_result(
    result: &ServerInsertResult,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    target: TypeId,
    return_column: &str,
) -> TestResult<()> {
    require(
        result.context().pair() == pair,
        "insert context pair differs",
    )?;
    require(
        result.context().function() == function,
        "insert context function differs",
    )?;
    require(
        result.context().function_revision() == revision,
        "insert context function revision differs",
    )?;
    require(result.pair() == pair, "insert result pair differs")?;
    require(
        result.function() == function,
        "insert result function differs",
    )?;
    require(
        result.function_revision() == revision,
        "insert result function revision differs",
    )?;
    require(result.target() == target, "insert result target differs")?;
    let [column] = result.rows().columns() else {
        return Err(failure("insert result does not have exactly one column"));
    };
    require(
        column.name() == return_column,
        "insert result lost its declared return-column name",
    )?;
    require(
        column.resolved_type() == ResolvedType::reference(target),
        "insert result column has the wrong reference type",
    )?;
    require(!column.nullable(), "insert result column became nullable")?;
    let [row] = result.rows().rows() else {
        return Err(failure("insert result does not have exactly one row"));
    };
    require(
        row.values()
            == [RuntimeValue::Reference {
                target,
                object: result.object(),
            }],
        "insert result row is not the allocated typed reference",
    )
}

fn require_update_result(
    result: &ServerUpdateResult,
    pair: RevisionPair,
    fixture: Fixture,
    selector: ObjectId,
    matched: bool,
) -> TestResult<()> {
    require(
        result.context().pair() == pair,
        "update context pair differs",
    )?;
    require(
        result.context().function() == fixture.update_task,
        "update context function differs",
    )?;
    require(
        result.context().function_revision() == fixture.update_task_revision,
        "update context function revision differs",
    )?;
    require(result.pair() == pair, "update result pair differs")?;
    require(
        result.function() == fixture.update_task,
        "update result function differs",
    )?;
    require(
        result.function_revision() == fixture.update_task_revision,
        "update result function revision differs",
    )?;
    require(result.target() == fixture.task, "update target differs")?;
    require(result.selector() == selector, "update selector differs")?;
    require(result.matched() == matched, "update match state differs")?;
    let [column] = result.rows().columns() else {
        return Err(failure("update result does not have exactly one column"));
    };
    require(
        column.name() == "updated_task",
        "update result lost its declared return-column name",
    )?;
    require(
        column.resolved_type() == ResolvedType::reference(fixture.task),
        "update result column has the wrong reference type",
    )?;
    require(!column.nullable(), "update result column became nullable")?;
    if matched {
        let [row] = result.rows().rows() else {
            return Err(failure("matched update does not have exactly one row"));
        };
        require(
            row.values()
                == [RuntimeValue::Reference {
                    target: fixture.task,
                    object: selector,
                }],
            "matched update did not return the selected typed reference",
        )
    } else {
        require(
            result.rows().rows().is_empty(),
            "absent update returned a row",
        )
    }
}

#[cfg(feature = "test-hooks")]
fn require_unique_insert_result(
    result: &ServerInsertResult,
    pair: RevisionPair,
    fixture: UniqueReferenceFixture,
    function: FunctionId,
    revision: FunctionRevisionId,
    return_column: &str,
) -> TestResult<()> {
    require_context(result.context(), pair, function, revision)?;
    require(
        result.target() == fixture.assignment,
        "unique INSERT target differs",
    )?;
    let [column] = result.rows().columns() else {
        return Err(failure("unique INSERT result does not have one column"));
    };
    require(
        column.name() == return_column
            && column.resolved_type() == ResolvedType::reference(fixture.assignment)
            && !column.nullable(),
        "unique INSERT result column differs",
    )?;
    let [row] = result.rows().rows() else {
        return Err(failure("unique INSERT result does not have one row"));
    };
    require(
        row.values()
            == [RuntimeValue::Reference {
                target: fixture.assignment,
                object: result.object(),
            }],
        "unique INSERT result row differs",
    )
}

#[cfg(feature = "test-hooks")]
fn require_unique_insert_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: UniqueReferenceFixture,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure(
            "unique INSERT conflict is not a SERVER INSERT error",
        ));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "unique INSERT conflict has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure(
            "unique INSERT conflict lacks pinned execution context",
        ));
    };
    require_context(*context, pair, function, revision)?;
    let unique @ ServerMutationError::UniqueReferenceConflict {
        owner,
        field: conflict_field,
        referenced_type,
        source: database_source,
    } = source.as_ref()
    else {
        return Err(failure(
            "unique INSERT was not classified as a typed reference conflict",
        ));
    };
    require(
        *owner == fixture.assignment,
        "unique INSERT conflict owner differs",
    )?;
    require(
        *conflict_field == fixture.assignment_owner_field,
        "unique INSERT conflict field differs",
    )?;
    require(
        *referenced_type == fixture.owner,
        "unique INSERT conflict referenced type differs",
    )?;
    require(
        database_source
            .as_db_error()
            .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION),
        "unique INSERT conflict lost SQLSTATE 23505",
    )?;
    require(
        database_source
            .as_db_error()
            .and_then(|database| database.constraint())
            == Some(unique_constraint_name(fixture.assignment_owner_field).as_str()),
        "unique INSERT conflict constraint differs",
    )?;
    require(
        unique.to_string() == "this reference is already used by another object",
        "unique INSERT inner display differs",
    )?;
    require(
        error.to_string()
            == "row creation failed: the row was not added: this reference is already used by another object",
        "unique INSERT outer display differs",
    )?;
    Ok(())
}

#[cfg(feature = "test-hooks")]
fn require_unique_update_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: UniqueReferenceFixture,
) -> TestResult<()> {
    let PostgresKernelError::ServerUpdate(update) = error else {
        return Err(failure(
            "unique UPDATE conflict is not a SERVER UPDATE error",
        ));
    };
    require(
        update.commit_state() == ServerUpdateCommitState::NotCommitted,
        "unique UPDATE conflict has the wrong commit state",
    )?;
    let ServerUpdateError::NotCommitted { context, source } = update else {
        return Err(failure(
            "unique UPDATE conflict lacks pinned execution context",
        ));
    };
    require_context(
        *context,
        pair,
        fixture.update_assignment,
        fixture.update_assignment_revision,
    )?;
    let unique @ ServerMutationError::UniqueReferenceConflict {
        owner,
        field: conflict_field,
        referenced_type,
        source: database_source,
    } = source.as_ref()
    else {
        return Err(failure(
            "unique UPDATE was not classified as a typed reference conflict",
        ));
    };
    require(
        *owner == fixture.assignment,
        "unique UPDATE conflict owner differs",
    )?;
    require(
        *conflict_field == fixture.assignment_owner_field,
        "unique UPDATE conflict field differs",
    )?;
    require(
        *referenced_type == fixture.owner,
        "unique UPDATE conflict referenced type differs",
    )?;
    require(
        database_source
            .as_db_error()
            .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION),
        "unique UPDATE conflict lost SQLSTATE 23505",
    )?;
    require(
        database_source
            .as_db_error()
            .and_then(|database| database.constraint())
            == Some(unique_constraint_name(fixture.assignment_owner_field).as_str()),
        "unique UPDATE conflict constraint differs",
    )?;
    require(
        unique.to_string() == "this reference is already used by another object",
        "unique UPDATE inner display differs",
    )?;
    require(
        error.to_string()
            == "object update failed: the object was not updated: this reference is already used by another object",
        "unique UPDATE outer display differs",
    )?;
    Ok(())
}

#[cfg(feature = "test-hooks")]
fn require_unique_update_result(
    result: &ServerUpdateResult,
    pair: RevisionPair,
    fixture: UniqueReferenceFixture,
    selector: ObjectId,
    matched: bool,
) -> TestResult<()> {
    require(result.context().pair() == pair, "self-update pair differs")?;
    require(
        result.context().function() == fixture.update_assignment,
        "self-update function differs",
    )?;
    require(
        result.context().function_revision() == fixture.update_assignment_revision,
        "self-update function revision differs",
    )?;
    require(
        result.target() == fixture.assignment,
        "self-update target differs",
    )?;
    require(
        result.selector() == selector,
        "self-update selector differs",
    )?;
    require(
        result.matched() == matched,
        "self-update match state differs",
    )?;
    let [column] = result.rows().columns() else {
        return Err(failure("self-update result does not have one column"));
    };
    require(
        column.name() == "updated_assignment"
            && column.resolved_type() == ResolvedType::reference(fixture.assignment)
            && !column.nullable(),
        "self-update result column differs",
    )?;
    let [row] = result.rows().rows() else {
        return Err(failure("self-update result does not have one row"));
    };
    require(
        row.values()
            == [RuntimeValue::Reference {
                target: fixture.assignment,
                object: selector,
            }],
        "self-update result row differs",
    )
}

#[cfg(feature = "test-hooks")]
fn require_unrelated_unique_insert_failure(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure(
            "unrelated unique violation is not a SERVER INSERT error",
        ));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "unrelated unique violation has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure(
            "unrelated unique violation lacks pinned execution context",
        ));
    };
    require_context(*context, pair, function, revision)?;
    let ServerMutationError::Database { source } = source.as_ref() else {
        return Err(failure(
            "unrelated SQLSTATE 23505 was incorrectly typed as a reference conflict",
        ));
    };
    require(
        source
            .as_db_error()
            .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION),
        "unrelated unique violation lost SQLSTATE 23505",
    )?;
    require(
        source
            .as_db_error()
            .and_then(|database| database.constraint())
            == Some("test_unrelated_unique"),
        "unrelated unique violation constraint differs",
    )?;
    require(
        error.to_string()
            == "row creation failed: the row was not added: the database operation failed before the change was saved",
        "unrelated unique violation lost its generic display",
    )?;
    Ok(())
}

fn require_delete_result(
    result: &ServerDeleteResult,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    target: TypeId,
    selector: ObjectId,
    matched: bool,
) -> TestResult<()> {
    require_context(result.context(), pair, function, revision)?;
    require(result.pair() == pair, "delete result pair differs")?;
    require(
        result.function() == function,
        "delete result function differs",
    )?;
    require(
        result.function_revision() == revision,
        "delete result function revision differs",
    )?;
    require(result.target() == target, "delete target differs")?;
    require(result.selector() == selector, "delete selector differs")?;
    require(result.matched() == matched, "delete match state differs")?;
    let [column] = result.rows().columns() else {
        return Err(failure("delete result does not have exactly one column"));
    };
    require(
        column.name() == "deleted",
        "delete result lost its declared return-column name",
    )?;
    require(
        column.resolved_type() == ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
        "delete result column is not BOOLEAN",
    )?;
    require(!column.nullable(), "delete result column became nullable")?;
    if matched {
        let [row] = result.rows().rows() else {
            return Err(failure("matched delete does not have exactly one row"));
        };
        require(
            row.values() == [RuntimeValue::Boolean(true)],
            "matched delete did not return TRUE",
        )
    } else {
        require(
            result.rows().rows().is_empty(),
            "absent delete returned a row",
        )
    }
}

async fn require_owner_row(
    database: &TestDatabase,
    fixture: Fixture,
    object: ObjectId,
    expected_name: &str,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(Vec<u8>, String)> = async {
        let row = session
            .client()
            .query_one(
                &format!(
                    "SELECT _orna_object_id, {} FROM {} WHERE _orna_object_id = $1",
                    field(fixture.owner_name),
                    relation(fixture.owner),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    let (stored_object, stored_name) =
        finish_session(session, operation, "owner row inspection").await?;
    require(
        stored_object == object.to_bytes(),
        "returned owner identity differs from the stored identity",
    )?;
    require(stored_name == expected_name, "stored owner name differs")
}

async fn require_task_row(
    database: &TestDatabase,
    fixture: Fixture,
    object: ObjectId,
    expected: &ExactTask,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<StoredTaskRow> = async {
        let row = session
            .client()
            .query_one(
                &format!(
                    "SELECT _orna_object_id, {}, {}, {}, {}, {}, {}, {}, {} FROM {} \
                     WHERE _orna_object_id = $1",
                    field(fixture.active),
                    field(fixture.count),
                    field(fixture.amount),
                    field(fixture.score),
                    field(fixture.title),
                    field(fixture.payload),
                    field(fixture.owner_field),
                    field(fixture.note),
                    relation(fixture.task),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?;
        Ok(StoredTaskRow {
            object: row.try_get(0)?,
            active: row.try_get(1)?,
            count: row.try_get(2)?,
            amount: row.try_get(3)?,
            score: row.try_get(4)?,
            title: row.try_get(5)?,
            payload: row.try_get(6)?,
            owner: row.try_get(7)?,
            note: row.try_get(8)?,
        })
    }
    .await;
    let stored = finish_session(session, operation, "task row inspection").await?;
    require(
        stored.object == object.to_bytes(),
        "returned task identity differs from the stored identity",
    )?;
    require(stored.active == expected.active, "stored BOOL differs")?;
    require(stored.count == expected.count, "stored INT differs")?;
    require(stored.amount == expected.amount, "stored BIGINT differs")?;
    require(stored.score == expected.score, "stored FLOAT differs")?;
    require(stored.title == expected.title, "stored TEXT differs")?;
    require(stored.payload == expected.payload, "stored BYTES differs")?;
    require(
        stored.owner == expected.owner.to_bytes(),
        "stored REF differs",
    )?;
    require(stored.note.is_none(), "omitted nullable field is not NULL")
}

#[cfg(feature = "test-hooks")]
async fn require_assignment_row(
    database: &TestDatabase,
    fixture: UniqueReferenceFixture,
    object: ObjectId,
    owner: ObjectId,
    label: &str,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(Vec<u8>, Vec<u8>, String)> = async {
        let row = session
            .client()
            .query_one(
                &format!(
                    "SELECT _orna_object_id, {}, {} FROM {} WHERE _orna_object_id = $1",
                    field(fixture.assignment_owner_field),
                    field(fixture.assignment_label_field),
                    relation(fixture.assignment),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?))
    }
    .await;
    let (stored_object, stored_owner, stored_label) =
        finish_session(session, operation, "unique assignment row inspection").await?;
    require(
        stored_object == object.to_bytes(),
        "unique assignment identity differs",
    )?;
    require(
        stored_owner == owner.to_bytes(),
        "unique assignment owner differs",
    )?;
    require(stored_label == label, "unique assignment label differs")
}

#[cfg(feature = "test-hooks")]
async fn assignment_label_for_owner(
    database: &TestDatabase,
    fixture: UniqueReferenceFixture,
    owner: ObjectId,
) -> TestResult<String> {
    let session = database.open().await?;
    let operation: TestResult<String> = async {
        Ok(session
            .client()
            .query_one(
                &format!(
                    "SELECT {} FROM {} WHERE {} = $1",
                    field(fixture.assignment_label_field),
                    relation(fixture.assignment),
                    field(fixture.assignment_owner_field),
                ),
                &[&owner.to_bytes().to_vec()],
            )
            .await?
            .try_get(0)?)
    }
    .await;
    finish_session(session, operation, "concurrent assignment inspection").await
}

async fn install_public_decoy(database: &TestDatabase, target: TypeId) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "CREATE TABLE public.{} (_orna_object_id bytea)",
                relation_component(target),
            ))
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "public decoy installation").await
}

async fn count_public_decoy_rows(database: &TestDatabase, target: TypeId) -> TestResult<i64> {
    let session = database.open().await?;
    let operation: TestResult<i64> = async {
        Ok(session
            .client()
            .query_one(
                &format!("SELECT count(*) FROM public.{}", relation_component(target)),
                &[],
            )
            .await?
            .try_get(0)?)
    }
    .await;
    finish_session(session, operation, "public decoy row count").await
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

async fn insert_reference_fixture_row(
    database: &TestDatabase,
    object_type: TypeId,
    reference_field: FieldId,
    object: ObjectId,
    referenced_object: ObjectId,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .execute(
                &format!(
                    "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
                    relation(object_type),
                    field(reference_field),
                ),
                &[
                    &object.to_bytes().to_vec(),
                    &referenced_object.to_bytes().to_vec(),
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "reference-policy fixture insertion").await
}

async fn delete_fixture_row(
    database: &TestDatabase,
    object_type: TypeId,
    object: ObjectId,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .execute(
                &format!(
                    "DELETE FROM {} WHERE _orna_object_id = $1",
                    relation(object_type),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "reference-policy fixture removal").await
}

async fn reference_fixture_value(
    database: &TestDatabase,
    object_type: TypeId,
    reference_field: FieldId,
    object: ObjectId,
) -> TestResult<Option<Vec<u8>>> {
    let session = database.open().await?;
    let operation: TestResult<Option<Vec<u8>>> = async {
        Ok(session
            .client()
            .query_one(
                &format!(
                    "SELECT {} FROM {} WHERE _orna_object_id = $1",
                    field(reference_field),
                    relation(object_type),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?
            .try_get(0)?)
    }
    .await;
    finish_session(session, operation, "reference-policy fixture inspection").await
}

fn require_not_committed_argument_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure("argument rejection is not a SERVER INSERT error"));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "argument rejection has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure("argument rejection lacks its pinned context"));
    };
    require_context(*context, pair, function, revision)?;
    require(
        matches!(source.as_ref(), ServerInsertError::Argument { .. }),
        "wrong-target REF did not fail argument validation",
    )
}

#[cfg(feature = "test-hooks")]
fn require_commit_rejected(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    target: TypeId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure("commit rejection is not a SERVER INSERT error"));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "commit rejection has the wrong commit state",
    )?;
    let ServerInsertError::CommitRejected {
        context,
        target: rejected_target,
        source,
        ..
    } = insert
    else {
        return Err(failure("failure did not occur during COMMIT"));
    };
    require_context(*context, pair, function, revision)?;
    require(
        *rejected_target == target,
        "commit rejection target differs",
    )?;
    let code = source
        .as_db_error()
        .map(|error| error.code())
        .ok_or_else(|| failure("commit rejection has no database error code"))?;
    require(
        code == &SqlState::RAISE_EXCEPTION,
        "deferred trigger commit error code differs",
    )
}

#[cfg(feature = "test-hooks")]
fn require_delete_commit_rejected(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: Fixture,
    selector: ObjectId,
) -> TestResult<()> {
    let PostgresKernelError::ServerDelete(delete) = error else {
        return Err(failure("commit rejection is not a SERVER DELETE error"));
    };
    require(
        delete.commit_state() == ServerDeleteCommitState::NotCommitted,
        "delete commit rejection has the wrong commit state",
    )?;
    let ServerDeleteError::CommitRejected {
        context,
        target,
        selector: rejected_selector,
        matched,
        source,
    } = delete
    else {
        return Err(failure("DELETE failure did not occur during COMMIT"));
    };
    require_context(
        *context,
        pair,
        fixture.delete_task,
        fixture.delete_task_revision,
    )?;
    require(*target == fixture.task, "delete rejection target differs")?;
    require(
        *rejected_selector == selector,
        "delete rejection selector differs",
    )?;
    require(*matched, "delete rejection lost its match state")?;
    let code = source
        .as_db_error()
        .map(|error| error.code())
        .ok_or_else(|| failure("delete commit rejection has no database error code"))?;
    require(
        code == &SqlState::RAISE_EXCEPTION,
        "deferred delete trigger error code differs",
    )
}

fn require_wrapped_database_failure(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    expected_code: &SqlState,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure(
            "database write failure is not a SERVER INSERT error",
        ));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "database write failure has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure("database write failure lacks its pinned context"));
    };
    require_context(*context, pair, function, revision)?;
    let ServerInsertError::Database { source } = source.as_ref() else {
        return Err(failure("failure did not occur during the database write"));
    };
    let code = source
        .as_db_error()
        .map(|error| error.code())
        .ok_or_else(|| failure("database write failure has no database error code"))?;
    require(code == expected_code, "database write error code differs")
}

fn require_update_database_failure(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: Fixture,
) -> TestResult<()> {
    let PostgresKernelError::ServerUpdate(update) = error else {
        return Err(failure(
            "database write failure is not a SERVER UPDATE error",
        ));
    };
    require(
        update.commit_state() == ServerUpdateCommitState::NotCommitted,
        "database update failure has the wrong commit state",
    )?;
    let ServerUpdateError::NotCommitted { context, source } = update else {
        return Err(failure("database update failure lacks its pinned context"));
    };
    require_context(
        *context,
        pair,
        fixture.update_task,
        fixture.update_task_revision,
    )?;
    let ServerMutationError::Database { source } = source.as_ref() else {
        return Err(failure("failure did not occur during the database update"));
    };
    let code = source
        .as_db_error()
        .map(|error| error.code())
        .ok_or_else(|| failure("database update failure has no database error code"))?;
    require(
        code == &SqlState::FOREIGN_KEY_VIOLATION,
        "database update error code differs",
    )
}

fn require_delete_restricted(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    expected_target: TypeId,
    selector: ObjectId,
    expected_code: &SqlState,
) -> TestResult<()> {
    let PostgresKernelError::ServerDelete(delete) = error else {
        return Err(failure(
            "reference restriction is not a SERVER DELETE error",
        ));
    };
    require(
        delete.commit_state() == ServerDeleteCommitState::NotCommitted,
        "reference restriction has the wrong commit state",
    )?;
    let ServerDeleteError::DeleteRestricted {
        context,
        target,
        selector: rejected_selector,
        source,
    } = delete
    else {
        return Err(failure(format!(
            "dependent reference did not produce DeleteRestricted: {delete:?}",
        )));
    };
    require_context(*context, pair, function, revision)?;
    require(
        *target == expected_target,
        "restricted delete target differs",
    )?;
    require(
        *rejected_selector == selector,
        "restricted delete selector differs",
    )?;
    let code = source
        .as_db_error()
        .map(|error| error.code())
        .ok_or_else(|| failure("reference restriction has no database error code"))?;
    require(
        code == expected_code,
        "reference restriction error code differs",
    )?;
    require(
        error.to_string()
            == format!(
                "object deletion failed: object {} cannot be deleted because another object still refers to it",
                selector.canonical(),
            ),
        "reference restriction exposed an internal constraint detail",
    )
}

fn require_context(
    context: orna_postgres::ServerInsertContext,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    require(context.pair() == pair, "error context pair differs")?;
    require(
        context.function() == function,
        "error context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "error context function revision differs",
    )
}

#[derive(Clone, Copy)]
enum Tamper {
    Artifact,
    Reference,
}

async fn assert_tamper_rejected_before_insert(tamper: Tamper) -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let applied = kernel.apply(&candidate(MUTATION_SOURCE, &empty)?).await?;
        let fixture = Fixture::from_active(&applied)?;
        let session = database.open().await?;
        let operation: TestResult<u64> = async {
            match tamper {
                Tamper::Artifact => Ok(session
                    .client()
                    .execute(
                        "UPDATE _orna_kernel.function_artifacts SET payload = $1 \
                         WHERE function_revision_id = $2",
                        &[
                            &vec![0_u8],
                            &fixture.create_task_revision.to_bytes().to_vec(),
                        ],
                    )
                    .await?),
                Tamper::Reference => Ok(session
                    .client()
                    .execute(
                        "UPDATE _orna_kernel.definition_references SET ordinal = ordinal + 1000 \
                         WHERE catalogue_revision_id = $1 AND source_function_id = $2 \
                         AND ordinal = (SELECT max(ordinal) FROM _orna_kernel.definition_references \
                           WHERE catalogue_revision_id = $1 AND source_function_id = $2)",
                        &[
                            &applied.pair().catalogue().to_bytes().to_vec(),
                            &fixture.create_task.to_bytes().to_vec(),
                        ],
                    )
                    .await?),
            }
        }
        .await;
        let changed = finish_session(session, operation, "durable function tamper").await?;
        require(changed == 1, "tamper fixture changed the wrong row count")?;

        let error = kernel
            .execute_server_insert(fixture.create_task, &[])
            .await
            .expect_err("tampered durable function must fail before target INSERT");
        let PostgresKernelError::ServerInsert(ServerInsertError::Kernel { source }) = &error else {
            return Err(failure(
                "tampered function did not fail during active database recovery",
            ));
        };
        let expected_relation = match tamper {
            Tamper::Artifact => "_orna_kernel.function_artifacts",
            Tamper::Reference => "_orna_kernel.definition_references",
        };
        require(
            matches!(
                source.as_ref(),
                PostgresKernelError::DurableInvariant { relation, .. }
                    if *relation == expected_relation
            ),
            format!(
                "tampered function recovery source was not a durable invariant for \
                 {expected_relation}: {source:?}"
            ),
        )?;
        require_unchanged_state(&database, fixture.task, applied.pair(), 0).await?;
        require_no_session_leaks(&database).await
    })
    .await
}

async fn require_unchanged_state(
    database: &TestDatabase,
    target: TypeId,
    pair: RevisionPair,
    expected_rows: i64,
) -> TestResult<()> {
    require(
        count_rows(database, target).await? == expected_rows,
        "failed INSERT changed the target row count",
    )?;
    let session = database.open().await?;
    let operation: TestResult<(Vec<u8>, Vec<u8>)> = async {
        let row = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id \
                 FROM _orna_kernel.active_revision WHERE singleton",
                &[],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    let (source, catalogue) =
        finish_session(session, operation, "active revision inspection").await?;
    require(
        source == pair.source().to_bytes(),
        "failed INSERT changed the active source revision",
    )?;
    require(
        catalogue == pair.catalogue().to_bytes(),
        "failed INSERT changed the active catalogue revision",
    )
}

#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
enum TriggerKind {
    AfterRow,
    DeferredConstraint,
    DeferredDeleteConstraint,
    UnrelatedUniqueViolation,
}

#[cfg(feature = "test-hooks")]
async fn execute_delete_with_installed_trigger(
    database: &TestDatabase,
    kernel: &PostgresKernel,
    fixture: Fixture,
    selector: ObjectId,
) -> TestResult<PostgresKernelError> {
    let reached = Arc::new(tokio::sync::Barrier::new(2));
    let resume = Arc::new(tokio::sync::Barrier::new(2));
    let executor = kernel.clone();
    let arguments = delete_argument(
        fixture.delete_task_selector_parameter,
        fixture.task,
        selector,
    )?;
    let execution_reached = reached.clone();
    let execution_resume = resume.clone();
    let execution = tokio::spawn(async move {
        executor
            .execute_server_delete_with_test_barrier(
                fixture.delete_task,
                &arguments,
                execution_reached,
                execution_resume,
            )
            .await
    });
    finish_triggered_failure(
        database,
        fixture.task,
        TriggerKind::DeferredDeleteConstraint,
        execution,
        reached,
        resume,
        "triggered delete",
    )
    .await
}

#[cfg(feature = "test-hooks")]
async fn execute_insert_with_installed_trigger(
    database: &TestDatabase,
    kernel: &PostgresKernel,
    function: FunctionId,
    target: TypeId,
    arguments: &[FunctionArgument],
    kind: TriggerKind,
    operation: &str,
) -> TestResult<PostgresKernelError> {
    let reached = Arc::new(tokio::sync::Barrier::new(2));
    let resume = Arc::new(tokio::sync::Barrier::new(2));
    let executor = kernel.clone();
    let owned_arguments = arguments.to_vec();
    let execution_reached = reached.clone();
    let execution_resume = resume.clone();
    let execution = tokio::spawn(async move {
        executor
            .execute_server_insert_with_test_barrier(
                function,
                &owned_arguments,
                execution_reached,
                execution_resume,
            )
            .await
    });
    finish_triggered_failure(
        database, target, kind, execution, reached, resume, operation,
    )
    .await
}

#[cfg(feature = "test-hooks")]
async fn finish_triggered_failure<T>(
    database: &TestDatabase,
    target: TypeId,
    kind: TriggerKind,
    mut execution: tokio::task::JoinHandle<Result<T, PostgresKernelError>>,
    reached: Arc<tokio::sync::Barrier>,
    resume: Arc<tokio::sync::Barrier>,
    operation: &str,
) -> TestResult<PostgresKernelError> {
    wait_for_barrier(&mut execution, reached, operation, "recovery").await?;
    let install = install_failure_trigger(database, target, kind).await;
    if let Err(error) = install {
        abort_and_wait(execution).await;
        return Err(error);
    }
    if let Err(resume_error) = wait_for_barrier(&mut execution, resume, operation, "resume").await {
        let cleanup = remove_failure_trigger(database, target, kind).await;
        return match cleanup {
            Ok(()) => Err(resume_error),
            Err(cleanup_error) => Err(failure(format!(
                "{operation} did not resume: {resume_error}; trigger cleanup failed: {cleanup_error}"
            ))),
        };
    }
    let outcome = wait_for_failure(execution, operation).await;
    let cleanup = remove_failure_trigger(database, target, kind).await;
    match (outcome, cleanup) {
        (Ok(error), Ok(())) => Ok(error),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(execution_error), Err(cleanup_error)) => Err(failure(format!(
            "{operation} failed: {execution_error}; trigger cleanup failed: {cleanup_error}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
async fn install_failure_trigger(
    database: &TestDatabase,
    target: TypeId,
    kind: TriggerKind,
) -> TestResult<()> {
    let (function_name, trigger_sql, trigger_body) = match kind {
        TriggerKind::AfterRow => (
            "test_fail_after_insert",
            "CREATE TRIGGER test_fail_after_insert AFTER INSERT",
            "RAISE EXCEPTION 'forced insert failure';",
        ),
        TriggerKind::DeferredConstraint => (
            "test_fail_deferred_insert",
            "CREATE CONSTRAINT TRIGGER test_fail_deferred_insert AFTER INSERT",
            "RAISE EXCEPTION 'forced insert failure';",
        ),
        TriggerKind::DeferredDeleteConstraint => (
            "test_fail_deferred_delete",
            "CREATE CONSTRAINT TRIGGER test_fail_deferred_delete AFTER DELETE",
            "RAISE EXCEPTION 'forced insert failure';",
        ),
        TriggerKind::UnrelatedUniqueViolation => (
            "test_unrelated_unique",
            "CREATE TRIGGER test_unrelated_unique BEFORE INSERT",
            "RAISE EXCEPTION USING ERRCODE = 'unique_violation', CONSTRAINT = 'test_unrelated_unique';",
        ),
    };
    let deferred = match kind {
        TriggerKind::AfterRow => "",
        TriggerKind::DeferredConstraint | TriggerKind::DeferredDeleteConstraint => {
            " DEFERRABLE INITIALLY DEFERRED"
        }
        TriggerKind::UnrelatedUniqueViolation => "",
    };
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "CREATE FUNCTION _orna_data.{function_name}() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN {trigger_body} END; $$; \
                 {trigger_sql} ON {}{deferred} FOR EACH ROW \
                 EXECUTE FUNCTION _orna_data.{function_name}()",
                relation(target),
            ))
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "failure trigger installation").await
}

#[cfg(feature = "test-hooks")]
async fn remove_failure_trigger(
    database: &TestDatabase,
    target: TypeId,
    kind: TriggerKind,
) -> TestResult<()> {
    let name = match kind {
        TriggerKind::AfterRow => "test_fail_after_insert",
        TriggerKind::DeferredConstraint => "test_fail_deferred_insert",
        TriggerKind::DeferredDeleteConstraint => "test_fail_deferred_delete",
        TriggerKind::UnrelatedUniqueViolation => "test_unrelated_unique",
    };
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "DROP TRIGGER IF EXISTS {name} ON {}; \
                 DROP FUNCTION IF EXISTS _orna_data.{name}()",
                relation(target),
            ))
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "failure trigger removal").await
}

#[cfg(feature = "test-hooks")]
async fn wait_for_barrier<T>(
    task: &mut tokio::task::JoinHandle<T>,
    barrier: Arc<tokio::sync::Barrier>,
    operation: &str,
    phase: &str,
) -> TestResult<()> {
    if tokio::time::timeout(WAIT, barrier.wait()).await.is_ok() {
        Ok(())
    } else {
        task.abort();
        let _ = task.await;
        Err(failure(format!(
            "{operation} did not reach the {phase} barrier"
        )))
    }
}

#[cfg(feature = "test-hooks")]
async fn wait_for_success<T>(
    mut task: tokio::task::JoinHandle<Result<T, PostgresKernelError>>,
    operation: &str,
) -> TestResult<T> {
    match tokio::time::timeout(WAIT, &mut task).await {
        Ok(result) => result
            .map_err(|error| failure(format!("{operation} task failed: {error}")))?
            .map_err(|error| failure(format!("{operation} failed: {error}"))),
        Err(_) => {
            abort_and_wait(task).await;
            Err(failure(format!("{operation} exceeded the bounded wait")))
        }
    }
}

#[cfg(feature = "test-hooks")]
async fn wait_for_failure<T>(
    mut task: tokio::task::JoinHandle<Result<T, PostgresKernelError>>,
    operation: &str,
) -> TestResult<PostgresKernelError> {
    match tokio::time::timeout(WAIT, &mut task).await {
        Ok(result) => {
            match result.map_err(|error| failure(format!("{operation} task failed: {error}")))? {
                Ok(_) => Err(failure(format!("{operation} unexpectedly committed"))),
                Err(error) => Ok(error),
            }
        }
        Err(_) => {
            abort_and_wait(task).await;
            Err(failure(format!("{operation} exceeded the bounded wait")))
        }
    }
}

#[cfg(feature = "test-hooks")]
async fn wait_for_outcome<T>(
    mut task: tokio::task::JoinHandle<Result<T, PostgresKernelError>>,
    operation: &str,
) -> TestResult<Result<T, PostgresKernelError>> {
    match tokio::time::timeout(WAIT, &mut task).await {
        Ok(result) => result.map_err(|error| failure(format!("{operation} task failed: {error}"))),
        Err(_) => {
            abort_and_wait(task).await;
            Err(failure(format!("{operation} exceeded the bounded wait")))
        }
    }
}

#[cfg(feature = "test-hooks")]
async fn abort_and_wait<T>(task: tokio::task::JoinHandle<T>) {
    task.abort();
    let _ = task.await;
}

#[cfg(feature = "test-hooks")]
fn function_revision(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> TestResult<FunctionRevisionId> {
    active
        .catalogue()
        .function_by_id(function)
        .map(|definition| definition.current_revision())
        .ok_or_else(|| failure("INSERT function is absent from the active catalogue"))
}

#[cfg(feature = "test-hooks")]
async fn start_commit_drop_proxy(
    database: &TestDatabase,
) -> TestResult<(Config, ThreadJoinHandle<TestResult<()>>)> {
    let base = database.config()?;
    let upstream = configured_tcp_address(&base)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    let proxy_config = proxy_config(&base, address.port())?;
    let proxy = std::thread::spawn(move || run_commit_drop_proxy(listener, upstream));
    Ok((proxy_config, proxy))
}

#[cfg(feature = "test-hooks")]
fn configured_tcp_address(config: &Config) -> TestResult<SocketAddr> {
    let host = match config.get_hosts().first() {
        Some(Host::Tcp(host)) => host,
        #[cfg(unix)]
        Some(Host::Unix(_)) => {
            return Err(failure(
                "commit-drop proxy requires a TCP PostgreSQL test connection",
            ));
        }
        None => return Err(failure("PostgreSQL test connection has no configured host")),
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);
    (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| failure("PostgreSQL test host did not resolve to a TCP address"))
}

#[cfg(feature = "test-hooks")]
fn proxy_config(base: &Config, port: u16) -> TestResult<Config> {
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.port(port);
    config.ssl_mode(SslMode::Disable);
    if let Some(user) = base.get_user() {
        config.user(user);
    }
    if let Some(password) = base.get_password() {
        config.password(password);
    }
    if let Some(database) = base.get_dbname() {
        config.dbname(database);
    }
    if let Some(options) = base.get_options() {
        config.options(options);
    }
    if config.get_dbname().is_none() {
        return Err(failure("proxy kernel has no target database"));
    }
    Ok(config)
}

#[cfg(feature = "test-hooks")]
fn run_commit_drop_proxy(listener: TcpListener, upstream: SocketAddr) -> TestResult<()> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + WAIT;
    let (client, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(failure("commit-drop proxy accepted no client connection"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    };
    let backend = TcpStream::connect_timeout(&upstream, WAIT)?;
    client.set_nodelay(true)?;
    backend.set_nodelay(true)?;
    client.set_read_timeout(Some(WAIT))?;
    client.set_write_timeout(Some(WAIT))?;
    backend.set_read_timeout(Some(WAIT))?;
    backend.set_write_timeout(Some(WAIT))?;

    let commit_seen = Arc::new(AtomicBool::new(false));
    let frontend_client = client.try_clone()?;
    let frontend_backend = backend.try_clone()?;
    let frontend_commit = commit_seen.clone();
    let frontend = std::thread::spawn(move || {
        forward_frontend(frontend_client, frontend_backend, &frontend_commit)
    });
    let backend_result = forward_backend_until_committed(&client, backend, &commit_seen);
    let _ = client.shutdown(Shutdown::Both);
    let frontend_result = frontend
        .join()
        .map_err(|_| failure("commit-drop proxy frontend thread panicked"))?;
    match backend_result {
        Ok(()) => Ok(()),
        Err(error) => {
            frontend_result?;
            Err(error)
        }
    }
}

#[cfg(feature = "test-hooks")]
fn forward_frontend(
    mut client: TcpStream,
    mut backend: TcpStream,
    commit_seen: &AtomicBool,
) -> TestResult<()> {
    let mut length = [0_u8; 4];
    client.read_exact(&mut length)?;
    let length = checked_frame_length(length)?;
    let mut startup = vec![0_u8; length - 4];
    client.read_exact(&mut startup)?;
    backend.write_all(&(length as u32).to_be_bytes())?;
    backend.write_all(&startup)?;
    backend.flush()?;

    loop {
        let (tag, payload) = match read_protocol_frame(&mut client) {
            Ok(frame) => frame,
            Err(_) if commit_seen.load(Ordering::SeqCst) => return Ok(()),
            Err(error) => return Err(error),
        };
        if tag == b'Q' && payload == b"COMMIT\0" {
            // Arm the backend interceptor before the server can acknowledge
            // the COMMIT that this thread is about to forward.
            commit_seen.store(true, Ordering::SeqCst);
        }
        write_protocol_frame(&mut backend, tag, &payload)?;
    }
}

#[cfg(feature = "test-hooks")]
fn forward_backend_until_committed(
    client: &TcpStream,
    mut backend: TcpStream,
    commit_seen: &AtomicBool,
) -> TestResult<()> {
    let mut client = client.try_clone()?;
    loop {
        let (tag, payload) = read_protocol_frame(&mut backend)?;
        if commit_seen.load(Ordering::SeqCst) && tag == b'C' && payload == b"COMMIT\0" {
            return Ok(());
        }
        write_protocol_frame(&mut client, tag, &payload)?;
    }
}

#[cfg(feature = "test-hooks")]
fn read_protocol_frame(stream: &mut TcpStream) -> TestResult<(u8, Vec<u8>)> {
    let mut tag = [0_u8; 1];
    stream.read_exact(&mut tag)?;
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = checked_frame_length(length)?;
    let mut payload = vec![0_u8; length - 4];
    stream.read_exact(&mut payload)?;
    Ok((tag[0], payload))
}

#[cfg(feature = "test-hooks")]
fn write_protocol_frame(stream: &mut TcpStream, tag: u8, payload: &[u8]) -> TestResult<()> {
    let length = payload
        .len()
        .checked_add(4)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| failure("PostgreSQL proxy frame length overflowed"))?;
    stream.write_all(&[tag])?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

#[cfg(feature = "test-hooks")]
fn checked_frame_length(bytes: [u8; 4]) -> TestResult<usize> {
    const MAX_FRAME_LENGTH: usize = 64 * 1024 * 1024;
    let length = u32::from_be_bytes(bytes) as usize;
    if (4..=MAX_FRAME_LENGTH).contains(&length) {
        Ok(length)
    } else {
        Err(failure("PostgreSQL proxy received an invalid frame length"))
    }
}

#[cfg(feature = "test-hooks")]
async fn wait_for_proxy(proxy: ThreadJoinHandle<TestResult<()>>) -> TestResult<()> {
    tokio::task::spawn_blocking(move || proxy.join())
        .await
        .map_err(|error| failure(format!("commit-drop proxy join task failed: {error}")))?
        .map_err(|_| failure("commit-drop proxy thread panicked"))?
}

async fn require_no_session_leaks(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(i64, i64)> = async {
        let row = session
            .client()
            .query_one(
                "SELECT count(*) FILTER (WHERE state = 'idle in transaction'), \
                        count(*) FILTER (WHERE pid <> pg_catalog.pg_backend_pid()) \
                 FROM pg_catalog.pg_stat_activity \
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
    context: &str,
) -> TestResult<T> {
    let shutdown = session.shutdown().await;
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(shutdown_error)) => Err(failure(format!(
            "{context} failed: {operation_error}; connection shutdown also failed: {shutdown_error}"
        ))),
    }
}

fn relation(type_id: TypeId) -> String {
    format!("_orna_data.{}", relation_component(type_id))
}

fn relation_component(type_id: TypeId) -> String {
    format!("t_{:032x}", u128::from_be_bytes(type_id.to_bytes()))
}

fn field(field_id: FieldId) -> String {
    format!("f_{:032x}", u128::from_be_bytes(field_id.to_bytes()))
}

#[cfg(feature = "test-hooks")]
fn unique_constraint_name(field_id: FieldId) -> String {
    format!("uq_{:032x}", u128::from_be_bytes(field_id.to_bytes()))
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
