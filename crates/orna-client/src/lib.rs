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
            return Err(Box::new(expression_error(
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
        if !active_supports_invocation_target(active, key.target()) {
            return Err(ClientResourceError::TargetMismatch {
                expected: key.target(),
            });
        }
        if !active_resource_result_type_matches(active, key.target(), kind, expected_type) {
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
        let target_invocation_id = operation.target().and_then(inspect_invocation_target);
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
            target_supported: active_supports_invocation_target(active, target),
            result_type_supported: active_resource_result_type_matches(
                active,
                target,
                kind,
                expected_type,
            ),
            expected_type_known: active_type_is_known(active, expected_type),
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
            ClientResourceValueKind::Scalar(scalar) => runtime_scalar_matches(*scalar, value),
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
                    inspect_invocation_target(value).is_some()
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
                if is_inspect_carrier_type(type_id) {
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
                if is_inspect_carrier_type(type_id) {
                    return Self::InspectCarrier(type_id);
                }
                if active_has_record_type(active, type_id) {
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
                    || active_has_object_type(active, target)
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
        active_supports_invocation_target(self, target)
    }

    fn result_type_supported(
        &self,
        target: InvocationTarget,
        kind: ResourceKind,
        expected: ResolvedType,
    ) -> bool {
        active_resource_result_type_matches(self, target, kind, expected)
    }

    fn value_matches(&self, value: &RuntimeValue, expected: ResolvedType) -> bool {
        runtime_value_matches(self, value, expected)
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
    match artifact.version() {
        EXPRESSION_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Expression,
        PROCEDURAL_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Procedural,
        orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION => {
            orna_core::source_metadata::SourceBodyKind::ControlFlow
        }
        STATE_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::State,
        OPAQUE_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::ExternalContract,
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
        ResolvedType::Named(type_id) => (active_has_enum_type(active, type_id)
            || active_has_record_type(active, type_id))
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
            active_has_object_type(active, target).then_some(descriptor)
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
                    runtime_value_matches(active, argument.value(), parameter.resolved_type())
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
        if !runtime_value_matches(active, argument.value(), parameter.resolved_type()) {
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
    if runtime_value_matches(active, value, expected) {
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
        "orna.kernel.value.boolean@1" => runtime_scalar_matches(StandardScalar::Boolean, value),
        "orna.kernel.value.integer@1" => runtime_scalar_matches(StandardScalar::Integer, value),
        "orna.kernel.value.bigint@1" => runtime_scalar_matches(StandardScalar::BigInt, value),
        "orna.kernel.value.float@1" => runtime_scalar_matches(StandardScalar::Float, value),
        "orna.kernel.value.character-large-object@1" => {
            runtime_scalar_matches(StandardScalar::CharacterLargeObject, value)
        }
        "orna.kernel.value.binary-large-object@1" => {
            runtime_scalar_matches(StandardScalar::BinaryLargeObject, value)
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

/// An active-revision validation failure for local CLIENT execution.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientActiveRevisionError {
    /// Canonical active catalogue semantics could not be calculated.
    Canonical(CanonicalHashError),
    /// The recorded active catalogue digest differs from canonical semantics.
    CatalogueHashMismatch,
}

impl fmt::Display for ClientActiveRevisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canonical(source) => source.fmt(formatter),
            Self::CatalogueHashMismatch => formatter
                .write_str("active revision catalogue hash differs from its canonical semantics"),
        }
    }
}

impl Error for ClientActiveRevisionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Canonical(source) => Some(source),
            Self::CatalogueHashMismatch => None,
        }
    }
}

/// A registered opaque-value failure during local CLIENT evaluation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientOpaqueValueError {
    /// The checked-in registry does not accept the active standard snapshot.
    Registry(Box<RegisteredOpaqueCodecsError>),
    /// The plan's nominal type differs from the function's declared return type.
    TypeMismatch {
        /// The function's declared opaque return type.
        expected: TypeId,
        /// The opaque type encoded in the saved plan.
        actual: TypeId,
    },
    /// The registered codec rejected the plan value.
    Value(OpaqueValueError),
}

impl fmt::Display for ClientOpaqueValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(source) => source.fmt(formatter),
            Self::TypeMismatch { .. } => {
                formatter.write_str("opaque CLIENT plan type does not match its function return")
            }
            Self::Value(source) => source.fmt(formatter),
        }
    }
}

impl Error for ClientOpaqueValueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(source) => Some(source),
            Self::Value(source) => Some(source),
            Self::TypeMismatch { .. } => None,
        }
    }
}

/// A closed CLIENT-function validation rule.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientExecutionRule {
    /// The function does not use the CLIENT execution domain.
    FunctionDomain,
    /// The function declares unsupported parameters.
    Parameters,
    /// The function does not return a supported CLIENT value.
    ReturnType,
    /// The function does not use INVOKER security.
    Security,
    /// The function is not immutable.
    Volatility,
    /// The function has unsupported definition references.
    References,
    /// The saved artefact format is unsupported.
    ArtifactFormat,
    /// The saved artefact version is unsupported.
    ArtifactVersion,
    /// The saved language label is unsupported.
    LanguageVersion,
}

impl fmt::Display for ClientExecutionRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FunctionDomain => formatter.write_str("this function does not run on the client"),
            Self::Parameters => {
                formatter.write_str("this CLIENT function requires unsupported parameters")
            }
            Self::ReturnType => {
                formatter.write_str("this CLIENT function has an unsupported return type")
            }
            Self::Security => {
                formatter.write_str("this CLIENT function has an unsupported security mode")
            }
            Self::Volatility => {
                formatter.write_str("this CLIENT function is not an immutable constant")
            }
            Self::References => {
                formatter.write_str("this CLIENT function depends on unsupported definitions")
            }
            Self::ArtifactFormat => {
                formatter.write_str("the saved CLIENT function uses an unsupported artefact format")
            }
            Self::ArtifactVersion => formatter
                .write_str("the saved CLIENT function uses an unsupported artefact version"),
            Self::LanguageVersion => formatter
                .write_str("the saved CLIENT function uses an unsupported language version"),
        }
    }
}

impl Error for ClientExecutionRule {}

/// A closed CLIENT expression could not produce a value.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientExpressionError {
    /// An expression read a parameter that was not bound at invocation time.
    ParameterNotBound,
    /// An expression value did not match the declared parameter or return type.
    TypeMismatch,
    /// A call did not bind exactly the target's declared parameters.
    InvalidCall,
    /// A field path did not resolve against its record value.
    FieldPath,
    /// The closed call-depth limit was reached.
    RecursionLimit,
    /// A checked INTEGER arithmetic operation failed.
    Arithmetic,
    /// The per-root CLIENT execution fuel was exhausted.
    ExecutionLimit,
    /// A control-flow function reached its end without returning a value.
    MissingReturn,
    /// The active client session cannot provide input.
    InputUnavailable,
    /// The active session rejected a dynamic command evaluation.
    DynamicInvocation,
}

impl fmt::Display for ClientExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ParameterNotBound => "a CLIENT expression parameter was not bound",
            Self::TypeMismatch => "a CLIENT expression value has the wrong type",
            Self::InvalidCall => "a CLIENT expression call has invalid arguments",
            Self::FieldPath => "a CLIENT expression field path could not be resolved",
            Self::RecursionLimit => "the CLIENT expression call-depth limit was exceeded",
            Self::Arithmetic => "client.arithmetic_error",
            Self::ExecutionLimit => "client.execution_limit",
            Self::MissingReturn => "client.control_flow_missing_return",
            Self::InputUnavailable => "client.input_unavailable",
            Self::DynamicInvocation => "client.dynamic_invocation_failed",
        })
    }
}

impl Error for ClientExpressionError {}

/// A CLIENT resource could not produce a value for an expression.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientResourceExecutionError {
    /// No explicit resource executor was supplied by the caller.
    ExecutorUnavailable,
    /// The resource request is active and has not produced a terminal result.
    Pending {
        /// The resource identity waiting for completion.
        key: ClientResourceKey,
        /// The active request generation.
        generation: ClientResourceGeneration,
    },
    /// The resource completed with a redacted structured failure code.
    Failed(String),
    /// The resource was cancelled before a value became available.
    Cancelled,
    /// The resource lifecycle or request invariants rejected the operation.
    Invalid(ClientResourceError),
}

impl fmt::Display for ClientResourceExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutorUnavailable => formatter
                .write_str("CLIENT resource execution requires an explicit resource executor"),
            Self::Failed(code) => write!(formatter, "CLIENT resource failed: {code}"),
            Self::Pending { generation, .. } => {
                write!(
                    formatter,
                    "CLIENT resource request is pending at generation {}",
                    generation.value(),
                )
            }
            Self::Cancelled => formatter.write_str("CLIENT resource was cancelled"),
            Self::Invalid(source) => source.fmt(formatter),
        }
    }
}

impl Error for ClientResourceExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(source) => Some(source),
            Self::ExecutorUnavailable
            | Self::Pending { .. }
            | Self::Failed(_)
            | Self::Cancelled => None,
        }
    }
}

/// A version-four CLIENT state failure (work ADR 0069).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientStateError {
    /// A `USER`-scoped slot has no runtime slice yet and must fail closed.
    UserScopeUnsupported {
        /// The declared user-scoped slot identity.
        slot: StateSlotId,
    },
    /// The slot type is not a supported scalar or registered value type.
    UnsupportedSlotType {
        /// The slot whose type cannot be resolved.
        slot: StateSlotId,
    },
    /// A caller-provided state value does not match the declared slot type.
    StoredTypeMismatch {
        /// The slot whose stored value has the wrong runtime type.
        slot: StateSlotId,
    },
    /// A state default value does not match the declared slot type.
    DefaultTypeMismatch {
        /// The slot whose checked default has the wrong runtime type.
        slot: StateSlotId,
    },
    /// A typed null default could not be constructed for the slot type.
    NullDefault {
        /// The slot whose null default cannot be represented.
        slot: StateSlotId,
    },
}

impl fmt::Display for ClientStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserScopeUnsupported { .. } => {
                formatter.write_str("USER CLIENT state has no runtime slice yet and fails closed")
            }
            Self::UnsupportedSlotType { .. } => {
                formatter.write_str("CLIENT state slot type is not supported locally")
            }
            Self::StoredTypeMismatch { .. } => {
                formatter.write_str("CLIENT state value has the wrong runtime type")
            }
            Self::DefaultTypeMismatch { .. } => {
                formatter.write_str("CLIENT state default has the wrong runtime type")
            }
            Self::NullDefault { .. } => {
                formatter.write_str("CLIENT state null default cannot be represented")
            }
        }
    }
}

impl Error for ClientStateError {}

/// A failure while checking the execution domain and payload digest of a CLIENT artifact.
///
/// This local check provides payload integrity only. It does not authenticate
/// an artifact's provenance, signature, sandbox policy, or host capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientArtifactIntegrityError {
    /// The artifact is not marked for client execution.
    WrongExecutionDomain,
    /// The canonical payload digest could not be computed or did not match.
    PayloadDigest,
}

impl fmt::Display for ClientArtifactIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongExecutionDomain => "client artifact has the wrong execution domain",
            Self::PayloadDigest => "client artifact payload digest is invalid",
        })
    }
}

impl Error for ClientArtifactIntegrityError {}

/// An error returned by the closed local CLIENT evaluator.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientExecutionError {
    /// The allow evidence targets another active revision.
    AuthorisationMismatch {
        /// The function and revision authorised by the security decision.
        authorised: InvocationTarget,
        /// The active revision supplied for local evaluation.
        active: RevisionPair,
    },
    /// The active revision cannot form trusted canonical semantics.
    InvalidActiveRevision {
        /// The active revision pair.
        pair: RevisionPair,
        /// The requested function identity.
        function: FunctionId,
        /// The active-revision validation failure.
        source: ClientActiveRevisionError,
    },
    /// The active catalogue does not contain the requested function.
    FunctionNotFound {
        /// The active revision pair.
        pair: RevisionPair,
        /// The requested function identity.
        function: FunctionId,
    },
    /// The resolved function violates the closed CLIENT contract.
    InvalidFunction {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The failed closed rule.
        rule: ClientExecutionRule,
    },
    /// The saved CLIENT artefact cannot be decoded.
    InvalidArtifact {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The artefact decoder error.
        source: ClientPlanError,
    },
    /// A version-2 opaque plan cannot produce a registered runtime value.
    InvalidOpaqueValue {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The registry or value validation failure.
        source: ClientOpaqueValueError,
    },
    /// The local capability gate denied evaluation (ADR 0060).
    ///
    /// The recorded capability is the redacted qualified name only — no
    /// path, host, or secret argument value is retained.
    CapabilityDenied {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The redacted qualified capability name.
        capability: String,
    },
    /// A version-3 expression could not produce a typed value.
    ExpressionEvaluation {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The closed expression failure.
        source: ClientExpressionError,
    },
    /// A version-3 external contract has no installed local runtime.
    ExternalContract {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The exact contract identity retained by the artifact.
        identity: String,
    },
    /// A version-four plan could not initialise or carry CLIENT state.
    StateEvaluation {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The closed state failure.
        source: ClientStateError,
    },
    /// A version-six resource expression could not produce a checked value.
    ResourceEvaluation {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The closed resource failure.
        source: ClientResourceExecutionError,
    },
    /// A version-nine Inspector expression could not produce a checked value.
    Inspect {
        /// The resolved execution context.
        context: ClientExecutionContext,
        /// The closed Inspector failure.
        source: ClientInspectError,
    },
}

impl From<Box<ClientExecutionError>> for ClientExecutionError {
    fn from(error: Box<ClientExecutionError>) -> Self {
        *error
    }
}
impl ClientExecutionError {
    /// Returns the active revision pair associated with this error.
    pub const fn pair(&self) -> RevisionPair {
        match self {
            Self::AuthorisationMismatch { active, .. } => *active,
            Self::InvalidActiveRevision { pair, .. } | Self::FunctionNotFound { pair, .. } => *pair,
            Self::InvalidFunction { context, .. }
            | Self::InvalidArtifact { context, .. }
            | Self::InvalidOpaqueValue { context, .. }
            | Self::CapabilityDenied { context, .. }
            | Self::ExpressionEvaluation { context, .. }
            | Self::ExternalContract { context, .. }
            | Self::StateEvaluation { context, .. }
            | Self::ResourceEvaluation { context, .. }
            | Self::Inspect { context, .. } => context.pair(),
        }
    }

    /// Returns the requested or resolved function identity associated with this error.
    pub const fn function(&self) -> FunctionId {
        match self {
            Self::AuthorisationMismatch { authorised, .. } => authorised.function(),
            Self::InvalidActiveRevision { function, .. }
            | Self::FunctionNotFound { function, .. } => *function,
            Self::InvalidFunction { context, .. }
            | Self::InvalidArtifact { context, .. }
            | Self::InvalidOpaqueValue { context, .. }
            | Self::CapabilityDenied { context, .. }
            | Self::ExpressionEvaluation { context, .. }
            | Self::ExternalContract { context, .. }
            | Self::StateEvaluation { context, .. }
            | Self::ResourceEvaluation { context, .. }
            | Self::Inspect { context, .. } => context.function(),
        }
    }

    /// Returns the resolved context after function resolution.
    pub const fn context(&self) -> Option<&ClientExecutionContext> {
        match self {
            Self::AuthorisationMismatch { .. }
            | Self::InvalidActiveRevision { .. }
            | Self::FunctionNotFound { .. } => None,
            Self::InvalidFunction { context, .. }
            | Self::InvalidArtifact { context, .. }
            | Self::InvalidOpaqueValue { context, .. }
            | Self::CapabilityDenied { context, .. }
            | Self::ExpressionEvaluation { context, .. }
            | Self::ExternalContract { context, .. }
            | Self::StateEvaluation { context, .. }
            | Self::ResourceEvaluation { context, .. }
            | Self::Inspect { context, .. } => Some(context),
        }
    }
}

impl fmt::Display for ClientExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorisationMismatch { .. } => {
                formatter.write_str("the CLIENT authorisation does not match the active revision")
            }
            Self::InvalidActiveRevision { .. } => {
                formatter.write_str("the active revision cannot be trusted")
            }
            Self::FunctionNotFound { .. } => {
                formatter.write_str("the active revision does not contain this function")
            }
            Self::InvalidFunction { rule, .. } => rule.fmt(formatter),
            Self::InvalidArtifact { .. } | Self::InvalidOpaqueValue { .. } => {
                formatter.write_str("the saved CLIENT function cannot be evaluated")
            }
            Self::CapabilityDenied { capability, .. } => write!(
                formatter,
                "the CLIENT function requires the capability {capability} which is not granted"
            ),
            Self::ExpressionEvaluation { source, .. } => source.fmt(formatter),
            Self::ExternalContract { identity, .. } => write!(
                formatter,
                "the CLIENT runtime contract {identity} is not available"
            ),
            Self::StateEvaluation { source, .. } => source.fmt(formatter),
            Self::ResourceEvaluation { source, .. } => source.fmt(formatter),
            Self::Inspect { source, .. } => source.fmt(formatter),
        }
    }
}
impl Error for ClientExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidActiveRevision { source, .. } => Some(source),
            Self::InvalidArtifact { source, .. } => Some(source),
            Self::InvalidOpaqueValue { source, .. } => Some(source),
            Self::StateEvaluation { source, .. } => Some(source),
            Self::ResourceEvaluation { source, .. } => source.source(),
            Self::Inspect { source, .. } => Some(source),
            Self::AuthorisationMismatch { .. }
            | Self::FunctionNotFound { .. }
            | Self::InvalidFunction { .. }
            | Self::CapabilityDenied { .. }
            | Self::ExpressionEvaluation { .. }
            | Self::ExternalContract { .. } => None,
        }
    }
}

/// Evaluates one closed CLIENT function from one active revision.
///
/// The allow evidence selects the only function and revision that may run. The
/// evaluator performs no database, protocol, filesystem, process, environment,
/// clock, random, network, or runtime-library operation.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_arguments(active, authorisation, &[])
}

/// Evaluates one closed CLIENT function with invocation arguments.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_arguments(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_grants_and_arguments(
        active,
        authorisation,
        arguments,
        &[],
        &capability::LocalCapabilityGrantSet::new(),
    )
}

/// Evaluates one closed CLIENT function after the local capability gate.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_grants(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_grants_and_arguments(
        active,
        authorisation,
        &[],
        declarations,
        grants,
    )
}

/// Evaluates one closed CLIENT function with invocation arguments and grants.
///
/// Version-four state plans run with a transient in-memory state store that
/// is discarded when the call returns. Callers that must retain `LOCAL` or
/// `SESSION` state across calls use
/// [`evaluate_client_function_with_state_and_grants_and_arguments`].
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_grants_and_arguments(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    let mut state = ClientStateStore::new();
    evaluate_client_function_with_state_and_grants_and_arguments(
        active,
        authorisation,
        arguments,
        declarations,
        grants,
        &mut state,
    )
}

/// Evaluates one closed CLIENT function with an explicit in-memory state store.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_state_and_arguments(active, authorisation, &[], state)
}

/// Evaluates one closed CLIENT function with invocation arguments and an
/// explicit in-memory state store.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state_and_arguments(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_state_and_grants_and_arguments(
        active,
        authorisation,
        arguments,
        &[],
        &capability::LocalCapabilityGrantSet::new(),
        state,
    )
}

/// Evaluates one closed CLIENT function after the local capability gate with
/// an explicit in-memory state store.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state_and_grants(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_state_and_grants_and_arguments(
        active,
        authorisation,
        &[],
        declarations,
        grants,
        state,
    )
}

/// Evaluates one closed CLIENT function with invocation arguments, grants, and
/// an explicit in-memory state store.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state_and_grants_and_arguments(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    let state_context = ClientStateContext::default_for(authorisation.target().function());
    evaluate_client_function_in_state_context(
        active,
        authorisation,
        &state_context,
        arguments,
        declarations,
        grants,
        state,
    )
}

/// Evaluates one closed CLIENT function with invocation arguments, grants,
/// and an explicit root state context, without an external resource executor.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_in_state_context_with_grants_and_arguments(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state_context: &ClientStateContext,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_in_state_context(
        active,
        authorisation,
        state_context,
        arguments,
        declarations,
        grants,
        state,
    )
}

/// Evaluates one CLIENT function in an explicit root state context.
///
/// Resource and `AWAIT` expressions fail closed because no external executor
/// is owned by this compatibility entrypoint. Call
/// [`evaluate_client_function_with_state_and_grants_and_arguments_and_executor`]
/// when the host owns the resource work boundary.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_in_state_context(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state_context: &ClientStateContext,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_in_state_context_with_executor(
        active,
        authorisation,
        state_context,
        arguments,
        declarations,
        grants,
        state,
        InvocationId::new(),
        None,
    )
}

/// Evaluates one closed CLIENT function with a caller-owned resource executor.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_executor(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_arguments_and_executor(active, authorisation, &[], executor)
}

/// Evaluates one CLIENT function with invocation arguments and a caller-owned
/// resource executor.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_arguments_and_executor(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    let mut state = ClientStateStore::new();
    let grants = capability::LocalCapabilityGrantSet::new();
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        active,
        authorisation,
        arguments,
        &[],
        &grants,
        &mut state,
        InvocationId::new(),
        executor,
    )
}

/// Evaluates one CLIENT function with a caller-owned resource executor.
///
/// The executor is the only seam that may perform external work. It receives
/// validated, principal- and revision-scoped requests; this evaluator never
/// invents transport or server execution.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state_and_grants_and_arguments_and_executor(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        active,
        authorisation,
        arguments,
        declarations,
        grants,
        state,
        InvocationId::new(),
        executor,
    )
}

/// Evaluates one CLIENT function with an explicit root state context, a
/// caller-owned resource executor, and an enclosing root invocation identity.
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state_context: &ClientStateContext,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    parent_invocation_id: InvocationId,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_in_state_context_with_executor(
        active,
        authorisation,
        state_context,
        arguments,
        declarations,
        grants,
        state,
        parent_invocation_id,
        Some(executor),
    )
}

/// Evaluates one CLIENT function with a caller-owned resource executor and an
/// enclosing root invocation identity.
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    parent_invocation_id: InvocationId,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    let state_context = ClientStateContext::default_for(authorisation.target().function());
    evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
        active,
        authorisation,
        &state_context,
        arguments,
        declarations,
        grants,
        state,
        parent_invocation_id,
        executor,
    )
}

