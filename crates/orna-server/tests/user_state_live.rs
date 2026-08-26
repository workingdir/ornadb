//! Live USER state service proof (ADR 0061 step 6).
//!
//! This suite drives the exact installed host flow through the
//! `run_user_state_with_kernel` seam against the Compose PostgreSQL
//! development service. The seam authenticates the invoking process's
//! effective UID through `authenticate_local_peer(geteuid())` exactly as the
//! installed product does, so the security snapshot maps that UID to each
//! principal under test via `LocalPeerCredential`.
//!
//! What is proved: a principal creates a cell, loads it back, increments its
//! revision, hits ORNA0902 on a stale expected revision, is isolated from a
//! remapped second principal, fails closed with ORNA0901 on a load-time type
//! mismatch, and the cells survive a fresh kernel reopen.

#![cfg(unix)]
#![allow(dead_code)]

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use std::collections::BTreeMap;

use orna_artifact::client_plan::{StateClientPlan, StateScope};
use orna_client::{ClientStateContext, ClientStateKey, ClientStateStore, ClientUserStateError};
use orna_compiler::{
    StandardApplicationCheckContext, check_standard_application, prepare_standard_application,
};
use orna_core::{
    FunctionId, PrincipalId, StateSlotId, TypeId,
    catalogue::FunctionDomain,
    security::{LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot},
    source::{SourceBundle, SourceUnit},
    state::{UserStateChange, UserStateWriteOutcome},
    value::RuntimeValue,
};
use orna_postgres::PostgresKernel;
use orna_protocol::encode_constructed_value;
use orna_server::{
    AuthenticatedClientStateAdapter, InstalledUserStateChange, InstalledUserStateError,
    InstalledUserStateExpectedType, InstalledUserStateInstance, InstalledUserStateOperation,
    InstalledUserStateOutcome, InstalledUserStateRequest, run_user_state_with_kernel,
};
use orna_standard::{INTEGER_TYPE_ID, registered_opaque_codecs};
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

#[cfg(feature = "test-hooks")]
use orna_core::security::{ExecuteDenial, RoleMembership, SecurityAuditKind, SecurityAuditOutcome};
#[cfg(feature = "test-hooks")]
use orna_postgres::PostgresKernelError;

/// Asserts one live condition, failing the whole test with a typed error.
fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

const PRINCIPAL_A: PrincipalId = PrincipalId::from_bytes([0xaa; 16]);
const PRINCIPAL_B: PrincipalId = PrincipalId::from_bytes([0xbb; 16]);
const UNKNOWN_ROOT: FunctionId = FunctionId::from_bytes([0xe1; 16]);
const INACTIVE_ROOT: FunctionId = FunctionId::from_bytes([0xe2; 16]);
const RAW_USER_STATE_SOURCE: &str = "CREATE SCHEMA user_state_fixture;\n\
    CREATE TYPE user_state_fixture.server_row AS OBJECT (value INTEGER NOT NULL);\n\
    CREATE CLIENT FUNCTION user_state_fixture.state() RETURNS BOOLEAN IS\n\
      STATE value INTEGER SCOPE USER DEFAULT 0;\n\
    BEGIN\n\
      RETURN TRUE;\n\
    END;\n\
    CREATE SERVER FUNCTION user_state_fixture.server()\n\
    RETURNS ROWS (value INTEGER)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT server_row.value FROM user_state_fixture.server_row server_row;\n";

fn kernel(database: &TestDatabase) -> PostgresKernel {
    database.connection_string().parse().expect("kernel URL")
}

/// Encodes one integer to the exact canonical ORV5 hex the renderer emits.
async fn integer_hex(database: &TestDatabase, value: i32) -> TestResult<String> {
    let active = kernel(database).recover().await?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("the standard snapshot is pinned by the V1-to-V2 upgrade"))?;
    let registry = registered_opaque_codecs(standard)?;
    let encoded = encode_constructed_value(&active, &registry, &RuntimeValue::Integer(value))?;
    let mut hex = String::with_capacity(encoded.len() * 2);
    for byte in encoded {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").map_err(|_| failure("hex formatting failed"))?;
    }
    Ok(hex)
}

