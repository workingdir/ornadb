//! Canonical `orna.client-plan` artefact formats, versions 1, 2, 3, and 4.
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
//! payload length: u32 big-endian = 16
//! canonical payload[16]
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
//! Version 5 (work ADR 0060) wraps one complete version 1-4 client plan
//! with the owning function's ordered, closed capability requirements:
//!
//! ```text
//! magic[8] = ORNACP\0\0
//! version: u32 big-endian = 5
//! operation: u8 = 5
//! inner plan version: u32 big-endian = 1|2|3|4
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
//! The version 1-4 formats contain no source text, source locations, Orna
//! names, or backend values.

use std::fmt;

use orna_core::{FieldId, FunctionId, ParameterId, StateSlotId, TypeId};

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
const CAPABILITY_ARGUMENT_TEXT: u8 = 1;
const CAPABILITY_ARGUMENT_PARAMETER: u8 = 2;
const ENCODED_LENGTH: usize = MAGIC.len() + size_of::<u32>() + 2;
const OPAQUE_PAYLOAD_LENGTH: usize = 16;
const OPAQUE_ENCODED_LENGTH: usize =
    MAGIC.len() + size_of::<u32>() + 1 + 16 + size_of::<u32>() + OPAQUE_PAYLOAD_LENGTH;

const NODE_CALL: u8 = 1;
const NODE_STRING: u8 = 2;
const NODE_INTEGER: u8 = 3;
const NODE_BOOLEAN: u8 = 4;
const NODE_PARAMETER_READ: u8 = 5;
const NODE_FIELD_PATH: u8 = 6;
const NODE_CONCAT: u8 = 7;
const NODE_EXTERNAL_CONTRACT: u8 = 8;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpaqueClientPlan {
    opaque_type: TypeId,
    canonical_payload: [u8; OPAQUE_PAYLOAD_LENGTH],
}

impl OpaqueClientPlan {
    /// Creates a checked plan from one nominal type and complete canonical payload.
    pub const fn return_opaque(
        opaque_type: TypeId,
        canonical_payload: [u8; OPAQUE_PAYLOAD_LENGTH],
    ) -> Self {
        Self {
            opaque_type,
            canonical_payload,
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
    pub const fn canonical_payload(&self) -> &[u8; OPAQUE_PAYLOAD_LENGTH] {
        &self.canonical_payload
    }

    /// Encodes this plan into its exact version-2 bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(OPAQUE_ENCODED_LENGTH);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_OPAQUE_OPERATION);
        bytes.extend_from_slice(&self.opaque_type.to_bytes());
        bytes.extend_from_slice(&(OPAQUE_PAYLOAD_LENGTH as u32).to_be_bytes());
        bytes.extend_from_slice(&self.canonical_payload);
        debug_assert_eq!(bytes.len(), OPAQUE_ENCODED_LENGTH);
        bytes
    }

    /// Decodes exactly one canonical version-2 opaque client-plan artefact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ClientPlanError> {
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
        if payload_length != OPAQUE_PAYLOAD_LENGTH as u32 {
            return Err(ClientPlanError::InvalidOpaquePayloadLength {
                actual: payload_length,
            });
        }
        let canonical_payload = reader.array()?;
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
}

/// One closed CLIENT expression-tree node (work ADR 0068).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientExpressionNode {
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
    /// A read of one declared parameter.
    ParameterRead {
        /// The read parameter identity.
        parameter: ParameterId,
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
}

impl ExpressionClientPlan {
    /// Creates a checked plan from one closed expression tree.
    pub fn new(expression: ClientExpressionNode) -> Self {
        Self { expression }
    }

    /// Returns the closed expression tree.
    pub const fn expression(&self) -> &ClientExpressionNode {
        &self.expression
    }

    /// Returns the canonical artefact version for this plan.
    pub const fn format_version(&self) -> u32 {
        EXPRESSION_FORMAT_VERSION
    }

    /// Encodes this plan into its exact version-3 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_EXPRESSION_OPERATION);
        let mut writer = NodeWriter::new();
        let mut count = 0;
        encode_expression_node(&self.expression, &mut writer, 0, &mut count)?;
        bytes.extend_from_slice(&writer.finish());
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientPlanError::ArtifactSizeLimit {
                size: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-3 expression client-plan.
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
        if version != EXPRESSION_FORMAT_VERSION {
            return Err(ClientPlanError::UnsupportedVersion(version));
        }
        let operation = reader.u8()?;
        if operation != RETURN_EXPRESSION_OPERATION {
            return Err(ClientPlanError::InvalidOperation(operation));
        }
        let mut count = 0usize;
        let expression = decode_expression_node(&mut reader, 0, &mut count, bytes.len())?;
        reader.require_finished()?;
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
                return Err(ClientPlanError::DuplicateStateSlotId(
                    slot.state_slot_id,
                ));
            }
            seen.push(slot.state_slot_id);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
        bytes.push(RETURN_STATE_OPERATION);
        let mut writer = NodeWriter::new();
        let mut count = 0;
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
        let expression = decode_expression_node(&mut reader, 0, &mut count, bytes.len())?;
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
                    StateDefault::Expression(decode_expression_node(
                        &mut reader,
                        0,
                        &mut count,
                        bytes.len(),
                    )?)
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
/// The envelope holds a complete decoded version 1-4 client plan so the
/// runtime can evaluate it directly after the capability gate admits it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InnerClientPlan {
    /// A version-1 Boolean-constant plan.
    Boolean(ClientPlan),
    /// A version-2 opaque-value plan.
    Opaque(OpaqueClientPlan),
    /// A version-3 expression plan.
    Expression(ExpressionClientPlan),
    /// A version-4 state plan.
    State(StateClientPlan),
}

impl InnerClientPlan {
    /// Returns the canonical version of the inner plan.
    pub const fn format_version(&self) -> u32 {
        match self {
            Self::Boolean(_) => FORMAT_VERSION,
            Self::Opaque(_) => OPAQUE_FORMAT_VERSION,
            Self::Expression(_) => EXPRESSION_FORMAT_VERSION,
            Self::State(_) => STATE_FORMAT_VERSION,
        }
    }

    /// Encodes the inner plan into its exact version bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ClientPlanError> {
        match self {
            Self::Boolean(plan) => Ok(plan.encode()),
            Self::Opaque(plan) => Ok(plan.encode()),
            Self::Expression(plan) => plan.encode(),
            Self::State(plan) => plan.encode(),
        }
    }
}

