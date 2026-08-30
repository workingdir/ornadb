//! Live PostgreSQL tests for atomic single-row SERVER mutation execution.

mod support;

use std::{collections::BTreeSet, str::FromStr};

#[cfg(feature = "test-hooks")]
use std::future::Future;
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
#[path = "server_mutation_execution/core.rs"]
mod core;
#[cfg(feature = "test-hooks")]
#[path = "server_mutation_execution/reference_insert.rs"]
mod reference_insert;
#[cfg(feature = "test-hooks")]
#[path = "server_mutation_execution/scalar_insert.rs"]
mod scalar_insert;
#[cfg(feature = "test-hooks")]
use scalar_insert::{
    raw_scalar_insert_reference, read_raw_scalar_values, require_exact_scalar_read,
    require_raw_scalar_target_unavailable,
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
const RAW_ARGUMENT_PAIR_SOURCE: &str = "CREATE SCHEMA raw_argument_pair_test;\n\
    CREATE TYPE raw_argument_pair_test.pair_value AS VALUE (\n\
      first TEXT, second TEXT\n\
    ) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE raw_argument_pair_test.probe AS OBJECT (\n\
      first TEXT, second TEXT, marker BOOLEAN NOT NULL\n\
    );\n\
    CREATE TYPE raw_argument_pair_test.indirect_probe AS OBJECT (\n\
      nested raw_argument_pair_test.pair_value NOT NULL\n\
    );\n\
    CREATE TYPE raw_argument_pair_test.owner AS OBJECT (name TEXT NOT NULL);\n\
    CREATE TYPE raw_argument_pair_test.assignment AS OBJECT (\n\
      label TEXT NOT NULL, owner REF raw_argument_pair_test.owner NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_pair(p_first TEXT, p_second TEXT)\n\
    RETURNS ROWS (created REF raw_argument_pair_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.probe AS made (first, second, marker)\n\
    VALUES (p_first, p_second, TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_unused(p_first TEXT, p_second TEXT)\n\
    RETURNS ROWS (created REF raw_argument_pair_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.probe AS made (first, marker)\n\
    VALUES (p_first, TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_indirect(p_first TEXT, p_second TEXT)\n\
    RETURNS ROWS (created REF raw_argument_pair_test.indirect_probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.indirect_probe AS made (nested)\n\
    VALUES (raw_argument_pair_test.pair_value{first: p_first, second: p_second})\n\
    RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_extra(\n\
      p_first TEXT, p_second TEXT, p_extra TEXT\n\
    ) RETURNS ROWS (created REF raw_argument_pair_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.probe AS made (first, second, marker)\n\
    VALUES (p_first, p_second, TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_owner(p_name TEXT)\n\
    RETURNS ROWS (created REF raw_argument_pair_test.owner)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.owner AS made (name)\n\
    VALUES (p_name) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.create_assignment(\n\
      p_label TEXT, p_owner REF raw_argument_pair_test.owner\n\
    ) RETURNS ROWS (created REF raw_argument_pair_test.assignment)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_argument_pair_test.assignment AS made (label, owner)\n\
    VALUES (p_label, p_owner) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.read_first()\n\
    RETURNS ROWS (first TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.first FROM raw_argument_pair_test.probe probe;\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.read_second()\n\
    RETURNS ROWS (second TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.second FROM raw_argument_pair_test.probe probe;\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.read_assignment_labels()\n\
    RETURNS ROWS (label TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT assignment.label FROM raw_argument_pair_test.assignment assignment;\n\
    CREATE SERVER FUNCTION raw_argument_pair_test.read_assignment_owners()\n\
    RETURNS ROWS (owner REF raw_argument_pair_test.owner)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT assignment.owner FROM raw_argument_pair_test.assignment assignment;\n";

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

// ADR 0050 keeps this fixture separate from the ADR 0041 constant UPDATE
// fixture. The scalar selector is declared after the value on purpose: calls
// below supply it first and therefore prove binding by ParameterId.
#[cfg(feature = "test-hooks")]
const RAW_REFERENCE_VALUE_UPDATE_SOURCE: &str = "CREATE SCHEMA raw_reference_value_update;\n\
    CREATE TYPE raw_reference_value_update.probe AS OBJECT (\n\
      stored TEXT NOT NULL, marker BOOLEAN NOT NULL, linked REF raw_reference_value_update.probe\n\
    );\n\
    CREATE SERVER FUNCTION raw_reference_value_update.create_probe(p_stored TEXT)\n\
    RETURNS ROWS (created REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO raw_reference_value_update.probe AS made (stored, marker)\n\
    VALUES (p_stored, TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION raw_reference_value_update.update_text(\n\
      p_value TEXT, p_probe REF raw_reference_value_update.probe\n\
    ) RETURNS ROWS (updated REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_update.probe AS changed\n\
    SET stored = p_value WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_update.update_link(\n\
      p_value REF raw_reference_value_update.probe,\n\
      p_probe REF raw_reference_value_update.probe\n\
    ) RETURNS ROWS (updated REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_update.probe AS changed\n\
    SET linked = p_value WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_update.update_unused(\n\
      p_value TEXT, p_probe REF raw_reference_value_update.probe\n\
    ) RETURNS ROWS (updated REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_update.probe AS changed\n\
    SET marker = FALSE WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_update.update_extra(\n\
      p_value TEXT, p_probe REF raw_reference_value_update.probe, p_extra TEXT\n\
    ) RETURNS ROWS (updated REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE raw_reference_value_update.probe AS changed\n\
    SET stored = p_value WHERE REF(changed) = p_probe RETURNING REF(changed);\n\
    CREATE SERVER FUNCTION raw_reference_value_update.read_stored()\n\
    RETURNS ROWS (stored TEXT)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.stored FROM raw_reference_value_update.probe probe;\n\
    CREATE SERVER FUNCTION raw_reference_value_update.read_links()\n\
    RETURNS ROWS (linked REF raw_reference_value_update.probe)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.linked FROM raw_reference_value_update.probe probe;\n";

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

// ADR 0051 uses one compact fixture for both nullable and required unique
// Text fields. The update parameter order intentionally puts the value before
// the selector, so this test also proves selector/value binding by ParameterId.
#[cfg(feature = "test-hooks")]
const UNIQUE_TEXT_SOURCE: &str = "CREATE SCHEMA text_claims;\n\
    CREATE TYPE text_claims.claim AS OBJECT (\n\
      nullable_value TEXT UNIQUE, required_value TEXT NOT NULL UNIQUE\n\
    );\n\
    CREATE SERVER FUNCTION text_claims.create(\n\
      p_nullable TEXT, p_required TEXT\n\
    ) RETURNS ROWS (created REF text_claims.claim)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO text_claims.claim AS made (nullable_value, required_value)\n\
    VALUES (p_nullable, p_required) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION text_claims.create_without_nullable(p_required TEXT)\n\
    RETURNS ROWS (created REF text_claims.claim)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO text_claims.claim AS made (required_value)\n\
    VALUES (p_required) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION text_claims.update(\n\
      p_value TEXT, p_claim REF text_claims.claim\n\
    ) RETURNS ROWS (updated REF text_claims.claim)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS UPDATE text_claims.claim AS changed SET required_value = p_value\n\
    WHERE REF(changed) = p_claim RETURNING REF(changed);\n";

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

#[cfg(feature = "test-hooks")]
#[derive(Clone, Copy)]
struct UniqueTextFixture {
    claim: TypeId,
    nullable_field: FieldId,
    required_field: FieldId,
    create: FunctionId,
    create_revision: FunctionRevisionId,
    create_nullable_parameter: ParameterId,
    create_required_parameter: ParameterId,
    create_without_nullable: FunctionId,
    create_without_nullable_parameter: ParameterId,
    update: FunctionId,
    update_revision: FunctionRevisionId,
    update_value_parameter: ParameterId,
    update_selector_parameter: ParameterId,
}

#[cfg(feature = "test-hooks")]
impl UniqueTextFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let object = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["text_claims", "claim"]))
            .ok_or_else(|| failure("text_claims.claim type is absent"))?;
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["text_claims", name]))
                .ok_or_else(|| failure(format!("text_claims.{name} function is absent")))
        };
        let parameter = |function: &orna_core::catalogue::FunctionDefinition, name| {
            function
                .parameter_by_name(name)
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("text_claims parameter {name} is absent")))
        };
        let create = function("create")?;
        let create_without_nullable = function("create_without_nullable")?;
        let update = function("update")?;
        Ok(Self {
            claim: object.id(),
            nullable_field: object
                .field_by_name("nullable_value")
                .map(|field| field.id())
                .ok_or_else(|| failure("nullable unique Text field is absent"))?,
            required_field: object
                .field_by_name("required_value")
                .map(|field| field.id())
                .ok_or_else(|| failure("required unique Text field is absent"))?,
            create: create.id(),
            create_revision: create.current_revision(),
            create_nullable_parameter: parameter(create, "p_nullable")?,
            create_required_parameter: parameter(create, "p_required")?,
            create_without_nullable: create_without_nullable.id(),
            create_without_nullable_parameter: parameter(create_without_nullable, "p_required")?,
            update: update.id(),
            update_revision: update.current_revision(),
            update_value_parameter: parameter(update, "p_value")?,
            update_selector_parameter: parameter(update, "p_claim")?,
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
fn run_large_stack_live_test<F, Fut>(name: &'static str, test: F) -> TestResult<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = TestResult<()>> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    failure(format!("{name} live runtime could not start: {error}"))
                })?;
            runtime.block_on(test())
        })
        .map_err(|error| failure(format!("{name} live thread could not start: {error}")))?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(failure(format!("{name} live thread panicked"))),
    }
}