/// Parses the `"value_hex"` field from one loaded-cell JSON record line.
///
/// The renderer emits exactly one JSON object per line, so the first line's
/// `value_hex` is authoritative. A malformed record fails the assertion.
fn loaded_value_hex(record: &str) -> Result<String, String> {
    let line = record.lines().next().ok_or("no record line")?;
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| format!("record is not JSON: {error}"))?;
    value
        .get("value_hex")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "record has no value_hex field".to_owned())
}

async fn install_standard(database: &TestDatabase) -> TestResult<(FunctionId, StateSlotId)> {
    let kernel = kernel(database);
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
    kernel.apply_standard_upgrade(&upgrade).await?;

    let active = kernel.recover().await?;
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new("user-state.orna", RAW_USER_STATE_SOURCE)])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        format!(
            "USER state fixture did not compile: {:?}",
            report.diagnostics()
        ),
    )?;
    let active = kernel
        .apply(&prepare_standard_application(
            &report,
            active.pair(),
            &active,
        )?)
        .await?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["user_state_fixture", "state"])
        .ok_or_else(|| failure("USER state fixture is missing its CLIENT function"))?
        .id();
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == function)
        .ok_or_else(|| failure("USER state fixture is missing its CLIENT revision"))?;
    let plan = StateClientPlan::decode(revision.artifact().payload())
        .map_err(|error| failure(format!("USER state fixture plan did not decode: {error}")))?;
    let slot = plan
        .slots()
        .iter()
        .find(|slot| slot.scope() == StateScope::User)
        .ok_or_else(|| failure("USER state fixture is missing its USER state slot"))?
        .state_slot_id();
    Ok((function, slot))
}

async fn map_peer(database: &TestDatabase, principal: PrincipalId) -> TestResult<()> {
    let kernel = kernel(database);
    let active = kernel.recover().await?;
    let pair = active.pair();

    let mut targets = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| orna_core::security::SecurityFunctionTarget::application(function.id()))
        .collect::<Vec<_>>();
    if let Some(standard) = active.catalogue_hash_context().standard() {
        for executable in standard.executables() {
            targets.push(
                orna_core::security::SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.revision(),
                    executable.revision().id(),
                ),
            );
        }
    }

    let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
        pair,
        targets,
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
        vec![LocalPeerCredential::new(
            nix::unistd::geteuid().as_raw(),
            principal,
        )],
    )?;
    kernel.replace_security_snapshot(&security).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_cell(
    database: &TestDatabase,
    root: FunctionId,
    function: FunctionId,
    profile: &str,
    instance_key: &str,
    slot: StateSlotId,
    expected_revision: Option<u64>,
    value: i32,
) -> TestResult<(
    Result<InstalledUserStateOutcome, InstalledUserStateError>,
    Vec<u8>,
)> {
    let active = kernel(database).recover().await?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("the standard snapshot is pinned by the V1-to-V2 upgrade"))?;
    let registry = registered_opaque_codecs(standard)?;
    let value_bytes = encode_constructed_value(&active, &registry, &RuntimeValue::Integer(value))?;
    let request = InstalledUserStateRequest::new(InstalledUserStateOperation::Write {
        root_function: root,
        state_profile: profile.to_owned(),
        change: InstalledUserStateChange {
            function,
            instance_key: instance_key.to_owned(),
            state_slot: slot,
            expected_revision,
            value_type: INTEGER_TYPE_ID,
            value_bytes,
        },
    });
    let mut stdout = Vec::new();
    let outcome = run_user_state_with_kernel(kernel(database).clone(), request, &mut stdout).await;
    Ok((outcome, stdout))
}

async fn load_cells(
    database: &TestDatabase,
    root: FunctionId,
    function: FunctionId,
    profile: &str,
) -> TestResult<(InstalledUserStateOutcome, Vec<u8>)> {
    let (outcome, stdout) =
        load_cells_with_types(database, root, function, profile, Vec::new()).await?;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(failure(format!("the default load failed closed: {error}")));
        }
    };
    Ok((outcome, stdout))
}

