//! The sealed registry of mandatory ring-1 system functions.
//!
//! Work ADR 0042 defines one compiled registry in `orna-core` for functions
//! that must exist before an application catalogue is available. The registry
//! is not reconstructed from application source, the standard library,
//! PostgreSQL rows, environment values, or configuration. The first two
//! entries are catalogue health and the root invocation gateway.
//!
//! Name comparison uses the complete case-sensitive resolved parts. It does
//! not perform case folding, prefix matching, alias lookup, search-path
//! resolution, or application-catalogue fallback.
//!
//! Application catalogues remain separate from this registry. They cannot
//! replace a sealed definition by reusing its identity or exact name.

use crate::{FunctionId, ParameterId, TypeId, catalogue::QualifiedSemanticName};

/// The stable identity of the sealed `sys.catalog.health` function.
pub const CATALOGUE_HEALTH_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// The exact resolved name of the sealed catalogue-health function.
pub const CATALOGUE_HEALTH_FUNCTION_NAME: &str = "sys.catalog.health";

/// The stable identity of the mandatory sealed `sys.invoke` function.
pub const SYS_INVOKE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

/// The exact resolved name of the mandatory invocation gateway.
pub const SYS_INVOKE_FUNCTION_NAME: &str = "sys.invoke";

/// The stable identity of the sealed `sys.inspect.snapshot` function.
pub const SYS_INSPECT_SNAPSHOT_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);

/// The exact resolved name of the sealed inspect-snapshot function.
pub const SYS_INSPECT_SNAPSHOT_FUNCTION_NAME: &str = "sys.inspect.snapshot";

/// The stable identity of the sealed `sys.inspect.invocation_nodes` function.
pub const SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04]);

/// The exact resolved name of the sealed invocation-nodes projection.
pub const SYS_INSPECT_INVOCATION_NODES_FUNCTION_NAME: &str = "sys.inspect.invocation_nodes";

/// The stable identity of the sealed `sys.inspect.calls` function.
pub const SYS_INSPECT_CALLS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);

/// The exact resolved name of the sealed calls projection.
pub const SYS_INSPECT_CALLS_FUNCTION_NAME: &str = "sys.inspect.calls";

/// The stable identity of the sealed `sys.inspect.resources` function.
pub const SYS_INSPECT_RESOURCES_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);

/// The exact resolved name of the sealed resources projection.
pub const SYS_INSPECT_RESOURCES_FUNCTION_NAME: &str = "sys.inspect.resources";

/// The stable identity of the sealed `sys.inspect.state_cells` function.
pub const SYS_INSPECT_STATE_CELLS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x07]);

/// The exact resolved name of the sealed state-cells projection.
pub const SYS_INSPECT_STATE_CELLS_FUNCTION_NAME: &str = "sys.inspect.state_cells";

/// The stable identity of the sealed `sys.inspect.ui_nodes` function.
pub const SYS_INSPECT_UI_NODES_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08]);

/// The exact resolved name of the sealed ui-nodes projection.
pub const SYS_INSPECT_UI_NODES_FUNCTION_NAME: &str = "sys.inspect.ui_nodes";

/// The stable identity of the sealed `sys.inspect.presentation_candidates` function.
pub const SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09]);

/// The exact resolved name of the sealed presentation-candidates projection.
pub const SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_NAME: &str =
    "sys.inspect.presentation_candidates";

/// The stable identity of the sealed `sys.inspect.runtime_bindings` function.
pub const SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0a]);

/// The exact resolved name of the sealed runtime-bindings projection.
pub const SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_NAME: &str = "sys.inspect.runtime_bindings";

/// The stable identity of the sealed `sys.inspect.security_decisions` function.
pub const SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0b]);

/// The exact resolved name of the sealed security-decisions projection.
pub const SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_NAME: &str = "sys.inspect.security_decisions";

/// The stable identity of the sealed `sys.inspect.trace` function.
pub const SYS_INSPECT_TRACE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c]);