fn same_revision_terminal_replacement(
    active: &ActiveDatabaseRevision,
    state: &ClientStateStore,
    key: &ClientResourceKey,
    resource: &ClientResource,
) -> bool {
    let Some(previous) = state.resources.get(key) else {
        return false;
    };
    previous.status() == ClientResourceStatus::Loading
        && previous.key().target().revision() == active.pair()
        && resource.key().target().revision() == active.pair()
        && resource.generation() == previous.generation()
        && matches!(
            resource.status(),
            ClientResourceStatus::Ready | ClientResourceStatus::Failed
        )
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_client_function_in_state_context_with_executor(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state_context: &ClientStateContext,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    parent_invocation_id: InvocationId,
    mut executor: Option<&mut dyn ClientResourceExecutor>,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(ClientExecutionError::AuthorisationMismatch {
            authorised: target,
            active: active.pair(),
        });
    }
    validate_active_catalogue(active, target.function())?;
    if !client_invocation_target_is_resolved(active, target) {
        return Err(ClientExecutionError::FunctionNotFound {
            pair: active.pair(),
            function: target.function(),
        });
    }
    let mut staged = state.clone();
    staged.set_context(state_context.clone());
    // Security is invocation-scoped; refresh it for every root evaluation while
    // retaining the host-configured data invalidation token in the context.
    staged.set_security_context_digest(security_context_digest(authorisation));
    let result = match evaluate_function(
        active,
        target.function(),
        arguments
            .iter()
            .map(|argument| (argument.parameter(), argument.value().clone()))
            .collect(),
        declarations,
        grants,
        &mut staged,
        0,
        authorisation.session_principal(),
        ObserverLineage::top_level(parent_invocation_id),
        &mut executor,
    ) {
        Ok(result) => result,
        Err(error) => {
            match &error {
                ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Pending { key, generation },
                    ..
                } => {
                    // Persist the pending resource and any same-revision
                    // replacement state committed while cancelling its old generation.
                    let changed_resources: Vec<_> = staged
                        .resources
                        .iter()
                        .filter_map(|(candidate_key, resource)| {
                            let replacement_cancelled =
                                state.resources.get(candidate_key).is_some_and(|previous| {
                                    previous.status() == ClientResourceStatus::Loading
                                        && resource.status() == ClientResourceStatus::Idle
                                        && resource.generation().value()
                                            > previous.generation().value()
                                });
                            let replacement_terminal = same_revision_terminal_replacement(
                                active,
                                state,
                                candidate_key,
                                resource,
                            );
                            let pending_resource = resource.key() == *key
                                && resource.generation() == *generation
                                && resource.status() == ClientResourceStatus::Loading;
                            (pending_resource || replacement_cancelled || replacement_terminal)
                                .then_some((*candidate_key, resource.clone()))
                        })
                        .collect();
                    for (candidate_key, resource) in changed_resources {
                        state.resources.insert(candidate_key, resource);
                    }
                }
                ClientExecutionError::ResourceEvaluation {
                    source:
                        ClientResourceExecutionError::Failed(_)
                        | ClientResourceExecutionError::Cancelled,
                    ..
                } => {
                    // Preserve terminal resource state when the invocation
                    // fails. The caller can inspect the redacted failure or
                    // cancellation and decide whether to retry or invalidate.
                    for (key, resource) in &staged.resources {
                        let replacement_cancelled =
                            state.resources.get(key).is_some_and(|previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            });
                        let replacement_terminal =
                            same_revision_terminal_replacement(active, state, key, resource);
                        if matches!(
                            resource.status(),
                            ClientResourceStatus::Failed | ClientResourceStatus::Cancelled
                        ) || replacement_cancelled
                            || replacement_terminal
                        {
                            state.resources.insert(*key, resource.clone());
                        }
                    }
                }
                ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Invalid(_),
                    ..
                } => {
                    // A malformed same-generation completion is returned as
                    // Invalid after the exact request is offered to the executor
                    // for cancellation. If cancellation is non-terminal or
                    // malformed, retain changed resource state so the executor-owned
                    // request is not stranded in the staged clone.
                    for (key, resource) in &staged.resources {
                        let replacement_cancelled =
                            state.resources.get(key).is_some_and(|previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            });
                        let replacement_terminal =
                            same_revision_terminal_replacement(active, state, key, resource);
                        let changed_identity = state.resources.get(key).is_none_or(|previous| {
                            previous.status() != resource.status()
                                || previous.generation() != resource.generation()
                                || previous.request_id() != resource.request_id()
                        });
                        let terminal_changed = matches!(
                            resource.status(),
                            ClientResourceStatus::Failed | ClientResourceStatus::Cancelled
                        ) && changed_identity;
                        let loading_owned =
                            resource.status() == ClientResourceStatus::Loading && changed_identity;
                        if terminal_changed
                            || loading_owned
                            || replacement_cancelled
                            || replacement_terminal
                        {
                            state.resources.insert(*key, resource.clone());
                        }
                    }
                }
                _ => {
                    // A later expression, state, Inspector, or external
                    // failure still commits any terminal result that won't
                    // be visible after this staged evaluation is dropped.
                    for (key, resource) in &staged.resources {
                        let replacement_cancelled =
                            state.resources.get(key).is_some_and(|previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            });
                        let replacement_terminal =
                            same_revision_terminal_replacement(active, state, key, resource);
                        if replacement_cancelled || replacement_terminal {
                            state.resources.insert(*key, resource.clone());
                        }
                    }
                }
            }
            return Err(error);
        }
    };
    *state = staged;
    let (context, value) = result;
    Ok(ClientExecutionResult { context, value })
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_function(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    arguments: Vec<(ParameterId, RuntimeValue)>,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    lineage: ObserverLineage,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
) -> Result<(ClientExecutionContext, RuntimeValue), ClientExecutionError> {
    let mut fuel = ClientExecutionFuel::new();
    Ok(evaluate_function_with_fuel(
        active,
        function,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        lineage,
        executor,
        &mut fuel,
    )?)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_function_with_fuel(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    arguments: Vec<(ParameterId, RuntimeValue)>,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    lineage: ObserverLineage,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    fuel: &mut ClientExecutionFuel,
) -> Result<(ClientExecutionContext, RuntimeValue), Box<ClientExecutionError>> {
    let pair = active.pair();
    let resolved = resolve_client_function(active, function)
        .ok_or(ClientExecutionError::FunctionNotFound { pair, function })?;
    let definition = resolved.definition;
    let revision = resolved.revision;
    let context = ClientExecutionContext {
        pair,
        function,
        function_revision: revision.id(),
        parent_invocation_id: lineage.parent,
        observer_lineage: Some(lineage),
    };
    // A version-5 capability envelope is decoded before function-shape
    // validation (work ADR 0060). Its inner plan version classifies the
    // function, and its stored requirements gate evaluation; the caller's
    // declaration list never replaces them. Verify the artifact identity
    // before this decoder so no untrusted payload reaches it.
    let envelope = if revision.artifact().version() == CAPABILITY_FORMAT_VERSION {
        validate_artifact_identity(revision.artifact(), context)?;
        Some(
            CapabilityClientPlan::decode(revision.artifact().payload())
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?,
        )
    } else {
        None
    };
    let artifact_version = envelope
        .as_ref()
        .map_or(revision.artifact().version(), |plan| {
            plan.inner_plan_version()
        });
    fuel.consume(context)?;
    // Bind caller-owned parameter references once, while the declaration owner
    // and its invocation arguments are still in scope. Passing the resulting
    // literal declarations through nested calls prevents a callee from trying
    // to resolve the caller parameter name against its own parameters.
    let bound_declarations = bind_capability_declarations(definition, &arguments, declarations);
    let resolve_parameter =
        |parameter: &str| resolve_parameter_argument(definition, &arguments, parameter);
    match &envelope {
        Some(plan) => {
            for requirement in plan.requirements() {
                let name =
                    capability::LocalCapabilityName::parse(requirement.name()).map_err(|_| {
                        ClientExecutionError::CapabilityDenied {
                            context,
                            capability: requirement.name().to_owned(),
                        }
                    })?;
                let declaration = capability::LocalCapabilityDeclaration::new(
                    name,
                    match requirement.argument() {
                        CapabilityArgumentSource::Text(text) => {
                            capability::LocalCapabilityArgumentSource::Text(text.clone())
                        }
                        CapabilityArgumentSource::Parameter(parameter) => {
                            capability::LocalCapabilityArgumentSource::Parameter(parameter.clone())
                        }
                    },
                );
                if !grants.satisfies_declaration(&declaration, resolve_parameter) {
                    return Err(Box::new(ClientExecutionError::CapabilityDenied {
                        context,
                        capability: requirement.name().to_owned(),
                    }));
                }
            }
        }
        None => {
            for declaration in &bound_declarations {
                if !grants.satisfies_declaration(declaration, resolve_parameter) {
                    return Err(Box::new(ClientExecutionError::CapabilityDenied {
                        context,
                        capability: declaration.name().as_str().to_owned(),
                    }));
                }
            }
        }
    }
    let return_shape = validate_function_shape(active, definition, context, artifact_version)?;
    if envelope.is_none() {
        validate_artifact_identity(revision.artifact(), context)?;
    }
    if arguments.len() != definition.parameters().len()
        || definition.parameters().iter().any(|parameter| {
            arguments
                .iter()
                .filter(|(candidate, _)| *candidate == parameter.id())
                .count()
                != 1
                || arguments
                    .iter()
                    .find(|(candidate, _)| *candidate == parameter.id())
                    .is_none_or(|(_, value)| {
                        !runtime_value_matches(active, value, parameter.resolved_type())
                    })
        })
    {
        return Err(Box::new(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        )));
    }
    validate_selected_references(
        active,
        resolved.references,
        definition,
        revision.semantic_hash_version(),
        context,
        return_shape,
    )?;
    validate_artifact(
        revision.artifact(),
        revision.language_version(),
        context,
        return_shape,
        artifact_version,
    )?;
    let mut local_environment = ClientLocalEnvironment::new();
    let value = match &envelope {
        Some(plan) => evaluate_capability_plan(
            active,
            plan,
            context,
            lineage,
            return_shape,
            &arguments,
            &bound_declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            &mut local_environment,
            fuel,
        )?,
        None => evaluate_plan(
            active,
            revision.artifact().payload(),
            context,
            lineage,
            return_shape,
            &arguments,
            &bound_declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            &mut local_environment,
            fuel,
        )?,
    };
    Ok((context, value))
}

/// Resolves one declared parameter name to its invocation value.
///
/// A parameter that is not declared, not bound at invocation time, or not a
/// text value cannot satisfy a capability scope and resolves to `None`, so
/// the capability gate fails closed.
fn resolve_parameter_argument(
    definition: &orna_core::catalogue::FunctionDefinition,
    arguments: &[(ParameterId, RuntimeValue)],
    parameter: &str,
) -> Option<String> {
    let parameter_id = definition
        .parameters()
        .iter()
        .find(|candidate| candidate.name() == parameter)
        .map(|candidate| candidate.id())?;
    arguments
        .iter()
        .find(|(candidate, _)| *candidate == parameter_id)
        .and_then(|(_, value)| match value {
            RuntimeValue::Text(value) => Some(value.clone()),
            _ => None,
        })
}

