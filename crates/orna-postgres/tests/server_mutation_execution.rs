//! Live PostgreSQL tests for atomic single-row SERVER mutation execution.

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
#[cfg(feature = "test-hooks")]
use orna_core::security::{
    AuthenticatedSession, CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
    ExecuteDenial, ExecuteGrant, InvocationTarget, SecurityAuditKind, SecurityAuditOutcome,
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
#[cfg(feature = "test-hooks")]
use orna_postgres::AuthenticatedRawCallResult;
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

#[cfg(feature = "test-hooks")]
const RAW_INSERT_SOURCE: &str = "CREATE SCHEMA raw_insert_test;\n\
    CREATE TYPE raw_insert_test.probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL,\n\
      note TEXT\n\
    );\n\
    CREATE SERVER FUNCTION raw_insert_test.create_probe()\n\
    RETURNS ROWS (created REF raw_insert_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_insert_test.probe AS made (stored)\n\
    VALUES (TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_insert_test.read_probes()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.stored FROM raw_insert_test.probe probe;\n\
    CREATE SERVER FUNCTION raw_insert_test.create_named(p_name TEXT)\n\
    RETURNS ROWS (created REF raw_insert_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_insert_test.probe AS made (stored, note)\n\
    VALUES (TRUE, p_name) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_insert_test.create_flagged(p_stored BOOLEAN)\n\
    RETURNS ROWS (created REF raw_insert_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_insert_test.probe AS made (stored)\n\
    VALUES (p_stored) RETURNING REF(made);\n\
    CREATE CLIENT FUNCTION raw_insert_test.client_boolean()\n\
    RETURNS BOOLEAN RETURN TRUE;\n";

#[cfg(feature = "test-hooks")]
const RAW_REFERENCE_UPDATE_SOURCE: &str = "CREATE SCHEMA raw_reference_test;\n\
    CREATE TYPE raw_reference_test.probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL,\n\
      linked REF raw_reference_test.probe\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_test.create_probe()\n\
    RETURNS ROWS (created REF raw_reference_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_test.probe AS made (stored)\n\
    VALUES (TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_reference_test.update_false(p_probe REF raw_reference_test.probe)\n\
    RETURNS ROWS (updated REF raw_reference_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_test.probe AS alias\n\
    SET stored = FALSE\n\
    WHERE REF(alias) = p_probe\n\
    RETURNING REF(alias);\n\
    CREATE SERVER FUNCTION raw_reference_test.delete_probe(p_probe REF raw_reference_test.probe)\n\
    RETURNS ROWS (deleted BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS DELETE FROM raw_reference_test.probe AS alias\n\
    WHERE REF(alias) = p_probe\n\
    RETURNING TRUE;\n\
    CREATE SERVER FUNCTION raw_reference_test.read_probes()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.stored FROM raw_reference_test.probe probe;\n\
    CREATE SERVER FUNCTION raw_reference_test.update_link(p_probe REF raw_reference_test.probe)\n\
    RETURNS ROWS (updated REF raw_reference_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_test.probe AS alias\n\
    SET linked = p_probe\n\
    WHERE REF(alias) = p_probe\n\
    RETURNING REF(alias);\n\
    CREATE SERVER FUNCTION raw_reference_test.read_links()\n\
    RETURNS ROWS (linked REF raw_reference_test.probe)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.linked FROM raw_reference_test.probe probe;\n";

#[cfg(feature = "test-hooks")]
const SERVICE_UID: u32 = 61_018;

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

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_insert_is_denied_then_granted_and_audited() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_insert_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_insert_test", "create_probe"])?;
        let read_probes = raw_function_id(&applied, &["raw_insert_test", "read_probes"])?;
        let create_named = raw_function_id(&applied, &["raw_insert_test", "create_named"])?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // The parameter-free raw INSERT is denied before its explicit grant.
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, create_probe)
            .await
            .expect_err("raw INSERT before its grant must be denied");
        require(
            matches!(denied, PostgresKernelError::RawExecuteDenied { .. }),
            "pre-grant raw INSERT returned the wrong typed error",
        )?;

        // Grant the raw SELECT only and prove the denied INSERT created nothing.
        kernel
            .grant_catalogue_health_service_execute(pair, read_probes)
            .await?;
        let empty_select = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                empty_select,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the denied INSERT must not create any object",
        )?;

        // Grant the parameter-free raw INSERT only and invoke it.
        kernel
            .grant_catalogue_health_service_execute(pair, create_probe)
            .await?;
        let inserted = kernel
            .dispatch_authenticated_raw_call(&session, create_probe)
            .await?;
        let inserted = match inserted {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference { target, object } = &inserted[0] else {
            return Err(failure("raw INSERT must return an object reference"));
        };
        require(
            *target == probe && *object != ObjectId::from_bytes([0; 16]),
            "raw INSERT reference must name the probe type and a real row",
        )?;

        // The raw SELECT now proves exactly one object exists.
        let one_probe = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                one_probe,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "raw SELECT must return exactly the stored Boolean true value",
        )?;

        // An allowed-but-invalid raw mutation target closes as an unavailable
        // authorised raw call target under ADR 0040.
        kernel
            .grant_catalogue_health_service_execute(pair, create_named)
            .await?;
        let unavailable = kernel
            .dispatch_authenticated_raw_call(&session, create_named)
            .await
            .expect_err("a parameterised raw INSERT target must be unavailable");
        require(
            matches!(
                unavailable,
                PostgresKernelError::RawCallTargetUnavailable { .. }
            ),
            "an invalid raw mutation target returned the wrong typed error",
        )?;
        let unchanged = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                unchanged,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "an invalid raw mutation target must not change any row",
        )?;

        // One authentication audit, then one audit per dispatch: the pre-grant
        // call was denied, every later call allowed.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 7, "raw dispatch audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[2..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                }),
            "raw dispatch audit kinds and outcomes differ",
        )?;

        // ADR 0040: a parameterised Boolean SERVER INSERT target stores its
        // Boolean argument. Its exact ParameterId comes from the compiled
        // active catalogue, never from source text or a fixed identity.
        let create_flagged = raw_function_id(&applied, &["raw_insert_test", "create_flagged"])?;
        let create_flagged_definition = applied
            .catalogue()
            .function_by_id(create_flagged)
            .ok_or_else(|| failure("create_flagged is absent from the active catalogue"))?;
        let stored_parameter = create_flagged_definition
            .parameter_by_name("p_stored")
            .ok_or_else(|| failure("create_flagged.p_stored is absent from the active catalogue"))?
            .id();
        let create_flagged_revision = create_flagged_definition.current_revision();
        let mut wrong_parameter_bytes = stored_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != stored_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let wrong_parameter_argument =
            FunctionArgument::new(wrong_parameter, RuntimeValue::Boolean(true))?;
        let client_boolean = raw_function_id(&applied, &["raw_insert_test", "client_boolean"])?;

        // Authorisation wins over argument validation: before its grant, even a
        // wrong-parameter Boolean call is denied and creates no row.
        let denied_with_argument = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                std::slice::from_ref(&wrong_parameter_argument),
            )
            .await
            .expect_err("an ungranted raw INSERT must be denied before argument validation");
        require(
            matches!(
                denied_with_argument,
                PostgresKernelError::RawExecuteDenied { .. }
            ),
            "pre-grant raw INSERT with arguments returned the wrong typed error",
        )?;
        let after_denied = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_denied,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the denied raw INSERT with arguments must not create any row",
        )?;

        // After the grant, the wrong ParameterId fails as a generic unavailable
        // raw target, rolls back its savepoint, and keeps the allowed audit.
        kernel
            .grant_catalogue_health_service_execute(pair, create_flagged)
            .await?;
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                std::slice::from_ref(&wrong_parameter_argument),
            )
            .await
            .expect_err("a wrong parameter id must make the raw INSERT target unavailable");
        require(
            matches!(
                wrong_parameter_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { .. }
            ),
            "a wrong parameter id returned the wrong typed error",
        )?;
        let after_wrong_parameter = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_wrong_parameter,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "a wrong parameter id must not create any row",
        )?;

        // A real PostgreSQL row write then fails through an AFTER INSERT
        // trigger. The raw dispatch pauses after recovery while the harness
        // installs the trigger, then resumes and fails the write. The typed
        // ServerInsert database failure must survive the raw dispatch
        // unchanged, and the tentative row must roll back.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let triggered_arguments = vec![FunctionArgument::new(
            stored_parameter,
            RuntimeValue::Boolean(true),
        )?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    create_flagged,
                    &triggered_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        // The helper waits for recovery, installs the trigger, resumes the
        // dispatch, awaits the task, and removes the trigger before it
        // returns. Cleanup runs even when the dispatch task fails, times out,
        // or unexpectedly commits.
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterRow,
            execution,
            reached,
            resume,
            "triggered raw dispatch",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted {
                context,
                source,
            }) => (context, source),
            other => {
                return Err(failure(format!(
                    "triggered raw dispatch returned {other:?}"
                )));
            }
        };
        require_context(
            context,
            applied.pair(),
            create_flagged,
            create_flagged_revision,
        )?;
        let source = match source.as_ref() {
            ServerInsertError::Database { source } => source,
            other => {
                return Err(failure(format!(
                    "triggered raw dispatch returned {other:?}"
                )));
            }
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("triggered raw dispatch has no database error code"))?;
        require(
            code == &SqlState::RAISE_EXCEPTION,
            "triggered raw dispatch error code differs",
        )?;
        let after_trigger = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        require(
            matches!(
                after_trigger,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the triggered raw INSERT must roll back its tentative row",
        )?;

        // The exact ParameterId binds TRUE then FALSE, each returning a real
        // probe reference with a distinct nonzero object identity.
        let inserted_true = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                &[FunctionArgument::new(
                    stored_parameter,
                    RuntimeValue::Boolean(true),
                )?],
            )
            .await?;
        let inserted_false = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_flagged,
                &[FunctionArgument::new(
                    stored_parameter,
                    RuntimeValue::Boolean(false),
                )?],
            )
            .await?;
        let true_values = match inserted_true {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "the TRUE raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: true_target,
            object: true_object,
        } = &true_values[0]
        else {
            return Err(failure("the TRUE raw INSERT must return a probe reference"));
        };
        let false_values = match inserted_false {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "the FALSE raw INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: false_target,
            object: false_object,
        } = &false_values[0]
        else {
            return Err(failure(
                "the FALSE raw INSERT must return a probe reference",
            ));
        };
        require(
            *true_target == probe
                && *true_object != ObjectId::from_bytes([0; 16])
                && *false_target == probe
                && *false_object != ObjectId::from_bytes([0; 16])
                && true_object != false_object,
            "argument raw INSERTs must return distinct real probe references",
        )?;

        // The stored Boolean multiset now contains the parameter-free TRUE, the
        // argument TRUE, and the argument FALSE, in no particular row order.
        let multiset = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        let multiset = match multiset {
            AuthenticatedRawCallResult::Server(values) => values,
            other => {
                return Err(failure(format!(
                    "raw SELECT must return Server values, got {other:?}"
                )));
            }
        };
        require(
            multiset.len() == 3
                && multiset
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2
                && multiset
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "raw SELECT must return the parameter-free TRUE plus the argument TRUE and FALSE",
        )?;

        // One Boolean argument rejects every non-INSERT raw target: the health
        // intrinsic, an active CLIENT function, and the granted SERVER SELECT.
        kernel
            .grant_catalogue_health_service_execute(pair, client_boolean)
            .await?;
        for (target, label) in [
            (CATALOGUE_HEALTH_FUNCTION_ID, "the health intrinsic"),
            (client_boolean, "the active CLIENT function"),
            (read_probes, "the granted SERVER SELECT"),
        ] {
            let rejected = kernel
                .dispatch_authenticated_raw_call_with_arguments(
                    &session,
                    target,
                    std::slice::from_ref(&wrong_parameter_argument),
                )
                .await
                .expect_err("a Boolean argument must reject a non-INSERT raw target");
            require(
                matches!(
                    rejected,
                    PostgresKernelError::RawCallTargetUnavailable { .. }
                ),
                format!("{label} with an argument returned the wrong typed error"),
            )?;
        }
        let after_rejected = kernel
            .dispatch_authenticated_raw_call(&session, read_probes)
            .await?;
        let after_rejected = match after_rejected {
            AuthenticatedRawCallResult::Server(values) => values,
            other => {
                return Err(failure(format!(
                    "raw SELECT must return Server values, got {other:?}"
                )));
            }
        };
        require(
            after_rejected.len() == 3
                && after_rejected
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2
                && after_rejected
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "rejected argument calls must not change any stored row",
        )?;

        // Audit index 7 is the denied wrong-parameter call before its grant.
        // Audit index 9 is the allowed wrong-parameter call: its audit survived
        // the savepoint rollback. Audit index 11 is the triggered write
        // failure: its allowed audit survived that rollback, then every later
        // dispatch was allowed.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 20, "raw dispatch audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().kind() == SecurityAuditKind::Execute
                && audits[1].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[2..7].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                })
                && audits[7].decision().kind() == SecurityAuditKind::Execute
                && audits[7].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[7]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[8..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                        && event.decision().outcome() == SecurityAuditOutcome::Allowed
                })
                && audits[9]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[11]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(create_flagged)
                && audits[16]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(CATALOGUE_HEALTH_FUNCTION_ID)
                && audits[17]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(client_boolean)
                && audits[18]
                    .decision()
                    .target()
                    .map(InvocationTarget::function)
                    == Some(read_probes),
            "raw dispatch audit kinds, outcomes, and targets differ",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, read_probes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_named),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_flagged),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, client_boolean),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the five fixed-service grants",
        )?;

        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