#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_insert_is_denied_then_granted_and_audited() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-insert-live",
        authenticated_raw_insert_is_denied_then_granted_and_audited_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_insert_is_denied_then_granted_and_audited_inner() -> TestResult<()> {
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

/// Proves that one authenticated raw pair binds two same-typed parameters by
/// their active identities, not by declaration or supplied argument order.
///
/// The existing singleton journeys retain their one-argument boundaries. A
/// defaulted SERVER parameter cannot become an active target because the
/// compiler rejects it before mutation preparation. This tracer owns the pair
/// shapes that can reach PostgreSQL, authorisation order, U+0000 rollback,
/// and the typed execution-failure path.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_argument_pair_binds_two_active_parameters_and_audits() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-argument-pair-live",
        authenticated_raw_argument_pair_binds_two_active_parameters_and_audits_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_argument_pair_binds_two_active_parameters_and_audits_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel
            .apply(&standard_application_candidate(
                RAW_ARGUMENT_PAIR_SOURCE,
                &standard,
                &upgrade,
            )?)
            .await?;
        let pair = applied.pair();
        let probe = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_argument_pair_test", "probe"]))
            .ok_or_else(|| failure("raw argument-pair probe type is absent"))?
            .id();
        let create_pair = raw_function_id(&applied, &["raw_argument_pair_test", "create_pair"])?;
        let create_unused = raw_function_id(&applied, &["raw_argument_pair_test", "create_unused"])?;
        let create_indirect =
            raw_function_id(&applied, &["raw_argument_pair_test", "create_indirect"])?;
        let create_extra = raw_function_id(&applied, &["raw_argument_pair_test", "create_extra"])?;
        let create_owner = raw_function_id(&applied, &["raw_argument_pair_test", "create_owner"])?;
        let create_assignment =
            raw_function_id(&applied, &["raw_argument_pair_test", "create_assignment"])?;
        let read_first = raw_function_id(&applied, &["raw_argument_pair_test", "read_first"])?;
        let read_second = raw_function_id(&applied, &["raw_argument_pair_test", "read_second"])?;
        let read_assignment_labels = raw_function_id(
            &applied,
            &["raw_argument_pair_test", "read_assignment_labels"],
        )?;
        let read_assignment_owners = raw_function_id(
            &applied,
            &["raw_argument_pair_test", "read_assignment_owners"],
        )?;
        let create_definition = applied
            .catalogue()
            .function_by_id(create_pair)
            .ok_or_else(|| failure("raw argument-pair creator is absent"))?;
        let first_parameter = create_definition
            .parameter_by_name("p_first")
            .ok_or_else(|| failure("raw argument-pair p_first is absent"))?
            .id();
        let second_parameter = create_definition
            .parameter_by_name("p_second")
            .ok_or_else(|| failure("raw argument-pair p_second is absent"))?
            .id();
        require(
            first_parameter != second_parameter,
            "raw argument-pair parameters must have distinct identities",
        )?;
        let create_revision = create_definition.current_revision();
        let parameter = |function: FunctionId, name: &str| {
            applied
                .catalogue()
                .function_by_id(function)
                .and_then(|definition| definition.parameter_by_name(name))
                .map(|parameter| parameter.id())
                .ok_or_else(|| {
                    failure(format!(
                        "raw argument-pair {name} parameter is absent from {function}"
                    ))
                })
        };
        let unused_first = parameter(create_unused, "p_first")?;
        let unused_second = parameter(create_unused, "p_second")?;
        let indirect_first = parameter(create_indirect, "p_first")?;
        let indirect_second = parameter(create_indirect, "p_second")?;
        let extra_first = parameter(create_extra, "p_first")?;
        let extra_second = parameter(create_extra, "p_second")?;
        let owner_parameter = parameter(create_owner, "p_name")?;
        let assignment_label = parameter(create_assignment, "p_label")?;
        let assignment_owner = parameter(create_assignment, "p_owner")?;
        let owner = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["raw_argument_pair_test", "owner"]))
            .ok_or_else(|| failure("raw argument-pair owner type is absent"))?
            .id();
        let assignment = applied
            .catalogue()
            .object_types()
            .iter()
            .find(|object| {
                name_is(
                    object.name().parts(),
                    &["raw_argument_pair_test", "assignment"],
                )
            })
            .ok_or_else(|| failure("raw argument-pair assignment type is absent"))?
            .id();
        let exact_arguments = vec![
            FunctionArgument::new(
                second_parameter,
                RuntimeValue::Text(String::from("second exact value")),
            )?,
            FunctionArgument::new(
                first_parameter,
                RuntimeValue::Text(String::from("first exact value")),
            )?,
        ];
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        // Denial precedes the two supplied parameter identities and values.
        let denied = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &exact_arguments)
            .await
            .expect_err("an ungranted raw argument pair must be denied");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair: denied_pair,
                    function: denied_function,
                    reason: ExecuteDenial::MissingExecuteGrant,
                } if denied_pair == pair && denied_function == create_pair
            ),
            "raw argument-pair denial did not precede target inspection",
        )?;

        for function in [
            create_pair,
            create_unused,
            create_indirect,
            create_extra,
            create_owner,
            create_assignment,
            read_first,
            read_second,
            read_assignment_labels,
            read_assignment_owners,
        ] {
            kernel
                .grant_catalogue_health_service_execute(pair, function)
                .await?;
        }

        // All pair-specific target rejections that can reach the protected
        // INSERT branch retain one allowed audit and create no probe row.
        let mut wrong_parameter_bytes = first_parameter.to_bytes();
        wrong_parameter_bytes[0] ^= 0x01;
        let wrong_parameter = ParameterId::from_bytes(wrong_parameter_bytes);
        let rejected_pairs = [
            (
                create_pair,
                vec![
                    FunctionArgument::new(wrong_parameter, RuntimeValue::Text(String::from("wrong")))?,
                    FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
                ],
                "a wrong pair parameter",
            ),
            (
                create_pair,
                vec![
                    FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
                    FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("duplicate")))?,
                ],
                "a duplicate pair parameter",
            ),
            (
                create_pair,
                vec![FunctionArgument::new(
                    first_parameter,
                    RuntimeValue::Text(String::from("missing second")),
                )?],
                "a missing pair parameter",
            ),
            (
                create_pair,
                vec![
                    FunctionArgument::new(first_parameter, RuntimeValue::Integer(7))?,
                    FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
                ],
                "a mistyped pair parameter",
            ),
            (
                create_unused,
                vec![
                    FunctionArgument::new(unused_first, RuntimeValue::Text(String::from("used")))?,
                    FunctionArgument::new(unused_second, RuntimeValue::Text(String::from("unused")))?,
                ],
                "an unused pair parameter",
            ),
            (
                create_indirect,
                vec![
                    FunctionArgument::new(indirect_first, RuntimeValue::Text(String::from("nested first")))?,
                    FunctionArgument::new(indirect_second, RuntimeValue::Text(String::from("nested second")))?,
                ],
                "an indirectly used pair parameter",
            ),
            (
                create_extra,
                vec![
                    FunctionArgument::new(extra_first, RuntimeValue::Text(String::from("first")))?,
                    FunctionArgument::new(extra_second, RuntimeValue::Text(String::from("second")))?,
                ],
                "an extra declared pair parameter",
            ),
        ];
        for (function, arguments, label) in rejected_pairs {
            let unavailable = kernel
                .dispatch_authenticated_raw_call_with_arguments(&session, function, &arguments)
                .await
                .expect_err(label);
            require(
                matches!(
                    unavailable,
                    PostgresKernelError::RawCallTargetUnavailable {
                        function: actual,
                        rule: "raw SERVER INSERT argument target is unavailable",
                    } if actual == function
                ),
                format!("{label} returned {unavailable:?}"),
            )?;
        }
        let outer_extra = vec![
            FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
            FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
            FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("extra")))?,
        ];
        let extra = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &outer_extra)
            .await
            .expect_err("a third raw argument must close before PostgreSQL");
        require_raw_scalar_target_unavailable(
            &extra,
            create_pair,
            "raw calls accept zero arguments, one supported value, or one supported argument pair",
        )?;
        let unsupported = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, read_first, &exact_arguments)
            .await
            .expect_err("a pair must reject a non-INSERT raw target");
        require_raw_scalar_target_unavailable(
            &unsupported,
            read_first,
            "raw call arguments require a supported active SERVER mutation target",
        )?;

        // A scalar and Reference pair crosses the same protected path without
        // source rendering or positional binding.
        let owner_value = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_owner,
                &[FunctionArgument::new(
                    owner_parameter,
                    RuntimeValue::Text(String::from("pair owner")),
                )?],
            )
            .await?;
        let owner_object = raw_scalar_insert_reference(owner_value, owner)?;
        let assignment_value = kernel
            .dispatch_authenticated_raw_call_with_arguments(
                &session,
                create_assignment,
                &[
                    FunctionArgument::new(
                        assignment_owner,
                        RuntimeValue::Reference {
                            target: owner,
                            object: owner_object,
                        },
                    )?,
                    FunctionArgument::new(
                        assignment_label,
                        RuntimeValue::Text(String::from("scalar-reference pair")),
                    )?,
                ],
            )
            .await?;
        raw_scalar_insert_reference(assignment_value, assignment)?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_assignment_labels).await?,
            &RuntimeValue::Text(String::from("scalar-reference pair")),
            "raw scalar-Reference pair label",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_assignment_owners).await?,
            &RuntimeValue::Reference {
                target: owner,
                object: owner_object,
            },
            "raw scalar-Reference pair owner",
        )?;

        // Reverse supplied order while keeping the two values attached to
        // their identities. Same-typed fields prove identity, not position,
        // controls the stored row.
        let inserted = kernel
            .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &exact_arguments)
            .await?;
        let inserted = raw_scalar_insert_reference(inserted, probe)?;
        require(
            inserted != ObjectId::from_bytes([0; 16]),
            "raw argument-pair INSERT must allocate a real object identity",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_first).await?,
            &RuntimeValue::Text(String::from("first exact value")),
            "raw argument-pair first field",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_second).await?,
            &RuntimeValue::Text(String::from("second exact value")),
            "raw argument-pair second field",
        )?;

        // A database failure after complete pair validation stays a typed
        // SERVER INSERT failure and commits no partial pair row.
        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = session.clone();
        let execution_arguments = exact_arguments.clone();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move {
            executor
                .dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
                    &execution_session,
                    create_pair,
                    &execution_arguments,
                    execution_reached,
                    execution_resume,
                )
                .await
        });
        let triggered = finish_triggered_failure(
            &database,
            probe,
            TriggerKind::AfterRow,
            execution,
            reached,
            resume,
            "triggered raw argument-pair dispatch",
        )
        .await?;
        let (context, source) = match triggered {
            PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted { context, source }) => {
                (context, source)
            }
            other => return Err(failure(format!("triggered raw argument pair returned {other:?}"))),
        };
        require_context(context, pair, create_pair, create_revision)?;
        let ServerInsertError::Database { source } = source.as_ref() else {
            return Err(failure("triggered raw argument pair lost its database failure"));
        };
        require(
            source.as_db_error().map(|error| error.code()) == Some(&SqlState::RAISE_EXCEPTION),
            "triggered raw argument pair changed the PostgreSQL error code",
        )?;

        // Either Text position rejects U+0000 after authorisation, retains its
        // allowed audit, and rolls back without adding a row. The last case
        // supplies both invalid values in reverse stable-identity order, so
        // the protected validator must canonicalise before its private check.
        let (lower_parameter, higher_parameter) = if first_parameter.to_bytes() < second_parameter.to_bytes() {
            (first_parameter, second_parameter)
        } else {
            (second_parameter, first_parameter)
        };
        for arguments in [
            vec![
                FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("a\u{0}b")))?,
                FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("second")))?,
            ],
            vec![
                FunctionArgument::new(first_parameter, RuntimeValue::Text(String::from("first")))?,
                FunctionArgument::new(second_parameter, RuntimeValue::Text(String::from("a\u{0}b")))?,
            ],
            vec![
                FunctionArgument::new(higher_parameter, RuntimeValue::Text(String::from("higher\u{0}")))?,
                FunctionArgument::new(lower_parameter, RuntimeValue::Text(String::from("lower\u{0}")))?,
            ],
        ] {
            let unavailable = kernel
                .dispatch_authenticated_raw_call_with_arguments(&session, create_pair, &arguments)
                .await
                .expect_err("Text U+0000 must make a raw argument pair unavailable");
            require(
                matches!(
                    unavailable,
                    PostgresKernelError::RawCallTargetUnavailable { function, .. } if function == create_pair
                ),
                "Text U+0000 raw argument-pair rejection lost its target identity",
            )?;
        }
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_first).await?,
            &RuntimeValue::Text(String::from("first exact value")),
            "Text U+0000 raw argument-pair first field",
        )?;
        require_exact_scalar_read(
            &read_raw_scalar_values(&kernel, &session, read_second).await?,
            &RuntimeValue::Text(String::from("second exact value")),
            "Text U+0000 raw argument-pair second field",
        )?;

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
        let allowed = SecurityAuditOutcome::Allowed;
        let execute = SecurityAuditKind::Execute;
        let expected = vec![
            (SecurityAuditKind::Authentication, allowed, None),
            (execute, SecurityAuditOutcome::Denied, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_unused)),
            (execute, allowed, Some(create_indirect)),
            (execute, allowed, Some(create_extra)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(create_owner)),
            (execute, allowed, Some(create_assignment)),
            (execute, allowed, Some(read_assignment_labels)),
            (execute, allowed, Some(read_assignment_owners)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(read_second)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(create_pair)),
            (execute, allowed, Some(read_first)),
            (execute, allowed, Some(read_second)),
        ];
        require(
            actual == expected,
            "raw argument-pair audit sequence differs",
        )?;

        Ok(())
    })
    .await
}