/// The exact resolved name of the sealed trace function.
pub const SYS_INSPECT_TRACE_FUNCTION_NAME: &str = "sys.inspect.trace";

/// The stable identity of the sealed `sys.security.session_principal` function.
pub const SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40]);

/// The exact resolved name of the sealed session-principal function.
pub const SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_NAME: &str = "sys.security.session_principal";

/// The stable identity of the sealed `sys.security.effective_principal` function.
pub const SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x41]);

/// The exact resolved name of the sealed effective-principal function.
pub const SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_NAME: &str = "sys.security.effective_principal";

/// The stable identity of the sealed `sys.security.active_roles` function.
pub const SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x42]);

/// The exact resolved name of the sealed active-roles function.
pub const SYS_SECURITY_ACTIVE_ROLES_FUNCTION_NAME: &str = "sys.security.active_roles";

/// The stable identity of the sealed `sys.security.create_principal` function.
pub const SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x43]);

/// The exact resolved name of the sealed create-principal function.
pub const SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_NAME: &str = "sys.security.create_principal";

/// The stable identity of the sealed `sys.security.disable_principal` function.
pub const SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x44]);

/// The exact resolved name of the sealed disable-principal function.
pub const SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_NAME: &str = "sys.security.disable_principal";

/// The stable identity of the sealed `sys.security.create_role` function.
pub const SYS_SECURITY_CREATE_ROLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x45]);

/// The exact resolved name of the sealed create-role function.
pub const SYS_SECURITY_CREATE_ROLE_FUNCTION_NAME: &str = "sys.security.create_role";

/// The stable identity of the sealed `sys.security.grant_role` function.
pub const SYS_SECURITY_GRANT_ROLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x46]);

/// The exact resolved name of the sealed grant-role function.
pub const SYS_SECURITY_GRANT_ROLE_FUNCTION_NAME: &str = "sys.security.grant_role";

/// The stable identity of the sealed `sys.security.revoke_role` function.
pub const SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x47]);

/// The exact resolved name of the sealed revoke-role function.
pub const SYS_SECURITY_REVOKE_ROLE_FUNCTION_NAME: &str = "sys.security.revoke_role";

/// The stable identity of the sealed `sys.security.grant_privilege` function.
pub const SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x48]);

/// The exact resolved name of the sealed grant-privilege function.
pub const SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_NAME: &str = "sys.security.grant_privilege";

/// The stable identity of the sealed `sys.security.revoke_privilege` function.
pub const SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49]);

/// The exact resolved name of the sealed revoke-privilege function.
pub const SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_NAME: &str = "sys.security.revoke_privilege";

/// The stable identity of the sealed `sys.security.can_execute` function.
pub const SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4a]);

/// The exact resolved name of the sealed can-execute check.
pub const SYS_SECURITY_CAN_EXECUTE_FUNCTION_NAME: &str = "sys.security.can_execute";

/// The stable identity of the sealed `sys.security.has_privilege` function.
pub const SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4b]);

/// The exact resolved name of the sealed has-privilege check.
pub const SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_NAME: &str = "sys.security.has_privilege";

/// The stable identity of the sole sealed `sys.invoke` parameter.
pub const SYS_INVOKE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);

/// The exact name of the sole sealed `sys.invoke` parameter.
pub const SYS_INVOKE_PARAMETER_NAME: &str = "p_request";

/// The stable identity of the sealed `sys.invoke.Value` carrier.
pub const SYS_INVOKE_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]);

/// The exact semantic name of the sealed invocation-value carrier.
pub const SYS_INVOKE_VALUE_TYPE_NAME: &str = "sys.invoke.Value";

/// The immutable representation contract of the sealed invocation-value carrier.
pub const SYS_INVOKE_VALUE_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.value@1";

/// The stable identity of the sealed `sys.invoke.Request` carrier.
pub const SYS_INVOKE_REQUEST_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]);

