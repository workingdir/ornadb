use super::*;

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

pub(super) fn active_resource_result_type_matches(
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

pub(super) fn inspect_invocation_target(value: &RuntimeValue) -> Option<InvocationId> {
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

pub(super) fn is_inspect_carrier_type(type_id: TypeId) -> bool {
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

pub(super) fn runtime_scalar_matches(scalar: StandardScalar, value: &RuntimeValue) -> bool {
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

pub(super) fn runtime_value_matches(
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
pub(super) fn active_type_is_known(
    active: &ActiveDatabaseRevision,
    resolved: ResolvedType,
) -> bool {
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

pub(super) fn active_supports_invocation_target(
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

pub(super) fn active_has_record_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active_type_matches(active, type_id, |definition| {
        definition.as_record_value().is_some()
    })
}

pub(super) fn active_has_enum_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
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

pub(super) fn active_has_object_type(active: &ActiveDatabaseRevision, type_id: TypeId) -> bool {
    active_type_matches(active, type_id, |definition| {
        definition.as_object().is_some()
    })
}

pub(super) fn expression_error(
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
pub(super) enum ClientReturnShape {
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
pub(super) fn validate_function_shape(
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
pub(super) fn validate_selected_references(
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
pub(super) fn preflight_client_expression_calls(
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
pub(super) fn preflight_client_state_calls(
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
pub(super) fn preflight_client_procedural_calls(
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
pub(super) fn preflight_client_control_flow_calls(
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
pub(super) fn preflight_client_action_calls(
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
pub(super) fn preflight_client_inner_plan_calls(
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
pub(super) fn validate_artifact(
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
#[path = "tests.rs"]
mod tests;
