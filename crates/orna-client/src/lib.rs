//! Local evaluation for closed CLIENT functions.

use orna_protocol::{
    ClientFrame, MAX_RESOURCE_ARGUMENTS, MAX_RESOURCE_BATCH_ITEMS, MAX_RESOURCE_TOTAL_ITEMS,
    decode_active_value, decode_constructed_value, encode_active_client_frame, encode_active_value,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};

use orna_artifact::client_plan::{
    ActionClientPlan, ActionTargetDomain, CAPABILITY_FORMAT_VERSION, CapabilityArgumentSource,
    CapabilityClientPlan, ClientExpressionNode, ClientLocal, ClientLocalKind, ClientPlan,
    ClientPlanError, ControlFlowBinaryOperator, ControlFlowClientPlan, ControlFlowStatement,
    ControlFlowUnaryOperator, EXPRESSION_FORMAT_VERSION, ExpressionClientPlan, FORMAT_IDENTITY,
    FORMAT_VERSION, InnerClientPlan, InspectOperationNode, InspectProjection,
    LANGUAGE_VERSION_IDENTITY, OPAQUE_FORMAT_VERSION, OpaqueClientPlan, PROCEDURAL_FORMAT_VERSION,
    ProceduralClientPlan, RESOURCE_FORMAT_VERSION, ResourceClientPlan, ResourceKind,
    ResourceOperationNode, STATE_FORMAT_VERSION, StateClientPlan, StateDefault, StateScope,
};
use orna_core::{
    CallSiteId, FieldId, FunctionId, FunctionRevisionId, InvocationId, LocalId, ObjectId,
    ParameterId, PrincipalId, StateSlotId, TypeId,
    canonical_hash::{CanonicalHashError, artifact_payload_digest, catalogue_digest_with_context},
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionVolatility,
        TypeDefinition, ValueTypeDefinition, ValueTypeKind,
    },
    inspect::{
        INSPECT_RENDER_CARRIER_SIGNATURE, INSPECT_RENDER_CONTRACT, stable_inspect_error_code,
    },
    inspect_carrier::{InspectCarrierEnvelope, InspectCarrierKind},
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, StandardExecutable,
        VerifiedStandardLibrarySnapshot,
    },
    security::{AuthenticatedSessionBinding, AuthorisedInvocation, InvocationTarget, TargetClass},
    state::{
        UserStateCell, UserStateChange, UserStateKeyWithoutPrincipal, UserStateWriteOutcome,
        UserStateWriteResult,
    },
    system::{
        SYS_INSPECT_CALLS_TYPE_ID, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        SYS_INSPECT_INVOCATION_TYPE_ID, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        SYS_INSPECT_RESOURCES_TYPE_ID, SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID, SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
        SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_STATE_CELLS_TYPE_ID,
        SYS_INSPECT_UI_NODES_TYPE_ID, SYS_SOURCE_FUNCTION_TYPE_ID,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    value::{ConstructedValueKind, FunctionArgument, OpaqueValue, OpaqueValueError, RuntimeValue},
};
/// The fixed amount of execution fuel granted to each root CLIENT
/// evaluation.  A plan cannot increase or disable this limit.
pub const DEFAULT_CLIENT_EXECUTION_FUEL: u64 = 100_000;
/// The maximum command size accepted by the native session evaluator.
const MAX_CLIENT_COMMAND_BYTES: usize = 16 * 1024;
/// The largest number of queued stream items retained by one CLIENT resource.
/// This matches the largest single completion batch, keeping one broker-sized
/// batch available while requiring consumption before another batch is retained.
const MAX_RESOURCE_QUEUED_ITEMS: u64 = MAX_RESOURCE_BATCH_ITEMS as u64;
// The V10 evaluator carries a large closed error state through its recursive
// expression helpers. Grow a temporary segment before a nested call so the
// artifact depth limit can return its structured error instead of relying on
// the caller's platform-specific thread stack size.
const CLIENT_RECURSION_STACK_RED_ZONE: usize = 1024 * 1024;
const CLIENT_RECURSION_STACK_SEGMENT: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientExecutionFuel {
    remaining: u64,
}

impl ClientExecutionFuel {
    fn new() -> Self {
        Self {
            remaining: DEFAULT_CLIENT_EXECUTION_FUEL,
        }
    }

    fn consume(
        &mut self,
        context: ClientExecutionContext,
    ) -> Result<(), Box<ClientExecutionError>> {
        if self.remaining == 0 {
            return Err(Box::new(execution::expression_error(
                context,
                ClientExpressionError::ExecutionLimit,
            )));
        }
        self.remaining -= 1;
        Ok(())
    }
}

use orna_standard::{
    ACTION_MAGIC, BINARY_LARGE_OBJECT_TYPE_ID, RegisteredOpaqueCodecsError,
    STANDARD_CATALOGUE_V9_REVISION_ID, STANDARD_CATALOGUE_V10_REVISION_ID,
    STANDARD_LIBRARY_V9_REVISION_ID, STANDARD_LIBRARY_V10_REVISION_ID, STD_ACTION_TYPE_ID,
    STD_UI_BUTTON_ENABLED_PARAMETER_ID, STD_UI_BUTTON_FUNCTION_ID,
    STD_UI_BUTTON_FUNCTION_REVISION_ID, STD_UI_BUTTON_LABEL_PARAMETER_ID,
    STD_UI_BUTTON_RUNTIME_CONTRACT, STD_UI_COLUMN_CONTENT_PARAMETER_ID, STD_UI_COLUMN_FUNCTION_ID,
    STD_UI_COLUMN_FUNCTION_REVISION_ID, STD_UI_COLUMN_RUNTIME_CONTRACT,
    STD_UI_PANEL_CONTENT_PARAMETER_ID, STD_UI_PANEL_FUNCTION_ID, STD_UI_PANEL_FUNCTION_REVISION_ID,
    STD_UI_PANEL_RUNTIME_CONTRACT, STD_UI_ROW_CONTENT_PARAMETER_ID, STD_UI_ROW_FUNCTION_ID,
    STD_UI_ROW_FUNCTION_REVISION_ID, STD_UI_ROW_RUNTIME_CONTRACT, STD_UI_TABS_CONTENT_PARAMETER_ID,
    STD_UI_TABS_FUNCTION_ID, STD_UI_TABS_FUNCTION_REVISION_ID, STD_UI_TABS_RUNTIME_CONTRACT,
    STD_UI_TEXT_FUNCTION_ID, STD_UI_TEXT_FUNCTION_REVISION_ID,
    STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID, STD_UI_TEXT_INPUT_FUNCTION_ID,
    STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID, STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
    STD_UI_TEXT_INPUT_RUNTIME_CONTRACT, STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
    STD_UI_TEXT_PARAMETER_ID, STD_UI_TEXT_RUNTIME_CONTRACT, STD_UI_TYPE_ID, UI_MAGIC,
    registered_opaque_codecs,
};

mod execution;
pub use execution::{
    ClientActiveRevisionError, ClientArtifactIntegrityError, ClientExecutionError,
    ClientExecutionRule, ClientExpressionError, ClientOpaqueValueError,
    ClientResourceExecutionError, ClientStateError, cancel_client_action_with_executor,
    complete_client_action, decode_action_payload, encode_action_payload, evaluate_client_function,
    evaluate_client_function_in_state_context,
    evaluate_client_function_in_state_context_with_grants_and_arguments,
    evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation,
    evaluate_client_function_with_arguments, evaluate_client_function_with_arguments_and_executor,
    evaluate_client_function_with_executor, evaluate_client_function_with_grants,
    evaluate_client_function_with_grants_and_arguments, evaluate_client_function_with_state,
    evaluate_client_function_with_state_and_arguments,
    evaluate_client_function_with_state_and_grants,
    evaluate_client_function_with_state_and_grants_and_arguments,
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor,
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation,
    trigger_client_action, validate_client_artifact_integrity,
};

pub mod capability;
pub mod connection;
pub mod endpoint;
pub mod inspect_lifecycle;
pub mod inspect_session;
pub mod runtime_adapter;
pub mod runtime_loader;
pub mod session;
pub mod vm;
pub use connection::{InvocationConnection, InvocationConnectionError};
pub use endpoint::{DEFAULT_REMOTE_PORT, DatabaseEndpoint, EndpointParseError};
pub use session::{TerminalSessionDriver, TerminalSessionDriverError};

pub use runtime_adapter::{QtRuntimeExecutor, RuntimeActionBinding};

pub use runtime_loader::{
    AbiActionEvent, AbiBindAction, AbiBytesView, AbiChildOperation, AbiClientApi,
    AbiContractVersion, AbiDescriptor, AbiDiagnosticEvent, AbiEventKind, AbiLayoutStateEvent,
    AbiModelChildrenRequest, AbiModelRangeRequest, AbiMountNode, AbiOwnedBytes, AbiRuntimeApi,
    AbiRuntimeCreateOptions, AbiRuntimeEvent, AbiRuntimeEventArgs, AbiRuntimeFeature,
    AbiSetProperty, AbiSinkOffer, AbiStatus, AbiStatusCode, AbiStringView, AbiSurfaceClosedEvent,
    AbiSurfaceCreateOptions, AbiSurfaceHandle, AbiThreadModel, AbiUiBatch, AbiUiOperation,
    AbiUiOperationKind, AbiValueRef, CLIENT_MAX_QUEUED_RUNTIME_EVENT_BYTES,
    CLIENT_MAX_QUEUED_RUNTIME_EVENTS, CLIENT_MAX_RUNTIME_BATCH_OPERATIONS,
    CLIENT_MAX_RUNTIME_CONFIGURATION_BYTES, CLIENT_MAX_RUNTIME_TEXT_BYTES,
    CLIENT_MAX_RUNTIME_VALUE_BYTES, RuntimeActionEvent, RuntimeContract, RuntimeDescriptor,
    RuntimeDiagnosticEvent, RuntimeDiagnosticEventSnapshot, RuntimeEvent, RuntimeEventSnapshot,
    RuntimeLibrary, RuntimeLoadError, RuntimeSession, RuntimeSessionError, RuntimeSink,
    RuntimeSurfaceClosedEvent, RuntimeSurfaceClosedEventSnapshot, RuntimeSurfaceOptions,
    RuntimeUiBatch, RuntimeUiOperation, RuntimeValueInput, RuntimeValueSnapshot,
};

pub use inspect_lifecycle::{
    ClientInspectLifecycle, ClientInspectLifecycleState, InspectEpochBinding, InspectFreezeToken,
    InspectLifecycleError, InspectProjectionVersions,
};
pub use inspect_session::{
    ClientInspectLifecycleCompletion, ClientInspectLifecycleRequest, ClientInspectLifecycleSession,
};

/// The active revision, function revision, and root invocation selected for
/// one CLIENT execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientExecutionContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
    parent_invocation_id: InvocationId,
    observer_lineage: Option<ObserverLineage>,
}

/// Internal observer provenance for one active CLIENT invocation.
///
/// The public accessors retain their existing API. A private compatibility
/// carrier preserves the bounded observer chain across returned contexts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObserverLineage {
    root: InvocationId,
    parent: InvocationId,
    current: InvocationId,
    ancestors: [InvocationId; orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1],
    ancestor_len: usize,
}

impl ObserverLineage {
    fn top_level(invocation: InvocationId) -> Self {
        let mut ancestors = [InvocationId::from_bytes([0; 16]);
            orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1];
        ancestors[0] = invocation;
        Self {
            root: invocation,
            parent: invocation,
            current: invocation,
            ancestors,
            ancestor_len: 1,
        }
    }

    fn nested(mut self) -> Self {
        let current = InvocationId::new();
        self.parent = self.current;
        self.current = current;
        if self.ancestor_len < self.ancestors.len() {
            self.ancestors[self.ancestor_len] = current;
            self.ancestor_len += 1;
        }
        self
    }

    fn with_current(mut self, current: InvocationId) -> Self {
        self.parent = self.current;
        self.current = current;
        if self.ancestor_len < self.ancestors.len() {
            self.ancestors[self.ancestor_len] = current;
            self.ancestor_len += 1;
        }
        self
    }

    #[cfg(test)]
    fn with_parent_and_current(mut self, parent: InvocationId, current: InvocationId) -> Self {
        self.parent = parent;
        self.current = current;
        self.ancestors[1] = parent;
        self.ancestors[2] = current;
        self.ancestor_len = 3;
        self
    }

    fn contains(&self, target: InvocationId) -> bool {
        self.ancestors[..self.ancestor_len].contains(&target) || target == self.current
    }

    fn compatibility(context: ClientExecutionContext) -> Self {
        context
            .observer_lineage
            .unwrap_or_else(|| Self::top_level(context.parent_invocation_id()))
    }
}

/// The client-side anchor for one Inspector execution.
///
/// Inspector carriers retain the server epoch in their stable ORNA-INSPECT
/// envelope. The client epoch is deliberately a different identity: it is
/// derived from the enclosing invocation and is carried on the typed provider
/// request rather than being added to the wire envelope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClientEpochId(InvocationId);

impl ClientEpochId {
    /// Binds one client epoch to its enclosing invocation identity.
    pub const fn from_parent_invocation(parent: InvocationId) -> Self {
        Self(parent)
    }

    /// Returns the enclosing invocation used as the client epoch anchor.
    pub const fn invocation_id(self) -> InvocationId {
        self.0
    }
}

impl ClientExecutionContext {
    /// Returns the active source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the selected function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the selected immutable function revision identity.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.function_revision
    }

    /// Returns the root invocation identity used by resource requests.
    pub const fn parent_invocation_id(&self) -> InvocationId {
        self.parent_invocation_id
    }

    /// Returns the observer lineage carried by this execution context.
    /// Contexts built by older callers have no embedded lineage and retain
    /// the single-anchor compatibility behaviour.
    fn observer_lineage(&self) -> ObserverLineage {
        self.observer_lineage
            .unwrap_or_else(|| ObserverLineage::top_level(self.parent_invocation_id))
    }

    /// Returns the trusted observer root invocation anchor used by Inspector.
    pub const fn observer_root_invocation_id(&self) -> InvocationId {
        match self.observer_lineage {
            Some(lineage) => lineage.root,
            None => self.parent_invocation_id,
        }
    }

    /// Returns the trusted observer parent invocation anchor used by Inspector.
    pub const fn observer_parent_invocation_id(&self) -> InvocationId {
        match self.observer_lineage {
            Some(lineage) => lineage.current,
            None => self.parent_invocation_id,
        }
    }

    /// Returns the distinct client-side Inspector epoch anchor.
    pub const fn client_epoch_id(&self) -> ClientEpochId {
        ClientEpochId::from_parent_invocation(self.parent_invocation_id)
    }
}

/// The result of one closed CLIENT function evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientExecutionResult {
    context: ClientExecutionContext,
    value: RuntimeValue,
}

impl ClientExecutionResult {
    /// Returns the active revision and function revision used for this result.
    pub const fn context(&self) -> &ClientExecutionContext {
        &self.context
    }

    /// Returns the evaluated typed runtime value.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }

    /// Transfers the evaluated value without cloning its payload.
    pub fn into_value(self) -> RuntimeValue {
        self.value
    }
}

