//! Canonical `orna.client-plan` artefact formats, versions 1 through 10.
//!
//! The first CLIENT function slice has one operation: return a non-null
//! BOOLEAN constant. The complete encoding is:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 1
//! operation: u8 = 1
//! value: u8 = 0|1
//! ```
//!
//! Version 2 returns one registered opaque value:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 2
//! operation: u8 = 2
//! type: TypeId[16]
//! payload length: u32 big-endian
//! canonical payload[payload length]
//! ```
//!
//! Version 3 (work ADR 0068) returns one closed CLIENT expression tree:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 3
//! operation: u8 = 3
//! tree: canonical recursive node encoding
//! ```
//!
//! Version 4 (work ADR 0069) returns one closed CLIENT expression tree
//! followed by the checked state-slot declarations of the owning function:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 4
//! operation: u8 = 4
//! return tree: canonical recursive node encoding
//! slot count: u32 big-endian, 1..=MAX_STATE_SLOTS
//! slot: StateSlotId[16]
//!       TypeId[16]
//!       scope: u8 = 1|2|3 (LOCAL|SESSION|USER)
//!       default tag: u8 = 0|1|2 (unset|null|expression)
//!       [default tree: canonical recursive node encoding]
//! ```
//!
//! Each node starts with a tag byte followed by its exact payload. The node
//! set is closed: call, string, integer, Boolean, parameter read, field
//! path, concatenation, and external contract. Decoding enforces the closed
//! shape, the depth cap, the node-count cap, and per-node limits, so an
//! untrusted artefact cannot exhaust the evaluator.
//!
//! Version 5 (work ADR 0060) wraps one complete version 1-4, version 6,
//! version 7, version 8, version 9, or version 10 plan
//! with the owning function's ordered, closed capability requirements:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 5
//! operation: u8 = 5
//! inner plan version: u32 big-endian = 1|2|3|4|6|7|8|9|10
//! inner payload length: u32 big-endian
//! inner payload: complete inner client-plan artefact bytes
//! capability count: u32 big-endian, 1..=MAX_CAPABILITY_REQUIREMENTS
//! capability: name length: u32 big-endian, 1..=MAX_CAPABILITY_NAME_LENGTH
//!             name: UTF-8
//!             argument tag: u8 = 1|2 (text|parameter)
//!             argument length: u32 big-endian, 1..=MAX_CAPABILITY_ARGUMENT_LENGTH
//!             argument: UTF-8
//! ```
//!
//! The inner payload is validated as a complete plan of the declared
//! version, so a decoded version-5 plan holds only checked inner plans and
//! checked requirements. Each requirement mirrors the closed
//! `CapabilitySpecification` form (a qualified name plus exactly one text
//! or parameter argument source); names and argument sources travel as
//! plain text so the envelope stays independent of the client grant model.
//!
//! Version 6 (work ADR 0077) carries one closed tree containing resource
//! operations. A resource operation is encoded as: kind `u8` (`1` scalar,
//! `2` stream), target `FunctionId[16]`, source and catalogue revision IDs
//! (`16` bytes each), `CallSiteId[16]`, argument count `u32`, ascending
//! `ParameterId[16]` plus expression pairs, and declared result `TypeId[16]`.
//! `AWAIT` is tag `9` and must directly wrap a resource node (tag `10`).
//! Resource plans reject a bare resource, an await over another expression,
//! and any trailing bytes.
//!
//! Version 7 carries a procedural CLIENT body:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 7
//! operation: u8 = 7
//! local count: u32 big-endian, 0..=MAX_PROCEDURAL_LOCALS
//! local: LocalId[16] TypeId[16] kind: u8 (0 value, 1 scalar resource, 2 stream resource)
//! statement count: u32 big-endian, 0..=MAX_PROCEDURAL_STATEMENTS
//! statement: tag u8 (1 LET, 2 ASSIGNMENT), LocalId[16], expression node
//! final return expression node
//! ```
//!
//! Procedural expressions additionally admit `LocalRead` nodes, the v6
//! resource/`AWAIT` nodes, and sealed Inspector nodes; versions 1-6 reject
//! `LocalRead` and Inspector nodes as unknown nodes.
//!
//! Version 8 carries one checked CLIENT action operation. Its descriptor is:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 8
//! operation: u8 = 8
//! domain: u8 = 1|2 (CLIENT|SERVER)
//! target: FunctionId[16]
//! target revision: SourceRevisionId[16] CatalogueRevisionId[16]
//! call site: CallSiteId[16]
//! result type: TypeId[16]
//! argument count: u32 big-endian, 0..=MAX_ACTION_ARGUMENTS
//! argument: ParameterId[16] expression node
//! ```
//!
//! Action arguments are encoded with the closed expression node set. Action
//! nodes cannot nest inside action arguments, and arguments are ordered by
//! ascending `ParameterId`.
//!
//! Version 9 carries an expression tree containing one or more sealed Inspector
//! nodes. Its header uses operation tag 9; the Inspector node tag is 13. The
//! node payload is operation tag 1 plus a target expression and an options
//! marker (0 for the structural-only default) for `Snapshot`, or operation
//! tag 2 plus projection tag 1..=8 and a snapshot expression for `Projection`.
//! Typed options markers are rejected because Inspector v1 has no canonical
//! snapshot-options payload decoder. Version 3 remains the format for trees
//! without Inspector nodes, and rejects the sealed node tag.
//!
//! Version 10 (work ADR 0084) carries a bounded programmable CLIENT block:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 10
//! operation: u8 = 10
//! local count: u32 big-endian, 0..=MAX_CONTROL_FLOW_LOCALS
//! local: LocalId[16] TypeId[16] kind: u8 (0 value, 1 scalar resource,
//!       2 stream resource)
//! block:
//!   statement count: u32 big-endian, 0..=MAX_CONTROL_FLOW_STATEMENTS
//!   statement:
//!     tag u8 = 1 LET, 2 ASSIGNMENT, 3 RETURN, 4 IF, 5 WHILE
//!     LET/ASSIGNMENT: LocalId[16] expression node
//!     RETURN: expression marker u8 = 0 (none) or 1, then expression node
//!     IF: branch count u32, 1..=MAX_CONTROL_FLOW_BRANCHES; each branch is
//!         condition expression node followed by a block; then else marker
//!         u8 = 0 (none) or 1 followed by a block
//!     WHILE: condition expression node followed by a block
//! ```
//!
//! Version-10 expression nodes retain the closed leaves, calls, resource,
//! action, Inspector, and local-read nodes above. They append unary and binary
//! nodes after the legacy node tags: unary is tag 14 followed by operator tag
//! 1 `PLUS`, 2 `MINUS`, or 3 `NOT`, then one operand; binary is tag 15 followed
//! by operator tag 1 `ADD`, 2 `SUBTRACT`, 3 `MULTIPLY`, 4 `DIVIDE`, 5
//! `MODULO`, 6 `EQUAL`, 7 `NOT_EQUAL`, 8 `LESS_THAN`, 9 `GREATER_THAN`, 10
//! `LESS_THAN_OR_EQUAL`, 11 `GREATER_THAN_OR_EQUAL`, 12 `AND`, or 13 `OR`,
//! then the left and right operands. All lengths and recursive values are
//! bounded before allocation. Versions 1 through 9 retain their exact node
//! sets and reject tags 14 and 15 as unknown nodes.
//!
//! A version-10 block is a sequence, not an implicit final expression:
//! `RETURN` carries an optional expression and exits the current function.
//! Blocks and branch bodies preserve source order. The version-10 decoder
//! requires canonical local identities, validates local references, rejects
//! malformed tags, enforces block/branch/depth/node/size limits, and requires
//! the input to end immediately after the root block.
//!
//! The version 1-4 formats contain no source text, source locations, Orna
//! names, or backend values.

use std::fmt;

use orna_core::{
    CallSiteId, CatalogueRevisionId, FieldId, FunctionId, LocalId, ParameterId, SourceRevisionId,
    StateSlotId, TypeId, revision::RevisionPair,
};

use crate::artifact_codec::{DecodeError, Reader};

/// The stable public identity of this artefact format.
pub const FORMAT_IDENTITY: &str = "orna.client-plan";
/// The Orna language version whose semantics this artefact executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The client-plan version that returns one Boolean constant.
pub const FORMAT_VERSION: u32 = 1;
/// The client-plan version that returns one registered opaque value.
pub const OPAQUE_FORMAT_VERSION: u32 = 2;
/// The client-plan version that returns one closed CLIENT expression tree.
pub const EXPRESSION_FORMAT_VERSION: u32 = 3;
/// The client-plan version that returns one closed CLIENT expression tree
/// and the owning function's checked state-slot declarations.
pub const STATE_FORMAT_VERSION: u32 = 4;
/// The exact first eight bytes of every client-plan artefact.
pub const MAGIC: [u8; 8] = *b"ORNACP\0\0";
/// The maximum accepted encoded artefact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
/// The maximum expression tree depth accepted by the decoder.
pub const MAX_EXPRESSION_DEPTH: usize = 64;
/// The maximum number of expression nodes in one tree.
pub const MAX_EXPRESSION_NODES: usize = 1024;
/// The maximum number of arguments in one call expression.
pub const MAX_CALL_ARGUMENTS: usize = 64;
/// The maximum number of fields in one field path.
pub const MAX_FIELD_PATH_LENGTH: usize = 64;
/// The maximum number of state slots in one state client plan.
pub const MAX_STATE_SLOTS: usize = 64;
/// The client-plan version that carries one inner plan and the owning
/// function's ordered, closed capability requirements (work ADR 0060).
pub const CAPABILITY_FORMAT_VERSION: u32 = 5;
/// The client-plan version that carries a closed expression tree with checked
/// CLIENT-to-SERVER resource operation nodes (work ADR 0077).
pub const RESOURCE_FORMAT_VERSION: u32 = 6;
/// The client-plan version that carries ordered procedural local declarations,
/// statements, and one final return expression (work ADR 0077).
pub const PROCEDURAL_FORMAT_VERSION: u32 = 7;
/// The client-plan version that carries one checked CLIENT action operation.
pub const ACTION_FORMAT_VERSION: u32 = 8;
/// The client-plan version that carries closed Inspector expression nodes.
pub const INSPECT_FORMAT_VERSION: u32 = 9;
/// The client-plan version that carries bounded programmable CLIENT control
/// flow and explicit return statements (work ADR 0084).
pub const CONTROL_FLOW_FORMAT_VERSION: u32 = 10;
/// Alias for [`CONTROL_FLOW_FORMAT_VERSION`] used by programmable CLIENT
/// callers.
pub const PROGRAMMABLE_FORMAT_VERSION: u32 = CONTROL_FLOW_FORMAT_VERSION;
/// The maximum number of ordered local declarations in a version-10 plan.
pub const MAX_CONTROL_FLOW_LOCALS: usize = 64;
/// The maximum number of statements directly contained by one version-10
/// block.
pub const MAX_CONTROL_FLOW_STATEMENTS: usize = 256;
/// The maximum number of IF/ELSIF branches in one version-10 IF statement.
pub const MAX_CONTROL_FLOW_BRANCHES: usize = 64;
/// The maximum nested block depth in a version-10 plan.
pub const MAX_CONTROL_FLOW_BLOCK_DEPTH: usize = MAX_EXPRESSION_DEPTH;

/// The maximum number of resource operation nodes in one resource plan.
pub const MAX_RESOURCE_OPERATIONS: usize = 64;
/// The maximum number of arguments in one resource operation.
pub const MAX_RESOURCE_ARGUMENTS: usize = 64;
/// The maximum number of arguments in one action operation.
pub const MAX_ACTION_ARGUMENTS: usize = 64;
/// The maximum number of local declarations in one procedural plan.
pub const MAX_PROCEDURAL_LOCALS: usize = 64;
/// The maximum number of ordered statements in one procedural plan.
pub const MAX_PROCEDURAL_STATEMENTS: usize = 256;
/// The maximum number of capability requirements in one capability plan.
pub const MAX_CAPABILITY_REQUIREMENTS: usize = 64;
/// The maximum encoded length of one capability requirement name.
pub const MAX_CAPABILITY_NAME_LENGTH: usize = 256;
/// The maximum encoded length of one capability requirement argument.
pub const MAX_CAPABILITY_ARGUMENT_LENGTH: usize = 1024;

const RETURN_BOOLEAN_OPERATION: u8 = 1;
const RETURN_OPAQUE_OPERATION: u8 = 2;
const RETURN_EXPRESSION_OPERATION: u8 = 3;
const RETURN_STATE_OPERATION: u8 = 4;
const RETURN_CAPABILITY_OPERATION: u8 = 5;
const RETURN_RESOURCE_OPERATION: u8 = 6;
const RETURN_PROCEDURAL_OPERATION: u8 = 7;
const RETURN_ACTION_OPERATION: u8 = 8;
const RETURN_CONTROL_FLOW_OPERATION: u8 = 10;

const RETURN_INSPECT_OPERATION: u8 = 9;
const CAPABILITY_ARGUMENT_TEXT: u8 = 1;
const CAPABILITY_ARGUMENT_PARAMETER: u8 = 2;
const RESOURCE_KIND_SCALAR: u8 = 1;
const RESOURCE_KIND_STREAM: u8 = 2;
const NODE_AWAIT: u8 = 9;
const NODE_RESOURCE: u8 = 10;
const ENCODED_LENGTH: usize = MAGIC.len() + size_of::<u32>() + 2;
const OPAQUE_FIXED_LENGTH: usize = MAGIC.len() + size_of::<u32>() + 1 + 16 + size_of::<u32>();
#[cfg(test)]
const OPAQUE_PAYLOAD_LENGTH: usize = 16;
#[cfg(test)]
const OPAQUE_ENCODED_LENGTH: usize = OPAQUE_FIXED_LENGTH + OPAQUE_PAYLOAD_LENGTH;

const NODE_CALL: u8 = 1;
const NODE_STRING: u8 = 2;
const NODE_INTEGER: u8 = 3;
const NODE_BOOLEAN: u8 = 4;
const NODE_PARAMETER_READ: u8 = 5;
const NODE_FIELD_PATH: u8 = 6;
const NODE_CONCAT: u8 = 7;
const NODE_EXTERNAL_CONTRACT: u8 = 8;
const NODE_LOCAL_READ: u8 = 11;
const NODE_ACTION: u8 = 12;
const NODE_INSPECT: u8 = 13;
const NODE_UNARY: u8 = 14;
const NODE_BINARY: u8 = 15;
const NODE_SOURCE_INTROSPECTION: u8 = 16;
const NODE_INPUT: u8 = 17;
const NODE_EVALUATE: u8 = 18;

const INSPECT_OPERATION_SNAPSHOT: u8 = 1;
const INSPECT_OPERATION_PROJECTION: u8 = 2;

const CONTROL_FLOW_STATEMENT_LET: u8 = 1;
const CONTROL_FLOW_STATEMENT_ASSIGNMENT: u8 = 2;
const CONTROL_FLOW_STATEMENT_RETURN: u8 = 3;
const CONTROL_FLOW_STATEMENT_IF: u8 = 4;
const CONTROL_FLOW_STATEMENT_WHILE: u8 = 5;
const CONTROL_FLOW_RETURN_NONE: u8 = 0;
const CONTROL_FLOW_RETURN_EXPRESSION: u8 = 1;
const CONTROL_FLOW_ELSE_NONE: u8 = 0;
const CONTROL_FLOW_ELSE_BODY: u8 = 1;

const PROCEDURAL_STATEMENT_LET: u8 = 1;
const PROCEDURAL_STATEMENT_ASSIGNMENT: u8 = 2;
const LOCAL_KIND_VALUE: u8 = 0;
const LOCAL_KIND_RESOURCE_SCALAR: u8 = 1;
const LOCAL_KIND_RESOURCE_STREAM: u8 = 2;

const STATE_SCOPE_LOCAL: u8 = 1;
const STATE_SCOPE_SESSION: u8 = 2;
const STATE_SCOPE_USER: u8 = 3;

const STATE_DEFAULT_UNSET: u8 = 0;
const STATE_DEFAULT_NULL: u8 = 1;
const STATE_DEFAULT_EXPRESSION: u8 = 2;

/// A checked CLIENT plan that returns one Boolean constant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientPlan {
    returned_boolean: bool,
}

/// A checked version-2 CLIENT plan that returns one registered opaque value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueClientPlan {
    opaque_type: TypeId,
    canonical_payload: Vec<u8>,
}

impl OpaqueClientPlan {
    /// Creates a checked plan from one nominal type and complete canonical payload.
    pub fn return_opaque<P: Into<Vec<u8>>>(opaque_type: TypeId, canonical_payload: P) -> Self {
        Self {
            opaque_type,
            canonical_payload: canonical_payload.into(),
        }
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        OPAQUE_FORMAT_VERSION
    }

    /// Returns the nominal opaque value-type identity.
    pub const fn opaque_type(&self) -> TypeId {
        self.opaque_type
    }

    /// Returns the complete canonical opaque payload.
    pub fn canonical_payload(&self) -> &[u8] {
        &self.canonical_payload
    }

