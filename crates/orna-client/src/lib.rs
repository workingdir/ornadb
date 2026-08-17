//! Local evaluation for closed CLIENT functions.

use std::{collections::{HashMap, hash_map::Entry}, error::Error, fmt, hash::{Hash, Hasher}};

use orna_artifact::client_plan::{
    CAPABILITY_FORMAT_VERSION, CapabilityArgumentSource, CapabilityClientPlan, ClientExpressionNode,
    ClientPlan, ClientPlanError, EXPRESSION_FORMAT_VERSION, ExpressionClientPlan, FORMAT_IDENTITY,
    FORMAT_VERSION, InnerClientPlan, LANGUAGE_VERSION_IDENTITY, OPAQUE_FORMAT_VERSION,
    OpaqueClientPlan, STATE_FORMAT_VERSION, StateClientPlan, StateDefault, StateScope,
};
use orna_core::{
    FunctionId, FunctionRevisionId, ParameterId, PrincipalId, StateSlotId, TypeId,
    canonical_hash::{CanonicalHashError, catalogue_digest_with_context},
    catalogue::{
        FunctionDomain, FunctionReturn, FunctionSecurity, FunctionVolatility, ValueTypeKind,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest,
    },
    security::{AuthorisedInvocation, InvocationTarget},
    state::{
        UserStateCell, UserStateChange, UserStateKeyWithoutPrincipal, UserStateWriteOutcome,
        UserStateWriteResult,
    },
    types::{ResolvedType, StandardScalar},
    value::{FunctionArgument, OpaqueValue, OpaqueValueError, RuntimeValue},
};
use orna_standard::{RegisteredOpaqueCodecsError, registered_opaque_codecs};

pub mod capability;

/// The active revision and function revision selected for one CLIENT execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientExecutionContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
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
/// The cache identity for one CLIENT resource request.
///
/// All four components are part of the cache boundary. A resource result must
/// not cross a principal, pinned revision, argument set, or invalidation epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientResourceKey {
    target: InvocationTarget,
    principal: PrincipalId,
    arguments_digest: Sha256Digest,
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

    /// Returns the catalogue or data invalidation token.
    pub const fn invalidation_token(self) -> Sha256Digest {
        self.invalidation_token
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// A failure code is empty or contains a forbidden NUL byte.
    InvalidFailureCode,
    /// The result does not match the declared resolved type.
    TypeMismatch,
}

impl fmt::Display for ClientResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationExhausted => formatter.write_str("CLIENT resource generation exhausted"),
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "CLIENT resource completion has generation {}, expected {}",
                actual.value(),
                expected.value(),
            ),
            Self::InvalidTransition { status } => {
                write!(formatter, "CLIENT resource operation is invalid in {status:?} state")
            }
            Self::InvalidFailureCode => {
                formatter.write_str("CLIENT resource failure code must be non-empty and contain no NUL")
            }
            Self::TypeMismatch => formatter.write_str("CLIENT resource value has the wrong runtime type"),
        }
    }
}

impl Error for ClientResourceError {}

/// One typed CLIENT resource lifecycle owned by the local evaluator.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientResource {
    key: ClientResourceKey,
    expected_type: ResolvedType,
    generation: ClientResourceGeneration,
    status: ClientResourceStatus,
    value: Option<RuntimeValue>,
    failure: Option<ClientResourceFailure>,
}

impl ClientResource {
    /// Creates an idle resource with no published value.
    pub const fn new(key: ClientResourceKey, expected_type: ResolvedType) -> Self {
        Self {
            key,
            expected_type,
            generation: ClientResourceGeneration(0),
            status: ClientResourceStatus::Idle,
            value: None,
            failure: None,
        }
    }

    /// Returns the complete cache identity.
    pub const fn key(&self) -> ClientResourceKey {
        self.key
    }

    /// Returns the expected result type.
    pub const fn expected_type(&self) -> ResolvedType {
        self.expected_type
    }

    /// Returns the current request generation.
    pub const fn generation(&self) -> ClientResourceGeneration {
        self.generation
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
        self.status = ClientResourceStatus::Loading;
        self.clear_result();
        Ok(generation)
    }

