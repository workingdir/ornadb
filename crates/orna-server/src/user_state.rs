//! Installed `orna state` access to the durable USER state service.
//!
//! This module runs one closed `orna state get|set` command against the fixed
//! private instance with the same host inspection and kernel access as
//! `orna invoke` and `orna security grant-execute` (work ADR 0061 step 5).
//! The server derives the principal from the authenticated session — the
//! local peer UID authenticated through [`PostgresKernel::authenticate_local_peer`]
//! — and a request never carries a principal identity.
//!
//! `orna state get` plans one `load_user_state` call: the root function and
//! state profile scope the load, optional instance requests filter the
//! returned cells, and optional expected-type entries arm the load-time
//! ORNA0901 check. `orna state set` plans one typed `write_user_state`
//! change carrying its expected revision; a conflict is a per-change closed
//! result (ORNA0902), never a transport failure. Every cell and write
//! result is rendered to `stdout` as one JSON record per line, with typed
//! values in their canonical ORV5 hex form.

use std::{collections::BTreeMap, fmt, io, io::Write};

use orna_client::{ClientStateContext, ClientStateStore, ClientUserStateError};
use orna_core::{
    FunctionId, StateSlotId, TypeId,
    security::AuthenticatedSession,
    state::{
        UserStateCell, UserStateChange, UserStateError, UserStateWriteOutcome, UserStateWriteResult,
    },
};
use orna_postgres::{PostgresKernel, PostgresKernelError, UserStateInstanceRequest};
use orna_protocol::{decode_constructed_value, encode_constructed_value};
use orna_standard::registered_opaque_codecs;

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// One complete installed `orna state` command request (ADR 0061 step 5).
///
/// The command parser (step 5) parses the root function, state profile,
/// instance filters, expected types, and the typed write change into this
/// closed request; the host derives the session principal and dispatches.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledUserStateRequest {
    /// The protected state operation to run.
    pub operation: InstalledUserStateOperation,
}

impl InstalledUserStateRequest {
    /// Creates one complete installed state command request.
    pub const fn new(operation: InstalledUserStateOperation) -> Self {
        Self { operation }
    }
}

/// One parsed protected USER state operation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledUserStateOperation {
    /// A `sys_state.load_user_state` operation.
    ///
    /// The load is scoped to the root function and state profile. `instances`
    /// is an optional `(function, instance_key)` filter: the empty list
    /// returns every cell for the root function and profile. `expected_types`
    /// carries the load-time declared slot types and arms the ORNA0901
    /// check; a cell whose persisted type no longer matches fails closed.
    Load {
        /// The root function whose invocation owns the cells.
        root_function: FunctionId,
        /// The state profile; the empty string is the default profile.
        state_profile: String,
        /// The requested function instances to load.
        instances: Vec<InstalledUserStateInstance>,
        /// The load-time declared type by function and state slot.
        expected_types: Vec<InstalledUserStateExpectedType>,
    },
    /// A `sys_state.write_user_state` operation with exactly one typed change.
    Write {
        /// The root function whose invocation owns the cell.
        root_function: FunctionId,
        /// The state profile; the empty string is the default profile.
        state_profile: String,
        /// The single typed change to write.
        change: InstalledUserStateChange,
    },
}

/// One function instance selected by an installed USER state load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledUserStateInstance {
    /// The function owning the requested instance.
    pub function: FunctionId,
    /// The requested instance key; the empty string is the default instance.
    pub instance_key: String,
}

/// One load-time declared type for an installed USER state load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledUserStateExpectedType {
    /// The function that owns the state slot.
    pub function: FunctionId,
    /// The stable state-slot identity.
    pub state_slot: StateSlotId,
    /// The type the slot currently declares.
    pub value_type: TypeId,
}

/// One typed change for an installed USER state write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledUserStateChange {
    /// The function that owns the state slot.
    pub function: FunctionId,
    /// The function-instance identity; the empty string is the default.
    pub instance_key: String,
    /// The stable state-slot identity.
    pub state_slot: StateSlotId,
    /// The expected current revision; `None` requires the cell not to exist.
    pub expected_revision: Option<u64>,
    /// The type of the value to store.
    pub value_type: TypeId,
    /// The canonical ORV5 encoded typed value to store.
    pub value_bytes: Vec<u8>,
}