    /// Encodes this plan into its exact version-2 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        let encoded_length = OPAQUE_FIXED_LENGTH.saturating_add(self.canonical_payload.len());
        if encoded_length > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: encoded_length,
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(encoded_length);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_OPAQUE_OPERATION);
        bytes.extend_from_slice(&self.opaque_type.to_bytes());
        bytes.extend_from_slice(&(self.canonical_payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.canonical_payload);
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-2 opaque client-plan artefact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != OPAQUE_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_OPAQUE_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let opaque_type = TypeId::from_bytes(reader.array()?);
        let payload_length = reader.u32()?;
        if payload_length as usize > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::InvalidOpaquePayloadLength {
                actual: payload_length,
            });
        }
        let canonical_payload = reader.take(payload_length as usize)?.to_vec();
        reader.require_finished()?;
        Ok(Self::return_opaque(opaque_type, canonical_payload))
    }
}

impl ClientPlan {
    /// Creates a checked plan that returns `value`.
    pub const fn return_boolean(value: bool) -> Self {
        Self {
            returned_boolean: value,
        }
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        FORMAT_VERSION
    }

    /// Returns the Boolean value carried by this plan.
    pub const fn returned_boolean(&self) -> bool {
        self.returned_boolean
    }

    /// Encodes this plan into its canonical version-1 bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ENCODED_LENGTH);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_BOOLEAN_OPERATION);
        bytes.push(u8::from(self.returned_boolean));
        debug_assert_eq!(bytes.len(), ENCODED_LENGTH);
        bytes
    }

    /// Decodes exactly one canonical version-1 client-plan artefact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_BOOLEAN_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let value = match reader.u8()? {
            0 => false,
            1 => true,
            value => return Err(ClientPlanError::InvalidBoolean(value)),
        };
        reader.require_finished()?;
        Ok(Self::return_boolean(value))
    }
}

/// A checked version-3 CLIENT plan that returns one closed expression tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpressionClientPlan {
    expression: ClientExpressionNode,
    contains_inspect: bool,
}

/// One of the eight bounded Inspector projections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectProjection {
    InvocationNodes,
    Calls,
    Resources,
    StateCells,
    UiNodes,
    PresentationCandidates,
    RuntimeBindings,
    SecurityDecisions,
}

impl InspectProjection {
    const fn tag(self) -> u8 {
        match self {
            Self::InvocationNodes => 1,
            Self::Calls => 2,
            Self::Resources => 3,
            Self::StateCells => 4,
            Self::UiNodes => 5,
            Self::PresentationCandidates => 6,
            Self::RuntimeBindings => 7,
            Self::SecurityDecisions => 8,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ClientPlanError> {
        match tag {
            1 => Ok(Self::InvocationNodes),
            2 => Ok(Self::Calls),
            3 => Ok(Self::Resources),
            4 => Ok(Self::StateCells),
            5 => Ok(Self::UiNodes),
            6 => Ok(Self::PresentationCandidates),
            7 => Ok(Self::RuntimeBindings),
            8 => Ok(Self::SecurityDecisions),
            tag => Err(ClientPlanError::InvalidInspectProjection(tag)),
        }
    }
}

/// One sealed Inspector expression operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectOperationNode {
    Snapshot {
        target: Box<ClientExpressionNode>,
        /// The explicit structural-only snapshot default. Typed options are
        /// rejected by the Inspector v1 compiler and artifact codec.
        options: Option<Box<ClientExpressionNode>>,
    },
    Projection {
        projection: InspectProjection,
        snapshot: Box<ClientExpressionNode>,
    },
}

impl InspectOperationNode {
    pub fn snapshot(target: ClientExpressionNode) -> Self {
        Self::Snapshot {
            target: Box::new(target),
            options: None,
        }
    }

    pub fn projection(projection: InspectProjection, snapshot: ClientExpressionNode) -> Self {
        Self::Projection {
            projection,
            snapshot: Box::new(snapshot),
        }
    }

    pub const fn target(&self) -> Option<&ClientExpressionNode> {
        match self {
            Self::Snapshot { target, .. } => Some(target),
            Self::Projection { .. } => None,
        }
    }

    /// Returns the snapshot-options expression when present. Inspector v1
    /// plans use the structural-only default and therefore return None.
    pub fn options(&self) -> Option<&ClientExpressionNode> {
        match self {
            Self::Snapshot { options, .. } => match options {
                Some(options) => Some(options.as_ref()),
                None => None,
            },
            Self::Projection { .. } => None,
        }
    }

    /// Alias for the options accessor with an explicit carrier name.
    pub fn snapshot_options(&self) -> Option<&ClientExpressionNode> {
        self.options()
    }

    pub const fn projection_kind(&self) -> Option<InspectProjection> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Projection { projection, .. } => Some(*projection),
        }
    }

    pub const fn snapshot_expression(&self) -> Option<&ClientExpressionNode> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Projection { snapshot, .. } => Some(snapshot),
        }
    }
}

/// One unary operator in the version-10 programmable CLIENT expression
/// language. `Plus` and `Minus` operate on checked signed `INTEGER` values;
/// `Not` operates on a strict `BOOLEAN` value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFlowUnaryOperator {
    /// Unary plus; leaves an INTEGER operand unchanged.
    Plus,
    /// Checked unary negation of an INTEGER operand.
    Minus,
    /// Boolean negation of a BOOLEAN operand.
    Not,
}

impl ControlFlowUnaryOperator {
    const fn tag(self) -> u8 {
        match self {
            Self::Plus => 1,
            Self::Minus => 2,
            Self::Not => 3,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ClientPlanError> {
        match tag {
            1 => Ok(Self::Plus),
            2 => Ok(Self::Minus),
            3 => Ok(Self::Not),
            tag => Err(ClientPlanError::InvalidControlFlowUnaryOperator(tag)),
        }
    }

    /// Returns the canonical wire tag for this operator.
    pub const fn wire_tag(self) -> u8 {
        self.tag()
    }
}

/// One binary operator in the version-10 programmable CLIENT expression
/// language. Arithmetic is checked signed `INTEGER`; Boolean operators are
/// strict and short-circuit; comparisons require two values of one supported
/// scalar type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFlowBinaryOperator {
    /// Checked INTEGER addition.
    Add,
    /// Checked INTEGER subtraction.
    Subtract,
    /// Checked INTEGER multiplication.
    Multiply,
    /// Checked INTEGER division.
    Divide,
    /// Checked INTEGER remainder.
    Modulo,
    /// Equality comparison.
    Equal,
    /// Inequality comparison.
    NotEqual,
    /// Less-than comparison.
    LessThan,
    /// Greater-than comparison.
    GreaterThan,
    /// Less-than-or-equal comparison.
    LessThanOrEqual,
    /// Greater-than-or-equal comparison.
    GreaterThanOrEqual,
    /// Short-circuit Boolean conjunction.
    And,
    /// Short-circuit Boolean disjunction.
    Or,
}

impl ControlFlowBinaryOperator {
    const fn tag(self) -> u8 {
        match self {
            Self::Add => 1,
            Self::Subtract => 2,
            Self::Multiply => 3,
            Self::Divide => 4,
            Self::Modulo => 5,
            Self::Equal => 6,
            Self::NotEqual => 7,
            Self::LessThan => 8,
            Self::GreaterThan => 9,
            Self::LessThanOrEqual => 10,
            Self::GreaterThanOrEqual => 11,
            Self::And => 12,
            Self::Or => 13,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ClientPlanError> {
        match tag {
            1 => Ok(Self::Add),
            2 => Ok(Self::Subtract),
            3 => Ok(Self::Multiply),
            4 => Ok(Self::Divide),
            5 => Ok(Self::Modulo),
            6 => Ok(Self::Equal),
            7 => Ok(Self::NotEqual),
            8 => Ok(Self::LessThan),
            9 => Ok(Self::GreaterThan),
            10 => Ok(Self::LessThanOrEqual),
            11 => Ok(Self::GreaterThanOrEqual),
            12 => Ok(Self::And),
            13 => Ok(Self::Or),
            tag => Err(ClientPlanError::InvalidControlFlowBinaryOperator(tag)),
        }
    }

    /// Returns the canonical wire tag for this operator.
    pub const fn wire_tag(self) -> u8 {
        self.tag()
    }
}

/// One closed CLIENT expression-tree node (work ADR 0068).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientExpressionNode {
    /// A suspension expression that resumes with the resource value.
    Await {
        /// The resource expression being awaited.
        expression: Box<ClientExpressionNode>,
    },
    /// A checked CLIENT-to-SERVER resource operation value.
    Resource {
        /// The target, revision, call-site, arguments, and result type.
        operation: ResourceOperationNode,
    },
    /// A checked sealed Inspector operation value.
    Inspect {
        /// The snapshot or projection operation.
        operation: InspectOperationNode,
    },
    /// A checked CLIENT action operation value.
    Action {
        /// The target, revision, call-site, arguments, and result type.
        operation: ActionOperationNode,
    },
    /// A call to one CLIENT function with bound arguments.
    Call {
        /// The called function identity.
        function: FunctionId,
        /// The bound arguments in call order: parameter then value.
        arguments: Vec<(ParameterId, ClientExpressionNode)>,
    },
    /// A text literal value.
    String {
        /// The unescaped text value.
        value: String,
    },
    /// An integer literal value.
    Integer {
        /// The parsed integer value.
        value: i64,
    },
    /// A Boolean literal value.
    Boolean {
        /// The Boolean value.
        value: bool,
    },
    /// A read-only view of the enclosing function's source metadata.
    SourceIntrospection,
    /// Reads one bounded line from the active client session.
    Input,
    /// Evaluates one bounded CLI command through the active session.
    Evaluate {
        /// The command expression.
        expression: Box<ClientExpressionNode>,
    },
    ParameterRead {
        /// The read parameter identity.
        parameter: ParameterId,
    },
    /// A read of one declared procedural local.
    LocalRead {
        /// The stable local binding identity.
        local: LocalId,
    },
    /// A path from one parameter through object fields.
    FieldPath {
        /// The parameter at the start of the path.
        root: ParameterId,
        /// The fields selected in source order.
        fields: Vec<FieldId>,
    },
    /// A left-associative text concatenation.
    Concat {
        /// The left operand.
        left: Box<ClientExpressionNode>,
        /// The right operand.
        right: Box<ClientExpressionNode>,
    },
    /// An external function body declared only by its runtime contract.
    ExternalContract {
        /// The exact contract identity string.
        identity: String,
    },
    /// A version-10 unary operator expression.
    Unary {
        /// The checked operator.
        operator: ControlFlowUnaryOperator,
        /// The operand expression.
        expression: Box<ClientExpressionNode>,
    },
    /// A version-10 binary operator expression.
    Binary {
        /// The checked operator.
        operator: ControlFlowBinaryOperator,
        /// The left operand.
        left: Box<ClientExpressionNode>,
        /// The right operand.
        right: Box<ClientExpressionNode>,
    },
}

/// The expression node type used by the version-10 programmable CLIENT
/// format. Legacy leaves are reused so calls, resources, actions, Inspector
/// operations, and local reads retain their checked identities.
pub type ControlFlowExpression = ClientExpressionNode;

impl ExpressionClientPlan {
    /// Creates a checked plan from one closed expression tree.
    pub fn new(expression: ClientExpressionNode) -> Self {
        let contains_inspect = expression_contains_inspect(&expression);
        Self {
            expression,
            contains_inspect,
        }
    }
}

/// The kind of value held by one procedural CLIENT local.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientLocalKind {
    /// A normal eagerly evaluated local value.
    Value,
    /// A local resource handle whose result is scalar or stream shaped.
    Resource(ResourceKind),
}

impl ClientLocalKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Value => LOCAL_KIND_VALUE,
            Self::Resource(ResourceKind::Scalar) => LOCAL_KIND_RESOURCE_SCALAR,
            Self::Resource(ResourceKind::Stream) => LOCAL_KIND_RESOURCE_STREAM,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ClientPlanError> {
        match tag {
            LOCAL_KIND_VALUE => Ok(Self::Value),
            LOCAL_KIND_RESOURCE_SCALAR => Ok(Self::Resource(ResourceKind::Scalar)),
            LOCAL_KIND_RESOURCE_STREAM => Ok(Self::Resource(ResourceKind::Stream)),
            tag => Err(ClientPlanError::InvalidLocalKind(tag)),
        }
    }
}

/// One stable procedural CLIENT local declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientLocal {
    local: LocalId,
    type_id: TypeId,
    kind: ClientLocalKind,
}

impl ClientLocal {
    /// Creates a local declaration from its stable identity, resolved type, and kind.
    pub const fn new(local: LocalId, type_id: TypeId, kind: ClientLocalKind) -> Self {
        Self {
            local,
            type_id,
            kind,
        }
    }

    /// Returns the stable local identity.
    pub const fn local(&self) -> LocalId {
        self.local
    }

    /// Returns the stable local identity.
    pub const fn local_id(&self) -> LocalId {
        self.local
    }

    /// Returns the resolved local value type.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns whether this local stores a value or resource handle.
    pub const fn kind(&self) -> ClientLocalKind {
        self.kind
    }
}

/// One ordered procedural CLIENT statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientStatement {
    /// Declares and initialises a local binding.
    Let {
        local: LocalId,
        expression: ClientExpressionNode,
    },
    /// Replaces an existing local binding.
    Assignment {
        local: LocalId,
        expression: ClientExpressionNode,
    },
}

impl ClientStatement {
    /// Creates a LET statement.
    pub const fn let_(local: LocalId, expression: ClientExpressionNode) -> Self {
        Self::Let { local, expression }
    }

    /// Creates an assignment statement.
    pub const fn assignment(local: LocalId, expression: ClientExpressionNode) -> Self {
        Self::Assignment { local, expression }
    }

    /// Returns the target local identity.
    pub const fn local(&self) -> LocalId {
        match self {
            Self::Let { local, .. } | Self::Assignment { local, .. } => *local,
        }
    }

    /// Returns the statement expression.
    pub const fn expression(&self) -> &ClientExpressionNode {
        match self {
            Self::Let { expression, .. } | Self::Assignment { expression, .. } => expression,
        }
    }
}

/// A checked version-7 procedural CLIENT plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProceduralClientPlan {
    locals: Vec<ClientLocal>,
    statements: Vec<ClientStatement>,
    return_expression: ClientExpressionNode,
}

impl ProceduralClientPlan {
    /// Creates a procedural plan from declaration-order locals, ordered statements,
    /// and one final return expression.
    pub fn new(
        locals: Vec<ClientLocal>,
        statements: Vec<ClientStatement>,
        return_expression: ClientExpressionNode,
    ) -> Self {
        Self {
            locals,
            statements,
            return_expression,
        }
    }

    /// Returns local declarations in source order.
    pub fn locals(&self) -> &[ClientLocal] {
        &self.locals
    }

    /// Returns statements in source order.
    pub fn statements(&self) -> &[ClientStatement] {
        &self.statements
    }