/// Authority-free call descriptor carried by a transient std.action.Action.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientActionDescriptor {
    domain: ActionTargetDomain,
    target: FunctionId,
    target_revision: RevisionPair,
    call_site: CallSiteId,
    result_type: TypeId,
    arguments: Vec<FunctionArgument>,
}
impl ClientActionDescriptor {
    pub fn new(
        domain: ActionTargetDomain,
        target: FunctionId,
        target_revision: RevisionPair,
        call_site: CallSiteId,
        arguments: Vec<FunctionArgument>,
        result_type: TypeId,
    ) -> Self {
        Self {
            domain,
            target,
            target_revision,
            call_site,
            result_type,
            arguments,
        }
    }
    pub const fn domain(&self) -> ActionTargetDomain {
        self.domain
    }
    pub const fn target(&self) -> FunctionId {
        self.target
    }
    pub const fn target_revision(&self) -> RevisionPair {
        self.target_revision
    }
    pub const fn call_site(&self) -> CallSiteId {
        self.call_site
    }
    pub const fn result_type(&self) -> TypeId {
        self.result_type
    }
    pub fn arguments(&self) -> &[FunctionArgument] {
        &self.arguments
    }
    // ClientActionError preserves its public diagnostic layout and wire-facing variants.
    #[allow(clippy::result_large_err)]
    pub fn encode_payload(
        &self,
        active: &ActiveDatabaseRevision,
    ) -> Result<Vec<u8>, ClientActionError> {
        encode_action_payload(active, self)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientActionOutcome {
    Completed,
    Failed { code: String },
    Cancelled,
}

const ACTION_FAILURE_CODE: &str = "action.failed";
const EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE: &str = "external_contract.runtime_unavailable";
// Keep action framing within the shared opaque codec payload ceiling.
const MAX_ACTION_PAYLOAD_LENGTH: usize = 16 * 1024 * 1024;

/// Caller-owned lifecycle state for one SERVER action trigger.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientActionState {
    resource: Option<ClientResource>,
    request: Option<ClientResourceRequest>,
    tombstone: ClientResourceGeneration,
    invocation_id: Option<InvocationId>,
}

impl Default for ClientActionState {
    fn default() -> Self {
        Self {
            resource: None,
            request: None,
            tombstone: ClientResourceGeneration(0),
            invocation_id: None,
        }
    }
}
impl ClientActionState {
    pub fn status(&self) -> ClientResourceStatus {
        self.resource
            .as_ref()
            .map_or(ClientResourceStatus::Idle, ClientResource::status)
    }
    /// Returns the active request generation.
    pub fn generation(&self) -> Option<ClientResourceGeneration> {
        self.resource.as_ref().map(ClientResource::generation)
    }
    /// Returns the fresh identity assigned to the currently staged action.
    pub fn invocation_id(&self) -> Option<InvocationId> {
        self.invocation_id
    }
    fn resource_mut(&mut self) -> Option<&mut ClientResource> {
        self.resource.as_mut()
    }
    fn set_resource(&mut self, resource: ClientResource) {
        if resource.generation().value() > self.tombstone.value() {
            self.tombstone = resource.generation();
        }
        self.resource = Some(resource);
    }
    fn stage_request(&mut self, request: ClientResourceRequest) {
        self.request = Some(request);
    }
    fn stage_invocation(&mut self, invocation_id: InvocationId) {
        self.invocation_id = Some(invocation_id);
    }
    fn clear(&mut self) {
        if let Some(resource) = self.resource.take()
            && resource.generation().value() > self.tombstone.value()
        {
            self.tombstone = resource.generation();
        }
        self.request = None;
        self.invocation_id = None;
    }
    fn is_stale(&self, generation: ClientResourceGeneration) -> bool {
        generation.value() <= self.tombstone.value()
    }
}
fn redacted_action_failure() -> ClientActionOutcome {
    ClientActionOutcome::Failed {
        code: ACTION_FAILURE_CODE.to_owned(),
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientActionError {
    InvalidValue,
    InvalidPayload(String),
    Registry(String),
    RevisionMismatch,
    TargetMismatch,
    ResultTypeMismatch,
    Arguments(Box<ClientResourceError>),
    Pending,
    StaleCompletion,
    Executor(String),
    /// The executor still owns a nested resource request after cancellation
    /// could not complete synchronously.
    ///
    /// The caller owns the retained resource in `ClientStateStore` and can use
    /// this identity to match a later executor completion or release attempt.
    ExecutorPending {
        code: String,
        request_id: InvocationId,
        key: ClientResourceKey,
        generation: ClientResourceGeneration,
    },
    Evaluation(String),
}
impl fmt::Display for ClientActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue => f.write_str("the CLIENT action value is invalid"),
            Self::InvalidPayload(m) => write!(f, "the CLIENT action payload is invalid: {m}"),
            Self::Registry(m) => write!(f, "the CLIENT action codec registry is invalid: {m}"),
            Self::RevisionMismatch => {
                f.write_str("the CLIENT action target revision is not active")
            }
            Self::TargetMismatch => f.write_str("the CLIENT action target is invalid"),
            Self::ResultTypeMismatch => f.write_str("the CLIENT action result type is invalid"),
            Self::Arguments(e) => e.fmt(f),
            Self::Pending => f.write_str("the CLIENT action remains pending"),
            Self::StaleCompletion => f.write_str("the CLIENT action completion is stale"),
            Self::Executor(m) => write!(f, "the CLIENT action executor failed: {m}"),
            Self::ExecutorPending { code, .. } => {
                write!(
                    f,
                    "the CLIENT action executor retains a pending resource: {code}"
                )
            }
            Self::Evaluation(m) => write!(f, "the CLIENT action target failed evaluation: {m}"),
        }
    }
}
impl Error for ClientActionError {}
/// The cache identity for one CLIENT resource request.
///
/// All four components are part of the cache boundary. A resource result must
/// not cross a principal, pinned revision, argument set, or invalidation epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientResourceKey {
    target: InvocationTarget,
    principal: PrincipalId,
    arguments_digest: Sha256Digest,
    /// Combined active-catalogue, host-data, and security-context digest.
    invalidation_token: Sha256Digest,
}

impl ClientResourceKey {
    /// Creates one principal- and revision-scoped resource identity.
    pub const fn new(
        target: InvocationTarget,
        principal: PrincipalId,
        arguments_digest: Sha256Digest,
        invalidation_token: Sha256Digest,
    ) -> Self {
        Self {
            target,
            principal,
            arguments_digest,
            invalidation_token,
        }
    }
    /// Calculates the canonical digest used by a resource key.
    pub fn canonical_arguments_digest(
        active: &ActiveDatabaseRevision,
        arguments: &[FunctionArgument],
    ) -> Result<Sha256Digest, ClientResourceError> {
        let arguments = canonical_resource_arguments(arguments)?;
        canonical_resource_argument_digest(active, &arguments)
    }

    /// Returns the pinned invocation target.
    pub const fn target(self) -> InvocationTarget {
        self.target
    }

    /// Returns the principal that owns the result.
    pub const fn principal(self) -> PrincipalId {
        self.principal
    }

    /// Returns the canonical typed argument digest.
    pub const fn arguments_digest(self) -> Sha256Digest {
        self.arguments_digest
    }

    /// Returns the local catalogue/data/security invalidation identity.
    pub const fn invalidation_token(self) -> Sha256Digest {
        self.invalidation_token
    }

    /// Matches the logical operation slot while ignoring the revision pair
    /// and invalidation token that make a complete key replacement.
    fn replacement_slot_matches(self, other: Self) -> bool {
        self.target.function() == other.target.function()
            && self.target.class() == other.target.class()
            && self.target.standard_revision() == other.target.standard_revision()
            && self.target.executable_revision() == other.target.executable_revision()
            && self.principal == other.principal
            && self.arguments_digest == other.arguments_digest
    }
}

impl Hash for ClientResourceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.target.function().hash(state);
        self.target.revision().hash(state);
        self.target.class().hash(state);
        self.target.standard_revision().hash(state);
        self.target.executable_revision().hash(state);
        self.principal.hash(state);
        self.arguments_digest.hash(state);
        self.invalidation_token.hash(state);
    }
}

/// The externally visible lifecycle state of one CLIENT resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientResourceStatus {
    /// No request generation is active.
    Idle,
    /// The current generation is waiting for its executor result.
    Loading,
    /// The current generation has one type-checked value.
    Ready,
    /// The current generation ended with a structured failure code.
    Failed,
    /// The current generation was cancelled before completion.
    Cancelled,
}

/// A monotonically increasing CLIENT resource request generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClientResourceGeneration(u64);

impl ClientResourceGeneration {
    /// Returns the durable generation number.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// A structured failure recorded by a CLIENT resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientResourceFailure {
    code: String,
}

impl ClientResourceFailure {
    /// Returns the stable failure code.
    pub fn code(&self) -> &str {
        &self.code
    }
}
/// Errors that leave a CLIENT resource unchanged.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientResourceError {
    /// The generation counter cannot advance safely.
    GenerationExhausted,
    /// A completion belongs to an older or unknown generation.
    StaleGeneration {
        /// The generation currently owned by the resource.
        expected: ClientResourceGeneration,
        /// The generation supplied by the executor.
        actual: ClientResourceGeneration,
    },
    /// The executor could not release a pending request.
    Executor(String),
    /// The operation is not valid while the resource has this status.
    InvalidTransition {
        /// The current resource status.
        status: ClientResourceStatus,
    },
    /// The completion was evaluated against a different active revision pair.
    RevisionMismatch {
        /// The revision pair pinned by the resource key.
        expected: RevisionPair,
        /// The revision pair supplied for publication.
        actual: RevisionPair,
    },
    /// The complete invocation target is not present in this active revision.
    TargetMismatch {
        /// The target pinned by the resource key.
        expected: InvocationTarget,
    },
    /// An argument value could not be encoded by the active ORV3 codec.
    ArgumentEncoding,
    /// A resource request contains more arguments than the transport accepts.
    ResourceArgumentLimitExceeded {
        /// The maximum number of arguments accepted by one request.
        limit: usize,
    },
    /// One parameter identity occurred more than once in a request.
    DuplicateArgument {
        /// The repeated parameter identity.
        parameter: ParameterId,
    },
    /// A target parameter is absent from a request.
    MissingArgument {
        /// The required parameter identity.
        parameter: ParameterId,
    },
    /// A request contains a parameter that the target does not declare.
    UnknownArgument {
        /// The undeclared parameter identity.
        parameter: ParameterId,
    },
    /// The request arguments do not match the resource key digest.
    ArgumentDigestMismatch {
        /// The digest retained by the resource key.
        expected: Sha256Digest,
        /// The digest calculated from the request arguments.
        actual: Sha256Digest,
    },
    /// The completion belongs to a different resource identity.
    RequestKeyMismatch {
        /// The key owned by the resource.
        expected: Box<ClientResourceKey>,
        /// The key carried by the completion.
        actual: Box<ClientResourceKey>,
    },
    /// The completion belongs to a different request instance.
    RequestIdMismatch {
        /// The request identity currently owned by the resource.
        expected: InvocationId,
        /// The request identity carried by the completion.
        actual: InvocationId,
    },
    /// A failure code is empty or contains a forbidden NUL byte.
    InvalidFailureCode,
    /// The result does not match the declared resolved type.
    TypeMismatch,
    /// The invocation context contains a zero lineage identity or a NUL byte
    /// in its state profile or function-instance key.
    InvalidInvocationContext,
}

impl fmt::Display for ClientResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationExhausted => {
                formatter.write_str("CLIENT resource generation exhausted")
            }
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "CLIENT resource completion has generation {}, expected {}",
                actual.value(),
                expected.value(),
            ),
            Self::Executor(message) => {
                write!(formatter, "CLIENT resource executor failed: {message}")
            }
            Self::InvalidTransition { status } => {
                write!(
                    formatter,
                    "CLIENT resource operation is invalid in {status:?} state"
                )
            }
            Self::RevisionMismatch { expected, actual } => write!(
                formatter,
                "CLIENT resource completion uses revision {:?}, expected {:?}",
                actual, expected,
            ),
            Self::TargetMismatch { expected } => write!(
                formatter,
                "CLIENT resource target {:?} is not present in the active revision",
                expected,
            ),
            Self::ArgumentEncoding => formatter
                .write_str("CLIENT resource argument cannot be encoded by the active codec"),
            Self::ResourceArgumentLimitExceeded { limit } => write!(
                formatter,
                "CLIENT resource argument count exceeds the limit {limit}"
            ),
            Self::DuplicateArgument { parameter } => {
                write!(
                    formatter,
                    "CLIENT resource argument repeats parameter {parameter}"
                )
            }
            Self::MissingArgument { parameter } => {
                write!(
                    formatter,
                    "CLIENT resource request is missing target parameter {parameter}"
                )
            }
            Self::UnknownArgument { parameter } => {
                write!(
                    formatter,
                    "CLIENT resource request contains unknown target parameter {parameter}"
                )
            }
            Self::ArgumentDigestMismatch { expected, actual } => write!(
                formatter,
                "CLIENT resource argument digest {:?} does not match expected {:?}",
                actual, expected,
            ),
            Self::RequestKeyMismatch { expected, actual } => write!(
                formatter,
                "CLIENT resource completion uses key {:?}, expected {:?}",
                actual, expected,
            ),
            Self::RequestIdMismatch { expected, actual } => write!(
                formatter,
                "CLIENT resource completion uses request {:?}, expected {:?}",
                actual, expected,
            ),
            Self::InvalidFailureCode => {
                formatter.write_str("CLIENT resource failure code must be non-empty and NUL-free")
            }
            Self::TypeMismatch => {
                formatter.write_str("CLIENT resource result does not match its type")
            }
            Self::InvalidInvocationContext => formatter.write_str(
                "CLIENT resource invocation context must use non-zero identities and valid NUL-free text",
            ),
        }
    }
}

impl Error for ClientResourceError {}

/// The invocation identity that a CLIENT resource request uses for server
/// correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientResourceInvocationContext {
    parent_invocation_id: InvocationId,
    call_site_id: CallSiteId,
    state_profile: String,
    function_instance_key: String,
}

impl ClientResourceInvocationContext {
    /// Creates one resource request invocation context.
    pub fn new(
        parent_invocation_id: InvocationId,
        call_site_id: CallSiteId,
        state_profile: String,
        function_instance_key: String,
    ) -> Self {
        Self {
            parent_invocation_id,
            call_site_id,
            state_profile,
            function_instance_key,
        }
    }

    /// Returns the enclosing invocation identity.
    pub const fn parent_invocation_id(&self) -> InvocationId {
        self.parent_invocation_id
    }

    /// Returns the compiled resource call-site identity.
    pub const fn call_site_id(&self) -> CallSiteId {
        self.call_site_id
    }

    /// Returns the inherited root state profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the inherited root function-instance key.
    pub fn function_instance_key(&self) -> &str {
        &self.function_instance_key
    }
}

/// One request submitted to a CLIENT resource executor.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientResourceRequest {
    request_id: InvocationId,
    key: ClientResourceKey,
    generation: ClientResourceGeneration,
    kind: ResourceKind,
    expected_type: ResolvedType,
    arguments: Vec<FunctionArgument>,
    invocation_context: Option<ClientResourceInvocationContext>,
}

impl ClientResourceRequest {
    fn new(
        active: &ActiveDatabaseRevision,
        key: ClientResourceKey,
        generation: ClientResourceGeneration,
        kind: ResourceKind,
        expected_type: ResolvedType,
        arguments: Vec<FunctionArgument>,
        invocation_context: Option<ClientResourceInvocationContext>,
    ) -> Result<Self, ClientResourceError> {
        if invocation_context.as_ref().is_some_and(|context| {
            context.parent_invocation_id == InvocationId::from_bytes([0; 16])
                || context.call_site_id == CallSiteId::from_bytes([0; 16])
                || context.state_profile.as_bytes().contains(&0)
                || context.function_instance_key.as_bytes().contains(&0)
        }) {
            return Err(ClientResourceError::InvalidInvocationContext);
        }
        if active.pair() != key.target().revision() {
            return Err(ClientResourceError::RevisionMismatch {
                expected: key.target().revision(),
                actual: active.pair(),
            });
        }
        if !execution::active_supports_invocation_target(active, key.target()) {
            return Err(ClientResourceError::TargetMismatch {
                expected: key.target(),
            });
        }
        if !execution::active_resource_result_type_matches(
            active,
            key.target(),
            kind,
            expected_type,
        ) {
            return Err(ClientResourceError::TypeMismatch);
        }
        let arguments = validate_resource_arguments(active, key.target(), &arguments)?;
        let actual = canonical_resource_argument_digest(active, &arguments)?;
        if actual != key.arguments_digest() {
            return Err(ClientResourceError::ArgumentDigestMismatch {
                expected: key.arguments_digest(),
                actual,
            });
        }
        Ok(Self {
            request_id: InvocationId::new(),
            key,
            generation,
            kind,
            expected_type,
            arguments,
            invocation_context,
        })
    }

    /// Returns the fresh request identity used for transport correlation.
    pub const fn request_id(&self) -> InvocationId {
        self.request_id
    }

    /// Returns the complete resource identity carried by this request.
    pub const fn key(&self) -> ClientResourceKey {
        self.key
    }

    /// Returns the request generation.
    pub const fn generation(&self) -> ClientResourceGeneration {
        self.generation
    }

    /// Returns the pinned invocation target.
    pub const fn target(&self) -> InvocationTarget {
        self.key.target()
    }

    /// Returns whether this request produces one scalar or streamed batches.
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the expected result item type.
    pub const fn expected_type(&self) -> ResolvedType {
        self.expected_type
    }

    /// Returns the canonical parameter-ordered arguments.
    pub fn arguments(&self) -> &[FunctionArgument] {
        &self.arguments
    }

    /// Returns the invocation context when the evaluator supplied one.
    pub fn invocation_context(&self) -> Option<ClientResourceInvocationContext> {
        self.invocation_context.clone()
    }

    /// Creates a successful completion for this request.
    pub fn ready(self, value: RuntimeValue) -> ClientResourceCompletion {
        ClientResourceCompletion::Ready {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
            value,
        }
    }
    /// Creates a non-terminal completion for this request.
    pub fn pending(self) -> ClientResourceCompletion {
        ClientResourceCompletion::Pending {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
        }
    }

    /// Creates one non-terminal stream value batch for this request.
    pub fn stream_values(self, values: Vec<RuntimeValue>) -> ClientResourceCompletion {
        ClientResourceCompletion::StreamValues {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
            values,
        }
    }

    /// Creates the successful terminal completion for a stream request.
    pub fn stream_completed(self) -> ClientResourceCompletion {
        ClientResourceCompletion::StreamCompleted {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
        }
    }

    /// Creates a failed completion for this request.
    pub fn failed(self, code: String) -> ClientResourceCompletion {
        ClientResourceCompletion::Failed {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
            code,
        }
    }
    /// Creates a cancelled completion for this request.
    pub fn cancelled(self) -> ClientResourceCompletion {
        ClientResourceCompletion::Cancelled {
            request_id: self.request_id,
            key: self.key,
            generation: self.generation,
        }
    }
}

