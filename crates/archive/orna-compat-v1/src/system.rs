//! The sealed registry of mandatory ring-1 system functions.
//!
//! Work ADR 0042 defines one compiled registry in `orna-compat-v1` for functions
//! that must exist before an application catalogue is available. The registry
//! is not reconstructed from application source, the standard library,
//! persisted rows, environment values, or configuration. The first two
//! entries are catalogue health and the root invocation gateway.
//!
//! Name comparison uses the complete case-sensitive resolved parts. It does
//! not perform case folding, prefix matching, alias lookup, search-path
//! resolution, or application-catalogue fallback.
//!
//! Application catalogues remain separate from this registry. They cannot
//! replace a sealed definition by reusing its identity or exact name.

use crate::{FunctionId, ParameterId, TypeId, catalogue::QualifiedSemanticName};
use orna_sys_id::EngineFunction;

/// Defines one sealed semantic name and its resolved parts from a single
/// segment list, so the dotted and part forms cannot drift.
macro_rules! sealed_name {
    ($(#[$meta:meta])* $name:ident, $parts:ident, $first:literal $(, $rest:literal)*) => {
        $(#[$meta])*
        pub const $name: &str = concat!($first $(, ".", $rest)*);
        const $parts: &[&str] = &[$first $(, $rest)*];
    };
}

/// Defines one sealed function's public identity from the generated engine
/// catalogue, so the durable id, exact name and name parts cannot drift.
macro_rules! sealed_function {
    ($(#[$meta:meta])* $id:ident, $name:ident, $variant:ident) => {
        $(#[$meta])*
        pub const $id: FunctionId = FunctionId::from_bytes(EngineFunction::$variant.function_id());
        pub const $name: &str = EngineFunction::$variant.label();
    };
}

sealed_function!(
/// The stable identity of the sealed `sys.catalog.health` function.
    CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME,
    CatalogHealth
);

sealed_function!(
/// The stable identity of the mandatory sealed `sys.invoke` function.
    SYS_INVOKE_FUNCTION_ID, SYS_INVOKE_FUNCTION_NAME,
    Invoke
);

sealed_function!(
/// The stable identity of the generic source-introspection function.
    SYS_SOURCE_CURRENT_FUNCTION_ID, SYS_SOURCE_CURRENT_FUNCTION_NAME,
    SourceCurrent
);

/// The stable identity of the source-introspection result carrier.
pub const SYS_SOURCE_FUNCTION_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x31]);

/// The exact semantic name of the source-introspection result carrier.
pub const SYS_SOURCE_FUNCTION_TYPE_NAME: &str = "sys.source.function";

/// The immutable representation contract of the source-introspection carrier.
pub const SYS_SOURCE_FUNCTION_REPRESENTATION_CONTRACT: &str = "orna.sys.source.function@1";

sealed_function!(
/// The stable identity of the sealed `sys.inspect.snapshot` function.
    SYS_INSPECT_SNAPSHOT_FUNCTION_ID, SYS_INSPECT_SNAPSHOT_FUNCTION_NAME,
    InspectSnapshot
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.invocation_nodes` function.
    SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID, SYS_INSPECT_INVOCATION_NODES_FUNCTION_NAME,
    InspectInvocationNodes
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.calls` function.
    SYS_INSPECT_CALLS_FUNCTION_ID, SYS_INSPECT_CALLS_FUNCTION_NAME,
    InspectCalls
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.resources` function.
    SYS_INSPECT_RESOURCES_FUNCTION_ID, SYS_INSPECT_RESOURCES_FUNCTION_NAME,
    InspectResources
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.state_cells` function.
    SYS_INSPECT_STATE_CELLS_FUNCTION_ID, SYS_INSPECT_STATE_CELLS_FUNCTION_NAME,
    InspectStateCells
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.ui_nodes` function.
    SYS_INSPECT_UI_NODES_FUNCTION_ID, SYS_INSPECT_UI_NODES_FUNCTION_NAME,
    InspectUiNodes
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.presentation_candidates` function.
    SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID, SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_NAME,
    InspectPresentationCandidates
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.runtime_bindings` function.
    SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID, SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_NAME,
    InspectRuntimeBindings
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.security_decisions` function.
    SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID, SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_NAME,
    InspectSecurityDecisions
);

sealed_function!(
/// The stable identity of the sealed `sys.inspect.trace` function.
    SYS_INSPECT_TRACE_FUNCTION_ID, SYS_INSPECT_TRACE_FUNCTION_NAME,
    InspectTrace
);

sealed_function!(
/// The stable identity of the sealed `sys.security.session_principal` function.
    SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_NAME,
    SecuritySessionPrincipal
);

sealed_function!(
/// The stable identity of the sealed `sys.security.effective_principal` function.
    SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_NAME,
    SecurityEffectivePrincipal
);

sealed_function!(
/// The stable identity of the sealed `sys.security.active_roles` function.
    SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_ACTIVE_ROLES_FUNCTION_NAME,
    SecurityActiveRoles
);

sealed_function!(
/// The stable identity of the sealed `sys.security.create_principal` function.
    SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_NAME,
    SecurityCreatePrincipal
);

sealed_function!(
/// The stable identity of the sealed `sys.security.disable_principal` function.
    SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_NAME,
    SecurityDisablePrincipal
);

sealed_function!(
/// The stable identity of the sealed `sys.security.create_role` function.
    SYS_SECURITY_CREATE_ROLE_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_NAME,
    SecurityCreateRole
);

sealed_function!(
/// The stable identity of the sealed `sys.security.grant_role` function.
    SYS_SECURITY_GRANT_ROLE_FUNCTION_ID, SYS_SECURITY_GRANT_ROLE_FUNCTION_NAME,
    SecurityGrantRole
);

sealed_function!(
/// The stable identity of the sealed `sys.security.revoke_role` function.
    SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID, SYS_SECURITY_REVOKE_ROLE_FUNCTION_NAME,
    SecurityRevokeRole
);

sealed_function!(
/// The stable identity of the sealed `sys.security.grant_privilege` function.
    SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_NAME,
    SecurityGrantPrivilege
);

sealed_function!(
/// The stable identity of the sealed `sys.security.revoke_privilege` function.
    SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_NAME,
    SecurityRevokePrivilege
);

sealed_function!(
/// The stable identity of the sealed `sys.security.can_execute` function.
    SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID, SYS_SECURITY_CAN_EXECUTE_FUNCTION_NAME,
    SecurityCanExecute
);

sealed_function!(
/// The stable identity of the sealed `sys.security.has_privilege` function.
    SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_NAME,
    SecurityHasPrivilege
);

sealed_function!(
/// The stable identity of the sealed `sys.state.load_user_state` function.
    SYS_STATE_LOAD_USER_STATE_FUNCTION_ID, SYS_STATE_LOAD_USER_STATE_FUNCTION_NAME,
    StateLoadUserState
);

sealed_function!(
/// The stable identity of the sealed `sys.state.write_user_state` function.
    SYS_STATE_WRITE_USER_STATE_FUNCTION_ID, SYS_STATE_WRITE_USER_STATE_FUNCTION_NAME,
    StateWriteUserState
);

/// The stable identity of the sole sealed `sys.invoke` parameter.
pub const SYS_INVOKE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);

/// The exact name of the sole sealed `sys.invoke` parameter.
pub const SYS_INVOKE_PARAMETER_NAME: &str = "p_request";

/// The stable identity of the sealed `sys.invoke.Value` carrier.
pub const SYS_INVOKE_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]);

sealed_name!(
    /// The exact semantic name of the sealed invocation-value carrier.
    SYS_INVOKE_VALUE_TYPE_NAME,
    SYS_INVOKE_VALUE_NAME_PARTS,
    "sys", "invoke", "Value"
);

/// The immutable representation contract of the sealed invocation-value carrier.
pub const SYS_INVOKE_VALUE_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.value@1";

/// The stable identity of the sealed `sys.invoke.Request` carrier.
pub const SYS_INVOKE_REQUEST_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]);

sealed_name!(
    /// The exact semantic name of the sealed invocation-request carrier.
    SYS_INVOKE_REQUEST_TYPE_NAME,
    SYS_INVOKE_REQUEST_NAME_PARTS,
    "sys", "invoke", "Request"
);

/// The immutable representation contract of the sealed invocation-request carrier.
pub const SYS_INVOKE_REQUEST_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.request@1";

/// The stable identity of the sealed `sys.invoke.Event` carrier.
pub const SYS_INVOKE_EVENT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]);

sealed_name!(
    /// The exact semantic name of the sealed invocation-event carrier.
    SYS_INVOKE_EVENT_TYPE_NAME,
    SYS_INVOKE_EVENT_NAME_PARTS,
    "sys", "invoke", "Event"
);

/// The immutable representation contract of the sealed invocation-event carrier.
pub const SYS_INVOKE_EVENT_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.event@1";

/// The stable identity of the sealed `sys.inspect.invocation` carrier.
pub const SYS_INSPECT_INVOCATION_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3]);