/// ADR 0050 RED tracer for a two-argument raw UPDATE. The compiler closes
/// nullable and defaulted mutation parameters before this live boundary; this
/// test covers every remaining public pair shape and the savepoint path.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "requires the Compose PostgreSQL development service and ADR 0050 dispatch"]
fn authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-value-update-live",
        authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_value_update_binds_by_parameter_id_and_audits_inner()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        let applied = kernel.apply(&standard_application_candidate(
            RAW_REFERENCE_VALUE_UPDATE_SOURCE, &standard, &upgrade,
        )?).await?;
        let pair = applied.pair();
        let object = applied.catalogue().object_types().iter().find(|object| {
            name_is(object.name().parts(), &["raw_reference_value_update", "probe"])
        }).ok_or_else(|| failure("raw reference value probe is absent"))?.id();
        let function = |name| raw_function_id(&applied, &["raw_reference_value_update", name]);
        let create = function("create_probe")?;
        let update_text = function("update_text")?;
        let update_link = function("update_link")?;
        let update_unused = function("update_unused")?;
        let update_extra = function("update_extra")?;
        let read_stored = function("read_stored")?;
        let read_links = function("read_links")?;
        let parameter = |function, name| -> TestResult<ParameterId> {
            applied.catalogue().function_by_id(function)
                .and_then(|definition| definition.parameter_by_name(name))
                .map(|parameter| parameter.id())
                .ok_or_else(|| failure(format!("{name} parameter is absent")))
        };
        let text_value = parameter(update_text, "p_value")?;
        let text_selector = parameter(update_text, "p_probe")?;
        let link_value = parameter(update_link, "p_value")?;
        let link_selector = parameter(update_link, "p_probe")?;
        let unused_value = parameter(update_unused, "p_value")?;
        let unused_selector = parameter(update_unused, "p_probe")?;
        let extra_value = parameter(update_extra, "p_value")?;
        let extra_selector = parameter(update_extra, "p_probe")?;
        let revision = applied.catalogue().function_by_id(update_text)
            .ok_or_else(|| failure("raw text UPDATE is absent"))?.current_revision();
        let mut wrong_bytes = text_selector.to_bytes();
        wrong_bytes[0] ^= 1;
        let wrong = ParameterId::from_bytes(wrong_bytes);
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;
        for target in [create, read_stored, read_links] {
            kernel.grant_catalogue_health_service_execute(pair, target).await?;
        }
        let first = create_raw_reference_value_update_probe(&kernel, &session, create, "seed").await?;
        let second = create_raw_reference_value_update_probe(&kernel, &session, create, "seed").await?;
        require(first != second, "the two raw value UPDATE rows must differ")?;

        // Denial precedes selector and value inspection.
        let denied = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(wrong, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("denied".into()))?]
        ).await.expect_err("ungranted pair UPDATE must deny before facts");
        require(matches!(denied, PostgresKernelError::RawExecuteDenied { pair: actual, function, .. }
            if actual == pair && function == update_text), "pair UPDATE denial differs")?;
        require(read_probe_values(&kernel, &session, read_stored).await? == [RuntimeValue::Text("seed".into()), RuntimeValue::Text("seed".into())], "denied pair UPDATE changed a row")?;
        for target in [update_text, update_link, update_unused, update_extra] {
            kernel.grant_catalogue_health_service_execute(pair, target).await?;
        }
        let unavailable = |error: PostgresKernelError, target| require(matches!(error,
            PostgresKernelError::RawCallTargetUnavailable { function, .. } if function == target),
            "allowed invalid raw value UPDATE must be unavailable");
        // Wrong, duplicate, missing, extra, mistyped, and unused public shapes
        // close after the allowed audit. The compiler rejects nullable,
        // defaulted, and indirect record-constructor UPDATE values before this
        // fixture reaches PostgreSQL.
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(wrong, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("x".into()))?]).await.expect_err("wrong parameter must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_selector, second.clone())?]).await.expect_err("duplicate parameter must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?]).await.expect_err("missing value must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_extra,
            &[FunctionArgument::new(extra_selector, first.clone())?, FunctionArgument::new(extra_value, RuntimeValue::Text("x".into()))?]).await.expect_err("extra declaration must close"), update_extra)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, RuntimeValue::Text("not-a-reference".into()))?, FunctionArgument::new(text_value, first.clone())?]).await.expect_err("mistyped pair must close"), update_text)?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_unused,
            &[FunctionArgument::new(unused_selector, first.clone())?, FunctionArgument::new(unused_value, RuntimeValue::Text("unused".into()))?]).await.expect_err("unused value must close"), update_unused)?;

        // Supplied order is reverse declaration order. ParameterId selects the
        // Reference and scalar slots, returns the exact selector, and updates
        // only one row.
        let updated = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("changed".into()))?]).await?;
        require(matches!(updated, AuthenticatedRawCallResult::Server(values) if values == [first.clone()]), "scalar pair UPDATE result differs")?;
        let stored = read_probe_values(&kernel, &session, read_stored).await?;
        require(stored.len() == 2 && stored.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1 && stored.iter().filter(|value| **value == RuntimeValue::Text("seed".into())).count() == 1, "scalar pair UPDATE did not select one row")?;
        let RuntimeValue::Reference { object: first_object, .. } = &first else { return Err(failure("first raw value row is not a reference")); };
        let RuntimeValue::Reference { object: second_object, .. } = &second else { return Err(failure("second raw value row is not a reference")); };
        let absent_object = [[7; 16], [8; 16], [9; 16]].into_iter().map(ObjectId::from_bytes)
            .find(|candidate| candidate != first_object && candidate != second_object)
            .ok_or_else(|| failure("no deterministic absent object identity remains"))?;
        let absent = RuntimeValue::Reference { target: object, object: absent_object };
        let empty = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_value, RuntimeValue::Text("absent".into()))?, FunctionArgument::new(text_selector, absent)?]).await?;
        require(matches!(empty, AuthenticatedRawCallResult::Server(values) if values.is_empty()), "absent selector must complete empty")?;
        let linked = kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_link,
            &[FunctionArgument::new(link_selector, second.clone())?, FunctionArgument::new(link_value, first.clone())?]).await?;
        require(matches!(linked, AuthenticatedRawCallResult::Server(values) if values == [second.clone()]), "Reference value pair UPDATE result differs")?;
        require(read_probe_values(&kernel, &session, read_links).await?.iter().filter(|value| **value == first).count() == 1, "Reference value pair UPDATE did not select one row")?;
        unavailable(kernel.dispatch_authenticated_raw_call_with_arguments(&session, update_text,
            &[FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("nul\0text".into()))?]).await.expect_err("U+0000 must close"), update_text)?;
        let after_nul = read_probe_values(&kernel, &session, read_stored).await?;
        require(after_nul.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1, "U+0000 changed a row")?;

        let reached = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone(); let execution_session = session.clone();
        let arguments = vec![FunctionArgument::new(text_selector, first.clone())?, FunctionArgument::new(text_value, RuntimeValue::Text("rollback".into()))?];
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let execution = tokio::spawn(async move { executor.dispatch_authenticated_raw_call_with_arguments_and_test_barrier(&execution_session, update_text, &arguments, execution_reached, execution_resume).await });
        let triggered = finish_triggered_failure(&database, object, TriggerKind::AfterUpdate, execution,
            reached, resume, "raw value UPDATE").await;
        let triggered = triggered?;
        let PostgresKernelError::ServerUpdate(ServerUpdateError::NotCommitted {
            context,
            source,
        }) = triggered else {
            return Err(failure("raw value UPDATE operational failure is not internal"));
        };
        require_context(context, pair, update_text, revision)?;
        require(matches!(source.as_ref(), ServerMutationError::Database { source }
            if source.as_db_error().is_some_and(|error| error.code() == &SqlState::RAISE_EXCEPTION)),
            "raw value UPDATE operational failure lost SQLSTATE  P0001")?;
        let after_failure = read_probe_values(&kernel, &session, read_stored).await?;
        require(after_failure.iter().filter(|value| **value == RuntimeValue::Text("changed".into())).count() == 1,
            "operational raw value UPDATE failure did not roll back")?;
        let audits = kernel.recover_security_audit_events().await?;
        let expected = [
            (create, SecurityAuditOutcome::Allowed), (create, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Denied), (read_stored, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_extra, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_unused, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (read_stored, SecurityAuditOutcome::Allowed),
            (update_text, SecurityAuditOutcome::Allowed), (update_link, SecurityAuditOutcome::Allowed),
            (read_links, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (read_stored, SecurityAuditOutcome::Allowed), (update_text, SecurityAuditOutcome::Allowed),
            (read_stored, SecurityAuditOutcome::Allowed),
        ];
        require(audits.len() == expected.len() + 1
            && audits[0].decision().kind() == SecurityAuditKind::Authentication
            && audits[0].decision().outcome() == SecurityAuditOutcome::Allowed
            && audits[1..].iter().zip(expected).all(|(event, (function, outcome))|
                event.decision().kind() == SecurityAuditKind::Execute
                    && event.decision().outcome() == outcome
                    && event.decision().target() == Some(InvocationTarget::new(function, pair))),
            "raw reference value UPDATE audit sequence differs")?;
        Ok(())
    }).await
}