    /// Returns the final return expression.
    pub const fn return_expression(&self) -> &ClientExpressionNode {
        &self.return_expression
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        PROCEDURAL_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-seven bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        validate_procedural_model(self)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&PROCEDURAL_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_PROCEDURAL_OPERATION);
        bytes.extend_from_slice(&(self.locals.len() as u32).to_be_bytes());
        for local in &self.locals {
            bytes.extend_from_slice(&local.local.to_bytes());
            bytes.extend_from_slice(&local.type_id.to_bytes());
            bytes.push(local.kind.tag());
        }
        bytes.extend_from_slice(&(self.statements.len() as u32).to_be_bytes());
        let mut writer = NodeWriter::new();
        let mut count = 0;
        let mut resource_count = 0;
        for statement in &self.statements {
            match statement {
                ClientStatement::Let { local, expression } => {
                    writer.push(PROCEDURAL_STATEMENT_LET);
                    writer.extend(&local.to_bytes());
                    encode_expression_node_with_resources(
                        expression,
                        &mut writer,
                        0,
                        &mut count,
                        true,
                        true,
                        true,
                        &mut resource_count,
                    )?;
                }
                ClientStatement::Assignment { local, expression } => {
                    writer.push(PROCEDURAL_STATEMENT_ASSIGNMENT);
                    writer.extend(&local.to_bytes());
                    encode_expression_node_with_resources(
                        expression,
                        &mut writer,
                        0,
                        &mut count,
                        true,
                        true,
                        true,
                        &mut resource_count,
                    )?;
                }
            }
        }
        encode_expression_node_with_resources(
            &self.return_expression,
            &mut writer,
            0,
            &mut count,
            true,
            true,
            true,
            &mut resource_count,
        )?;
        bytes.extend_from_slice(&writer.finish());
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-seven procedural client plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != PROCEDURAL_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_PROCEDURAL_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let local_count = reader.u32()? as usize;
        if local_count > MAX_PROCEDURAL_LOCALS {
            return Err(ClientPlanError::ProceduralLocalLimitExceeded {
                limit: MAX_PROCEDURAL_LOCALS,
            });
        }
        let mut locals = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            let local = LocalId::from_bytes(reader.array()?);
            if locals.iter().any(|item: &ClientLocal| item.local == local) {
                return Err(ClientPlanError::DuplicateProceduralLocal(local));
            }
            let type_id = TypeId::from_bytes(reader.array()?);
            let kind = ClientLocalKind::from_tag(reader.u8()?)?;
            locals.push(ClientLocal::new(local, type_id, kind));
        }
        let statement_count = reader.u32()? as usize;
        if statement_count > MAX_PROCEDURAL_STATEMENTS {
            return Err(ClientPlanError::ProceduralStatementLimitExceeded {
                limit: MAX_PROCEDURAL_STATEMENTS,
            });
        }
        let mut statements = Vec::with_capacity(statement_count);
        let mut count = 0;
        let mut resource_count = 0;
        for _ in 0..statement_count {
            let tag = reader.u8()?;
            let local = LocalId::from_bytes(reader.array()?);
            if !locals.iter().any(|item| item.local == local) {
                return Err(ClientPlanError::UnknownProceduralLocal(local));
            }
            let statement_kind = match tag {
                PROCEDURAL_STATEMENT_LET => PROCEDURAL_STATEMENT_LET,
                PROCEDURAL_STATEMENT_ASSIGNMENT => PROCEDURAL_STATEMENT_ASSIGNMENT,
                tag => return Err(ClientPlanError::InvalidProceduralStatement(tag)),
            };
            let expression = decode_expression_node_with_resources(
                &mut reader,
                0,
                &mut count,
                true,
                true,
                true,
                &mut resource_count,
            )?;
            let statement = match statement_kind {
                PROCEDURAL_STATEMENT_LET => ClientStatement::let_(local, expression),
                PROCEDURAL_STATEMENT_ASSIGNMENT => ClientStatement::assignment(local, expression),
                _ => unreachable!(),
            };
            statements.push(statement);
        }
        let return_expression = decode_expression_node_with_resources(
            &mut reader,
            0,
            &mut count,
            true,
            true,
            true,
            &mut resource_count,
        )?;
        reader.require_finished()?;
        let plan = Self::new(locals, statements, return_expression);
        validate_procedural_model(&plan)?;
        Ok(plan)
    }
}

// Compatibility aliases for callers that name the declaration/statement by role.
pub type LocalDeclaration = ClientLocal;
pub type LocalKind = ClientLocalKind;
pub type ProceduralStatement = ClientStatement;
pub type ClientProceduralStatement = ClientStatement;

/// One explicit version-10 `RETURN` statement. An absent expression is the
/// checked representation of `RETURN;`; a present expression is evaluated
/// before leaving the current CLIENT function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFlowReturnStatement {
    expression: Option<ControlFlowExpression>,
}

impl ControlFlowReturnStatement {
    /// Creates a return statement with an optional value expression.
    pub fn new(expression: Option<ControlFlowExpression>) -> Self {
        Self { expression }
    }

    /// Creates a value-less `RETURN;` statement.
    pub const fn empty() -> Self {
        Self { expression: None }
    }

    /// Returns the expression evaluated by this return, when present.
    pub fn expression(&self) -> Option<&ControlFlowExpression> {
        self.expression.as_ref()
    }
}

/// One ordered `IF` or `ELSIF` branch in a version-10 control-flow block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFlowIfBranch {
    condition: ControlFlowExpression,
    statements: Vec<ControlFlowStatement>,
}

impl ControlFlowIfBranch {
    /// Creates a branch from its strict Boolean condition and body.
    pub fn new(condition: ControlFlowExpression, statements: Vec<ControlFlowStatement>) -> Self {
        Self {
            condition,
            statements,
        }
    }

    /// Returns the branch condition.
    pub const fn condition(&self) -> &ControlFlowExpression {
        &self.condition
    }

    /// Returns the branch body in source order.
    pub fn statements(&self) -> &[ControlFlowStatement] {
        &self.statements
    }

    /// Alias for [`Self::statements`].
    pub fn body(&self) -> &[ControlFlowStatement] {
        self.statements()
    }
}

/// One version-10 `IF` statement with ordered branches and an optional
/// `ELSE` body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFlowIfStatement {
    branches: Vec<ControlFlowIfBranch>,
    else_statements: Option<Vec<ControlFlowStatement>>,
}

impl ControlFlowIfStatement {
    /// Creates an `IF` from one or more ordered branches and an optional else
    /// body.
    pub fn new(
        branches: Vec<ControlFlowIfBranch>,
        else_statements: Option<Vec<ControlFlowStatement>>,
    ) -> Self {
        Self {
            branches,
            else_statements,
        }
    }

    /// Returns the ordered `IF`/`ELSIF` branches.
    pub fn branches(&self) -> &[ControlFlowIfBranch] {
        &self.branches
    }

    /// Returns the optional `ELSE` body in source order.
    pub fn else_statements(&self) -> Option<&[ControlFlowStatement]> {
        self.else_statements.as_deref()
    }

    /// Alias for [`Self::else_statements`].
    pub fn else_body(&self) -> Option<&[ControlFlowStatement]> {
        self.else_statements()
    }
}

/// One version-10 `WHILE` statement with a strict Boolean condition and body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFlowWhileStatement {
    condition: ControlFlowExpression,
    statements: Vec<ControlFlowStatement>,
}

impl ControlFlowWhileStatement {
    /// Creates a `WHILE` from its condition and body.
    pub fn new(condition: ControlFlowExpression, statements: Vec<ControlFlowStatement>) -> Self {
        Self {
            condition,
            statements,
        }
    }

    /// Returns the loop condition.
    pub const fn condition(&self) -> &ControlFlowExpression {
        &self.condition
    }

    /// Returns the loop body in source order.
    pub fn statements(&self) -> &[ControlFlowStatement] {
        &self.statements
    }

    /// Alias for [`Self::statements`].
    pub fn body(&self) -> &[ControlFlowStatement] {
        self.statements()
    }
}

/// One explicit version-10 programmable CLIENT statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlFlowStatement {
    /// Declares and initialises one ordered local.
    Let {
        /// The declared local identity.
        local: LocalId,
        /// The initializer expression.
        expression: ControlFlowExpression,
    },
    /// Replaces one previously initialized local.
    Assignment {
        /// The target local identity.
        local: LocalId,
        /// The replacement expression.
        expression: ControlFlowExpression,
    },
    /// Exits the current CLIENT function.
    Return(ControlFlowReturnStatement),
    /// Selects the first true branch, or the optional else body.
    If(ControlFlowIfStatement),
    /// Repeats its body while the strict Boolean condition is true.
    While(ControlFlowWhileStatement),
}

impl ControlFlowStatement {
    /// Creates a `LET` statement.
    pub const fn let_(local: LocalId, expression: ControlFlowExpression) -> Self {
        Self::Let { local, expression }
    }

    /// Creates an assignment statement.
    pub const fn assignment(local: LocalId, expression: ControlFlowExpression) -> Self {
        Self::Assignment { local, expression }
    }

    /// Creates a value-bearing `RETURN` statement.
    pub fn return_(expression: Option<ControlFlowExpression>) -> Self {
        Self::Return(ControlFlowReturnStatement::new(expression))
    }
    /// Creates a value-bearing `RETURN` statement without an `Option` at the
    /// call site.
    pub fn return_value(expression: ControlFlowExpression) -> Self {
        Self::return_(Some(expression))
    }

    /// Creates a value-less `RETURN;` statement.
    pub const fn return_empty() -> Self {
        Self::Return(ControlFlowReturnStatement::empty())
    }

    /// Creates an `IF` statement.
    pub fn if_(statement: ControlFlowIfStatement) -> Self {
        Self::If(statement)
    }

    /// Creates a `WHILE` statement.
    pub fn while_(statement: ControlFlowWhileStatement) -> Self {
        Self::While(statement)
    }

    /// Returns the local target for a `LET` or assignment statement.
    pub const fn local(&self) -> Option<LocalId> {
        match self {
            Self::Let { local, .. } | Self::Assignment { local, .. } => Some(*local),
            Self::Return(_) | Self::If(_) | Self::While(_) => None,
        }
    }

    /// Returns the expression for a `LET` or assignment statement.
    pub const fn expression(&self) -> Option<&ControlFlowExpression> {
        match self {
            Self::Let { expression, .. } | Self::Assignment { expression, .. } => Some(expression),
            Self::Return(_) | Self::If(_) | Self::While(_) => None,
        }
    }
    /// Returns the explicit return statement when this is a `RETURN`.
    pub const fn return_statement(&self) -> Option<&ControlFlowReturnStatement> {
        match self {
            Self::Return(statement) => Some(statement),
            Self::Let { .. } | Self::Assignment { .. } | Self::If(_) | Self::While(_) => None,
        }
    }

    /// Returns the conditional statement when this is an `IF`.
    pub const fn if_statement(&self) -> Option<&ControlFlowIfStatement> {
        match self {
            Self::If(statement) => Some(statement),
            Self::Let { .. } | Self::Assignment { .. } | Self::Return(_) | Self::While(_) => None,
        }
    }

    /// Returns the loop statement when this is a `WHILE`.
    pub const fn while_statement(&self) -> Option<&ControlFlowWhileStatement> {
        match self {
            Self::While(statement) => Some(statement),
            Self::Let { .. } | Self::Assignment { .. } | Self::Return(_) | Self::If(_) => None,
        }
    }
}

/// A checked version-10 programmable CLIENT plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFlowClientPlan {
    locals: Vec<ClientLocal>,
    statements: Vec<ControlFlowStatement>,
}

impl ControlFlowClientPlan {
    /// Creates a plan from ordered local declarations and a root block body.
    pub fn new(locals: Vec<ClientLocal>, statements: Vec<ControlFlowStatement>) -> Self {
        Self { locals, statements }
    }

    /// Returns local declarations in source order.
    pub fn locals(&self) -> &[ClientLocal] {
        &self.locals
    }

    /// Returns root statements in source order.
    pub fn statements(&self) -> &[ControlFlowStatement] {
        &self.statements
    }

    /// Alias for [`Self::statements`].
    pub fn body(&self) -> &[ControlFlowStatement] {
        self.statements()
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        CONTROL_FLOW_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-10 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        encode_control_flow_plan(self)
    }

    /// Decodes exactly one canonical version-10 control-flow artefact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        decode_control_flow_plan(bytes)
    }
}

/// The kind of server result represented by a resource operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    /// One scalar result value.
    Scalar,
    /// A bounded stream of result batches.
    Stream,
}

impl ResourceKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Scalar => RESOURCE_KIND_SCALAR,
            Self::Stream => RESOURCE_KIND_STREAM,
        }
    }
}

/// One checked CLIENT-to-SERVER resource operation node (work ADR 0077).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceOperationNode {
    kind: ResourceKind,
    target: FunctionId,
    target_revision: RevisionPair,
    call_site: CallSiteId,
    arguments: Vec<(ParameterId, ClientExpressionNode)>,
    result_type: TypeId,
}

impl ResourceOperationNode {
    /// Creates a resource node from its checked target and argument metadata.
    pub fn new(
        kind: ResourceKind,
        target: FunctionId,
        target_revision: RevisionPair,
        call_site: CallSiteId,
        arguments: Vec<(ParameterId, ClientExpressionNode)>,
        result_type: TypeId,
    ) -> Self {
        Self {
            kind,
            target,
            target_revision,
            call_site,
            arguments,
            result_type,
        }
    }

    /// Returns whether the target produces a scalar or stream result.
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the resolved SERVER target function identity.
    pub const fn target(&self) -> FunctionId {
        self.target
    }

    /// Returns the resolved SERVER target function identity.
    pub const fn target_function(&self) -> FunctionId {
        self.target
    }

    /// Returns the pinned source and catalogue revision pair.
    pub const fn target_revision(&self) -> RevisionPair {
        self.target_revision
    }

    /// Returns the stable compiled call-site identity.
    pub const fn call_site(&self) -> CallSiteId {
        self.call_site
    }

    /// Returns the stable compiled call-site identity.
    pub const fn call_site_id(&self) -> CallSiteId {
        self.call_site
    }

    /// Returns canonical parameter-to-expression pairs.
    pub fn arguments(&self) -> &[(ParameterId, ClientExpressionNode)] {
        &self.arguments
    }

    /// Returns the target declaration's result type identity.
    pub const fn result_type(&self) -> TypeId {
        self.result_type
    }

    /// Returns the target declaration's result type identity.
    pub const fn declared_result_type(&self) -> TypeId {
        self.result_type
    }
}

/// The execution domain of one checked CLIENT action target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionTargetDomain {
    /// Invoke another CLIENT function locally.
    Client,
    /// Submit a SERVER function through the authenticated server boundary.
    Server,
}

impl ActionTargetDomain {
    const fn tag(self) -> u8 {
        match self {
            Self::Client => 1,
            Self::Server => 2,
        }
    }
}

/// One checked CLIENT action operation node (work ADR 0079).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionOperationNode {
    domain: ActionTargetDomain,
    target: FunctionId,
    target_revision: RevisionPair,
    call_site: CallSiteId,
    arguments: Vec<(ParameterId, ClientExpressionNode)>,
    result_type: TypeId,
}

impl ActionOperationNode {
    /// Creates an action node from its checked target and argument metadata.
    pub fn new(
        domain: ActionTargetDomain,
        target: FunctionId,
        target_revision: RevisionPair,
        call_site: CallSiteId,
        arguments: Vec<(ParameterId, ClientExpressionNode)>,
        result_type: TypeId,
    ) -> Self {
        Self {
            domain,
            target,
            target_revision,
            call_site,
            arguments,
            result_type,
        }
    }

    /// Returns the CLIENT or SERVER target domain.
    pub const fn domain(&self) -> ActionTargetDomain {
        self.domain
    }

    /// Returns the resolved action target function identity.
    pub const fn target(&self) -> FunctionId {
        self.target
    }

    /// Returns the resolved action target function identity.
    pub const fn target_function(&self) -> FunctionId {
        self.target
    }

    /// Returns the pinned source and catalogue revision pair.
    pub const fn target_revision(&self) -> RevisionPair {
        self.target_revision
    }

    /// Returns the stable compiled call-site identity.
    pub const fn call_site(&self) -> CallSiteId {
        self.call_site
    }

    /// Returns the stable compiled call-site identity.
    pub const fn call_site_id(&self) -> CallSiteId {
        self.call_site
    }

    /// Returns canonical parameter-to-expression pairs.
    pub fn arguments(&self) -> &[(ParameterId, ClientExpressionNode)] {
        &self.arguments
    }

    /// Returns the target declaration's result type identity.
    pub const fn result_type(&self) -> TypeId {
        self.result_type
    }

    /// Returns the target declaration's result type identity.
    pub const fn declared_result_type(&self) -> TypeId {
        self.result_type
    }
}

/// A checked version-8 CLIENT plan carrying one action operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionClientPlan {
    operation: ActionOperationNode,
}

impl ActionClientPlan {
    /// Creates an action plan from one checked operation.
    pub fn new(operation: ActionOperationNode) -> Self {
        Self { operation }
    }

    /// Returns the checked action operation.
    pub const fn operation(&self) -> &ActionOperationNode {
        &self.operation
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        ACTION_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-8 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        encode_action_plan(self)
    }

    /// Decodes exactly one canonical version-8 action client-plan artefact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        decode_action_plan(bytes)
    }
}

/// A checked version-6 CLIENT plan containing resource operation nodes in a
/// closed expression tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceClientPlan {
    expression: ClientExpressionNode,
}

impl ResourceClientPlan {
    /// Creates a resource plan from a checked expression tree.
    pub fn new(expression: ClientExpressionNode) -> Self {
        Self { expression }
    }

    /// Returns the closed expression tree.
    pub const fn expression(&self) -> &ClientExpressionNode {
        &self.expression
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        RESOURCE_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-6 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        validate_external_contract_placement(&self.expression, false)?;
        validate_resource_await_placement(&self.expression, true, false)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&RESOURCE_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_RESOURCE_OPERATION);
        let mut writer = NodeWriter::new();
        let mut expression_count = 0;
        let mut resource_count = 0;
        encode_expression_node_with_resources(
            &self.expression,
            &mut writer,
            0,
            &mut expression_count,
            true,
            false,
            false,
            &mut resource_count,
        )?;
        if resource_count == 0 {
            return Err(ClientPlanError::InvalidResourceOperationCount { actual: 0 });
        }
        bytes.extend_from_slice(&writer.finish());
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-6 resource client-plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != RESOURCE_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_RESOURCE_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let mut expression_count = 0;
        let mut resource_count = 0;
        let expression = decode_expression_node_with_resources(
            &mut reader,
            0,
            &mut expression_count,
            true,
            false,
            false,
            &mut resource_count,
        )?;
        reader.require_finished()?;
        validate_external_contract_placement(&expression, false)?;
        validate_resource_await_placement(&expression, true, false)?;
        if resource_count == 0 {
            return Err(ClientPlanError::InvalidResourceOperationCount { actual: 0 });
        }
        Ok(Self::new(expression))
    }
}
impl ExpressionClientPlan {
    /// Returns the closed expression tree.
    pub const fn expression(&self) -> &ClientExpressionNode {
        &self.expression
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        if self.contains_inspect {
            INSPECT_FORMAT_VERSION
        } else {
            EXPRESSION_FORMAT_VERSION
        }
    }