/// One result returned by a CLIENT resource executor.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientResourceCompletion {
    /// One typed value produced by the request.
    Ready {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The request generation.
        generation: ClientResourceGeneration,
        /// The returned runtime value.
        value: RuntimeValue,
    },
    /// One non-terminal batch produced by a stream request.
    StreamValues {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The active request generation.
        generation: ClientResourceGeneration,
        /// The non-empty batch of typed stream items.
        values: Vec<RuntimeValue>,
    },
    /// Successful terminal completion for a stream request.
    StreamCompleted {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The active request generation.
        generation: ClientResourceGeneration,
    },
    /// The request remains active and will complete through a later call.
    Pending {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The active request generation.
        generation: ClientResourceGeneration,
    },
    /// One structured failure produced by the request.
    Failed {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The request generation.
        generation: ClientResourceGeneration,
        /// The stable failure code.
        code: String,
    },
    /// One terminal cancellation produced by the request.
    Cancelled {
        /// The unique request identity that produced this completion.
        request_id: InvocationId,
        /// The resource identity that created the request.
        key: ClientResourceKey,
        /// The request generation.
        generation: ClientResourceGeneration,
    },
}
impl ClientResourceCompletion {
    /// Returns the complete request identity carried by this completion.
    fn identity(&self) -> (InvocationId, ClientResourceKey, ClientResourceGeneration) {
        match self {
            Self::Ready {
                request_id,
                key,
                generation,
                ..
            }
            | Self::StreamValues {
                request_id,
                key,
                generation,
                ..
            }
            | Self::StreamCompleted {
                request_id,
                key,
                generation,
            }
            | Self::Pending {
                request_id,
                key,
                generation,
            }
            | Self::Failed {
                request_id,
                key,
                generation,
                ..
            }
            | Self::Cancelled {
                request_id,
                key,
                generation,
            } => (*request_id, *key, *generation),
        }
    }

    /// Returns the request identity that produced this completion.
    pub const fn request_id(&self) -> InvocationId {
        match self {
            Self::Ready { request_id, .. }
            | Self::StreamValues { request_id, .. }
            | Self::StreamCompleted { request_id, .. }
            | Self::Pending { request_id, .. }
            | Self::Failed { request_id, .. }
            | Self::Cancelled { request_id, .. } => *request_id,
        }
    }

    /// Returns whether this completion belongs to the supplied request.
    fn matches_request(&self, request: &ClientResourceRequest) -> bool {
        let (request_id, key, generation) = self.identity();
        request_id == request.request_id()
            && key == request.key()
            && generation == request.generation()
    }
}

/// One sealed Inspector operation evaluated by the local CLIENT runtime.
///
/// The operation carries only already-evaluated, typed arguments. It never
/// carries caller-supplied authority; the provider receives the enclosing
/// [`ClientExecutionContext`] separately through [`ClientInspectRequest`].
#[derive(Clone, Debug, PartialEq)]
pub enum ClientInspectOperation {
    /// Capture one immutable structural snapshot of an invocation target.
    Snapshot { target: RuntimeValue },
    /// Materialise one bounded projection from a snapshot.
    Projection {
        projection: InspectProjection,
        snapshot: RuntimeValue,
    },
}

impl ClientInspectOperation {
    /// Returns the invocation target for a snapshot operation.
    pub fn target(&self) -> Option<&RuntimeValue> {
        match self {
            Self::Snapshot { target } => Some(target),
            Self::Projection { .. } => None,
        }
    }

    /// Returns the sealed carrier tag for a projection operation (2 through 9).
    ///
    /// The carrier tags are distinct from client-plan projection tags and let
    /// server adapters remain independent of the artifact crate.
    pub const fn projection_carrier_tag(&self) -> Option<u8> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Projection { projection, .. } => Some(match projection {
                InspectProjection::InvocationNodes => 2,
                InspectProjection::Calls => 3,
                InspectProjection::Resources => 4,
                InspectProjection::StateCells => 5,
                InspectProjection::UiNodes => 6,
                InspectProjection::PresentationCandidates => 7,
                InspectProjection::RuntimeBindings => 8,
                InspectProjection::SecurityDecisions => 9,
            }),
        }
    }

    /// Returns the projection selector for a projection operation.
    pub const fn projection(&self) -> Option<InspectProjection> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Projection { projection, .. } => Some(*projection),
        }
    }

    /// Returns the snapshot argument for a projection operation.
    pub fn snapshot(&self) -> Option<&RuntimeValue> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Projection { snapshot, .. } => Some(snapshot),
        }
    }
}

/// One typed request submitted to the installed CLIENT Inspector provider.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientInspectRequest {
    operation: ClientInspectOperation,
    context: ClientExecutionContext,
    client_epoch_id: ClientEpochId,
    observer_root_invocation_id: InvocationId,
    observer_parent_invocation_id: InvocationId,
    observer_lineage: [InvocationId; orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1],
    observer_lineage_len: usize,
    target_invocation_id: Option<InvocationId>,
    snapshot_options: Option<RuntimeValue>,
}

impl ClientInspectRequest {
    /// Creates a request bound to one CLIENT execution context.
    ///
    /// Snapshot operations derive their target from the operation. Projection
    /// callers must use [`Self::projection`] so the decoded snapshot target is
    /// carried into lifecycle binding instead of being silently omitted.
    pub fn new(context: ClientExecutionContext, operation: ClientInspectOperation) -> Self {
        let target_invocation_id = operation
            .target()
            .and_then(execution::inspect_invocation_target);
        Self::with_provenance(
            context,
            operation,
            target_invocation_id,
            None,
            ObserverLineage::compatibility(context),
        )
    }

    /// Creates a checked projection request from a snapshot and its target.
    ///
    /// Projection operations do not repeat the target reference in their
    /// operation payload. Callers must therefore carry the invocation identity
    /// proven when the snapshot was captured. A zero identity is rejected
    /// before the request can enter a lifecycle session; the session performs
    /// the remaining epoch and revision binding checks.
    pub fn projection(
        context: ClientExecutionContext,
        projection: InspectProjection,
        snapshot: RuntimeValue,
        target_invocation_id: InvocationId,
    ) -> Result<Self, ClientInspectError> {
        if target_invocation_id.to_bytes() == [0; 16] {
            return Err(ClientInspectError::InvalidTarget);
        }
        Ok(Self::with_target_invocation(
            context,
            ClientInspectOperation::Projection {
                projection,
                snapshot,
            },
            target_invocation_id,
            ObserverLineage::compatibility(context),
        ))
    }

    /// Creates a request with a target identity recovered from canonical snapshot evidence.
    fn with_target_invocation(
        context: ClientExecutionContext,
        operation: ClientInspectOperation,
        target_invocation_id: InvocationId,
        lineage: ObserverLineage,
    ) -> Self {
        Self::with_provenance(
            context,
            operation,
            Some(target_invocation_id),
            None,
            lineage,
        )
    }

    /// Creates a request carrying the checked snapshot-options value.
    fn with_target_invocation_and_options(
        context: ClientExecutionContext,
        operation: ClientInspectOperation,
        target_invocation_id: InvocationId,
        snapshot_options: RuntimeValue,
        lineage: ObserverLineage,
    ) -> Self {
        Self::with_provenance(
            context,
            operation,
            Some(target_invocation_id),
            Some(snapshot_options),
            lineage,
        )
    }

    fn with_provenance(
        context: ClientExecutionContext,
        operation: ClientInspectOperation,
        target_invocation_id: Option<InvocationId>,
        snapshot_options: Option<RuntimeValue>,
        lineage: ObserverLineage,
    ) -> Self {
        Self {
            operation,
            context,
            client_epoch_id: context.client_epoch_id(),
            observer_root_invocation_id: lineage.root,
            observer_parent_invocation_id: lineage.current,
            observer_lineage: lineage.ancestors,
            observer_lineage_len: lineage.ancestor_len,
            target_invocation_id,
            snapshot_options,
        }
    }

    /// Returns the typed sealed operation.
    pub const fn operation(&self) -> &ClientInspectOperation {
        &self.operation
    }

    /// Returns the enclosing execution context and revision evidence.
    pub const fn context(&self) -> ClientExecutionContext {
        self.context
    }

    /// Returns the active revision pair pinned by this request.
    pub const fn pair(&self) -> RevisionPair {
        self.context.pair()
    }

    /// Returns the observer's parent invocation identity.
    pub const fn parent_invocation_id(&self) -> InvocationId {
        self.context.parent_invocation_id()
    }

    /// Returns the trusted observer root invocation identity.
    pub const fn observer_root_invocation_id(&self) -> InvocationId {
        self.observer_root_invocation_id
    }

    /// Returns the trusted observer parent invocation identity.
    pub const fn observer_parent_invocation_id(&self) -> InvocationId {
        self.observer_parent_invocation_id
    }

    /// Returns all bounded observer anchors from root through current.
    pub fn observer_lineage(&self) -> &[InvocationId] {
        &self.observer_lineage[..self.observer_lineage_len]
    }

    /// Returns the fixed observer purpose for this sealed request.
    pub const fn observer_purpose(&self) -> &'static str {
        "inspect"
    }

    /// Returns the target invocation identity when canonical evidence supplied it.
    pub const fn target_invocation_id(&self) -> Option<InvocationId> {
        self.target_invocation_id
    }

    /// Returns the checked snapshot-options value for a snapshot request.
    pub fn snapshot_options(&self) -> Option<&RuntimeValue> {
        self.snapshot_options.as_ref()
    }

    /// Returns the client-side epoch anchor for this request.
    pub const fn client_epoch_id(&self) -> ClientEpochId {
        self.client_epoch_id
    }
}

/// One typed request submitted to an installed CLIENT runtime contract.
///
/// The request carries already-evaluated argument values and the enclosing
/// execution context. It does not carry caller-supplied authority.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientExternalContractRequest {
    identity: String,
    arguments: Vec<(ParameterId, RuntimeValue)>,
    context: ClientExecutionContext,
    observer_root_invocation_id: InvocationId,
    observer_parent_invocation_id: InvocationId,
}

impl ClientExternalContractRequest {
    /// Creates a request for one exact runtime contract identity.
    pub fn new(
        context: ClientExecutionContext,
        identity: impl Into<String>,
        arguments: Vec<(ParameterId, RuntimeValue)>,
    ) -> Self {
        Self::with_lineage(
            context,
            identity,
            arguments,
            ObserverLineage::compatibility(context),
        )
    }

    fn with_lineage(
        context: ClientExecutionContext,
        identity: impl Into<String>,
        arguments: Vec<(ParameterId, RuntimeValue)>,
        lineage: ObserverLineage,
    ) -> Self {
        Self {
            identity: identity.into(),
            arguments,
            context,
            observer_root_invocation_id: lineage.root,
            observer_parent_invocation_id: lineage.current,
        }
    }

    /// Returns the exact runtime contract identity.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Returns the evaluated arguments with their declared parameter identities.
    pub fn arguments(&self) -> &[(ParameterId, RuntimeValue)] {
        &self.arguments
    }

    /// Returns the enclosing CLIENT execution context.
    pub const fn context(&self) -> ClientExecutionContext {
        self.context
    }

    /// Returns the trusted observer root invocation identity.
    pub const fn observer_root_invocation_id(&self) -> InvocationId {
        self.observer_root_invocation_id
    }

    /// Returns the trusted observer parent invocation identity.
    pub const fn observer_parent_invocation_id(&self) -> InvocationId {
        self.observer_parent_invocation_id
    }
}

/// A sealed Inspector operation failed during local CLIENT evaluation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientInspectError {
    /// The provider is not installed or returned the stable unavailable code.
    Failed(String),
    /// The operation attempted to use an invocation value of the wrong type.
    InvalidTarget,
    /// The operation attempted to project a value that is not a snapshot carrier.
    InvalidSnapshot,
    /// The provider returned a value outside the operation's sealed result type.
    TypeMismatch,
    /// Nested Inspector operations exceeded the closed expression depth bound.
    RecursionLimit,
    /// The request context did not match the active revision.
    RevisionMismatch {
        expected: RevisionPair,
        actual: RevisionPair,
    },
}

impl fmt::Display for ClientInspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(code) => write!(formatter, "CLIENT Inspector failed: {code}"),
            Self::InvalidTarget => formatter.write_str("CLIENT Inspector target is invalid"),
            Self::InvalidSnapshot => formatter.write_str("CLIENT Inspector snapshot is invalid"),
            Self::TypeMismatch => {
                formatter.write_str("CLIENT Inspector provider returned the wrong type")
            }
            Self::RecursionLimit => {
                formatter.write_str("CLIENT Inspector recursion limit was exceeded")
            }
            Self::RevisionMismatch { .. } => formatter
                .write_str("CLIENT Inspector request revision does not match the active revision"),
        }
    }
}

impl Error for ClientInspectError {}

/// A runtime adapter that evaluates one resource request.
pub trait ClientResourceExecutor {
    /// Binds the kernel-generated invocation that owns the current dispatch.
    ///
    /// The authenticated server dispatch supplies this identity as the
    /// current invocation anchor; it is provenance, not caller authority.
    fn bind_current_invocation(&mut self, _invocation: InvocationId) {}
    /// Executes one request and returns its completion.
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion;
    /// Reports one completion without blocking `execute`.
    ///
    /// Transport-backed executors use this for stream value batches and
    /// terminal completions; immediate executors keep the default.
    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        None
    }
    /// Requests cancellation of one live request.
    ///
    /// The default reports `Pending` because a generic executor cannot prove
    /// that it released executor-owned work. An executor that can complete
    /// cancellation must override this method and return the terminal
    /// completion only after it has removed the request from its ownership.
    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.pending()
    }
    /// Releases one request after its owner chooses a local terminal outcome.
    ///
    /// Implementations must remove transport ownership and must not expose a
    /// later completion through [`Self::poll`]. The default fails closed because
    /// a generic executor cannot prove that it released a pending request.
    fn abandon(&mut self, _request: ClientResourceRequest) -> Result<(), String> {
        Err("resource executor cannot abandon a pending request".to_owned())
    }
    /// Cancels the active transport request, when one is pending.
    ///
    /// The default keeps immediate executors unchanged.
    fn cancel_pending(&mut self) -> Option<ClientResourceCompletion> {
        None
    }
    /// Evaluates one typed Inspector operation.
    ///
    /// The default is deliberately fail-closed. Hosts that do not install the
    /// headless Inspector provider therefore expose the stable public failure
    /// code rather than selecting another runtime or generic resource path.
    fn inspect(&mut self, _request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        Err("inspect.runtime_unavailable".to_owned())
    }
    /// Reads one bounded input value from the active client session.
    ///
    /// This is a language interaction primitive, not an external runtime
    /// contract. Hosts provide the terminal or graphical input source.
    fn read_input(&mut self, _context: ClientExecutionContext) -> Result<RuntimeValue, String> {
        Err("client.input_unavailable".to_owned())
    }
    /// Evaluates one bounded command through the authenticated client session.
    ///
    /// The host owns transport and authority; the CLIENT function owns when
    /// this primitive is called.
    fn evaluate_command(
        &mut self,
        _context: ClientExecutionContext,
        _command: &str,
    ) -> Result<RuntimeValue, String> {
        Err("client.dynamic_invocation_unavailable".to_owned())
    }
    /// Evaluates one typed external CLIENT runtime contract.
    ///
    /// Hosts that do not install the exact contract fail closed. Generic
    /// external contracts remain unavailable unless a host opts in explicitly.
    fn external_contract(
        &mut self,
        _request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        Err(EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE.to_owned())
    }
}

type InspectProvider = Box<dyn FnMut(&ClientInspectRequest) -> Result<RuntimeValue, String>>;
type ExternalContractProvider =
    Box<dyn FnMut(&ClientExternalContractRequest) -> Result<RuntimeValue, String>>;

/// A deterministic immediate executor for host glue and focused tests.
pub struct DeterministicClientResourceExecutor<F> {
    evaluate: F,
    inspect: Option<InspectProvider>,
    external_contract: Option<ExternalContractProvider>,
}

impl<F> DeterministicClientResourceExecutor<F> {
    /// Creates an immediate executor around one evaluation closure.
    pub const fn new(evaluate: F) -> Self {
        Self {
            evaluate,
            inspect: None,
            external_contract: None,
        }
    }

    /// Installs a deterministic Inspector provider for focused tests and host glue.
    pub fn with_inspect<I>(mut self, inspect: I) -> Self
    where
        I: FnMut(&ClientInspectRequest) -> Result<RuntimeValue, String> + 'static,
    {
        self.inspect = Some(Box::new(inspect));
        self
    }

    /// Installs a deterministic external contract provider for focused tests and host glue.
    pub fn with_external_contract<I>(mut self, external_contract: I) -> Self
    where
        I: FnMut(&ClientExternalContractRequest) -> Result<RuntimeValue, String> + 'static,
    {
        self.external_contract = Some(Box::new(external_contract));
        self
    }
}