#[cfg(feature = "test-hooks")]
async fn create_raw_reference_value_update_probe(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    create: FunctionId,
    stored: &str,
) -> TestResult<RuntimeValue> {
    let parameter = kernel
        .recover()
        .await?
        .catalogue()
        .function_by_id(create)
        .and_then(|definition| definition.parameter_by_name("p_stored"))
        .map(|parameter| parameter.id())
        .ok_or_else(|| failure("raw reference value create parameter is absent"))?;
    let created = kernel
        .dispatch_authenticated_raw_call_with_arguments(
            session,
            create,
            &[FunctionArgument::new(
                parameter,
                RuntimeValue::Text(stored.into()),
            )?],
        )
        .await?;
    match created {
        AuthenticatedRawCallResult::Server(values) if values.len() == 1 => match &values[0] {
            RuntimeValue::Reference { target, object }
                if *object != ObjectId::from_bytes([0; 16]) =>
            {
                Ok(RuntimeValue::Reference {
                    target: *target,
                    object: *object,
                })
            }
            _ => Err(failure(
                "raw reference value create did not return a real reference",
            )),
        },
        other => Err(failure(format!(
            "raw reference value create must return one Server value, got {other:?}"
        ))),
    }
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
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_mutation_authority_and_selection() -> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-authority-live",
        authenticated_raw_reference_mutation_authority_and_selection_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_mutation_authority_and_selection_inner() -> TestResult<()> {
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
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit() -> TestResult<()>
{
    run_large_stack_live_test(
        "authenticated-raw-reference-non-literal-live",
        authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_update_rejects_non_literal_assignment_after_audit_inner()
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
#[test]
#[ignore = "requires the Compose PostgreSQL development service"]
fn authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit()
-> TestResult<()> {
    run_large_stack_live_test(
        "authenticated-raw-reference-database-failure-live",
        authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit_inner,
    )
}

#[cfg(feature = "test-hooks")]
async fn authenticated_raw_reference_update_database_failure_rolls_back_rows_and_retains_audit_inner()
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

#[cfg(feature = "test-hooks")]
async fn insert_unique_text(
    kernel: &PostgresKernel,
    fixture: UniqueTextFixture,
    nullable: RuntimeValue,
    required: &str,
) -> TestResult<ServerInsertResult> {
    Ok(kernel
        .execute_server_insert(
            fixture.create,
            &unique_text_arguments(fixture, nullable, required)?,
        )
        .await?)
}

#[cfg(feature = "test-hooks")]
async fn insert_unique_text_null(
    kernel: &PostgresKernel,
    fixture: UniqueTextFixture,
    required: &str,
) -> TestResult<ServerInsertResult> {
    Ok(kernel
        .execute_server_insert(
            fixture.create_without_nullable,
            &[FunctionArgument::new(
                fixture.create_without_nullable_parameter,
                RuntimeValue::Text(required.into()),
            )?],
        )
        .await?)
}

#[cfg(feature = "test-hooks")]
fn unique_text_arguments(
    fixture: UniqueTextFixture,
    nullable: RuntimeValue,
    required: &str,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.create_required_parameter,
            RuntimeValue::Text(required.into()),
        )?,
        FunctionArgument::new(fixture.create_nullable_parameter, nullable)?,
    ])
}

#[cfg(feature = "test-hooks")]
fn unique_text_update_arguments(
    fixture: UniqueTextFixture,
    selector: ObjectId,
    required: &str,
) -> TestResult<Vec<FunctionArgument>> {
    Ok(vec![
        FunctionArgument::new(
            fixture.update_selector_parameter,
            RuntimeValue::Reference {
                target: fixture.claim,
                object: selector,
            },
        )?,
        FunctionArgument::new(
            fixture.update_value_parameter,
            RuntimeValue::Text(required.into()),
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
fn require_unique_text_insert_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: UniqueTextFixture,
    expected_field: FieldId,
) -> TestResult<()> {
    let PostgresKernelError::ServerInsert(ServerInsertError::NotCommitted { context, source }) =
        error
    else {
        return Err(failure(
            "unique Text INSERT lacks a contextual NotCommitted result",
        ));
    };
    require_context(*context, pair, fixture.create, fixture.create_revision)?;
    require_unique_text_private_source(source.as_ref(), fixture, expected_field, "INSERT")?;
    require(
        source.to_string() == "this text value is already used by another object",
        "unique Text INSERT display exposes the wrong text",
    )?;
    require(
        error.to_string()
            == "row creation failed: the row was not added: this text value is already used by another object",
        "unique Text INSERT outer display differs",
    )
}

#[cfg(feature = "test-hooks")]
fn require_unique_text_update_conflict(
    error: &PostgresKernelError,
    pair: RevisionPair,
    fixture: UniqueTextFixture,
) -> TestResult<()> {
    let PostgresKernelError::ServerUpdate(ServerUpdateError::NotCommitted { context, source }) =
        error
    else {
        return Err(failure(
            "unique Text UPDATE lacks a contextual NotCommitted result",
        ));
    };
    require_context(*context, pair, fixture.update, fixture.update_revision)?;
    require_unique_text_private_source(source.as_ref(), fixture, fixture.required_field, "UPDATE")?;
    require(
        source.to_string() == "this text value is already used by another object",
        "unique Text UPDATE display exposes the wrong text",
    )?;
    require(
        error.to_string()
            == "object update failed: the object was not updated: this text value is already used by another object",
        "unique Text UPDATE outer display differs",
    )
}

#[cfg(feature = "test-hooks")]
fn require_unique_text_private_source(
    source: &ServerMutationError,
    fixture: UniqueTextFixture,
    expected_field: FieldId,
    operation: &str,
) -> TestResult<()> {
    // This stays a runtime oracle until production provides the public enum
    // variant. Debug retains the private typed context without placing it in a
    // stable display, audit record, or socket frame.
    let debug = format!("{source:?}");
    require(
        debug.contains("UniqueTextConflict"),
        format!("unique Text {operation} was not classified as UniqueTextConflict"),
    )?;
    require(
        debug.contains(&format!("{:?}", fixture.claim))
            && debug.contains(&format!("{expected_field:?}")),
        format!("unique Text {operation} private owner or field differs"),
    )?;
    let database = std::error::Error::source(source)
        .and_then(|error| error.downcast_ref::<tokio_postgres::Error>())
        .ok_or_else(|| {
            failure(format!(
                "unique Text {operation} lost its PostgreSQL source"
            ))
        })?;
    let database = database.as_db_error().ok_or_else(|| {
        failure(format!(
            "unique Text {operation} PostgreSQL source has no database diagnostics"
        ))
    })?;
    require(
        database.code() == &SqlState::UNIQUE_VIOLATION,
        format!("unique Text {operation} lost SQLSTATE 23505"),
    )?;
    require(
        database.constraint() == Some(unique_constraint_name(expected_field).as_str()),
        format!("unique Text {operation} constraint differs"),
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

#[cfg(feature = "test-hooks")]
async fn require_unique_text_row(
    database: &TestDatabase,
    fixture: UniqueTextFixture,
    object: ObjectId,
    nullable: Option<&str>,
    required: &str,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<(Option<String>, String)> = async {
        let row = session
            .client()
            .query_one(
                &format!(
                    "SELECT {}, {} FROM {} WHERE _orna_object_id = $1",
                    field(fixture.nullable_field),
                    field(fixture.required_field),
                    relation(fixture.claim),
                ),
                &[&object.to_bytes().to_vec()],
            )
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    .await;
    let (stored_nullable, stored_required) =
        finish_session(session, operation, "unique Text row inspection").await?;
    require(
        stored_nullable.as_deref() == nullable,
        "stored nullable unique Text differs",
    )?;
    require(
        stored_required == required,
        "stored required unique Text differs",
    )
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