/// The exact semantic name of the sealed invocation-request carrier.
pub const SYS_INVOKE_REQUEST_TYPE_NAME: &str = "sys.invoke.Request";

/// The immutable representation contract of the sealed invocation-request carrier.
pub const SYS_INVOKE_REQUEST_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.request@1";

/// The stable identity of the sealed `sys.invoke.Event` carrier.
pub const SYS_INVOKE_EVENT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]);

/// The exact semantic name of the sealed invocation-event carrier.
pub const SYS_INVOKE_EVENT_TYPE_NAME: &str = "sys.invoke.Event";

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

/// The stable identity of the sealed `sys.security.principal` carrier.
pub const SYS_SECURITY_PRINCIPAL_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf7]);

/// The exact semantic name of the sealed security-principal carrier.
pub const SYS_SECURITY_PRINCIPAL_TYPE_NAME: &str = "sys.security.principal";

/// The immutable representation contract of the sealed security-principal carrier.
pub const SYS_SECURITY_PRINCIPAL_REPRESENTATION_CONTRACT: &str = "orna.sys.security.principal@1";

const CATALOGUE_HEALTH_NAME_PARTS: &[&str] = &["sys", "catalog", "health"];
const SYS_INVOKE_NAME_PARTS: &[&str] = &["sys", "invoke"];
const SYS_INVOKE_VALUE_NAME_PARTS: &[&str] = &["sys", "invoke", "Value"];
const SYS_INVOKE_REQUEST_NAME_PARTS: &[&str] = &["sys", "invoke", "Request"];
const SYS_INVOKE_EVENT_NAME_PARTS: &[&str] = &["sys", "invoke", "Event"];
const SYS_INSPECT_SNAPSHOT_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "snapshot"];
const SYS_INSPECT_INVOCATION_NODES_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "inspect", "invocation_nodes"];
const SYS_INSPECT_CALLS_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "calls"];
const SYS_INSPECT_RESOURCES_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "resources"];
const SYS_INSPECT_STATE_CELLS_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "state_cells"];
const SYS_INSPECT_UI_NODES_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "ui_nodes"];
const SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "inspect", "presentation_candidates"];
const SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "inspect", "runtime_bindings"];
const SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "inspect", "security_decisions"];
const SYS_INSPECT_TRACE_FUNCTION_NAME_PARTS: &[&str] = &["sys", "inspect", "trace"];
const SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "session_principal"];
const SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "effective_principal"];
const SYS_SECURITY_ACTIVE_ROLES_FUNCTION_NAME_PARTS: &[&str] = &["sys", "security", "active_roles"];
const SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "create_principal"];
const SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "disable_principal"];
const SYS_SECURITY_CREATE_ROLE_FUNCTION_NAME_PARTS: &[&str] = &["sys", "security", "create_role"];
const SYS_SECURITY_GRANT_ROLE_FUNCTION_NAME_PARTS: &[&str] = &["sys", "security", "grant_role"];
const SYS_SECURITY_REVOKE_ROLE_FUNCTION_NAME_PARTS: &[&str] = &["sys", "security", "revoke_role"];
const SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "grant_privilege"];
const SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "revoke_privilege"];
const SYS_SECURITY_CAN_EXECUTE_FUNCTION_NAME_PARTS: &[&str] = &["sys", "security", "can_execute"];
const SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_NAME_PARTS: &[&str] =
    &["sys", "security", "has_privilege"];

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
    /// Reads one session identity fact from a bound authenticated session.
    SecurityIdentity,
    /// Mutates protected principal, role, or privilege state.
    SecurityAdmin,
    /// Checks one execute grant or privilege class against the snapshot.
    SecurityCheck,
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
/// The ten entries use three closed shapes: snapshot capture (two sealed
/// parameters and no result stream), the eight projections (one sealed
/// snapshot parameter and no result stream), and the trace stream (two
/// sealed parameters and one sealed stream item type). A caller can obtain a
/// signature only from the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemInspectSignature {
    parameter_count: u8,
    stream_item_type: Option<TypeId>,
}