/// Binds capability declarations to the function invocation that owns them.
///
/// Caller-supplied declarations are checked before nested CLIENT calls run. A
/// parameter reference that resolves here is converted to a literal so that
/// nested callees never reinterpret the caller parameter name in their own
/// parameter namespace. Unresolved references remain parameter-scoped and are
/// rejected by the owning gate, preserving fail-closed behavior.
fn bind_capability_declarations(
    definition: &orna_core::catalogue::FunctionDefinition,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
) -> Vec<capability::LocalCapabilityDeclaration> {
    declarations
        .iter()
        .map(|declaration| match declaration.argument() {
            capability::LocalCapabilityArgumentSource::Text(_) => declaration.clone(),
            capability::LocalCapabilityArgumentSource::Parameter(parameter) => {
                match resolve_parameter_argument(definition, arguments, parameter) {
                    Some(value) => capability::LocalCapabilityDeclaration::new(
                        declaration.name(),
                        capability::LocalCapabilityArgumentSource::Text(value),
                    ),
                    None => declaration.clone(),
                }
            }
        })
        .collect()
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_plan(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    return_shape: ClientReturnShape,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, Box<ClientExecutionError>> {
    match return_shape {
        ClientReturnShape::LegacyBoolean | ClientReturnShape::StandardBoolean(_) => {
            let plan = ClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            Ok(RuntimeValue::Boolean(plan.returned_boolean()))
        }
        ClientReturnShape::Opaque(expected) => {
            let plan = OpaqueClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            evaluate_opaque_plan(active, &plan, context, expected).map_err(Box::new)
        }
        ClientReturnShape::Expression(expected) | ClientReturnShape::StreamExpression(expected) => {
            let plan = ExpressionClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            if matches!(return_shape, ClientReturnShape::StreamExpression(_)) {
                evaluate_stream_expression_plan(
                    active,
                    plan.expression(),
                    context,
                    lineage,
                    expected,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )
                .map_err(Box::new)
            } else {
                evaluate_expression_plan_with_fuel(
                    active,
                    plan.expression(),
                    context,
                    lineage,
                    expected,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )
                .map_err(Box::new)
            }
        }
        ClientReturnShape::Inspect(expected) | ClientReturnShape::Source(expected) => {
            let plan = ExpressionClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            evaluate_expression_plan_with_fuel(
                active,
                plan.expression(),
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::StreamState(expected) => {
            let plan = StateClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_state_calls(active, &plan, context)?;
            evaluate_stream_state_plan(
                active,
                &plan,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::State(expected) => {
            let plan = StateClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_state_calls(active, &plan, context)?;
            evaluate_state_plan(
                active,
                &plan,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::StreamProcedural(expected) => {
            let plan = ProceduralClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_procedural_calls(active, &plan, context)?;
            evaluate_procedural_plan_with_fuel(
                active,
                &plan,
                context,
                lineage,
                expected,
                true,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::Procedural(expected) => {
            let plan = ProceduralClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_procedural_calls(active, &plan, context)?;
            evaluate_procedural_plan_with_fuel(
                active,
                &plan,
                context,
                lineage,
                expected,
                false,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::StreamControlFlow(expected)
        | ClientReturnShape::ControlFlow(expected) => {
            let plan = ControlFlowClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_control_flow_calls(active, &plan, context)?;
            validate_control_flow_plan_types(active, &plan, context)?;
            evaluate_control_flow_plan(
                active,
                &plan,
                context,
                lineage,
                expected,
                matches!(return_shape, ClientReturnShape::StreamControlFlow(_)),
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::Action(_expected) => {
            let plan = ActionClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_action_calls(active, plan.operation(), context)?;
            evaluate_action_operation(
                active,
                plan.operation(),
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::StreamResource(expected) => {
            let plan = ResourceClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            evaluate_stream_resource_plan(
                active,
                &plan,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::Resource(expected) => {
            let plan = ResourceClientPlan::decode(payload).map_err(|source| {
                Box::new(ClientExecutionError::InvalidArtifact { context, source })
            })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            evaluate_resource_plan(
                active,
                &plan,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Box::new)
        }
        ClientReturnShape::OtherValue => unreachable!("definition references were validated"),
        ClientReturnShape::Unsupported => unreachable!("function shape was validated"),
    }
}

/// Evaluates one decoded version-2 opaque plan against the function return
/// type, sharing the closed value-creation contract of the plain path.
// ClientExecutionError or action errors retain their accepted diagnostic context and variants.
#[allow(clippy::result_large_err)]
fn evaluate_opaque_plan(
    active: &ActiveDatabaseRevision,
    plan: &OpaqueClientPlan,
    context: ClientExecutionContext,
    expected: TypeId,
) -> Result<RuntimeValue, ClientExecutionError> {
    if plan.opaque_type() != expected {
        return Err(ClientExecutionError::InvalidOpaqueValue {
            context,
            source: ClientOpaqueValueError::TypeMismatch {
                expected,
                actual: plan.opaque_type(),
            },
        });
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(ClientExecutionError::InvalidOpaqueValue {
            context,
            source: ClientOpaqueValueError::Value(OpaqueValueError::ActiveStandardRequired),
        });
    };
    let registry = registered_opaque_codecs(standard).map_err(|source| {
        ClientExecutionError::InvalidOpaqueValue {
            context,
            source: ClientOpaqueValueError::Registry(Box::new(source)),
        }
    })?;
    let value = OpaqueValue::new(active, &registry, expected, plan.canonical_payload()).map_err(
        |source| ClientExecutionError::InvalidOpaqueValue {
            context,
            source: ClientOpaqueValueError::Value(source),
        },
    )?;
    Ok(RuntimeValue::Opaque(value))
}

/// Evaluates a stream expression only when it starts an actual stream await.
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_stream_expression_plan(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    if matches!(expression, ClientExpressionNode::ExternalContract { .. }) {
        return Ok(evaluate_expression_with_fuel(
            active,
            expression,
            context,
            &lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )?);
    }
    if !expression_returns_stream(active, expression, local_environment) {
        return Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ));
    }
    evaluate_expression_plan_with_fuel(
        active,
        expression,
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )
}

/// Evaluates one decoded expression tree and type-checks its value.
#[cfg(test)]
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_expression_plan(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut fuel = ClientExecutionFuel::new();
    evaluate_expression_plan_with_fuel(
        active,
        expression,
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        &mut fuel,
    )
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_expression_plan_with_fuel(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let value = evaluate_expression_with_fuel(
        active,
        expression,
        context,
        &lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?;
    if runtime_expression_value_matches(active, expression, &value, expected, local_environment) {
        Ok(value)
    } else {
        Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ))
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_stream_state_plan(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    initialize_client_state(
        active,
        plan,
        context,
        lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?;
    evaluate_stream_expression_plan(
        active,
        plan.expression(),
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )
}

/// Evaluates one decoded version-4 state plan after initialising its slots.
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_state_plan(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    initialize_client_state(
        active,
        plan,
        context,
        lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?;
    evaluate_expression_plan_with_fuel(
        active,
        plan.expression(),
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_stream_resource_plan(
    active: &ActiveDatabaseRevision,
    plan: &ResourceClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    evaluate_stream_expression_plan(
        active,
        plan.expression(),
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_resource_plan(
    active: &ActiveDatabaseRevision,
    plan: &ResourceClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    evaluate_expression_plan_with_fuel(
        active,
        plan.expression(),
        context,
        lineage,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )
}

/// Evaluates one decoded version-5 capability envelope after its stored
/// requirements passed the capability gate (work ADR 0060).
///
/// The envelope's requirements are the only capability gate for version-5
/// plans: the caller's declaration list is not consulted, so a recursive
/// CLIENT call validates its own stored requirements instead of inheriting
/// the parent declaration list.
#[cfg(test)]
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_procedural_plan(
    active: &ActiveDatabaseRevision,
    plan: &ProceduralClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    stream_result: bool,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut fuel = ClientExecutionFuel::new();
    evaluate_procedural_plan_with_fuel(
        active,
        plan,
        context,
        lineage,
        expected,
        stream_result,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        &mut fuel,
    )
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_procedural_plan_with_fuel(
    active: &ActiveDatabaseRevision,
    plan: &ProceduralClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    stream_result: bool,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    for statement in plan.statements() {
        fuel.consume(context)?;
        let local_id = statement.local();
        let Some(local) = plan
            .locals()
            .iter()
            .find(|candidate| candidate.local_id() == local_id)
        else {
            return Err(expression_error(
                context,
                ClientExpressionError::ParameterNotBound,
            ));
        };
        match statement {
            orna_artifact::client_plan::ClientStatement::Let { expression, .. } => {
                if local_environment.contains_key(&local_id) {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::InvalidCall,
                    ));
                }
                let binding = evaluate_procedural_local_with_fuel(
                    active,
                    local,
                    expression,
                    context,
                    lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                local_environment.insert(local_id, binding);
            }
            orna_artifact::client_plan::ClientStatement::Assignment { expression, .. } => {
                if !local_environment.contains_key(&local_id) {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::ParameterNotBound,
                    ));
                }
                let binding = evaluate_procedural_local_with_fuel(
                    active,
                    local,
                    expression,
                    context,
                    lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                local_environment.insert(local_id, binding);
            }
        }
    }
    let value = evaluate_expression_with_fuel(
        active,
        plan.return_expression(),
        context,
        &lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?;
    let result_matches = if stream_result {
        expression_returns_stream(active, plan.return_expression(), local_environment)
            && runtime_stream_value_matches(active, &value, expected)
    } else {
        runtime_expression_value_matches(
            active,
            plan.return_expression(),
            &value,
            expected,
            local_environment,
        )
    };
    if result_matches {
        Ok(value)
    } else {
        Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ))
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_procedural_local_with_fuel(
    active: &ActiveDatabaseRevision,
    local: &ClientLocal,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<ClientLocalBinding, ClientExecutionError> {
    match local.kind() {
        ClientLocalKind::Value => {
            if procedural_resource_kind_for_runtime(expression, local_environment).is_some() {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            }
            let expected = resolve_client_local_type(active, local.type_id())
                .ok_or_else(|| expression_error(context, ClientExpressionError::TypeMismatch))?;
            let stream_await = expression_returns_stream(active, expression, local_environment);
            let value = evaluate_expression_plan_with_fuel(
                active,
                expression,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            if stream_await {
                Ok(ClientLocalBinding::StreamValue(value))
            } else {
                Ok(ClientLocalBinding::Value(value))
            }
        }
        ClientLocalKind::Resource(kind) => {
            fuel.consume(context)?;
            let ClientExpressionNode::Resource { operation } = expression else {
                let ClientExpressionNode::LocalRead { local: source } = expression else {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::TypeMismatch,
                    ));
                };
                let Some(ClientLocalBinding::Resource(operation)) = local_environment.get(source)
                else {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::ParameterNotBound,
                    ));
                };
                validate_procedural_resource_binding(active, local, kind, operation, context)?;
                return Ok(ClientLocalBinding::Resource(operation.clone()));
            };
            validate_procedural_resource_binding(active, local, kind, operation, context)?;
            Ok(ClientLocalBinding::Resource(operation.clone()))
        }
    }
}

fn procedural_resource_kind_for_runtime(
    expression: &ClientExpressionNode,
    local_environment: &ClientLocalEnvironment,
) -> Option<ResourceKind> {
    match expression {
        ClientExpressionNode::Resource { operation } => Some(operation.kind()),
        ClientExpressionNode::Inspect { .. } => None,
        ClientExpressionNode::LocalRead { local } => match local_environment.get(local) {
            Some(ClientLocalBinding::Resource(operation)) => Some(operation.kind()),
            _ => None,
        },
        _ => None,
    }
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn validate_procedural_resource_binding(
    active: &ActiveDatabaseRevision,
    local: &ClientLocal,
    kind: ResourceKind,
    operation: &ResourceOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    if operation.kind() != kind {
        return Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ));
    }
    let resolved = resource_operation_result_type(active, operation, context)?;
    if !resource_type_matches_id(active, resolved, local.type_id()) {
        return Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ControlFlowReturnValue {
    value: RuntimeValue,
    stream: bool,
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_control_flow_plan(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    stream_result: bool,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let returned = evaluate_control_flow_block(
        active,
        plan,
        plan.statements(),
        context,
        lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?
    .ok_or_else(|| expression_error(context, ClientExpressionError::MissingReturn))?;

    let matches = if stream_result {
        returned.stream && runtime_stream_value_matches(active, &returned.value, expected)
    } else {
        !returned.stream && runtime_value_matches(active, &returned.value, expected)
    };
    if matches {
        Ok(returned.value)
    } else {
        Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ))
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_control_flow_block(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statements: &[ControlFlowStatement],
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<Option<ControlFlowReturnValue>, ClientExecutionError> {
    for statement in statements {
        fuel.consume(context)?;
        if let Some(returned) = evaluate_control_flow_statement(
            active,
            plan,
            statement,
            context,
            lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )? {
            return Ok(Some(returned));
        }
    }
    Ok(None)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_control_flow_statement(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statement: &ControlFlowStatement,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<Option<ControlFlowReturnValue>, ClientExecutionError> {
    match statement {
        ControlFlowStatement::Let { local, expression }
        | ControlFlowStatement::Assignment { local, expression } => {
            let Some(declaration) = plan
                .locals()
                .iter()
                .find(|candidate| candidate.local_id() == *local)
            else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::ParameterNotBound,
                ));
            };
            if matches!(statement, ControlFlowStatement::Assignment { .. })
                && !local_environment.contains_key(local)
            {
                return Err(expression_error(
                    context,
                    ClientExpressionError::ParameterNotBound,
                ));
            }
            let binding = evaluate_procedural_local_with_fuel(
                active,
                declaration,
                expression,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            // A validated plan has one declaration per local identity. A LET
            // inside a repeated block reinitialises that declaration each time.
            local_environment.insert(*local, binding);
            Ok(None)
        }
        ControlFlowStatement::Return(return_statement) => {
            let Some(expression) = return_statement.expression() else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            };
            let stream = expression_returns_stream(active, expression, local_environment);
            let value = evaluate_expression_with_fuel(
                active,
                expression,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            Ok(Some(ControlFlowReturnValue { value, stream }))
        }
        ControlFlowStatement::If(if_statement) => {
            for branch in if_statement.branches() {
                fuel.consume(context)?;
                let condition = evaluate_expression_with_fuel(
                    active,
                    branch.condition(),
                    context,
                    &lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                let RuntimeValue::Boolean(condition) = condition else {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::TypeMismatch,
                    ));
                };
                if condition {
                    return evaluate_control_flow_block(
                        active,
                        plan,
                        branch.statements(),
                        context,
                        lineage,
                        arguments,
                        declarations,
                        grants,
                        state,
                        depth,
                        principal,
                        executor,
                        local_environment,
                        fuel,
                    );
                }
            }
            if let Some(statements) = if_statement.else_statements() {
                evaluate_control_flow_block(
                    active,
                    plan,
                    statements,
                    context,
                    lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )
            } else {
                Ok(None)
            }
        }
        ControlFlowStatement::While(while_statement) => loop {
            fuel.consume(context)?;
            fuel.consume(context)?;
            let condition = evaluate_expression_with_fuel(
                active,
                while_statement.condition(),
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let RuntimeValue::Boolean(condition) = condition else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            };
            if !condition {
                return Ok(None);
            }
            if let Some(returned) = evaluate_control_flow_block(
                active,
                plan,
                while_statement.statements(),
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )? {
                return Ok(Some(returned));
            }
        },
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_capability_plan(
    active: &ActiveDatabaseRevision,
    plan: &CapabilityClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    return_shape: ClientReturnShape,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    preflight_client_inner_plan_calls(active, plan.inner_plan(), context)?;

    match plan.inner_plan() {
        InnerClientPlan::Boolean(inner) => Ok(RuntimeValue::Boolean(inner.returned_boolean())),
        InnerClientPlan::Opaque(inner) => {
            let ClientReturnShape::Opaque(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_opaque_plan(active, inner, context, expected)
        }
        InnerClientPlan::Expression(inner) => {
            if let ClientReturnShape::StreamExpression(expected) = return_shape {
                return evaluate_stream_expression_plan(
                    active,
                    inner.expression(),
                    context,
                    lineage,
                    expected,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                );
            }
            let (ClientReturnShape::Expression(expected) | ClientReturnShape::Inspect(expected)) =
                return_shape
            else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_expression_plan_with_fuel(
                active,
                inner.expression(),
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
        InnerClientPlan::State(inner) => {
            if let ClientReturnShape::StreamState(expected) = return_shape {
                return evaluate_stream_state_plan(
                    active,
                    inner,
                    context,
                    lineage,
                    expected,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                );
            }
            let ClientReturnShape::State(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_state_plan(
                active,
                inner,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
        InnerClientPlan::Procedural(inner) => {
            if let ClientReturnShape::StreamProcedural(expected) = return_shape {
                return evaluate_procedural_plan_with_fuel(
                    active,
                    inner,
                    context,
                    lineage,
                    expected,
                    true,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                );
            }
            let ClientReturnShape::Procedural(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_procedural_plan_with_fuel(
                active,
                inner,
                context,
                lineage,
                expected,
                false,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
        InnerClientPlan::ControlFlow(inner) => {
            let (expected, stream_result) = match return_shape {
                ClientReturnShape::ControlFlow(expected) => (expected, false),
                ClientReturnShape::StreamControlFlow(expected) => (expected, true),
                _ => unreachable!("function shape was validated against the inner plan version"),
            };
            evaluate_control_flow_plan(
                active,
                inner,
                context,
                lineage,
                expected,
                stream_result,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
        InnerClientPlan::Action(inner) => {
            let ClientReturnShape::Action(_) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_action_operation(
                active,
                inner.operation(),
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
        InnerClientPlan::Resource(inner) => {
            if let ClientReturnShape::StreamResource(expected) = return_shape {
                return evaluate_stream_resource_plan(
                    active,
                    inner,
                    context,
                    lineage,
                    expected,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                );
            }
            let ClientReturnShape::Resource(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_resource_plan(
                active,
                inner,
                context,
                lineage,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
        }
    }
}

fn evaluate_resource_error(
    context: ClientExecutionContext,
    source: ClientResourceExecutionError,
) -> ClientExecutionError {
    ClientExecutionError::ResourceEvaluation { context, source }
}

fn active_resource_result_type_matches(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
    kind: ResourceKind,
    expected: ResolvedType,
) -> bool {
    let Some(resolved) = resolve_resource_target(active, target) else {
        return false;
    };
    match (kind, resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => *result == expected,
        (ResourceKind::Stream, FunctionReturn::Stream(item)) => *item == expected,
        _ => false,
    }
}

fn resource_type_matches_id(
    active: &ActiveDatabaseRevision,
    resolved: ResolvedType,
    type_id: TypeId,
) -> bool {
    match resolved {
        ResolvedType::Scalar(scalar) => active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
            .is_some_and(|definition| {
                definition.representation_contract()
                    == match scalar {
                        StandardScalar::Boolean => "orna.kernel.value.boolean@1",
                        StandardScalar::Integer => "orna.kernel.value.integer@1",
                        StandardScalar::BigInt => "orna.kernel.value.bigint@1",
                        StandardScalar::Float => "orna.kernel.value.float@1",
                        StandardScalar::CharacterLargeObject => {
                            "orna.kernel.value.character-large-object@1"
                        }
                        StandardScalar::BinaryLargeObject => {
                            "orna.kernel.value.binary-large-object@1"
                        }
                        _ => return false,
                    }
            }),
        ResolvedType::Value(actual)
        | ResolvedType::Named(actual)
        | ResolvedType::Reference { target: actual } => actual == type_id,
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn resource_operation_result_type(
    active: &ActiveDatabaseRevision,
    operation: &ResourceOperationNode,
    context: ClientExecutionContext,
) -> Result<ResolvedType, ClientExecutionError> {
    let raw_target =
        InvocationTarget::new(operation.target_function(), operation.target_revision());
    let invalid = || {
        evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::TargetMismatch {
                expected: raw_target,
            }),
        )
    };
    let Some(resolved) = resolve_resource_operation_target(active, operation) else {
        return Err(invalid());
    };
    if resolved.definition.domain() != FunctionDomain::Server {
        return Err(invalid());
    }
    let (expected_kind, expected) = match (operation.kind(), resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => (ResourceKind::Scalar, *result),
        (ResourceKind::Stream, FunctionReturn::Stream(item)) => (ResourceKind::Stream, *item),
        _ => {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::TypeMismatch),
            ));
        }
    };
    if expected_kind != operation.kind()
        || !resource_type_matches_id(active, expected, operation.declared_result_type())
    {
        return Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::TypeMismatch),
        ));
    }
    Ok(expected)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_resource_expression(
    active: &ActiveDatabaseRevision,
    operation: &ResourceOperationNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let raw_target =
        InvocationTarget::new(operation.target_function(), operation.target_revision());
    let Some(resolved_target) = resolve_resource_operation_target(active, operation) else {
        return Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::TargetMismatch {
                expected: raw_target,
            }),
        ));
    };
    let expected_type = resource_operation_result_type(active, operation, context)?;
    let target = resolved_target.target;
    let target_definition = resolved_target.definition;
    let mut evaluated = Vec::with_capacity(operation.arguments().len());
    for (parameter, expression) in operation.arguments() {
        if evaluated
            .iter()
            .any(|candidate: &FunctionArgument| candidate.parameter() == *parameter)
        {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::DuplicateArgument {
                    parameter: *parameter,
                }),
            ));
        }
        let value = evaluate_expression_with_fuel(
            active,
            expression,
            context,
            &lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )?;
        let Some(parameter_definition) = target_definition
            .parameters()
            .iter()
            .find(|candidate| candidate.id() == *parameter)
        else {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::UnknownArgument {
                    parameter: *parameter,
                }),
            ));
        };
        if !runtime_value_matches(active, &value, parameter_definition.resolved_type()) {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::TypeMismatch),
            ));
        }
        let argument = FunctionArgument::new(*parameter, value).map_err(|_| {
            evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::ArgumentEncoding),
            )
        })?;
        evaluated.push(argument);
    }
    let evaluated = validate_resource_arguments(active, target, &evaluated).map_err(|source| {
        evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
    })?;
    let digest =
        ClientResourceKey::canonical_arguments_digest(active, &evaluated).map_err(|source| {
            evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
        })?;
    let key = ClientResourceKey::new(
        target,
        principal,
        digest,
        resource_invalidation_identity(
            active.catalogue_hash(),
            state.context().data_invalidation_token(),
            state.security_context_digest(),
            state.context(),
            state.user_state_epoch(),
        ),
    );
    let state_profile = state.context().state_profile().to_owned();
    let function_instance_key = state.context().instance_key().to_owned();
    // A changed complete key is a dependency replacement. Let the owning
    // runtime cancel any matching active generation before this lookup makes
    // the new key visible.
    let resource = if let Some(executor) = executor.as_deref_mut() {
        state
            .get_or_create_resource_with_kind_and_executor(
                active,
                key,
                operation.kind(),
                expected_type,
                executor,
            )
            .map_err(|source| {
                evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
            })?
    } else {
        state.get_or_create_resource_with_kind(key, operation.kind(), expected_type)
    };
    if resource.kind() != operation.kind() {
        return Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::TypeMismatch),
        ));
    }
    if resource.kind() == ResourceKind::Stream && resource.status() != ClientResourceStatus::Idle {
        return read_stream_resource_value(active, resource, context);
    }
    match resource.status() {
        ClientResourceStatus::Ready => {
            return resource.value().cloned().ok_or_else(|| {
                evaluate_resource_error(
                    context,
                    ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition {
                        status: ClientResourceStatus::Ready,
                    }),
                )
            });
        }
        ClientResourceStatus::Failed => {
            let code = resource
                .failure()
                .map(|failure| failure.code().to_owned())
                .unwrap_or_else(|| "resource.failed".to_owned());
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Failed(code),
            ));
        }
        ClientResourceStatus::Cancelled => {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Cancelled,
            ));
        }
        ClientResourceStatus::Loading => {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Pending {
                    key: resource.key(),
                    generation: resource.generation(),
                },
            ));
        }
        ClientResourceStatus::Idle => {}
    }
    let Some(executor) = executor.as_deref_mut() else {
        return Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::ExecutorUnavailable,
        ));
    };
    // CLIENT helper requests remain nested under the active local invocation.
    // The fresh current identity preserves audit correlation across nested calls.
    let request = resource
        .begin_request_with_context_and_kind(
            active,
            operation.kind(),
            ClientResourceInvocationContext::new(
                lineage.current,
                operation.call_site_id(),
                state_profile,
                function_instance_key,
            ),
            evaluated,
        )
        .map_err(|source| {
            evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
        })?;
    let completion = executor.execute(request.clone());
    let completion_request_id = completion.request_id();
    let (completion_key, completion_generation) = match &completion {
        ClientResourceCompletion::Ready {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamValues {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamCompleted {
            key, generation, ..
        }
        | ClientResourceCompletion::Pending {
            key, generation, ..
        }
        | ClientResourceCompletion::Failed {
            key, generation, ..
        }
        | ClientResourceCompletion::Cancelled {
            key, generation, ..
        } => (*key, *generation),
    };
    let same_generation =
        completion_key == request.key() && completion_generation == request.generation();
    let same_request = completion_request_id == request.request_id();
    if let Err(source) = resource.apply_completion(active, completion) {
        if same_generation && same_request {
            let cancellation = executor.cancel(request.clone());
            if let Ok(()) = resource.apply_completion(active, cancellation) {
                match resource.status() {
                    ClientResourceStatus::Ready if resource.kind() == ResourceKind::Stream => {
                        return read_stream_resource_value(active, resource, context);
                    }
                    ClientResourceStatus::Ready => {
                        return resource.value().cloned().ok_or_else(|| {
                            evaluate_resource_error(
                                context,
                                ClientResourceExecutionError::Invalid(
                                    ClientResourceError::TypeMismatch,
                                ),
                            )
                        });
                    }
                    ClientResourceStatus::Failed => {
                        let code = resource
                            .failure()
                            .map(|failure| failure.code().to_owned())
                            .unwrap_or_else(|| "resource.failed".to_owned());
                        return Err(evaluate_resource_error(
                            context,
                            ClientResourceExecutionError::Failed(code),
                        ));
                    }
                    ClientResourceStatus::Cancelled => {
                        return Err(evaluate_resource_error(
                            context,
                            ClientResourceExecutionError::Cancelled,
                        ));
                    }
                    ClientResourceStatus::Loading | ClientResourceStatus::Idle => {}
                }
            }
        }
        return Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(source),
        ));
    }
    if resource.kind() == ResourceKind::Stream {
        return read_stream_resource_value(active, resource, context);
    }
    match resource.status() {
        ClientResourceStatus::Ready => resource.value().cloned().ok_or_else(|| {
            evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::TypeMismatch),
            )
        }),
        ClientResourceStatus::Failed => {
            let code = resource
                .failure()
                .map(|failure| failure.code().to_owned())
                .unwrap_or_else(|| "resource.failed".to_owned());
            Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Failed(code),
            ))
        }
        ClientResourceStatus::Cancelled => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Cancelled,
        )),
        ClientResourceStatus::Loading => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Pending {
                key: resource.key(),
                generation: resource.generation(),
            },
        )),
        status => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition {
                status,
            }),
        )),
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn read_stream_resource_value(
    active: &ActiveDatabaseRevision,
    resource: &mut ClientResource,
    context: ClientExecutionContext,
) -> Result<RuntimeValue, ClientExecutionError> {
    if resource.stream_batches.is_empty() {
        match resource.status() {
            ClientResourceStatus::Failed => {
                let code = resource
                    .failure()
                    .map(|failure| failure.code().to_owned())
                    .unwrap_or_else(|| "resource.failed".to_owned());
                return Err(evaluate_resource_error(
                    context,
                    ClientResourceExecutionError::Failed(code),
                ));
            }
            ClientResourceStatus::Cancelled => {
                return Err(evaluate_resource_error(
                    context,
                    ClientResourceExecutionError::Cancelled,
                ));
            }
            ClientResourceStatus::Idle => {
                return Err(evaluate_resource_error(
                    context,
                    ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition {
                        status: ClientResourceStatus::Idle,
                    }),
                ));
            }
            ClientResourceStatus::Loading | ClientResourceStatus::Ready => {}
        }
    }
    if let Some(value) = resource.take_stream_value(active).map_err(|source| {
        evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
    })? {
        return Ok(value);
    }
    match resource.status() {
        ClientResourceStatus::Loading => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Pending {
                key: resource.key(),
                generation: resource.generation(),
            },
        )),
        ClientResourceStatus::Ready => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition {
                status: ClientResourceStatus::Ready,
            }),
        )),
        ClientResourceStatus::Failed => {
            let code = resource
                .failure()
                .map(|failure| failure.code().to_owned())
                .unwrap_or_else(|| "resource.failed".to_owned());
            Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Failed(code),
            ))
        }
        ClientResourceStatus::Cancelled => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Cancelled,
        )),
        ClientResourceStatus::Idle => Err(evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition {
                status: ClientResourceStatus::Idle,
            }),
        )),
    }
}