async fn load_cells_with_types(
    database: &TestDatabase,
    root: FunctionId,
    function: FunctionId,
    profile: &str,
    expected_types: Vec<InstalledUserStateExpectedType>,
) -> TestResult<(
    Result<InstalledUserStateOutcome, InstalledUserStateError>,
    Vec<u8>,
)> {
    let request = InstalledUserStateRequest::new(InstalledUserStateOperation::Load {
        root_function: root,
        state_profile: profile.to_owned(),
        instances: vec![InstalledUserStateInstance {
            function,
            instance_key: String::new(),
        }],
        expected_types,
    });
    let mut stdout = Vec::new();
    let outcome = run_user_state_with_kernel(kernel(database).clone(), request, &mut stdout).await;
    Ok((outcome, stdout))
}
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_user_state_end_to_end() -> TestResult<()> {
    with_test_database(|database| async move {
        let (state_function, state_slot) = install_standard(&database).await?;

        // Principal A creates a cell under the default profile.
        map_peer(&database, PRINCIPAL_A).await?;
        let (create, _) = write_cell(
            &database,
            state_function,
            state_function,
            "",
            "",
            state_slot,
            None,
            41,
        )
        .await?;
        require(
            create == Ok(InstalledUserStateOutcome::Completed),
            "the principal A first write must complete",
        )?;

        // Load it back: one record carrying the exact value and revision 1.
        let (load, load_stdout) = load_cells(&database, state_function, state_function, "").await?;
        require(
            load == InstalledUserStateOutcome::Completed,
            "the principal A load must complete",
        )?;
        let load_text = String::from_utf8(load_stdout)
            .map_err(|_| failure("the load stdout was not UTF-8 text"))?;
        let loaded_41 = loaded_value_hex(&load_text).map_err(|message| failure(&message))?;
        let exact_41 = integer_hex(&database, 41).await?;
        require(
            loaded_41 == exact_41,
            "the loaded cell must carry the exact 41 encoding, got {loaded_41}",
        )?;
        require(
            load_text.contains("\"revision\":1"),
            "the loaded cell must carry revision 1, got: {load_text}",
        )?;

        // A matching expected revision increments to 2.
        let (write_two, _) = write_cell(
            &database,
            state_function,
            state_function,
            "",
            "",
            state_slot,
            Some(1),
            42,
        )
        .await?;
        require(
            write_two == Ok(InstalledUserStateOutcome::Completed),
            "the matching-revision write must complete",
        )?;
        let (load_two, load_two_stdout) =
            load_cells(&database, state_function, state_function, "").await?;
        require(
            load_two == InstalledUserStateOutcome::Completed,
            "the second load must complete",
        )?;
        let load_two_text = String::from_utf8(load_two_stdout)
            .map_err(|_| failure("the second load stdout was not UTF-8 text"))?;
        let loaded_42 = loaded_value_hex(&load_two_text).map_err(|message| failure(&message))?;
        let exact_42 = integer_hex(&database, 42).await?;
        require(
            loaded_42 == exact_42,
            "the loaded cell must carry the exact 42 encoding, got: {loaded_42}",
        )?;
        require(
            load_two_text.contains("\"revision\":2"),
            "the loaded cell must carry revision 2, got: {load_two_text}",
        )?;

        // A stale expected revision yields a per-change conflict outcome; the
        // request still completes and renders the ORNA0902 conflict record.
        let (stale, stale_stdout) = write_cell(
            &database,
            state_function,
            state_function,
            "",
            "",
            state_slot,
            Some(1),
            99,
        )
        .await?;
        require(
            stale == Ok(InstalledUserStateOutcome::Completed),
            "a stale write must complete with a conflict record",
        )?;
        let stale_text = String::from_utf8(stale_stdout)
            .map_err(|_| failure("the stale write stdout was not UTF-8 text"))?;
        require(
            stale_text.contains("\"outcome\":\"conflict\"")
                && stale_text.contains("\"current_revision\":2"),
            "the stale write must render the ORNA0902 conflict record, got: {stale_text}",
        )?;

        // The conflict applied nothing: the cell still carries exact 42 at 2.
        let (after_conflict, after_conflict_stdout) =
            load_cells(&database, state_function, state_function, "").await?;
        require(
            after_conflict == InstalledUserStateOutcome::Completed,
            "the post-conflict load must complete",
        )?;
        let after_conflict_text = String::from_utf8(after_conflict_stdout)
            .map_err(|_| failure("the post-conflict stdout was not UTF-8 text"))?;
        let after_conflict_42 =
            loaded_value_hex(&after_conflict_text).map_err(|message| failure(&message))?;
        require(
            after_conflict_42 == exact_42,
            "the conflict must not change the stored value, got: {after_conflict_42}",
        )?;
        require(
            after_conflict_text.contains("\"revision\":2"),
            "the post-conflict load must keep revision 2, got: {after_conflict_text}",
        )?;

        // A load-time type mismatch fails closed with ORNA0901.
        let (type_mismatch, _) = load_cells_with_types(
            &database,
            state_function,
            state_function,
            "",
            vec![InstalledUserStateExpectedType {
                function: state_function,
                state_slot: state_slot,
                value_type: TypeId::from_bytes([0x99; 16]),
            }],
        )
        .await?;
        let type_error = match type_mismatch {
            Err(error) => error,
            Ok(_) => return Err(failure("a load-time type mismatch must fail closed")),
        };
        let type_text = type_error.to_string();
        require(
            type_text.contains("ORNA0901"),
            "the type mismatch must carry ORNA0901, got: {type_text}",
        )?;

        // Remap the local peer to principal B; its load sees no cells.
        map_peer(&database, PRINCIPAL_B).await?;
        let (isolation, isolation_stdout) =
            load_cells(&database, state_function, state_function, "").await?;
        require(
            isolation == InstalledUserStateOutcome::Completed,
            "the isolated load must complete",
        )?;
        require(
            isolation_stdout.is_empty(),
            "principal B must not see principal A's cells",
        )?;

        // A fresh kernel reopen preserves the cells for principal A.
        map_peer(&database, PRINCIPAL_A).await?;
        let (reopen, reopen_stdout) =
            load_cells(&database, state_function, state_function, "").await?;
        require(
            reopen == InstalledUserStateOutcome::Completed,
            "the reopen load must complete",
        )?;
        let reopen_text = String::from_utf8(reopen_stdout)
            .map_err(|_| failure("the reopen load stdout was not UTF-8 text"))?;
        let reopened_42 = loaded_value_hex(&reopen_text).map_err(|message| failure(&message))?;
        require(
            reopened_42 == exact_42,
            "reopen must preserve the exact 42 encoding, got: {reopened_42}",
        )?;
        require(
            reopen_text.contains("\"revision\":2"),
            "reopen must preserve revision 2, got: {reopen_text}",
        )?;

        Ok(())
    })
    .await
}