    /// Encodes this plan into its exact version-3 or version-9 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        // Validate before format detection so an untrusted tree cannot recurse
        // past the bounded expression depth or node count.
        validate_external_contract_placement(&self.expression, true)?;
        let version = self.format_version();
        if version == INSPECT_FORMAT_VERSION {
            validate_external_contract_placement(&self.expression, false)?;
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.push(if version == INSPECT_FORMAT_VERSION {
            RETURN_INSPECT_OPERATION
        } else {
            RETURN_EXPRESSION_OPERATION
        });
        let mut writer = NodeWriter::new();
        let mut count = 0;
        encode_expression_node_with_inspect(
            &self.expression,
            &mut writer,
            0,
            &mut count,
            version == INSPECT_FORMAT_VERSION,
        )?;
        bytes.extend_from_slice(&writer.finish());
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-3 or version-9 expression client-plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != EXPRESSION_FORMAT_VERSION && version != INSPECT_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        let expected_operation = if version == INSPECT_FORMAT_VERSION {
            RETURN_INSPECT_OPERATION
        } else {
            RETURN_EXPRESSION_OPERATION
        };
        if operation != expected_operation {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let mut count = 0usize;
        let expression = decode_expression_node_with_inspect(
            &mut reader,
            0,
            &mut count,
            version == INSPECT_FORMAT_VERSION,
        )?;
        validate_external_contract_placement(&expression, version == EXPRESSION_FORMAT_VERSION)?;
        reader.require_finished()?;
        if version == INSPECT_FORMAT_VERSION && !expression_contains_inspect(&expression) {
            return Err(ClientPlanError::InvalidInspectPlan);
        }
        Ok(Self::new(expression))
    }
}

/// The scope of one checked CLIENT state slot (work ADR 0069).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateScope {
    /// State private to one mounted function instance.
    Local,
    /// State retained for the client invocation session.
    Session,
    /// State associated with the authenticated principal.
    User,
}

impl StateScope {
    /// Returns the canonical scope tag byte.
    const fn tag(self) -> u8 {
        match self {
            Self::Local => STATE_SCOPE_LOCAL,
            Self::Session => STATE_SCOPE_SESSION,
            Self::User => STATE_SCOPE_USER,
        }
    }
}

/// The checked initial value of one CLIENT state slot (work ADR 0069).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateDefault {
    /// No DEFAULT clause was written.
    Unset,
    /// The slot starts with an explicit null value.
    Null,
    /// The slot starts with a checked CLIENT expression value.
    Expression(ClientExpressionNode),
}

/// One checked CLIENT state slot with source-free semantic metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateSlot {
    state_slot_id: StateSlotId,
    type_id: TypeId,
    scope: StateScope,
    default: StateDefault,
}

impl StateSlot {
    /// Creates a checked state slot from its durable identity, nominal type,
    /// scope, and checked default.
    pub const fn new(
        state_slot_id: StateSlotId,
        type_id: TypeId,
        scope: StateScope,
        default: StateDefault,
    ) -> Self {
        Self {
            state_slot_id,
            type_id,
            scope,
            default,
        }
    }

    /// Returns the durable state-slot identity.
    pub const fn state_slot_id(&self) -> StateSlotId {
        self.state_slot_id
    }

    /// Returns the nominal state value type.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns the declared state scope.
    pub const fn scope(&self) -> StateScope {
        self.scope
    }

    /// Returns the checked initial value of this slot.
    pub const fn default(&self) -> &StateDefault {
        &self.default
    }
}

/// A checked version-4 CLIENT plan that returns one closed expression tree
/// and carries the owning function's ordered state-slot declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateClientPlan {
    expression: ClientExpressionNode,
    slots: Vec<StateSlot>,
}

impl StateClientPlan {
    /// Creates a checked state plan from one closed expression tree and the
    /// ordered state-slot records.
    pub fn new(expression: ClientExpressionNode, slots: Vec<StateSlot>) -> Self {
        Self { expression, slots }
    }

    /// Returns the closed return expression tree.
    pub const fn expression(&self) -> &ClientExpressionNode {
        &self.expression
    }

    /// Returns the state slots in declaration order.
    pub fn slots(&self) -> &[StateSlot] {
        &self.slots
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        STATE_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-4 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        let slot_count = self.slots.len();
        if slot_count == 0 {
            return Err(ClientPlanError::InvalidStateSlotCount { actual: 0 });
        }
        if slot_count > MAX_STATE_SLOTS {
            return Err(ClientPlanError::StateSlotLimitExceeded {
                limit: MAX_STATE_SLOTS,
            });
        }
        let mut seen: Vec<StateSlotId> = Vec::with_capacity(slot_count);
        for slot in &self.slots {
            if seen.contains(&slot.state_slot_id) {
                return Err(ClientPlanError::DuplicateStateSlotId(slot.state_slot_id));
            }
            seen.push(slot.state_slot_id);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_STATE_OPERATION);
        let mut writer = NodeWriter::new();
        let mut count = 0;
        validate_external_contract_placement(&self.expression, false)?;
        encode_expression_node(&self.expression, &mut writer, 0, &mut count)?;
        bytes.extend_from_slice(&writer.finish());
        bytes.extend_from_slice(&(slot_count as u32).to_be_bytes());
        for slot in &self.slots {
            bytes.extend_from_slice(&slot.state_slot_id.to_bytes());
            bytes.extend_from_slice(&slot.type_id.to_bytes());
            bytes.push(slot.scope.tag());
            match &slot.default {
                StateDefault::Unset => bytes.push(STATE_DEFAULT_UNSET),
                StateDefault::Null => bytes.push(STATE_DEFAULT_NULL),
                StateDefault::Expression(node) => {
                    bytes.push(STATE_DEFAULT_EXPRESSION);
                    let mut writer = NodeWriter::new();
                    let mut count = 0;
                    validate_external_contract_placement(node, false)?;
                    encode_expression_node(node, &mut writer, 0, &mut count)?;
                    bytes.extend_from_slice(&writer.finish());
                }
            }
        }
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-4 state client-plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != STATE_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_STATE_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let mut count = 0usize;
        let expression = decode_expression_node(&mut reader, 0, &mut count)?;
        validate_external_contract_placement(&expression, false)?;
        let slot_count = reader.u32()?;
        if slot_count == 0 {
            return Err(ClientPlanError::InvalidStateSlotCount { actual: 0 });
        }
        if slot_count as usize > MAX_STATE_SLOTS {
            return Err(ClientPlanError::StateSlotLimitExceeded {
                limit: MAX_STATE_SLOTS,
            });
        }
        let mut slots = Vec::with_capacity(slot_count as usize);
        let mut seen: Vec<StateSlotId> = Vec::with_capacity(slot_count as usize);
        for _ in 0..slot_count {
            let state_slot_id = StateSlotId::from_bytes(reader.array()?);
            if seen.contains(&state_slot_id) {
                return Err(ClientPlanError::DuplicateStateSlotId(state_slot_id));
            }
            seen.push(state_slot_id);
            let type_id = TypeId::from_bytes(reader.array()?);
            let scope = match reader.u8()? {
                STATE_SCOPE_LOCAL => StateScope::Local,
                STATE_SCOPE_SESSION => StateScope::Session,
                STATE_SCOPE_USER => StateScope::User,
                tag => return Err(ClientPlanError::InvalidStateScope(tag)),
            };
            let default = match reader.u8()? {
                STATE_DEFAULT_UNSET => StateDefault::Unset,
                STATE_DEFAULT_NULL => StateDefault::Null,
                STATE_DEFAULT_EXPRESSION => {
                    let mut count = 0usize;
                    let expression = decode_expression_node(&mut reader, 0, &mut count)?;
                    validate_external_contract_placement(&expression, false)?;
                    StateDefault::Expression(expression)
                }
                tag => return Err(ClientPlanError::InvalidStateDefaultTag(tag)),
            };
            slots.push(StateSlot::new(state_slot_id, type_id, scope, default));
        }
        reader.require_finished()?;
        Ok(Self::new(expression, slots))
    }
}

/// The argument source of one CLIENT capability requirement (work ADR 0060).
///
/// The envelope is independent of the client grant model: literal scope
/// text and parameter names travel as plain strings, mirroring the closed
/// `CapabilitySpecification` form (exactly one argument, either a literal
/// or a declared parameter reference).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityArgumentSource {
    /// A literal scope value written in the declaration.
    Text(String),
    /// A reference to one declared function parameter by name.
    Parameter(String),
}

/// One ordered, closed CLIENT capability requirement (work ADR 0060).
///
/// A requirement is a qualified capability name plus exactly one argument
/// source. The name and argument are plain text so the artifact layer
/// stays independent of the client grant model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequirement {
    name: String,
    argument: CapabilityArgumentSource,
}

impl CapabilityRequirement {
    /// Creates a requirement from one capability name and one argument source.
    pub fn new(name: impl Into<String>, argument: CapabilityArgumentSource) -> Self {
        Self {
            name: name.into(),
            argument,
        }
    }

    /// Returns the qualified capability name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declared argument source.
    pub const fn argument(&self) -> &CapabilityArgumentSource {
        &self.argument
    }
}

/// The inner plan carried by one version-5 capability envelope.
///
/// The envelope holds a complete decoded version 1-4, version-6, version-7,
/// version-8, version-9, or version-10 client plan so the runtime can evaluate
/// it directly after the capability gate
/// admits it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InnerClientPlan {
    /// A version-1 Boolean-constant plan.
    Boolean(ClientPlan),
    /// A version-2 opaque-value plan.
    Opaque(OpaqueClientPlan),
    /// A version-3 or version-9 expression plan.
    Expression(ExpressionClientPlan),
    /// A version-4 state plan.
    State(StateClientPlan),
    /// A version-6 resource-operation plan.
    Resource(ResourceClientPlan),
    /// A version-7 procedural plan.
    Procedural(ProceduralClientPlan),
    /// A version-8 action plan.
    Action(ActionClientPlan),
    /// A version-10 programmable control-flow plan.
    ControlFlow(ControlFlowClientPlan),
}

impl InnerClientPlan {
    /// Returns the canonical version of the inner plan.
    pub const fn format_version(&self) -> u32 {
        match self {
            Self::Boolean(_) => FORMAT_VERSION,
            Self::Opaque(_) => OPAQUE_FORMAT_VERSION,
            Self::Expression(plan) => plan.format_version(),
            Self::State(_) => STATE_FORMAT_VERSION,
            Self::Resource(_) => RESOURCE_FORMAT_VERSION,
            Self::Procedural(_) => PROCEDURAL_FORMAT_VERSION,
            Self::Action(_) => ACTION_FORMAT_VERSION,
            Self::ControlFlow(_) => CONTROL_FLOW_FORMAT_VERSION,
        }
    }

    /// Encodes the inner plan into its exact version bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        match self {
            Self::Boolean(plan) => Ok(plan.encode()),
            Self::Opaque(plan) => plan.encode(),
            Self::Expression(plan) => plan.encode(),
            Self::State(plan) => plan.encode(),
            Self::Resource(plan) => plan.encode(),
            Self::Procedural(plan) => plan.encode(),
            Self::Action(plan) => plan.encode(),
            Self::ControlFlow(plan) => plan.encode(),
        }
    }
}
/// A checked version-5 CLIENT plan that carries one version 1-4, version-6,
/// version-7, version-8, version-9, or version-10 inner plan and the owning
/// function's ordered, closed capability requirements (work ADR 0060).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityClientPlan {
    inner_plan_version: u32,
    inner_plan: InnerClientPlan,
    requirements: Vec<CapabilityRequirement>,
}

impl CapabilityClientPlan {
    /// Creates a checked capability plan from one inner plan and the ordered
    /// capability requirements.
    pub fn new(inner_plan: InnerClientPlan, requirements: Vec<CapabilityRequirement>) -> Self {
        Self {
            inner_plan_version: inner_plan.format_version(),
            inner_plan,
            requirements,
        }
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        CAPABILITY_FORMAT_VERSION
    }

    /// Returns the canonical version of the carried inner plan.
    pub const fn inner_plan_version(&self) -> u32 {
        self.inner_plan_version
    }

    /// Returns the decoded inner client plan.
    pub const fn inner_plan(&self) -> &InnerClientPlan {
        &self.inner_plan
    }

    /// Returns the capability requirements in declaration order.
    pub fn requirements(&self) -> &[CapabilityRequirement] {
        &self.requirements
    }

    /// Encodes this plan into its exact version-5 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        let requirement_count = self.requirements.len();
        if requirement_count == 0 {
            return Err(ClientPlanError::InvalidCapabilityCount { actual: 0 });
        }
        if requirement_count > MAX_CAPABILITY_REQUIREMENTS {
            return Err(ClientPlanError::CapabilityLimitExceeded {
                limit: MAX_CAPABILITY_REQUIREMENTS,
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(requirement_count);
        for requirement in &self.requirements {
            if requirement.name.is_empty() {
                return Err(ClientPlanError::EmptyCapabilityName);
            }
            if requirement.name.len() > MAX_CAPABILITY_NAME_LENGTH {
                return Err(ClientPlanError::CapabilityNameTooLong {
                    length: requirement.name.len(),
                    limit: MAX_CAPABILITY_NAME_LENGTH,
                });
            }
            if seen.contains(&requirement.name.as_str()) {
                return Err(ClientPlanError::DuplicateCapabilityName(
                    requirement.name.clone(),
                ));
            }
            seen.push(&requirement.name);
            let argument = match &requirement.argument {
                CapabilityArgumentSource::Text(text) => text,
                CapabilityArgumentSource::Parameter(text) => text,
            };
            if argument.is_empty() {
                return Err(ClientPlanError::EmptyCapabilityArgument);
            }
            if argument.len() > MAX_CAPABILITY_ARGUMENT_LENGTH {
                return Err(ClientPlanError::CapabilityArgumentTooLong {
                    length: argument.len(),
                    limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
                });
            }
        }
        let inner_payload = self.inner_plan.encode()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_CAPABILITY_OPERATION);
        bytes.extend_from_slice(&self.inner_plan_version.to_be_bytes());
        // The inner plan encoder bounds its artefact to MAX_ARTIFACT_BYTES,
        // so the length always fits a u32.
        bytes.extend_from_slice(&(inner_payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&inner_payload);
        bytes.extend_from_slice(&(requirement_count as u32).to_be_bytes());
        for requirement in &self.requirements {
            // Lengths were bounded above, so the casts always fit a u32.
            bytes.extend_from_slice(&(requirement.name.len() as u32).to_be_bytes());
            bytes.extend_from_slice(requirement.name.as_bytes());
            match &requirement.argument {
                CapabilityArgumentSource::Text(text) => {
                    bytes.push(CAPABILITY_ARGUMENT_TEXT);
                    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
                    bytes.extend_from_slice(text.as_bytes());
                }
                CapabilityArgumentSource::Parameter(text) => {
                    bytes.push(CAPABILITY_ARGUMENT_PARAMETER);
                    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
                    bytes.extend_from_slice(text.as_bytes());
                }
            }
        }
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-5 capability client-plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ClientPlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != CAPABILITY_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_CAPABILITY_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let inner_plan_version = reader.u32()?;
        let inner_payload_length = reader.u32()? as usize;
        let inner_payload = reader.take(inner_payload_length)?;
        let inner_plan = match inner_plan_version {
            FORMAT_VERSION => InnerClientPlan::Boolean(ClientPlan::decode(inner_payload)?),
            OPAQUE_FORMAT_VERSION => {
                InnerClientPlan::Opaque(OpaqueClientPlan::decode(inner_payload)?)
            }
            EXPRESSION_FORMAT_VERSION | INSPECT_FORMAT_VERSION => {
                InnerClientPlan::Expression(ExpressionClientPlan::decode(inner_payload)?)
            }
            STATE_FORMAT_VERSION => InnerClientPlan::State(StateClientPlan::decode(inner_payload)?),
            RESOURCE_FORMAT_VERSION => {
                InnerClientPlan::Resource(ResourceClientPlan::decode(inner_payload)?)
            }
            PROCEDURAL_FORMAT_VERSION => {
                InnerClientPlan::Procedural(ProceduralClientPlan::decode(inner_payload)?)
            }
            ACTION_FORMAT_VERSION => {
                InnerClientPlan::Action(ActionClientPlan::decode(inner_payload)?)
            }
            CONTROL_FLOW_FORMAT_VERSION => {
                InnerClientPlan::ControlFlow(ControlFlowClientPlan::decode(inner_payload)?)
            }
            version => return Err(ClientPlanError::UnsupportedInnerVersion(version)),
        };
        let actual_inner_plan_version = inner_plan.format_version();
        if inner_plan_version != actual_inner_plan_version {
            return Err(ClientPlanError::InnerVersionMismatch {
                declared: inner_plan_version,
                actual: actual_inner_plan_version,
            });
        }
        let requirement_count = reader.u32()?;
        if requirement_count == 0 {
            return Err(ClientPlanError::InvalidCapabilityCount { actual: 0 });
        }
        if requirement_count as usize > MAX_CAPABILITY_REQUIREMENTS {
            return Err(ClientPlanError::CapabilityLimitExceeded {
                limit: MAX_CAPABILITY_REQUIREMENTS,
            });
        }
        let mut requirements = Vec::with_capacity(requirement_count as usize);
        let mut seen: Vec<String> = Vec::with_capacity(requirement_count as usize);
        for _ in 0..requirement_count {
            let name_length = reader.u32()? as usize;
            if name_length == 0 {
                return Err(ClientPlanError::EmptyCapabilityName);
            }
            if name_length > MAX_CAPABILITY_NAME_LENGTH {
                return Err(ClientPlanError::CapabilityNameTooLong {
                    length: name_length,
                    limit: MAX_CAPABILITY_NAME_LENGTH,
                });
            }
            let name = std::str::from_utf8(reader.take(name_length)?)
                .map_err(|_| ClientPlanError::InvalidCapabilityNameUtf8)?
                .to_owned();
            if seen.contains(&name) {
                return Err(ClientPlanError::DuplicateCapabilityName(name));
            }
            seen.push(name.clone());
            let argument_tag = reader.u8()?;
            let argument_length = reader.u32()? as usize;
            if argument_length == 0 {
                return Err(ClientPlanError::EmptyCapabilityArgument);
            }
            if argument_length > MAX_CAPABILITY_ARGUMENT_LENGTH {
                return Err(ClientPlanError::CapabilityArgumentTooLong {
                    length: argument_length,
                    limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
                });
            }
            let argument_text = std::str::from_utf8(reader.take(argument_length)?)
                .map_err(|_| ClientPlanError::InvalidCapabilityArgumentUtf8)?
                .to_owned();
            let argument = match argument_tag {
                CAPABILITY_ARGUMENT_TEXT => CapabilityArgumentSource::Text(argument_text),
                CAPABILITY_ARGUMENT_PARAMETER => CapabilityArgumentSource::Parameter(argument_text),
                tag => return Err(ClientPlanError::InvalidCapabilityArgumentTag(tag)),
            };
            requirements.push(CapabilityRequirement::new(name, argument));
        }
        reader.require_finished()?;
        Ok(Self::new(inner_plan, requirements))
    }
}

/// The maximum encoded length of one expression-tree byte slice.
struct NodeWriter {
    bytes: Vec<u8>,
}

impl NodeWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn push(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn encode_control_flow_plan(plan: &ControlFlowClientPlan) -> Result<Vec<u8>, ClientPlanError> {
    validate_control_flow_model(plan)?;

    let local_count = u32::try_from(plan.locals.len()).map_err(|_| {
        ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        }
    })?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&CONTROL_FLOW_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_CONTROL_FLOW_OPERATION);
    bytes.extend_from_slice(&local_count.to_be_bytes());
    for local in &plan.locals {
        bytes.extend_from_slice(&local.local.to_bytes());
        bytes.extend_from_slice(&local.type_id.to_bytes());
        bytes.push(local.kind.tag());
    }

    let mut writer = NodeWriter::new();
    let mut expression_count = 0usize;
    let mut resource_count = 0usize;
    let mut statement_count = 0usize;
    encode_control_flow_block(
        &plan.statements,
        0,
        &mut writer,
        &mut expression_count,
        &mut resource_count,
        &mut statement_count,
    )?;
    bytes.extend_from_slice(&writer.finish());
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(bytes)
}

fn decode_control_flow_plan(bytes: &[u8]) -> Result<ControlFlowClientPlan, ClientPlanError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    let mut reader = Reader::new(bytes);
    if reader.array::<8>()? != MAGIC {
        return Err(ClientPlanError::InvalidMagic);
    }
    let version = reader.u32()?;
    if version != CONTROL_FLOW_FORMAT_VERSION {
        return Err(ClientPlanError::UnsupportedVersion(version));
    }
    let operation = reader.u8()?;
    if operation != RETURN_CONTROL_FLOW_OPERATION {
        return Err(ClientPlanError::InvalidOperation(operation));
    }