impl<F> ClientResourceExecutor for DeterministicClientResourceExecutor<F>
where
    F: FnMut(&ClientResourceRequest) -> Result<RuntimeValue, String>,
{
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        match (self.evaluate)(&request) {
            Ok(value) => request.ready(value),
            Err(code) => request.failed(code),
        }
    }
    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
    }

    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        match self.inspect.as_mut() {
            Some(provider) => provider(&request),
            None => Err("inspect.runtime_unavailable".to_owned()),
        }
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        match self.external_contract.as_mut() {
            Some(provider) => provider(&request),
            None => Err(EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE.to_owned()),
        }
    }
}

/// The small immutable validation witness retained for a request's lifetime.
///
/// `ActiveDatabaseRevision` owns the complete source/catalogue/revision graph.
/// Keeping a clone of it for every loading resource would duplicate that graph.
/// Requests only need the pinned identity, target/result admission, and the
/// type facts required to validate a late completion, so capture those facts at
/// the client boundary instead.
#[derive(Clone)]
struct ClientResourceValidationContext {
    pair: RevisionPair,
    target: InvocationTarget,
    kind: ResourceKind,
    expected_type: ResolvedType,
    target_supported: bool,
    result_type_supported: bool,
    expected_type_known: bool,
    value_kind: ClientResourceValueKind,
}

#[derive(Clone)]
enum ClientResourceValueKind {
    Scalar(StandardScalar),
    Opaque(TypeId),
    InspectCarrier(TypeId),
    Record(TypeId),
    Enum {
        type_id: TypeId,
        labels: Arc<[String]>,
    },
    Reference {
        target: TypeId,
        inspect_invocation: bool,
    },
    Reject,
}

impl ClientResourceValidationContext {
    fn from_active(
        active: &ActiveDatabaseRevision,
        target: InvocationTarget,
        kind: ResourceKind,
        expected_type: ResolvedType,
    ) -> Self {
        Self {
            pair: active.pair(),
            target,
            kind,
            expected_type,
            target_supported: execution::active_supports_invocation_target(active, target),
            result_type_supported: execution::active_resource_result_type_matches(
                active,
                target,
                kind,
                expected_type,
            ),
            expected_type_known: execution::active_type_is_known(active, expected_type),
            value_kind: ClientResourceValueKind::from_active(active, expected_type),
        }
    }

    fn inspect_carrier_matches(&self, value: &RuntimeValue, expected: TypeId) -> bool {
        let RuntimeValue::Opaque(opaque) = value else {
            return false;
        };
        if opaque.opaque_type() != expected {
            return false;
        }
        let Some(kind) = InspectCarrierKind::from_type_id(expected) else {
            return false;
        };
        let Ok(envelope) = InspectCarrierEnvelope::decode(opaque.canonical_payload()) else {
            return false;
        };
        envelope.carrier_kind() == kind
            && envelope.source_revision_id() == self.pair.source()
            && envelope.catalogue_revision_id() == self.pair.catalogue()
    }

    fn value_matches(&self, value: &RuntimeValue, expected: ResolvedType) -> bool {
        if let RuntimeValue::Null(null) = value {
            return null.resolved_type() == expected && self.expected_type_known;
        }
        if expected != self.expected_type {
            return false;
        }
        match &self.value_kind {
            ClientResourceValueKind::Scalar(scalar) => {
                execution::runtime_scalar_matches(*scalar, value)
            }
            ClientResourceValueKind::Opaque(type_id) => {
                matches!(value, RuntimeValue::Opaque(opaque) if opaque.opaque_type() == *type_id)
            }
            ClientResourceValueKind::InspectCarrier(type_id) => {
                self.inspect_carrier_matches(value, *type_id)
            }
            ClientResourceValueKind::Record(type_id) => {
                matches!(value, RuntimeValue::Record(record) if record.record_type() == *type_id)
            }
            ClientResourceValueKind::Enum { type_id, labels } => {
                matches!(value, RuntimeValue::Enum(enum_value) if enum_value.enum_type() == *type_id && labels.iter().any(|label| label == enum_value.label()))
            }
            ClientResourceValueKind::Reference {
                target,
                inspect_invocation,
            } => {
                if *inspect_invocation {
                    execution::inspect_invocation_target(value).is_some()
                } else {
                    matches!(value, RuntimeValue::Reference { target: actual, .. } if *actual == *target)
                }
            }
            ClientResourceValueKind::Reject => false,
        }
    }
}

impl ClientResourceValueKind {
    fn from_active(active: &ActiveDatabaseRevision, expected: ResolvedType) -> Self {
        match expected {
            ResolvedType::Scalar(scalar) => match scalar {
                StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject => Self::Scalar(scalar),
                StandardScalar::Decimal
                | StandardScalar::Uuid
                | StandardScalar::Date
                | StandardScalar::Time
                | StandardScalar::Timestamp
                | StandardScalar::Duration
                | StandardScalar::Void => Self::Reject,
            },
            ResolvedType::Value(type_id) => {
                if execution::is_inspect_carrier_type(type_id) {
                    return Self::InspectCarrier(type_id);
                }
                if type_id == SYS_INSPECT_INVOCATION_TYPE_ID {
                    return Self::Reject;
                }
                if type_id == STD_UI_TYPE_ID {
                    return Self::Opaque(type_id);
                }
                let Some(definition) = active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
                else {
                    return Self::Reject;
                };
                if definition.kind() == ValueTypeKind::Opaque {
                    return Self::Opaque(type_id);
                }
                match definition.representation_contract() {
                    "orna.kernel.value.boolean@1" => Self::Scalar(StandardScalar::Boolean),
                    "orna.kernel.value.integer@1" => Self::Scalar(StandardScalar::Integer),
                    "orna.kernel.value.bigint@1" => Self::Scalar(StandardScalar::BigInt),
                    "orna.kernel.value.float@1" => Self::Scalar(StandardScalar::Float),
                    "orna.kernel.value.character-large-object@1" => {
                        Self::Scalar(StandardScalar::CharacterLargeObject)
                    }
                    "orna.kernel.value.binary-large-object@1" => {
                        Self::Scalar(StandardScalar::BinaryLargeObject)
                    }
                    _ => Self::Reject,
                }
            }
            ResolvedType::Named(type_id) => {
                if execution::is_inspect_carrier_type(type_id) {
                    return Self::InspectCarrier(type_id);
                }
                if execution::active_has_record_type(active, type_id) {
                    return Self::Record(type_id);
                }
                let application = active
                    .catalogue()
                    .enum_type_by_id(type_id)
                    .map(|enum_type| enum_type.labels().to_vec());
                let standard = active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|snapshot| snapshot.catalogue().enum_type_by_id(type_id))
                    .map(|enum_type| enum_type.labels().to_vec());
                match (application, standard) {
                    (Some(_), Some(_)) | (None, None) => Self::Reject,
                    (Some(labels), None) | (None, Some(labels)) => Self::Enum {
                        type_id,
                        labels: labels.into(),
                    },
                }
            }
            ResolvedType::Reference { target } => {
                if target == SYS_INSPECT_INVOCATION_TYPE_ID
                    || execution::active_has_object_type(active, target)
                {
                    Self::Reference {
                        target,
                        inspect_invocation: target == SYS_INSPECT_INVOCATION_TYPE_ID,
                    }
                } else {
                    Self::Reject
                }
            }
        }
    }
}

trait ClientResourceRevisionValidation {
    fn pair(&self) -> RevisionPair;
    fn target_supported(&self, target: InvocationTarget) -> bool;
    fn result_type_supported(
        &self,
        target: InvocationTarget,
        kind: ResourceKind,
        expected: ResolvedType,
    ) -> bool;
    fn value_matches(&self, value: &RuntimeValue, expected: ResolvedType) -> bool;
}

impl ClientResourceRevisionValidation for ActiveDatabaseRevision {
    fn pair(&self) -> RevisionPair {
        self.pair()
    }

    fn target_supported(&self, target: InvocationTarget) -> bool {
        execution::active_supports_invocation_target(self, target)
    }

    fn result_type_supported(
        &self,
        target: InvocationTarget,
        kind: ResourceKind,
        expected: ResolvedType,
    ) -> bool {
        execution::active_resource_result_type_matches(self, target, kind, expected)
    }

    fn value_matches(&self, value: &RuntimeValue, expected: ResolvedType) -> bool {
        execution::runtime_value_matches(self, value, expected)
    }
}

impl ClientResourceRevisionValidation for ClientResourceValidationContext {
    fn pair(&self) -> RevisionPair {
        self.pair
    }

    fn target_supported(&self, target: InvocationTarget) -> bool {
        target == self.target && self.target_supported
    }

    fn result_type_supported(
        &self,
        target: InvocationTarget,
        kind: ResourceKind,
        expected: ResolvedType,
    ) -> bool {
        target == self.target
            && kind == self.kind
            && expected == self.expected_type
            && self.result_type_supported
    }

    fn value_matches(&self, value: &RuntimeValue, expected: ResolvedType) -> bool {
        ClientResourceValidationContext::value_matches(self, value, expected)
    }
}

/// Request metadata retained by the runtime solely for executor cancellation.
///
/// The request payload can contain sensitive argument values, so the wrapper
/// deliberately redacts it from the parent resource's derived `Debug` output.
#[derive(Clone)]
struct ActiveClientResourceRequest {
    request: ClientResourceRequest,
    validation: ClientResourceValidationContext,
}
impl PartialEq for ActiveClientResourceRequest {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request
    }
}

impl fmt::Debug for ActiveClientResourceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ActiveClientResourceRequest")
            .field(&self.request.request_id())
            .finish()
    }
}

/// One typed CLIENT resource lifecycle owned by the local evaluator.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientResource {
    key: ClientResourceKey,
    kind: ResourceKind,
    expected_type: ResolvedType,
    request_id: Option<InvocationId>,
    /// A runtime-owned copy of the request metadata needed to ask the
    /// executor to cancel its owned request, plus the active revision that
    /// validated that request. The executor still owns the submitted request;
    /// this copy is only runtime cancellation and validation context.
    active_request: Option<ActiveClientResourceRequest>,
    generation: ClientResourceGeneration,
    status: ClientResourceStatus,
    value: Option<RuntimeValue>,
    failure: Option<ClientResourceFailure>,
    stream_batches: VecDeque<Vec<RuntimeValue>>,
    stream_queued_items: u64,
    stream_total_items: u64,
    stream_complete: bool,
}

impl ClientResource {
    /// Creates an idle resource with no published value.
    pub const fn new(key: ClientResourceKey, expected_type: ResolvedType) -> Self {
        Self::new_with_kind(key, ResourceKind::Scalar, expected_type)
    }

    /// Creates an idle stream resource whose expected type is the item type.
    pub const fn new_stream(key: ClientResourceKey, expected_type: ResolvedType) -> Self {
        Self::new_with_kind(key, ResourceKind::Stream, expected_type)
    }

    /// Creates an idle resource with an explicit scalar or stream kind.
    pub const fn new_with_kind(
        key: ClientResourceKey,
        kind: ResourceKind,
        expected_type: ResolvedType,
    ) -> Self {
        Self {
            key,
            kind,
            expected_type,
            request_id: None,
            active_request: None,
            generation: ClientResourceGeneration(0),
            status: ClientResourceStatus::Idle,
            value: None,
            failure: None,
            stream_batches: VecDeque::new(),
            stream_queued_items: 0,
            stream_total_items: 0,
            stream_complete: false,
        }
    }
    /// Returns the complete cache identity.
    pub const fn key(&self) -> ClientResourceKey {
        self.key
    }

    /// Returns whether this resource is scalar or streamed.
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the expected result item type.
    pub const fn expected_type(&self) -> ResolvedType {
        self.expected_type
    }

    /// Returns whether a stream has received its terminal completion.
    pub const fn stream_complete(&self) -> bool {
        self.stream_complete
    }

    /// Returns the current request generation.
    pub const fn generation(&self) -> ClientResourceGeneration {
        self.generation
    }

    /// Returns the identity of the currently active request, if any.
    pub const fn request_id(&self) -> Option<InvocationId> {
        self.request_id
    }

    /// Returns the current lifecycle state.
    pub const fn status(&self) -> ClientResourceStatus {
        self.status
    }

    /// Returns the published value in the `READY` state.
    pub fn value(&self) -> Option<&RuntimeValue> {
        self.value.as_ref()
    }

    /// Returns the structured failure in the `FAILED` state.
    pub fn failure(&self) -> Option<&ClientResourceFailure> {
        self.failure.as_ref()
    }

    /// Starts a new request and invalidates every older completion.
    pub fn begin_loading(&mut self) -> Result<ClientResourceGeneration, ClientResourceError> {
        let generation = self.advance_generation()?;
        self.request_id = Some(InvocationId::new());
        self.active_request = None;
        self.status = ClientResourceStatus::Loading;
        self.clear_result();
        Ok(generation)
    }

    /// Starts a request without an enclosing invocation context.
    ///
    /// This compatibility entrypoint is for local resource lifecycle users.
    /// Installed host evaluation uses [`Self::begin_request_with_context`].
    pub fn begin_request(
        &mut self,
        active: &ActiveDatabaseRevision,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        self.begin_request_inner(active, None, ResourceKind::Scalar, arguments)
    }

    /// Starts a stream request without an enclosing invocation context.
    pub fn begin_stream_request(
        &mut self,
        active: &ActiveDatabaseRevision,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        self.begin_request_inner(active, None, ResourceKind::Stream, arguments)
    }

    /// Starts a request with its enclosing invocation and compiled call site.
    pub fn begin_request_with_context(
        &mut self,
        active: &ActiveDatabaseRevision,
        invocation_context: ClientResourceInvocationContext,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        self.begin_request_with_context_and_kind(
            active,
            ResourceKind::Scalar,
            invocation_context,
            arguments,
        )
    }

    /// Starts a request with its enclosing invocation and explicit kind.
    pub fn begin_request_with_context_and_kind(
        &mut self,
        active: &ActiveDatabaseRevision,
        kind: ResourceKind,
        invocation_context: ClientResourceInvocationContext,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        self.begin_request_inner(active, Some(invocation_context), kind, arguments)
    }

    /// Starts a request with its enclosing invocation and stream kind.
    pub fn begin_stream_request_with_context(
        &mut self,
        active: &ActiveDatabaseRevision,
        invocation_context: ClientResourceInvocationContext,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        self.begin_request_with_context_and_kind(
            active,
            ResourceKind::Stream,
            invocation_context,
            arguments,
        )
    }

    fn begin_request_inner(
        &mut self,
        active: &ActiveDatabaseRevision,
        invocation_context: Option<ClientResourceInvocationContext>,
        kind: ResourceKind,
        arguments: Vec<FunctionArgument>,
    ) -> Result<ClientResourceRequest, ClientResourceError> {
        if kind != self.kind {
            return Err(ClientResourceError::TypeMismatch);
        }
        let generation = self.next_generation()?;
        let request = ClientResourceRequest::new(
            active,
            self.key,
            generation,
            kind,
            self.expected_type,
            arguments,
            invocation_context,
        )?;
        self.generation = generation;
        self.request_id = Some(request.request_id());
        self.active_request = Some(ActiveClientResourceRequest {
            request: request.clone(),
            validation: ClientResourceValidationContext::from_active(
                active,
                self.key.target(),
                kind,
                self.expected_type,
            ),
        });
        self.status = ClientResourceStatus::Loading;
        self.clear_result();
        Ok(request)
    }

    /// Applies one executor completion through the resource invariants.
    pub fn apply_completion(
        &mut self,
        active: &ActiveDatabaseRevision,
        completion: ClientResourceCompletion,
    ) -> Result<(), ClientResourceError> {
        self.apply_completion_with(active, completion)
    }

    fn apply_completion_with<V: ClientResourceRevisionValidation>(
        &mut self,
        validation: &V,
        completion: ClientResourceCompletion,
    ) -> Result<(), ClientResourceError> {
        self.validate_completion_identity(&completion)?;
        match completion {
            ClientResourceCompletion::Ready {
                generation, value, ..
            } => {
                if self.kind != ResourceKind::Scalar {
                    return Err(ClientResourceError::TypeMismatch);
                }
                self.publish_ready_with(validation, generation, value)
            }
            ClientResourceCompletion::StreamValues {
                generation, values, ..
            } => self.append_stream_values_with(validation, generation, values),
            ClientResourceCompletion::StreamCompleted { generation, .. } => {
                self.complete_stream_with(validation, generation)
            }
            ClientResourceCompletion::Cancelled { generation, .. } => {
                self.validate_active_identity_with(validation)?;
                self.cancel(generation)
            }
            ClientResourceCompletion::Pending { generation, .. } => {
                self.validate_active_identity_with(validation)?;
                self.require_loading(generation)
            }
            ClientResourceCompletion::Failed {
                generation, code, ..
            } => {
                self.validate_active_identity_with(validation)?;
                self.publish_failure(generation, code)
            }
        }
    }

    /// Publishes one type-checked result for the current generation.
    pub fn publish_ready(
        &mut self,
        active: &ActiveDatabaseRevision,
        generation: ClientResourceGeneration,
        value: RuntimeValue,
    ) -> Result<(), ClientResourceError> {
        self.publish_ready_with(active, generation, value)
    }