/// The terminal public result of one installed state command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledUserStateOutcome {
    /// The protected operation completed and its records were rendered.
    Completed,
}

/// The closed failure class of one installed state command.
///
/// The CLI maps each kind to a closed exit code: `Authentication` 3,
/// `State` 1, `Presentation` 5, `Internal` 7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledUserStateErrorKind {
    /// The local peer could not establish an Orna session.
    Authentication,
    /// The protected operation failed closed with a state error.
    State,
    /// A rendered record could not reach standard output.
    Presentation,
    /// Host inspection, recovery, or a kernel failure.
    Internal,
}

/// A failure that prevents or ends one installed state command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledUserStateError {
    kind: InstalledUserStateErrorKind,
    message: String,
    code: Option<&'static str>,
}

impl InstalledUserStateError {
    /// Creates one closed state failure with its message.
    pub fn new(kind: InstalledUserStateErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            code: None,
        }
    }

    /// Creates one closed state failure carrying a spec error code.
    pub fn with_code(
        kind: InstalledUserStateErrorKind,
        message: String,
        code: &'static str,
    ) -> Self {
        Self {
            kind,
            message,
            code: Some(code),
        }
    }

    /// Returns the closed failure class.
    pub const fn kind(&self) -> InstalledUserStateErrorKind {
        self.kind
    }

    /// Returns the stable spec error code for spec-flavoured failures.
    pub const fn code(&self) -> Option<&'static str> {
        self.code
    }

    /// Returns the closed failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InstalledUserStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "orna state: {}", self.message)
    }
}

impl std::error::Error for InstalledUserStateError {}

/// Runs one installed `orna state` command in-process.
///
/// The host inspection retains the package and instance guards for the
/// complete authentication, load, write, and rendering operation. All result
/// records are written to `stdout` as JSON lines; failures are returned to
/// the CLI, which writes them to `stderr`.
///
/// # Errors
///
/// Returns [`InstalledUserStateError`] for host inspection, recovery,
/// authentication, model validation, value codec, kernel, or rendering
/// failures.
pub fn run_installed_user_state(
    request: InstalledUserStateRequest,
    stdout: &mut impl Write,
) -> Result<InstalledUserStateOutcome, InstalledUserStateError> {
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            InstalledUserStateError::new(
                InstalledUserStateErrorKind::Internal,
                "the private runtime could not start".to_owned(),
            )
        })?;

    runtime.block_on(run_user_state_with_kernel(kernel, request, stdout))
}

/// Runs one installed `orna state` command against a caller-supplied kernel
/// (ADR 0061 step 6 live-proof seam).
///
/// The public entry [`run_installed_user_state`] inspects the fixed private
/// instance and delegates here; the live proof drives the exact
/// authenticate-load/write-render path against the Compose PostgreSQL test
/// kernel with the invoking process's local peer credentials. Public
/// consumers keep [`run_installed_user_state`]; this seam is hidden from
/// the documented API surface.
#[doc(hidden)]
pub async fn run_user_state_with_kernel(
    kernel: PostgresKernel,
    request: InstalledUserStateRequest,
    stdout: &mut impl Write,
) -> Result<InstalledUserStateOutcome, InstalledUserStateError> {
    execute_user_state(kernel, &request, stdout).await
}

/// An authenticated CLIENT state transport backed by the protected USER state
/// service.
///
/// The authenticated session is supplied when the adapter is created. The
/// client state model never supplies a principal.
pub struct AuthenticatedClientStateAdapter<'a> {
    kernel: &'a PostgresKernel,
    session: &'a AuthenticatedSession,
}

impl<'a> AuthenticatedClientStateAdapter<'a> {
    /// Creates an adapter for one authenticated kernel session.
    pub const fn new(
        kernel: &'a PostgresKernel,
        session: &'a AuthenticatedSession,
    ) -> Self {
        Self { kernel, session }
    }