    /// Publishes one type-checked result for the current generation.
    pub fn publish_ready(
        &mut self,
        active: &ActiveDatabaseRevision,
        generation: ClientResourceGeneration,
        value: RuntimeValue,
    ) -> Result<(), ClientResourceError> {
        self.require_loading(generation)?;
        if !runtime_value_matches(active, &value, self.expected_type) {
            return Err(ClientResourceError::TypeMismatch);
        }
        self.status = ClientResourceStatus::Ready;
        self.value = Some(value);
        self.failure = None;
        Ok(())
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
        self.clear_result();
        Ok(())
    }

    /// Invalidates the current generation and returns to `IDLE`.
    pub fn invalidate(&mut self) -> Result<(), ClientResourceError> {
        self.advance_generation()?;
        self.status = ClientResourceStatus::Idle;
        self.clear_result();
        Ok(())
    }

    fn clear_result(&mut self) {
        self.value = None;
        self.failure = None;
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
}

impl ClientStateContext {
    /// Creates one state context. Empty profile and instance values select
    /// their default identities.
    pub fn new(
        root_function: FunctionId,
        state_profile: String,
        instance_key: String,
    ) -> Result<Self, ClientStateIdentityError> {
        validate_state_text(&state_profile, "state profile")?;
        validate_state_text(&instance_key, "instance key")?;
        Ok(Self {
            root_function,
            state_profile,
            instance_key,
        })
    }