async fn create_probe_reference(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    create_probe: FunctionId,
) -> TestResult<RuntimeValue> {
    let created = kernel
        .dispatch_authenticated_raw_call(session, create_probe)
        .await?;
    let created = match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference { target, object } = &created[0] else {
        return Err(failure("raw INSERT must return an object reference"));
    };
    require(
        *object != ObjectId::from_bytes([0; 16]),
        "the created reference must name a real row",
    )?;
    Ok(RuntimeValue::Reference {
        target: *target,
        object: *object,
    })
}

#[cfg(feature = "test-hooks")]
async fn read_probe_values(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    read_probes: FunctionId,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, read_probes)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_reference_mutation_authority_and_selection() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_false = raw_function_id(&applied, &["raw_reference_test", "update_false"])?;
        let delete_probe = raw_function_id(&applied, &["raw_reference_test", "delete_probe"])?;
        let read_probes = raw_function_id(&applied, &["raw_reference_test", "read_probes"])?;
        let update_parameter = applied
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_false.p_probe is absent from the active catalogue"))?
            .id();
        let delete_parameter = applied
            .catalogue()
            .function_by_id(delete_probe)
            .ok_or_else(|| failure("delete_probe is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("delete_probe.p_probe is absent from the active catalogue"))?
            .id();
        let mut wrong_parameter_bytes = update_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the writer and the reader, so the reference mutations
        // stay unauthorised for the denial proof.
        for function in [create_probe, read_probes] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create two distinct rows and retain both exact references.
        let first = create_probe_reference(&kernel, &session, create_probe).await?;
        let second = create_probe_reference(&kernel, &session, create_probe).await?;
        let RuntimeValue::Reference {
            target: first_target,
            object: first_object,
        } = &first
        else {
            return Err(failure("first created value is not a reference"));
        };
        let RuntimeValue::Reference {
            target: second_target,
            object: second_object,
        } = &second
        else {
            return Err(failure("second created value is not a reference"));
        };
        require(
            first_target == second_target && *first_target != TypeId::from_bytes([0; 16]),
            "both created references must share one nonzero target type",
        )?;
        require(
            *first_object != *second_object
                && *first_object != ObjectId::from_bytes([0; 16])
                && *second_object != ObjectId::from_bytes([0; 16]),
            "the two created references must name distinct nonzero rows",
        )?;
        let mut wrong_target_bytes = first_target.to_bytes();
        wrong_target_bytes[0] ^= 0x01;
        let wrong_target_id = TypeId::from_bytes(wrong_target_bytes);
        require(
            wrong_target_id != *first_target,
            "the deliberately wrong target must differ from the created target",
        )?;

        // A wrong-binding reference UPDATE before its grant is denied before
        // binding validation, and both rows stay unchanged.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(wrong_parameter, first.clone())?],
            )
            .await
            .expect_err("reference UPDATE before its grant must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == update_false
            ),
            "pre-grant wrong-binding UPDATE returned the wrong typed denial",
        )?;
        let two_true = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            two_true.len() == 2
                && two_true
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the denied reference UPDATE must leave both rows TRUE",
        )?;

        // Grant the two reference mutations.
        for function in [update_false, delete_probe] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // The same wrong-binding argument is rejected after the grant as an
        // unavailable raw target, retaining an allowed audit.
        let wrong_binding = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(wrong_parameter, first.clone())?],
            )
            .await
            .expect_err("the same wrong-binding argument must reject the reference UPDATE");
        require(
            matches!(
                wrong_binding,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_false
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "the wrong-binding argument returned the wrong typed error",
        )?;
        let wrong_target = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(
                    update_parameter,
                    RuntimeValue::Reference {
                        target: wrong_target_id,
                        object: *first_object,
                    },
                )?],
            )
            .await
            .expect_err("a wrong target TypeId must reject the reference UPDATE");
        require(
            matches!(
                wrong_target,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_false
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "a wrong target TypeId returned the wrong typed error",
        )?;
        let unchanged = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            unchanged.len() == 2
                && unchanged
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the rejected reference UPDATEs must leave both rows TRUE",
        )?;

        // The UPDATE selects exactly the first row: the reader returns one
        // FALSE and one TRUE in no particular order.
        let updated = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(update_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                updated,
                AuthenticatedRawCallResult::Server(values)
                    if values == [first.clone()]
            ),
            "the reference UPDATE must return the identical input reference",
        )?;
        let mixed = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            mixed.len() == 2
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 1
                && mixed
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(false))
                    .count()
                    == 1,
            "the UPDATE must select exactly one row: one FALSE and one TRUE value",
        )?;

        // DELETE selects exactly the first row, then repeats as an empty
        // success, leaving the second row in place.
        let deleted = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                delete_probe,
                &[FunctionArgument::new(delete_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                deleted,
                AuthenticatedRawCallResult::Server(values)
                    if values == [RuntimeValue::Boolean(true)]
            ),
            "the reference DELETE must return exactly one TRUE value",
        )?;
        let one_true = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            one_true == [RuntimeValue::Boolean(true)],
            "the reference DELETE must leave exactly the second row TRUE",
        )?;

        // An exact UPDATE using the deleted reference matches no row and
        // completes empty, leaving the surviving row unchanged.
        let updated_deleted = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_false,
                &[FunctionArgument::new(update_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                updated_deleted,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the UPDATE of a deleted reference must complete with an empty value list",
        )?;
        let still_one_after_update = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            still_one_after_update == [RuntimeValue::Boolean(true)],
            "the UPDATE of a deleted reference must leave the surviving row unchanged",
        )?;

        let repeated = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                delete_probe,
                &[FunctionArgument::new(delete_parameter, first.clone())?],
            )
            .await?;
        require(
            matches!(
                repeated,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the repeated reference DELETE must complete with an empty value list",
        )?;
        let still_one = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            still_one == [RuntimeValue::Boolean(true)],
            "the repeated reference DELETE must leave the second row unchanged",
        )?;

        // The allowed rejections retained allowed audits before the savepoint
        // rollback; every dispatch decision is exact.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 16,
            "raw reference mutation audit count differs",
        )?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[3].decision().kind() == SecurityAuditKind::Execute
                && audits[3].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[3].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[7].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[8].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[9].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[10].decision().target()
                    == Some(InvocationTarget::new(delete_probe, pair))
                && audits[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[11].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[12].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[12].decision().target()
                    == Some(InvocationTarget::new(update_false, pair))
                && audits[13].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[13].decision().target() == Some(InvocationTarget::new(read_probes, pair))
                && audits[14].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[14].decision().target()
                    == Some(InvocationTarget::new(delete_probe, pair))
                && audits[15].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[15].decision().target() == Some(InvocationTarget::new(read_probes, pair)),
            "raw reference mutation audit kinds, outcomes, and targets differ",
        )?;

        // The recovered grant set is exactly the four fixed-service targets.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, update_false),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, delete_probe),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, read_probes),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the four fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw reference mutations must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_reference_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_link = raw_function_id(&applied, &["raw_reference_test", "update_link"])?;
        let read_links = raw_function_id(&applied, &["raw_reference_test", "read_links"])?;
        let p_probe = applied
            .catalogue()
            .function_by_id(update_link)
            .ok_or_else(|| failure("update_link is absent from the active catalogue"))?
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_link.p_probe is absent from the active catalogue"))?
            .id();
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant the three fixed-service targets.
        for function in [create_probe, update_link, read_links] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create one row and retain its exact reference.
        let reference = create_probe_reference(&kernel, &session, create_probe).await?;

        // A Reference argument against the parameter-free reader closes as an
        // unavailable raw target, retaining an allowed audit.
        let rejected_read = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                read_links,
                &[FunctionArgument::new(p_probe, reference.clone())?],
            )
            .await
            .expect_err("a Reference argument must reject the parameter-free reader");
        require(
            matches!(
                rejected_read,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == read_links
                        && rule
                            == "raw call arguments require a supported active SERVER mutation target"
            ),
            "the Reference-bearing read_links call returned the wrong typed error",
        )?;

        // The public reader exposes the exact typed NULL reference for the
        // one unlinked row.
        let initial = read_probe_values(&kernel, &session, read_links).await?;
        require(
            matches!(
                initial.as_slice(),
                [RuntimeValue::Null(null)]
                    if null.resolved_type() == ResolvedType::reference(probe)
            ),
            "read_links must initially return the exact typed NULL reference",
        )?;

        // update_link assigns a non-literal parameter expression, so it must
        // close as an unavailable raw target before any assignment runs.
        let rejected = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                update_link,
                &[FunctionArgument::new(p_probe, reference.clone())?],
            )
            .await
            .expect_err("a non-literal assignment UPDATE must be rejected");
        require(
            matches!(
                rejected,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == update_link
                        && rule == "raw SERVER UPDATE reference target is unavailable"
            ),
            "the non-literal assignment UPDATE returned the wrong typed error",
        )?;

        // The reader still exposes the exact typed NULL reference.
        let after = read_probe_values(&kernel, &session, read_links).await?;
        require(
            matches!(
                after.as_slice(),
                [RuntimeValue::Null(null)]
                    if null.resolved_type() == ResolvedType::reference(probe)
            ),
            "the rejected UPDATE must not assign the linked reference",
        )?;

        // Authentication, then allowed create, rejected Reference-bearing
        // read, ordinary read, rejected update, and final read, with the two
        // rejected audits retained at their exact targets.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 6, "raw reference audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(read_links, pair))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target() == Some(InvocationTarget::new(read_links, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(update_link, pair))
                && audits[5].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[5].decision().target() == Some(InvocationTarget::new(read_links, pair)),
            "raw reference audit kinds, outcomes, and targets differ",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "the rejected raw reference UPDATE must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_UPDATE_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_reference_test", "probe"]))
            .ok_or_else(|| failure("probe object type is absent"))?
            .id();
        let create_probe = raw_function_id(&applied, &["raw_reference_test", "create_probe"])?;
        let update_false = raw_function_id(&applied, &["raw_reference_test", "update_false"])?;
        let read_probes = raw_function_id(&applied, &["raw_reference_test", "read_probes"])?;
        let update_false_definition = applied
            .catalogue()
            .function_by_id(update_false)
            .ok_or_else(|| failure("update_false is absent from the active catalogue"))?;
        let p_probe = update_false_definition
            .parameter_by_name("p_probe")
            .ok_or_else(|| failure("update_false.p_probe is absent from the active catalogue"))?
            .id();
        let update_false_revision = update_false_definition.current_revision();
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant the three fixed-service targets.
        for function in [create_probe, update_false, read_probes] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Two rows: the first is the UPDATE target, the second is unrelated.
        let first = create_probe_reference(&kernel, &session, create_probe).await?;
        let second = create_probe_reference(&kernel, &session, create_probe).await?;
        require(
            first != second,
            "the two created references must be distinct",
        )?;

        // A real PostgreSQL UPDATE then fails through an AFTER UPDATE trigger.
        // The dispatch pauses after recovery while the harness installs the
        // trigger, then resumes and fails the write. The typed ServerUpdate
        // database failure must survive the raw dispatch unchanged, the
        // savepoint must roll back the tentative row, and the allowed audit
        // must commit.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let triggered_arguments = vec![FunctionArgument::new(p_probe, first.clone())?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    update_false,
                    &triggered_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterUpdate,
            execution,
            reached,
            resume,
            "triggered raw UPDATE",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerUpdate(ServerUpdateError::NotCommitted {
                context,
                source,
            }) => (context, source),
            other => {
                return Err(failure(format!("triggered raw UPDATE returned {other:?}")));
            }
        };
        require_context(context, pair, update_false, update_false_revision)?;
        let source = match source.as_ref() {
            ServerMutationError::Database { source } => source,
            other => {
                return Err(failure(format!("triggered raw UPDATE returned {other:?}")));
            }
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("triggered raw UPDATE has no database error code"))?;
        require(
            code == &SqlState::RAISE_EXCEPTION,
            "triggered raw UPDATE error code differs",
        )?;

        // The savepoint rolled back: the target row stays TRUE and the
        // unrelated second row stays TRUE.
        let values = read_probe_values(&kernel, &session, read_probes).await?;
        require(
            values.len() == 2
                && values
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the failed UPDATE must leave both rows TRUE",
        )?;

        // The allowed UPDATE audit was retained across the rollback.
        let audits = kernel.recover_security_audit_events().await?;
        require(audits.len() == 5, "raw reference audit count differs")?;
        require(
            audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..]
                    .iter()
                    .all(|event| event.decision().kind() == SecurityAuditKind::Execute)
                && audits[1].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[2].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[2].decision().target() == Some(InvocationTarget::new(create_probe, pair))
                && audits[3].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[3].decision().target() == Some(InvocationTarget::new(update_false, pair))
                && audits[4].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[4].decision().target() == Some(InvocationTarget::new(read_probes, pair)),
            "raw reference audit kinds, outcomes, and targets differ",
        )?;

        // The active revision pair is unchanged.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "the failed raw reference UPDATE must not change the active revision pair",
        )?;

        Ok(())
    })
    .await
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
    AfterUpdate,
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
        TriggerKind::AfterUpdate => (
            "test_fail_after_update",
            "CREATE TRIGGER test_fail_after_update AFTER UPDATE",
            "RAISE EXCEPTION 'forced update failure';",
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
        TriggerKind::AfterRow | TriggerKind::AfterUpdate => "",
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
        TriggerKind::AfterUpdate => "test_fail_after_update",
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
                 WHERE datname = pg_catalog.current_database() \
                   AND backend_type = 'client backend'",
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

#[cfg(feature = "test-hooks")]
/// The canonical identity of one active catalogue function by exact name.
fn raw_function_id(active: &ActiveDatabaseRevision, name: &[&str]) -> TestResult<FunctionId> {
    active
        .catalogue()
        .functions()
        .iter()
        .find(|function| name_is(function.name().parts(), name))
        .map(|function| function.id())
        .ok_or_else(|| {
            failure(format!(
                "function {name:?} is absent from the active catalogue"
            ))
        })
}

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message.into()))
    }
}