/// A checked version-5 CLIENT plan that carries one version 1-4 inner plan
/// and the owning function's ordered, closed capability requirements
/// (work ADR 0060).
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
        let inner_payload = reader.bytes(inner_payload_length)?;
        let inner_plan = match inner_plan_version {
            FORMAT_VERSION => InnerClientPlan::Boolean(ClientPlan::decode(inner_payload)?),
            OPAQUE_FORMAT_VERSION => {
                InnerClientPlan::Opaque(OpaqueClientPlan::decode(inner_payload)?)
            }
            EXPRESSION_FORMAT_VERSION => {
                InnerClientPlan::Expression(ExpressionClientPlan::decode(inner_payload)?)
            }
            STATE_FORMAT_VERSION => InnerClientPlan::State(StateClientPlan::decode(inner_payload)?),
            version => return Err(ClientPlanError::UnsupportedInnerVersion(version)),
        };
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
            let name = std::str::from_utf8(reader.bytes(name_length)?)
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
            let argument_text = std::str::from_utf8(reader.bytes(argument_length)?)
                .map_err(|_| ClientPlanError::InvalidCapabilityArgumentUtf8)?
                .to_owned();
            let argument = match argument_tag {
                CAPABILITY_ARGUMENT_TEXT => CapabilityArgumentSource::Text(argument_text),
                CAPABILITY_ARGUMENT_PARAMETER => {
                    CapabilityArgumentSource::Parameter(argument_text)
                }
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

/// Encodes one expression node recursively.
fn encode_expression_node(
    node: &ClientExpressionNode,
    writer: &mut NodeWriter,
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
                encode_expression_node(value, writer, depth + 1, count)?;
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
            encode_expression_node(left, writer, depth + 1, count)?;
            encode_expression_node(right, writer, depth + 1, count)?;
        }
        ClientExpressionNode::ExternalContract { identity } => {
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
    }
    Ok(())
}