    /// Loads the state for one root context into the caller-owned store.
    pub async fn load(
        &self,
        context: &ClientStateContext,
        instances: &[UserStateInstanceRequest],
        expected_types: &BTreeMap<(FunctionId, StateSlotId), TypeId>,
        store: &mut ClientStateStore,
    ) -> Result<(), AuthenticatedClientStateError> {
        let cells = self
            .kernel
            .load_user_state(
                self.session,
                context.root_function(),
                context.state_profile(),
                instances,
                expected_types,
            )
            .await
            .map_err(AuthenticatedClientStateError::Kernel)?;
        store.set_context(context.clone());
        store
            .load_user_state(&cells)
            .map_err(AuthenticatedClientStateError::Client)
    }

    /// Flushes the store's dirty USER values as one bounded authenticated
    /// batch.
    pub async fn flush(
        &self,
        store: &mut ClientStateStore,
    ) -> Result<(), AuthenticatedClientStateError> {
        let changes = store
            .pending_user_state_changes()
            .map_err(AuthenticatedClientStateError::Client)?;
        if changes.is_empty() {
            return Ok(());
        }
        let context = store.context();
        if changes.iter().any(|change| {
            change.root_function() != context.root_function()
                || change.state_profile() != context.state_profile()
        }) {
            return Err(AuthenticatedClientStateError::Client(
                ClientUserStateError::InvalidChange(
                    "dirty USER state spans more than one root context".to_owned(),
                ),
            ));
        }
        let results = self
            .kernel
            .write_user_state(self.session, &changes)
            .await
            .map_err(AuthenticatedClientStateError::Kernel)?;
        store
            .apply_user_state_write_results(&changes, &results)
            .map_err(AuthenticatedClientStateError::Client)
    }
}

/// A failure from an authenticated CLIENT state load or flush.
#[derive(Debug)]
pub enum AuthenticatedClientStateError {
    /// The protected PostgreSQL state service failed.
    Kernel(PostgresKernelError),
    /// The caller-owned client state store rejected the batch.
    Client(ClientUserStateError),
}

impl fmt::Display for AuthenticatedClientStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(source) => source.fmt(formatter),
            Self::Client(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for AuthenticatedClientStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Kernel(source) => Some(source),
            Self::Client(source) => Some(source),
        }
    }
}

async fn execute_user_state(
    kernel: PostgresKernel,
    request: &InstalledUserStateRequest,
    stdout: &mut impl Write,
) -> Result<InstalledUserStateOutcome, InstalledUserStateError> {
    let active = kernel.recover().await.map_err(|_| {
        InstalledUserStateError::new(
            InstalledUserStateErrorKind::Internal,
            "the active revision could not be recovered".to_owned(),
        )
    })?;
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        InstalledUserStateError::new(
            InstalledUserStateErrorKind::Internal,
            "the USER state service requires the verified standard snapshot".to_owned(),
        )
    })?;
    let registry = registered_opaque_codecs(standard).map_err(|_| {
        InstalledUserStateError::new(
            InstalledUserStateErrorKind::Internal,
            "the verified standard snapshot does not bind its opaque codec registry".to_owned(),
        )
    })?;

    let uid = nix::unistd::geteuid().as_raw();
    let session = kernel
        .authenticate_local_peer(uid)
        .await
        .map_err(map_kernel_error)?;

    match &request.operation {
        InstalledUserStateOperation::Load {
            root_function,
            state_profile,
            instances,
            expected_types,
        } => {
            let instances = plan_load_instances(instances)?;
            let expected_types = plan_expected_types(expected_types);
            let cells = kernel
                .load_user_state(
                    &session,
                    *root_function,
                    state_profile,
                    &instances,
                    &expected_types,
                )
                .await
                .map_err(map_kernel_error)?;
            for cell in &cells {
                let encoded = encode_constructed_value(&active, &registry, cell.value())
                    .map_err(map_value_codec_error)?;
                write_json_line(
                    stdout,
                    &load_record(*root_function, state_profile, cell, &encoded),
                )?;
            }
            Ok(InstalledUserStateOutcome::Completed)
        }
        InstalledUserStateOperation::Write {
            root_function,
            state_profile,
            change,
        } => {
            let value = decode_constructed_value(&active, &registry, &change.value_bytes)
                .map_err(map_value_codec_error)?;
            let change = UserStateChange::new(
                *root_function,
                state_profile.clone(),
                change.function,
                change.instance_key.clone(),
                change.state_slot,
                change.expected_revision,
                value,
                change.value_type,
            )
            .map_err(map_model_error)?;
            let results = kernel
                .write_user_state(&session, std::slice::from_ref(&change))
                .await
                .map_err(map_kernel_error)?;
            for result in &results {
                write_json_line(stdout, &write_record(result))?;
            }
            Ok(InstalledUserStateOutcome::Completed)
        }
    }
}