#[cfg(feature = "test-hooks")]
const RAW_REFERENCE_INSERT_SOURCE: &str = "CREATE SCHEMA raw_reference_insert;\n\
    CREATE TYPE raw_reference_insert.owner AS OBJECT (\n\
      flag BOOLEAN NOT NULL\n\
    );\n\
    CREATE TYPE raw_reference_insert.assignment AS OBJECT (\n\
      owner REF raw_reference_insert.owner NOT NULL UNIQUE, marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_owner()\n\
    RETURNS ROWS (created_owner REF raw_reference_insert.owner)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.owner AS made_owner (flag)\n\
    VALUES (TRUE) RETURNING REF(made_owner);\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_assignment(\n\
      p_owner REF raw_reference_insert.owner\n\
    ) RETURNS ROWS (created_assignment REF raw_reference_insert.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.assignment AS made_assignment (owner, marker)\n\
    VALUES (p_owner, TRUE) RETURNING REF(made_assignment);\n\
    CREATE SERVER FUNCTION raw_reference_insert.read_assignments()\n\
    RETURNS ROWS (marker BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT assignment.marker FROM raw_reference_insert.assignment assignment;\n\
    CREATE TYPE raw_reference_insert.unused_assignment AS OBJECT (\n\
      marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_insert.create_unused(\n\
      p_owner REF raw_reference_insert.owner\n\
    ) RETURNS ROWS (created_unused REF raw_reference_insert.unused_assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_insert.unused_assignment AS made_unused (marker)\n\
    VALUES (TRUE) RETURNING REF(made_unused);\n\
    CREATE SERVER FUNCTION raw_reference_insert.read_unused()\n\
    RETURNS ROWS (marker BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT unused_assignment.marker FROM raw_reference_insert.unused_assignment unused_assignment;\n";