/// The exact semantic name of the sealed inspect-invocation carrier.
pub const SYS_INSPECT_INVOCATION_TYPE_NAME: &str = "sys.inspect.invocation";

/// The immutable representation contract of the sealed inspect-invocation carrier.
pub const SYS_INSPECT_INVOCATION_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.invocation@1";

/// The stable identity of the sealed `sys.inspect.snapshot` carrier.
pub const SYS_INSPECT_SNAPSHOT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf4]);

/// The exact semantic name of the sealed inspect-snapshot carrier.
pub const SYS_INSPECT_SNAPSHOT_TYPE_NAME: &str = "sys.inspect.snapshot";

/// The immutable representation contract of the sealed inspect-snapshot carrier.
pub const SYS_INSPECT_SNAPSHOT_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.snapshot@1";

/// The stable identity of the sealed `sys.inspect.snapshot_options` carrier.
pub const SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf5]);

/// The exact semantic name of the sealed inspect-snapshot-options carrier.
pub const SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_NAME: &str = "sys.inspect.snapshot_options";

/// The immutable representation contract of the sealed inspect-options carrier.
pub const SYS_INSPECT_SNAPSHOT_OPTIONS_REPRESENTATION_CONTRACT: &str =
    "orna.sys.inspect.snapshot_options@1";

/// The stable identity of the sealed `sys.inspect.trace_event` carrier.
pub const SYS_INSPECT_TRACE_EVENT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf6]);

/// The exact semantic name of the sealed inspect-trace-event carrier.
pub const SYS_INSPECT_TRACE_EVENT_TYPE_NAME: &str = "sys.inspect.trace_event";

/// The immutable representation contract of the sealed inspect-trace-event carrier.
pub const SYS_INSPECT_TRACE_EVENT_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.trace_event@1";

// `BIGINT` is the retained standard scalar identity (...0x03). Keeping this
// local avoids a dependency from `orna-compat-v1` back to `orna-standard`.
const SYS_INSPECT_TRACE_AFTER_SEQUENCE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);

/// The stable identity of the sealed `sys.inspect.invocation_nodes` result carrier.
pub const SYS_INSPECT_INVOCATION_NODES_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf8]);

/// The exact semantic name of the sealed invocation-nodes result carrier.
pub const SYS_INSPECT_INVOCATION_NODES_TYPE_NAME: &str = "sys.inspect.invocation_nodes";

/// The immutable representation contract of the sealed invocation-nodes result carrier.
pub const SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT: &str =
    "orna.sys.inspect.invocation_nodes@1";

/// The stable identity of the sealed `sys.inspect.calls` result carrier.
pub const SYS_INSPECT_CALLS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf9]);

/// The exact semantic name of the sealed calls result carrier.
pub const SYS_INSPECT_CALLS_TYPE_NAME: &str = "sys.inspect.calls";

/// The immutable representation contract of the sealed calls result carrier.
pub const SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.calls@1";

/// The stable identity of the sealed `sys.inspect.resources` result carrier.
pub const SYS_INSPECT_RESOURCES_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfa]);

/// The exact semantic name of the sealed resources result carrier.
pub const SYS_INSPECT_RESOURCES_TYPE_NAME: &str = "sys.inspect.resources";

/// The immutable representation contract of the sealed resources result carrier.
pub const SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.resources@1";

/// The stable identity of the sealed `sys.inspect.state_cells` result carrier.
pub const SYS_INSPECT_STATE_CELLS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfb]);

/// The exact semantic name of the sealed state-cells result carrier.
pub const SYS_INSPECT_STATE_CELLS_TYPE_NAME: &str = "sys.inspect.state_cells";

/// The immutable representation contract of the sealed state-cells result carrier.
pub const SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.state_cells@1";

/// The stable identity of the sealed `sys.inspect.ui_nodes` result carrier.
pub const SYS_INSPECT_UI_NODES_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfc]);

/// The exact semantic name of the sealed UI-nodes result carrier.
pub const SYS_INSPECT_UI_NODES_TYPE_NAME: &str = "sys.inspect.ui_nodes";

/// The immutable representation contract of the sealed UI-nodes result carrier.
pub const SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT: &str = "orna.sys.inspect.ui_nodes@1";

/// The stable identity of the sealed `sys.inspect.presentation_candidates` result carrier.
pub const SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfd]);

/// The exact semantic name of the sealed presentation-candidates result carrier.
pub const SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_NAME: &str =
    "sys.inspect.presentation_candidates";

/// The immutable representation contract of the sealed presentation-candidates result carrier.
pub const SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT: &str =
    "orna.sys.inspect.presentation_candidates@1";

/// The stable identity of the sealed `sys.inspect.runtime_bindings` result carrier.
pub const SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfe]);

/// The exact semantic name of the sealed runtime-bindings result carrier.
pub const SYS_INSPECT_RUNTIME_BINDINGS_TYPE_NAME: &str = "sys.inspect.runtime_bindings";

/// The immutable representation contract of the sealed runtime-bindings result carrier.
pub const SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT: &str =
    "orna.sys.inspect.runtime_bindings@1";

/// The stable identity of the sealed `sys.inspect.security_decisions` result carrier.
pub const SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff]);

/// The exact semantic name of the sealed security-decisions result carrier.
pub const SYS_INSPECT_SECURITY_DECISIONS_TYPE_NAME: &str = "sys.inspect.security_decisions";

/// The immutable representation contract of the sealed security-decisions result carrier.
pub const SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT: &str =
    "orna.sys.inspect.security_decisions@1";
/// The stable identity of the sealed `sys.security.principal` carrier.
pub const SYS_SECURITY_PRINCIPAL_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf7]);

/// The exact semantic name of the sealed security-principal carrier.
pub const SYS_SECURITY_PRINCIPAL_TYPE_NAME: &str = "sys.security.principal";

/// The immutable representation contract of the sealed security-principal carrier.
pub const SYS_SECURITY_PRINCIPAL_REPRESENTATION_CONTRACT: &str = "orna.sys.security.principal@1";

/// The behaviour selected for one sealed system function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SystemFunctionKind {
    /// Verifies the complete active database and protected security state.
    Health,
    /// Plans and coordinates one authenticated root invocation.
    Invoke,
    /// Captures one immutable inspection snapshot epoch.
    InspectSnapshot,
    /// Reads one closed projection over an inspection epoch.
    InspectProjection,
    /// Streams the sequence-addressable trace of one invocation.
    InspectTrace,
    /// Reads one source declaration from the active execution revision.
    SourceIntrospection,
    /// Reads one session identity fact from a bound authenticated session.
    SecurityIdentity,
    /// Mutates protected principal, role, or privilege state.
    SecurityAdmin,
    /// Checks one execute grant or privilege class against the snapshot.
    SecurityCheck,
    /// Reads or writes one authenticated principal's durable USER state.
    State,
}

/// The sealed invocation-carrier behaviour selected by one registry entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InvocationCarrierKind {
    /// A typed invocation value.
    Value,
    /// A root invocation request.
    Request,
    /// A root invocation event.
    Event,
}

/// One immutable entry in the sealed invocation-carrier registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvocationCarrierDefinition {
    kind: InvocationCarrierKind,
    id: TypeId,
    name: &'static str,
    name_parts: &'static [&'static str],
    representation_contract: &'static str,
}

impl InvocationCarrierDefinition {
    const fn new(
        kind: InvocationCarrierKind,
        id: TypeId,
        name: &'static str,
        name_parts: &'static [&'static str],
        representation_contract: &'static str,
    ) -> Self {
        Self {
            kind,
            id,
            name,
            name_parts,
            representation_contract,
        }
    }

    /// Returns the sealed behaviour selected for this carrier.
    pub const fn kind(self) -> InvocationCarrierKind {
        self.kind
    }

    /// Returns this carrier's stable identity.
    pub const fn id(self) -> TypeId {
        self.id
    }

    /// Returns this carrier's exact case-sensitive semantic name.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the exact resolved qualified-name parts.
    pub const fn name_parts(self) -> &'static [&'static str] {
        self.name_parts
    }

    /// Returns this carrier's immutable representation contract.
    pub const fn representation_contract(self) -> &'static str {
        self.representation_contract
    }

    pub(crate) fn has_name(self, name: &QualifiedSemanticName) -> bool {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(self.name_parts.iter().copied())
    }
}

/// The complete sealed invocation-carrier registry in canonical order.
///
/// These definitions use fixed `TypeId` bytes. This registry does not allocate
/// an identity and cannot register an additional carrier at run time.
pub const INVOCATION_CARRIERS: &[InvocationCarrierDefinition] = &[
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Value,
        SYS_INVOKE_VALUE_TYPE_ID,
        SYS_INVOKE_VALUE_TYPE_NAME,
        SYS_INVOKE_VALUE_NAME_PARTS,
        SYS_INVOKE_VALUE_REPRESENTATION_CONTRACT,
    ),
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Request,
        SYS_INVOKE_REQUEST_TYPE_ID,
        SYS_INVOKE_REQUEST_TYPE_NAME,
        SYS_INVOKE_REQUEST_NAME_PARTS,
        SYS_INVOKE_REQUEST_REPRESENTATION_CONTRACT,
    ),
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Event,
        SYS_INVOKE_EVENT_TYPE_ID,
        SYS_INVOKE_EVENT_TYPE_NAME,
        SYS_INVOKE_EVENT_NAME_PARTS,
        SYS_INVOKE_EVENT_REPRESENTATION_CONTRACT,
    ),
];

