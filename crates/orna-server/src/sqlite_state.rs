//! Direct local SQLite adapter for the protected USER state command.

use std::{io::Write, path::PathBuf};

use orna_artifact::client_plan::{
    CAPABILITY_FORMAT_VERSION, CapabilityClientPlan, FORMAT_IDENTITY as CLIENT_PLAN_FORMAT,
    InnerClientPlan, STATE_FORMAT_VERSION, StateClientPlan, StateScope,
};
use orna_core::{
    FunctionId, StateSlotId, TypeId,
    catalogue::FunctionDomain,
    revision::ExecutableArtifactKind,
    state::{UserStateChange, UserStateWriteOutcome},
};
use orna_protocol::{decode_value, encode_value};
use orna_sqlite::{SqliteConfig, SqliteError, SqliteRevisionStore};

use crate::{
    InstalledUserStateError, InstalledUserStateErrorKind, InstalledUserStateExpectedType,
    InstalledUserStateInstance, InstalledUserStateOperation, InstalledUserStateOutcome,
    InstalledUserStateRequest,
};

/// Runs one protected USER-state operation directly against a local SQLite
/// database.
///
/// The local peer is authenticated from the process UID, values remain
/// canonical ORV5 payloads at the command boundary, and writes use the same
/// core conflict/type model as the PostgreSQL service.
pub fn run_sqlite_user_state(
    database_path: impl Into<PathBuf>,
    request: InstalledUserStateRequest,
    stdout: &mut impl Write,
) -> Result<InstalledUserStateOutcome, InstalledUserStateError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| state_error(InstalledUserStateErrorKind::Internal, error.to_string()))?;
    runtime.block_on(run_sqlite_user_state_async(
        database_path.into(),
        request,
        stdout,
    ))
}

async fn run_sqlite_user_state_async(
    database_path: PathBuf,
    request: InstalledUserStateRequest,
    stdout: &mut impl Write,
) -> Result<InstalledUserStateOutcome, InstalledUserStateError> {
    let store = SqliteRevisionStore::open(&SqliteConfig::new(database_path))
        .await
        .map_err(|error| state_sqlite_error(InstalledUserStateErrorKind::Internal, error))?;
    let uid = nix::unistd::geteuid().as_raw();
    store
        .provision_local_peer(uid)
        .await
        .map_err(|error| state_sqlite_error(InstalledUserStateErrorKind::Authentication, error))?;
    let active = store
        .recover()
        .await
        .map_err(|error| state_error(InstalledUserStateErrorKind::Internal, error.to_string()))?;
    let session = store
        .authenticate_local_peer(&active, uid)
        .await
        .map_err(|error| state_sqlite_error(InstalledUserStateErrorKind::Authentication, error))?;
    let root_function = match &request.operation {
        InstalledUserStateOperation::Load { root_function, .. }
        | InstalledUserStateOperation::Write { root_function, .. } => *root_function,
    };
    validate_active_state_plan(&active, root_function)?;

    match request.operation {
        InstalledUserStateOperation::Load {
            root_function,
            state_profile,
            instances,
            expected_types,
        } => {
            validate_state_text(&state_profile)?;
            let instances = plan_instances(&active, &instances)?;
            let expected_types = plan_expected_types(&expected_types);
            for ((function, state_slot), expected) in &expected_types {
                let declared = active_user_state_slot_type(&active, *function, *state_slot)?;
                if declared != *expected {
                    return Err(state_type_mismatch(*expected, declared));
                }
            }
            let cells = store
                .load_user_state(session.principal(), root_function, &state_profile)
                .await
                .map_err(|error| {
                    state_sqlite_error(InstalledUserStateErrorKind::Internal, error)
                })?;
            for cell in cells {
                if !instances.is_empty()
                    && !instances.iter().any(|(function, instance)| {
                        *function == cell.key().function() && instance == cell.key().instance_key()
                    })
                {
                    continue;
                }
                if instances.is_empty() && !cell.key().instance_key().is_empty() {
                    continue;
                }
                let declared = active_user_state_slot_type(
                    &active,
                    cell.key().function(),
                    cell.key().state_slot(),
                )?;
                if declared != cell.value_type() {
                    return Err(state_type_mismatch(declared, cell.value_type()));
                }
                if let Some(expected) =
                    expected_types.get(&(cell.key().function(), cell.key().state_slot()))
                    && *expected != cell.value_type()
                {
                    return Err(state_type_mismatch(*expected, cell.value_type()));
                }
                let value_bytes = encode_value(cell.value()).map_err(|error| {
                    state_error(
                        InstalledUserStateErrorKind::Internal,
                        format!("USER state value encoding failed: {error}"),
                    )
                })?;
                let record = serde_json::json!({
                    "root_function": root_function.canonical(),
                    "state_profile": state_profile,
                    "function": cell.key().function().canonical(),
                    "instance_key": cell.key().instance_key(),
                    "state_slot": cell.key().state_slot().canonical(),
                    "revision": cell.revision(),
                    "value_type": cell.value_type().canonical(),
                    "value_hex": hex_encode(&value_bytes),
                });
                write_json_line(stdout, &record)?;
            }
        }
        InstalledUserStateOperation::Write {
            root_function,
            state_profile,
            change,
        } => {
            validate_state_text(&state_profile)?;
            let declared =
                active_user_state_slot_type(&active, change.function, change.state_slot)?;
            if declared != change.value_type {
                return Err(state_type_mismatch(declared, change.value_type));
            }
            let value = decode_value(&change.value_bytes).map_err(|error| {
                state_error(
                    InstalledUserStateErrorKind::State,
                    format!("USER state value is not valid ORV5: {error}"),
                )
            })?;
            let change = UserStateChange::new(
                root_function,
                state_profile,
                change.function,
                change.instance_key,
                change.state_slot,
                change.expected_revision,
                value,
                change.value_type,
            )
            .map_err(|error| match error.code() {
                Some(code) => InstalledUserStateError::with_code(
                    InstalledUserStateErrorKind::State,
                    error.to_string(),
                    code,
                ),
                None => state_error(InstalledUserStateErrorKind::State, error.to_string()),
            })?;
            let result = store
                .write_user_state(session.principal(), &change)
                .await
                .map_err(|error| state_sqlite_error(InstalledUserStateErrorKind::State, error))?;
            write_json_line(stdout, &write_record(&result))?;
        }
    }
    Ok(InstalledUserStateOutcome::Completed)
}