/// One raw reference-INSERT fixture with a unique reference owner field.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct RawReferenceInsertFixture {
    owner: TypeId,
    assignment: TypeId,
    assignment_owner_field: FieldId,
    create_owner: FunctionId,
    create_assignment: FunctionId,
    create_assignment_revision: FunctionRevisionId,
    create_assignment_owner_parameter: ParameterId,
    read_assignments: FunctionId,
    create_unused: FunctionId,
    create_unused_owner_parameter: ParameterId,
    read_unused: FunctionId,
}

#[cfg(feature = "test-hooks")]
impl RawReferenceInsertFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = |name| {
            active
                .catalogue()
                .object_types()
                .iter()
                .find(|object| name_is(object.name().parts(), &["raw_reference_insert", name]))
                .ok_or_else(|| failure(format!("raw_reference_insert.{name} type is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["raw_reference_insert", name]))
                .ok_or_else(|| failure(format!("raw_reference_insert.{name} function is absent")))
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
        let read_assignments = function("read_assignments")?;
        let create_unused = function("create_unused")?;
        let read_unused = function("read_unused")?;
        Ok(Self {
            owner: owner.id(),
            assignment: assignment.id(),
            assignment_owner_field: assignment
                .field_by_name("owner")
                .map(|field| field.id())
                .ok_or_else(|| failure("raw_reference_insert.assignment.owner field is absent"))?,
            create_owner: create_owner.id(),
            create_assignment: create_assignment.id(),
            create_assignment_revision: create_assignment.current_revision(),
            create_assignment_owner_parameter: parameter(create_assignment, "p_owner")?,
            read_assignments: read_assignments.id(),
            create_unused: create_unused.id(),
            create_unused_owner_parameter: parameter(create_unused, "p_owner")?,
            read_unused: read_unused.id(),
        })
    }
}

#[cfg(feature = "test-hooks")]
fn raw_reference_insert_arguments(
    fixture: RawReferenceInsertFixture,
    owner: RuntimeValue,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![FunctionArgument::new(
        fixture.create_assignment_owner_parameter,
        owner,
    )?])
}

#[cfg(feature = "test-hooks")]
async fn create_raw_reference_insert_owner(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    fixture: RawReferenceInsertFixture,
) -> TestResult<RuntimeValue> {
    let created = kernel
        .dispatch_authenticated_raw_call(session, fixture.create_owner)
        .await?;
    let created = match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw owner INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference { target, object } = &created[0] else {
        return Err(failure("raw owner INSERT must return an object reference"));
    };
    require(
        *target == fixture.owner && *object != ObjectId::from_bytes([0; 16]),
        "the created owner reference must name the owner type and a real row",
    )?;
    Ok(RuntimeValue::Reference {
        target: *target,
        object: *object,
    })
}

#[cfg(feature = "test-hooks")]
async fn read_raw_reference_insert_markers(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    fixture: RawReferenceInsertFixture,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, fixture.read_assignments)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw assignment SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
fn require_raw_reference_insert_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: RawReferenceInsertFixture,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(insert) = error else {
        return Err(failure(
            "raw reference INSERT conflict is not a SERVER INSERT error",
        ));
    };
    require(
        insert.commit_state() == ServerInsertCommitState::NotCommitted,
        "raw reference INSERT conflict has the wrong commit state",
    )?;
    let ServerInsertError::NotCommitted { context, source } = insert else {
        return Err(failure(
            "raw reference INSERT conflict lacks pinned execution context",
        ));
    };
    require_context(*context, pair, function, revision)?;
    let unique @ ServerMutationError::UniqueReferenceConflict {
        owner,
        field,
        referenced_type,
        source: database_source,
    } = source.as_ref()
    else {
        return Err(failure(
            "raw reference INSERT was not classified as a typed reference conflict",
        ));
    };
    require(
        *owner == fixture.assignment,
        "raw reference INSERT conflict owner differs",
    )?;
    require(
        *field == fixture.assignment_owner_field,
        "raw reference INSERT conflict field differs",
    )?;
    require(
        *referenced_type == fixture.owner,
        "raw reference INSERT conflict referenced type differs",
    )?;
    require(
        database_source
            .as_db_error()
            .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION),
        "raw reference INSERT conflict lost SQLSTATE 23505",
    )?;
    require(
        database_source
            .as_db_error()
            .and_then(|database| database.constraint())
            == Some(unique_constraint_name(fixture.assignment_owner_field).as_str()),
        "raw reference INSERT conflict constraint differs",
    )?;
    require(
        unique.to_string() == "this reference is already used by another object",
        "raw reference INSERT inner display differs",
    )
}

