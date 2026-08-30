use std::fmt;

use crate::artifact_codec::DecodeError;

use orna_core::{LocalId, ParameterId, StateSlotId, TypeId};

/// An error returned for an invalid or unsupported client-plan artefact.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientPlanError {
    /// The artefact does not start with the shared client-plan magic bytes.
    InvalidMagic,
    /// The artefact version is not supported.
    UnsupportedVersion(u32),
    /// The operation tag is not defined by the selected artefact version.
    InvalidOperation(u8),
    /// The Boolean payload is not zero or one.
    InvalidBoolean(u8),
    /// A version-2 opaque payload length exceeds the client-plan size bound.
    InvalidOpaquePayloadLength {
        /// The non-canonical length from the artefact.
        actual: u32,
    },
    /// A version-3 expression node uses an unknown tag.
    InvalidExpressionNode(u8),
    /// A version-10 unary operator uses an unknown tag.
    InvalidControlFlowUnaryOperator(u8),
    /// A version-10 binary operator uses an unknown tag.
    InvalidControlFlowBinaryOperator(u8),
    /// A version-10 statement uses an unknown tag.
    InvalidControlFlowStatement(u8),
    /// A version-10 return expression marker uses an unknown tag.
    InvalidControlFlowReturnTag(u8),
    /// A version-10 `IF` else-body marker uses an unknown tag.
    InvalidControlFlowElseTag(u8),
    /// A version-10 `IF` statement contains no branches.
    InvalidControlFlowBranchCount {
        /// The non-canonical branch count from the artefact.
        actual: u32,
    },
    /// A version-10 plan exceeds its local declaration limit.
    ControlFlowLocalLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A version-10 block or plan exceeds its statement limit.
    ControlFlowStatementLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A version-10 `IF` statement exceeds its branch limit.
    ControlFlowBranchLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A version-10 block nesting depth exceeds the format limit.
    ControlFlowBlockDepthExceeded,
    /// A version-10 plan repeats one local identity.
    DuplicateControlFlowLocal(LocalId),
    /// A version-10 statement or expression reads an undeclared local.
    UnknownControlFlowLocal(LocalId),
    /// A version-10 expression reads a local before its initializing LET.
    ControlFlowLocalReadBeforeLet(LocalId),
    /// A version-10 assignment targets a local before its initializing LET.
    ControlFlowAssignmentBeforeLet(LocalId),
    /// A version-10 local has more than one initializing LET.
    DuplicateControlFlowLet(LocalId),
    /// A version-10 local has no initializing LET statement.
    MissingControlFlowLet(LocalId),
    /// A version-10 initializer does not match its local's kind.
    ControlFlowLocalKindMismatch(LocalId),

    /// An Inspector operation uses an unknown operation tag.
    InvalidInspectOperation(u8),
    /// Inspector snapshot options are outside the structural-only v1 contract.
    UnsupportedInspectOptions,
    /// An Inspector projection uses an unknown projection tag.
    InvalidInspectProjection(u8),
    /// Version nine must contain at least one Inspector node.
    InvalidInspectPlan,
    /// A version-3 expression tree exceeds the depth cap.
    ExpressionDepthExceeded,
    /// A version-3 expression tree exceeds the node-count cap.
    ExpressionNodeCountExceeded,
    /// A version-3 call or field path exceeds its per-node cap.
    ExpressionCollectionExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// An action operation uses an unknown CLIENT/SERVER domain tag.
    InvalidActionDomain(u8),
    /// An action operation carries an empty stable identity.
    InvalidActionIdentity,
    /// An action operation exceeds its argument limit.
    ActionArgumentLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// An action argument parameter identity occurs more than once.
    DuplicateActionArgument(ParameterId),
    /// Action arguments are not sorted by ascending ParameterId.
    NonCanonicalActionArgumentOrder,
    /// A resource operation uses an unknown scalar/stream tag.
    InvalidResourceKind(u8),
    /// A resource operation or argument carries an empty stable identity.
    InvalidResourceIdentity,
    /// A version-6 plan contains no resource operation nodes.
    InvalidResourceOperationCount {
        /// The non-canonical count from the artefact.
        actual: u32,
    },
    /// A version-6 plan exceeds its resource operation limit.
    ResourceOperationLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A resource operation exceeds its argument limit.
    ResourceArgumentLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A procedural local declaration uses an unknown value/resource kind.
    InvalidLocalKind(u8),
    /// A procedural statement uses an unknown tag.
    InvalidProceduralStatement(u8),
    /// A procedural plan exceeds its local declaration limit.
    ProceduralLocalLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A procedural plan exceeds its ordered statement limit.
    ProceduralStatementLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A procedural plan repeats one local identity.
    DuplicateProceduralLocal(LocalId),
    /// A statement or expression reads a local not declared by the plan.
    UnknownProceduralLocal(LocalId),
    /// A procedural expression reads a declared local before its LET statement.
    ProceduralLocalReadBeforeLet(LocalId),
    /// A statement initializer does not match its local's resource/value kind.
    ProceduralLocalKindMismatch(LocalId),
    /// A direct resource initializer result type does not match its local declaration.
    ProceduralLocalTypeMismatch {
        /// The target local identity.
        local: LocalId,
        /// The local declaration's type identity.
        expected: TypeId,
        /// The resource operation's result type identity.
        actual: TypeId,
    },
    /// A resource local is read where a value is required without AWAIT.
    UnawaitedResourceLocal(LocalId),
    /// A local has more than one LET statement.
    DuplicateProceduralLet(LocalId),
    /// An assignment appears before the target local's LET statement.
    ProceduralAssignmentBeforeLet(LocalId),
    /// A declared local has no LET statement.
    MissingProceduralLet(LocalId),
    /// An AWAIT reads a local that is not a resource handle.
    InvalidAwaitOperand(LocalId),
    /// A resource argument parameter identity occurs more than once.
    DuplicateResourceArgument(ParameterId),
    /// Resource arguments are not sorted by ascending ParameterId.
    NonCanonicalResourceArgumentOrder,
    /// A version-4 state slot uses an unknown scope tag.
    InvalidStateScope(u8),
    /// A version-4 state slot uses an unknown default tag.
    InvalidStateDefaultTag(u8),
    /// A version-4 plan declares zero state slots.
    InvalidStateSlotCount {
        /// The non-canonical count from the artefact.
        actual: u32,
    },
    /// A version-4 plan declares more state slots than the format allows.
    StateSlotLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A version-4 plan repeats one state-slot identity.
    DuplicateStateSlotId(StateSlotId),
    /// A version-5 plan declares zero capability requirements.
    InvalidCapabilityCount {
        /// The non-canonical count from the artefact.
        actual: u32,
    },
    /// A version-5 plan declares more requirements than the format allows.
    CapabilityLimitExceeded {
        /// The exceeded limit.
        limit: usize,
    },
    /// A version-5 plan repeats one capability requirement name.
    DuplicateCapabilityName(String),
    /// A version-5 capability name is empty.
    EmptyCapabilityName,
    /// A version-5 capability argument is empty.
    EmptyCapabilityArgument,
    /// A version-5 capability name exceeds the length limit.
    CapabilityNameTooLong {
        /// The offending name length.
        length: usize,
        /// The largest accepted name length.
        limit: usize,
    },
    /// A version-5 capability argument exceeds the length limit.
    CapabilityArgumentTooLong {
        /// The offending argument length.
        length: usize,
        /// The largest accepted argument length.
        limit: usize,
    },
    /// A version-5 capability name is not valid UTF-8.
    InvalidCapabilityNameUtf8,
    /// A version-5 capability argument is not valid UTF-8.
    InvalidCapabilityArgumentUtf8,
    /// A version-5 capability argument uses an unknown tag.
    InvalidCapabilityArgumentTag(u8),
    /// A version-5 envelope carries an unsupported inner plan version.
    UnsupportedInnerVersion(u32),
    /// A version-5 envelope declares a version that does not match its payload.
    InnerVersionMismatch {
        /// The version declared by the envelope.
        declared: u32,
        /// The canonical version decoded from the payload.
        actual: u32,
    },
    /// The encoded artefact exceeds the format byte limit.
    ArtifactSizeLimit {
        /// The supplied artefact size.
        size: usize,
        /// The largest accepted artefact size.
        maximum: usize,
    },
    /// The artefact ends before a complete value can be read.
    Truncated,
    /// The artefact contains bytes after a complete value.
    TrailingBytes,
}