/// Resolves one exact sealed invocation-carrier identity.
pub fn invocation_carrier_by_id(id: TypeId) -> Option<InvocationCarrierDefinition> {
    INVOCATION_CARRIERS
        .iter()
        .copied()
        .find(|definition| definition.id == id)
}

/// Resolves one exact sealed invocation-carrier name.
pub fn invocation_carrier_by_name(
    name: &QualifiedSemanticName,
) -> Option<InvocationCarrierDefinition> {
    INVOCATION_CARRIERS
        .iter()
        .copied()
        .find(|definition| definition.has_name(name))
}

/// Returns whether `type_id` is one of the transient sealed Inspector carriers:
/// the invocation handle, the snapshot and its options, the trace event, and the
/// eight projection result carriers.
///
/// Durable state and catalogue declarations must never turn any of these sealed
/// identities into persistent value type identity.
pub(crate) fn is_sealed_inspect_type_id(type_id: TypeId) -> bool {
    matches!(
        type_id,
        SYS_INSPECT_INVOCATION_TYPE_ID
            | SYS_INSPECT_SNAPSHOT_TYPE_ID
            | SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            | SYS_INSPECT_TRACE_EVENT_TYPE_ID
            | SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            | SYS_INSPECT_CALLS_TYPE_ID
            | SYS_INSPECT_RESOURCES_TYPE_ID
            | SYS_INSPECT_STATE_CELLS_TYPE_ID
            | SYS_INSPECT_UI_NODES_TYPE_ID
            | SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            | SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            | SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
    )
}

/// The immutable sealed signature of `sys.invoke`.
///
/// This is not an application function signature. It has exactly one required,
/// non-null Request parameter and one stream item type for Event. A caller can
/// obtain it only from the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemInvocationSignature {
    request_parameter_id: ParameterId,
    request_parameter_name: &'static str,
    request_type: TypeId,
    request_required: bool,
    request_non_null: bool,
    event_stream_item_type: TypeId,
}

impl SystemInvocationSignature {
    const fn new(
        request_parameter_id: ParameterId,
        request_parameter_name: &'static str,
        request_type: TypeId,
        request_required: bool,
        request_non_null: bool,
        event_stream_item_type: TypeId,
    ) -> Self {
        Self {
            request_parameter_id,
            request_parameter_name,
            request_type,
            request_required,
            request_non_null,
            event_stream_item_type,
        }
    }

    /// Returns the stable identity of the sole Request parameter.
    pub const fn request_parameter_id(self) -> ParameterId {
        self.request_parameter_id
    }

    /// Returns the exact name of the sole Request parameter.
    pub const fn request_parameter_name(self) -> &'static str {
        self.request_parameter_name
    }

    /// Returns the sealed type of the sole Request parameter.
    pub const fn request_type(self) -> TypeId {
        self.request_type
    }

    /// Returns whether the sole Request parameter is required.
    pub const fn request_is_required(self) -> bool {
        self.request_required
    }

    /// Returns whether the sole Request parameter rejects null.
    pub const fn request_is_non_null(self) -> bool {
        self.request_non_null
    }

    /// Returns the sealed Event type carried by the result stream.
    pub const fn event_stream_item_type(self) -> TypeId {
        self.event_stream_item_type
    }
}

const SYS_INVOKE_SIGNATURE: SystemInvocationSignature = SystemInvocationSignature::new(
    SYS_INVOKE_PARAMETER_ID,
    SYS_INVOKE_PARAMETER_NAME,
    SYS_INVOKE_REQUEST_TYPE_ID,
    true,
    true,
    SYS_INVOKE_EVENT_TYPE_ID,
);

/// The immutable sealed signature shape of one `sys.inspect` entry.
///
/// Snapshot capture and the eight materialized projections expose their exact
/// sealed parameter and result carrier identities. The trace entry retains its
/// existing count and stream-item metadata and also identifies its BIGINT
/// sequence parameter without introducing a result carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemInspectSignature {
    parameter_count: u8,
    parameter_types: &'static [TypeId],
    result_type: Option<TypeId>,
    stream_item_type: Option<TypeId>,
}

impl SystemInspectSignature {
    const fn new(
        parameter_count: u8,
        parameter_types: &'static [TypeId],
        result_type: Option<TypeId>,
        stream_item_type: Option<TypeId>,
    ) -> Self {
        Self {
            parameter_count,
            parameter_types,
            result_type,
            stream_item_type,
        }
    }

    /// Returns the fixed sealed parameter count.
    pub const fn parameter_count(self) -> u8 {
        self.parameter_count
    }

    /// Returns the exact typed sealed parameter identities in declaration order.
    pub const fn parameter_types(self) -> &'static [TypeId] {
        self.parameter_types
    }

    /// Returns one typed sealed parameter identity by declaration index.
    pub const fn parameter_type(self, index: usize) -> Option<TypeId> {
        if index < self.parameter_types.len() {
            Some(self.parameter_types[index])
        } else {
            None
        }
    }

    /// Returns the materialized sealed result carrier identity, when present.
    pub const fn result_type(self) -> Option<TypeId> {
        self.result_type
    }

    /// Returns the sealed stream item type when the entry returns a stream.
    pub const fn stream_item_type(self) -> Option<TypeId> {
        self.stream_item_type
    }
}

const SYS_INSPECT_SNAPSHOT_PARAMETER_TYPES: &[TypeId] = &[
    SYS_INSPECT_INVOCATION_TYPE_ID,
    SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
];
const SYS_INSPECT_PROJECTION_PARAMETER_TYPES: &[TypeId] = &[SYS_INSPECT_SNAPSHOT_TYPE_ID];
const SYS_INSPECT_TRACE_PARAMETER_TYPES: &[TypeId] = &[
    SYS_INSPECT_INVOCATION_TYPE_ID,
    SYS_INSPECT_TRACE_AFTER_SEQUENCE_TYPE_ID,
];

const SYS_INSPECT_SNAPSHOT_SIGNATURE: SystemInspectSignature = SystemInspectSignature::new(
    2,
    SYS_INSPECT_SNAPSHOT_PARAMETER_TYPES,
    Some(SYS_INSPECT_SNAPSHOT_TYPE_ID),
    None,
);
const fn sys_inspect_projection_signature(result_type: TypeId) -> SystemInspectSignature {
    SystemInspectSignature::new(
        1,
        SYS_INSPECT_PROJECTION_PARAMETER_TYPES,
        Some(result_type),
        None,
    )
}

const SYS_INSPECT_INVOCATION_NODES_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_INVOCATION_NODES_TYPE_ID);
const SYS_INSPECT_CALLS_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_CALLS_TYPE_ID);
const SYS_INSPECT_RESOURCES_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_RESOURCES_TYPE_ID);
const SYS_INSPECT_STATE_CELLS_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_STATE_CELLS_TYPE_ID);
const SYS_INSPECT_UI_NODES_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_UI_NODES_TYPE_ID);
const SYS_INSPECT_PRESENTATION_CANDIDATES_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID);
const SYS_INSPECT_RUNTIME_BINDINGS_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID);
const SYS_INSPECT_SECURITY_DECISIONS_SIGNATURE: SystemInspectSignature =
    sys_inspect_projection_signature(SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID);

const SYS_INSPECT_TRACE_SIGNATURE: SystemInspectSignature = SystemInspectSignature::new(
    2,
    SYS_INSPECT_TRACE_PARAMETER_TYPES,
    None,
    Some(SYS_INSPECT_TRACE_EVENT_TYPE_ID),
);

/// The closed result carrier shape of one `sys.security` entry.
///
/// The twelve entries use exactly four shapes, so the result is an enum rather
/// than independent booleans that could combine into a shape no entry has. No
/// entry returns a stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemSecurityResult {
    /// Returns one reference to the sealed `sys.security.principal` carrier.
    Principal,
    /// Returns a set of references to the sealed `sys.security.principal`
    /// carrier.
    PrincipalSet,
    /// Returns a boolean.
    Boolean,
    /// Mutates state and returns nothing.
    Nothing,
}

/// The immutable sealed signature shape of one `sys.security` entry.
///
/// The twelve entries use four closed shapes: the two session identity
/// functions return one reference to the sealed `sys.security.principal`
/// carrier, `active_roles` returns a set of those references, the seven
/// protected admin functions mutate state and return nothing, and the two
/// checks return a boolean. No entry returns a stream. A caller can obtain
/// a signature only from the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemSecuritySignature {
    parameter_count: u8,
    result: SystemSecurityResult,
}