impl SystemInspectSignature {
    const fn new(parameter_count: u8, stream_item_type: Option<TypeId>) -> Self {
        Self {
            parameter_count,
            stream_item_type,
        }
    }

    /// Returns the fixed sealed parameter count.
    pub const fn parameter_count(self) -> u8 {
        self.parameter_count
    }

    /// Returns the sealed stream item type when the entry returns a stream.
    pub const fn stream_item_type(self) -> Option<TypeId> {
        self.stream_item_type
    }
}

const SYS_INSPECT_SNAPSHOT_SIGNATURE: SystemInspectSignature = SystemInspectSignature::new(2, None);
const SYS_INSPECT_PROJECTION_SIGNATURE: SystemInspectSignature =
    SystemInspectSignature::new(1, None);
const SYS_INSPECT_TRACE_SIGNATURE: SystemInspectSignature =
    SystemInspectSignature::new(2, Some(SYS_INSPECT_TRACE_EVENT_TYPE_ID));

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
    returns_set: bool,
    returns_ref_principal: bool,
    returns_boolean: bool,
    stream_item_type: Option<TypeId>,
}

impl SystemSecuritySignature {
    const fn new(
        parameter_count: u8,
        returns_set: bool,
        returns_ref_principal: bool,
        returns_boolean: bool,
        stream_item_type: Option<TypeId>,
    ) -> Self {
        Self {
            parameter_count,
            returns_set,
            returns_ref_principal,
            returns_boolean,
            stream_item_type,
        }
    }

    /// Returns the fixed sealed parameter count.
    pub const fn parameter_count(self) -> u8 {
        self.parameter_count
    }

    /// Returns whether the entry returns a set of values.
    pub const fn returns_set(self) -> bool {
        self.returns_set
    }

    /// Returns whether the entry returns a reference to the sealed
    /// `sys.security.principal` carrier.
    pub const fn returns_ref_principal(self) -> bool {
        self.returns_ref_principal
    }

    /// Returns whether the entry returns a boolean.
    pub const fn returns_boolean(self) -> bool {
        self.returns_boolean
    }

    /// Returns the sealed stream item type when the entry returns a stream.
    pub const fn stream_item_type(self) -> Option<TypeId> {
        self.stream_item_type
    }
}

const SYS_SECURITY_SESSION_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, false, true, false, None);
const SYS_SECURITY_EFFECTIVE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, false, true, false, None);
const SYS_SECURITY_ACTIVE_ROLES_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(0, true, true, false, None);
const SYS_SECURITY_CREATE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, false, false, false, None);
const SYS_SECURITY_DISABLE_PRINCIPAL_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(1, false, false, false, None);
const SYS_SECURITY_CREATE_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(1, false, false, false, None);
const SYS_SECURITY_GRANT_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, false, false, false, None);
const SYS_SECURITY_REVOKE_ROLE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, false, false, false, None);
const SYS_SECURITY_GRANT_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, false, false, false, None);
const SYS_SECURITY_REVOKE_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, false, false, false, None);
const SYS_SECURITY_CAN_EXECUTE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(2, false, false, true, None);
const SYS_SECURITY_HAS_PRIVILEGE_SIGNATURE: SystemSecuritySignature =
    SystemSecuritySignature::new(3, false, false, true, None);

/// One immutable entry in the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemFunctionDefinition {
    kind: SystemFunctionKind,
    id: FunctionId,
    name_parts: &'static [&'static str],
    invocation_signature: Option<SystemInvocationSignature>,
    inspect_signature: Option<SystemInspectSignature>,
    security_signature: Option<SystemSecuritySignature>,
}