    fn publish_ready_with<V: ClientResourceRevisionValidation>(
        &mut self,
        validation: &V,
        generation: ClientResourceGeneration,
        value: RuntimeValue,
    ) -> Result<(), ClientResourceError> {
        if self.kind != ResourceKind::Scalar {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.require_loading(generation)?;
        self.validate_active_identity_with(validation)?;
        if !validation.value_matches(&value, self.expected_type) {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.status = ClientResourceStatus::Ready;
        self.active_request = None;
        self.value = Some(value);
        self.failure = None;
        Ok(())
    }

    fn append_stream_values_with<V: ClientResourceRevisionValidation>(
        &mut self,
        validation: &V,
        generation: ClientResourceGeneration,
        values: Vec<RuntimeValue>,
    ) -> Result<(), ClientResourceError> {
        let (total_items, queued_items) =
            self.validate_stream_values_with(validation, generation, &values)?;
        self.stream_batches.push_back(values);
        self.stream_queued_items = queued_items;
        self.stream_total_items = total_items;
        Ok(())
    }

    fn validate_stream_values_with<V: ClientResourceRevisionValidation>(
        &self,
        validation: &V,
        generation: ClientResourceGeneration,
        values: &[RuntimeValue],
    ) -> Result<(u64, u64), ClientResourceError> {
        if self.kind != ResourceKind::Stream || values.is_empty() {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.require_loading(generation)?;
        if values.len() > MAX_RESOURCE_BATCH_ITEMS {
            return Err(ClientResourceError::TypeMismatch);
        }
        let total_items = self
            .stream_total_items
            .checked_add(values.len() as u64)
            .filter(|total| *total <= MAX_RESOURCE_TOTAL_ITEMS)
            .ok_or(ClientResourceError::TypeMismatch)?;
        let queued_items = self
            .stream_queued_items
            .checked_add(values.len() as u64)
            .filter(|total| *total <= MAX_RESOURCE_QUEUED_ITEMS)
            .ok_or(ClientResourceError::TypeMismatch)?;
        self.validate_stream_item_type_with(validation)?;
        if values
            .iter()
            .any(|value| !validation.value_matches(value, self.expected_type))
        {
            return Err(ClientResourceError::TypeMismatch);
        }
        Ok((total_items, queued_items))
    }

    fn complete_stream_with<V: ClientResourceRevisionValidation>(
        &mut self,
        validation: &V,
        generation: ClientResourceGeneration,
    ) -> Result<(), ClientResourceError> {
        if self.kind != ResourceKind::Stream {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.require_loading(generation)?;
        self.validate_stream_item_type_with(validation)?;
        self.stream_complete = true;
        self.status = ClientResourceStatus::Ready;
        self.active_request = None;
        self.value = None;
        self.failure = None;
        Ok(())
    }

    fn validate_stream_item_type(
        &self,
        active: &ActiveDatabaseRevision,
    ) -> Result<(), ClientResourceError> {
        self.validate_stream_item_type_with(active)
    }

    fn validate_stream_item_type_with<V: ClientResourceRevisionValidation>(
        &self,
        validation: &V,
    ) -> Result<(), ClientResourceError> {
        if validation.pair() != self.key.target().revision() {
            return Err(ClientResourceError::RevisionMismatch {
                expected: self.key.target().revision(),
                actual: validation.pair(),
            });
        }
        if !validation.target_supported(self.key.target())
            || !validation.result_type_supported(self.key.target(), self.kind, self.expected_type)
        {
            return Err(ClientResourceError::TypeMismatch);
        }
        Ok(())
    }

    /// Takes the next stream batch as canonical OPTION<LIST<T>>.
    ///
    /// `None` means the stream is still loading and has no batch available.
    /// A terminal empty stream returns `Some(OPTION(None))`; batches are never
    /// replayed after this method consumes them.
    pub fn take_stream_value(
        &mut self,
        active: &ActiveDatabaseRevision,
    ) -> Result<Option<RuntimeValue>, ClientResourceError> {
        if self.kind != ResourceKind::Stream {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.validate_stream_item_type(active)?;
        let Some(item_descriptor) = supported_stream_item_descriptor(active, self.expected_type)
        else {
            return Err(ClientResourceError::TypeMismatch);
        };
        let list_descriptor =
            TypeDescriptor::list(item_descriptor).map_err(|_| ClientResourceError::TypeMismatch)?;
        let option_descriptor = TypeDescriptor::option(list_descriptor.clone())
            .map_err(|_| ClientResourceError::TypeMismatch)?;
        if let Some(values) = self.stream_batches.front() {
            let queued_items = self
                .stream_queued_items
                .checked_sub(values.len() as u64)
                .ok_or(ClientResourceError::TypeMismatch)?;
            let values = self
                .stream_batches
                .pop_front()
                .expect("stream batch was checked");
            self.stream_queued_items = queued_items;
            let list = RuntimeValue::list(active, list_descriptor, values)
                .map_err(|_| ClientResourceError::TypeMismatch)?;
            let value = RuntimeValue::option(active, option_descriptor, Some(list))
                .map_err(|_| ClientResourceError::TypeMismatch)?;
            return Ok(Some(value));
        }
        if self.stream_complete {
            let value = RuntimeValue::option(active, option_descriptor, None)
                .map_err(|_| ClientResourceError::TypeMismatch)?;
            return Ok(Some(value));
        }
        match self.status {
            ClientResourceStatus::Loading => Ok(None),
            status => Err(ClientResourceError::InvalidTransition { status }),
        }
    }

    /// Records one structured failure for the current generation.
    pub fn publish_failure(
        &mut self,
        generation: ClientResourceGeneration,
        code: String,
    ) -> Result<(), ClientResourceError> {
        if code.is_empty() || code.contains('\0') {
            return Err(ClientResourceError::InvalidFailureCode);
        }
        self.require_loading(generation)?;
        self.status = ClientResourceStatus::Failed;
        self.active_request = None;
        self.value = None;
        self.failure = Some(ClientResourceFailure { code });
        Ok(())
    }

    /// Cancels the current generation without retaining a value or failure.
    pub fn cancel(
        &mut self,
        generation: ClientResourceGeneration,
    ) -> Result<(), ClientResourceError> {
        self.require_loading(generation)?;
        self.status = ClientResourceStatus::Cancelled;
        self.active_request = None;
        self.clear_result();
        Ok(())
    }

    /// Invalidates the current generation and returns to `IDLE`.
    pub fn invalidate(&mut self) -> Result<(), ClientResourceError> {
        self.advance_generation()?;
        self.request_id = None;
        self.active_request = None;
        self.status = ClientResourceStatus::Idle;
        self.clear_result();
        Ok(())
    }

    /// Asks the owning executor to cancel the active request before local
    /// invalidation makes its generation stale.
    ///
    /// A terminal completion committed before cancellation wins: matching
    /// `READY`, `FAILED`, or `STREAM_COMPLETED` results are applied and the
    /// resource remains terminal at its current generation. A non-terminal
    /// cancellation (`PENDING` or `STREAM_VALUES`) does not release executor
    /// ownership; the executor must explicitly abandon that request before
    /// local invalidation can advance the generation. If abandonment fails,
    /// this method leaves the resource
    /// unchanged and returns the executor error.
    pub fn invalidate_with_executor(
        &mut self,
        active: &ActiveDatabaseRevision,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<(), ClientResourceError> {
        // Check the next generation before releasing executor ownership. If
        // the counter is exhausted, the resource must remain unchanged.
        self.next_generation()?;
        if let Some(request) = self.active_request() {
            // The active revision is local knowledge. Validate it before
            // asking the executor to consume ownership; a mismatch must leave
            // both the loading resource and its pending request intact.
            self.validate_owned_request(&request)?;
            self.validate_active_identity(active)?;
            let cancellation = executor.cancel(request.clone());
            let cancellation_matches_request = cancellation.matches_request(&request);
            let terminal = matches!(
                cancellation,
                ClientResourceCompletion::Ready { .. }
                    | ClientResourceCompletion::Failed { .. }
                    | ClientResourceCompletion::StreamCompleted { .. },
            );
            let requires_abandon = matches!(
                cancellation,
                ClientResourceCompletion::Pending { .. }
                    | ClientResourceCompletion::StreamValues { .. },
            );
            if requires_abandon {
                // Validate without publishing a non-terminal completion. If
                // abandonment fails, the owned request and local resource
                // remain unchanged so the caller can retry safely.
                self.validate_cancellation_completion(active, &cancellation)?;
                if let Err(message) = executor.abandon(request) {
                    return Err(ClientResourceError::Executor(message));
                }
            } else {
                // Cancellation completions use the same identity, active
                // revision, target, declared type, runtime value, and
                // stream-item checks as ordinary executor completions. A
                // malformed terminal response with matching request identity
                // transitions to an explicit safe state after the executor
                // has consumed it; mismatched identity retains ownership.
                if let Err(error) = self.apply_completion(active, cancellation) {
                    if cancellation_matches_request {
                        self.mark_executor_released_cancelled();
                    }
                    return Err(error);
                }
                if terminal {
                    return Ok(());
                }
            }
        }

        self.invalidate()
    }

    /// Releases a loading request whose pinned revision is stale relative to
    /// the active revision that selected a replacement key.
    ///
    /// This is deliberately separate from [`Self::invalidate_with_executor`]:
    /// replacement selection runs under a newer active revision, so cleanup
    /// validates the request against the revision captured when it started.
    /// Request identity, completion shape, and the old active catalogue remain
    /// fail-closed while terminal results from the old key are discarded.
    fn invalidate_stale_replacement_with_executor(
        &mut self,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<(), ClientResourceError> {
        // Check the next generation before releasing executor ownership. If
        // the counter is exhausted, the resource must remain unchanged.
        self.next_generation()?;
        let Some(request) = self.active_request() else {
            return self.invalidate();
        };
        // A replacement may be selected under a newer active revision, but
        // the request must be validated against the revision that created it.
        // This keeps cross-revision cleanup fail-closed without weakening the
        // ordinary same-revision invalidation path.
        let Some(validation) = self.active_request_validation() else {
            return self.invalidate();
        };
        self.validate_owned_request(&request)?;
        self.validate_active_identity_with(&validation)?;
        let cancellation = executor.cancel(request.clone());
        let cancellation_matches_request = cancellation.matches_request(&request);
        let requires_abandon = matches!(
            cancellation,
            ClientResourceCompletion::Pending { .. }
                | ClientResourceCompletion::StreamValues { .. },
        );
        if requires_abandon {
            // Validate without publishing a non-terminal completion. If
            // abandonment fails, the owned request and local resource remain
            // unchanged so the caller can retry safely.
            self.validate_cancellation_completion_with(&validation, &cancellation)?;
            if let Err(message) = executor.abandon(request) {
                return Err(ClientResourceError::Executor(message));
            }
        } else if let Err(error) = self.apply_completion_with(&validation, cancellation) {
            // A matching terminal completion proves that the executor consumed
            // its request even when the completion itself was malformed. Keep
            // late results from being accepted while reporting the validation
            // failure to the caller.
            if cancellation_matches_request {
                self.mark_executor_released_cancelled();
            }
            return Err(error);
        }

        // The replacement key wins after the old request has been released;
        // terminal results from the old revision are intentionally discarded.
        self.invalidate()
    }

    fn clear_result(&mut self) {
        self.value = None;
        self.failure = None;
        self.stream_batches.clear();
        self.stream_queued_items = 0;
        self.stream_total_items = 0;
        self.stream_complete = false;
    }

    fn active_request(&self) -> Option<ClientResourceRequest> {
        self.active_request
            .as_ref()
            .map(|request| request.request.clone())
    }

    fn active_request_validation(&self) -> Option<ClientResourceValidationContext> {
        self.active_request
            .as_ref()
            .map(|request| request.validation.clone())
    }

    fn mark_executor_released_cancelled(&mut self) {
        // Match the ordinary cancellation transition: retain the generation
        // and request identity for late-completion rejection, but drop the
        // executor-owned request itself.
        self.status = ClientResourceStatus::Cancelled;
        self.active_request = None;
        self.clear_result();
    }

    fn next_generation(&self) -> Result<ClientResourceGeneration, ClientResourceError> {
        self.generation
            .0
            .checked_add(1)
            .map(ClientResourceGeneration)
            .ok_or(ClientResourceError::GenerationExhausted)
    }

    fn advance_generation(&mut self) -> Result<ClientResourceGeneration, ClientResourceError> {
        self.generation = ClientResourceGeneration(
            self.generation
                .0
                .checked_add(1)
                .ok_or(ClientResourceError::GenerationExhausted)?,
        );
        Ok(self.generation)
    }

    fn validate_completion_identity(
        &self,
        completion: &ClientResourceCompletion,
    ) -> Result<(), ClientResourceError> {
        let (request_id, key, generation) = completion.identity();
        self.require_generation(generation)?;
        self.require_request_id(request_id)?;
        self.require_key(key)
    }

    fn validate_owned_request(
        &self,
        request: &ClientResourceRequest,
    ) -> Result<(), ClientResourceError> {
        self.require_generation(request.generation())?;
        self.require_request_id(request.request_id())?;
        self.require_key(request.key())?;
        self.require_loading(request.generation())?;
        if request.target() != self.key.target() {
            return Err(ClientResourceError::TargetMismatch {
                expected: self.key.target(),
            });
        }
        if request.kind() != self.kind || request.expected_type() != self.expected_type {
            return Err(ClientResourceError::TypeMismatch);
        }
        Ok(())
    }

    fn validate_cancellation_completion(
        &self,
        active: &ActiveDatabaseRevision,
        completion: &ClientResourceCompletion,
    ) -> Result<(), ClientResourceError> {
        self.validate_cancellation_completion_with(active, completion)
    }

    fn validate_cancellation_completion_with<V: ClientResourceRevisionValidation>(
        &self,
        validation: &V,
        completion: &ClientResourceCompletion,
    ) -> Result<(), ClientResourceError> {
        self.validate_completion_identity(completion)?;
        match completion {
            ClientResourceCompletion::Pending { generation, .. } => {
                self.validate_active_identity_with(validation)?;
                self.require_loading(*generation)
            }
            ClientResourceCompletion::StreamValues {
                generation, values, ..
            } => {
                self.validate_active_identity_with(validation)?;
                self.validate_stream_values_with(validation, *generation, values)
                    .map(|_| ())
            }
            _ => Err(ClientResourceError::InvalidTransition {
                status: self.status,
            }),
        }
    }

    fn validate_active_identity(
        &self,
        active: &ActiveDatabaseRevision,
    ) -> Result<(), ClientResourceError> {
        self.validate_active_identity_with(active)
    }

    fn validate_active_identity_with<V: ClientResourceRevisionValidation>(
        &self,
        validation: &V,
    ) -> Result<(), ClientResourceError> {
        if validation.pair() != self.key.target().revision() {
            return Err(ClientResourceError::RevisionMismatch {
                expected: self.key.target().revision(),
                actual: validation.pair(),
            });
        }
        if !validation.target_supported(self.key.target()) {
            return Err(ClientResourceError::TargetMismatch {
                expected: self.key.target(),
            });
        }
        if !validation.result_type_supported(self.key.target(), self.kind, self.expected_type) {
            return Err(ClientResourceError::TypeMismatch);
        }
        Ok(())
    }

    fn require_generation(
        &self,
        generation: ClientResourceGeneration,
    ) -> Result<(), ClientResourceError> {
        if generation != self.generation {
            return Err(ClientResourceError::StaleGeneration {
                expected: self.generation,
                actual: generation,
            });
        }
        Ok(())
    }

    fn require_key(&self, actual: ClientResourceKey) -> Result<(), ClientResourceError> {
        if actual != self.key {
            return Err(ClientResourceError::RequestKeyMismatch {
                expected: Box::new(self.key),
                actual: Box::new(actual),
            });
        }
        Ok(())
    }

    fn require_request_id(&self, actual: InvocationId) -> Result<(), ClientResourceError> {
        match self.request_id {
            Some(expected) if expected == actual => Ok(()),
            Some(expected) => Err(ClientResourceError::RequestIdMismatch { expected, actual }),
            None => Err(ClientResourceError::RequestIdMismatch {
                expected: InvocationId::from_bytes([0; 16]),
                actual,
            }),
        }
    }

    fn require_loading(
        &self,
        generation: ClientResourceGeneration,
    ) -> Result<(), ClientResourceError> {
        if generation != self.generation {
            return Err(ClientResourceError::StaleGeneration {
                expected: self.generation,
                actual: generation,
            });
        }
        if self.status != ClientResourceStatus::Loading {
            return Err(ClientResourceError::InvalidTransition {
                status: self.status,
            });
        }
        Ok(())
    }
}
fn stream_item_descriptor(expected: ResolvedType) -> Option<TypeDescriptor> {
    match expected {
        ResolvedType::Scalar(scalar) => {
            let type_id = match scalar {
                StandardScalar::Boolean => orna_standard::BOOLEAN_TYPE_ID,
                StandardScalar::Integer => orna_standard::INTEGER_TYPE_ID,
                StandardScalar::BigInt => orna_standard::BIGINT_TYPE_ID,
                StandardScalar::Float => orna_standard::FLOAT_TYPE_ID,
                StandardScalar::CharacterLargeObject => {
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID
                }
                StandardScalar::BinaryLargeObject => orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
                StandardScalar::Decimal
                | StandardScalar::Uuid
                | StandardScalar::Date
                | StandardScalar::Time
                | StandardScalar::Timestamp
                | StandardScalar::Duration
                | StandardScalar::Void => return None,
            };
            Some(TypeDescriptor::named(type_id))
        }
        ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
            Some(TypeDescriptor::named(type_id))
        }
        ResolvedType::Reference { target } => Some(TypeDescriptor::reference(target)),
    }
}
fn source_reference_target_name(
    active: &ActiveDatabaseRevision,
    target: DefinitionReferenceTarget,
) -> Option<String> {
    let standard_catalogue = active
        .catalogue_hash_context()
        .standard()
        .map(|snapshot| snapshot.catalogue());
    match target {
        DefinitionReferenceTarget::ObjectType(id) => active
            .catalogue()
            .object_type_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.object_type_by_id(id)))
            .map(|definition| definition.name().to_string()),
        DefinitionReferenceTarget::ValueType(id) => active
            .catalogue()
            .value_type_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.value_type_by_id(id)))
            .map(|definition| definition.name().to_string())
            .or_else(|| {
                (id == orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID)
                    .then_some(orna_core::system::SYS_SOURCE_FUNCTION_TYPE_NAME.to_string())
            }),
        DefinitionReferenceTarget::Function(id) => active
            .catalogue()
            .function_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.function_by_id(id)))
            .map(|definition| definition.name().to_string())
            .or_else(|| {
                orna_core::system::system_function_by_id(id)
                    .map(|definition| definition.name_parts().join("."))
            }),
        DefinitionReferenceTarget::Parameter { owner, parameter } => active
            .catalogue()
            .function_by_id(owner)
            .and_then(|function| {
                function
                    .parameter_by_id(parameter)
                    .map(|parameter| format!("{}.{}", function.name(), parameter.name()))
            })
            .or_else(|| {
                standard_catalogue
                    .and_then(|catalogue| catalogue.function_by_id(owner))
                    .and_then(|function| {
                        function
                            .parameter_by_id(parameter)
                            .map(|parameter| format!("{}.{}", function.name(), parameter.name()))
                    })
            }),
        DefinitionReferenceTarget::Field { owner, field } => active
            .catalogue()
            .object_type_by_id(owner)
            .and_then(|definition| {
                definition.field_by_id(field).map(|field_definition| {
                    format!("{}.{}", definition.name(), field_definition.name())
                })
            })
            .or_else(|| {
                standard_catalogue
                    .and_then(|catalogue| catalogue.object_type_by_id(owner))
                    .and_then(|definition| {
                        definition.field_by_id(field).map(|field_definition| {
                            format!("{}.{}", definition.name(), field_definition.name())
                        })
                    })
            }),
        DefinitionReferenceTarget::Expression(id) => Some(format!("expression:{id:?}")),
        _ => None,
    }
}
fn source_metadata_type_id(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
) -> Option<TypeId> {
    match resolved_type {
        ResolvedType::Value(type_id)
        | ResolvedType::Named(type_id)
        | ResolvedType::Reference { target: type_id } => Some(type_id),
        ResolvedType::Scalar(scalar) => {
            let contract = match scalar {
                StandardScalar::Boolean => "orna.kernel.value.boolean@1",
                StandardScalar::Integer => "orna.kernel.value.integer@1",
                StandardScalar::BigInt => "orna.kernel.value.bigint@1",
                StandardScalar::Float => "orna.kernel.value.float@1",
                StandardScalar::Decimal => "orna.kernel.value.decimal@1",
                StandardScalar::CharacterLargeObject => {
                    "orna.kernel.value.character-large-object@1"
                }
                StandardScalar::BinaryLargeObject => "orna.kernel.value.binary-large-object@1",
                StandardScalar::Uuid => "orna.kernel.value.uuid@1",
                StandardScalar::Date => "orna.kernel.value.date@1",
                StandardScalar::Time => "orna.kernel.value.time@1",
                StandardScalar::Timestamp => "orna.kernel.value.timestamp@1",
                StandardScalar::Duration => "orna.kernel.value.duration@1",
                StandardScalar::Void => return None,
            };
            active
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.catalogue())
                .into_iter()
                .flat_map(|catalogue| catalogue.value_types())
                .chain(active.catalogue().value_types())
                .find(|definition| definition.representation_contract() == contract)
                .map(|definition| definition.id())
        }
    }
}

