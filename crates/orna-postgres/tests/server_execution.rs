//! Live PostgreSQL tests for the bounded active SERVER SELECT entry point.

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod support;

use std::str::FromStr;

#[cfg(feature = "test-hooks")]
use std::{future::Future, time::Duration};

use orna_compiler::{
    StandardApplicationCheckContext, check, check_standard_application, prepare,
    prepare_standard_application,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    catalogue::FunctionReturn,
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
    value::{EnumValue, FunctionArgument, RecordValue, RuntimeFloat, RuntimeValue},
};
#[cfg(feature = "test-hooks")]
use orna_core::{
    PrincipalId,
    security::{
        ExecuteDenial, ExecuteGrant, InvocationTarget, Principal, PrincipalKind, PrincipalStatus,
        RoleMembership, SecurityAuditDenial, SecurityAuditEvent, SecurityAuditKind,
        SecurityAuditOutcome, SecuritySnapshot,
    },
};
use orna_postgres::{
    PostgresKernel, PostgresKernelError, ServerSelectError, ServerSelectResult,
};
use orna_protocol::{ValueCodecError, encode_active_value};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    FLOAT_TYPE_ID, INTEGER_TYPE_ID,
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

const ENUM_EXECUTION_SOURCE: &str = r"CREATE SCHEMA enum_exec;
    CREATE TYPE enum_exec.stage AS ENUM ('lead', 'qualified', 'customer');
    CREATE TYPE enum_exec.case AS OBJECT (stage enum_exec.stage NOT NULL);
    CREATE SERVER FUNCTION enum_exec.read()
    RETURNS ROWS (stage enum_exec.stage)
    AS SELECT item.stage FROM enum_exec.case item;
";

const RECORD_EXECUTION_SOURCE: &str = r"CREATE SCHEMA record_exec;
    CREATE TYPE record_exec.stage AS ENUM ('lead', 'qualified');
    CREATE TYPE record_exec.status AS VALUE (enabled BOOLEAN, stage record_exec.stage)
    IMMUTABLE PERSISTABLE;
    CREATE TYPE record_exec.case AS OBJECT (status record_exec.status NOT NULL);
    CREATE SERVER FUNCTION record_exec.read()
    RETURNS ROWS (status record_exec.status)
    AS SELECT item.status FROM record_exec.case item;
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

const DIRECT_BOOLEAN_PREDICATE_SOURCE: &str = r"CREATE SCHEMA predicate;
    CREATE TYPE predicate.grandchild AS OBJECT (active BOOL);
    CREATE TYPE predicate.child AS OBJECT (
      active BOOL, grandchild REF predicate.grandchild
    );
    CREATE TYPE predicate.row AS OBJECT (
      child REF predicate.child, active BOOL, value INT NOT NULL, label TEXT NOT NULL
    );
    CREATE SERVER FUNCTION predicate.active()
    RETURNS ROWS (value INT, label TEXT)
    AS SELECT r.value, r.label FROM predicate.row r WHERE r.active ORDER BY r.value;
    CREATE SERVER FUNCTION predicate.child_active()
    RETURNS ROWS (value INT, label TEXT)
    AS SELECT r.value, r.label FROM predicate.row r WHERE r.child.active ORDER BY r.value;
    CREATE SERVER FUNCTION predicate.always()
    RETURNS ROWS (value INT, label TEXT)
    AS SELECT r.value, r.label FROM predicate.row r WHERE TRUE ORDER BY r.value;
    CREATE SERVER FUNCTION predicate.never()
    RETURNS ROWS (value INT, label TEXT)
    AS SELECT r.value, r.label FROM predicate.row r WHERE FALSE ORDER BY r.value;
    CREATE SERVER FUNCTION predicate.distinct_child_active()
    RETURNS ROWS (value INT)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT DISTINCT r.value FROM predicate.row r WHERE r.child.grandchild.active;
    CREATE SERVER FUNCTION predicate.distinct_always()
    RETURNS ROWS (value INT)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT DISTINCT r.value FROM predicate.row r WHERE TRUE;
    CREATE SERVER FUNCTION predicate.distinct_never()
    RETURNS ROWS (value INT)
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
    AS SELECT DISTINCT r.value FROM predicate.row r WHERE FALSE;