impl SystemFunctionDefinition {
    const fn new(
        kind: SystemFunctionKind,
        id: FunctionId,
        name_parts: &'static [&'static str],
        invocation_signature: Option<SystemInvocationSignature>,
        inspect_signature: Option<SystemInspectSignature>,
        security_signature: Option<SystemSecuritySignature>,
    ) -> Self {
        Self {
            kind,
            id,
            name_parts,
            invocation_signature,
            inspect_signature,
            security_signature,
        }
    }

    /// Returns the sealed behaviour selected for this entry.
    pub const fn kind(self) -> SystemFunctionKind {
        self.kind
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
        self.invocation_signature
    }

    /// Returns the sealed inspect signature when this entry is a `sys.inspect`
    /// function.
    ///
    /// Other system functions do not have an inspect signature.
    pub const fn inspect_signature(self) -> Option<SystemInspectSignature> {
        self.inspect_signature
    }

    /// Returns the sealed security signature when this entry is a
    /// `sys.security` function.
    ///
    /// Other system functions do not have a security signature.
    pub const fn security_signature(self) -> Option<SystemSecuritySignature> {
        self.security_signature
    }

    pub(crate) fn has_name(self, name: &QualifiedSemanticName) -> bool {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(self.name_parts.iter().copied())
    }
}