fn source_metadata_body_kind(
    artifact: &ExecutableArtifact,
) -> orna_core::source_metadata::SourceBodyKind {
    let version = if artifact.version() == CAPABILITY_FORMAT_VERSION {
        CapabilityClientPlan::decode(artifact.payload())
            .map(|plan| plan.inner_plan_version())
            .unwrap_or_default()
    } else {
        artifact.version()
    };
    match version {
        FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::BooleanLiteral,
        EXPRESSION_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Expression,
        PROCEDURAL_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Procedural,
        orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION => {
            orna_core::source_metadata::SourceBodyKind::ControlFlow
        }
        STATE_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::State,
        _ => orna_core::source_metadata::SourceBodyKind::Unknown,
    }
}

fn source_metadata_return_metadata(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
) -> Option<orna_core::source_metadata::SourceReturnMetadata> {
    match return_type {
        FunctionReturn::Single(resolved_type) => source_metadata_type_id(active, *resolved_type)
            .map(orna_core::source_metadata::SourceReturnMetadata::Single),
        FunctionReturn::Stream(resolved_type) => source_metadata_type_id(active, *resolved_type)
            .map(orna_core::source_metadata::SourceReturnMetadata::Stream),
        FunctionReturn::Rows(_) => None,
    }
}

fn supported_stream_item_descriptor(
    active: &ActiveDatabaseRevision,
    expected: ResolvedType,
) -> Option<TypeDescriptor> {
    let descriptor = stream_item_descriptor(expected)?;
    match expected {
        ResolvedType::Scalar(_) => Some(descriptor),
        ResolvedType::Named(type_id) => (execution::active_has_enum_type(active, type_id)
            || execution::active_has_record_type(active, type_id))
        .then_some(descriptor),
        ResolvedType::Value(type_id) => {
            let definition = active
                .catalogue_hash_context()
                .standard()
                .and_then(|standard| standard.catalogue().value_type_by_id(type_id))?;
            if definition.kind() == ValueTypeKind::Opaque {
                return None;
            }
            matches!(
                definition.representation_contract(),
                "orna.kernel.value.boolean@1"
                    | "orna.kernel.value.integer@1"
                    | "orna.kernel.value.bigint@1"
                    | "orna.kernel.value.float@1"
                    | "orna.kernel.value.character-large-object@1"
                    | "orna.kernel.value.binary-large-object@1"
            )
            .then_some(descriptor)
        }
        ResolvedType::Reference { target } => {
            execution::active_has_object_type(active, target).then_some(descriptor)
        }
    }
}

fn canonical_resource_arguments(
    arguments: &[FunctionArgument],
) -> Result<Vec<FunctionArgument>, ClientResourceError> {
    if arguments.len() > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientResourceError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut arguments = arguments.to_vec();
    arguments.sort_by_key(FunctionArgument::parameter);
    for pair in arguments.windows(2) {
        if pair[0].parameter() == pair[1].parameter() {
            return Err(ClientResourceError::DuplicateArgument {
                parameter: pair[0].parameter(),
            });
        }
    }
    Ok(arguments)
}

struct ResolvedResourceTarget<'a> {
    target: InvocationTarget,
    definition: &'a FunctionDefinition,
}

struct ResolvedClientFunction<'a> {
    definition: &'a FunctionDefinition,
    revision: &'a FunctionRevisionRecord,
    references: &'a [orna_core::revision::DefinitionReference],
    standard: Option<&'a VerifiedStandardLibrarySnapshot>,
}

fn verified_standard_executable(
    standard: &VerifiedStandardLibrarySnapshot,
    function: FunctionId,
) -> Option<&StandardExecutable> {
    let mut executables = standard
        .executables()
        .iter()
        .filter(|executable| executable.function() == function);
    let executable = executables.next()?;
    executables.next().is_none().then_some(executable)
}

/// Resolves one CLIENT function from the active application first, then from
/// the exact verified standard snapshot pinned by that application revision.
///
/// Application definitions retain precedence even when a malformed or
/// incomplete application revision would otherwise allow a standard fallback.
/// A standard definition is executable only when its snapshot carries exactly
/// one executable whose immutable revision is the definition's current
/// revision.
fn resolve_client_function<'a>(
    active: &'a ActiveDatabaseRevision,
    function: FunctionId,
) -> Option<ResolvedClientFunction<'a>> {
    if let Some(definition) = active.catalogue().function_by_id(function) {
        let revision = active.function_revisions().iter().find(|candidate| {
            candidate.function() == function && candidate.id() == definition.current_revision()
        })?;
        return Some(ResolvedClientFunction {
            definition,
            revision,
            references: active.references(),
            standard: None,
        });
    }

    let standard = active.catalogue_hash_context().standard()?;
    let definition = standard.catalogue().function_by_id(function)?;
    let executable = verified_standard_executable(standard, function)?;
    let revision = executable.revision();
    if revision.function() != function || revision.id() != definition.current_revision() {
        return None;
    }
    Some(ResolvedClientFunction {
        definition,
        revision,
        references: executable.references(),
        standard: Some(standard),
    })
}

fn client_invocation_target_is_resolved(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
) -> bool {
    let Some(resolved) = resolve_client_function(active, target.function()) else {
        return false;
    };
    match resolved.standard {
        Some(standard) => {
            target.class() == Some(TargetClass::VerifiedStandard)
                && target.standard_revision() == Some(standard.revision())
                && target.executable_revision() == Some(resolved.revision.id())
        }
        None => {
            matches!(target.class(), None | Some(TargetClass::Application))
                && target.standard_revision().is_none()
                && target.executable_revision().is_none()
        }
    }
}

fn verified_standard_executable_revision(
    standard: &VerifiedStandardLibrarySnapshot,
    function: FunctionId,
) -> Option<FunctionRevisionId> {
    verified_standard_executable(standard, function).map(|executable| executable.revision().id())
}

/// Resolves a resource target against the active application catalogue and its
/// exact verified standard snapshot. A standard target must carry both the
/// snapshot and executable revision pins; a raw class-less target is never
/// upgraded implicitly by this path.
fn resolve_resource_target<'a>(
    active: &'a ActiveDatabaseRevision,
    target: InvocationTarget,
) -> Option<ResolvedResourceTarget<'a>> {
    if target.revision() != active.pair() {
        return None;
    }
    let application = active.catalogue().function_by_id(target.function());
    let standard = active.catalogue_hash_context().standard();
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(target.function()));
    match target.class() {
        None | Some(TargetClass::Application) => {
            if target.standard_revision().is_some() || target.executable_revision().is_some() {
                return None;
            }
            match (application, standard_definition) {
                (Some(definition), None) => Some(ResolvedResourceTarget { target, definition }),
                _ => None,
            }
        }
        Some(TargetClass::VerifiedStandard) => {
            let standard_revision = target.standard_revision()?;
            let executable_revision = target.executable_revision()?;
            let standard = standard?;
            if application.is_some() || standard.revision() != standard_revision {
                return None;
            }
            let definition = standard_definition?;
            if verified_standard_executable_revision(standard, target.function())
                != Some(executable_revision)
            {
                return None;
            }
            Some(ResolvedResourceTarget { target, definition })
        }
    }
}

/// Resolves artifact resource metadata to the canonical application or pinned
/// verified-standard invocation target. The artifact stores the active pair;
/// standard identity is admitted only when the function exists in the exact
/// pinned snapshot and has one executable record.
fn resolve_resource_operation_target<'a>(
    active: &'a ActiveDatabaseRevision,
    operation: &ResourceOperationNode,
) -> Option<ResolvedResourceTarget<'a>> {
    resolve_unclassified_target(
        active,
        InvocationTarget::new(operation.target_function(), operation.target_revision()),
    )
}

/// Resolves an action's unclassified target to the application function or to
/// the exact verified-standard executable selected by the active catalogue
/// hash context. A raw target is intentionally never upgraded unless the
/// active catalogue proves that the identity belongs only to the standard
/// snapshot.
fn resolve_unclassified_target<'a>(
    active: &'a ActiveDatabaseRevision,
    raw_target: InvocationTarget,
) -> Option<ResolvedResourceTarget<'a>> {
    if raw_target.revision() != active.pair() {
        return None;
    }
    let application = active.catalogue().function_by_id(raw_target.function());
    let standard = active.catalogue_hash_context().standard();
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(raw_target.function()));
    match (application, standard_definition) {
        (Some(_), None) => resolve_resource_target(active, raw_target),
        (None, Some(_)) => {
            let standard = standard?;
            let executable_revision =
                verified_standard_executable_revision(standard, raw_target.function())?;
            let target = InvocationTarget::verified_standard(
                raw_target.function(),
                raw_target.revision(),
                standard.revision(),
                executable_revision,
            );
            resolve_resource_target(active, target)
        }
        _ => None,
    }
}

// ClientActionError preserves its public diagnostic layout at this resolver boundary.
#[allow(clippy::result_large_err)]
fn resolve_action_target<'a>(
    active: &'a ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<ResolvedResourceTarget<'a>, ClientActionError> {
    if descriptor.target_revision != active.pair() {
        return Err(ClientActionError::RevisionMismatch);
    }
    let Some(resolved) = resolve_unclassified_target(
        active,
        InvocationTarget::new(descriptor.target, descriptor.target_revision),
    ) else {
        return Err(ClientActionError::TargetMismatch);
    };
    let expected_domain = match descriptor.domain {
        ActionTargetDomain::Client => FunctionDomain::Client,
        ActionTargetDomain::Server => FunctionDomain::Server,
    };
    if resolved.definition.domain() != expected_domain {
        return Err(ClientActionError::TargetMismatch);
    }
    Ok(resolved)
}

/// Returns whether raw arguments match a function's exact active signature.
///
/// Matching is by stable `ParameterId`, not declaration position or source
/// name. The argument count must match exactly, every parameter may occur only
/// once, and each runtime value must match its active resolved type.
pub fn client_function_arguments_match(
    active: &ActiveDatabaseRevision,
    definition: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> bool {
    if arguments.len() != definition.parameters().len() {
        return false;
    }
    let mut seen = HashSet::with_capacity(arguments.len());
    arguments.iter().all(|argument| {
        seen.insert(argument.parameter())
            && definition
                .parameter_by_id(argument.parameter())
                .is_some_and(|parameter| {
                    execution::runtime_value_matches(
                        active,
                        argument.value(),
                        parameter.resolved_type(),
                    )
                })
    })
}

fn validate_resource_arguments(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
    arguments: &[FunctionArgument],
) -> Result<Vec<FunctionArgument>, ClientResourceError> {
    let Some(resolved) = resolve_resource_target(active, target) else {
        return Err(ClientResourceError::TargetMismatch { expected: target });
    };
    let definition = resolved.definition;
    let arguments = canonical_resource_arguments(arguments)?;
    for argument in &arguments {
        let Some(parameter) = definition
            .parameters()
            .iter()
            .find(|candidate| candidate.id() == argument.parameter())
        else {
            return Err(ClientResourceError::UnknownArgument {
                parameter: argument.parameter(),
            });
        };
        if !execution::runtime_value_matches(active, argument.value(), parameter.resolved_type()) {
            return Err(ClientResourceError::TypeMismatch);
        }
    }
    for parameter in definition.parameters() {
        if !arguments
            .iter()
            .any(|argument| argument.parameter() == parameter.id())
        {
            return Err(ClientResourceError::MissingArgument {
                parameter: parameter.id(),
            });
        }
    }
    Ok(arguments)
}

fn canonical_resource_argument_digest(
    active: &ActiveDatabaseRevision,
    arguments: &[FunctionArgument],
) -> Result<Sha256Digest, ClientResourceError> {
    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-arguments/v1\0");
    let argument_count =
        u32::try_from(arguments.len()).map_err(|_| ClientResourceError::ArgumentEncoding)?;
    hasher.update(argument_count.to_be_bytes());
    for argument in arguments {
        let frame = ClientFrame::CallArgument {
            stream: 1,
            parameter: argument.parameter(),
            value: argument.value().clone(),
        };
        let encoded = encode_active_client_frame(active, &frame)
            .map_err(|_| ClientResourceError::ArgumentEncoding)?;
        let encoded_length =
            u32::try_from(encoded.len()).map_err(|_| ClientResourceError::ArgumentEncoding)?;
        hasher.update(encoded_length.to_be_bytes());
        hasher.update(encoded);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

const DEFAULT_DATA_INVALIDATION_TOKEN: Sha256Digest = Sha256Digest::from_bytes([0; 32]);
const DEFAULT_SECURITY_CONTEXT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([0; 32]);

/// Derives the local cache identity for the authenticated security context.
///
/// The role list is sorted defensively even though AuthorisedInvocation
/// already exposes roles in canonical order. This keeps the digest stable for
/// every trusted invocation representation and makes the ordering contract
/// explicit at this cache boundary. The immutable authorization snapshot
/// digest is appended after these retained invocation bindings.
fn security_context_digest(authorisation: &AuthorisedInvocation) -> Sha256Digest {
    let mut roles = authorisation.active_roles().to_vec();
    roles.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-security-context/v2\0");
    hasher.update(authorisation.session_principal().to_bytes());
    hasher.update(authorisation.effective_principal().to_bytes());
    hasher.update(authorisation.authorising_principal().to_bytes());
    hasher.update((roles.len() as u64).to_be_bytes());
    for role in roles {
        hasher.update(role.to_bytes());
    }
    hasher.update(authorisation.security_context_digest().to_bytes());
    Sha256Digest::from_bytes(hasher.finalize().into())
}

/// Returns the derived security-context digest used by CLIENT evaluation.
///
/// Hosts that preload reference objects must bind their loader with this
/// digest from the same authorised invocation that will be evaluated.
pub fn client_security_context_digest(authorisation: &AuthorisedInvocation) -> Sha256Digest {
    security_context_digest(authorisation)
}

/// Combines the active catalogue, host data epoch, security context, root state context, and USER mutation epoch into
/// one local-only invalidation identity. None of these bytes are transport
/// fields; only the resulting key selects a local resource cache entry.
fn resource_invalidation_identity(
    catalogue_hash: Sha256Digest,
    data_invalidation_token: Sha256Digest,
    security_digest: Sha256Digest,
    state_context: &ClientStateContext,
    user_state_epoch: u64,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-invalidation/v2\0");
    hasher.update(catalogue_hash.to_bytes());
    hasher.update(data_invalidation_token.to_bytes());
    hasher.update(security_digest.to_bytes());
    hasher.update(state_context.root_function().to_bytes());
    hasher.update((state_context.state_profile().len() as u64).to_be_bytes());
    hasher.update(state_context.state_profile().as_bytes());
    hasher.update((state_context.instance_key().len() as u64).to_be_bytes());
    hasher.update(state_context.instance_key().as_bytes());
    hasher.update(user_state_epoch.to_be_bytes());
    Sha256Digest::from_bytes(hasher.finalize().into())
}

fn validate_state_text(value: &str, field: &'static str) -> Result<(), ClientStateIdentityError> {
    if value.contains('\0') {
        return Err(ClientStateIdentityError::InvalidText { field });
    }
    Ok(())
}

/// A CLIENT state context or key contains text that cannot cross the state
/// service boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientStateIdentityError {
    /// One profile or instance component contains a NUL byte.
    InvalidText {
        /// The rejected logical component.
        field: &'static str,
    },
}

impl fmt::Display for ClientStateIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidText { field } => write!(formatter, "{field} must not contain a NUL byte"),
        }
    }
}