/// Plans the instance filter for one installed load.
///
/// An empty list plans the empty filter: every cell for the root function
/// and profile. Each admitted request validates its instance key through the
/// core durable-key model, so a NUL-bearing request fails closed as a state
/// error before any kernel call.
fn plan_load_instances(
    instances: &[InstalledUserStateInstance],
) -> Result<Vec<UserStateInstanceRequest>, InstalledUserStateError> {
    let mut planned = Vec::with_capacity(instances.len());
    for instance in instances {
        let request =
            UserStateInstanceRequest::new(instance.function, instance.instance_key.clone())
                .map_err(map_model_error)?;
        planned.push(request);
    }
    Ok(planned)
}

/// Plans the load-time declared types for one installed load.
///
/// The map key is the exact `(function, state_slot)` pair; a repeated pair
/// plans as its last declared type, matching the kernel's authoritative
/// lookup.
fn plan_expected_types(
    expected_types: &[InstalledUserStateExpectedType],
) -> BTreeMap<(FunctionId, StateSlotId), TypeId> {
    expected_types
        .iter()
        .map(|entry| ((entry.function, entry.state_slot), entry.value_type))
        .collect()
}

/// Renders one loaded cell as its closed JSON record.
fn load_record(
    root_function: FunctionId,
    state_profile: &str,
    cell: &UserStateCell,
    encoded: &[u8],
) -> serde_json::Value {
    serde_json::json!({
        "root_function": root_function.canonical(),
        "state_profile": state_profile,
        "function": cell.key().function().canonical(),
        "instance_key": cell.key().instance_key(),
        "state_slot": cell.key().state_slot().canonical(),
        "revision": cell.revision(),
        "value_type": cell.value_type().canonical(),
        "value_hex": encode_hex(encoded),
    })
}

/// Renders one closed write result as its JSON record.
fn write_record(result: &UserStateWriteResult) -> serde_json::Value {
    let key = result.key();
    let identity = |outcome: &str| {
        serde_json::json!({
            "root_function": key.root_function().canonical(),
            "state_profile": key.state_profile(),
            "function": key.function().canonical(),
            "instance_key": key.instance_key(),
            "state_slot": key.state_slot().canonical(),
            "outcome": outcome,
        })
    };
    match result.outcome() {
        UserStateWriteOutcome::Written { revision } => {
            let mut record = identity("written");
            record["revision"] = serde_json::json!(revision);
            record
        }
        UserStateWriteOutcome::Conflict { current_revision } => {
            let mut record = identity("conflict");
            record["current_revision"] = serde_json::json!(current_revision);
            record
        }
    }
}

/// Writes exactly one JSON record followed by the record newline.
fn write_json_line(
    output: &mut impl Write,
    record: &serde_json::Value,
) -> Result<(), InstalledUserStateError> {
    let mut line = serde_json::to_string(record).map_err(|error| {
        InstalledUserStateError::new(
            InstalledUserStateErrorKind::Internal,
            format!("a state record could not be rendered: {error}"),
        )
    })?;
    line.push('\n');
    output
        .write_all(line.as_bytes())
        .map_err(presentation_error)
}

/// Renders one canonical bytes payload as lowercase hex.
fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

fn map_host_error(error: EmbeddedHostError) -> InstalledUserStateError {
    InstalledUserStateError::new(
        InstalledUserStateErrorKind::Internal,
        format!("the installed Orna instance is not available: {error}"),
    )
}

