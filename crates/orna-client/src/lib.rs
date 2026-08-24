//! Local evaluation for closed CLIENT functions.

use orna_protocol::{
    ClientFrame, decode_active_value, decode_constructed_value, encode_active_client_frame,
    encode_active_value, MAX_RESOURCE_BATCH_ITEMS, MAX_RESOURCE_TOTAL_ITEMS,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    error::Error,
    fmt,
    hash::{Hash, Hasher},
};

use orna_artifact::client_plan::{
    CAPABILITY_FORMAT_VERSION, CapabilityArgumentSource, CapabilityClientPlan,
    ClientExpressionNode, ClientLocal, ClientLocalKind, ClientPlan, ClientPlanError, EXPRESSION_FORMAT_VERSION,
    InspectOperationNode, InspectProjection,
    ExpressionClientPlan, FORMAT_IDENTITY, FORMAT_VERSION, InnerClientPlan,
    LANGUAGE_VERSION_IDENTITY, OPAQUE_FORMAT_VERSION, OpaqueClientPlan, RESOURCE_FORMAT_VERSION, PROCEDURAL_FORMAT_VERSION,
    ActionClientPlan, ActionTargetDomain, ProceduralClientPlan, ResourceClientPlan, ResourceKind,
    ResourceOperationNode, STATE_FORMAT_VERSION, StateClientPlan,
    StateDefault, StateScope,
};
use orna_core::{
    CallSiteId, FunctionId, FunctionRevisionId, InvocationId, LocalId, ParameterId, PrincipalId, StateSlotId,
    TypeId,
    system::{
        SYS_INSPECT_INVOCATION_NODES_TYPE_ID, SYS_INSPECT_CALLS_TYPE_ID,
        SYS_INSPECT_RESOURCES_TYPE_ID, SYS_INSPECT_STATE_CELLS_TYPE_ID,
        SYS_INSPECT_UI_NODES_TYPE_ID, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID, SYS_INSPECT_INVOCATION_TYPE_ID,
    },
    canonical_hash::{CanonicalHashError, artifact_payload_digest, catalogue_digest_with_context},
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionVolatility,
        TypeDefinition, ValueTypeDefinition, ValueTypeKind,
    },
    inspect::{
        INSPECT_RENDER_CARRIER_SIGNATURE, INSPECT_RENDER_CONTRACT, stable_inspect_error_code,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifactKind, FunctionSemanticHashVersion, RevisionPair, Sha256Digest,
        VerifiedStandardLibrarySnapshot,
    },
    inspect_carrier::{InspectCarrierEnvelope, InspectCarrierKind},
    security::{AuthorisedInvocation, InvocationTarget, TargetClass},
    state::{
        UserStateCell, UserStateChange, UserStateKeyWithoutPrincipal, UserStateWriteOutcome,
        UserStateWriteResult,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    value::{ConstructedValueKind, FunctionArgument, OpaqueValue, OpaqueValueError, RuntimeValue},
};
/// The largest number of queued stream items retained by one CLIENT resource.
/// This matches the largest single completion batch, keeping one broker-sized
/// batch available while requiring consumption before another batch is retained.
const MAX_RESOURCE_QUEUED_ITEMS: u64 = MAX_RESOURCE_BATCH_ITEMS as u64;

use orna_standard::{
    ACTION_MAGIC, BINARY_LARGE_OBJECT_TYPE_ID, RegisteredOpaqueCodecsError, STD_ACTION_TYPE_ID,
    STD_UI_TYPE_ID, registered_opaque_codecs,
};

pub mod capability;
pub mod inspect_lifecycle;
pub mod inspect_session;

pub use inspect_lifecycle::{
    ClientInspectLifecycle, ClientInspectLifecycleState, InspectEpochBinding, InspectFreezeToken,
    InspectLifecycleError, InspectProjectionVersions,
};
pub use inspect_session::{
    ClientInspectLifecycleCompletion, ClientInspectLifecycleRequest,
    ClientInspectLifecycleSession,
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
        let mut ancestors = [
            InvocationId::from_bytes([0; 16]);
            orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1
        ];
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
    pub fn invocation_id(&self) -> Option<InvocationId> { self.invocation_id }
    fn resource_mut(&mut self) -> Option<&mut ClientResource> { self.resource.as_mut() }
    fn set_resource(&mut self, resource: ClientResource) {
        if resource.generation().value() > self.tombstone.value() { self.tombstone = resource.generation(); }
        self.resource = Some(resource);
    }
    fn stage_request(&mut self, request: ClientResourceRequest) {
        self.request = Some(request);
    }
    fn stage_invocation(&mut self, invocation_id: InvocationId) { self.invocation_id = Some(invocation_id); }
    fn clear(&mut self) {
        if let Some(resource) = self.resource.take() {
            if resource.generation().value() > self.tombstone.value() {
                self.tombstone = resource.generation();
            }
        }
        self.request = None;
        self.invocation_id = None;
    }
    fn is_stale(&self, generation: ClientResourceGeneration) -> bool { generation.value() <= self.tombstone.value() }
}
fn redacted_action_failure() -> ClientActionOutcome {
    ClientActionOutcome::Failed { code: ACTION_FAILURE_CODE.to_owned() }
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
    /// The invocation context contains a NUL byte in its state profile or
    /// function-instance key.
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
            Self::InvalidInvocationContext => {
                formatter.write_str(
                    "CLIENT resource invocation context must be valid NUL-free text",
                )
            }
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
            context.state_profile.as_bytes().contains(&0)
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
    pub fn new(context: ClientExecutionContext, operation: ClientInspectOperation) -> Self {
        let target_invocation_id = operation.target().and_then(inspect_invocation_target);
        Self::with_provenance(context, operation, target_invocation_id, None, ObserverLineage::compatibility(context))
    }

    /// Creates a request with a target identity recovered from canonical snapshot evidence.
    fn with_target_invocation(
        context: ClientExecutionContext,
        operation: ClientInspectOperation,
        target_invocation_id: InvocationId,
        lineage: ObserverLineage,
    ) -> Self {
        Self::with_provenance(context, operation, Some(target_invocation_id), None, lineage)
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
    RevisionMismatch { expected: RevisionPair, actual: RevisionPair },
}

impl fmt::Display for ClientInspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(code) => write!(formatter, "CLIENT Inspector failed: {code}"),
            Self::InvalidTarget => formatter.write_str("CLIENT Inspector target is invalid"),
            Self::InvalidSnapshot => formatter.write_str("CLIENT Inspector snapshot is invalid"),
            Self::TypeMismatch => formatter.write_str("CLIENT Inspector provider returned the wrong type"),
            Self::RecursionLimit => formatter.write_str("CLIENT Inspector recursion limit was exceeded"),
            Self::RevisionMismatch { .. } => formatter.write_str("CLIENT Inspector request revision does not match the active revision"),
        }
    }
}

impl Error for ClientInspectError {}

/// A runtime adapter that evaluates one resource request.
pub trait ClientResourceExecutor {
    /// Executes one request and returns its completion.
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion;
    /// Reports one completion without blocking `execute`.
    ///
    /// Transport-backed executors use this for stream value batches and
    /// terminal completions; immediate executors keep the default.
    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        None
    }
    /// Cancels one live request and returns its terminal completion.
    ///
    /// The default completes the local lifecycle. A transport-backed executor
    /// can override this method to send its protocol cancellation control.
    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
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

/// A deterministic immediate executor for host glue and focused tests.
pub struct DeterministicClientResourceExecutor<F> {
    evaluate: F,
    inspect: Option<Box<dyn FnMut(&ClientInspectRequest) -> Result<RuntimeValue, String>>>,
    external_contract:
        Option<Box<dyn FnMut(&ClientExternalContractRequest) -> Result<RuntimeValue, String>>>,
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


/// Request metadata retained by the runtime solely for executor cancellation.
///
/// The request payload can contain sensitive argument values, so the wrapper
/// deliberately redacts it from the parent resource's derived `Debug` output.
#[derive(Clone, PartialEq)]
struct ActiveClientResourceRequest(ClientResourceRequest);

impl fmt::Debug for ActiveClientResourceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ActiveClientResourceRequest")
            .field(&self.0.request_id())
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
    /// executor to cancel its owned request. The executor still owns the
    /// submitted request; this copy is only the cancellation descriptor.
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
        self.active_request = Some(ActiveClientResourceRequest(request.clone()));
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
        match completion {
            ClientResourceCompletion::Ready {
                request_id,
                key,
                generation,
                value,
            } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
                if self.kind != ResourceKind::Scalar {
                    return Err(ClientResourceError::TypeMismatch);
                }
                self.publish_ready(active, generation, value)
            }
            ClientResourceCompletion::StreamValues {
                request_id,
                key,
                generation,
                values,
            } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
                self.append_stream_values(active, generation, values)
            }
            ClientResourceCompletion::StreamCompleted { request_id, key, generation } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
                self.complete_stream(active, generation)
            }
            ClientResourceCompletion::Cancelled { request_id, key, generation } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
                self.cancel(generation)
            }
            ClientResourceCompletion::Pending { request_id, key, generation } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
                self.require_loading(generation)
            }
            ClientResourceCompletion::Failed {
                request_id,
                key,
                generation,
                code,
            } => {
                self.require_generation(generation)?;
                self.require_request_id(request_id)?;
                self.require_key(key)?;
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
        if self.kind != ResourceKind::Scalar {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.require_loading(generation)?;
        if active.pair() != self.key.target().revision() {
            return Err(ClientResourceError::RevisionMismatch {
                expected: self.key.target().revision(),
                actual: active.pair(),
            });
        }
        if !active_supports_invocation_target(active, self.key.target()) {
            return Err(ClientResourceError::TargetMismatch {
                expected: self.key.target(),
            });
        }
        if !active_resource_result_type_matches(active, self.key.target(), self.kind, self.expected_type) {
            return Err(ClientResourceError::TypeMismatch);
        }
        if !runtime_value_matches(active, &value, self.expected_type) {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.status = ClientResourceStatus::Ready;
        self.active_request = None;
        self.value = Some(value);
        self.failure = None;
        Ok(())
    }

    fn append_stream_values(
        &mut self,
        active: &ActiveDatabaseRevision,
        generation: ClientResourceGeneration,
        values: Vec<RuntimeValue>,
    ) -> Result<(), ClientResourceError> {
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
        self.validate_stream_item_type(active)?;
        if values
            .iter()
            .any(|value| !runtime_value_matches(active, value, self.expected_type))
        {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.stream_batches.push_back(values);
        self.stream_queued_items = queued_items;
        self.stream_total_items = total_items;
        Ok(())
    }

    fn complete_stream(
        &mut self,
        active: &ActiveDatabaseRevision,
        generation: ClientResourceGeneration,
    ) -> Result<(), ClientResourceError> {
        if self.kind != ResourceKind::Stream {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.require_loading(generation)?;
        self.validate_stream_item_type(active)?;
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
        if active.pair() != self.key.target().revision() {
            return Err(ClientResourceError::RevisionMismatch {
                expected: self.key.target().revision(),
                actual: active.pair(),
            });
        }
        if !active_supports_invocation_target(active, self.key.target())
            || !active_resource_result_type_matches(active, self.key.target(), self.kind, self.expected_type)
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
        let Some(item_descriptor) = supported_stream_item_descriptor(active, self.expected_type) else {
            return Err(ClientResourceError::TypeMismatch);
        };
        let list_descriptor = TypeDescriptor::list(item_descriptor)
            .map_err(|_| ClientResourceError::TypeMismatch)?;
        let option_descriptor = TypeDescriptor::option(list_descriptor.clone())
            .map_err(|_| ClientResourceError::TypeMismatch)?;
        if let Some(values) = self.stream_batches.front() {
            let queued_items = self
                .stream_queued_items
                .checked_sub(values.len() as u64)
                .ok_or(ClientResourceError::TypeMismatch)?;
            let values = self.stream_batches.pop_front().expect("stream batch was checked");
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
    /// The returned executor completion is intentionally discarded: the
    /// invalidation itself is the local linearisation point, so every
    /// completion for the old generation must be rejected as stale.
    pub fn invalidate_with_executor(
        &mut self,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<(), ClientResourceError> {
        if let Some(request) = self.active_request() {
            let _ = executor.cancel(request);
        }
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
            .map(|request| request.0.clone())
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

    fn require_generation(&self, generation: ClientResourceGeneration) -> Result<(), ClientResourceError> {
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

fn supported_stream_item_descriptor(
    active: &ActiveDatabaseRevision,
    expected: ResolvedType,
) -> Option<TypeDescriptor> {
    let descriptor = stream_item_descriptor(expected)?;
    match expected {
        ResolvedType::Scalar(_) => Some(descriptor),
        ResolvedType::Named(type_id) => {
            (active_has_enum_type(active, type_id) || active_has_record_type(active, type_id))
                .then_some(descriptor)
        }
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

fn verified_standard_executable_revision(
    standard: &VerifiedStandardLibrarySnapshot,
    function: FunctionId,
) -> Option<FunctionRevisionId> {
    let mut revisions = standard
        .executables()
        .iter()
        .filter(|executable| executable.function() == function)
        .map(|executable| executable.revision().id());
    let revision = revisions.next()?;
    revisions.next().is_none().then_some(revision)
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
/// explicit at this cache boundary.
fn security_context_digest(authorisation: &AuthorisedInvocation) -> Sha256Digest {
    let mut roles = authorisation.active_roles().to_vec();
    roles.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-security-context/v1\0");
    hasher.update(authorisation.session_principal().to_bytes());
    hasher.update(authorisation.effective_principal().to_bytes());
    hasher.update(authorisation.authorising_principal().to_bytes());
    hasher.update((roles.len() as u64).to_be_bytes());
    for role in roles {
        hasher.update(role.to_bytes());
    }
    Sha256Digest::from_bytes(hasher.finalize().into())
}

/// Combines the active catalogue, host data epoch, and security context into
/// one local-only invalidation identity. None of these bytes are transport
/// fields; only the resulting key selects a local resource cache entry.
fn resource_invalidation_identity(
    catalogue_hash: Sha256Digest,
    data_invalidation_token: Sha256Digest,
    security_digest: Sha256Digest,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-invalidation/v1\0");
    hasher.update(catalogue_hash.to_bytes());
    hasher.update(data_invalidation_token.to_bytes());
    hasher.update(security_digest.to_bytes());
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
#[derive(Clone, Debug, PartialEq)]
pub struct ClientStateStore {
    context: ClientStateContext,
    security_context_digest: Sha256Digest,
    local: HashMap<ClientStateKey, RuntimeValue>,
    session: HashMap<ClientStateKey, RuntimeValue>,
    user: HashMap<ClientStateKey, ClientUserState>,
    resources: HashMap<ClientResourceKey, ClientResource>,
}

impl Default for ClientStateStore {
    fn default() -> Self {
        Self {
            context: ClientStateContext::default_for(FunctionId::from_bytes([0; 16])),
            security_context_digest: DEFAULT_SECURITY_CONTEXT_DIGEST,
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
            Entry::Vacant(entry) => entry.insert(ClientResource::new_with_kind(key, kind, expected_type)),
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
    /// The executor owns the submitted request. The resource retains only a
    /// cancellation descriptor, and any completion returned by the hook is
    /// deliberately discarded because the invalidation makes that generation
    /// stale. This keeps cancellation in the owning runtime rather than in
    /// stale-completion handling.
    pub fn invalidate_resource_with_executor(
        &mut self,
        key: ClientResourceKey,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<bool, ClientResourceError> {
        let Some(resource) = self.resources.get_mut(&key) else {
            return Ok(false);
        };
        resource.invalidate_with_executor(executor)?;
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
            self.invalidate_resource_with_executor(replacement, executor)?;
        }
        Ok(self.get_or_create_resource_with_kind(key, kind, expected_type))
    }

    /// Returns or creates a scalar resource through the executor-aware key
    /// replacement path.
    pub fn get_or_create_resource_with_executor(
        &mut self,
        key: ClientResourceKey,
        expected_type: ResolvedType,
        executor: &mut dyn ClientResourceExecutor,
    ) -> Result<&mut ClientResource, ClientResourceError> {
        self.get_or_create_resource_with_kind_and_executor(
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
    /// A USER state batch contained the same logical cell more than once.
    DuplicateKey(ClientStateKey),
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
            Self::DuplicateKey(key) => {
                write!(formatter, "USER state batch contains duplicate key {key:?}")
            }
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
    /// Loads one complete authenticated USER state batch.
    pub fn load_user_state(&mut self, cells: &[UserStateCell]) -> Result<(), ClientUserStateError> {
        let mut loaded = HashMap::with_capacity(cells.len());
        for cell in cells {
            let key = ClientStateKey::from_user_cell(cell);
            if loaded
                .insert(key.clone(), ClientUserState::loaded(cell))
                .is_some()
            {
                return Err(ClientUserStateError::DuplicateKey(key));
            }
        }
        self.user.extend(loaded);
        Ok(())
    }

    /// Updates one USER value and marks it for the next explicit flush.
    pub fn set_user_state(
        &mut self,
        key: ClientStateKey,
        value: RuntimeValue,
        value_type: TypeId,
    ) -> Result<(), ClientUserStateError> {
        if let Some(existing) = self.user.get(&key)
            && existing.value_type != value_type
        {
            return Err(ClientUserStateError::ValueMismatch(key));
        }
        let revision = self.user.get(&key).and_then(ClientUserState::revision);
        self.user
            .insert(key, ClientUserState::local(value, value_type, revision));
        Ok(())
    }

    /// Returns dirty USER values as one deterministic change batch.
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
        pending.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(pending.into_iter().map(|(_, change)| change).collect())
    }

    /// Applies one aligned server write-result batch.
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
        for (key, revision) in staged_writes {
            let local = self
                .user
                .get_mut(&key)
                .expect("USER state key was validated above");
            local.revision = Some(revision);
            local.dirty = false;
        }
        if let Some(error) = first_conflict {
            return Err(error);
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
}

impl fmt::Display for ClientExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ParameterNotBound => "a CLIENT expression parameter was not bound",
            Self::TypeMismatch => "a CLIENT expression value has the wrong type",
            Self::InvalidCall => "a CLIENT expression call has invalid arguments",
            Self::FieldPath => "a CLIENT expression field path could not be resolved",
            Self::RecursionLimit => "the CLIENT expression call-depth limit was exceeded",
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
            Self::ExecutorUnavailable | Self::Pending { .. } | Self::Failed(_) | Self::Cancelled => None,
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
pub fn evaluate_client_function(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_arguments(active, authorisation, &[])
}

/// Evaluates one closed CLIENT function with invocation arguments.
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
pub fn evaluate_client_function_with_state(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state: &mut ClientStateStore,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_state_and_arguments(active, authorisation, &[], state)
}

/// Evaluates one closed CLIENT function with invocation arguments and an
/// explicit in-memory state store.
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
pub fn evaluate_client_function_with_executor(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientExecutionResult, ClientExecutionError> {
    evaluate_client_function_with_arguments_and_executor(active, authorisation, &[], executor)
}

/// Evaluates one CLIENT function with invocation arguments and a caller-owned
/// resource executor.
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
                    // Persist the pending resource and any matching generation
                    // that was cancelled while replacing its local identity.
                    let changed_resources: Vec<_> = staged
                        .resources
                        .iter()
                        .filter_map(|(candidate_key, resource)| {
                            let replacement_cancelled = state.resources.get(candidate_key).is_some_and(
                                |previous| {
                                    previous.status() == ClientResourceStatus::Loading
                                        && resource.status() == ClientResourceStatus::Idle
                                        && resource.generation().value() > previous.generation().value()
                                },
                            );
                            let pending_resource = resource.key() == *key
                                && resource.generation() == *generation
                                && resource.status() == ClientResourceStatus::Loading;
                            (pending_resource || replacement_cancelled)
                                .then_some((*candidate_key, resource.clone()))
                        })
                        .collect();
                    for (candidate_key, resource) in changed_resources {
                        state.resources.insert(candidate_key, resource);
                    }
                }
                ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Failed(_)
                        | ClientResourceExecutionError::Cancelled,
                    ..
                } => {
                    // Preserve terminal resource state when the invocation
                    // fails. The caller can inspect the redacted failure or
                    // cancellation and decide whether to retry or invalidate.
                    for (key, resource) in &staged.resources {
                        let replacement_cancelled = state.resources.get(key).is_some_and(
                            |previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            },
                        );
                        if matches!(
                            resource.status(),
                            ClientResourceStatus::Failed | ClientResourceStatus::Cancelled
                        ) || replacement_cancelled {
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
                    // malformed, retain the changed Loading resource so the
                    // executor-owned request is not stranded in the staged clone.
                    for (key, resource) in &staged.resources {
                        let replacement_cancelled = state.resources.get(key).is_some_and(
                            |previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            },
                        );
                        let changed_identity = state.resources.get(key).is_none_or(|previous| {
                            previous.status() != resource.status()
                                || previous.generation() != resource.generation()
                                || previous.request_id() != resource.request_id()
                        });
                        let terminal_changed = matches!(
                            resource.status(),
                            ClientResourceStatus::Failed | ClientResourceStatus::Cancelled
                        ) && changed_identity;
                        let loading_owned = resource.status() == ClientResourceStatus::Loading
                            && changed_identity;
                        if terminal_changed || loading_owned || replacement_cancelled {
                            state.resources.insert(*key, resource.clone());
                        }
                    }
                }
                _ => {}
            }
            return Err(error);
        }
    };
    *state = staged;
    let (context, value) = result;
    Ok(ClientExecutionResult { context, value })
}


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
    let pair = active.pair();
    let definition = active
        .catalogue()
        .function_by_id(function)
        .ok_or(ClientExecutionError::FunctionNotFound { pair, function })?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|candidate| {
            candidate.function() == function && candidate.id() == definition.current_revision()
        })
        .ok_or(ClientExecutionError::FunctionNotFound { pair, function })?;
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
                    return Err(ClientExecutionError::CapabilityDenied {
                        context,
                        capability: requirement.name().to_owned(),
                    });
                }
            }
        }
        None => {
            for declaration in &bound_declarations {
                if !grants.satisfies_declaration(declaration, resolve_parameter) {
                    return Err(ClientExecutionError::CapabilityDenied {
                        context,
                        capability: declaration.name().as_str().to_owned(),
                    });
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
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    validate_selected_references(
        active,
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
) -> Result<RuntimeValue, ClientExecutionError> {
    match return_shape {
        ClientReturnShape::LegacyBoolean | ClientReturnShape::StandardBoolean(_) => {
            let plan = ClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            Ok(RuntimeValue::Boolean(plan.returned_boolean()))
        }
        ClientReturnShape::Opaque(expected) => {
            let plan = OpaqueClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            evaluate_opaque_plan(active, &plan, context, expected)
        }
        ClientReturnShape::Expression(expected) | ClientReturnShape::StreamExpression(expected) => {
            let plan = ExpressionClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
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
                )
            } else {
                evaluate_expression_plan(
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
                )
            }
        }
        ClientReturnShape::Inspect(expected) => {
            let plan = ExpressionClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            evaluate_expression_plan(
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
            )
        }
        ClientReturnShape::StreamState(expected) => {
            let plan = StateClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_state_calls(active, &plan, context)?;
            evaluate_stream_state_plan(
                active, &plan, context, lineage, expected, arguments, declarations, grants, state, depth,
                principal, executor, local_environment,
            )
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
            )
        }
        ClientReturnShape::StreamProcedural(expected) => {
            let plan = ProceduralClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_procedural_calls(active, &plan, context)?;
            evaluate_procedural_plan(
                active, &plan, context, lineage, expected, true, arguments, declarations, grants, state,
                depth, principal, executor, local_environment,
            )
        }
        ClientReturnShape::Procedural(expected) => {
            let plan = ProceduralClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_procedural_calls(active, &plan, context)?;
            evaluate_procedural_plan(
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
            )
        }
        ClientReturnShape::Action(_expected) => {
            let plan = ActionClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_action_calls(active, plan.operation(), context)?;
            evaluate_action_operation(active, plan.operation(), context, lineage, arguments, declarations, grants, state, depth, principal, executor, local_environment)
        }
        ClientReturnShape::StreamResource(expected) => {
            let plan = ResourceClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            preflight_client_expression_calls(active, plan.expression(), context)?;
            evaluate_stream_resource_plan(
                active, &plan, context, lineage, expected, arguments, declarations, grants, state, depth,
                principal, executor, local_environment,
            )
        }
        ClientReturnShape::Resource(expected) => {
            let plan = ResourceClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
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
            )
        }
        ClientReturnShape::OtherValue => unreachable!("definition references were validated"),
        ClientReturnShape::Unsupported => unreachable!("function shape was validated"),
    }
}

/// Evaluates one decoded version-2 opaque plan against the function return
/// type, sharing the closed value-creation contract of the plain path.
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
) -> Result<RuntimeValue, ClientExecutionError> {
    if matches!(expression, ClientExpressionNode::ExternalContract { .. }) {
        return evaluate_expression(
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
        );
    }
    if !expression_returns_stream(active, expression, local_environment) {
        return Err(expression_error(context, ClientExpressionError::TypeMismatch));
    }
    evaluate_expression_plan(
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
    )
}

/// Evaluates one decoded expression tree and type-checks its value.
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
    let value = evaluate_expression(
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
) -> Result<RuntimeValue, ClientExecutionError> {
    initialize_client_state(
        active, plan, context, lineage, arguments, declarations, grants, state, depth, principal, executor,
        local_environment,
    )?;
    evaluate_stream_expression_plan(
        active, plan.expression(), context, lineage, expected, arguments, declarations, grants, state, depth,
        principal, executor, local_environment,
    )
}

/// Evaluates one decoded version-4 state plan after initialising its slots.
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
    )?;
    evaluate_expression_plan(
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
    )
}

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
) -> Result<RuntimeValue, ClientExecutionError> {
    evaluate_stream_expression_plan(
        active, plan.expression(), context, lineage, expected, arguments, declarations, grants, state, depth,
        principal, executor, local_environment,
    )
}

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
) -> Result<RuntimeValue, ClientExecutionError> {
    evaluate_expression_plan(
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
    )
}

/// Evaluates one decoded version-5 capability envelope after its stored
/// requirements passed the capability gate (work ADR 0060).
///
/// The envelope's requirements are the only capability gate for version-5
/// plans: the caller's declaration list is not consulted, so a recursive
/// CLIENT call validates its own stored requirements instead of inheriting
/// the parent declaration list.
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
    for statement in plan.statements() {
        let local_id = statement.local();
        let Some(local) = plan.locals().iter().find(|candidate| candidate.local_id() == local_id) else {
            return Err(expression_error(context, ClientExpressionError::ParameterNotBound));
        };
        match statement {
            orna_artifact::client_plan::ClientStatement::Let { expression, .. } => {
                if local_environment.contains_key(&local_id) {
                    return Err(expression_error(context, ClientExpressionError::InvalidCall));
                }
                let binding = evaluate_procedural_local(
                    active, local, expression, context, lineage, arguments, declarations, grants, state,
                    depth, principal, executor, local_environment,
                )?;
                local_environment.insert(local_id, binding);
            }
            orna_artifact::client_plan::ClientStatement::Assignment { expression, .. } => {
                if !local_environment.contains_key(&local_id) {
                    return Err(expression_error(context, ClientExpressionError::ParameterNotBound));
                }
                let binding = evaluate_procedural_local(
                    active, local, expression, context, lineage, arguments, declarations, grants, state,
                    depth, principal, executor, local_environment,
                )?;
                local_environment.insert(local_id, binding);
            }
        }
    }
    let value = evaluate_expression(
        active, plan.return_expression(), context, lineage, arguments, declarations, grants, state, depth,
        principal, executor, local_environment,
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
        Err(expression_error(context, ClientExpressionError::TypeMismatch))
    }
}

fn evaluate_procedural_local(
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
) -> Result<ClientLocalBinding, ClientExecutionError> {
    match local.kind() {
        ClientLocalKind::Value => {
            if procedural_resource_kind_for_runtime(expression, local_environment).is_some() {
                return Err(expression_error(context, ClientExpressionError::TypeMismatch));
            }
            let expected = resolve_client_local_type(active, local.type_id())
                .ok_or_else(|| expression_error(context, ClientExpressionError::TypeMismatch))?;
            let stream_await = expression_returns_stream(active, expression, local_environment);
            let value = evaluate_expression_plan(
                active, expression, context, lineage, expected, arguments, declarations, grants, state, depth,
                principal, executor, local_environment,
            )?;
            if stream_await {
                Ok(ClientLocalBinding::StreamValue(value))
            } else {
                Ok(ClientLocalBinding::Value(value))
            }
        }
        ClientLocalKind::Resource(kind) => {
            let ClientExpressionNode::Resource { operation } = expression else {
                let ClientExpressionNode::LocalRead { local: source } = expression else {
                    return Err(expression_error(context, ClientExpressionError::TypeMismatch));
                };
                let Some(ClientLocalBinding::Resource(operation)) = local_environment.get(source) else {
                    return Err(expression_error(context, ClientExpressionError::ParameterNotBound));
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

fn validate_procedural_resource_binding(
    active: &ActiveDatabaseRevision,
    local: &ClientLocal,
    kind: ResourceKind,
    operation: &ResourceOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    if operation.kind() != kind {
        return Err(expression_error(context, ClientExpressionError::TypeMismatch));
    }
    let resolved = resource_operation_result_type(active, operation, context)?;
    if !resource_type_matches_id(active, resolved, local.type_id()) {
        return Err(expression_error(context, ClientExpressionError::TypeMismatch));
    }
    Ok(())
}

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
                );
            }
            let (ClientReturnShape::Expression(expected) | ClientReturnShape::Inspect(expected)) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_expression_plan(
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
            )
        }
        InnerClientPlan::State(inner) => {
            if let ClientReturnShape::StreamState(expected) = return_shape {
                return evaluate_stream_state_plan(
                    active, inner, context, lineage, expected, arguments, declarations, grants, state, depth,
                    principal, executor, local_environment,
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
            )
        }
        InnerClientPlan::Procedural(inner) => {
            if let ClientReturnShape::StreamProcedural(expected) = return_shape {
                return evaluate_procedural_plan(
                    active, inner, context, lineage, expected, true, arguments, declarations, grants, state, depth,
                    principal, executor, local_environment,
                );
            }
            let ClientReturnShape::Procedural(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_procedural_plan(
                active, inner, context, lineage, expected, false, arguments, declarations, grants, state, depth, principal,
                executor, local_environment,
            )
        }
        InnerClientPlan::Action(inner) => {
            let ClientReturnShape::Action(_expected) = return_shape else { unreachable!("function shape was validated against the inner plan version"); };
            evaluate_action_operation(active, inner.operation(), context, lineage, arguments, declarations, grants, state, depth, principal, executor, local_environment)
        }
        InnerClientPlan::Resource(inner) => {
            if let ClientReturnShape::StreamResource(expected) = return_shape {
                return evaluate_stream_resource_plan(
                    active, inner, context, lineage, expected, arguments, declarations, grants, state, depth,
                    principal, executor, local_environment,
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

fn resource_operation_result_type(
    active: &ActiveDatabaseRevision,
    operation: &ResourceOperationNode,
    context: ClientExecutionContext,
) -> Result<ResolvedType, ClientExecutionError> {
    let raw_target = InvocationTarget::new(operation.target_function(), operation.target_revision());
    let invalid = ||
        evaluate_resource_error(
            context,
            ClientResourceExecutionError::Invalid(ClientResourceError::TargetMismatch {
                expected: raw_target,
            }),
        );
    let Some(resolved) = resolve_resource_operation_target(active, operation) else {
        return Err(invalid());
    };
    if resolved.definition.domain() != FunctionDomain::Server {
        return Err(invalid());
    }
    let (expected_kind, expected) = match (operation.kind(), resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => {
            (ResourceKind::Scalar, *result)
        }
        (ResourceKind::Stream, FunctionReturn::Stream(item)) => {
            (ResourceKind::Stream, *item)
        }
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
) -> Result<RuntimeValue, ClientExecutionError> {
    let raw_target = InvocationTarget::new(operation.target_function(), operation.target_revision());
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
        if evaluated.iter().any(|candidate: &FunctionArgument| candidate.parameter() == *parameter) {
            return Err(evaluate_resource_error(
                context,
                ClientResourceExecutionError::Invalid(ClientResourceError::DuplicateArgument {
                    parameter: *parameter,
                }),
            ));
        }
        let value = evaluate_expression(
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
    let evaluated = validate_resource_arguments(active, target, &evaluated)
    .map_err(|source| {
        evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source))
    })?;
    let digest = ClientResourceKey::canonical_arguments_digest(active, &evaluated)
        .map_err(|source| evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source)))?;
    let key = ClientResourceKey::new(
        target,
        principal,
        digest,
        resource_invalidation_identity(
            active.catalogue_hash(),
            state.context().data_invalidation_token(),
            state.security_context_digest(),
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
        .map_err(|source| evaluate_resource_error(context, ClientResourceExecutionError::Invalid(source)))?;
    let completion = executor.execute(request.clone());
    let completion_request_id = completion.request_id();
    let (completion_key, completion_generation) = match &completion {
        ClientResourceCompletion::Ready { key, generation, .. }
        | ClientResourceCompletion::StreamValues { key, generation, .. }
        | ClientResourceCompletion::StreamCompleted { key, generation, .. }
        | ClientResourceCompletion::Pending { key, generation, .. }
        | ClientResourceCompletion::Failed { key, generation, .. }
        | ClientResourceCompletion::Cancelled { key, generation, .. } => (*key, *generation),
    };
    let same_generation = completion_key == request.key()
        && completion_generation == request.generation();
    let same_request = completion_request_id == request.request_id();
    if let Err(source) = resource.apply_completion(active, completion) {
        if same_generation && same_request {
            let cancellation = executor.cancel(request.clone());
            match resource.apply_completion(active, cancellation) {
                Ok(()) => match resource.status() {
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
                },
                Err(_) => {}
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
            ClientResourceExecutionError::Invalid(ClientResourceError::InvalidTransition { status }),
        )),
    }
}

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
                    ClientResourceExecutionError::Invalid(
                        ClientResourceError::InvalidTransition {
                            status: ClientResourceStatus::Idle,
                        },
                    ),
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

pub fn encode_action_payload(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<Vec<u8>, ClientActionError> {
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
    payload.try_reserve(payload_len)
        .map_err(|_| action_payload_error("action payload allocation failed"))?;
    payload.extend_from_slice(ACTION_MAGIC.as_bytes());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&body);
    Ok(payload)
}

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
    let target = FunctionId::from_bytes(action_take(body, &mut offset, 16)?.try_into().unwrap());
    let source = orna_core::SourceRevisionId::from_bytes(
        action_take(body, &mut offset, 16)?.try_into().unwrap(),
    );
    let catalogue = orna_core::CatalogueRevisionId::from_bytes(
        action_take(body, &mut offset, 16)?.try_into().unwrap(),
    );
    let target_revision = RevisionPair::new(source, catalogue);
    if target_revision != active.pair() {
        return Err(ClientActionError::RevisionMismatch);
    }
    let call_site = CallSiteId::from_bytes(action_take(body, &mut offset, 16)?.try_into().unwrap());
    let result_type = TypeId::from_bytes(action_take(body, &mut offset, 16)?.try_into().unwrap());
    let count = u32::from_be_bytes(action_take(body, &mut offset, 4)?.try_into().unwrap()) as usize;
    if count > orna_artifact::client_plan::MAX_ACTION_ARGUMENTS {
        return Err(action_payload_error("too many action arguments"));
    }
    let mut arguments = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let parameter =
            ParameterId::from_bytes(action_take(body, &mut offset, 16)?.try_into().unwrap());
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

fn action_target_result_type(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<(ResourceKind, ResolvedType), ClientActionError> {
    let resolved_target = resolve_action_target(active, descriptor)?;
    let resolved = match resolved_target.definition.return_type() {
        FunctionReturn::Single(resolved) => *resolved,
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => {
            return Err(ClientActionError::ResultTypeMismatch)
        }
    };
    let kind = ResourceKind::Scalar;
    if !resource_type_matches_id(active, resolved, descriptor.result_type) {
        return Err(ClientActionError::ResultTypeMismatch);
    }
    Ok((kind, resolved))
}

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
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut values = Vec::with_capacity(operation.arguments().len());
    for (parameter, expression) in operation.arguments() {
        let value = evaluate_expression(
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

pub fn complete_client_action(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    complete_client_action_inner(active, action_state, completion, executor, true)
}

fn complete_client_action_inner(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
    cancel_on_invalid: bool,
) -> Result<ClientActionOutcome, ClientActionError> {
    let completion_request_id = completion.request_id();
    let (completion_key, completion_generation) = match &completion {
        ClientResourceCompletion::Ready { key, generation, .. }
        | ClientResourceCompletion::StreamValues { key, generation, .. }
        | ClientResourceCompletion::StreamCompleted { key, generation, .. }
        | ClientResourceCompletion::Pending { key, generation, .. }
        | ClientResourceCompletion::Failed { key, generation, .. }
        | ClientResourceCompletion::Cancelled { key, generation, .. } => (*key, *generation),
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
    let apply_result = action_state
        .resource_mut()
        .expect("action resource was checked above")
        .apply_completion(active, completion);
    if apply_result.is_err() {
        // A same-generation malformed completion must not strand the request
        // owned by the executor. Generation and key mismatches remain stale
        // and do not cancel a newer or unrelated request. A completion returned
        // by an explicit cancellation has already traversed that path once. If
        // the cancellation is itself pending or malformed, retain Loading state
        // so the caller can poll without losing executor ownership.
        if cancel_on_invalid {
            let cancel_request = action_state
                .resource
                .as_ref()
                .and_then(|resource| resource.active_request());
            if let Some(request) = cancel_request {
                let cancellation = executor.cancel(request);
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
                            ClientResourceStatus::Idle | ClientResourceStatus::Loading => unreachable!(),
                        };
                        action_state.clear();
                        return Ok(outcome);
                    }
                    Err(_) => return Err(ClientActionError::Pending),
                }
            }
        } else {
            return Err(ClientActionError::Pending);
        }
        action_state.clear();
        return Ok(redacted_action_failure());
    }
    let status = action_state
        .resource
        .as_ref()
        .expect("action resource remains after completion")
        .status();
    if status == ClientResourceStatus::Loading { return Err(ClientActionError::Pending); }
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
        active, action, authorisation, parent, action_state, declarations, grants, state,
        parent.observer_lineage(), executor,
    )
}

fn client_action_target_is_provenance_safe(
    active: &ActiveDatabaseRevision,
    parent: ClientExecutionContext,
    target: FunctionId,
) -> bool {
    if active
        .catalogue()
        .function_by_id(parent.function())
        .is_none_or(|definition| definition.domain() != FunctionDomain::Client)
    {
        return false;
    }
    active.references().iter().any(|reference| {
        reference.source_function() == parent.function()
            && reference.source_revision() == parent.function_revision()
            && reference.kind() == DefinitionReferenceKind::FunctionCall
            && reference.target() == DefinitionReferenceTarget::Function(target)
    })
}

/// Adapts nested CLIENT resource execution to the terminal action contract.
///
/// A nested resource has no continuation surface in action v1. If its executor reports
/// `Pending`, cancel that request before publishing one redacted terminal action
/// failure; otherwise the staged resource would keep a live request after the action has ended.
struct ClientActionNestedExecutor<'a> {
    inner: &'a mut dyn ClientResourceExecutor,
}

impl ClientResourceExecutor for ClientActionNestedExecutor<'_> {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        let failure_request = request.clone();
        let completion = self.inner.execute(request);
        if matches!(completion, ClientResourceCompletion::Pending { .. }) {
            let _ = self.inner.cancel(failure_request.clone());
            return failure_request.failed(ACTION_FAILURE_CODE.to_owned());
        }
        completion
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.inner.cancel(request)
    }
}

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
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key { return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned())); }
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
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key { return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned())); }
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
            let mut nested_executor = ClientActionNestedExecutor { inner: executor };
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
            let completion = match result {
                Ok((_, value)) => request.ready(value),
                Err(ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Cancelled,
                    ..
                }) => request.cancelled(),
                Err(_) => request.failed(ACTION_FAILURE_CODE.to_owned()),
            };
            let outcome = complete_client_action(active, action_state, completion, &mut nested_executor)?;
            if outcome == ClientActionOutcome::Completed {
                *state = staged;
            }
            Ok(outcome)
        }
    }
}

pub(crate) fn stable_inspect_provider_error(error: &str) -> String {
    stable_inspect_error_code(error).to_owned()
}

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
    let request = ClientExternalContractRequest::with_lineage(
        context,
        identity,
        arguments.to_vec(),
        lineage,
    );
    executor.external_contract(request).map_err(|code| {
        if identity == INSPECT_RENDER_CONTRACT {
            ClientExecutionError::Inspect {
                context,
                source: ClientInspectError::Failed(if code == EXTERNAL_CONTRACT_RUNTIME_UNAVAILABLE {
                    "inspect.runtime_unavailable".to_owned()
                } else {
                    stable_inspect_provider_error(&code)
                }),
            }
        } else {
            ClientExecutionError::ExternalContract {
                context,
                identity: identity.to_owned(),
            }
        }
    })
}


fn inspect_render_contract_error(
    context: ClientExecutionContext,
) -> ClientExecutionError {
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
                InnerClientPlan::Expression(expression) => Some(is_external(expression.expression())),
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

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
    for (index, ((parameter_id, value), (expected_name, expected_type, _))) in
        arguments.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE).enumerate()
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
    for ((_, value), (_, expected_type, expected_kind)) in arguments
        .iter()
        .zip(INSPECT_RENDER_CARRIER_SIGNATURE)
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

fn inspect_render_ui_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> bool {
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
    OpaqueValue::new(active, &registry, STD_UI_TYPE_ID, opaque.canonical_payload()).is_ok()
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
        let row_target = InvocationId::from_bytes(
            payload[25..41]
                .try_into()
                .expect("projection target width"),
        );
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
fn inspect_target_is_observer(
    context: ClientExecutionContext,
    target: InvocationId,
) -> bool {
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
    let outcome = *row.get(offset).ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
    if !(1..=4).contains(&outcome) {
        return Err(inspect_carrier_error("inspect.malformed_carrier"));
    }
    offset += 1 + 8;
    let result = *row.get(offset).ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
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
        },
        _ => return Err(inspect_carrier_error("inspect.malformed_carrier")),
    }
    let duration = *row.get(offset).ok_or_else(|| inspect_carrier_error("inspect.malformed_carrier"))?;
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
    decode_inspect_snapshot_target_row(payload, envelope.epoch_id())
}

fn inspect_carrier_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: TypeId,
) -> bool {
    decode_inspect_carrier(active, value, expected).is_ok()
}

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
            let target = evaluate_expression(
                active, target, context, lineage, arguments, declarations, grants, state, depth + 1,
                principal, executor, local_environment,
            )?;
            let Some(invocation) = inspect_invocation_target(&target) else {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: ClientInspectError::InvalidTarget,
                });
            };
            if inspect_target_is_observer_with_lineage(lineage, invocation)
            {
                return Err(ClientExecutionError::Inspect {
                    context,
                    source: inspect_carrier_error("inspect.recursion"),
                });
            }
            if let Some(options) = options {
                let options = evaluate_expression(
                    active, options, context, lineage, arguments, declarations, grants, state, depth + 1,
                    principal, executor, local_environment,
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
        InspectOperationNode::Projection { projection, snapshot } => {
            let snapshot = evaluate_expression(
                active, snapshot, context, lineage, arguments, declarations, grants, state, depth + 1,
                principal, executor, local_environment,
            )?;
            let snapshot_envelope = match decode_inspect_carrier(
                active,
                &snapshot,
                SYS_INSPECT_SNAPSHOT_TYPE_ID,
            ) {
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
        (None, None) => ClientInspectRequest::with_provenance(
            context,
            operation.clone(),
            None,
            None,
            lineage,
        ),
        (None, Some(_)) => unreachable!("snapshot options require a target"),
    };
    let value = executor.inspect(request).map_err(|code| ClientExecutionError::Inspect {
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
    match expression {
        ClientExpressionNode::Await { expression } => match expression.as_ref() {
            ClientExpressionNode::Resource { operation } => evaluate_resource_expression(
                active, operation, context, lineage, arguments, declarations, grants, state, depth, principal,
                executor, local_environment,
            ),
            ClientExpressionNode::LocalRead { local } => {
                let Some(ClientLocalBinding::Resource(operation)) = local_environment.get(local) else {
                    return Err(expression_error(context, ClientExpressionError::ParameterNotBound));
                };
                let operation = operation.clone();
                evaluate_resource_expression(
                    active, &operation, context, lineage, arguments, declarations, grants, state, depth, principal,
                    executor, local_environment,
                )
            }
            _ => Err(expression_error(context, ClientExpressionError::InvalidCall)),
        },
        ClientExpressionNode::Resource { operation } => evaluate_resource_expression(
            active, operation, context, lineage, arguments, declarations, grants, state, depth, principal,
            executor, local_environment,
        ),
        ClientExpressionNode::Action { operation } => evaluate_action_operation(
            active, operation, context, lineage, arguments, declarations, grants, state, depth, principal,
            executor, local_environment,
        ),
        ClientExpressionNode::Inspect { operation } => evaluate_inspect_expression(
            active, operation, context, lineage, arguments, declarations, grants, state, depth, principal,
            executor, local_environment,
        ),
        ClientExpressionNode::String { value } => Ok(RuntimeValue::Text(value.clone())),
        ClientExpressionNode::Integer { value } => i32::try_from(*value)
            .map(RuntimeValue::Integer)
            .map_err(|_| expression_error(context, ClientExpressionError::TypeMismatch)),
        ClientExpressionNode::Boolean { value } => Ok(RuntimeValue::Boolean(*value)),
        ClientExpressionNode::ParameterRead { parameter } => arguments
            .iter()
            .find(|(candidate, _)| candidate == parameter)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| expression_error(context, ClientExpressionError::ParameterNotBound)),
        ClientExpressionNode::LocalRead { local } => match local_environment.get(local) {
            Some(ClientLocalBinding::Value(value) | ClientLocalBinding::StreamValue(value)) => {
                Ok(value.clone())
            }
            Some(ClientLocalBinding::Resource(_)) => {
                Err(expression_error(context, ClientExpressionError::TypeMismatch))
            }
            None => Err(expression_error(context, ClientExpressionError::ParameterNotBound)),
        },
        ClientExpressionNode::FieldPath { root, fields } => {
            let value = arguments
                .iter()
                .find(|(candidate, _)| candidate == root)
                .map(|(_, value)| value)
                .ok_or_else(|| expression_error(context, ClientExpressionError::ParameterNotBound))?;
            evaluate_field_path(active, value, fields, context)
        }
        ClientExpressionNode::Concat { left, right } => {
            let left = evaluate_expression(
                active, left, context, lineage, arguments, declarations, grants, state, depth, principal,
                executor, local_environment,
            )?;
            let right = evaluate_expression(
                active, right, context, lineage, arguments, declarations, grants, state, depth, principal,
                executor, local_environment,
            )?;
            let (RuntimeValue::Text(left), RuntimeValue::Text(right)) = (left, right) else {
                return Err(expression_error(context, ClientExpressionError::TypeMismatch));
            };
            Ok(RuntimeValue::Text(format!("{left}{right}")))
        }
        ClientExpressionNode::Call { function, arguments: bound } => {
            if depth > orna_artifact::client_plan::MAX_EXPRESSION_DEPTH {
                return Err(expression_error(context, ClientExpressionError::RecursionLimit));
            }
            if !client_call_target_is_referenced(active, context, *function) {
                return Err(expression_error(context, ClientExpressionError::InvalidCall));
            }
            let mut evaluated = Vec::with_capacity(bound.len());
            for (parameter, expression) in bound {
                if evaluated.iter().any(|(candidate, _)| candidate == parameter) {
                    return Err(expression_error(context, ClientExpressionError::InvalidCall));
                }
                let value = evaluate_expression(
                    active, expression, context, lineage, arguments, declarations, grants, state, depth, principal,
                    executor, local_environment,
                )?;
                evaluated.push((*parameter, value));
            }
            let (_, value) = evaluate_function(
                active,
                *function,
                evaluated,
                declarations,
                grants,
                state,
                depth + 1,
                principal,
                lineage.nested(),
                executor,
            )?;
            Ok(value)
        }
        ClientExpressionNode::ExternalContract { identity } => {
            if identity == INSPECT_RENDER_CONTRACT {
                validate_inspect_render_contract(active, context, identity, arguments)?;
                let value = evaluate_external_contract(identity, context, lineage, arguments, executor)?;
                if !inspect_render_ui_value_matches(active, &value) {
                    return Err(ClientExecutionError::Inspect {
                        context,
                        source: ClientInspectError::TypeMismatch,
                    });
                }
                Ok(value)
            } else {
                evaluate_external_contract(identity, context, lineage, arguments, executor)
            }
        }
}
}

fn evaluate_field_path(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    fields: &[orna_core::FieldId],
    context: ClientExecutionContext,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut current = value;
    for field_id in fields {
        let RuntimeValue::Record(record) = current else {
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
            .ok_or_else(|| expression_error(context, ClientExpressionError::FieldPath))?;
    }
    Ok(current.clone())
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
        ClientExpressionNode::Call { function, .. } => active
            .catalogue()
            .function_by_id(*function)
            .is_some_and(|function| matches!(function.return_type(), FunctionReturn::Stream(_))),
        ClientExpressionNode::Inspect { .. } => false,
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
            | SYS_INSPECT_SNAPSHOT_TYPE_ID
            | SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
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

fn runtime_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: ResolvedType,
) -> bool {
    if let RuntimeValue::Null(null) = value {
        return null.resolved_type() == expected && active_type_is_known(active, expected);
    }
    let scalar_matches = |scalar| match (scalar, value) {
        (StandardScalar::Boolean, RuntimeValue::Boolean(_))
        | (StandardScalar::Integer, RuntimeValue::Integer(_))
        | (StandardScalar::BigInt, RuntimeValue::BigInt(_))
        | (StandardScalar::Float, RuntimeValue::Float(_))
        | (StandardScalar::CharacterLargeObject, RuntimeValue::Text(_))
        | (StandardScalar::BinaryLargeObject, RuntimeValue::Bytes(_)) => true,
        _ => false,
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
            if type_id == SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID {
                // V1 has no registered options codec. Never admit an opaque
                // payload merely because its type id is sealed; supplied
                // options therefore fail closed instead of being discarded.
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
) -> Result<(), ClientExecutionError> {
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
                let value = evaluate_expression(
                    active,
                    node,
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
        match slot.scope() {
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
                    entry.insert(ClientUserState::defaulted(value, slot.type_id()));
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
    Action(TypeId),
    Inspect(ResolvedType),
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
        EXPRESSION_FORMAT_VERSION | STATE_FORMAT_VERSION | RESOURCE_FORMAT_VERSION | PROCEDURAL_FORMAT_VERSION | orna_artifact::client_plan::ACTION_FORMAT_VERSION | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
    );
    let stream_expression_eligible = artifact_version == EXPRESSION_FORMAT_VERSION;
    let expression_shape = |resolved_type: ResolvedType| {
        if artifact_version == STATE_FORMAT_VERSION {
            ClientReturnShape::State(resolved_type)
        } else if artifact_version == RESOURCE_FORMAT_VERSION {
            ClientReturnShape::Resource(resolved_type)
        } else if artifact_version == PROCEDURAL_FORMAT_VERSION {
            ClientReturnShape::Procedural(resolved_type)
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
    if resolved_type.reference_target().is_some() || resolved_type.named_type().is_some() {
        return ClientReturnShape::Unsupported;
    }
    if let Some(type_id) = resolved_type.value_type() {
        if artifact_version == orna_artifact::client_plan::ACTION_FORMAT_VERSION && type_id == STD_ACTION_TYPE_ID {
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
        EXPRESSION_FORMAT_VERSION | STATE_FORMAT_VERSION | RESOURCE_FORMAT_VERSION | PROCEDURAL_FORMAT_VERSION | orna_artifact::client_plan::ACTION_FORMAT_VERSION | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
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
                definition.parameters().iter().any(|parameter| {
                    parameter.resolved_type().reference_target() == Some(target)
                })
            })
        }
        _ => false,
    }
}

fn validate_selected_references(
    active: &ActiveDatabaseRevision,
    semantic_hash_version: FunctionSemanticHashVersion,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
) -> Result<(), ClientExecutionError> {
    let selected = active
        .references()
        .iter()
        .filter(|reference| {
            reference.source_function() == context.function()
                && reference.source_revision() == context.function_revision()
        })
        .collect::<Vec<_>>();
    let function = active.catalogue().function_by_id(context.function());

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
                | ClientReturnShape::Action(_)
                | ClientReturnShape::Inspect(_)
            ) {
                if selected
                    .iter()
                    .any(|reference| !is_expression_reference_allowed(function, reference))
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
                                return_type == type_id && type_id == STD_ACTION_TYPE_ID
                                    && definition.is_some_and(|value| value.kind() == ValueTypeKind::Opaque)
                            }
                            ClientReturnShape::Expression(_)
                            | ClientReturnShape::StreamExpression(_)
                            | ClientReturnShape::State(_)
                            | ClientReturnShape::StreamState(_)
                            | ClientReturnShape::Resource(_)
                            | ClientReturnShape::StreamResource(_)
                            | ClientReturnShape::Procedural(_)
                            | ClientReturnShape::StreamProcedural(_)
                            | ClientReturnShape::Inspect(_)
                            | ClientReturnShape::OtherValue
                            | ClientReturnShape::Unsupported => false,
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
    active.references().iter().any(|reference| {
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
fn preflight_client_expression_calls(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;

    preflight_client_call_targets(active, context, decoded_targets)
}

fn preflight_client_call_targets(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    decoded_targets: Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    let mut durable_references = active
        .references()
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
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    }
    Ok(())
}

fn preflight_client_state_calls(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for slot in plan.slots() {
        if let StateDefault::Expression(expression) = slot.default() {
            collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;
        }
    }
    collect_client_expression_call_targets(active, plan.expression(), context, &mut decoded_targets)?;
    preflight_client_call_targets(active, context, decoded_targets)
}

fn preflight_client_procedural_calls(
    active: &ActiveDatabaseRevision,
    plan: &ProceduralClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for statement in plan.statements() {
        collect_client_expression_call_targets(active, statement.expression(), context, &mut decoded_targets)?;
    }
    collect_client_expression_call_targets(active, plan.return_expression(), context, &mut decoded_targets)?;
    preflight_client_call_targets(active, context, decoded_targets)
}

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
        InnerClientPlan::Procedural(inner) => preflight_client_procedural_calls(active, inner, context),
        InnerClientPlan::Action(inner) => preflight_client_action_calls(active, inner.operation(), context),
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

fn validate_client_resource_operation(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    operation: &ResourceOperationNode,
) -> Result<(), ClientExecutionError> {
    let Some(resolved) = resolve_resource_operation_target(active, operation) else {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    };
    if resolved.definition.domain() != FunctionDomain::Server
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    }
    let expected = match (operation.kind(), resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => *result,
        (ResourceKind::Stream, FunctionReturn::Stream(result)) => *result,
        _ => return Err(expression_error(context, ClientExpressionError::InvalidCall)),
    };
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    }
    Ok(())
}

fn validate_client_action_operation(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let raw_target = InvocationTarget::new(operation.target_function(), operation.target_revision());
    let Some(resolved) = resolve_unclassified_target(active, raw_target) else {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    };
    let expected_domain = match operation.domain() {
        ActionTargetDomain::Client => FunctionDomain::Client,
        ActionTargetDomain::Server => FunctionDomain::Server,
    };
    if resolved.definition.domain() != expected_domain
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    }
    let FunctionReturn::Single(expected) = resolved.definition.return_type() else {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    };
    let expected = *expected;
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(context, ClientExpressionError::InvalidCall));
    }
    Ok(())
}

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
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Action { operation } => {
            validate_client_action_operation(active, operation, context)?;
            for (_, expression) in operation.arguments() {
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(expression) = operation.target() {
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
            if let Some(expression) = operation.options() {
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
            if let Some(expression) = operation.snapshot_expression() {
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
        }
        ClientExpressionNode::Call { function, arguments } => {
            let Some(definition) = active.catalogue().function_by_id(*function) else {
                return Err(expression_error(context, ClientExpressionError::InvalidCall));
            };
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
                return Err(expression_error(context, ClientExpressionError::InvalidCall));
            }
            for (_, expression) in arguments {
                collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
            }
            decoded_targets.push(*function);
        }
        ClientExpressionNode::Concat { left, right } => {
            collect_client_expression_call_targets(active, left, context, decoded_targets)?;
            collect_client_expression_call_targets(active, right, context, decoded_targets)?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. } => {}
    }
    Ok(())
}

/// Validates the saved artefact contract against the effective plan version.
///
/// For a version-5 capability envelope the effective version is the inner
/// plan version (the envelope decode already fixed the outer version); for
/// versions 1-4 it is the artefact's own version.
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
        ClientReturnShape::State(_) | ClientReturnShape::StreamState(_) => STATE_FORMAT_VERSION,
        ClientReturnShape::Resource(_) | ClientReturnShape::StreamResource(_) => {
            RESOURCE_FORMAT_VERSION
        }
        ClientReturnShape::Action(_) => orna_artifact::client_plan::ACTION_FORMAT_VERSION,
        ClientReturnShape::Inspect(_) => orna_artifact::client_plan::INSPECT_FORMAT_VERSION,
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

/// Validates the saved CLIENT artefact identity and exact payload digest before
/// any plan decoder or evaluation side effect. Integrity failures deliberately
/// use the existing redacted invalid-artifact contract.
fn validate_artifact_identity(
    artifact: &orna_core::revision::ExecutableArtifact,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    if artifact.kind() != ExecutableArtifactKind::Client {
        return Err(invalid_artifact(context));
    }
    let digest = artifact_payload_digest(artifact.payload())
        .map_err(|_| invalid_artifact(context))?;
    if digest != artifact.content_hash() {
        return Err(invalid_artifact(context));
    }
    Ok(())
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
mod tests {
    use super::{
        capability, complete_client_action, decode_action_payload, encode_action_payload,
        trigger_client_action, action_target_result_type, ClientActionDescriptor, ClientActionError, ClientActionOutcome,
        ClientActionState, ClientExecutionContext, ClientResource, ClientResourceCompletion,
        ClientResourceKey, ClientResourceRequest, ClientResourceStatus, ClientStateStore,
        ClientResourceExecutor, DeterministicClientResourceExecutor, ResourceKind,
        ACTION_FAILURE_CODE,
    };
    use orna_artifact::client_plan::{ActionTargetDomain, InspectProjection};
    use std::{cell::Cell, rc::Rc, time::SystemTime};

    use orna_core::{
        CallSiteId, CatalogueRevisionId, FunctionId, FunctionRevisionId, InvocationId, LocalId, ParameterId, PrincipalId, SchemaId,
        SourceBundleId, SourceRevisionId, SourceUnitId, StateSlotId, TypeId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
            function_declaration_digest, function_semantic_digest,
            function_semantic_digest_with_version, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionReturnColumnDefinition, FunctionSecurity, FunctionVolatility, ParameterDefinition,
            QualifiedSemanticName, SchemaDefinition, ValueTypeDefinition,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            DefinitionIdentity, DefinitionOrigin, DefinitionReference, DefinitionReferenceKind,
            DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
            ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion,
            RevisionInvariantError, RevisionPair, Sha256Digest, SourceOrigin,
            StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
        },
        security::{
            AuthorisedInvocation, ExecuteDecision, ExecuteGrant, InvocationTarget, Principal,
            PrincipalKind, PrincipalStatus, RoleMembership, SecuritySnapshot,
        },
        source::{SourceBundle, SourceUnit},
        state::{UserStateCell, UserStateKey, UserStateWriteOutcome, UserStateWriteResult},
        types::{ResolvedType, StandardScalar},
        value::{FunctionArgument, OpaqueValue, RuntimeFloat, RuntimeValue},
    };

    #[derive(Default)]
    struct RecordingActionExecutor {
        executed: Vec<ClientResourceRequest>,
        cancelled: Vec<ClientResourceRequest>,
        result: Option<RuntimeValue>,
        cancel_pending: bool,
        cancel_value: Option<RuntimeValue>,
    }

    impl RecordingActionExecutor {
        fn new(result: Option<RuntimeValue>) -> Self {
            Self { result, ..Self::default() }
        }

        fn with_cancel_pending(mut self) -> Self {
            self.cancel_pending = true;
            self
        }

        fn with_cancel_value(mut self, value: RuntimeValue) -> Self {
            self.cancel_value = Some(value);
            self
        }
    }

    impl ClientResourceExecutor for RecordingActionExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.executed.push(request.clone());
            match self.result.clone() {
                Some(value) => request.ready(value),
                None => request.pending(),
            }
        }

        fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.cancelled.push(request.clone());
            if self.cancel_pending {
                request.pending()
            } else if let Some(value) = self.cancel_value.clone() {
                request.ready(value)
            } else {
                request.cancelled()
            }
        }
    }

    #[derive(Default)]
    struct FailingActionExecutor {
        request: Option<ClientResourceRequest>,
    }

    #[derive(Default)]
    struct CancelledActionExecutor {
        request: Option<ClientResourceRequest>,
    }

    impl ClientResourceExecutor for CancelledActionExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.request = Some(request.clone());
            request.cancelled()
        }
    }

    #[derive(Default)]
    struct MalformedResourceExecutor {
        executed: Option<ClientResourceRequest>,
        cancelled: Vec<ClientResourceRequest>,
        cancel_ready: bool,
        stale_request_id: bool,
    }

    impl ClientResourceExecutor for MalformedResourceExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.executed = Some(request.clone());
            if self.stale_request_id {
                ClientResourceCompletion::Ready {
                    request_id: InvocationId::from_bytes([0xff; 16]),
                    key: request.key(),
                    generation: request.generation(),
                    value: RuntimeValue::Integer(7),
                }
            } else {
                request.ready(RuntimeValue::Integer(7))
            }
        }

        fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.cancelled.push(request.clone());
            if self.cancel_ready {
                request.ready(RuntimeValue::Text("cancelled-ready".to_owned()))
            } else {
                request.cancelled()
            }
        }
    }

    #[derive(Default)]
    struct PollingTestExecutor {
        pending: Option<ClientResourceRequest>,
    }

    impl ClientResourceExecutor for PollingTestExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.pending = Some(request.clone());
            request.pending()
        }

        fn poll(&mut self) -> Option<ClientResourceCompletion> {
            self.pending.take().map(|request| request.ready(RuntimeValue::Boolean(true)))
        }
    }

    struct StreamBatchTestExecutor {
        value: bool,
    }

    impl ClientResourceExecutor for StreamBatchTestExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            request.stream_values(vec![RuntimeValue::Boolean(self.value)])
        }
    }

    impl ClientResourceExecutor for FailingActionExecutor {
        fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            self.request = Some(request.clone());
            request.failed("secret.executor.detail".to_owned())
        }

        fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
            request.cancelled()
        }
    }

    #[test]
    fn inspect_executor_default_is_fail_closed() {
        let context = super::ClientExecutionContext {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x11; 16]),
                CatalogueRevisionId::from_bytes([0x22; 16]),
            ),
            function: FunctionId::from_bytes([0x33; 16]),
            function_revision: FunctionRevisionId::from_bytes([0x44; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x55; 16]),
            observer_lineage: None,
        };
        let operation = super::ClientInspectOperation::Snapshot {
            target: RuntimeValue::Boolean(true),
        };
        let request = super::ClientInspectRequest::new(context, operation);
        let mut executor = super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        });
        assert_eq!(
            executor.inspect(request),
            Err("inspect.runtime_unavailable".to_owned())
        );
    }
    #[test]
    fn inspect_render_external_contract_dispatches_typed_arguments() {
        let context = super::ClientExecutionContext {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x71; 16]),
                CatalogueRevisionId::from_bytes([0x72; 16]),
            ),
            function: FunctionId::from_bytes([0x73; 16]),
            function_revision: FunctionRevisionId::from_bytes([0x74; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x75; 16]),
            observer_lineage: None,
        };
        let parameter = ParameterId::from_bytes([0x76; 16]);
        let mut executor = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        )
        .with_external_contract(move |request| {
            assert_eq!(request.identity(), super::INSPECT_RENDER_CONTRACT);
            assert_eq!(request.context(), context);
            assert_eq!(
                request.arguments(),
                &[(parameter, RuntimeValue::Boolean(true))],
            );
            Ok(RuntimeValue::Text("ui".to_owned()))
        });
        let mut optional: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
        assert_eq!(
            super::evaluate_external_contract(
                super::INSPECT_RENDER_CONTRACT,
                context,
            super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
                &mut optional,
            )
            .unwrap(),
            RuntimeValue::Text("ui".to_owned()),
        );
    }


    #[test]
    fn generic_external_contracts_forward_and_fail_closed_without_executor() {
        let context = super::ClientExecutionContext {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x81; 16]),
                CatalogueRevisionId::from_bytes([0x82; 16]),
            ),
            function: FunctionId::from_bytes([0x83; 16]),
            function_revision: FunctionRevisionId::from_bytes([0x84; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x85; 16]),
            observer_lineage: None,
        };
        let parameter = ParameterId::from_bytes([0x86; 16]);
        let mut executor = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        )
        .with_external_contract(move |request| {
            assert_eq!(request.identity(), "app.other@1");
            assert_eq!(request.arguments(), &[(parameter, RuntimeValue::Boolean(true))]);
            Ok(RuntimeValue::Boolean(false))
        });
        let mut optional: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
        assert_eq!(
            super::evaluate_external_contract(
                "app.other@1",
                context,
            super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
                &mut optional,
            ),
            Ok(RuntimeValue::Boolean(false)),
        );
        let mut failing = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        )
        .with_external_contract(|_| Err("inspect.denied".to_owned()));
        let mut failing_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut failing);
        assert!(matches!(
            super::evaluate_external_contract(
                "app.other@1",
                context,
            super::ObserverLineage::compatibility(context),
            &[],
                &mut failing_slot,
            ),
            Err(super::ClientExecutionError::ExternalContract { identity, .. })
                if identity == "app.other@1"
        ));
        let mut default_executor = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        );
        let mut default_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut default_executor);
        assert_eq!(
            super::evaluate_external_contract(
                super::INSPECT_RENDER_CONTRACT,
                context,
            super::ObserverLineage::compatibility(context),
            &[],
                &mut default_slot,
            ),
            Err(super::ClientExecutionError::Inspect {
                context,
                source: super::ClientInspectError::Failed(
                    "inspect.runtime_unavailable".to_owned(),
                ),
            }),
        );
        let mut absent: Option<&mut dyn super::ClientResourceExecutor> = None;
        assert!(matches!(
            super::evaluate_external_contract(
                "app.other@1",
                context,
            super::ObserverLineage::compatibility(context),
            &[],
                &mut absent,
            ),
            Err(super::ClientExecutionError::ExternalContract { identity, .. })
                if identity == "app.other@1"
        ));
        assert_eq!(
            super::evaluate_external_contract(
                super::INSPECT_RENDER_CONTRACT,
                context,
            super::ObserverLineage::compatibility(context),
            &[],
                &mut absent,
            ),
            Err(super::ClientExecutionError::Inspect {
                context,
                source: super::ClientInspectError::Failed(
                    "inspect.runtime_unavailable".to_owned(),
                ),
            }),
        );
    }
    #[test]
    fn inspect_render_provider_errors_are_whitelisted_and_redacted() {
        assert_eq!(
            super::stable_inspect_provider_error("inspect.denied"),
            "inspect.denied"
        );
        assert_eq!(
            super::stable_inspect_provider_error("inspect.revision_mismatch"),
            "inspect.epoch_mismatch"
        );
        assert_eq!(
            super::stable_inspect_provider_error("inspect.epoch_unavailable"),
            "inspect.stale_epoch"
        );
        assert_eq!(
            super::stable_inspect_provider_error("secret provider detail"),
            "inspect.projection_failed"
        );
        assert_eq!(
            super::stable_inspect_provider_error("inspect.projection_failed\0secret"),
            "inspect.projection_failed"
        );
    }

    #[test]
    fn inspect_request_provenance_rejects_observer_target() {
        let context = super::ClientExecutionContext {
            pair: super::RevisionPair::new(
                orna_core::SourceRevisionId::from_bytes([0x01; 16]),
                orna_core::CatalogueRevisionId::from_bytes([0x02; 16]),
            ),
            function: super::FunctionId::from_bytes([0x03; 16]),
            function_revision: super::FunctionRevisionId::from_bytes([0x04; 16]),
            parent_invocation_id: super::InvocationId::from_bytes([0x05; 16]),
            observer_lineage: None,
        };
        assert!(super::inspect_target_is_observer(
            context,
            context.observer_root_invocation_id(),
        ));
        let request = super::ClientInspectRequest::new(
            context,
            super::ClientInspectOperation::Snapshot {
                target: super::RuntimeValue::Reference {
                    target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                    object: orna_core::ObjectId::from_bytes([0x06; 16]),
                },
            },
        );
        assert_eq!(request.observer_root_invocation_id(), context.parent_invocation_id());
        assert_eq!(request.observer_parent_invocation_id(), context.parent_invocation_id());
        assert_eq!(request.observer_purpose(), "inspect");
        assert_eq!(request.target_invocation_id(), Some(super::InvocationId::from_bytes([0x06; 16])));
    }

    #[test]
    fn nested_observer_lineage_propagates_root_parent_and_current() {
        let root = super::InvocationId::from_bytes([0x31; 16]);
        let top = super::ObserverLineage::top_level(root);
        assert_eq!(top.root, root);
        assert_eq!(top.parent, root);
        assert_eq!(top.current, root);

        let nested = top.nested();
        assert_eq!(nested.root, root);
        assert_eq!(nested.parent, root);
        assert_ne!(nested.current, root);

        let child = nested.nested();
        assert_eq!(child.root, root);
        assert_eq!(child.parent, nested.current);
        assert_ne!(child.current, nested.current);

        let grandchild = child.nested();
        assert_eq!(grandchild.root, root);
        assert_eq!(grandchild.parent, child.current);
        assert!(grandchild.contains(root));
        assert!(grandchild.contains(nested.current));
        assert!(grandchild.contains(child.current));
        assert!(grandchild.contains(grandchild.current));

        let context = super::ClientExecutionContext {
            pair: super::RevisionPair::new(
                orna_core::SourceRevisionId::from_bytes([0x32; 16]),
                orna_core::CatalogueRevisionId::from_bytes([0x33; 16]),
            ),
            function: super::FunctionId::from_bytes([0x34; 16]),
            function_revision: super::FunctionRevisionId::from_bytes([0x35; 16]),
            parent_invocation_id: nested.current,
            observer_lineage: Some(nested),
        };
        assert_eq!(context.observer_root_invocation_id(), root);
        assert_eq!(context.observer_parent_invocation_id(), nested.current);
        let operation = super::ClientInspectOperation::Snapshot {
            target: super::RuntimeValue::Boolean(true),
        };
        let compatibility_request = super::ClientInspectRequest::new(context, operation.clone());
        assert_eq!(compatibility_request.observer_root_invocation_id(), root);
        assert_eq!(compatibility_request.observer_parent_invocation_id(), nested.current);
        assert_eq!(compatibility_request.observer_lineage(), &[root, nested.current]);
        let external_request = super::ClientExternalContractRequest::new(
            context,
            "app.test@1",
            Vec::new(),
        );
        assert_eq!(external_request.observer_root_invocation_id(), root);
        assert_eq!(external_request.observer_parent_invocation_id(), nested.current);
        let request = super::ClientInspectRequest::with_provenance(
            context, operation, None, None, nested,
        );
        assert_eq!(request.observer_root_invocation_id(), root);
        assert_eq!(request.observer_parent_invocation_id(), nested.current);
        assert_eq!(request.observer_lineage(), &[root, nested.current]);

        let mut executor = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
        )
        .with_external_contract(move |request| {
            assert_eq!(request.observer_root_invocation_id(), root);
            assert_eq!(request.observer_parent_invocation_id(), child.current);
            Ok(RuntimeValue::Text("ui".to_owned()))
        });
        let mut executor_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
        let value = super::evaluate_external_contract(
            super::INSPECT_RENDER_CONTRACT,
            context,
            child,
            &[],
            &mut executor_slot,
        )
        .unwrap();
        assert_eq!(value, RuntimeValue::Text("ui".to_owned()));
    }

    #[test]
    fn inspect_snapshot_validation_rejects_empty_carrier_rows() {
        let (active, _, pair, _) = version_one_active(true);
        let envelope = super::InspectCarrierEnvelope::new(
            super::InspectCarrierKind::Snapshot,
            7,
            pair.source(),
            pair.catalogue(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            super::inspect_snapshot_target_from_envelope(&active, &envelope),
            Err(super::ClientInspectError::Failed(
                "inspect.malformed_carrier".to_owned(),
            )),
        );
    }

    #[test]
    fn inspect_snapshot_row_binding_rejects_epoch_mismatch_and_trailing_bytes() {
        let target = super::InvocationId::from_bytes([0x17; 16]);
        let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
        row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
        row.extend_from_slice(&target.to_bytes());
        row.extend_from_slice(&[0x18; 16]);
        row.push(1);
        row.extend_from_slice(&0_u64.to_be_bytes());
        row.push(0);
        row.push(0);
        assert_eq!(super::decode_inspect_snapshot_target_row(&row, 7), Ok(target));
        assert_eq!(
            super::decode_inspect_snapshot_target_row(&row, 8),
            Err(super::ClientInspectError::Failed("inspect.epoch_mismatch".to_owned()))
        );
        row.extend_from_slice(&[0x19; 16]);
        assert_eq!(
            super::decode_inspect_snapshot_target_row(&row, 7),
            Err(super::ClientInspectError::Failed("inspect.malformed_carrier".to_owned()))
        );
    }

    #[test]
    fn inspect_snapshot_row_rejects_zero_value_batch_count() {
        let target = super::InvocationId::from_bytes([0x17; 16]);
        let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
        row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
        row.extend_from_slice(&target.to_bytes());
        row.extend_from_slice(&[0x18; 16]);
        row.push(1);
        row.extend_from_slice(&0_u64.to_be_bytes());
        row.push(1);
        row.extend_from_slice(&0_u64.to_be_bytes());
        row.push(0);

        assert_eq!(row.len(), 76);
        assert_eq!(
            super::decode_inspect_snapshot_target_row(&row, 7),
            Err(super::ClientInspectError::Failed(
                "inspect.malformed_carrier".to_owned()
            ))
        );
        row[67..75].copy_from_slice(&1_u64.to_be_bytes());
        assert_eq!(super::decode_inspect_snapshot_target_row(&row, 7), Ok(target));
        row.push(0x19);
        assert_eq!(
            super::decode_inspect_snapshot_target_row(&row, 7),
            Err(super::ClientInspectError::Failed(
                "inspect.malformed_carrier".to_owned()
            ))
        );
    }

    #[test]
    fn inspect_render_wrong_contract_identity_fails_closed_before_provider() {
        let context = super::ClientExecutionContext {
            pair: super::RevisionPair::new(
                orna_core::SourceRevisionId::from_bytes([0x21; 16]),
                orna_core::CatalogueRevisionId::from_bytes([0x22; 16]),
            ),
            function: super::FunctionId::from_bytes([0x23; 16]),
            function_revision: super::FunctionRevisionId::from_bytes([0x24; 16]),
            parent_invocation_id: super::InvocationId::from_bytes([0x25; 16]),
            observer_lineage: None,
        };
        let (active, _, _, _) = version_one_active(true);
        assert!(matches!(
            super::validate_inspect_render_contract(&active, context, "std.inspect.render@2", &[]),
            Err(super::ClientExecutionError::Inspect {
                source: super::ClientInspectError::Failed(code), ..
            }) if code == "inspect.malformed_carrier"
        ));
    }

    #[test]
    fn inspect_render_rejects_mixed_target_before_rendering() {
        let verified = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let active = empty_version_two_active(&verified);
        let pair = active.pair();
        let function = FunctionId::from_bytes([0x90; 16]);
        let function_revision = FunctionRevisionId::from_bytes([0x8f; 16]);
        let target = InvocationId::from_bytes([0x91; 16]);
        let epoch = 7_u64;
        let parameter = ParameterId::from_bytes([0x92; 16]);
        let context = super::ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: InvocationId::from_bytes([0x93; 16]),
            observer_lineage: None,
        };
        let encode_row = |payload: Vec<u8>| {
            let standard = active
                .catalogue_hash_context()
                .standard()
                .expect("standard catalogue");
            let registry = super::registered_opaque_codecs(standard).expect("opaque registry");
            let descriptor = super::TypeDescriptor::list(super::TypeDescriptor::named(
                super::BINARY_LARGE_OBJECT_TYPE_ID,
            ))
            .expect("row descriptor");
            let value = super::RuntimeValue::list(
                &active,
                descriptor,
                vec![super::RuntimeValue::Bytes(payload)],
            )
            .expect("row value");
            orna_protocol::encode_constructed_value(&active, &registry, &value).expect("encoded row")
        };

        let mut epoch_bytes = [0x96; 16];
        epoch_bytes[8..].copy_from_slice(&epoch.to_be_bytes());
        let mut snapshot_row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
        snapshot_row.extend_from_slice(&epoch_bytes);
        snapshot_row.extend_from_slice(&target.to_bytes());
        snapshot_row.extend_from_slice(&[0x94; 16]);
        snapshot_row.push(1);
        snapshot_row.extend_from_slice(&0_u64.to_be_bytes());
        snapshot_row.push(0);
        snapshot_row.push(0);
        let snapshot_bytes = super::InspectCarrierEnvelope::new(
            super::InspectCarrierKind::Snapshot,
            epoch,
            pair.source(),
            pair.catalogue(),
            vec![encode_row(snapshot_row)],
        )
        .expect("snapshot envelope")
        .encode()
        .expect("snapshot bytes");
        let snapshot = super::RuntimeValue::Opaque(
            super::OpaqueValue::new_inspect_carrier(
                &active,
                super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
                snapshot_bytes,
            )
            .expect("snapshot carrier"),
        );

        let mut mixed_row = vec![3, 0, 0, 0, 0, 0, 0, 0, 0];
        mixed_row.extend_from_slice(&epoch_bytes);
        mixed_row.extend_from_slice(&[0xaa; 16]);
        mixed_row.extend_from_slice(&[0x94; 16]);
        mixed_row.extend_from_slice(&pair.source().to_bytes());
        mixed_row.extend_from_slice(&pair.catalogue().to_bytes());
        mixed_row.push(1);
        mixed_row.push(0);
        let mixed_bytes = super::InspectCarrierEnvelope::new(
            super::InspectCarrierKind::Calls,
            epoch,
            pair.source(),
            pair.catalogue(),
            vec![encode_row(mixed_row)],
        )
        .expect("mixed projection envelope")
        .encode()
        .expect("mixed projection bytes");
        let mixed = super::RuntimeValue::Opaque(
            super::OpaqueValue::new_inspect_carrier(
                &active,
                super::SYS_INSPECT_CALLS_TYPE_ID,
                mixed_bytes,
            )
            .expect("mixed projection carrier"),
        );

        let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
            operation: orna_artifact::client_plan::InspectOperationNode::Projection {
                projection: InspectProjection::Calls,
                snapshot: Box::new(orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter,
                }),
            },
        };
        let provider_calls = Rc::new(Cell::new(0_u8));
        let provider_calls_for_executor = Rc::clone(&provider_calls);
        let mut executor = super::DeterministicClientResourceExecutor::new(
            |_: &super::ClientResourceRequest| Ok::<_, String>(super::RuntimeValue::Boolean(false)),
        )
        .with_inspect(move |_| {
            provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
            Ok(mixed.clone())
        });
        let mut executor_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
        let mut state = super::ClientStateStore::new();
        let mut locals = std::collections::HashMap::new();
        let arguments = [(parameter, snapshot)];

        let result = super::evaluate_expression_plan(
            &active,
            &expression,
            context,
            super::ObserverLineage::compatibility(context),
            ResolvedType::Value(super::SYS_INSPECT_CALLS_TYPE_ID),
            &arguments,
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
            &mut state,
            0,
            PrincipalId::from_bytes([0x95; 16]),
            &mut executor_slot,
            &mut locals,
        );
        assert!(matches!(
            result,
            Err(super::ClientExecutionError::Inspect {
                source: super::ClientInspectError::Failed(code),
                ..
            }) if code == "inspect.epoch_mismatch"
        ));
        assert_eq!(provider_calls.get(), 1, "the mixed carrier came from the custom provider");
    }

    #[test]
    fn inspect_render_wrong_ui_type_fails_closed() {
        let (active, _, _, _) = version_one_active(true);
        assert!(!super::inspect_render_ui_value_matches(
            &active,
            &super::RuntimeValue::Boolean(false),
        ));
    }

    #[test]
    fn inspector_invocation_references_require_sealed_type_and_nonzero_object() {
        let (active, _, _, _) = version_one_active(true);
        let expected = ResolvedType::Reference {
            target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
        };
        assert!(super::runtime_value_matches(
            &active,
            &RuntimeValue::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0x11; 16]),
            },
            expected,
        ));
        assert!(!super::runtime_value_matches(
            &active,
            &RuntimeValue::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0; 16]),
            },
            expected,
        ));
        assert!(!super::runtime_value_matches(
            &active,
            &RuntimeValue::Reference {
                target: TypeId::from_bytes([0x12; 16]),
                object: orna_core::ObjectId::from_bytes([0x11; 16]),
            },
            expected,
        ));
    }

    #[test]
    fn inspector_procedural_local_types_preserve_reference_and_carrier_shapes() {
        let (active, _, _, _) = version_one_active(true);
        assert_eq!(
            super::resolve_client_local_type(&active, super::SYS_INSPECT_INVOCATION_TYPE_ID),
            Some(ResolvedType::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
            }),
        );
        assert_eq!(
            super::resolve_client_local_type(&active, super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
            Some(ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID)),
        );
    }

    #[test]
    fn inspector_carriers_reject_malformed_and_stale_revision_envelopes() {
        let (active, _, pair, _) = version_one_active(true);
        let payload = super::InspectCarrierEnvelope::new(
            super::InspectCarrierKind::Snapshot,
            7,
            pair.source(),
            pair.catalogue(),
            vec![],
        )
        .unwrap()
        .encode()
        .unwrap();
        let value = RuntimeValue::Opaque(
            OpaqueValue::new_inspect_carrier(
                &active,
                super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
                payload.clone(),
            )
            .unwrap(),
        );
        assert!(super::runtime_value_matches(
            &active,
            &value,
            ResolvedType::Named(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
        ));
        assert!(!super::runtime_value_matches(
            &active,
            &RuntimeValue::Bytes(vec![0; 4]),
            ResolvedType::Named(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
        ));

        let stale_payload = super::InspectCarrierEnvelope::new(
            super::InspectCarrierKind::Snapshot,
            7,
            SourceRevisionId::from_bytes([0x91; 16]),
            pair.catalogue(),
            vec![],
        )
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(
            super::decode_inspect_carrier_payload(
                &active,
                &stale_payload,
                super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            )
            .unwrap_err(),
            super::ClientInspectError::Failed("inspect.epoch_mismatch".to_owned())
        );
    }

    #[test]
    fn inspector_request_exposes_distinct_client_epoch_anchor() {
        let context = super::ClientExecutionContext {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0xa1; 16]),
                CatalogueRevisionId::from_bytes([0xa2; 16]),
            ),
            function: FunctionId::from_bytes([0xa3; 16]),
            function_revision: FunctionRevisionId::from_bytes([0xa4; 16]),
            parent_invocation_id: InvocationId::from_bytes([0xa5; 16]),
            observer_lineage: None,
        };
        let request = super::ClientInspectRequest::new(
            context,
            super::ClientInspectOperation::Snapshot {
                target: RuntimeValue::Reference {
                    target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                    object: orna_core::ObjectId::from_bytes([0xa6; 16]),
                },
            },
        );
        assert_eq!(request.client_epoch_id(), context.client_epoch_id());
        assert_eq!(
            request.client_epoch_id().invocation_id(),
            context.parent_invocation_id()
        );
    }

    #[test]
    fn inspect_executor_forwards_typed_request_to_provider() {
        let context = super::ClientExecutionContext {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x61; 16]),
                CatalogueRevisionId::from_bytes([0x62; 16]),
            ),
            function: FunctionId::from_bytes([0x63; 16]),
            function_revision: FunctionRevisionId::from_bytes([0x64; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x65; 16]),
            observer_lineage: None,
        };
        let operation = super::ClientInspectOperation::Projection {
            projection: InspectProjection::Calls,
            snapshot: RuntimeValue::Boolean(true),
        };
        let request = super::ClientInspectRequest::new(context, operation);
        let mut executor = super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_inspect(move |request| {
            assert_eq!(request.context(), context);
            assert_eq!(request.operation().projection(), Some(InspectProjection::Calls));
            assert_eq!(request.operation().projection_carrier_tag(), Some(3));
            assert!(matches!(request.operation().snapshot(), Some(RuntimeValue::Boolean(true))));
            Ok(RuntimeValue::Boolean(false))
        });
        assert_eq!(
            executor.inspect(request),
            Ok(RuntimeValue::Boolean(false))
        );
    }

    #[test]
    fn inspect_expression_rejects_observer_lineage_targets_before_provider() {
        let (active, function, pair, function_revision) = version_one_active(true);
        let root = InvocationId::from_bytes([0x91; 16]);
        let parent = InvocationId::from_bytes([0x92; 16]);
        let current = InvocationId::from_bytes([0x93; 16]);
        let explicit_lineage = super::ObserverLineage::top_level(root)
            .with_parent_and_current(parent, current);
        let top_level = super::ObserverLineage::top_level(root);
        let nested = top_level.nested();
        let child = nested.nested();
        let cases = [
            ("root", explicit_lineage, root),
            ("parent", explicit_lineage, parent),
            ("current", explicit_lineage, current),
            ("recorded nested descendant", child, nested.current),
        ];

        for (label, lineage, target) in cases {
            let context = super::ClientExecutionContext {
                pair,
                function,
                function_revision,
                parent_invocation_id: lineage.parent,
                observer_lineage: None,
            };
            let parameter = ParameterId::from_bytes([0x94; 16]);
            let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
                operation: orna_artifact::client_plan::InspectOperationNode::snapshot(
                    orna_artifact::client_plan::ClientExpressionNode::ParameterRead { parameter },
                ),
            };
            let arguments = [(
                parameter,
                RuntimeValue::Reference {
                    target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                    object: orna_core::ObjectId::from_bytes(target.to_bytes()),
                },
            )];
            let grants = capability::LocalCapabilityGrantSet::new();
            let mut state = ClientStateStore::new();
            let provider_calls = Rc::new(Cell::new(0));
            let provider_calls_for_executor = Rc::clone(&provider_calls);
            let mut executor = super::DeterministicClientResourceExecutor::new(
                |_: &super::ClientResourceRequest| Ok::<_, String>(RuntimeValue::Boolean(false)),
            )
            .with_inspect(move |_| {
                provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
                Ok(RuntimeValue::Boolean(false))
            });
            let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
            let mut locals = std::collections::HashMap::new();

            let result = super::evaluate_expression_plan(
                &active,
                &expression,
                context,
                lineage,
                ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
                &arguments,
                &[],
                &grants,
                &mut state,
                0,
                PrincipalId::from_bytes([0x95; 16]),
                &mut executor_slot,
                &mut locals,
            );

            assert_eq!(
                result,
                Err(super::ClientExecutionError::Inspect {
                    context,
                    source: super::ClientInspectError::Failed("inspect.recursion".to_owned()),
                }),
                "{label} target must be rejected by the expression evaluator",
            );
            assert_eq!(
                provider_calls.get(),
                0,
                "{label} target must not invoke the Inspector provider",
            );
        }
    }

    #[test]
    fn inspect_snapshot_options_reject_before_evaluating_target_or_options() {
        let (active, function, pair, function_revision) = version_one_active(true);
        let context = super::ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: super::InvocationId::from_bytes([0xa7; 16]),
            observer_lineage: None,
        };
        let target = super::ParameterId::from_bytes([0xa8; 16]);
        let options = super::ParameterId::from_bytes([0xa9; 16]);
        let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
            operation: orna_artifact::client_plan::InspectOperationNode::Snapshot {
                target: Box::new(orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: target,
                }),
                options: Some(Box::new(
                    orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                        parameter: options,
                    },
                )),
            },
        };
        let mut state = super::ClientStateStore::new();
        let mut locals = std::collections::HashMap::new();
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

        let result = super::evaluate_expression_plan(
            &active,
            &expression,
            context,
            super::ObserverLineage::compatibility(context),
            ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
            &[],
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
            &mut state,
            0,
            PrincipalId::from_bytes([0xaa; 16]),
            &mut executor_slot,
            &mut locals,
        );

        assert_eq!(
            result,
            Err(super::ClientExecutionError::Inspect {
                context,
                source: super::ClientInspectError::Failed("inspect.projection_failed".to_owned()),
            }),
            "unsupported snapshot options must be rejected before either expression is evaluated",
        );
    }

    fn authorise(pair: RevisionPair, function: FunctionId) -> AuthorisedInvocation {
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let snapshot = SecuritySnapshot::new(
            pair,
            vec![function],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(principal, function)],
        )
        .expect("test security snapshot should validate");
        let session = snapshot
            .bind_authenticated_session(principal, vec![])
            .expect("test security session should bind");
        let ExecuteDecision::Allowed(authorisation) =
            snapshot.authorise_execute(&session, InvocationTarget::new(function, pair))
        else {
            panic!("test security grant should allow the function");
        };
        authorisation
    }

    fn authorise_with_role_context(
        pair: RevisionPair,
        function: FunctionId,
    ) -> (AuthorisedInvocation, AuthorisedInvocation) {
        let session_principal = PrincipalId::from_bytes([0x7a; 16]);
        let role = PrincipalId::from_bytes([0x7b; 16]);
        let principals = vec![
            Principal::new(session_principal, PrincipalKind::User, PrincipalStatus::Active),
            Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
        ];
        let direct_snapshot = SecuritySnapshot::new(
            pair,
            vec![function],
            principals.clone(),
            vec![RoleMembership::new(role, session_principal)],
            vec![ExecuteGrant::new(session_principal, function)],
        )
        .expect("direct security snapshot should validate");
        let role_snapshot = SecuritySnapshot::new(
            pair,
            vec![function],
            principals,
            vec![RoleMembership::new(role, session_principal)],
            vec![ExecuteGrant::new(role, function)],
        )
        .expect("role security snapshot should validate");
        let direct_session = direct_snapshot
            .bind_authenticated_session(session_principal, vec![])
            .expect("direct session should bind");
        let role_session = role_snapshot
            .bind_authenticated_session(session_principal, vec![role])
            .expect("role session should bind");
        let ExecuteDecision::Allowed(direct) = direct_snapshot.authorise_execute(
            &direct_session,
            InvocationTarget::new(function, pair),
        ) else {
            panic!("direct grant should allow the function");
        };
        let ExecuteDecision::Allowed(role_authorisation) = role_snapshot.authorise_execute(
            &role_session,
            InvocationTarget::new(function, pair),
        ) else {
            panic!("role grant should allow the function");
        };
        (direct, role_authorisation)
    }

    fn evaluate_client_function(
        active: &ActiveDatabaseRevision,
        function: FunctionId,
    ) -> Result<super::ClientExecutionResult, super::ClientExecutionError> {
        super::evaluate_client_function(active, &authorise(active.pair(), function))
    }

    #[test]
    fn evaluates_version_one_client_constants() {
        for value in [true, false] {
            let (active, function, pair, function_revision) = version_one_active(value);

            let result = evaluate_client_function(&active, function).unwrap();

            assert_eq!(result.context().pair(), pair);
            assert_eq!(result.context().function(), function);
            assert_eq!(result.context().function_revision(), function_revision);
            assert_eq!(result.value(), &RuntimeValue::Boolean(value));
        }
    }

    #[test]
    fn resource_request_rejects_nul_invocation_context_before_loading() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7b; 16]);
        let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            Sha256Digest::from_bytes([0x23; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

        for (profile, instance) in [
            ("profile\0invalid", "instance"),
            ("profile", "instance\0invalid"),
        ] {
            let context = super::ClientResourceInvocationContext::new(
                InvocationId::from_bytes([0x24; 16]),
                CallSiteId::from_bytes([0x25; 16]),
                profile.to_owned(),
                instance.to_owned(),
            );
            assert!(matches!(
                resource.begin_request_with_context(&active, context, Vec::new()),
                Err(super::ClientResourceError::InvalidInvocationContext)
            ));
            assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
            assert_eq!(resource.generation().value(), 0);
        }
    }

    #[test]
    fn client_resource_lifecycle_rejects_stale_and_invalid_results() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            Sha256Digest::from_bytes([0x11; 32]),
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 0);

        let first = resource.begin_loading().unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(first.value(), 1);
        assert_eq!(
            resource.publish_ready(
                &active,
                super::ClientResourceGeneration(0),
                RuntimeValue::Boolean(true),
            ),
            Err(super::ClientResourceError::StaleGeneration {
                expected: first,
                actual: super::ClientResourceGeneration(0),
            }),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);

        resource
            .publish_ready(&active, first, RuntimeValue::Boolean(true))
            .unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

        let second = resource.begin_loading().unwrap();
        assert_eq!(resource.value(), None);
        assert_eq!(
            resource.publish_failure(second, String::new()),
            Err(super::ClientResourceError::InvalidFailureCode),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        resource
            .publish_failure(second, "network.timeout".to_owned())
            .unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Failed);
        assert_eq!(
            resource.failure().map(super::ClientResourceFailure::code),
            Some("network.timeout"),
        );

        let third = resource.begin_loading().unwrap();
        resource.cancel(third).unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
        assert_eq!(resource.value(), None);
        assert_eq!(resource.failure(), None);
        assert_eq!(
            resource.publish_failure(third, "late".to_owned()),
            Err(super::ClientResourceError::InvalidTransition {
                status: super::ClientResourceStatus::Cancelled,
            }),
        );

        resource.invalidate().unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 4);
    }

    #[test]
    fn client_action_argument_error_preserves_display_and_equality() {
        let resource_error = super::ClientResourceError::DuplicateArgument {
            parameter: ParameterId::from_bytes([0x7b; 16]),
        };
        let action_error = super::ClientActionError::Arguments(Box::new(resource_error.clone()));

        assert_eq!(action_error.to_string(), resource_error.to_string());
        assert_eq!(
            action_error,
            super::ClientActionError::Arguments(Box::new(resource_error)),
        );
    }

    #[test]
    fn client_resource_rejects_completion_with_mismatched_request_key() {
        let (active, function, pair, _) = version_one_active(true);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x11; 32]),
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let wrong_key = super::ClientResourceKey::new(
            key.target(),
            key.principal(),
            Sha256Digest::from_bytes([0xaa; 32]),
            key.invalidation_token(),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();
        let request_id = resource.request_id().unwrap();
        let completion = super::ClientResourceCompletion::Ready {
            request_id,
            key: wrong_key,
            generation,
            value: RuntimeValue::Boolean(true),
        };
        let before = resource.clone();

        let error = resource
            .apply_completion(&active, completion)
            .expect_err("the completion key must be rejected");
        assert_eq!(
            error,
            super::ClientResourceError::RequestKeyMismatch {
                expected: Box::new(key),
                actual: Box::new(wrong_key),
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "CLIENT resource completion uses key {:?}, expected {:?}",
                wrong_key, key,
            ),
        );
        assert_eq!(resource, before);
    }

    #[test]
    fn client_resource_rejects_completion_with_mismatched_request_id() {
        let (active, function, pair, _) = version_one_active(true);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, Vec::new()).unwrap();
        let completion = super::ClientResourceCompletion::Ready {
            request_id: InvocationId::from_bytes([0xff; 16]),
            key,
            generation: request.generation(),
            value: RuntimeValue::Boolean(true),
        };
        let before = resource.clone();

        assert_eq!(
            resource.apply_completion(&active, completion),
            Err(super::ClientResourceError::RequestIdMismatch {
                expected: request.request_id(),
                actual: InvocationId::from_bytes([0xff; 16]),
            })
        );
        assert_eq!(resource, before);
    }

    #[test]
    fn client_resource_ready_value_must_match_declared_type() {
        let (active, function, pair, _) = version_one_active(true);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x31; 32]),
            Sha256Digest::from_bytes([0x32; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();

        assert_eq!(
            resource.publish_ready(&active, generation, RuntimeValue::Integer(4)),
            Err(super::ClientResourceError::TypeMismatch),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
    }

    #[test]
    fn client_resource_rejects_expected_type_that_differs_from_target_declaration() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x33; 32]),
            Sha256Digest::from_bytes([0x34; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Integer));
        let generation = resource.begin_loading().unwrap();

        assert_eq!(
            resource.publish_ready(&active, generation, RuntimeValue::Integer(7)),
            Err(super::ClientResourceError::TypeMismatch),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
    }

    #[test]
    fn client_resource_rejects_completion_from_a_different_revision() {
        let (active, function, _, _) = version_one_active(true);
        let resource_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x7b; 16]),
            CatalogueRevisionId::from_bytes([0x7c; 16]),
        );
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, resource_pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x41; 32]),
            Sha256Digest::from_bytes([0x42; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();

        assert_eq!(
            resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
            Err(super::ClientResourceError::RevisionMismatch {
                expected: resource_pair,
                actual: active.pair(),
            }),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
    }

    #[test]
    fn client_resource_executor_validates_arguments_and_applies_completion() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            vec![
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x02; 16]),
                    "count",
                    0,
                    ResolvedType::Scalar(StandardScalar::Integer),
                    None,
                ),
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x01; 16]),
                    "enabled",
                    1,
                    ResolvedType::Scalar(StandardScalar::Boolean),
                    None,
                ),
            ],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let first = FunctionArgument::new(
            ParameterId::from_bytes([0x02; 16]),
            RuntimeValue::Integer(7),
        )
        .unwrap();
        let second = FunctionArgument::new(
            ParameterId::from_bytes([0x01; 16]),
            RuntimeValue::Boolean(true),
        )
        .unwrap();
        let arguments = vec![first.clone(), second.clone()];
        let digest =
            super::ClientResourceKey::canonical_arguments_digest(&active, &arguments).unwrap();
        assert_eq!(
            digest,
            super::ClientResourceKey::canonical_arguments_digest(
                &active,
                &[second.clone(), first.clone()],
            )
            .unwrap(),
        );
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let mut executor = super::DeterministicClientResourceExecutor::new(
            |request: &super::ClientResourceRequest| {
                assert_eq!(request.arguments()[0].parameter(), second.parameter());
                assert_eq!(request.arguments()[1].parameter(), first.parameter());
                Ok(RuntimeValue::Boolean(true))
            },
        );

        let request = resource
            .begin_request(&active, vec![first.clone(), second.clone()])
            .unwrap();
        assert_eq!(request.arguments()[0].parameter(), second.parameter());
        let first_request_id = request.request_id();
        let completion = super::ClientResourceExecutor::execute(&mut executor, request);
        resource.apply_completion(&active, completion).unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

        let second_request = resource
            .begin_request(&active, vec![first, second])
            .unwrap();
        assert_ne!(second_request.request_id(), first_request_id);
        let failed = second_request.failed("resource.denied".to_owned());
        resource.apply_completion(&active, failed).unwrap();
        assert_eq!(resource.status(), super::ClientResourceStatus::Failed);
        assert_eq!(
            resource.failure().map(super::ClientResourceFailure::code),
            Some("resource.denied"),
        );
    }

    #[test]
    fn client_resource_pending_completion_preserves_loading_until_resume() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, Vec::new()).unwrap();
        let generation = request.generation();
        let request_id = request.request_id();

        resource
            .apply_completion(&active, request.pending())
            .expect("pending completion should retain the active generation");
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
        assert_eq!(resource.failure(), None);

        resource
            .apply_completion(
                &active,
                super::ClientResourceCompletion::Ready {
                    request_id,
                    key,
                    generation,
                    value: RuntimeValue::Boolean(true),
                },
            )
            .expect("the matching completion should resume the resource");
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
    }

    #[test]
    fn resource_executor_poll_surfaces_pending_completion_without_affecting_immediate_executor() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );

        let mut pending_resource = super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let pending_request = pending_resource.begin_request(&active, vec![]).unwrap();
        let pending_request_id = pending_request.request_id();
        let expected_pending = pending_request.clone().pending();
        let mut polling = PollingTestExecutor::default();
        assert_eq!(polling.execute(pending_request), expected_pending);
        assert_eq!(polling.poll(), Some(ClientResourceCompletion::Ready { request_id: pending_request_id, key, generation: pending_resource.generation(), value: RuntimeValue::Boolean(true) }));
        assert_eq!(polling.poll(), None);

        let mut immediate_resource = super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let immediate_request = immediate_resource.begin_request(&active, vec![]).unwrap();
        let mut immediate = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| Ok(RuntimeValue::Boolean(true)));
        assert!(matches!(immediate.execute(immediate_request), ClientResourceCompletion::Ready { .. }));
        assert_eq!(immediate.poll(), None);
    }

    #[test]
    fn client_resource_cancelled_completion_terminates_current_generation() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, Vec::new()).unwrap();

        resource
            .apply_completion(&active, request.cancelled())
            .expect("matching cancellation should terminate the active generation");

        assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
        assert_eq!(resource.value(), None);
        assert_eq!(resource.failure(), None);
    }


    #[test]
    fn client_stream_request_preserves_batch_order_and_returns_terminal_option() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    assert_eq!(request.kind(), ResourceKind::Stream);

    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.stream_completed())
        .unwrap();

    let first = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(first, &[true, false]);
    let second = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(second, &[false, true]);
    let terminal = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_terminal(terminal);
}

#[test]
fn client_stream_rejects_scalar_ready_completion() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    assert_eq!(
        resource.publish_ready(&active, request.generation(), RuntimeValue::Boolean(true)),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
    assert!(!resource.stream_complete());
}

#[test]
fn client_stream_rejects_oversized_batches_and_totals_before_queueing() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    let oversized_batch = vec![
        RuntimeValue::Boolean(true);
        super::MAX_RESOURCE_BATCH_ITEMS + 1
    ];
    assert_eq!(
        resource.apply_completion(
            &active,
            request.clone().stream_values(oversized_batch),
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(resource.stream_total_items, 0);

    resource.stream_total_items = super::MAX_RESOURCE_TOTAL_ITEMS;
    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(true)]),
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
}

#[test]
fn client_stream_queue_overflow_preserves_existing_batches() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x24; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let batch = vec![RuntimeValue::Boolean(true); super::MAX_RESOURCE_BATCH_ITEMS];
    resource
        .apply_completion(&active, request.clone().stream_values(batch))
        .unwrap();
    let before = resource.clone();

    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource, before);
}

#[test]
fn client_stream_queue_dequeue_releases_capacity() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x25; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    for _ in 0..super::MAX_RESOURCE_BATCH_ITEMS {
        resource
            .apply_completion(
                &active,
                request.clone().stream_values(vec![RuntimeValue::Boolean(true)]),
            )
            .unwrap();
    }
    assert_eq!(resource.stream_queued_items, super::MAX_RESOURCE_QUEUED_ITEMS);
    resource.take_stream_value(&active).unwrap().unwrap();
    assert_eq!(resource.stream_queued_items, super::MAX_RESOURCE_QUEUED_ITEMS - 1);

    resource
        .apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    assert_eq!(resource.stream_queued_items, super::MAX_RESOURCE_QUEUED_ITEMS);
}

#[test]
fn client_stream_failure_drains_queued_batches_before_evaluator_reports_failure() {
    let (active, function, pair, function_revision) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.failed("stream.failed".to_owned()))
        .unwrap();

    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
        observer_lineage: None,
    };
    let first = super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(first, &[true]);
    let second = super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(second, &[false]);
    assert!(matches!(
        super::read_stream_resource_value(&active, &mut resource, context),
        Err(super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Failed(code),
            ..
        }) if code == "stream.failed"
    ));
}

#[test]
fn client_stream_cancellation_clears_batches_and_rejects_stale_completions() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource = ClientResource::new_stream(
        key,
        ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
    );
    let first = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let second = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            second
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, second.clone().cancelled())
        .unwrap();

    assert_eq!(
        resource.take_stream_value(&active),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        })
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.failure(), None);
    assert!(matches!(
        resource.apply_completion(
            &active,
            first.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::ClientResourceError::StaleGeneration { .. })
    ));
    assert_eq!(
        resource.apply_completion(&active, second.stream_completed()),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        })
    );
}


    fn assert_boolean_stream_batch(value: RuntimeValue, expected: &[bool]) {
        let RuntimeValue::Constructed(option) = value else {
            panic!("stream value must be a constructed OPTION");
        };
        let orna_core::value::ConstructedValueKind::Option(Some(list)) = option.kind() else {
            panic!("stream value must contain a present LIST");
        };
        let RuntimeValue::Constructed(list) = list else {
            panic!("stream OPTION must contain a constructed LIST");
        };
        let orna_core::value::ConstructedValueKind::List(values) = list.kind() else {
            panic!("stream OPTION must contain a LIST");
        };
        let expected = expected
            .iter()
            .copied()
            .map(RuntimeValue::Boolean)
            .collect::<Vec<_>>();
        assert_eq!(values, expected.as_slice());
    }

    fn assert_boolean_stream_terminal(value: RuntimeValue) {
        let RuntimeValue::Constructed(option) = value else {
            panic!("stream terminal must be a constructed OPTION");
        };
        assert_eq!(
            option.kind(),
            orna_core::value::ConstructedValueKind::Option(None)
        );
    }

    #[test]
    fn stream_descriptor_rejects_unsupported_scalar_items() {
        for scalar in [
            StandardScalar::Decimal,
            StandardScalar::Uuid,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
            StandardScalar::Void,
        ] {
            assert!(super::stream_item_descriptor(ResolvedType::Scalar(scalar)).is_none());
        }
    }

    #[test]
    fn stream_await_expression_and_procedural_local_return_option_list_values() {
        let (active, target, pair, target_revision) = version_two_server_stream_active();
        let item_type = ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID);
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            ResourceKind::Stream,
            target,
            pair,
            CallSiteId::from_bytes([0x91; 16]),
            Vec::new(),
            orna_standard::BOOLEAN_TYPE_ID,
        );
        let expression = orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                operation: operation.clone(),
            }),
        };
        let context = super::ClientExecutionContext {
            pair,
            function: target,
            function_revision: target_revision,
            parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
            observer_lineage: None,
        };
        let grants = capability::LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
        let mut executor = StreamBatchTestExecutor { value: true };
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
        let mut locals = std::collections::HashMap::new();
        let value = super::evaluate_expression_plan(
            &active,
            &expression,
            context,
            super::ObserverLineage::compatibility(context),
            item_type,
            &[],
            &[],
            &grants,
            &mut state,
            0,
            PrincipalId::from_bytes([0x93; 16]),
            &mut executor_slot,
            &mut locals,
        )
        .expect("stream AWAIT must be checked against its OPTION<LIST<T>> result");
        assert_boolean_stream_batch(value, &[true]);

        let local = LocalId::from_bytes([0x94; 16]);
        let procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(
                local,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Resource(ResourceKind::Stream),
            )],
            vec![
                orna_artifact::client_plan::ClientStatement::let_(
                    local,
                    orna_artifact::client_plan::ClientExpressionNode::Resource { operation: operation.clone() },
                ),
                orna_artifact::client_plan::ClientStatement::assignment(
                    local,
                    orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
                ),
            ],
            orna_artifact::client_plan::ClientExpressionNode::Await {
                expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::LocalRead { local }),
            },
        );
        let mut state = ClientStateStore::new();
        let mut executor = StreamBatchTestExecutor { value: false };
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
        let mut locals = std::collections::HashMap::new();
        let value = super::evaluate_procedural_plan(
            &active,
            &procedural,
            context,
            super::ObserverLineage::compatibility(context),
            item_type,
            false,
            &[],
            &[],
            &grants,
            &mut state,
            0,
            PrincipalId::from_bytes([0x93; 16]),
            &mut executor_slot,
            &mut locals,
        )
        .expect("procedural stream AWAIT must preserve the outer result shape");
        assert_boolean_stream_batch(value, &[false]);

        let value_local = LocalId::from_bytes([0x95; 16]);
        let copy_local = LocalId::from_bytes([0x96; 16]);
        let value_procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![
                orna_artifact::client_plan::ClientLocal::new(
                    value_local,
                    orna_standard::BOOLEAN_TYPE_ID,
                    orna_artifact::client_plan::ClientLocalKind::Value,
                ),
                orna_artifact::client_plan::ClientLocal::new(
                    copy_local,
                    orna_standard::BOOLEAN_TYPE_ID,
                    orna_artifact::client_plan::ClientLocalKind::Value,
                ),
            ],
            vec![
                orna_artifact::client_plan::ClientStatement::let_(
                    value_local,
                    orna_artifact::client_plan::ClientExpressionNode::Await {
                        expression: Box::new(
                            orna_artifact::client_plan::ClientExpressionNode::Resource {
                                operation: operation.clone(),
                            },
                        ),
                    },
                ),
                orna_artifact::client_plan::ClientStatement::let_(
                    copy_local,
                    orna_artifact::client_plan::ClientExpressionNode::Boolean { value: false },
                ),
                orna_artifact::client_plan::ClientStatement::assignment(
                    copy_local,
                    orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: value_local },
                ),
            ],
            orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: copy_local },
        );
        let mut state = ClientStateStore::new();
        let mut executor = StreamBatchTestExecutor { value: true };
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
        let mut locals = std::collections::HashMap::new();
        let value = super::evaluate_procedural_plan(
            &active,
            &value_procedural,
            context,
            super::ObserverLineage::compatibility(context),
            item_type,
            false,
            &[],
            &[],
            &grants,
            &mut state,
            0,
            PrincipalId::from_bytes([0x93; 16]),
            &mut executor_slot,
            &mut locals,
        )
        .expect("a value local containing stream AWAIT must preserve its outer result shape");
        assert_boolean_stream_batch(value, &[true]);
    }

    #[test]
    fn client_resource_ready_completion_wins_over_late_cancellation() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, Vec::new()).unwrap();
        let generation = request.generation();
        let late_cancellation = request.clone().cancelled();

        resource
            .apply_completion(&active, request.ready(RuntimeValue::Boolean(true)))
            .expect("the accepted completion should make the resource ready");
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

        assert_eq!(
            resource.cancel(generation),
            Err(super::ClientResourceError::InvalidTransition {
                status: super::ClientResourceStatus::Ready,
            }),
        );
        assert_eq!(
            resource.apply_completion(&active, late_cancellation),
            Err(super::ClientResourceError::InvalidTransition {
                status: super::ClientResourceStatus::Ready,
            }),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
    }


    #[test]
    fn client_resource_executor_rejects_digest_duplicates_stale_and_cancelled() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes([0x01; 16]),
                "enabled",
                0,
                ResolvedType::Scalar(StandardScalar::Boolean),
                None,
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0x01; 16]),
            RuntimeValue::Boolean(true),
        )
        .unwrap();
        let digest = super::ClientResourceKey::canonical_arguments_digest(
            &active,
            std::slice::from_ref(&argument),
        )
        .unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource =
            super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

        let wrong_key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            key.principal(),
            Sha256Digest::from_bytes([0xaa; 32]),
            key.invalidation_token(),
        );
        let mut wrong_resource =
            super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
        assert!(matches!(
            wrong_resource.begin_request(&active, vec![argument.clone()]),
            Err(super::ClientResourceError::ArgumentDigestMismatch { .. }),
        ));
        assert_eq!(wrong_resource.status(), super::ClientResourceStatus::Idle);

        assert_eq!(
            resource.begin_request(&active, vec![argument.clone(), argument.clone()]),
            Err(super::ClientResourceError::DuplicateArgument {
                parameter: argument.parameter(),
            }),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);

        let first = resource
            .begin_request(&active, vec![argument.clone()])
            .unwrap();
        let second = resource.begin_request(&active, vec![argument]).unwrap();
        let first_completion = first.ready(RuntimeValue::Boolean(false));
        assert!(matches!(
            resource.apply_completion(&active, first_completion),
            Err(super::ClientResourceError::StaleGeneration { .. }),
        ));
        let second_generation = second.generation();
        resource.cancel(second_generation).unwrap();
        assert!(matches!(
            resource.apply_completion(&active, second.ready(RuntimeValue::Boolean(true))),
            Err(super::ClientResourceError::InvalidTransition {
                status: super::ClientResourceStatus::Cancelled,
            }),
        ));
        assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
        assert_eq!(resource.value(), None);
    }

    #[test]
    fn client_resource_accepts_supported_scalar_runtime_values() {
        let cases = [
            (
                ResolvedType::Scalar(StandardScalar::BigInt),
                RuntimeValue::BigInt(42),
            ),
            (
                ResolvedType::Scalar(StandardScalar::Float),
                RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
            ),
            (
                ResolvedType::Scalar(StandardScalar::BinaryLargeObject),
                RuntimeValue::Bytes(vec![0x01, 0x02]),
            ),
        ];

        for (index, (expected, value)) in cases.into_iter().enumerate() {
            let (active, function, pair, _) = version_one_active_with_shape(
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(expected),
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
            );
            let key = super::ClientResourceKey::new(
                InvocationTarget::new(function, pair),
                PrincipalId::from_bytes([0x7a; 16]),
                Sha256Digest::from_bytes([0x50 + index as u8; 32]),
                Sha256Digest::from_bytes([0x60 + index as u8; 32]),
            );
            let mut resource = super::ClientResource::new(key, expected);
            let generation = resource.begin_loading().unwrap();
            resource
                .publish_ready(&active, generation, value)
                .expect("supported scalar value should publish");
            assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        }
    }

    #[test]
    fn client_resource_accepts_standard_value_contracts() {
        let cases = [
            (orna_standard::BIGINT_TYPE_ID, RuntimeValue::BigInt(42)),
            (
                orna_standard::FLOAT_TYPE_ID,
                RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
            ),
            (
                orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
                RuntimeValue::Bytes(vec![0x01, 0x02]),
            ),
        ];

        for (index, (type_id, value)) in cases.into_iter().enumerate() {
            let (active, function, pair, _) = version_two_value_active(type_id, type_id);
            let key = super::ClientResourceKey::new(
                InvocationTarget::new(function, pair),
                PrincipalId::from_bytes([0x7a; 16]),
                Sha256Digest::from_bytes([0x90 + index as u8; 32]),
                Sha256Digest::from_bytes([0xa0 + index as u8; 32]),
            );
            let mut resource = super::ClientResource::new(key, ResolvedType::Value(type_id));
            let generation = resource.begin_loading().unwrap();

            resource
                .publish_ready(&active, generation, value)
                .expect("standard value contract should publish");
            assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
        }
    }

    #[test]
    fn client_resource_requires_the_full_verified_standard_target_pin() {
        let (active, function, pair, _) = version_two_value_active(
            orna_standard::BOOLEAN_TYPE_ID,
            orna_standard::BOOLEAN_TYPE_ID,
        );
        let wrong_target = InvocationTarget::verified_standard(
            function,
            pair,
            orna_core::StandardLibraryRevisionId::from_bytes([0xee; 16]),
            FunctionRevisionId::from_bytes([0xef; 16]),
        );
        let wrong_key = super::ClientResourceKey::new(
            wrong_target,
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0xb1; 32]),
            Sha256Digest::from_bytes([0xb2; 32]),
        );
        let mut resource =
            super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();

        assert_eq!(
            resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
            Err(super::ClientResourceError::TargetMismatch {
                expected: wrong_target,
            }),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    }

    #[test]
    fn client_resource_resolves_compiled_verified_standard_server_target() {
        let (active, _, pair, _) = version_two_client_call_active();
        let argument = FunctionArgument::new(
            orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
            RuntimeValue::Integer(42),
        )
        .unwrap();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("version-two fixture pins the verified standard snapshot");
        let target = InvocationTarget::verified_standard(
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            standard.revision(),
            orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        );
        let digest = ClientResourceKey::canonical_arguments_digest(
            &active,
            std::slice::from_ref(&argument),
        )
        .unwrap();
        let key = ClientResourceKey::new(
            target,
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            active.catalogue_hash(),
        );
        let mut resource = ClientResource::new(
            key,
            ResolvedType::Scalar(StandardScalar::Integer),
        );

        let request = resource
            .begin_request(&active, vec![argument])
            .expect("the pinned standard resource target should validate");

        assert_eq!(request.target(), target);
        assert_eq!(request.expected_type(), ResolvedType::Scalar(StandardScalar::Integer));
    }

    #[test]
    fn client_resource_validates_named_and_reference_catalogue_membership() {
        let (active, function, pair, _) = version_one_active(true);
        let unknown = TypeId::from_bytes([0xee; 16]);
        let cases = [
            (
                ResolvedType::Named(unknown),
                RuntimeValue::null(ResolvedType::Named(unknown)).unwrap(),
            ),
            (
                ResolvedType::Reference { target: unknown },
                RuntimeValue::Reference {
                    target: unknown,
                    object: orna_core::ObjectId::from_bytes([0xef; 16]),
                },
            ),
        ];

        for (index, (expected, value)) in cases.into_iter().enumerate() {
            let key = super::ClientResourceKey::new(
                InvocationTarget::new(function, pair),
                PrincipalId::from_bytes([0x7a; 16]),
                Sha256Digest::from_bytes([0x70 + index as u8; 32]),
                Sha256Digest::from_bytes([0x80 + index as u8; 32]),
            );
            let mut resource = super::ClientResource::new(key, expected);
            let generation = resource.begin_loading().unwrap();
            assert_eq!(
                resource.publish_ready(&active, generation, value),
                Err(super::ClientResourceError::TypeMismatch),
            );
            assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        }
    }

    #[test]
    fn client_resource_cache_keeps_key_and_transitions() {
        let (active, function, pair, _) = version_one_active(true);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0xc1; 32]),
            Sha256Digest::from_bytes([0xc2; 32]),
        );
        let mut state = super::ClientStateStore::new();

        assert!(state.resource(key).is_none());
        {
            let resource =
                state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean));
            let generation = resource.begin_loading().unwrap();
            resource
                .publish_ready(&active, generation, RuntimeValue::Boolean(true))
                .unwrap();
        }
        assert_eq!(
            state.resource(key).and_then(super::ClientResource::value),
            Some(&RuntimeValue::Boolean(true)),
        );

        // A duplicate lookup returns the existing resource and keeps its
        // original type and published value.
        let resource =
            state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer));
        assert_eq!(
            resource.expected_type(),
            ResolvedType::Scalar(StandardScalar::Boolean),
        );
        assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

        let first = resource.begin_loading().unwrap();
        let second = resource.begin_loading().unwrap();
        assert_eq!(
            state
                .resource_mut(key)
                .expect("resource remains in the cache")
                .publish_failure(first, "stale".to_owned()),
            Err(super::ClientResourceError::StaleGeneration {
                expected: second,
                actual: first,
            }),
        );
        assert_eq!(
            state.resource(key).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Loading),
        );

        state
            .resource_mut(key)
            .expect("resource remains in the cache")
            .cancel(second)
            .unwrap();
        assert_eq!(
            state.resource(key).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Cancelled),
        );
        let generation_before_invalidation = state
            .resource(key)
            .expect("cancelled resource remains in the cache")
            .generation();
        assert_eq!(state.invalidate_resource(key), Ok(true));
        let resource = state
            .resource(key)
            .expect("invalidated resource remains cached");
        assert_eq!(resource.key(), key);
        assert_eq!(
            resource.expected_type(),
            ResolvedType::Scalar(StandardScalar::Boolean),
        );
        assert_eq!(
            resource.generation().value(),
            generation_before_invalidation.value() + 1,
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
        assert_eq!(resource.value(), None);
        assert_eq!(resource.failure(), None);
        assert_eq!(
            state.invalidate_resource(super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7b; 16]),
            Sha256Digest::from_bytes([0xc1; 32]),
            Sha256Digest::from_bytes([0xc2; 32]),
            )),
            Ok(false)
        );
    }

    #[test]
    fn resource_invalidation_cancels_owned_request_and_rejects_late_completion() {
        let (active, _, pair, _) = version_two_client_call_active();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("version-two fixture pins the verified standard snapshot");
        let target = InvocationTarget::verified_standard(
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            standard.revision(),
            orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        );
        let argument = FunctionArgument::new(
            orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
            RuntimeValue::Integer(42),
        )
        .unwrap();
        let digest = ClientResourceKey::canonical_arguments_digest(
            &active,
            std::slice::from_ref(&argument),
        )
        .unwrap();
        let key = ClientResourceKey::new(
            target,
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0xc2; 32]),
        );
        let mut state = ClientStateStore::new();
        let request = state
            .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
            .begin_request(&active, vec![argument])
            .unwrap();
        let late_completion = request.clone().ready(RuntimeValue::Integer(42));
        let mut executor = RecordingActionExecutor::new(None);

        assert_eq!(
            state.invalidate_resource_with_executor(key, &mut executor),
            Ok(true),
        );
        assert_eq!(executor.cancelled, vec![request.clone()]);
        assert_eq!(
            state
                .resource(key)
                .expect("invalidated resource remains cached")
                .status(),
            super::ClientResourceStatus::Idle,
        );
        assert_eq!(
            state
                .resource_mut(key)
                .expect("invalidated resource remains cached")
                .apply_completion(&active, late_completion),
            Err(super::ClientResourceError::StaleGeneration {
                expected: super::ClientResourceGeneration(2),
                actual: request.generation(),
            }),
        );
    }

    #[test]
    fn replacing_complete_resource_key_cancels_previous_generation() {
        let (active, _, pair, _) = version_two_client_call_active();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("version-two fixture pins the verified standard snapshot");
        let target = InvocationTarget::verified_standard(
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            standard.revision(),
            orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        );
        let argument = FunctionArgument::new(
            orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
            RuntimeValue::Integer(42),
        )
        .unwrap();
        let digest = ClientResourceKey::canonical_arguments_digest(
            &active,
            std::slice::from_ref(&argument),
        )
        .unwrap();
        let key_a = ClientResourceKey::new(
            target,
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0xd2; 32]),
        );
        let key_b = ClientResourceKey::new(
            target,
            PrincipalId::from_bytes([0x7a; 16]),
            digest,
            Sha256Digest::from_bytes([0xd3; 32]),
        );
        let mut state = ClientStateStore::new();
        let request = state
            .get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Integer))
            .begin_request(&active, vec![argument])
            .unwrap();
        let mut executor = RecordingActionExecutor::new(None);

        state
            .get_or_create_resource_with_executor(
                key_b,
                ResolvedType::Scalar(StandardScalar::Integer),
                &mut executor,
            )
            .unwrap();

        assert_eq!(executor.cancelled, vec![request.clone()]);
        assert_eq!(
            state.resource(key_a).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Idle),
        );
        assert!(matches!(
            state
                .resource_mut(key_a)
                .expect("replaced resource remains cached")
                .apply_completion(&active, request.ready(RuntimeValue::Integer(42))),
            Err(super::ClientResourceError::StaleGeneration { .. }),
        ));
        assert_eq!(
            state.resource(key_b).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Idle),
        );
    }

    #[test]
    fn client_resource_cache_keeps_distinct_complete_keys_independent() {
        let (_, function, pair, _) = version_one_active(true);
        let target = InvocationTarget::new(function, pair);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let key_a = super::ClientResourceKey::new(
            target,
            principal,
            Sha256Digest::from_bytes([0xd1; 32]),
            Sha256Digest::from_bytes([0xd2; 32]),
        );
        let key_b = super::ClientResourceKey::new(
            target,
            principal,
            Sha256Digest::from_bytes([0xd1; 32]),
            Sha256Digest::from_bytes([0xd3; 32]),
        );
        assert_ne!(key_a, key_b);
        let mut state = super::ClientStateStore::new();

        state.get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Boolean));
        state.get_or_create_resource(key_b, ResolvedType::Scalar(StandardScalar::Boolean));

        let resource_a = state.resource(key_a).expect("first resource is cached");
        let resource_b = state.resource(key_b).expect("second resource is cached");
        assert_eq!(resource_a.key(), key_a);
        assert_eq!(resource_b.key(), key_b);

        let generation = state
            .resource_mut(key_a)
            .expect("first resource is cached")
            .begin_loading()
            .unwrap();
        assert_eq!(
            state.resource(key_a).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Loading),
        );
        assert_eq!(
            state.resource(key_b).map(super::ClientResource::status),
            Some(super::ClientResourceStatus::Idle),
        );
        state
            .resource_mut(key_a)
            .expect("first resource is cached")
            .cancel(generation)
            .unwrap();
    }

    fn version_four_text_state_plan() -> (
        ActiveDatabaseRevision,
        FunctionId,
        orna_artifact::client_plan::StateClientPlan,
    ) {
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Concat {
                left: Box::new(orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "hello ".to_owned(),
                }),
                right: Box::new(orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "world".to_owned(),
                }),
            },
            vec![
                orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x11; 16]),
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Expression(
                        orna_artifact::client_plan::ClientExpressionNode::String {
                            value: "local-default".to_owned(),
                        },
                    ),
                ),
                orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x12; 16]),
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                    orna_artifact::client_plan::StateScope::Session,
                    orna_artifact::client_plan::StateDefault::Null,
                ),
                orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x13; 16]),
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Unset,
                ),
            ],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        (active, function, plan)
    }

    #[test]
    fn evaluates_version_four_state_plans_and_initialises_local_and_session_state() {
        let (active, function, plan) = version_four_text_state_plan();
        let mut state = super::ClientStateStore::new();

        let result = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap();

        assert_eq!(
            result.value(),
            &RuntimeValue::Text("hello world".to_owned())
        );
        assert_eq!(
            state.local().get(&super::ClientStateKey::new(
                function,
                StateSlotId::from_bytes([0x11; 16])
            )),
            Some(&RuntimeValue::Text("local-default".to_owned()))
        );
        let expected_null = RuntimeValue::null(ResolvedType::value(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        ))
        .unwrap();
        assert_eq!(
            state.session().get(&super::ClientStateKey::new(
                function,
                StateSlotId::from_bytes([0x12; 16])
            )),
            Some(&expected_null)
        );
        assert!(!state.local().contains_key(&super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x13; 16])
        )));
        assert!(state.user().is_empty());
        assert_eq!(
            plan.format_version(),
            orna_artifact::client_plan::STATE_FORMAT_VERSION
        );

        super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap();
    }

    #[test]
    fn state_context_data_invalidation_token_preserves_existing_defaults() {
        let function = FunctionId::from_bytes([0x61; 16]);
        let mut context = super::ClientStateContext::default_for(function);
        assert_eq!(context.data_invalidation_token(), Sha256Digest::from_bytes([0; 32]));
        assert_eq!(
            super::ClientStateContext::new(function, "profile".to_owned(), "instance".to_owned())
                .unwrap()
                .data_invalidation_token(),
            Sha256Digest::from_bytes([0; 32]),
        );
        let token = Sha256Digest::from_bytes([0x62; 32]);
        context.set_data_invalidation_token(token);
        assert_eq!(context.data_invalidation_token(), token);
        assert_eq!(context.root_function(), function);
        assert_eq!(context.state_profile(), "");
        assert_eq!(context.instance_key(), "");
    }

    #[test]
    fn version_four_state_context_profiles_are_isolated() {
        let (active, function, _) = version_four_text_state_plan();
        let profile_a =
            super::ClientStateContext::new(function, "profile-a".to_owned(), String::new())
                .unwrap();
        let profile_b =
            super::ClientStateContext::new(function, "profile-b".to_owned(), String::new())
                .unwrap();
        let mut state = super::ClientStateStore::new();
        let grants = super::capability::LocalCapabilityGrantSet::new();
        let slot = StateSlotId::from_bytes([0x12; 16]);

        super::evaluate_client_function_in_state_context_with_grants_and_arguments(
            &active,
            &authorise(active.pair(), function),
            &profile_a,
            &[],
            &[],
            &grants,
            &mut state,
        )
        .unwrap();
        let mut executor =
            super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
                Ok(RuntimeValue::Boolean(true))
            });
        super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(active.pair(), function),
            &profile_b,
            &[],
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x44; 16]),
            &mut executor,
        )
        .unwrap();

        let key_a = super::ClientStateKey::from_context(&profile_a, function, slot);
        let key_b = super::ClientStateKey::from_context(&profile_b, function, slot);
        assert_ne!(key_a, key_b);
        assert!(state.session().contains_key(&key_a));
        assert!(state.session().contains_key(&key_b));
        assert_eq!(state.context(), &profile_b);
    }

    #[test]
    fn version_four_keeps_caller_state_input_over_the_plan_default() {
        let (active, function, _) = version_four_text_state_plan();
        let mut state = super::ClientStateStore::new();
        state.session_mut().insert(
            super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
            RuntimeValue::Text("remounted-session".to_owned()),
        );

        super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap();

        assert_eq!(
            state.session().get(&super::ClientStateKey::new(
                function,
                StateSlotId::from_bytes([0x12; 16])
            )),
            Some(&RuntimeValue::Text("remounted-session".to_owned()))
        );
    }

    #[test]
    fn version_four_rejects_caller_state_with_the_wrong_type() {
        let (active, function, _) = version_four_text_state_plan();
        let mut state = super::ClientStateStore::new();
        state.session_mut().insert(
            super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
            RuntimeValue::Boolean(true),
        );

        let error = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::StateEvaluation {
                context,
                source: super::ClientStateError::StoredTypeMismatch { slot },

            } if context.function() == function
                && *slot == StateSlotId::from_bytes([0x12; 16])
        ));
    }

    #[test]
    fn version_four_user_state_with_matching_persisted_type_is_accepted() {
        let slot = StateSlotId::from_bytes([0x20; 16]);
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::User,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();
        let durable_key = UserStateKey::new(
            PrincipalId::from_bytes([0x20; 16]),
            function,
            String::new(),
            function,
            String::new(),
            slot,
        )
        .unwrap();
        state
            .load_user_state(&[UserStateCell::new(
                durable_key,
                RuntimeValue::Boolean(true),
                orna_standard::BOOLEAN_TYPE_ID,
                1,
                SystemTime::UNIX_EPOCH,
            )])
            .unwrap();

        let result = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap();

        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
        assert_eq!(state.user().len(), 1);
        assert_eq!(
            state
                .user()
                .values()
                .next()
                .expect("the matching USER state remains loaded")
                .value_type(),
            orna_standard::BOOLEAN_TYPE_ID,
        );
    }

    #[test]
    fn version_four_user_state_rejects_wrong_persisted_type_without_mutating_state() {
        let slot = StateSlotId::from_bytes([0x22; 16]);
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::User,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();
        let durable_key = UserStateKey::new(
            PrincipalId::from_bytes([0x22; 16]),
            function,
            String::new(),
            function,
            String::new(),
            slot,
        )
        .unwrap();
        state
            .load_user_state(&[UserStateCell::new(
                durable_key,
                RuntimeValue::Boolean(true),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                1,
                SystemTime::UNIX_EPOCH,
            )])
            .unwrap();
        let before = state.clone();

        let error = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::StateEvaluation {
                context,
                source: super::ClientStateError::StoredTypeMismatch { slot: actual_slot },
            } if context.function() == function && actual_slot == slot
        ));
        assert_eq!(state, before);
    }

    #[test]
    fn version_four_user_state_without_persisted_value_uses_unset_default() {
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x21; 16]),
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::User,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();

        let result = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap();

        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
        assert!(state.user().is_empty());
        assert!(state.local().is_empty() && state.session().is_empty());
    }
    #[test]
    fn client_user_state_store_loads_updates_and_applies_write_results() {
        let root_function = FunctionId::from_bytes([0x31; 16]);
        let function = FunctionId::from_bytes([0x32; 16]);
        let slot = StateSlotId::from_bytes([0x33; 16]);
        let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let context = super::ClientStateContext::new(
            root_function,
            "profile".to_owned(),
            "root-instance".to_owned(),
        )
        .unwrap();
        let client_key = super::ClientStateKey::from_context(&context, function, slot);
        let durable_key = UserStateKey::new(
            PrincipalId::from_bytes([0x34; 16]),
            root_function,
            "profile".to_owned(),
            function,
            "root-instance".to_owned(),
            slot,
        )
        .unwrap();
        let cell = UserStateCell::new(
            durable_key,
            RuntimeValue::Text("loaded".to_owned()),
            value_type,
            7,
            SystemTime::UNIX_EPOCH,
        );
        let mut state = super::ClientStateStore::new();
        state.set_context(context);
        state.load_user_state(&[cell]).unwrap();
        assert!(state.pending_user_state_changes().unwrap().is_empty());

        state
            .set_user_state(
                client_key.clone(),
                RuntimeValue::Text("changed".to_owned()),
                value_type,
            )
            .unwrap();
        let changes = state.pending_user_state_changes().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].expected_revision(), Some(7));
        let before = state.user().clone();
        let leap_result = UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 9 },
        );
        let leap_error = state
            .apply_user_state_write_results(&changes, &[leap_result])
            .unwrap_err();
        assert!(matches!(
            leap_error,
            super::ClientUserStateError::InvalidRevision(key) if key == client_key
        ));
        assert_eq!(state.user(), &before);
        assert_eq!(state.pending_user_state_changes().unwrap(), changes);

        let result = UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 8 },
        );
        state
            .apply_user_state_write_results(&changes, &[result])
            .unwrap();

        let stored = state.user().get(&client_key).unwrap();
        assert_eq!(stored.value(), &RuntimeValue::Text("changed".to_owned()));
        assert_eq!(stored.revision(), Some(8));
        assert!(!stored.is_dirty());
        assert!(state.pending_user_state_changes().unwrap().is_empty());
    }

    #[test]
    fn client_user_state_store_rejects_first_write_revision_leap() {
        let root_function = FunctionId::from_bytes([0x51; 16]);
        let function = FunctionId::from_bytes([0x52; 16]);
        let slot = StateSlotId::from_bytes([0x53; 16]);
        let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let context = super::ClientStateContext::new(
            root_function,
            "profile".to_owned(),
            "root-instance".to_owned(),
        )
        .unwrap();
        let client_key = super::ClientStateKey::from_context(&context, function, slot);
        let mut state = super::ClientStateStore::new();
        state.set_context(context);
        state
            .set_user_state(
                client_key.clone(),
                RuntimeValue::Text("new".to_owned()),
                value_type,
            )
            .unwrap();
        let changes = state.pending_user_state_changes().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].expected_revision(), None);
        let before = state.user().clone();
        let result = UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 2 },
        );

        let error = state
            .apply_user_state_write_results(&changes, &[result])
            .unwrap_err();

        assert!(matches!(
            error,
            super::ClientUserStateError::InvalidRevision(key) if key == client_key
        ));
        assert_eq!(state.user(), &before);
        assert_eq!(state.pending_user_state_changes().unwrap(), changes);
    }

    #[test]
    fn client_user_state_write_results_leave_all_cells_unchanged_when_a_later_revision_is_invalid() {
        let root_function = FunctionId::from_bytes([0x41; 16]);
        let function = FunctionId::from_bytes([0x42; 16]);
        let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let context = super::ClientStateContext::new(
            root_function,
            "profile".to_owned(),
            "root-instance".to_owned(),
        )
        .unwrap();
        let first_slot = StateSlotId::from_bytes([0x43; 16]);
        let second_slot = StateSlotId::from_bytes([0x44; 16]);
        let principal = PrincipalId::from_bytes([0x45; 16]);
        let first_key = UserStateKey::new(
            principal,
            root_function,
            "profile".to_owned(),
            function,
            "root-instance".to_owned(),
            first_slot,
        )
        .unwrap();
        let second_key = UserStateKey::new(
            principal,
            root_function,
            "profile".to_owned(),
            function,
            "root-instance".to_owned(),
            second_slot,
        )
        .unwrap();
        let first_client_key = super::ClientStateKey::from_context(&context, function, first_slot);
        let second_client_key = super::ClientStateKey::from_context(&context, function, second_slot);
        let mut state = super::ClientStateStore::new();
        state.set_context(context);
        state
            .load_user_state(&[
                UserStateCell::new(
                    first_key,
                    RuntimeValue::Text("first-loaded".to_owned()),
                    value_type,
                    7,
                    SystemTime::UNIX_EPOCH,
                ),
                UserStateCell::new(
                    second_key,
                    RuntimeValue::Text("second-loaded".to_owned()),
                    value_type,
                    11,
                    SystemTime::UNIX_EPOCH,
                ),
            ])
            .unwrap();
        state
            .set_user_state(
                first_client_key.clone(),
                RuntimeValue::Text("first-changed".to_owned()),
                value_type,
            )
            .unwrap();
        state
            .set_user_state(
                second_client_key.clone(),
                RuntimeValue::Text("second-changed".to_owned()),
                value_type,
            )
            .unwrap();
        let changes = state.pending_user_state_changes().unwrap();
        assert_eq!(changes.len(), 2);
        let before = state.user().clone();
        let results = vec![
            UserStateWriteResult::new(
                changes[0].key_without_principal(),
                UserStateWriteOutcome::Written { revision: 8 },
            ),
            UserStateWriteResult::new(
                changes[1].key_without_principal(),
                UserStateWriteOutcome::Written { revision: 0 },
            ),
        ];

        let error = state
            .apply_user_state_write_results(&changes, &results)
            .unwrap_err();

        assert!(matches!(error, super::ClientUserStateError::InvalidRevision(_)));
        assert_eq!(state.user(), &before);

        let mixed_results = vec![
            UserStateWriteResult::new(
                changes[0].key_without_principal(),
                UserStateWriteOutcome::Written { revision: 8 },
            ),
            UserStateWriteResult::new(
                changes[1].key_without_principal(),
                UserStateWriteOutcome::Conflict {
                    current_revision: 15,
                },
            ),
        ];
        let mixed_error = state
            .apply_user_state_write_results(&changes, &mixed_results)
            .unwrap_err();

        assert!(matches!(
            mixed_error,
            super::ClientUserStateError::Conflict {
                key,
                expected: Some(11),
                current: 15,
            } if key == second_client_key
        ));
        let first_after_mixed = state.user().get(&first_client_key).unwrap();
        assert_eq!(first_after_mixed.revision(), Some(8));
        assert!(!first_after_mixed.is_dirty());
        let second_after_mixed = state.user().get(&second_client_key).unwrap();
        assert_eq!(second_after_mixed.revision(), Some(11));
        assert!(second_after_mixed.is_dirty());
        let after_mixed = state.user().clone();

        let duplicate_changes = vec![changes[1].clone(), changes[1].clone()];
        let duplicate_results = vec![
            UserStateWriteResult::new(
                duplicate_changes[0].key_without_principal(),
                UserStateWriteOutcome::Written { revision: 16 },
            ),
            UserStateWriteResult::new(
                duplicate_changes[1].key_without_principal(),
                UserStateWriteOutcome::Written { revision: 17 },
            ),
        ];
        let duplicate_error = state
            .apply_user_state_write_results(&duplicate_changes, &duplicate_results)
            .unwrap_err();

        assert!(matches!(
            duplicate_error,
            super::ClientUserStateError::DuplicateKey(key) if key == super::ClientStateKey::from_user_change(&changes[1])
        ));
        assert_eq!(state.user(), &after_mixed);
    }

    #[test]
    fn version_four_state_default_type_mismatch_fails_closed() {
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![
                orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x30; 16]),
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Expression(
                        orna_artifact::client_plan::ClientExpressionNode::String {
                            value: "must-not-commit".to_owned(),
                        },
                    ),
                ),
                orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x31; 16]),
                    orna_standard::BOOLEAN_TYPE_ID,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Expression(
                        orna_artifact::client_plan::ClientExpressionNode::String {
                            value: "not-a-boolean".to_owned(),
                        },
                    ),
                ),
            ],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();

        let error = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();
        assert!(state.local().is_empty());

        assert!(matches!(
            &error,
            super::ClientExecutionError::StateEvaluation {
                context,
                source: super::ClientStateError::DefaultTypeMismatch { slot },
            } if context.function() == function
                && *slot == StateSlotId::from_bytes([0x31; 16])
        ));
    }
    #[test]
    fn version_four_supported_scalar_slot_types_initialise() {
        for type_id in [
            orna_standard::BIGINT_TYPE_ID,
            orna_standard::FLOAT_TYPE_ID,
            orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
        ] {
            let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
            let plan = orna_artifact::client_plan::StateClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
                vec![orna_artifact::client_plan::StateSlot::new(
                    slot_id,
                    type_id,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Null,
                )],
            );
            let (active, function, _, _) = version_four_state_active(
                orna_standard::BOOLEAN_TYPE_ID,
                plan.encode().expect("the state plan encodes"),
            );
            let mut state = super::ClientStateStore::new();

            let result = super::evaluate_client_function_with_state(
                &active,
                &authorise(active.pair(), function),
                &mut state,
            )
            .expect("supported scalar state slot initialises");

            assert_eq!(result.value(), &RuntimeValue::Boolean(true));
            assert_eq!(
                state.local().get(&super::ClientStateKey::new(function, slot_id)),
                Some(
                    &RuntimeValue::null(ResolvedType::value(type_id))
                        .expect("supported scalar null constructs"),
                ),
            );
        }
    }

    #[test]
    fn version_four_unsupported_slot_type_fails_closed() {
        for type_id in [
            orna_standard::DATE_TYPE_ID,
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
        ] {
            let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
            let plan = orna_artifact::client_plan::StateClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
                vec![orna_artifact::client_plan::StateSlot::new(
                    slot_id,
                    type_id,
                    orna_artifact::client_plan::StateScope::Local,
                    orna_artifact::client_plan::StateDefault::Unset,
                )],
            );
            let (active, function, _, _) = version_four_state_active(
                orna_standard::BOOLEAN_TYPE_ID,
                plan.encode().expect("the state plan encodes"),
            );
            let mut state = super::ClientStateStore::new();

            let error = super::evaluate_client_function_with_state(
                &active,
                &authorise(active.pair(), function),
                &mut state,
            )
            .unwrap_err();

            assert!(matches!(
                &error,
                super::ClientExecutionError::StateEvaluation {
                    context,
                    source: super::ClientStateError::UnsupportedSlotType { slot },
                } if context.function() == function && *slot == slot_id
            ));
        }
    }

    #[test]
    fn opaque_value_with_scalar_contract_is_not_a_supported_state_slot_type() {
        let definition = ValueTypeDefinition::opaque(
            TypeId::from_bytes([0xf2; 16]),
            QualifiedSemanticName::new(["tests", "opaque_scalar"]).unwrap(),
            "orna.kernel.value.boolean@1",
        );

        assert!(!super::state_slot_type_is_supported(&definition));
    }

    #[test]
    fn version_four_return_type_mismatch_fails_as_an_expression_error() {
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Integer { value: 42 },
            vec![orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x51; 16]),
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();

        let error = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::ExpressionEvaluation {
                context,
                source: super::ClientExpressionError::TypeMismatch,
            } if context.function() == function
        ));
    }

    #[test]
    fn version_four_plans_run_through_the_legacy_entry_point_with_transient_state() {
        let (active, function, _) = version_four_text_state_plan();

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(
            result.value(),
            &RuntimeValue::Text("hello world".to_owned())
        );
    }

    #[test]
    fn procedural_literals_and_assignments_use_declaration_locals() {
        let local = LocalId::from_bytes([0xc1; 16]);
        let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(local, text_type, orna_artifact::client_plan::ClientLocalKind::Value)],
            vec![
                orna_artifact::client_plan::ClientStatement::let_(local, orna_artifact::client_plan::ClientExpressionNode::String { value: "first".to_owned() }),
                orna_artifact::client_plan::ClientStatement::assignment(local, orna_artifact::client_plan::ClientExpressionNode::String { value: "second".to_owned() }),
            ],
            orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
        );
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new("std.fs.read", orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()))],
        ).encode().unwrap();
        let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
        let grant = super::capability::LocalCapabilityGrant::new(super::capability::LocalCapabilityName::StdFsRead, super::capability::LocalCapabilityScope::path("/tmp").unwrap()).unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(ParameterId::from_bytes([0xb1; 16]), RuntimeValue::Text("/tmp".to_owned())).unwrap();
        let result = super::evaluate_client_function_with_grants_and_arguments(&active, &authorise(pair, function), &[argument], &[], &grants).unwrap();
        assert_eq!(result.value(), &RuntimeValue::Text("second".to_owned()));
    }

    #[test]
    fn resource_request_rejects_missing_target_arguments() {
        let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([3; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Boolean(
                orna_artifact::client_plan::ClientPlan::return_boolean(false),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
            )],
        )
        .encode()
        .unwrap();
        let (active, _, _, _, _) = version_five_expression_active_with_parameter(payload);
        let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
            PrincipalId::from_bytes([0x71; 16]),
            digest,
            active.catalogue_hash(),
        );
        let mut resource = super::ClientResource::new(
            key,
            ResolvedType::Value(text_type),
        );

        let error = resource.begin_request(&active, Vec::new()).unwrap_err();

        assert!(matches!(
            error,
            super::ClientResourceError::MissingArgument { parameter }
                if parameter == ParameterId::from_bytes([0xd3; 16])
        ));
    }

    #[test]
    fn procedural_await_without_executor_fails_closed() {
        let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([3; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            FunctionId::from_bytes([0xd1; 16]),
            pair,
            orna_core::CallSiteId::from_bytes([8; 16]),
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            text_type,
        );
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(Vec::new(), Vec::new(), orna_artifact::client_plan::ClientExpressionNode::Await { expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource { operation }) });
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new("std.fs.read", orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()))],
        ).encode().unwrap();
        let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
        let grant = super::capability::LocalCapabilityGrant::new(super::capability::LocalCapabilityName::StdFsRead, super::capability::LocalCapabilityScope::path("/tmp").unwrap()).unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(ParameterId::from_bytes([0xb1; 16]), RuntimeValue::Text("/tmp".to_owned())).unwrap();
        let error = super::evaluate_client_function_with_grants_and_arguments(&active, &authorise(pair, function), &[argument], &[], &grants).unwrap_err();
        assert!(matches!(error, super::ClientExecutionError::ResourceEvaluation { source: super::ClientResourceExecutionError::ExecutorUnavailable, .. }));
    }


    #[test]
    fn procedural_scalar_resource_local_awaits_through_assignment_with_executor_value() {
        let local = LocalId::from_bytes([0xc2; 16]);
        let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let target_revision = RevisionPair::new(
            SourceRevisionId::from_bytes([3; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );
        let target = FunctionId::from_bytes([0xd1; 16]);
        let parent_invocation_id = orna_core::InvocationId::from_bytes([0x91; 16]);
        let call_site_id = orna_core::CallSiteId::from_bytes([0x82; 16]);
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            target,
            target_revision,
            call_site_id,
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            text_type,
        );
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(
                local,
                text_type,
                orna_artifact::client_plan::ClientLocalKind::Resource(
                    orna_artifact::client_plan::ResourceKind::Scalar,
                ),
            )],
            vec![
                orna_artifact::client_plan::ClientStatement::let_(
                    local,
                    orna_artifact::client_plan::ClientExpressionNode::Resource { operation: operation.clone() },
                ),
                orna_artifact::client_plan::ClientStatement::assignment(
                    local,
                    orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
                ),
            ],
            orna_artifact::client_plan::ClientExpressionNode::Await {
                expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::LocalRead { local }),
            },
        );
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
            )],
        )
        .encode()
        .unwrap();
        let (active, function, pair, _, parameter) = version_five_expression_active_with_parameter(payload);
        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();
        let state_context = super::ClientStateContext::new(
            function,
            "profile-a".to_owned(),
            "instance-a".to_owned(),
        )
        .unwrap();
        let mut state = super::ClientStateStore::new();
        state.set_context(state_context);
        let mut executor = super::DeterministicClientResourceExecutor::new(
            |request: &super::ClientResourceRequest| {
                assert_eq!(
                    request.invocation_context(),
                    Some(super::ClientResourceInvocationContext::new(
                        parent_invocation_id,
                        call_site_id,
                        "profile-a".to_owned(),
                        "instance-a".to_owned(),
                    )),
                );
                assert_eq!(request.key().target(), InvocationTarget::new(target, pair));
                assert_eq!(request.arguments().len(), 1);
                assert_eq!(
                    request.arguments()[0].parameter(),
                    ParameterId::from_bytes([0xd3; 16]),
                );
                assert_eq!(
                    request.arguments()[0].value(),
                    &RuntimeValue::Text("/tmp".to_owned()),
                );
                Ok(RuntimeValue::Text("executor-value".to_owned()))
            },
        );

        let result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &state.context().clone(),
            &[argument],
            &[],
            &grants,
            &mut state,
            parent_invocation_id,
            &mut executor,
        )
        .unwrap();

        assert_eq!(result.value(), &RuntimeValue::Text("executor-value".to_owned()));
    }

    #[test]
    fn evaluator_resource_key_includes_host_data_invalidation_token() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let grants = capability::LocalCapabilityGrantSet::from_grants([
            capability::LocalCapabilityGrant::new(
                capability::LocalCapabilityName::StdFsRead,
                capability::LocalCapabilityScope::path("/tmp").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/data-token".to_owned()),
        )
        .unwrap();
        let context_a = super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([0x11; 32]),
        )
        .unwrap();
        let context_b = super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([0x12; 32]),
        )
        .unwrap();
        let mut state = ClientStateStore::new();
        let mut executor = RecordingActionExecutor::new(None);
        let pending_key = |error: super::ClientExecutionError| match error {
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Pending { key, .. },
                ..
            } => key,
            other => panic!("expected pending resource evaluation, got {other:?}"),
        };
        let key_a = pending_key(
            super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context_a,
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0x31; 16]),
                &mut executor,
            )
            .unwrap_err(),
        );
        let key_b = pending_key(
            super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context_b,
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0x32; 16]),
                &mut executor,
            )
            .unwrap_err(),
        );

        assert_ne!(key_a, key_b, "host data invalidation must select a new local key");
        assert_eq!(executor.cancelled.len(), 1, "the old loading generation is cancelled");
        assert_eq!(state.resource(key_a).map(ClientResource::status), Some(ClientResourceStatus::Idle));
        assert_eq!(state.resource(key_b).map(ClientResource::status), Some(ClientResourceStatus::Loading));
        assert_eq!(key_a.target(), key_b.target());
        assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    }

    #[test]
    fn evaluator_resource_key_includes_authorised_security_context() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let (direct_authorisation, role_authorisation) = authorise_with_role_context(pair, function);
        let grants = capability::LocalCapabilityGrantSet::from_grants([
            capability::LocalCapabilityGrant::new(
                capability::LocalCapabilityName::StdFsRead,
                capability::LocalCapabilityScope::path("/tmp").unwrap(),
            )
            .unwrap(),
        ])
        .unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/security-context".to_owned()),
        )
        .unwrap();
        let context = super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([0x21; 32]),
        )
        .unwrap();

        // A changed security context cannot reuse a READY value.
        let mut ready_state = ClientStateStore::new();
        let mut ready_executor = RecordingActionExecutor::new(Some(RuntimeValue::Text(
            "direct".to_owned(),
        )));
        let direct_result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &direct_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut ready_state,
            InvocationId::from_bytes([0x41; 16]),
            &mut ready_executor,
        )
        .unwrap();
        let key_a = ready_executor.executed[0].key();
        assert_eq!(direct_result.value(), &RuntimeValue::Text("direct".to_owned()));
        assert_eq!(ready_state.resource(key_a).map(ClientResource::status), Some(ClientResourceStatus::Ready));

        let mut role_executor = RecordingActionExecutor::new(Some(RuntimeValue::Text(
            "role".to_owned(),
        )));
        let role_result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &role_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut ready_state,
            InvocationId::from_bytes([0x42; 16]),
            &mut role_executor,
        )
        .unwrap();
        let key_b = role_executor.executed[0].key();
        assert_ne!(key_a, key_b, "security context must select a new local key");
        assert_eq!(role_result.value(), &RuntimeValue::Text("role".to_owned()));
        assert_eq!(ready_state.resource(key_a).map(ClientResource::status), Some(ClientResourceStatus::Ready));
        assert_eq!(ready_state.resource(key_b).map(ClientResource::status), Some(ClientResourceStatus::Ready));
        assert_eq!(key_a.target(), key_b.target());
        assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
        assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());

        // The same security change also replaces an old loading generation and
        // routes cancellation through the caller-owned executor.
        let mut loading_state = ClientStateStore::new();
        let mut loading_executor = RecordingActionExecutor::new(None);
        let direct_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &direct_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut loading_state,
            InvocationId::from_bytes([0x43; 16]),
            &mut loading_executor,
        )
        .unwrap_err();
        let loading_key_a = match direct_error {
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Pending { key, .. },
                ..
            } => key,
            other => panic!("expected pending direct resource, got {other:?}"),
        };
        let role_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &role_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut loading_state,
            InvocationId::from_bytes([0x44; 16]),
            &mut loading_executor,
        )
        .unwrap_err();
        let loading_key_b = match role_error {
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Pending { key, .. },
                ..
            } => key,
            other => panic!("expected pending role resource, got {other:?}"),
        };
        assert_ne!(loading_key_a, loading_key_b);
        assert_eq!(loading_executor.cancelled.len(), 1);
        assert_eq!(loading_state.resource(loading_key_a).map(ClientResource::status), Some(ClientResourceStatus::Idle));
        assert_eq!(loading_state.resource(loading_key_b).map(ClientResource::status), Some(ClientResourceStatus::Loading));
    }

    #[test]
    fn ordinary_resource_pending_persists_only_the_loading_resource() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let state_context = super::ClientStateContext::new(
            FunctionId::from_bytes([0xa1; 16]),
            "profile".to_owned(),
            "instance".to_owned(),
        )
        .unwrap();
        let local_key = super::ClientStateKey::from_context(
            &state_context,
            function,
            StateSlotId::from_bytes([0xa2; 16]),
        );
        let session_key = super::ClientStateKey::from_context(
            &state_context,
            function,
            StateSlotId::from_bytes([0xa3; 16]),
        );
        let user_key = UserStateKey::new(
            principal,
            state_context.root_function(),
            state_context.state_profile().to_owned(),
            function,
            state_context.instance_key().to_owned(),
            StateSlotId::from_bytes([0xa4; 16]),
        )
        .unwrap();
        let mut state = ClientStateStore::new();
        state.set_context(state_context.clone());
        state.local_mut().insert(
            local_key,
            RuntimeValue::Text("local".to_owned()),
        );
        state.session_mut().insert(
            session_key,
            RuntimeValue::Text("session".to_owned()),
        );
        state
            .load_user_state(&[UserStateCell::new(
                user_key,
                RuntimeValue::Text("user".to_owned()),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                1,
                SystemTime::UNIX_EPOCH,
            )])
            .unwrap();
        let prior_context = state.context().clone();
        let prior_local = state.local().clone();
        let prior_session = state.session().clone();
        let prior_user = state.user().clone();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/pending").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/pending".to_owned()),
        )
        .unwrap();
        let mut executor = RecordingActionExecutor::new(None);

        let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &[argument],
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x91; 16]),
            &mut executor,
        )
        .unwrap_err();
        let (key, generation) = match error {
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Pending { key, generation },
                ..
            } => (key, generation),
            other => panic!("expected Pending resource evaluation, got {other:?}"),
        };

        assert_eq!(state.context(), &prior_context);
        assert_eq!(state.local(), &prior_local);
        assert_eq!(state.session(), &prior_session);
        assert_eq!(state.user(), &prior_user);
        let resource = state.resource(key).expect("pending resource remains in caller state");
        let request_id = resource.request_id().expect("pending resource has request identity");
        assert_eq!(resource.key(), key);
        assert_eq!(resource.generation(), generation);
        assert_eq!(resource.status(), ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
        assert_eq!(resource.failure(), None);
        state
            .resource_mut(key)
            .expect("pending resource remains mutable in caller state")
            .apply_completion(
                &active,
                ClientResourceCompletion::Ready {
                    request_id,
                    key,
                    generation,
                    value: RuntimeValue::Text("resumed".to_owned()),
                },
            )
            .unwrap();
        assert_eq!(
            state.resource(key).map(ClientResource::status),
            Some(ClientResourceStatus::Ready),
        );
    }
    #[test]
    fn terminal_resource_states_persist_when_evaluation_fails() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/resource".to_owned()),
        )
        .unwrap();

        let mut failed_state = ClientStateStore::new();
        let mut failing_executor = FailingActionExecutor::default();
        let failure = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut failed_state,
            InvocationId::from_bytes([0x92; 16]),
            &mut failing_executor,
        )
        .unwrap_err();

        assert!(matches!(
            failure,
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Failed(code),
                ..
            } if code == "secret.executor.detail"
        ));
        let failed_request = failing_executor
            .request
            .as_ref()
            .expect("failing executor received a resource request");
        let failed_resource = failed_state
            .resource(failed_request.key())
            .expect("failed resource remains at the evaluated request key");
        assert_eq!(failed_resource.key(), failed_request.key());
        assert_eq!(failed_resource.generation(), failed_request.generation());
        assert_eq!(
            failed_resource.request_id(),
            Some(failed_request.request_id()),
        );
        assert_eq!(failed_resource.status(), ClientResourceStatus::Failed);
        assert_eq!(
            failed_resource.failure().map(super::ClientResourceFailure::code),
            Some("secret.executor.detail"),
        );

        let mut cancelled_state = ClientStateStore::new();
        let mut cancelled_executor = CancelledActionExecutor::default();
        let cancellation = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut cancelled_state,
            InvocationId::from_bytes([0x93; 16]),
            &mut cancelled_executor,
        )
        .unwrap_err();

        assert!(matches!(
            cancellation,
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Cancelled,
                ..
            }
        ));
        let cancelled_request = cancelled_executor
            .request
            .as_ref()
            .expect("cancelled executor received a resource request");
        let cancelled_resource = cancelled_state
            .resource(cancelled_request.key())
            .expect("cancelled resource remains at the evaluated request key");
        assert_eq!(cancelled_resource.key(), cancelled_request.key());
        assert_eq!(cancelled_resource.generation(), cancelled_request.generation());
        assert_eq!(
            cancelled_resource.request_id(),
            Some(cancelled_request.request_id()),
        );
        assert_eq!(cancelled_resource.status(), ClientResourceStatus::Cancelled);
        assert_eq!(cancelled_resource.failure(), None);
    }



    #[test]
    fn malformed_resource_completion_cancels_executor_and_persists_terminal_state() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/malformed".to_owned()),
        )
        .unwrap();
        let mut state = ClientStateStore::new();
        let mut executor = MalformedResourceExecutor::default();

        let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x94; 16]),
            &mut executor,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Cancelled,
                ..
            }
        ));

        let request = executor
            .executed
            .clone()
            .expect("malformed executor received a resource request");
        assert_eq!(executor.cancelled, vec![request.clone()]);
        let mut resource = state
            .resource(request.key())
            .expect("cancelled resource remains in caller state")
            .clone();
        assert_eq!(resource.status(), ClientResourceStatus::Cancelled);
        assert_eq!(resource.generation(), request.generation());
        assert!(matches!(
            resource.apply_completion(
                &active,
                request.ready(RuntimeValue::Text("late".to_owned())),
            ),
            Err(super::ClientResourceError::InvalidTransition {
                status: ClientResourceStatus::Cancelled,
            })
        ));
    }

    #[test]
    fn mismatched_request_id_completion_does_not_cancel_request() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/stale-request".to_owned()),
        )
        .unwrap();
        let mut state = ClientStateStore::new();
        let mut executor = MalformedResourceExecutor {
            stale_request_id: true,
            ..MalformedResourceExecutor::default()
        };

        let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x96; 16]),
            &mut executor,
        )
        .expect_err("a mismatched request ID must not cancel the active request");
        let request = executor
            .executed
            .clone()
            .expect("executor received a resource request");
        assert!(executor.cancelled.is_empty());
        assert!(matches!(
            error,
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Invalid(_),
                ..
            }
        ));
        let resource = state
            .resource(request.key())
            .expect("the active request remains in caller state");
        assert_eq!(resource.status(), ClientResourceStatus::Loading);
        assert_eq!(resource.generation(), request.generation());
        assert_eq!(resource.request_id(), Some(request.request_id()));
    }

    #[test]
    fn malformed_resource_completion_returns_terminal_cancel_result() {
        let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/malformed-ready".to_owned()),
        )
        .unwrap();
        let mut state = ClientStateStore::new();
        let mut executor = MalformedResourceExecutor {
            cancel_ready: true,
            ..MalformedResourceExecutor::default()
        };

        let result = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x95; 16]),
            &mut executor,
        )
        .expect("valid terminal cancellation completion wins over malformed execute result");
        assert_eq!(result.value(), &RuntimeValue::Text("cancelled-ready".to_owned()));
        let request = executor
            .executed
            .clone()
            .expect("malformed executor received a resource request");
        assert_eq!(executor.cancelled, vec![request.clone()]);
        assert_eq!(state.resource(request.key()).map(ClientResource::status), Some(ClientResourceStatus::Ready));
    }

    #[test]
    fn procedural_scalar_resource_local_await_without_executor_fails_closed() {
        let local = LocalId::from_bytes([0xc3; 16]);
        let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([3; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            FunctionId::from_bytes([0xd1; 16]),
            pair,
            orna_core::CallSiteId::from_bytes([0x83; 16]),
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            text_type,
        );
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(
                local,
                text_type,
                orna_artifact::client_plan::ClientLocalKind::Resource(
                    orna_artifact::client_plan::ResourceKind::Scalar,
                ),
            )],
            vec![orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
            )],
            orna_artifact::client_plan::ClientExpressionNode::Await {
                expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::LocalRead { local }),
            },
        );
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
            )],
        )
        .encode()
        .unwrap();
        let (active, function, pair, _, parameter) = version_five_expression_active_with_parameter(payload);
        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();

        let error = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorise(pair, function),
            &[argument],
            &[],
            &grants,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::ExecutorUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn capability_gate_denies_an_ungranted_declared_capability() {
        let (active, function, _, _) = version_one_active(true);
        let grants = super::capability::LocalCapabilityGrantSet::new();
        let declaration = super::capability::LocalCapabilityDeclaration::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityArgumentSource::Text("/home/bob".to_owned()),
        );

        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(active.pair(), function),
            &[declaration],
            &grants,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::CapabilityDenied {
                context,
                capability,
            } if context.function() == function && capability == "std.fs.read"
        ));
    }

    #[test]
    fn capability_gate_admits_a_granted_declared_capability() {
        let (active, function, pair, _) = version_one_active(true);
        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let declaration = super::capability::LocalCapabilityDeclaration::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityArgumentSource::Text("/home/bob/x".to_owned()),
        );

        let result = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, function),
            &[declaration],
            &grants,
        )
        .unwrap();

        assert_eq!(result.context().function(), function);
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn capability_gate_keeps_zero_declaration_functions_unchanged() {
        let (active, function, pair, _) = version_one_active(true);
        let empty_grants = super::capability::LocalCapabilityGrantSet::new();

        let result = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, function),
            &[],
            &empty_grants,
        )
        .unwrap();

        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn version_five_stored_literal_capability_denies_without_grants() {
        let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
        )];
        let (active, function, _, _) =
            version_five_boolean_active(version_five_boolean_envelope(true, requirements));
        let empty_grants = super::capability::LocalCapabilityGrantSet::new();
        // A caller-supplied declaration must never replace the stored
        // requirements of a version-5 envelope.
        let declaration = super::capability::LocalCapabilityDeclaration::new(
            super::capability::LocalCapabilityName::StdSecretUse,
            super::capability::LocalCapabilityArgumentSource::Text("secret-1".to_owned()),
        );

        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(active.pair(), function),
            &[declaration],
            &empty_grants,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::CapabilityDenied {
                context,
                capability,
            } if context.function() == function && capability == "std.fs.read"
        ));
        assert_eq!(
            error.to_string(),
            "the CLIENT function requires the capability std.fs.read which is not granted"
        );
    }

    #[test]
    fn version_five_artifact_hash_is_checked_before_capability_decode() {
        let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
        )];
        let (active, function, pair, _) =
            version_five_boolean_active(version_five_boolean_envelope(true, requirements));
        let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
        let mut state = ClientStateStore::new();
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

        let error = super::evaluate_function(
            &untrusted,
            function,
            Vec::new(),
            &[],
            &capability::LocalCapabilityGrantSet::new(),
            &mut state,
            0,
            PrincipalId::from_bytes([0x7b; 16]),
            super::ObserverLineage::top_level(InvocationId::from_bytes([0xa2; 16])),
            &mut executor_slot,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidArtifact { context, .. }
                if context.pair() == pair && context.function() == function
        ));
    }

    #[test]
    fn version_five_stored_literal_capability_evaluates_when_covered() {
        let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
        )];
        let (active, function, pair, _) =
            version_five_boolean_active(version_five_boolean_envelope(true, requirements));
        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

        let result = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, function),
            &[],
            &grants,
        )
        .unwrap();

        assert_eq!(result.context().function(), function);
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn version_five_unknown_stored_capability_name_fails_closed() {
        let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.bogus.op",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("anything".to_owned()),
        )];
        let (active, function, _, _) =
            version_five_boolean_active(version_five_boolean_envelope(true, requirements));
        // Every vocabulary grant present: the unknown stored name still fails
        // closed and never falls back to an empty requirement set.
        let grants = super::capability::LocalCapabilityGrantSet::from_grants(
            super::capability::LocalCapabilityName::ALL
                .into_iter()
                .map(|name| {
                let scope = match name {
                    super::capability::LocalCapabilityName::StdFsRead
                    | super::capability::LocalCapabilityName::StdFsWrite => {
                        super::capability::LocalCapabilityScope::path("/home/bob").unwrap()
                    }
                    super::capability::LocalCapabilityName::StdNetConnect => {
                        super::capability::LocalCapabilityScope::host("example.com").unwrap()
                    }
                    super::capability::LocalCapabilityName::StdSecretUse => {
                        super::capability::LocalCapabilityScope::secret("secret-1").unwrap()
                    }
                };
                super::capability::LocalCapabilityGrant::new(name, scope).unwrap()
            }),
        )
        .unwrap();

        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(active.pair(), function),
            &[],
            &grants,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::CapabilityDenied {
                context,
                capability,
            } if context.function() == function && capability == "std.bogus.op"
        ));
    }

    #[test]
    fn version_five_stored_parameter_capability_resolves_the_invocation_argument() {
        let parameter_id = ParameterId::from_bytes([0xb1; 16]);
        let plan = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Expression(
                orna_artifact::client_plan::ExpressionClientPlan::new(
                    orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                        parameter: parameter_id,
                    },
                ),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Parameter(
                    "p_path".to_owned(),
                ),
            )],
        );
        let (active, function, pair, _, _) =
            version_five_expression_active_with_parameter(plan.encode().unwrap());
        let argument = orna_core::value::FunctionArgument::new(
            parameter_id,
            RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
        )
        .unwrap();

        let result = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorise(pair, function),
            &[argument],
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
        )
        .unwrap_err();

        assert!(matches!(
            &result,
            super::ClientExecutionError::CapabilityDenied { capability, .. }
                if capability == "std.fs.read"
        ));

        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let argument = orna_core::value::FunctionArgument::new(
            parameter_id,
            RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
        )
        .unwrap();

        let result = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorise(pair, function),
            &[argument],
            &[],
            &grants,
        )
        .unwrap();

        assert_eq!(
            result.value(),
            &RuntimeValue::Text("/home/bob/notes.txt".to_owned())
        );
    }

    #[test]
    fn version_five_recursive_calls_enforce_the_callee_capability() {
        let (base, caller_id, pair, caller_revision_id) = version_one_active(true);
        let callee_id = FunctionId::from_bytes([0xc2; 16]);
        let callee_revision_id = FunctionRevisionId::from_bytes([0xc3; 16]);
        let previous_revision = &base.function_revisions()[0];
        let caller_name = base
            .catalogue()
            .function_by_id(caller_id)
            .unwrap()
            .name()
            .clone();
        let caller_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Call {
                function: callee_id,
                arguments: Vec::new(),
            },
        );
        let caller_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Expression(caller_plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.write",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
            )],
        )
        .encode()
        .unwrap();
        let callee_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        );
        let callee_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Expression(callee_plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
            )],
        )
        .encode()
        .unwrap();
        let caller = FunctionDefinition::new(
            caller_id,
            caller_name,
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
            caller_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let callee = FunctionDefinition::new(
            callee_id,
            QualifiedSemanticName::new(["app", "callee"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
            callee_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            base.catalogue().revision(),
            base.catalogue().schemas().to_vec(),
            base.catalogue().object_types().to_vec(),
            vec![caller.clone(), callee.clone()],
        )
        .unwrap();
        let caller_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            caller_payload.clone(),
            artifact_payload_digest(&caller_payload).unwrap(),
        )
        .unwrap();
        let callee_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            callee_payload.clone(),
            artifact_payload_digest(&callee_payload).unwrap(),
        )
        .unwrap();
        let caller_reference = DefinitionReference::new(
            caller_id,
            caller_revision_id,
            0,
            DefinitionReferenceTarget::Function(callee_id),
            DefinitionReferenceKind::FunctionCall,
            previous_revision.declaration_origin(),
        );
        let caller_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &caller,
            previous_revision.language_version(),
            &caller_artifact,
            base.expressions(),
            std::slice::from_ref(&caller_reference),
        )
        .unwrap();
        let callee_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &callee,
            previous_revision.language_version(),
            &callee_artifact,
            base.expressions(),
            &[],
        )
        .unwrap();
        let caller_revision = FunctionRevisionRecord::new(
            caller_id,
            caller_revision_id,
            previous_revision.revision_number(),
            previous_revision.declaration_origin(),
            previous_revision.declaration_content_hash(),
            caller_semantic_hash,
            previous_revision.language_version(),
            caller_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let callee_origin = SourceOrigin::new(
            previous_revision.declaration_origin().source_unit(),
            previous_revision.declaration_origin().byte_start(),
            previous_revision.declaration_origin().byte_end(),
        )
        .unwrap();
        let callee_revision = FunctionRevisionRecord::new(
            callee_id,
            callee_revision_id,
            previous_revision.revision_number(),
            callee_origin,
            previous_revision.declaration_content_hash(),
            callee_semantic_hash,
            previous_revision.language_version(),
            callee_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let mut origins = base.origins().to_vec();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Function(callee_id),
            callee_origin,
        ));
        let revisions = vec![caller_revision, callee_revision];
        let references = vec![caller_reference];
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            &revisions,
            base.expressions(),
            &origins,
            &references,
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                base.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    base.expressions().to_vec(),
                    revisions,
                    origins,
                    references,
                ),
            ),
            context,
        )
        .unwrap();
        let write_grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsWrite,
            super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let write_only =
            super::capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, caller_id),
            &[],
            &write_only,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::CapabilityDenied {
                context,
                capability,
            } if context.function() == callee_id && capability == "std.fs.read"
        ));
        let read_grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants(
            write_only
                .as_slice()
                .iter()
                .cloned()
                .chain(std::iter::once(read_grant)),
        )
        .unwrap();
        let result = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, caller_id),
            &[],
            &grants,
        )
        .unwrap();
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn nested_call_preserves_caller_bound_capability_parameter() {
        let prepared = prepared_client_source(
            "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first(p_path TEXT) RETURNS TEXT RETURN app.second(); \
             CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
        );
        let initial = active_from_prepared_candidate(&prepared);
        let caller = initial
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.first")
            .expect("caller is present")
            .clone();
        let callee = initial
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.second")
            .expect("callee is present")
            .clone();
        let parameter = caller
            .parameters()
            .first()
            .expect("caller path parameter is present")
            .id();
        let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Call {
                function: callee.id(),
                arguments: Vec::new(),
            },
        )
        .encode()
        .expect("caller expression plan encodes");
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let current = initial
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == caller.id())
            .expect("caller revision is present");
        let caller_references = initial
            .references()
            .iter()
            .filter(|reference| reference.source_function() == caller.id())
            .cloned()
            .collect::<Vec<_>>();
        let semantic_hash = function_semantic_digest_with_version(
            current.semantic_hash_version(),
            &caller,
            current.language_version(),
            &artifact,
            initial.expressions(),
            &caller_references,
        )
        .unwrap();
        let replacement = FunctionRevisionRecord::new(
            caller.id(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            current.declaration_content_hash(),
            semantic_hash,
            current.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(current.semantic_hash_version());
        let revisions = initial
            .function_revisions()
            .iter()
            .map(|revision| {
                if revision.function() == caller.id() {
                    replacement.clone()
                } else {
                    revision.clone()
                }
            })
            .collect::<Vec<_>>();
        let catalogue_hash = catalogue_digest_with_context(
            initial.catalogue_hash_context(),
            initial.catalogue(),
            &revisions,
            initial.expressions(),
            initial.origins(),
            initial.references(),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                initial.pair(),
                initial.source().clone(),
                initial.catalogue().clone(),
                catalogue_hash,
                ActiveRevisionContent::new(
                    initial.expressions().to_vec(),
                    revisions,
                    initial.origins().to_vec(),
                    initial.references().to_vec(),
                ),
            ),
            initial.catalogue_hash_context().clone(),
        )
        .unwrap();
        let declaration = capability::LocalCapabilityDeclaration::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityArgumentSource::Parameter("p_path".to_owned()),
        );
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
        )
        .unwrap();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/home/bob").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

        let result = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorise(active.pair(), caller.id()),
            std::slice::from_ref(&argument),
            std::slice::from_ref(&declaration),
            &grants,
        )
        .expect("caller-scoped capability remains bound in the nested call");
        assert_eq!(result.value(), &RuntimeValue::Text("ok".to_owned()));

        let mismatched_grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let mismatched_grants = capability::LocalCapabilityGrantSet::from_grants([mismatched_grant]).unwrap();
        let error = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorise(active.pair(), caller.id()),
            &[argument],
            &[declaration],
            &mismatched_grants,
        )
        .expect_err("a mismatched caller scope still fails closed");
        assert!(matches!(
            error,
            super::ClientExecutionError::CapabilityDenied { context, capability }
                if context.function() == caller.id() && capability == "std.fs.read"
        ));
    }

    #[test]
    fn expression_calls_reject_targets_absent_from_the_active_reference_set() {
        let prepared = prepared_client_source(
            "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN app.second(); \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;",
        );
        let first = prepared
            .candidate()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.first")
            .expect("first function is present");
        let second = prepared
            .candidate()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.second")
            .expect("second function is present");
        let mut references = prepared.references().to_vec();
        let index = references
            .iter()
            .position(|reference| {
                reference.source_function() == first.id()
                    && reference.target() == DefinitionReferenceTarget::Function(second.id())
            })
            .expect("first call reference is present");
        let original = references[index].clone();
        references[index] = DefinitionReference::new(
            original.source_function(),
            original.source_revision(),
            original.ordinal(),
            DefinitionReferenceTarget::Function(first.id()),
            original.kind(),
            original.source_origin(),
        );
        let active = active_from_prepared_with_references(&prepared, references);

        let error = evaluate_client_function(&active, first.id()).unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::ExpressionEvaluation {
                context,
                source: super::ClientExpressionError::InvalidCall,
            } if context.function() == first.id()
        ));
    }

    fn assert_reordered_client_plan_rejects_before_executor(source: &str, function_name: &str) {
        let prepared = prepared_client_source_v6(source);
        let (active, function) = active_with_reordered_client_call_references(&prepared, function_name);
        let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(1)));
        let error = super::evaluate_client_function_with_executor(
            &active,
            &authorise(active.pair(), function),
            &mut executor,
        )
        .expect_err("the durable call sequence must be checked before execution");

        assert!(matches!(
            error,
            super::ClientExecutionError::ExpressionEvaluation {
                context,
                source: super::ClientExpressionError::InvalidCall,
            } if context.function() == function
        ));
        assert!(executor.executed.is_empty());
    }

    #[test]
    fn state_plan_preflights_defaults_before_return_expression() {
        assert_reordered_client_plan_rejects_before_executor(
            r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  STATE value INTEGER DEFAULT app.first();
  BEGIN RETURN app.second(); END;"#,
            "app.owner",
        );
    }

    #[test]
    fn procedural_plan_preflights_statements_before_return_expression() {
        assert_reordered_client_plan_rejects_before_executor(
            r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
    LET value INTEGER := app.first();
    value := app.second();
    RETURN value;
  END;"#,
            "app.owner",
        );
    }

    #[test]
    fn action_plan_preflights_arguments_before_operation_target() {
        assert_reordered_client_plan_rejects_before_executor(
            r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
    target => std.invoke.echo,
    arguments => std.call.args(p_value => app.first())
  );"#,
            "app.owner",
        );
    }

    #[test]
    fn action_plan_accepts_untampered_call_reference_order_and_builds_action() {
        let prepared = prepared_client_source_v6(
            r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
    target => std.invoke.echo,
    arguments => std.call.args(p_value => app.first())
  );"#,
        );
        let active = active_from_prepared_candidate(&prepared);
        let function = active
            .catalogue()
            .functions()
            .iter()
            .find(|candidate| candidate.name().to_string() == "app.owner")
            .expect("the action owner is present")
            .id();
        let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(7)));

        let result = super::evaluate_client_function_with_executor(
            &active,
            &authorise(active.pair(), function),
            &mut executor,
        )
        .expect("an untampered action plan evaluates successfully");

        assert!(matches!(result.value(), RuntimeValue::Opaque(_)));
        assert!(executor.executed.is_empty());
    }

    #[test]
    fn resource_plan_preflights_arguments_before_operation_target() {
        assert_reordered_client_plan_rejects_before_executor(
            r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
    RETURN AWAIT std.data.resource(
      target => std.invoke.echo,
      arguments => std.call.args(p_value => app.first())
    );
  END;"#,
            "app.owner",
        );
    }

    #[test]
    fn capability_expression_calls_reject_reference_sequence_mismatch() {
        let function = FunctionId::from_bytes([6; 16]);
        let call = || orna_artifact::client_plan::ClientExpressionNode::Call {
            function,
            arguments: Vec::new(),
        };
        let expression = orna_artifact::client_plan::ClientExpressionNode::Concat {
            left: Box::new(call()),
            right: Box::new(call()),
        };
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Expression(
                orna_artifact::client_plan::ExpressionClientPlan::new(expression),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
            )],
        )
        .encode()
        .expect("the capability expression plan encodes");
        let (active, function, pair, _) = version_two_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::Function(function),
            DefinitionReferenceKind::FunctionCall,
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            payload,
        );

        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(pair, function),
            &[],
            &grants,
        )
        .expect_err("the decoded call sequence must match durable references");

        assert!(matches!(
            error,
            super::ClientExecutionError::ExpressionEvaluation {
                context,
                source: super::ClientExpressionError::InvalidCall,
            } if context.function() == function
        ));
    }

    fn capability_direct_callee_denies_ungranted_declaration<F>(make_plan: F)
    where
        F: FnOnce(FunctionId) -> orna_artifact::client_plan::InnerClientPlan,
    {
        let prepared = prepared_client_source(
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.first() RETURNS TEXT RETURN app.second(); CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
        );
        let initial = active_from_prepared_candidate(&prepared);
        let caller = initial
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.first")
            .expect("caller is present")
            .clone();
        let callee = initial
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().to_string() == "app.second")
            .expect("callee is present")
            .clone();
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            make_plan(callee.id()),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.write",
                orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
            )],
        )
        .encode()
        .expect("the capability plan encodes");
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let current = initial
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == caller.id())
            .expect("caller revision is present");
        let caller_references = initial
            .references()
            .iter()
            .filter(|reference| reference.source_function() == caller.id())
            .cloned()
            .collect::<Vec<_>>();
        let semantic_hash = function_semantic_digest_with_version(
            current.semantic_hash_version(),
            &caller,
            current.language_version(),
            &artifact,
            initial.expressions(),
            &caller_references,
        )
        .unwrap();
        let replacement = FunctionRevisionRecord::new(
            caller.id(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            current.declaration_content_hash(),
            semantic_hash,
            current.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(current.semantic_hash_version());
        let revisions = initial
            .function_revisions()
            .iter()
            .map(|revision| {
                if revision.function() == caller.id() {
                    replacement.clone()
                } else {
                    revision.clone()
                }
            })
            .collect::<Vec<_>>();
        let catalogue_hash = catalogue_digest_with_context(
            initial.catalogue_hash_context(),
            initial.catalogue(),
            &revisions,
            initial.expressions(),
            initial.origins(),
            initial.references(),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                initial.pair(),
                initial.source().clone(),
                initial.catalogue().clone(),
                catalogue_hash,
                ActiveRevisionContent::new(
                    initial.expressions().to_vec(),
                    revisions,
                    initial.origins().to_vec(),
                    initial.references().to_vec(),
                ),
            ),
            initial.catalogue_hash_context().clone(),
        )
        .unwrap();
        let declaration = capability::LocalCapabilityDeclaration::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityArgumentSource::Text("/tmp".to_owned()),
        );
        let write_grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsWrite,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
        let error = super::evaluate_client_function_with_grants(
            &active,
            &authorise(active.pair(), caller.id()),
            &[declaration],
            &grants,
        )
        .expect_err("the direct callee must inherit the checked declaration context");
        assert!(matches!(
            error,
            super::ClientExecutionError::CapabilityDenied { context, capability }
                if context.function() == callee.id() && capability == "std.fs.read"
        ));
    }

    #[test]
    fn capability_expression_calls_preserve_declarations_for_direct_callees() {
        capability_direct_callee_denies_ungranted_declaration(|callee| {
            orna_artifact::client_plan::InnerClientPlan::Expression(
                orna_artifact::client_plan::ExpressionClientPlan::new(
                    orna_artifact::client_plan::ClientExpressionNode::Call {
                        function: callee,
                        arguments: Vec::new(),
                    },
                ),
            )
        });
    }

    #[test]
    fn capability_procedural_calls_preserve_declarations_for_direct_callees() {
        capability_direct_callee_denies_ungranted_declaration(|callee| {
            orna_artifact::client_plan::InnerClientPlan::Procedural(
                orna_artifact::client_plan::ProceduralClientPlan::new(
                    Vec::new(),
                    Vec::new(),
                    orna_artifact::client_plan::ClientExpressionNode::Call {
                        function: callee,
                        arguments: Vec::new(),
                    },
                ),
            )
        });
    }

    #[test]
    fn transfers_the_evaluated_value_without_cloning_its_payload() {
        let (active, function, _, _) = version_one_active(true);

        assert_eq!(
            evaluate_client_function(&active, function)
                .unwrap()
                .into_value(),
            RuntimeValue::Boolean(true),
        );
    }

    #[test]
    fn rejects_mismatched_authorisation_before_active_revision_validation() {
        let (active, function, pair, _) = version_one_active(true);
        let other_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x7b; 16]),
            CatalogueRevisionId::from_bytes([0x7c; 16]),
        );
        let untrusted = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            orna_core::revision::Sha256Digest::from_bytes([0x7d; 32]),
            active.expressions().to_vec(),
            active.function_revisions().to_vec(),
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .expect("tampered hash remains structurally valid");

        let error = super::evaluate_client_function(&untrusted, &authorise(other_pair, function))
            .expect_err("mismatched authorisation must fail");

        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), function);
        assert_eq!(error.context(), None);
        assert_eq!(
            error.to_string(),
            "the CLIENT authorisation does not match the active revision"
        );
        assert!(matches!(
            error,
            super::ClientExecutionError::AuthorisationMismatch {
                authorised,
                active,
            } if authorised == InvocationTarget::new(function, other_pair) && active == pair
        ));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn rejects_an_active_revision_with_a_stale_catalogue_hash_before_function_checks() {
        let (active, _, pair, _) = version_one_active(true);
        let requested = FunctionId::from_bytes([0x8c; 16]);
        let stale = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            orna_core::revision::Sha256Digest::from_bytes([0x8a; 32]),
            active.expressions().to_vec(),
            active.function_revisions().to_vec(),
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();

        let error = evaluate_client_function(&stale, requested).unwrap_err();

        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), requested);
        assert_eq!(error.context(), None);
        assert_eq!(error.to_string(), "the active revision cannot be trusted");
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidActiveRevision {
                source: super::ClientActiveRevisionError::CatalogueHashMismatch,
                ..
            }
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn wraps_a_canonical_active_semantics_failure_before_function_checks() {
        let (active, function, pair, function_revision) = version_one_active(true);
        let original = &active.function_revisions()[0];
        let inconsistent_revision = FunctionRevisionRecord::new(
            function,
            function_revision,
            original.revision_number(),
            original.declaration_origin(),
            original.declaration_content_hash(),
            orna_core::revision::Sha256Digest::from_bytes([0x8b; 32]),
            original.language_version(),
            original.artifact().clone(),
        )
        .unwrap();
        let untrusted = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            active.expressions().to_vec(),
            vec![inconsistent_revision],
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();

        let error = evaluate_client_function(&untrusted, function).unwrap_err();

        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), function);
        assert_eq!(error.context(), None);
        assert_eq!(error.to_string(), "the active revision cannot be trusted");
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidActiveRevision {
                source: super::ClientActiveRevisionError::Canonical(
                    orna_core::canonical_hash::CanonicalHashError::FunctionSemanticHashMismatch {
                        function: actual_function,
                        revision: actual_revision,
                    }
                ),
                ..
            } if actual_function == function && actual_revision == function_revision
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn rejects_a_mismatched_function_artifact_payload_hash_before_function_checks() {
        let (active, _, pair, _) = version_one_active(true);
        let requested = FunctionId::from_bytes([0x8d; 16]);
        let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);

        let error = evaluate_client_function(&untrusted, requested).unwrap_err();

        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), requested);
        assert_eq!(error.context(), None);
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidActiveRevision {
                source: super::ClientActiveRevisionError::Canonical(
                    orna_core::canonical_hash::CanonicalHashError::ArtifactPayloadHashMismatch {
                        artifact: "function artifact",
                    }
                ),
                ..
            }
        ));
        assert_eq!(error.to_string(), "the active revision cannot be trusted");
        let source = std::error::Error::source(&error).unwrap();
        assert_eq!(
            source.to_string(),
            "function artifact payload hash differs from exact payload"
        );
        assert!(std::error::Error::source(source).is_some());
    }

    #[test]
    fn client_evaluator_rejects_mismatched_payload_hash_before_resource_execution() {
        let (active, function, pair, _) = version_one_active(true);
        let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
        let mut state = ClientStateStore::new();
        let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Boolean(true)));
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);

        let error = super::evaluate_function(
            &untrusted,
            function,
            Vec::new(),
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            0,
            PrincipalId::from_bytes([0x7a; 16]),
            super::ObserverLineage::top_level(InvocationId::from_bytes([0xa1; 16])),
            &mut executor_slot,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidArtifact { context, .. }
                if context.pair() == pair && context.function() == function
        ));
        assert!(executor.executed.is_empty());
        assert!(executor.cancelled.is_empty());
    }

    #[test]
    fn client_artifact_guard_rejects_server_kind_with_client_payload() {
        let (_active, function, pair, function_revision) = version_one_active(true);
        let payload = orna_artifact::client_plan::ClientPlan::return_boolean(true).encode();
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.client-plan",
            orna_artifact::client_plan::FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let context = super::ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: InvocationId::from_bytes([0xa2; 16]),
            observer_lineage: None,
        };

        let error = super::validate_artifact_identity(&artifact, context).unwrap_err();

        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidArtifact { context: actual, .. }
                if actual == context
        ));
        assert_eq!(error.to_string(), "the saved CLIENT function cannot be evaluated");
    }

    #[test]
    fn public_active_revision_construction_preserves_client_evaluator_boundaries() {
        let (version_one, function, _, function_revision) = version_one_active(true);
        let value_type = TypeId::from_bytes([0x93; 16]);
        let value_reference = DefinitionReference::new(
            function,
            function_revision,
            0,
            DefinitionReferenceTarget::ValueType(value_type),
            DefinitionReferenceKind::NamedType,
            version_one.function_revisions()[0].declaration_origin(),
        );
        let version_two_revision = version_one.function_revisions()[0]
            .clone()
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let error = ActiveDatabaseRevision::new(
            version_one.pair(),
            version_one.source().clone(),
            version_one.catalogue().clone(),
            version_one.catalogue_hash(),
            version_one.expressions().to_vec(),
            vec![version_two_revision],
            version_one.origins().to_vec(),
            vec![value_reference],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                function: actual_function,
                revision: actual_revision,
                target,
            } if actual_function == function && actual_revision == function_revision && target == value_type
        ));
        assert_eq!(
            error.to_string(),
            "value-type references require catalogue hash version 2"
        );
        assert!(std::error::Error::source(&error).is_none());

        let error = ActiveDatabaseRevision::new(
            version_one.pair(),
            version_one.source().clone(),
            version_one.catalogue().clone(),
            version_one.catalogue_hash(),
            version_one.expressions().to_vec(),
            vec![
                version_one.function_revisions()[0]
                    .clone()
                    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
            ],
            version_one.origins().to_vec(),
            Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                function: actual_function,
                revision: actual_revision,
            } if actual_function == function && actual_revision == function_revision
        ));
        assert_eq!(
            error.to_string(),
            "function semantic hash version 2 requires catalogue hash version 2"
        );
        assert!(std::error::Error::source(&error).is_none());

        let missing_target = TypeId::from_bytes([0x92; 16]);
        let error = ActiveDatabaseRevision::new(
            version_one.pair(),
            version_one.source().clone(),
            version_one.catalogue().clone(),
            version_one.catalogue_hash(),
            version_one.expressions().to_vec(),
            version_one.function_revisions().to_vec(),
            version_one.origins().to_vec(),
            vec![DefinitionReference::new(
                function,
                function_revision,
                0,
                DefinitionReferenceTarget::ObjectType(missing_target),
                DefinitionReferenceKind::ObjectReference,
                version_one.function_revisions()[0].declaration_origin(),
            )],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ReferenceTargetNotInRevision {
                target: DefinitionReferenceTarget::ObjectType(target),
            } if target == missing_target
        ));
        assert_eq!(
            error.to_string(),
            "reference target is absent from revision"
        );
        assert!(std::error::Error::source(&error).is_none());

        let prepared = prepared_client_functions();
        let active = active_from_prepared_candidate(&prepared);
        let prepared_function = active.catalogue().functions()[0].id();
        let current_revision = active.catalogue().functions()[0].current_revision();
        let selected = active
            .references()
            .iter()
            .find(|reference| reference.source_function() == prepared_function)
            .unwrap();
        assert!(matches!(
            selected.target(),
            DefinitionReferenceTarget::ValueType(_)
        ));
        let selected_target = match selected.target() {
            DefinitionReferenceTarget::ValueType(target) => target,
            _ => TypeId::from_bytes([0; 16]),
        };
        let unavailable_revision = FunctionRevisionId::from_bytes([0x94; 16]);
        let unavailable_reference = DefinitionReference::new(
            prepared_function,
            unavailable_revision,
            selected.ordinal(),
            selected.target(),
            selected.kind(),
            selected.source_origin(),
        );
        let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                active.catalogue_hash(),
                ActiveRevisionContent::new(
                    active.expressions().to_vec(),
                    active.function_revisions().to_vec(),
                    active.origins().to_vec(),
                    vec![unavailable_reference],
                ),
            ),
            active.catalogue_hash_context().clone(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
                function: actual_function,
                revision,
                target,
            } if actual_function == prepared_function && revision == unavailable_revision && target == selected_target
        ));
        assert_eq!(
            error.to_string(),
            "cannot verify a value-type reference without its function revision record"
        );
        assert!(std::error::Error::source(&error).is_none());

        let version_one_revisions = active
            .function_revisions()
            .iter()
            .cloned()
            .map(|revision| {
                revision.with_semantic_hash_version(FunctionSemanticHashVersion::Version1)
            })
            .collect::<Vec<_>>();
        let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                active.catalogue_hash(),
                ActiveRevisionContent::new(
                    active.expressions().to_vec(),
                    version_one_revisions,
                    active.origins().to_vec(),
                    active.references().to_vec(),
                ),
            ),
            active.catalogue_hash_context().clone(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                function: actual_function,
                revision,
                target,
            } if actual_function == prepared_function && revision == current_revision && target == selected_target
        ));
        assert_eq!(
            error.to_string(),
            "value-type references require function semantic hash version 2"
        );
        assert!(std::error::Error::source(&error).is_none());

        let object = active.catalogue().object_types()[0].id();
        let kind_mismatch = DefinitionReference::new(
            prepared_function,
            current_revision,
            97,
            DefinitionReferenceTarget::ValueType(selected_target),
            DefinitionReferenceKind::ObjectReference,
            selected.source_origin(),
        );
        let error = active_with_extra_reference(&active, kind_mismatch).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ReferenceKindTargetMismatch {
                kind: DefinitionReferenceKind::ObjectReference,
                target: DefinitionReferenceTarget::ValueType(target),
            } if target == selected_target
        ));
        assert_eq!(
            error.to_string(),
            "reference kind cannot target that definition kind"
        );
        assert!(std::error::Error::source(&error).is_none());

        let duplicate = DefinitionReference::new(
            selected.source_function(),
            selected.source_revision(),
            selected.ordinal(),
            selected.target(),
            selected.kind(),
            selected.source_origin(),
        );
        let error = active_with_extra_reference(&active, duplicate).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::DuplicateReferenceOrdinal { revision, ordinal }
                if revision == current_revision && ordinal == selected.ordinal()
        ));
        assert_eq!(error.to_string(), "duplicate reference ordinal");
        assert!(std::error::Error::source(&error).is_none());

        let reference_not_in_catalogue = DefinitionReference::new(
            FunctionId::from_bytes([0x95; 16]),
            FunctionRevisionId::from_bytes([0x96; 16]),
            99,
            DefinitionReferenceTarget::ObjectType(object),
            DefinitionReferenceKind::ObjectReference,
            selected.source_origin(),
        );
        let error = active_with_extra_reference(&active, reference_not_in_catalogue).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ReferenceFunctionNotInCatalogue {
                function: actual_function,
                revision,
            } if actual_function == FunctionId::from_bytes([0x95; 16])
                && revision == FunctionRevisionId::from_bytes([0x96; 16])
        ));
        assert_eq!(
            error.to_string(),
            "reference function is absent from catalogue"
        );
        assert!(std::error::Error::source(&error).is_none());

        let stale_revision = FunctionRevisionId::from_bytes([0x97; 16]);
        let non_current_reference = DefinitionReference::new(
            prepared_function,
            stale_revision,
            99,
            DefinitionReferenceTarget::ObjectType(object),
            DefinitionReferenceKind::ObjectReference,
            selected.source_origin(),
        );
        let error = active_with_extra_reference(&active, non_current_reference).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::ReferenceRevisionNotCurrent {
                function: actual_function,
                expected,
                actual,
            } if actual_function == prepared_function && expected == current_revision && actual == stale_revision
        ));
        assert_eq!(
            error.to_string(),
            "reference revision is not catalogue current revision"
        );
        assert!(std::error::Error::source(&error).is_none());

        let unit_not_in_revision =
            SourceOrigin::new(SourceUnitId::from_bytes([0x98; 16]), 0, 0).unwrap();
        let error =
            active_with_replaced_first_origin(&version_one, unit_not_in_revision).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit }
                if source_unit == SourceUnitId::from_bytes([0x98; 16])
        ));
        assert_eq!(
            error.to_string(),
            "source origin unit is absent from stored revision"
        );
        assert!(std::error::Error::source(&error).is_none());

        let source_unit = version_one.source().units()[0].id();
        let out_of_bounds = SourceOrigin::new(
            source_unit,
            0,
            u32::try_from(version_one.source().units()[0].content().len() + 1).unwrap(),
        )
        .unwrap();
        let error = active_with_replaced_first_origin(&version_one, out_of_bounds).unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginOutOfBounds {
                source_unit: actual_unit,
                byte_start: 0,
                ..
            } if actual_unit == source_unit
        ));
        assert_eq!(
            error.to_string(),
            "source origin is outside stored source content"
        );
        assert!(std::error::Error::source(&error).is_none());

        let unicode_source = replacement_source(&version_one, "é");
        let split_character = SourceOrigin::new(source_unit, 1, 1).unwrap();
        let error =
            active_with_source_and_first_origin(&version_one, unicode_source, split_character)
                .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginNotCharacterBoundary {
                source_unit: actual_unit,
                byte_start: 1,
                byte_end: 1,
            } if actual_unit == source_unit
        ));
        assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn public_active_revision_construction_rejects_invalid_reference_source_origins() {
        let prepared = prepared_client_functions();
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();
        let source_unit = active.source().units()[0].id();

        let error = active_with_replaced_reference_origin(
            &active,
            active.source().clone(),
            function,
            SourceOrigin::new(SourceUnitId::from_bytes([0x99; 16]), 0, 0).unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit: actual }
                if actual == SourceUnitId::from_bytes([0x99; 16])
        ));
        assert_eq!(
            error.to_string(),
            "source origin unit is absent from stored revision"
        );
        assert!(std::error::Error::source(&error).is_none());

        let error = active_with_replaced_reference_origin(
            &active,
            active.source().clone(),
            function,
            SourceOrigin::new(
                source_unit,
                0,
                u32::try_from(active.source().units()[0].content().len() + 1).unwrap(),
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginOutOfBounds {
                source_unit: actual,
                byte_start: 0,
                ..
            } if actual == source_unit
        ));
        assert_eq!(
            error.to_string(),
            "source origin is outside stored source content"
        );
        assert!(std::error::Error::source(&error).is_none());

        let unicode_source = replacement_source(
            &active,
            &format!("{}é", active.source().units()[0].content()),
        );
        let original_length = active.source().units()[0].content().len();
        let error = active_with_replaced_reference_origin(
            &active,
            unicode_source,
            function,
            SourceOrigin::new(
                source_unit,
                u32::try_from(original_length + 1).unwrap(),
                u32::try_from(original_length + 1).unwrap(),
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RevisionInvariantError::SourceOriginNotCharacterBoundary {
                source_unit: actual,
                byte_start,
                byte_end,
            } if actual == source_unit
                && byte_start == u32::try_from(original_length + 1).unwrap()
                && byte_end == u32::try_from(original_length + 1).unwrap()
        ));
        assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn stream_expression_rejects_scalar_literal_plan() {
        let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap();
        let (active, function, _, _) = version_two_client_stream_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            payload,
        );
        let error = evaluate_client_function(&active, function).unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::ExpressionEvaluation {
                source: super::ClientExpressionError::TypeMismatch,
                ..
            }
        ));
    }

    #[test]
    fn stream_artifact_versions_reject_scalar_roots() {
        let scalar = orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true };
        for (artifact_version, payload) in [
            (
                orna_artifact::client_plan::STATE_FORMAT_VERSION,
                orna_artifact::client_plan::StateClientPlan::new(
                    scalar.clone(),
                    vec![orna_artifact::client_plan::StateSlot::new(
                        StateSlotId::from_bytes([0x21; 16]),
                        orna_standard::BOOLEAN_TYPE_ID,
                        orna_artifact::client_plan::StateScope::User,
                        orna_artifact::client_plan::StateDefault::Unset,
                    )],
                )
                    .encode()
                    .expect("the state plan encodes"),
            ),
            (
                orna_artifact::client_plan::RESOURCE_FORMAT_VERSION,
                orna_artifact::client_plan::ResourceClientPlan::new(
                    orna_artifact::client_plan::ClientExpressionNode::Await {
                        expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation: orna_artifact::client_plan::ResourceOperationNode::new(
                                orna_artifact::client_plan::ResourceKind::Scalar,
                                FunctionId::from_bytes([6; 16]),
                                RevisionPair::new(
                                    SourceRevisionId::from_bytes([1; 16]),
                                    CatalogueRevisionId::from_bytes([2; 16]),
                                ),
                                CallSiteId::from_bytes([0xe1; 16]),
                                Vec::new(),
                                orna_standard::BOOLEAN_TYPE_ID,
                            ),
                        }),
                    },
                )
                .encode()
                .expect("the resource plan encodes"),
            ),
            (
                orna_artifact::client_plan::PROCEDURAL_FORMAT_VERSION,
                orna_artifact::client_plan::ProceduralClientPlan::new(
                    Vec::new(),
                    Vec::new(),
                    scalar,
                )
                .encode()
                .expect("the procedural plan encodes"),
            ),
        ] {
            let (active, function, _, _) = version_two_client_stream_active_with_artifact(
                standard_v6(),
                orna_standard::BOOLEAN_TYPE_ID,
                DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
                DefinitionReferenceKind::NamedType,
                artifact_version,
                payload,
            );
            let error = evaluate_client_function(&active, function).unwrap_err();
            if artifact_version == orna_artifact::client_plan::RESOURCE_FORMAT_VERSION {
                assert!(matches!(
                    error,
                    super::ClientExecutionError::ExpressionEvaluation {
                        source: super::ClientExpressionError::InvalidCall,
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    error,
                    super::ClientExecutionError::ExpressionEvaluation {
                        source: super::ClientExpressionError::TypeMismatch,
                        ..
                    }
                ));
            }
        }
    }

    #[test]
    fn prepared_client_stream_shape_reaches_runtime_contract_boundary() {
        let prepared = prepared_client_source(
            "CREATE SCHEMA app; \
             CREATE EXTERNAL CLIENT FUNCTION app.events() \
             RETURNS STREAM<BOOLEAN> RUNTIME CONTRACT 'app.events@1';",
        );
        let active = active_from_prepared_candidate(&prepared);
        let definition = &active.catalogue().functions()[0];
        assert!(matches!(
            definition.return_type(),
            FunctionReturn::Stream(ResolvedType::Value(type_id))
                if *type_id == orna_standard::BOOLEAN_TYPE_ID
        ));
        let function = definition.id();
        let error = evaluate_client_function(&active, function).unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::ExternalContract { identity, .. }
                if identity == "app.events@1"
        ));
    }

    #[test]
    fn compiler_emitted_v5_capability_gate_fails_closed_before_runtime() {
        let prepared = prepared_client_source(
            "CREATE SCHEMA app; \
             CREATE EXTERNAL CLIENT FUNCTION app.read() \
             RETURNS BOOLEAN RUNTIME CONTRACT 'std.fs.read@1' \
             REQUIRES CAPABILITY std.fs.read('/tmp/input');",
        );
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();
        let authorisation = authorise(active.pair(), function);

        let missing = super::evaluate_client_function_with_grants(
            &active,
            &authorisation,
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            super::ClientExecutionError::CapabilityDenied { capability, .. }
                if capability == "std.fs.read"
        ));

        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        // The local grant passes. The runtime contract is not installed in this evaluator,
        // so the next error must be the external-contract boundary.
        let unavailable =
            super::evaluate_client_function_with_grants(&active, &authorisation, &[], &grants)
                .unwrap_err();

        assert!(matches!(
            unavailable,
            super::ClientExecutionError::ExternalContract { identity, .. }
                if identity == "std.fs.read@1"
        ));
    }

    #[test]
    fn evaluates_a_version_five_expression_parameter_read() {
        use orna_artifact::client_plan::{
            CapabilityArgumentSource, CapabilityClientPlan, CapabilityRequirement,
            ClientExpressionNode, ExpressionClientPlan, InnerClientPlan,
        };

        let parameter = ParameterId::from_bytes([0xb1; 16]);
        let payload = CapabilityClientPlan::new(
            InnerClientPlan::Expression(ExpressionClientPlan::new(
                ClientExpressionNode::ParameterRead { parameter },
            )),
            vec![CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Parameter("p_path".to_owned()),
            )],
        )
        .encode()
        .expect("the expression capability plan encodes");
        let (active, function, pair, _, parameter) =
            version_five_expression_active_with_parameter(payload);
        let authorisation = authorise(pair, function);
        let argument =
            FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/input".to_owned())).unwrap();
        let grant = super::capability::LocalCapabilityGrant::new(
            super::capability::LocalCapabilityName::StdFsRead,
            super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap();
        let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

        let result = super::evaluate_client_function_with_grants_and_arguments(
            &active,
            &authorisation,
            std::slice::from_ref(&argument),
            &[],
            &grants,
        )
        .expect("the version-five expression evaluates");

        assert_eq!(result.value(), &RuntimeValue::Text("/tmp/input".to_owned()));
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().pair(), active.pair());
    }

    #[test]
    fn evaluates_prepared_version_two_client_constants() {
        for (literal, expected) in [("TRUE", true), ("FALSE", false)] {
            let prepared = prepared_client_constant(literal);
            let active = active_from_prepared_candidate(&prepared);
            let function = active.catalogue().functions()[0].id();

            let result = evaluate_client_function(&active, function).unwrap();

            assert_eq!(result.context().pair(), active.pair());
            assert_eq!(result.context().function(), function);
            assert_eq!(
                result.context().function_revision(),
                active.catalogue().functions()[0].current_revision()
            );
            assert_eq!(result.value(), &RuntimeValue::Boolean(expected));
        }
    }

    #[test]
    fn evaluates_a_hand_built_version_two_value_return() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let boolean_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| {
                definition.representation_contract() == "orna.kernel.value.boolean@1"
            })
            .unwrap()
            .id();
        let (active, function, pair, function_revision) =
            version_two_value_active(boolean_type, boolean_type);
        assert_eq!(
            active.function_revisions()[0].artifact().payload(),
            b"ORNACP\0\0\0\0\0\x01\x01\x01"
        );

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), pair);
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().function_revision(), function_revision);
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn evaluates_a_registered_opaque_client_result() {
        let payload = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let (active, function, pair, function_revision) =
            version_two_opaque_active(orna_standard::OPAQUE_TOKEN_TYPE_ID, payload);

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), pair);
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().function_revision(), function_revision);
        let RuntimeValue::Opaque(value) = result.value() else {
            panic!("opaque plan must produce one opaque value");
        };
        assert_eq!(value.opaque_type(), orna_standard::OPAQUE_TOKEN_TYPE_ID);
        assert_eq!(value.canonical_payload(), payload);
    }

    #[test]
    fn opaque_client_result_rejects_plan_type_and_structure_before_value_creation() {
        let payload = [0x5a; 16];
        let wrong_type = TypeId::from_bytes([0xa7; 16]);
        let (active, function, pair, function_revision) =
            version_two_opaque_active(wrong_type, payload);

        let error = evaluate_client_function(&active, function).unwrap_err();
        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), function);
        assert_eq!(
            error.context().map(|context| context.function_revision()),
            Some(function_revision)
        );
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidOpaqueValue {
                source: super::ClientOpaqueValueError::TypeMismatch {
                    expected,
                    actual,
                },
                ..
            } if expected == orna_standard::OPAQUE_TOKEN_TYPE_ID && actual == wrong_type
        ));
        assert_eq!(
            error.to_string(),
            "the saved CLIENT function cannot be evaluated"
        );
        let source = std::error::Error::source(&error).unwrap();
        assert_eq!(
            source.to_string(),
            "opaque CLIENT plan type does not match its function return"
        );
        assert!(std::error::Error::source(source).is_none());

        let mut malformed = orna_artifact::client_plan::OpaqueClientPlan::return_opaque(
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
            payload,
        )
        .encode();
        malformed[29..33].copy_from_slice(&15_u32.to_be_bytes());
        let (active, function, _, _) = version_two_value_active_with_artifact(
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
            2,
            malformed,
        );
        let error = evaluate_client_function(&active, function).unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidArtifact {
                source: orna_artifact::client_plan::ClientPlanError::InvalidOpaquePayloadLength {
                    actual: 15,
                },
                ..
            }
        ));
    }

    #[test]
    fn rejects_a_value_return_that_disagrees_with_its_selected_reference() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let boolean_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| {
                definition.representation_contract() == "orna.kernel.value.boolean@1"
            })
            .unwrap()
            .id();
        let alternate_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| definition.id() != boolean_type)
            .unwrap()
            .id();
        let (active, function, pair, function_revision) =
            version_two_value_active(alternate_type, boolean_type);

        let error = evaluate_client_function(&active, function).unwrap_err();

        assert_eq!(error.pair(), pair);
        assert_eq!(error.function(), function);
        let context = error.context().expect("invalid function error context");
        assert_eq!(context.pair(), pair);
        assert_eq!(context.function(), function);
        assert_eq!(context.function_revision(), function_revision);
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidFunction {
                rule: super::ClientExecutionRule::References,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "this CLIENT function depends on unsupported definitions"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn version_two_reference_validation_uses_only_the_selected_current_function() {
        let prepared = prepared_client_functions();
        let active = active_from_prepared_candidate(&prepared);
        let functions = active.catalogue().functions();
        let first = functions[0].id();
        let second = functions[1].id();

        let result = evaluate_client_function(&active, first).unwrap();
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));

        let references = active
            .references()
            .iter()
            .filter(|reference| reference.source_function() == second)
            .cloned()
            .collect::<Vec<_>>();
        let b_only = active_from_prepared_with_references(&prepared, references);

        assert_references_rule(evaluate_client_function(&b_only, first), first);
        assert_eq!(
            evaluate_client_function(&b_only, second).unwrap().value(),
            &RuntimeValue::Boolean(true)
        );
    }

    #[test]
    fn accepts_a_rehashed_self_consistent_selected_reference_origin() {
        let prepared = prepared_client_functions();
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();
        let revision = active.catalogue().functions()[0].current_revision();
        let source = active.source().units()[0].content();
        let body_start = source.find("TRUE").unwrap();
        let replacement_origin = SourceOrigin::new(
            active.source().units()[0].id(),
            u32::try_from(body_start).unwrap(),
            u32::try_from(body_start + "TRUE".len()).unwrap(),
        )
        .unwrap();
        let mut references = active.references().to_vec();
        replace_reference(&mut references, function, |reference| {
            DefinitionReference::new(
                reference.source_function(),
                reference.source_revision(),
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                replacement_origin,
            )
        });

        let stale = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                active.catalogue_hash(),
                ActiveRevisionContent::new(
                    active.expressions().to_vec(),
                    active.function_revisions().to_vec(),
                    active.origins().to_vec(),
                    references.clone(),
                ),
            ),
            active.catalogue_hash_context().clone(),
        )
        .unwrap();
        let error = evaluate_client_function(&stale, function).unwrap_err();
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidActiveRevision {
                source: super::ClientActiveRevisionError::CatalogueHashMismatch,
                ..
            }
        ));
        assert_eq!(error.pair(), active.pair());
        assert_eq!(error.function(), function);
        assert_eq!(error.context(), None);
        assert_eq!(error.to_string(), "the active revision cannot be trusted");
        assert!(std::error::Error::source(&error).is_some());

        let repaired = active_from_prepared_with_references(&prepared, references);
        let result = evaluate_client_function(&repaired, function).unwrap();
        assert_eq!(result.context().pair(), repaired.pair());
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().function_revision(), revision);
        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    }

    #[test]
    fn version_two_rejects_each_publicly_constructible_selected_reference_mismatch() {
        let prepared = prepared_client_functions();
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();
        let reference = active
            .references()
            .iter()
            .find(|reference| reference.source_function() == function)
            .unwrap();
        assert!(matches!(
            active.catalogue_hash_context(),
            orna_core::revision::CatalogueHashContext::Version2 { .. }
        ));
        let standard = active.catalogue_hash_context().standard().unwrap();
        let alternate_value_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|value_type| {
                value_type.representation_contract() != "orna.kernel.value.boolean@1"
            })
            .unwrap()
            .id();
        let object = active.catalogue().object_types()[0].id();

        let missing = active
            .references()
            .iter()
            .filter(|candidate| candidate.source_function() != function)
            .cloned()
            .collect::<Vec<_>>();
        assert_references_rule(
            evaluate_client_function(
                &active_from_prepared_with_references(&prepared, missing),
                function,
            ),
            function,
        );

        let mut extra = active.references().to_vec();
        extra.push(DefinitionReference::new(
            reference.source_function(),
            reference.source_revision(),
            1,
            reference.target(),
            reference.kind(),
            reference.source_origin(),
        ));
        assert_references_rule(
            evaluate_client_function(
                &active_from_prepared_with_references(&prepared, extra),
                function,
            ),
            function,
        );

        let mut wrong_ordinal = active.references().to_vec();
        replace_reference(&mut wrong_ordinal, function, |candidate| {
            DefinitionReference::new(
                candidate.source_function(),
                candidate.source_revision(),
                1,
                candidate.target(),
                candidate.kind(),
                candidate.source_origin(),
            )
        });
        let error = active_from_prepared_with_references_result(&prepared, wrong_ordinal)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<RevisionInvariantError>(),
            Some(RevisionInvariantError::ReferenceOrdinalOutOfSequence {
                expected: 0,
                actual: 1,
                ..
            })
        ));

        let mut wrong_target = active.references().to_vec();
        replace_reference(&mut wrong_target, function, |candidate| {
            DefinitionReference::new(
                candidate.source_function(),
                candidate.source_revision(),
                candidate.ordinal(),
                DefinitionReferenceTarget::ValueType(alternate_value_type),
                candidate.kind(),
                candidate.source_origin(),
            )
        });
        assert_references_rule(
            evaluate_client_function(
                &active_from_prepared_with_references(&prepared, wrong_target),
                function,
            ),
            function,
        );

        let mut wrong_kind_and_target = active.references().to_vec();
        replace_reference(&mut wrong_kind_and_target, function, |candidate| {
            DefinitionReference::new(
                candidate.source_function(),
                candidate.source_revision(),
                candidate.ordinal(),
                DefinitionReferenceTarget::ObjectType(object),
                DefinitionReferenceKind::ObjectReference,
                candidate.source_origin(),
            )
        });
        assert_references_rule(
            evaluate_client_function(
                &active_from_prepared_with_references(&prepared, wrong_kind_and_target),
                function,
            ),
            function,
        );

        let semantic_version_one = active_from_prepared_with_semantic_versions(
            &prepared,
            FunctionSemanticHashVersion::Version1,
            Vec::new(),
        );
        assert_references_rule(
            evaluate_client_function(&semantic_version_one, function),
            function,
        );
    }

    #[test]
    fn expression_like_reference_validation_accepts_declared_ref_parameter_object_references() {
        let function_id = FunctionId::from_bytes([0xd1; 16]);
        let function_revision = FunctionRevisionId::from_bytes([0xd2; 16]);
        let parameter_id = ParameterId::from_bytes([0xd3; 16]);
        let object_type = TypeId::from_bytes([0xd4; 16]);
        let function = FunctionDefinition::new(
            function_id,
            QualifiedSemanticName::new(["action_fixture", "call"]).unwrap(),
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                parameter_id,
                "p_value",
                0,
                ResolvedType::reference(object_type),
                None,
            )],
            FunctionReturn::Single(ResolvedType::Value(orna_standard::STD_ACTION_TYPE_ID)),
            function_revision,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let source_origin =
            SourceOrigin::new(SourceUnitId::from_bytes([0xd5; 16]), 0, 0).unwrap();
        let reference = |kind, target| {
            DefinitionReference::new(
                function_id,
                function_revision,
                0,
                target,
                kind,
                source_origin,
            )
        };

        assert!(super::is_expression_reference_allowed(
            Some(&function),
            &reference(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(object_type),
            ),
        ));
        assert!(!super::is_expression_reference_allowed(
            Some(&function),
            &reference(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(TypeId::from_bytes([0xd6; 16])),
            ),
        ));
        assert!(!super::is_expression_reference_allowed(
            Some(&function),
            &reference(
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(object_type),
            ),
        ));
        assert!(!super::is_expression_reference_allowed(
            Some(&function),
            &reference(
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(object_type),
            ),
        ));
    }

    #[test]
    fn public_errors_and_rules_preserve_the_closed_adr0015_surface() {
        use orna_artifact::client_plan::ClientPlan;

        use super::{
            ClientActiveRevisionError, ClientExecutionContext, ClientExecutionError,
            ClientExecutionRule, ClientOpaqueValueError,
        };

        let (active, function, pair, function_revision) = version_one_active(true);
        let context = ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: orna_core::InvocationId::from_bytes([0; 16]),
            observer_lineage: None,
        };
        let rules = [
            (
                ClientExecutionRule::FunctionDomain,
                "this function does not run on the client",
            ),
            (
                ClientExecutionRule::Parameters,
                "this CLIENT function requires unsupported parameters",
            ),
            (
                ClientExecutionRule::ReturnType,
                "this CLIENT function has an unsupported return type",
            ),
            (
                ClientExecutionRule::Security,
                "this CLIENT function has an unsupported security mode",
            ),
            (
                ClientExecutionRule::Volatility,
                "this CLIENT function is not an immutable constant",
            ),
            (
                ClientExecutionRule::References,
                "this CLIENT function depends on unsupported definitions",
            ),
            (
                ClientExecutionRule::ArtifactFormat,
                "the saved CLIENT function uses an unsupported artefact format",
            ),
            (
                ClientExecutionRule::ArtifactVersion,
                "the saved CLIENT function uses an unsupported artefact version",
            ),
            (
                ClientExecutionRule::LanguageVersion,
                "the saved CLIENT function uses an unsupported language version",
            ),
        ];
        for (rule, display) in rules {
            assert_eq!(rule.to_string(), display);
            assert!(std::error::Error::source(&rule).is_none());
        }

        let mismatch = ClientActiveRevisionError::CatalogueHashMismatch;
        assert_eq!(
            mismatch.to_string(),
            "active revision catalogue hash differs from its canonical semantics"
        );
        assert!(std::error::Error::source(&mismatch).is_none());

        let not_found =
            evaluate_client_function(&active, FunctionId::from_bytes([0x77; 16])).unwrap_err();
        assert_eq!(not_found.pair(), pair);
        assert_eq!(not_found.function(), FunctionId::from_bytes([0x77; 16]));
        assert_eq!(not_found.context(), None);
        assert_eq!(
            not_found.to_string(),
            "the active revision does not contain this function"
        );
        assert!(std::error::Error::source(&not_found).is_none());

        let invalid = ClientExecutionError::InvalidFunction {
            context,
            rule: ClientExecutionRule::Security,
        };
        assert_eq!(invalid.pair(), pair);
        assert_eq!(invalid.function(), function);
        assert_eq!(invalid.context(), Some(&context));
        assert_eq!(
            invalid.to_string(),
            "this CLIENT function has an unsupported security mode"
        );
        assert!(std::error::Error::source(&invalid).is_none());

        let active_error = ClientExecutionError::InvalidActiveRevision {
            pair,
            function,
            source: mismatch,
        };
        assert_eq!(
            active_error.to_string(),
            "the active revision cannot be trusted"
        );
        assert!(std::error::Error::source(&active_error).is_some());

        let artifact_error = ClientPlan::decode(b"invalid").unwrap_err();
        let invalid_artifact = ClientExecutionError::InvalidArtifact {
            context,
            source: artifact_error,
        };
        assert!(invalid_artifact.context().is_some());
        assert!(std::error::Error::source(&invalid_artifact).is_some());
        assert_eq!(
            invalid_artifact.to_string(),
            "the saved CLIENT function cannot be evaluated"
        );

        let opaque_error = ClientOpaqueValueError::TypeMismatch {
            expected: orna_standard::OPAQUE_TOKEN_TYPE_ID,
            actual: TypeId::from_bytes([0x78; 16]),
        };
        assert_eq!(
            opaque_error.to_string(),
            "opaque CLIENT plan type does not match its function return"
        );
        assert!(std::error::Error::source(&opaque_error).is_none());
        let invalid_opaque = ClientExecutionError::InvalidOpaqueValue {
            context,
            source: opaque_error,
        };
        assert_eq!(invalid_opaque.pair(), pair);
        assert_eq!(invalid_opaque.function(), function);
        assert_eq!(invalid_opaque.context(), Some(&context));
        assert_eq!(
            invalid_opaque.to_string(),
            "the saved CLIENT function cannot be evaluated"
        );
        assert!(std::error::Error::source(&invalid_opaque).is_some());
    }

    #[test]
    fn artefact_contract_failures_follow_closed_validation_after_active_trust() {
        let valid_payload = b"ORNACP\0\0\0\0\0\x01\x01\x01";
        let cases = [
            (
                "unsupported format",
                ExecutableArtifact::new(
                    ExecutableArtifactKind::Client,
                    "other.format",
                    1,
                    valid_payload.to_vec(),
                    artifact_payload_digest(valid_payload).unwrap(),
                )
                .unwrap(),
                "orna.language/1",
                Some(super::ClientExecutionRule::ArtifactFormat),
            ),
            (
                "unsupported version",
                ExecutableArtifact::new(
                    ExecutableArtifactKind::Client,
                    "orna.client-plan",
                    orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
                    valid_payload.to_vec(),
                    artifact_payload_digest(valid_payload).unwrap(),
                )
                .unwrap(),
                "orna.language/1",
                Some(super::ClientExecutionRule::ArtifactVersion),
            ),
            (
                "unsupported language",
                ExecutableArtifact::new(
                    ExecutableArtifactKind::Client,
                    "orna.client-plan",
                    1,
                    valid_payload.to_vec(),
                    artifact_payload_digest(valid_payload).unwrap(),
                )
                .unwrap(),
                "orna.language/2",
                Some(super::ClientExecutionRule::LanguageVersion),
            ),
            (
                "undecodable plan",
                ExecutableArtifact::new(
                    ExecutableArtifactKind::Client,
                    "orna.client-plan",
                    1,
                    b"not a client plan".to_vec(),
                    artifact_payload_digest(b"not a client plan").unwrap(),
                )
                .unwrap(),
                "orna.language/1",
                None,
            ),
        ];

        for (name, artifact, language, expected_rule) in cases {
            let (active, function, _, _) = version_one_active_with_artifact(artifact, language);
            let error = evaluate_client_function(&active, function).unwrap_err();

            assert_eq!(error.function(), function, "{name}");
            assert!(error.context().is_some(), "{name}");
            match expected_rule {
                Some(rule) => {
                    assert!(matches!(
                        error,
                        super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                            if actual == rule
                    ));
                    assert_eq!(error.to_string(), rule.to_string(), "{name}");
                    assert!(std::error::Error::source(&error).is_none(), "{name}");
                }
                None => {
                    assert!(matches!(
                        error,
                        super::ClientExecutionError::InvalidArtifact { .. }
                    ));
                    assert_eq!(
                        error.to_string(),
                        "the saved CLIENT function cannot be evaluated"
                    );
                    assert!(std::error::Error::source(&error).is_some());
                }
            }
        }
    }

    #[test]
    fn function_shape_rules_are_public_and_follow_the_closed_precedence_order() {
        let cases = [
            (
                "domain before parameters",
                FunctionDomain::Server,
                vec![boolean_parameter()],
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
                super::ClientExecutionRule::FunctionDomain,
            ),
            (
                "parameters before return type",
                FunctionDomain::Client,
                vec![boolean_parameter()],
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
                super::ClientExecutionRule::Parameters,
            ),
            (
                "return type before security",
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
                FunctionSecurity::Definer,
                FunctionVolatility::Immutable,
                super::ClientExecutionRule::ReturnType,
            ),
            (
                "security before volatility",
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
                FunctionSecurity::Definer,
                FunctionVolatility::Stable,
                super::ClientExecutionRule::Security,
            ),
            (
                "volatility",
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
                FunctionSecurity::Invoker,
                FunctionVolatility::Stable,
                super::ClientExecutionRule::Volatility,
            ),
        ];

        for (name, domain, parameters, return_type, security, volatility, rule) in cases {
            let (active, function, pair, function_revision) = version_one_active_with_shape(
                domain,
                parameters,
                return_type,
                security,
                volatility,
            );
            let error = evaluate_client_function(&active, function).unwrap_err();

            assert_eq!(error.pair(), pair, "{name}");
            assert_eq!(error.function(), function, "{name}");
            let context = error.context().expect("invalid function error context");
            assert_eq!(context.pair(), pair, "{name}");
            assert_eq!(context.function(), function, "{name}");
            assert_eq!(context.function_revision(), function_revision, "{name}");
            assert!(matches!(
                error,
                super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                    if actual == rule
            ));
            assert_eq!(error.to_string(), rule.to_string(), "{name}");
            assert!(std::error::Error::source(&error).is_none(), "{name}");
        }
    }

    #[test]
    fn version_one_public_evaluation_accepts_only_a_legacy_boolean_single_return() {
        for scalar in StandardScalar::ALL {
            let (active, function, _, _) = version_one_active_with_shape(
                FunctionDomain::Client,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::scalar(scalar)),
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
            );
            let result = evaluate_client_function(&active, function);
            if scalar == StandardScalar::Boolean {
                assert_eq!(result.unwrap().value(), &RuntimeValue::Boolean(true));
                continue;
            }
            let error = result.unwrap_err();
            assert_return_type_rule(error);
        }

        for return_type in [
            FunctionReturn::Single(ResolvedType::named(TypeId::from_bytes([0x71; 16]))),
            FunctionReturn::Single(ResolvedType::reference(TypeId::from_bytes([0x72; 16]))),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )]),
        ] {
            let (active, function, _, _) = version_one_active_with_shape(
                FunctionDomain::Client,
                Vec::new(),
                return_type,
                FunctionSecurity::Invoker,
                FunctionVolatility::Immutable,
            );
            assert_return_type_rule(evaluate_client_function(&active, function).unwrap_err());
        }
    }

    fn assert_return_type_rule(error: super::ClientExecutionError) {
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidFunction {
                rule: super::ClientExecutionRule::ReturnType,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "this CLIENT function has an unsupported return type"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_references_rule(
        result: Result<super::ClientExecutionResult, super::ClientExecutionError>,
        function: FunctionId,
    ) {
        let error = result.unwrap_err();
        assert_eq!(error.function(), function);
        assert_eq!(
            error.to_string(),
            "this CLIENT function depends on unsupported definitions"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidFunction {
                rule: super::ClientExecutionRule::References,
                ..
            }
        ));
    }

    fn replace_reference(
        references: &mut [DefinitionReference],
        function: FunctionId,
        replacement: impl FnOnce(&DefinitionReference) -> DefinitionReference,
    ) {
        let index = references
            .iter()
            .position(|reference| reference.source_function() == function)
            .unwrap();
        references[index] = replacement(&references[index]);
    }

    fn prepared_client_constant(literal: &str) -> DeployableRevision {
        prepared_client_source(&format!(
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN {literal};"
        ))
    }

    fn prepared_client_source_v6(source: &str) -> DeployableRevision {
        let snapshot = orna_standard::retained_standard_library_v6_snapshot().unwrap();
        let verified = orna_standard::verify_standard_library_v6_snapshot(snapshot).unwrap();
        let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
                .unwrap();
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = orna_compiler::check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
    }

    fn active_with_reordered_client_call_references(
        prepared: &DeployableRevision,
        function_name: &str,
    ) -> (ActiveDatabaseRevision, FunctionId) {
        let function = prepared
            .candidate()
            .functions()
            .iter()
            .find(|candidate| candidate.name().to_string() == function_name)
            .expect("the reordered-call owner is present")
            .id();
        let mut references = prepared.references().to_vec();
        let mut call_indices = references
            .iter()
            .enumerate()
            .filter(|(_, reference)| {
                reference.source_function() == function
                    && reference.kind() == DefinitionReferenceKind::FunctionCall
            })
            .map(|(index, reference)| (index, reference.ordinal()))
            .collect::<Vec<_>>();
        call_indices.sort_unstable_by_key(|(_, ordinal)| *ordinal);
        assert!(call_indices.len() >= 2, "the fixture must contain two calls");
        let first = references[call_indices[0].0].clone();
        let second = references[call_indices[1].0].clone();
        references[call_indices[0].0] = DefinitionReference::new(
            first.source_function(),
            first.source_revision(),
            first.ordinal(),
            second.target(),
            first.kind(),
            first.source_origin(),
        );
        references[call_indices[1].0] = DefinitionReference::new(
            second.source_function(),
            second.source_revision(),
            second.ordinal(),
            first.target(),
            second.kind(),
            second.source_origin(),
        );
        (active_from_prepared_with_references(prepared, references), function)
    }

    fn prepared_client_source(source: &str) -> DeployableRevision {
        let snapshot = orna_standard::retained_standard_library_snapshot().unwrap();
        let verified = orna_standard::verify_standard_library_snapshot(snapshot).unwrap();
        let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
                .unwrap();
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = orna_compiler::check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
    }

    fn prepared_client_functions() -> DeployableRevision {
        let snapshot = orna_standard::retained_standard_library_snapshot().unwrap();
        let verified = orna_standard::verify_standard_library_snapshot(snapshot).unwrap();
        let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
                .unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; \
             CREATE TYPE app.item AS OBJECT (); \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = orna_compiler::check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
    }

    fn empty_version_two_active(
        standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
    ) -> ActiveDatabaseRevision {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x41; 16]),
            0,
            "active.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x42; 16]),
            SourceRevisionId::from_bytes([0x43; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x42; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = orna_core::revision::CatalogueHashContext::version_two(standard.clone());
        let catalogue_hash = orna_core::canonical_hash::catalogue_digest_with_context(
            &context,
            &catalogue,
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn active_from_prepared_candidate(prepared: &DeployableRevision) -> ActiveDatabaseRevision {
        active_from_prepared_with_references(prepared, prepared.references().to_vec())
    }

    fn active_from_prepared_with_semantic_versions(
        prepared: &DeployableRevision,
        semantic_hash_version: FunctionSemanticHashVersion,
        references: Vec<DefinitionReference>,
    ) -> ActiveDatabaseRevision {
        active_from_prepared_with_current_revisions(prepared, references, |revision| {
            semantic_hash_version_for(revision, semantic_hash_version)
        })
        .unwrap()
    }

    fn active_from_prepared_with_references_result(
        prepared: &DeployableRevision,
        references: Vec<DefinitionReference>,
    ) -> Result<ActiveDatabaseRevision, Box<dyn std::error::Error>> {
        active_from_prepared_with_current_revisions(prepared, references, |revision| {
            revision.semantic_hash_version()
        })
    }

    fn active_from_prepared_with_references(
        prepared: &DeployableRevision,
        references: Vec<DefinitionReference>,
    ) -> ActiveDatabaseRevision {
        active_from_prepared_with_references_result(prepared, references).unwrap()
    }

    fn active_from_prepared_with_current_revisions(
        prepared: &DeployableRevision,
        references: Vec<DefinitionReference>,
        semantic_hash_version: impl Fn(&FunctionRevisionRecord) -> FunctionSemanticHashVersion,
    ) -> Result<ActiveDatabaseRevision, Box<dyn std::error::Error>> {
        let current_function_revisions = prepared
            .current_function_revisions()
            .unwrap()
            .iter()
            .map(|revision| {
                let function = prepared
                    .candidate()
                    .function_by_id(revision.function())
                    .unwrap();
                let version = semantic_hash_version(revision);
                let function_references = references
                    .iter()
                    .filter(|reference| reference.source_function() == revision.function())
                    .cloned()
                    .collect::<Vec<_>>();
                let semantic_hash = function_semantic_digest_with_version(
                    version,
                    function,
                    revision.language_version(),
                    revision.artifact(),
                    prepared.expressions(),
                    &function_references,
                )?;
                Ok::<_, Box<dyn std::error::Error>>(
                    FunctionRevisionRecord::new(
                        revision.function(),
                        revision.id(),
                        revision.revision_number(),
                        revision.declaration_origin(),
                        revision.declaration_content_hash(),
                        semantic_hash,
                        revision.language_version(),
                        revision.artifact().clone(),
                    )?
                    .with_semantic_hash_version(version),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let context = prepared.catalogue_hash_context().clone();
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            prepared.candidate(),
            &current_function_revisions,
            prepared.expressions(),
            prepared.origins(),
            &references,
        )?;
        Ok(ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                prepared.candidate_pair(),
                prepared.source().clone(),
                prepared.candidate().clone(),
                catalogue_hash,
                ActiveRevisionContent::new(
                    prepared.expressions().to_vec(),
                    current_function_revisions,
                    prepared.origins().to_vec(),
                    references,
                ),
            ),
            context,
        )?)
    }

    fn active_with_extra_reference(
        active: &ActiveDatabaseRevision,
        extra: DefinitionReference,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        let mut references = active.references().to_vec();
        references.push(extra);
        active_with_content(
            active,
            active.source().clone(),
            active.origins().to_vec(),
            references,
        )
    }

    fn active_with_mismatched_function_artifact_payload_hash(
        active: &ActiveDatabaseRevision,
    ) -> ActiveDatabaseRevision {
        let current = &active.function_revisions()[0];
        let artifact = ExecutableArtifact::new(
            current.artifact().kind(),
            current.artifact().format(),
            current.artifact().version(),
            current.artifact().payload().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([0x8e; 32]),
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            current.declaration_content_hash(),
            current.semantic_hash(),
            current.language_version(),
            artifact,
        )
        .unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                active.catalogue_hash(),
                ActiveRevisionContent::new(
                    active.expressions().to_vec(),
                    vec![revision],
                    active.origins().to_vec(),
                    active.references().to_vec(),
                ),
            ),
            active.catalogue_hash_context().clone(),
        )
        .unwrap()
    }

    fn active_with_replaced_first_origin(
        active: &ActiveDatabaseRevision,
        source_origin: SourceOrigin,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        active_with_source_and_first_origin(active, active.source().clone(), source_origin)
    }

    fn active_with_replaced_reference_origin(
        active: &ActiveDatabaseRevision,
        source: StoredSourceRevision,
        function: FunctionId,
        source_origin: SourceOrigin,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        let mut references = active.references().to_vec();
        replace_reference(&mut references, function, |reference| {
            DefinitionReference::new(
                reference.source_function(),
                reference.source_revision(),
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                source_origin,
            )
        });
        active_with_content(active, source, active.origins().to_vec(), references)
    }

    fn active_with_source_and_first_origin(
        active: &ActiveDatabaseRevision,
        source: StoredSourceRevision,
        source_origin: SourceOrigin,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        let mut origins = active.origins().to_vec();
        origins[0] = DefinitionOrigin::new(origins[0].identity(), source_origin);
        active_with_content(active, source, origins, active.references().to_vec())
    }

    fn active_with_content(
        active: &ActiveDatabaseRevision,
        source: StoredSourceRevision,
        origins: Vec<DefinitionOrigin>,
        references: Vec<DefinitionReference>,
    ) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                active.pair(),
                source,
                active.catalogue().clone(),
                active.catalogue_hash(),
                ActiveRevisionContent::new(
                    active.expressions().to_vec(),
                    active.function_revisions().to_vec(),
                    origins,
                    references,
                ),
            ),
            active.catalogue_hash_context().clone(),
        )
    }

    fn replacement_source(active: &ActiveDatabaseRevision, content: &str) -> StoredSourceRevision {
        let old = active.source();
        let old_unit = &old.units()[0];
        let replacement = StoredSourceUnit::new(
            old_unit.id(),
            0,
            old_unit.logical_path(),
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&replacement)).unwrap();
        StoredSourceRevision::new(
            old.bundle(),
            old.id(),
            old.parent(),
            vec![replacement],
            bundle_hash,
            source_revision_record_digest(old.bundle(), old.parent(), bundle_hash).unwrap(),
        )
        .unwrap()
    }

    const fn semantic_hash_version_for(
        _revision: &FunctionRevisionRecord,
        semantic_hash_version: FunctionSemanticHashVersion,
    ) -> FunctionSemanticHashVersion {
        semantic_hash_version
    }
    fn standard_v5() -> VerifiedStandardLibrarySnapshot {
        orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap()
    }

    fn standard_v6() -> VerifiedStandardLibrarySnapshot {
        orna_standard::verify_standard_library_v6_snapshot(
            orna_standard::retained_standard_library_v6_snapshot().unwrap(),
        )
        .unwrap()
    }


    fn version_two_value_active(
        return_type: TypeId,
        reference_target: TypeId,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_artifact(
            standard_v5(),
            return_type,
            DefinitionReferenceTarget::ValueType(reference_target),
            DefinitionReferenceKind::NamedType,
            1,
            b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
        )
    }

    fn version_two_opaque_active(
        plan_type: TypeId,
        payload: [u8; 16],
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_artifact(
            standard_v5(),
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
            DefinitionReferenceTarget::ValueType(orna_standard::OPAQUE_TOKEN_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
            orna_artifact::client_plan::OpaqueClientPlan::return_opaque(plan_type, payload)
                .encode(),
        )
    }

    fn version_two_value_active_with_artifact(
        return_type: TypeId,
        reference_target: TypeId,
        artifact_version: u32,
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_artifact(
            standard_v5(),
            return_type,
            DefinitionReferenceTarget::ValueType(reference_target),
            DefinitionReferenceKind::NamedType,
            artifact_version,
            payload,
        )
    }

    fn version_two_client_call_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::Function(FunctionId::from_bytes([6; 16])),
            DefinitionReferenceKind::FunctionCall,
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            )
            .encode()
            .unwrap(),
        )
    }

    fn version_two_local_action_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let (base, parent_id, pair, parent_revision_id) = version_one_active(true);
        let target_id = FunctionId::from_bytes([0xc2; 16]);
        let target_revision_id = FunctionRevisionId::from_bytes([0xc3; 16]);
        let previous_revision = &base.function_revisions()[0];
        let parent_name = base
            .catalogue()
            .function_by_id(parent_id)
            .unwrap()
            .name()
            .clone();
        let parent = FunctionDefinition::new(
            parent_id,
            parent_name,
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
            parent_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let target = FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["app", "action_target"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
            target_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            base.catalogue().revision(),
            base.catalogue().schemas().to_vec(),
            base.catalogue().object_types().to_vec(),
            vec![parent.clone(), target.clone()],
        )
        .unwrap();
        let parent_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Call {
                function: target_id,
                arguments: Vec::new(),
            },
        )
        .encode()
        .unwrap();
        let target_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap();
        let parent_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            parent_payload.clone(),
            artifact_payload_digest(&parent_payload).unwrap(),
        )
        .unwrap();
        let target_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            target_payload.clone(),
            artifact_payload_digest(&target_payload).unwrap(),
        )
        .unwrap();
        let parent_reference = DefinitionReference::new(
            parent_id,
            parent_revision_id,
            0,
            DefinitionReferenceTarget::Function(target_id),
            DefinitionReferenceKind::FunctionCall,
            previous_revision.declaration_origin(),
        );
        let parent_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &parent,
            previous_revision.language_version(),
            &parent_artifact,
            base.expressions(),
            std::slice::from_ref(&parent_reference),
        )
        .unwrap();
        let target_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &target,
            previous_revision.language_version(),
            &target_artifact,
            base.expressions(),
            &[],
        )
        .unwrap();
        let parent_revision = FunctionRevisionRecord::new(
            parent_id,
            parent_revision_id,
            previous_revision.revision_number(),
            previous_revision.declaration_origin(),
            previous_revision.declaration_content_hash(),
            parent_semantic_hash,
            previous_revision.language_version(),
            parent_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let target_origin = SourceOrigin::new(
            previous_revision.declaration_origin().source_unit(),
            previous_revision.declaration_origin().byte_start(),
            previous_revision.declaration_origin().byte_end(),
        )
        .unwrap();
        let target_revision = FunctionRevisionRecord::new(
            target_id,
            target_revision_id,
            previous_revision.revision_number(),
            target_origin,
            previous_revision.declaration_content_hash(),
            target_semantic_hash,
            previous_revision.language_version(),
            target_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let mut origins = base.origins().to_vec();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Function(target_id),
            target_origin,
        ));
        let revisions = vec![parent_revision, target_revision];
        let standard = orna_standard::verify_standard_library_v6_snapshot(
            orna_standard::retained_standard_library_v6_snapshot().unwrap(),
        )
        .unwrap();
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            &revisions,
            base.expressions(),
            &origins,
            std::slice::from_ref(&parent_reference),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                base.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    base.expressions().to_vec(),
                    revisions,
                    origins,
                    vec![parent_reference],
                ),
            ),
            context,
        )
        .unwrap();

        (active, parent_id, target_id, pair, parent_revision_id)
    }
    fn version_two_active_with_artifact(
        standard: VerifiedStandardLibrarySnapshot,
        return_type: TypeId,
        reference_target: DefinitionReferenceTarget,
        reference_kind: DefinitionReferenceKind,
        artifact_version: u32,
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_function_return(
            standard,
            FunctionReturn::Single(ResolvedType::Value(return_type)),
            reference_target,
            reference_kind,
            artifact_version,
            payload,
        )
    }

    fn version_two_client_stream_active_with_artifact(
        standard: VerifiedStandardLibrarySnapshot,
        item_type: TypeId,
        reference_target: DefinitionReferenceTarget,
        reference_kind: DefinitionReferenceKind,
        artifact_version: u32,
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_active_with_function_return(
            standard,
            FunctionReturn::Stream(ResolvedType::Value(item_type)),
            reference_target,
            reference_kind,
            artifact_version,
            payload,
        )
    }

    fn version_two_active_with_function_return(
        standard: VerifiedStandardLibrarySnapshot,
        function_return: FunctionReturn,
        reference_target: DefinitionReferenceTarget,
        reference_kind: DefinitionReferenceKind,
        artifact_version: u32,
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
        let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
        let function = FunctionDefinition::new(
            function_id,
            prior_function.name().clone(),
            FunctionDomain::Client,
            Vec::new(),
            function_return,
            function_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            version_one.catalogue().revision(),
            version_one.catalogue().schemas().to_vec(),
            version_one.catalogue().object_types().to_vec(),
            vec![function.clone()],
        )
        .unwrap();
        let prior_revision = &version_one.function_revisions()[0];
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            artifact_version,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let reference = DefinitionReference::new(
            function_id,
            function_revision_id,
            0,
            reference_target,
            reference_kind,
            prior_revision.declaration_origin(),
        );
        let semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &function,
            prior_revision.language_version(),
            &artifact,
            version_one.expressions(),
            std::slice::from_ref(&reference),
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            semantic_hash,
            prior_revision.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            std::slice::from_ref(&revision),
            version_one.expressions(),
            version_one.origins(),
            std::slice::from_ref(&reference),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                version_one.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    version_one.expressions().to_vec(),
                    vec![revision],
                    version_one.origins().to_vec(),
                    vec![reference],
                ),
            ),
            context,
        )
        .unwrap();

        (active, function_id, pair, function_revision_id)
    }

    fn version_two_server_rows_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_server_active(FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new(
                "first",
                0,
                ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
            ),
            FunctionReturnColumnDefinition::new(
                "second",
                1,
                ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
            ),
        ]))
    }

    fn version_two_server_stream_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_server_active(FunctionReturn::Stream(ResolvedType::value(
            orna_standard::BOOLEAN_TYPE_ID,
        )))
    }

    fn version_two_server_active(
        return_type: FunctionReturn,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let (initial, function_id, pair, function_revision_id) =
            version_two_active_with_artifact(
                standard_v6(),
                orna_standard::BOOLEAN_TYPE_ID,
                DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
                DefinitionReferenceKind::NamedType,
                1,
                b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
            );
        let prior_function = initial.catalogue().function_by_id(function_id).unwrap();
        let function = FunctionDefinition::new(
            function_id,
            prior_function.name().clone(),
            FunctionDomain::Server,
            Vec::new(),
            return_type,
            function_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            initial.catalogue().revision(),
            initial.catalogue().schemas().to_vec(),
            initial.catalogue().object_types().to_vec(),
            vec![function.clone()],
        )
        .unwrap();
        let prior_revision = &initial.function_revisions()[0];
        let payload = vec![0x53];
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            1,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &function,
            prior_revision.language_version(),
            &artifact,
            initial.expressions(),
            &[],
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            semantic_hash,
            prior_revision.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let mut origins = initial.origins().to_vec();
        if let FunctionReturn::Rows(columns) = function.return_type() {
            origins.extend(columns.iter().map(|column| {
                DefinitionOrigin::new(
                    DefinitionIdentity::FunctionReturnColumn {
                        owner: function_id,
                        ordinal: column.ordinal(),
                    },
                    prior_revision.declaration_origin(),
                )
            }));
        }
        let context = initial.catalogue_hash_context().clone();
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            std::slice::from_ref(&revision),
            initial.expressions(),
            &origins,
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                initial.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    initial.expressions().to_vec(),
                    vec![revision],
                    origins,
                    Vec::new(),
                ),
            ),
            context,
        )
        .unwrap();
        (active, function_id, pair, function_revision_id)
    }

    fn version_four_state_active(
        return_type: TypeId,
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
        let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
        let function = FunctionDefinition::new(
            function_id,
            prior_function.name().clone(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Value(return_type)),
            function_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            version_one.catalogue().revision(),
            version_one.catalogue().schemas().to_vec(),
            version_one.catalogue().object_types().to_vec(),
            vec![function.clone()],
        )
        .unwrap();
        let prior_revision = &version_one.function_revisions()[0];
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::STATE_FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &function,
            prior_revision.language_version(),
            &artifact,
            version_one.expressions(),
            &[],
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            semantic_hash,
            prior_revision.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            std::slice::from_ref(&revision),
            version_one.expressions(),
            version_one.origins(),
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                version_one.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    version_one.expressions().to_vec(),
                    vec![revision],
                    version_one.origins().to_vec(),
                    Vec::new(),
                ),
            ),
            context,
        )
        .unwrap();

        (active, function_id, pair, function_revision_id)
    }

    fn version_one_active(
        value: bool,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let source = match value {
            true => {
                "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;"
            }
            false => {
                "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;"
            }
        };
        let function_start = "CREATE SCHEMA app;\n".len();
        let source_unit_id = SourceUnitId::from_bytes([1; 16]);
        let source_bundle_id = SourceBundleId::from_bytes([2; 16]);
        let source_revision_id = SourceRevisionId::from_bytes([3; 16]);
        let catalogue_revision_id = CatalogueRevisionId::from_bytes([4; 16]);
        let schema_id = SchemaId::from_bytes([5; 16]);
        let function_id = FunctionId::from_bytes([6; 16]);
        let function_revision_id = FunctionRevisionId::from_bytes([7; 16]);
        let pair = RevisionPair::new(source_revision_id, catalogue_revision_id);

        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "application.orna",
            source,
            source_unit_content_digest(source).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let stored_source = StoredSourceRevision::new(
            source_bundle_id,
            source_revision_id,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(source_bundle_id, None, bundle_hash).unwrap(),
        )
        .unwrap();

        let schema = SchemaDefinition::new(schema_id, QualifiedSemanticName::new(["app"]).unwrap());
        let function = FunctionDefinition::new(
            function_id,
            QualifiedSemanticName::new(["app", "enabled"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            function_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            catalogue_revision_id,
            vec![schema],
            Vec::new(),
            vec![function.clone()],
        )
        .unwrap();

        let payload = match value {
            true => b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
            false => b"ORNACP\0\0\0\0\0\x01\x01\x00".to_vec(),
        };
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            1,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let function_origin = SourceOrigin::new(
            source_unit_id,
            u32::try_from(function_start).unwrap(),
            u32::try_from(source.len()).unwrap(),
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            1,
            function_origin,
            function_declaration_digest(&source.as_bytes()[function_start..]).unwrap(),
            function_semantic_digest(&function, "orna.language/1", &artifact, &[], &[]).unwrap(),
            "orna.language/1",
            artifact,
        )
        .unwrap();
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema_id),
                SourceOrigin::new(
                    source_unit_id,
                    0,
                    u32::try_from(function_start - 1).unwrap(),
                )
                .unwrap(),
            ),
            DefinitionOrigin::new(DefinitionIdentity::Function(function_id), function_origin),
        ];
        let catalogue_hash = catalogue_digest(
            &catalogue,
            std::slice::from_ref(&revision),
            &[],
            &origins,
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new(
            pair,
            stored_source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            vec![revision],
            origins,
            Vec::new(),
        )
        .unwrap();

        (active, function_id, pair, function_revision_id)
    }

    fn version_one_active_with_artifact(
        artifact: ExecutableArtifact,
        language_version: &str,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let (initial, function, pair, function_revision) = version_one_active(true);
        let definition = initial.catalogue().function_by_id(function).unwrap();
        let previous = &initial.function_revisions()[0];
        let semantic_hash = function_semantic_digest(
            definition,
            language_version,
            &artifact,
            initial.expressions(),
            &[],
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function,
            function_revision,
            previous.revision_number(),
            previous.declaration_origin(),
            previous.declaration_content_hash(),
            semantic_hash,
            language_version,
            artifact,
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(
            initial.catalogue(),
            std::slice::from_ref(&revision),
            initial.expressions(),
            initial.origins(),
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new(
            pair,
            initial.source().clone(),
            initial.catalogue().clone(),
            catalogue_hash,
            initial.expressions().to_vec(),
            vec![revision],
            initial.origins().to_vec(),
            Vec::new(),
        )
        .unwrap();

        (active, function, pair, function_revision)
    }

    fn version_five_boolean_envelope(
        value: bool,
        requirements: Vec<orna_artifact::client_plan::CapabilityRequirement>,
    ) -> Vec<u8> {
        orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Boolean(
                orna_artifact::client_plan::ClientPlan::return_boolean(value),
            ),
            requirements,
        )
        .encode()
        .expect("the version-5 capability envelope encodes")
    }

    fn version_five_boolean_active(
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        version_one_active_with_artifact(artifact, "orna.language/1")
    }

    fn collect_fixture_expression_call_targets(
        expression: &orna_artifact::client_plan::ClientExpressionNode,
        targets: &mut Vec<FunctionId>,
    ) {
        use orna_artifact::client_plan::ClientExpressionNode;

        match expression {
            ClientExpressionNode::Await { expression } => {
                collect_fixture_expression_call_targets(expression, targets);
            }
            ClientExpressionNode::Resource { operation } => {
                for (_, expression) in operation.arguments() {
                    collect_fixture_expression_call_targets(expression, targets);
                }
                targets.push(operation.target_function());
            }
            ClientExpressionNode::Action { operation } => {
                for (_, expression) in operation.arguments() {
                    collect_fixture_expression_call_targets(expression, targets);
                }
                targets.push(operation.target_function());
            }
            ClientExpressionNode::Inspect { operation } => {
                if let Some(expression) = operation.target() {
                    collect_fixture_expression_call_targets(expression, targets);
                }
                if let Some(expression) = operation.options() {
                    collect_fixture_expression_call_targets(expression, targets);
                }
                if let Some(expression) = operation.snapshot_expression() {
                    collect_fixture_expression_call_targets(expression, targets);
                }
            }
            ClientExpressionNode::Call { function, arguments } => {
                for (_, expression) in arguments {
                    collect_fixture_expression_call_targets(expression, targets);
                }
                targets.push(*function);
            }
            ClientExpressionNode::Concat { left, right } => {
                collect_fixture_expression_call_targets(left, targets);
                collect_fixture_expression_call_targets(right, targets);
            }
            ClientExpressionNode::String { .. }
            | ClientExpressionNode::Integer { .. }
            | ClientExpressionNode::Boolean { .. }
            | ClientExpressionNode::ParameterRead { .. }
            | ClientExpressionNode::LocalRead { .. }
            | ClientExpressionNode::FieldPath { .. }
            | ClientExpressionNode::ExternalContract { .. } => {}
        }
    }

    fn fixture_client_call_references(
        function: FunctionId,
        revision: FunctionRevisionId,
        origin: SourceOrigin,
        payload: &[u8],
    ) -> Vec<DefinitionReference> {
        let plan = orna_artifact::client_plan::CapabilityClientPlan::decode(payload)
            .expect("the capability fixture payload decodes");
        let mut targets = Vec::new();
        match plan.inner_plan() {
            orna_artifact::client_plan::InnerClientPlan::Boolean(_)
            | orna_artifact::client_plan::InnerClientPlan::Opaque(_) => {}
            orna_artifact::client_plan::InnerClientPlan::Expression(inner) => {
                collect_fixture_expression_call_targets(inner.expression(), &mut targets);
            }
            orna_artifact::client_plan::InnerClientPlan::State(inner) => {
                for slot in inner.slots() {
                    if let orna_artifact::client_plan::StateDefault::Expression(expression) = slot.default() {
                        collect_fixture_expression_call_targets(expression, &mut targets);
                    }
                }
                collect_fixture_expression_call_targets(inner.expression(), &mut targets);
            }
            orna_artifact::client_plan::InnerClientPlan::Procedural(inner) => {
                for statement in inner.statements() {
                    collect_fixture_expression_call_targets(statement.expression(), &mut targets);
                }
                collect_fixture_expression_call_targets(inner.return_expression(), &mut targets);
            }
            orna_artifact::client_plan::InnerClientPlan::Action(inner) => {
                for (_, expression) in inner.operation().arguments() {
                    collect_fixture_expression_call_targets(expression, &mut targets);
                }
                targets.push(inner.operation().target_function());
            }
            orna_artifact::client_plan::InnerClientPlan::Resource(inner) => {
                collect_fixture_expression_call_targets(inner.expression(), &mut targets);
            }
        }
        targets
            .into_iter()
            .enumerate()
            .map(|(ordinal, target)| {
                DefinitionReference::new(
                    function,
                    revision,
                    u32::try_from(ordinal).expect("fixture call ordinal fits"),
                    DefinitionReferenceTarget::Function(target),
                    DefinitionReferenceKind::FunctionCall,
                    origin.clone(),
                )
            })
            .collect()
    }

    fn version_five_expression_active_with_parameter(
        payload: Vec<u8>,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
        ParameterId,
    ) {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap();
        let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
        let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
        let parameter = ParameterDefinition::new(
            ParameterId::from_bytes([0xb1; 16]),
            "p_path",
            0,
            ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            None,
        );
        let function = FunctionDefinition::new(
            function_id,
            prior_function.name().clone(),
            FunctionDomain::Client,
            vec![parameter.clone()],
            FunctionReturn::Single(ResolvedType::Value(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            )),
            function_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let resource_parameter = ParameterDefinition::new(
            ParameterId::from_bytes([0xd3; 16]),
            "p_resource",
            0,
            ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            None,
        );
        let resource_target = FunctionDefinition::new(
            FunctionId::from_bytes([0xd1; 16]),
            QualifiedSemanticName::new(["app", "resource"]).unwrap(),
            FunctionDomain::Server,
            vec![resource_parameter.clone()],
            FunctionReturn::Single(ResolvedType::Value(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            )),
            FunctionRevisionId::from_bytes([0xd2; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            version_one.catalogue().revision(),
            version_one.catalogue().schemas().to_vec(),
            version_one.catalogue().object_types().to_vec(),
            vec![function.clone(), resource_target.clone()],
        )
        .unwrap();
        let prior_revision = &version_one.function_revisions()[0];
        let origin = prior_revision.declaration_origin();
        let references = fixture_client_call_references(
            function_id,
            function_revision_id,
            origin.clone(),
            &payload,
        );
        let resource_payload = vec![0x53];
        let resource_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            1,
            resource_payload.clone(),
            artifact_payload_digest(&resource_payload).unwrap(),
        )
        .unwrap();
        let resource_revision = FunctionRevisionRecord::new(
            resource_target.id(),
            FunctionRevisionId::from_bytes([0xd2; 16]),
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            function_semantic_digest_with_version(
                FunctionSemanticHashVersion::Version2,
                &resource_target,
                prior_revision.language_version(),
                &resource_artifact,
                &[],
                &[],
            )
            .unwrap(),
            prior_revision.language_version(),
            resource_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &function,
            prior_revision.language_version(),
            &artifact,
            version_one.expressions(),
            &references,
        )
        .unwrap();
        let revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            semantic_hash,
            prior_revision.language_version(),
            artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let mut origins = version_one.origins().to_vec();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: function_id,
                parameter: parameter.id(),
            },
            origin.clone(),
        ));
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Function(resource_target.id()),
            origin.clone(),
        ));
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: resource_target.id(),
                parameter: resource_parameter.id(),
            },
            origin.clone(),
        ));
        let revisions = vec![revision, resource_revision];
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            &revisions,
            version_one.expressions(),
            &origins,
            &references,
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                version_one.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    version_one.expressions().to_vec(),
                    revisions,
                    origins,
                    references.clone(),
                ),
            ),
            context,
        )
        .unwrap();

        (
            active,
            function_id,
            pair,
            function_revision_id,
            parameter.id(),
        )
    }


    fn version_six_client_resource_action_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
        ParameterId,
    ) {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([3; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );
        let target = FunctionId::from_bytes([0xd1; 16]);
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            target,
            pair,
            CallSiteId::from_bytes([0xe1; 16]),
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Resource(
                orna_artifact::client_plan::ResourceClientPlan::new(
                    orna_artifact::client_plan::ClientExpressionNode::Await {
                        expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation,
                        }),
                    },
                ),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Parameter(
                    "p_path".to_owned(),
                ),
            )],
        )
        .encode()
        .unwrap();
        let (base, function, pair, revision, parameter) =
            version_five_expression_active_with_parameter(payload);
        let origin = base.function_revisions()[0].declaration_origin();
        let mut references = Vec::new();
        references.push(DefinitionReference::new(
            function,
            revision,
            0,
            DefinitionReferenceTarget::Function(target),
            DefinitionReferenceKind::FunctionCall,
            origin,
        ));
        let mut revisions = base.function_revisions().to_vec();
        let client_revision = revisions
            .iter()
            .find(|candidate| candidate.function() == function)
            .unwrap()
            .clone();
        let client_definition = base.catalogue().function_by_id(function).unwrap();
        let client_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            client_definition,
            client_revision.language_version(),
            client_revision.artifact(),
            base.expressions(),
            &references,
        )
        .unwrap();
        let rebuilt_client_revision = FunctionRevisionRecord::new(
            client_revision.function(),
            client_revision.id(),
            client_revision.revision_number(),
            client_revision.declaration_origin(),
            client_revision.declaration_content_hash(),
            client_semantic_hash,
            client_revision.language_version(),
            client_revision.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let client_revision_index = revisions
            .iter()
            .position(|candidate| candidate.function() == function)
            .unwrap();
        revisions[client_revision_index] = rebuilt_client_revision;
        let context = orna_core::revision::CatalogueHashContext::version_two(standard_v6());
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            base.catalogue(),
            &revisions,
            base.expressions(),
            base.origins(),
            &references,
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                base.source().clone(),
                base.catalogue().clone(),
                catalogue_hash,
                ActiveRevisionContent::new(
                    base.expressions().to_vec(),
                    revisions,
                    base.origins().to_vec(),
                    references,
                ),
            ),
            context,
        )
        .unwrap();
        (active, function, pair, revision, parameter)
    }

    fn version_six_client_action_provenance_active() -> (
        ActiveDatabaseRevision,
        FunctionId,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
        ParameterId,
    ) {
        let (base, child_id, pair, _child_revision_id, parameter) =
            version_six_client_resource_action_active();
        let previous_revision = base
            .function_revisions()
            .iter()
            .find(|revision| revision.function() == child_id)
            .expect("resource child revision is present");
        let parent_id = FunctionId::from_bytes([0xc4; 16]);
        let parent_revision_id = FunctionRevisionId::from_bytes([0xc5; 16]);
        let parent_parameter = ParameterDefinition::new(
            ParameterId::from_bytes([0xc6; 16]),
            "p_path",
            0,
            ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            None,
        );
        let parent = FunctionDefinition::new(
            parent_id,
            QualifiedSemanticName::new(["app", "action_parent"]).unwrap(),
            FunctionDomain::Client,
            vec![parent_parameter.clone()],
            FunctionReturn::Single(ResolvedType::Value(
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            )),
            parent_revision_id,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        );
        let mut functions = base.catalogue().functions().to_vec();
        functions.push(parent.clone());
        let catalogue = CatalogueSnapshot::new_with_functions(
            base.catalogue().revision(),
            base.catalogue().schemas().to_vec(),
            base.catalogue().object_types().to_vec(),
            functions,
        )
        .unwrap();
        let parent_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Call {
                function: child_id,
                arguments: vec![(
                    ParameterId::from_bytes([0xb1; 16]),
                    orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                        parameter: parent_parameter.id(),
                    },
                )],
            },
        )
        .encode()
        .unwrap();
        let parent_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            parent_payload.clone(),
            artifact_payload_digest(&parent_payload).unwrap(),
        )
        .unwrap();
        let parent_parameter_reference = DefinitionReference::new(
            parent_id,
            parent_revision_id,
            0,
            DefinitionReferenceTarget::Parameter {
                owner: parent_id,
                parameter: parent_parameter.id(),
            },
            DefinitionReferenceKind::ParameterRead,
            previous_revision.declaration_origin(),
        );
        let parent_reference = DefinitionReference::new(
            parent_id,
            parent_revision_id,
            1,
            DefinitionReferenceTarget::Function(child_id),
            DefinitionReferenceKind::FunctionCall,
            previous_revision.declaration_origin(),
        );
        let parent_references = vec![parent_parameter_reference, parent_reference.clone()];
        let parent_semantic_hash = function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &parent,
            previous_revision.language_version(),
            &parent_artifact,
            base.expressions(),
            &parent_references,
        )
        .unwrap();
        let parent_revision = FunctionRevisionRecord::new(
            parent_id,
            parent_revision_id,
            previous_revision.revision_number(),
            previous_revision.declaration_origin(),
            previous_revision.declaration_content_hash(),
            parent_semantic_hash,
            previous_revision.language_version(),
            parent_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let mut revisions = base.function_revisions().to_vec();
        revisions.push(parent_revision);
        let mut origins = base.origins().to_vec();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Function(parent_id),
            previous_revision.declaration_origin(),
        ));
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: parent_id,
                parameter: parent_parameter.id(),
            },
            previous_revision.declaration_origin(),
        ));
        let mut references = base.references().to_vec();
        references.extend(parent_references);
        let catalogue_hash = catalogue_digest_with_context(
            base.catalogue_hash_context(),
            &catalogue,
            &revisions,
            base.expressions(),
            &origins,
            &references,
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                base.source().clone(),
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(
                    base.expressions().to_vec(),
                    revisions,
                    origins,
                    references,
                ),
            ),
            base.catalogue_hash_context().clone(),
        )
        .unwrap();
        (
            active,
            parent_id,
            child_id,
            pair,
            parent_revision_id,
            parameter,
        )
    }

    fn action_value(
        active: &ActiveDatabaseRevision,
        domain: ActionTargetDomain,
        target: FunctionId,
        pair: RevisionPair,
        call_site: CallSiteId,
        arguments: Vec<FunctionArgument>,
        result_type: TypeId,
    ) -> RuntimeValue {
        let descriptor = ClientActionDescriptor::new(
            domain, target, pair, call_site, arguments, result_type,
        );
        let payload = encode_action_payload(active, &descriptor).unwrap();
        let registry = super::registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        )
        .unwrap();
        RuntimeValue::Opaque(
            OpaqueValue::new(active, &registry, super::STD_ACTION_TYPE_ID, payload).unwrap(),
        )
    }

    fn boolean_parameter() -> ParameterDefinition {
        ParameterDefinition::new(
            ParameterId::from_bytes([0xa1; 16]),
            "enabled",
            0,
            ResolvedType::Scalar(StandardScalar::Boolean),
            None,
        )
    }

    fn version_one_active_with_shape(
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        volatility: FunctionVolatility,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        let (initial, function, pair, function_revision) = version_one_active(true);
        let prior = initial.catalogue().function_by_id(function).unwrap();
        let definition = FunctionDefinition::new(
            function,
            prior.name().clone(),
            domain,
            parameters,
            return_type,
            function_revision,
            security,
            None,
            volatility,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            initial.catalogue().revision(),
            initial.catalogue().schemas().to_vec(),
            initial.catalogue().object_types().to_vec(),
            vec![definition.clone()],
        )
        .unwrap();
        let payload = match domain {
            FunctionDomain::Client => b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
            FunctionDomain::Server => vec![0x53],
        };
        let (kind, format) = match domain {
            FunctionDomain::Client => (ExecutableArtifactKind::Client, "orna.client-plan"),
            FunctionDomain::Server => (ExecutableArtifactKind::Server, "orna.server-plan"),
        };
        let artifact = ExecutableArtifact::new(
            kind,
            format,
            1,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let prior_revision = &initial.function_revisions()[0];
        let revision = FunctionRevisionRecord::new(
            function,
            function_revision,
            prior_revision.revision_number(),
            prior_revision.declaration_origin(),
            prior_revision.declaration_content_hash(),
            function_semantic_digest(&definition, "orna.language/1", &artifact, &[], &[]).unwrap(),
            "orna.language/1",
            artifact,
        )
        .unwrap();
        let mut origins = initial.origins().to_vec();
        origins.extend(definition.parameters().iter().map(|parameter| {
            DefinitionOrigin::new(
                DefinitionIdentity::Parameter {
                    owner: function,
                    parameter: parameter.id(),
                },
                prior_revision.declaration_origin(),
            )
        }));
        if let FunctionReturn::Rows(columns) = definition.return_type() {
            origins.extend(columns.iter().map(|column| {
                DefinitionOrigin::new(
                    DefinitionIdentity::FunctionReturnColumn {
                        owner: function,
                        ordinal: column.ordinal(),
                    },
                    prior_revision.declaration_origin(),
                )
            }));
        }
        let catalogue_hash = catalogue_digest(
            &catalogue,
            std::slice::from_ref(&revision),
            initial.expressions(),
            &origins,
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new(
            pair,
            initial.source().clone(),
            catalogue,
            catalogue_hash,
            initial.expressions().to_vec(),
            vec![revision],
            origins,
            Vec::new(),
        )
        .unwrap();

        (active, function, pair, function_revision)
    }

    #[test]
    fn action_trigger_rejects_domain_mismatch_and_stale_revision() {
        let (active, parent_function, pair, parent_revision, parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(None);

        let domain_mismatch = action_value(
            &active,
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0xf7; 16]),
            vec![argument.clone()],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        assert_eq!(
            trigger_client_action(
                &active,
                &domain_mismatch,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::TargetMismatch),
        );

        let stale_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0xf8; 16]),
            pair.catalogue(),
        );
        let stale_target_revision = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            stale_pair,
            CallSiteId::from_bytes([0xf9; 16]),
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        assert_eq!(
            trigger_client_action(
                &active,
                &stale_target_revision,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::RevisionMismatch),
        );
    }

    #[test]
    fn action_trigger_rejects_wrong_result_type_and_non_single_column_target() {
        let (active, parent_function, pair, parent_revision, parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0xfa; 16]),
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let wrong_type = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            pair,
            CallSiteId::from_bytes([0xfb; 16]),
            vec![argument],
            orna_standard::INTEGER_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(None);
        assert_eq!(
            trigger_client_action(
                &active,
                &wrong_type,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::ResultTypeMismatch),
        );

        let (multi_column_active, multi_column_function, multi_column_pair, multi_column_revision) =
            version_two_server_rows_active();
        let multi_column_auth = authorise(multi_column_pair, multi_column_function);
        let multi_column_parent = ClientExecutionContext {
            pair: multi_column_pair,
            function: multi_column_function,
            function_revision: multi_column_revision,
            parent_invocation_id: InvocationId::from_bytes([0xfc; 16]),
            observer_lineage: None,
        };
        let multi_column_action = action_value(
            &multi_column_active,
            ActionTargetDomain::Server,
            multi_column_function,
            multi_column_pair,
            CallSiteId::from_bytes([0xfd; 16]),
            Vec::new(),
            orna_standard::BOOLEAN_TYPE_ID,
        );
        let mut multi_column_state = ClientStateStore::default();
        let mut multi_column_action_state = ClientActionState::default();
        let mut multi_column_executor = RecordingActionExecutor::new(None);
        assert_eq!(
            trigger_client_action(
                &multi_column_active,
                &multi_column_action,
                &multi_column_auth,
                &multi_column_parent,
                &mut multi_column_action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut multi_column_state,
                &mut multi_column_executor,
            ),
            Err(ClientActionError::ResultTypeMismatch),
        );
    }

    #[test]
    fn action_target_result_type_rejects_one_column_rows() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::Named(TypeId::from_bytes([0x66; 16])),
            )]),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Server,
            function,
            pair,
            CallSiteId::from_bytes([0xfe; 16]),
            Vec::new(),
            TypeId::from_bytes([0x66; 16]),
        );

        assert_eq!(
            action_target_result_type(&active, &descriptor),
            Err(ClientActionError::ResultTypeMismatch)
        );
    }
    #[test]
    fn action_target_result_type_rejects_stream_targets() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Stream(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Server,
            function,
            pair,
            CallSiteId::from_bytes([0x70; 16]),
            Vec::new(),
            orna_standard::INTEGER_TYPE_ID,
        );

        assert_eq!(
            action_target_result_type(&active, &descriptor),
            Err(ClientActionError::ResultTypeMismatch)
        );
    }


    #[test]
    fn action_payload_rejects_malformed_and_noncanonical_frames() {
        let (active, target, pair, _) = version_two_value_active(
            orna_standard::INTEGER_TYPE_ID,
            orna_standard::INTEGER_TYPE_ID,
        );
        let parameter = ParameterId::from_bytes([0x71; 16]);
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0x72; 16]),
            vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
            orna_standard::INTEGER_TYPE_ID,
        );
        let payload = encode_action_payload(&active, &descriptor).unwrap();
        let magic_length = super::ACTION_MAGIC.len();
        let body_offset = magic_length + 4;
        let metadata_length = 1 + (16 * 5);
        let count_offset = body_offset + metadata_length;
        let first_parameter_offset = count_offset + 4;
        let frame_length_offset = first_parameter_offset + 16;
        let frame_offset = frame_length_offset + 4;

        let mut invalid_magic = payload.clone();
        invalid_magic[0] ^= 0xff;

        let mut truncated = payload.clone();
        truncated.pop();

        let mut invalid_count = payload.clone();
        invalid_count[count_offset..count_offset + 4]
            .copy_from_slice(&u32::MAX.to_be_bytes());

        let two_argument_descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0x73; 16]),
            vec![
                FunctionArgument::new(
                    ParameterId::from_bytes([1; 16]),
                    RuntimeValue::Integer(1),
                )
                .unwrap(),
                FunctionArgument::new(
                    ParameterId::from_bytes([2; 16]),
                    RuntimeValue::Integer(2),
                )
                .unwrap(),
            ],
            orna_standard::INTEGER_TYPE_ID,
        );
        let two_argument_payload = encode_action_payload(&active, &two_argument_descriptor).unwrap();
        let first_two_argument_offset = first_parameter_offset;
        let first_frame_length = u32::from_be_bytes(
            two_argument_payload[first_two_argument_offset + 16..first_two_argument_offset + 20]
                .try_into()
                .unwrap(),
        ) as usize;
        let second_parameter_offset =
            first_two_argument_offset + 16 + 4 + first_frame_length;
        let mut invalid_order = two_argument_payload;
        invalid_order[second_parameter_offset..second_parameter_offset + 16]
            .copy_from_slice(&[0; 16]);

        let mut trailing = payload.clone();
        trailing.push(0xaa);
        let body_length = u32::from_be_bytes(
            trailing[magic_length..magic_length + 4]
                .try_into()
                .unwrap(),
        );
        trailing[magic_length..magic_length + 4]
            .copy_from_slice(&(body_length + 1).to_be_bytes());

        let mut invalid_orv3_frame = payload;
        invalid_orv3_frame[frame_offset..frame_offset + 4].copy_from_slice(b"ORV2");

        for malformed in [
            invalid_magic,
            truncated,
            invalid_count,
            invalid_order,
            trailing,
            invalid_orv3_frame,
        ] {
            assert!(matches!(
                decode_action_payload(&active, &malformed),
                Err(ClientActionError::InvalidPayload(_))
            ));
        }
    }

    #[test]
    fn action_payload_encodes_multiple_arguments_in_parameter_order_and_round_trips() {
        let (active, target, pair, _) = version_two_value_active(
            orna_standard::INTEGER_TYPE_ID,
            orna_standard::INTEGER_TYPE_ID,
        );
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0x74; 16]),
            vec![
                FunctionArgument::new(
                    ParameterId::from_bytes([1; 16]),
                    RuntimeValue::Integer(11),
                )
                .unwrap(),
                FunctionArgument::new(
                    ParameterId::from_bytes([2; 16]),
                    RuntimeValue::Integer(22),
                )
                .unwrap(),
            ],
            orna_standard::INTEGER_TYPE_ID,
        );
        let payload = encode_action_payload(&active, &descriptor).unwrap();
        let body_offset = super::ACTION_MAGIC.len() + 4;
        let first_parameter_offset = body_offset + 1 + (16 * 5) + 4;
        let first_frame_length = u32::from_be_bytes(
            payload[first_parameter_offset + 16..first_parameter_offset + 20]
                .try_into()
                .unwrap(),
        ) as usize;
        let second_parameter_offset = first_parameter_offset + 16 + 4 + first_frame_length;
        assert_eq!(&payload[first_parameter_offset..first_parameter_offset + 16], &[1; 16]);
        assert_eq!(&payload[second_parameter_offset..second_parameter_offset + 16], &[2; 16]);

        let decoded = decode_action_payload(&active, &payload).unwrap();
        assert_eq!(decoded, descriptor);
        assert_eq!(encode_action_payload(&active, &decoded).unwrap(), payload);
    }

    #[test]
    fn action_trigger_rejects_repeated_pending_server_request_without_mutating_generation() {
        let (active, parent_function, pair, parent_revision, _parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let auth = authorise(pair, parent_function);
        let observer_root = InvocationId::from_bytes([0xfb; 16]);
        let observer_parent = InvocationId::from_bytes([0xfa; 16]);
        let observer_current = InvocationId::from_bytes([0xf9; 16]);
        let observer_lineage = super::ObserverLineage::top_level(observer_root)
            .with_parent_and_current(observer_parent, observer_current);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0xfe; 16]),
            observer_lineage: Some(observer_lineage),
        };
        assert_eq!(parent.observer_root_invocation_id(), observer_root);
        assert_eq!(parent.observer_parent_invocation_id(), observer_current);
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0xd3; 16]),
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            pair,
            CallSiteId::from_bytes([0xff; 16]),
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(None);

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::Pending),
        );
        let first_request = executor.executed[0].clone();
        assert_eq!(
            first_request
                .invocation_context()
                .expect("server action carries observer provenance")
                .parent_invocation_id(),
            observer_current
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Loading);

        // The one-active-action contract rejects a repeated trigger while loading.
        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::Pending),
        );
        assert_eq!(executor.executed.len(), 1);
        assert_eq!(action_state.invocation_id(), Some(first_request.request_id()));
        assert_eq!(action_state.generation(), Some(first_request.generation()));
        assert_eq!(action_state.status(), ClientResourceStatus::Loading);
    }

    #[test]
    fn action_trigger_after_terminal_completion_allocates_fresh_request_identity() {
        let (active, parent_function, pair, parent_revision, _parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0xd3; 16]),
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            pair,
            CallSiteId::from_bytes([0xff; 16]),
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Text(
            "completed".to_owned(),
        )));

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Completed),
        );
        let first_request = executor.executed[0].clone();
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(action_state.invocation_id(), None);
        assert_eq!(action_state.generation(), None);

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Completed),
        );
        assert_eq!(executor.executed.len(), 2);
        let second_request = executor.executed[1].clone();
        assert_ne!(first_request.request_id(), second_request.request_id());
        assert!(second_request.generation().value() > first_request.generation().value());
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    }

    #[test]
    fn action_trigger_redacts_executor_failure() {
        let (active, parent_function, pair, parent_revision, _parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0xd3; 16]),
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            pair,
            CallSiteId::from_bytes([0x02; 16]),
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = FailingActionExecutor::default();

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Failed {
                code: ACTION_FAILURE_CODE.to_owned(),
            }),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    }

    #[test]
    fn action_payload_round_trip_and_rejects_trailing_bytes() {
        let (active, target, pair, _) = version_two_value_active(
            orna_standard::INTEGER_TYPE_ID,
            orna_standard::INTEGER_TYPE_ID,
        );
        let parameter = ParameterId::from_bytes([0x71; 16]);
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0x72; 16]),
            vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
            orna_standard::INTEGER_TYPE_ID,
        );
        let payload = encode_action_payload(&active, &descriptor).unwrap();
        assert_eq!(
            decode_action_payload(&active, &payload).unwrap(),
            descriptor
        );
        let mut trailing = payload.clone();
        trailing.push(0);
        assert!(decode_action_payload(&active, &trailing).is_err());
        let stale_descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x73; 16]),
                pair.catalogue(),
            ),
            CallSiteId::from_bytes([0x74; 16]),
            vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
            orna_standard::INTEGER_TYPE_ID,
        );
        let stale_payload = encode_action_payload(&active, &stale_descriptor).unwrap();
        assert_eq!(
            decode_action_payload(&active, &stale_payload),
            Err(ClientActionError::RevisionMismatch),
        );
    }

    #[test]
    fn action_pending_completion_retains_generation_and_redacts_failure() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            active.catalogue_hash(),
        );
        let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, vec![]).unwrap();
        let generation = request.generation();
        let request_id = request.request_id();
        let mut action_state = ClientActionState::default();
        action_state.set_resource(resource);
        let mut executor = RecordingActionExecutor::new(None);
        assert_eq!(complete_client_action(&active, &mut action_state, request.pending(), &mut executor), Err(ClientActionError::Pending));
        assert_eq!(action_state.generation(), Some(generation));
        let failed = ClientResourceCompletion::Failed { request_id, key, generation, code: "secret.internal.detail".to_owned() };
        assert_eq!(complete_client_action(&active, &mut action_state, failed, &mut executor), Ok(ClientActionOutcome::Failed { code: ACTION_FAILURE_CODE.to_owned() }));
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    }

    #[test]
    fn action_cancellation_uses_executor_and_rejects_late_completion() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            active.catalogue_hash(),
        );
        let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, vec![]).unwrap();
        let mut action_state = ClientActionState::default();
        action_state.set_resource(resource);
        action_state.stage_invocation(request.request_id());
        action_state.stage_request(request.clone());
        assert_eq!(action_state.invocation_id(), Some(request.request_id()));
        let mut executor = DeterministicClientResourceExecutor::new(
            |_: &ClientResourceRequest| Ok(RuntimeValue::Boolean(true)),
        );

        assert_eq!(
            super::cancel_client_action_with_executor(
                &active,
                &mut action_state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(
            complete_client_action(&active, &mut action_state, request.ready(RuntimeValue::Boolean(true)), &mut executor),
            Err(ClientActionError::StaleCompletion),
        );
        assert_eq!(action_state.generation(), None);
    }

    #[test]
    fn action_trigger_rejects_non_action_values() {
        let (active, function, pair, revision) = version_one_active(true);
        let auth = authorise(pair, function);
        let parent = ClientExecutionContext {
            pair,
            function,
            function_revision: revision,
            parent_invocation_id: InvocationId::from_bytes([0xf1; 16]),
            observer_lineage: None,
        };
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
            Ok(RuntimeValue::Boolean(true))
        });
        assert_eq!(
            trigger_client_action(
                &active,
                &RuntimeValue::Boolean(true),
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor
            ),
            Err(super::ClientActionError::InvalidValue)
        );
    }
    #[test]
    fn action_current_generation_mismatched_request_is_stale_but_same_request_malformed_completion_cancels() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            active.catalogue_hash(),
        );
        let wrong_key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7b; 16]),
            digest,
            active.catalogue_hash(),
        );

        let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, vec![]).unwrap();
        let generation = request.generation();
        let mut stale_state = ClientActionState::default();
        stale_state.set_resource(resource);
        let mut stale_executor = RecordingActionExecutor::new(None);
        assert_eq!(
            complete_client_action(
                &active,
                &mut stale_state,
                ClientResourceCompletion::Ready {
                    request_id: request.request_id(),
                    key: wrong_key,
                    generation,
                    value: RuntimeValue::Boolean(true),
                },
                &mut stale_executor,
            ),
            Err(ClientActionError::StaleCompletion),
        );
        assert_eq!(stale_state.status(), ClientResourceStatus::Loading);
        assert!(stale_executor.cancelled.is_empty());
        assert_eq!(
            complete_client_action(
                &active,
                &mut stale_state,
                ClientResourceCompletion::Ready {
                    request_id: request.request_id(),
                    key,
                    generation,
                    value: RuntimeValue::Integer(1),
                },
                &mut stale_executor,
            ),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(stale_state.status(), ClientResourceStatus::Idle);
        assert_eq!(stale_executor.cancelled, vec![request]);

        for malformed_kind in [0_u8, 1_u8] {
            let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
            let request = resource.begin_request(&active, vec![]).unwrap();
            assert_eq!(request.generation(), generation);
            let completion = if malformed_kind == 0 {
                ClientResourceCompletion::Ready {
                    request_id: request.request_id(),
                    key,
                    generation,
                    value: RuntimeValue::Integer(1),
                }
            } else {
                ClientResourceCompletion::Failed {
                    request_id: request.request_id(),
                    key,
                    generation,
                    code: String::new(),
                }
            };
            let mut action_state = ClientActionState::default();
            action_state.set_resource(resource);
            action_state.stage_request(request.clone());
            let mut executor = RecordingActionExecutor::new(None);
            assert_eq!(
                complete_client_action(&active, &mut action_state, completion, &mut executor),
                Ok(ClientActionOutcome::Cancelled),
            );
            assert_eq!(action_state.status(), ClientResourceStatus::Idle);
            assert_eq!(executor.cancelled, vec![request]);
        }
    }

    #[test]
    fn action_uncertain_cancel_retains_loading_request() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
        let key = ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            digest,
            active.catalogue_hash(),
        );

        for malformed_cancel in [false, true] {
            let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
            let request = resource.begin_request(&active, vec![]).unwrap();
            let generation = request.generation();
            let mut action_state = ClientActionState::default();
            action_state.set_resource(resource);
            action_state.stage_request(request.clone());
            let mut executor = if malformed_cancel {
                RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(7))
            } else {
                RecordingActionExecutor::new(None).with_cancel_pending()
            };
            let malformed = request.clone().ready(RuntimeValue::Integer(1));

            assert_eq!(
                complete_client_action(&active, &mut action_state, malformed, &mut executor),
                Err(ClientActionError::Pending),
            );
            assert_eq!(action_state.status(), ClientResourceStatus::Loading);
            assert_eq!(action_state.generation(), Some(generation));
            assert_eq!(executor.cancelled, vec![request]);
        }
    }

    #[test]
    fn action_local_resource_pending_is_cancelled_and_fails_redacted_with_fresh_parent() {
        let (active, parent_function, target, pair, revision, parameter) =
            version_six_client_action_provenance_active();
        let auth = authorise(pair, parent_function);
        let enclosing_parent = InvocationId::from_bytes([0xf5; 16]);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: revision,
            parent_invocation_id: enclosing_parent,
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            parameter,
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0xe2; 16]),
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let grant = capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap();
        let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
        let mut executor = RecordingActionExecutor::new(None);

        for previous_parent in [None, Some(enclosing_parent)] {
            assert_eq!(
                trigger_client_action(
                    &active,
                    &action,
                    &auth,
                    &parent,
                    &mut action_state,
                    &[],
                    &grants,
                    &mut state,
                    &mut executor,
                ),
                Ok(ClientActionOutcome::Failed {
                    code: ACTION_FAILURE_CODE.to_owned(),
                }),
            );
            assert_eq!(action_state.status(), ClientResourceStatus::Idle);
            assert_eq!(executor.cancelled.len(), executor.executed.len());
            let request = executor.executed.last().unwrap();
            let cancelled = executor.cancelled.last().unwrap();
            assert_eq!(request, cancelled);
            let nested_parent = request
                .invocation_context()
                .expect("nested resource carries invocation provenance")
                .parent_invocation_id();
            assert_ne!(nested_parent, enclosing_parent);
            if let Some(previous_parent) = previous_parent {
                assert_ne!(nested_parent, previous_parent);
            }
            assert!(state.resource(request.key()).is_none());
        }
    }

    #[test]
    fn action_trigger_executes_a_verified_standard_server_target() {
        let (active, parent_function, _pair, parent_revision) = version_two_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::Function(orna_standard::STD_INVOKE_ECHO_FUNCTION_ID),
            DefinitionReferenceKind::FunctionCall,
            orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            )
            .encode()
            .unwrap(),
        );
        let pair = active.pair();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("standard action fixture has a pinned snapshot");
        let argument = FunctionArgument::new(
            orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
            RuntimeValue::Integer(42),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Server,
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            CallSiteId::from_bytes([0xf6; 16]),
            vec![argument.clone()],
            orna_standard::INTEGER_TYPE_ID,
        );
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0xf7; 16]),
            observer_lineage: None,
        };
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(42)));

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Completed),
        );
        let request = executor.executed.first().expect("action was dispatched");
        assert_eq!(
            request.target(),
            InvocationTarget::verified_standard(
                orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
                pair,
                standard.revision(),
                orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )
        );
        assert_eq!(request.arguments(), &[argument]);
        assert_eq!(request.expected_type(), ResolvedType::Scalar(StandardScalar::Integer));
        assert_ne!(
            request
                .invocation_context()
                .expect("server action carries invocation provenance")
                .call_site_id(),
            CallSiteId::from_bytes([0xf6; 16]),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    }

    #[test]
    fn action_trigger_executes_a_local_client_target() {
        let (active, parent_function, target, pair, revision) = version_two_local_action_active();
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: revision,
            parent_invocation_id: InvocationId::from_bytes([0xf3; 16]),
            observer_lineage: None,
        };
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0xf4; 16]),
            Vec::new(),
            orna_standard::BOOLEAN_TYPE_ID,
        );
        let payload = encode_action_payload(&active, &descriptor).unwrap();
        let registry = super::registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .expect("action test fixture has a standard snapshot"),
        )
        .unwrap();
        let action = OpaqueValue::new(
            &active,
            &registry,
            super::STD_ACTION_TYPE_ID,
            payload,
        )
        .unwrap();
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = DeterministicClientResourceExecutor::new(
            |_: &ClientResourceRequest| Ok(RuntimeValue::Boolean(false)),
        );

        assert_eq!(
            trigger_client_action(
                &active,
                &RuntimeValue::Opaque(action),
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Completed),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    }


    #[test]
    fn action_trigger_does_not_forward_forged_call_site_metadata() {
        let (active, parent_function, pair, parent_revision, _parameter) =
            version_six_client_resource_action_active();
        let target = FunctionId::from_bytes([0xd1; 16]);
        let forged_call_site = CallSiteId::from_bytes([0x9a; 16]);
        let auth = authorise(pair, parent_function);
        let parent = ClientExecutionContext {
            pair,
            function: parent_function,
            function_revision: parent_revision,
            parent_invocation_id: InvocationId::from_bytes([0x9b; 16]),
            observer_lineage: None,
        };
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0xd3; 16]),
            RuntimeValue::Text("/tmp/action".to_owned()),
        )
        .unwrap();
        let action = action_value(
            &active,
            ActionTargetDomain::Server,
            target,
            pair,
            forged_call_site,
            vec![argument],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = RecordingActionExecutor::new(None);

        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::Pending),
        );
        let request = executor.executed.first().expect("action was dispatched");
        let context = request
            .invocation_context()
            .expect("server action carries invocation provenance");
        assert_ne!(context.call_site_id(), forged_call_site);
        assert_eq!(context.parent_invocation_id(), parent.parent_invocation_id());
        assert_eq!(request.target().function(), target);
    }

    #[test]
    fn action_trigger_rejects_unreferenced_target_provenance() {
        let (original_active, target, pair, revision) = version_two_value_active(
            orna_standard::INTEGER_TYPE_ID,
            orna_standard::INTEGER_TYPE_ID,
        );
        let standard_v6 = orna_standard::verify_standard_library_v6_snapshot(
            orna_standard::retained_standard_library_v6_snapshot().unwrap(),
        )
        .unwrap();
        let context = orna_core::revision::CatalogueHashContext::version_two(standard_v6);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            original_active.catalogue(),
            original_active.function_revisions(),
            original_active.expressions(),
            original_active.origins(),
            original_active.references(),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                pair,
                original_active.source().clone(),
                original_active.catalogue().clone(),
                catalogue_hash,
                ActiveRevisionContent::new(
                    original_active.expressions().to_vec(),
                    original_active.function_revisions().to_vec(),
                    original_active.origins().to_vec(),
                    original_active.references().to_vec(),
                ),
            ),
            context,
        )
        .unwrap();
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([0x75; 16]),
            Vec::new(),
            orna_standard::INTEGER_TYPE_ID,
        );
        let payload = encode_action_payload(&active, &descriptor).unwrap();
        let registry = super::registered_opaque_codecs(
            active
                .catalogue_hash_context()
                .standard()
                .expect("action test fixture has a standard snapshot"),
        )
        .unwrap();
        let action = OpaqueValue::new(
            &active,
            &registry,
            super::STD_ACTION_TYPE_ID,
            payload,
        )
        .unwrap();
        let auth = authorise(pair, target);
        let parent = ClientExecutionContext {
            pair,
            function: target,
            function_revision: revision,
            parent_invocation_id: InvocationId::from_bytes([0xf2; 16]),
            observer_lineage: None,
        };
        let mut state = ClientStateStore::default();
        let mut action_state = ClientActionState::default();
        let mut executor = DeterministicClientResourceExecutor::new(
            |_: &ClientResourceRequest| Ok(RuntimeValue::Integer(1)),
        );

        assert_eq!(
            trigger_client_action(
                &active,
                &RuntimeValue::Opaque(action),
                &auth,
                &parent,
                &mut action_state,
                &[],
                &capability::LocalCapabilityGrantSet::default(),
                &mut state,
                &mut executor,
            ),
            Err(ClientActionError::TargetMismatch),
        );
    }

    #[test]
    fn action_payload_rejects_noncanonical_argument_order() {
        let (active, target, pair, _) = version_two_value_active(
            orna_standard::INTEGER_TYPE_ID,
            orna_standard::INTEGER_TYPE_ID,
        );
        let first =
            FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(1))
                .unwrap();
        let second =
            FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(2))
                .unwrap();
        let descriptor = ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            target,
            pair,
            CallSiteId::from_bytes([3; 16]),
            vec![first, second],
            orna_standard::INTEGER_TYPE_ID,
        );
        assert!(encode_action_payload(&active, &descriptor).is_err());
    }
}

#[cfg(test)]
mod runtime_abi {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(super) struct StringView {
        pub(super) data: *const c_char,
        pub(super) len: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(super) struct BytesView {
        pub(super) data: *const u8,
        pub(super) len: usize,
    }

    pub(super) type ReleaseFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize);

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct OwnedBytes {
        pub(super) data: *mut u8,
        pub(super) len: usize,
        pub(super) owner: *mut c_void,
        pub(super) release: ReleaseFn,
    }

    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct StatusCode(pub(super) i32);

    #[allow(dead_code, non_upper_case_globals)]
    impl StatusCode {
        pub(super) const Ok: Self = Self(0);
        pub(super) const InvalidArgument: Self = Self(1);
        pub(super) const Unsupported: Self = Self(2);
        pub(super) const NotFound: Self = Self(3);
        pub(super) const Busy: Self = Self(4);
        pub(super) const Cancelled: Self = Self(5);
        pub(super) const Failed: Self = Self(6);
        pub(super) const Internal: Self = Self(7);
        pub(super) const StaleRevision: Self = Self(8);
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(super) struct Status {
        pub(super) code: StatusCode,
        pub(super) message: StringView,
    }

    pub(super) type Handle = u64;
    pub(super) type RuntimeHandle = Handle;
    pub(super) type SurfaceHandle = Handle;
    pub(super) type NodeHandle = Handle;
    pub(super) type ActionHandle = Handle;
    pub(super) type ModelHandle = Handle;
    pub(super) type RequestHandle = Handle;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct SurfaceClosedEvent {
        pub(super) surface: SurfaceHandle,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct DiagnosticEvent {
        pub(super) status: Status,
    }

    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct ThreadModel(pub(super) i32);

    #[allow(dead_code, non_upper_case_globals)]
    impl ThreadModel {
        pub(super) const ClientEventLoop: Self = Self(1);
        pub(super) const RuntimeEventLoop: Self = Self(2);
        pub(super) const CallerPumps: Self = Self(3);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ContractVersion {
        pub(super) name: StringView,
        pub(super) major: u32,
        pub(super) minor: u32,
        pub(super) features: *const StringView,
        pub(super) feature_count: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct SinkOffer {
        pub(super) type_name: StringView,
        pub(super) media_types: *const StringView,
        pub(super) media_type_count: usize,
        pub(super) supports_streaming: u8,
        pub(super) preference_rank: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct Descriptor {
        pub(super) abi_major: u32,
        pub(super) abi_minor: u32,
        pub(super) runtime_name: StringView,
        pub(super) runtime_version: StringView,
        pub(super) build_id: StringView,
        pub(super) platform: StringView,
        pub(super) thread_model: ThreadModel,
        pub(super) features: u64,
        pub(super) sinks: *const SinkOffer,
        pub(super) sink_count: usize,
        pub(super) contracts: *const ContractVersion,
        pub(super) contract_count: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ValueRef {
        pub(super) handle: Handle,
        pub(super) type_name: StringView,
        pub(super) canonical_encoding: BytesView,
    }

    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct UiOperationKind(pub(super) i32);

    #[allow(non_upper_case_globals)]
    impl UiOperationKind {
        pub(super) const MountNode: Self = Self(1);
        pub(super) const UnmountNode: Self = Self(2);
        pub(super) const SetProperty: Self = Self(3);
        pub(super) const ClearProperty: Self = Self(4);
        pub(super) const InsertChild: Self = Self(5);
        pub(super) const RemoveChild: Self = Self(6);
        pub(super) const MoveChild: Self = Self(7);
        pub(super) const BindAction: Self = Self(8);
        pub(super) const UnbindAction: Self = Self(9);
        pub(super) const SetFocus: Self = Self(10);
        pub(super) const SetAccessibility: Self = Self(11);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct MountNode {
        pub(super) node: NodeHandle,
        pub(super) parent: NodeHandle,
        pub(super) slot: StringView,
        pub(super) ordinal: usize,
        pub(super) contract_name: StringView,
        pub(super) contract_major: u32,
        pub(super) contract_minor: u32,
        pub(super) explicit_key: ValueRef,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct SetProperty {
        pub(super) node: NodeHandle,
        pub(super) property: StringView,
        pub(super) value: ValueRef,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ChildOperation {
        pub(super) parent: NodeHandle,
        pub(super) slot: StringView,
        pub(super) child: NodeHandle,
        pub(super) ordinal: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct BindAction {
        pub(super) node: NodeHandle,
        pub(super) event_name: StringView,
        pub(super) action: ActionHandle,
        pub(super) input_type: StringView,
    }

    #[repr(C)]
    pub(super) union UiOperationArgs {
        pub(super) mount_node: MountNode,
        pub(super) unmount_node: NodeHandle,
        pub(super) set_property: SetProperty,
        pub(super) child: ChildOperation,
        pub(super) bind_action: BindAction,
    }

    #[repr(C)]
    pub(super) struct UiOperation {
        pub(super) kind: UiOperationKind,
        pub(super) as_: UiOperationArgs,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct UiBatch {
        pub(super) semantic_revision: u64,
        pub(super) operations: *const UiOperation,
        pub(super) operation_count: usize,
    }

    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct EventKind(pub(super) i32);

    #[allow(non_upper_case_globals)]
    impl EventKind {
        pub(super) const Action: Self = Self(1);
        pub(super) const FocusChanged: Self = Self(2);
        pub(super) const LayoutStateChanged: Self = Self(3);
        pub(super) const SurfaceClosed: Self = Self(4);
        pub(super) const ModelRangeRequest: Self = Self(5);
        pub(super) const ModelChildrenRequest: Self = Self(6);
        pub(super) const Diagnostic: Self = Self(7);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ActionEvent {
        pub(super) surface: SurfaceHandle,
        pub(super) node: NodeHandle,
        pub(super) action: ActionHandle,
        pub(super) payload: ValueRef,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct LayoutStateEvent {
        pub(super) surface: SurfaceHandle,
        pub(super) node: NodeHandle,
        pub(super) semantic_state_name: StringView,
        pub(super) semantic_state: ValueRef,
        pub(super) opaque_runtime_state: BytesView,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ModelRangeRequest {
        pub(super) request: RequestHandle,
        pub(super) model: ModelHandle,
        pub(super) start: u64,
        pub(super) count: u64,
        pub(super) sort_filter_token: StringView,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ModelChildrenRequest {
        pub(super) request: RequestHandle,
        pub(super) model: ModelHandle,
        pub(super) parent_key: ValueRef,
    }

    #[repr(C)]
    pub(super) union RuntimeEventArgs {
        pub(super) action: ActionEvent,
        pub(super) layout_state: LayoutStateEvent,
        pub(super) range_request: ModelRangeRequest,
        pub(super) children_request: ModelChildrenRequest,
        pub(super) surface_closed: SurfaceClosedEvent,
        pub(super) diagnostic: DiagnosticEvent,
    }

    #[repr(C)]
    pub(super) struct RuntimeEvent {
        pub(super) kind: EventKind,
        pub(super) as_: RuntimeEventArgs,
    }

    pub(super) type LogFn = unsafe extern "C" fn(*mut c_void, u32, StringView, StringView);
    pub(super) type EmitRuntimeEventFn =
        unsafe extern "C" fn(*mut c_void, RuntimeHandle, *const RuntimeEvent) -> Status;
    pub(super) type CompleteModelRequestFn =
        unsafe extern "C" fn(*mut c_void, RequestHandle, ValueRef) -> Status;
    pub(super) type FailModelRequestFn =
        unsafe extern "C" fn(*mut c_void, RequestHandle, Status) -> Status;
    pub(super) type ReadActionMetadataFn =
        unsafe extern "C" fn(*mut c_void, ActionHandle, *mut OwnedBytes) -> Status;
    pub(super) type ReadValueDebugJsonFn =
        unsafe extern "C" fn(*mut c_void, ValueRef, *mut OwnedBytes) -> Status;
    pub(super) type MonotonicTimeFn = unsafe extern "C" fn(*mut c_void) -> u64;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct ClientApi {
        pub(super) abi_major: u32,
        pub(super) abi_minor: u32,
        pub(super) context: *mut c_void,
        pub(super) log: LogFn,
        pub(super) emit_runtime_event: EmitRuntimeEventFn,
        pub(super) complete_model_request: CompleteModelRequestFn,
        pub(super) fail_model_request: FailModelRequestFn,
        pub(super) read_action_metadata: ReadActionMetadataFn,
        pub(super) read_value_debug_json: ReadValueDebugJsonFn,
        pub(super) monotonic_time_ns: MonotonicTimeFn,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct RuntimeCreateOptions {
        pub(super) client: *const ClientApi,
        pub(super) locale: StringView,
        pub(super) timezone: StringView,
        pub(super) theme: StringView,
        pub(super) accessibility_preferences_json: StringView,
        pub(super) runtime_configuration_json: StringView,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct SurfaceCreateOptions {
        pub(super) surface_kind: StringView,
        pub(super) title: StringView,
        pub(super) state_profile: StringView,
        pub(super) opaque_runtime_restore_state: BytesView,
    }

    pub(super) type DescribeFn = unsafe extern "C" fn() -> *const Descriptor;
    pub(super) type CreateFn =
        unsafe extern "C" fn(*const RuntimeCreateOptions, *mut RuntimeHandle) -> Status;
    pub(super) type DestroyFn = unsafe extern "C" fn(RuntimeHandle);
    pub(super) type StartEventLoopFn = unsafe extern "C" fn(RuntimeHandle) -> Status;
    pub(super) type PollEventLoopFn = unsafe extern "C" fn(RuntimeHandle, u32) -> Status;
    pub(super) type RequestShutdownFn = unsafe extern "C" fn(RuntimeHandle) -> Status;
    pub(super) type CreateSurfaceFn = unsafe extern "C" fn(
        RuntimeHandle,
        *const SurfaceCreateOptions,
        *mut SurfaceHandle,
    ) -> Status;
    pub(super) type DestroySurfaceFn = unsafe extern "C" fn(RuntimeHandle, SurfaceHandle) -> Status;
    pub(super) type ApplyUiBatchFn =
        unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *const UiBatch) -> Status;
    pub(super) type SetSurfaceVisibleFn =
        unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, u8) -> Status;
    pub(super) type CaptureSemanticStateFn =
        unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *mut OwnedBytes) -> Status;
    pub(super) type CaptureOpaqueStateFn =
        unsafe extern "C" fn(RuntimeHandle, SurfaceHandle, *mut OwnedBytes) -> Status;
    pub(super) type ApplyModelRowsFn =
        unsafe extern "C" fn(RuntimeHandle, RequestHandle, ValueRef) -> Status;
    pub(super) type CancelRequestFn = unsafe extern "C" fn(RuntimeHandle, RequestHandle) -> Status;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct RuntimeApi {
        pub(super) abi_major: u32,
        pub(super) abi_minor: u32,
        pub(super) describe: DescribeFn,
        pub(super) create: CreateFn,
        pub(super) destroy: DestroyFn,
        pub(super) start_event_loop: StartEventLoopFn,
        pub(super) poll_event_loop: PollEventLoopFn,
        pub(super) request_shutdown: RequestShutdownFn,
        pub(super) create_surface: CreateSurfaceFn,
        pub(super) destroy_surface: DestroySurfaceFn,
        pub(super) apply_ui_batch: ApplyUiBatchFn,
        pub(super) set_surface_visible: SetSurfaceVisibleFn,
        pub(super) capture_semantic_state: CaptureSemanticStateFn,
        pub(super) capture_opaque_state: CaptureOpaqueStateFn,
        pub(super) apply_model_rows: ApplyModelRowsFn,
        pub(super) cancel_request: CancelRequestFn,
    }

    const _: () = {
        assert!(std::mem::size_of::<StringView>() == 16);
        assert!(std::mem::align_of::<StringView>() == 8);
        assert!(std::mem::offset_of!(StringView, data) == 0);
        assert!(std::mem::offset_of!(StringView, len) == 8);
        assert!(std::mem::size_of::<BytesView>() == 16);
        assert!(std::mem::align_of::<BytesView>() == 8);
        assert!(std::mem::offset_of!(BytesView, data) == 0);
        assert!(std::mem::offset_of!(BytesView, len) == 8);
        assert!(std::mem::size_of::<OwnedBytes>() == 32);
        assert!(std::mem::align_of::<OwnedBytes>() == 8);
        assert!(std::mem::offset_of!(OwnedBytes, data) == 0);
        assert!(std::mem::offset_of!(OwnedBytes, len) == 8);
        assert!(std::mem::offset_of!(OwnedBytes, owner) == 16);
        assert!(std::mem::offset_of!(OwnedBytes, release) == 24);
        assert!(std::mem::size_of::<StatusCode>() == 4);
        assert!(std::mem::align_of::<StatusCode>() == 4);
        assert!(StatusCode::Ok.0 == 0);
        assert!(StatusCode::InvalidArgument.0 == 1);
        assert!(StatusCode::Unsupported.0 == 2);
        assert!(StatusCode::NotFound.0 == 3);
        assert!(StatusCode::Busy.0 == 4);
        assert!(StatusCode::Cancelled.0 == 5);
        assert!(StatusCode::Failed.0 == 6);
        assert!(StatusCode::Internal.0 == 7);
        assert!(StatusCode::StaleRevision.0 == 8);
        assert!(std::mem::size_of::<Status>() == 24);
        assert!(std::mem::align_of::<Status>() == 8);
        assert!(std::mem::offset_of!(Status, code) == 0);
        assert!(std::mem::offset_of!(Status, message) == 8);
        assert!(std::mem::size_of::<ThreadModel>() == 4);
        assert!(std::mem::align_of::<ThreadModel>() == 4);
        assert!(ThreadModel::ClientEventLoop.0 == 1);
        assert!(ThreadModel::RuntimeEventLoop.0 == 2);
        assert!(ThreadModel::CallerPumps.0 == 3);
        assert!(std::mem::size_of::<ContractVersion>() == 40);
        assert!(std::mem::align_of::<ContractVersion>() == 8);
        assert!(std::mem::offset_of!(ContractVersion, name) == 0);
        assert!(std::mem::offset_of!(ContractVersion, major) == 16);
        assert!(std::mem::offset_of!(ContractVersion, minor) == 20);
        assert!(std::mem::offset_of!(ContractVersion, features) == 24);
        assert!(std::mem::offset_of!(ContractVersion, feature_count) == 32);
        assert!(std::mem::size_of::<SinkOffer>() == 40);
        assert!(std::mem::align_of::<SinkOffer>() == 8);
        assert!(std::mem::offset_of!(SinkOffer, type_name) == 0);
        assert!(std::mem::offset_of!(SinkOffer, media_types) == 16);
        assert!(std::mem::offset_of!(SinkOffer, media_type_count) == 24);
        assert!(std::mem::offset_of!(SinkOffer, supports_streaming) == 32);
        assert!(std::mem::offset_of!(SinkOffer, preference_rank) == 36);
        assert!(std::mem::size_of::<Descriptor>() == 120);
        assert!(std::mem::align_of::<Descriptor>() == 8);
        assert!(std::mem::offset_of!(Descriptor, abi_major) == 0);
        assert!(std::mem::offset_of!(Descriptor, abi_minor) == 4);
        assert!(std::mem::offset_of!(Descriptor, runtime_name) == 8);
        assert!(std::mem::offset_of!(Descriptor, runtime_version) == 24);
        assert!(std::mem::offset_of!(Descriptor, build_id) == 40);
        assert!(std::mem::offset_of!(Descriptor, platform) == 56);
        assert!(std::mem::offset_of!(Descriptor, thread_model) == 72);
        assert!(std::mem::offset_of!(Descriptor, features) == 80);
        assert!(std::mem::offset_of!(Descriptor, sinks) == 88);
        assert!(std::mem::offset_of!(Descriptor, sink_count) == 96);
        assert!(std::mem::offset_of!(Descriptor, contracts) == 104);
        assert!(std::mem::offset_of!(Descriptor, contract_count) == 112);
        assert!(std::mem::size_of::<ValueRef>() == 40);
        assert!(std::mem::align_of::<ValueRef>() == 8);
        assert!(std::mem::offset_of!(ValueRef, handle) == 0);
        assert!(std::mem::offset_of!(ValueRef, type_name) == 8);
        assert!(std::mem::offset_of!(ValueRef, canonical_encoding) == 24);
        assert!(std::mem::size_of::<UiOperationKind>() == 4);
        assert!(std::mem::align_of::<UiOperationKind>() == 4);
        assert!(UiOperationKind::MountNode.0 == 1);
        assert!(UiOperationKind::UnmountNode.0 == 2);
        assert!(UiOperationKind::SetProperty.0 == 3);
        assert!(UiOperationKind::ClearProperty.0 == 4);
        assert!(UiOperationKind::InsertChild.0 == 5);
        assert!(UiOperationKind::RemoveChild.0 == 6);
        assert!(UiOperationKind::MoveChild.0 == 7);
        assert!(UiOperationKind::BindAction.0 == 8);
        assert!(UiOperationKind::UnbindAction.0 == 9);
        assert!(UiOperationKind::SetFocus.0 == 10);
        assert!(UiOperationKind::SetAccessibility.0 == 11);
        assert!(std::mem::size_of::<MountNode>() == 104);
        assert!(std::mem::align_of::<MountNode>() == 8);
        assert!(std::mem::offset_of!(MountNode, node) == 0);
        assert!(std::mem::offset_of!(MountNode, parent) == 8);
        assert!(std::mem::offset_of!(MountNode, slot) == 16);
        assert!(std::mem::offset_of!(MountNode, ordinal) == 32);
        assert!(std::mem::offset_of!(MountNode, contract_name) == 40);
        assert!(std::mem::offset_of!(MountNode, contract_major) == 56);
        assert!(std::mem::offset_of!(MountNode, contract_minor) == 60);
        assert!(std::mem::offset_of!(MountNode, explicit_key) == 64);
        assert!(std::mem::size_of::<SetProperty>() == 64);
        assert!(std::mem::align_of::<SetProperty>() == 8);
        assert!(std::mem::offset_of!(SetProperty, node) == 0);
        assert!(std::mem::offset_of!(SetProperty, property) == 8);
        assert!(std::mem::offset_of!(SetProperty, value) == 24);
        assert!(std::mem::size_of::<ChildOperation>() == 40);
        assert!(std::mem::align_of::<ChildOperation>() == 8);
        assert!(std::mem::offset_of!(ChildOperation, parent) == 0);
        assert!(std::mem::offset_of!(ChildOperation, slot) == 8);
        assert!(std::mem::offset_of!(ChildOperation, child) == 24);
        assert!(std::mem::offset_of!(ChildOperation, ordinal) == 32);
        assert!(std::mem::size_of::<BindAction>() == 48);
        assert!(std::mem::align_of::<BindAction>() == 8);
        assert!(std::mem::offset_of!(BindAction, node) == 0);
        assert!(std::mem::offset_of!(BindAction, event_name) == 8);
        assert!(std::mem::offset_of!(BindAction, action) == 24);
        assert!(std::mem::offset_of!(BindAction, input_type) == 32);
        assert!(std::mem::size_of::<UiOperationArgs>() == 104);
        assert!(std::mem::align_of::<UiOperationArgs>() == 8);
        assert!(std::mem::size_of::<UiOperation>() == 112);
        assert!(std::mem::align_of::<UiOperation>() == 8);
        assert!(std::mem::offset_of!(UiOperation, kind) == 0);
        assert!(std::mem::offset_of!(UiOperation, as_) == 8);
        assert!(std::mem::size_of::<UiBatch>() == 24);
        assert!(std::mem::align_of::<UiBatch>() == 8);
        assert!(std::mem::offset_of!(UiBatch, semantic_revision) == 0);
        assert!(std::mem::offset_of!(UiBatch, operations) == 8);
        assert!(std::mem::offset_of!(UiBatch, operation_count) == 16);
        assert!(std::mem::size_of::<EventKind>() == 4);
        assert!(std::mem::align_of::<EventKind>() == 4);
        assert!(EventKind::Action.0 == 1);
        assert!(EventKind::FocusChanged.0 == 2);
        assert!(EventKind::LayoutStateChanged.0 == 3);
        assert!(EventKind::SurfaceClosed.0 == 4);
        assert!(EventKind::ModelRangeRequest.0 == 5);
        assert!(EventKind::ModelChildrenRequest.0 == 6);
        assert!(EventKind::Diagnostic.0 == 7);
        assert!(std::mem::size_of::<ActionEvent>() == 64);
        assert!(std::mem::align_of::<ActionEvent>() == 8);
        assert!(std::mem::offset_of!(ActionEvent, surface) == 0);
        assert!(std::mem::offset_of!(ActionEvent, node) == 8);
        assert!(std::mem::offset_of!(ActionEvent, action) == 16);
        assert!(std::mem::offset_of!(ActionEvent, payload) == 24);
        assert!(std::mem::size_of::<LayoutStateEvent>() == 88);
        assert!(std::mem::align_of::<LayoutStateEvent>() == 8);
        assert!(std::mem::offset_of!(LayoutStateEvent, surface) == 0);
        assert!(std::mem::offset_of!(LayoutStateEvent, node) == 8);
        assert!(std::mem::offset_of!(LayoutStateEvent, semantic_state_name) == 16);
        assert!(std::mem::offset_of!(LayoutStateEvent, semantic_state) == 32);
        assert!(std::mem::offset_of!(LayoutStateEvent, opaque_runtime_state) == 72);
        assert!(std::mem::size_of::<ModelRangeRequest>() == 48);
        assert!(std::mem::align_of::<ModelRangeRequest>() == 8);
        assert!(std::mem::offset_of!(ModelRangeRequest, request) == 0);
        assert!(std::mem::offset_of!(ModelRangeRequest, model) == 8);
        assert!(std::mem::offset_of!(ModelRangeRequest, start) == 16);
        assert!(std::mem::offset_of!(ModelRangeRequest, count) == 24);
        assert!(std::mem::offset_of!(ModelRangeRequest, sort_filter_token) == 32);
        assert!(std::mem::size_of::<ModelChildrenRequest>() == 56);
        assert!(std::mem::align_of::<ModelChildrenRequest>() == 8);
        assert!(std::mem::offset_of!(ModelChildrenRequest, request) == 0);
        assert!(std::mem::offset_of!(ModelChildrenRequest, model) == 8);
        assert!(std::mem::offset_of!(ModelChildrenRequest, parent_key) == 16);
        assert!(std::mem::size_of::<RuntimeEventArgs>() == 88);
        assert!(std::mem::align_of::<RuntimeEventArgs>() == 8);
        assert!(std::mem::size_of::<RuntimeEvent>() == 96);
        assert!(std::mem::align_of::<RuntimeEvent>() == 8);
        assert!(std::mem::offset_of!(RuntimeEvent, kind) == 0);
        assert!(std::mem::offset_of!(RuntimeEvent, as_) == 8);
        assert!(std::mem::size_of::<ClientApi>() == 72);
        assert!(std::mem::align_of::<ClientApi>() == 8);
        assert!(std::mem::offset_of!(ClientApi, abi_major) == 0);
        assert!(std::mem::offset_of!(ClientApi, abi_minor) == 4);
        assert!(std::mem::offset_of!(ClientApi, context) == 8);
        assert!(std::mem::offset_of!(ClientApi, log) == 16);
        assert!(std::mem::offset_of!(ClientApi, emit_runtime_event) == 24);
        assert!(std::mem::offset_of!(ClientApi, complete_model_request) == 32);
        assert!(std::mem::offset_of!(ClientApi, fail_model_request) == 40);
        assert!(std::mem::offset_of!(ClientApi, read_action_metadata) == 48);
        assert!(std::mem::offset_of!(ClientApi, read_value_debug_json) == 56);
        assert!(std::mem::offset_of!(ClientApi, monotonic_time_ns) == 64);
        assert!(std::mem::size_of::<RuntimeCreateOptions>() == 88);
        assert!(std::mem::align_of::<RuntimeCreateOptions>() == 8);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, client) == 0);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, locale) == 8);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, timezone) == 24);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, theme) == 40);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, accessibility_preferences_json) == 56);
        assert!(std::mem::offset_of!(RuntimeCreateOptions, runtime_configuration_json) == 72);
        assert!(std::mem::size_of::<SurfaceCreateOptions>() == 64);
        assert!(std::mem::align_of::<SurfaceCreateOptions>() == 8);
        assert!(std::mem::offset_of!(SurfaceCreateOptions, surface_kind) == 0);
        assert!(std::mem::offset_of!(SurfaceCreateOptions, title) == 16);
        assert!(std::mem::offset_of!(SurfaceCreateOptions, state_profile) == 32);
        assert!(std::mem::offset_of!(SurfaceCreateOptions, opaque_runtime_restore_state) == 48);
        assert!(std::mem::size_of::<RuntimeApi>() == 120);
        assert!(std::mem::align_of::<RuntimeApi>() == 8);
        assert!(std::mem::offset_of!(RuntimeApi, abi_major) == 0);
        assert!(std::mem::offset_of!(RuntimeApi, abi_minor) == 4);
        assert!(std::mem::offset_of!(RuntimeApi, describe) == 8);
        assert!(std::mem::offset_of!(RuntimeApi, create) == 16);
        assert!(std::mem::offset_of!(RuntimeApi, destroy) == 24);
        assert!(std::mem::offset_of!(RuntimeApi, start_event_loop) == 32);
        assert!(std::mem::offset_of!(RuntimeApi, poll_event_loop) == 40);
        assert!(std::mem::offset_of!(RuntimeApi, request_shutdown) == 48);
        assert!(std::mem::offset_of!(RuntimeApi, create_surface) == 56);
        assert!(std::mem::offset_of!(RuntimeApi, destroy_surface) == 64);
        assert!(std::mem::offset_of!(RuntimeApi, apply_ui_batch) == 72);
        assert!(std::mem::offset_of!(RuntimeApi, set_surface_visible) == 80);
        assert!(std::mem::offset_of!(RuntimeApi, capture_semantic_state) == 88);
        assert!(std::mem::offset_of!(RuntimeApi, capture_opaque_state) == 96);
        assert!(std::mem::offset_of!(RuntimeApi, apply_model_rows) == 104);
        assert!(std::mem::offset_of!(RuntimeApi, cancel_request) == 112);
    };
}

#[cfg(test)]
mod runtime_conformance {
    use super::runtime_abi::*;
    use std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        ffi::{c_char, c_void},
        ptr, slice,
        sync::{
            Arc, Condvar, LazyLock, Mutex, MutexGuard, TryLockError,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
        thread::{self, ThreadId},
    };

    unsafe impl Sync for StringView {}
    unsafe impl Sync for ContractVersion {}
    unsafe impl Sync for SinkOffer {}
    unsafe impl Sync for Descriptor {}

    const ABI_MAJOR: u32 = 1;
    const ABI_MINOR: u32 = 0;
    const RUNTIME_NAME: &str = "orna-runtime-headless-conformance";
    const RUNTIME_VERSION: &str = "1.0.0";
    const PLATFORM: &str = "linux-x86_64";
    const SINK_NAME: &str = "std.ui.UI";
    static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
    static HANDLE_RESERVATIONS: LazyLock<Mutex<HashSet<Handle>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    const MAX_VIEW_BYTES: usize = 16 * 1024 * 1024;
    const MAX_BATCH_OPERATIONS: usize = 1024;

    const fn view(bytes: &'static [u8]) -> StringView {
        StringView {
            data: bytes.as_ptr() as *const c_char,
            len: bytes.len(),
        }
    }

    fn status_message(code: StatusCode) -> &'static [u8] {
        match code {
            StatusCode::Ok => b"ORNA-S000 ok",
            StatusCode::InvalidArgument => b"ORNA-E100 invalid argument",
            StatusCode::Unsupported => b"ORNA-E101 unsupported",
            StatusCode::NotFound => b"ORNA-E102 not found",
            StatusCode::Busy => b"ORNA-E103 busy",
            StatusCode::Cancelled => b"ORNA-E104 cancelled",
            StatusCode::Failed => b"ORNA-E105 failed",
            StatusCode::Internal => b"ORNA-E106 internal",
            StatusCode::StaleRevision => b"ORNA-E107 stale revision",
            _ => b"ORNA-E199 unknown status",
        }
    }

    fn status(code: StatusCode, _detail: &'static [u8]) -> Status {
        Status {
            code,
            message: view(status_message(code)),
        }
    }
    fn ok() -> Status {
        status(StatusCode::Ok, b"ok")
    }

    fn next_unreserved_handle() -> Handle {
        let mut reservations = HANDLE_RESERVATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
            assert_ne!(handle, 0, "handle allocation exhausted");
            if reservations.insert(handle) {
                return handle;
            }
        }
    }

    fn next_unreserved_alias_handle() -> Handle {
        loop {
            let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
            assert_ne!(handle, 0, "handle allocation exhausted");
            if !is_reserved_handle(handle) {
                return handle;
            }
        }
    }

    fn reserve_alias(handle: Handle) -> bool {
        HANDLE_RESERVATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(handle)
    }
    fn is_reserved_handle(handle: Handle) -> bool {
        HANDLE_RESERVATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(&handle)
    }

    unsafe fn text(view: StringView) -> Option<&'static str> {
        if view.len == 0 {
            return Some("");
        }
        if view.len > MAX_VIEW_BYTES {
            return None;
        }
        if view.data.is_null() {
            return None;
        }
        let bytes = unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len) };

        std::str::from_utf8(bytes).ok()
    }

    unsafe fn owned_text(view: StringView) -> Option<String> {
        unsafe { text(view) }.map(ToOwned::to_owned)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LoadError {
        AbiMajor(u32),
        AbiMinor(u32),
        NullDescriptor,
        Descriptor(&'static str),
    }

    fn valid_client_api(client: &ClientApi) -> bool {
        client.log as usize != 0
            && client.emit_runtime_event as usize != 0
            && client.complete_model_request as usize != 0
            && client.fail_model_request as usize != 0
            && client.read_action_metadata as usize != 0
            && client.read_value_debug_json as usize != 0
            && client.monotonic_time_ns as usize != 0
    }

    fn validate_api(api: &RuntimeApi) -> Result<(), LoadError> {
        if api.abi_major != ABI_MAJOR {
            return Err(LoadError::AbiMajor(api.abi_major));
        }
        if api.abi_minor > ABI_MINOR {
            return Err(LoadError::AbiMinor(api.abi_minor));
        }
        let required_functions = [
            api.describe as usize,
            api.create as usize,
            api.destroy as usize,
            api.start_event_loop as usize,
            api.poll_event_loop as usize,
            api.request_shutdown as usize,
            api.create_surface as usize,
            api.destroy_surface as usize,
            api.apply_ui_batch as usize,
            api.set_surface_visible as usize,
            api.capture_semantic_state as usize,
            api.capture_opaque_state as usize,
            api.apply_model_rows as usize,
            api.cancel_request as usize,
        ];
        if required_functions.into_iter().any(|function| function == 0) {
            return Err(LoadError::Descriptor("runtime API function"));
        }
        let descriptor = unsafe { (api.describe)() };
        if descriptor.is_null() {
            return Err(LoadError::NullDescriptor);
        }
        validate_descriptor(unsafe { &*descriptor })
    }

    fn validate_descriptor(descriptor: &Descriptor) -> Result<(), LoadError> {
        if descriptor.abi_major != ABI_MAJOR {
            return Err(LoadError::Descriptor("descriptor ABI major"));
        }
        if descriptor.abi_minor > ABI_MINOR {
            return Err(LoadError::Descriptor("descriptor ABI minor"));
        }
        let required_strings = [
            (descriptor.runtime_name, RUNTIME_NAME),
            (descriptor.runtime_version, RUNTIME_VERSION),
            (descriptor.build_id, "test-fixture"),
            (descriptor.platform, PLATFORM),
        ];
        for (value, expected) in required_strings {
            if unsafe { text(value) } != Some(expected) {
                return Err(LoadError::Descriptor("runtime identity"));
            }
        }
        if descriptor.thread_model != ThreadModel::CallerPumps {
            return Err(LoadError::Descriptor("thread model"));
        }
        if descriptor.features & !0x7f != 0 {
            return Err(LoadError::Descriptor("unknown feature"));
        }
        if descriptor.sink_count != 1 || descriptor.sinks.is_null() {
            return Err(LoadError::Descriptor("sink count"));
        }
        let sinks = unsafe { slice::from_raw_parts(descriptor.sinks, descriptor.sink_count) };
        let sink = sinks[0];
        if unsafe { text(sink.type_name) } != Some(SINK_NAME)
            || sink.supports_streaming > 1
            || sink.media_type_count > 16
            || (sink.media_type_count == 0) != sink.media_types.is_null()
        {
            return Err(LoadError::Descriptor("sink offer"));
        }
        if sink.media_type_count != 0 {
            let media_types =
                unsafe { slice::from_raw_parts(sink.media_types, sink.media_type_count) };
            let mut media_names = HashSet::new();
            for media_type in media_types {
                let Some(media_name) = (unsafe { text(*media_type) }) else {
                    return Err(LoadError::Descriptor("sink offer"));
                };
                if media_name.is_empty() || !media_names.insert(media_name.to_owned()) {
                    return Err(LoadError::Descriptor("sink offer"));
                }
            }
        }
        if descriptor.contract_count != 1 || descriptor.contracts.is_null() {
            return Err(LoadError::Descriptor("contract count"));
        }
        let contract = unsafe { &*descriptor.contracts };
        let Some(name) = (unsafe { text(contract.name) }) else {
            return Err(LoadError::Descriptor("contract name"));
        };
        if name != SINK_NAME {
            return Err(LoadError::Descriptor("contract name"));
        }
        if contract.major != 1 || contract.minor != 0 {
            return Err(LoadError::Descriptor("contract version"));
        }
        if contract.feature_count > 16
            || (contract.feature_count == 0) != contract.features.is_null()
        {
            return Err(LoadError::Descriptor("contract features"));
        }
        if contract.feature_count != 0 {
            let features =
                unsafe { slice::from_raw_parts(contract.features, contract.feature_count) };
            let mut feature_names = HashSet::new();
            for feature in features {
                let Some(feature_name) = (unsafe { text(*feature) }) else {
                    return Err(LoadError::Descriptor("contract features"));
                };
                if feature_name.is_empty() || !feature_names.insert(feature_name.to_owned()) {
                    return Err(LoadError::Descriptor("contract features"));
                }
            }
        }
        Ok(())
    }

    static CONTRACT: ContractVersion = ContractVersion {
        name: view(b"std.ui.UI"),
        major: 1,
        minor: 0,
        features: ptr::null(),
        feature_count: 0,
    };

    static SINK: SinkOffer = SinkOffer {
        type_name: view(b"std.ui.UI"),
        media_types: ptr::null(),
        media_type_count: 0,
        supports_streaming: 0,
        preference_rank: 0,
    };

    static DESCRIPTOR: Descriptor = Descriptor {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        runtime_name: view(b"orna-runtime-headless-conformance"),
        runtime_version: view(b"1.0.0"),
        build_id: view(b"test-fixture"),
        platform: view(b"linux-x86_64"),
        thread_model: ThreadModel::CallerPumps,
        features: 0,
        sinks: &SINK,
        sink_count: 1,
        contracts: &CONTRACT,
        contract_count: 1,
    };

    static DESCRIBE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn fixture_describe() -> *const Descriptor {
        DESCRIBE_CALLS.fetch_add(1, Ordering::SeqCst);
        &DESCRIPTOR
    }

    #[derive(Default)]
    struct ReleaseCounters {
        releases: AtomicUsize,
        invalid: AtomicUsize,
    }

    struct OwnedAllocation {
        bytes: Vec<u8>,
        counters: Arc<ReleaseCounters>,
    }

    struct AllocationRecord {
        data: usize,
        len: usize,
        counters: Arc<ReleaseCounters>,
        _allocation: Box<OwnedAllocation>,
    }

    static NEXT_ALLOCATION_OWNER: AtomicUsize = AtomicUsize::new(1);
    static ALLOCATIONS: LazyLock<Mutex<HashMap<usize, AllocationRecord>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    static UNKNOWN_RELEASES: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn release_owned(owner: *mut c_void, data: *mut u8, len: usize) {
        let key = owner as usize;
        let mut allocations = ALLOCATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(record) = allocations.get(&key) else {
            UNKNOWN_RELEASES.fetch_add(1, Ordering::SeqCst);
            return;
        };

        if record.len != len || record.data != data as usize {
            record.counters.invalid.fetch_add(1, Ordering::SeqCst);
            return;
        }

        let record = allocations
            .remove(&key)
            .expect("allocation record should exist");
        record.counters.releases.fetch_add(1, Ordering::SeqCst);
        drop(record);
    }

    fn owned_bytes(bytes: Vec<u8>, counters: Arc<ReleaseCounters>) -> OwnedBytes {
        let mut allocation = Box::new(OwnedAllocation { bytes, counters });
        let data = if allocation.bytes.is_empty() {
            ptr::null_mut()
        } else {
            allocation.bytes.as_mut_ptr()
        };
        let len = allocation.bytes.len();
        let owner = NEXT_ALLOCATION_OWNER.fetch_add(1, Ordering::SeqCst);
        assert_ne!(owner, 0, "owned allocation owner exhausted");
        let record = AllocationRecord {
            data: data as usize,
            len,
            counters: Arc::clone(&allocation.counters),
            _allocation: allocation,
        };
        ALLOCATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(owner, record);
        OwnedBytes {
            data,
            len,
            owner: owner as *mut c_void,
            release: release_owned,
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct EventRecord {
        kind: EventKind,
        surface: SurfaceHandle,
        request: RequestHandle,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum CallbackKind {
        Event(EventRecord),
        Completion(RequestHandle),
        Failure(RequestHandle, StatusCode),
        Terminal,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct CallbackRecord {
        sequence: u64,
        terminal: bool,
        kind: CallbackKind,
    }

    #[derive(Default)]
    struct CallbackLog {
        events: Vec<EventRecord>,
        action_payloads: Vec<Vec<u8>>,
        completions: Vec<RequestHandle>,
        failures: Vec<(RequestHandle, StatusCode)>,
        sequence: Vec<CallbackRecord>,
        next_sequence: u64,
        terminal: bool,
        reenter: bool,
        reentry_status: Option<StatusCode>,
    }

    impl CallbackLog {
        fn record(&mut self, kind: CallbackKind) {
            let sequence = self.next_sequence;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .expect("callback sequence exhausted");
            self.sequence.push(CallbackRecord {
                sequence,
                terminal: self.terminal,
                kind,
            });
        }

        fn mark_terminal(&mut self) {
            self.terminal = true;
            self.record(CallbackKind::Terminal);
        }
    }

    #[derive(Default)]
    struct HandleRegistry {
        known_surfaces: HashSet<SurfaceHandle>,
        live_surfaces: HashSet<SurfaceHandle>,
        known_nodes: HashSet<NodeHandle>,
        live_nodes: HashSet<NodeHandle>,
        known_actions: HashSet<ActionHandle>,
        live_actions: HashSet<ActionHandle>,
        known_models: HashSet<ModelHandle>,
        live_models: HashSet<ModelHandle>,
        known_requests: HashSet<RequestHandle>,
        live_requests: HashSet<RequestHandle>,
        node_surfaces: HashMap<NodeHandle, SurfaceHandle>,
        action_surfaces: HashMap<ActionHandle, SurfaceHandle>,
        action_input_types: HashMap<ActionHandle, String>,
        model_surfaces: HashMap<ModelHandle, SurfaceHandle>,
        request_surfaces: HashMap<RequestHandle, SurfaceHandle>,
        request_models: HashMap<RequestHandle, ModelHandle>,
        terminal_requests: HashSet<RequestHandle>,
    }

    impl HandleRegistry {
        fn register_surface(&mut self, surface: SurfaceHandle) {
            self.known_surfaces.insert(surface);
            self.live_surfaces.insert(surface);
        }

        fn register_node(&mut self, node: NodeHandle, surface: SurfaceHandle) {
            self.known_nodes.insert(node);
            self.live_nodes.insert(node);
            self.node_surfaces.insert(node, surface);
        }

        fn register_action(
            &mut self,
            action: ActionHandle,
            surface: SurfaceHandle,
            input_type: String,
        ) {
            self.known_actions.insert(action);
            self.live_actions.insert(action);
            self.action_surfaces.insert(action, surface);
            self.action_input_types.insert(action, input_type);
        }

        fn register_model(&mut self, model: ModelHandle, surface: SurfaceHandle) {
            self.known_models.insert(model);
            self.live_models.insert(model);
            self.model_surfaces.insert(model, surface);
        }

        fn register_request(
            &mut self,
            request: RequestHandle,
            model: ModelHandle,
            surface: SurfaceHandle,
        ) {
            self.known_requests.insert(request);
            self.live_requests.insert(request);
            self.request_surfaces.insert(request, surface);
            self.request_models.insert(request, model);
        }

        fn retire_surface(&mut self, surface: SurfaceHandle) {
            self.live_surfaces.remove(&surface);
            let nodes = self
                .node_surfaces
                .iter()
                .filter_map(|(node, owner)| (*owner == surface).then_some(*node))
                .collect::<Vec<_>>();
            for node in nodes {
                self.retire_node(node);
            }
            let actions = self
                .action_surfaces
                .iter()
                .filter_map(|(action, owner)| (*owner == surface).then_some(*action))
                .collect::<Vec<_>>();
            for action in actions {
                self.retire_action(action);
            }
            let models = self
                .model_surfaces
                .iter()
                .filter_map(|(model, owner)| (*owner == surface).then_some(*model))
                .collect::<Vec<_>>();
            for model in models {
                self.retire_model(model);
            }
            let requests = self
                .request_surfaces
                .iter()
                .filter_map(|(request, owner)| (*owner == surface).then_some(*request))
                .collect::<Vec<_>>();
            for request in requests {
                self.retire_request(request);
            }
        }

        fn retire_node(&mut self, node: NodeHandle) {
            self.live_nodes.remove(&node);
            self.node_surfaces.remove(&node);
        }

        fn retire_action(&mut self, action: ActionHandle) {
            self.live_actions.remove(&action);
            self.action_surfaces.remove(&action);
            self.action_input_types.remove(&action);
        }

        fn retire_model(&mut self, model: ModelHandle) {
            self.live_models.remove(&model);
            self.model_surfaces.remove(&model);
        }

        fn retire_request(&mut self, request: RequestHandle) {
            self.live_requests.remove(&request);
            self.request_surfaces.remove(&request);
            self.request_models.remove(&request);
        }

        fn check_live(
            handle: Handle,
            known: &HashSet<Handle>,
            live: &HashSet<Handle>,
            kind: &'static [u8],
        ) -> Result<(), Status> {
            if handle == 0 || !known.contains(&handle) {
                return Err(status(StatusCode::InvalidArgument, kind));
            }
            if !live.contains(&handle) {
                return Err(status(StatusCode::NotFound, kind));
            }
            Ok(())
        }

        fn check_surface(&self, surface: SurfaceHandle) -> Result<(), Status> {
            Self::check_live(
                surface,
                &self.known_surfaces,
                &self.live_surfaces,
                b"surface handle is not live",
            )
        }

        fn check_node(&self, node: NodeHandle) -> Result<(), Status> {
            Self::check_live(
                node,
                &self.known_nodes,
                &self.live_nodes,
                b"node handle is not live",
            )
        }

        fn check_action(&self, action: ActionHandle) -> Result<(), Status> {
            Self::check_live(
                action,
                &self.known_actions,
                &self.live_actions,
                b"action handle is not live",
            )
        }

        fn check_model(&self, model: ModelHandle) -> Result<(), Status> {
            Self::check_live(
                model,
                &self.known_models,
                &self.live_models,
                b"model handle is not live",
            )
        }

        fn check_request(&self, request: RequestHandle) -> Result<(), Status> {
            Self::check_live(
                request,
                &self.known_requests,
                &self.live_requests,
                b"request handle is not live",
            )
        }
        fn claim_request_callback(&mut self, request: RequestHandle) -> Result<(), Status> {
            self.check_request(request)?;
            if !self.terminal_requests.insert(request) {
                return Err(status(
                    StatusCode::NotFound,
                    b"request callback already completed",
                ));
            }
            Ok(())
        }

        fn check_node_on_surface(
            &self,
            node: NodeHandle,
            surface: SurfaceHandle,
        ) -> Result<(), Status> {
            self.check_node(node)?;
            if self.node_surfaces.get(&node) != Some(&surface) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"node belongs to another surface",
                ));
            }
            Ok(())
        }

        fn check_action_on_surface(
            &self,
            action: ActionHandle,
            surface: SurfaceHandle,
        ) -> Result<(), Status> {
            self.check_action(action)?;
            if self.action_surfaces.get(&action) != Some(&surface) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"action belongs to another surface",
                ));
            }
            Ok(())
        }

        fn check_action_payload_type(
            &self,
            action: ActionHandle,
            payload: ValueRef,
        ) -> Result<(), Status> {
            let Some(actual) = (unsafe { text(payload.type_name) }) else {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid action payload",
                ));
            };
            if self.action_input_types.get(&action).map(String::as_str) != Some(actual) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"action payload type mismatch",
                ));
            }
            Ok(())
        }

        fn check_request_model(
            &self,
            request: RequestHandle,
            model: ModelHandle,
        ) -> Result<(), Status> {
            self.check_request(request)?;
            self.check_model(model)?;
            if self.request_models.get(&request) != Some(&model) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"request belongs to another model",
                ));
            }
            Ok(())
        }
    }

    struct ContextEntry {
        pointer: usize,
        active: bool,
        in_flight: usize,
    }

    #[derive(Default)]
    struct ContextRegistry {
        entries: HashMap<usize, ContextEntry>,
    }

    static CONTEXT_REGISTRY: LazyLock<(Mutex<ContextRegistry>, Condvar)> =
        LazyLock::new(|| (Mutex::new(ContextRegistry::default()), Condvar::new()));

    fn register_context(context: *mut c_void) {
        let mut registry = CONTEXT_REGISTRY
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let key = context as usize;
        assert!(
            registry
                .entries
                .insert(
                    key,
                    ContextEntry {
                        pointer: key,
                        active: true,
                        in_flight: 0,
                    },
                )
                .is_none(),
            "client context is already registered"
        );
    }

    fn unregister_context(context: *mut c_void) {
        let key = context as usize;
        let mut registry = CONTEXT_REGISTRY
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = registry.entries.get_mut(&key) {
            entry.active = false;
        } else {
            return;
        }
        loop {
            let in_flight = registry
                .entries
                .get(&key)
                .map_or(0, |entry| entry.in_flight);
            if in_flight == 0 {
                break;
            }
            registry = CONTEXT_REGISTRY
                .1
                .wait(registry)
                .unwrap_or_else(|error| error.into_inner());
        }
        registry.entries.remove(&key);
    }

    struct ContextCallGuard {
        key: usize,
        pointer: usize,
    }

    impl ContextCallGuard {
        fn acquire(context: *mut c_void) -> Option<Self> {
            let key = context as usize;
            let mut registry = CONTEXT_REGISTRY
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let entry = registry.entries.get_mut(&key)?;
            if !entry.active {
                return None;
            }
            entry.in_flight += 1;
            Some(Self {
                key,
                pointer: entry.pointer,
            })
        }

        fn context(&self) -> &ClientContext {
            unsafe { &*(self.pointer as *const ClientContext) }
        }
    }

    impl Drop for ContextCallGuard {
        fn drop(&mut self) {
            let mut registry = CONTEXT_REGISTRY
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = registry.entries.get_mut(&self.key) {
                entry.in_flight -= 1;
                if entry.in_flight == 0 {
                    CONTEXT_REGISTRY.1.notify_all();
                }
            }
        }
    }

    fn with_registered_context<T>(
        context: *mut c_void,
        operation: impl FnOnce(&ClientContext) -> T,
    ) -> Option<T> {
        let guard = ContextCallGuard::acquire(context)?;
        Some(operation(guard.context()))
    }

    struct ClientContext {
        log: Mutex<CallbackLog>,
        counters: Arc<ReleaseCounters>,
        runtime: AtomicU64,
        fail_model_callback: AtomicBool,
        handles: Mutex<HandleRegistry>,
    }

    impl ClientContext {
        fn new() -> Self {
            Self {
                log: Mutex::new(CallbackLog::default()),
                counters: Arc::new(ReleaseCounters::default()),
                runtime: AtomicU64::new(0),
                fail_model_callback: AtomicBool::new(false),
                handles: Mutex::new(HandleRegistry::default()),
            }
        }
    }

    unsafe extern "C" fn client_log(
        _context: *mut c_void,
        _level: u32,
        _subsystem: StringView,
        _message: StringView,
    ) {
    }

    fn validate_callback_event(
        context: &ClientContext,
        event: &RuntimeEvent,
    ) -> Result<(SurfaceHandle, RequestHandle), Status> {
        let handles = context
            .handles
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match event.kind {
            EventKind::Action => {
                let value = unsafe { event.as_.action };
                handles.check_node_on_surface(value.node, value.surface)?;
                handles.check_action_on_surface(value.action, value.surface)?;
                if !RuntimeState::valid_value_ref(value.payload) {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid action payload",
                    ));
                }
                handles.check_action_payload_type(value.action, value.payload)?;
                Ok((value.surface, 0))
            }
            EventKind::FocusChanged => {
                let value = unsafe { event.as_.action };
                handles.check_node_on_surface(value.node, value.surface)?;
                if value.action != 0 {
                    handles.check_action_on_surface(value.action, value.surface)?;
                    if !RuntimeState::valid_value_ref(value.payload) {
                        return Err(status(
                            StatusCode::InvalidArgument,
                            b"invalid focus payload",
                        ));
                    }
                    handles.check_action_payload_type(value.action, value.payload)?;
                } else if !RuntimeState::valid_value_ref(value.payload) {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid focus payload",
                    ));
                }
                Ok((value.surface, 0))
            }
            EventKind::LayoutStateChanged => {
                let value = unsafe { event.as_.layout_state };
                handles.check_node_on_surface(value.node, value.surface)?;
                let Some(name) = (unsafe { text(value.semantic_state_name) }) else {
                    return Err(status(
                        StatusCode::InvalidArgument,
                        b"invalid layout state name",
                    ));
                };
                if name.is_empty()
                    || !RuntimeState::valid_value_ref(value.semantic_state)
                    || !RuntimeState::valid_bytes_view(value.opaque_runtime_state)
                {
                    return Err(status(StatusCode::InvalidArgument, b"invalid layout state"));
                }
                Ok((value.surface, 0))
            }
            EventKind::SurfaceClosed => {
                let surface = unsafe { event.as_.surface_closed.surface };
                handles.check_surface(surface)?;
                Ok((surface, 0))
            }
            EventKind::ModelRangeRequest => {
                let value = unsafe { event.as_.range_request };
                handles.check_request_model(value.request, value.model)?;
                if unsafe { text(value.sort_filter_token) }.is_none() {
                    return Err(status(StatusCode::InvalidArgument, b"invalid sort filter"));
                }
                Ok((
                    *handles
                        .request_surfaces
                        .get(&value.request)
                        .expect("request ownership should exist"),
                    value.request,
                ))
            }
            EventKind::ModelChildrenRequest => {
                let value = unsafe { event.as_.children_request };
                handles.check_request_model(value.request, value.model)?;
                if !RuntimeState::valid_value_ref(value.parent_key) {
                    return Err(status(StatusCode::InvalidArgument, b"invalid parent key"));
                }
                Ok((
                    *handles
                        .request_surfaces
                        .get(&value.request)
                        .expect("request ownership should exist"),
                    value.request,
                ))
            }
            EventKind::Diagnostic => {
                let diagnostic = unsafe { event.as_.diagnostic };
                if !RuntimeState::valid_status(diagnostic.status) {
                    return Err(status(StatusCode::InvalidArgument, b"invalid diagnostic"));
                }
                Ok((0, 0))
            }
            _ => Err(status(
                StatusCode::InvalidArgument,
                b"unknown runtime event",
            )),
        }
    }

    unsafe extern "C" fn client_emit_runtime_event(
        context: *mut c_void,
        runtime: RuntimeHandle,
        event: *const RuntimeEvent,
    ) -> Status {
        if context.is_null() || event.is_null() {
            return status(
                StatusCode::InvalidArgument,
                b"missing event callback argument",
            );
        }
        let Some(result) = with_registered_context(context, |context| {
            if runtime == 0 || context.runtime.load(Ordering::SeqCst) != runtime {
                return status(StatusCode::InvalidArgument, b"foreign runtime handle");
            }
            let event = unsafe { &*event };
            let (surface, request) = match validate_callback_event(context, event) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let action_payload = match event.kind {
                EventKind::Action | EventKind::FocusChanged => {
                    let value = unsafe { event.as_.action };
                    Some(OwnedValueRef::from_ref(value.payload).canonical_encoding)
                }
                _ => None,
            };
            let reenter = {
                let mut log = context
                    .log
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if log.terminal {
                    return status(StatusCode::Failed, b"runtime is shut down");
                }
                let record = EventRecord {
                    kind: event.kind,
                    surface,
                    request,
                };
                if let Some(payload) = action_payload {
                    log.action_payloads.push(payload);
                }
                log.events.push(record.clone());
                log.record(CallbackKind::Event(record));
                std::mem::take(&mut log.reenter)
            };
            if reenter {
                let result = unsafe { fixture_poll(runtime, 0) };
                let mut log = context
                    .log
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                log.reentry_status = Some(result.code);
            }
            ok()
        }) else {
            return status(
                StatusCode::InvalidArgument,
                b"unregistered callback context",
            );
        };
        result
    }

    unsafe extern "C" fn client_complete_model_request(
        context: *mut c_void,
        request: RequestHandle,
        result: ValueRef,
    ) -> Status {
        if context.is_null() {
            return status(StatusCode::InvalidArgument, b"missing callback context");
        }
        let Some(result) = with_registered_context(context, |context| {
            {
                let log = context
                    .log
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if log.terminal {
                    return status(StatusCode::Failed, b"runtime is shut down");
                }
            }
            {
                let handles = context
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Err(error) = handles.check_request(request) {
                    return error;
                }
            }
            if !RuntimeState::valid_value_ref(result) {
                return status(StatusCode::InvalidArgument, b"invalid model result");
            }
            {
                let mut handles = context
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Err(error) = handles.claim_request_callback(request) {
                    return error;
                }
            }
            let mut log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if log.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            log.completions.push(request);
            log.record(CallbackKind::Completion(request));
            ok()
        }) else {
            return status(
                StatusCode::InvalidArgument,
                b"unregistered callback context",
            );
        };
        result
    }

    unsafe extern "C" fn client_fail_model_request(
        context: *mut c_void,
        request: RequestHandle,
        failure: Status,
    ) -> Status {
        if context.is_null() {
            return status(StatusCode::InvalidArgument, b"missing callback context");
        }
        let Some(result) = with_registered_context(context, |context| {
            {
                let log = context
                    .log
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if log.terminal {
                    return status(StatusCode::Failed, b"runtime is shut down");
                }
            }
            {
                let handles = context
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Err(error) = handles.check_request(request) {
                    return error;
                }
            }
            if !RuntimeState::valid_status(failure) {
                return status(StatusCode::InvalidArgument, b"invalid model failure");
            }
            if context.fail_model_callback.swap(false, Ordering::SeqCst) {
                return status(StatusCode::Failed, b"callback failure");
            }
            {
                let mut handles = context
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Err(error) = handles.claim_request_callback(request) {
                    return error;
                }
            }
            let mut log = context
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if log.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            log.failures.push((request, failure.code));
            log.record(CallbackKind::Failure(request, failure.code));
            ok()
        }) else {
            return status(
                StatusCode::InvalidArgument,
                b"unregistered callback context",
            );
        };
        result
    }

    unsafe extern "C" fn client_read_action_metadata(
        context: *mut c_void,
        action: ActionHandle,
        output: *mut OwnedBytes,
    ) -> Status {
        if context.is_null() || output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing metadata argument");
        }
        let Some(result) = with_registered_context(context, |context| {
            {
                let handles = context
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Err(error) = handles.check_action(action) {
                    return error;
                }
            }
            unsafe {
                *output = owned_bytes(Vec::new(), Arc::clone(&context.counters));
            }
            ok()
        }) else {
            return status(
                StatusCode::InvalidArgument,
                b"unregistered callback context",
            );
        };
        result
    }

    unsafe extern "C" fn client_read_value_debug_json(
        context: *mut c_void,
        value: ValueRef,
        output: *mut OwnedBytes,
    ) -> Status {
        if context.is_null() || output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing value argument");
        }
        let Some(result) = with_registered_context(context, |context| {
            if !RuntimeState::valid_value_ref(value) {
                return status(StatusCode::InvalidArgument, b"invalid value argument");
            }
            unsafe {
                *output = owned_bytes(b"{}".to_vec(), Arc::clone(&context.counters));
            }
            ok()
        }) else {
            return status(
                StatusCode::InvalidArgument,
                b"unregistered callback context",
            );
        };
        result
    }

    unsafe extern "C" fn client_monotonic_time_ns(_context: *mut c_void) -> u64 {
        0
    }

    fn client_api(context: *mut ClientContext) -> ClientApi {
        ClientApi {
            abi_major: ABI_MAJOR,
            abi_minor: ABI_MINOR,
            context: context.cast::<c_void>(),
            log: client_log,
            emit_runtime_event: client_emit_runtime_event,
            complete_model_request: client_complete_model_request,
            fail_model_request: client_fail_model_request,
            read_action_metadata: client_read_action_metadata,
            read_value_debug_json: client_read_value_debug_json,
            monotonic_time_ns: client_monotonic_time_ns,
        }
    }

    #[derive(Clone)]
    struct ValueData {
        type_name: String,
        canonical_encoding: Vec<u8>,
    }

    impl ValueData {
        fn from_ref(value: ValueRef) -> Result<Self, Status> {
            if value.handle != 0 {
                return Err(status(StatusCode::InvalidArgument, b"invalid value handle"));
            }
            let Some(type_name) = (unsafe { text(value.type_name) }) else {
                return Err(status(StatusCode::InvalidArgument, b"invalid value type"));
            };
            let bytes = value.canonical_encoding;
            if type_name.is_empty()
                || bytes.len > MAX_VIEW_BYTES
                || (bytes.len == 0 && !bytes.data.is_null())
                || (bytes.len > 0 && bytes.data.is_null())
            {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid value encoding",
                ));
            }
            let canonical_encoding = if bytes.len == 0 {
                Vec::new()
            } else {
                unsafe { slice::from_raw_parts(bytes.data, bytes.len).to_vec() }
            };
            Ok(Self {
                type_name: type_name.to_owned(),
                canonical_encoding,
            })
        }
    }

    #[derive(Clone)]
    struct ActionBinding {
        action: ActionHandle,
        input_type: String,
    }

    #[derive(Clone)]
    struct NodeState {
        parent: NodeHandle,
        slot: String,
        contract_name: String,
        contract_major: u32,
        contract_minor: u32,
        explicit_key: ValueData,
        properties: BTreeMap<String, ValueData>,
        children: BTreeMap<String, Vec<NodeHandle>>,
        actions: BTreeMap<String, ActionBinding>,
    }

    fn append_json_string(output: &mut Vec<u8>, value: &str) {
        output.push(b'"');
        for byte in value.bytes() {
            match byte {
                b'"' => output.extend_from_slice(b"\\\""),
                b'\\' => output.extend_from_slice(b"\\\\"),
                0x08 => output.extend_from_slice(b"\\b"),
                b'\n' => output.extend_from_slice(b"\\n"),
                0x0c => output.extend_from_slice(b"\\f"),
                b'\r' => output.extend_from_slice(b"\\r"),
                b'\t' => output.extend_from_slice(b"\\t"),
                0x00..=0x1f => {
                    output.extend_from_slice(format!("\\u{byte:04x}").as_bytes());
                }
                _ => output.push(byte),
            }
        }
        output.push(b'"');
    }

    fn append_json_value(output: &mut Vec<u8>, value: &ValueData) {
        output.extend_from_slice(b"{\"type\":");
        append_json_string(output, &value.type_name);
        output.extend_from_slice(b",\"value\":");
        output.push(b'"');
        for byte in &value.canonical_encoding {
            output.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        output.extend_from_slice(b"\"}");
    }

    fn append_node_json(
        output: &mut Vec<u8>,
        node: NodeHandle,
        nodes: &HashMap<NodeHandle, NodeState>,
    ) {
        let state = nodes.get(&node).expect("node state should exist");
        output.extend_from_slice(b"{\"actions\":{");
        for (index, (event_name, action)) in state.actions.iter().enumerate() {
            if index != 0 {
                output.push(b',');
            }
            append_json_string(output, event_name);
            output.extend_from_slice(b":{\"action_id\":");
            append_json_string(output, &action.action.to_string());
            output.extend_from_slice(b",\"debug_kind\":null,\"input_type\":");
            append_json_string(output, &action.input_type);
            output.push(b'}');
        }
        output.extend_from_slice(b"},\"call_site_id\":null,\"contract\":{\"id\":");
        append_json_string(output, &state.contract_name);
        output.extend_from_slice(b",\"name\":");
        append_json_string(output, &state.contract_name);
        output.extend_from_slice(b",\"version\":");
        append_json_string(
            output,
            &format!("{}.{}", state.contract_major, state.contract_minor),
        );
        output.extend_from_slice(b"},\"function_instance_id\":null,\"key\":");
        append_json_value(output, &state.explicit_key);
        output.extend_from_slice(b",\"kind\":\"node\",\"properties\":{");
        for (index, (property, value)) in state.properties.iter().enumerate() {
            if index != 0 {
                output.push(b',');
            }
            append_json_string(output, property);
            output.push(b':');
            append_json_value(output, value);
        }
        output.extend_from_slice(b"},\"slots\":{");
        for (index, (slot, children)) in state.children.iter().enumerate() {
            if index != 0 {
                output.push(b',');
            }
            append_json_string(output, slot);
            output.extend_from_slice(b":[");
            for (child_index, child) in children.iter().enumerate() {
                if child_index != 0 {
                    output.push(b',');
                }
                append_node_json(output, *child, nodes);
            }
            output.push(b']');
        }
        output.extend_from_slice(b"}}");
    }

    fn encode_surface_state(
        roots: &[NodeHandle],
        nodes: &HashMap<NodeHandle, NodeState>,
    ) -> Result<Vec<u8>, Status> {
        let mut body = Vec::new();
        match roots {
            [] => body.extend_from_slice(b"{\"kind\":\"empty\"}"),
            [root] => append_node_json(&mut body, *root, nodes),
            roots => {
                body.extend_from_slice(b"{\"children\":[");
                for (index, root) in roots.iter().enumerate() {
                    if index != 0 {
                        body.push(b',');
                    }
                    append_node_json(&mut body, *root, nodes);
                }
                body.extend_from_slice(b"],\"kind\":\"fragment\"}");
            }
        }
        let length = u32::try_from(body.len())
            .map_err(|_| status(StatusCode::InvalidArgument, b"semantic state is too large"))?;
        let mut frame = b"ORNA-UI/1 ".to_vec();
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&body);
        Ok(frame)
    }

    fn valid_typed_value(value: &serde_json::Value) -> bool {
        let Some(value) = value.as_object() else {
            return false;
        };
        value.len() == 2
            && value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value.contains_key("value")
    }

    fn valid_contract(value: &serde_json::Value) -> bool {
        let Some(value) = value.as_object() else {
            return false;
        };
        value.len() == 3
            && value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value
                .get("version")
                .and_then(serde_json::Value::as_str)
                .is_some()
    }

    fn valid_action(value: &serde_json::Value) -> bool {
        let Some(value) = value.as_object() else {
            return false;
        };
        value
            .get("action_id")
            .and_then(serde_json::Value::as_str)
            .is_some()
            && value
                .get("input_type")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value
                .get("debug_kind")
                .is_none_or(|debug_kind| debug_kind.is_null() || debug_kind.is_string())
    }

    fn valid_source_origin(value: &serde_json::Value) -> bool {
        let Some(value) = value.as_object() else {
            return value.is_null();
        };
        value
            .keys()
            .all(|key| matches!(key.as_str(), "source_unit_id" | "start" | "end"))
            && value
                .get("source_unit_id")
                .is_none_or(|source_unit_id| source_unit_id.is_string())
            && value
                .get("start")
                .is_none_or(|start| start.as_i64().is_some())
            && value.get("end").is_none_or(|end| end.as_i64().is_some())
    }

    fn valid_ui_value(value: &serde_json::Value) -> bool {
        let Some(value) = value.as_object() else {
            return false;
        };
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("empty") => value.len() == 1,
            Some("fragment") => {
                value.len() == 2
                    && value
                        .get("children")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|children| children.iter().all(valid_ui_value))
            }
            Some("node") => {
                if value.len() < 5 || value.len() > 9 {
                    return false;
                }
                if value.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "kind"
                            | "contract"
                            | "call_site_id"
                            | "function_instance_id"
                            | "key"
                            | "properties"
                            | "slots"
                            | "actions"
                            | "source_origin"
                    )
                }) {
                    return false;
                }
                if !value.get("contract").is_some_and(valid_contract)
                    || !value
                        .get("call_site_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                    || !value
                        .get("function_instance_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                {
                    return false;
                }
                let Some(properties) = value
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                else {
                    return false;
                };
                if !properties.values().all(valid_typed_value) {
                    return false;
                }
                let Some(slots) = value.get("slots").and_then(serde_json::Value::as_object) else {
                    return false;
                };
                if !slots.values().all(|children| {
                    children
                        .as_array()
                        .is_some_and(|children| children.iter().all(valid_ui_value))
                }) {
                    return false;
                }
                let Some(actions) = value.get("actions").and_then(serde_json::Value::as_object)
                else {
                    return false;
                };
                actions.values().all(valid_action)
                    && value.get("source_origin").is_none_or(valid_source_origin)
            }
            _ => false,
        }
    }

    fn valid_canonical_frame(frame: &[u8]) -> bool {
        if frame.len() < 14 || &frame[..10] != b"ORNA-UI/1 " {
            return false;
        }
        let Ok(body_length) = u32::try_from(frame.len() - 14) else {
            return false;
        };
        let declared_length = u32::from_be_bytes(
            frame[10..14]
                .try_into()
                .expect("frame length is four bytes"),
        );
        declared_length == body_length
            && serde_json::from_slice::<serde_json::Value>(&frame[14..]).is_ok_and(|value| {
                valid_ui_value(&value)
                    && serde_json::to_vec(&value).is_ok_and(|canonical| canonical == frame[14..])
            })
    }

    struct SurfaceState {
        revision: u64,
        nodes: HashSet<NodeHandle>,
        node_state: HashMap<NodeHandle, NodeState>,
        roots: Vec<NodeHandle>,
        /// Caller-provided operation tokens are only aliases. The fixture owns
        /// the handles that appear in the semantic tree and callback registry.
        node_aliases: HashMap<NodeHandle, NodeHandle>,
        action_aliases: HashMap<ActionHandle, ActionHandle>,
        owned_handles: HashSet<Handle>,
        records: Vec<String>,
        semantic: Vec<u8>,
        visible: bool,
    }

    impl SurfaceState {
        fn resolve_node(&self, token: NodeHandle) -> Option<NodeHandle> {
            if self.nodes.contains(&token) {
                Some(token)
            } else {
                self.node_aliases
                    .get(&token)
                    .copied()
                    .filter(|node| self.nodes.contains(node))
            }
        }

        fn resolve_action(&self, token: ActionHandle) -> Option<ActionHandle> {
            if self
                .node_state
                .values()
                .any(|node| node.actions.values().any(|binding| binding.action == token))
            {
                Some(token)
            } else {
                self.action_aliases.get(&token).copied().filter(|action| {
                    self.node_state.values().any(|node| {
                        node.actions
                            .values()
                            .any(|binding| binding.action == *action)
                    })
                })
            }
        }

        fn detach_child(&mut self, child: NodeHandle) {
            self.roots.retain(|node| *node != child);
            for state in self.node_state.values_mut() {
                for children in state.children.values_mut() {
                    children.retain(|node| *node != child);
                }
                state.children.retain(|_, children| !children.is_empty());
            }
        }

        fn remove_subtree(
            &mut self,
            root: NodeHandle,
            retired_nodes: &mut Vec<NodeHandle>,
            retired_actions: &mut Vec<ActionHandle>,
        ) -> bool {
            if !self.nodes.contains(&root) {
                return false;
            }
            let mut stack = vec![root];
            let mut removed = Vec::new();
            while let Some(node) = stack.pop() {
                if let Some(state) = self.node_state.get(&node) {
                    for children in state.children.values() {
                        stack.extend(children.iter().copied());
                    }
                }
                removed.push(node);
            }
            self.detach_child(root);
            for node in removed {
                if let Some(state) = self.node_state.remove(&node) {
                    retired_actions.extend(state.actions.values().map(|binding| binding.action));
                }
                self.nodes.remove(&node);
                self.node_aliases.retain(|_, actual| *actual != node);
                retired_nodes.push(node);
            }
            for action in retired_actions.iter().copied() {
                self.action_aliases.retain(|_, actual| *actual != action);
            }
            true
        }
    }

    #[derive(Clone, Copy)]
    struct RequestRecord {
        surface: SurfaceHandle,
        _model: ModelHandle,
    }
    #[derive(Clone)]
    struct OwnedValueRef {
        handle: Handle,
        type_name: Vec<u8>,
        canonical_encoding: Vec<u8>,
    }

    impl OwnedValueRef {
        fn from_ref(value: ValueRef) -> Self {
            let type_name = unsafe {
                slice::from_raw_parts(value.type_name.data.cast::<u8>(), value.type_name.len)
            }
            .to_vec();
            let canonical_encoding = if value.canonical_encoding.len == 0 {
                Vec::new()
            } else {
                unsafe {
                    slice::from_raw_parts(
                        value.canonical_encoding.data,
                        value.canonical_encoding.len,
                    )
                    .to_vec()
                }
            };
            Self {
                handle: value.handle,
                type_name,
                canonical_encoding,
            }
        }

        fn as_ref(&self) -> ValueRef {
            ValueRef {
                handle: self.handle,
                type_name: StringView {
                    data: self.type_name.as_ptr().cast::<c_char>(),
                    len: self.type_name.len(),
                },
                canonical_encoding: BytesView {
                    data: if self.canonical_encoding.is_empty() {
                        ptr::null()
                    } else {
                        self.canonical_encoding.as_ptr()
                    },
                    len: self.canonical_encoding.len(),
                },
            }
        }
    }

    fn owned_string_view(view: StringView) -> Vec<u8> {
        if view.len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len).to_vec() }
        }
    }

    fn owned_bytes_view(view: BytesView) -> Vec<u8> {
        if view.len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(view.data, view.len).to_vec() }
        }
    }

    enum OwnedRuntimeEvent {
        Action {
            surface: SurfaceHandle,
            node: NodeHandle,
            action: ActionHandle,
            payload: OwnedValueRef,
        },
        FocusChanged {
            surface: SurfaceHandle,
            node: NodeHandle,
            action: ActionHandle,
            payload: OwnedValueRef,
        },
        LayoutStateChanged {
            surface: SurfaceHandle,
            node: NodeHandle,
            semantic_state_name: Vec<u8>,
            semantic_state: OwnedValueRef,
            opaque_runtime_state: Vec<u8>,
        },
        SurfaceClosed {
            surface: SurfaceHandle,
        },
        ModelRangeRequest {
            request: RequestHandle,
            model: ModelHandle,
            start: u64,
            count: u64,
            sort_filter_token: Vec<u8>,
        },
        ModelChildrenRequest {
            request: RequestHandle,
            model: ModelHandle,
            parent_key: OwnedValueRef,
        },
        Diagnostic {
            code: StatusCode,
            message: Vec<u8>,
        },
    }
    impl OwnedRuntimeEvent {
        fn from_event(event: &RuntimeEvent) -> Result<Self, Status> {
            match event.kind {
                EventKind::Action => {
                    let value = unsafe { event.as_.action };
                    Ok(Self::Action {
                        surface: value.surface,
                        node: value.node,
                        action: value.action,
                        payload: OwnedValueRef::from_ref(value.payload),
                    })
                }
                EventKind::FocusChanged => {
                    let value = unsafe { event.as_.action };
                    Ok(Self::FocusChanged {
                        surface: value.surface,
                        node: value.node,
                        action: value.action,
                        payload: OwnedValueRef::from_ref(value.payload),
                    })
                }
                EventKind::LayoutStateChanged => {
                    let value = unsafe { event.as_.layout_state };
                    Ok(Self::LayoutStateChanged {
                        surface: value.surface,
                        node: value.node,
                        semantic_state_name: owned_string_view(value.semantic_state_name),
                        semantic_state: OwnedValueRef::from_ref(value.semantic_state),
                        opaque_runtime_state: owned_bytes_view(value.opaque_runtime_state),
                    })
                }
                EventKind::SurfaceClosed => Ok(Self::SurfaceClosed {
                    surface: unsafe { event.as_.surface_closed.surface },
                }),
                EventKind::ModelRangeRequest => {
                    let value = unsafe { event.as_.range_request };
                    Ok(Self::ModelRangeRequest {
                        request: value.request,
                        model: value.model,
                        start: value.start,
                        count: value.count,
                        sort_filter_token: owned_string_view(value.sort_filter_token),
                    })
                }
                EventKind::ModelChildrenRequest => {
                    let value = unsafe { event.as_.children_request };
                    Ok(Self::ModelChildrenRequest {
                        request: value.request,
                        model: value.model,
                        parent_key: OwnedValueRef::from_ref(value.parent_key),
                    })
                }
                EventKind::Diagnostic => {
                    let status = unsafe { event.as_.diagnostic.status };
                    Ok(Self::Diagnostic {
                        code: status.code,
                        message: owned_string_view(status.message),
                    })
                }
                _ => Err(status(
                    StatusCode::InvalidArgument,
                    b"unknown runtime event",
                )),
            }
        }

        fn as_ffi(&self) -> RuntimeEvent {
            match self {
                Self::Action {
                    surface,
                    node,
                    action,
                    payload,
                } => RuntimeEvent {
                    kind: EventKind::Action,
                    as_: RuntimeEventArgs {
                        action: ActionEvent {
                            surface: *surface,
                            node: *node,
                            action: *action,
                            payload: payload.as_ref(),
                        },
                    },
                },
                Self::FocusChanged {
                    surface,
                    node,
                    action,
                    payload,
                } => RuntimeEvent {
                    kind: EventKind::FocusChanged,
                    as_: RuntimeEventArgs {
                        action: ActionEvent {
                            surface: *surface,
                            node: *node,
                            action: *action,
                            payload: payload.as_ref(),
                        },
                    },
                },
                Self::LayoutStateChanged {
                    surface,
                    node,
                    semantic_state_name,
                    semantic_state,
                    opaque_runtime_state,
                } => RuntimeEvent {
                    kind: EventKind::LayoutStateChanged,
                    as_: RuntimeEventArgs {
                        layout_state: LayoutStateEvent {
                            surface: *surface,
                            node: *node,
                            semantic_state_name: StringView {
                                data: semantic_state_name.as_ptr().cast::<c_char>(),
                                len: semantic_state_name.len(),
                            },
                            semantic_state: semantic_state.as_ref(),
                            opaque_runtime_state: BytesView {
                                data: if opaque_runtime_state.is_empty() {
                                    ptr::null()
                                } else {
                                    opaque_runtime_state.as_ptr()
                                },
                                len: opaque_runtime_state.len(),
                            },
                        },
                    },
                },
                Self::SurfaceClosed { surface } => RuntimeEvent {
                    kind: EventKind::SurfaceClosed,
                    as_: RuntimeEventArgs {
                        surface_closed: SurfaceClosedEvent { surface: *surface },
                    },
                },
                Self::ModelRangeRequest {
                    request,
                    model,
                    start,
                    count,
                    sort_filter_token,
                } => RuntimeEvent {
                    kind: EventKind::ModelRangeRequest,
                    as_: RuntimeEventArgs {
                        range_request: ModelRangeRequest {
                            request: *request,
                            model: *model,
                            start: *start,
                            count: *count,
                            sort_filter_token: StringView {
                                data: sort_filter_token.as_ptr().cast::<c_char>(),
                                len: sort_filter_token.len(),
                            },
                        },
                    },
                },
                Self::ModelChildrenRequest {
                    request,
                    model,
                    parent_key,
                } => RuntimeEvent {
                    kind: EventKind::ModelChildrenRequest,
                    as_: RuntimeEventArgs {
                        children_request: ModelChildrenRequest {
                            request: *request,
                            model: *model,
                            parent_key: parent_key.as_ref(),
                        },
                    },
                },
                Self::Diagnostic { code, message } => RuntimeEvent {
                    kind: EventKind::Diagnostic,
                    as_: RuntimeEventArgs {
                        diagnostic: DiagnosticEvent {
                            status: Status {
                                code: *code,
                                message: StringView {
                                    data: message.as_ptr().cast::<c_char>(),
                                    len: message.len(),
                                },
                            },
                        },
                    },
                },
            }
        }
    }

    struct RuntimeState {
        owner: ThreadId,
        handle: RuntimeHandle,
        client: ClientApi,
        shutdown_requested: bool,
        terminal: bool,
        surfaces: HashMap<SurfaceHandle, SurfaceState>,
        requests: HashMap<RequestHandle, RequestRecord>,
        pending_events: VecDeque<OwnedRuntimeEvent>,
        cancelled_requests: HashMap<RequestHandle, RequestRecord>,
        node_tokens: HashMap<NodeHandle, SurfaceHandle>,
        action_tokens: HashMap<ActionHandle, SurfaceHandle>,
        known_handles: HashSet<Handle>,
        retired_handles: HashSet<Handle>,
        known_surfaces: HashSet<SurfaceHandle>,
        known_nodes: HashSet<NodeHandle>,
        known_actions: HashSet<ActionHandle>,
        known_models: HashSet<ModelHandle>,
        known_requests: HashSet<RequestHandle>,
        allocated_nodes: HashSet<NodeHandle>,
        allocated_actions: HashSet<ActionHandle>,
    }

    unsafe impl Send for RuntimeState {}

    impl RuntimeState {
        fn new(client: ClientApi) -> Self {
            let handle = next_unreserved_handle();
            let mut known_handles = HashSet::new();
            known_handles.insert(handle);
            Self {
                owner: thread::current().id(),
                handle,
                client,
                shutdown_requested: false,
                terminal: false,
                surfaces: HashMap::new(),
                requests: HashMap::new(),
                pending_events: VecDeque::new(),
                cancelled_requests: HashMap::new(),
                node_tokens: HashMap::new(),
                action_tokens: HashMap::new(),
                known_handles,
                retired_handles: HashSet::new(),
                known_surfaces: HashSet::new(),
                known_nodes: HashSet::new(),
                known_actions: HashSet::new(),
                known_models: HashSet::new(),
                known_requests: HashSet::new(),
                allocated_nodes: HashSet::new(),
                allocated_actions: HashSet::new(),
            }
        }

        fn context(&self) -> &ClientContext {
            unsafe { &*self.client.context.cast::<ClientContext>() }
        }

        fn next_handle(&mut self) -> Handle {
            let handle = next_unreserved_handle();
            self.known_handles.insert(handle);
            handle
        }

        fn allocate_node_handle(&mut self) -> NodeHandle {
            let handle = self.next_handle();
            self.known_nodes.insert(handle);
            self.allocated_nodes.insert(handle);
            handle
        }

        fn allocate_action_handle(&mut self) -> ActionHandle {
            let handle = self.next_handle();
            self.known_actions.insert(handle);
            self.allocated_actions.insert(handle);
            handle
        }

        fn operational(&self) -> Result<(), Status> {
            if self.terminal || self.shutdown_requested {
                Err(status(StatusCode::Failed, b"runtime is shutting down"))
            } else {
                Ok(())
            }
        }

        fn check_surface(&self, handle: SurfaceHandle) -> Result<(), Status> {
            if handle == 0 || !self.known_surfaces.contains(&handle) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign surface handle",
                ));
            }
            if self.retired_handles.contains(&handle) || !self.surfaces.contains_key(&handle) {
                return Err(status(StatusCode::NotFound, b"surface handle is not live"));
            }
            Ok(())
        }

        fn check_node(&self, handle: NodeHandle) -> Result<(), Status> {
            if handle == 0 || !self.known_nodes.contains(&handle) {
                return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
            }
            if self.retired_handles.contains(&handle)
                || !self
                    .surfaces
                    .values()
                    .any(|surface| surface.nodes.contains(&handle))
            {
                return Err(status(StatusCode::NotFound, b"node handle is not live"));
            }
            Ok(())
        }

        fn check_action(&self, handle: ActionHandle) -> Result<(), Status> {
            if handle == 0 || !self.known_actions.contains(&handle) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign action handle",
                ));
            }
            if self.retired_handles.contains(&handle)
                || !self.surfaces.values().any(|surface| {
                    surface
                        .node_state
                        .values()
                        .any(|node| node.actions.values().any(|action| action.action == handle))
                })
            {
                return Err(status(StatusCode::NotFound, b"action handle is not live"));
            }
            Ok(())
        }

        fn check_model(&self, handle: ModelHandle) -> Result<(), Status> {
            if handle == 0 || !self.known_models.contains(&handle) {
                return Err(status(StatusCode::InvalidArgument, b"foreign model handle"));
            }
            if self.retired_handles.contains(&handle)
                || !self
                    .requests
                    .values()
                    .any(|request| request._model == handle)
            {
                return Err(status(StatusCode::NotFound, b"model handle is not live"));
            }
            Ok(())
        }

        fn check_request(&self, handle: RequestHandle) -> Result<(), Status> {
            if handle == 0 || !self.known_requests.contains(&handle) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign request handle",
                ));
            }
            if self.retired_handles.contains(&handle) || !self.requests.contains_key(&handle) {
                return Err(status(StatusCode::NotFound, b"request handle is not live"));
            }
            Ok(())
        }

        fn check_node_on_surface(
            &self,
            node: NodeHandle,
            surface: SurfaceHandle,
        ) -> Result<(), Status> {
            self.check_surface(surface)?;
            self.check_node(node)?;
            if !self
                .surfaces
                .get(&surface)
                .expect("surface checked above")
                .nodes
                .contains(&node)
            {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"node belongs to another surface",
                ));
            }
            Ok(())
        }

        fn check_action_on_surface(
            &self,
            action: ActionHandle,
            surface: SurfaceHandle,
        ) -> Result<(), Status> {
            self.check_surface(surface)?;
            self.check_action(action)?;
            let belongs = self
                .surfaces
                .get(&surface)
                .expect("surface checked above")
                .node_state
                .values()
                .any(|node| {
                    node.actions
                        .values()
                        .any(|binding| binding.action == action)
                });
            if !belongs {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"action belongs to another surface",
                ));
            }
            Ok(())
        }
        fn check_action_payload_type(
            &self,
            action: ActionHandle,
            surface: SurfaceHandle,
            payload: ValueRef,
        ) -> Result<(), Status> {
            let Some(actual) = (unsafe { text(payload.type_name) }) else {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"invalid action payload",
                ));
            };
            let expected = self
                .surfaces
                .get(&surface)
                .expect("surface checked above")
                .node_state
                .values()
                .find_map(|node| {
                    node.actions
                        .values()
                        .find(|binding| binding.action == action)
                        .map(|binding| binding.input_type.as_str())
                });
            if expected != Some(actual) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"action payload type mismatch",
                ));
            }
            Ok(())
        }

        fn check_request_model(
            &self,
            request: RequestHandle,
            model: ModelHandle,
        ) -> Result<SurfaceHandle, Status> {
            self.check_request(request)?;
            self.check_model(model)?;
            let record = self.requests.get(&request).expect("request checked above");
            if record._model != model {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"request belongs to another model",
                ));
            }
            Ok(record.surface)
        }
        fn resolve_node_token(
            &self,
            surface: SurfaceHandle,
            staged: &SurfaceState,
            token: NodeHandle,
        ) -> Result<NodeHandle, Status> {
            if token == 0 {
                return Err(status(StatusCode::InvalidArgument, b"zero node handle"));
            }
            if let Some(node) = staged.resolve_node(token) {
                return Ok(node);
            }
            if let Some(owner) = self.node_tokens.get(&token) {
                let live_elsewhere = *owner != surface
                    && self
                        .surfaces
                        .get(owner)
                        .is_some_and(|state| state.resolve_node(token).is_some());
                return Err(if live_elsewhere {
                    status(
                        StatusCode::InvalidArgument,
                        b"node belongs to another surface",
                    )
                } else {
                    status(StatusCode::NotFound, b"node handle is not live")
                });
            }
            if self.known_nodes.contains(&token) {
                let live_elsewhere = self
                    .surfaces
                    .iter()
                    .any(|(owner, state)| *owner != surface && state.nodes.contains(&token));
                return Err(if live_elsewhere {
                    status(
                        StatusCode::InvalidArgument,
                        b"node belongs to another surface",
                    )
                } else {
                    status(StatusCode::NotFound, b"node handle is not live")
                });
            }
            if self.known_handles.contains(&token) {
                return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
            }
            if is_reserved_handle(token) {
                return Err(status(StatusCode::InvalidArgument, b"foreign node handle"));
            }
            Err(status(StatusCode::NotFound, b"node handle is not live"))
        }

        fn resolve_action_token(
            &self,
            surface: SurfaceHandle,
            staged: &SurfaceState,
            token: ActionHandle,
        ) -> Result<ActionHandle, Status> {
            if token == 0 {
                return Err(status(StatusCode::InvalidArgument, b"zero action handle"));
            }
            if let Some(action) = staged.resolve_action(token) {
                return Ok(action);
            }
            if let Some(owner) = self.action_tokens.get(&token) {
                let live_elsewhere = *owner != surface
                    && self
                        .surfaces
                        .get(owner)
                        .is_some_and(|state| state.resolve_action(token).is_some());
                return Err(if live_elsewhere {
                    status(
                        StatusCode::InvalidArgument,
                        b"action belongs to another surface",
                    )
                } else {
                    status(StatusCode::NotFound, b"action handle is not live")
                });
            }
            if self.known_actions.contains(&token) {
                let live_elsewhere = self.surfaces.iter().any(|(owner, state)| {
                    *owner != surface && state.resolve_action(token).is_some()
                });
                return Err(if live_elsewhere {
                    status(
                        StatusCode::InvalidArgument,
                        b"action belongs to another surface",
                    )
                } else {
                    status(StatusCode::NotFound, b"action handle is not live")
                });
            }
            if self.known_handles.contains(&token) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign action handle",
                ));
            }
            if is_reserved_handle(token) {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign action handle",
                ));
            }
            Err(status(StatusCode::NotFound, b"action handle is not live"))
        }

        fn check_runtime(&self, runtime: RuntimeHandle) -> Result<(), Status> {
            if runtime == 0 || self.handle != runtime {
                return Err(status(
                    StatusCode::InvalidArgument,
                    b"foreign runtime handle",
                ));
            }
            Ok(())
        }

        fn valid_bytes_view(bytes: BytesView) -> bool {
            bytes.len <= MAX_VIEW_BYTES
                && ((bytes.len == 0 && bytes.data.is_null())
                    || (bytes.len > 0 && !bytes.data.is_null()))
        }

        fn valid_value_ref(value: ValueRef) -> bool {
            value.handle == 0
                && unsafe { text(value.type_name) }.is_some_and(|name| !name.is_empty())
                && Self::valid_bytes_view(value.canonical_encoding)
        }

        fn valid_status(status: Status) -> bool {
            let valid_code = matches!(
                status.code,
                StatusCode::Ok
                    | StatusCode::InvalidArgument
                    | StatusCode::Unsupported
                    | StatusCode::NotFound
                    | StatusCode::Busy
                    | StatusCode::Cancelled
                    | StatusCode::Failed
                    | StatusCode::Internal
                    | StatusCode::StaleRevision
            );
            valid_code
                && (unsafe { text(status.message) })
                    .is_some_and(|message| message.as_bytes() == status_message(status.code))
        }

        fn validate_event(
            &self,
            event: &RuntimeEvent,
        ) -> Result<(SurfaceHandle, RequestHandle), Status> {
            match event.kind {
                EventKind::Action => {
                    let value = unsafe { event.as_.action };
                    self.check_node_on_surface(value.node, value.surface)?;
                    self.check_action_on_surface(value.action, value.surface)?;
                    if !Self::valid_value_ref(value.payload) {
                        return Err(status(
                            StatusCode::InvalidArgument,
                            b"invalid action payload",
                        ));
                    }
                    self.check_action_payload_type(value.action, value.surface, value.payload)?;
                    Ok((value.surface, 0))
                }
                EventKind::FocusChanged => {
                    let value = unsafe { event.as_.action };
                    self.check_node_on_surface(value.node, value.surface)?;
                    if value.action != 0 {
                        self.check_action_on_surface(value.action, value.surface)?;
                        if !Self::valid_value_ref(value.payload) {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"invalid focus payload",
                            ));
                        }
                        self.check_action_payload_type(value.action, value.surface, value.payload)?;
                    } else if !Self::valid_value_ref(value.payload) {
                        return Err(status(
                            StatusCode::InvalidArgument,
                            b"invalid focus payload",
                        ));
                    }
                    Ok((value.surface, 0))
                }
                EventKind::LayoutStateChanged => {
                    let value = unsafe { event.as_.layout_state };
                    self.check_node_on_surface(value.node, value.surface)?;
                    let Some(name) = (unsafe { text(value.semantic_state_name) }) else {
                        return Err(status(
                            StatusCode::InvalidArgument,
                            b"invalid layout state name",
                        ));
                    };
                    if name.is_empty()
                        || !Self::valid_value_ref(value.semantic_state)
                        || !Self::valid_bytes_view(value.opaque_runtime_state)
                    {
                        return Err(status(StatusCode::InvalidArgument, b"invalid layout state"));
                    }
                    Ok((value.surface, 0))
                }
                EventKind::SurfaceClosed => {
                    let surface = unsafe { event.as_.surface_closed.surface };
                    self.check_surface(surface)?;
                    Ok((surface, 0))
                }
                EventKind::ModelRangeRequest => {
                    let value = unsafe { event.as_.range_request };
                    let surface = self.check_request_model(value.request, value.model)?;
                    if unsafe { text(value.sort_filter_token) }.is_none() {
                        return Err(status(StatusCode::InvalidArgument, b"invalid sort filter"));
                    }
                    Ok((surface, value.request))
                }
                EventKind::ModelChildrenRequest => {
                    let value = unsafe { event.as_.children_request };
                    let surface = self.check_request_model(value.request, value.model)?;
                    if !Self::valid_value_ref(value.parent_key) {
                        return Err(status(StatusCode::InvalidArgument, b"invalid parent key"));
                    }
                    Ok((surface, value.request))
                }
                EventKind::Diagnostic => {
                    let diagnostic = unsafe { event.as_.diagnostic };
                    if !Self::valid_status(diagnostic.status) {
                        return Err(status(StatusCode::InvalidArgument, b"invalid diagnostic"));
                    }
                    Ok((0, 0))
                }
                _ => Err(status(
                    StatusCode::InvalidArgument,
                    b"unknown runtime event",
                )),
            }
        }

        fn counters(&self) -> Arc<ReleaseCounters> {
            Arc::clone(&self.context().counters)
        }

        fn emit(&mut self, event: RuntimeEvent) -> Status {
            if let Err(error) = self.validate_event(&event) {
                return error;
            }
            let event = match OwnedRuntimeEvent::from_event(&event) {
                Ok(event) => event,
                Err(error) => return error,
            };
            self.pending_events.push_back(event);
            ok()
        }

        fn drain_events(&mut self) -> Status {
            while let Some(event) = self.pending_events.pop_front() {
                let event = event.as_ffi();
                let result = unsafe {
                    (self.client.emit_runtime_event)(self.client.context, self.handle, &event)
                };
                if result.code != StatusCode::Ok {
                    return result;
                }
            }
            ok()
        }

        fn create_surface(
            &mut self,
            options: *const SurfaceCreateOptions,
            output: *mut SurfaceHandle,
        ) -> Status {
            if let Err(error) = self.operational() {
                return error;
            }
            if options.is_null() || output.is_null() {
                return status(StatusCode::InvalidArgument, b"missing surface argument");
            }
            let options = unsafe { &*options };
            let Some(kind) = (unsafe { text(options.surface_kind) }) else {
                return status(StatusCode::InvalidArgument, b"invalid surface kind");
            };
            if kind.is_empty() {
                return status(StatusCode::InvalidArgument, b"empty surface kind");
            }
            let handle = self.next_handle();
            self.known_surfaces.insert(handle);
            self.surfaces.insert(
                handle,
                SurfaceState {
                    revision: 0,
                    nodes: HashSet::new(),
                    node_state: HashMap::new(),
                    roots: Vec::new(),
                    node_aliases: HashMap::new(),
                    action_aliases: HashMap::new(),
                    owned_handles: HashSet::new(),
                    records: Vec::new(),
                    semantic: encode_surface_state(&[], &HashMap::new())
                        .expect("empty semantic state should encode"),
                    visible: false,
                },
            );
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .register_surface(handle);
            unsafe {
                *output = handle;
            }
            ok()
        }

        fn cancel_requests_for_surface(&mut self, surface: SurfaceHandle) -> Status {
            let requests = self
                .requests
                .iter()
                .filter_map(|(request, record)| {
                    (record.surface == surface).then_some((*request, *record))
                })
                .collect::<Vec<_>>();
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            for (request, record) in requests {
                let failure = status(StatusCode::Cancelled, b"request cancelled with surface");
                let result = unsafe {
                    (self.client.fail_model_request)(self.client.context, request, failure)
                };
                if result.code != StatusCode::Ok {
                    // Keep ownership until the cancellation outcome is delivered so a failed
                    // callback can be retried by a later teardown or shutdown attempt.
                    return result;
                }
                self.requests.remove(&request);
                self.retired_handles.insert(request);
                self.retired_handles.insert(record._model);
                let mut handles = self
                    .context()
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                handles.retire_request(request);
                handles.retire_model(record._model);
                drop(handles);
            }
            ok()
        }
        fn destroy_surface(&mut self, handle: SurfaceHandle) -> Status {
            if let Err(error) = self.operational() {
                return error;
            }
            if let Err(error) = self.check_surface(handle) {
                return error;
            }
            let owned_handles = self
                .surfaces
                .get(&handle)
                .expect("surface checked above")
                .owned_handles
                .clone();
            let result = self.cancel_requests_for_surface(handle);
            if result.code != StatusCode::Ok {
                return result;
            }
            let event = RuntimeEvent {
                kind: EventKind::SurfaceClosed,
                as_: RuntimeEventArgs {
                    surface_closed: SurfaceClosedEvent { surface: handle },
                },
            };
            let result = self.emit(event);
            if result.code != StatusCode::Ok {
                return result;
            }
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            self.surfaces
                .remove(&handle)
                .expect("surface should remain until closed event");
            self.retired_handles.insert(handle);
            self.retired_handles.extend(owned_handles);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_surface(handle);
            result
        }

        fn apply_batch(&mut self, handle: SurfaceHandle, batch: *const UiBatch) -> Status {
            if let Err(error) = self.operational() {
                return error;
            }
            if let Err(error) = self.check_surface(handle) {
                return error;
            }
            if batch.is_null() {
                return status(StatusCode::InvalidArgument, b"missing UI batch");
            }
            let batch = unsafe { &*batch };
            let Some(current) = self.surfaces.get(&handle) else {
                return status(StatusCode::NotFound, b"surface handle is not live");
            };
            if batch.semantic_revision <= current.revision {
                return status(StatusCode::StaleRevision, b"stale semantic revision");
            }
            let Some(expected_revision) = current.revision.checked_add(1) else {
                return status(StatusCode::InvalidArgument, b"semantic revision exhausted");
            };
            if batch.semantic_revision != expected_revision {
                return status(StatusCode::InvalidArgument, b"semantic revision gap");
            }
            if batch.operation_count == 0
                || batch.operation_count > MAX_BATCH_OPERATIONS
                || batch.operations.is_null()
            {
                return status(StatusCode::InvalidArgument, b"invalid UI batch");
            }
            let operations =
                unsafe { slice::from_raw_parts(batch.operations, batch.operation_count) };
            let mut next = SurfaceState {
                revision: batch.semantic_revision,
                nodes: current.nodes.clone(),
                node_state: current.node_state.clone(),
                roots: current.roots.clone(),
                node_aliases: current.node_aliases.clone(),
                action_aliases: current.action_aliases.clone(),
                owned_handles: current.owned_handles.clone(),
                records: current.records.clone(),
                semantic: current.semantic.clone(),
                visible: current.visible,
            };
            let mut allocated_nodes = Vec::new();
            let mut allocated_actions = Vec::new();
            let mut allocated_action_inputs = Vec::new();
            let mut retired_nodes = Vec::new();
            let mut retired_actions = Vec::new();
            let mut reserved_node_tokens = Vec::new();
            let mut reserved_action_tokens = Vec::new();
            let result: Result<(), Status> = (|| {
                for operation in operations {
                    match operation.kind {
                        UiOperationKind::MountNode => {
                            let value = unsafe { operation.as_.mount_node };
                            let Some(contract) = (unsafe { owned_text(value.contract_name) })
                            else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid node contract",
                                ));
                            };
                            let Some(slot) = (unsafe { owned_text(value.slot) }) else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid node slot",
                                ));
                            };
                            let parent = if value.parent == 0 {
                                0
                            } else {
                                self.resolve_node_token(handle, &next, value.parent)
                                    .map_err(|error| {
                                        if error.code == StatusCode::NotFound
                                            && !self.known_handles.contains(&value.parent)
                                        {
                                            status(
                                                StatusCode::InvalidArgument,
                                                b"invalid mount parent",
                                            )
                                        } else {
                                            error
                                        }
                                    })?
                            };
                            if value.node == 0
                                || next.node_aliases.contains_key(&value.node)
                                || next.nodes.contains(&value.node)
                                || slot.is_empty()
                                || contract != SINK_NAME
                                || value.contract_major != 1
                                || value.contract_minor != 0
                            {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid mount operation",
                                ));
                            }
                            if let Some(owner) = self.node_tokens.get(&value.node) {
                                return Err(
                                    if *owner == handle || !self.surfaces.contains_key(owner) {
                                        status(StatusCode::NotFound, b"node handle is not live")
                                    } else {
                                        status(
                                            StatusCode::InvalidArgument,
                                            b"node belongs to another surface",
                                        )
                                    },
                                );
                            }
                            if self.retired_handles.contains(&value.node) {
                                return Err(status(
                                    StatusCode::NotFound,
                                    b"node handle is not live",
                                ));
                            }
                            if self.known_handles.contains(&value.node) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign node handle",
                                ));
                            }
                            if self.action_tokens.contains_key(&value.node) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign node handle",
                                ));
                            }
                            let ordinal_limit = if parent == 0 {
                                next.roots.len()
                            } else {
                                next.node_state
                                    .get(&parent)
                                    .and_then(|state| state.children.get(&slot))
                                    .map_or(0, Vec::len)
                            };
                            if value.ordinal > ordinal_limit {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid mount ordinal",
                                ));
                            }
                            let explicit_key = ValueData::from_ref(value.explicit_key)?;
                            if !reserve_alias(value.node) {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"foreign node handle",
                                ));
                            }
                            reserved_node_tokens.push(value.node);
                            self.node_tokens.insert(value.node, handle);
                            let actual = self.allocate_node_handle();
                            allocated_nodes.push(actual);
                            next.nodes.insert(actual);
                            next.node_aliases.insert(value.node, actual);
                            next.owned_handles.insert(actual);
                            next.node_state.insert(
                                actual,
                                NodeState {
                                    parent,
                                    slot: slot.clone(),
                                    contract_name: contract,
                                    contract_major: value.contract_major,
                                    contract_minor: value.contract_minor,
                                    explicit_key,
                                    properties: BTreeMap::new(),
                                    children: BTreeMap::new(),
                                    actions: BTreeMap::new(),
                                },
                            );
                            if parent == 0 {
                                next.roots.insert(value.ordinal, actual);
                            } else {
                                next.node_state
                                    .get_mut(&parent)
                                    .expect("validated mount parent")
                                    .children
                                    .entry(slot.clone())
                                    .or_default()
                                    .insert(value.ordinal, actual);
                            }
                            next.records
                                .push(format!("mount:{actual}:{slot}:{}", value.ordinal));
                        }
                        UiOperationKind::UnmountNode => {
                            let token = unsafe { operation.as_.unmount_node };
                            let node = self.resolve_node_token(handle, &next, token)?;
                            if !next.remove_subtree(node, &mut retired_nodes, &mut retired_actions)
                            {
                                return Err(status(
                                    StatusCode::NotFound,
                                    b"node handle is not live",
                                ));
                            }
                            next.records.push(format!("unmount:{node}"));
                        }
                        UiOperationKind::SetProperty | UiOperationKind::ClearProperty => {
                            let value = unsafe { operation.as_.set_property };
                            let Some(property) = (unsafe { owned_text(value.property) }) else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid property",
                                ));
                            };
                            let node = self.resolve_node_token(handle, &next, value.node)?;
                            if property.is_empty() {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid property",
                                ));
                            }
                            let state = next.node_state.get_mut(&node).expect("live node state");
                            if operation.kind == UiOperationKind::SetProperty {
                                state
                                    .properties
                                    .insert(property.clone(), ValueData::from_ref(value.value)?);
                            } else {
                                state.properties.remove(&property);
                            }
                            next.records.push(format!(
                                "property:{}:{}:{}",
                                operation.kind.0, node, property
                            ));
                        }
                        UiOperationKind::InsertChild
                        | UiOperationKind::RemoveChild
                        | UiOperationKind::MoveChild => {
                            let value = unsafe { operation.as_.child };
                            let Some(slot) = (unsafe { owned_text(value.slot) }) else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid child slot",
                                ));
                            };
                            let parent = self.resolve_node_token(handle, &next, value.parent)?;
                            let child = self.resolve_node_token(handle, &next, value.child)?;
                            if slot.is_empty() || parent == child {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid child operation",
                                ));
                            }
                            let mut ancestor = parent;
                            while ancestor != 0 {
                                if ancestor == child {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"child cycle",
                                    ));
                                }
                                ancestor = next
                                    .node_state
                                    .get(&ancestor)
                                    .map_or(0, |state| state.parent);
                            }
                            let kind = operation.kind;
                            if kind == UiOperationKind::RemoveChild {
                                let valid = next
                                    .node_state
                                    .get(&parent)
                                    .and_then(|state| state.children.get(&slot))
                                    .and_then(|children| children.get(value.ordinal))
                                    == Some(&child);
                                if !valid {
                                    return Err(status(
                                        StatusCode::NotFound,
                                        b"child is not mounted in slot",
                                    ));
                                }
                                next.detach_child(child);
                                let child_state =
                                    next.node_state.get_mut(&child).expect("live child state");
                                child_state.parent = 0;
                                child_state.slot.clear();
                            } else {
                                let attached = next
                                    .node_state
                                    .get(&child)
                                    .is_some_and(|state| state.parent != 0)
                                    || next.roots.contains(&child);
                                if kind == UiOperationKind::InsertChild && attached {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"child is already mounted",
                                    ));
                                }
                                if kind == UiOperationKind::MoveChild && !attached {
                                    return Err(status(
                                        StatusCode::NotFound,
                                        b"child is not mounted",
                                    ));
                                }
                                next.detach_child(child);
                                let ordinal_limit = next
                                    .node_state
                                    .get(&parent)
                                    .and_then(|state| state.children.get(&slot))
                                    .map_or(0, Vec::len);
                                if value.ordinal > ordinal_limit {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"invalid child ordinal",
                                    ));
                                }
                                next.node_state
                                    .get_mut(&parent)
                                    .expect("validated child parent")
                                    .children
                                    .entry(slot.clone())
                                    .or_default()
                                    .insert(value.ordinal, child);
                                let child_state =
                                    next.node_state.get_mut(&child).expect("live child state");
                                child_state.parent = parent;
                                child_state.slot = slot.clone();
                            }
                            next.records.push(format!(
                                "child:{}:{}:{}:{}",
                                kind.0, parent, child, value.ordinal
                            ));
                        }
                        UiOperationKind::BindAction | UiOperationKind::UnbindAction => {
                            let value = unsafe { operation.as_.bind_action };
                            let Some(event_name) = (unsafe { owned_text(value.event_name) }) else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid action event",
                                ));
                            };
                            let Some(input_type) = (unsafe { owned_text(value.input_type) }) else {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid action input type",
                                ));
                            };
                            let node = self.resolve_node_token(handle, &next, value.node)?;
                            if event_name.is_empty() {
                                return Err(status(
                                    StatusCode::InvalidArgument,
                                    b"invalid action event",
                                ));
                            }
                            if operation.kind == UiOperationKind::BindAction {
                                if value.action == 0
                                    || next.action_aliases.contains_key(&value.action)
                                    || next.resolve_action(value.action).is_some()
                                    || input_type.is_empty()
                                {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"invalid action operation",
                                    ));
                                }
                                if let Some(owner) = self.action_tokens.get(&value.action) {
                                    return Err(
                                        if *owner == handle || !self.surfaces.contains_key(owner) {
                                            status(
                                                StatusCode::NotFound,
                                                b"action handle is not live",
                                            )
                                        } else {
                                            status(
                                                StatusCode::InvalidArgument,
                                                b"action belongs to another surface",
                                            )
                                        },
                                    );
                                }
                                if self.retired_handles.contains(&value.action) {
                                    return Err(status(
                                        StatusCode::NotFound,
                                        b"action handle is not live",
                                    ));
                                }
                                if self.known_handles.contains(&value.action) {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"foreign action handle",
                                    ));
                                }
                                if self.node_tokens.contains_key(&value.action) {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"foreign action handle",
                                    ));
                                }
                                if next
                                    .node_state
                                    .get(&node)
                                    .expect("live node state")
                                    .actions
                                    .contains_key(&event_name)
                                {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"action event is already bound",
                                    ));
                                }
                                if !reserve_alias(value.action) {
                                    return Err(status(
                                        StatusCode::InvalidArgument,
                                        b"foreign action handle",
                                    ));
                                }
                                reserved_action_tokens.push(value.action);
                                self.action_tokens.insert(value.action, handle);
                                let actual = self.allocate_action_handle();
                                allocated_actions.push(actual);
                                next.action_aliases.insert(value.action, actual);
                                allocated_action_inputs.push((actual, input_type.clone()));
                                next.owned_handles.insert(actual);
                                next.node_state
                                    .get_mut(&node)
                                    .expect("live node state")
                                    .actions
                                    .insert(
                                        event_name.clone(),
                                        ActionBinding {
                                            action: actual,
                                            input_type,
                                        },
                                    );
                                next.records
                                    .push(format!("action:{node}:{actual}:{event_name}"));
                            } else {
                                let actual =
                                    self.resolve_action_token(handle, &next, value.action)?;
                                let binding_matches = next
                                    .node_state
                                    .get(&node)
                                    .and_then(|state| state.actions.get(&event_name))
                                    .is_some_and(|binding| binding.action == actual);
                                if !binding_matches {
                                    return Err(status(
                                        StatusCode::NotFound,
                                        b"action handle is not bound",
                                    ));
                                }
                                next.node_state
                                    .get_mut(&node)
                                    .expect("live node state")
                                    .actions
                                    .remove(&event_name);
                                next.action_aliases.retain(|_, mapped| *mapped != actual);
                                retired_actions.push(actual);
                                next.records
                                    .push(format!("action:{}:{}:{}", node, actual, event_name));
                            }
                        }
                        UiOperationKind::SetFocus | UiOperationKind::SetAccessibility => {
                            return Err(status(
                                StatusCode::Unsupported,
                                b"operation is not in the fixture",
                            ));
                        }
                        _ => {
                            return Err(status(
                                StatusCode::InvalidArgument,
                                b"unknown UI operation",
                            ));
                        }
                    }
                }
                next.semantic = encode_surface_state(&next.roots, &next.node_state)?;
                Ok(())
            })();
            if let Err(error) = result {
                for token in &reserved_node_tokens {
                    self.node_tokens.remove(token);
                }
                for token in &reserved_action_tokens {
                    self.action_tokens.remove(token);
                }
                let mut reservations = HANDLE_RESERVATIONS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for token in reserved_node_tokens
                    .iter()
                    .chain(reserved_action_tokens.iter())
                {
                    reservations.remove(token);
                }
                for node in &allocated_nodes {
                    self.known_handles.remove(node);
                    self.known_nodes.remove(node);
                    self.allocated_nodes.remove(node);
                    reservations.remove(node);
                }
                for action in &allocated_actions {
                    self.known_handles.remove(action);
                    self.known_actions.remove(action);
                    self.allocated_actions.remove(action);
                    reservations.remove(action);
                }
                return error;
            }
            self.retired_handles.extend(retired_nodes.iter().copied());
            self.retired_handles.extend(retired_actions.iter().copied());
            let surface = self
                .surfaces
                .get_mut(&handle)
                .expect("surface checked above");
            *surface = next;
            let mut handles = self
                .context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for node in allocated_nodes {
                handles.register_node(node, handle);
            }
            for (action, input_type) in allocated_action_inputs {
                handles.register_action(action, handle, input_type);
            }
            for node in retired_nodes {
                handles.retire_node(node);
            }
            for action in retired_actions {
                handles.retire_action(action);
            }
            ok()
        }

        fn capture(&self, handle: SurfaceHandle, output: *mut OwnedBytes) -> Status {
            if self.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            if output.is_null() {
                return status(StatusCode::InvalidArgument, b"missing output");
            }
            if let Err(error) = self.check_surface(handle) {
                return error;
            }
            let surface = self.surfaces.get(&handle).expect("surface checked above");
            if !valid_canonical_frame(&surface.semantic) {
                return status(StatusCode::Internal, b"invalid semantic state frame");
            }
            unsafe {
                *output = owned_bytes(surface.semantic.clone(), self.counters());
            }
            ok()
        }

        fn set_visible(&mut self, handle: SurfaceHandle, visible: u8) -> Status {
            if let Err(error) = self.operational() {
                return error;
            }
            if let Err(error) = self.check_surface(handle) {
                return error;
            }
            self.surfaces
                .get_mut(&handle)
                .expect("surface checked above")
                .visible = visible != 0;
            ok()
        }

        fn start_model_request(
            &mut self,
            surface: SurfaceHandle,
        ) -> Result<(ModelHandle, RequestHandle), Status> {
            self.operational()?;
            self.check_surface(surface)?;
            let model = self.next_handle();
            let request = self.next_handle();
            self.known_models.insert(model);
            self.known_requests.insert(request);
            self.requests.insert(
                request,
                RequestRecord {
                    surface,
                    _model: model,
                },
            );
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .register_model(model, surface);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .register_request(request, model, surface);
            let event = RuntimeEvent {
                kind: EventKind::ModelRangeRequest,
                as_: RuntimeEventArgs {
                    range_request: ModelRangeRequest {
                        request,
                        model,
                        start: 0,
                        count: 16,
                        sort_filter_token: view(b"fixture"),
                    },
                },
            };
            let result = self.emit(event);
            if result.code != StatusCode::Ok {
                self.requests.remove(&request);
                if let Some(state) = self.surfaces.get_mut(&surface) {
                    state.owned_handles.remove(&model);
                    state.owned_handles.remove(&request);
                }
                self.context()
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .retire_request(request);
                self.context()
                    .handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .retire_model(model);
                self.retired_handles.insert(model);
                self.retired_handles.insert(request);
                return Err(result);
            }
            Ok((model, request))
        }

        fn cancel_request(&mut self, request: RequestHandle) -> Status {
            if self.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            if self.cancelled_requests.contains_key(&request) {
                return status(StatusCode::Cancelled, b"request is already cancelled");
            }
            if let Err(error) = self.check_request(request) {
                return error;
            }
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            let record = self
                .requests
                .get(&request)
                .copied()
                .expect("request checked above");
            let failure = status(StatusCode::Cancelled, b"request cancelled");
            let result =
                unsafe { (self.client.fail_model_request)(self.client.context, request, failure) };
            if result.code != StatusCode::Ok {
                return result;
            }
            self.requests.remove(&request);
            self.cancelled_requests.insert(request, record);
            if let Some(surface) = self.surfaces.get_mut(&record.surface) {
                surface.owned_handles.remove(&request);
                surface.owned_handles.remove(&record._model);
            }
            self.retired_handles.insert(request);
            self.retired_handles.insert(record._model);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_request(request);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_model(record._model);
            result
        }

        fn apply_model_rows(&mut self, request: RequestHandle, rows: ValueRef) -> Status {
            if self.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            if self.cancelled_requests.contains_key(&request) {
                return status(StatusCode::Cancelled, b"request was cancelled");
            }
            let Some(record) = self.requests.get(&request).copied() else {
                return if request == 0 || !self.known_handles.contains(&request) {
                    status(StatusCode::InvalidArgument, b"foreign request handle")
                } else {
                    status(StatusCode::NotFound, b"request handle is not live")
                };
            };
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            if !Self::valid_value_ref(rows) {
                return status(StatusCode::InvalidArgument, b"invalid model result");
            }
            let result =
                unsafe { (self.client.complete_model_request)(self.client.context, request, rows) };
            self.requests.remove(&request);
            if let Some(surface_state) = self.surfaces.get_mut(&record.surface) {
                surface_state.owned_handles.remove(&request);
                surface_state.owned_handles.remove(&record._model);
            }
            self.retired_handles.insert(request);
            self.retired_handles.insert(record._model);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_request(request);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_model(record._model);
            result
        }

        fn emit_diagnostic(&mut self, diagnostic: Status) -> Status {
            if self.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            let event = RuntimeEvent {
                kind: EventKind::Diagnostic,
                as_: RuntimeEventArgs {
                    diagnostic: DiagnosticEvent { status: diagnostic },
                },
            };
            self.emit(event)
        }

        fn shutdown(&mut self) -> Status {
            if self.terminal {
                return ok();
            }
            self.shutdown_requested = true;
            let surfaces = self.surfaces.keys().copied().collect::<Vec<_>>();
            for surface in surfaces {
                let result = self.destroy_surface_for_shutdown(surface);
                if result.code != StatusCode::Ok {
                    return result;
                }
            }
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            self.terminal = true;
            let mut log = self
                .context()
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            log.mark_terminal();
            ok()
        }

        fn destroy_surface_for_shutdown(&mut self, handle: SurfaceHandle) -> Status {
            let Some(owned_handles) = self
                .surfaces
                .get(&handle)
                .map(|surface| surface.owned_handles.clone())
            else {
                return ok();
            };
            let result = self.cancel_requests_for_surface(handle);
            if result.code != StatusCode::Ok {
                return result;
            }
            let event = RuntimeEvent {
                kind: EventKind::SurfaceClosed,
                as_: RuntimeEventArgs {
                    surface_closed: SurfaceClosedEvent { surface: handle },
                },
            };
            let result = self.emit(event);
            if result.code != StatusCode::Ok {
                return result;
            }
            let result = self.drain_events();
            if result.code != StatusCode::Ok {
                return result;
            }
            self.surfaces
                .remove(&handle)
                .expect("surface should remain until closed event");
            self.retired_handles.insert(handle);
            self.retired_handles.extend(owned_handles);
            self.context()
                .handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_surface(handle);
            result
        }
    }

    struct GlobalRuntime {
        runtime: Option<RuntimeState>,
    }

    unsafe impl Send for GlobalRuntime {}

    fn global() -> &'static Mutex<GlobalRuntime> {
        static GLOBAL: LazyLock<Mutex<GlobalRuntime>> =
            LazyLock::new(|| Mutex::new(GlobalRuntime { runtime: None }));
        &GLOBAL
    }

    fn serial_lock() -> MutexGuard<'static, ()> {
        static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        SERIAL.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn with_runtime<F>(runtime: RuntimeHandle, operation: F) -> Status
    where
        F: FnOnce(&mut RuntimeState) -> Status,
    {
        let guard = match global().try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                return status(StatusCode::Busy, b"runtime call is already active");
            }
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let mut guard = guard;
        let Some(state) = guard.runtime.as_mut() else {
            return status(StatusCode::InvalidArgument, b"runtime is not loaded");
        };
        if state.owner != thread::current().id() {
            return status(StatusCode::Busy, b"runtime belongs to another thread");
        }
        if let Err(error) = state.check_runtime(runtime) {
            return error;
        }
        operation(state)
    }

    unsafe extern "C" fn fixture_create(
        options: *const RuntimeCreateOptions,
        output: *mut RuntimeHandle,
    ) -> Status {
        if options.is_null() || output.is_null() {
            return status(StatusCode::InvalidArgument, b"missing runtime argument");
        }
        if validate_api(&FIXTURE_API).is_err() {
            return status(StatusCode::Internal, b"fixture descriptor is invalid");
        }
        let mut guard = match global().try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                return status(StatusCode::Busy, b"runtime call is already active");
            }
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if guard.runtime.is_some() {
            return status(StatusCode::Busy, b"runtime already exists");
        }
        let options = unsafe { &*options };

        if options.client.is_null() {
            return status(StatusCode::InvalidArgument, b"missing client API");
        }
        let client = unsafe { *options.client };
        if client.abi_major != ABI_MAJOR
            || client.abi_minor > ABI_MINOR
            || client.context.is_null()
            || !valid_client_api(&client)
            || with_registered_context(client.context, |_| ()).is_none()
        {
            return status(StatusCode::InvalidArgument, b"incompatible client API");
        }
        let state = RuntimeState::new(client);
        let Some(()) = with_registered_context(client.context, |context| {
            context.runtime.store(state.handle, Ordering::SeqCst);
        }) else {
            return status(StatusCode::InvalidArgument, b"unregistered client context");
        };
        unsafe {
            *output = state.handle;
        }
        guard.runtime = Some(state);
        ok()
    }

    unsafe extern "C" fn fixture_destroy(runtime: RuntimeHandle) {
        let guard = match global().try_lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let mut guard = guard;
        let Some(state) = guard.runtime.as_ref() else {
            return;
        };
        if state.owner != thread::current().id() || state.handle != runtime {
            return;
        }
        if !state.terminal || !state.surfaces.is_empty() || !state.requests.is_empty() {
            return;
        }
        let context = state.client.context;
        let Some(()) = with_registered_context(context, |context| {
            context.runtime.store(0, Ordering::SeqCst);
        }) else {
            return;
        };
        guard.runtime = None;
    }

    unsafe extern "C" fn fixture_start(runtime: RuntimeHandle) -> Status {
        with_runtime(runtime, |state| {
            state.operational().map_or_else(|error| error, |_| ok())
        })
    }

    unsafe extern "C" fn fixture_poll(runtime: RuntimeHandle, _timeout_ms: u32) -> Status {
        with_runtime(runtime, |state| {
            if let Err(error) = state.operational() {
                return error;
            }
            state.drain_events()
        })
    }

    unsafe extern "C" fn fixture_request_shutdown(runtime: RuntimeHandle) -> Status {
        with_runtime(runtime, RuntimeState::shutdown)
    }

    unsafe extern "C" fn fixture_create_surface(
        runtime: RuntimeHandle,
        options: *const SurfaceCreateOptions,
        output: *mut SurfaceHandle,
    ) -> Status {
        with_runtime(runtime, |state| state.create_surface(options, output))
    }

    unsafe extern "C" fn fixture_destroy_surface(
        runtime: RuntimeHandle,
        surface: SurfaceHandle,
    ) -> Status {
        with_runtime(runtime, |state| state.destroy_surface(surface))
    }

    unsafe extern "C" fn fixture_apply_ui_batch(
        runtime: RuntimeHandle,
        surface: SurfaceHandle,
        batch: *const UiBatch,
    ) -> Status {
        with_runtime(runtime, |state| state.apply_batch(surface, batch))
    }

    unsafe extern "C" fn fixture_set_surface_visible(
        runtime: RuntimeHandle,
        surface: SurfaceHandle,
        visible: u8,
    ) -> Status {
        with_runtime(runtime, |state| state.set_visible(surface, visible))
    }

    unsafe extern "C" fn fixture_capture_semantic_state(
        runtime: RuntimeHandle,
        surface: SurfaceHandle,
        output: *mut OwnedBytes,
    ) -> Status {
        with_runtime(runtime, |state| state.capture(surface, output))
    }

    unsafe extern "C" fn fixture_capture_opaque_state(
        runtime: RuntimeHandle,
        surface: SurfaceHandle,
        output: *mut OwnedBytes,
    ) -> Status {
        with_runtime(runtime, |state| {
            if state.terminal {
                return status(StatusCode::Failed, b"runtime is shut down");
            }
            if output.is_null() {
                return status(StatusCode::InvalidArgument, b"missing output");
            }
            if let Err(error) = state.check_surface(surface) {
                return error;
            }
            unsafe {
                *output = owned_bytes(Vec::new(), state.counters());
            }
            ok()
        })
    }

    unsafe extern "C" fn fixture_apply_model_rows(
        runtime: RuntimeHandle,
        request: RequestHandle,
        rows: ValueRef,
    ) -> Status {
        with_runtime(runtime, |state| state.apply_model_rows(request, rows))
    }

    unsafe extern "C" fn fixture_cancel_request(
        runtime: RuntimeHandle,
        request: RequestHandle,
    ) -> Status {
        with_runtime(runtime, |state| state.cancel_request(request))
    }

    static FIXTURE_API: RuntimeApi = RuntimeApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        describe: fixture_describe,
        create: fixture_create,
        destroy: fixture_destroy,
        start_event_loop: fixture_start,
        poll_event_loop: fixture_poll,
        request_shutdown: fixture_request_shutdown,
        create_surface: fixture_create_surface,
        destroy_surface: fixture_destroy_surface,
        apply_ui_batch: fixture_apply_ui_batch,
        set_surface_visible: fixture_set_surface_visible,
        capture_semantic_state: fixture_capture_semantic_state,
        capture_opaque_state: fixture_capture_opaque_state,
        apply_model_rows: fixture_apply_model_rows,
        cancel_request: fixture_cancel_request,
    };

    struct FixtureSession {
        _serial: MutexGuard<'static, ()>,
        client: Box<ClientContext>,
        runtime: RuntimeHandle,
    }

    impl FixtureSession {
        fn new() -> Self {
            Self::new_with_serial(serial_lock())
        }

        fn new_with_serial(serial: MutexGuard<'static, ()>) -> Self {
            {
                let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
                guard.runtime = None;
            }
            let mut client = Box::new(ClientContext::new());
            let context = (&mut *client) as *mut ClientContext as *mut c_void;
            register_context(context);
            let client_api = client_api(context.cast::<ClientContext>());
            let options = RuntimeCreateOptions {
                client: &client_api,
                locale: view(b"en-GB"),
                timezone: view(b"UTC"),
                theme: view(b"default"),
                accessibility_preferences_json: view(b"{}"),
                runtime_configuration_json: view(b"{}"),
            };
            let mut runtime = 0;
            let result = unsafe { (FIXTURE_API.create)(&options, &mut runtime) };
            if result.code != StatusCode::Ok {
                unregister_context(context);
                panic!("fixture runtime creation failed: {:?}", result.code);
            }
            Self {
                _serial: serial,
                client,
                runtime,
            }
        }

        fn create_surface_result(
            &self,
            title: &'static [u8],
        ) -> Result<SurfaceHandle, StatusCode> {
            let options = SurfaceCreateOptions {
                surface_kind: view(b"window"),
                title: view(title),
                state_profile: view(b"local"),
                opaque_runtime_restore_state: BytesView {
                    data: ptr::null(),
                    len: 0,
                },
            };
            let mut surface = 0;
            let result = unsafe {
                (FIXTURE_API.create_surface)(self.runtime, &options, &mut surface)
            };
            if result.code == StatusCode::Ok {
                Ok(surface)
            } else {
                Err(result.code)
            }
        }

        fn create_surface(&self, title: &'static [u8]) -> SurfaceHandle {
            let options = SurfaceCreateOptions {
                surface_kind: view(b"window"),
                title: view(title),
                state_profile: view(b"local"),
                opaque_runtime_restore_state: BytesView {
                    data: ptr::null(),
                    len: 0,
                },
            };
            let mut surface = 0;
            let result =
                unsafe { (FIXTURE_API.create_surface)(self.runtime, &options, &mut surface) };
            assert_eq!(result.code, StatusCode::Ok);
            surface
        }

        fn apply(&self, surface: SurfaceHandle, batch: &UiBatch) -> StatusCode {
            unsafe { (FIXTURE_API.apply_ui_batch)(self.runtime, surface, batch) }.code
        }

        fn capture(&self, surface: SurfaceHandle) -> Vec<u8> {
            let mut output = OwnedBytes {
                data: ptr::null_mut(),
                len: 0,
                owner: ptr::null_mut(),
                release: release_owned,
            };
            let result =
                unsafe { (FIXTURE_API.capture_semantic_state)(self.runtime, surface, &mut output) };
            assert_eq!(result.code, StatusCode::Ok);
            let bytes = if output.len == 0 {
                assert!(
                    output.data.is_null(),
                    "empty owned bytes must have a null data pointer"
                );
                Vec::new()
            } else {
                assert!(
                    !output.data.is_null(),
                    "non-empty owned bytes must have a data pointer"
                );
                unsafe { slice::from_raw_parts(output.data, output.len).to_vec() }
            };
            unsafe {
                (output.release)(output.owner, output.data, output.len);
            }
            bytes
        }

        fn capture_result(&self, surface: SurfaceHandle) -> Result<Vec<u8>, StatusCode> {
            let mut output = OwnedBytes {
                data: ptr::null_mut(),
                len: 0,
                owner: ptr::null_mut(),
                release: release_owned,
            };
            let result = unsafe {
                (FIXTURE_API.capture_semantic_state)(self.runtime, surface, &mut output)
            };
            if result.code != StatusCode::Ok {
                return Err(result.code);
            }
            let bytes = if output.len == 0 {
                if !output.data.is_null() {
                    return Err(StatusCode::Internal);
                }
                Vec::new()
            } else {
                if output.data.is_null() {
                    return Err(StatusCode::Internal);
                }
                unsafe { slice::from_raw_parts(output.data, output.len).to_vec() }
            };
            unsafe {
                (output.release)(output.owner, output.data, output.len);
            }
            Ok(bytes)
        }

        fn destroy_surface(&self, surface: SurfaceHandle) -> StatusCode {
            unsafe { (FIXTURE_API.destroy_surface)(self.runtime, surface) }.code
        }

        fn shutdown(&self) -> StatusCode {
            unsafe { (FIXTURE_API.request_shutdown)(self.runtime) }.code
        }

        fn start_model_request(&self, surface: SurfaceHandle) -> (ModelHandle, RequestHandle) {
            let guard = global().lock().unwrap_or_else(|error| error.into_inner());
            let mut guard = guard;
            let state = guard
                .runtime
                .as_mut()
                .expect("fixture runtime should exist");
            state
                .start_model_request(surface)
                .expect("fixture callback should accept model request")
        }
        fn queue_event(&self, event: RuntimeEvent) -> StatusCode {
            let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
            let state = guard
                .runtime
                .as_mut()
                .expect("fixture runtime should exist");
            state.emit(event).code
        }

        fn fail_next_model_callback(&self) {
            self.client
                .fail_model_callback
                .store(true, Ordering::SeqCst);
        }

        fn node_and_action(&self, surface: SurfaceHandle) -> (NodeHandle, ActionHandle) {
            let guard = global().lock().unwrap_or_else(|error| error.into_inner());
            let state = guard
                .runtime
                .as_ref()
                .expect("fixture runtime should exist");
            let surface_state = state
                .surfaces
                .get(&surface)
                .expect("surface should be live");
            let node = *surface_state
                .nodes
                .iter()
                .next()
                .expect("surface should have a node");
            let action = surface_state
                .node_state
                .get(&node)
                .and_then(|node| node.actions.values().next())
                .map(|binding| binding.action)
                .expect("node should have an action");
            (node, action)
        }

        fn apply_model_rows(&self, request: RequestHandle) -> StatusCode {
            let rows = ValueRef {
                handle: 0,
                type_name: view(b"std.json.Value"),
                canonical_encoding: BytesView {
                    data: ptr::null(),
                    len: 0,
                },
            };
            unsafe { (FIXTURE_API.apply_model_rows)(self.runtime, request, rows) }.code
        }

        fn cancel_request(&self, request: RequestHandle) -> StatusCode {
            unsafe { (FIXTURE_API.cancel_request)(self.runtime, request) }.code
        }

        fn emit_diagnostic(&self) -> StatusCode {
            let guard = global().lock().unwrap_or_else(|error| error.into_inner());
            let mut guard = guard;
            let state = guard
                .runtime
                .as_mut()
                .expect("fixture runtime should exist");
            let result = state.emit_diagnostic(status(StatusCode::Failed, b"fixture diagnostic"));
            if result.code != StatusCode::Ok {
                return result.code;
            }
            state.drain_events().code
        }
        fn poll(&self) -> StatusCode {
            unsafe { (FIXTURE_API.poll_event_loop)(self.runtime, 0) }.code
        }

        fn set_reentry(&self) {
            let mut log = self
                .client
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            log.reenter = true;
        }

        fn callback_log(&self) -> CallbackLogSnapshot {
            let log = self
                .client
                .log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            CallbackLogSnapshot {
                events: log.events.clone(),
                action_payloads: log.action_payloads.clone(),
                completions: log.completions.clone(),
                failures: log.failures.clone(),
                sequence: log.sequence.clone(),
                terminal: log.terminal,
                reentry_status: log.reentry_status,
            }
        }

        fn release_counts(&self) -> (usize, usize) {
            (
                self.client.counters.releases.load(Ordering::SeqCst),
                self.client.counters.invalid.load(Ordering::SeqCst),
            )
        }
    }

    impl Drop for FixtureSession {
        fn drop(&mut self) {
            unsafe {
                let _ = (FIXTURE_API.request_shutdown)(self.runtime);
                (FIXTURE_API.destroy)(self.runtime);
            }
            let context = (&mut *self.client) as *mut ClientContext as *mut c_void;
            unregister_context(context);
        }
    }

    struct HeadlessFixtureState {
        surface: Option<SurfaceHandle>,
        node: Option<NodeHandle>,
        revision: u64,
    }

    struct HeadlessFixtureSession {
        fixture: FixtureSession,
        state: Mutex<HeadlessFixtureState>,
    }

    impl HeadlessFixtureSession {
        fn new() -> Self {
            Self {
                fixture: FixtureSession::new(),
                state: Mutex::new(HeadlessFixtureState {
                    surface: None,
                    node: None,
                    revision: 0,
                }),
            }
        }

        fn create_surface(&self) -> Result<u64, String> {
            let surface = self
                .fixture
                .create_surface_result(b"Headless fixture")
                .map_err(status_error)?;
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.surface = Some(surface);
            state.node = None;
            state.revision = 0;
            Ok(surface)
        }

        fn apply_ui_payload(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
            if !valid_canonical_frame(payload) {
                return Err(status_error(StatusCode::InvalidArgument));
            }
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let Some(surface) = state.surface else {
                return Err(status_error(StatusCode::NotFound));
            };
            let revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| status_error(StatusCode::InvalidArgument))?;
            let had_node = state.node.is_some();
            let node = state.node.unwrap_or_else(next_unreserved_alias_handle);
            let mut operations = [mount(node, 0, view(b"root")), set_property(node, view(b"payload"))];
            operations[1].as_.set_property.value = ValueRef {
                handle: 0,
                type_name: view(b"std.ui.UI"),
                canonical_encoding: BytesView {
                    data: if payload.is_empty() {
                        ptr::null()
                    } else {
                        payload.as_ptr()
                    },
                    len: payload.len(),
                },
            };
            let first_operation = if had_node { 1 } else { 0 };
            let batch = batch(revision, &operations[first_operation..]);
            let result = self.fixture.apply(surface, &batch);
            if result != StatusCode::Ok {
                return Err(status_error(result));
            }
            state.node = Some(node);
            state.revision = revision;
            self.fixture
                .capture_result(surface)
                .map_err(status_error)
        }

        fn destroy_surface(&self, surface: u64) -> Result<(), String> {
            let result = self.fixture.destroy_surface(surface);
            if result != StatusCode::Ok {
                return Err(status_error(result));
            }
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.surface == Some(surface) {
                state.surface = None;
                state.node = None;
                state.revision = 0;
            }
            Ok(())
        }

        fn shutdown(&self) -> Result<(), String> {
            let result = self.fixture.shutdown();
            if result == StatusCode::Ok {
                Ok(())
            } else {
                Err(status_error(result))
            }
        }

        fn is_terminal(&self) -> bool {
            let guard = global().lock().unwrap_or_else(|error| error.into_inner());
            guard.runtime.as_ref().is_some_and(|runtime| runtime.terminal)
        }

        fn last_callback_is_terminal(&self) -> bool {
            self.fixture
                .callback_log()
                .sequence
                .last()
                .is_some_and(|record| record.terminal)
        }
    }

    fn status_error(code: StatusCode) -> String {
        String::from_utf8_lossy(status_message(code)).into_owned()
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct CallbackLogSnapshot {
        events: Vec<EventRecord>,
        action_payloads: Vec<Vec<u8>>,
        completions: Vec<RequestHandle>,
        failures: Vec<(RequestHandle, StatusCode)>,
        sequence: Vec<CallbackRecord>,
        terminal: bool,
        reentry_status: Option<StatusCode>,
    }
    fn empty_value() -> ValueRef {
        ValueRef {
            handle: 0,
            type_name: view(b"std.json.Value"),
            canonical_encoding: BytesView {
                data: ptr::null(),
                len: 0,
            },
        }
    }

    fn mount(node: NodeHandle, parent: NodeHandle, slot: StringView) -> UiOperation {
        UiOperation {
            kind: UiOperationKind::MountNode,
            as_: UiOperationArgs {
                mount_node: MountNode {
                    node,
                    parent,
                    slot,
                    ordinal: 0,
                    contract_name: view(b"std.ui.UI"),
                    contract_major: 1,
                    contract_minor: 0,
                    explicit_key: empty_value(),
                },
            },
        }
    }
    fn unmount(node: NodeHandle) -> UiOperation {
        UiOperation {
            kind: UiOperationKind::UnmountNode,
            as_: UiOperationArgs { unmount_node: node },
        }
    }

    fn bind_action(
        node: NodeHandle,
        event_name: StringView,
        action: ActionHandle,
        input_type: StringView,
    ) -> UiOperation {
        UiOperation {
            kind: UiOperationKind::BindAction,
            as_: UiOperationArgs {
                bind_action: BindAction {
                    node,
                    event_name,
                    action,
                    input_type,
                },
            },
        }
    }
    fn unbind_action(
        node: NodeHandle,
        event_name: StringView,
        action: ActionHandle,
        input_type: StringView,
    ) -> UiOperation {
        let mut operation = bind_action(node, event_name, action, input_type);
        operation.kind = UiOperationKind::UnbindAction;
        operation
    }
    fn child_operation(
        kind: UiOperationKind,
        parent: NodeHandle,
        slot: StringView,
        child: NodeHandle,
        ordinal: usize,
    ) -> UiOperation {
        UiOperation {
            kind,
            as_: UiOperationArgs {
                child: ChildOperation {
                    parent,
                    slot,
                    child,
                    ordinal,
                },
            },
        }
    }

    fn set_property(node: NodeHandle, property: StringView) -> UiOperation {
        UiOperation {
            kind: UiOperationKind::SetProperty,
            as_: UiOperationArgs {
                set_property: SetProperty {
                    node,
                    property,
                    value: empty_value(),
                },
            },
        }
    }

    fn batch(revision: u64, operations: &[UiOperation]) -> UiBatch {
        UiBatch {
            semantic_revision: revision,
            operations: operations.as_ptr(),
            operation_count: operations.len(),
        }
    }
    fn frame_body(frame: &[u8]) -> &[u8] {
        assert!(frame.starts_with(b"ORNA-UI/1 "));
        assert!(frame.len() >= 14);
        let body_length = u32::from_be_bytes(
            frame[10..14]
                .try_into()
                .expect("frame length is four bytes"),
        );
        assert_eq!(frame.len(), 14 + body_length as usize);
        &frame[14..]
    }

    #[test]
    fn loads_valid_fixture_and_rejects_incompatible_table_before_describe() {
        let serial = serial_lock();
        DESCRIBE_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(validate_api(&FIXTURE_API), Ok(()));
        assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 1);

        let mut incompatible = FIXTURE_API;
        incompatible.abi_major = 2;
        DESCRIBE_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(validate_api(&incompatible), Err(LoadError::AbiMajor(2)));
        assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 0);

        DESCRIBE_CALLS.store(0, Ordering::SeqCst);
        let session = FixtureSession::new_with_serial(serial);
        assert_eq!(DESCRIBE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            unsafe { (FIXTURE_API.start_event_loop)(session.runtime) }.code,
            StatusCode::Ok
        );
    }
    #[test]
    fn direct_destroy_before_shutdown_keeps_fixture_runtime_registered() {
        let session = FixtureSession::new();
        let runtime = session.runtime;

        unsafe { (FIXTURE_API.destroy)(runtime) };

        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(guard.runtime.as_ref().map(|state| state.handle), Some(runtime));
        assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);
    }

    #[test]
    fn direct_destroy_from_non_owner_thread_keeps_fixture_runtime_registered() {
        let session = FixtureSession::new();
        let runtime = session.runtime;

        thread::spawn(move || unsafe { (FIXTURE_API.destroy)(runtime) })
            .join()
            .expect("non-owner destroy call should join");

        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(guard.runtime.as_ref().map(|state| state.handle), Some(runtime));
        assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);
    }

    #[test]
    fn owner_destroy_after_shutdown_clears_global_runtime_and_client_handle() {
        let session = FixtureSession::new();
        let runtime = session.runtime;

        assert_eq!(session.shutdown(), StatusCode::Ok);
        assert_eq!(session.client.runtime.load(Ordering::SeqCst), runtime);

        unsafe { (FIXTURE_API.destroy)(runtime) };

        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        assert!(guard.runtime.is_none());
        assert_eq!(session.client.runtime.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn status_messages_have_stable_codes_and_reject_unstructured_text() {
        let codes = [
            StatusCode::Ok,
            StatusCode::InvalidArgument,
            StatusCode::Unsupported,
            StatusCode::NotFound,
            StatusCode::Busy,
            StatusCode::Cancelled,
            StatusCode::Failed,
            StatusCode::Internal,
            StatusCode::StaleRevision,
        ];
        for code in codes {
            let status = status(code, b"contains secret credentials");
            let message = unsafe { text(status.message) }.expect("status message should be valid");
            assert_eq!(message.as_bytes(), status_message(code));
            assert!(!message.contains("secret"));
            assert!(RuntimeState::valid_status(status));
        }
        let invalid = Status {
            code: StatusCode::Failed,
            message: view(b"raw request argument"),
        };
        assert!(!RuntimeState::valid_status(invalid));
    }

    #[test]
    fn rejects_descriptor_with_wrong_contract_name() {
        static WRONG_NAME: ContractVersion = ContractVersion {
            name: view(b"std.ui.Other"),
            major: 1,
            minor: 0,
            features: ptr::null(),
            feature_count: 0,
        };
        let mut descriptor = DESCRIPTOR;
        descriptor.contracts = &WRONG_NAME;
        assert_eq!(
            validate_descriptor(&descriptor),
            Err(LoadError::Descriptor("contract name"))
        );
    }

    #[test]
    fn rejects_duplicate_contracts_versions_unknown_features_and_bad_counts() {
        static DUPLICATES: [ContractVersion; 2] = [CONTRACT, CONTRACT];
        static BAD_VERSION: ContractVersion = ContractVersion {
            name: view(b"std.ui.UI"),
            major: 2,
            minor: 0,
            features: ptr::null(),
            feature_count: 0,
        };
        static DUPLICATE_FEATURES: [StringView; 2] =
            [view(b"accessibility"), view(b"accessibility")];
        static DUPLICATE_FEATURE_CONTRACT: ContractVersion = ContractVersion {
            name: view(b"std.ui.UI"),
            major: 1,
            minor: 0,
            features: DUPLICATE_FEATURES.as_ptr(),
            feature_count: DUPLICATE_FEATURES.len(),
        };

        let mut duplicate = DESCRIPTOR;
        duplicate.contracts = DUPLICATES.as_ptr();
        duplicate.contract_count = DUPLICATES.len();
        assert_eq!(
            validate_descriptor(&duplicate),
            Err(LoadError::Descriptor("contract count"))
        );

        let mut unsupported = DESCRIPTOR;
        unsupported.contracts = &BAD_VERSION;
        assert_eq!(
            validate_descriptor(&unsupported),
            Err(LoadError::Descriptor("contract version"))
        );
        let mut duplicate_features = DESCRIPTOR;
        duplicate_features.contracts = &DUPLICATE_FEATURE_CONTRACT;
        assert_eq!(
            validate_descriptor(&duplicate_features),
            Err(LoadError::Descriptor("contract features"))
        );

        let mut unknown_feature = DESCRIPTOR;
        unknown_feature.features = 1u64 << 63;
        assert_eq!(
            validate_descriptor(&unknown_feature),
            Err(LoadError::Descriptor("unknown feature"))
        );
        let mut malformed_thread_model = DESCRIPTOR;
        malformed_thread_model.thread_model = ThreadModel(99);
        assert_eq!(
            validate_descriptor(&malformed_thread_model),
            Err(LoadError::Descriptor("thread model"))
        );

        let mut malformed = DESCRIPTOR;
        malformed.sinks = ptr::null();
        assert_eq!(
            validate_descriptor(&malformed),
            Err(LoadError::Descriptor("sink count"))
        );
    }

    #[test]
    fn rejects_unknown_operation_and_event_kinds() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Unknown kinds");
        let operations = [UiOperation {
            kind: UiOperationKind(99),
            as_: UiOperationArgs { unmount_node: 0 },
        }];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::InvalidArgument
        );

        let event = RuntimeEvent {
            kind: EventKind(99),
            as_: RuntimeEventArgs {
                diagnostic: DiagnosticEvent {
                    status: status(StatusCode::Failed, b"unknown"),
                },
            },
        };
        assert_eq!(
            unsafe {
                client_emit_runtime_event(
                    (&*session.client as *const ClientContext).cast_mut().cast(),
                    session.runtime,
                    &event,
                )
            }
            .code,
            StatusCode::InvalidArgument
        );
        assert!(session.callback_log().events.is_empty());
    }

    #[test]
    fn rejects_oversized_batches_and_value_payloads() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Input limits");
        let operations = std::iter::repeat_with(|| set_property(1, view(b"property")))
            .take(MAX_BATCH_OPERATIONS + 1)
            .collect::<Vec<_>>();
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::InvalidArgument
        );

        let mut invalid_mount = mount(1, 0, view(b"slot"));
        invalid_mount
            .as_
            .mount_node
            .explicit_key
            .canonical_encoding
            .len = MAX_VIEW_BYTES + 1;
        assert_eq!(
            session.apply(surface, &batch(1, &[invalid_mount])),
            StatusCode::InvalidArgument
        );
    }
    #[test]
    fn rejects_nonzero_value_handles_at_operation_boundary() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Value handles");
        let mut invalid_mount = mount(0x1201, 0, view(b"root"));
        invalid_mount.as_.mount_node.explicit_key.handle = 1;

        assert_eq!(
            session.apply(surface, &batch(1, &[invalid_mount])),
            StatusCode::InvalidArgument
        );
        assert_eq!(
            frame_body(&session.capture(surface)),
            b"{\"kind\":\"empty\"}"
        );
    }

    #[test]
    fn borrowed_batch_input_is_not_retained_and_capture_preserves_values() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Borrowed input");
        assert_eq!(
            frame_body(&session.capture(surface)),
            b"{\"kind\":\"empty\"}"
        );

        let mut slot = *b"slot";
        let slot_view = StringView {
            data: slot.as_mut_ptr().cast::<c_char>(),
            len: slot.len(),
        };
        let mut key = *b"first";
        let mut operation = mount(0xffff, 0, slot_view);
        operation.as_.mount_node.explicit_key = ValueRef {
            handle: 0,
            type_name: view(b"std.json.Value"),
            canonical_encoding: BytesView {
                data: key.as_ptr(),
                len: key.len(),
            },
        };
        assert_eq!(
            session.apply(surface, &batch(1, &[operation])),
            StatusCode::Ok
        );
        slot.copy_from_slice(b"xxxx");
        key.copy_from_slice(b"other");

        let captured = session.capture(surface);
        assert_eq!(
            frame_body(&captured),
            b"{\"actions\":{},\"call_site_id\":null,\"contract\":{\"id\":\"std.ui.UI\",\"name\":\"std.ui.UI\",\"version\":\"1.0\"},\"function_instance_id\":null,\"key\":{\"type\":\"std.json.Value\",\"value\":\"6669727374\"},\"kind\":\"node\",\"properties\":{},\"slots\":{}}",
        );
        assert!(captured.windows(4).any(|window| window == b"slot"));
        assert!(!captured.windows(4).any(|window| window == b"xxxx"));
        assert!(captured.windows(10).any(|window| window == b"6669727374"));
        assert!(!captured.windows(10).any(|window| window == b"6f74686572"));
        assert_eq!(session.release_counts(), (2, 0));
    }
    #[test]
    fn owned_outputs_reject_changed_length_owner_and_double_release() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Owned outputs");
        let mut output = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { (FIXTURE_API.capture_semantic_state)(session.runtime, surface, &mut output) }
                .code,
            StatusCode::Ok,
        );
        let baseline_unknown = UNKNOWN_RELEASES.load(Ordering::SeqCst);
        unsafe {
            (output.release)(output.owner, output.data, output.len + 1);
        }
        assert_eq!(session.release_counts(), (0, 1));
        unsafe {
            (output.release)(output.owner, output.data.wrapping_add(1), output.len);
        }
        assert_eq!(session.release_counts(), (0, 2));
        unsafe {
            (output.release)(output.owner, output.data, output.len);
        }
        assert_eq!(session.release_counts(), (1, 2));
        unsafe {
            (output.release)(output.owner, output.data, output.len);
        }
        assert_eq!(
            UNKNOWN_RELEASES.load(Ordering::SeqCst),
            baseline_unknown + 1
        );
        let mut opaque = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { (FIXTURE_API.capture_opaque_state)(session.runtime, surface, &mut opaque) }
                .code,
            StatusCode::Ok,
        );
        assert!(opaque.data.is_null());
        assert_eq!(opaque.len, 0);
        assert!(!opaque.owner.is_null());
        unsafe {
            (opaque.release)(opaque.owner, opaque.data, opaque.len);
        }
        assert_eq!(session.release_counts(), (2, 2));

        let mut second = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { (FIXTURE_API.capture_semantic_state)(session.runtime, surface, &mut second) }
                .code,
            StatusCode::Ok,
        );
        let wrong_owner = (second.owner as usize + 1) as *mut c_void;
        unsafe {
            (second.release)(wrong_owner, second.data, second.len);
        }
        assert_eq!(
            UNKNOWN_RELEASES.load(Ordering::SeqCst),
            baseline_unknown + 2
        );
        unsafe {
            (second.release)(second.owner, second.data, second.len);
        }
        assert_eq!(session.release_counts(), (3, 2));

        let mut unchanged = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe {
                (FIXTURE_API.capture_semantic_state)(session.runtime, 0xffff, &mut unchanged)
            }
            .code,
            StatusCode::InvalidArgument,
        );
        assert!(unchanged.data.is_null());
        assert_eq!(unchanged.len, 0);
        assert!(unchanged.owner.is_null());
    }

    #[test]
    fn foreign_and_stale_handles_cannot_mutate_a_surface() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Handle provenance");
        let foreign = 0xffff_u64;
        assert_eq!(
            session.destroy_surface(foreign),
            StatusCode::InvalidArgument
        );
        let operations = [mount(9, foreign, view(b"root"))];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::InvalidArgument
        );
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        assert_eq!(session.destroy_surface(surface), StatusCode::NotFound);

        let runtime = session.runtime;
        let cross_thread =
            thread::spawn(move || unsafe { (FIXTURE_API.poll_event_loop)(runtime, 0).code })
                .join()
                .expect("cross-thread probe should join");
        assert_eq!(cross_thread, StatusCode::Busy);
    }
    #[test]
    fn caller_tokens_remain_provenant_across_surfaces_and_lifetimes() {
        let session = FixtureSession::new();
        let owner = session.create_surface(b"Token owner");
        let other = session.create_surface(b"Other surface");
        assert_eq!(
            session.apply(owner, &batch(1, &[mount(0x2001, 0, view(b"root"))])),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(other, &batch(1, &[mount(0x3001, 0, view(b"root"))])),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(other, &batch(2, &[mount(0x2001, 0, view(b"root"))])),
            StatusCode::InvalidArgument
        );
        assert_eq!(
            session.apply(
                owner,
                &batch(
                    2,
                    &[bind_action(
                        0x2001,
                        view(b"activate"),
                        0x2002,
                        view(b"bool")
                    )]
                )
            ),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(
                other,
                &batch(
                    2,
                    &[bind_action(
                        0x3001,
                        view(b"activate"),
                        0x2002,
                        view(b"bool")
                    )]
                )
            ),
            StatusCode::InvalidArgument
        );
        assert_eq!(
            session.apply(
                owner,
                &batch(
                    3,
                    &[unbind_action(
                        0x2001,
                        view(b"activate"),
                        0x2002,
                        view(b"bool")
                    )]
                )
            ),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(
                owner,
                &batch(
                    4,
                    &[bind_action(
                        0x2001,
                        view(b"activate"),
                        0x2002,
                        view(b"bool")
                    )]
                )
            ),
            StatusCode::NotFound
        );
        assert_eq!(
            session.apply(owner, &batch(4, &[unmount(0x2001)])),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(owner, &batch(5, &[mount(0x2001, 0, view(b"root"))])),
            StatusCode::NotFound
        );
        assert_eq!(session.destroy_surface(owner), StatusCode::Ok);
        assert_eq!(
            session.apply(other, &batch(2, &[mount(0x2001, 0, view(b"root"))])),
            StatusCode::NotFound
        );
    }
    #[test]
    fn caller_aliases_reserve_future_generated_handles() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Alias reservation");
        let node_token = NEXT_HANDLE.load(Ordering::SeqCst);
        assert_eq!(
            session.apply(surface, &batch(1, &[mount(node_token, 0, view(b"root"))])),
            StatusCode::Ok
        );

        let action_token = NEXT_HANDLE.load(Ordering::SeqCst);
        assert_eq!(
            session.apply(
                surface,
                &batch(
                    2,
                    &[bind_action(
                        node_token,
                        view(b"activate"),
                        action_token,
                        view(b"bool")
                    )]
                )
            ),
            StatusCode::Ok
        );
        let captured = session.capture(surface);
        let aliased_action_id = format!("\"action_id\":\"{action_token}\"");
        assert!(
            !captured
                .windows(aliased_action_id.len())
                .any(|window| { window == aliased_action_id.as_bytes() })
        );
        assert!(captured.windows(8).any(|window| window == b"activate"));
    }

    #[test]
    fn reserved_handles_are_foreign_to_all_surface_operations() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Reserved handles");
        let foreign = next_unreserved_handle();
        let before = session.capture(surface);
        let operations = [
            unmount(foreign),
            set_property(foreign, view(b"title")),
            child_operation(
                UiOperationKind::InsertChild,
                foreign,
                view(b"slot"),
                0x7001,
                0,
            ),
            bind_action(foreign, view(b"submit"), 0x7002, view(b"std.json.Value")),
            unbind_action(foreign, view(b"submit"), 0x7002, view(b"std.json.Value")),
        ];

        for operation in operations {
            assert_eq!(
                session.apply(surface, &batch(1, std::slice::from_ref(&operation))),
                StatusCode::InvalidArgument
            );
            assert_eq!(session.capture(surface), before);
        }

        let mut reservations = HANDLE_RESERVATIONS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(reservations.remove(&foreign));
    }

    #[test]
    fn failed_batches_release_alias_and_generated_handle_reservations() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Failed reservations");
        let alias = NEXT_HANDLE.load(Ordering::SeqCst);
        let failed = [
            mount(alias, 0, view(b"root")),
            set_property(0x7fff, view(b"title")),
        ];

        assert_eq!(
            session.apply(surface, &batch(1, &failed)),
            StatusCode::NotFound
        );
        assert!(!is_reserved_handle(alias));
        assert!(!is_reserved_handle(alias + 1));
        assert_eq!(
            session.apply(surface, &batch(1, &[mount(alias, 0, view(b"root"))])),
            StatusCode::Ok
        );
    }

    #[test]
    fn insert_child_rejects_an_already_mounted_child_without_moving_it() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Child ownership");
        assert_eq!(
            session.apply(surface, &batch(1, &[mount(0x4001, 0, view(b"root"))])),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(surface, &batch(2, &[mount(0x4002, 0x4001, view(b"child"))])),
            StatusCode::Ok
        );
        let before = session.capture(surface);
        let invalid_insert = [
            child_operation(
                UiOperationKind::InsertChild,
                0x4001,
                view(b"other"),
                0x4002,
                0,
            ),
            set_property(0x4002, view(b"title")),
        ];
        assert_eq!(
            session.apply(surface, &batch(3, &invalid_insert)),
            StatusCode::InvalidArgument
        );
        assert_eq!(session.capture(surface), before);
        assert_eq!(
            session.apply(
                surface,
                &batch(
                    3,
                    &[child_operation(
                        UiOperationKind::MoveChild,
                        0x4001,
                        view(b"other"),
                        0x4002,
                        0,
                    )]
                )
            ),
            StatusCode::Ok
        );
        assert_eq!(
            session.apply(
                surface,
                &batch(
                    4,
                    &[child_operation(
                        UiOperationKind::InsertChild,
                        0x4001,
                        view(b"third"),
                        0x4002,
                        0,
                    )]
                )
            ),
            StatusCode::InvalidArgument
        );
    }

    #[test]
    fn valid_batches_are_atomic_and_revisions_are_deterministic() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Atomic batches");
        let operations = [
            mount(0x1001, 0, view(b"root")),
            set_property(0x1001, view(b"title")),
        ];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::Ok
        );
        let before = session.capture(surface);

        let malformed = [UiOperation {
            kind: UiOperationKind::UnmountNode,
            as_: UiOperationArgs {
                unmount_node: u64::MAX,
            },
        }];
        assert_eq!(
            session.apply(surface, &batch(2, &malformed)),
            StatusCode::NotFound
        );
        assert_eq!(session.capture(surface), before);
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::StaleRevision
        );
        let second = [set_property(0x1001, view(b"status"))];
        assert_eq!(session.apply(surface, &batch(2, &second)), StatusCode::Ok);
        let after_second = session.capture(surface);
        assert_ne!(after_second, before);
        assert_eq!(
            session.apply(surface, &batch(2, &second)),
            StatusCode::StaleRevision
        );
        assert_eq!(
            session.apply(surface, &batch(4, &second)),
            StatusCode::InvalidArgument
        );
        assert_eq!(session.capture(surface), after_second);
    }
    #[test]
    fn canonical_capture_escapes_json_control_characters() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"JSON escaping");
        let operations = [
            mount(0x7201, 0, view(b"root")),
            set_property(0x7201, view(b"\x08\x0c")),
        ];

        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::Ok
        );
        let frame = session.capture(surface);
        assert!(valid_canonical_frame(&frame));
        assert_eq!(
            frame_body(&frame),
            br#"{"actions":{},"call_site_id":null,"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"function_instance_id":null,"key":{"type":"std.json.Value","value":""},"kind":"node","properties":{"\b\f":{"type":"std.json.Value","value":""}},"slots":{}}"#
        );
    }

    #[test]
    fn canonical_frame_validation_rejects_invalid_headers_lengths_and_values() {
        let valid =
            encode_surface_state(&[], &HashMap::new()).expect("empty semantic state should encode");
        assert!(valid_canonical_frame(&valid));

        let mut wrong_magic = valid.clone();
        wrong_magic[0] = b'X';
        assert!(!valid_canonical_frame(&wrong_magic));

        let mut wrong_length = valid.clone();
        wrong_length[13] -= 1;
        assert!(!valid_canonical_frame(&wrong_length));

        assert!(!valid_canonical_frame(b"ORNA-UI/1 \0\0\0\x08not json"));
        assert!(!valid_canonical_frame(
            b"ORNA-UI/1 \0\0\0\x0f{\"kind\":\"node\"}"
        ));
    }
    #[test]
    fn canonical_frame_validation_accepts_minimal_nodes_and_optional_metadata() {
        let frame_for = |body: &[u8]| {
            let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
            frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
            frame.extend_from_slice(body);
            frame
        };
        let minimal = frame_for(
            br#"{"actions":{},"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"kind":"node","properties":{},"slots":{}}"#,
        );
        assert!(valid_canonical_frame(&minimal));

        let with_optional = frame_for(
            br#"{"actions":{"activate":{"action_id":"action","debug_kind":null,"input_type":"bool","label":"Activate"}},"call_site_id":"call-site","contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"function_instance_id":null,"key":null,"kind":"node","properties":{},"slots":{},"source_origin":{"source_unit_id":"unit"}}"#,
        );
        assert!(valid_canonical_frame(&with_optional));
    }

    #[test]
    fn canonical_frame_validation_rejects_unknown_node_fields() {
        let body = br#"{"actions":{},"contract":{"id":"std.ui.UI","name":"std.ui.UI","version":"1.0"},"kind":"node","properties":{},"slots":{},"unexpected":true}"#;
        let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        assert!(!valid_canonical_frame(&frame));
    }
    #[test]
    fn headless_fixture_validates_and_retains_canonical_ui_payload() {
        let session = HeadlessFixtureSession::new();
        let surface = session
            .create_surface()
            .expect("headless fixture surface should be created");
        assert!(
            session
                .apply_ui_payload(b"not an ORNA-UI frame")
                .is_err(),
            "headless fixture must reject invalid UI frames"
        );

        let frame_for = |body: &[u8]| {
            let mut frame = Vec::from(b"ORNA-UI/1 ".as_slice());
            frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
            frame.extend_from_slice(body);
            frame
        };
        let body = br#"{"kind":"empty"}"#;
        let payload = frame_for(body);
        assert!(
            session
                .apply_ui_payload(&frame_for(br#"{ "kind": "empty" }"#))
                .is_err(),
            "headless fixture must reject non-canonical JSON whitespace"
        );
        assert!(
            session
                .apply_ui_payload(&frame_for(br#"{"kind":"fragment","children":[]}"#))
                .is_err(),
            "headless fixture must reject non-canonical JSON object key order"
        );
        let captured = session
            .apply_ui_payload(&payload)
            .expect("headless fixture should accept a canonical UI frame");
        assert!(valid_canonical_frame(&captured));
        let captured_body: serde_json::Value =
            serde_json::from_slice(frame_body(&captured)).expect("capture body should be JSON");
        let encoded_payload = payload
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            captured_body["properties"]["payload"]["type"],
            serde_json::Value::String("std.ui.UI".to_owned())
        );
        assert_eq!(
            captured_body["properties"]["payload"]["value"],
            serde_json::Value::String(encoded_payload)
        );
        session
            .destroy_surface(surface)
            .expect("headless fixture surface should be destroyed");
        session
            .shutdown()
            .expect("headless fixture should shut down");
        assert!(session.is_terminal());
        assert!(session.last_callback_is_terminal());
    }


    #[test]
    fn capture_rejects_a_corrupted_canonical_frame() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Corrupted frame");
        {
            let mut guard = global().lock().unwrap_or_else(|error| error.into_inner());
            guard
                .runtime
                .as_mut()
                .expect("fixture runtime should exist")
                .surfaces
                .get_mut(&surface)
                .expect("fixture surface should exist")
                .semantic = b"corrupted".to_vec();
        }
        let mut output = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { fixture_capture_semantic_state(session.runtime, surface, &mut output) }.code,
            StatusCode::Internal,
        );
        assert!(output.data.is_null());
        assert_eq!(output.len, 0);
    }

    #[test]
    fn callbacks_are_fifo_reentrant_calls_are_busy_and_requests_complete_once() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Callbacks");
        let request_surface = session.create_surface(b"Requests");
        session.set_reentry();
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        let (model, request) = session.start_model_request(request_surface);
        assert_ne!(model, 0);
        assert_eq!(session.apply_model_rows(request), StatusCode::Ok);
        assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
        let log = session.callback_log();
        assert_eq!(log.reentry_status, Some(StatusCode::Busy));
        assert_eq!(log.events[0].kind, EventKind::SurfaceClosed);
        assert_eq!(log.events[1].kind, EventKind::ModelRangeRequest);
        assert_eq!(log.events[1].request, request);
        assert_eq!(log.completions, vec![request]);
        assert_eq!(log.sequence.len(), 3);
        assert_eq!(log.sequence[0].sequence, 0);
        assert_eq!(
            log.sequence[0].kind,
            CallbackKind::Event(log.events[0].clone())
        );
        assert_eq!(log.sequence[1].sequence, 1);
        assert_eq!(
            log.sequence[1].kind,
            CallbackKind::Event(log.events[1].clone())
        );
        assert_eq!(log.sequence[2].sequence, 2);
        assert_eq!(log.sequence[2].kind, CallbackKind::Completion(request));
        assert!(log.sequence.iter().all(|record| !record.terminal));
    }
    #[test]
    fn caller_pump_drains_queued_runtime_events_in_fifo_order() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Caller pumps");
        let (_, request) = session.start_model_request(surface);
        assert!(session.callback_log().events.is_empty());
        assert_eq!(session.poll(), StatusCode::Ok);
        let log = session.callback_log();
        assert_eq!(log.events.len(), 1);
        assert_eq!(log.events[0].kind, EventKind::ModelRangeRequest);
        assert_eq!(log.events[0].request, request);
        assert_eq!(log.sequence.len(), 1);
        assert_eq!(
            log.sequence[0].kind,
            CallbackKind::Event(log.events[0].clone())
        );
        assert!(!log.sequence[0].terminal);
    }
    #[test]
    fn queued_events_copy_borrowed_payloads_before_caller_pump() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Borrowed events");
        let operations = [
            mount(0x7001, 0, view(b"root")),
            bind_action(0x7001, view(b"submit"), 0x7101, view(b"std.json.Value")),
        ];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::Ok
        );
        let (node, action) = session.node_and_action(surface);
        let mut payload = b"before".to_vec();
        let event = RuntimeEvent {
            kind: EventKind::Action,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action,
                    payload: ValueRef {
                        handle: 0,
                        type_name: view(b"std.json.Value"),
                        canonical_encoding: BytesView {
                            data: payload.as_ptr(),
                            len: payload.len(),
                        },
                    },
                },
            },
        };
        assert_eq!(session.queue_event(event), StatusCode::Ok);
        payload.copy_from_slice(b"after!");
        assert_eq!(session.poll(), StatusCode::Ok);
        assert_eq!(
            session.callback_log().action_payloads,
            vec![b"before".to_vec()]
        );
    }

    #[test]
    fn typed_runtime_events_accept_all_declared_payloads() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Typed payloads");
        let operations = [
            mount(0x5001, 0, view(b"root")),
            bind_action(0x5001, view(b"submit"), 0x6001, view(b"std.json.Value")),
        ];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::Ok
        );
        let (node, action) = session.node_and_action(surface);
        let context = (&*session.client as *const ClientContext).cast_mut().cast();

        let action_event = RuntimeEvent {
            kind: EventKind::Action,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action,
                    payload: empty_value(),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &action_event) }.code,
            StatusCode::Ok
        );
        let mut wrong_type_payload = empty_value();
        wrong_type_payload.type_name = view(b"bool");
        let wrong_type_event = RuntimeEvent {
            kind: EventKind::Action,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action,
                    payload: wrong_type_payload,
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &wrong_type_event) }.code,
            StatusCode::InvalidArgument
        );

        let mut foreign_value = empty_value();
        foreign_value.handle = 1;
        let foreign_value_event = RuntimeEvent {
            kind: EventKind::Action,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action,
                    payload: foreign_value,
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &foreign_value_event) }
                .code,
            StatusCode::InvalidArgument
        );

        let focus_event = RuntimeEvent {
            kind: EventKind::FocusChanged,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action: 0,
                    payload: empty_value(),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &focus_event) }.code,
            StatusCode::Ok
        );

        let layout_event = RuntimeEvent {
            kind: EventKind::LayoutStateChanged,
            as_: RuntimeEventArgs {
                layout_state: LayoutStateEvent {
                    surface,
                    node,
                    semantic_state_name: view(b"expanded"),
                    semantic_state: empty_value(),
                    opaque_runtime_state: BytesView {
                        data: ptr::null(),
                        len: 0,
                    },
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &layout_event) }.code,
            StatusCode::Ok
        );

        let (model, request) = session.start_model_request(surface);
        assert_eq!(session.poll(), StatusCode::Ok);
        let children_event = RuntimeEvent {
            kind: EventKind::ModelChildrenRequest,
            as_: RuntimeEventArgs {
                children_request: ModelChildrenRequest {
                    request,
                    model,
                    parent_key: empty_value(),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &children_event) }.code,
            StatusCode::Ok
        );
        assert_eq!(session.emit_diagnostic(), StatusCode::Ok);
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);

        let log = session.callback_log();
        assert_eq!(
            log.events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                EventKind::Action,
                EventKind::FocusChanged,
                EventKind::LayoutStateChanged,
                EventKind::ModelRangeRequest,
                EventKind::ModelChildrenRequest,
                EventKind::Diagnostic,
                EventKind::SurfaceClosed,
            ]
        );
    }
    #[test]
    fn callbacks_reject_foreign_requests_and_invalid_callback_values() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Callback validation");
        let (_, request) = session.start_model_request(surface);
        let context = (&*session.client as *const ClientContext).cast_mut().cast();
        let invalid_value = ValueRef {
            handle: 0,
            type_name: view(b""),
            canonical_encoding: BytesView {
                data: ptr::null(),
                len: 0,
            },
        };
        assert_eq!(
            unsafe { client_complete_model_request(context, request, invalid_value) }.code,
            StatusCode::InvalidArgument,
        );
        assert_eq!(
            unsafe {
                client_fail_model_request(
                    context,
                    request,
                    Status {
                        code: StatusCode::Failed,
                        message: view(b"raw failure"),
                    },
                )
            }
            .code,
            StatusCode::InvalidArgument,
        );
        session.fail_next_model_callback();
        assert_eq!(
            unsafe {
                client_fail_model_request(context, u64::MAX, status(StatusCode::Failed, b"foreign"))
            }
            .code,
            StatusCode::InvalidArgument,
        );
        assert_eq!(
            unsafe {
                client_fail_model_request(context, request, status(StatusCode::Failed, b"failure"))
            }
            .code,
            StatusCode::Failed,
        );
        let mut metadata = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { client_read_action_metadata(context, 0xffff, &mut metadata) }.code,
            StatusCode::InvalidArgument,
        );
        let mut debug_json = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };

        assert_eq!(
            unsafe { client_read_value_debug_json(context, invalid_value, &mut debug_json) }.code,
            StatusCode::InvalidArgument,
        );
        assert!(session.callback_log().completions.is_empty());

        assert!(session.callback_log().failures.is_empty());

        assert_eq!(session.apply_model_rows(request), StatusCode::Ok);
        assert_eq!(
            unsafe { client_complete_model_request(context, request, empty_value()) }.code,
            StatusCode::NotFound,
        );
        assert_eq!(session.callback_log().completions, vec![request]);
    }
    #[test]
    fn callbacks_reject_events_for_cancelled_requests() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Cancelled event");
        let (model, request) = session.start_model_request(surface);
        let context = (&*session.client as *const ClientContext).cast_mut().cast();
        assert_eq!(session.poll(), StatusCode::Ok);
        let event_count = session.callback_log().events.len();
        assert_eq!(session.cancel_request(request), StatusCode::Ok);

        let range = RuntimeEvent {
            kind: EventKind::ModelRangeRequest,
            as_: RuntimeEventArgs {
                range_request: ModelRangeRequest {
                    request,
                    model,
                    start: 0,
                    count: 1,
                    sort_filter_token: view(b""),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &range) }.code,
            StatusCode::NotFound
        );
        let children = RuntimeEvent {
            kind: EventKind::ModelChildrenRequest,
            as_: RuntimeEventArgs {
                children_request: ModelChildrenRequest {
                    request,
                    model,
                    parent_key: empty_value(),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &children) }.code,
            StatusCode::NotFound
        );
        assert_eq!(session.callback_log().events.len(), event_count);
    }
    #[test]
    fn model_callbacks_record_one_terminal_outcome_per_request() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Callback outcomes");
        let (_, completed) = session.start_model_request(surface);
        let (_, failed) = session.start_model_request(surface);
        assert_eq!(session.poll(), StatusCode::Ok);
        let context = (&*session.client as *const ClientContext).cast_mut().cast();

        assert_eq!(
            unsafe { client_complete_model_request(context, completed, empty_value()) }.code,
            StatusCode::Ok
        );
        assert_eq!(
            unsafe { client_complete_model_request(context, completed, empty_value()) }.code,
            StatusCode::NotFound
        );
        assert_eq!(
            unsafe {
                client_fail_model_request(context, failed, status(StatusCode::Failed, b"fixture"))
            }
            .code,
            StatusCode::Ok
        );
        assert_eq!(
            unsafe { client_complete_model_request(context, failed, empty_value()) }.code,
            StatusCode::NotFound
        );

        let log = session.callback_log();
        assert_eq!(log.completions, vec![completed]);
        assert_eq!(log.failures, vec![(failed, StatusCode::Failed)]);
        assert_eq!(log.sequence.len(), 4);
        assert_eq!(log.sequence[2].kind, CallbackKind::Completion(completed));
        assert_eq!(
            log.sequence[3].kind,
            CallbackKind::Failure(failed, StatusCode::Failed)
        );
        assert!(!log.terminal);
    }

    #[test]
    fn failed_surface_cancellation_retries_request_before_surface_close() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Failed cancellation");
        let (_, request) = session.start_model_request(surface);
        session.fail_next_model_callback();

        assert_eq!(session.destroy_surface(surface), StatusCode::Failed);
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);

        let log = session.callback_log();
        assert_eq!(log.failures, vec![(request, StatusCode::Cancelled)]);
        assert_eq!(
            log.events
                .iter()
                .filter(|event| event.kind == EventKind::SurfaceClosed)
                .count(),
            1
        );
    }

    #[test]
    fn failed_direct_cancellation_preserves_request_until_callback_succeeds() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Failed direct cancellation");
        let (_, request) = session.start_model_request(surface);
        session.fail_next_model_callback();

        assert_eq!(session.cancel_request(request), StatusCode::Failed);
        assert_eq!(session.cancel_request(request), StatusCode::Ok);
        assert_eq!(session.apply_model_rows(request), StatusCode::Cancelled);
        assert_eq!(
            session.callback_log().failures,
            vec![(request, StatusCode::Cancelled)]
        );
    }

    #[test]
    fn shutdown_retries_failed_request_cancellation_without_losing_request() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Shutdown retry");
        let (_, request) = session.start_model_request(surface);
        session.fail_next_model_callback();

        assert_eq!(session.shutdown(), StatusCode::Failed);
        let first = session.callback_log();
        assert!(first.failures.is_empty());
        assert!(!first.terminal);

        assert_eq!(session.shutdown(), StatusCode::Ok);
        let terminal = session.callback_log();
        assert_eq!(terminal.failures, vec![(request, StatusCode::Cancelled)]);
        assert_eq!(
            terminal.failures.iter().filter(|(id, _)| *id == request).count(),
            1
        );
        assert!(terminal.terminal);
        assert_eq!(
            terminal.sequence.last().map(|record| &record.kind),
            Some(&CallbackKind::Terminal)
        );

        let sequence = terminal.sequence.clone();
        assert_eq!(session.apply_model_rows(request), StatusCode::Failed);
        assert_eq!(session.callback_log().sequence, sequence);
    }

    #[test]
    fn destroying_a_surface_cancels_its_pending_requests_once() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Request ownership");
        let (_, request) = session.start_model_request(surface);
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
        assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
        assert_eq!(session.destroy_surface(surface), StatusCode::NotFound);
        assert_eq!(
            session.callback_log().failures,
            vec![(request, StatusCode::Cancelled)]
        );
    }

    #[test]
    fn destroying_a_surface_retires_all_owned_handles_and_suppresses_stale_work() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Owned handle retirement");
        let node_alias = next_unreserved_alias_handle();
        let action_alias = next_unreserved_alias_handle();
        let operations = [
            mount(node_alias, 0, view(b"root")),
            bind_action(
                node_alias,
                view(b"submit"),
                action_alias,
                view(b"std.json.Value"),
            ),
        ];
        assert_eq!(
            session.apply(surface, &batch(1, &operations)),
            StatusCode::Ok
        );
        let (node, action) = session.node_and_action(surface);
        let (model, request) = session.start_model_request(surface);
        let context = (&*session.client as *const ClientContext).cast_mut().cast();

        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        let after_destroy = session.callback_log();
        assert_eq!(
            after_destroy.failures,
            vec![(request, StatusCode::Cancelled)]
        );

        let stale_node_event = RuntimeEvent {
            kind: EventKind::FocusChanged,
            as_: RuntimeEventArgs {
                action: ActionEvent {
                    surface,
                    node,
                    action: 0,
                    payload: empty_value(),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &stale_node_event) }.code,
            StatusCode::NotFound
        );

        let mut metadata = OwnedBytes {
            data: ptr::null_mut(),
            len: 0,
            owner: ptr::null_mut(),
            release: release_owned,
        };
        assert_eq!(
            unsafe { client_read_action_metadata(context, action, &mut metadata) }.code,
            StatusCode::NotFound
        );
        assert!(metadata.data.is_null());
        assert_eq!(metadata.len, 0);

        let stale_model_event = RuntimeEvent {
            kind: EventKind::ModelRangeRequest,
            as_: RuntimeEventArgs {
                range_request: ModelRangeRequest {
                    request,
                    model,
                    start: 0,
                    count: 1,
                    sort_filter_token: view(b"fixture"),
                },
            },
        };
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &stale_model_event) }.code,
            StatusCode::NotFound
        );
        assert_eq!(session.apply_model_rows(request), StatusCode::NotFound);
        assert_eq!(session.cancel_request(request), StatusCode::NotFound);
        assert_eq!(
            unsafe { client_complete_model_request(context, request, empty_value()) }.code,
            StatusCode::NotFound
        );
        assert_eq!(session.capture_result(surface), Err(StatusCode::NotFound));
        assert_eq!(session.callback_log(), after_destroy);

        assert_eq!(session.shutdown(), StatusCode::Ok);
        assert!(session.callback_log().terminal);
    }

    #[test]
    fn cancellation_wins_late_completion_and_shutdown_is_terminal() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Shutdown");
        let (_, cancelled) = session.start_model_request(surface);
        assert_eq!(session.cancel_request(cancelled), StatusCode::Ok);
        assert_eq!(session.cancel_request(cancelled), StatusCode::Cancelled);
        assert_eq!(session.apply_model_rows(cancelled), StatusCode::Cancelled);
        let (_, pending) = session.start_model_request(surface);
        assert_eq!(session.shutdown(), StatusCode::Ok);
        assert_eq!(session.cancel_request(cancelled), StatusCode::Failed);
        assert_eq!(session.apply_model_rows(cancelled), StatusCode::Failed);
        assert_eq!(session.apply_model_rows(pending), StatusCode::Failed);
        assert_eq!(session.apply_model_rows(pending), StatusCode::Failed);
        assert_eq!(
            unsafe { (FIXTURE_API.poll_event_loop)(session.runtime, 0) }.code,
            StatusCode::Failed
        );
        let log = session.callback_log();
        assert_eq!(
            log.failures,
            vec![
                (cancelled, StatusCode::Cancelled),
                (pending, StatusCode::Cancelled)
            ]
        );
        assert!(
            log.events.iter().any(|event| {
                event.kind == EventKind::SurfaceClosed && event.surface == surface
            })
        );
        assert!(log.terminal);
        assert_eq!(
            log.sequence.last().map(|record| &record.kind),
            Some(&CallbackKind::Terminal)
        );
        assert!(log.sequence.last().is_some_and(|record| record.terminal));
        assert!(
            log.sequence[..log.sequence.len() - 1]
                .iter()
                .all(|record| !record.terminal)
        );
        let context = (&*session.client as *const ClientContext).cast_mut().cast();
        let post_terminal = RuntimeEvent {
            kind: EventKind::Diagnostic,
            as_: RuntimeEventArgs {
                diagnostic: DiagnosticEvent {
                    status: status(StatusCode::Failed, b"fixture diagnostic"),
                },
            },
        };
        let sequence = log.sequence.clone();
        assert_eq!(
            unsafe { client_emit_runtime_event(context, session.runtime, &post_terminal) }.code,
            StatusCode::Failed
        );
        assert_eq!(session.callback_log().sequence, sequence);
        assert_eq!(session.emit_diagnostic(), StatusCode::Failed);
        assert_eq!(session.callback_log().events, log.events);
    }

    #[test]
    fn typed_diagnostic_and_surface_events_keep_provenance() {
        let session = FixtureSession::new();
        let surface = session.create_surface(b"Typed events");
        assert_eq!(session.emit_diagnostic(), StatusCode::Ok);
        assert_eq!(session.destroy_surface(surface), StatusCode::Ok);
        let log = session.callback_log();
        assert_eq!(
            log.events[0],
            EventRecord {
                kind: EventKind::Diagnostic,
                surface: 0,
                request: 0,
            }
        );
        assert_eq!(
            log.events[1],
            EventRecord {
                kind: EventKind::SurfaceClosed,
                surface,
                request: 0,
            }
        );
    }
}