impl Error for ClientStateIdentityError {}

/// The root invocation context used to address CLIENT state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientStateContext {
    root_function: FunctionId,
    state_profile: String,
    instance_key: String,
    data_invalidation_token: Sha256Digest,
}

impl ClientStateContext {
    /// Creates one state context. Empty profile and instance values select
    /// their default identities. The host data invalidation token defaults to
    /// the all-zero digest for compatibility with existing callers.
    pub fn new(
        root_function: FunctionId,
        state_profile: String,
        instance_key: String,
    ) -> Result<Self, ClientStateIdentityError> {
        Self::new_with_data_invalidation_token(
            root_function,
            state_profile,
            instance_key,
            DEFAULT_DATA_INVALIDATION_TOKEN,
        )
    }

    /// Creates one state context with a host-owned data invalidation token.
    pub fn new_with_data_invalidation_token(
        root_function: FunctionId,
        state_profile: String,
        instance_key: String,
        data_invalidation_token: Sha256Digest,
    ) -> Result<Self, ClientStateIdentityError> {
        validate_state_text(&state_profile, "state profile")?;
        validate_state_text(&instance_key, "instance key")?;
        Ok(Self {
            root_function,
            state_profile,
            instance_key,
            data_invalidation_token,
        })
    }

    /// Creates the default context for one root function.
    pub fn default_for(root_function: FunctionId) -> Self {
        Self {
            root_function,
            state_profile: String::new(),
            instance_key: String::new(),
            data_invalidation_token: DEFAULT_DATA_INVALIDATION_TOKEN,
        }
    }

    /// Sets the host-owned data invalidation token used by local resources.
    pub fn set_data_invalidation_token(&mut self, data_invalidation_token: Sha256Digest) {
        self.data_invalidation_token = data_invalidation_token;
    }

    /// Returns the host-owned data invalidation token.
    pub const fn data_invalidation_token(&self) -> Sha256Digest {
        self.data_invalidation_token
    }

    /// Returns the root function identity.
    pub const fn root_function(&self) -> FunctionId {
        self.root_function
    }

    /// Returns the root state profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the mounted root instance key.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }
}

/// One host-preloaded durable object used by a CLIENT reference field path.
///
/// The host supplies the object's stable type and object identities together
/// with a declaration-ordered subset of fields. The client validates each
/// supplied field against the active catalogue before exposing its value to an
/// expression; an omitted requested field remains unavailable.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientReferenceObject {
    target: TypeId,
    object: ObjectId,
    fields: Vec<(FieldId, RuntimeValue)>,
}

impl ClientReferenceObject {
    /// Creates one host-preloaded object record.
    pub fn new(target: TypeId, object: ObjectId, fields: Vec<(FieldId, RuntimeValue)>) -> Self {
        Self {
            target,
            object,
            fields,
        }
    }

    /// Returns the stable object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the stable durable object identity.
    pub const fn object(&self) -> ObjectId {
        self.object
    }

    /// Returns fields in the host-supplied declaration order.
    pub fn fields(&self) -> &[(FieldId, RuntimeValue)] {
        &self.fields
    }
}

/// A typed error raised while constructing a host-preloaded reference loader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientReferenceLoaderError {
    /// More than one supplied object has the same type and object identity.
    DuplicateIdentity {
        /// The repeated object's stable type identity.
        target: TypeId,
        /// The repeated object's stable durable identity.
        object: ObjectId,
    },
}

impl fmt::Display for ClientReferenceLoaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateIdentity { target, object } => write!(
                formatter,
                "CLIENT reference loader contains duplicate object identity ({target}, {object})"
            ),
        }
    }
}

impl Error for ClientReferenceLoaderError {}

/// A host-preloaded, authenticated CLIENT reference-object loader.
///
/// This value contains no storage or transport capability. It is a snapshot
/// of objects already loaded by the authenticated host, bound to one active
/// revision, principal, and derived security-context digest.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientReferenceLoader {
    revision: RevisionPair,
    principal: PrincipalId,
    security_context_digest: Sha256Digest,
    objects: HashMap<(TypeId, ObjectId), ClientReferenceObject>,
}

impl ClientReferenceLoader {
    /// Creates a preloaded loader bound to one authenticated invocation context.
    ///
    /// Every `(TypeId, ObjectId)` identity must occur at most once. Rejecting
    /// duplicates keeps host input deterministic instead of silently retaining
    /// whichever object happened to be supplied last.
    pub fn new(
        revision: RevisionPair,
        principal: PrincipalId,
        security_context_digest: Sha256Digest,
        objects: impl IntoIterator<Item = ClientReferenceObject>,
    ) -> Result<Self, ClientReferenceLoaderError> {
        let mut indexed = HashMap::new();
        for object in objects {
            let identity = (object.target(), object.object());
            if indexed.insert(identity, object).is_some() {
                return Err(ClientReferenceLoaderError::DuplicateIdentity {
                    target: identity.0,
                    object: identity.1,
                });
            }
        }
        Ok(Self {
            revision,
            principal,
            security_context_digest,
            objects: indexed,
        })
    }

    fn load(
        &self,
        active: &ActiveDatabaseRevision,
        principal: PrincipalId,
        security_context_digest: Sha256Digest,
        reference: &RuntimeValue,
    ) -> Option<&ClientReferenceObject> {
        if self.revision != active.pair()
            || self.principal != principal
            || self.security_context_digest != security_context_digest
        {
            return None;
        }
        let RuntimeValue::Reference { target, object } = reference else {
            return None;
        };
        active.catalogue().object_type_by_id(*target)?;
        self.objects.get(&(*target, *object))
    }
}

fn client_reference_object_is_active(
    active: &ActiveDatabaseRevision,
    target: TypeId,
    object: ObjectId,
    value: &ClientReferenceObject,
) -> bool {
    if value.target() != target || value.object() != object {
        return false;
    }
    let Some(definition) = active.catalogue().object_type_by_id(target) else {
        return false;
    };
    let mut previous_index = None;
    for (field_id, field_value) in value.fields() {
        let Some((index, field)) = definition
            .fields()
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.id() == *field_id)
        else {
            return false;
        };
        if previous_index.is_some_and(|previous| index <= previous) {
            return false;
        }
        if (field_value.is_null() && !field.nullable())
            || !client_reference_field_value_matches(active, field_value, field.resolved_type())
        {
            return false;
        }
        previous_index = Some(index);
    }
    true
}

fn client_reference_field_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: ResolvedType,
) -> bool {
    if execution::runtime_value_matches(active, value, expected) {
        return true;
    }
    let ResolvedType::Named(type_id) = expected else {
        return false;
    };
    let Some(definition) = active
        .catalogue_hash_context()
        .standard()
        .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
    else {
        return false;
    };
    if let RuntimeValue::Null(null) = value {
        return null.resolved_type() == expected;
    }
    match definition.representation_contract() {
        "orna.kernel.value.boolean@1" => {
            execution::runtime_scalar_matches(StandardScalar::Boolean, value)
        }
        "orna.kernel.value.integer@1" => {
            execution::runtime_scalar_matches(StandardScalar::Integer, value)
        }
        "orna.kernel.value.bigint@1" => {
            execution::runtime_scalar_matches(StandardScalar::BigInt, value)
        }
        "orna.kernel.value.float@1" => {
            execution::runtime_scalar_matches(StandardScalar::Float, value)
        }
        "orna.kernel.value.character-large-object@1" => {
            execution::runtime_scalar_matches(StandardScalar::CharacterLargeObject, value)
        }
        "orna.kernel.value.binary-large-object@1" => {
            execution::runtime_scalar_matches(StandardScalar::BinaryLargeObject, value)
        }
        _ => false,
    }
}

/// A private, deterministic object-loader seam used by the CLIENT evaluator.
///
/// Durable object storage is owned by the authenticated host, not by this
/// crate. Keeping this fixture private lets tests exercise reference-root
/// paths without introducing a public storage contract. The evaluator binds
/// the fixture to the active revision and the derived authenticated security
/// context before every lookup; source expressions contribute only the
/// reference identity itself.
#[derive(Clone, Debug, PartialEq)]
struct ClientReferenceLoaderFixture {
    revision: RevisionPair,
    principal: PrincipalId,
    security_context_digest: Sha256Digest,
    objects: HashMap<(TypeId, ObjectId), RuntimeValue>,
}

impl ClientReferenceLoaderFixture {
    fn load(
        &self,
        active: &ActiveDatabaseRevision,
        principal: PrincipalId,
        security_context_digest: Sha256Digest,
        reference: &RuntimeValue,
    ) -> Option<RuntimeValue> {
        if self.revision != active.pair()
            || self.principal != principal
            || self.security_context_digest != security_context_digest
        {
            return None;
        }
        let RuntimeValue::Reference { target, object } = reference else {
            return None;
        };
        let value = self.objects.get(&(*target, *object))?;
        reference_record_is_active_and_matches_target(active, *target, value).then(|| value.clone())
    }
}

fn reference_record_is_active_and_matches_target(
    active: &ActiveDatabaseRevision,
    target: TypeId,
    value: &RuntimeValue,
) -> bool {
    let RuntimeValue::Record(record) = value else {
        return false;
    };
    let Some(object) = active.catalogue().object_type_by_id(target) else {
        return false;
    };
    let Some(record_definition) = active
        .catalogue()
        .record_value_type_by_id(record.record_type())
    else {
        return false;
    };
    if object.fields().len() != record_definition.fields().len()
        || record.fields().len() != object.fields().len()
    {
        return false;
    }
    if object
        .fields()
        .iter()
        .zip(record_definition.fields())
        .any(|(object_field, record_field)| {
            object_field.id() != record_field.id()
                || object_field.ordinal() != record_field.ordinal()
        })
    {
        return false;
    }
    encode_active_value(active, value).is_ok()
}

/// One state slot of one CLIENT function inside an in-memory state store.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClientStateKey {
    root_function: FunctionId,
    state_profile: String,
    function: FunctionId,
    instance_key: String,
    slot: StateSlotId,
}

impl ClientStateKey {
    /// Creates a key in the default root context.
    pub fn new(function: FunctionId, slot: StateSlotId) -> Self {
        Self::from_context(&ClientStateContext::default_for(function), function, slot)
    }

    /// Creates a key from a root context and the owning function.
    pub fn from_context(
        context: &ClientStateContext,
        function: FunctionId,
        slot: StateSlotId,
    ) -> Self {
        Self {
            root_function: context.root_function,
            state_profile: context.state_profile.clone(),
            function,
            instance_key: context.instance_key.clone(),
            slot,
        }
    }

    /// Creates a key from one durable USER state cell.
    pub fn from_user_cell(cell: &UserStateCell) -> Self {
        let key = cell.key();
        Self {
            root_function: key.root_function(),
            state_profile: key.state_profile().to_owned(),
            function: key.function(),
            instance_key: key.instance_key().to_owned(),
            slot: key.state_slot(),
        }
    }

    /// Creates a key from a server write change.
    fn from_user_change(change: &UserStateChange) -> Self {
        let key = change.key_without_principal();
        Self {
            root_function: key.root_function(),
            state_profile: key.state_profile().to_owned(),
            function: key.function(),
            instance_key: key.instance_key().to_owned(),
            slot: key.state_slot(),
        }
    }

    /// Creates a key from a server result key.
    fn from_user_key(key: &UserStateKeyWithoutPrincipal) -> Self {
        Self {
            root_function: key.root_function(),
            state_profile: key.state_profile().to_owned(),
            function: key.function(),
            instance_key: key.instance_key().to_owned(),
            slot: key.state_slot(),
        }
    }

    /// Returns the root function identity.
    pub const fn root_function(&self) -> FunctionId {
        self.root_function
    }

    /// Returns the root state profile.
    pub fn state_profile(&self) -> &str {
        &self.state_profile
    }

    /// Returns the owning function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the function-instance key.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }

    /// Returns the durable state-slot identity.
    pub const fn slot(&self) -> StateSlotId {
        self.slot
    }
}

/// One loaded or locally updated USER state value.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientUserState {
    value: RuntimeValue,
    value_type: TypeId,
    revision: Option<u64>,
    dirty: bool,
}

impl ClientUserState {
    fn loaded(cell: &UserStateCell) -> Self {
        Self {
            value: cell.value().clone(),
            value_type: cell.value_type(),
            revision: Some(cell.revision()),
            dirty: false,
        }
    }

    fn local(value: RuntimeValue, value_type: TypeId, revision: Option<u64>) -> Self {
        Self {
            value,
            value_type,
            revision,
            dirty: true,
        }
    }
    fn defaulted(value: RuntimeValue, value_type: TypeId) -> Self {
        Self {
            value,
            value_type,
            revision: None,
            dirty: false,
        }
    }

    /// Returns the current local value.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }

    /// Returns the persisted value type.
    pub const fn value_type(&self) -> TypeId {
        self.value_type
    }

    /// Returns the acknowledged server revision, or `None` for a new cell.
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// Returns whether the value needs a server flush.
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }
}
/// The explicit in-memory CLIENT state for one invocation session
/// (work ADRs 0069 and 0070).
///
/// The local USER cache is explicitly bound to one authenticated session.
/// The binding is opaque and is cleared only with the store itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientStateStore {
    context: ClientStateContext,
    /// The opaque authenticated-session binding for the local USER cache.
    session_binding: Option<AuthenticatedSessionBinding>,
    security_context_digest: Sha256Digest,
    /// Monotonic local epoch that changes whenever USER state is explicitly
    /// mutated. Resource keys include this epoch so READY results cannot be
    /// reused after a state update.
    user_state_epoch: u64,
    reference_loader: Option<ClientReferenceLoaderFixture>,
    installed_reference_loader: Option<ClientReferenceLoader>,
    local: HashMap<ClientStateKey, RuntimeValue>,
    session: HashMap<ClientStateKey, RuntimeValue>,
    user: HashMap<ClientStateKey, ClientUserState>,
    resources: HashMap<ClientResourceKey, ClientResource>,
}

impl Default for ClientStateStore {
    fn default() -> Self {
        Self {
            context: ClientStateContext::default_for(FunctionId::from_bytes([0; 16])),
            session_binding: None,
            security_context_digest: DEFAULT_SECURITY_CONTEXT_DIGEST,
            user_state_epoch: 0,
            reference_loader: None,
            installed_reference_loader: None,
            local: HashMap::new(),
            session: HashMap::new(),
            user: HashMap::new(),
            resources: HashMap::new(),
        }
    }
}

impl ClientStateStore {
    /// Creates one empty in-memory CLIENT state store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects the root context used for subsequent evaluator state keys.
    pub fn set_context(&mut self, context: ClientStateContext) {
        self.context = context;
    }

