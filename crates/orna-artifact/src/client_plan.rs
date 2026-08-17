//! Canonical `orna.client-plan` artefact formats, versions 1, 2, and 3.
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
//! Each node starts with a tag byte followed by its exact payload. The node
//! set is closed: call, string, integer, Boolean, parameter read, field
//! path, concatenation, and external contract. Decoding enforces the closed
//! shape, the depth cap, the node-count cap, and per-node limits, so an
//! untrusted artefact cannot exhaust the evaluator.
//!
//! The format contains no source text, source locations, Orna names, or
//! backend values.

use std::fmt;

use orna_core::{FieldId, FunctionId, ParameterId, TypeId};

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

const RETURN_BOOLEAN_OPERATION: u8 = 1;
const RETURN_OPAQUE_OPERATION: u8 = 2;
const RETURN_EXPRESSION_OPERATION: u8 = 3;
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
        encode_expression_node(&self.expression, &mut writer)?;
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
) -> Result<(), ClientPlanError> {
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
                encode_expression_node(value, writer)?;
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
            if fields.len() > MAX_FIELD_PATH_LENGTH {
                return Err(ClientPlanError::ExpressionCollectionExceeded {
                    limit: MAX_FIELD_PATH_LENGTH,
                });
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
            encode_expression_node(left, writer)?;
            encode_expression_node(right, writer)?;
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

        let deep = ExpressionClientPlan::new(deep_concat(MAX_EXPRESSION_DEPTH + 1));
        assert_eq!(
            ExpressionClientPlan::decode(&deep.encode().expect("the deep plan encodes")),
            Err(ClientPlanError::ExpressionDepthExceeded)
        );

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
}