fn map_kernel_error(error: PostgresKernelError) -> InstalledUserStateError {
    match error {
        PostgresKernelError::UserState(model) => InstalledUserStateError {
            kind: InstalledUserStateErrorKind::State,
            message: model.to_string(),
            code: model.code(),
        },
        PostgresKernelError::UserStateValueCodec(error) => InstalledUserStateError::new(
            InstalledUserStateErrorKind::State,
            format!("USER state value codec failed: {error}"),
        ),
        PostgresKernelError::LocalPeerAuthentication(error) => InstalledUserStateError::new(
            InstalledUserStateErrorKind::Authentication,
            format!("the local peer could not authenticate: {error}"),
        ),
        other => InstalledUserStateError::new(
            InstalledUserStateErrorKind::Internal,
            format!("the USER state operation failed: {other}"),
        ),
    }
}

fn map_model_error(error: UserStateError) -> InstalledUserStateError {
    InstalledUserStateError {
        kind: InstalledUserStateErrorKind::State,
        message: error.to_string(),
        code: error.code(),
    }
}

fn map_value_codec_error(error: orna_protocol::ValueCodecError) -> InstalledUserStateError {
    InstalledUserStateError::new(
        InstalledUserStateErrorKind::State,
        format!("the canonical typed value codec failed: {error}"),
    )
}