/// Decodes one expression node recursively with the closed limits.
fn decode_expression_node(
    reader: &mut Reader<'_>,
    depth: usize,
    count: &mut usize,
    total_bytes: usize,
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
                let value = decode_expression_node(reader, depth + 1, count, total_bytes)?;
                arguments.push((parameter, value));
            }
            Ok(ClientExpressionNode::Call {
                function,
                arguments,
            })
        }
        NODE_STRING => {
            let length = reader.u32()? as usize;
            let bytes = reader.bytes(length)?;
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
            let left = decode_expression_node(reader, depth + 1, count, total_bytes)?;
            let right = decode_expression_node(reader, depth + 1, count, total_bytes)?;
            Ok(ClientExpressionNode::Concat {
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        NODE_EXTERNAL_CONTRACT => {
            let length = reader.u32()? as usize;
            let bytes = reader.bytes(length)?;
            let identity = std::str::from_utf8(bytes)
                .map_err(|_| ClientPlanError::InvalidExpressionNode(NODE_EXTERNAL_CONTRACT))?
                .to_owned();
            Ok(ClientExpressionNode::ExternalContract { identity })
        }
        tag => Err(ClientPlanError::InvalidExpressionNode(tag)),
    }
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
    /// A version-2 opaque payload length is not exactly sixteen bytes.
    InvalidOpaquePayloadLength {
        /// The non-canonical length from the artefact.
        actual: u32,
    },
    /// A version-3 expression node uses an unknown tag.
    InvalidExpressionNode(u8),
    /// A version-3 expression tree exceeds the depth cap.
    ExpressionDepthExceeded,
    /// A version-3 expression tree exceeds the node-count cap.
    ExpressionNodeCountExceeded,
    /// A version-3 call or field path exceeds its per-node cap.
    ExpressionCollectionExceeded {
        /// The exceeded limit.
        limit: usize,
    },
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
                write!(formatter, "client-plan state slot count exceeds the limit {limit}")
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
                write!(formatter, "invalid client-plan capability argument tag {tag}")
            }
            Self::UnsupportedInnerVersion(version) => {
                write!(formatter, "unsupported inner client-plan version {version}")
            }
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

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], ClientPlanError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ClientPlanError::Truncated)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ClientPlanError::Truncated)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], ClientPlanError> {
        self.bytes(LENGTH)?
            .try_into()
            .map_err(|_| ClientPlanError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, ClientPlanError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ClientPlanError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn require_finished(&self) -> Result<(), ClientPlanError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ClientPlanError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUE_BYTES: [u8; ENCODED_LENGTH] = *b"ORNACP\0\0\0\0\0\x01\x01\x01";
    const FALSE_BYTES: [u8; ENCODED_LENGTH] = *b"ORNACP\0\0\0\0\0\x01\x01\0";
    const OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x42; 16]);
    const OPAQUE_PAYLOAD: [u8; OPAQUE_PAYLOAD_LENGTH] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    #[test]
    fn encodes_exact_golden_true_and_false_bytes() {
        assert_eq!(ClientPlan::return_boolean(true).encode(), TRUE_BYTES);
        assert_eq!(ClientPlan::return_boolean(false).encode(), FALSE_BYTES);
    }

    #[test]
    fn round_trips_both_boolean_values() {
        for value in [false, true] {
            let plan = ClientPlan::return_boolean(value);
            let decoded = ClientPlan::decode(&plan.encode()).expect("golden plan must decode");
            assert_eq!(decoded, plan);
            assert_eq!(decoded.format_version(), FORMAT_VERSION);
            assert_eq!(decoded.returned_boolean(), value);
        }
    }

    #[test]
    fn opaque_plan_has_exact_version_two_bytes_and_round_trips() {
        let plan = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD);
        let mut expected = b"ORNACP\0\0\0\0\0\x02\x02".to_vec();
        expected.extend_from_slice(&OPAQUE_TYPE.to_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(&OPAQUE_PAYLOAD);

        assert_eq!(expected.len(), 49);
        assert_eq!(plan.encode(), expected);
        assert_eq!(OpaqueClientPlan::decode(&expected), Ok(plan));
        assert_eq!(plan.format_version(), OPAQUE_FORMAT_VERSION);
        assert_eq!(plan.opaque_type(), OPAQUE_TYPE);
        assert_eq!(plan.canonical_payload(), &OPAQUE_PAYLOAD);
    }

    #[test]
    fn client_plan_versions_remain_mutually_closed() {
        let opaque = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD).encode();
        assert_eq!(
            ClientPlan::decode(&opaque),
            Err(ClientPlanError::UnsupportedVersion(OPAQUE_FORMAT_VERSION))
        );
        assert_eq!(
            OpaqueClientPlan::decode(&TRUE_BYTES),
            Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
        );
        for length in 0..OPAQUE_ENCODED_LENGTH {
            assert_eq!(
                OpaqueClientPlan::decode(&opaque[..length]),
                Err(ClientPlanError::Truncated),
                "opaque prefix length {length} must be truncated"
            );
        }
    }

    #[test]
    fn opaque_plan_rejects_operation_length_and_trailing_corruption() {
        let encoded = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD).encode();

        let mut wrong_operation = encoded.clone();
        wrong_operation[12] = RETURN_BOOLEAN_OPERATION;
        assert_eq!(
            OpaqueClientPlan::decode(&wrong_operation),
            Err(ClientPlanError::InvalidOperation(RETURN_BOOLEAN_OPERATION))
        );
        let mut wrong_length = encoded.clone();
        wrong_length[29..33].copy_from_slice(&15_u32.to_be_bytes());
        assert_eq!(
            OpaqueClientPlan::decode(&wrong_length),
            Err(ClientPlanError::InvalidOpaquePayloadLength { actual: 15 })
        );
        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(
            OpaqueClientPlan::decode(&trailing),
            Err(ClientPlanError::TrailingBytes)
        );
    }

    #[test]
    fn rejects_every_truncated_prefix() {
        for length in 0..ENCODED_LENGTH {
            assert_eq!(
                ClientPlan::decode(&TRUE_BYTES[..length]),
                Err(ClientPlanError::Truncated),
                "prefix length {length} must be truncated"
            );
        }
    }

    #[test]
    fn rejects_invalid_magic_version_operation_boolean_and_trailing_bytes() {
        let mut bytes = TRUE_BYTES;
        bytes[0] = b'X';
        assert_eq!(
            ClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidMagic)
        );

        let mut bytes = TRUE_BYTES;
        bytes[11] = 2;
        assert_eq!(
            ClientPlan::decode(&bytes),
            Err(ClientPlanError::UnsupportedVersion(2))
        );

        let mut bytes = TRUE_BYTES;
        bytes[12] = 2;
        assert_eq!(
            ClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidOperation(2))
        );

        let mut bytes = TRUE_BYTES;
        bytes[13] = 2;
        assert_eq!(
            ClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidBoolean(2))
        );

        let mut bytes = TRUE_BYTES.to_vec();
        bytes.push(0);
        assert_eq!(
            ClientPlan::decode(&bytes),
            Err(ClientPlanError::TrailingBytes)
        );
    }

    #[test]
    fn displays_the_public_error_contract() {
        let duplicate_slot = StateSlotId::from_bytes([0x61; 16]);
        let duplicate_display = format!("duplicate client-plan state slot identity {duplicate_slot}");
        let cases = [
            (
                ClientPlanError::InvalidMagic,
                "invalid orna.client-plan artefact magic",
            ),
            (
                ClientPlanError::UnsupportedVersion(2),
                "unsupported orna.client-plan artefact version 2",
            ),
            (
                ClientPlanError::InvalidOperation(7),
                "invalid client-plan operation tag 7",
            ),
            (
                ClientPlanError::InvalidBoolean(3),
                "invalid client-plan Boolean byte 3",
            ),
            (
                ClientPlanError::InvalidOpaquePayloadLength { actual: 15 },
                "invalid client-plan opaque payload length 15",
            ),
            (
                ClientPlanError::Truncated,
                "truncated orna.client-plan artefact",
            ),
            (
                ClientPlanError::TrailingBytes,
                "trailing bytes after orna.client-plan artefact",
            ),
            (
                ClientPlanError::InvalidExpressionNode(9),
                "invalid client-plan expression node tag 9",
            ),
            (
                ClientPlanError::ExpressionDepthExceeded,
                "client-plan expression tree exceeds the depth cap",
            ),
            (
                ClientPlanError::ExpressionNodeCountExceeded,
                "client-plan expression tree exceeds the node-count cap",
            ),
            (
                ClientPlanError::ExpressionCollectionExceeded { limit: 64 },
                "client-plan expression collection exceeds the limit 64",
            ),
            (
                ClientPlanError::InvalidStateScope(9),
                "invalid client-plan state scope tag 9",
            ),
            (
                ClientPlanError::InvalidStateDefaultTag(7),
                "invalid client-plan state default tag 7",
            ),
            (
                ClientPlanError::InvalidStateSlotCount { actual: 0 },
                "invalid client-plan state slot count 0; a state plan requires at least one slot",
            ),
            (
                ClientPlanError::StateSlotLimitExceeded { limit: 64 },
                "client-plan state slot count exceeds the limit 64",
            ),
            (
                ClientPlanError::DuplicateStateSlotId(duplicate_slot),
                duplicate_display.as_str(),
            ),
            (
                ClientPlanError::InvalidCapabilityCount { actual: 0 },
                "invalid client-plan capability count 0; a capability plan requires at least one requirement",
            ),
            (
                ClientPlanError::CapabilityLimitExceeded { limit: 64 },
                "client-plan capability count exceeds the limit 64",
            ),
            (
                ClientPlanError::DuplicateCapabilityName("std.fs.read".to_owned()),
                "duplicate client-plan capability requirement std.fs.read",
            ),
            (
                ClientPlanError::EmptyCapabilityName,
                "client-plan capability name must not be empty",
            ),
            (
                ClientPlanError::EmptyCapabilityArgument,
                "client-plan capability argument must not be empty",
            ),
            (
                ClientPlanError::CapabilityNameTooLong {
                    length: 300,
                    limit: 256,
                },
                "client-plan capability name length 300 exceeds the limit 256",
            ),
            (
                ClientPlanError::CapabilityArgumentTooLong {
                    length: 2000,
                    limit: 1024,
                },
                "client-plan capability argument length 2000 exceeds the limit 1024",
            ),
            (
                ClientPlanError::InvalidCapabilityNameUtf8,
                "client-plan capability name is not valid UTF-8",
            ),
            (
                ClientPlanError::InvalidCapabilityArgumentUtf8,
                "client-plan capability argument is not valid UTF-8",
            ),
            (
                ClientPlanError::InvalidCapabilityArgumentTag(9),
                "invalid client-plan capability argument tag 9",
            ),
            (
                ClientPlanError::UnsupportedInnerVersion(6),
                "unsupported inner client-plan version 6",
            ),
            (
                ClientPlanError::ArtifactSizeLimit {
                    size: 100,
                    maximum: 50,
                },
                "orna.client-plan artefact size 100 exceeds the limit 50",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    fn expression_plan() -> ExpressionClientPlan {
        let function = FunctionId::from_bytes([0x21; 16]);
        let parameter = ParameterId::from_bytes([0x31; 16]);
        let field = FieldId::from_bytes([0x41; 16]);
        let plan = ExpressionClientPlan::new(ClientExpressionNode::Call {
            function,
            arguments: vec![(
                parameter,
                ClientExpressionNode::Concat {
                    left: Box::new(ClientExpressionNode::String {
                        value: "hello ".to_owned(),
                    }),
                    right: Box::new(ClientExpressionNode::FieldPath {
                        root: parameter,
                        fields: vec![field],
                    }),
                },
            )],
        });
        plan
    }

    #[test]
    fn expression_plan_round_trips_every_node_form() {
        let plans = [
            ExpressionClientPlan::new(ClientExpressionNode::Call {
                function: FunctionId::from_bytes([0x21; 16]),
                arguments: vec![(
                    ParameterId::from_bytes([0x31; 16]),
                    ClientExpressionNode::Boolean { value: true },
                )],
            }),
            ExpressionClientPlan::new(ClientExpressionNode::String {
                value: "a'b\"c".to_owned(),
            }),
            ExpressionClientPlan::new(ClientExpressionNode::Integer { value: -42 }),
            ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: false }),
            ExpressionClientPlan::new(ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0x32; 16]),
            }),
            ExpressionClientPlan::new(ClientExpressionNode::FieldPath {
                root: ParameterId::from_bytes([0x33; 16]),
                fields: vec![FieldId::from_bytes([0x43; 16])],
            }),
            expression_plan(),
            ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
                identity: "std.ui.window@1".to_owned(),
            }),
        ];
        for plan in plans {
            let bytes = plan.encode().expect("the plan encodes");
            let decoded = ExpressionClientPlan::decode(&bytes).expect("the plan decodes");
            assert_eq!(decoded, plan);
            assert_eq!(decoded.format_version(), EXPRESSION_FORMAT_VERSION);
        }
    }

    #[test]
    fn expression_plan_has_the_exact_version_three_header() {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: true });
        let bytes = plan.encode().expect("the plan encodes");
        assert_eq!(&bytes[..8], &MAGIC);
        assert_eq!(&bytes[8..12], &EXPRESSION_FORMAT_VERSION.to_be_bytes());
        assert_eq!(bytes[12], RETURN_EXPRESSION_OPERATION);
        assert_eq!(bytes[13], NODE_BOOLEAN);
        assert_eq!(bytes[14], 1);
    }

    #[test]
    fn expression_plan_versions_remain_mutually_closed() {
        let expression = expression_plan().encode().expect("the plan encodes");
        assert_eq!(
            ClientPlan::decode(&expression),
            Err(ClientPlanError::UnsupportedVersion(
                EXPRESSION_FORMAT_VERSION
            ))
        );
        assert_eq!(
            OpaqueClientPlan::decode(&expression),
            Err(ClientPlanError::UnsupportedVersion(
                EXPRESSION_FORMAT_VERSION
            ))
        );
        assert_eq!(
            ExpressionClientPlan::decode(&TRUE_BYTES),
            Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
        );
    }

    #[test]
    fn expression_plan_rejects_magic_version_operation_and_trailing_corruption() {
        let encoded = expression_plan().encode().expect("the plan encodes");

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert_eq!(
            ExpressionClientPlan::decode(&wrong_magic),
            Err(ClientPlanError::InvalidMagic)
        );

        let mut wrong_version = encoded.clone();
        wrong_version[8..12].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            ExpressionClientPlan::decode(&wrong_version),
            Err(ClientPlanError::UnsupportedVersion(2))
        );

        let mut wrong_operation = encoded.clone();
        wrong_operation[12] = RETURN_BOOLEAN_OPERATION;
        assert_eq!(
            ExpressionClientPlan::decode(&wrong_operation),
            Err(ClientPlanError::InvalidOperation(RETURN_BOOLEAN_OPERATION))
        );

        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(
            ExpressionClientPlan::decode(&trailing),
            Err(ClientPlanError::TrailingBytes)
        );
    }

    #[test]
    fn expression_plan_rejects_unknown_tags_and_exceeded_limits() {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: true });
        let mut encoded = plan.encode().expect("the plan encodes");
        encoded[13] = 9;
        assert_eq!(
            ExpressionClientPlan::decode(&encoded),
            Err(ClientPlanError::InvalidExpressionNode(9))
        );

        let mut boolean_byte = plan.encode().expect("the plan encodes");
        boolean_byte[14] = 2;
        assert_eq!(
            ExpressionClientPlan::decode(&boolean_byte),
            Err(ClientPlanError::InvalidExpressionNode(NODE_BOOLEAN))
        );

        let mut empty_field_path = ExpressionClientPlan::new(ClientExpressionNode::FieldPath {
            root: ParameterId::from_bytes([0x32; 16]),
            fields: vec![FieldId::from_bytes([0x42; 16])],
        })
        .encode()
        .expect("the field path encodes");
        empty_field_path[30..34].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            ExpressionClientPlan::decode(&empty_field_path),
            Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH))
        );

        let deep = ExpressionClientPlan::new(deep_concat(MAX_EXPRESSION_DEPTH + 1));
        assert_eq!(deep.encode(), Err(ClientPlanError::ExpressionDepthExceeded));

        let call = ExpressionClientPlan::new(ClientExpressionNode::Call {
            function: FunctionId::from_bytes([0x21; 16]),
            arguments: (0..=MAX_CALL_ARGUMENTS)
                .map(|index| {
                    (
                        ParameterId::from_bytes([index as u8; 16]),
                        ClientExpressionNode::Boolean { value: true },
                    )
                })
                .collect(),
        });
        assert_eq!(
            call.encode(),
            Err(ClientPlanError::ExpressionCollectionExceeded {
                limit: MAX_CALL_ARGUMENTS,
            })
        );

        let wide = ExpressionClientPlan::new(ClientExpressionNode::Call {
            function: FunctionId::from_bytes([0x51; 16]),
            arguments: (0..MAX_CALL_ARGUMENTS)
                .map(|outer| {
                    (
                        ParameterId::from_bytes([outer as u8; 16]),
                        ClientExpressionNode::Call {
                            function: FunctionId::from_bytes([0x52; 16]),
                            arguments: (0..MAX_CALL_ARGUMENTS)
                                .map(|inner| {
                                    (
                                        ParameterId::from_bytes([inner as u8; 16]),
                                        ClientExpressionNode::Boolean { value: true },
                                    )
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        });
        assert_eq!(
            wide.encode(),
            Err(ClientPlanError::ExpressionNodeCountExceeded)
        );
    }

    fn deep_concat(depth: usize) -> ClientExpressionNode {
        if depth == 0 {
            ClientExpressionNode::Boolean { value: true }
        } else {
            ClientExpressionNode::Concat {
                left: Box::new(deep_concat(depth - 1)),
                right: Box::new(ClientExpressionNode::Boolean { value: false }),
            }
        }
    }

    #[test]
    fn expression_plan_rejects_every_truncated_prefix() {
        let encoded = expression_plan().encode().expect("the plan encodes");
        for length in 0..encoded.len() {
            assert_eq!(
                ExpressionClientPlan::decode(&encoded[..length]),
                Err(ClientPlanError::Truncated),
                "prefix length {length} must be truncated"
            );
        }
    }

    fn state_plan() -> StateClientPlan {
        let function = FunctionId::from_bytes([0x21; 16]);
        let parameter = ParameterId::from_bytes([0x31; 16]);
        let field = FieldId::from_bytes([0x41; 16]);
        StateClientPlan::new(
            ClientExpressionNode::Call {
                function,
                arguments: vec![(
                    parameter,
                    ClientExpressionNode::FieldPath {
                        root: parameter,
                        fields: vec![field],
                    },
                )],
            },
            vec![
                StateSlot::new(
                    StateSlotId::from_bytes([0x11; 16]),
                    TypeId::from_bytes([0x12; 16]),
                    StateScope::Local,
                    StateDefault::Unset,
                ),
                StateSlot::new(
                    StateSlotId::from_bytes([0x21; 16]),
                    TypeId::from_bytes([0x22; 16]),
                    StateScope::Session,
                    StateDefault::Null,
                ),
                StateSlot::new(
                    StateSlotId::from_bytes([0x31; 16]),
                    TypeId::from_bytes([0x32; 16]),
                    StateScope::User,
                    StateDefault::Expression(ClientExpressionNode::Concat {
                        left: Box::new(ClientExpressionNode::String {
                            value: "prefix".to_owned(),
                        }),
                        right: Box::new(ClientExpressionNode::ParameterRead { parameter }),
                    }),
                ),
            ],
        )
    }

    fn expression_default_plan() -> StateClientPlan {
        let function = FunctionId::from_bytes([0x21; 16]);
        let parameter = ParameterId::from_bytes([0x31; 16]);
        let field = FieldId::from_bytes([0x41; 16]);
        let forms = [
            ClientExpressionNode::Call {
                function,
                arguments: vec![(parameter, ClientExpressionNode::Boolean { value: true })],
            },
            ClientExpressionNode::String {
                value: "a'b\"c".to_owned(),
            },
            ClientExpressionNode::Integer { value: -42 },
            ClientExpressionNode::Boolean { value: false },
            ClientExpressionNode::ParameterRead { parameter },
            ClientExpressionNode::FieldPath {
                root: parameter,
                fields: vec![field],
            },
            ClientExpressionNode::Concat {
                left: Box::new(ClientExpressionNode::String {
                    value: "x".to_owned(),
                }),
                right: Box::new(ClientExpressionNode::Integer { value: 7 }),
            },
            ClientExpressionNode::ExternalContract {
                identity: "std.ui.window@1".to_owned(),
            },
        ];
        let slots = forms
            .iter()
            .enumerate()
            .map(|(index, node)| {
                StateSlot::new(
                    StateSlotId::from_bytes([index as u8; 16]),
                    TypeId::from_bytes([0x40 + index as u8; 16]),
                    match index % 3 {
                        0 => StateScope::Local,
                        1 => StateScope::Session,
                        _ => StateScope::User,
                    },
                    StateDefault::Expression(node.clone()),
                )
            })
            .collect();
        StateClientPlan::new(
            ClientExpressionNode::String {
                value: "ready".to_owned(),
            },
            slots,
        )
    }

    fn minimal_state_plan() -> StateClientPlan {
        StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::Local,
                StateDefault::Unset,
            )],
        )
    }

    #[test]
    fn state_plan_round_trips_every_scope_and_default_form() {
        let plans = [
            state_plan(),
            expression_default_plan(),
            minimal_state_plan(),
            StateClientPlan::new(
                ClientExpressionNode::Boolean { value: false },
                vec![StateSlot::new(
                    StateSlotId::from_bytes([0x51; 16]),
                    TypeId::from_bytes([0x52; 16]),
                    StateScope::Session,
                    StateDefault::Null,
                )],
            ),
        ];
        for plan in plans {
            let bytes = plan.encode().expect("the plan encodes");
            let decoded = StateClientPlan::decode(&bytes).expect("the plan decodes");
            assert_eq!(decoded, plan);
            assert_eq!(decoded.format_version(), STATE_FORMAT_VERSION);
            assert_eq!(decoded.slots().len(), plan.slots().len());
            assert_eq!(decoded.expression(), plan.expression());
        }
    }

    #[test]
    fn state_plan_exposes_its_slot_accessors() {
        let plan = state_plan();
        let bytes = plan.encode().expect("the plan encodes");
        let decoded = StateClientPlan::decode(&bytes).expect("the plan decodes");
        let slots = decoded.slots();
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0].state_slot_id(), StateSlotId::from_bytes([0x11; 16]));
        assert_eq!(slots[0].type_id(), TypeId::from_bytes([0x12; 16]));
        assert_eq!(slots[0].scope(), StateScope::Local);
        assert_eq!(slots[0].default(), &StateDefault::Unset);
        assert_eq!(slots[1].scope(), StateScope::Session);
        assert_eq!(slots[1].default(), &StateDefault::Null);
        assert_eq!(slots[2].scope(), StateScope::User);
        assert!(matches!(
            slots[2].default(),
            StateDefault::Expression(ClientExpressionNode::Concat { .. })
        ));
    }

    #[test]
    fn state_plan_has_the_exact_version_four_layout() {
        let plan = minimal_state_plan();
        let bytes = plan.encode().expect("the plan encodes");
        let mut expected = Vec::new();
        expected.extend_from_slice(&MAGIC);
        expected.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
        expected.push(RETURN_STATE_OPERATION);
        expected.push(NODE_BOOLEAN);
        expected.push(1);
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(&[0x11; 16]);
        expected.extend_from_slice(&[0x12; 16]);
        expected.push(STATE_SCOPE_LOCAL);
        expected.push(STATE_DEFAULT_UNSET);
        assert_eq!(bytes, expected);
        assert_eq!(plan.format_version(), STATE_FORMAT_VERSION);
    }

    #[test]
    fn state_plan_has_the_exact_expression_default_layout() {
        let plan = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: false },
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::User,
                StateDefault::Expression(ClientExpressionNode::String {
                    value: "hi".to_owned(),
                }),
            )],
        );
        let bytes = plan.encode().expect("the plan encodes");
        let mut expected = Vec::new();
        expected.extend_from_slice(&MAGIC);
        expected.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
        expected.push(RETURN_STATE_OPERATION);
        expected.push(NODE_BOOLEAN);
        expected.push(0);
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(&[0x11; 16]);
        expected.extend_from_slice(&[0x12; 16]);
        expected.push(STATE_SCOPE_USER);
        expected.push(STATE_DEFAULT_EXPRESSION);
        expected.push(NODE_STRING);
        expected.extend_from_slice(&2_u32.to_be_bytes());
        expected.extend_from_slice(b"hi");
        assert_eq!(bytes, expected);
    }

    #[test]
    fn state_plan_versions_remain_mutually_closed() {
        let state = state_plan().encode().expect("the plan encodes");
        assert_eq!(
            ClientPlan::decode(&state),
            Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
        );
        assert_eq!(
            OpaqueClientPlan::decode(&state),
            Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
        );
        assert_eq!(
            ExpressionClientPlan::decode(&state),
            Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
        );
        assert_eq!(
            StateClientPlan::decode(&TRUE_BYTES),
            Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
        );
        let expression = expression_plan().encode().expect("the plan encodes");
        assert_eq!(
            StateClientPlan::decode(&expression),
            Err(ClientPlanError::UnsupportedVersion(
                EXPRESSION_FORMAT_VERSION
            ))
        );
    }

    #[test]
    fn state_plan_rejects_magic_version_operation_and_trailing_corruption() {
        let encoded = state_plan().encode().expect("the plan encodes");

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert_eq!(
            StateClientPlan::decode(&wrong_magic),
            Err(ClientPlanError::InvalidMagic)
        );

        let mut wrong_version = encoded.clone();
        wrong_version[8..12].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            StateClientPlan::decode(&wrong_version),
            Err(ClientPlanError::UnsupportedVersion(2))
        );

        let mut wrong_operation = encoded.clone();
        wrong_operation[12] = RETURN_EXPRESSION_OPERATION;
        assert_eq!(
            StateClientPlan::decode(&wrong_operation),
            Err(ClientPlanError::InvalidOperation(RETURN_EXPRESSION_OPERATION))
        );

        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(
            StateClientPlan::decode(&trailing),
            Err(ClientPlanError::TrailingBytes)
        );
    }

    #[test]
    fn state_plan_rejects_unknown_scope_and_default_tags() {
        let plan = minimal_state_plan();
        let mut bytes = plan.encode().expect("the plan encodes");
        assert_eq!(bytes.len(), 53);
        bytes[51] = 4;
        assert_eq!(
            StateClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidStateScope(4))
        );

        let mut bytes = plan.encode().expect("the plan encodes");
        bytes[52] = 3;
        assert_eq!(
            StateClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidStateDefaultTag(3))
        );
    }

    #[test]
    fn state_plan_rejects_zero_and_oversized_slot_counts() {
        let plan = minimal_state_plan();
        let mut zero_slots = plan.encode().expect("the plan encodes");
        zero_slots[15..19].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            StateClientPlan::decode(&zero_slots),
            Err(ClientPlanError::InvalidStateSlotCount { actual: 0 })
        );

        let mut oversized = plan.encode().expect("the plan encodes");
        oversized[15..19].copy_from_slice(&(MAX_STATE_SLOTS as u32 + 1).to_be_bytes());
        assert_eq!(
            StateClientPlan::decode(&oversized),
            Err(ClientPlanError::StateSlotLimitExceeded {
                limit: MAX_STATE_SLOTS,
            })
        );
    }

    #[test]
    fn state_plan_rejects_duplicate_state_slot_identities() {
        let duplicated = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![
                StateSlot::new(
                    StateSlotId::from_bytes([0x11; 16]),
                    TypeId::from_bytes([0x12; 16]),
                    StateScope::Local,
                    StateDefault::Unset,
                ),
                StateSlot::new(
                    StateSlotId::from_bytes([0x11; 16]),
                    TypeId::from_bytes([0x13; 16]),
                    StateScope::Session,
                    StateDefault::Null,
                ),
            ],
        );
        assert_eq!(
            duplicated.encode(),
            Err(ClientPlanError::DuplicateStateSlotId(
                StateSlotId::from_bytes([0x11; 16])
            ))
        );

        let mut crafted = minimal_state_plan().encode().expect("the plan encodes");
        crafted[15..19].copy_from_slice(&2_u32.to_be_bytes());
        crafted.extend_from_slice(&[0x11; 16]);
        crafted.extend_from_slice(&[0x13; 16]);
        crafted.push(STATE_SCOPE_SESSION);
        crafted.push(STATE_DEFAULT_NULL);
        assert_eq!(
            StateClientPlan::decode(&crafted),
            Err(ClientPlanError::DuplicateStateSlotId(
                StateSlotId::from_bytes([0x11; 16])
            ))
        );
    }

    #[test]
    fn state_plan_encode_rejects_empty_and_oversized_slot_lists() {
        let empty = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            Vec::new(),
        );
        assert_eq!(
            empty.encode(),
            Err(ClientPlanError::InvalidStateSlotCount { actual: 0 })
        );

        let slot = StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Local,
            StateDefault::Unset,
        );
        let oversized = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![slot; MAX_STATE_SLOTS + 1],
        );
        assert_eq!(
            oversized.encode(),
            Err(ClientPlanError::StateSlotLimitExceeded {
                limit: MAX_STATE_SLOTS,
            })
        );
    }

    #[test]
    fn state_plan_rejects_malformed_return_and_default_trees() {
        let mut unknown_return = minimal_state_plan().encode().expect("the plan encodes");
        unknown_return[13] = 9;
        assert_eq!(
            StateClientPlan::decode(&unknown_return),
            Err(ClientPlanError::InvalidExpressionNode(9))
        );

        let expression_default = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::Session,
                StateDefault::Expression(ClientExpressionNode::String {
                    value: "ab".to_owned(),
                }),
            )],
        );
        let mut unknown_default = expression_default.encode().expect("the plan encodes");
        unknown_default[53] = 9;
        assert_eq!(
            StateClientPlan::decode(&unknown_default),
            Err(ClientPlanError::InvalidExpressionNode(9))
        );

        let mut truncated_default = expression_default.encode().expect("the plan encodes");
        truncated_default[54..58].copy_from_slice(&3_u32.to_be_bytes());
        assert_eq!(
            StateClientPlan::decode(&truncated_default),
            Err(ClientPlanError::Truncated)
        );

        let mut invalid_utf8 = expression_default.encode().expect("the plan encodes");
        invalid_utf8[54..58].copy_from_slice(&1_u32.to_be_bytes());
        invalid_utf8[58] = 0xff;
        assert_eq!(
            StateClientPlan::decode(&invalid_utf8),
            Err(ClientPlanError::InvalidExpressionNode(NODE_STRING))
        );
    }

    #[test]
    fn state_plan_rejects_depth_and_collection_violations() {
        let deep = StateClientPlan::new(
            deep_concat(MAX_EXPRESSION_DEPTH + 1),
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::Local,
                StateDefault::Unset,
            )],
        );
        assert_eq!(deep.encode(), Err(ClientPlanError::ExpressionDepthExceeded));

        let wide_default = StateClientPlan::new(
            ClientExpressionNode::Boolean { value: true },
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::User,
                StateDefault::Expression(ClientExpressionNode::Call {
                    function: FunctionId::from_bytes([0x51; 16]),
                    arguments: (0..=MAX_CALL_ARGUMENTS)
                        .map(|index| {
                            (
                                ParameterId::from_bytes([index as u8; 16]),
                                ClientExpressionNode::Boolean { value: true },
                            )
                        })
                        .collect(),
                }),
            )],
        );
        assert_eq!(
            wide_default.encode(),
            Err(ClientPlanError::ExpressionCollectionExceeded {
                limit: MAX_CALL_ARGUMENTS,
            })
        );
    }

    #[test]
    fn state_plan_rejects_every_truncated_prefix() {
        let encoded = minimal_state_plan().encode().expect("the plan encodes");
        for length in 0..encoded.len() {
            assert_eq!(
                StateClientPlan::decode(&encoded[..length]),
                Err(ClientPlanError::Truncated),
                "prefix length {length} must be truncated"
            );
        }
    }

    fn capability_plan() -> CapabilityClientPlan {
        let function = FunctionId::from_bytes([0x21; 16]);
        let parameter = ParameterId::from_bytes([0x31; 16]);
        let field = FieldId::from_bytes([0x41; 16]);
        CapabilityClientPlan::new(
            InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::Call {
                function,
                arguments: vec![(
                    parameter,
                    ClientExpressionNode::FieldPath {
                        root: parameter,
                        fields: vec![field],
                    },
                )],
            })),
            vec![
                CapabilityRequirement::new(
                    "std.fs.read",
                    CapabilityArgumentSource::Text("/home/bob".to_owned()),
                ),
                CapabilityRequirement::new(
                    "std.net.connect",
                    CapabilityArgumentSource::Parameter("p_host".to_owned()),
                ),
            ],
        )
    }

    fn minimal_capability_plan() -> CapabilityClientPlan {
        CapabilityClientPlan::new(
            InnerClientPlan::Boolean(ClientPlan::return_boolean(true)),
            vec![CapabilityRequirement::new(
                "std.secret.use",
                CapabilityArgumentSource::Parameter("p_secret".to_owned()),
            )],
        )
    }

    #[test]
    fn capability_plan_round_trips_every_inner_form_and_argument_source() {
        let inner_forms = [
            InnerClientPlan::Boolean(ClientPlan::return_boolean(false)),
            InnerClientPlan::Opaque(OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD)),
            InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::String {
                value: "hi".to_owned(),
            })),
            InnerClientPlan::State(minimal_state_plan()),
        ];
        for inner in inner_forms {
            let plan = CapabilityClientPlan::new(
                inner.clone(),
                vec![
                    CapabilityRequirement::new(
                        "std.fs.read",
                        CapabilityArgumentSource::Text("/home/bob".to_owned()),
                    ),
                    CapabilityRequirement::new(
                        "std.fs.write",
                        CapabilityArgumentSource::Parameter("p_path".to_owned()),
                    ),
                ],
            );
            let bytes = plan.encode().expect("the plan encodes");
            let decoded = CapabilityClientPlan::decode(&bytes).expect("the plan decodes");
            assert_eq!(decoded, plan);
            assert_eq!(decoded.format_version(), CAPABILITY_FORMAT_VERSION);
            assert_eq!(decoded.inner_plan_version(), inner.format_version());
            assert_eq!(decoded.inner_plan(), &inner);
            assert_eq!(decoded.requirements(), plan.requirements());
        }
    }

    #[test]
    fn capability_plan_has_the_exact_version_five_layout() {
        let plan = minimal_capability_plan();
        let bytes = plan.encode().expect("the plan encodes");
        let mut expected = Vec::new();
        expected.extend_from_slice(&MAGIC);
        expected.extend_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
        expected.push(RETURN_CAPABILITY_OPERATION);
        expected.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        expected.extend_from_slice(&(TRUE_BYTES.len() as u32).to_be_bytes());
        expected.extend_from_slice(&TRUE_BYTES);
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(&(b"std.secret.use".len() as u32).to_be_bytes());
        expected.extend_from_slice(b"std.secret.use");
        expected.push(CAPABILITY_ARGUMENT_PARAMETER);
        expected.extend_from_slice(&(b"p_secret".len() as u32).to_be_bytes());
        expected.extend_from_slice(b"p_secret");
        assert_eq!(bytes, expected);
        assert_eq!(plan.format_version(), CAPABILITY_FORMAT_VERSION);
        assert_eq!(plan.inner_plan_version(), FORMAT_VERSION);
        assert_eq!(
            plan.inner_plan(),
            &InnerClientPlan::Boolean(ClientPlan::return_boolean(true))
        );
    }

    #[test]
    fn capability_plan_versions_remain_mutually_closed() {
        let capability = capability_plan().encode().expect("the plan encodes");
        assert_eq!(
            ClientPlan::decode(&capability),
            Err(ClientPlanError::UnsupportedVersion(
                CAPABILITY_FORMAT_VERSION
            ))
        );
        assert_eq!(
            OpaqueClientPlan::decode(&capability),
            Err(ClientPlanError::UnsupportedVersion(
                CAPABILITY_FORMAT_VERSION
            ))
        );
        assert_eq!(
            ExpressionClientPlan::decode(&capability),
            Err(ClientPlanError::UnsupportedVersion(
                CAPABILITY_FORMAT_VERSION
            ))
        );
        assert_eq!(
            StateClientPlan::decode(&capability),
            Err(ClientPlanError::UnsupportedVersion(
                CAPABILITY_FORMAT_VERSION
            ))
        );
        let inner_artefacts = [
            (
                FORMAT_VERSION,
                ClientPlan::return_boolean(true).encode(),
            ),
            (
                OPAQUE_FORMAT_VERSION,
                OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD).encode(),
            ),
            (
                EXPRESSION_FORMAT_VERSION,
                expression_plan().encode().expect("the plan encodes"),
            ),
            (
                STATE_FORMAT_VERSION,
                minimal_state_plan().encode().expect("the plan encodes"),
            ),
        ];
        for (version, bytes) in inner_artefacts {
            assert_eq!(
                CapabilityClientPlan::decode(&bytes),
                Err(ClientPlanError::UnsupportedVersion(version))
            );
        }
    }

    #[test]
    fn capability_plan_rejects_magic_version_operation_and_trailing_corruption() {
        let encoded = capability_plan().encode().expect("the plan encodes");

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert_eq!(
            CapabilityClientPlan::decode(&wrong_magic),
            Err(ClientPlanError::InvalidMagic)
        );

        let mut wrong_version = encoded.clone();
        wrong_version[8..12].copy_from_slice(&4_u32.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&wrong_version),
            Err(ClientPlanError::UnsupportedVersion(4))
        );

        let mut wrong_operation = encoded.clone();
        wrong_operation[12] = RETURN_STATE_OPERATION;
        assert_eq!(
            CapabilityClientPlan::decode(&wrong_operation),
            Err(ClientPlanError::InvalidOperation(RETURN_STATE_OPERATION))
        );

        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(
            CapabilityClientPlan::decode(&trailing),
            Err(ClientPlanError::TrailingBytes)
        );
    }

    #[test]
    fn capability_plan_rejects_invalid_argument_tags_and_utf8() {
        let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
        let name_length_offset = count_offset + 4;
        let tag_offset = name_length_offset + 4 + b"std.secret.use".len();
        let argument_length_offset = tag_offset + 1;

        let mut wrong_tag = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        wrong_tag[tag_offset] = 3;
        assert_eq!(
            CapabilityClientPlan::decode(&wrong_tag),
            Err(ClientPlanError::InvalidCapabilityArgumentTag(3))
        );

        let mut text_form = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        text_form[tag_offset] = CAPABILITY_ARGUMENT_TEXT;
        let decoded = CapabilityClientPlan::decode(&text_form).expect("the text form decodes");
        assert_eq!(
            decoded.requirements()[0].argument(),
            &CapabilityArgumentSource::Text("p_secret".to_owned())
        );

        let mut bad_name_utf8 = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        bad_name_utf8[name_length_offset..name_length_offset + 4]
            .copy_from_slice(&1_u32.to_be_bytes());
        bad_name_utf8[name_length_offset + 4] = 0xff;
        assert_eq!(
            CapabilityClientPlan::decode(&bad_name_utf8),
            Err(ClientPlanError::InvalidCapabilityNameUtf8)
        );

        let mut bad_argument_utf8 = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        bad_argument_utf8[argument_length_offset..argument_length_offset + 4]
            .copy_from_slice(&1_u32.to_be_bytes());
        bad_argument_utf8[argument_length_offset + 4] = 0xff;
        assert_eq!(
            CapabilityClientPlan::decode(&bad_argument_utf8),
            Err(ClientPlanError::InvalidCapabilityArgumentUtf8)
        );
    }

    #[test]
    fn capability_plan_rejects_zero_and_oversized_counts_and_lengths() {
        let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
        let name_length_offset = count_offset + 4;
        let tag_offset = name_length_offset + 4 + b"std.secret.use".len();
        let argument_length_offset = tag_offset + 1;

        let mut zero_count = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        zero_count[count_offset..count_offset + 4].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&zero_count),
            Err(ClientPlanError::InvalidCapabilityCount { actual: 0 })
        );

        let mut oversized_count = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        oversized_count[count_offset..count_offset + 4]
            .copy_from_slice(&(MAX_CAPABILITY_REQUIREMENTS as u32 + 1).to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&oversized_count),
            Err(ClientPlanError::CapabilityLimitExceeded {
                limit: MAX_CAPABILITY_REQUIREMENTS,
            })
        );

        let mut zero_name = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        zero_name[name_length_offset..name_length_offset + 4].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&zero_name),
            Err(ClientPlanError::EmptyCapabilityName)
        );

        let mut long_name = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        long_name[name_length_offset..name_length_offset + 4]
            .copy_from_slice(&(MAX_CAPABILITY_NAME_LENGTH as u32 + 1).to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&long_name),
            Err(ClientPlanError::CapabilityNameTooLong {
                length: MAX_CAPABILITY_NAME_LENGTH + 1,
                limit: MAX_CAPABILITY_NAME_LENGTH,
            })
        );

        let mut zero_argument = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        zero_argument[argument_length_offset..argument_length_offset + 4]
            .copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&zero_argument),
            Err(ClientPlanError::EmptyCapabilityArgument)
        );

        let mut long_argument = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        long_argument[argument_length_offset..argument_length_offset + 4]
            .copy_from_slice(&(MAX_CAPABILITY_ARGUMENT_LENGTH as u32 + 1).to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&long_argument),
            Err(ClientPlanError::CapabilityArgumentTooLong {
                length: MAX_CAPABILITY_ARGUMENT_LENGTH + 1,
                limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
            })
        );

        let inner = InnerClientPlan::Boolean(ClientPlan::return_boolean(true));
        let empty = CapabilityClientPlan::new(inner.clone(), Vec::new());
        assert_eq!(
            empty.encode(),
            Err(ClientPlanError::InvalidCapabilityCount { actual: 0 })
        );

        let requirement = CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Text("/home/bob".to_owned()),
        );
        let oversized = CapabilityClientPlan::new(
            inner.clone(),
            vec![requirement; MAX_CAPABILITY_REQUIREMENTS + 1],
        );
        assert_eq!(
            oversized.encode(),
            Err(ClientPlanError::CapabilityLimitExceeded {
                limit: MAX_CAPABILITY_REQUIREMENTS,
            })
        );

        let empty_name = CapabilityClientPlan::new(
            inner.clone(),
            vec![CapabilityRequirement::new(
                "",
                CapabilityArgumentSource::Text("x".to_owned()),
            )],
        );
        assert_eq!(empty_name.encode(), Err(ClientPlanError::EmptyCapabilityName));

        let long_name_plan = CapabilityClientPlan::new(
            inner.clone(),
            vec![CapabilityRequirement::new(
                "x".repeat(MAX_CAPABILITY_NAME_LENGTH + 1),
                CapabilityArgumentSource::Text("x".to_owned()),
            )],
        );
        assert_eq!(
            long_name_plan.encode(),
            Err(ClientPlanError::CapabilityNameTooLong {
                length: MAX_CAPABILITY_NAME_LENGTH + 1,
                limit: MAX_CAPABILITY_NAME_LENGTH,
            })
        );

        let empty_argument = CapabilityClientPlan::new(
            inner.clone(),
            vec![CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Text(String::new()),
            )],
        );
        assert_eq!(
            empty_argument.encode(),
            Err(ClientPlanError::EmptyCapabilityArgument)
        );

        let long_argument_plan = CapabilityClientPlan::new(
            inner,
            vec![CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Text("x".repeat(MAX_CAPABILITY_ARGUMENT_LENGTH + 1)),
            )],
        );
        assert_eq!(
            long_argument_plan.encode(),
            Err(ClientPlanError::CapabilityArgumentTooLong {
                length: MAX_CAPABILITY_ARGUMENT_LENGTH + 1,
                limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
            })
        );
    }

    #[test]
    fn capability_plan_rejects_duplicate_requirement_names() {
        let duplicated = CapabilityClientPlan::new(
            InnerClientPlan::Boolean(ClientPlan::return_boolean(true)),
            vec![
                CapabilityRequirement::new(
                    "std.fs.read",
                    CapabilityArgumentSource::Text("/home/bob".to_owned()),
                ),
                CapabilityRequirement::new(
                    "std.fs.read",
                    CapabilityArgumentSource::Parameter("p_path".to_owned()),
                ),
            ],
        );
        assert_eq!(
            duplicated.encode(),
            Err(ClientPlanError::DuplicateCapabilityName(
                "std.fs.read".to_owned()
            ))
        );

        let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
        let mut crafted = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        crafted[count_offset..count_offset + 4].copy_from_slice(&2_u32.to_be_bytes());
        crafted.extend_from_slice(&(b"std.secret.use".len() as u32).to_be_bytes());
        crafted.extend_from_slice(b"std.secret.use");
        crafted.push(CAPABILITY_ARGUMENT_TEXT);
        crafted.extend_from_slice(&(b"/home/bob".len() as u32).to_be_bytes());
        crafted.extend_from_slice(b"/home/bob");
        assert_eq!(
            CapabilityClientPlan::decode(&crafted),
            Err(ClientPlanError::DuplicateCapabilityName(
                "std.secret.use".to_owned()
            ))
        );
    }

    #[test]
    fn capability_plan_rejects_unsupported_inner_versions_and_malformed_payloads() {
        let mut zero_inner = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        zero_inner[13..17].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&zero_inner),
            Err(ClientPlanError::UnsupportedInnerVersion(0))
        );

        let mut inner_five = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        inner_five[13..17].copy_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&inner_five),
            Err(ClientPlanError::UnsupportedInnerVersion(
                CAPABILITY_FORMAT_VERSION
            ))
        );

        let mut mismatched = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        mismatched[13..17].copy_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
        // The inner payload is a version-1 artefact; the version-2 decoder
        // rejects it with the payload's own version.
        assert_eq!(
            CapabilityClientPlan::decode(&mismatched),
            Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
        );

        let mut corrupt_inner = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        corrupt_inner[8 + 4 + 1 + 4 + 4] = b'X';
        assert_eq!(
            CapabilityClientPlan::decode(&corrupt_inner),
            Err(ClientPlanError::InvalidMagic)
        );

        let mut oversized_inner = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        oversized_inner[17..21].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            CapabilityClientPlan::decode(&oversized_inner),
            Err(ClientPlanError::Truncated)
        );
    }

    #[test]
    fn capability_plan_exposes_inner_plan_and_requirement_accessors() {
        let plan = capability_plan();
        let bytes = plan.encode().expect("the plan encodes");
        let decoded = CapabilityClientPlan::decode(&bytes).expect("the plan decodes");
        assert_eq!(decoded.format_version(), CAPABILITY_FORMAT_VERSION);
        assert_eq!(decoded.inner_plan_version(), EXPRESSION_FORMAT_VERSION);
        assert!(matches!(
            decoded.inner_plan(),
            InnerClientPlan::Expression(_)
        ));
        let requirements = decoded.requirements();
        assert_eq!(requirements.len(), 2);
        assert_eq!(requirements[0].name(), "std.fs.read");
        assert_eq!(
            requirements[0].argument(),
            &CapabilityArgumentSource::Text("/home/bob".to_owned())
        );
        assert_eq!(requirements[1].name(), "std.net.connect");
        assert_eq!(
            requirements[1].argument(),
            &CapabilityArgumentSource::Parameter("p_host".to_owned())
        );
    }

    #[test]
    fn capability_plan_rejects_every_truncated_prefix() {
        let encoded = minimal_capability_plan()
            .encode()
            .expect("the plan encodes");
        for length in 0..encoded.len() {
            assert_eq!(
                CapabilityClientPlan::decode(&encoded[..length]),
                Err(ClientPlanError::Truncated),
                "prefix length {length} must be truncated"
            );
        }
    }
}