/// One authenticated raw reference-INSERT journey across denial, grant,
/// argument-target rejection, a database failure, a typed unique conflict,
/// and public recovery.
///
/// The test installs the exact checked-in reference-INSERT source, grants
/// only the owner create and the reader, creates two distinct owners, and
/// proves a wrong-parameter reference call is denied before its grant with
/// zero assignments. After granting the assignment create, a wrong parameter
/// id and a wrong reference target type each close as the exact
/// `raw SERVER INSERT argument target is unavailable` rule. A correct-type
/// reference to a definitely missing object returns the typed internal
/// SERVER INSERT database failure, leaves zero rows, and retains its allowed
/// audit. A correct first owner succeeds with a nonzero assignment reference
/// whose target differs from the owner type; repeating the same owner returns
/// the exact typed unique-reference conflict with a `NotCommitted` context,
/// leaves exactly one assignment, and retains its allowed audit. The second
/// owner succeeds with a distinct object id and the same assignment target
/// type. The public raw reader returns exactly two TRUE marker values.
/// A granted SERVER INSERT whose plan never reads its sole Reference
/// parameter (`create_unused`) closes as the exact unavailable raw target
/// rule after classification and savepoint creation, rolls back, retains its
/// allowed audit, and leaves `read_unused` empty. Recovery proves the active
/// pair is unchanged and the grant set is exactly the five fixed-service
/// grants. Rows are asserted only through the public raw reader.
#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_REFERENCE_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let fixture = RawReferenceInsertFixture::from_active(&applied)?;
        let owner_parameter = fixture.create_assignment_owner_parameter;
        let mut wrong_parameter_bytes = owner_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != owner_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the owner create and the reader; the assignment create
        // stays unauthorised for the denial proof.
        for function in [fixture.create_owner, fixture.read_assignments] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // Create two distinct owners through the public raw path.
        let first_owner = create_raw_reference_insert_owner(&kernel, &session, fixture).await?;
        let second_owner = create_raw_reference_insert_owner(&kernel, &session, fixture).await?;
        let RuntimeValue::Reference {
            target: first_owner_target,
            object: first_owner_object,
        } = &first_owner
        else {
            return Err(failure("first owner value is not a reference"));
        };
        let RuntimeValue::Reference {
            target: second_owner_target,
            object: second_owner_object,
        } = &second_owner
        else {
            return Err(failure("second owner value is not a reference"));
        };
        require(
            first_owner_target == second_owner_target && *first_owner_target == fixture.owner,
            "both owners must share the exact owner target type",
        )?;
        require(
            *first_owner_object != *second_owner_object
                && *first_owner_object != ObjectId::from_bytes([0; 16])
                && *second_owner_object != ObjectId::from_bytes([0; 16]),
            "the two owners must name distinct nonzero rows",
        )?;
        let mut wrong_target_bytes = fixture.owner.to_bytes();
        wrong_target_bytes[0] ^= 0x01;
        let wrong_target = TypeId::from_bytes(wrong_target_bytes);
        require(
            wrong_target != fixture.owner,
            "the deliberately wrong target must differ from the owner target",
        )?;
        let missing_object = ObjectId::from_bytes([0xaa; 16]);
        require(
            missing_object != *first_owner_object && missing_object != *second_owner_object,
            "the missing object must not name either created owner",
        )?;

        // The reader proves zero assignments before any assignment create.
        let zero_before = read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(zero_before.is_empty(), "no assignment may exist before any grant")?;

        // Authorisation wins over argument validation: before its grant, even
        // a wrong-parameter reference call is denied and creates nothing.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(wrong_parameter, first_owner.clone())?],
            )
            .await
            .expect_err("an ungranted raw reference INSERT must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_assignment
            ),
            "pre-grant raw reference INSERT returned the wrong typed denial",
        )?;
        let zero_after_denied =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            zero_after_denied.is_empty(),
            "the denied raw reference INSERT must not create any assignment",
        )?;

        // Grant the assignment create and the unused-parameter proof pair.
        for function in [
            fixture.create_assignment,
            fixture.create_unused,
            fixture.read_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // A wrong parameter id closes as the exact unavailable raw target and
        // retains its allowed audit.
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(wrong_parameter, first_owner.clone())?],
            )
            .await
            .expect_err("a wrong parameter id must make the raw reference INSERT unavailable");
        require(
            matches!(
                wrong_parameter_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_assignment
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "a wrong parameter id returned the wrong typed error",
        )?;

        // A wrong reference target type closes with the same exact rule.
        let wrong_target_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Reference {
                        target: wrong_target,
                        object: *first_owner_object,
                    },
                )?],
            )
            .await
            .expect_err("a wrong reference target type must make the raw reference INSERT unavailable");
        require(
            matches!(
                wrong_target_unavailable,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_assignment
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "a wrong reference target type returned the wrong typed error",
        )?;

        // The SOL proof pair: a granted SERVER INSERT whose plan never reads
        // its sole Reference parameter passes classification and the normal
        // active validator, then closes inside its savepoint as the exact
        // unavailable raw target rule, rolls back, retains its allowed audit,
        // and creates no row.
        let read_unused_before = kernel
            .dispatch_authenticated_raw_call(&session, fixture.read_unused)
            .await?;
        require(
            matches!(
                read_unused_before,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "read_unused must be empty before any create_unused call",
        )?;
        let unused = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_unused,
                &[FunctionArgument::new(
                    fixture.create_unused_owner_parameter,
                    first_owner.clone(),
                )?],
            )
            .await
            .expect_err("create_unused must reject a Reference argument it never reads");
        require(
            matches!(
                unused,
                PostgresKernelError::RawCallTargetUnavailable { function, rule }
                    if function == fixture.create_unused
                        && rule == "raw SERVER INSERT argument target is unavailable"
            ),
            "create_unused returned the wrong typed error",
        )?;
        let read_unused_after = kernel
            .dispatch_authenticated_raw_call(&session, fixture.read_unused)
            .await?;
        require(
            matches!(
                read_unused_after,
                AuthenticatedRawCallResult::Server(values) if values.is_empty()
            ),
            "the rejected create_unused must leave read_unused empty",
        )?;

        // A correct-type reference to a definitely missing object reaches the
        // database, fails as the typed internal SERVER INSERT database
        // failure, rolls back its savepoint, and retains its allowed audit.
        let missing = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Reference {
                        target: fixture.owner,
                        object: missing_object,
                    },
                )?],
            )
            .await
            .expect_err("a missing owner object must fail the raw reference INSERT");
        let PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted {
            context,
            source,
        }) = missing
        else {
            return Err(failure(format!(
                "missing-object raw INSERT returned {missing:?}"
            )));
        };
        require_context(context, pair, fixture.create_assignment, fixture.create_assignment_revision)?;
        let ServerInsertError::Database { source } = source.as_ref() else {
            return Err(failure("missing-object raw INSERT lost its database failure"));
        };
        let code = source
            .as_db_error()
            .map(|error| error.code())
            .ok_or_else(|| failure("missing-object raw INSERT has no database error code"))?;
        require(
            code == &SqlState::FOREIGN_KEY_VIOLATION,
            "missing-object raw INSERT error code differs",
        )?;
        let zero_after_missing =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            zero_after_missing.is_empty(),
            "the failed raw reference INSERT must leave zero assignments",
        )?;

        // The first owner succeeds and returns one nonzero assignment
        // reference whose target differs from the owner type.
        let first_created = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, first_owner.clone())?,
            )
            .await?;
        let first_created = match first_created {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "first owner raw reference INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: first_assignment_target,
            object: first_assignment_object,
        } = &first_created[0]
        else {
            return Err(failure("first owner raw reference INSERT must return an assignment reference"));
        };
        require(
            *first_assignment_target == fixture.assignment
                && *first_assignment_target != fixture.owner
                && *first_assignment_object != ObjectId::from_bytes([0; 16]),
            "the first assignment reference must name the assignment type and a real row",
        )?;

        // Repeating the same owner returns the exact typed unique-reference
        // conflict with a NotCommitted context, and exactly one assignment
        // survives.
        let conflict = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, first_owner.clone())?,
            )
            .await
            .expect_err("a repeated owner must conflict on the unique reference");
        require_raw_reference_insert_conflict(
            &conflict,
            pair,
            fixture,
            fixture.create_assignment,
            fixture.create_assignment_revision,
        )?;
        let one_after_conflict =
            read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            one_after_conflict == [RuntimeValue::Boolean(true)],
            "the unique conflict must leave exactly one TRUE assignment",
        )?;

        // The second owner succeeds with a distinct object id and the same
        // assignment target type.
        let second_created = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_assignment,
                &raw_reference_insert_arguments(fixture, second_owner.clone())?,
            )
            .await?;
        let second_created = match second_created {
            AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
            other => {
                return Err(failure(format!(
                    "second owner raw reference INSERT must return exactly one Server value, got {other:?}"
                )));
            }
        };
        let RuntimeValue::Reference {
            target: second_assignment_target,
            object: second_assignment_object,
        } = &second_created[0]
        else {
            return Err(failure("second owner raw reference INSERT must return an assignment reference"));
        };
        require(
            *second_assignment_target == fixture.assignment
                && *second_assignment_target == *first_assignment_target
                && *second_assignment_object != ObjectId::from_bytes([0; 16])
                && second_assignment_object != first_assignment_object,
            "the second assignment must share the target type and use a distinct nonzero object",
        )?;

        // The public raw reader returns exactly two TRUE marker values.
        let two = read_raw_reference_insert_markers(&kernel, &session, fixture).await?;
        require(
            two.len() == 2
                && two
                    .iter()
                    .filter(|value| **value == RuntimeValue::Boolean(true))
                    .count()
                    == 2,
            "the raw reader must return exactly two TRUE assignment markers",
        )?;

        // One authentication audit, then one audit per dispatch: the pre-grant
        // call at index 4 was denied, every allowed rejection and success
        // retained an allowed target audit.
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 18
                && audits[0].decision().kind() == SecurityAuditKind::Authentication
                && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[1..].iter().all(|event| {
                    event.decision().kind() == SecurityAuditKind::Execute
                })
                && audits[4].decision().outcome() == SecurityAuditOutcome::Denied
                && audits[4].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[6].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[6].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[7].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[7].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[8].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[8].decision().target()
                    == Some(InvocationTarget::new(fixture.read_unused, pair))
                && audits[9].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[9].decision().target()
                    == Some(InvocationTarget::new(fixture.create_unused, pair))
                && audits[10].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[10].decision().target()
                    == Some(InvocationTarget::new(fixture.read_unused, pair))
                && audits[11].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[11].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[13].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[13].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[14].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[14].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[16].decision().outcome() == SecurityAuditOutcome::Allowed
                && audits[16].decision().target()
                    == Some(InvocationTarget::new(fixture.create_assignment, pair))
                && audits[1..]
                    .iter()
                    .enumerate()
                    .all(|(index, event)| index == 3 || {
                        event.decision().outcome() == SecurityAuditOutcome::Allowed
                    }),
            "raw reference INSERT audit kinds, outcomes, and targets differ",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_owner),
            ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                fixture.create_assignment,
            ),
            ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                fixture.read_assignments,
            ),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_unused),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_unused),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the five fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw reference INSERTs must not change the active revision pair",
        )?;

        require_no_session_leaks(&database).await
    })
    .await
}