fn presentation_error(error: io::Error) -> InstalledUserStateError {
    InstalledUserStateError::new(
        InstalledUserStateErrorKind::Presentation,
        format!("a state record could not be written: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use orna_core::{
        PrincipalId,
        security::LocalPeerAuthenticationError,
        state::{UserStateKey, UserStateKeyWithoutPrincipal, UserStateWriteOutcome},
        value::RuntimeValue,
    };

    use super::*;

    fn function(value: u8) -> FunctionId {
        FunctionId::from_bytes([value; 16])
    }

    fn slot(value: u8) -> StateSlotId {
        StateSlotId::from_bytes([value; 16])
    }

    fn value_type(value: u8) -> TypeId {
        TypeId::from_bytes([value; 16])
    }

    fn principal(value: u8) -> PrincipalId {
        PrincipalId::from_bytes([value; 16])
    }

    fn key_without_principal() -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal::new(
            function(0x11),
            "profile".to_owned(),
            function(0x22),
            "instance".to_owned(),
            slot(0x33),
        )
        .expect("fixture key must validate")
    }

    /// The load filter is the exact root function, state profile, instance
    /// pair set, and expected-type map the kernel operation receives.
    #[test]
    fn load_plan_builds_the_exact_kernel_arguments() {
        let instances = vec![
            InstalledUserStateInstance {
                function: function(0x21),
                instance_key: "player-7".to_owned(),
            },
            InstalledUserStateInstance {
                function: function(0x22),
                instance_key: String::new(),
            },
        ];
        let expected_types = vec![InstalledUserStateExpectedType {
            function: function(0x21),
            state_slot: slot(0x31),
            value_type: value_type(0x41),
        }];

        let planned = plan_load_instances(&instances).expect("fixture instances must plan");
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].function(), function(0x21));
        assert_eq!(planned[0].instance_key(), "player-7");
        assert_eq!(planned[1].function(), function(0x22));
        assert_eq!(planned[1].instance_key(), "");

        let planned_types = plan_expected_types(&expected_types);
        assert_eq!(planned_types.len(), 1);
        assert_eq!(
            planned_types.get(&(function(0x21), slot(0x31))),
            Some(&value_type(0x41))
        );
    }

    /// An empty instance list plans the empty filter (every cell for the
    /// root and profile), and an empty expected-type list plans the empty
    /// declared-type map.
    #[test]
    fn load_plan_keeps_absent_filters_empty() {
        assert!(
            plan_load_instances(&[])
                .expect("no instances must plan")
                .is_empty()
        );
        assert!(plan_expected_types(&[]).is_empty());
    }

    /// A repeated expected-type pair plans as its last declared type.
    #[test]
    fn load_plan_repeated_expected_type_pair_takes_the_last_declaration() {
        let expected_types = vec![
            InstalledUserStateExpectedType {
                function: function(0x21),
                state_slot: slot(0x31),
                value_type: value_type(0x41),
            },
            InstalledUserStateExpectedType {
                function: function(0x21),
                state_slot: slot(0x31),
                value_type: value_type(0x42),
            },
        ];
        let planned = plan_expected_types(&expected_types);
        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned.get(&(function(0x21), slot(0x31))),
            Some(&value_type(0x42))
        );
    }

    /// A NUL-bearing instance key fails closed through the durable-key model
    /// as a typed state error before any kernel call.
    #[test]
    fn load_plan_rejects_nul_bearing_instance_keys() {
        let instances = vec![InstalledUserStateInstance {
            function: function(0x21),
            instance_key: "bad\0key".to_owned(),
        }];
        let error = plan_load_instances(&instances).expect_err("NUL key must fail closed");
        assert_eq!(error.kind(), InstalledUserStateErrorKind::State);
        assert_eq!(error.code(), None);
        assert!(error.message().contains("NUL"), "{}", error.message());
    }

    /// The write plan maps the parsed change exactly: root, profile, slot,
    /// expectation, typed value, and the closed key without the principal.
    #[test]
    fn write_plan_maps_every_change_component() {
        let request = InstalledUserStateChange {
            function: function(0x22),
            instance_key: "instance".to_owned(),
            state_slot: slot(0x33),
            expected_revision: Some(7),
            value_type: value_type(0x41),
            value_bytes: vec![0x0a, 0x0b],
        };
        let change = UserStateChange::new(
            function(0x11),
            "profile".to_owned(),
            request.function,
            request.instance_key.clone(),
            request.state_slot,
            request.expected_revision,
            RuntimeValue::Boolean(true),
            request.value_type,
        )
        .expect("fixture change must validate");

        assert_eq!(change.root_function(), function(0x11));
        assert_eq!(change.state_profile(), "profile");
        assert_eq!(change.function(), function(0x22));
        assert_eq!(change.instance_key(), "instance");
        assert_eq!(change.state_slot(), slot(0x33));
        assert_eq!(change.expected_revision(), Some(7));
        assert_eq!(change.value(), &RuntimeValue::Boolean(true));
        assert_eq!(change.value_type(), value_type(0x41));
        assert_eq!(change.key_without_principal(), key_without_principal());
    }

    /// Revision expectations stay closed: `None` requires the cell not to
    /// exist and `Some(r)` requires the current revision to equal `r`.
    #[test]
    fn write_plan_accepts_a_create_expectation() {
        let change = UserStateChange::new(
            function(0x11),
            String::new(),
            function(0x22),
            String::new(),
            slot(0x33),
            None,
            RuntimeValue::Boolean(true),
            value_type(0x41),
        )
        .expect("fixture change must validate");
        assert_eq!(change.expected_revision(), None);
    }

    /// A NUL-bearing profile fails closed through the durable-key model as a
    /// typed state error.
    #[test]
    fn write_plan_rejects_nul_bearing_profiles() {
        let error = UserStateChange::new(
            function(0x11),
            "bad\0profile".to_owned(),
            function(0x22),
            String::new(),
            slot(0x33),
            None,
            RuntimeValue::Boolean(true),
            value_type(0x41),
        )
        .expect_err("NUL profile must fail closed");
        assert_eq!(
            error,
            UserStateError::InvalidKey {
                reason: "state profile must not contain a NUL byte".to_owned(),
            }
        );
    }

    /// Every write result renders exactly one closed JSON record with the
    /// revision outcome spelled out.
    #[test]
    fn write_results_render_closed_records() {
        let key = key_without_principal();

        let written =
            UserStateWriteResult::new(key.clone(), UserStateWriteOutcome::Written { revision: 4 });
        assert_eq!(
            write_record(&written),
            serde_json::json!({
                "root_function": function(0x11).canonical(),
                "state_profile": "profile",
                "function": function(0x22).canonical(),
                "instance_key": "instance",
                "state_slot": slot(0x33).canonical(),
                "outcome": "written",
                "revision": 4,
            })
        );

        let conflict = UserStateWriteResult::new(
            key,
            UserStateWriteOutcome::Conflict {
                current_revision: 2,
            },
        );
        assert_eq!(
            write_record(&conflict),
            serde_json::json!({
                "root_function": function(0x11).canonical(),
                "state_profile": "profile",
                "function": function(0x22).canonical(),
                "instance_key": "instance",
                "state_slot": slot(0x33).canonical(),
                "outcome": "conflict",
                "current_revision": 2,
            })
        );
    }

    /// A loaded cell renders its key, revision, persisted type, and the
    /// canonical ORV5 value in lowercase hex.
    #[test]
    fn cells_render_closed_records() {
        let key = UserStateKey::new(
            principal(0x01),
            function(0x11),
            "profile".to_owned(),
            function(0x22),
            "instance".to_owned(),
            slot(0x33),
        )
        .expect("fixture key must validate");
        let cell = UserStateCell::new(
            key,
            RuntimeValue::Boolean(true),
            value_type(0x41),
            3,
            SystemTime::UNIX_EPOCH,
        );

        assert_eq!(
            load_record(function(0x11), "profile", &cell, &[0xca, 0x42]),
            serde_json::json!({
                "root_function": function(0x11).canonical(),
                "state_profile": "profile",
                "function": function(0x22).canonical(),
                "instance_key": "instance",
                "state_slot": slot(0x33).canonical(),
                "revision": 3,
                "value_type": value_type(0x41).canonical(),
                "value_hex": "ca42",
            })
        );
    }

    /// Kernel USER state failures surface as typed outcomes with the stable
    /// spec codes; every other kernel failure is closed as internal.
    #[test]
    fn kernel_errors_classify_user_state_outcomes() {
        let key = key_without_principal();

        let type_error = UserStateError::TypeIncompatible {
            key: Box::new(key.clone()),
            expected: value_type(0x41),
            current: value_type(0x42),
        };
        let mapped = map_kernel_error(PostgresKernelError::UserState(type_error));
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::State);
        assert_eq!(mapped.code(), Some("ORNA0901"));
        assert!(mapped.message().contains("ORNA0901"));

        let revision_error = UserStateError::RevisionConflict {
            key: Box::new(key.clone()),
            expected: Some(2),
            current: 4,
        };
        let mapped = map_kernel_error(PostgresKernelError::UserState(revision_error));
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::State);
        assert_eq!(mapped.code(), Some("ORNA0902"));
        assert!(mapped.message().contains("current 4"));

        let spoof_error = UserStateError::PrincipalSpoofAttempt {
            cell_principal: principal(0x02),
            session_principal: principal(0x01),
        };
        let mapped = map_kernel_error(PostgresKernelError::UserState(spoof_error));
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::State);
        assert_eq!(mapped.code(), Some("ORNA0903"));

        let invalid_key = UserStateError::InvalidKey {
            reason: "a key component cannot round-trip".to_owned(),
        };
        let mapped = map_kernel_error(PostgresKernelError::UserState(invalid_key));
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::State);
        assert_eq!(mapped.code(), None);

        let denied = PostgresKernelError::RawCallTargetUnavailable {
            function: function(0x11),
            rule: "closed raw-call rule",
        };
        let mapped = map_kernel_error(denied);
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::Internal);
        assert_eq!(mapped.code(), None);
    }

    /// A failed local peer authentication surfaces as the closed
    /// authentication class.
    #[test]
    fn kernel_errors_classify_authentication_failures() {
        let mapped = map_kernel_error(PostgresKernelError::LocalPeerAuthentication(
            LocalPeerAuthenticationError::UnknownUid,
        ));
        assert_eq!(mapped.kind(), InstalledUserStateErrorKind::Authentication);
        assert_eq!(mapped.code(), None);
    }

    /// Canonical payloads render as stable lowercase hex.
    #[test]
    fn hex_encoding_is_lowercase_and_stable() {
        assert_eq!(encode_hex(&[]), "");
        assert_eq!(encode_hex(&[0x00, 0x0a, 0x0f, 0x10, 0xff]), "000a0f10ff");
    }
}