fn action_payload_error(message: impl Into<String>) -> ClientActionError {
    ClientActionError::InvalidPayload(message.into())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub fn encode_action_payload(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<Vec<u8>, ClientActionError> {
    for identity in [
        descriptor.target.to_bytes(),
        descriptor.target_revision.source().to_bytes(),
        descriptor.target_revision.catalogue().to_bytes(),
        descriptor.call_site.to_bytes(),
        descriptor.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(action_payload_error("invalid action identity"));
        }
    }
    for pair in descriptor.arguments.windows(2) {
        if pair[0].parameter() >= pair[1].parameter() {
            return Err(action_payload_error(
                "arguments are not in ascending parameter order",
            ));
        }
    }
    if descriptor.arguments.len() > orna_artifact::client_plan::MAX_ACTION_ARGUMENTS {
        return Err(action_payload_error("too many action arguments"));
    }
    for argument in &descriptor.arguments {
        if argument.parameter().to_bytes() == [0; 16] {
            return Err(action_payload_error("invalid action identity"));
        }
    }
    let mut body = Vec::new();
    body.push(match descriptor.domain {
        ActionTargetDomain::Client => 1,
        ActionTargetDomain::Server => 2,
    });
    body.extend_from_slice(&descriptor.target.to_bytes());
    body.extend_from_slice(&descriptor.target_revision.source().to_bytes());
    body.extend_from_slice(&descriptor.target_revision.catalogue().to_bytes());
    body.extend_from_slice(&descriptor.call_site.to_bytes());
    body.extend_from_slice(&descriptor.result_type.to_bytes());
    body.extend_from_slice(&(descriptor.arguments.len() as u32).to_be_bytes());
    for argument in &descriptor.arguments {
        body.extend_from_slice(&argument.parameter().to_bytes());
        let frame = encode_active_value(active, argument.value())
            .map_err(|source| action_payload_error(source.to_string()))?;
        let length = u32::try_from(frame.len())
            .map_err(|_| action_payload_error("argument frame is too large"))?;
        let additional = 4usize
            .checked_add(frame.len())
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        let next_len = body
            .len()
            .checked_add(additional)
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        let payload_len = ACTION_MAGIC
            .len()
            .checked_add(4)
            .and_then(|prefix| prefix.checked_add(next_len))
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        if payload_len > MAX_ACTION_PAYLOAD_LENGTH {
            return Err(action_payload_error("action payload is too large"));
        }
        body.try_reserve(additional)
            .map_err(|_| action_payload_error("action payload allocation failed"))?;
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&frame);
    }
    let length = u32::try_from(body.len())
        .map_err(|_| action_payload_error("action payload is too large"))?;
    let payload_len = ACTION_MAGIC
        .len()
        .checked_add(4)
        .and_then(|prefix| prefix.checked_add(body.len()))
        .ok_or_else(|| action_payload_error("action payload is too large"))?;
    if payload_len > MAX_ACTION_PAYLOAD_LENGTH {
        return Err(action_payload_error("action payload is too large"));
    }
    let mut payload = Vec::new();
    payload
        .try_reserve(payload_len)
        .map_err(|_| action_payload_error("action payload allocation failed"))?;
    payload.extend_from_slice(ACTION_MAGIC.as_bytes());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&body);
    Ok(payload)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn action_take<'a>(
    body: &'a [u8],
    offset: &mut usize,
    length: usize,
) -> Result<&'a [u8], ClientActionError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| action_payload_error("action payload overflow"))?;
    if end > body.len() {
        return Err(action_payload_error("truncated action payload"));
    }
    let value = &body[*offset..end];
    *offset = end;
    Ok(value)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn action_identity_bytes(body: &[u8], offset: &mut usize) -> Result<[u8; 16], ClientActionError> {
    let identity = action_take(body, offset, 16)?
        .try_into()
        .expect("action identities are exactly sixteen bytes");
    if identity == [0; 16] {
        return Err(action_payload_error("invalid action identity"));
    }
    Ok(identity)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub fn decode_action_payload(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
) -> Result<ClientActionDescriptor, ClientActionError> {
    let magic = ACTION_MAGIC.as_bytes();
    if payload.len() < magic.len() + 4 || !payload.starts_with(magic) {
        return Err(action_payload_error("invalid action magic"));
    }
    let mut cursor = magic.len();
    let body_length = u32::from_be_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    if payload.len() > MAX_ACTION_PAYLOAD_LENGTH || body_length > MAX_ACTION_PAYLOAD_LENGTH {
        return Err(action_payload_error("action payload is too large"));
    }
    if body_length != payload.len() - cursor {
        return Err(action_payload_error("action payload length does not match"));
    }
    let body = &payload[cursor..];
    let mut offset = 0usize;
    let domain = match action_take(body, &mut offset, 1)?[0] {
        1 => ActionTargetDomain::Client,
        2 => ActionTargetDomain::Server,
        _ => return Err(action_payload_error("unknown action domain")),
    };
    let target = FunctionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let source = orna_core::SourceRevisionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let catalogue =
        orna_core::CatalogueRevisionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let target_revision = RevisionPair::new(source, catalogue);
    if target_revision != active.pair() {
        return Err(ClientActionError::RevisionMismatch);
    }
    let call_site = CallSiteId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let result_type = TypeId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let count = u32::from_be_bytes(action_take(body, &mut offset, 4)?.try_into().unwrap()) as usize;
    if count > orna_artifact::client_plan::MAX_ACTION_ARGUMENTS {
        return Err(action_payload_error("too many action arguments"));
    }
    let mut arguments = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let parameter = ParameterId::from_bytes(action_identity_bytes(body, &mut offset)?);
        if previous.is_some_and(|value| parameter <= value) {
            return Err(action_payload_error("action arguments are not canonical"));
        }
        previous = Some(parameter);
        let frame_length =
            u32::from_be_bytes(action_take(body, &mut offset, 4)?.try_into().unwrap()) as usize;
        let frame = action_take(body, &mut offset, frame_length)?;
        let value = decode_active_value(active, frame)
            .map_err(|source| action_payload_error(source.to_string()))?;
        arguments.push(
            FunctionArgument::new(parameter, value)
                .map_err(|source| action_payload_error(source.to_string()))?,
        );
    }
    if offset != body.len() {
        return Err(action_payload_error("trailing action payload bytes"));
    }
    let descriptor = ClientActionDescriptor::new(
        domain,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    );
    if encode_action_payload(active, &descriptor)? != payload {
        return Err(action_payload_error("non-canonical action payload"));
    }
    Ok(descriptor)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn action_target_result_type(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<(ResourceKind, ResolvedType), ClientActionError> {
    let resolved_target = resolve_action_target(active, descriptor)?;
    let resolved = match resolved_target.definition.return_type() {
        FunctionReturn::Single(resolved) => *resolved,
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => {
            return Err(ClientActionError::ResultTypeMismatch);
        }
    };
    let kind = ResourceKind::Scalar;
    if !resource_type_matches_id(active, resolved, descriptor.result_type) {
        return Err(ClientActionError::ResultTypeMismatch);
    }
    Ok((kind, resolved))
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_action_arguments(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<Vec<FunctionArgument>, ClientActionError> {
    let resolved_target = resolve_action_target(active, descriptor)?;
    let definition = resolved_target.definition;
    if descriptor.arguments.len() != definition.parameters().len() {
        return Err(ClientActionError::Arguments(Box::new(
            ClientResourceError::TypeMismatch,
        )));
    }
    let mut previous = None;
    for argument in &descriptor.arguments {
        if previous.is_some_and(|value| argument.parameter() <= value) {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::DuplicateArgument {
                    parameter: argument.parameter(),
                },
            )));
        }
        previous = Some(argument.parameter());
        let Some(parameter) = definition
            .parameters()
            .iter()
            .find(|candidate| candidate.id() == argument.parameter())
        else {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::UnknownArgument {
                    parameter: argument.parameter(),
                },
            )));
        };
        if !runtime_value_matches(active, argument.value(), parameter.resolved_type()) {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::TypeMismatch,
            )));
        }
    }
    Ok(descriptor.arguments.clone())
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_action_operation(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut values = Vec::with_capacity(operation.arguments().len());
    for (parameter, expression) in operation.arguments() {
        let value = evaluate_expression_with_fuel(
            active,
            expression,
            context,
            &lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )?;
        values.push(
            FunctionArgument::new(*parameter, value)
                .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?,
        );
    }
    let descriptor = ClientActionDescriptor::new(
        operation.domain(),
        operation.target(),
        operation.target_revision(),
        operation.call_site_id(),
        values,
        operation.result_type(),
    );
    action_target_result_type(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    validate_action_arguments(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    let payload = encode_action_payload(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| expression_error(context, ClientExpressionError::TypeMismatch))?;
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| expression_error(context, ClientExpressionError::TypeMismatch))?;
    let value = OpaqueValue::new(active, &registry, STD_ACTION_TYPE_ID, payload)
        .map_err(|_| expression_error(context, ClientExpressionError::TypeMismatch))?;
    Ok(RuntimeValue::Opaque(value))
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn complete_client_action(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    complete_client_action_inner(active, action_state, completion, executor, true)
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn complete_client_action_inner(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
    cancel_on_invalid: bool,
) -> Result<ClientActionOutcome, ClientActionError> {
    let completion_request_id = completion.request_id();
    let (completion_key, completion_generation) = match &completion {
        ClientResourceCompletion::Ready {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamValues {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamCompleted {
            key, generation, ..
        }
        | ClientResourceCompletion::Pending {
            key, generation, ..
        }
        | ClientResourceCompletion::Failed {
            key, generation, ..
        }
        | ClientResourceCompletion::Cancelled {
            key, generation, ..
        } => (*key, *generation),
    };
    let Some(resource) = action_state.resource.as_ref() else {
        return if action_state.is_stale(completion_generation) {
            Err(ClientActionError::StaleCompletion)
        } else {
            Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()))
        };
    };
    if completion_generation != resource.generation()
        || completion_key != resource.key()
        || resource.request_id() != Some(completion_request_id)
    {
        return Err(ClientActionError::StaleCompletion);
    }
    let completion_is_non_terminal = matches!(
        &completion,
        ClientResourceCompletion::Pending { .. } | ClientResourceCompletion::StreamValues { .. }
    );
    let apply_result = action_state
        .resource_mut()
        .expect("action resource was checked above")
        .apply_completion(active, completion);
    if apply_result.is_err() {
        // A same-generation malformed completion must not strand the request
        // owned by the executor. Generation and key mismatches remain stale
        // and do not cancel a newer or unrelated request. A valid pending
        // cancellation retains Loading state because the executor still owns
        // the request; a malformed terminal cancellation is treated as
        // consumed and moves the resource to the explicit Cancelled state.
        if cancel_on_invalid {
            let cancel_request = action_state
                .resource
                .as_ref()
                .and_then(|resource| resource.active_request());
            if let Some(request) = cancel_request {
                let cancellation = executor.cancel(request);
                let cancellation_is_non_terminal = matches!(
                    &cancellation,
                    ClientResourceCompletion::Pending { .. }
                        | ClientResourceCompletion::StreamValues { .. }
                );
                match action_state
                    .resource_mut()
                    .expect("action resource remains after malformed completion")
                    .apply_completion(active, cancellation)
                {
                    Ok(()) => {
                        let status = action_state
                            .resource
                            .as_ref()
                            .expect("action resource remains after cancellation")
                            .status();
                        if status == ClientResourceStatus::Loading {
                            return Err(ClientActionError::Pending);
                        }
                        let outcome = match status {
                            ClientResourceStatus::Ready => ClientActionOutcome::Completed,
                            ClientResourceStatus::Failed => redacted_action_failure(),
                            ClientResourceStatus::Cancelled => ClientActionOutcome::Cancelled,
                            ClientResourceStatus::Idle | ClientResourceStatus::Loading => {
                                unreachable!()
                            }
                        };
                        action_state.clear();
                        return Ok(outcome);
                    }
                    Err(error) => {
                        if matches!(
                            error,
                            ClientResourceError::StaleGeneration { .. }
                                | ClientResourceError::RequestKeyMismatch { .. }
                                | ClientResourceError::RequestIdMismatch { .. }
                        ) {
                            return Err(ClientActionError::StaleCompletion);
                        }
                        if cancellation_is_non_terminal {
                            return Err(ClientActionError::Pending);
                        }
                        action_state
                            .resource_mut()
                            .expect("action resource remains after consumed cancellation")
                            .mark_executor_released_cancelled();
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                }
            }
        } else if completion_is_non_terminal {
            return Err(ClientActionError::Pending);
        } else {
            action_state
                .resource_mut()
                .expect("action resource remains after consumed cancellation")
                .mark_executor_released_cancelled();
            return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
        }
        action_state.clear();
        return Ok(redacted_action_failure());
    }
    let status = action_state
        .resource
        .as_ref()
        .expect("action resource remains after completion")
        .status();
    if status == ClientResourceStatus::Loading {
        return Err(ClientActionError::Pending);
    }
    let outcome = match status {
        ClientResourceStatus::Ready => ClientActionOutcome::Completed,
        ClientResourceStatus::Failed => redacted_action_failure(),
        ClientResourceStatus::Cancelled => ClientActionOutcome::Cancelled,
        ClientResourceStatus::Idle | ClientResourceStatus::Loading => unreachable!(),
    };
    action_state.clear();
    Ok(outcome)
}

/// Cancels one pending SERVER action through its resource executor.
///
/// The executor owns the transport control. A terminal completion clears the
/// action state; a pending completion retains it for a later completion.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn cancel_client_action_with_executor(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    let Some(resource) = action_state.resource.as_ref() else {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    };
    if resource.status() != ClientResourceStatus::Loading {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    }
    let Some(request) = action_state.request.clone() else {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    };
    let completion = executor.cancel(request);
    complete_client_action_inner(active, action_state, completion, executor, false)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn trigger_client_action(
    active: &ActiveDatabaseRevision,
    action: &RuntimeValue,
    authorisation: &AuthorisedInvocation,
    parent: &ClientExecutionContext,
    action_state: &mut ClientActionState,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    trigger_client_action_with_lineage(
        active,
        action,
        authorisation,
        parent,
        action_state,
        declarations,
        grants,
        state,
        parent.observer_lineage(),
        executor,
    )
}

fn client_action_target_is_provenance_safe(
    active: &ActiveDatabaseRevision,
    parent: ClientExecutionContext,
    target: FunctionId,
) -> bool {
    let Some(owner) = resolve_client_function(active, parent.function()) else {
        return false;
    };
    owner.revision.id() == parent.function_revision()
        && owner.definition.domain() == FunctionDomain::Client
        && owner.references.iter().any(|reference| {
            reference.source_function() == parent.function()
                && reference.source_revision() == parent.function_revision()
                && reference.kind() == DefinitionReferenceKind::FunctionCall
                && reference.target() == DefinitionReferenceTarget::Function(target)
        })
}

/// Adapts nested CLIENT resource execution to the terminal action contract.
///
/// A nested resource has no independent action completion surface. If its
/// executor reports `Pending`, the adapter cannot create a local cancellation:
/// the remote executor may still publish a committed terminal result. It
/// retains the request for the caller instead.
struct ClientActionNestedExecutor<'a> {
    inner: &'a mut dyn ClientResourceExecutor,
    pending_request: Option<ClientResourceRequest>,
}

impl ClientActionNestedExecutor<'_> {
    fn release_failed(&self) -> bool {
        self.pending_request.is_some()
    }

    fn pending_request_identity(
        &self,
    ) -> Option<(InvocationId, ClientResourceKey, ClientResourceGeneration)> {
        self.pending_request
            .as_ref()
            .map(|request| (request.request_id(), request.key(), request.generation()))
    }

    fn pending_matches(&self, request: &ClientResourceRequest) -> bool {
        self.pending_request.as_ref().is_none_or(|pending| {
            pending.request_id() == request.request_id()
                && pending.key() == request.key()
                && pending.generation() == request.generation()
        })
    }
}

impl ClientResourceExecutor for ClientActionNestedExecutor<'_> {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if !self.pending_matches(&request) {
            return request.failed(ACTION_FAILURE_CODE.to_owned());
        }
        let completion = self.inner.execute(request.clone());
        if !completion.matches_request(&request) {
            // A mismatched child result cannot prove that the original
            // request was released. Retain the original until explicit
            // abandonment.
            self.pending_request = Some(request.clone());
            return request.pending();
        }

        if matches!(completion, ClientResourceCompletion::Pending { .. }) {
            return self.cancel(request);
        }
        if matches!(completion, ClientResourceCompletion::StreamValues { .. }) {
            // A nested action has no poll surface of its own. Retain the
            // executor-owned request until a later terminal completion or
            // explicit abandonment arrives.
            self.pending_request = Some(request);
        } else if self.pending_request.is_some() {
            // A matching terminal completion proves that the child executor
            // consumed its request. Do not report a released child as still
            // owned when a prior stream batch was followed by completion.
            self.pending_request = None;
        }
        completion
    }

    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        if !self.pending_matches(&request) {
            return Err("resource executor request mismatch".to_owned());
        }
        match self.inner.abandon(request.clone()) {
            Ok(()) => {
                if self.pending_request.is_some() {
                    self.pending_request = None;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if !self.pending_matches(&request) {
            return request.failed(ACTION_FAILURE_CODE.to_owned());
        }
        let completion = self.inner.cancel(request.clone());
        if !completion.matches_request(&request) {
            // A mismatched child result cannot prove that the original
            // request was released. Retain the original until explicit
            // abandonment.
            self.pending_request = Some(request.clone());
            return request.pending();
        }
        if matches!(
            completion,
            ClientResourceCompletion::Pending { .. }
                | ClientResourceCompletion::StreamValues { .. },
        ) {
            self.pending_request = Some(request);
        } else if self.pending_request.is_some() {
            self.pending_request = None;
        }
        completion
    }

    fn read_input(&mut self, context: ClientExecutionContext) -> Result<RuntimeValue, String> {
        self.inner.read_input(context)
    }

    fn evaluate_command(
        &mut self,
        context: ClientExecutionContext,
        command: &str,
    ) -> Result<RuntimeValue, String> {
        self.inner.evaluate_command(context, command)
    }
    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        self.inner.inspect(request)
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        self.inner.external_contract(request)
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn trigger_client_action_with_lineage(
    active: &ActiveDatabaseRevision,
    action: &RuntimeValue,
    authorisation: &AuthorisedInvocation,
    parent: &ClientExecutionContext,
    action_state: &mut ClientActionState,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    lineage: ObserverLineage,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    if parent.pair() != active.pair()
        || authorisation.target().revision() != active.pair()
        || authorisation.target().function() != parent.function()
    {
        return Err(ClientActionError::RevisionMismatch);
    }
    validate_active_catalogue(active, parent.function())
        .map_err(|_| ClientActionError::TargetMismatch)?;
    let RuntimeValue::Opaque(value) = action else {
        return Err(ClientActionError::InvalidValue);
    };
    if value.opaque_type() != STD_ACTION_TYPE_ID {
        return Err(ClientActionError::InvalidValue);
    }
    let descriptor = decode_action_payload(active, value.canonical_payload())?;
    let (kind, expected) = action_target_result_type(active, &descriptor)?;
    let values = validate_action_arguments(active, &descriptor)?;
    let target = resolve_action_target(active, &descriptor)?.target;
    let digest = ClientResourceKey::canonical_arguments_digest(active, &values)
        .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
    if !client_action_target_is_provenance_safe(active, *parent, descriptor.target) {
        return Err(ClientActionError::TargetMismatch);
    }
    // Call-site metadata in a transient action payload is caller-controlled.
    // Keep it out of the invocation context until the reference schema carries
    // an authenticated binding for it; a fresh identity prevents forged
    // metadata from spoofing nested audit correlation.
    let call_site = CallSiteId::new();
    match descriptor.domain {
        ActionTargetDomain::Server => {
            let key = ClientResourceKey::new(
                target,
                authorisation.session_principal(),
                digest,
                resource_invalidation_identity(
                    active.catalogue_hash(),
                    state.context().data_invalidation_token(),
                    security_context_digest(authorisation),
                    state.context(),
                    state.user_state_epoch(),
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key {
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                    return Err(ClientActionError::Pending);
                }
                action_state.clear();
            }
            let mut resource = ClientResource::new_with_kind(key, kind, expected);
            // Preserve a monotonic generation across terminal clears so an old
            // completion can never be accepted by a later action.
            resource.generation = ClientResourceGeneration(action_state.tombstone.value());
            let request = resource
                .begin_request_with_context_and_kind(
                    active,
                    kind,
                    ClientResourceInvocationContext::new(
                        lineage.current,
                        call_site,
                        state.context().state_profile().to_owned(),
                        state.context().instance_key().to_owned(),
                    ),
                    values,
                )
                .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
            action_state.stage_invocation(request.request_id());
            action_state.stage_request(request.clone());
            action_state.set_resource(resource);
            let completion = executor.execute(request);
            complete_client_action(active, action_state, completion, executor)
        }
        ActionTargetDomain::Client => {
            let key = ClientResourceKey::new(
                target,
                authorisation.session_principal(),
                digest,
                resource_invalidation_identity(
                    active.catalogue_hash(),
                    state.context().data_invalidation_token(),
                    security_context_digest(authorisation),
                    state.context(),
                    state.user_state_epoch(),
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key {
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                    return Err(ClientActionError::Pending);
                }
                action_state.clear();
            }
            let mut resource = ClientResource::new_with_kind(key, kind, expected);
            // Preserve a monotonic generation across terminal clears so an old
            // completion can never be accepted by a later action.
            resource.generation = ClientResourceGeneration(action_state.tombstone.value());
            let request = resource
                .begin_request_with_context_and_kind(
                    active,
                    kind,
                    ClientResourceInvocationContext::new(
                        lineage.current,
                        call_site,
                        state.context().state_profile().to_owned(),
                        state.context().instance_key().to_owned(),
                    ),
                    values,
                )
                .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
            action_state.stage_invocation(request.request_id());
            action_state.stage_request(request.clone());
            action_state.set_resource(resource);

            let mut staged = state.clone();
            staged.set_security_context_digest(security_context_digest(authorisation));
            let mut nested_executor = ClientActionNestedExecutor {
                inner: executor,
                pending_request: None,
            };
            let mut nested = Some(&mut nested_executor as &mut dyn ClientResourceExecutor);
            let result = evaluate_function(
                active,
                descriptor.target,
                request
                    .arguments()
                    .iter()
                    .map(|argument| (argument.parameter(), argument.value().clone()))
                    .collect(),
                declarations,
                grants,
                &mut staged,
                0,
                authorisation.session_principal(),
                lineage.with_current(request.request_id()),
                &mut nested,
            );
            if nested_executor.release_failed() {
                let changed_resources: Vec<_> = staged
                    .resources
                    .iter()
                    .filter_map(|(candidate_key, resource)| {
                        let replacement_cancelled =
                            state.resources.get(candidate_key).is_some_and(|previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            });
                        let replacement_terminal = same_revision_terminal_replacement(
                            active,
                            state,
                            candidate_key,
                            resource,
                        );
                        let pending_resource = nested_executor
                            .pending_request_identity()
                            .is_some_and(|(_, pending_key, pending_generation)| {
                                resource.key() == pending_key
                                    && resource.generation() == pending_generation
                                    && resource.status() == ClientResourceStatus::Loading
                            });
                        (pending_resource || replacement_cancelled || replacement_terminal)
                            .then_some((*candidate_key, resource.clone()))
                    })
                    .collect();
                for (_, resource) in changed_resources {
                    state.retain_resource(resource);
                }
                if let Some((request_id, key, generation)) =
                    nested_executor.pending_request_identity()
                {
                    action_state.clear();
                    return Err(ClientActionError::ExecutorPending {
                        code: ACTION_FAILURE_CODE.to_owned(),
                        request_id,
                        key,
                        generation,
                    });
                }
                // The child request remains owned by the executor, but no
                // retained resource can safely consume it until the caller
                // resumes the handoff. Do not retain the synthetic outer
                // request.
                action_state.clear();
                return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
            }
            let result_is_err = result.is_err();
            let completion = match result {
                Ok((_, value)) => request.ready(value),
                Err(ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Cancelled,
                    ..
                }) => request.cancelled(),
                Err(ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Pending { .. },
                    ..
                }) => request.cancelled(),
                Err(_) => request.failed(ACTION_FAILURE_CODE.to_owned()),
            };
            if result_is_err {
                for (key, resource) in &staged.resources {
                    let replacement_cancelled = state.resources.get(key).is_some_and(|previous| {
                        previous.status() == ClientResourceStatus::Loading
                            && resource.status() == ClientResourceStatus::Idle
                            && resource.generation().value() > previous.generation().value()
                    });
                    let replacement_terminal =
                        same_revision_terminal_replacement(active, state, key, resource);
                    if replacement_cancelled || replacement_terminal {
                        state.retain_resource(resource.clone());
                    }
                }
            }

            let outcome =
                complete_client_action(active, action_state, completion, &mut nested_executor)?;
            if outcome == ClientActionOutcome::Completed {
                *state = staged;
            }
            Ok(outcome)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardUiConstructorKind {
    Text,
    Button,
    Panel,
    Row,
    Column,
    TextInput,
    Tabs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardUiConstructorParameterKind {
    Text,
    Boolean,
    Content,
}

#[derive(Clone, Copy)]
struct StandardUiConstructorSpec {
    function: FunctionId,
    revision: FunctionRevisionId,
    identity: &'static str,
    node_contract: &'static str,
    kind: StandardUiConstructorKind,
    parameters: &'static [(ParameterId, StandardUiConstructorParameterKind)],
}

const STD_UI_TEXT_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_TEXT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    )];
const STD_UI_BUTTON_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[
        (
            STD_UI_BUTTON_LABEL_PARAMETER_ID,
            StandardUiConstructorParameterKind::Text,
        ),
        (
            STD_UI_BUTTON_ENABLED_PARAMETER_ID,
            StandardUiConstructorParameterKind::Boolean,
        ),
    ];
const STD_UI_PANEL_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_PANEL_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_ROW_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_ROW_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_COLUMN_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_COLUMN_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_TEXT_INPUT_CONSTRUCTOR_PARAMETERS: &[(
    ParameterId,
    StandardUiConstructorParameterKind,
)] = &[
    (
        STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    ),
    (
        STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    ),
    (
        STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
        StandardUiConstructorParameterKind::Boolean,
    ),
];
const STD_UI_TABS_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_TABS_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];

const STD_UI_TEXT_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TEXT_FUNCTION_ID,
    revision: STD_UI_TEXT_FUNCTION_REVISION_ID,
    identity: STD_UI_TEXT_RUNTIME_CONTRACT,
    node_contract: "std.ui.text",
    kind: StandardUiConstructorKind::Text,
    parameters: STD_UI_TEXT_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_BUTTON_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_BUTTON_FUNCTION_ID,
    revision: STD_UI_BUTTON_FUNCTION_REVISION_ID,
    identity: STD_UI_BUTTON_RUNTIME_CONTRACT,
    node_contract: "std.ui.button",
    kind: StandardUiConstructorKind::Button,
    parameters: STD_UI_BUTTON_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_PANEL_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_PANEL_FUNCTION_ID,
    revision: STD_UI_PANEL_FUNCTION_REVISION_ID,
    identity: STD_UI_PANEL_RUNTIME_CONTRACT,
    node_contract: "std.ui.panel",
    kind: StandardUiConstructorKind::Panel,
    parameters: STD_UI_PANEL_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_ROW_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_ROW_FUNCTION_ID,
    revision: STD_UI_ROW_FUNCTION_REVISION_ID,
    identity: STD_UI_ROW_RUNTIME_CONTRACT,
    node_contract: "std.ui.row",
    kind: StandardUiConstructorKind::Row,
    parameters: STD_UI_ROW_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_COLUMN_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_COLUMN_FUNCTION_ID,
    revision: STD_UI_COLUMN_FUNCTION_REVISION_ID,
    identity: STD_UI_COLUMN_RUNTIME_CONTRACT,
    node_contract: "std.ui.column",
    kind: StandardUiConstructorKind::Column,
    parameters: STD_UI_COLUMN_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_TEXT_INPUT_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TEXT_INPUT_FUNCTION_ID,
    revision: STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
    identity: STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
    node_contract: "std.ui.text_input",
    kind: StandardUiConstructorKind::TextInput,
    parameters: STD_UI_TEXT_INPUT_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_TABS_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TABS_FUNCTION_ID,
    revision: STD_UI_TABS_FUNCTION_REVISION_ID,
    identity: STD_UI_TABS_RUNTIME_CONTRACT,
    node_contract: "std.ui.tabs",
    kind: StandardUiConstructorKind::Tabs,
    parameters: STD_UI_TABS_CONSTRUCTOR_PARAMETERS,
};

fn standard_ui_constructor_spec(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    identity: &str,
) -> Option<&'static StandardUiConstructorSpec> {
    // Application definitions retain precedence. A user-owned function that
    // happens to spell a standard contract must remain a generic external
    // contract, even if it reuses one of the reserved identities.
    if context.pair() != active.pair()
        || active
            .catalogue()
            .function_by_id(context.function())
            .is_some()
    {
        return None;
    }
    let spec = match context.function() {
        STD_UI_TEXT_FUNCTION_ID => &STD_UI_TEXT_CONSTRUCTOR,
        STD_UI_BUTTON_FUNCTION_ID => &STD_UI_BUTTON_CONSTRUCTOR,
        STD_UI_PANEL_FUNCTION_ID => &STD_UI_PANEL_CONSTRUCTOR,
        STD_UI_ROW_FUNCTION_ID => &STD_UI_ROW_CONSTRUCTOR,
        STD_UI_COLUMN_FUNCTION_ID => &STD_UI_COLUMN_CONSTRUCTOR,
        STD_UI_TEXT_INPUT_FUNCTION_ID => &STD_UI_TEXT_INPUT_CONSTRUCTOR,
        STD_UI_TABS_FUNCTION_ID => &STD_UI_TABS_CONSTRUCTOR,
        _ => return None,
    };
    (spec.function == context.function()
        && spec.revision == context.function_revision
        && spec.identity == identity)
        .then_some(spec)
}