impl SystemSecuritySignature {
    const fn new(parameter_count: u8, result: SystemSecurityResult) -> Self {
        Self {
            parameter_count,
            result,
        }
    }

    /// Returns the fixed sealed parameter count.
    pub const fn parameter_count(self) -> u8 {
        self.parameter_count
    }

    /// Returns the closed result carrier shape of the entry.
    pub const fn result(self) -> SystemSecurityResult {
        self.result
    }
}

const SYS_SECURITY_SESSION_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, SystemSecurityResult::Principal);
const SYS_SECURITY_EFFECTIVE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, SystemSecurityResult::Principal);
const SYS_SECURITY_ACTIVE_ROLES_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, SystemSecurityResult::PrincipalSet);
const SYS_SECURITY_CREATE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, SystemSecurityResult::Nothing);
const SYS_SECURITY_DISABLE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(1, SystemSecurityResult::Nothing);
const SYS_SECURITY_CREATE_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(1, SystemSecurityResult::Nothing);
const SYS_SECURITY_GRANT_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, SystemSecurityResult::Nothing);
const SYS_SECURITY_REVOKE_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, SystemSecurityResult::Nothing);
const SYS_SECURITY_GRANT_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, SystemSecurityResult::Nothing);
const SYS_SECURITY_REVOKE_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, SystemSecurityResult::Nothing);
const SYS_SECURITY_CAN_EXECUTE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, SystemSecurityResult::Boolean);
const SYS_SECURITY_HAS_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, SystemSecurityResult::Boolean);

/// The immutable sealed signature shape of one `sys.state` entry.
///
/// The two entries share one closed shape: a fixed sealed parameter count
/// and a set result, so only the parameter count is carried. `load_user_state`
/// takes three parameters and `write_user_state` takes one; both return a set
/// of sealed carriers. No entry returns a stream. A caller can obtain a
/// signature only from the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemStateSignature {
    parameter_count: u8,
}

impl SystemStateSignature {
    const fn new(parameter_count: u8) -> Self {
        Self { parameter_count }
    }

    /// Returns the fixed sealed parameter count.
    pub const fn parameter_count(self) -> u8 {
        self.parameter_count
    }
}

const SYS_STATE_LOAD_USER_STATE_SIGNATURE: SystemStateSignature = SystemStateSignature::new(3);
const SYS_STATE_WRITE_USER_STATE_SIGNATURE: SystemStateSignature = SystemStateSignature::new(1);

/// The sealed behaviour of one registry entry together with the signature facts
/// its class carries. A class that has no sealed signature carries none, and an
/// entry can never carry a signature of another class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SystemFunctionBehaviour {
    Health,
    Invoke(SystemInvocationSignature),
    SourceIntrospection,
    InspectSnapshot(SystemInspectSignature),
    InspectProjection(SystemInspectSignature),
    InspectTrace(SystemInspectSignature),
    SecurityIdentity(SystemSecuritySignature),
    SecurityAdmin(SystemSecuritySignature),
    SecurityCheck(SystemSecuritySignature),
    State(SystemStateSignature),
}

/// One immutable entry in the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemFunctionDefinition {
    id: FunctionId,
    name_parts: &'static [&'static str],
    behaviour: SystemFunctionBehaviour,
}

impl SystemFunctionDefinition {
    const fn new(
        id: FunctionId,
        name_parts: &'static [&'static str],
        behaviour: SystemFunctionBehaviour,
    ) -> Self {
        Self {
            id,
            name_parts,
            behaviour,
        }
    }

    /// Returns the sealed behaviour selected for this entry.
    pub const fn kind(self) -> SystemFunctionKind {
        match self.behaviour {
            SystemFunctionBehaviour::Health => SystemFunctionKind::Health,
            SystemFunctionBehaviour::Invoke(_) => SystemFunctionKind::Invoke,
            SystemFunctionBehaviour::SourceIntrospection => SystemFunctionKind::SourceIntrospection,
            SystemFunctionBehaviour::InspectSnapshot(_) => SystemFunctionKind::InspectSnapshot,
            SystemFunctionBehaviour::InspectProjection(_) => SystemFunctionKind::InspectProjection,
            SystemFunctionBehaviour::InspectTrace(_) => SystemFunctionKind::InspectTrace,
            SystemFunctionBehaviour::SecurityIdentity(_) => SystemFunctionKind::SecurityIdentity,
            SystemFunctionBehaviour::SecurityAdmin(_) => SystemFunctionKind::SecurityAdmin,
            SystemFunctionBehaviour::SecurityCheck(_) => SystemFunctionKind::SecurityCheck,
            SystemFunctionBehaviour::State(_) => SystemFunctionKind::State,
        }
    }

    /// Returns this system function's stable identity.
    pub const fn id(self) -> FunctionId {
        self.id
    }

    /// Returns the exact resolved qualified-name parts.
    pub const fn name_parts(self) -> &'static [&'static str] {
        self.name_parts
    }

    /// Returns the sealed invocation signature when this entry is `sys.invoke`.
    ///
    /// Other system functions do not have an invocation signature.
    pub const fn invocation_signature(self) -> Option<SystemInvocationSignature> {
        match self.behaviour {
            SystemFunctionBehaviour::Invoke(signature) => Some(signature),
            SystemFunctionBehaviour::Health
            | SystemFunctionBehaviour::SourceIntrospection
            | SystemFunctionBehaviour::InspectSnapshot(_)
            | SystemFunctionBehaviour::InspectProjection(_)
            | SystemFunctionBehaviour::InspectTrace(_)
            | SystemFunctionBehaviour::SecurityIdentity(_)
            | SystemFunctionBehaviour::SecurityAdmin(_)
            | SystemFunctionBehaviour::SecurityCheck(_)
            | SystemFunctionBehaviour::State(_) => None,
        }
    }

    /// Returns the sealed inspect signature when this entry is a `sys.inspect`
    /// function.
    ///
    /// Other system functions do not have an inspect signature.
    pub const fn inspect_signature(self) -> Option<SystemInspectSignature> {
        match self.behaviour {
            SystemFunctionBehaviour::InspectSnapshot(signature)
            | SystemFunctionBehaviour::InspectProjection(signature)
            | SystemFunctionBehaviour::InspectTrace(signature) => Some(signature),
            SystemFunctionBehaviour::Health
            | SystemFunctionBehaviour::Invoke(_)
            | SystemFunctionBehaviour::SourceIntrospection
            | SystemFunctionBehaviour::SecurityIdentity(_)
            | SystemFunctionBehaviour::SecurityAdmin(_)
            | SystemFunctionBehaviour::SecurityCheck(_)
            | SystemFunctionBehaviour::State(_) => None,
        }
    }

    /// Returns the sealed security signature when this entry is a
    /// `sys.security` function.
    ///
    /// Other system functions do not have a security signature.
    pub const fn security_signature(self) -> Option<SystemSecuritySignature> {
        match self.behaviour {
            SystemFunctionBehaviour::SecurityIdentity(signature)
            | SystemFunctionBehaviour::SecurityAdmin(signature)
            | SystemFunctionBehaviour::SecurityCheck(signature) => Some(signature),
            SystemFunctionBehaviour::Health
            | SystemFunctionBehaviour::Invoke(_)
            | SystemFunctionBehaviour::SourceIntrospection
            | SystemFunctionBehaviour::InspectSnapshot(_)
            | SystemFunctionBehaviour::InspectProjection(_)
            | SystemFunctionBehaviour::InspectTrace(_)
            | SystemFunctionBehaviour::State(_) => None,
        }
    }

    /// Returns the sealed state signature when this entry is a `sys.state`
    /// function.
    ///
    /// Other system functions do not have a state signature.
    pub const fn state_signature(self) -> Option<SystemStateSignature> {
        match self.behaviour {
            SystemFunctionBehaviour::State(signature) => Some(signature),
            SystemFunctionBehaviour::Health
            | SystemFunctionBehaviour::Invoke(_)
            | SystemFunctionBehaviour::SourceIntrospection
            | SystemFunctionBehaviour::InspectSnapshot(_)
            | SystemFunctionBehaviour::InspectProjection(_)
            | SystemFunctionBehaviour::InspectTrace(_)
            | SystemFunctionBehaviour::SecurityIdentity(_)
            | SystemFunctionBehaviour::SecurityAdmin(_)
            | SystemFunctionBehaviour::SecurityCheck(_) => None,
        }
    }

    pub(crate) fn has_name(self, name: &QualifiedSemanticName) -> bool {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(self.name_parts.iter().copied())
    }
}