#[cfg(feature = "test-hooks")]
const RAW_SCALAR_INSERT_SOURCE: &str = "CREATE SCHEMA raw_scalar_insert;\n\
    CREATE TYPE raw_scalar_insert.int_probe AS OBJECT (\n\
      stored INT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_int(p_value INT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.int_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.int_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_ints()\n\
    RETURNS ROWS (stored INT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT int_probe.stored FROM raw_scalar_insert.int_probe int_probe;\n\
    CREATE TYPE raw_scalar_insert.bigint_probe AS OBJECT (\n\
      stored BIGINT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bigint(p_value BIGINT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bigint_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bigint_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bigints()\n\
    RETURNS ROWS (stored BIGINT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bigint_probe.stored FROM raw_scalar_insert.bigint_probe bigint_probe;\n\
    CREATE TYPE raw_scalar_insert.float_probe AS OBJECT (\n\
      stored FLOAT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_float(p_value FLOAT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.float_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.float_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_floats()\n\
    RETURNS ROWS (stored FLOAT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT float_probe.stored FROM raw_scalar_insert.float_probe float_probe;\n\
    CREATE TYPE raw_scalar_insert.text_probe AS OBJECT (\n\
      stored TEXT NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_text(p_value TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.text_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.text_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_texts()\n\
    RETURNS ROWS (stored TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT text_probe.stored FROM raw_scalar_insert.text_probe text_probe;\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_extra(p_used TEXT, p_extra TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.text_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.text_probe AS made (stored)\n\
    VALUES (p_used) RETURNING REF(made);\n\
    CREATE TYPE raw_scalar_insert.unused_probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_unused(p_unused TEXT)\n\
    RETURNS ROWS (created REF raw_scalar_insert.unused_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.unused_probe AS made (stored)\n\
    VALUES (TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_unused()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT unused_probe.stored FROM raw_scalar_insert.unused_probe unused_probe;\n\
    CREATE TYPE raw_scalar_insert.bytes_probe AS OBJECT (\n\
      stored BYTES NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bytes(p_value BYTES)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bytes_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bytes_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bytes()\n\
    RETURNS ROWS (stored BYTES)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bytes_probe.stored FROM raw_scalar_insert.bytes_probe bytes_probe;\n\
    CREATE TYPE raw_scalar_insert.bool_probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_scalar_insert.create_bool(p_value BOOLEAN)\n\
    RETURNS ROWS (created REF raw_scalar_insert.bool_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_scalar_insert.bool_probe AS made (stored)\n\
    VALUES (p_value) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_scalar_insert.read_bools()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT bool_probe.stored FROM raw_scalar_insert.bool_probe bool_probe;\n";

/// One raw scalar-INSERT fixture: one exact single-parameter INSERT and one
/// public single-column reader per accepted scalar type, plus one Boolean
/// regression pair.
#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct RawScalarInsertFixture {
    int_probe: TypeId,
    bigint_probe: TypeId,
    float_probe: TypeId,
    text_probe: TypeId,
    bytes_probe: TypeId,
    bool_probe: TypeId,
    create_int: FunctionId,
    create_bigint: FunctionId,
    create_float: FunctionId,
    create_text: FunctionId,
    create_bytes: FunctionId,
    create_bool: FunctionId,
    create_extra: FunctionId,
    create_unused: FunctionId,
    create_int_parameter: ParameterId,
    create_bigint_parameter: ParameterId,
    create_float_parameter: ParameterId,
    create_text_parameter: ParameterId,
    create_bytes_parameter: ParameterId,
    create_bool_parameter: ParameterId,
    create_extra_used_parameter: ParameterId,
    create_unused_parameter: ParameterId,
    read_ints: FunctionId,
    read_bigints: FunctionId,
    read_floats: FunctionId,
    read_texts: FunctionId,
    read_bytes: FunctionId,
    read_bools: FunctionId,
    read_unused: FunctionId,
}