fn invalid_ui_constructor_value(
    context: ClientExecutionContext,
    source: OpaqueValueError,
) -> Box<ClientExecutionError> {
    Box::new(ClientExecutionError::InvalidOpaqueValue {
        context,
        source: ClientOpaqueValueError::Value(source),
    })
}

fn invalid_ui_constructor_registry(
    context: ClientExecutionContext,
    source: RegisteredOpaqueCodecsError,
) -> Box<ClientExecutionError> {
    Box::new(ClientExecutionError::InvalidOpaqueValue {
        context,
        source: ClientOpaqueValueError::Registry(Box::new(source)),
    })
}

fn ui_constructor_parameter_matches(
    value: &RuntimeValue,
    kind: StandardUiConstructorParameterKind,
) -> bool {
    match kind {
        StandardUiConstructorParameterKind::Text => matches!(value, RuntimeValue::Text(_)),
        StandardUiConstructorParameterKind::Boolean => matches!(value, RuntimeValue::Boolean(_)),
        StandardUiConstructorParameterKind::Content => {
            matches!(value, RuntimeValue::Opaque(opaque) if opaque.opaque_type() == STD_UI_TYPE_ID)
        }
    }
}

fn ui_constructor_text_property(value: &str) -> Value {
    let mut property = Map::new();
    property.insert(
        "type".to_owned(),
        Value::String("std.types.text".to_owned()),
    );
    property.insert("value".to_owned(), Value::String(value.to_owned()));
    Value::Object(property)
}

fn ui_constructor_boolean_property(value: bool) -> Value {
    let mut property = Map::new();
    property.insert(
        "type".to_owned(),
        Value::String("std.types.boolean".to_owned()),
    );
    property.insert("value".to_owned(), Value::Bool(value));
    Value::Object(property)
}

