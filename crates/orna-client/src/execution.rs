use super::*;

mod action;
mod inspect;
mod ui;
mod validation;

use action::evaluate_action_operation;
#[cfg(test)]
use action::{ClientActionNestedExecutor, action_target_result_type};
pub use action::{
    cancel_client_action_with_executor, complete_client_action, decode_action_payload,
    encode_action_payload, trigger_client_action,
};
pub(super) use inspect::inspect_invocation_target;
pub(crate) use inspect::stable_inspect_provider_error;
#[cfg(test)]
use inspect::{
    decode_inspect_carrier_payload, decode_inspect_snapshot_target_row,
    inspect_snapshot_target_from_envelope, inspect_target_is_observer,
};
use inspect::{
    evaluate_external_contract, evaluate_inspect_expression, inspect_carrier_value_matches,
    inspect_render_ui_value_matches, validate_inspect_render_contract,
};
#[cfg(test)]
use ui::decode_ui_constructor_body;
use ui::{evaluate_standard_ui_constructor, standard_ui_constructor_spec};
#[cfg(test)]
use validation::is_expression_reference_allowed;
pub use validation::validate_client_artifact_integrity;
pub(super) use validation::{
    ClientReturnShape, preflight_client_action_calls, preflight_client_control_flow_calls,
    preflight_client_expression_calls, preflight_client_inner_plan_calls,
    preflight_client_procedural_calls, preflight_client_state_calls, validate_artifact,
    validate_function_shape, validate_selected_references,
};
use validation::{
    client_call_target_is_referenced, validate_active_catalogue, validate_artifact_identity,
};

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

type ClientLocalEnvironment = HashMap<LocalId, ClientLocalBinding>;

#[derive(Clone, Debug)]
enum ClientLocalBinding {
    Value(RuntimeValue),
    StreamValue(RuntimeValue),
    Resource(ResourceOperationNode),
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