/// Invalid USER-state roots fail before the state relation or allowed audit append.
///
/// This proof is intentionally ignored with the other Compose-backed USER-state
/// proofs; it records the live mutation boundary without claiming local evidence.
#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_non_client_roots_before_user_state_or_allowed_audit() -> TestResult<()> {
    with_test_database(|database| async move {
        let (state_function, state_slot) = install_standard(&database).await?;
        map_peer(&database, PRINCIPAL_A).await?;
        let kernel = kernel(&database);
        let active = kernel.recover().await?;
        let server_root = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.domain() == FunctionDomain::Server)
            .map(|function| function.id())
            .ok_or_else(|| failure("the USER-state fixture needs an active SERVER function"))?;
        let inactive_root = active
            .historical_function_revisions()
            .iter()
            .map(|revision| revision.function())
            .find(|function| active.catalogue().function_by_id(*function).is_none())
            .unwrap_or(INACTIVE_ROOT);
        let session = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let seed = UserStateChange::new(
            state_function,
            String::new(),
            state_function,
            String::new(),
            state_slot,
            None,
            RuntimeValue::Integer(7),
            INTEGER_TYPE_ID,
        )?;
        let seeded = kernel.write_user_state(&session, &[seed]).await?;
        require(
            seeded.first().is_some_and(|result| {
                matches!(
                    result.outcome(),
                    UserStateWriteOutcome::Written { revision: 1 }
                )
            }),
            "the root-admission fixture write must create revision 1",
        )?;
        let baseline_allowed =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);

        for (label, root, expected_rule) in [
            (
                "unknown",
                UNKNOWN_ROOT,
                "USER state root must identify an active CLIENT function",
            ),
            (
                "SERVER",
                server_root,
                "USER state root must be a CLIENT function",
            ),
            (
                "inactive",
                inactive_root,
                "USER state root must identify an active CLIENT function",
            ),
        ] {
            let load = kernel
                .load_user_state(&session, root, "", &[], &BTreeMap::new())
                .await;
            require(
                matches!(
                    load,
                    Err(PostgresKernelError::DurableInvariant { rule, .. })
                        if rule == expected_rule
                ),
                format!("{label} USER-state root load must fail closed before state access"),
            )?;
            let change = UserStateChange::new(
                root,
                String::new(),
                state_function,
                String::new(),
                state_slot,
                Some(1),
                RuntimeValue::Integer(8),
                INTEGER_TYPE_ID,
            )?;
            let write = kernel.write_user_state(&session, &[change]).await;
            require(
                matches!(
                    write,
                    Err(PostgresKernelError::DurableInvariant { rule, .. })
                        if rule == expected_rule
                ),
                format!("{label} USER-state root write must fail closed before mutation"),
            )?;
        }

        let after_rejections =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);
        require(
            after_rejections == baseline_allowed,
            "invalid USER-state roots must not append allowed audit evidence",
        )?;
        let cells = kernel
            .load_user_state(&session, state_function, "", &[], &BTreeMap::new())
            .await?;
        require(
            cells.len() == 1
                && cells[0].value() == &RuntimeValue::Integer(7)
                && cells[0].revision() == 1,
            "invalid USER-state roots must not alter the existing cell",
        )
    })
    .await
}