fn decode_ui_constructor_body(payload: &[u8]) -> Result<Value, OpaqueValueError> {
    let magic = UI_MAGIC.as_bytes();
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_UI_TYPE_ID,
        })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            }
        } else {
            OpaqueValueError::InvalidMagic {
                opaque_type: STD_UI_TYPE_ID,
            }
        });
    }
    let body_length = usize::try_from(u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the UI length prefix is exactly four bytes"),
    ))
    .map_err(|_| OpaqueValueError::InvalidFrameLength {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    let body_end =
        prefix_length
            .checked_add(body_length)
            .ok_or(OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            })?;
    if body_length > orna_core::value::MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || body_end != payload.len()
    {
        return Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_UI_TYPE_ID,
        });
    }
    let body = &payload[prefix_length..body_end];
    let value = serde_json::from_slice(body).map_err(|_| OpaqueValueError::InvalidJsonBody {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    let canonical = serde_json::to_vec(&value).map_err(|_| OpaqueValueError::InvalidJsonBody {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    if canonical != body {
        return Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_UI_TYPE_ID,
        });
    }
    Ok(value)
}

fn evaluate_standard_ui_constructor(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    spec: &StandardUiConstructorSpec,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<RuntimeValue, Box<ClientExecutionError>> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::ActiveStandardRequired,
        ));
    };
    if !((standard.revision() == STANDARD_LIBRARY_V9_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V9_REVISION_ID)
        || (standard.revision() == STANDARD_LIBRARY_V10_REVISION_ID
            && standard.catalogue().revision() == STANDARD_CATALOGUE_V10_REVISION_ID))
    {
        return Err(invalid_ui_constructor_registry(
            context,
            RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot,
        ));
    }
    let registry = registered_opaque_codecs(standard)
        .map_err(|source| invalid_ui_constructor_registry(context, source))?;

    if arguments.len() != spec.parameters.len()
        || arguments
            .iter()
            .zip(spec.parameters)
            .any(|((parameter, _), (expected, _))| parameter != expected)
    {
        return Err(Box::new(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        )));
    }
    if arguments
        .iter()
        .zip(spec.parameters)
        .any(|((_, value), (_, kind))| !ui_constructor_parameter_matches(value, *kind))
    {
        return Err(Box::new(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        )));
    }
    if arguments
        .iter()
        .zip(spec.parameters)
        .any(|((_, value), (_, kind))| {
            matches!(
                (kind, value),
                (
                    StandardUiConstructorParameterKind::Text,
                    RuntimeValue::Text(text)
                ) if text.len() > runtime_loader::CLIENT_MAX_RUNTIME_TEXT_BYTES
            )
        })
    {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        ));
    }

    let mut properties = Map::new();
    let mut slots = Map::new();
    match spec.kind {
        StandardUiConstructorKind::Text => {
            let RuntimeValue::Text(text) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("text".to_owned(), ui_constructor_text_property(text));
        }
        StandardUiConstructorKind::Button => {
            let RuntimeValue::Text(label) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Boolean(enabled) = arguments[1].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("label".to_owned(), ui_constructor_text_property(label));
            properties.insert(
                "enabled".to_owned(),
                ui_constructor_boolean_property(enabled),
            );
        }
        StandardUiConstructorKind::TextInput => {
            let RuntimeValue::Text(text) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Text(placeholder) = &arguments[1].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Boolean(enabled) = arguments[2].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("text".to_owned(), ui_constructor_text_property(text));
            properties.insert(
                "placeholder".to_owned(),
                ui_constructor_text_property(placeholder),
            );
            properties.insert(
                "enabled".to_owned(),
                ui_constructor_boolean_property(enabled),
            );
        }
        StandardUiConstructorKind::Panel
        | StandardUiConstructorKind::Row
        | StandardUiConstructorKind::Column
        | StandardUiConstructorKind::Tabs => {
            let RuntimeValue::Opaque(content) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let content = OpaqueValue::new(
                active,
                &registry,
                STD_UI_TYPE_ID,
                content.canonical_payload(),
            )
            .map_err(|source| invalid_ui_constructor_value(context, source))?;
            let content = decode_ui_constructor_body(content.canonical_payload())
                .map_err(|source| invalid_ui_constructor_value(context, source))?;
            slots.insert("content".to_owned(), Value::Array(vec![content]));
        }
    }

    let mut node = Map::new();
    node.insert("kind".to_owned(), Value::String("node".to_owned()));
    let mut contract = Map::new();
    contract.insert(
        "id".to_owned(),
        Value::String(spec.node_contract.to_owned()),
    );
    contract.insert(
        "name".to_owned(),
        Value::String(spec.node_contract.to_owned()),
    );
    contract.insert("version".to_owned(), Value::String("1.0".to_owned()));
    node.insert("contract".to_owned(), Value::Object(contract));
    node.insert("properties".to_owned(), Value::Object(properties));
    node.insert("slots".to_owned(), Value::Object(slots));
    node.insert("actions".to_owned(), Value::Object(Map::new()));
    let body = serde_json::to_vec(&Value::Object(node)).map_err(|_| {
        invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidJsonBody {
                opaque_type: STD_UI_TYPE_ID,
            },
        )
    })?;
    let body_length = u32::try_from(body.len()).map_err(|_| {
        invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        )
    })?;
    if body.len() > orna_core::value::MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        ));
    }
    let payload_capacity = UI_MAGIC
        .len()
        .checked_add(4)
        .and_then(|length| length.checked_add(body.len()))
        .ok_or_else(|| {
            invalid_ui_constructor_value(
                context,
                OpaqueValueError::InvalidFrameLength {
                    opaque_type: STD_UI_TYPE_ID,
                },
            )
        })?;
    let mut payload = Vec::with_capacity(payload_capacity);
    payload.extend_from_slice(UI_MAGIC.as_bytes());
    payload.extend_from_slice(&body_length.to_be_bytes());
    payload.extend_from_slice(&body);
    let value = OpaqueValue::new(active, &registry, STD_UI_TYPE_ID, payload)
        .map_err(|source| invalid_ui_constructor_value(context, source))?;
    Ok(RuntimeValue::Opaque(value))
}
pub(crate) fn stable_inspect_provider_error(error: &str) -> String {
    stable_inspect_error_code(error).to_owned()
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_external_contract(
    identity: &str,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
) -> Result<RuntimeValue, ClientExecutionError> {
    let Some(executor) = executor.as_deref_mut() else {
        if identity == INSPECT_RENDER_CONTRACT {
            return Err(ClientExecutionError::Inspect {
                context,
                source: ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
            });
        }
        return Err(ClientExecutionError::ExternalContract {
            context,
            identity: identity.to_owned(),
        });
    };
    let request =
        ClientExternalContractRequest::with_lineage(context, identity, arguments.to_vec(), lineage);
    executor.external_contract(request).map_err(|code| {
        if identity == INSPECT_RENDER_CONTRACT {
            ClientExecutionError::Inspect {
                context,
                source: ClientInspectError::Failed(
                    if code == EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE {
                        "inspect.runtime_unavailable".to_owned()
                    } else {
                        stable_inspect_provider_error(&code)
                    },
                ),
            }
        } else {
            ClientExecutionError::ExternalContract {
                context,
                identity: identity.to_owned(),
            }
        }
    })
}

fn inspect_render_contract_error(context: ClientExecutionContext) -> ClientExecutionError {
    ClientExecutionError::Inspect {
        context,
        source: inspect_carrier_error("inspect.malformed_carrier"),
    }
}

fn inspect_render_artifact_is_external(
    revision: &orna_core::revision::FunctionRevisionRecord,
) -> bool {
    fn is_external(expression: &ClientExpressionNode) -> bool {
        matches!(
            expression,
            ClientExpressionNode::ExternalContract { identity }
                if identity == INSPECT_RENDER_CONTRACT
        )
    }

    match revision.artifact().version() {
        EXPRESSION_FORMAT_VERSION => ExpressionClientPlan::decode(revision.artifact().payload())
            .ok()
            .is_some_and(|plan| is_external(plan.expression())),
        CAPABILITY_FORMAT_VERSION => CapabilityClientPlan::decode(revision.artifact().payload())
            .ok()
            .and_then(|plan| match plan.inner_plan() {
                InnerClientPlan::Expression(expression) => {
                    Some(is_external(expression.expression()))
                }
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_inspect_render_contract(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    identity: &str,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<(), ClientExecutionError> {
    if identity != INSPECT_RENDER_CONTRACT || context.pair() != active.pair() {
        return Err(inspect_render_contract_error(context));
    }
    let Some(definition) = active.catalogue().function_by_id(context.function()) else {
        return Err(inspect_render_contract_error(context));
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == context.function() && revision.id() == context.function_revision()
    }) else {
        return Err(inspect_render_contract_error(context));
    };
    if definition.domain() != FunctionDomain::Client
        || definition.current_revision() != context.function_revision()
        || !matches!(
            definition.return_type(),
            FunctionReturn::Single(ResolvedType::Value(type_id)) if *type_id == STD_UI_TYPE_ID
        )
        || definition.parameters().len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || arguments.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || !inspect_render_artifact_is_external(revision)
    {
        return Err(inspect_render_contract_error(context));
    }
    for (index, ((parameter_id, value), (expected_name, expected_type, _))) in arguments
        .iter()
        .zip(INSPECT_RENDER_CARRIER_SIGNATURE)
        .enumerate()
    {
        let parameter = &definition.parameters()[index];
        if parameter.id() != *parameter_id
            || parameter.name() != expected_name
            || parameter.resolved_type() != ResolvedType::Value(expected_type)
            || !runtime_value_matches(active, value, ResolvedType::Value(expected_type))
        {
            return Err(inspect_render_contract_error(context));
        }
    }
    let Some((_, snapshot)) = arguments.first() else {
        return Err(inspect_render_contract_error(context));
    };
    let snapshot = decode_inspect_carrier(active, snapshot, SYS_INSPECT_SNAPSHOT_TYPE_ID)
        .map_err(|_| inspect_render_contract_error(context))?;
    let snapshot_target = inspect_snapshot_target_from_envelope(active, &snapshot)
        .map_err(|_| inspect_render_contract_error(context))?;

    // The render provider is a generic executor boundary, so it cannot rely on
    // the installed server provider's request-side checks. Validate every
    // carrier against the decoded snapshot before allowing the provider to
    // render. ORNA-INSPECT/1 intentionally omits target provenance from the
    // envelope; projection rows retain that fact in memory when populated.
    // Empty projections remain valid, but then there is no carrier-local target
    // evidence to compare (the opaque API exposes no generic target metadata).
    for ((_, value), (_, expected_type, expected_kind)) in
        arguments.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE)
    {
        let carrier = decode_inspect_carrier(active, value, expected_type)
            .map_err(|_| inspect_render_contract_error(context))?;
        inspect_carrier_matches_snapshot(
            active,
            &snapshot,
            snapshot_target,
            expected_kind,
            &carrier,
        )
        .map_err(|_| inspect_render_contract_error(context))?;
    }
    Ok(())
}

fn inspect_render_ui_value_matches(active: &ActiveDatabaseRevision, value: &RuntimeValue) -> bool {
    let RuntimeValue::Opaque(opaque) = value else {
        return false;
    };
    if opaque.opaque_type() != STD_UI_TYPE_ID {
        return false;
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return false;
    };
    let Ok(registry) = registered_opaque_codecs(standard) else {
        return false;
    };
    OpaqueValue::new(
        active,
        &registry,
        STD_UI_TYPE_ID,
        opaque.canonical_payload(),
    )
    .is_ok()
}

fn inspect_carrier_error(code: &'static str) -> ClientInspectError {
    ClientInspectError::Failed(code.to_owned())
}

fn decode_inspect_carrier_payload(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
    expected: TypeId,
) -> Result<InspectCarrierEnvelope, ClientInspectError> {
    let Some(kind) = InspectCarrierKind::from_type_id(expected) else {
        return Err(inspect_carrier_error("inspect.unknown_carrier"));
    };
    let envelope = InspectCarrierEnvelope::decode(payload)
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    if envelope.carrier_kind() != kind {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let pair = active.pair();
    if envelope.source_revision_id() != pair.source()
        || envelope.catalogue_revision_id() != pair.catalogue()
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    Ok(envelope)
}

fn decode_inspect_carrier(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: TypeId,
) -> Result<InspectCarrierEnvelope, ClientInspectError> {
    let RuntimeValue::Opaque(opaque) = value else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if opaque.opaque_type() != expected {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    decode_inspect_carrier_payload(active, opaque.canonical_payload(), expected)
}

/// Decodes one canonical ORV5 row into the opaque byte payload emitted by the
/// installed Inspector provider.
///
/// Projection carrier provenance is carried in this in-memory row prefix, not
/// in the ORNA-INSPECT/1 envelope. Keep this decoder local to the client: the
/// opaque carrier API intentionally exposes no generic row/provenance object.
fn decode_inspect_carrier_row_payload(
    active: &ActiveDatabaseRevision,
    row: &[u8],
) -> Result<Vec<u8>, ClientInspectError> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(inspect_carrier_error("inspect.projection_failed"));
    };
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| inspect_carrier_error("inspect.projection_failed"))?;
    let row = decode_constructed_value(active, &registry, row)
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    let RuntimeValue::Constructed(constructed) = row else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    Ok(payload.clone())
}

/// Returns the target invocation proven by projection rows, if any.
///
/// A projection with no rows is valid (notably the currently accepted
/// resource/UI carriers), so it returns None rather than treating an empty
/// payload as malformed. A non-empty row must carry the common provenance
/// prefix emitted by the installed provider; accepting an unrecognised row
/// would let a custom provider bypass target/revision binding.
fn inspect_projection_target_from_envelope(
    active: &ActiveDatabaseRevision,
    envelope: &InspectCarrierEnvelope,
    expected_kind: InspectCarrierKind,
) -> Result<Option<InvocationId>, ClientInspectError> {
    let mut target = None;
    for row in envelope.rows() {
        let payload = decode_inspect_carrier_row_payload(active, row)?;
        if payload.len() < 91 || payload[0] != expected_kind.tag() {
            return Err(inspect_carrier_error("inspect.malformed_carrier"));
        }
        if u64::from_be_bytes(payload[17..25].try_into().expect("projection epoch width"))
            != envelope.epoch_id()
        {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        if payload[57..73] != active.pair().source().to_bytes()
            || payload[73..89] != active.pair().catalogue().to_bytes()
        {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        if payload[89] != 1 || payload[90] > 4 {
            return Err(inspect_carrier_error("inspect.malformed_carrier"));
        }
        let row_target =
            InvocationId::from_bytes(payload[25..41].try_into().expect("projection target width"));
        if row_target.to_bytes() == [0; 16] {
            return Err(inspect_carrier_error("inspect.invalid_target"));
        }
        if target.is_some_and(|known| known != row_target) {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        target = Some(row_target);
    }
    Ok(target)
}

/// Checks one carrier's accepted provenance against the render snapshot.
fn inspect_carrier_matches_snapshot(
    active: &ActiveDatabaseRevision,
    snapshot: &InspectCarrierEnvelope,
    snapshot_target: InvocationId,
    expected_kind: InspectCarrierKind,
    carrier: &InspectCarrierEnvelope,
) -> Result<(), ClientInspectError> {
    if carrier.carrier_kind() != expected_kind {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    if carrier.source_revision_id() != snapshot.source_revision_id()
        || carrier.catalogue_revision_id() != snapshot.catalogue_revision_id()
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    if carrier.epoch_id() != snapshot.epoch_id() {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    if expected_kind == InspectCarrierKind::Snapshot {
        let target = inspect_snapshot_target_from_envelope(active, carrier)?;
        if target != snapshot_target {
            return Err(inspect_carrier_error("inspect.epoch_mismatch"));
        }
        return Ok(());
    }
    if let Some(target) = inspect_projection_target_from_envelope(active, carrier, expected_kind)?
        && target != snapshot_target
    {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    Ok(())
}

fn inspect_projection_result_type(projection: InspectProjection) -> TypeId {
    match projection {
        InspectProjection::InvocationNodes => SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        InspectProjection::Calls => SYS_INSPECT_CALLS_TYPE_ID,
        InspectProjection::Resources => SYS_INSPECT_RESOURCES_TYPE_ID,
        InspectProjection::StateCells => SYS_INSPECT_STATE_CELLS_TYPE_ID,
        InspectProjection::UiNodes => SYS_INSPECT_UI_NODES_TYPE_ID,
        InspectProjection::PresentationCandidates => SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        InspectProjection::RuntimeBindings => SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        InspectProjection::SecurityDecisions => SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
    }
}

#[cfg(test)]
fn inspect_target_is_observer(context: ClientExecutionContext, target: InvocationId) -> bool {
    inspect_target_is_observer_with_lineage(ObserverLineage::compatibility(context), target)
}

fn inspect_target_is_observer_with_lineage(lineage: ObserverLineage, target: InvocationId) -> bool {
    lineage.contains(target)
}

fn inspect_invocation_target(value: &RuntimeValue) -> Option<InvocationId> {
    let RuntimeValue::Reference { target, object } = value else {
        return None;
    };
    if *target != SYS_INSPECT_INVOCATION_TYPE_ID || object.to_bytes() == [0; 16] {
        return None;
    }
    Some(InvocationId::from_bytes(object.to_bytes()))
}

const INSPECT_SNAPSHOT_ROW_TAG: u8 = 1;

fn decode_inspect_snapshot_target_row(
    row: &[u8],
    epoch_id: u64,
) -> Result<InvocationId, ClientInspectError> {
    if row.len() < 68 || row[0] != INSPECT_SNAPSHOT_ROW_TAG || row[1..9] != [0; 8] {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    if u64::from_be_bytes(row[17..25].try_into().expect("snapshot epoch width")) != epoch_id {
        return Err(inspect_carrier_error("inspect.epoch_mismatch"));
    }
    let target = InvocationId::from_bytes(row[25..41].try_into().expect("snapshot target width"));
    if target.to_bytes() == [0; 16] {
        return Err(inspect_carrier_error("inspect.invalid_target"));
    }
    let mut offset = 57;
    let outcome = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    if !(1..=4).contains(&outcome) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    offset += 1 + 8;
    let result = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    offset += 1;
    match result {
        0 => {}
        1 => {
            let value_count = row
                .get(offset..)
                .and_then(|bytes| bytes.get(..8))
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_be_bytes)
                .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
            if value_count == 0 {
                return Err(inspect_carrier_error("inspect.malformed_carrier"));
            }
            offset += 8;
        }
        _ => return Err(inspect_carrier_error("inspect.malformed_carrier")),
    }
    let duration = *row
        .get(offset)
        .ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    offset += 1;
    match duration {
        0 => {}
        1 => offset += 8,
        _ => return Err(inspect_carrier_error("inspect.malformed_carrier")),
    }
    if offset != row.len() {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    Ok(target)
}

fn inspect_snapshot_target_from_envelope(
    active: &ActiveDatabaseRevision,
    envelope: &InspectCarrierEnvelope,
) -> Result<InvocationId, ClientInspectError> {
    if envelope.carrier_kind() != InspectCarrierKind::Snapshot || envelope.rows().len() != 1 {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(inspect_carrier_error("inspect.projection_failed"));
    };
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| inspect_carrier_error("inspect.projection_failed"))?;
    let row = decode_constructed_value(active, &registry, &envelope.rows()[0])
        .map_err(|_| inspect_carrier_error("inspect.malformed_carrier"))?;
    let RuntimeValue::Constructed(constructed) = row else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    };
    // Encoded root_target bytes are checked against the authenticated
    // AuthenticatedInspectSnapshot on the server. This client decoder only has
    // the opaque envelope and no authenticated FunctionId root context, so the
    // server remains authoritative for that binding.
    decode_inspect_snapshot_target_row(payload, envelope.epoch_id())
}

fn inspect_carrier_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: TypeId,
) -> bool {
    decode_inspect_carrier(active, value, expected).is_ok()
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_inspect_expression(
    active: &ActiveDatabaseRevision,
    operation: &InspectOperationNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    if context.pair() != active.pair() {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::RevisionMismatch {
                expected: active.pair(),
                actual: context.pair(),
            },
        });
    }
    if depth > orna_artifact::client_plan::MAX_EXPRESSION_DEPTH {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::RecursionLimit,
        });
    }
    let mut snapshot_epoch_id = None;
    let mut snapshot_envelope_for_projection = None;
    let target_invocation_id;
    let mut snapshot_options = None;
    let operation = match operation {
        InspectOperationNode::Snapshot { target, options } => {
            if options.is_some() {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: ClientInspectError::Failed(stable_inspect_provider_error(
                        "inspect.invalid_options",
                    )),
                });
            }
            let target = evaluate_expression_with_fuel(
                active,
                target,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth + 1,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let Some(invocation) = inspect_invocation_target(&target) else {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: ClientInspectError::InvalidTarget,
                });
            };
            if inspect_target_is_observer_with_lineage(lineage, invocation) {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: inspect_carrier_error("inspect.recursion"),
                });
            }
            if let Some(options) = options {
                let options = evaluate_expression_with_fuel(
                    active,
                    options,
                    context,
                    &lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth + 1,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                if !runtime_value_matches(
                    active,
                    &options,
                    ResolvedType::Named(SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID),
                ) {
                    return Err(ClientExecutionError::Inspect {
                        context,
                        source: ClientInspectError::InvalidSnapshot,
                    });
                }
                snapshot_options = Some(options);
            }
            target_invocation_id = Some(invocation);
            ClientInspectOperation::Snapshot { target }
        }
        InspectOperationNode::Projection {
            projection,
            snapshot,
        } => {
            let snapshot = evaluate_expression_with_fuel(
                active,
                snapshot,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth + 1,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let snapshot_envelope =
                match decode_inspect_carrier(active, &snapshot, SYS_INSPECT_SNAPSHOT_TYPE_ID) {
                    Ok(envelope) => envelope,
                    Err(source) => {
                        return Err(ClientExecutionError::Inspect { context, source });
                    }
                };
            let invocation = inspect_snapshot_target_from_envelope(active, &snapshot_envelope)
                .map_err(|source| ClientExecutionError::Inspect { context, source })?;
            if inspect_target_is_observer_with_lineage(lineage, invocation) {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: inspect_carrier_error("inspect.recursion"),
                });
            }
            target_invocation_id = Some(invocation);
            snapshot_epoch_id = Some(snapshot_envelope.epoch_id());
            snapshot_envelope_for_projection = Some(snapshot_envelope);
            ClientInspectOperation::Projection {
                projection: *projection,
                snapshot,
            }
        }
    };
    let Some(executor) = executor.as_deref_mut() else {
        return Err(ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
        });
    };
    let request = match (target_invocation_id, snapshot_options) {
        (Some(target), Some(options)) => ClientInspectRequest::with_target_invocation_and_options(
            context,
            operation.clone(),
            target,
            options,
            lineage,
        ),
        (Some(target), None) => ClientInspectRequest::with_target_invocation(
            context,
            operation.clone(),
            target,
            lineage,
        ),
        (None, None) => {
            ClientInspectRequest::with_provenance(context, operation.clone(), None, None, lineage)
        }
        (None, Some(_)) => unreachable!("snapshot options require a target"),
    };
    let value = executor
        .inspect(request)
        .map_err(|code| ClientExecutionError::Inspect {
            context,
            source: ClientInspectError::Failed(stable_inspect_provider_error(&code)),
        })?;
    let expected = match operation {
        ClientInspectOperation::Snapshot { .. } => SYS_INSPECT_SNAPSHOT_TYPE_ID,
        ClientInspectOperation::Projection { projection, .. } => {
            inspect_projection_result_type(projection)
        }
    };
    let envelope = match decode_inspect_carrier(active, &value, expected) {
        Ok(envelope) => envelope,
        Err(source) => {
            return Err(ClientExecutionError::Inspect { context, source });
        }
    };
    if snapshot_epoch_id.is_some_and(|epoch_id| epoch_id != envelope.epoch_id()) {
        return Err(ClientExecutionError::Inspect {
            context,
            source: inspect_carrier_error("inspect.epoch_mismatch"),
        });
    }
    if let Some(expected_target) = target_invocation_id {
        match operation {
            ClientInspectOperation::Snapshot { .. } => {
                let actual_target = inspect_snapshot_target_from_envelope(active, &envelope)
                    .map_err(|source| ClientExecutionError::Inspect { context, source })?;
                if actual_target != expected_target {
                    return Err(ClientExecutionError::Inspect {
                        context,
                        source: inspect_carrier_error("inspect.epoch_mismatch"),
                    });
                }
            }
            ClientInspectOperation::Projection { projection, .. } => {
                let snapshot = snapshot_envelope_for_projection
                    .as_ref()
                    .expect("projection operations retain their decoded snapshot");
                inspect_carrier_matches_snapshot(
                    active,
                    snapshot,
                    expected_target,
                    InspectCarrierKind::from_type_id(inspect_projection_result_type(projection))
                        .expect("sealed projection type must map to a carrier"),
                    &envelope,
                )
                .map_err(|source| ClientExecutionError::Inspect { context, source })?;
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_expression(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut fuel = ClientExecutionFuel::new();
    Ok(evaluate_expression_with_fuel(
        active,
        expression,
        context,
        &lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        &mut fuel,
    )?)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_control_flow_plan_types(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_control_flow_statements_types(active, plan, plan.statements(), context)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_control_flow_statements_types(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statements: &[orna_artifact::client_plan::ControlFlowStatement],
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    for statement in statements {
        match statement {
            orna_artifact::client_plan::ControlFlowStatement::Let { expression, .. }
            | orna_artifact::client_plan::ControlFlowStatement::Assignment { expression, .. } => {
                validate_control_flow_expression_type(active, plan, expression, context)?;
            }
            orna_artifact::client_plan::ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression() {
                    validate_control_flow_expression_type(active, plan, expression, context)?;
                }
            }
            orna_artifact::client_plan::ControlFlowStatement::If(if_statement) => {
                for branch in if_statement.branches() {
                    if validate_control_flow_expression_type(
                        active,
                        plan,
                        branch.condition(),
                        context,
                    )? != Some(StandardScalar::Boolean)
                    {
                        return Err(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        ));
                    }
                    validate_control_flow_statements_types(
                        active,
                        plan,
                        branch.statements(),
                        context,
                    )?;
                }
                if let Some(statements) = if_statement.else_statements() {
                    validate_control_flow_statements_types(active, plan, statements, context)?;
                }
            }
            orna_artifact::client_plan::ControlFlowStatement::While(while_statement) => {
                if validate_control_flow_expression_type(
                    active,
                    plan,
                    while_statement.condition(),
                    context,
                )? != Some(StandardScalar::Boolean)
                {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::TypeMismatch,
                    ));
                }
                validate_control_flow_statements_types(
                    active,
                    plan,
                    while_statement.statements(),
                    context,
                )?;
            }
        }
    }
    Ok(())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_control_flow_expression_type(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
) -> Result<Option<StandardScalar>, ClientExecutionError> {
    let mismatch = || expression_error(context, ClientExpressionError::TypeMismatch);
    match expression {
        ClientExpressionNode::String { .. } => Ok(Some(StandardScalar::CharacterLargeObject)),
        ClientExpressionNode::Integer { value } => {
            i32::try_from(*value).map_err(|_| mismatch())?;
            Ok(Some(StandardScalar::Integer))
        }
        ClientExpressionNode::Boolean { .. } => Ok(Some(StandardScalar::Boolean)),
        ClientExpressionNode::ParameterRead { parameter } => {
            Ok(resolve_client_function(active, context.function())
                .and_then(|resolved| resolved.definition.parameter_by_id(*parameter))
                .and_then(|parameter| {
                    static_control_flow_scalar_for_type(active, parameter.resolved_type())
                }))
        }
        ClientExpressionNode::LocalRead { local } => {
            let Some(declaration) = plan
                .locals()
                .iter()
                .find(|candidate| candidate.local() == *local)
            else {
                return Err(mismatch());
            };
            if declaration.kind() == ClientLocalKind::Value {
                let Some(resolved) = resolve_client_local_type(active, declaration.type_id())
                else {
                    return Err(mismatch());
                };
                Ok(static_control_flow_scalar_for_type(active, resolved))
            } else {
                Ok(None)
            }
        }
        ClientExpressionNode::FieldPath { root, fields } => {
            let Some(mut resolved) = resolve_client_function(active, context.function())
                .and_then(|function| function.definition.parameter_by_id(*root))
                .map(|parameter| parameter.resolved_type())
            else {
                return Ok(None);
            };
            for field in fields {
                let Some(target) = resolved.reference_target() else {
                    return Ok(None);
                };
                let Some(definition) = active.catalogue().object_type_by_id(target).or_else(|| {
                    active
                        .catalogue_hash_context()
                        .standard()
                        .and_then(|standard| standard.catalogue().object_type_by_id(target))
                }) else {
                    return Ok(None);
                };
                let Some(field) = definition.field_by_id(*field) else {
                    return Ok(None);
                };
                resolved = field.resolved_type();
            }
            Ok(static_control_flow_scalar_for_type(active, resolved))
        }
        ClientExpressionNode::Concat { left, right } => {
            let left = validate_control_flow_expression_type(active, plan, left, context)?;
            let right = validate_control_flow_expression_type(active, plan, right, context)?;
            if left != Some(StandardScalar::CharacterLargeObject)
                || right != Some(StandardScalar::CharacterLargeObject)
            {
                return Err(mismatch());
            }
            Ok(Some(StandardScalar::CharacterLargeObject))
        }
        ClientExpressionNode::Unary {
            operator,
            expression,
        } => {
            let operand = validate_control_flow_expression_type(active, plan, expression, context)?;
            let expected = match operator {
                ControlFlowUnaryOperator::Plus | ControlFlowUnaryOperator::Minus => {
                    StandardScalar::Integer
                }
                ControlFlowUnaryOperator::Not => StandardScalar::Boolean,
            };
            if operand != Some(expected) {
                return Err(mismatch());
            }
            Ok(Some(expected))
        }
        ClientExpressionNode::Binary {
            operator,
            left,
            right,
        } => {
            let left = validate_control_flow_expression_type(active, plan, left, context)?;
            let right = validate_control_flow_expression_type(active, plan, right, context)?;
            match operator {
                ControlFlowBinaryOperator::And | ControlFlowBinaryOperator::Or => {
                    if left != Some(StandardScalar::Boolean)
                        || right != Some(StandardScalar::Boolean)
                    {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Boolean))
                }
                ControlFlowBinaryOperator::Add
                | ControlFlowBinaryOperator::Subtract
                | ControlFlowBinaryOperator::Multiply
                | ControlFlowBinaryOperator::Divide
                | ControlFlowBinaryOperator::Modulo => {
                    if left != Some(StandardScalar::Integer)
                        || right != Some(StandardScalar::Integer)
                    {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Integer))
                }
                ControlFlowBinaryOperator::Equal
                | ControlFlowBinaryOperator::NotEqual
                | ControlFlowBinaryOperator::LessThan
                | ControlFlowBinaryOperator::GreaterThan
                | ControlFlowBinaryOperator::LessThanOrEqual
                | ControlFlowBinaryOperator::GreaterThanOrEqual => {
                    let supported = |scalar| {
                        matches!(
                            scalar,
                            Some(
                                StandardScalar::Integer
                                    | StandardScalar::Boolean
                                    | StandardScalar::CharacterLargeObject
                            )
                        )
                    };
                    if !supported(left) || left != right {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Boolean))
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            for (_, argument) in arguments {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(
                resolve_client_function(active, *function).and_then(|resolved| {
                    let FunctionReturn::Single(return_type) = resolved.definition.return_type()
                    else {
                        return None;
                    };
                    static_control_flow_scalar_for_type(active, *return_type)
                }),
            )
        }
        ClientExpressionNode::Await { expression } => {
            validate_control_flow_expression_type(active, plan, expression, context)?;
            let type_id = match expression.as_ref() {
                ClientExpressionNode::Resource { operation } => operation.declared_result_type(),
                ClientExpressionNode::LocalRead { local } => {
                    let Some(declaration) = plan
                        .locals()
                        .iter()
                        .find(|candidate| candidate.local() == *local)
                    else {
                        return Err(mismatch());
                    };
                    if !matches!(declaration.kind(), ClientLocalKind::Resource(_)) {
                        return Err(mismatch());
                    }
                    declaration.type_id()
                }
                _ => return Err(mismatch()),
            };
            Ok(static_control_flow_scalar_for_type_id(active, type_id))
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, argument) in operation.arguments() {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(None)
        }
        ClientExpressionNode::Action { operation } => {
            for (_, argument) in operation.arguments() {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(static_control_flow_scalar_for_type_id(
                active,
                operation.declared_result_type(),
            ))
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(target) = operation.target() {
                validate_control_flow_expression_type(active, plan, target, context)?;
            }
            if let Some(options) = operation.options() {
                validate_control_flow_expression_type(active, plan, options, context)?;
            }
            if let Some(snapshot) = operation.snapshot_expression() {
                validate_control_flow_expression_type(active, plan, snapshot, context)?;
            }
            Ok(None)
        }
        ClientExpressionNode::SourceIntrospection
        | ClientExpressionNode::Input
        | ClientExpressionNode::Evaluate { .. } => Ok(None),
        ClientExpressionNode::ExternalContract { .. } => Ok(None),
    }
}

fn static_control_flow_scalar_for_type_id(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
) -> Option<StandardScalar> {
    resolve_client_local_type(active, type_id)
        .and_then(|resolved| static_control_flow_scalar_for_type(active, resolved))
}

fn static_control_flow_scalar_for_type(
    active: &ActiveDatabaseRevision,
    resolved: ResolvedType,
) -> Option<StandardScalar> {
    match ClientResourceValueKind::from_active(active, resolved) {
        ClientResourceValueKind::Scalar(scalar) => Some(scalar),
        _ => None,
    }
}

#[allow(clippy::result_large_err, clippy::too_many_arguments)]
fn evaluate_expression_with_fuel(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    lineage: &ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, Box<ClientExecutionError>> {
    fuel.consume(context)?;
    match expression {
        ClientExpressionNode::Await { expression } => match expression.as_ref() {
            ClientExpressionNode::Resource { operation } => evaluate_resource_expression(
                active,
                operation,
                context,
                *lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )
            .map_err(Into::into),
            ClientExpressionNode::LocalRead { local } => {
                let Some(ClientLocalBinding::Resource(operation)) = local_environment.get(local)
                else {
                    return Err(Box::new(expression_error(
                        context,
                        ClientExpressionError::ParameterNotBound,
                    )));
                };
                let operation = operation.clone();
                evaluate_resource_expression(
                    active,
                    &operation,
                    context,
                    *lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )
                .map_err(Into::into)
            }
            _ => Err(Box::new(expression_error(
                context,
                ClientExpressionError::InvalidCall,
            ))),
        },
        ClientExpressionNode::Resource { operation } => evaluate_resource_expression(
            active,
            operation,
            context,
            *lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )
        .map_err(Into::into),
        ClientExpressionNode::Action { operation } => evaluate_action_operation(
            active,
            operation,
            context,
            *lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )
        .map_err(Into::into),
        ClientExpressionNode::Inspect { operation } => evaluate_inspect_expression(
            active,
            operation,
            context,
            *lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )
        .map_err(Into::into),
        ClientExpressionNode::String { value } => Ok(RuntimeValue::Text(value.clone())),
        ClientExpressionNode::Integer { value } => i32::try_from(*value)
            .map(RuntimeValue::Integer)
            .map_err(|_| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ))
            }),
        ClientExpressionNode::Boolean { value } => Ok(RuntimeValue::Boolean(*value)),
        ClientExpressionNode::ParameterRead { parameter } => arguments
            .iter()
            .find(|(candidate, _)| candidate == parameter)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::ParameterNotBound,
                ))
            }),
        ClientExpressionNode::LocalRead { local } => match local_environment.get(local) {
            Some(ClientLocalBinding::Value(value) | ClientLocalBinding::StreamValue(value)) => {
                Ok(value.clone())
            }
            Some(ClientLocalBinding::Resource(_)) => Err(Box::new(expression_error(
                context,
                ClientExpressionError::TypeMismatch,
            ))),
            None => Err(Box::new(expression_error(
                context,
                ClientExpressionError::ParameterNotBound,
            ))),
        },
        ClientExpressionNode::FieldPath { root, fields } => {
            let value = arguments
                .iter()
                .find(|(candidate, _)| candidate == root)
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    Box::new(expression_error(
                        context,
                        ClientExpressionError::ParameterNotBound,
                    ))
                })?;
            evaluate_field_path(active, value, fields, context, principal, state)
                .map_err(Into::into)
        }
        ClientExpressionNode::Concat { left, right } => {
            let left = evaluate_expression_with_fuel(
                active,
                left,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let right = evaluate_expression_with_fuel(
                active,
                right,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let (RuntimeValue::Text(left), RuntimeValue::Text(right)) = (left, right) else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                )));
            };
            Ok(RuntimeValue::Text(format!("{left}{right}")))
        }
        ClientExpressionNode::Unary {
            operator,
            expression,
        } => {
            let value = evaluate_expression_with_fuel(
                active,
                expression,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            match (operator, value) {
                (ControlFlowUnaryOperator::Plus, RuntimeValue::Integer(value)) => {
                    Ok(RuntimeValue::Integer(value))
                }
                (ControlFlowUnaryOperator::Minus, RuntimeValue::Integer(value)) => value
                    .checked_neg()
                    .map(RuntimeValue::Integer)
                    .ok_or_else(|| Box::new(arithmetic_error(context))),
                (ControlFlowUnaryOperator::Not, RuntimeValue::Boolean(value)) => {
                    Ok(RuntimeValue::Boolean(!value))
                }
                _ => Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ))),
            }
        }
        ClientExpressionNode::Binary {
            operator,
            left,
            right,
        } => {
            let left = evaluate_expression_with_fuel(
                active,
                left,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            match operator {
                ControlFlowBinaryOperator::And => {
                    let RuntimeValue::Boolean(left) = left else {
                        return Err(Box::new(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        )));
                    };
                    if !left {
                        return Ok(RuntimeValue::Boolean(false));
                    }
                    let right = evaluate_expression_with_fuel(
                        active,
                        right,
                        context,
                        lineage,
                        arguments,
                        declarations,
                        grants,
                        state,
                        depth,
                        principal,
                        executor,
                        local_environment,
                        fuel,
                    )?;
                    return match right {
                        RuntimeValue::Boolean(right) => Ok(RuntimeValue::Boolean(right)),
                        _ => Err(Box::new(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        ))),
                    };
                }
                ControlFlowBinaryOperator::Or => {
                    let RuntimeValue::Boolean(left) = left else {
                        return Err(Box::new(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        )));
                    };
                    if left {
                        return Ok(RuntimeValue::Boolean(true));
                    }
                    let right = evaluate_expression_with_fuel(
                        active,
                        right,
                        context,
                        lineage,
                        arguments,
                        declarations,
                        grants,
                        state,
                        depth,
                        principal,
                        executor,
                        local_environment,
                        fuel,
                    )?;
                    return match right {
                        RuntimeValue::Boolean(right) => Ok(RuntimeValue::Boolean(right)),
                        _ => Err(Box::new(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        ))),
                    };
                }
                _ => {}
            }
            let right = evaluate_expression_with_fuel(
                active,
                right,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            match operator {
                ControlFlowBinaryOperator::Add
                | ControlFlowBinaryOperator::Subtract
                | ControlFlowBinaryOperator::Multiply
                | ControlFlowBinaryOperator::Divide
                | ControlFlowBinaryOperator::Modulo => {
                    let (RuntimeValue::Integer(left), RuntimeValue::Integer(right)) = (left, right)
                    else {
                        return Err(Box::new(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        )));
                    };
                    let left = i64::from(left);
                    let right = i64::from(right);
                    let result = match operator {
                        ControlFlowBinaryOperator::Add => left.checked_add(right),
                        ControlFlowBinaryOperator::Subtract => left.checked_sub(right),
                        ControlFlowBinaryOperator::Multiply => left.checked_mul(right),
                        ControlFlowBinaryOperator::Divide => left.checked_div(right),
                        ControlFlowBinaryOperator::Modulo => left.checked_rem(right),
                        _ => unreachable!(),
                    }
                    .ok_or_else(|| Box::new(arithmetic_error(context)))?;
                    i32::try_from(result)
                        .map(RuntimeValue::Integer)
                        .map_err(|_| Box::new(arithmetic_error(context)))
                }
                ControlFlowBinaryOperator::Equal
                | ControlFlowBinaryOperator::NotEqual
                | ControlFlowBinaryOperator::LessThan
                | ControlFlowBinaryOperator::GreaterThan
                | ControlFlowBinaryOperator::LessThanOrEqual
                | ControlFlowBinaryOperator::GreaterThanOrEqual => {
                    compare_control_flow_values(*operator, &left, &right, context)
                        .map_err(Into::into)
                }
                ControlFlowBinaryOperator::And | ControlFlowBinaryOperator::Or => {
                    unreachable!("short-circuit operators return before right evaluation")
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments: bound,
        } => {
            if depth > orna_artifact::client_plan::MAX_EXPRESSION_DEPTH {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::RecursionLimit,
                )));
            }
            if !client_call_target_is_referenced(active, context, *function) {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                )));
            }
            let mut evaluated = Vec::with_capacity(bound.len());
            for (parameter, expression) in bound {
                if evaluated
                    .iter()
                    .any(|(candidate, _)| candidate == parameter)
                {
                    return Err(Box::new(expression_error(
                        context,
                        ClientExpressionError::InvalidCall,
                    )));
                }
                let value = evaluate_expression_with_fuel(
                    active,
                    expression,
                    context,
                    lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                evaluated.push((*parameter, value));
            }
            let (_, value) = stacker::maybe_grow(
                CLIENT_RECURSION_STACK_RED_ZONE,
                CLIENT_RECURSION_STACK_SEGMENT,
                || {
                    evaluate_function_with_fuel(
                        active,
                        *function,
                        evaluated,
                        declarations,
                        grants,
                        state,
                        depth + 1,
                        principal,
                        (*lineage).nested(),
                        executor,
                        fuel,
                    )
                },
            )?;
            Ok(value)
        }
        ClientExpressionNode::SourceIntrospection => {
            let Some(function) = active.catalogue().function_by_id(context.function()) else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                )));
            };
            let Some(revision) = active
                .function_revisions()
                .iter()
                .find(|candidate| candidate.id() == context.function_revision())
            else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                )));
            };
            let declaration = revision.declaration_origin();
            let parameters = function
                .parameters()
                .iter()
                .map(|parameter| {
                    let type_id = source_metadata_type_id(active, parameter.resolved_type())
                        .ok_or_else(|| {
                            Box::new(expression_error(
                                context,
                                ClientExpressionError::TypeMismatch,
                            ))
                        })?;
                    Ok(orna_core::source_metadata::SourceParameterMetadata::new(
                        parameter.id(),
                        parameter.name(),
                        parameter.ordinal(),
                        type_id,
                    ))
                })
                .collect::<Result<Vec<_>, Box<ClientExecutionError>>>()?;
            let references = active
                .references()
                .iter()
                .filter(|reference| {
                    reference.source_function() == context.function()
                        && reference.source_revision() == context.function_revision()
                })
                .map(|reference| {
                    let target_name = source_reference_target_name(active, reference.target())
                        .unwrap_or_else(|| format!("{:?}", reference.target()));
                    orna_core::source_metadata::SourceReferenceMetadata::new(
                        reference.ordinal(),
                        reference.target(),
                        target_name,
                        reference.source_origin().source_unit(),
                        reference.source_origin().byte_start(),
                        reference.source_origin().byte_end(),
                    )
                })
                .collect();
            let body_kind = source_metadata_body_kind(revision.artifact());
            let return_metadata = source_metadata_return_metadata(active, function.return_type());
            let metadata = orna_core::source_metadata::SourceFunctionMetadata::new_with_signature(
                function.id(),
                revision.id(),
                function.name().to_string(),
                declaration.source_unit(),
                declaration.byte_start(),
                declaration.byte_end(),
                revision.declaration_content_hash(),
                body_kind,
                return_metadata,
                parameters,
                references,
            )
            .map_err(|_| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ))
            })?;
            let payload = metadata.encode_with_signature();
            let value = OpaqueValue::new_source_metadata_carrier(
                active,
                SYS_SOURCE_FUNCTION_TYPE_ID,
                payload,
            )
            .map_err(|_| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ))
            })?;
            Ok(RuntimeValue::Opaque(value))
        }
        ClientExpressionNode::Input => {
            let Some(executor) = executor.as_deref_mut() else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::InputUnavailable,
                )));
            };
            let value = executor.read_input(context).map_err(|_| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::InputUnavailable,
                ))
            })?;
            if matches!(&value, RuntimeValue::Text(_)) {
                Ok(value)
            } else {
                Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                )))
            }
        }
        ClientExpressionNode::Evaluate { expression } => {
            let command = evaluate_expression_with_fuel(
                active,
                expression,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let RuntimeValue::Text(command) = command else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                )));
            };
            if command.len() > MAX_CLIENT_COMMAND_BYTES {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::DynamicInvocation,
                )));
            }
            let Some(executor) = executor.as_deref_mut() else {
                return Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::DynamicInvocation,
                )));
            };
            let value = executor.evaluate_command(context, &command).map_err(|_| {
                Box::new(expression_error(
                    context,
                    ClientExpressionError::DynamicInvocation,
                ))
            })?;
            if matches!(
                &value,
                RuntimeValue::Opaque(value) if value.opaque_type() == STD_UI_TYPE_ID
            ) {
                Ok(value)
            } else {
                Err(Box::new(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                )))
            }
        }
        ClientExpressionNode::ExternalContract { identity } => {
            if let Some(spec) = standard_ui_constructor_spec(active, context, identity) {
                return evaluate_standard_ui_constructor(active, context, spec, arguments);
            }
            if identity == INSPECT_RENDER_CONTRACT {
                validate_inspect_render_contract(active, context, identity, arguments)?;
                let value =
                    evaluate_external_contract(identity, context, *lineage, arguments, executor)?;
                if !inspect_render_ui_value_matches(active, &value) {
                    return Err(Box::new(ClientExecutionError::Inspect {
                        context,
                        source: ClientInspectError::TypeMismatch,
                    }));
                }
                Ok(value)
            } else {
                evaluate_external_contract(identity, context, *lineage, arguments, executor)
                    .map_err(Into::into)
            }
        }
    }
}

