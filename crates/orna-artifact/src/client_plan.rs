//! Canonical `orna.client-plan` artefact formats, versions 1 and 2.
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
//! The format contains no source text, source locations, Orna names, or
//! backend values.

use std::fmt;

use orna_core::TypeId;

/// The stable public identity of this artefact format.
pub const FORMAT_IDENTITY: &str = "orna.client-plan";
/// The Orna language version whose semantics this artefact executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The client-plan version that returns one Boolean constant.
pub const FORMAT_VERSION: u32 = 1;
/// The client-plan version that returns one registered opaque value.
pub const OPAQUE_FORMAT_VERSION: u32 = 2;
/// The exact first eight bytes of every client-plan artefact.
pub const MAGIC: [u8; 8] = *b"ORNACP\0\0";

const RETURN_BOOLEAN_OPERATION: u8 = 1;
const RETURN_OPAQUE_OPERATION: u8 = 2;
const ENCODED_LENGTH: usize = MAGIC.len() + size_of::<u32>() + 2;
const OPAQUE_PAYLOAD_LENGTH: usize = 16;
const OPAQUE_ENCODED_LENGTH: usize =
    MAGIC.len() + size_of::<u32>() + 1 + 16 + size_of::<u32>() + OPAQUE_PAYLOAD_LENGTH;

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
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