/// Maps every generated sealed variant to its compiled behaviour. There is no
/// wildcard arm: a new catalogue entry stops this crate compiling until bound.
const fn sealed_behaviour(function: EngineFunction) -> SystemFunctionBehaviour {
    match function {
        EngineFunction::CatalogHealth => SystemFunctionBehaviour::Health,
        EngineFunction::Invoke => SystemFunctionBehaviour::Invoke(SYS_INVOKE_SIGNATURE),
        EngineFunction::SourceCurrent => SystemFunctionBehaviour::SourceIntrospection,
        EngineFunction::InspectSnapshot => {
            SystemFunctionBehaviour::InspectSnapshot(SYS_INSPECT_SNAPSHOT_SIGNATURE)
        }
        EngineFunction::InspectInvocationNodes => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_INVOCATION_NODES_SIGNATURE)
        }
        EngineFunction::InspectCalls => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_CALLS_SIGNATURE)
        }
        EngineFunction::InspectResources => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_RESOURCES_SIGNATURE)
        }
        EngineFunction::InspectStateCells => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_STATE_CELLS_SIGNATURE)
        }
        EngineFunction::InspectUiNodes => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_UI_NODES_SIGNATURE)
        }
        EngineFunction::InspectPresentationCandidates => {
            SystemFunctionBehaviour::InspectProjection(
                SYS_INSPECT_PRESENTATION_CANDIDATES_SIGNATURE,
            )
        }
        EngineFunction::InspectRuntimeBindings => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_RUNTIME_BINDINGS_SIGNATURE)
        }
        EngineFunction::InspectSecurityDecisions => {
            SystemFunctionBehaviour::InspectProjection(SYS_INSPECT_SECURITY_DECISIONS_SIGNATURE)
        }
        EngineFunction::InspectTrace => {
            SystemFunctionBehaviour::InspectTrace(SYS_INSPECT_TRACE_SIGNATURE)
        }
        EngineFunction::SecuritySessionPrincipal => {
            SystemFunctionBehaviour::SecurityIdentity(SYS_SECURITY_SESSION_PRINCIPAL_SIGNATURE)
        }
        EngineFunction::SecurityEffectivePrincipal => {
            SystemFunctionBehaviour::SecurityIdentity(SYS_SECURITY_EFFECTIVE_PRINCIPAL_SIGNATURE)
        }
        EngineFunction::SecurityActiveRoles => {
            SystemFunctionBehaviour::SecurityIdentity(SYS_SECURITY_ACTIVE_ROLES_SIGNATURE)
        }
        EngineFunction::SecurityCreatePrincipal => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_CREATE_PRINCIPAL_SIGNATURE)
        }
        EngineFunction::SecurityDisablePrincipal => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_DISABLE_PRINCIPAL_SIGNATURE)
        }
        EngineFunction::SecurityCreateRole => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_CREATE_ROLE_SIGNATURE)
        }
        EngineFunction::SecurityGrantRole => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_GRANT_ROLE_SIGNATURE)
        }
        EngineFunction::SecurityRevokeRole => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_REVOKE_ROLE_SIGNATURE)
        }
        EngineFunction::SecurityGrantPrivilege => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_GRANT_PRIVILEGE_SIGNATURE)
        }
        EngineFunction::SecurityRevokePrivilege => {
            SystemFunctionBehaviour::SecurityAdmin(SYS_SECURITY_REVOKE_PRIVILEGE_SIGNATURE)
        }
        EngineFunction::SecurityCanExecute => {
            SystemFunctionBehaviour::SecurityCheck(SYS_SECURITY_CAN_EXECUTE_SIGNATURE)
        }
        EngineFunction::SecurityHasPrivilege => {
            SystemFunctionBehaviour::SecurityCheck(SYS_SECURITY_HAS_PRIVILEGE_SIGNATURE)
        }
        EngineFunction::StateLoadUserState => {
            SystemFunctionBehaviour::State(SYS_STATE_LOAD_USER_STATE_SIGNATURE)
        }
        EngineFunction::StateWriteUserState => {
            SystemFunctionBehaviour::State(SYS_STATE_WRITE_USER_STATE_SIGNATURE)
        }
    }
}

macro_rules! sealed_function_entry {
    ($variant:ident) => {
        SystemFunctionDefinition::new(
            FunctionId::from_bytes(EngineFunction::$variant.function_id()),
            EngineFunction::$variant.name_parts(),
            sealed_behaviour(EngineFunction::$variant),
        )
    };
}

/// The complete sealed system-function registry in canonical order.
pub const SYSTEM_FUNCTIONS: &[SystemFunctionDefinition] = &[
    sealed_function_entry!(CatalogHealth),
    sealed_function_entry!(Invoke),
    sealed_function_entry!(SourceCurrent),
    sealed_function_entry!(InspectSnapshot),
    sealed_function_entry!(InspectInvocationNodes),
    sealed_function_entry!(InspectCalls),
    sealed_function_entry!(InspectResources),
    sealed_function_entry!(InspectStateCells),
    sealed_function_entry!(InspectUiNodes),
    sealed_function_entry!(InspectPresentationCandidates),
    sealed_function_entry!(InspectRuntimeBindings),
    sealed_function_entry!(InspectSecurityDecisions),
    sealed_function_entry!(InspectTrace),
    sealed_function_entry!(SecuritySessionPrincipal),
    sealed_function_entry!(SecurityEffectivePrincipal),
    sealed_function_entry!(SecurityActiveRoles),
    sealed_function_entry!(SecurityCreatePrincipal),
    sealed_function_entry!(SecurityDisablePrincipal),
    sealed_function_entry!(SecurityCreateRole),
    sealed_function_entry!(SecurityGrantRole),
    sealed_function_entry!(SecurityRevokeRole),
    sealed_function_entry!(SecurityGrantPrivilege),
    sealed_function_entry!(SecurityRevokePrivilege),
    sealed_function_entry!(SecurityCanExecute),
    sealed_function_entry!(SecurityHasPrivilege),
    sealed_function_entry!(StateLoadUserState),
    sealed_function_entry!(StateWriteUserState),
];

/// Resolves one exact sealed system-function identity.
pub fn system_function_by_id(id: FunctionId) -> Option<SystemFunctionDefinition> {
    SYSTEM_FUNCTIONS
        .iter()
        .copied()
        .find(|definition| definition.id == id)
}

/// Resolves one exact sealed system-function name.
pub fn system_function_by_name(name: &QualifiedSemanticName) -> Option<SystemFunctionDefinition> {
    SYSTEM_FUNCTIONS
        .iter()
        .copied()
        .find(|definition| definition.has_name(name))
}