impl From<DecodeError> for ClientPlanError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Truncated => Self::Truncated,
            DecodeError::TrailingBytes => Self::TrailingBytes,
        }
    }
}

impl fmt::Display for ClientPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("invalid orna.client-plan artefact magic"),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.client-plan artefact version {version}"
            ),
            Self::InvalidOperation(tag) => {
                write!(formatter, "invalid client-plan operation tag {tag}")
            }
            Self::InvalidBoolean(value) => {
                write!(formatter, "invalid client-plan Boolean byte {value}")
            }
            Self::InvalidOpaquePayloadLength { actual } => write!(
                formatter,
                "invalid client-plan opaque payload length {actual}"
            ),
            Self::InvalidExpressionNode(tag) => {
                write!(formatter, "invalid client-plan expression node tag {tag}")
            }
            Self::InvalidControlFlowUnaryOperator(tag) => write!(
                formatter,
                "invalid client-plan control-flow unary operator tag {tag}"
            ),
            Self::InvalidControlFlowBinaryOperator(tag) => write!(
                formatter,
                "invalid client-plan control-flow binary operator tag {tag}"
            ),
            Self::InvalidControlFlowStatement(tag) => write!(
                formatter,
                "invalid client-plan control-flow statement tag {tag}"
            ),
            Self::InvalidControlFlowReturnTag(tag) => write!(
                formatter,
                "invalid client-plan control-flow return tag {tag}"
            ),
            Self::InvalidControlFlowElseTag(tag) => write!(
                formatter,
                "invalid client-plan control-flow else-body tag {tag}"
            ),
            Self::InvalidControlFlowBranchCount { actual } => write!(
                formatter,
                "invalid client-plan control-flow branch count {actual}; an IF requires at least one branch"
            ),
            Self::ControlFlowLocalLimitExceeded { limit } => write!(
                formatter,
                "client-plan control-flow local count exceeds the limit {limit}"
            ),
            Self::ControlFlowStatementLimitExceeded { limit } => write!(
                formatter,
                "client-plan control-flow statement count exceeds the limit {limit}"
            ),
            Self::ControlFlowBranchLimitExceeded { limit } => write!(
                formatter,
                "client-plan control-flow branch count exceeds the limit {limit}"
            ),
            Self::ControlFlowBlockDepthExceeded => {
                formatter.write_str("client-plan control-flow block nesting exceeds the depth cap")
            }
            Self::DuplicateControlFlowLocal(local) => {
                write!(
                    formatter,
                    "duplicate client-plan control-flow local {local}"
                )
            }
            Self::UnknownControlFlowLocal(local) => {
                write!(formatter, "unknown client-plan control-flow local {local}")
            }
            Self::ControlFlowLocalReadBeforeLet(local) => write!(
                formatter,
                "client-plan control-flow local {local} is read before LET"
            ),
            Self::ControlFlowAssignmentBeforeLet(local) => write!(
                formatter,
                "client-plan control-flow assignment targets local {local} before LET"
            ),
            Self::DuplicateControlFlowLet(local) => {
                write!(
                    formatter,
                    "duplicate LET for client-plan control-flow local {local}"
                )
            }
            Self::MissingControlFlowLet(local) => {
                write!(
                    formatter,
                    "client-plan control-flow local {local} has no LET statement"
                )
            }
            Self::ControlFlowLocalKindMismatch(local) => write!(
                formatter,
                "client-plan control-flow local {local} has an incompatible initializer kind"
            ),
            Self::InvalidInspectOperation(tag) => {
                write!(
                    formatter,
                    "invalid client-plan Inspector operation tag {tag}"
                )
            }
            Self::UnsupportedInspectOptions => formatter
                .write_str("typed client-plan Inspector snapshot options are unsupported in v1"),
            Self::InvalidInspectProjection(tag) => {
                write!(
                    formatter,
                    "invalid client-plan Inspector projection tag {tag}"
                )
            }
            Self::InvalidInspectPlan => {
                formatter.write_str("version-nine client plan contains no Inspector node")
            }
            Self::ExpressionDepthExceeded => {
                formatter.write_str("client-plan expression tree exceeds the depth cap")
            }
            Self::ExpressionNodeCountExceeded => {
                formatter.write_str("client-plan expression tree exceeds the node-count cap")
            }
            Self::ExpressionCollectionExceeded { limit } => write!(
                formatter,
                "client-plan expression collection exceeds the limit {limit}"
            ),
            Self::InvalidActionDomain(tag) => {
                write!(formatter, "invalid client-plan action domain tag {tag}")
            }
            Self::InvalidActionIdentity => {
                formatter.write_str("invalid client-plan action identity")
            }
            Self::ActionArgumentLimitExceeded { limit } => write!(
                formatter,
                "client-plan action argument count exceeds the limit {limit}"
            ),
            Self::DuplicateActionArgument(parameter) => {
                write!(
                    formatter,
                    "duplicate client-plan action argument {parameter}"
                )
            }
            Self::NonCanonicalActionArgumentOrder => formatter
                .write_str("client-plan action arguments are not in canonical ParameterId order"),
            Self::InvalidResourceKind(tag) => {
                write!(formatter, "invalid client-plan resource kind tag {tag}")
            }
            Self::InvalidResourceIdentity => {
                formatter.write_str("invalid client-plan resource identity")
            }
            Self::InvalidResourceOperationCount { actual } => write!(
                formatter,
                "invalid client-plan resource operation count {actual}; a resource plan requires at least one operation"
            ),
            Self::ResourceOperationLimitExceeded { limit } => write!(
                formatter,
                "client-plan resource operation count exceeds the limit {limit}"
            ),
            Self::ResourceArgumentLimitExceeded { limit } => write!(
                formatter,
                "client-plan resource argument count exceeds the limit {limit}"
            ),
            Self::InvalidLocalKind(tag) => write!(
                formatter,
                "invalid client-plan procedural local kind tag {tag}"
            ),
            Self::InvalidProceduralStatement(tag) => write!(
                formatter,
                "invalid client-plan procedural statement tag {tag}"
            ),
            Self::ProceduralLocalLimitExceeded { limit } => write!(
                formatter,
                "client-plan procedural local count exceeds the limit {limit}"
            ),
            Self::ProceduralStatementLimitExceeded { limit } => write!(
                formatter,
                "client-plan procedural statement count exceeds the limit {limit}"
            ),
            Self::DuplicateProceduralLocal(local) => {
                write!(formatter, "duplicate client-plan procedural local {local}")
            }
            Self::UnknownProceduralLocal(local) => {
                write!(formatter, "unknown client-plan procedural local {local}")
            }
            Self::ProceduralLocalReadBeforeLet(local) => write!(
                formatter,
                "client-plan procedural local {local} is read before LET"
            ),
            Self::ProceduralLocalKindMismatch(local) => write!(
                formatter,
                "client-plan procedural local {local} has an incompatible initializer kind"
            ),
            Self::ProceduralLocalTypeMismatch {
                local,
                expected,
                actual,
            } => write!(
                formatter,
                "client-plan procedural local {local} expects resource type {expected}, got {actual}"
            ),
            Self::UnawaitedResourceLocal(local) => write!(
                formatter,
                "client-plan resource local {local} must be awaited before use as a value"
            ),
            Self::DuplicateProceduralLet(local) => {
                write!(
                    formatter,
                    "duplicate LET for client-plan procedural local {local}"
                )
            }
            Self::ProceduralAssignmentBeforeLet(local) => write!(
                formatter,
                "client-plan procedural assignment targets local {local} before LET"
            ),
            Self::MissingProceduralLet(local) => {
                write!(
                    formatter,
                    "client-plan procedural local {local} has no LET statement"
                )
            }
            Self::InvalidAwaitOperand(local) => write!(
                formatter,
                "client-plan AWAIT operand local {local} is not a resource"
            ),
            Self::DuplicateResourceArgument(parameter) => {
                write!(
                    formatter,
                    "duplicate client-plan resource argument {parameter}"
                )
            }
            Self::NonCanonicalResourceArgumentOrder => formatter
                .write_str("client-plan resource arguments are not in canonical ParameterId order"),
            Self::InvalidStateScope(tag) => {
                write!(formatter, "invalid client-plan state scope tag {tag}")
            }
            Self::InvalidStateDefaultTag(tag) => {
                write!(formatter, "invalid client-plan state default tag {tag}")
            }
            Self::InvalidStateSlotCount { actual } => write!(
                formatter,
                "invalid client-plan state slot count {actual}; a state plan requires at least one slot"
            ),
            Self::StateSlotLimitExceeded { limit } => {
                write!(
                    formatter,
                    "client-plan state slot count exceeds the limit {limit}"
                )
            }
            Self::DuplicateStateSlotId(id) => {
                write!(formatter, "duplicate client-plan state slot identity {id}")
            }
            Self::InvalidCapabilityCount { actual } => write!(
                formatter,
                "invalid client-plan capability count {actual}; a capability plan requires at least one requirement"
            ),
            Self::CapabilityLimitExceeded { limit } => write!(
                formatter,
                "client-plan capability count exceeds the limit {limit}"
            ),
            Self::DuplicateCapabilityName(name) => write!(
                formatter,
                "duplicate client-plan capability requirement {name}"
            ),
            Self::EmptyCapabilityName => {
                formatter.write_str("client-plan capability name must not be empty")
            }
            Self::EmptyCapabilityArgument => {
                formatter.write_str("client-plan capability argument must not be empty")
            }
            Self::CapabilityNameTooLong { length, limit } => write!(
                formatter,
                "client-plan capability name length {length} exceeds the limit {limit}"
            ),
            Self::CapabilityArgumentTooLong { length, limit } => write!(
                formatter,
                "client-plan capability argument length {length} exceeds the limit {limit}"
            ),
            Self::InvalidCapabilityNameUtf8 => {
                formatter.write_str("client-plan capability name is not valid UTF-8")
            }
            Self::InvalidCapabilityArgumentUtf8 => {
                formatter.write_str("client-plan capability argument is not valid UTF-8")
            }
            Self::InvalidCapabilityArgumentTag(tag) => {
                write!(
                    formatter,
                    "invalid client-plan capability argument tag {tag}"
                )
            }
            Self::UnsupportedInnerVersion(version) => {
                write!(formatter, "unsupported inner client-plan version {version}")
            }
            Self::InnerVersionMismatch { declared, actual } => write!(
                formatter,
                "declared inner client-plan version {declared} does not match payload version {actual}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "orna.client-plan artefact size {size} exceeds the limit {maximum}"
            ),
            Self::Truncated => formatter.write_str("truncated orna.client-plan artefact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.client-plan artefact")
            }
        }
    }
}

impl std::error::Error for ClientPlanError {}