fn arithmetic_error(context: ClientExecutionContext) -> ClientExecutionError {
    expression_error(context, ClientExpressionError::Arithmetic)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn compare_control_flow_values(
    operator: ControlFlowBinaryOperator,
    left: &RuntimeValue,
    right: &RuntimeValue,
    context: ClientExecutionContext,
) -> Result<RuntimeValue, ClientExecutionError> {
    let ordering = match (left, right) {
        (RuntimeValue::Integer(left), RuntimeValue::Integer(right)) => left.cmp(right),
        (RuntimeValue::Boolean(left), RuntimeValue::Boolean(right)) => left.cmp(right),
        (RuntimeValue::Text(left), RuntimeValue::Text(right)) => left.cmp(right),
        _ => {
            return Err(expression_error(
                context,
                ClientExpressionError::TypeMismatch,
            ));
        }
    };
    let value = match operator {
        ControlFlowBinaryOperator::Equal => ordering == std::cmp::Ordering::Equal,
        ControlFlowBinaryOperator::NotEqual => ordering != std::cmp::Ordering::Equal,
        ControlFlowBinaryOperator::LessThan => ordering == std::cmp::Ordering::Less,
        ControlFlowBinaryOperator::GreaterThan => ordering == std::cmp::Ordering::Greater,
        ControlFlowBinaryOperator::LessThanOrEqual => ordering != std::cmp::Ordering::Greater,
        ControlFlowBinaryOperator::GreaterThanOrEqual => ordering != std::cmp::Ordering::Less,
        ControlFlowBinaryOperator::Add
        | ControlFlowBinaryOperator::Subtract
        | ControlFlowBinaryOperator::Multiply
        | ControlFlowBinaryOperator::Divide
        | ControlFlowBinaryOperator::Modulo
        | ControlFlowBinaryOperator::And
        | ControlFlowBinaryOperator::Or => {
            unreachable!("comparison helper received non-comparison")
        }
    };
    Ok(RuntimeValue::Boolean(value))
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn evaluate_field_path(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    fields: &[orna_core::FieldId],
    context: ClientExecutionContext,
    principal: PrincipalId,
    state: &ClientStateStore,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut current = value.clone();
    for field_id in fields {
        if let RuntimeValue::Reference { target, object } = &current {
            let target = *target;
            let object = *object;
            if let Some(loader) = state.installed_reference_loader.as_ref() {
                let Some(loaded) =
                    loader.load(active, principal, state.security_context_digest(), &current)
                else {
                    return Err(expression_error(context, ClientExpressionError::FieldPath));
                };
                if !client_reference_object_is_active(active, target, object, loaded) {
                    return Err(expression_error(context, ClientExpressionError::FieldPath));
                }
                current = loaded
                    .fields()
                    .iter()
                    .find(|(candidate, _)| candidate == field_id)
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| expression_error(context, ClientExpressionError::FieldPath))?;
                continue;
            } else {
                let Some(loader) = state.reference_loader.as_ref() else {
                    return Err(expression_error(context, ClientExpressionError::FieldPath));
                };
                current = loader
                    .load(active, principal, state.security_context_digest(), &current)
                    .ok_or_else(|| expression_error(context, ClientExpressionError::FieldPath))?;
            }
        }
        let RuntimeValue::Record(record) = &current else {
            return Err(expression_error(context, ClientExpressionError::FieldPath));
        };
        let definition = active
            .catalogue()
            .record_value_type_by_id(record.record_type())
            .and_then(|definition| definition.field_by_id(*field_id))
            .or_else(|| {
                active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|standard| {
                        standard
                            .catalogue()
                            .record_value_type_by_id(record.record_type())
                            .and_then(|definition| definition.field_by_id(*field_id))
                    })
            })
            .ok_or_else(|| expression_error(context, ClientExpressionError::FieldPath))?;
        let index = usize::try_from(definition.ordinal())
            .map_err(|_| expression_error(context, ClientExpressionError::FieldPath))?;
        current = record
            .fields()
            .get(index)
            .ok_or_else(|| expression_error(context, ClientExpressionError::FieldPath))?
            .clone();
    }
    Ok(current)
}

fn expression_returns_stream(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    local_environment: &ClientLocalEnvironment,
) -> bool {
    match expression {
        ClientExpressionNode::Await { expression } => {
            procedural_resource_kind_for_runtime(expression, local_environment)
                == Some(ResourceKind::Stream)
        }
        ClientExpressionNode::LocalRead { local } => matches!(
            local_environment.get(local),
            Some(ClientLocalBinding::StreamValue(_))
        ),
        ClientExpressionNode::Call { function, .. } => resolve_client_function(active, *function)
            .is_some_and(|resolved| {
                matches!(resolved.definition.return_type(), FunctionReturn::Stream(_))
            }),
        _ => false,
    }
}

fn runtime_expression_value_matches(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    value: &RuntimeValue,
    expected: ResolvedType,
    local_environment: &ClientLocalEnvironment,
) -> bool {
    if expression_returns_stream(active, expression, local_environment) {
        runtime_stream_value_matches(active, value, expected)
    } else {
        runtime_value_matches(active, value, expected)
    }
}

fn runtime_stream_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected_item: ResolvedType,
) -> bool {
    let Some(item_descriptor) = supported_stream_item_descriptor(active, expected_item) else {
        return false;
    };
    let Ok(list_descriptor) = TypeDescriptor::list(item_descriptor) else {
        return false;
    };
    let Ok(option_descriptor) = TypeDescriptor::option(list_descriptor) else {
        return false;
    };
    let RuntimeValue::Constructed(constructed) = value else {
        return false;
    };
    constructed.descriptor() == &option_descriptor
}

fn is_sealed_inspect_type(type_id: TypeId) -> bool {
    matches!(
        type_id,
        SYS_INSPECT_INVOCATION_TYPE_ID
            | SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            | SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            | SYS_INSPECT_CALLS_TYPE_ID
            | SYS_INSPECT_RESOURCES_TYPE_ID
            | SYS_INSPECT_UI_NODES_TYPE_ID
            | SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            | SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            | SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
    )
}

fn is_inspect_carrier_type(type_id: TypeId) -> bool {
    matches!(
        type_id,
        SYS_INSPECT_SNAPSHOT_TYPE_ID
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

fn runtime_scalar_matches(scalar: StandardScalar, value: &RuntimeValue) -> bool {
    matches!(
        (scalar, value),
        (StandardScalar::Boolean, RuntimeValue::Boolean(_))
            | (StandardScalar::Integer, RuntimeValue::Integer(_))
            | (StandardScalar::BigInt, RuntimeValue::BigInt(_))
            | (StandardScalar::Float, RuntimeValue::Float(_))
            | (StandardScalar::CharacterLargeObject, RuntimeValue::Text(_))
            | (StandardScalar::BinaryLargeObject, RuntimeValue::Bytes(_))
    )
}

fn runtime_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: ResolvedType,
) -> bool {
    if let RuntimeValue::Null(null) = value {
        return null.resolved_type() == expected && active_type_is_known(active, expected);
    }
    let scalar_matches = |scalar| {
        matches!(
            (scalar, value),
            (StandardScalar::Boolean, RuntimeValue::Boolean(_))
                | (StandardScalar::Integer, RuntimeValue::Integer(_))
                | (StandardScalar::BigInt, RuntimeValue::BigInt(_))
                | (StandardScalar::Float, RuntimeValue::Float(_))
                | (StandardScalar::CharacterLargeObject, RuntimeValue::Text(_))
                | (StandardScalar::BinaryLargeObject, RuntimeValue::Bytes(_))
        )
    };
    match expected {
        ResolvedType::Scalar(scalar) => scalar_matches(scalar),
        ResolvedType::Value(type_id) => {
            if is_inspect_carrier_type(type_id) {
                return inspect_carrier_value_matches(active, value, type_id);
            }
            if type_id == SYS_INSPECT_INVOCATION_TYPE_ID {
                return false;
            }
            if type_id == STD_UI_TYPE_ID {
                return matches!(value, RuntimeValue::Opaque(opaque) if opaque.opaque_type() == type_id);
            }
            let Some(definition) = active
                .catalogue_hash_context()
                .standard()
                .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
            else {
                return false;
            };
            if definition.kind() == ValueTypeKind::Opaque {
                return matches!(value, RuntimeValue::Opaque(opaque) if opaque.opaque_type() == type_id);
            }
            match definition.representation_contract() {
                "orna.kernel.value.boolean@1" => scalar_matches(StandardScalar::Boolean),
                "orna.kernel.value.integer@1" => scalar_matches(StandardScalar::Integer),
                "orna.kernel.value.bigint@1" => scalar_matches(StandardScalar::BigInt),
                "orna.kernel.value.float@1" => scalar_matches(StandardScalar::Float),
                "orna.kernel.value.character-large-object@1" => {
                    scalar_matches(StandardScalar::CharacterLargeObject)
                }
                "orna.kernel.value.binary-large-object@1" => {
                    scalar_matches(StandardScalar::BinaryLargeObject)
                }
                _ => false,
            }
        }
        ResolvedType::Named(type_id) => {
            if type_id == SYS_SOURCE_FUNCTION_TYPE_ID {
                return matches!(
                    value,
                    RuntimeValue::Opaque(opaque) if opaque.opaque_type() == type_id
                );
            }
            if type_id == SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID {
                return false;
            }
            if is_inspect_carrier_type(type_id) {
                return inspect_carrier_value_matches(active, value, type_id);
            }
            match value {
                RuntimeValue::Record(record) => {
                    record.record_type() == type_id && active_has_record_type(active, type_id)
                }
                RuntimeValue::Enum(enum_value) => {
                    enum_value.enum_type() == type_id
                        && active_enum_label_is_valid(active, type_id, enum_value.label())
                }
                _ => false,
            }
        }
        ResolvedType::Reference { target } => {
            if target == SYS_INSPECT_INVOCATION_TYPE_ID {
                return inspect_invocation_target(value).is_some();
            }
            matches!(value, RuntimeValue::Reference { target: actual, .. } if *actual == target)
                && active_has_object_type(active, target)
        }
    }
}
fn active_type_is_known(active: &ActiveDatabaseRevision, resolved: ResolvedType) -> bool {
    match resolved {
        ResolvedType::Scalar(_) => true,
        ResolvedType::Value(type_id) => {
            is_sealed_inspect_type(type_id) || active_has_value_type(active, type_id)
        }
        ResolvedType::Named(type_id) => {
            is_inspect_carrier_type(type_id)
                || active_has_record_type(active, type_id)
                || active_has_enum_type(active, type_id)
        }
        ResolvedType::Reference { target } => active_has_object_type(active, target),
    }
}

fn active_type_matches(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
    predicate: impl for<'catalogue> Fn(TypeDefinition<'catalogue>) -> bool,
) -> bool {
    let application = active.catalogue().type_definition_by_id(type_id);
    let standard = active
        .catalogue_hash_context()
        .standard()
        .and_then(|snapshot| snapshot.catalogue().type_definition_by_id(type_id));
    match (application, standard) {
        (Some(_), Some(_)) => false,
        (Some(definition), None) | (None, Some(definition)) => predicate(definition),
        (None, None) => false,
    }
}

fn active_supports_invocation_target(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
) -> bool {
    resolve_resource_target(active, target).is_some()
}

fn active_has_value_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active
        .catalogue_hash_context()
        .standard()
        .is_some_and(|standard| {
            standard
                .catalogue()
                .type_definition_by_id(type_id)
                .is_some_and(|definition| definition.as_value().is_some())
        })
}

fn active_has_record_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active_type_matches(active, type_id, |definition| {
        definition.as_record_value().is_some()
    })
}

fn active_has_enum_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active_type_matches(active, type_id, |definition| definition.as_enum().is_some())
}

fn active_enum_label_is_valid(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
    label: &str,
) -> bool {
    let application = active.catalogue().enum_type_by_id(type_id);
    let standard = active
        .catalogue_hash_context()
        .standard()
        .and_then(|snapshot| snapshot.catalogue().enum_type_by_id(type_id));
    match (application, standard) {
        (Some(_), Some(_)) => false,
        (Some(definition), None) | (None, Some(definition)) => {
            definition.labels().iter().any(|declared| declared == label)
        }
        (None, None) => false,
    }
}

fn active_has_object_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active_type_matches(active, type_id, |definition| {
        definition.as_object().is_some()
    })
}

fn expression_error(
    context: ClientExecutionContext,
    source: ClientExpressionError,
) -> ClientExecutionError {
    ClientExecutionError::ExpressionEvaluation { context, source }
}

fn state_error(context: ClientExecutionContext, source: ClientStateError) -> ClientExecutionError {
    ClientExecutionError::StateEvaluation { context, source }
}

/// Initialises the LOCAL, SESSION, and loaded USER slots of one version-four
/// plan in the caller-owned in-memory store.
///
/// A slot that already has an entry in the store keeps its value (caller
/// state input wins over the plan default). `Unset` defaults leave no entry;
/// `Null` and checked expression defaults are evaluated and type-checked
/// against the declared slot type.
// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn initialize_client_state(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<(), ClientExecutionError> {
    // Evaluate and type-check every missing default before committing any
    // staged value to the caller-owned LOCAL, SESSION, or USER maps.
    let mut staged = Vec::with_capacity(plan.slots().len());
    for slot in plan.slots() {
        let key = state.key_for(context.function(), slot.state_slot_id());
        let resolved = resolve_state_slot_type(active, slot.type_id()).ok_or_else(|| {
            state_error(
                context,
                ClientStateError::UnsupportedSlotType {
                    slot: slot.state_slot_id(),
                },
            )
        })?;
        let stored_value = match slot.scope() {
            StateScope::Local => state.local.get(&key),
            StateScope::Session => state.session.get(&key),
            StateScope::User => state.user.get(&key).map(|value| &value.value),
        };
        let stored_user_type_mismatch = matches!(slot.scope(), StateScope::User)
            && state
                .user
                .get(&key)
                .is_some_and(|value| value.value_type() != slot.type_id());
        if stored_user_type_mismatch
            || stored_value.is_some_and(|value| !runtime_value_matches(active, value, resolved))
        {
            return Err(state_error(
                context,
                ClientStateError::StoredTypeMismatch {
                    slot: slot.state_slot_id(),
                },
            ));
        }
        if stored_value.is_some() {
            continue;
        }
        let value = match slot.default() {
            StateDefault::Unset => continue,
            StateDefault::Null => RuntimeValue::null(resolved).map_err(|_| {
                state_error(
                    context,
                    ClientStateError::NullDefault {
                        slot: slot.state_slot_id(),
                    },
                )
            })?,
            StateDefault::Expression(node) => {
                let value = evaluate_expression_with_fuel(
                    active,
                    node,
                    context,
                    &lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                if !runtime_value_matches(active, &value, resolved) {
                    return Err(state_error(
                        context,
                        ClientStateError::DefaultTypeMismatch {
                            slot: slot.state_slot_id(),
                        },
                    ));
                }
                value
            }
        };
        staged.push((slot.scope(), key, value, slot.type_id()));
    }

    for (scope, key, value, type_id) in staged {
        match scope {
            StateScope::Local => {
                if let Entry::Vacant(entry) = state.local.entry(key) {
                    entry.insert(value);
                }
            }
            StateScope::Session => {
                if let Entry::Vacant(entry) = state.session.entry(key) {
                    entry.insert(value);
                }
            }
            StateScope::User => {
                if let Entry::Vacant(entry) = state.user.entry(key) {
                    entry.insert(ClientUserState::defaulted(value, type_id));
                }
            }
        }
    }
    Ok(())
}

/// Resolves one procedural CLIENT local identity to the runtime type used by
/// expression evaluation. State slots use a narrower value-type contract.
fn resolve_client_local_type(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
) -> Option<ResolvedType> {
    if let Some(resolved) = resolve_state_slot_type(active, type_id) {
        return Some(resolved);
    }
    if type_id == SYS_INSPECT_INVOCATION_TYPE_ID {
        return Some(ResolvedType::reference(type_id));
    }
    if is_inspect_carrier_type(type_id) {
        return Some(ResolvedType::value(type_id));
    }
    let scalar = if type_id == orna_standard::BIGINT_TYPE_ID {
        Some(StandardScalar::BigInt)
    } else if type_id == orna_standard::FLOAT_TYPE_ID {
        Some(StandardScalar::Float)
    } else if type_id == orna_standard::BINARY_LARGE_OBJECT_TYPE_ID {
        Some(StandardScalar::BinaryLargeObject)
    } else {
        None
    };
    if let Some(scalar) = scalar {
        return Some(ResolvedType::scalar(scalar));
    }
    if active_has_value_type(active, type_id) {
        return Some(ResolvedType::value(type_id));
    }
    if active_has_enum_type(active, type_id) || active_has_record_type(active, type_id) {
        return Some(ResolvedType::named(type_id));
    }
    if active_has_object_type(active, type_id) {
        return Some(ResolvedType::reference(type_id));
    }
    None
}

/// Resolves one checked state slot type to the runtime type used to check
/// defaults and construct null values.
fn resolve_state_slot_type(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
) -> Option<ResolvedType> {
    let definition = active
        .catalogue_hash_context()
        .standard()?
        .catalogue()
        .value_type_by_id(type_id)?;
    if state_slot_type_is_supported(definition) {
        Some(ResolvedType::value(type_id))
    } else {
        None
    }
}