#[cfg(feature = "test-hooks")]
impl RawScalarInsertFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = |name| {
            active
                .catalogue()
                .object_types()
                .iter()
                .find(|object| name_is(object.name().parts(), &["raw_scalar_insert", name]))
                .ok_or_else(|| failure(format!("raw_scalar_insert.{name} type is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["raw_scalar_insert", name]))
                .ok_or_else(|| failure(format!("raw_scalar_insert.{name} function is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("parameter {name} is absent")))
        };
        let int_probe = object("int_probe")?;
        let bigint_probe = object("bigint_probe")?;
        let float_probe = object("float_probe")?;
        let text_probe = object("text_probe")?;
        let bytes_probe = object("bytes_probe")?;
        let bool_probe = object("bool_probe")?;
        let create_int = function("create_int")?;
        let create_bigint = function("create_bigint")?;
        let create_float = function("create_float")?;
        let create_text = function("create_text")?;
        let create_bytes = function("create_bytes")?;
        let create_bool = function("create_bool")?;
        let create_extra = function("create_extra")?;
        let create_unused = function("create_unused")?;
        Ok(Self {
            int_probe: int_probe.id(),
            bigint_probe: bigint_probe.id(),
            float_probe: float_probe.id(),
            text_probe: text_probe.id(),
            bytes_probe: bytes_probe.id(),
            bool_probe: bool_probe.id(),
            create_int: create_int.id(),
            create_bigint: create_bigint.id(),
            create_float: create_float.id(),
            create_text: create_text.id(),
            create_bytes: create_bytes.id(),
            create_bool: create_bool.id(),
            create_extra: create_extra.id(),
            create_unused: create_unused.id(),
            create_int_parameter: parameter(create_int, "p_value")?,
            create_bigint_parameter: parameter(create_bigint, "p_value")?,
            create_float_parameter: parameter(create_float, "p_value")?,
            create_text_parameter: parameter(create_text, "p_value")?,
            create_bytes_parameter: parameter(create_bytes, "p_value")?,
            create_bool_parameter: parameter(create_bool, "p_value")?,
            create_extra_used_parameter: parameter(create_extra, "p_used")?,
            create_unused_parameter: parameter(create_unused, "p_unused")?,
            read_ints: function("read_ints")?.id(),
            read_bigints: function("read_bigints")?.id(),
            read_floats: function("read_floats")?.id(),
            read_texts: function("read_texts")?.id(),
            read_bytes: function("read_bytes")?.id(),
            read_bools: function("read_bools")?.id(),
            read_unused: function("read_unused")?.id(),
        })
    }
}

#[cfg(feature = "test-hooks")]
fn raw_scalar_insert_reference(
    result: AuthenticatedRawCallResult,
    target: TypeId,
) -> TestResult<ObjectId> {
    let values = match result {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => values,
        other => {
            return Err(failure(format!(
                "raw scalar INSERT must return exactly one Server value, got {other:?}"
            )));
        }
    };
    let RuntimeValue::Reference {
        target: actual_target,
        object,
    } = &values[0]
    else {
        return Err(failure("raw scalar INSERT must return an object reference"));
    };
    require(
        *actual_target == target && *object != ObjectId::from_bytes([0; 16]),
        "raw scalar INSERT reference must name the exact target type and a real row",
    )?;
    Ok(*object)
}

#[cfg(feature = "test-hooks")]
async fn read_raw_scalar_values(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    reader: FunctionId,
) -> TestResult<Vec<RuntimeValue>> {
    let read = kernel
        .dispatch_authenticated_raw_call(session, reader)
        .await?;
    match read {
        AuthenticatedRawCallResult::Server(values) => Ok(values),
        other => Err(failure(format!(
            "raw scalar SELECT must return Server values, got {other:?}"
        ))),
    }
}

#[cfg(feature = "test-hooks")]
fn require_exact_scalar_read(
    values: &[RuntimeValue],
    expected: &RuntimeValue,
    label: &str,
) -> TestResult<()> {
    require(
        values == std::slice::from_ref(expected),
        format!("{label} reader must return exactly the stored value"),
    )
}

#[cfg(feature = "test-hooks")]
fn require_raw_scalar_target_unavailable(
    error: &PostgresKernelError,
    function: FunctionId,
    rule: &'static str,
) -> TestResult<()> {
    require(
        matches!(
            error,
            PostgresKernelError::RawCallTargetUnavailable {
                function: actual,
                rule: actual_rule,
            } if *actual == function && *actual_rule == rule
        ),
        "raw scalar target unavailable error lost its exact function or rule",
    )
}

/// One authenticated raw scalar-INSERT journey across denial, grant,
/// wrong-binding rejection, Text U+0000 rejection, exact stored values,
/// Boolean regression, and public recovery.
///
/// The test installs the exact checked-in scalar-INSERT source with one
/// single-parameter INSERT and one reader per accepted scalar type. It grants
/// only the seven readers, then proves a wrong-parameter Integer call and a
/// U+0000 Text call are each denied before their grants with zero rows.
/// After granting the eight INSERT targets, a wrong parameter id, a wrong
/// scalar type, an extra declared parameter, and an unused sole scalar
/// parameter each close as the exact `raw SERVER INSERT argument target is
/// unavailable` rule, roll back their savepoints, and retain their allowed
/// audits. An allowed Integer-bearing raw SELECT closes as the unsupported
/// raw target rule. Text U+0000 returns the same unavailable raw target
/// after an allowed audit, creates no row, and never reaches the driver bind.
/// Each exact scalar then binds its exact `ParameterId` and direct assignment,
/// returns a distinct nonzero typed reference, and the public reader returns
/// the exact stored value and byte pattern. A Boolean INSERT proves the
/// accepted Boolean shape remains unchanged beside the five new scalars.
/// Recovery proves the active pair is unchanged and the grant set is exactly
/// the fifteen fixed-service grants. Rows are asserted only through the
/// public raw readers.
///
/// The authenticated Reference INSERT journey stays covered by the separate
/// `authenticated_raw_reference_insert_is_denied_then_granted_transactional_and_unique`
/// test; this test does not duplicate its reference setup.
#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_scalar_insert_binds_exact_parameters_and_stores_exact_values()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_SCALAR_INSERT_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let fixture = RawScalarInsertFixture::from_active(&applied)?;
        let mut wrong_parameter_bytes = fixture.create_int_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        require(
            wrong_parameter != fixture.create_int_parameter,
            "the deliberately wrong parameter must differ from the declared parameter",
        )?;
        let u0000_text = RuntimeValue::Text(String::from("a\u{0}b"));
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Grant only the seven readers; every INSERT target stays
        // unauthorised for the denial proof.
        for reader in [
            fixture.read_ints,
            fixture.read_bigints,
            fixture.read_floats,
            fixture.read_texts,
            fixture.read_bytes,
            fixture.read_bools,
            fixture.read_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, reader)
                .await?;
        }

        // Denial wins over every parameter, type, and U+0000 fact: the
        // wrong-parameter Integer call and the U+0000 Text call are denied
        // before their grants, and no row exists.
        let denied_wrong_parameter = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    wrong_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an ungranted raw Integer INSERT must be denied");
        require(
            matches!(
                denied_wrong_parameter,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_int
            ),
            "pre-grant wrong-parameter raw Integer INSERT returned the wrong typed denial",
        )?;
        let denied_u0000 = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    u0000_text.clone(),
                )?],
            )
            .await
            .expect_err("an ungranted U+0000 raw Text INSERT must be denied");
        require(
            matches!(
                denied_u0000,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == fixture.create_text
            ),
            "pre-grant U+0000 raw Text INSERT returned the wrong typed denial",
        )?;
        let zero_ints = read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            zero_ints.is_empty(),
            "the denied raw Integer INSERT must not create any row",
        )?;
        let zero_texts = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            zero_texts.is_empty(),
            "the denied U+0000 raw Text INSERT must not create any row",
        )?;

        // Grant the eight INSERT targets.
        for create in [
            fixture.create_int,
            fixture.create_bigint,
            fixture.create_float,
            fixture.create_text,
            fixture.create_bytes,
            fixture.create_bool,
            fixture.create_extra,
            fixture.create_unused,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, create)
                .await?;
        }

        // A wrong parameter id closes as the exact unavailable raw target,
        // rolls back its savepoint, and retains its allowed audit.
        let wrong_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    wrong_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("a wrong parameter id must make the raw Integer INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &wrong_parameter_unavailable,
            fixture.create_int,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_wrong_parameter =
            read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            after_wrong_parameter.is_empty(),
            "a wrong parameter id must not create any row",
        )?;

        // A wrong scalar type closes with the same exact rule.
        let wrong_type_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an Integer argument must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &wrong_type_unavailable,
            fixture.create_text,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_wrong_type =
            read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_wrong_type.is_empty(),
            "a wrong scalar type must not create any row",
        )?;

        // An allowed scalar-bearing non-INSERT SERVER target closes as the
        // unsupported raw target rule.
        let unsupported_select = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.read_ints,
                &[FunctionArgument::new(
                    fixture.create_int_parameter,
                    RuntimeValue::Integer(7),
                )?],
            )
            .await
            .expect_err("an Integer argument must reject the granted raw SELECT");
        require_raw_scalar_target_unavailable(
            &unsupported_select,
            fixture.read_ints,
            "raw call arguments require a supported active SERVER mutation target",
        )?;
        let after_unsupported =
            read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require(
            after_unsupported.is_empty(),
            "an unsupported scalar target must not create any row",
        )?;

        // Text U+0000 is an authorised target failure: it rolls back the raw
        // INSERT savepoint, creates no row, and retains its allowed audit.
        let u0000_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    u0000_text,
                )?],
            )
            .await
            .expect_err("Text U+0000 must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &u0000_unavailable,
            fixture.create_text,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_u0000 = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_u0000.is_empty(),
            "Text U+0000 must not create any row",
        )?;

        // An INSERT target that declares a second parameter closes with the
        // same exact rule even though the supplied argument names the one
        // parameter the plan reads.
        let extra_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_extra,
                &[FunctionArgument::new(
                    fixture.create_extra_used_parameter,
                    RuntimeValue::Text(String::from("extra")),
                )?],
            )
            .await
            .expect_err("an extra declared parameter must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &extra_parameter_unavailable,
            fixture.create_extra,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_extra = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require(
            after_extra.is_empty(),
            "an extra declared parameter must not create any row",
        )?;

        // An INSERT target whose sole scalar parameter is never read by a
        // direct assignment closes with the same exact rule.
        let unused_parameter_unavailable = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_unused,
                &[FunctionArgument::new(
                    fixture.create_unused_parameter,
                    RuntimeValue::Text(String::from("unused")),
                )?],
            )
            .await
            .expect_err("an unused sole parameter must make the raw Text INSERT unavailable");
        require_raw_scalar_target_unavailable(
            &unused_parameter_unavailable,
            fixture.create_unused,
            "raw SERVER INSERT argument target is unavailable",
        )?;
        let after_unused = read_raw_scalar_values(&kernel, &session, fixture.read_unused).await?;
        require(
            after_unused.is_empty(),
            "an unused sole parameter must not create any row",
        )?;

        // Each exact scalar binds its exact ParameterId through a direct
        // assignment and stores its exact value; every INSERT returns a
        // distinct nonzero reference to its exact target type.
        let mut identities = BTreeSet::new();
        let inserted_int = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_int,
                &[FunctionArgument::new(
                    fixture.create_int_parameter,
                    RuntimeValue::Integer(i32::MIN),
                )?],
            )
            .await?;
        let int_object = raw_scalar_insert_reference(inserted_int, fixture.int_probe)?;
        require(
            identities.insert(int_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let ints = read_raw_scalar_values(&kernel, &session, fixture.read_ints).await?;
        require_exact_scalar_read(&ints, &RuntimeValue::Integer(i32::MIN), "INT")?;

        let inserted_bigint = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bigint,
                &[FunctionArgument::new(
                    fixture.create_bigint_parameter,
                    RuntimeValue::BigInt(i64::MAX),
                )?],
            )
            .await?;
        let bigint_object = raw_scalar_insert_reference(inserted_bigint, fixture.bigint_probe)?;
        require(
            identities.insert(bigint_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bigints = read_raw_scalar_values(&kernel, &session, fixture.read_bigints).await?;
        require_exact_scalar_read(&bigints, &RuntimeValue::BigInt(i64::MAX), "BIGINT")?;

        let inserted_float = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_float,
                &[FunctionArgument::new(
                    fixture.create_float_parameter,
                    RuntimeValue::Float(RuntimeFloat::new(0.1)?),
                )?],
            )
            .await?;
        let float_object = raw_scalar_insert_reference(inserted_float, fixture.float_probe)?;
        require(
            identities.insert(float_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let floats = read_raw_scalar_values(&kernel, &session, fixture.read_floats).await?;
        require_exact_scalar_read(
            &floats,
            &RuntimeValue::Float(RuntimeFloat::new(0.1)?),
            "FLOAT",
        )?;
        let RuntimeValue::Float(stored_float) = &floats[0] else {
            return Err(failure("raw FLOAT reader must return a Float value"));
        };
        require(
            stored_float.value().to_bits() == 0.1_f64.to_bits(),
            "FLOAT reader must preserve the exact canonical bit pattern",
        )?;

        let inserted_text = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_text,
                &[FunctionArgument::new(
                    fixture.create_text_parameter,
                    RuntimeValue::Text(String::from("caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}")),
                )?],
            )
            .await?;
        let text_object = raw_scalar_insert_reference(inserted_text, fixture.text_probe)?;
        require(
            identities.insert(text_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let texts = read_raw_scalar_values(&kernel, &session, fixture.read_texts).await?;
        require_exact_scalar_read(
            &texts,
            &RuntimeValue::Text(String::from("caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}")),
            "TEXT",
        )?;

        let inserted_bytes = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bytes,
                &[FunctionArgument::new(
                    fixture.create_bytes_parameter,
                    RuntimeValue::Bytes(vec![0x00, 0xff, 0x7f, 0x00, 0x01]),
                )?],
            )
            .await?;
        let bytes_object = raw_scalar_insert_reference(inserted_bytes, fixture.bytes_probe)?;
        require(
            identities.insert(bytes_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bytes = read_raw_scalar_values(&kernel, &session, fixture.read_bytes).await?;
        require_exact_scalar_read(
            &bytes,
            &RuntimeValue::Bytes(vec![0x00, 0xff, 0x7f, 0x00, 0x01]),
            "BYTES",
        )?;

        // The Boolean shape remains accepted beside the five new scalars.
        // The Reference INSERT regression is not duplicated here: it stays
        // covered by the dedicated reference-INSERT test above.
        let inserted_bool = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                fixture.create_bool,
                &[FunctionArgument::new(
                    fixture.create_bool_parameter,
                    RuntimeValue::Boolean(true),
                )?],
            )
            .await?;
        let bool_object = raw_scalar_insert_reference(inserted_bool, fixture.bool_probe)?;
        require(
            identities.insert(bool_object),
            "raw scalar INSERTs must allocate distinct object identities",
        )?;
        let bools = read_raw_scalar_values(&kernel, &session, fixture.read_bools).await?;
        require_exact_scalar_read(&bools, &RuntimeValue::Boolean(true), "BOOLEAN")?;
        require(
            identities.len() == 6,
            "the six successful raw scalar INSERTs must use six distinct identities",
        )?;

        // One authentication decision, then one execute decision per
        // dispatched call in dispatch order. The only denied execute
        // decisions are the two pre-grant calls; every granted call,
        // including every rejected target, retained exactly one allowed
        // execute decision at its dispatch position.
        let audits = kernel.recover_security_audit_events().await?;
        let actual: Vec<(SecurityAuditKind, SecurityAuditOutcome, Option<FunctionId>)> = audits
            .iter()
            .map(|event| {
                let decision = event.decision();
                (
                    decision.kind(),
                    decision.outcome(),
                    decision.target().map(InvocationTarget::function),
                )
            })
            .collect();
        let expected: Vec<(SecurityAuditKind, SecurityAuditOutcome, Option<FunctionId>)> = vec![
            (
                SecurityAuditKind::Authentication,
                SecurityAuditOutcome::Allowed,
                None,
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Denied,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_extra),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_unused),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_unused),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_int),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_ints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bigint),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bigints),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_float),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_floats),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_text),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_texts),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bytes),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bytes),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.create_bool),
            ),
            (
                SecurityAuditKind::Execute,
                SecurityAuditOutcome::Allowed,
                Some(fixture.read_bools),
            ),
        ];
        require(
            actual == expected,
            "raw scalar INSERT audit chain differs from the dispatch order",
        )?;

        // Public recovery proves the exact fixed-service grant set.
        let mut grants = kernel
            .recover_security_snapshot()
            .await?
            .execute_grants()
            .collect::<Vec<_>>();
        grants.sort();
        let mut expected = vec![
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_int),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bigint),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_float),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_text),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bytes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_bool),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_extra),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.create_unused),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_ints),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bigints),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_floats),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_texts),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bytes),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_bools),
            ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, fixture.read_unused),
        ];
        expected.sort();
        require(
            grants == expected,
            "recovered grants must contain exactly the fifteen fixed-service grants",
        )?;

        // The active revision pair is unchanged throughout.
        let recovered = kernel.recover().await?;
        require(
            recovered.pair() == pair,
            "raw scalar INSERTs must not change the active revision pair",
        )?;

        require_no_session_leaks(&database).await
    })
    .await
}