";

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
        execute_exact_fixture(&database, &kernel, &applied, fixture).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn executes_installed_standard_values_with_the_legacy_select_result_shape() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA exec;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let standard_candidate =
            standard_execution_candidate(EXECUTION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&standard_candidate).await?;
        let fixture = Fixture::from_active(&applied)?;
        require_standard_execution_value_identities(&applied, fixture, &upgrade)?;
        execute_exact_fixture(&database, &kernel, &applied, fixture).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn executes_catalogue_enum_results_and_rejects_undeclared_storage_labels() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA enum_exec;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_execution_candidate(ENUM_EXECUTION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;
        let enum_type = applied
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("enum execution catalogue has no enum"))?;
        let object = applied
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("enum execution catalogue has no object"))?;
        let enum_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("enum execution object has no field"))?;
        let function = applied
            .catalogue()
            .functions()
            .first()
            .ok_or_else(|| failure("enum execution catalogue has no function"))?;
        let object_id = ObjectId::from_bytes([0xe1; 16]);
        let session = database.open().await?;
        let insert = format!(
            "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
            relation(object.id()),
            field(enum_field.id()),
        );
        session
            .client()
            .execute(&insert, &[&object_id.to_bytes().to_vec(), &"qualified"])
            .await?;
        session.shutdown().await?;

        let result = kernel.execute_server_select(function.id()).await?;
        require(
            result.rows().columns().len() == 1
                && result.rows().columns()[0].resolved_type()
                    == ResolvedType::named(enum_type.id())
                && result.rows().rows().len() == 1,
            "enum SELECT did not preserve its declared result shape",
        )?;
        let [RuntimeValue::Enum(value)] = result.rows().rows()[0].values() else {
            return Err(failure("enum SELECT did not return one typed enum value"));
        };
        require(
            value.enum_type() == enum_type.id() && value.label() == "qualified",
            "enum SELECT returned the wrong type or label",
        )?;

        let session = database.open().await?;
        let update = format!(
            "UPDATE {} SET {} = $1 WHERE _orna_object_id = $2",
            relation(object.id()),
            field(enum_field.id()),
        );
        session
            .client()
            .execute(&update, &[&"retired", &object_id.to_bytes().to_vec()])
            .await?;
        session.shutdown().await?;
        let error = kernel
            .execute_server_select(function.id())
            .await
            .expect_err("undeclared stored label must fail");
        require_enum_value_error(
            &error,
            applied.pair(),
            function.id(),
            function.current_revision(),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn executes_canonical_named_record_results_and_rejects_malformed_storage() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA record_exec;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_execution_candidate(RECORD_EXECUTION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;
        let record = applied
            .catalogue()
            .record_value_types()
            .first()
            .ok_or_else(|| failure("record execution catalogue has no record"))?;
        let enum_type = applied
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("record execution catalogue has no enum"))?;
        let object = applied
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("record execution catalogue has no object"))?;
        let record_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("record execution object has no field"))?;
        let function = applied
            .catalogue()
            .functions()
            .first()
            .ok_or_else(|| failure("record execution catalogue has no function"))?;
        let value = RuntimeValue::Record(RecordValue::new(
            &applied,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(EnumValue::new(
                        applied.catalogue(),
                        enum_type.id(),
                        "qualified",
                    )?),
                ),
            ],
        )?);
        let encoded = encode_active_value(&applied, &value)?;
        let object_id = ObjectId::from_bytes([0xe3; 16]);
        let session = database.open().await?;
        session
            .client()
            .execute(
                &format!(
                    "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
                    relation(object.id()),
                    field(record_field.id()),
                ),
                &[&object_id.to_bytes().to_vec(), &encoded],
            )
            .await?;
        session.shutdown().await?;

        let result = kernel.execute_server_select(function.id()).await?;
        require(
            result.pair() == applied.pair()
                && result.function() == function.id()
                && result.function_revision() == function.current_revision()
                && result.rows().columns().len() == 1
                && result.rows().columns()[0].name() == "status"
                && result.rows().columns()[0].resolved_type() == ResolvedType::named(record.id())
                && result.rows().rows().len() == 1,
            "record SELECT changed its pinned result identity or shape",
        )?;
        let [RuntimeValue::Record(actual)] = result.rows().rows()[0].values() else {
            return Err(failure("record SELECT did not return one named record"));
        };
        require(
            actual.record_type() == record.id()
                && actual.fields()
                    == [
                        RuntimeValue::Boolean(true),
                        RuntimeValue::Enum(EnumValue::new(
                            applied.catalogue(),
                            enum_type.id(),
                            "qualified",
                        )?),
                    ],
            "record SELECT changed the nominal type, field order, or values",
        )?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                &format!(
                    "UPDATE {} SET {} = $1 WHERE _orna_object_id = $2",
                    relation(object.id()),
                    field(record_field.id()),
                ),
                &[&b"ORV3".to_vec(), &object_id.to_bytes().to_vec()],
            )
            .await?;
        session.shutdown().await?;
        let error = kernel
            .execute_server_select(function.id())
            .await
            .expect_err("malformed stored record must fail");
        require_record_codec_error(
            &error,
            applied.pair(),
            function.id(),
            function.current_revision(),
        )
    })
    .await
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_server_select_commits_allowed_and_denied_execute_decisions() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one = kernel
            .apply(&candidate("CREATE SCHEMA enum_exec;\n", &empty)?)
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_execution_candidate(ENUM_EXECUTION_SOURCE, &version_two, &upgrade)?;
        let applied = kernel.apply(&candidate).await?;
        let enum_type = applied
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no enum"))?;
        let object = applied
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no object"))?;
        let enum_field = object
            .fields()
            .first()
            .ok_or_else(|| failure("authenticated enum object has no field"))?;
        let function = applied
            .catalogue()
            .functions()
            .first()
            .ok_or_else(|| failure("authenticated enum catalogue has no function"))?;
        let principal = PrincipalId::from_bytes([0xa7; 16]);
        let function_ids = applied
            .catalogue()
            .functions()
            .iter()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        let allowed = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(principal, function.id())],
        )?;
        let allowed = kernel.replace_security_snapshot(&allowed).await?;
        let allowed_session = allowed.bind_authenticated_session(principal, vec![])?;
        let object_id = ObjectId::from_bytes([0xe2; 16]);
        let session = database.open().await?;
        let insert = format!(
            "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
            relation(object.id()),
            field(enum_field.id()),
        );
        session
            .client()
            .execute(&insert, &[&object_id.to_bytes().to_vec(), &"customer"])
            .await?;
        session.shutdown().await?;

        let result = kernel
            .execute_authenticated_server_select(&allowed_session, function.id(), &[])
            .await?;
        let [RuntimeValue::Enum(value)] = result.rows().rows()[0].values() else {
            return Err(failure(
                "authenticated SERVER SELECT did not return one enum value",
            ));
        };
        require(
            result.pair() == applied.pair()
                && result.function() == function.id()
                && result.function_revision() == function.current_revision()
                && value.enum_type() == enum_type.id()
                && value.label() == "customer",
            "authenticated SERVER SELECT changed its pinned result",
        )?;

        let unexpected_parameter = ParameterId::from_bytes([0xa8; 16]);
        let invalid_arguments = vec![FunctionArgument::new(
            unexpected_parameter,
            RuntimeValue::Boolean(true),
        )?];
        let error = kernel
            .execute_authenticated_server_select(
                &allowed_session,
                function.id(),
                &invalid_arguments,
            )
            .await
            .expect_err("allowed invalid arguments must fail after audit");
        require_select_argument_error(
            &error,
            applied.pair(),
            function.id(),
            function.current_revision(),
            None,
            "this function does not accept arguments",
        )?;

        let role = PrincipalId::from_bytes([0xa9; 16]);
        let selected = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        let selected = kernel.replace_security_snapshot(&selected).await?;
        let selected_session = selected.bind_authenticated_session(principal, vec![role])?;
        let reached = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let executor = kernel.clone();
        let execution_session = selected_session.clone();
        let execution_function = function.id();
        let execution_reached = reached.clone();
        let execution_resume = resume.clone();
        let mut execution = ExecutionTask::new(tokio::spawn(async move {
            executor
                .execute_authenticated_server_select_with_test_barrier(
                    &execution_session,
                    execution_function,
                    &[],
                    execution_reached,
                    execution_resume,
                )
                .await
        }));
        if tokio::time::timeout(WAIT, reached.wait()).await.is_err() {
            execution.abort_and_wait().await;
            return Err(failure(
                "authenticated SERVER SELECT did not pin its security snapshot",
            ));
        }
        let revoked = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        if tokio::time::timeout(WAIT, resume.wait()).await.is_err() {
            execution.abort_and_wait().await;
            return Err(failure(
                "authenticated SERVER SELECT did not resume after security replacement",
            ));
        }
        execution
            .finish("authenticated SERVER SELECT snapshot proof")
            .await?;
        kernel.replace_security_snapshot(&selected).await?;

        let unselected_session = selected.bind_authenticated_session(principal, vec![])?;
        let error = kernel
            .execute_authenticated_server_select(&unselected_session, function.id(), &[])
            .await
            .expect_err("unselected role must not authorise SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::MissingExecuteGrant,
        )?;

        let unknown_function = FunctionId::from_bytes([0xaa; 16]);
        let error = kernel
            .execute_authenticated_server_select(&selected_session, unknown_function, &[])
            .await
            .expect_err("unknown function must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            unknown_function,
            ExecuteDenial::UnknownFunction,
        )?;

        let stale = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        kernel.replace_security_snapshot(&stale).await?;
        let error = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await
            .expect_err("stale active-role selection must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        let disabled = SecuritySnapshot::new(
            applied.pair(),
            function_ids.clone(),
            vec![
                Principal::new(principal, PrincipalKind::User, PrincipalStatus::Disabled),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, principal)],
            vec![ExecuteGrant::new(role, function.id())],
        )?;
        kernel.replace_security_snapshot(&disabled).await?;
        let error = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await
            .expect_err("disabled session must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        let replacement_principal = PrincipalId::from_bytes([0xab; 16]);
        let unknown = SecuritySnapshot::new(
            applied.pair(),
            function_ids,
            vec![Principal::new(
                replacement_principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&unknown).await?;
        let error = kernel
            .execute_authenticated_server_select(&allowed_session, function.id(), &[])
            .await
            .expect_err("unknown session must deny SERVER SELECT");
        require_server_execute_denial(
            &error,
            applied.pair(),
            function.id(),
            ExecuteDenial::InvalidSession,
        )?;

        kernel.replace_security_snapshot(&selected).await?;
        let error = kernel
            .execute_authenticated_server_select_with_forced_post_commit_driver_shutdown(
                &selected_session,
                function.id(),
                &[],
            )
            .await
            .expect_err("post-commit driver shutdown must fail SERVER SELECT");
        require(
            matches!(error, PostgresKernelError::DriverTask(_)),
            "authenticated SERVER SELECT hid its post-commit driver failure",
        )?;

        let audits = kernel.recover_security_audit_events().await?;
        let execute = audits
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 9,
            "authenticated SERVER audit count differs",
        )?;
        let target = InvocationTarget::new(function.id(), applied.pair());
        require_server_execute_audit(
            execute[0],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(principal),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[1],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(principal),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[2],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(role),
            target,
            None,
        )?;
        require_server_execute_audit(
            execute[3],
            SecurityAuditOutcome::Denied,
            principal,
            None,
            None,
            target,
            Some(ExecuteDenial::MissingExecuteGrant),
        )?;
        require_server_execute_audit(
            execute[4],
            SecurityAuditOutcome::Denied,
            principal,
            None,
            None,
            InvocationTarget::new(unknown_function, applied.pair()),
            Some(ExecuteDenial::UnknownFunction),
        )?;
        for event in &execute[5..8] {
            require_server_execute_audit(
                event,
                SecurityAuditOutcome::Denied,
                principal,
                None,
                None,
                target,
                Some(ExecuteDenial::InvalidSession),
            )?;
        }
        require_server_execute_audit(
            execute[8],
            SecurityAuditOutcome::Allowed,
            principal,
            Some(principal),
            Some(role),
            target,
            None,
        )?;

        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 ADD CONSTRAINT reject_authenticated_server_audit
                 CHECK (false) NOT VALID",
            )
            .await?;
        session.shutdown().await?;
        let audit_failure = kernel
            .execute_authenticated_server_select(&selected_session, function.id(), &[])
            .await;
        let session = database.open().await?;
        session
            .client()
            .batch_execute(
                "ALTER TABLE _orna_kernel.security_audit_events
                 DROP CONSTRAINT reject_authenticated_server_audit",
            )
            .await?;
        session.shutdown().await?;
        let error = audit_failure.expect_err("rejected audit insert must fail SERVER SELECT");
        require(
            matches!(
                error,
                PostgresKernelError::Database(ref source)
                    if source.as_db_error().and_then(|error| error.constraint())
                        == Some("reject_authenticated_server_audit")
            ),
            "authenticated SERVER SELECT hid its audit insert failure",
        )?;
        let after_failure = kernel.recover_security_audit_events().await?;
        require(
            after_failure
                .iter()
                .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
                .count()
                == 9,
            "failed authenticated SERVER audit inserted a decision",
        )?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
fn require_server_execute_denial(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    reason: ExecuteDenial,
) -> TestResult<()> {
    require(
        matches!(
            error,
            PostgresKernelError::ServerExecuteDenied {
                pair: denied_pair,
                function: denied_function,
                reason: denied_reason,
            } if *denied_pair == pair && *denied_function == function && *denied_reason == reason
        ),
        "authenticated SERVER denial changed its exact context",
    )
}

#[cfg(feature = "test-hooks")]
fn require_server_execute_audit(
    event: &SecurityAuditEvent,
    outcome: SecurityAuditOutcome,
    session: PrincipalId,
    effective: Option<PrincipalId>,
    authorising: Option<PrincipalId>,
    target: InvocationTarget,
    denial: Option<ExecuteDenial>,
) -> TestResult<()> {
    require(
        event.decision().kind() == SecurityAuditKind::Execute
            && event.decision().outcome() == outcome
            && event.decision().session_principal() == Some(session)
            && event.decision().effective_principal() == effective
            && event.decision().authorising_principal() == authorising
            && event.decision().target() == Some(target)
            && event.decision().denial() == denial.map(SecurityAuditDenial::Execute),
        "authenticated SERVER audit decision changed its closed evidence",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn direct_boolean_predicates_preserve_v1_and_distinct_truth_null_order_and_duplicates()
-> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = hostile_kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let applied = kernel
            .apply(&candidate(DIRECT_BOOLEAN_PREDICATE_SOURCE, &active)?)
            .await?;
        let fixture = DirectBooleanPredicateFixture::from_active(&applied)?;
        insert_direct_boolean_predicate_rows(&database, fixture).await?;
        install_direct_boolean_predicate_decoy(&database, fixture).await?;
        let before_grandchildren = count_rows(&database, fixture.grandchild).await?;
        let before_children = count_rows(&database, fixture.child).await?;
        let before_rows = count_rows(&database, fixture.row).await?;

        let root_active = kernel.execute_server_select(fixture.active).await?;
        require_result_identity(
            &root_active,
            applied.pair(),
            fixture.active,
            fixture.active_revision,
        )?;
        require_direct_boolean_predicate_columns(&root_active)?;
        require_direct_boolean_predicate_rows(
            &root_active,
            &[
                (10, "duplicate"),
                (10, "duplicate"),
                (40, "missing child"),
                (50, "child false"),
                (60, "child null"),
            ],
            "direct root Boolean predicate",
        )?;

        let child_active = kernel
            .execute_server_select(fixture.child_active_function)
            .await?;
        require_result_identity(
            &child_active,
            applied.pair(),
            fixture.child_active_function,
            fixture.child_active_revision,
        )?;
        require_direct_boolean_predicate_columns(&child_active)?;
        require_direct_boolean_predicate_rows(
            &child_active,
            &[
                (10, "duplicate"),
                (10, "duplicate"),
                (20, "root false"),
                (30, "root null"),
            ],
            "nullable child Boolean predicate",
        )?;

        let always = kernel.execute_server_select(fixture.always).await?;
        require_result_identity(
            &always,
            applied.pair(),
            fixture.always,
            fixture.always_revision,
        )?;
        require_direct_boolean_predicate_columns(&always)?;
        require_direct_boolean_predicate_rows(
            &always,
            &[
                (10, "duplicate"),
                (10, "duplicate"),
                (20, "root false"),
                (30, "root null"),
                (40, "missing child"),
                (50, "child false"),
                (60, "child null"),
            ],
            "TRUE Boolean predicate",
        )?;

        let never = kernel.execute_server_select(fixture.never).await?;
        require_result_identity(
            &never,
            applied.pair(),
            fixture.never,
            fixture.never_revision,
        )?;
        require_direct_boolean_predicate_columns(&never)?;
        require(
            never.rows().rows().is_empty(),
            "FALSE Boolean predicate returned a row",
        )?;

        let distinct_child_active = kernel
            .execute_server_select(fixture.distinct_child_active)
            .await?;
        require_result_identity(
            &distinct_child_active,
            applied.pair(),
            fixture.distinct_child_active,
            fixture.distinct_child_active_revision,
        )?;
        require_direct_boolean_distinct_columns(&distinct_child_active)?;
        require_unordered_rows(
            &distinct_child_active,
            direct_boolean_distinct_values(&[10, 20, 30]),
            "nullable multi-hop direct Boolean DISTINCT predicate",
        )?;

        let distinct_always = kernel
            .execute_server_select(fixture.distinct_always)
            .await?;
        require_result_identity(
            &distinct_always,
            applied.pair(),
            fixture.distinct_always,
            fixture.distinct_always_revision,
        )?;
        require_direct_boolean_distinct_columns(&distinct_always)?;
        require_unordered_rows(
            &distinct_always,
            direct_boolean_distinct_values(&[10, 20, 30, 40, 50, 60]),
            "TRUE direct Boolean DISTINCT predicate",
        )?;

        let distinct_never = kernel.execute_server_select(fixture.distinct_never).await?;
        require_result_identity(
            &distinct_never,
            applied.pair(),
            fixture.distinct_never,
            fixture.distinct_never_revision,
        )?;
        require_direct_boolean_distinct_columns(&distinct_never)?;
        require(
            distinct_never.rows().rows().is_empty(),
            "FALSE direct Boolean DISTINCT predicate returned a row",
        )?;
        require(
            count_rows(&database, fixture.row).await? == before_rows,
            "direct Boolean predicate execution changed source rows",
        )?;
        require(
            count_rows(&database, fixture.child).await? == before_children,
            "direct Boolean predicate execution changed child rows",
        )?;
        require(
            count_rows(&database, fixture.grandchild).await? == before_grandchildren,
            "direct Boolean predicate execution changed grandchild rows",
        )?;
        require(
            kernel.recover().await?.pair() == applied.pair(),
            "direct Boolean predicate execution changed the active pair",
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

#[derive(Clone, Copy)]
struct DirectBooleanPredicateFixture {
    grandchild: TypeId,
    child: TypeId,
    row: TypeId,
    grandchild_active: FieldId,
    child_active: FieldId,
    child_grandchild: FieldId,
    row_child: FieldId,
    row_active: FieldId,
    row_value: FieldId,
    row_label: FieldId,
    active: FunctionId,
    child_active_function: FunctionId,
    always: FunctionId,
    never: FunctionId,
    distinct_child_active: FunctionId,
    distinct_always: FunctionId,
    distinct_never: FunctionId,
    active_revision: FunctionRevisionId,
    child_active_revision: FunctionRevisionId,
    always_revision: FunctionRevisionId,
    never_revision: FunctionRevisionId,
    distinct_child_active_revision: FunctionRevisionId,
    distinct_always_revision: FunctionRevisionId,
    distinct_never_revision: FunctionRevisionId,
    true_grandchild: ObjectId,
    false_grandchild: ObjectId,
    null_grandchild: ObjectId,
    true_child: ObjectId,
    false_child: ObjectId,
    null_child: ObjectId,
}

impl DirectBooleanPredicateFixture {
    fn from_active(active: &ActiveDatabaseRevision) -> TestResult<Self> {
        let grandchild = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["predicate", "grandchild"]))
            .ok_or_else(|| failure("direct Boolean grandchild type is absent"))?;
        let child = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["predicate", "child"]))
            .ok_or_else(|| failure("direct Boolean child type is absent"))?;
        let row = active
            .catalogue()
            .object_types()
            .iter()
            .find(|object| name_is(object.name().parts(), &["predicate", "row"]))
            .ok_or_else(|| failure("direct Boolean row type is absent"))?;
        let grandchild_field = |name| {
            grandchild
                .field_by_name(name)
                .map(|field| field.id())
                .ok_or_else(|| failure(format!("direct Boolean grandchild field {name} is absent")))
        };
        let child_field = |name| {
            child
                .field_by_name(name)
                .map(|field| field.id())
                .ok_or_else(|| failure(format!("direct Boolean child field {name} is absent")))
        };
        let row_field = |name| {
            row.field_by_name(name)
                .map(|field| field.id())
                .ok_or_else(|| failure(format!("direct Boolean row field {name} is absent")))
        };
        let function = |name| {
            active
                .catalogue()
                .functions()
                .iter()
                .find(|function| name_is(function.name().parts(), &["predicate", name]))
                .ok_or_else(|| failure(format!("direct Boolean function {name} is absent")))
        };
        let active_function = function("active")?;
        let child_active_function = function("child_active")?;
        let always = function("always")?;
        let never = function("never")?;
        let distinct_child_active = function("distinct_child_active")?;
        let distinct_always = function("distinct_always")?;
        let distinct_never = function("distinct_never")?;
        Ok(Self {
            grandchild: grandchild.id(),
            child: child.id(),
            row: row.id(),
            grandchild_active: grandchild_field("active")?,
            child_active: child_field("active")?,
            child_grandchild: child_field("grandchild")?,
            row_child: row_field("child")?,
            row_active: row_field("active")?,
            row_value: row_field("value")?,
            row_label: row_field("label")?,
            active: active_function.id(),
            child_active_function: child_active_function.id(),
            always: always.id(),
            never: never.id(),
            distinct_child_active: distinct_child_active.id(),
            distinct_always: distinct_always.id(),
            distinct_never: distinct_never.id(),
            active_revision: active_function.current_revision(),
            child_active_revision: child_active_function.current_revision(),
            always_revision: always.current_revision(),
            never_revision: never.current_revision(),
            distinct_child_active_revision: distinct_child_active.current_revision(),
            distinct_always_revision: distinct_always.current_revision(),
            distinct_never_revision: distinct_never.current_revision(),
            true_grandchild: ObjectId::from_bytes([0x71; 16]),
            false_grandchild: ObjectId::from_bytes([0x72; 16]),
            null_grandchild: ObjectId::from_bytes([0x73; 16]),
            true_child: ObjectId::from_bytes([0x81; 16]),
            false_child: ObjectId::from_bytes([0x82; 16]),
            null_child: ObjectId::from_bytes([0x83; 16]),
        })
    }
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

async fn execute_exact_fixture(
    database: &TestDatabase,
    kernel: &PostgresKernel,
    active: &ActiveDatabaseRevision,
    fixture: Fixture,
) -> TestResult<()> {
    insert_execution_rows(database, fixture).await?;

    let result = kernel.execute_server_select(fixture.read).await?;
    require_result_identity(&result, active.pair(), fixture.read, fixture.read_revision)?;
    require_exact_columns(&result, fixture)?;
    require_exact_rows(&result, fixture, 20)?;

    let empty = kernel.execute_server_select(fixture.none).await?;
    require_result_identity(&empty, active.pair(), fixture.none, fixture.none_revision)?;
    require(
        empty.rows().rows().is_empty(),
        "zero-match function returned a row",
    )?;
    require_no_session_leaks(database).await
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

async fn insert_direct_boolean_predicate_rows(
    database: &TestDatabase,
    fixture: DirectBooleanPredicateFixture,
) -> TestResult<()> {
    let grandchild_statement = format!(
        "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
        relation(fixture.grandchild),
        field(fixture.grandchild_active),
    );
    let child_statement = format!(
        "INSERT INTO {} (_orna_object_id, {}, {}) VALUES ($1, $2, $3)",
        relation(fixture.child),
        field(fixture.child_active),
        field(fixture.child_grandchild),
    );
    let row_statement = format!(
        "INSERT INTO {} (_orna_object_id, {}, {}, {}, {}) VALUES ($1, $2, $3, $4, $5)",
        relation(fixture.row),
        field(fixture.row_child),
        field(fixture.row_active),
        field(fixture.row_value),
        field(fixture.row_label),
    );
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        for (object, active) in [
            (fixture.true_grandchild, Some(true)),
            (fixture.false_grandchild, Some(false)),
            (fixture.null_grandchild, None),
        ] {
            session
                .client()
                .execute(
                    &grandchild_statement,
                    &[&object.to_bytes().to_vec(), &active],
                )
                .await?;
        }
        for (object, active, grandchild) in [
            (
                fixture.true_child,
                Some(true),
                Some(fixture.true_grandchild),
            ),
            (
                fixture.false_child,
                Some(false),
                Some(fixture.false_grandchild),
            ),
            (fixture.null_child, None, Some(fixture.null_grandchild)),
        ] {
            session
                .client()
                .execute(
                    &child_statement,
                    &[
                        &object.to_bytes().to_vec(),
                        &active,
                        &grandchild.map(|object| object.to_bytes().to_vec()),
                    ],
                )
                .await?;
        }
        for (object, child, active, value, label) in [
            (
                ObjectId::from_bytes([0x91; 16]),
                Some(fixture.true_child),
                Some(true),
                10,
                "duplicate",
            ),
            (
                ObjectId::from_bytes([0x92; 16]),
                Some(fixture.true_child),
                Some(true),
                10,
                "duplicate",
            ),
            (
                ObjectId::from_bytes([0x93; 16]),
                Some(fixture.true_child),
                Some(false),
                20,
                "root false",
            ),
            (
                ObjectId::from_bytes([0x94; 16]),
                Some(fixture.true_child),
                None,
                30,
                "root null",
            ),
            (
                ObjectId::from_bytes([0x95; 16]),
                None,
                Some(true),
                40,
                "missing child",
            ),
            (
                ObjectId::from_bytes([0x96; 16]),
                Some(fixture.false_child),
                Some(true),
                50,
                "child false",
            ),
            (
                ObjectId::from_bytes([0x97; 16]),
                Some(fixture.null_child),
                Some(true),
                60,
                "child null",
            ),
        ] {
            session
                .client()
                .execute(
                    &row_statement,
                    &[
                        &object.to_bytes().to_vec(),
                        &child.map(|object| object.to_bytes().to_vec()),
                        &active,
                        &value,
                        &label,
                    ],
                )
                .await?;
        }
        Ok(())
    }
    .await;
    finish_session(
        session,
        operation,
        "direct Boolean predicate fixture insert",
    )
    .await
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

async fn install_direct_boolean_predicate_decoy(
    database: &TestDatabase,
    fixture: DirectBooleanPredicateFixture,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation: TestResult<()> = async {
        session
            .client()
            .batch_execute(&format!(
                "CREATE TABLE public.t_{:032x} \
                 (_orna_object_id bytea, {} bytea, {} boolean, {} integer, {} text);",
                u128::from_be_bytes(fixture.row.to_bytes()),
                field(fixture.row_child),
                field(fixture.row_active),
                field(fixture.row_value),
                field(fixture.row_label),
            ))
            .await?;
        session
            .client()
            .execute(
                &format!(
                    "INSERT INTO public.t_{:032x} (_orna_object_id, {}, {}, {}, {}) \
                     VALUES ($1, $2, $3, $4, $5)",
                    u128::from_be_bytes(fixture.row.to_bytes()),
                    field(fixture.row_child),
                    field(fixture.row_active),
                    field(fixture.row_value),
                    field(fixture.row_label),
                ),
                &[
                    &vec![0xa1_u8; 16],
                    &Some(fixture.true_child.to_bytes().to_vec()),
                    &true,
                    &-1_i32,
                    &"hostile public row",
                ],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_session(session, operation, "direct Boolean public execution decoy").await
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

fn standard_execution_candidate(
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
            "standard application diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
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

fn require_standard_execution_value_identities(
    active: &ActiveDatabaseRevision,
    fixture: Fixture,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<()> {
    let expected_standard = upgrade.verified_standard_snapshot();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("installed-standard execution active context is absent"))?;
    require(
        standard.revision() == expected_standard.revision()
            && standard.catalogue().revision() == expected_standard.catalogue().revision()
            && standard.digest() == expected_standard.digest(),
        "installed-standard execution active context differs from the opaque upgrade snapshot",
    )?;
    let node = active
        .catalogue()
        .object_type_by_id(fixture.node)
        .ok_or_else(|| failure("installed-standard execution node type is absent"))?;
    let values = [
        ("active", fixture.active, BOOLEAN_TYPE_ID),
        ("value", fixture.value, INTEGER_TYPE_ID),
        ("amount", fixture.amount, BIGINT_TYPE_ID),
        ("score", fixture.score, FLOAT_TYPE_ID),
        ("label", fixture.label, CHARACTER_LARGE_OBJECT_TYPE_ID),
        ("blob", fixture.blob, BINARY_LARGE_OBJECT_TYPE_ID),
    ];
    for (name, field_id, value_type) in values {
        let field = node.field_by_id(field_id).ok_or_else(|| {
            failure(format!(
                "installed-standard execution field {name} is absent"
            ))
        })?;
        require(
            standard.catalogue().value_type_by_id(value_type).is_some()
                && field.resolved_type() == ResolvedType::value(value_type),
            format!("installed-standard execution field {name} lost its exact Value identity"),
        )?;
    }
    let child = node
        .field_by_id(fixture.child)
        .ok_or_else(|| failure("installed-standard execution child reference field is absent"))?;
    require(
        child.resolved_type() == ResolvedType::reference(fixture.node),
        "installed-standard execution child field changed its exact REF target",
    )?;

    let function = active
        .catalogue()
        .function_by_id(fixture.read)
        .ok_or_else(|| failure("installed-standard execution read function is absent"))?;
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(failure(
            "installed-standard execution read function did not retain ROWS return columns",
        ));
    };
    let expected = [
        ("root", ResolvedType::reference(fixture.node)),
        ("active", ResolvedType::value(BOOLEAN_TYPE_ID)),
        ("value", ResolvedType::value(INTEGER_TYPE_ID)),
        ("amount", ResolvedType::value(BIGINT_TYPE_ID)),
        ("score", ResolvedType::value(FLOAT_TYPE_ID)),
        ("label", ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID)),
        ("blob", ResolvedType::value(BINARY_LARGE_OBJECT_TYPE_ID)),
        (
            "child_label",
            ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
        ),
    ];
    require(
        columns.len() == expected.len(),
        "installed-standard execution read function changed its ROWS column count",
    )?;
    for (column, (name, resolved_type)) in columns.iter().zip(expected) {
        require(
            column.name() == name && column.resolved_type() == resolved_type,
            format!(
                "installed-standard execution ROWS column {name} lost its exact Value identity"
            ),
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

fn require_direct_boolean_predicate_columns(result: &ServerSelectResult) -> TestResult<()> {
    let expected = [
        (
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        ),
        (
            "label",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        ),
    ];
    require(
        result.rows().columns().len() == expected.len(),
        "direct Boolean predicate result has the wrong column count",
    )?;
    for (column, (name, resolved_type, nullable)) in result.rows().columns().iter().zip(expected) {
        require(
            column.name() == name,
            format!("direct Boolean predicate column name is not {name}"),
        )?;
        require(
            column.resolved_type() == resolved_type,
            format!("direct Boolean predicate column {name} has the wrong type"),
        )?;
        require(
            column.nullable() == nullable,
            format!("direct Boolean predicate column {name} has the wrong nullability"),
        )?;
    }
    Ok(())
}

fn require_direct_boolean_predicate_rows(
    result: &ServerSelectResult,
    expected: &[(i32, &'static str)],
    name: &'static str,
) -> TestResult<()> {
    require(
        result.rows().rows().len() == expected.len(),
        format!("{name} returned the wrong row count"),
    )?;
    for (index, (actual, (value, label))) in result.rows().rows().iter().zip(expected).enumerate() {
        require(
            actual.values()
                == [
                    RuntimeValue::Integer(*value),
                    RuntimeValue::Text((*label).to_owned()),
                ],
            format!("{name} row {index} differs from the exact ordered typed row"),
        )?;
    }
    Ok(())
}

fn require_direct_boolean_distinct_columns(result: &ServerSelectResult) -> TestResult<()> {
    let columns = result.rows().columns();
    require(
        columns.len() == 1,
        "direct Boolean DISTINCT predicate must not become a result column",
    )?;
    let column = &columns[0];
    require(
        column.name() == "value",
        "direct Boolean DISTINCT result column is not value",
    )?;
    require(
        column.resolved_type() == ResolvedType::scalar(StandardScalar::Integer),
        "direct Boolean DISTINCT result column has the wrong type",
    )?;
    require(
        !column.nullable(),
        "direct Boolean DISTINCT result column has the wrong nullability",
    )
}

fn direct_boolean_distinct_values(values: &[i32]) -> Vec<Vec<RuntimeValue>> {
    values
        .iter()
        .copied()
        .map(|value| vec![RuntimeValue::Integer(value)])
        .collect()
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

fn require_enum_value_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "enum error is not contextual SERVER SELECT execution",
        ));
    };
    require(context.pair() == pair, "enum error context pair differs")?;
    require(
        context.function() == function,
        "enum error context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "enum error context revision differs",
    )?;
    match source.as_ref() {
        ServerSelectError::ValueInvariant { row, column, rule } => require(
            *row == 0
                && *column == 0
                && *rule
                    == "enum result must contain one exact label declared by the active enum type",
            "enum value invariant evidence differs",
        ),
        _ => Err(failure("enum execution source is not ValueInvariant")),
    }
}

fn require_record_codec_error(
    error: &PostgresKernelError,
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
) -> TestResult<()> {
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution { context, source }) = error
    else {
        return Err(failure(
            "record codec error is not contextual SERVER SELECT execution",
        ));
    };
    require(context.pair() == pair, "record codec context pair differs")?;
    require(
        context.function() == function,
        "record codec context function differs",
    )?;
    require(
        context.function_revision() == revision,
        "record codec context revision differs",
    )?;
    match source.as_ref() {
        ServerSelectError::ValueCodec {
            row,
            column,
            source: ValueCodecError::TruncatedHeader { actual },
        } => require(
            *row == 0 && *column == 0 && *actual == 4,
            "record codec location or truncation evidence differs",
        ),
        _ => Err(failure("record execution source is not ValueCodec")),
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