fn validate_active_state_plan(
    active: &orna_core::revision::ActiveDatabaseRevision,
    root_function: FunctionId,
) -> Result<(), InstalledUserStateError> {
    active_user_state_plan(active, root_function).map(|_| ())
}

fn active_user_state_slot_type(
    active: &orna_core::revision::ActiveDatabaseRevision,
    function: FunctionId,
    state_slot: StateSlotId,
) -> Result<TypeId, InstalledUserStateError> {
    let plan = active_user_state_plan(active, function)?;
    let Some(slot) = plan
        .slots()
        .iter()
        .find(|slot| slot.state_slot_id() == state_slot)
    else {
        return Err(state_schema_error(format!(
            "USER state slot {} is not declared by CLIENT function {}",
            state_slot.canonical(),
            function.canonical()
        )));
    };
    if slot.scope() != StateScope::User {
        return Err(state_schema_error(format!(
            "USER state slot {} is not user-scoped",
            state_slot.canonical()
        )));
    }
    Ok(slot.type_id())
}

fn active_user_state_plan(
    active: &orna_core::revision::ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<StateClientPlan, InstalledUserStateError> {
    let definition = active.catalogue().function_by_id(function).ok_or_else(|| {
        state_schema_error(format!(
            "USER state function {} is not installed",
            function.canonical()
        ))
    })?;
    if definition.domain() != FunctionDomain::Client {
        return Err(state_schema_error(format!(
            "USER state function {} must be a CLIENT function",
            function.canonical()
        )));
    }
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function && revision.id() == definition.current_revision()
        })
        .ok_or_else(|| {
            state_schema_error(format!(
                "USER state function {} has no active CLIENT revision",
                function.canonical()
            ))
        })?;
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client || artifact.format() != CLIENT_PLAN_FORMAT
    {
        return Err(state_schema_error(format!(
            "USER state function {} has no client-plan artifact",
            function.canonical()
        )));
    }
    match artifact.version() {
        STATE_FORMAT_VERSION => StateClientPlan::decode(artifact.payload()).map_err(|error| {
            state_schema_error(format!(
                "USER state function {} has an invalid state plan: {error}",
                function.canonical()
            ))
        }),
        CAPABILITY_FORMAT_VERSION => CapabilityClientPlan::decode(artifact.payload())
            .map_err(|error| {
                state_schema_error(format!(
                    "USER state function {} has an invalid capability plan: {error}",
                    function.canonical()
                ))
            })
            .and_then(|plan| match plan.inner_plan() {
                InnerClientPlan::State(state) => Ok(state.clone()),
                _ => Err(state_schema_error(format!(
                    "USER state function {} does not carry a state plan",
                    function.canonical()
                ))),
            }),
        version => Err(state_schema_error(format!(
            "USER state function {} has unsupported client-plan version {version}",
            function.canonical()
        ))),
    }
}