#[cfg(test)]
mod tests {
    use crate::catalogue::QualifiedSemanticName;
    use crate::security::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME};
    use crate::{FunctionId, ParameterId};

    use super::*;

    fn qualified(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn name_of(function: SystemFunctionDefinition) -> QualifiedSemanticName {
        qualified(function.name_parts())
    }

    fn parts_equal(actual: &[&str], expected: &[&str]) -> bool {
        actual.iter().copied().eq(expected.iter().copied())
    }

    #[test]
    fn system_registry_contains_exactly_the_sealed_entries_in_order() {
        assert_eq!(SYSTEM_FUNCTIONS.len(), 27);
        let health = SYSTEM_FUNCTIONS[0];
        assert_eq!(health.kind(), SystemFunctionKind::Health);
        assert_eq!(health.id(), CATALOGUE_HEALTH_FUNCTION_ID);
        assert!(parts_equal(
            health.name_parts(),
            &["sys", "catalog", "health"]
        ));
        let invoke = SYSTEM_FUNCTIONS[1];
        assert_eq!(invoke.kind(), SystemFunctionKind::Invoke);
        assert_eq!(invoke.id(), SYS_INVOKE_FUNCTION_ID);
        assert!(parts_equal(invoke.name_parts(), &["sys", "invoke"]));
        let source = SYSTEM_FUNCTIONS[2];
        assert_eq!(source.kind(), SystemFunctionKind::SourceIntrospection);
        assert_eq!(source.id(), SYS_SOURCE_CURRENT_FUNCTION_ID);
        assert!(parts_equal(
            source.name_parts(),
            &["sys", "source", "current"]
        ));
        let snapshot = SYSTEM_FUNCTIONS[3];
        assert_eq!(snapshot.kind(), SystemFunctionKind::InspectSnapshot);
        assert_eq!(snapshot.id(), SYS_INSPECT_SNAPSHOT_FUNCTION_ID);
        assert!(parts_equal(
            snapshot.name_parts(),
            &["sys", "inspect", "snapshot"]
        ));
        let trace = SYSTEM_FUNCTIONS[12];
        assert_eq!(trace.kind(), SystemFunctionKind::InspectTrace);
        assert_eq!(trace.id(), SYS_INSPECT_TRACE_FUNCTION_ID);
        assert!(parts_equal(
            trace.name_parts(),
            &["sys", "inspect", "trace"]
        ));
        let session_principal = SYSTEM_FUNCTIONS[13];
        assert_eq!(
            session_principal.kind(),
            SystemFunctionKind::SecurityIdentity
        );
        assert_eq!(
            session_principal.id(),
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
        );
        assert!(parts_equal(
            session_principal.name_parts(),
            &["sys", "security", "session_principal"]
        ));
        let has_privilege = SYSTEM_FUNCTIONS[24];
        assert_eq!(has_privilege.kind(), SystemFunctionKind::SecurityCheck);
        assert_eq!(has_privilege.id(), SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID);
        assert!(parts_equal(
            has_privilege.name_parts(),
            &["sys", "security", "has_privilege"]
        ));
        let load_user_state = SYSTEM_FUNCTIONS[25];
        assert_eq!(load_user_state.kind(), SystemFunctionKind::State);
        assert_eq!(load_user_state.id(), SYS_STATE_LOAD_USER_STATE_FUNCTION_ID);
        assert!(parts_equal(
            load_user_state.name_parts(),
            &["sys", "state", "load_user_state"]
        ));
        let write_user_state = SYSTEM_FUNCTIONS[26];
        assert_eq!(write_user_state.kind(), SystemFunctionKind::State);
        assert_eq!(
            write_user_state.id(),
            SYS_STATE_WRITE_USER_STATE_FUNCTION_ID
        );
        assert!(parts_equal(
            write_user_state.name_parts(),
            &["sys", "state", "write_user_state"]
        ));
    }

    #[test]
    fn system_function_lookup_returns_the_same_exact_definitions_by_id_and_name() {
        for &function in SYSTEM_FUNCTIONS {
            let by_id = system_function_by_id(function.id())
                .expect("the registry must resolve its own identity");
            let by_name = system_function_by_name(&name_of(function))
                .expect("the registry must resolve its own name");
            assert_eq!(by_id, function);
            assert_eq!(by_name, function);
            assert_eq!(by_id.kind(), function.kind());
            assert_eq!(by_name.id(), function.id());
        }
    }

    #[test]
    fn sealed_registry_function_ids_are_disjoint_from_the_standard_library() {
        // The sealed registry and the retained standard library share the
        // FunctionId space. A collision would make `security_function_targets`
        // silently filter a standard function as a sealed system function,
        // so the complete-active-function-set proof can never install it.
        // ADR 0065 originally placed the sys.security block at ...0d-...1a,
        // overlapping std.invoke.echo (...0x10), std.json.encode (...0x11),
        // and std.terminal.present_table (...0x12); the block now lives in
        // the documented ...0x40-...0x4b range below.
        let sealed = SYSTEM_FUNCTIONS.iter().map(|function| function.id());
        // The standard crate pins these FunctionIds independently from the
        // system registry:
        // std.invoke.echo = ...0x10, std.json.encode = ...0x11,
        // std.terminal.present_table = ...0x12.
        let standard = [
            FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]),
            FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]),
            FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]),
        ];
        let overlap = sealed
            .filter(|sealed_id| standard.contains(sealed_id))
            .collect::<Vec<_>>();
        assert!(
            overlap.is_empty(),
            "sealed system identities collide with the standard library: {overlap:?}"
        );
    }

    #[test]
    fn system_function_lookup_rejects_similar_prefix_case_unqualified_and_unknown_names() {
        for parts in [
            &["sys", "catalog", "healthx"][..],
            &["sys", "catalog"][..],
            &["Sys", "catalog", "health"][..],
            &["catalog", "health"][..],
            &["sys", "invok"][..],
            &["sys", "inspect"][..],
            &["sys", "inspect", "snapshotx"][..],
            &["sys", "inspect", "Snapshot"][..],
            &["sys", "inspect", "snapshot", "extra"][..],
            &["sys", "inspect", "trace_events"][..],
            &["app", "unknown"][..],
        ] {
            assert!(
                system_function_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
    }

    #[test]
    fn system_function_lookup_rejects_unknown_identities() {
        for bytes in [
            [0; 16],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19],
            [0x7f; 16],
        ] {
            assert!(system_function_by_id(FunctionId::from_bytes(bytes)).is_none());
        }
    }

    #[test]
    fn catalogue_health_compatibility_facts_match_the_registry() {
        assert_eq!(CATALOGUE_HEALTH_FUNCTION_ID, SYSTEM_FUNCTIONS[0].id());
        assert_eq!(CATALOGUE_HEALTH_FUNCTION_NAME, "sys.catalog.health");
        assert_eq!(SYS_INVOKE_FUNCTION_NAME, "sys.invoke");
        assert_eq!(
            SYS_INVOKE_FUNCTION_ID,
            FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2])
        );
        assert_eq!(SYS_INVOKE_FUNCTION_ID, SYSTEM_FUNCTIONS[1].id());
    }

    #[test]
    fn invoke_entry_exposes_the_exact_sealed_request_to_event_stream_signature() {
        let invoke = system_function_by_id(FunctionId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        ]))
        .expect("the literal sys.invoke identity must resolve");
        let signature = invoke
            .invocation_signature()
            .expect("sys.invoke must expose its sealed signature");

        assert_eq!(
            signature.request_parameter_id(),
            ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3])
        );
        assert_eq!(signature.request_parameter_name(), "p_request");
        assert_eq!(
            signature.request_type(),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1])
        );
        assert!(signature.request_is_required());
        assert!(signature.request_is_non_null());
        assert_eq!(
            signature.event_stream_item_type(),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2])
        );
    }

    #[test]
    fn health_entry_exposes_no_invocation_signature() {
        let health = system_function_by_id(FunctionId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ]))
        .expect("the literal health identity must resolve");

        assert!(health.invocation_signature().is_none());
    }

    #[test]
    fn inspect_entries_expose_the_sealed_function_ids_and_names_in_order() {
        let expected = [
            (
                SYS_INSPECT_SNAPSHOT_FUNCTION_ID,
                "sys.inspect.snapshot",
                SystemFunctionKind::InspectSnapshot,
            ),
            (
                SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID,
                "sys.inspect.invocation_nodes",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_CALLS_FUNCTION_ID,
                "sys.inspect.calls",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_RESOURCES_FUNCTION_ID,
                "sys.inspect.resources",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_STATE_CELLS_FUNCTION_ID,
                "sys.inspect.state_cells",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_UI_NODES_FUNCTION_ID,
                "sys.inspect.ui_nodes",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID,
                "sys.inspect.presentation_candidates",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID,
                "sys.inspect.runtime_bindings",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID,
                "sys.inspect.security_decisions",
                SystemFunctionKind::InspectProjection,
            ),
            (
                SYS_INSPECT_TRACE_FUNCTION_ID,
                "sys.inspect.trace",
                SystemFunctionKind::InspectTrace,
            ),
        ];
        for (index, (id, name, kind)) in expected.into_iter().enumerate() {
            let entry = SYSTEM_FUNCTIONS[3 + index];
            assert_eq!(entry.id(), id);
            assert_eq!(entry.kind(), kind);
            assert_eq!(entry.name_parts().join("."), name);
            assert_eq!(system_function_by_id(id), Some(entry));
            let parts: Vec<&str> = name.split('.').collect();
            assert_eq!(system_function_by_name(&qualified(&parts)), Some(entry));
        }
    }

    #[test]
    fn inspect_entries_expose_typed_parameter_and_result_identities() {
        let snapshot = system_function_by_id(SYS_INSPECT_SNAPSHOT_FUNCTION_ID)
            .expect("the snapshot identity must resolve");
        let signature = snapshot
            .inspect_signature()
            .expect("sys.inspect.snapshot must expose its sealed signature");
        assert_eq!(signature.parameter_count(), 2);
        assert_eq!(
            signature.parameter_types(),
            &[
                SYS_INSPECT_INVOCATION_TYPE_ID,
                SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            ]
        );
        assert_eq!(
            signature.parameter_type(0),
            Some(SYS_INSPECT_INVOCATION_TYPE_ID)
        );
        assert_eq!(
            signature.parameter_type(1),
            Some(SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID)
        );
        assert_eq!(signature.parameter_type(2), None);
        assert_eq!(signature.result_type(), Some(SYS_INSPECT_SNAPSHOT_TYPE_ID));
        assert_eq!(signature.stream_item_type(), None);
        assert!(snapshot.invocation_signature().is_none());

        for (id, result_type) in [
            (
                SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID,
                SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            ),
            (SYS_INSPECT_CALLS_FUNCTION_ID, SYS_INSPECT_CALLS_TYPE_ID),
            (
                SYS_INSPECT_RESOURCES_FUNCTION_ID,
                SYS_INSPECT_RESOURCES_TYPE_ID,
            ),
            (
                SYS_INSPECT_STATE_CELLS_FUNCTION_ID,
                SYS_INSPECT_STATE_CELLS_TYPE_ID,
            ),
            (
                SYS_INSPECT_UI_NODES_FUNCTION_ID,
                SYS_INSPECT_UI_NODES_TYPE_ID,
            ),
            (
                SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID,
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            ),
            (
                SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID,
                SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            ),
            (
                SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID,
                SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
            ),
        ] {
            let projection = system_function_by_id(id).expect("a projection identity must resolve");
            let signature = projection
                .inspect_signature()
                .expect("a projection must expose its sealed signature");
            assert_eq!(signature.parameter_count(), 1);
            assert_eq!(signature.parameter_types(), &[SYS_INSPECT_SNAPSHOT_TYPE_ID]);
            assert_eq!(
                signature.parameter_type(0),
                Some(SYS_INSPECT_SNAPSHOT_TYPE_ID)
            );
            assert_eq!(signature.result_type(), Some(result_type));
            assert_eq!(signature.stream_item_type(), None);
            assert!(projection.invocation_signature().is_none());
        }

        let trace = system_function_by_id(SYS_INSPECT_TRACE_FUNCTION_ID)
            .expect("the trace identity must resolve");
        let signature = trace
            .inspect_signature()
            .expect("sys.inspect.trace must expose its sealed signature");
        assert_eq!(signature.parameter_count(), 2);
        assert_eq!(
            signature.parameter_types(),
            &[
                SYS_INSPECT_INVOCATION_TYPE_ID,
                SYS_INSPECT_TRACE_AFTER_SEQUENCE_TYPE_ID,
            ]
        );
        assert_eq!(signature.result_type(), None);
        assert_eq!(
            signature.stream_item_type(),
            Some(SYS_INSPECT_TRACE_EVENT_TYPE_ID)
        );
        assert!(trace.invocation_signature().is_none());

        let health = system_function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
            .expect("the health identity must resolve");
        assert!(health.inspect_signature().is_none());
        let invoke = system_function_by_id(SYS_INVOKE_FUNCTION_ID)
            .expect("the invoke identity must resolve");
        assert!(invoke.inspect_signature().is_none());
    }

    #[test]
    fn security_entries_expose_the_sealed_function_ids_and_names_in_order() {
        let expected = [
            (
                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
                "sys.security.session_principal",
                SystemFunctionKind::SecurityIdentity,
            ),
            (
                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
                "sys.security.effective_principal",
                SystemFunctionKind::SecurityIdentity,
            ),
            (
                SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
                "sys.security.active_roles",
                SystemFunctionKind::SecurityIdentity,
            ),
            (
                SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
                "sys.security.create_principal",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
                "sys.security.disable_principal",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
                "sys.security.create_role",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
                "sys.security.grant_role",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
                "sys.security.revoke_role",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
                "sys.security.grant_privilege",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
                "sys.security.revoke_privilege",
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID,
                "sys.security.can_execute",
                SystemFunctionKind::SecurityCheck,
            ),
            (
                SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID,
                "sys.security.has_privilege",
                SystemFunctionKind::SecurityCheck,
            ),
        ];
        for (index, (id, name, kind)) in expected.into_iter().enumerate() {
            let entry = SYSTEM_FUNCTIONS[13 + index];
            assert_eq!(entry.id(), id);
            assert_eq!(entry.kind(), kind);
            assert_eq!(entry.name_parts().join("."), name);
            assert_eq!(system_function_by_id(id), Some(entry));
            let parts: Vec<&str> = name.split('.').collect();
            assert_eq!(system_function_by_name(&qualified(&parts)), Some(entry));
        }
    }

    #[test]
    fn security_entries_expose_the_closed_signature_shapes() {
        for id in [
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
            SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        ] {
            let identity = system_function_by_id(id).expect("an identity function must resolve");
            let signature = identity
                .security_signature()
                .expect("a security function must expose its sealed signature");
            assert_eq!(signature.parameter_count(), 0);
            assert_eq!(signature.result(), SystemSecurityResult::Principal);
            assert!(identity.invocation_signature().is_none());
            assert!(identity.inspect_signature().is_none());
        }

        let active_roles = system_function_by_id(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID)
            .expect("the active-roles identity must resolve");
        let signature = active_roles
            .security_signature()
            .expect("active_roles must expose its sealed signature");
        assert_eq!(signature.parameter_count(), 0);
        assert_eq!(signature.result(), SystemSecurityResult::PrincipalSet);

        let admin_shapes = [
            (
                SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
                2,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
                1,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
                1,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
                2,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
                2,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
                3,
                SystemFunctionKind::SecurityAdmin,
            ),
            (
                SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
                3,
                SystemFunctionKind::SecurityAdmin,
            ),
        ];
        for (id, parameter_count, kind) in admin_shapes {
            let admin = system_function_by_id(id).expect("an admin identity must resolve");
            assert_eq!(admin.kind(), kind);
            let signature = admin
                .security_signature()
                .expect("an admin function must expose its sealed signature");
            assert_eq!(signature.parameter_count(), parameter_count);
            assert_eq!(signature.result(), SystemSecurityResult::Nothing);
            assert!(admin.invocation_signature().is_none());
            assert!(admin.inspect_signature().is_none());
        }

        let checks = [
            (SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID, 2),
            (SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID, 3),
        ];
        for (id, parameter_count) in checks {
            let check = system_function_by_id(id).expect("a check identity must resolve");
            assert_eq!(check.kind(), SystemFunctionKind::SecurityCheck);
            let signature = check
                .security_signature()
                .expect("a check must expose its sealed signature");
            assert_eq!(signature.parameter_count(), parameter_count);
            assert_eq!(signature.result(), SystemSecurityResult::Boolean);
            assert!(check.invocation_signature().is_none());
            assert!(check.inspect_signature().is_none());
        }

        let health = system_function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
            .expect("the health identity must resolve");
        assert!(health.security_signature().is_none());
        let invoke = system_function_by_id(SYS_INVOKE_FUNCTION_ID)
            .expect("the invoke identity must resolve");
        assert!(invoke.security_signature().is_none());
        let snapshot = system_function_by_id(SYS_INSPECT_SNAPSHOT_FUNCTION_ID)
            .expect("the snapshot identity must resolve");
        assert!(snapshot.security_signature().is_none());
    }

    #[test]
    fn security_lookup_rejects_deferred_and_unregistered_identities() {
        for parts in [
            &["sys", "security", "create_delegation"][..],
            &["sys", "security", "terminate_session"][..],
            &["sys", "security"][..],
            &["sys", "security", "session_principalx"][..],
            &["sys", "security", "SessionPrincipal"][..],
            &["sys", "security", "session_principal", "extra"][..],
            &["security", "session_principal"][..],
        ] {
            assert!(
                system_function_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
        for bytes in [
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1a],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1b],
        ] {
            assert!(system_function_by_id(FunctionId::from_bytes(bytes)).is_none());
        }
    }

    #[test]
    fn security_ids_occupy_the_sealed_function_byte_range_in_order() {
        let ids = [
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
            SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
            SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
            SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
            SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
            SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
            SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
            SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
            SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
            SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
            SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID,
            SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID,
        ];
        for (index, id) in ids.into_iter().enumerate() {
            assert_eq!(id.to_bytes()[15], 0x40 + index as u8);
            assert_eq!(SYSTEM_FUNCTIONS[13 + index].id(), id);
        }
    }

    #[test]
    fn state_entries_expose_the_sealed_function_ids_and_names_in_order() {
        let expected = [
            (
                SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
                "sys.state.load_user_state",
                SystemFunctionKind::State,
            ),
            (
                SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
                "sys.state.write_user_state",
                SystemFunctionKind::State,
            ),
        ];
        for (index, (id, name, kind)) in expected.into_iter().enumerate() {
            let entry = SYSTEM_FUNCTIONS[25 + index];
            assert_eq!(entry.id(), id);
            assert_eq!(entry.kind(), kind);
            assert_eq!(entry.name_parts().join("."), name);
            assert_eq!(system_function_by_id(id), Some(entry));
            let parts: Vec<&str> = name.split('.').collect();
            assert_eq!(system_function_by_name(&qualified(&parts)), Some(entry));
        }
    }

    #[test]
    fn state_entries_expose_the_closed_signature_shapes() {
        let load = system_function_by_id(SYS_STATE_LOAD_USER_STATE_FUNCTION_ID)
            .expect("the load-user-state identity must resolve");
        assert_eq!(load.kind(), SystemFunctionKind::State);
        let load_signature = load
            .state_signature()
            .expect("load_user_state must expose its sealed signature");
        assert_eq!(load_signature.parameter_count(), 3);
        assert!(load.invocation_signature().is_none());
        assert!(load.inspect_signature().is_none());
        assert!(load.security_signature().is_none());

        let write = system_function_by_id(SYS_STATE_WRITE_USER_STATE_FUNCTION_ID)
            .expect("the write-user-state identity must resolve");
        assert_eq!(write.kind(), SystemFunctionKind::State);
        let write_signature = write
            .state_signature()
            .expect("write_user_state must expose its sealed signature");
        assert_eq!(write_signature.parameter_count(), 1);
        assert!(write.invocation_signature().is_none());
        assert!(write.inspect_signature().is_none());
        assert!(write.security_signature().is_none());

        let health = system_function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
            .expect("the health identity must resolve");
        assert!(health.state_signature().is_none());
        let invoke = system_function_by_id(SYS_INVOKE_FUNCTION_ID)
            .expect("the invoke identity must resolve");
        assert!(invoke.state_signature().is_none());
        let snapshot = system_function_by_id(SYS_INSPECT_SNAPSHOT_FUNCTION_ID)
            .expect("the snapshot identity must resolve");
        assert!(snapshot.state_signature().is_none());
        let session_principal = system_function_by_id(SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID)
            .expect("the session-principal identity must resolve");
        assert!(session_principal.state_signature().is_none());
    }

    #[test]
    fn state_ids_occupy_the_sealed_function_byte_range_after_security() {
        let ids = [
            SYS_STATE_LOAD_USER_STATE_FUNCTION_ID,
            SYS_STATE_WRITE_USER_STATE_FUNCTION_ID,
        ];
        for (index, id) in ids.into_iter().enumerate() {
            assert_eq!(id.to_bytes()[15], 0x4c + index as u8);
            assert_eq!(SYSTEM_FUNCTIONS[25 + index].id(), id);
        }
    }

    #[test]
    fn state_lookup_rejects_unregistered_names_and_identities() {
        for parts in [
            &["sys", "state"][..],
            &["sys", "state", "load"][..],
            &["sys", "state", "load_user_states"][..],
            &["sys", "state", "LoadUserState"][..],
            &["sys", "state", "write_user_state", "extra"][..],
            &["state", "load_user_state"][..],
        ] {
            assert!(
                system_function_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
        for bytes in [
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4e],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4f],
        ] {
            assert!(system_function_by_id(FunctionId::from_bytes(bytes)).is_none());
        }
    }

    #[test]
    fn security_carrier_is_sealed_and_collision_free() {
        assert_eq!(
            SYS_SECURITY_PRINCIPAL_TYPE_ID,
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf7])
        );
        assert_eq!(SYS_SECURITY_PRINCIPAL_TYPE_ID.to_bytes()[15], 0xf7);
        assert_eq!(SYS_SECURITY_PRINCIPAL_TYPE_NAME, "sys.security.principal");
        assert_eq!(
            SYS_SECURITY_PRINCIPAL_REPRESENTATION_CONTRACT,
            "orna.sys.security.principal@1"
        );
        assert!(SYS_SECURITY_PRINCIPAL_REPRESENTATION_CONTRACT.ends_with("@1"));
        assert!(
            !INVOCATION_CARRIERS
                .iter()
                .any(|carrier| carrier.id() == SYS_SECURITY_PRINCIPAL_TYPE_ID),
            "the security carrier must not collide with the invocation carriers"
        );
        assert_ne!(
            SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_INSPECT_INVOCATION_TYPE_ID,
            "the security carrier must not collide with the inspect carriers"
        );
        assert_ne!(
            SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_INSPECT_SNAPSHOT_TYPE_ID,
            "the security carrier must not collide with the inspect carriers"
        );
        assert_ne!(
            SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
            "the security carrier must not collide with the inspect carriers"
        );
        assert_ne!(
            SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_INSPECT_TRACE_EVENT_TYPE_ID,
            "the security carrier must not collide with the inspect carriers"
        );
    }

    #[test]
    fn inspect_carrier_identities_are_sealed_and_collision_free() {
        let identities = [
            (
                SYS_INSPECT_INVOCATION_TYPE_ID,
                "sys.inspect.invocation",
                "orna.sys.inspect.invocation@1",
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3]),
            ),
            (
                SYS_INSPECT_SNAPSHOT_TYPE_ID,
                "sys.inspect.snapshot",
                "orna.sys.inspect.snapshot@1",
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf4]),
            ),
            (
                SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
                "sys.inspect.snapshot_options",
                "orna.sys.inspect.snapshot_options@1",
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf5]),
            ),
            (
                SYS_INSPECT_TRACE_EVENT_TYPE_ID,
                "sys.inspect.trace_event",
                "orna.sys.inspect.trace_event@1",
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf6]),
            ),
        ];
        let mut ids = [SYS_INSPECT_INVOCATION_TYPE_ID; 4];
        for (index, (id, name, contract, expected_bytes)) in identities.into_iter().enumerate() {
            ids[index] = id;
            assert_eq!(id, expected_bytes);
            assert_eq!(id.to_bytes()[15], 0xf3 + index as u8);
            assert!(name.starts_with("sys.inspect."));
            assert!(contract.starts_with("orna.sys.inspect."));
            assert!(contract.ends_with("@1"));
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            4,
            "inspect carriers must be mutually distinct"
        );
        assert!(
            !INVOCATION_CARRIERS
                .iter()
                .any(|carrier| ids.contains(&carrier.id())),
            "inspect carriers must not collide with the invocation carriers"
        );
    }

    #[test]
    fn inspect_projection_carrier_identities_are_fixed_and_collision_free() {
        let carriers = [
            (
                SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
                SYS_INSPECT_INVOCATION_NODES_TYPE_NAME,
                SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT,
                0xf8,
            ),
            (
                SYS_INSPECT_CALLS_TYPE_ID,
                SYS_INSPECT_CALLS_TYPE_NAME,
                SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT,
                0xf9,
            ),
            (
                SYS_INSPECT_RESOURCES_TYPE_ID,
                SYS_INSPECT_RESOURCES_TYPE_NAME,
                SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT,
                0xfa,
            ),
            (
                SYS_INSPECT_STATE_CELLS_TYPE_ID,
                SYS_INSPECT_STATE_CELLS_TYPE_NAME,
                SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
                0xfb,
            ),
            (
                SYS_INSPECT_UI_NODES_TYPE_ID,
                SYS_INSPECT_UI_NODES_TYPE_NAME,
                SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT,
                0xfc,
            ),
            (
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_NAME,
                SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
                0xfd,
            ),
            (
                SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
                SYS_INSPECT_RUNTIME_BINDINGS_TYPE_NAME,
                SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
                0xfe,
            ),
            (
                SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
                SYS_INSPECT_SECURITY_DECISIONS_TYPE_NAME,
                SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
                0xff,
            ),
        ];
        let mut ids = [SYS_INSPECT_INVOCATION_NODES_TYPE_ID; 8];
        for (index, (id, name, contract, last_byte)) in carriers.into_iter().enumerate() {
            ids[index] = id;
            assert_eq!(id.to_bytes()[15], last_byte);
            assert!(name.starts_with("sys.inspect."));
            assert!(contract.starts_with("orna.sys.inspect."));
            assert!(contract.ends_with("@1"));
        }
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), carriers.len());
        assert!(!ids.contains(&SYS_SECURITY_PRINCIPAL_TYPE_ID));
        assert!(!ids.iter().any(|id| {
            [
                SYS_INSPECT_INVOCATION_TYPE_ID,
                SYS_INSPECT_SNAPSHOT_TYPE_ID,
                SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
                SYS_INSPECT_TRACE_EVENT_TYPE_ID,
                SYS_SECURITY_PRINCIPAL_TYPE_ID,
            ]
            .contains(id)
        }));
    }

    #[test]
    fn invocation_carrier_registry_contains_the_three_fixed_entries_in_order() {
        assert_eq!(INVOCATION_CARRIERS.len(), 3);
        let expected = [
            (
                InvocationCarrierKind::Value,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]),
                "sys.invoke.Value",
                "orna.sys.invoke.value@1",
            ),
            (
                InvocationCarrierKind::Request,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]),
                "sys.invoke.Request",
                "orna.sys.invoke.request@1",
            ),
            (
                InvocationCarrierKind::Event,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]),
                "sys.invoke.Event",
                "orna.sys.invoke.event@1",
            ),
        ];

        for (definition, (kind, id, name, contract)) in INVOCATION_CARRIERS.iter().zip(expected) {
            assert_eq!(definition.kind(), kind);
            assert_eq!(definition.id(), id);
            assert_eq!(definition.name(), name);
            assert_eq!(definition.representation_contract(), contract);
            assert_eq!(definition.name_parts().join("."), name);
        }
    }

    #[test]
    fn invocation_carrier_lookup_returns_exact_definitions_without_allocating_identities() {
        let expected = [
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]),
                qualified(&["sys", "invoke", "Value"]),
            ),
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]),
                qualified(&["sys", "invoke", "Request"]),
            ),
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]),
                qualified(&["sys", "invoke", "Event"]),
            ),
        ];

        for (index, (id, name)) in expected.into_iter().enumerate() {
            let carrier = INVOCATION_CARRIERS[index];
            assert_eq!(invocation_carrier_by_id(id), Some(carrier));
            assert_eq!(invocation_carrier_by_name(&name), Some(carrier));
        }

        for id in [
            TypeId::from_bytes([0; 16]),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xef]),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3]),
        ] {
            assert_eq!(invocation_carrier_by_id(id), None);
        }
    }

    #[test]
    fn invocation_carrier_lookup_rejects_similar_and_unknown_names() {
        for parts in [
            &["sys", "invoke", "value"][..],
            &["sys", "invoke", "Request", "extra"][..],
            &["sys", "invoke"][..],
            &["invoke", "Event"][..],
            &["app", "Value"][..],
        ] {
            assert!(
                invocation_carrier_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
    }
}