    /// Creates the default context for one root function.
    pub fn default_for(root_function: FunctionId) -> Self {
        Self {
            root_function,
            state_profile: String::new(),
            instance_key: String::new(),
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
    local: HashMap<ClientStateKey, RuntimeValue>,
    session: HashMap<ClientStateKey, RuntimeValue>,
    user: HashMap<ClientStateKey, ClientUserState>,
}

impl Default for ClientStateStore {
    fn default() -> Self {
        Self {
            context: ClientStateContext::default_for(FunctionId::from_bytes([0; 16])),
            local: HashMap::new(),
            session: HashMap::new(),
            user: HashMap::new(),
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

    /// Creates one state key in the selected root context.
    fn key_for(&self, function: FunctionId, slot: StateSlotId) -> ClientStateKey {
        ClientStateKey::from_context(&self.context, function, slot)
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
    /// A load contained the same logical cell more than once.
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
            Self::DuplicateKey(key) => write!(formatter, "USER state load contains duplicate key {key:?}"),
            Self::WriteBatchLength { expected, actual } => write!(
                formatter,
                "USER state write result count {actual} does not match change count {expected}",
            ),
            Self::WriteKeyMismatch { expected, actual } => {
                write!(formatter, "USER state write result key {actual:?} does not match {expected:?}")
            }
            Self::UnknownKey(key) => write!(formatter, "USER state write result names unknown key {key:?}"),
            Self::ValueMismatch(key) => {
                write!(formatter, "USER state write result does not match local value for {key:?}")
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
                write!(formatter, "USER state write returned an invalid revision for {key:?}")
            }
            Self::InvalidChange(reason) => write!(formatter, "USER state change is invalid: {reason}"),
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
    pub fn load_user_state(
        &mut self,
        cells: &[UserStateCell],
    ) -> Result<(), ClientUserStateError> {
        let mut loaded = HashMap::with_capacity(cells.len());
        for cell in cells {
            let key = ClientStateKey::from_user_cell(cell);
            if loaded.insert(key.clone(), ClientUserState::loaded(cell)).is_some() {
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
        for (change, result) in changes.iter().zip(results) {
            let expected_key = ClientStateKey::from_user_change(change);
            let actual_key = ClientStateKey::from_user_key(result.key());
            if expected_key != actual_key {
                return Err(ClientUserStateError::WriteKeyMismatch {
                    expected: expected_key,
                    actual: actual_key,
                });
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
        for (change, result) in changes.iter().zip(results) {
            let key = ClientStateKey::from_user_change(change);
            let local = self
                .user
                .get_mut(&key)
                .expect("USER state key was validated above");
            match result.outcome() {
                UserStateWriteOutcome::Written { revision } => {
                    let valid = revision > 0
                        && local.revision.is_none_or(|current| revision > current);
                    if !valid {
                        return Err(ClientUserStateError::InvalidRevision(key));
                    }
                    local.revision = Some(revision);
                    local.dirty = false;
                }
                UserStateWriteOutcome::Conflict { current_revision } => {
                    return Err(ClientUserStateError::Conflict {
                        key,
                        expected: change.expected_revision(),
                        current: current_revision,
                    });
                }
            }
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
            | Self::StateEvaluation { context, .. } => context.pair(),
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
            | Self::StateEvaluation { context, .. } => context.function(),
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
            | Self::StateEvaluation { context, .. } => Some(context),
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

/// Evaluates one CLIENT function in an explicit root state context.
pub fn evaluate_client_function_in_state_context(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    state_context: &ClientStateContext,
    arguments: &[FunctionArgument],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
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
    let result = evaluate_function(
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
    )?;
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
    };
    // A version-5 capability envelope is decoded before function-shape
    // validation (work ADR 0060). Its inner plan version classifies the
    // function, and its stored requirements gate evaluation; the caller's
    // declaration list never replaces them. Any decode failure fails closed
    // with the invalid-artifact path before anything executes.
    let envelope = if revision.artifact().version() == CAPABILITY_FORMAT_VERSION {
        Some(
            CapabilityClientPlan::decode(revision.artifact().payload()).map_err(|source| {
                ClientExecutionError::InvalidArtifact { context, source }
            })?,
        )
    } else {
        None
    };
    let artifact_version = envelope
        .as_ref()
        .map_or(revision.artifact().version(), |plan| plan.inner_plan_version());
    let resolve_parameter = |parameter: &str| {
        resolve_parameter_argument(definition, &arguments, parameter)
    };
    match &envelope {
        Some(plan) => {
            for requirement in plan.requirements() {
                let name = capability::LocalCapabilityName::parse(requirement.name()).map_err(
                    |_| ClientExecutionError::CapabilityDenied {
                        context,
                        capability: requirement.name().to_owned(),
                    },
                )?;
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
            for declaration in declarations {
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
    let value = match &envelope {
        Some(plan) => evaluate_capability_plan(
            active,
            plan,
            context,
            return_shape,
            &arguments,
            grants,
            state,
            depth,
        )?,
        None => evaluate_plan(
            active,
            revision.artifact().payload(),
            context,
            return_shape,
            &arguments,
            declarations,
            grants,
            state,
            depth,
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

fn evaluate_plan(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
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
        ClientReturnShape::Expression(expected) => {
            let plan = ExpressionClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            evaluate_expression_plan(
                active,
                plan.expression(),
                context,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
            )
        }
        ClientReturnShape::State(expected) => {
            let plan = StateClientPlan::decode(payload)
                .map_err(|source| ClientExecutionError::InvalidArtifact { context, source })?;
            evaluate_state_plan(
                active,
                &plan,
                context,
                expected,
                arguments,
                declarations,
                grants,
                state,
                depth,
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
    let value = OpaqueValue::new(active, &registry, expected, plan.canonical_payload())
        .map_err(|source| ClientExecutionError::InvalidOpaqueValue {
            context,
            source: ClientOpaqueValueError::Value(source),
        })?;
    Ok(RuntimeValue::Opaque(value))
}

/// Evaluates one decoded expression tree and type-checks its value.
fn evaluate_expression_plan(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
) -> Result<RuntimeValue, ClientExecutionError> {
    let value = evaluate_expression(
        active,
        expression,
        context,
        arguments,
        declarations,
        grants,
        state,
        depth,
    )?;
    if runtime_value_matches(active, &value, expected) {
        Ok(value)
    } else {
        Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ))
    }
}

/// Evaluates one decoded version-4 state plan after initialising its slots.
fn evaluate_state_plan(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
    expected: ResolvedType,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
) -> Result<RuntimeValue, ClientExecutionError> {
    initialize_client_state(
        active,
        plan,
        context,
        arguments,
        declarations,
        grants,
        state,
        depth,
    )?;
    evaluate_expression_plan(
        active,
        plan.expression(),
        context,
        expected,
        arguments,
        declarations,
        grants,
        state,
        depth,
    )
}

/// Evaluates one decoded version-5 capability envelope after its stored
/// requirements passed the capability gate (work ADR 0060).
///
/// The envelope's requirements are the only capability gate for version-5
/// plans: the caller's declaration list is not consulted, so a recursive
/// CLIENT call validates its own stored requirements instead of inheriting
/// the parent declaration list.
fn evaluate_capability_plan(
    active: &ActiveDatabaseRevision,
    plan: &CapabilityClientPlan,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
    arguments: &[(ParameterId, RuntimeValue)],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
) -> Result<RuntimeValue, ClientExecutionError> {
    match plan.inner_plan() {
        InnerClientPlan::Boolean(inner) => Ok(RuntimeValue::Boolean(inner.returned_boolean())),
        InnerClientPlan::Opaque(inner) => {
            let ClientReturnShape::Opaque(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_opaque_plan(active, inner, context, expected)
        }
        InnerClientPlan::Expression(inner) => {
            let ClientReturnShape::Expression(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_expression_plan(
                active,
                inner.expression(),
                context,
                expected,
                arguments,
                &[],
                grants,
                state,
                depth,
            )
        }
        InnerClientPlan::State(inner) => {
            let ClientReturnShape::State(expected) = return_shape else {
                unreachable!("function shape was validated against the inner plan version");
            };
            evaluate_state_plan(
                active,
                inner,
                context,
                expected,
                arguments,
                &[],
                grants,
                state,
                depth,
            )
        }
    }
}

fn evaluate_expression(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
) -> Result<RuntimeValue, ClientExecutionError> {
    match expression {
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
        ClientExpressionNode::FieldPath { root, fields } => {
            let value = arguments
                .iter()
                .find(|(candidate, _)| candidate == root)
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    expression_error(context, ClientExpressionError::ParameterNotBound)
                })?;
            evaluate_field_path(active, value, fields, context)
        }
        ClientExpressionNode::Concat { left, right } => {
            let left = evaluate_expression(
                active,
                left,
                context,
                arguments,
                declarations,
                grants,
                state,
                depth,
            )?;
            let right = evaluate_expression(
                active,
                right,
                context,
                arguments,
                declarations,
                grants,
                state,
                depth,
            )?;
            let (RuntimeValue::Text(left), RuntimeValue::Text(right)) = (left, right) else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            };
            Ok(RuntimeValue::Text(format!("{left}{right}")))
        }
        ClientExpressionNode::Call {
            function,
            arguments: bound,
        } => {
            if depth >= orna_artifact::client_plan::MAX_EXPRESSION_DEPTH {
                return Err(expression_error(
                    context,
                    ClientExpressionError::RecursionLimit,
                ));
            }
            let mut evaluated = Vec::with_capacity(bound.len());
            for (parameter, expression) in bound {
                if evaluated
                    .iter()
                    .any(|(candidate, _)| candidate == parameter)
                {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::InvalidCall,
                    ));
                }
                let value = evaluate_expression(
                    active,
                    expression,
                    context,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
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
            )?;
            Ok(value)
        }
        ClientExpressionNode::ExternalContract { identity } => {
            Err(ClientExecutionError::ExternalContract {
                context,
                identity: identity.clone(),
            })
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

fn runtime_value_matches(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: ResolvedType,
) -> bool {
    if let RuntimeValue::Null(null) = value {
        return null.resolved_type() == expected;
    }
    let scalar_matches = |scalar| match (scalar, value) {
        (StandardScalar::Boolean, RuntimeValue::Boolean(_))
        | (StandardScalar::Integer, RuntimeValue::Integer(_))
        | (StandardScalar::CharacterLargeObject, RuntimeValue::Text(_)) => true,
        _ => false,
    };
    match expected {
        ResolvedType::Scalar(scalar) => scalar_matches(scalar),
        ResolvedType::Value(type_id) => {
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
                "orna.kernel.value.character-large-object@1" => {
                    scalar_matches(StandardScalar::CharacterLargeObject)
                }
                _ => false,
            }
        }
        ResolvedType::Named(type_id) => {
            matches!(value, RuntimeValue::Record(record) if record.record_type() == type_id)
        }
        ResolvedType::Reference { target } => {
            matches!(value, RuntimeValue::Reference { target: actual, .. } if *actual == target)
        }
    }
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
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
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
        if stored_value.is_some_and(|value| !runtime_value_matches(active, value, resolved)) {
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
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
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
    if definition.kind() == ValueTypeKind::Opaque
        || matches!(
            definition.representation_contract(),
            "orna.kernel.value.boolean@1"
                | "orna.kernel.value.integer@1"
                | "orna.kernel.value.character-large-object@1"
        )
    {
        Some(ResolvedType::value(type_id))
    } else {
        None
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientReturnShape {
    LegacyBoolean,
    StandardBoolean(TypeId),
    Opaque(TypeId),
    Expression(ResolvedType),
    State(ResolvedType),
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
        EXPRESSION_FORMAT_VERSION | STATE_FORMAT_VERSION
    );
    let expression_shape = |resolved_type: ResolvedType| {
        if artifact_version == STATE_FORMAT_VERSION {
            ClientReturnShape::State(resolved_type)
        } else {
            ClientReturnShape::Expression(resolved_type)
        }
    };
    let FunctionReturn::Single(resolved_type) = return_type else {
        return ClientReturnShape::Unsupported;
    };
    if let Some(scalar) = resolved_type.legacy_scalar() {
        return if scalar == StandardScalar::Boolean {
            if expression_eligible {
                expression_shape(*resolved_type)
            } else {
                ClientReturnShape::LegacyBoolean
            }
        } else if expression_eligible
            && matches!(
                scalar,
                StandardScalar::Integer | StandardScalar::CharacterLargeObject
            )
        {
            expression_shape(*resolved_type)
        } else {
            ClientReturnShape::Unsupported
        };
    }
    if resolved_type.reference_target().is_some() || resolved_type.named_type().is_some() {
        return ClientReturnShape::Unsupported;
    }
    if let Some(type_id) = resolved_type.value_type() {
        let Some(definition) = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
        else {
            return ClientReturnShape::Unsupported;
        };
        if definition.representation_contract() == "orna.kernel.value.boolean@1" {
            return if expression_eligible {
                expression_shape(*resolved_type)
            } else {
                ClientReturnShape::StandardBoolean(type_id)
            };
        }
        if definition.kind() == ValueTypeKind::Opaque {
            return if expression_eligible {
                expression_shape(*resolved_type)
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
            return expression_shape(*resolved_type);
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
        EXPRESSION_FORMAT_VERSION | STATE_FORMAT_VERSION
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
                ClientReturnShape::Expression(_) | ClientReturnShape::State(_)
            ) {
                if selected.iter().any(|reference| {
                    !matches!(
                        reference.kind(),
                        DefinitionReferenceKind::FunctionCall
                            | DefinitionReferenceKind::NamedType
                            | DefinitionReferenceKind::ParameterRead
                            | DefinitionReferenceKind::QueryField
                            | DefinitionReferenceKind::Expression
                    )
                }) {
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
                            ClientReturnShape::Expression(_)
                            | ClientReturnShape::State(_)
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
        ClientReturnShape::Expression(_) => EXPRESSION_FORMAT_VERSION,
        ClientReturnShape::State(_) => STATE_FORMAT_VERSION,
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

fn invalid_function(
    context: ClientExecutionContext,
    rule: ClientExecutionRule,
) -> ClientExecutionError {
    ClientExecutionError::InvalidFunction { context, rule }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use orna_core::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId, ParameterId, PrincipalId, SchemaId,
        SourceBundleId, SourceRevisionId, SourceUnitId, StateSlotId, TypeId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
            function_declaration_digest, function_semantic_digest,
            function_semantic_digest_with_version, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionReturnColumnDefinition, FunctionSecurity, FunctionVolatility,
            ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            DefinitionIdentity, DefinitionOrigin, DefinitionReference, DefinitionReferenceKind,
            DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
            ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion,
            RevisionInvariantError, RevisionPair, Sha256Digest, SourceOrigin, StoredSourceRevision,
            StoredSourceUnit,
        },
        security::{
            AuthorisedInvocation, ExecuteDecision, ExecuteGrant, InvocationTarget, Principal,
            PrincipalKind, PrincipalStatus, SecuritySnapshot,
        },
        source::{SourceBundle, SourceUnit},
        state::{
            UserStateCell, UserStateKey, UserStateWriteOutcome, UserStateWriteResult,
        },
        types::{ResolvedType, StandardScalar},
        value::RuntimeValue,
    };

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
    fn client_resource_lifecycle_rejects_stale_and_invalid_results() {
        let (active, function, pair, _) = version_one_active(true);
        let principal = PrincipalId::from_bytes([0x7a; 16]);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            principal,
            Sha256Digest::from_bytes([0x11; 32]),
            Sha256Digest::from_bytes([0x22; 32]),
        );
        let mut resource = super::ClientResource::new(
            key,
            ResolvedType::Scalar(StandardScalar::Boolean),
        );

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
    fn client_resource_ready_value_must_match_declared_type() {
        let (active, function, pair, _) = version_one_active(true);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x31; 32]),
            Sha256Digest::from_bytes([0x32; 32]),
        );
        let mut resource = super::ClientResource::new(
            key,
            ResolvedType::Scalar(StandardScalar::Boolean),
        );
        let generation = resource.begin_loading().unwrap();

        assert_eq!(
            resource.publish_ready(&active, generation, RuntimeValue::Integer(4)),
            Err(super::ClientResourceError::TypeMismatch),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
        assert_eq!(resource.value(), None);
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
    fn version_four_unsupported_slot_type_fails_closed() {
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x41; 16]),
                orna_standard::BIGINT_TYPE_ID,
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
            } if context.function() == function
                && *slot == StateSlotId::from_bytes([0x41; 16])
        ));
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
            super::capability::LocalCapabilityName::ALL.into_iter().map(|name| {
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
        let caller_name = base.catalogue().function_by_id(caller_id).unwrap().name().clone();
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
                orna_artifact::client_plan::CapabilityArgumentSource::Text(
                    "/home/bob".to_owned(),
                ),
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
                orna_artifact::client_plan::CapabilityArgumentSource::Text(
                    "/home/bob".to_owned(),
                ),
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
        let write_only = super::capability::LocalCapabilityGrantSet::from_grants([write_grant])
            .unwrap();
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
        assert_eq!(
            error.context().copied(),
            Some(super::ClientExecutionContext {
                pair,
                function,
                function_revision,
            })
        );
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
        assert_references_rule(
            evaluate_client_function(
                &active_from_prepared_with_references(&prepared, wrong_ordinal),
                function,
            ),
            function,
        );

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
            assert_eq!(
                error.context().copied(),
                Some(super::ClientExecutionContext {
                    pair,
                    function,
                    function_revision,
                }),
                "{name}"
            );
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
    }

    fn active_from_prepared_with_references(
        prepared: &DeployableRevision,
        references: Vec<DefinitionReference>,
    ) -> ActiveDatabaseRevision {
        active_from_prepared_with_current_revisions(prepared, references, |revision| {
            revision.semantic_hash_version()
        })
    }

    fn active_from_prepared_with_current_revisions(
        prepared: &DeployableRevision,
        references: Vec<DefinitionReference>,
        semantic_hash_version: impl Fn(&FunctionRevisionRecord) -> FunctionSemanticHashVersion,
    ) -> ActiveDatabaseRevision {
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
                )
                .unwrap();
                FunctionRevisionRecord::new(
                    revision.function(),
                    revision.id(),
                    revision.revision_number(),
                    revision.declaration_origin(),
                    revision.declaration_content_hash(),
                    semantic_hash,
                    revision.language_version(),
                    revision.artifact().clone(),
                )
                .unwrap()
                .with_semantic_hash_version(version)
            })
            .collect::<Vec<_>>();
        let context = prepared.catalogue_hash_context().clone();
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            prepared.candidate(),
            &current_function_revisions,
            prepared.expressions(),
            prepared.origins(),
            &references,
        )
        .unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
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
        )
        .unwrap()
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

    fn version_two_value_active(
        return_type: TypeId,
        reference_target: TypeId,
    ) -> (
        ActiveDatabaseRevision,
        FunctionId,
        RevisionPair,
        FunctionRevisionId,
    ) {
        version_two_value_active_with_artifact(
            return_type,
            reference_target,
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
        version_two_value_active_with_artifact(
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
            orna_standard::OPAQUE_TOKEN_TYPE_ID,
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
            artifact_version,
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let reference = DefinitionReference::new(
            function_id,
            function_revision_id,
            0,
            DefinitionReferenceTarget::ValueType(reference_target),
            DefinitionReferenceKind::NamedType,
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

    fn version_five_boolean_active(payload: Vec<u8>) -> (
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

    fn version_five_expression_active_with_parameter(payload: Vec<u8>) -> (
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
        let mut origins = version_one.origins().to_vec();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: function_id,
                parameter: parameter.id(),
            },
            prior_revision.declaration_origin(),
        ));
        let context = orna_core::revision::CatalogueHashContext::version_two(standard);
        let catalogue_hash = catalogue_digest_with_context(
            &context,
            &catalogue,
            std::slice::from_ref(&revision),
            version_one.expressions(),
            &origins,
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
                    origins,
                    Vec::new(),
                ),
            ),
            context,
        )
        .unwrap();

        (active, function_id, pair, function_revision_id, parameter.id())
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
}