/// Proves the authenticated CLIENT state adapter lifecycle (ADR 0070).
///
/// The adapter uses the authenticated kernel session for principal selection,
/// loads typed USER values into the caller-owned store, flushes one explicit
/// update, reloads it, and reports a stale revision without replacing the
/// local value.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_authenticated_client_state_adapter_lifecycle() -> TestResult<()> {
    with_test_database(|database| async move {
        let (state_function, state_slot) = install_standard(&database).await?;
        map_peer(&database, PRINCIPAL_A).await?;

        let kernel = kernel(&database);
        let session = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let context =
            ClientStateContext::new(state_function, "adapter-profile".to_owned(), String::new())
                .map_err(|error| failure(error.to_string()))?;
        let expected_types = BTreeMap::from([((state_function, state_slot), INTEGER_TYPE_ID)]);
        let initial_change = UserStateChange::new(
            state_function,
            "adapter-profile".to_owned(),
            state_function,
            String::new(),
            state_slot,
            None,
            RuntimeValue::Integer(7),
            INTEGER_TYPE_ID,
        )?;
        let initial_results = kernel.write_user_state(&session, &[initial_change]).await?;
        require(
            matches!(
                initial_results.first().map(|result| result.outcome()),
                Some(UserStateWriteOutcome::Written { revision: 1 })
            ),
            "the adapter fixture write must create revision 1",
        )?;

        let adapter = AuthenticatedClientStateAdapter::new(&kernel, &session);
        let mut state = ClientStateStore::new();
        adapter
            .load(&context, &[], &expected_types, &mut state)
            .await
            .map_err(|error| failure(error.to_string()))?;
        let key = ClientStateKey::from_context(&context, state_function, state_slot);
        require(
            state.user().get(&key).is_some_and(|value| {
                value.value() == &RuntimeValue::Integer(7)
                    && value.revision() == Some(1)
                    && !value.is_dirty()
            }),
            "the adapter load must restore the typed value and revision",
        )?;
        require(
            state.pending_user_state_changes()?.is_empty(),
            "a loaded USER value must not be dirty",
        )?;

        state.set_user_state(key.clone(), RuntimeValue::Integer(8), INTEGER_TYPE_ID)?;
        adapter
            .flush(&mut state)
            .await
            .map_err(|error| failure(error.to_string()))?;
        require(
            state.user().get(&key).is_some_and(|value| {
                value.value() == &RuntimeValue::Integer(8)
                    && value.revision() == Some(2)
                    && !value.is_dirty()
            }),
            "the adapter flush must acknowledge revision 2",
        )?;

        let mut reloaded = ClientStateStore::new();
        adapter
            .load(&context, &[], &expected_types, &mut reloaded)
            .await
            .map_err(|error| failure(error.to_string()))?;
        require(
            reloaded.user().get(&key).is_some_and(|value| {
                value.value() == &RuntimeValue::Integer(8) && value.revision() == Some(2)
            }),
            "the adapter reload must return the flushed value",
        )?;

        reloaded.set_user_state(key.clone(), RuntimeValue::Integer(9), INTEGER_TYPE_ID)?;
        let external_change = UserStateChange::new(
            state_function,
            "adapter-profile".to_owned(),
            state_function,
            String::new(),
            state_slot,
            Some(2),
            RuntimeValue::Integer(10),
            INTEGER_TYPE_ID,
        )?;
        let external_results = kernel
            .write_user_state(&session, &[external_change])
            .await?;
        require(
            matches!(
                external_results.first().map(|result| result.outcome()),
                Some(UserStateWriteOutcome::Written { revision: 3 })
            ),
            "the external write must advance the server revision to 3",
        )?;

        let conflict = adapter.flush(&mut reloaded).await;
        require(
            matches!(
                conflict,
                Err(orna_server::AuthenticatedClientStateError::Client(
                    ClientUserStateError::Conflict { current: 3, .. }
                ))
            ),
            "the adapter must report the stale revision conflict",
        )?;
        require(
            reloaded.user().get(&key).is_some_and(|value| {
                value.value() == &RuntimeValue::Integer(9)
                    && value.revision() == Some(2)
                    && value.is_dirty()
            }),
            "a conflict must preserve the dirty local value",
        )
    })
    .await
}