    let local_count = reader.u32()? as usize;
    if local_count > MAX_CONTROL_FLOW_LOCALS {
        return Err(ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        });
    }
    let mut locals = Vec::with_capacity(local_count);
    for _ in 0..local_count {
        let local = LocalId::from_bytes(reader.array()?);
        if locals
            .iter()
            .any(|candidate: &ClientLocal| candidate.local == local)
        {
            return Err(ClientPlanError::DuplicateControlFlowLocal(local));
        }
        let type_id = TypeId::from_bytes(reader.array()?);
        let kind = ClientLocalKind::from_tag(reader.u8()?)?;
        locals.push(ClientLocal::new(local, type_id, kind));
    }

    let mut expression_count = 0usize;
    let mut resource_count = 0usize;
    let mut statement_count = 0usize;
    let statements = decode_control_flow_block(
        &mut reader,
        0,
        &mut expression_count,
        &mut resource_count,
        &mut statement_count,
    )?;
    reader.require_finished()?;

    let plan = ControlFlowClientPlan::new(locals, statements);
    validate_control_flow_model(&plan)?;
    Ok(plan)
}

fn encode_control_flow_block(
    statements: &[ControlFlowStatement],
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
    statement_count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    if statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }
    let count = u32::try_from(statements.len()).map_err(|_| {
        ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        }
    })?;
    writer.extend(&count.to_be_bytes());
    for statement in statements {
        *statement_count = statement_count.saturating_add(1);
        if *statement_count > MAX_CONTROL_FLOW_STATEMENTS {
            return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
                limit: MAX_CONTROL_FLOW_STATEMENTS,
            });
        }
        match statement {
            ControlFlowStatement::Let { local, expression } => {
                writer.push(CONTROL_FLOW_STATEMENT_LET);
                writer.extend(&local.to_bytes());
                encode_control_flow_expression(
                    expression,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
            ControlFlowStatement::Assignment { local, expression } => {
                writer.push(CONTROL_FLOW_STATEMENT_ASSIGNMENT);
                writer.extend(&local.to_bytes());
                encode_control_flow_expression(
                    expression,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
            ControlFlowStatement::Return(return_statement) => {
                writer.push(CONTROL_FLOW_STATEMENT_RETURN);
                match return_statement.expression.as_ref() {
                    None => writer.push(CONTROL_FLOW_RETURN_NONE),
                    Some(expression) => {
                        writer.push(CONTROL_FLOW_RETURN_EXPRESSION);
                        encode_control_flow_expression(
                            expression,
                            0,
                            writer,
                            expression_count,
                            resource_count,
                        )?;
                    }
                }
            }
            ControlFlowStatement::If(if_statement) => {
                if if_statement.branches.is_empty() {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if if_statement.branches.len() > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                writer.push(CONTROL_FLOW_STATEMENT_IF);
                let branch_count = u32::try_from(if_statement.branches.len()).map_err(|_| {
                    ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    }
                })?;
                writer.extend(&branch_count.to_be_bytes());
                for branch in &if_statement.branches {
                    encode_control_flow_expression(
                        &branch.condition,
                        0,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                    encode_control_flow_block(
                        &branch.statements,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?;
                }
                match if_statement.else_statements.as_ref() {
                    None => writer.push(CONTROL_FLOW_ELSE_NONE),
                    Some(statements) => {
                        writer.push(CONTROL_FLOW_ELSE_BODY);
                        encode_control_flow_block(
                            statements,
                            depth + 1,
                            writer,
                            expression_count,
                            resource_count,
                            statement_count,
                        )?;
                    }
                }
            }
            ControlFlowStatement::While(while_statement) => {
                writer.push(CONTROL_FLOW_STATEMENT_WHILE);
                encode_control_flow_expression(
                    &while_statement.condition,
                    0,
                    writer,
                    expression_count,
                    resource_count,
                )?;
                encode_control_flow_block(
                    &while_statement.statements,
                    depth + 1,
                    writer,
                    expression_count,
                    resource_count,
                    statement_count,
                )?;
            }
        }
    }
    Ok(())
}

fn decode_control_flow_block(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
    statement_count: &mut usize,
) -> Result<Vec<ControlFlowStatement>, ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    let count = reader.u32()? as usize;
    if count > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }
    let mut statements = Vec::with_capacity(count);
    for _ in 0..count {
        *statement_count = statement_count.saturating_add(1);
        if *statement_count > MAX_CONTROL_FLOW_STATEMENTS {
            return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
                limit: MAX_CONTROL_FLOW_STATEMENTS,
            });
        }
        let tag = reader.u8()?;
        let statement = match tag {
            CONTROL_FLOW_STATEMENT_LET | CONTROL_FLOW_STATEMENT_ASSIGNMENT => {
                let local = LocalId::from_bytes(reader.array()?);
                let expression =
                    decode_control_flow_expression(reader, 0, expression_count, resource_count)?;
                if tag == CONTROL_FLOW_STATEMENT_LET {
                    ControlFlowStatement::let_(local, expression)
                } else {
                    ControlFlowStatement::assignment(local, expression)
                }
            }
            CONTROL_FLOW_STATEMENT_RETURN => {
                let expression = match reader.u8()? {
                    CONTROL_FLOW_RETURN_NONE => None,
                    CONTROL_FLOW_RETURN_EXPRESSION => Some(decode_control_flow_expression(
                        reader,
                        0,
                        expression_count,
                        resource_count,
                    )?),
                    tag => return Err(ClientPlanError::InvalidControlFlowReturnTag(tag)),
                };
                ControlFlowStatement::Return(ControlFlowReturnStatement::new(expression))
            }
            CONTROL_FLOW_STATEMENT_IF => {
                let branch_count = reader.u32()? as usize;
                if branch_count == 0 {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if branch_count > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                let mut branches = Vec::with_capacity(branch_count);
                for _ in 0..branch_count {
                    let condition = decode_control_flow_expression(
                        reader,
                        0,
                        expression_count,
                        resource_count,
                    )?;
                    let statements = decode_control_flow_block(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?;
                    branches.push(ControlFlowIfBranch::new(condition, statements));
                }
                let else_statements = match reader.u8()? {
                    CONTROL_FLOW_ELSE_NONE => None,
                    CONTROL_FLOW_ELSE_BODY => Some(decode_control_flow_block(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                        statement_count,
                    )?),
                    tag => return Err(ClientPlanError::InvalidControlFlowElseTag(tag)),
                };
                ControlFlowStatement::If(ControlFlowIfStatement::new(branches, else_statements))
            }
            CONTROL_FLOW_STATEMENT_WHILE => {
                let condition =
                    decode_control_flow_expression(reader, 0, expression_count, resource_count)?;
                let statements = decode_control_flow_block(
                    reader,
                    depth + 1,
                    expression_count,
                    resource_count,
                    statement_count,
                )?;
                ControlFlowStatement::While(ControlFlowWhileStatement::new(condition, statements))
            }
            tag => return Err(ClientPlanError::InvalidControlFlowStatement(tag)),
        };
        statements.push(statement);
    }
    Ok(statements)
}
fn encode_control_flow_expression(
    node: &ControlFlowExpression,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *expression_count = expression_count.saturating_add(1);
    if *expression_count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }

    match node {
        ClientExpressionNode::Await { expression } => {
            writer.push(NODE_AWAIT);
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Resource { operation } => {
            encode_control_flow_resource_operation(
                operation,
                depth,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Action { operation } => {
            encode_control_flow_action_operation(
                operation,
                depth,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Inspect { operation } => {
            writer.push(NODE_INSPECT);
            match operation {
                InspectOperationNode::Snapshot { target, options } => {
                    writer.push(INSPECT_OPERATION_SNAPSHOT);
                    encode_control_flow_expression(
                        target,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                    if options.is_some() {
                        return Err(ClientPlanError::UnsupportedInspectOptions);
                    }
                    writer.push(0);
                }
                InspectOperationNode::Projection {
                    projection,
                    snapshot,
                } => {
                    writer.push(INSPECT_OPERATION_PROJECTION);
                    writer.push(projection.tag());
                    encode_control_flow_expression(
                        snapshot,
                        depth + 1,
                        writer,
                        expression_count,
                        resource_count,
                    )?;
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            writer.push(NODE_CALL);
            writer.extend(&function.to_bytes());
            let length = u32::try_from(arguments.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for (parameter, value) in arguments {
                writer.extend(&parameter.to_bytes());
                encode_control_flow_expression(
                    value,
                    depth + 1,
                    writer,
                    expression_count,
                    resource_count,
                )?;
            }
        }
        ClientExpressionNode::String { value } => {
            writer.push(NODE_STRING);
            let bytes = value.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::Integer { value } => {
            writer.push(NODE_INTEGER);
            writer.extend(&value.to_be_bytes());
        }
        ClientExpressionNode::Boolean { value } => {
            writer.push(NODE_BOOLEAN);
            writer.push(u8::from(*value));
        }
        ClientExpressionNode::ParameterRead { parameter } => {
            writer.push(NODE_PARAMETER_READ);
            writer.extend(&parameter.to_bytes());
        }
        ClientExpressionNode::LocalRead { local } => {
            writer.push(NODE_LOCAL_READ);
            writer.extend(&local.to_bytes());
        }
        ClientExpressionNode::FieldPath { root, fields } => {
            if fields.is_empty() || fields.len() > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            writer.push(NODE_FIELD_PATH);
            writer.extend(&root.to_bytes());
            let length = u32::try_from(fields.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for field in fields {
                writer.extend(&field.to_bytes());
            }
        }
        ClientExpressionNode::Concat { left, right } => {
            writer.push(NODE_CONCAT);
            encode_control_flow_expression(
                left,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
            encode_control_flow_expression(
                right,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::ExternalContract { identity } => {
            validate_external_contract_identity(identity)?;
            writer.push(NODE_EXTERNAL_CONTRACT);
            let bytes = identity.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::Unary {
            operator,
            expression,
        } => {
            writer.push(NODE_UNARY);
            writer.push(operator.tag());
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::Binary {
            operator,
            left,
            right,
        } => {
            writer.push(NODE_BINARY);
            writer.push(operator.tag());
            encode_control_flow_expression(
                left,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
            encode_control_flow_expression(
                right,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
        ClientExpressionNode::SourceIntrospection => writer.push(NODE_SOURCE_INTROSPECTION),
        ClientExpressionNode::Input => writer.push(NODE_INPUT),
        ClientExpressionNode::Evaluate { expression } => {
            writer.push(NODE_EVALUATE);
            encode_control_flow_expression(
                expression,
                depth + 1,
                writer,
                expression_count,
                resource_count,
            )?;
        }
    }
    Ok(())
}

fn encode_control_flow_resource_operation(
    operation: &ResourceOperationNode,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    *resource_count = resource_count.saturating_add(1);
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        validate_resource_identity(identity)?;
    }
    validate_resource_arguments(&operation.arguments)?;
    writer.push(NODE_RESOURCE);
    writer.push(operation.kind.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    let argument_count = u32::try_from(operation.arguments.len()).map_err(|_| {
        ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        }
    })?;
    writer.extend(&argument_count.to_be_bytes());
    for (parameter, value) in &operation.arguments {
        writer.extend(&parameter.to_bytes());
        encode_control_flow_expression(value, depth + 1, writer, expression_count, resource_count)?;
    }
    writer.extend(&operation.result_type.to_bytes());
    Ok(())
}

fn encode_control_flow_action_operation(
    operation: &ActionOperationNode,
    depth: usize,
    writer: &mut NodeWriter,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    validate_action_arguments(&operation.arguments)?;
    writer.push(NODE_ACTION);
    writer.push(operation.domain.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    writer.extend(&operation.result_type.to_bytes());
    let argument_count = u32::try_from(operation.arguments.len()).map_err(|_| {
        ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        }
    })?;
    writer.extend(&argument_count.to_be_bytes());
    for (parameter, value) in &operation.arguments {
        if parameter.to_bytes() == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
        writer.extend(&parameter.to_bytes());
        encode_control_flow_expression(value, depth + 1, writer, expression_count, resource_count)?;
    }
    Ok(())
}

fn decode_control_flow_expression(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ControlFlowExpression, ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *expression_count = expression_count.saturating_add(1);
    if *expression_count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }

    let tag = reader.u8()?;
    match tag {
        NODE_AWAIT => Ok(ClientExpressionNode::Await {
            expression: Box::new(decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?),
        }),
        NODE_RESOURCE => Ok(ClientExpressionNode::Resource {
            operation: decode_control_flow_resource_operation(
                reader,
                depth,
                expression_count,
                resource_count,
            )?,
        }),
        NODE_ACTION => Ok(ClientExpressionNode::Action {
            operation: decode_control_flow_action_operation(
                reader,
                depth,
                expression_count,
                resource_count,
            )?,
        }),
        NODE_INSPECT => {
            let operation = match reader.u8()? {
                INSPECT_OPERATION_SNAPSHOT => InspectOperationNode::Snapshot {
                    target: Box::new(decode_control_flow_expression(
                        reader,
                        depth + 1,
                        expression_count,
                        resource_count,
                    )?),
                    options: match reader.u8()? {
                        0 => None,
                        1 => return Err(ClientPlanError::UnsupportedInspectOptions),
                        tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
                    },
                },
                INSPECT_OPERATION_PROJECTION => {
                    let projection = InspectProjection::from_tag(reader.u8()?)?;
                    InspectOperationNode::Projection {
                        projection,
                        snapshot: Box::new(decode_control_flow_expression(
                            reader,
                            depth + 1,
                            expression_count,
                            resource_count,
                        )?),
                    }
                }
                tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
            };
            Ok(ClientExpressionNode::Inspect { operation })
        }
        NODE_CALL => {
            let function = FunctionId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            let mut arguments = Vec::with_capacity(length);
            for _ in 0..length {
                let parameter = ParameterId::from_bytes(reader.array()?);
                let value = decode_control_flow_expression(
                    reader,
                    depth + 1,
                    expression_count,
                    resource_count,
                )?;
                arguments.push((parameter, value));
            }
            Ok(ClientExpressionNode::Call {
                function,
                arguments,
            })
        }
        NODE_STRING => {
            let length = reader.u32()? as usize;
            let value = std::str::from_utf8(reader.take(length)?)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_STRING))?
                .to_owned();
            Ok(ClientExpressionNode::String { value })
        }
        NODE_INTEGER => Ok(ClientExpressionNode::Integer {
            value: i64::from_be_bytes(reader.array()?),
        }),
        NODE_BOOLEAN => match reader.u8()? {
            0 => Ok(ClientExpressionNode::Boolean { value: false }),
            1 => Ok(ClientExpressionNode::Boolean { value: true }),
            _ => Err(ClientPlanError::InvalidExpressionNode(NODE_BOOLEAN)),
        },
        NODE_PARAMETER_READ => Ok(ClientExpressionNode::ParameterRead {
            parameter: ParameterId::from_bytes(reader.array()?),
        }),
        NODE_LOCAL_READ => Ok(ClientExpressionNode::LocalRead {
            local: LocalId::from_bytes(reader.array()?),
        }),
        NODE_FIELD_PATH => {
            let root = ParameterId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length == 0 {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            if length > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                });
            }
            let mut fields = Vec::with_capacity(length);
            for _ in 0..length {
                fields.push(FieldId::from_bytes(reader.array()?));
            }
            Ok(ClientExpressionNode::FieldPath { root, fields })
        }
        NODE_CONCAT => {
            let left = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            let right = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Concat {
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        NODE_SOURCE_INTROSPECTION => Ok(ClientExpressionNode::SourceIntrospection),
        NODE_INPUT => Ok(ClientExpressionNode::Input),
        NODE_EVALUATE => Ok(ClientExpressionNode::Evaluate {
            expression: Box::new(decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?),
        }),
        NODE_EXTERNAL_CONTRACT => {
            let length = reader.u32()? as usize;
            let identity = std::str::from_utf8(reader.take(length)?)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_EXTERNAL_CONTRACT))?;
            validate_external_contract_identity(identity)?;
            Ok(ClientExpressionNode::ExternalContract {
                identity: identity.to_owned(),
            })
        }
        NODE_UNARY => {
            let operator = ControlFlowUnaryOperator::from_tag(reader.u8()?)?;
            let expression = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Unary {
                operator,
                expression: Box::new(expression),
            })
        }
        NODE_BINARY => {
            let operator = ControlFlowBinaryOperator::from_tag(reader.u8()?)?;
            let left = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            let right = decode_control_flow_expression(
                reader,
                depth + 1,
                expression_count,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        tag => Err(ClientPlanError::InvalidExpressionNode(tag)),
    }
}

fn decode_control_flow_resource_operation(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ResourceOperationNode, ClientPlanError> {
    *resource_count = resource_count.saturating_add(1);
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    let kind = match reader.u8()? {
        RESOURCE_KIND_SCALAR => ResourceKind::Scalar,
        RESOURCE_KIND_STREAM => ResourceKind::Stream,
        tag => return Err(ClientPlanError::InvalidResourceKind(tag)),
    };
    let target = FunctionId::from_bytes(read_resource_identity(reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_resource_identity(reader)?),
        CatalogueRevisionId::from_bytes(read_resource_identity(reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_resource_identity(reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_resource_identity(reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalResourceArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateResourceArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value =
            decode_control_flow_expression(reader, depth + 1, expression_count, resource_count)?;
        arguments.push((parameter, value));
    }
    let result_type = TypeId::from_bytes(read_resource_identity(reader)?);
    Ok(ResourceOperationNode::new(
        kind,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    ))
}

fn decode_control_flow_action_operation(
    reader: &mut Reader<'_>,
    depth: usize,
    expression_count: &mut usize,
    resource_count: &mut usize,
) -> Result<ActionOperationNode, ClientPlanError> {
    let domain = match reader.u8()? {
        1 => ActionTargetDomain::Client,
        2 => ActionTargetDomain::Server,
        tag => return Err(ClientPlanError::InvalidActionDomain(tag)),
    };
    let target = FunctionId::from_bytes(read_action_identity(reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_action_identity(reader)?),
        CatalogueRevisionId::from_bytes(read_action_identity(reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_action_identity(reader)?);
    let result_type = TypeId::from_bytes(read_action_identity(reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_ACTION_ARGUMENTS {
        return Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_action_identity(reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalActionArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateActionArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value =
            decode_control_flow_expression(reader, depth + 1, expression_count, resource_count)?;
        arguments.push((parameter, value));
    }
    Ok(ActionOperationNode::new(
        domain,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    ))
}

fn validate_control_flow_model(plan: &ControlFlowClientPlan) -> Result<(), ClientPlanError> {
    if plan.locals.len() > MAX_CONTROL_FLOW_LOCALS {
        return Err(ClientPlanError::ControlFlowLocalLimitExceeded {
            limit: MAX_CONTROL_FLOW_LOCALS,
        });
    }
    if plan.statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }

    let mut seen_locals = Vec::with_capacity(plan.locals.len());
    for local in &plan.locals {
        if seen_locals.contains(&local.local) {
            return Err(ClientPlanError::DuplicateControlFlowLocal(local.local));
        }
        seen_locals.push(local.local);
    }

    let mut initialized = Vec::new();
    let mut let_seen = Vec::new();
    validate_control_flow_block(
        &plan.statements,
        0,
        &plan.locals,
        &mut initialized,
        &mut let_seen,
    )?;
    for local in &plan.locals {
        if !let_seen.contains(&local.local) {
            return Err(ClientPlanError::MissingControlFlowLet(local.local));
        }
    }
    Ok(())
}

fn validate_control_flow_block(
    statements: &[ControlFlowStatement],
    depth: usize,
    locals: &[ClientLocal],
    initialized: &mut Vec<LocalId>,
    let_seen: &mut Vec<LocalId>,
) -> Result<(), ClientPlanError> {
    if depth > MAX_CONTROL_FLOW_BLOCK_DEPTH {
        return Err(ClientPlanError::ControlFlowBlockDepthExceeded);
    }
    if statements.len() > MAX_CONTROL_FLOW_STATEMENTS {
        return Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS,
        });
    }

    for statement in statements {
        match statement {
            ControlFlowStatement::Let { local, expression } => {
                let declaration = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                if let_seen.contains(local) {
                    return Err(ClientPlanError::DuplicateControlFlowLet(*local));
                }
                let allow_resource_root = matches!(declaration.kind, ClientLocalKind::Resource(_));
                validate_control_flow_expression(
                    expression,
                    locals,
                    initialized,
                    allow_resource_root,
                    true,
                    matches!(declaration.kind, ClientLocalKind::Value),
                )?;
                validate_control_flow_initializer_kind(declaration, expression, locals)?;
                let_seen.push(*local);
                if !initialized.contains(local) {
                    initialized.push(*local);
                }
            }
            ControlFlowStatement::Assignment { local, expression } => {
                let declaration = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                if !initialized.contains(local) {
                    return Err(ClientPlanError::ControlFlowAssignmentBeforeLet(*local));
                }
                let allow_resource_root = matches!(declaration.kind, ClientLocalKind::Resource(_));
                validate_control_flow_expression(
                    expression,
                    locals,
                    initialized,
                    allow_resource_root,
                    true,
                    matches!(declaration.kind, ClientLocalKind::Value),
                )?;
                validate_control_flow_initializer_kind(declaration, expression, locals)?;
            }
            ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression.as_ref() {
                    validate_control_flow_expression(
                        expression,
                        locals,
                        initialized,
                        false,
                        true,
                        true,
                    )?;
                }
            }
            ControlFlowStatement::If(if_statement) => {
                if if_statement.branches.is_empty() {
                    return Err(ClientPlanError::InvalidControlFlowBranchCount { actual: 0 });
                }
                if if_statement.branches.len() > MAX_CONTROL_FLOW_BRANCHES {
                    return Err(ClientPlanError::ControlFlowBranchLimitExceeded {
                        limit: MAX_CONTROL_FLOW_BRANCHES,
                    });
                }
                let incoming = initialized.clone();
                let mut branch_exits = Vec::with_capacity(if_statement.branches.len() + 1);
                for branch in &if_statement.branches {
                    validate_control_flow_expression(
                        &branch.condition,
                        locals,
                        &incoming,
                        false,
                        true,
                        true,
                    )?;
                    let mut branch_initialized = incoming.clone();
                    validate_control_flow_block(
                        &branch.statements,
                        depth + 1,
                        locals,
                        &mut branch_initialized,
                        let_seen,
                    )?;
                    branch_exits.push(branch_initialized);
                }
                if let Some(statements) = if_statement.else_statements.as_ref() {
                    let mut else_initialized = incoming.clone();
                    validate_control_flow_block(
                        statements,
                        depth + 1,
                        locals,
                        &mut else_initialized,
                        let_seen,
                    )?;
                    branch_exits.push(else_initialized);
                    initialized
                        .retain(|local| branch_exits.iter().all(|exit| exit.contains(local)));
                } else {
                    *initialized = incoming;
                }
            }
            ControlFlowStatement::While(while_statement) => {
                let incoming = initialized.clone();
                validate_control_flow_expression(
                    &while_statement.condition,
                    locals,
                    &incoming,
                    false,
                    true,
                    true,
                )?;
                let mut body_initialized = incoming.clone();
                validate_control_flow_block(
                    &while_statement.statements,
                    depth + 1,
                    locals,
                    &mut body_initialized,
                    let_seen,
                )?;
                // A WHILE body may execute zero times, so no local initialized
                // only in that body is definite after the loop.
                *initialized = incoming;
            }
        }
    }
    Ok(())
}

fn validate_control_flow_initializer_kind(
    declaration: &ClientLocal,
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
) -> Result<(), ClientPlanError> {
    let actual = control_flow_resource_kind(expression, locals);
    match declaration.kind {
        ClientLocalKind::Value if actual.is_some() => Err(
            ClientPlanError::ControlFlowLocalKindMismatch(declaration.local),
        ),
        ClientLocalKind::Resource(expected) if actual != Some(expected) => Err(
            ClientPlanError::ControlFlowLocalKindMismatch(declaration.local),
        ),
        ClientLocalKind::Value | ClientLocalKind::Resource(_) => Ok(()),
    }
}

fn control_flow_resource_kind(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
) -> Option<ResourceKind> {
    match expression {
        ClientExpressionNode::Resource { operation } => Some(operation.kind()),
        ClientExpressionNode::LocalRead { local } => locals
            .iter()
            .find(|candidate| candidate.local == *local)
            .and_then(|candidate| match candidate.kind {
                ClientLocalKind::Resource(kind) => Some(kind),
                ClientLocalKind::Value => None,
            }),
        _ => None,
    }
}

fn validate_control_flow_expression(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
    initialized: &[LocalId],
    allow_resource_root: bool,
    allow_await_root: bool,
    value_position: bool,
) -> Result<(), ClientPlanError> {
    validate_external_contract_placement(expression, true)?;
    validate_control_flow_expression_shape(
        expression,
        locals,
        initialized,
        allow_resource_root,
        allow_await_root,
        value_position,
    )
}

fn validate_control_flow_expression_shape(
    expression: &ControlFlowExpression,
    locals: &[ClientLocal],
    initialized: &[LocalId],
    allow_resource_root: bool,
    allow_await_root: bool,
    value_position: bool,
) -> Result<(), ClientPlanError> {
    match expression {
        ClientExpressionNode::Await { expression } => {
            if !allow_await_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            match expression.as_ref() {
                ClientExpressionNode::Resource { operation } => {
                    validate_control_flow_resource_operation(operation, locals, initialized)?;
                }
                ClientExpressionNode::LocalRead { local } => {
                    let declaration = locals
                        .iter()
                        .find(|candidate| candidate.local == *local)
                        .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
                    if !matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                        return Err(ClientPlanError::InvalidAwaitOperand(*local));
                    }
                    if !initialized.contains(local) {
                        return Err(ClientPlanError::ControlFlowLocalReadBeforeLet(*local));
                    }
                }
                _ => return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT)),
            }
        }
        ClientExpressionNode::Resource { operation } => {
            if !allow_resource_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            validate_control_flow_resource_operation(operation, locals, initialized)?;
        }
        ClientExpressionNode::Action { operation } => {
            validate_control_flow_action_operation(operation, locals, initialized)?;
        }
        ClientExpressionNode::Inspect { operation } => match operation {
            InspectOperationNode::Snapshot { target, options } => {
                if options.is_some() {
                    return Err(ClientPlanError::UnsupportedInspectOptions);
                }
                validate_control_flow_expression_shape(
                    target,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
            InspectOperationNode::Projection { snapshot, .. } => {
                validate_control_flow_expression_shape(
                    snapshot,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
        },
        ClientExpressionNode::LocalRead { local } => {
            let declaration = locals
                .iter()
                .find(|candidate| candidate.local == *local)
                .ok_or(ClientPlanError::UnknownControlFlowLocal(*local))?;
            if !initialized.contains(local) {
                return Err(ClientPlanError::ControlFlowLocalReadBeforeLet(*local));
            }
            if value_position && matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                return Err(ClientPlanError::UnawaitedResourceLocal(*local));
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            for (_, value) in arguments {
                validate_control_flow_expression_shape(
                    value,
                    locals,
                    initialized,
                    false,
                    false,
                    true,
                )?;
            }
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            validate_control_flow_expression_shape(left, locals, initialized, false, false, true)?;
            validate_control_flow_expression_shape(right, locals, initialized, false, false, true)?;
        }
        ClientExpressionNode::Unary { expression, .. } => {
            validate_control_flow_expression_shape(
                expression,
                locals,
                initialized,
                false,
                false,
                true,
            )?;
        }
        ClientExpressionNode::Input => {}
        ClientExpressionNode::Evaluate { expression } => {
            validate_control_flow_expression_shape(
                expression,
                locals,
                initialized,
                false,
                false,
                true,
            )?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
    Ok(())
}

fn validate_control_flow_resource_operation(
    operation: &ResourceOperationNode,
    locals: &[ClientLocal],
    initialized: &[LocalId],
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        validate_resource_identity(identity)?;
    }
    validate_resource_arguments(&operation.arguments)?;
    for (_, value) in &operation.arguments {
        validate_control_flow_expression_shape(value, locals, initialized, false, false, true)?;
    }
    Ok(())
}

fn validate_control_flow_action_operation(
    operation: &ActionOperationNode,
    locals: &[ClientLocal],
    initialized: &[LocalId],
) -> Result<(), ClientPlanError> {
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    validate_action_arguments(&operation.arguments)?;
    for (_, value) in &operation.arguments {
        validate_control_flow_expression_shape(value, locals, initialized, false, false, true)?;
    }
    Ok(())
}

fn expression_contains_inspect(node: &ClientExpressionNode) -> bool {
    match node {
        ClientExpressionNode::Inspect { operation } => match operation {
            InspectOperationNode::Snapshot { .. } | InspectOperationNode::Projection { .. } => true,
        },
        ClientExpressionNode::Await { expression } => expression_contains_inspect(expression),
        ClientExpressionNode::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| expression_contains_inspect(value)),
        ClientExpressionNode::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| expression_contains_inspect(value)),
        ClientExpressionNode::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, value)| expression_contains_inspect(value)),
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            expression_contains_inspect(left) || expression_contains_inspect(right)
        }
        ClientExpressionNode::Unary { expression, .. } => expression_contains_inspect(expression),
        ClientExpressionNode::Input => false,
        ClientExpressionNode::Evaluate { expression } => expression_contains_inspect(expression),
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => false,
    }
}

fn validate_external_contract_identity(identity: &str) -> Result<(), ClientPlanError> {
    let invalid = || {
        Err(ClientPlanError::InvalidExpressionNode(
            NODE_EXTERNAL_CONTRACT,
        ))
    };

    let Some((name, revision)) = identity.rsplit_once('@') else {
        return invalid();
    };
    if name.is_empty() || name.contains('@') {
        return invalid();
    }
    if revision
        .parse::<u64>()
        .ok()
        .is_none_or(|revision| revision == 0)
    {
        return invalid();
    }

    for segment in name.split(".") {
        if !valid_external_contract_name_segment(segment) {
            return invalid();
        }
    }

    Ok(())
}

fn validate_external_contract_placement(
    node: &ClientExpressionNode,
    allow_root: bool,
) -> Result<(), ClientPlanError> {
    let mut count = 0;
    validate_external_contract_placement_inner(node, allow_root, 0, &mut count)
}

fn validate_external_contract_placement_inner(
    node: &ClientExpressionNode,
    allow_root: bool,
    depth: usize,
    count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *count += 1;
    if *count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }
    match node {
        ClientExpressionNode::ExternalContract { .. } => {
            if allow_root {
                Ok(())
            } else {
                Err(ClientPlanError::InvalidExpressionNode(
                    NODE_EXTERNAL_CONTRACT,
                ))
            }
        }
        ClientExpressionNode::Await { expression } => {
            validate_external_contract_placement_inner(expression, false, depth + 1, count)
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, value) in &operation.arguments {
                validate_external_contract_placement_inner(value, false, depth + 1, count)?;
            }
            Ok(())
        }
        ClientExpressionNode::Inspect { operation } => {
            match operation {
                InspectOperationNode::Snapshot { target, options } => {
                    validate_external_contract_placement_inner(target, false, depth + 1, count)?;
                    if let Some(options) = options {
                        validate_external_contract_placement_inner(
                            options,
                            false,
                            depth + 1,
                            count,
                        )?;
                    }
                }
                InspectOperationNode::Projection { snapshot, .. } => {
                    validate_external_contract_placement_inner(snapshot, false, depth + 1, count)?;
                }
            }
            Ok(())
        }
        ClientExpressionNode::Action { operation } => {
            for (_, value) in &operation.arguments {
                validate_external_contract_placement_inner(value, false, depth + 1, count)?;
            }
            Ok(())
        }
        ClientExpressionNode::Call { arguments, .. } => {
            for (_, value) in arguments {
                validate_external_contract_placement_inner(value, false, depth + 1, count)?;
            }
            Ok(())
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            validate_external_contract_placement_inner(left, false, depth + 1, count)?;
            validate_external_contract_placement_inner(right, false, depth + 1, count)
        }
        ClientExpressionNode::Unary { expression, .. } => {
            validate_external_contract_placement_inner(expression, false, depth + 1, count)
        }
        ClientExpressionNode::Input => Ok(()),
        ClientExpressionNode::Evaluate { expression } => {
            validate_external_contract_placement_inner(expression, false, depth + 1, count)
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::SourceIntrospection => Ok(()),
    }
}

fn valid_external_contract_name_segment(segment: &str) -> bool {
    let quote = '"';
    let underscore = '_';
    if segment.starts_with(quote) {
        if !segment.ends_with(quote) || segment.len() < 2 {
            return false;
        }
        let inner = &segment[1..segment.len() - 1];
        if inner.is_empty() {
            return false;
        }
        let mut characters = inner.chars().peekable();
        while let Some(character) = characters.next() {
            if character == quote {
                if characters.peek() == Some(&quote) {
                    characters.next();
                } else {
                    return false;
                }
            }
        }
        return true;
    }

    let mut characters = segment.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == underscore || first.is_alphabetic())
        && characters.all(|character| {
            character == underscore || character.is_alphabetic() || character.is_numeric()
        })
}

/// Encodes one expression node recursively.
fn encode_expression_node(
    node: &ClientExpressionNode,
    writer: &mut NodeWriter,
    depth: usize,
    count: &mut usize,
) -> Result<(), ClientPlanError> {
    let mut resource_count = 0;
    encode_expression_node_with_resources(
        node,
        writer,
        depth,
        count,
        false,
        false,
        false,
        &mut resource_count,
    )
}

fn encode_expression_node_with_inspect(
    node: &ClientExpressionNode,
    writer: &mut NodeWriter,
    depth: usize,
    count: &mut usize,
    allow_inspect: bool,
) -> Result<(), ClientPlanError> {
    let mut resource_count = 0;
    encode_expression_node_with_resources(
        node,
        writer,
        depth,
        count,
        false,
        false,
        allow_inspect,
        &mut resource_count,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_expression_node_with_resources(
    node: &ClientExpressionNode,
    writer: &mut NodeWriter,
    depth: usize,
    count: &mut usize,
    allow_resources: bool,
    allow_local: bool,
    allow_inspect: bool,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *count += 1;
    if *count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }
    match node {
        ClientExpressionNode::Await { expression } => {
            if !allow_resources {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            writer.push(NODE_AWAIT);
            encode_expression_node_with_resources(
                expression,
                writer,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
        }
        ClientExpressionNode::Resource { operation } => {
            if !allow_resources {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            encode_resource_operation(
                operation,
                writer,
                depth,
                count,
                allow_local,
                resource_count,
            )?;
        }
        ClientExpressionNode::Action { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_ACTION));
        }
        ClientExpressionNode::Inspect { operation } => {
            if !allow_inspect {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_INSPECT));
            }
            writer.push(NODE_INSPECT);
            match operation {
                InspectOperationNode::Snapshot { target, options } => {
                    writer.push(INSPECT_OPERATION_SNAPSHOT);
                    encode_expression_node_with_resources(
                        target,
                        writer,
                        depth + 1,
                        count,
                        allow_resources,
                        allow_local,
                        allow_inspect,
                        resource_count,
                    )?;
                    match options {
                        None => writer.push(0),
                        Some(_) => return Err(ClientPlanError::UnsupportedInspectOptions),
                    }
                }
                InspectOperationNode::Projection {
                    projection,
                    snapshot,
                } => {
                    writer.push(INSPECT_OPERATION_PROJECTION);
                    writer.push(projection.tag());
                    encode_expression_node_with_resources(
                        snapshot,
                        writer,
                        depth + 1,
                        count,
                        allow_resources,
                        allow_local,
                        allow_inspect,
                        resource_count,
                    )?;
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            writer.push(NODE_CALL);
            writer.extend(&function.to_bytes());
            let length = u32::try_from(arguments.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for (parameter, value) in arguments {
                writer.extend(&parameter.to_bytes());
                encode_expression_node_with_resources(
                    value,
                    writer,
                    depth + 1,
                    count,
                    allow_resources,
                    allow_local,
                    allow_inspect,
                    resource_count,
                )?;
            }
        }
        ClientExpressionNode::String { value } => {
            writer.push(NODE_STRING);
            let bytes = value.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::Integer { value } => {
            writer.push(NODE_INTEGER);
            writer.extend(&value.to_be_bytes());
        }
        ClientExpressionNode::Boolean { value } => {
            writer.push(NODE_BOOLEAN);
            writer.push(u8::from(*value));
        }
        ClientExpressionNode::ParameterRead { parameter } => {
            writer.push(NODE_PARAMETER_READ);
            writer.extend(&parameter.to_bytes());
        }
        ClientExpressionNode::LocalRead { local } => {
            if !allow_local {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_LOCAL_READ));
            }
            writer.push(NODE_LOCAL_READ);
            writer.extend(&local.to_bytes());
        }
        ClientExpressionNode::FieldPath { root, fields } => {
            if fields.is_empty() || fields.len() > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            writer.push(NODE_FIELD_PATH);
            writer.extend(&root.to_bytes());
            let length = u32::try_from(fields.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            for field in fields {
                writer.extend(&field.to_bytes());
            }
        }
        ClientExpressionNode::Concat { left, right } => {
            writer.push(NODE_CONCAT);
            encode_expression_node_with_resources(
                left,
                writer,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
            encode_expression_node_with_resources(
                right,
                writer,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
        }
        ClientExpressionNode::ExternalContract { identity } => {
            validate_external_contract_identity(identity)?;
            writer.push(NODE_EXTERNAL_CONTRACT);
            let bytes = identity.as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| {
                ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_ARTIFACT_BYTES,
                }
            })?;
            writer.extend(&length.to_be_bytes());
            writer.extend(bytes);
        }
        ClientExpressionNode::SourceIntrospection => writer.push(NODE_SOURCE_INTROSPECTION),
        ClientExpressionNode::Input => writer.push(NODE_INPUT),
        ClientExpressionNode::Evaluate { expression } => {
            writer.push(NODE_EVALUATE);
            encode_expression_node_with_resources(
                expression,
                writer,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
        }
        ClientExpressionNode::Unary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_UNARY));
        }
        ClientExpressionNode::Binary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_BINARY));
        }
    }
    Ok(())
}

fn encode_resource_operation(
    operation: &ResourceOperationNode,
    writer: &mut NodeWriter,
    depth: usize,
    count: &mut usize,
    allow_local: bool,
    resource_count: &mut usize,
) -> Result<(), ClientPlanError> {
    *resource_count += 1;
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        validate_resource_identity(identity)?;
    }
    validate_resource_arguments(&operation.arguments)?;
    writer.push(NODE_RESOURCE);
    writer.push(operation.kind.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    let argument_count = u32::try_from(operation.arguments.len()).map_err(|_| {
        ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        }
    })?;
    writer.extend(&argument_count.to_be_bytes());
    for (parameter, value) in &operation.arguments {
        writer.extend(&parameter.to_bytes());
        encode_expression_node_with_resources(
            value,
            writer,
            depth + 1,
            count,
            true,
            allow_local,
            false,
            resource_count,
        )?;
    }
    writer.extend(&operation.result_type.to_bytes());
    Ok(())
}

fn encode_action_plan(plan: &ActionClientPlan) -> Result<Vec<u8>, ClientPlanError> {
    let operation = &plan.operation;
    for identity in [
        operation.target.to_bytes(),
        operation.target_revision.source().to_bytes(),
        operation.target_revision.catalogue().to_bytes(),
        operation.call_site.to_bytes(),
        operation.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    validate_action_arguments(&operation.arguments)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&ACTION_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_ACTION_OPERATION);
    let mut writer = NodeWriter::new();
    writer.push(operation.domain.tag());
    writer.extend(&operation.target.to_bytes());
    writer.extend(&operation.target_revision.source().to_bytes());
    writer.extend(&operation.target_revision.catalogue().to_bytes());
    writer.extend(&operation.call_site.to_bytes());
    writer.extend(&operation.result_type.to_bytes());
    writer.extend(&(operation.arguments.len() as u32).to_be_bytes());
    let mut expression_count = 0;
    for (parameter, value) in &operation.arguments {
        writer.extend(&parameter.to_bytes());
        encode_expression_node(value, &mut writer, 0, &mut expression_count)?;
    }
    bytes.extend_from_slice(&writer.finish());
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(bytes)
}

fn decode_action_plan(bytes: &[u8]) -> Result<ActionClientPlan, ClientPlanError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ClientPlanError::ArtifactSizeLimit {
            size: bytes.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    let mut reader = Reader::new(bytes);
    if reader.array::<8>()? != MAGIC {
        return Err(ClientPlanError::InvalidMagic);
    }
    let version = reader.u32()?;
    if version != ACTION_FORMAT_VERSION {
        return Err(ClientPlanError::UnsupportedVersion(version));
    }
    let operation = reader.u8()?;
    if operation != RETURN_ACTION_OPERATION {
        return Err(ClientPlanError::InvalidOperation(operation));
    }
    let domain = match reader.u8()? {
        1 => ActionTargetDomain::Client,
        2 => ActionTargetDomain::Server,
        tag => return Err(ClientPlanError::InvalidActionDomain(tag)),
    };
    let target = FunctionId::from_bytes(read_action_identity(&mut reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_action_identity(&mut reader)?),
        CatalogueRevisionId::from_bytes(read_action_identity(&mut reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_action_identity(&mut reader)?);
    let result_type = TypeId::from_bytes(read_action_identity(&mut reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_ACTION_ARGUMENTS {
        return Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    let mut expression_count = 0;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_action_identity(&mut reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalActionArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateActionArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value = decode_expression_node(&mut reader, 0, &mut expression_count)?;
        validate_external_contract_placement(&value, false)?;
        arguments.push((parameter, value));
    }
    reader.require_finished()?;
    Ok(ActionClientPlan::new(ActionOperationNode::new(
        domain,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    )))
}

fn read_action_identity(reader: &mut Reader<'_>) -> Result<[u8; 16], ClientPlanError> {
    let identity = reader.array()?;
    if identity == [0; 16] {
        return Err(ClientPlanError::InvalidActionIdentity);
    }
    Ok(identity)
}

fn validate_action_arguments(
    arguments: &[(ParameterId, ClientExpressionNode)],
) -> Result<(), ClientPlanError> {
    if arguments.len() > MAX_ACTION_ARGUMENTS {
        return Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS,
        });
    }
    for (parameter, _) in arguments {
        if parameter.to_bytes() == [0; 16] {
            return Err(ClientPlanError::InvalidActionIdentity);
        }
    }
    let mut previous = None;
    for (parameter, _) in arguments {
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalActionArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateActionArgument(*parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(*parameter);
    }
    for (_, value) in arguments {
        validate_external_contract_placement(value, false)?;
    }
    Ok(())
}

fn validate_resource_arguments(
    arguments: &[(ParameterId, ClientExpressionNode)],
) -> Result<(), ClientPlanError> {
    if arguments.len() > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut previous = None;
    for (parameter, _) in arguments {
        validate_resource_identity(parameter.to_bytes())?;
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalResourceArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateResourceArgument(*parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(*parameter);
    }
    Ok(())
}

fn validate_procedural_model(plan: &ProceduralClientPlan) -> Result<(), ClientPlanError> {
    if plan.locals.len() > MAX_PROCEDURAL_LOCALS {
        return Err(ClientPlanError::ProceduralLocalLimitExceeded {
            limit: MAX_PROCEDURAL_LOCALS,
        });
    }
    if plan.statements.len() > MAX_PROCEDURAL_STATEMENTS {
        return Err(ClientPlanError::ProceduralStatementLimitExceeded {
            limit: MAX_PROCEDURAL_STATEMENTS,
        });
    }
    let mut locals = Vec::with_capacity(plan.locals.len());
    for local in &plan.locals {
        if locals
            .iter()
            .any(|candidate: &ClientLocal| candidate.local == local.local)
        {
            return Err(ClientPlanError::DuplicateProceduralLocal(local.local));
        }
        locals.push(*local);
    }
    let mut initialized_locals = Vec::with_capacity(locals.len());
    for statement in &plan.statements {
        let target = locals
            .iter()
            .find(|candidate| candidate.local == statement.local())
            .ok_or(ClientPlanError::UnknownProceduralLocal(statement.local()))?;
        let value_position = matches!(target.kind, ClientLocalKind::Value);
        validate_external_contract_placement(statement.expression(), false)?;
        validate_procedural_expression(
            statement.expression(),
            &locals,
            true,
            true,
            value_position,
        )?;
        if matches!(statement, ClientStatement::Assignment { .. })
            && !initialized_locals.contains(&statement.local())
        {
            return Err(ClientPlanError::ProceduralAssignmentBeforeLet(
                statement.local(),
            ));
        }
        validate_procedural_local_reads(statement.expression(), &initialized_locals)?;
        if matches!(target.kind, ClientLocalKind::Value) {
            let awaited_type = match statement.expression() {
                ClientExpressionNode::Await { expression } => match expression.as_ref() {
                    ClientExpressionNode::Resource { operation } => Some(operation.result_type()),
                    ClientExpressionNode::LocalRead { local } => locals
                        .iter()
                        .find(|candidate| candidate.local == *local)
                        .and_then(|source| match source.kind {
                            ClientLocalKind::Resource(_) => Some(source.type_id),
                            ClientLocalKind::Value => None,
                        }),
                    _ => None,
                },
                _ => None,
            };
            if let Some(actual) = awaited_type
                && actual != target.type_id
            {
                return Err(ClientPlanError::ProceduralLocalTypeMismatch {
                    local: target.local,
                    expected: target.type_id,
                    actual,
                });
            }
        }
        let expression_kind = procedural_resource_kind(statement.expression(), &locals);
        match target.kind {
            ClientLocalKind::Value if expression_kind.is_some() => {
                return Err(ClientPlanError::ProceduralLocalKindMismatch(target.local));
            }
            ClientLocalKind::Resource(expected) if expression_kind != Some(expected) => {
                return Err(ClientPlanError::ProceduralLocalKindMismatch(target.local));
            }
            ClientLocalKind::Value | ClientLocalKind::Resource(_) => {}
        }
        match (target.kind, statement.expression()) {
            (ClientLocalKind::Value, ClientExpressionNode::LocalRead { local }) => {
                let source = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownProceduralLocal(*local))?;
                if source.type_id != target.type_id {
                    return Err(ClientPlanError::ProceduralLocalTypeMismatch {
                        local: target.local,
                        expected: target.type_id,
                        actual: source.type_id,
                    });
                }
            }
            (ClientLocalKind::Resource(_), ClientExpressionNode::Resource { operation })
                if operation.result_type() != target.type_id =>
            {
                return Err(ClientPlanError::ProceduralLocalTypeMismatch {
                    local: target.local,
                    expected: target.type_id,
                    actual: operation.result_type(),
                });
            }
            (ClientLocalKind::Resource(_), ClientExpressionNode::LocalRead { local }) => {
                let source = locals
                    .iter()
                    .find(|candidate| candidate.local == *local)
                    .ok_or(ClientPlanError::UnknownProceduralLocal(*local))?;
                if source.type_id != target.type_id {
                    return Err(ClientPlanError::ProceduralLocalTypeMismatch {
                        local: target.local,
                        expected: target.type_id,
                        actual: source.type_id,
                    });
                }
            }
            _ => {}
        }
        if matches!(statement, ClientStatement::Let { .. }) {
            initialized_locals.push(statement.local());
        }
    }
    let mut let_locals = Vec::with_capacity(locals.len());
    for statement in &plan.statements {
        match statement {
            ClientStatement::Let { local, .. } => {
                if let_locals.contains(local) {
                    return Err(ClientPlanError::DuplicateProceduralLet(*local));
                }
                let_locals.push(*local);
            }
            ClientStatement::Assignment { local, .. } => {
                if !let_locals.contains(local) {
                    return Err(ClientPlanError::ProceduralAssignmentBeforeLet(*local));
                }
            }
        }
    }
    for local in &locals {
        if !let_locals.contains(&local.local) {
            return Err(ClientPlanError::MissingProceduralLet(local.local));
        }
    }
    // A final return must produce a value; a resource handle has to be awaited.
    validate_external_contract_placement(&plan.return_expression, false)?;
    validate_procedural_expression(&plan.return_expression, &locals, false, true, true)?;
    validate_procedural_local_reads(&plan.return_expression, &initialized_locals)
}

fn validate_procedural_local_reads(
    node: &ClientExpressionNode,
    initialized_locals: &[LocalId],
) -> Result<(), ClientPlanError> {
    match node {
        ClientExpressionNode::Await { expression } => {
            validate_procedural_local_reads(expression, initialized_locals)?;
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, value) in &operation.arguments {
                validate_procedural_local_reads(value, initialized_locals)?;
            }
        }
        ClientExpressionNode::Inspect { operation } => match operation {
            InspectOperationNode::Snapshot { target, options } => {
                validate_procedural_local_reads(target, initialized_locals)?;
                if let Some(options) = options {
                    validate_procedural_local_reads(options, initialized_locals)?;
                }
            }
            InspectOperationNode::Projection { snapshot, .. } => {
                validate_procedural_local_reads(snapshot, initialized_locals)?;
            }
        },
        ClientExpressionNode::Action { operation } => {
            for (_, value) in &operation.arguments {
                validate_procedural_local_reads(value, initialized_locals)?;
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            for (_, value) in arguments {
                validate_procedural_local_reads(value, initialized_locals)?;
            }
        }
        ClientExpressionNode::LocalRead { local } => {
            if !initialized_locals.contains(local) {
                return Err(ClientPlanError::ProceduralLocalReadBeforeLet(*local));
            }
        }
        ClientExpressionNode::Concat { left, right } => {
            validate_procedural_local_reads(left, initialized_locals)?;
            validate_procedural_local_reads(right, initialized_locals)?;
        }
        ClientExpressionNode::Unary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_UNARY));
        }
        ClientExpressionNode::Binary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_BINARY));
        }
        ClientExpressionNode::Input => {}
        ClientExpressionNode::Evaluate { expression } => {
            validate_procedural_local_reads(expression, initialized_locals)?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
    Ok(())
}

fn procedural_resource_kind(
    node: &ClientExpressionNode,
    locals: &[ClientLocal],
) -> Option<ResourceKind> {
    match node {
        ClientExpressionNode::Resource { operation } => Some(operation.kind()),
        ClientExpressionNode::LocalRead { local } => locals
            .iter()
            .find(|candidate| candidate.local == *local)
            .and_then(|candidate| match candidate.kind {
                ClientLocalKind::Resource(kind) => Some(kind),
                ClientLocalKind::Value => None,
            }),
        _ => None,
    }
}

fn validate_procedural_expression(
    node: &ClientExpressionNode,
    locals: &[ClientLocal],
    allow_resource_root: bool,
    allow_await_root: bool,
    value_position: bool,
) -> Result<(), ClientPlanError> {
    match node {
        ClientExpressionNode::Await { expression } => {
            if !allow_await_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            match expression.as_ref() {
                ClientExpressionNode::Resource { operation } => {
                    validate_resource_arguments(&operation.arguments)?;
                    for (_, value) in &operation.arguments {
                        validate_procedural_expression(value, locals, false, false, true)?;
                    }
                }
                ClientExpressionNode::LocalRead { local } => {
                    let declaration = locals
                        .iter()
                        .find(|candidate| candidate.local == *local)
                        .ok_or(ClientPlanError::UnknownProceduralLocal(*local))?;
                    if !matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                        return Err(ClientPlanError::InvalidAwaitOperand(*local));
                    }
                }
                _ => return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT)),
            }
        }
        ClientExpressionNode::Resource { operation } => {
            if !allow_resource_root {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            validate_resource_arguments(&operation.arguments)?;
            for (_, value) in &operation.arguments {
                validate_procedural_expression(value, locals, false, false, true)?;
            }
        }
        ClientExpressionNode::LocalRead { local } => {
            let declaration = locals
                .iter()
                .find(|candidate| candidate.local == *local)
                .ok_or(ClientPlanError::UnknownProceduralLocal(*local))?;
            if value_position && matches!(declaration.kind, ClientLocalKind::Resource(_)) {
                return Err(ClientPlanError::UnawaitedResourceLocal(*local));
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            if arguments.len() > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            for (_, value) in arguments {
                validate_procedural_expression(value, locals, false, false, true)?;
            }
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            validate_procedural_expression(left, locals, false, false, true)?;
            validate_procedural_expression(right, locals, false, false, true)?;
        }
        ClientExpressionNode::Unary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_UNARY));
        }
        ClientExpressionNode::Input => {}
        ClientExpressionNode::Evaluate { expression } => {
            validate_procedural_expression(expression, locals, false, false, true)?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
        ClientExpressionNode::Action { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_ACTION));
        }
        ClientExpressionNode::Inspect { operation } => match operation {
            InspectOperationNode::Snapshot { target, options } => {
                if options.is_some() {
                    return Err(ClientPlanError::UnsupportedInspectOptions);
                }
                validate_procedural_expression(target, locals, false, false, true)?;
            }
            InspectOperationNode::Projection { snapshot, .. } => {
                validate_procedural_expression(snapshot, locals, false, false, true)?;
            }
        },
    }
    Ok(())
}

/// Verifies the closed placement rule for version-six resource expressions.
///
/// A resource constructor is only meaningful as the direct operand of one
/// root `AWAIT`. Keep this check at the artifact boundary as well as in the
/// compiler/runtime: decoded bytes are untrusted and the runtime must not
/// receive a bare resource value, an `AWAIT` over an ordinary expression, or
/// a suspension nested inside another expression.
fn validate_resource_await_placement(
    node: &ClientExpressionNode,
    allow_await: bool,
    awaited_operand: bool,
) -> Result<(), ClientPlanError> {
    match node {
        ClientExpressionNode::Await { expression } => {
            if !allow_await || !matches!(expression.as_ref(), ClientExpressionNode::Resource { .. })
            {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            validate_resource_await_placement(expression, false, true)?;
        }
        ClientExpressionNode::Resource { operation } => {
            if !awaited_operand {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            for (_, value) in &operation.arguments {
                validate_resource_await_placement(value, false, false)?;
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            for (_, value) in arguments {
                validate_resource_await_placement(value, false, false)?;
            }
        }
        ClientExpressionNode::Concat { left, right } => {
            validate_resource_await_placement(left, false, false)?;
            validate_resource_await_placement(right, false, false)?;
        }
        ClientExpressionNode::Input => {}
        ClientExpressionNode::Evaluate { expression } => {
            validate_resource_await_placement(expression, false, false)?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
        ClientExpressionNode::Action { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_ACTION));
        }
        ClientExpressionNode::Inspect { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_INSPECT));
        }
        ClientExpressionNode::Unary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_UNARY));
        }
        ClientExpressionNode::Binary { .. } => {
            return Err(ClientPlanError::InvalidExpressionNode(NODE_BINARY));
        }
    }
    Ok(())
}

/// Decodes one expression node recursively with the closed limits.
fn decode_expression_node(
    reader: &mut Reader<'_>,
    depth: usize,
    count: &mut usize,
) -> Result<ClientExpressionNode, ClientPlanError> {
    decode_expression_node_with_inspect(reader, depth, count, false)
}

fn decode_expression_node_with_inspect(
    reader: &mut Reader<'_>,
    depth: usize,
    count: &mut usize,
    allow_inspect: bool,
) -> Result<ClientExpressionNode, ClientPlanError> {
    let mut resource_count = 0;
    decode_expression_node_with_resources(
        reader,
        depth,
        count,
        false,
        false,
        allow_inspect,
        &mut resource_count,
    )
}

fn decode_expression_node_with_resources(
    reader: &mut Reader<'_>,
    depth: usize,
    count: &mut usize,
    allow_resources: bool,
    allow_local: bool,
    allow_inspect: bool,
    resource_count: &mut usize,
) -> Result<ClientExpressionNode, ClientPlanError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ClientPlanError::ExpressionDepthExceeded);
    }
    *count += 1;
    if *count > MAX_EXPRESSION_NODES {
        return Err(ClientPlanError::ExpressionNodeCountExceeded);
    }
    let tag = reader.u8()?;
    match tag {
        NODE_AWAIT => {
            if !allow_resources {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT));
            }
            Ok(ClientExpressionNode::Await {
                expression: Box::new(decode_expression_node_with_resources(
                    reader,
                    depth + 1,
                    count,
                    allow_resources,
                    allow_local,
                    allow_inspect,
                    resource_count,
                )?),
            })
        }
        NODE_RESOURCE => {
            if !allow_resources {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE));
            }
            Ok(ClientExpressionNode::Resource {
                operation: decode_resource_operation(
                    reader,
                    depth,
                    count,
                    allow_local,
                    resource_count,
                )?,
            })
        }
        NODE_INSPECT => {
            if !allow_inspect {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_INSPECT));
            }
            let operation = match reader.u8()? {
                INSPECT_OPERATION_SNAPSHOT => InspectOperationNode::Snapshot {
                    target: Box::new(decode_expression_node_with_resources(
                        reader,
                        depth + 1,
                        count,
                        allow_resources,
                        allow_local,
                        allow_inspect,
                        resource_count,
                    )?),
                    options: match reader.u8()? {
                        0 => None,
                        1 => return Err(ClientPlanError::UnsupportedInspectOptions),
                        tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
                    },
                },
                INSPECT_OPERATION_PROJECTION => {
                    let projection = InspectProjection::from_tag(reader.u8()?)?;
                    InspectOperationNode::Projection {
                        projection,
                        snapshot: Box::new(decode_expression_node_with_resources(
                            reader,
                            depth + 1,
                            count,
                            allow_resources,
                            allow_local,
                            allow_inspect,
                            resource_count,
                        )?),
                    }
                }
                tag => return Err(ClientPlanError::InvalidInspectOperation(tag)),
            };
            Ok(ClientExpressionNode::Inspect { operation })
        }
        NODE_CALL => {
            let function = FunctionId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length > MAX_CALL_ARGUMENTS {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_CALL_ARGUMENTS,
                });
            }
            let mut arguments = Vec::with_capacity(length);
            for _ in 0..length {
                let parameter = ParameterId::from_bytes(reader.array()?);
                let value = decode_expression_node_with_resources(
                    reader,
                    depth + 1,
                    count,
                    allow_resources,
                    allow_local,
                    allow_inspect,
                    resource_count,
                )?;
                arguments.push((parameter, value));
            }
            Ok(ClientExpressionNode::Call {
                function,
                arguments,
            })
        }
        NODE_STRING => {
            let length = reader.u32()? as usize;
            let bytes = reader.take(length)?;
            let value = std::str::from_utf8(bytes)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_STRING))?
                .to_owned();
            Ok(ClientExpressionNode::String { value })
        }
        NODE_INTEGER => {
            let value = i64::from_be_bytes(reader.array()?);
            Ok(ClientExpressionNode::Integer { value })
        }
        NODE_BOOLEAN => {
            let value = reader.u8()?;
            match value {
                0 => Ok(ClientExpressionNode::Boolean { value: false }),
                1 => Ok(ClientExpressionNode::Boolean { value: true }),
                _ => Err(ClientPlanError::InvalidExpressionNode(NODE_BOOLEAN)),
            }
        }
        NODE_PARAMETER_READ => {
            let parameter = ParameterId::from_bytes(reader.array()?);
            Ok(ClientExpressionNode::ParameterRead { parameter })
        }
        NODE_LOCAL_READ => {
            if !allow_local {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_LOCAL_READ));
            }
            Ok(ClientExpressionNode::LocalRead {
                local: LocalId::from_bytes(reader.array()?),
            })
        }
        NODE_FIELD_PATH => {
            let root = ParameterId::from_bytes(reader.array()?);
            let length = reader.u32()? as usize;
            if length == 0 {
                return Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH));
            }
            if length > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                });
            }
            let mut fields = Vec::with_capacity(length);
            for _ in 0..length {
                fields.push(FieldId::from_bytes(reader.array()?));
            }
            Ok(ClientExpressionNode::FieldPath { root, fields })
        }
        NODE_CONCAT => {
            let left = decode_expression_node_with_resources(
                reader,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
            let right = decode_expression_node_with_resources(
                reader,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?;
            Ok(ClientExpressionNode::Concat {
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        NODE_EXTERNAL_CONTRACT => {
            let length = reader.u32()? as usize;
            let bytes = reader.take(length)?;
            let identity = std::str::from_utf8(bytes)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_EXTERNAL_CONTRACT))?;
            validate_external_contract_identity(identity)?;
            Ok(ClientExpressionNode::ExternalContract {
                identity: identity.to_owned(),
            })
        }
        NODE_SOURCE_INTROSPECTION => Ok(ClientExpressionNode::SourceIntrospection),
        NODE_INPUT => Ok(ClientExpressionNode::Input),
        NODE_EVALUATE => Ok(ClientExpressionNode::Evaluate {
            expression: Box::new(decode_expression_node_with_resources(
                reader,
                depth + 1,
                count,
                allow_resources,
                allow_local,
                allow_inspect,
                resource_count,
            )?),
        }),
        tag => Err(ClientPlanError::InvalidExpressionNode(tag)),
    }
}

fn decode_resource_operation(
    reader: &mut Reader<'_>,
    depth: usize,
    count: &mut usize,
    allow_local: bool,
    resource_count: &mut usize,
) -> Result<ResourceOperationNode, ClientPlanError> {
    *resource_count += 1;
    if *resource_count > MAX_RESOURCE_OPERATIONS {
        return Err(ClientPlanError::ResourceOperationLimitExceeded {
            limit: MAX_RESOURCE_OPERATIONS,
        });
    }
    let kind = match reader.u8()? {
        RESOURCE_KIND_SCALAR => ResourceKind::Scalar,
        RESOURCE_KIND_STREAM => ResourceKind::Stream,
        tag => return Err(ClientPlanError::InvalidResourceKind(tag)),
    };
    let target = FunctionId::from_bytes(read_resource_identity(reader)?);
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes(read_resource_identity(reader)?),
        CatalogueRevisionId::from_bytes(read_resource_identity(reader)?),
    );
    let call_site = CallSiteId::from_bytes(read_resource_identity(reader)?);
    let argument_count = reader.u32()? as usize;
    if argument_count > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    let mut previous = None;
    for _ in 0..argument_count {
        let parameter = ParameterId::from_bytes(read_resource_identity(reader)?);
        if let Some(previous) = previous {
            match parameter.cmp(&previous) {
                std::cmp::Ordering::Less => {
                    return Err(ClientPlanError::NonCanonicalResourceArgumentOrder);
                }
                std::cmp::Ordering::Equal => {
                    return Err(ClientPlanError::DuplicateResourceArgument(parameter));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(parameter);
        let value = decode_expression_node_with_resources(
            reader,
            depth + 1,
            count,
            true,
            allow_local,
            false,
            resource_count,
        )?;
        arguments.push((parameter, value));
    }
    let result_type = TypeId::from_bytes(read_resource_identity(reader)?);
    Ok(ResourceOperationNode::new(
        kind,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    ))
}
fn validate_resource_identity(identity: [u8; 16]) -> Result<(), ClientPlanError> {
    if identity == [0; 16] {
        return Err(ClientPlanError::InvalidResourceIdentity);
    }
    Ok(())
}

fn read_resource_identity(reader: &mut Reader<'_>) -> Result<[u8; 16], ClientPlanError> {
    let identity = reader.array()?;
    validate_resource_identity(identity)?;
    Ok(identity)
}

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

#[cfg(test)]
mod tests;