    /// Binds the local USER cache to one authenticated session.
    ///
    /// The first binding is retained. A different session is rejected without
    /// clearing or changing any caller-owned USER values.
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn bind_authenticated_session(
        &mut self,
        binding: AuthenticatedSessionBinding,
    ) -> Result<(), ClientUserStateError> {
        if let Some(expected) = self.session_binding
            && expected != binding
        {
            return Err(ClientUserStateError::SessionMismatch);
        }
        self.session_binding = Some(binding);
        Ok(())
    }

    /// Installs a host-preloaded loader for authenticated reference field paths.
    pub fn install_reference_loader(&mut self, loader: ClientReferenceLoader) {
        self.installed_reference_loader = Some(loader);
    }

    /// Installs a private fixture for trusted reference-root evaluation tests.
    #[cfg(test)]
    fn set_reference_loader_fixture(&mut self, fixture: ClientReferenceLoaderFixture) {
        self.reference_loader = Some(fixture);
    }

    /// Returns the selected root state context.
    pub const fn context(&self) -> &ClientStateContext {
        &self.context
    }

    /// Refreshes the invocation-derived security identity for local resources.
    fn set_security_context_digest(&mut self, digest: Sha256Digest) {
        self.security_context_digest = digest;
    }

    /// Returns the invocation-derived security identity for local resources.
    fn security_context_digest(&self) -> Sha256Digest {
        self.security_context_digest
    }

    /// Returns the local USER-state mutation epoch used by resource keys.
    fn user_state_epoch(&self) -> u64 {
        self.user_state_epoch
    }

    /// Creates one state key in the selected root context.
    fn key_for(&self, function: FunctionId, slot: StateSlotId) -> ClientStateKey {
        ClientStateKey::from_context(&self.context, function, slot)
    }

    /// Returns the resource for one complete cache identity.
    pub fn resource(&self, key: ClientResourceKey) -> Option<&ClientResource> {
        self.resources.get(&key)
    }

    /// Returns mutable access to one cached resource.
    pub fn resource_mut(&mut self, key: ClientResourceKey) -> Option<&mut ClientResource> {
        self.resources.get_mut(&key)
    }
    /// Retains a resource whose executor ownership was handed back to the
    /// caller after a nested action failure.
    fn retain_resource(&mut self, resource: ClientResource) {
        self.resources.insert(resource.key(), resource);
    }

    /// Returns the existing resource, or creates one with its first declared type.
    ///
    /// A repeated lookup does not replace the cached resource or its expected
    /// type. The complete key is the cache boundary, so callers must use a new
    /// key when the target, principal, arguments, or invalidation token changes.
    pub fn get_or_create_resource(
        &mut self,
        key: ClientResourceKey,
        expected_type: ResolvedType,
    ) -> &mut ClientResource {
        self.get_or_create_resource_with_kind(key, ResourceKind::Scalar, expected_type)
    }

    /// Returns or creates a resource with an explicit scalar/stream kind.
    pub fn get_or_create_resource_with_kind(
        &mut self,
        key: ClientResourceKey,
        kind: ResourceKind,
        expected_type: ResolvedType,
    ) -> &mut ClientResource {
        match self.resources.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                entry.insert(ClientResource::new_with_kind(key, kind, expected_type))
            }
        }
    }

    /// Invalidates one cached resource without removing its identity.
    ///
    /// The generation advances and any published value or failure is cleared.
    /// An absent key is already invalidated and returns `false`. This direct
    /// entrypoint intentionally does not touch an executor; use
    /// [`Self::invalidate_resource_with_executor`] when the runtime owns an
    /// active request for this resource.
    pub fn invalidate_resource(
        &mut self,
        key: ClientResourceKey,
    ) -> Result<bool, ClientResourceError> {
        let Some(resource) = self.resources.get_mut(&key) else {
            return Ok(false);
        };
        resource.invalidate()?;
        Ok(true)
    }

    /// Invalidates one cached resource and asks the owning executor to cancel
    /// its active request before the local generation changes.
    ///
    /// A terminal completion returned before cancellation wins and remains
    /// published at the current generation. A non-terminal completion is
    /// validated and abandoned before invalidation can advance the generation.
    /// This keeps cancellation in the owning runtime while preserving terminal
    /// ordering.
    pub fn invalidate_resource_with_executor(
        &mut self,
        active: &ActiveDatabaseRevision,
        key: ClientResourceKey,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<bool, ClientResourceError> {
        let Some(resource) = self.resources.get_mut(&key) else {
            return Ok(false);
        };
        resource.invalidate_with_executor(active, executor)?;
        Ok(true)
    }

    /// Returns or creates a resource while cancelling active resources whose
    /// complete key is being replaced by a new invalidation identity.
    ///
    /// The ordinary [`Self::get_or_create_resource_with_kind`] method remains
    /// independent-key and executor-free for compatibility. Evaluator code
    /// uses this ownership-aware path so a dependency change cannot leave the
    /// previous executor-owned request running.
    pub fn get_or_create_resource_with_kind_and_executor(
        &mut self,
        active: &ActiveDatabaseRevision,
        key: ClientResourceKey,
        kind: ResourceKind,
        expected_type: ResolvedType,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<&mut ClientResource, ClientResourceError> {
        let replacements: Vec<_> = self
            .resources
            .iter()
            .filter_map(|(candidate_key, candidate)| {
                (*candidate_key != key
                    && candidate_key.replacement_slot_matches(key)
                    && candidate.status() == ClientResourceStatus::Loading)
                    .then_some(*candidate_key)
            })
            .collect();
        for replacement in replacements {
            if replacement.target().revision() == active.pair() {
                self.invalidate_resource_with_executor(active, replacement, executor)?;
            } else {
                let resource = self
                    .resources
                    .get_mut(&replacement)
                    .expect("replacement key was collected from the resource cache");
                resource.invalidate_stale_replacement_with_executor(executor)?;
            }
        }
        Ok(self.get_or_create_resource_with_kind(key, kind, expected_type))
    }

    /// Returns or creates a scalar resource through the executor-aware key
    /// replacement path.
    pub fn get_or_create_resource_with_executor(
        &mut self,
        active: &ActiveDatabaseRevision,
        key: ClientResourceKey,
        expected_type: ResolvedType,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<&mut ClientResource, ClientResourceError> {
        self.get_or_create_resource_with_kind_and_executor(
            active,
            key,
            ResourceKind::Scalar,
            expected_type,
            executor,
        )
    }

    /// Returns the `LOCAL` slot values of one mounted function instance.
    pub fn local(&self) -> &HashMap<ClientStateKey, RuntimeValue> {
        &self.local
    }

    /// Returns mutable access to the `LOCAL` slot values.
    pub fn local_mut(&mut self) -> &mut HashMap<ClientStateKey, RuntimeValue> {
        &mut self.local
    }

    /// Returns the `SESSION` slot values of one client invocation session.
    pub fn session(&self) -> &HashMap<ClientStateKey, RuntimeValue> {
        &self.session
    }

    /// Returns mutable access to the `SESSION` slot values.
    pub fn session_mut(&mut self) -> &mut HashMap<ClientStateKey, RuntimeValue> {
        &mut self.session
    }

    /// Returns loaded and locally updated `USER` slot values.
    pub fn user(&self) -> &HashMap<ClientStateKey, ClientUserState> {
        &self.user
    }
}

/// A USER state store rejected a lifecycle operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientUserStateError {
    /// A caller-owned USER cache was presented to a different authenticated session.
    SessionMismatch,
    /// A USER state batch contained the same logical cell more than once.
    DuplicateKey(ClientStateKey),
    /// A USER state key is outside the selected root context.
    ContextMismatch(ClientStateKey),
    /// A transport result did not align with the submitted change batch.
    WriteBatchLength { expected: usize, actual: usize },
    /// A transport result named a different cell from its change.
    WriteKeyMismatch {
        /// The submitted change key.
        expected: ClientStateKey,
        /// The returned result key.
        actual: ClientStateKey,
    },
    /// A write result named a cell that is not in the local store.
    UnknownKey(ClientStateKey),
    /// A write result did not describe the current dirty local value.
    ValueMismatch(ClientStateKey),
    /// The server reported a revision conflict.
    Conflict {
        /// The conflicted logical cell.
        key: ClientStateKey,
        /// The revision sent by the client.
        expected: Option<u64>,
        /// The revision currently held by the server.
        current: u64,
    },
    /// A successful write returned an invalid revision transition.
    InvalidRevision(ClientStateKey),
    /// The state change could not be constructed from the local key.
    InvalidChange(String),
    /// A state context contains invalid text.
    InvalidIdentity(ClientStateIdentityError),
}

impl fmt::Display for ClientUserStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionMismatch => {
                formatter.write_str("USER state cache belongs to a different authenticated session")
            }
            Self::DuplicateKey(key) => {
                write!(formatter, "USER state batch contains duplicate key {key:?}")
            }
            Self::ContextMismatch(key) => write!(
                formatter,
                "USER state key is outside the selected context {key:?}"
            ),
            Self::WriteBatchLength { expected, actual } => write!(
                formatter,
                "USER state write result count {actual} does not match change count {expected}",
            ),
            Self::WriteKeyMismatch { expected, actual } => {
                write!(
                    formatter,
                    "USER state write result key {actual:?} does not match {expected:?}"
                )
            }
            Self::UnknownKey(key) => write!(
                formatter,
                "USER state write result names unknown key {key:?}"
            ),
            Self::ValueMismatch(key) => {
                write!(
                    formatter,
                    "USER state write result does not match local value for {key:?}"
                )
            }
            Self::Conflict {
                key,
                expected,
                current,
            } => write!(
                formatter,
                "USER state revision conflict for {key:?}: expected {expected:?}, current {current}",
            ),
            Self::InvalidRevision(key) => {
                write!(
                    formatter,
                    "USER state write returned an invalid revision for {key:?}"
                )
            }
            Self::InvalidChange(reason) => {
                write!(formatter, "USER state change is invalid: {reason}")
            }
            Self::InvalidIdentity(source) => source.fmt(formatter),
        }
    }
}

impl Error for ClientUserStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidIdentity(source) => Some(source),
            _ => None,
        }
    }
}

impl ClientStateStore {
    /// Loads one complete USER state batch for the selected root function and
    /// profile. The batch may contain multiple function-instance keys; the
    /// authenticated adapter binds the store before transport access, and the
    /// store itself carries no principal identity. Existing cells in that root
    /// context are replaced; cells for other contexts remain available.
    /// Single-instance updates remain enforced by [`Self::set_user_state`].
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn load_user_state(&mut self, cells: &[UserStateCell]) -> Result<(), ClientUserStateError> {
        let context = &self.context;
        let mut loaded = HashMap::with_capacity(cells.len());
        for cell in cells {
            let key = ClientStateKey::from_user_cell(cell);
            if key.root_function() != context.root_function()
                || key.state_profile() != context.state_profile()
            {
                return Err(ClientUserStateError::ContextMismatch(key));
            }
            if loaded
                .insert(key.clone(), ClientUserState::loaded(cell))
                .is_some()
            {
                return Err(ClientUserStateError::DuplicateKey(key));
            }
        }
        self.user.retain(|key, _| {
            key.root_function() != context.root_function()
                || key.state_profile() != context.state_profile()
        });
        self.user.extend(loaded);
        Ok(())
    }

    /// Loads USER cells for selected function-instance pairs in one root context.
    ///
    /// Unlike [`Self::load_user_state`], this filtered operation replaces only
    /// the requested instances. Existing cells for other instances, including
    /// dirty values, remain untouched; requested instances absent from `cells`
    /// are removed after the complete batch passes validation.
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn load_user_state_for_instances(
        &mut self,
        cells: &[UserStateCell],
        requested_instances: &[(FunctionId, String)],
    ) -> Result<(), ClientUserStateError> {
        if requested_instances.is_empty() {
            return self.load_user_state(cells);
        }
        for (_, instance_key) in requested_instances {
            validate_state_text(instance_key, "instance key")
                .map_err(ClientUserStateError::InvalidIdentity)?;
        }
        let context = &self.context;
        let requested: HashMap<(FunctionId, String), ()> = requested_instances
            .iter()
            .map(|(function, instance_key)| ((*function, instance_key.clone()), ()))
            .collect();
        let mut loaded = HashMap::with_capacity(cells.len());
        for cell in cells {
            let key = ClientStateKey::from_user_cell(cell);
            if key.root_function() != context.root_function()
                || key.state_profile() != context.state_profile()
                || !requested.contains_key(&(key.function(), key.instance_key().to_owned()))
            {
                return Err(ClientUserStateError::ContextMismatch(key));
            }
            if loaded
                .insert(key.clone(), ClientUserState::loaded(cell))
                .is_some()
            {
                return Err(ClientUserStateError::DuplicateKey(key));
            }
        }
        self.user.retain(|key, _| {
            key.root_function() != context.root_function()
                || key.state_profile() != context.state_profile()
                || !requested.contains_key(&(key.function(), key.instance_key().to_owned()))
        });
        self.user.extend(loaded);
        Ok(())
    }

    /// Updates one USER value and marks it for the next explicit flush.
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn set_user_state(
        &mut self,
        key: ClientStateKey,
        value: RuntimeValue,
        value_type: TypeId,
    ) -> Result<(), ClientUserStateError> {
        if key.root_function() != self.context.root_function()
            || key.state_profile() != self.context.state_profile()
            || key.instance_key() != self.context.instance_key()
        {
            return Err(ClientUserStateError::ContextMismatch(key));
        }
        if let Some(existing) = self.user.get(&key)
            && existing.value_type != value_type
        {
            return Err(ClientUserStateError::ValueMismatch(key));
        }
        let next_epoch = self.user_state_epoch.checked_add(1).ok_or_else(|| {
            ClientUserStateError::InvalidChange(
                "USER state invalidation epoch exhausted".to_owned(),
            )
        })?;
        let revision = self.user.get(&key).and_then(ClientUserState::revision);
        self.user
            .insert(key, ClientUserState::local(value, value_type, revision));
        self.user_state_epoch = next_epoch;
        Ok(())
    }

    /// Returns dirty USER values as one deterministic change batch.
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn pending_user_state_changes(&self) -> Result<Vec<UserStateChange>, ClientUserStateError> {
        let mut pending = self
            .user
            .iter()
            .filter(|(_, value)| value.dirty)
            .map(|(key, value)| {
                UserStateChange::new(
                    key.root_function,
                    key.state_profile.clone(),
                    key.function,
                    key.instance_key.clone(),
                    key.slot,
                    value.revision,
                    value.value.clone(),
                    value.value_type,
                )
                .map(|change| (key, change))
                .map_err(|error| ClientUserStateError::InvalidChange(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        pending.sort_by_key(|(left, _)| *left);
        Ok(pending.into_iter().map(|(_, change)| change).collect())
    }

    /// Applies one aligned server write-result batch.
    // ClientUserStateError preserves both keys in its public mismatch diagnostic.
    #[allow(clippy::result_large_err)]
    pub fn apply_user_state_write_results(
        &mut self,
        changes: &[UserStateChange],
        results: &[UserStateWriteResult],
    ) -> Result<(), ClientUserStateError> {
        if changes.len() != results.len() {
            return Err(ClientUserStateError::WriteBatchLength {
                expected: changes.len(),
                actual: results.len(),
            });
        }
        let mut seen_keys = HashMap::with_capacity(changes.len());
        for (change, result) in changes.iter().zip(results) {
            let expected_key = ClientStateKey::from_user_change(change);
            let actual_key = ClientStateKey::from_user_key(result.key());
            if expected_key != actual_key {
                return Err(ClientUserStateError::WriteKeyMismatch {
                    expected: expected_key,
                    actual: actual_key,
                });
            }
            if seen_keys.insert(expected_key.clone(), ()).is_some() {
                return Err(ClientUserStateError::DuplicateKey(expected_key));
            }
            let Some(local) = self.user.get(&expected_key) else {
                return Err(ClientUserStateError::UnknownKey(expected_key));
            };
            if !local.dirty
                || local.revision != change.expected_revision()
                || local.value != *change.value()
                || local.value_type != change.value_type()
            {
                return Err(ClientUserStateError::ValueMismatch(expected_key));
            }
        }
        let mut staged_writes = Vec::with_capacity(changes.len());
        let mut first_conflict = None;
        for (change, result) in changes.iter().zip(results) {
            let key = ClientStateKey::from_user_change(change);
            let local = self
                .user
                .get(&key)
                .expect("USER state key was validated above");
            match result.outcome() {
                UserStateWriteOutcome::Written { revision } => {
                    let expected_revision = local
                        .revision
                        .map_or(Some(1), |current| current.checked_add(1));
                    if expected_revision != Some(revision) {
                        return Err(ClientUserStateError::InvalidRevision(key));
                    }
                    staged_writes.push((key, revision));
                }
                UserStateWriteOutcome::Conflict { current_revision } => {
                    if first_conflict.is_none() {
                        first_conflict = Some(ClientUserStateError::Conflict {
                            key,
                            expected: change.expected_revision(),
                            current: current_revision,
                        });
                    }
                }
            }
        }
        if let Some(error) = first_conflict {
            return Err(error);
        }
        for (key, revision) in staged_writes {
            let local = self
                .user
                .get_mut(&key)
                .expect("USER state key was validated above");
            local.revision = Some(revision);
            local.dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod runtime_abi;

#[cfg(test)]
mod runtime_conformance;