/// A caller-owned USER store remains bound to its original authenticated session when a
/// different authenticated session is presented to the adapter. Neither a
/// flush nor a load may silently rebind (and thereby discard) that state.
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_authenticated_client_state_session_mismatch() -> TestResult<()> {
    with_test_database(|database| async move {
        let (state_function, state_slot) = install_standard(&database).await?;
        map_peer(&database, PRINCIPAL_A).await?;

        let kernel = kernel(&database);
        let session_a = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let context =
            ClientStateContext::new(state_function, "adapter-profile".to_owned(), String::new())
                .map_err(|error| failure(error.to_string()))?;
        let expected_types = BTreeMap::from([((state_function, state_slot), INTEGER_TYPE_ID)]);
        let initial_change = UserStateChange::new(
            state_function,
            "adapter-profile".to_owned(),
            state_function,
            String::new(),
            state_slot,
            None,
            RuntimeValue::Integer(7),
            INTEGER_TYPE_ID,
        )?;
        kernel
            .write_user_state(&session_a, &[initial_change])
            .await?;

        let adapter_a = AuthenticatedClientStateAdapter::new(&kernel, &session_a);
        let mut state = ClientStateStore::new();
        adapter_a
            .load(&context, &[], &expected_types, &mut state)
            .await
            .map_err(|error| failure(error.to_string()))?;
        let key = ClientStateKey::from_context(&context, state_function, state_slot);
        state.set_user_state(key.clone(), RuntimeValue::Integer(8), INTEGER_TYPE_ID)?;
        let before_mismatch = state.clone();

        map_peer(&database, PRINCIPAL_B).await?;
        let session_b = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let principal_b_change = UserStateChange::new(
            state_function,
            "adapter-profile".to_owned(),
            state_function,
            String::new(),
            state_slot,
            None,
            RuntimeValue::Integer(70),
            INTEGER_TYPE_ID,
        )?;
        kernel
            .write_user_state(&session_b, &[principal_b_change])
            .await?;
        let adapter_b = AuthenticatedClientStateAdapter::new(&kernel, &session_b);

        let flush = adapter_b.flush(&mut state).await;
        require(
            matches!(
                flush,
                Err(orna_server::AuthenticatedClientStateError::Client(
                    ClientUserStateError::SessionMismatch
                ))
            ),
            "flushing through another authenticated session must return a typed mismatch",
        )?;
        require(
            state == before_mismatch,
            "a rejected flush must preserve the original session's dirty state",
        )?;
        // Rebind the local peer for each durable read so both principals are
        // checked through a currently authenticated session.
        map_peer(&database, PRINCIPAL_A).await?;
        let durable_session_a = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let principal_a_cells = kernel
            .load_user_state(
                &durable_session_a,
                context.root_function(),
                context.state_profile(),
                &[],
                &expected_types,
            )
            .await?;
        require(
            principal_a_cells.len() == 1
                && principal_a_cells[0].value() == &RuntimeValue::Integer(7)
                && principal_a_cells[0].revision() == 1,
            "a rejected flush must preserve principal A's existing cell",
        )?;
        map_peer(&database, PRINCIPAL_B).await?;
        let durable_session_b = kernel
            .authenticate_local_peer(nix::unistd::geteuid().as_raw())
            .await?;
        let principal_b_cells = kernel
            .load_user_state(
                &durable_session_b,
                context.root_function(),
                context.state_profile(),
                &[],
                &expected_types,
            )
            .await?;
        require(
            principal_b_cells.len() == 1
                && principal_b_cells[0].value() == &RuntimeValue::Integer(70)
                && principal_b_cells[0].revision() == 1,
            "a rejected flush must not alter principal B's existing cell",
        )?;

        let load = adapter_b
            .load(&context, &[], &expected_types, &mut state)
            .await;
        require(
            matches!(
                load,
                Err(orna_server::AuthenticatedClientStateError::Client(
                    ClientUserStateError::SessionMismatch
                ))
            ),
            "loading through another authenticated session must return a typed mismatch",
        )?;
        require(
            state == before_mismatch,
            "a rejected load must preserve the original session's dirty state",
        )?;
        Ok(())
    })
    .await
}

