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