fn state_schema_error(message: impl Into<String>) -> InstalledUserStateError {
    state_error(InstalledUserStateErrorKind::State, message)
}

fn state_type_mismatch(expected: TypeId, current: TypeId) -> InstalledUserStateError {
    InstalledUserStateError::with_code(
        InstalledUserStateErrorKind::State,
        format!(
            "the USER state cell type {} does not match expected type {}",
            current.canonical(),
            expected.canonical()
        ),
        "ORNA0901",
    )
}

fn plan_instances(
    active: &orna_core::revision::ActiveDatabaseRevision,
    instances: &[InstalledUserStateInstance],
) -> Result<Vec<(FunctionId, String)>, InstalledUserStateError> {
    instances
        .iter()
        .map(|instance| {
            validate_state_text(&instance.instance_key)?;
            active_user_state_plan(active, instance.function)?;
            Ok((instance.function, instance.instance_key.clone()))
        })
        .collect()
}

fn plan_expected_types(
    expected_types: &[InstalledUserStateExpectedType],
) -> std::collections::BTreeMap<(FunctionId, StateSlotId), TypeId> {
    expected_types
        .iter()
        .map(|entry| ((entry.function, entry.state_slot), entry.value_type))
        .collect()
}

fn write_record(result: &orna_core::state::UserStateWriteResult) -> serde_json::Value {
    let key = result.key();
    let mut record = serde_json::json!({
        "root_function": key.root_function().canonical(),
        "state_profile": key.state_profile(),
        "function": key.function().canonical(),
        "instance_key": key.instance_key(),
        "state_slot": key.state_slot().canonical(),
    });
    match result.outcome() {
        UserStateWriteOutcome::Written { revision } => {
            record["outcome"] = serde_json::json!("written");
            record["revision"] = serde_json::json!(revision);
        }
        UserStateWriteOutcome::Conflict { current_revision } => {
            record["outcome"] = serde_json::json!("conflict");
            record["current_revision"] = serde_json::json!(current_revision);
        }
    }
    record
}

fn write_json_line(
    stdout: &mut impl Write,
    record: &serde_json::Value,
) -> Result<(), InstalledUserStateError> {
    let mut line = serde_json::to_vec(record).map_err(|error| {
        state_error(
            InstalledUserStateErrorKind::Presentation,
            format!("could not render USER state output: {error}"),
        )
    })?;
    line.push(b'\n');
    stdout.write_all(&line).map_err(|error| {
        state_error(
            InstalledUserStateErrorKind::Presentation,
            format!("could not write USER state output: {error}"),
        )
    })
}

fn validate_state_text(value: &str) -> Result<(), InstalledUserStateError> {
    if value.contains('\0') {
        return Err(state_error(
            InstalledUserStateErrorKind::State,
            "USER state text keys must not contain NUL bytes",
        ));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn state_error(
    kind: InstalledUserStateErrorKind,
    message: impl Into<String>,
) -> InstalledUserStateError {
    InstalledUserStateError::new(kind, message.into())
}

fn state_sqlite_error(
    kind: InstalledUserStateErrorKind,
    error: SqliteError,
) -> InstalledUserStateError {
    state_error(kind, format!("local SQLite backend error: {error}"))
}