#[cfg(feature = "test-hooks")]
fn state_security_snapshot(
    active: &orna_core::revision::ActiveDatabaseRevision,
    principals: Vec<Principal>,
    memberships: Vec<RoleMembership>,
    peer_principal: PrincipalId,
) -> TestResult<SecuritySnapshot> {
    let mut targets = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| orna_core::security::SecurityFunctionTarget::application(function.id()))
        .collect::<Vec<_>>();
    if let Some(standard) = active.catalogue_hash_context().standard() {
        for executable in standard.executables() {
            targets.push(
                orna_core::security::SecurityFunctionTarget::verified_standard(
                    executable.function(),
                    standard.revision(),
                    executable.revision().id(),
                ),
            );
        }
    }
    Ok(
        SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            targets,
            principals,
            memberships,
            vec![],
            vec![LocalPeerCredential::new(
                nix::unistd::geteuid().as_raw(),
                peer_principal,
            )],
        )?,
    )
}

#[cfg(feature = "test-hooks")]
fn user_state_allowed_audit_count(events: &[orna_core::security::SecurityAuditEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            event.decision().kind() == SecurityAuditKind::UserState
                && event.decision().outcome() == SecurityAuditOutcome::Allowed
        })
        .count()
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn retained_user_state_session_is_denied_after_principal_disable_and_role_revoke()
-> TestResult<()> {
    with_test_database(|database| async move {
        let (state_function, state_slot) = install_standard(&database).await?;
        let kernel = kernel(&database);
        let active = kernel.recover().await?;
        let role = PrincipalId::from_bytes([0xcc; 16]);
        let initial_security = state_security_snapshot(
            &active,
            vec![
                Principal::new(PRINCIPAL_A, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, PRINCIPAL_A)],
            PRINCIPAL_A,
        )?;
        kernel.replace_security_snapshot(&initial_security).await?;
        let retained_session =
            initial_security.bind_authenticated_session(PRINCIPAL_A, vec![role])?;
        let seed = UserStateChange::new(
            state_function,
            String::new(),
            state_function,
            String::new(),
            state_slot,
            None,
            RuntimeValue::Integer(7),
            INTEGER_TYPE_ID,
        )?;
        let seeded = kernel.write_user_state(&retained_session, &[seed]).await?;
        require(
            seeded.first().is_some_and(|result| {
                matches!(
                    result.outcome(),
                    UserStateWriteOutcome::Written { revision: 1 }
                )
            }),
            "retained-session fixture write must create revision 1",
        )?;
        let baseline_allowed =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);

        let disabled_security = state_security_snapshot(
            &active,
            vec![
                Principal::new(PRINCIPAL_A, PrincipalKind::User, PrincipalStatus::Disabled),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![RoleMembership::new(role, PRINCIPAL_A)],
            PRINCIPAL_A,
        )?;
        kernel.replace_security_snapshot(&disabled_security).await?;
        let load_denied = kernel
            .load_user_state(&retained_session, state_function, "", &[], &BTreeMap::new())
            .await;
        require(
            matches!(
                load_denied,
                Err(PostgresKernelError::StateExecuteDenied {
                    function: orna_core::system::SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "a disabled principal must be denied before USER-state load",
        )?;
        let denied_write = UserStateChange::new(
            state_function,
            String::new(),
            state_function,
            String::new(),
            state_slot,
            Some(1),
            RuntimeValue::Integer(8),
            INTEGER_TYPE_ID,
        )?;
        let write_denied = kernel
            .write_user_state(&retained_session, &[denied_write])
            .await;
        require(
            matches!(
                write_denied,
                Err(PostgresKernelError::StateExecuteDenied {
                    function: orna_core::system::SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "a disabled principal must be denied before USER-state write",
        )?;
        let after_disable_allowed =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);
        require(
            after_disable_allowed == baseline_allowed,
            "disabled-session denial must not append a USER-state allowed audit",
        )?;

        kernel.replace_security_snapshot(&initial_security).await?;
        let valid_session = initial_security.bind_authenticated_session(PRINCIPAL_A, vec![role])?;
        let restored = kernel
            .load_user_state(&valid_session, state_function, "", &[], &BTreeMap::new())
            .await?;
        require(
            restored.len() == 1
                && restored[0].value() == &RuntimeValue::Integer(7)
                && restored[0].revision() == 1,
            "disabled-session denial must preserve the existing USER-state cell",
        )?;
        let before_revoke_allowed =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);

        let revoked_security = state_security_snapshot(
            &active,
            vec![
                Principal::new(PRINCIPAL_A, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            vec![],
            PRINCIPAL_A,
        )?;
        kernel.replace_security_snapshot(&revoked_security).await?;
        let role_load_denied = kernel
            .load_user_state(&retained_session, state_function, "", &[], &BTreeMap::new())
            .await;
        require(
            matches!(
                role_load_denied,
                Err(PostgresKernelError::StateExecuteDenied {
                    function: orna_core::system::SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "a revoked selected role must be denied before USER-state load",
        )?;
        let role_write_denied = kernel
            .write_user_state(
                &retained_session,
                &[UserStateChange::new(
                    state_function,
                    String::new(),
                    state_function,
                    String::new(),
                    state_slot,
                    Some(1),
                    RuntimeValue::Integer(9),
                    INTEGER_TYPE_ID,
                )?],
            )
            .await;
        require(
            matches!(
                role_write_denied,
                Err(PostgresKernelError::StateExecuteDenied {
                    function: orna_core::system::SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
                    reason: ExecuteDenial::InvalidSession,
                    ..
                })
            ),
            "a revoked selected role must be denied before USER-state write",
        )?;
        let after_revoke_allowed =
            user_state_allowed_audit_count(&kernel.recover_security_audit_events().await?);
        require(
            after_revoke_allowed == before_revoke_allowed,
            "role-revocation denial must not append a USER-state allowed audit",
        )?;

        kernel.replace_security_snapshot(&initial_security).await?;
        let final_session = initial_security.bind_authenticated_session(PRINCIPAL_A, vec![role])?;
        let final_cells = kernel
            .load_user_state(&final_session, state_function, "", &[], &BTreeMap::new())
            .await?;
        require(
            final_cells.len() == 1
                && final_cells[0].value() == &RuntimeValue::Integer(7)
                && final_cells[0].revision() == 1,
            "role-revocation denial must preserve the existing USER-state cell",
        )
    })
    .await
}