fn state_slot_type_is_supported(definition: &ValueTypeDefinition) -> bool {
    definition.kind() != ValueTypeKind::Opaque
        && matches!(
            definition.representation_contract(),
            "orna.kernel.value.boolean@1"
                | "orna.kernel.value.integer@1"
                | "orna.kernel.value.bigint@1"
                | "orna.kernel.value.float@1"
                | "orna.kernel.value.character-large-object@1"
                | "orna.kernel.value.binary-large-object@1"
        )
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_active_catalogue(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<(), ClientExecutionError> {
    let canonical = catalogue_digest_with_context(
        active.catalogue_hash_context(),
        active.catalogue(),
        active.function_revisions(),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .map_err(|source| invalid_active_revision(active.pair(), function, source))?;
    if canonical != active.catalogue_hash() {
        return Err(ClientExecutionError::InvalidActiveRevision {
            pair: active.pair(),
            function,
            source: ClientActiveRevisionError::CatalogueHashMismatch,
        });
    }
    Ok(())
}

fn invalid_active_revision(
    pair: RevisionPair,
    function: FunctionId,
    source: CanonicalHashError,
) -> ClientExecutionError {
    ClientExecutionError::InvalidActiveRevision {
        pair,
        function,
        source: ClientActiveRevisionError::Canonical(source),
    }
}

type ClientLocalEnvironment = HashMap<LocalId, ClientLocalBinding>;

#[derive(Clone, Debug)]
enum ClientLocalBinding {
    Value(RuntimeValue),
    StreamValue(RuntimeValue),
    Resource(ResourceOperationNode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientReturnShape {
    LegacyBoolean,
    StandardBoolean(TypeId),
    Opaque(TypeId),
    Expression(ResolvedType),
    StreamExpression(ResolvedType),
    State(ResolvedType),
    StreamState(ResolvedType),
    Resource(ResolvedType),
    StreamResource(ResolvedType),
    Procedural(ResolvedType),
    StreamProcedural(ResolvedType),
    ControlFlow(ResolvedType),
    StreamControlFlow(ResolvedType),
    Action(TypeId),
    Inspect(ResolvedType),
    Source(ResolvedType),
    OtherValue,
    Unsupported,
}

fn classify_client_return(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
    artifact_version: u32,
) -> ClientReturnShape {
    let expression_eligible = matches!(
        artifact_version,
        EXPRESSION_FORMAT_VERSION
            | STATE_FORMAT_VERSION
            | RESOURCE_FORMAT_VERSION
            | PROCEDURAL_FORMAT_VERSION
            | orna_artifact::client_plan::ACTION_FORMAT_VERSION
            | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            | orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
    );
    let stream_expression_eligible = artifact_version == EXPRESSION_FORMAT_VERSION;
    let expression_shape = |resolved_type: ResolvedType| {
        if artifact_version == STATE_FORMAT_VERSION {
            ClientReturnShape::State(resolved_type)
        } else if artifact_version == RESOURCE_FORMAT_VERSION {
            ClientReturnShape::Resource(resolved_type)
        } else if artifact_version == PROCEDURAL_FORMAT_VERSION {
            ClientReturnShape::Procedural(resolved_type)
        } else if artifact_version == orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION {
            ClientReturnShape::ControlFlow(resolved_type)
        } else if artifact_version == orna_artifact::client_plan::INSPECT_FORMAT_VERSION {
            ClientReturnShape::Inspect(resolved_type)
        } else {
            ClientReturnShape::Expression(resolved_type)
        }
    };
    let resolved_type = match return_type {
        FunctionReturn::Single(resolved_type) => *resolved_type,
        FunctionReturn::Stream(resolved_type) if stream_expression_eligible => {
            return ClientReturnShape::StreamExpression(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == STATE_FORMAT_VERSION => {
            return ClientReturnShape::StreamState(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == RESOURCE_FORMAT_VERSION => {
            return ClientReturnShape::StreamResource(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == PROCEDURAL_FORMAT_VERSION => {
            return ClientReturnShape::StreamProcedural(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type)
            if artifact_version == orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION =>
        {
            return ClientReturnShape::StreamControlFlow(*resolved_type);
        }
        FunctionReturn::Rows(_) | FunctionReturn::Stream(_) => {
            return ClientReturnShape::Unsupported;
        }
    };
    if let Some(scalar) = resolved_type.legacy_scalar() {
        return if scalar == StandardScalar::Boolean {
            if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::LegacyBoolean
            }
        } else if expression_eligible
            && matches!(
                scalar,
                StandardScalar::Integer | StandardScalar::CharacterLargeObject
            )
        {
            expression_shape(resolved_type)
        } else {
            ClientReturnShape::Unsupported
        };
    }
    if resolved_type.reference_target().is_some() {
        return ClientReturnShape::Unsupported;
    }
    if resolved_type.named_type() == Some(SYS_SOURCE_FUNCTION_TYPE_ID) {
        return if matches!(
            artifact_version,
            EXPRESSION_FORMAT_VERSION | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
        ) {
            ClientReturnShape::Source(resolved_type)
        } else {
            ClientReturnShape::Unsupported
        };
    }
    if let Some(type_id) = resolved_type.value_type() {
        if artifact_version == orna_artifact::client_plan::ACTION_FORMAT_VERSION
            && type_id == STD_ACTION_TYPE_ID
        {
            return ClientReturnShape::Action(type_id);
        }
        if artifact_version == orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            && is_sealed_inspect_type(type_id)
        {
            return expression_shape(resolved_type);
        }
        let Some(definition) = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
        else {
            return ClientReturnShape::Unsupported;
        };
        if definition.representation_contract() == "orna.kernel.value.boolean@1" {
            return if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::StandardBoolean(type_id)
            };
        }
        if definition.kind() == ValueTypeKind::Opaque {
            return if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::Opaque(type_id)
            };
        }
        if expression_eligible
            && matches!(
                definition.representation_contract(),
                "orna.kernel.value.integer@1" | "orna.kernel.value.character-large-object@1"
            )
        {
            return expression_shape(resolved_type);
        }
        return ClientReturnShape::OtherValue;
    }
    ClientReturnShape::Unsupported
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_function_shape(
    active: &ActiveDatabaseRevision,
    definition: &orna_core::catalogue::FunctionDefinition,
    context: ClientExecutionContext,
    artifact_version: u32,
) -> Result<ClientReturnShape, ClientExecutionError> {
    if definition.domain() != FunctionDomain::Client {
        return Err(invalid_function(
            context,
            ClientExecutionRule::FunctionDomain,
        ));
    }
    if !matches!(
        artifact_version,
        EXPRESSION_FORMAT_VERSION
            | STATE_FORMAT_VERSION
            | RESOURCE_FORMAT_VERSION
            | PROCEDURAL_FORMAT_VERSION
            | orna_artifact::client_plan::ACTION_FORMAT_VERSION
            | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            | orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
    ) && !definition.parameters().is_empty()
    {
        return Err(invalid_function(context, ClientExecutionRule::Parameters));
    }
    let return_shape = classify_client_return(active, definition.return_type(), artifact_version);
    if matches!(return_shape, ClientReturnShape::Unsupported) {
        return Err(invalid_function(context, ClientExecutionRule::ReturnType));
    }
    if definition.security() != FunctionSecurity::Invoker {
        return Err(invalid_function(context, ClientExecutionRule::Security));
    }
    if definition.volatility() != FunctionVolatility::Immutable {
        return Err(invalid_function(context, ClientExecutionRule::Volatility));
    }
    Ok(return_shape)
}

fn is_expression_reference_allowed(
    function: Option<&orna_core::catalogue::FunctionDefinition>,
    reference: &orna_core::revision::DefinitionReference,
) -> bool {
    match reference.kind() {
        DefinitionReferenceKind::FunctionCall
        | DefinitionReferenceKind::NamedType
        | DefinitionReferenceKind::ParameterRead
        | DefinitionReferenceKind::QueryField
        | DefinitionReferenceKind::Expression => true,
        DefinitionReferenceKind::ObjectReference => {
            let DefinitionReferenceTarget::ObjectType(target) = reference.target() else {
                return false;
            };
            function.is_some_and(|definition| {
                definition
                    .parameters()
                    .iter()
                    .any(|parameter| parameter.resolved_type().reference_target() == Some(target))
            })
        }
        _ => false,
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_selected_references(
    active: &ActiveDatabaseRevision,
    references: &[orna_core::revision::DefinitionReference],
    function: &FunctionDefinition,
    semantic_hash_version: FunctionSemanticHashVersion,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
) -> Result<(), ClientExecutionError> {
    let selected = references
        .iter()
        .filter(|reference| {
            reference.source_function() == context.function()
                && reference.source_revision() == context.function_revision()
        })
        .collect::<Vec<_>>();

    match active.catalogue_hash_context() {
        orna_core::revision::CatalogueHashContext::Version1 => {
            if return_shape != ClientReturnShape::LegacyBoolean
                || semantic_hash_version != FunctionSemanticHashVersion::Version1
                || !selected.is_empty()
            {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
        }
        orna_core::revision::CatalogueHashContext::Version2 { standard } => {
            if semantic_hash_version != FunctionSemanticHashVersion::Version2 {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
            if matches!(
                return_shape,
                ClientReturnShape::Expression(_)
                    | ClientReturnShape::StreamExpression(_)
                    | ClientReturnShape::State(_)
                    | ClientReturnShape::StreamState(_)
                    | ClientReturnShape::Resource(_)
                    | ClientReturnShape::StreamResource(_)
                    | ClientReturnShape::Procedural(_)
                    | ClientReturnShape::StreamProcedural(_)
                    | ClientReturnShape::ControlFlow(_)
                    | ClientReturnShape::StreamControlFlow(_)
                    | ClientReturnShape::Action(_)
                    | ClientReturnShape::Inspect(_)
                    | ClientReturnShape::Source(_)
            ) {
                if selected
                    .iter()
                    .any(|reference| !is_expression_reference_allowed(Some(function), reference))
                {
                    return Err(invalid_function(context, ClientExecutionRule::References));
                }
                return Ok(());
            }
            let Some(reference) = selected.first() else {
                return Err(invalid_function(context, ClientExecutionRule::References));
            };
            let valid = selected.len() == 1
                && reference.ordinal() == 0
                && reference.kind() == DefinitionReferenceKind::NamedType
                && match reference.target() {
                    DefinitionReferenceTarget::ValueType(type_id) => {
                        let definition = standard.catalogue().value_type_by_id(type_id);
                        match return_shape {
                            ClientReturnShape::LegacyBoolean => definition.is_some_and(|value| {
                                value.representation_contract() == "orna.kernel.value.boolean@1"
                            }),
                            ClientReturnShape::StandardBoolean(return_type) => {
                                return_type == type_id
                                    && definition.is_some_and(|value| {
                                        value.representation_contract()
                                            == "orna.kernel.value.boolean@1"
                                    })
                            }
                            ClientReturnShape::Opaque(return_type) => {
                                return_type == type_id
                                    && definition
                                        .is_some_and(|value| value.kind() == ValueTypeKind::Opaque)
                            }
                            ClientReturnShape::Action(return_type) => {
                                return_type == type_id
                                    && type_id == STD_ACTION_TYPE_ID
                                    && definition
                                        .is_some_and(|value| value.kind() == ValueTypeKind::Opaque)
                            }
                            ClientReturnShape::Source(_) => type_id == SYS_SOURCE_FUNCTION_TYPE_ID,
                            _ => false,
                        }
                    }
                    _ => false,
                };
            if !valid {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
        }
        _ => return Err(invalid_function(context, ClientExecutionRule::References)),
    }
    Ok(())
}

/// Checks that a decoded expression call targets one of the durable
/// `FunctionCall` references recorded for its owning revision.
///
/// The artifact payload is integrity checked, but its function IDs are still
/// untrusted input at this boundary. The compiler emits one resolved
/// `FunctionCall` reference for every call node; requiring the target to be in
/// that set prevents a validly encoded artifact from invoking an unrelated
/// function that was not part of the checked call graph.
fn client_call_target_is_referenced(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    target: FunctionId,
) -> bool {
    let Some(owner) = resolve_client_function(active, context.function()) else {
        return false;
    };
    if owner.revision.id() != context.function_revision() {
        return false;
    }
    owner.references.iter().any(|reference| {
        reference.source_function() == context.function()
            && reference.source_revision() == context.function_revision()
            && reference.kind() == DefinitionReferenceKind::FunctionCall
            && reference.target() == DefinitionReferenceTarget::Function(target)
    })
}

/// Preflights every CLIENT call in one decoded version-3 expression plan.
///
/// The compiler records call references in postorder, so nested calls precede
/// their enclosing call. Matching that sequence against the owning revision's
/// durable references closes the gap left by a target-set-only check: target
/// substitutions, reordered/duplicated/missing calls, and malformed argument
/// bindings are all rejected before any expression is evaluated.
// ClientExecutionError or action errors retain their accepted diagnostic context and variants.
#[allow(clippy::result_large_err)]
fn preflight_client_expression_calls(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;

    preflight_client_call_targets(active, context, decoded_targets)
}
// ClientExecutionError or action errors retain their accepted diagnostic context and variants.
#[allow(clippy::result_large_err)]
fn preflight_client_call_targets(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    decoded_targets: Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    let Some(owner) = resolve_client_function(active, context.function()) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    if owner.revision.id() != context.function_revision() {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let mut durable_references = owner
        .references
        .iter()
        .filter(|reference| {
            reference.source_function() == context.function()
                && reference.source_revision() == context.function_revision()
                && reference.kind() == DefinitionReferenceKind::FunctionCall
        })
        .collect::<Vec<_>>();
    durable_references.sort_unstable_by_key(|reference| reference.ordinal());

    if durable_references.len() != decoded_targets.len()
        || durable_references
            .iter()
            .zip(decoded_targets)
            .any(|(reference, target)| {
                reference.target() != DefinitionReferenceTarget::Function(target)
            })
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn preflight_client_state_calls(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for slot in plan.slots() {
        if let StateDefault::Expression(expression) = slot.default() {
            collect_client_expression_call_targets(
                active,
                expression,
                context,
                &mut decoded_targets,
            )?;
        }
    }
    collect_client_expression_call_targets(
        active,
        plan.expression(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn preflight_client_procedural_calls(
    active: &ActiveDatabaseRevision,
    plan: &ProceduralClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for statement in plan.statements() {
        collect_client_expression_call_targets(
            active,
            statement.expression(),
            context,
            &mut decoded_targets,
        )?;
    }
    collect_client_expression_call_targets(
        active,
        plan.return_expression(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn preflight_client_control_flow_calls(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    collect_control_flow_block_call_targets(
        active,
        plan.statements(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn collect_control_flow_block_call_targets(
    active: &ActiveDatabaseRevision,
    statements: &[ControlFlowStatement],
    context: ClientExecutionContext,
    decoded_targets: &mut Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    for statement in statements {
        match statement {
            ControlFlowStatement::Let { expression, .. }
            | ControlFlowStatement::Assignment { expression, .. } => {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression() {
                    collect_client_expression_call_targets(
                        active,
                        expression,
                        context,
                        decoded_targets,
                    )?;
                }
            }
            ControlFlowStatement::If(if_statement) => {
                for branch in if_statement.branches() {
                    collect_client_expression_call_targets(
                        active,
                        branch.condition(),
                        context,
                        decoded_targets,
                    )?;
                    collect_control_flow_block_call_targets(
                        active,
                        branch.statements(),
                        context,
                        decoded_targets,
                    )?;
                }
                if let Some(statements) = if_statement.else_statements() {
                    collect_control_flow_block_call_targets(
                        active,
                        statements,
                        context,
                        decoded_targets,
                    )?;
                }
            }
            ControlFlowStatement::While(while_statement) => {
                collect_client_expression_call_targets(
                    active,
                    while_statement.condition(),
                    context,
                    decoded_targets,
                )?;
                collect_control_flow_block_call_targets(
                    active,
                    while_statement.statements(),
                    context,
                    decoded_targets,
                )?;
            }
        }
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn preflight_client_action_calls(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_client_action_operation(active, operation, context)?;
    let mut decoded_targets = Vec::new();
    for (_, expression) in operation.arguments() {
        collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;
    }
    decoded_targets.push(operation.target_function());
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn preflight_client_inner_plan_calls(
    active: &ActiveDatabaseRevision,
    plan: &InnerClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    match plan {
        InnerClientPlan::Boolean(_) | InnerClientPlan::Opaque(_) => Ok(()),
        InnerClientPlan::Expression(inner) => {
            preflight_client_expression_calls(active, inner.expression(), context)
        }
        InnerClientPlan::State(inner) => preflight_client_state_calls(active, inner, context),
        InnerClientPlan::Resource(inner) => {
            preflight_client_expression_calls(active, inner.expression(), context)
        }
        InnerClientPlan::Procedural(inner) => {
            preflight_client_procedural_calls(active, inner, context)
        }
        InnerClientPlan::ControlFlow(inner) => {
            preflight_client_control_flow_calls(active, inner, context)
        }
        InnerClientPlan::Action(inner) => {
            preflight_client_action_calls(active, inner.operation(), context)
        }
    }
}

fn operation_arguments_match_definition(
    definition: &FunctionDefinition,
    arguments: &[(ParameterId, ClientExpressionNode)],
) -> bool {
    if arguments.len() != definition.parameters().len() {
        return false;
    }
    let mut expected = definition
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    arguments
        .iter()
        .map(|(parameter, _)| *parameter)
        .eq(expected)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn validate_client_resource_operation(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    operation: &ResourceOperationNode,
) -> Result<(), ClientExecutionError> {
    let Some(resolved) = resolve_resource_operation_target(active, operation) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    if resolved.definition.domain() != FunctionDomain::Server
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let expected = match (operation.kind(), resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => *result,
        (ResourceKind::Stream, FunctionReturn::Stream(result)) => *result,
        _ => {
            return Err(expression_error(
                context,
                ClientExpressionError::InvalidCall,
            ));
        }
    };
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn validate_client_action_operation(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let raw_target =
        InvocationTarget::new(operation.target_function(), operation.target_revision());
    let Some(resolved) = resolve_unclassified_target(active, raw_target) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    let expected_domain = match operation.domain() {
        ActionTargetDomain::Client => FunctionDomain::Client,
        ActionTargetDomain::Server => FunctionDomain::Server,
    };
    if resolved.definition.domain() != expected_domain
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let FunctionReturn::Single(expected) = resolved.definition.return_type() else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    let expected = *expected;
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn collect_client_expression_call_targets(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    decoded_targets: &mut Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    match expression {
        ClientExpressionNode::Await { expression } => {
            collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
        }
        ClientExpressionNode::Resource { operation } => {
            validate_client_resource_operation(active, context, operation)?;
            for (_, expression) in operation.arguments() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Action { operation } => {
            validate_client_action_operation(active, operation, context)?;
            for (_, expression) in operation.arguments() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(expression) = operation.target() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            if let Some(expression) = operation.options() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            if let Some(expression) = operation.snapshot_expression() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            let Some(resolved) = resolve_client_function(active, *function) else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ));
            };
            let definition = resolved.definition;
            if arguments.len() != definition.parameters().len()
                || definition.parameters().iter().any(|parameter| {
                    arguments
                        .iter()
                        .filter(|(candidate, _)| *candidate == parameter.id())
                        .count()
                        != 1
                })
                || arguments.iter().any(|(parameter, _)| {
                    definition
                        .parameters()
                        .iter()
                        .all(|candidate| candidate.id() != *parameter)
                })
            {
                return Err(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ));
            }
            for (_, expression) in arguments {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(*function);
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            collect_client_expression_call_targets(active, left, context, decoded_targets)?;
            collect_client_expression_call_targets(active, right, context, decoded_targets)?;
        }
        ClientExpressionNode::Unary { expression, .. } => {
            collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
        }
        ClientExpressionNode::Input | ClientExpressionNode::Evaluate { .. } => {}
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
    Ok(())
}

/// Validates the saved artefact contract against the effective plan version.
///
/// For a version-5 capability envelope the effective version is the inner
/// plan version (the envelope decode already fixed the outer version); for
/// versions 1-4 it is the artefact's own version.
// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn validate_artifact(
    artifact: &orna_core::revision::ExecutableArtifact,
    language_version: &str,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
    artifact_version: u32,
) -> Result<(), ClientExecutionError> {
    if artifact.format() != FORMAT_IDENTITY {
        return Err(invalid_function(
            context,
            ClientExecutionRule::ArtifactFormat,
        ));
    }
    let expected_version = match return_shape {
        ClientReturnShape::LegacyBoolean | ClientReturnShape::StandardBoolean(_) => FORMAT_VERSION,
        ClientReturnShape::Opaque(_) => OPAQUE_FORMAT_VERSION,
        ClientReturnShape::Expression(_) | ClientReturnShape::StreamExpression(_) => {
            EXPRESSION_FORMAT_VERSION
        }
        ClientReturnShape::Procedural(_) | ClientReturnShape::StreamProcedural(_) => {
            PROCEDURAL_FORMAT_VERSION
        }
        ClientReturnShape::ControlFlow(_) | ClientReturnShape::StreamControlFlow(_) => {
            orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
        }
        ClientReturnShape::State(_) | ClientReturnShape::StreamState(_) => STATE_FORMAT_VERSION,
        ClientReturnShape::Resource(_) | ClientReturnShape::StreamResource(_) => {
            RESOURCE_FORMAT_VERSION
        }
        ClientReturnShape::Action(_) => orna_artifact::client_plan::ACTION_FORMAT_VERSION,
        ClientReturnShape::Inspect(_) => orna_artifact::client_plan::INSPECT_FORMAT_VERSION,
        ClientReturnShape::Source(_) => EXPRESSION_FORMAT_VERSION,
        ClientReturnShape::OtherValue => unreachable!("definition references were validated"),
        ClientReturnShape::Unsupported => unreachable!("function shape was validated"),
    };
    if artifact_version != expected_version {
        return Err(invalid_function(
            context,
            ClientExecutionRule::ArtifactVersion,
        ));
    }
    if language_version != LANGUAGE_VERSION_IDENTITY {
        return Err(invalid_function(
            context,
            ClientExecutionRule::LanguageVersion,
        ));
    }
    Ok(())
}

/// Validates a CLIENT artifact's execution domain and canonical payload digest.
///
/// This check runs before plan decoding or evaluation. It proves payload
/// integrity only; provenance, signatures, sandbox policy, and host
/// capabilities remain separate contract surfaces.
pub fn validate_client_artifact_integrity(
    artifact: &orna_core::revision::ExecutableArtifact,
) -> Result<(), ClientArtifactIntegrityError> {
    if artifact.kind() != ExecutableArtifactKind::Client {
        return Err(ClientArtifactIntegrityError::WrongExecutionDomain);
    }
    let digest = artifact_payload_digest(artifact.payload())
        .map_err(|_| ClientArtifactIntegrityError::PayloadDigest)?;
    if digest != artifact.content_hash() {
        return Err(ClientArtifactIntegrityError::PayloadDigest);
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
fn validate_artifact_identity(
    artifact: &orna_core::revision::ExecutableArtifact,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_client_artifact_integrity(artifact).map_err(|_| invalid_artifact(context))
}

fn invalid_artifact(context: ClientExecutionContext) -> ClientExecutionError {
    ClientExecutionError::InvalidArtifact {
        context,
        source: ClientPlanError::InvalidMagic,
    }
}

fn invalid_function(
    context: ClientExecutionContext,
    rule: ClientExecutionRule,
) -> ClientExecutionError {
    ClientExecutionError::InvalidFunction { context, rule }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod runtime_abi;

#[cfg(test)]
mod runtime_conformance;