/// The complete sealed system-function registry in canonical order.
pub const SYSTEM_FUNCTIONS: &[SystemFunctionDefinition] = &[
    SystemFunctionDefinition::new(
        SystemFunctionKind::Health,
        CATALOGUE_HEALTH_FUNCTION_ID,
        CATALOGUE_HEALTH_NAME_PARTS,
        None,
        None,
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::Invoke,
        SYS_INVOKE_FUNCTION_ID,
        SYS_INVOKE_NAME_PARTS,
        Some(SYS_INVOKE_SIGNATURE),
        None,
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectSnapshot,
        SYS_INSPECT_SNAPSHOT_FUNCTION_ID,
        SYS_INSPECT_SNAPSHOT_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_SNAPSHOT_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID,
        SYS_INSPECT_INVOCATION_NODES_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_CALLS_FUNCTION_ID,
        SYS_INSPECT_CALLS_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_RESOURCES_FUNCTION_ID,
        SYS_INSPECT_RESOURCES_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_STATE_CELLS_FUNCTION_ID,
        SYS_INSPECT_STATE_CELLS_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_UI_NODES_FUNCTION_ID,
        SYS_INSPECT_UI_NODES_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID,
        SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID,
        SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectProjection,
        SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID,
        SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_PROJECTION_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::InspectTrace,
        SYS_INSPECT_TRACE_FUNCTION_ID,
        SYS_INSPECT_TRACE_FUNCTION_NAME_PARTS,
        None,
        Some(SYS_INSPECT_TRACE_SIGNATURE),
        None,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityIdentity,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_SESSION_PRINCIPAL_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityIdentity,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_EFFECTIVE_PRINCIPAL_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityIdentity,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_ACTIVE_ROLES_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_CREATE_PRINCIPAL_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_DISABLE_PRINCIPAL_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        SYS_SECURITY_CREATE_ROLE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_CREATE_ROLE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
        SYS_SECURITY_GRANT_ROLE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_GRANT_ROLE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_REVOKE_ROLE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_GRANT_PRIVILEGE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityAdmin,
        SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_REVOKE_PRIVILEGE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityCheck,
        SYS_SECURITY_CAN_EXECUTE_FUNCTION_ID,
        SYS_SECURITY_CAN_EXECUTE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_CAN_EXECUTE_SIGNATURE),
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::SecurityCheck,
        SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_NAME_PARTS,
        None,
        None,
        Some(SYS_SECURITY_HAS_PRIVILEGE_SIGNATURE),
    ),
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
    fn system_registry_contains_exactly_the_twenty_four_sealed_entries_in_order() {
        assert_eq!(SYSTEM_FUNCTIONS.len(), 24);
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
        let snapshot = SYSTEM_FUNCTIONS[2];
        assert_eq!(snapshot.kind(), SystemFunctionKind::InspectSnapshot);
        assert_eq!(snapshot.id(), SYS_INSPECT_SNAPSHOT_FUNCTION_ID);
        assert!(parts_equal(
            snapshot.name_parts(),
            &["sys", "inspect", "snapshot"]
        ));
        let trace = SYSTEM_FUNCTIONS[11];
        assert_eq!(trace.kind(), SystemFunctionKind::InspectTrace);
        assert_eq!(trace.id(), SYS_INSPECT_TRACE_FUNCTION_ID);
        assert!(parts_equal(
            trace.name_parts(),
            &["sys", "inspect", "trace"]
        ));
        let session_principal = SYSTEM_FUNCTIONS[12];
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
        let has_privilege = SYSTEM_FUNCTIONS[23];
        assert_eq!(has_privilege.kind(), SystemFunctionKind::SecurityCheck);
        assert_eq!(has_privilege.id(), SYS_SECURITY_HAS_PRIVILEGE_FUNCTION_ID);
        assert!(parts_equal(
            has_privilege.name_parts(),
            &["sys", "security", "has_privilege"]
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
        // The retained standard library pins these FunctionIds in
        // `orna-compiler` (which cannot be imported from `orna-core`):
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
            let entry = SYSTEM_FUNCTIONS[2 + index];
            assert_eq!(entry.id(), id);
            assert_eq!(entry.kind(), kind);
            assert_eq!(entry.name_parts().join("."), name);
            assert_eq!(system_function_by_id(id), Some(entry));
            let parts: Vec<&str> = name.split('.').collect();
            assert_eq!(system_function_by_name(&qualified(&parts)), Some(entry));
        }
    }

    #[test]
    fn inspect_entries_expose_the_closed_signature_shapes() {
        let snapshot = system_function_by_id(SYS_INSPECT_SNAPSHOT_FUNCTION_ID)
            .expect("the snapshot identity must resolve");
        let signature = snapshot
            .inspect_signature()
            .expect("sys.inspect.snapshot must expose its sealed signature");
        assert_eq!(signature.parameter_count(), 2);
        assert_eq!(signature.stream_item_type(), None);
        assert!(snapshot.invocation_signature().is_none());

        for id in [
            SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID,
            SYS_INSPECT_CALLS_FUNCTION_ID,
            SYS_INSPECT_RESOURCES_FUNCTION_ID,
            SYS_INSPECT_STATE_CELLS_FUNCTION_ID,
            SYS_INSPECT_UI_NODES_FUNCTION_ID,
            SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID,
            SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID,
            SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID,
        ] {
            let projection = system_function_by_id(id).expect("a projection identity must resolve");
            let signature = projection
                .inspect_signature()
                .expect("a projection must expose its sealed signature");
            assert_eq!(signature.parameter_count(), 1);
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
            let entry = SYSTEM_FUNCTIONS[12 + index];
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
            assert!(signature.returns_ref_principal());
            assert!(!signature.returns_set());
            assert!(!signature.returns_boolean());
            assert_eq!(signature.stream_item_type(), None);
            assert!(identity.invocation_signature().is_none());
            assert!(identity.inspect_signature().is_none());
        }

        let active_roles = system_function_by_id(SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID)
            .expect("the active-roles identity must resolve");
        let signature = active_roles
            .security_signature()
            .expect("active_roles must expose its sealed signature");
        assert_eq!(signature.parameter_count(), 0);
        assert!(signature.returns_set());
        assert!(signature.returns_ref_principal());
        assert!(!signature.returns_boolean());
        assert_eq!(signature.stream_item_type(), None);

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
            assert!(!signature.returns_ref_principal());
            assert!(!signature.returns_set());
            assert!(!signature.returns_boolean());
            assert_eq!(signature.stream_item_type(), None);
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
            assert!(signature.returns_boolean());
            assert!(!signature.returns_ref_principal());
            assert!(!signature.returns_set());
            assert_eq!(signature.stream_item_type(), None);
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
            assert_eq!(SYSTEM_FUNCTIONS[12 + index].id(), id);
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
