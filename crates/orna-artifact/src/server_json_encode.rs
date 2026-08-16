//! Canonical `orna.server-json-encode` artifact format.
//!
//! The version-1 byte order is:
//!
//! ```text
//! magic[8] = ORNAJE\0\0
//! version: u32 big-endian = 1
//! parameter: [u8; 16]
//! value_type: [u8; 16]
//! ```
//!
//! The canonical payload is exactly 44 bytes. The parameter identity is the
//! fixed ADR 0057 `std.json.encode.p_value` identity (`...11`) and the value
//! type identity is the resolved `std.json.Value` identity (`...11`); both are
//! validated against the expected identities supplied by the caller at decode
//! time. This format contains no SQL text, expression tree, function name,
//! default, object identifier, predicate, cast, value literal, or general
//! parameter-selection feature.

use std::fmt;

use orna_core::{ParameterId, TypeId};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.server-json-encode";
/// The Orna language version whose semantics this artifact version executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The version used by all server json-encode artifacts.
pub const FORMAT_VERSION: u32 = 1;
/// The exact first eight bytes of every server json-encode artifact.
pub const MAGIC: [u8; 8] = *b"ORNAJE\0\0";
/// The exact encoded length in bytes of one version-1 artifact.
pub const PAYLOAD_LEN: usize = 44;
/// The maximum accepted encoded artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// A checked server json-encode artifact.
///
/// The model carries exactly the pinned parameter identity and the resolved
/// value type identity. The identities are pinned by the caller at
/// construction and validated against the expected identities at decode time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonEncodePlan {
    parameter: ParameterId,
    value_type: TypeId,
}

impl JsonEncodePlan {
    /// Builds a checked server json-encode model.
    ///
    /// The model itself has no invariant beyond its two opaque identities;
    /// signature pinning happens at [`Self::decode`] time against the
    /// expected identities supplied by the caller.
    pub fn new(parameter: ParameterId, value_type: TypeId) -> Result<Self, JsonEncodePlanError> {
        Ok(Self {
            parameter,
            value_type,
        })
    }

    /// Returns the pinned parameter identity.
    pub const fn parameter(&self) -> ParameterId {
        self.parameter
    }

    /// Returns the resolved value type identity.
    pub const fn value_type(&self) -> TypeId {
        self.value_type
    }

    /// Encodes this model into the canonical version-1 artifact bytes.
    pub fn encode(&self) -> Result<Vec<u8>, JsonEncodePlanError> {
        let mut writer = Writer::with_capacity(PAYLOAD_LEN);
        writer.bytes(&MAGIC);
        writer.u32(FORMAT_VERSION);
        writer.parameter_id(self.parameter);
        writer.type_id(self.value_type);
        let bytes = writer.finish();
        validate_artifact_size(bytes.len())?;
        Ok(bytes)
    }

    /// Decodes exactly one canonical version-1 artifact.
    ///
    /// The decoder requires the exact magic, version, and 44-byte length. It
    /// consumes all 44 bytes and rejects a wrong magic, a wrong version,
    /// truncation, excess bytes, a parameter identity other than
    /// `expected_parameter`, and a value type identity other than
    /// `expected_type` before it returns an executable representation.
    pub fn decode(
        bytes: &[u8],
        expected_parameter: ParameterId,
        expected_type: TypeId,
    ) -> Result<Self, JsonEncodePlanError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        let magic = reader.array::<8>()?;
        if magic != MAGIC {
            return Err(JsonEncodePlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(JsonEncodePlanError::UnsupportedVersion(version));
        }
        let parameter = reader.parameter_id()?;
        if parameter != expected_parameter {
            return Err(JsonEncodePlanError::UnexpectedParameter {
                actual: parameter,
                expected: expected_parameter,
            });
        }
        let value_type = reader.type_id()?;
        if value_type != expected_type {
            return Err(JsonEncodePlanError::UnexpectedType {
                actual: value_type,
                expected: expected_type,
            });
        }
        reader.require_finished()?;
        Ok(Self {
            parameter,
            value_type,
        })
    }
}

/// An error returned when a server json-encode artifact cannot be decoded or
/// encoded safely.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonEncodePlanError {
    /// The artifact does not start with the json-encode magic bytes.
    InvalidMagic,
    /// The artifact version is not supported.
    UnsupportedVersion(u32),
    /// The artifact pins a parameter identity other than the expected one.
    UnexpectedParameter {
        /// The identity pinned by the artifact.
        actual: ParameterId,
        /// The identity required by the pinned function signature.
        expected: ParameterId,
    },
    /// The artifact pins a value type identity other than the expected one.
    UnexpectedType {
        /// The identity pinned by the artifact.
        actual: TypeId,
        /// The identity required by the pinned function signature.
        expected: TypeId,
    },
    /// The encoded artifact exceeds the format byte limit.
    ArtifactSizeLimit {
        /// The supplied artifact size.
        size: usize,
        /// The largest accepted artifact size.
        maximum: usize,
    },
    /// The artifact ends before a complete value can be read.
    Truncated,
    /// The artifact contains bytes after a complete value.
    TrailingBytes,
    /// A decoded or supplied model violates an internal invariant.
    Internal(&'static str),
}

impl fmt::Display for JsonEncodePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => {
                formatter.write_str("invalid orna.server-json-encode artifact magic")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.server-json-encode artifact version {version}"
            ),
            Self::UnexpectedParameter { actual, expected } => write!(
                formatter,
                "server json-encode artifact pins parameter {actual}, expected {expected}"
            ),
            Self::UnexpectedType { actual, expected } => write!(
                formatter,
                "server json-encode artifact pins value type {actual}, expected {expected}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "orna.server-json-encode artifact size {size} exceeds the limit {maximum}"
            ),
            Self::Truncated => formatter.write_str("truncated orna.server-json-encode artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.server-json-encode artifact")
            }
            Self::Internal(reason) => {
                write!(formatter, "invalid server json-encode model: {reason}")
            }
        }
    }
}

impl std::error::Error for JsonEncodePlanError {}

fn validate_artifact_size(size: usize) -> Result<(), JsonEncodePlanError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(JsonEncodePlanError::ArtifactSizeLimit {
            size,
            maximum: MAX_ARTIFACT_BYTES,
        })
    } else {
        Ok(())
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn parameter_id(&mut self, id: ParameterId) {
        self.bytes(&id.to_bytes());
    }

    fn type_id(&mut self, id: TypeId) {
        self.bytes(&id.to_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], JsonEncodePlanError> {
        let bytes = self.take(LENGTH)?;
        bytes.try_into().map_err(|_| JsonEncodePlanError::Truncated)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], JsonEncodePlanError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(JsonEncodePlanError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(JsonEncodePlanError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, JsonEncodePlanError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn parameter_id(&mut self) -> Result<ParameterId, JsonEncodePlanError> {
        Ok(ParameterId::from_bytes(self.array()?))
    }

    fn type_id(&mut self) -> Result<TypeId, JsonEncodePlanError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    fn require_finished(&self) -> Result<(), JsonEncodePlanError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(JsonEncodePlanError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed ADR 0057 `std.json.encode.p_value` parameter identity: `...11`.
    const PARAMETER: ParameterId =
        ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
    const OTHER_PARAMETER: ParameterId =
        ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x21]);
    /// The resolved ADR 0057 `std.json.Value` type identity: `...11`.
    const VALUE_TYPE: TypeId =
        TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
    const OTHER_TYPE: TypeId =
        TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x22]);

    /// The exact canonical version-1 payload from ADR 0057 step 4.
    const CANONICAL: [u8; PAYLOAD_LEN] = [
        // bytes 0..8: ASCII `ORNAJE\0\0`
        b'O', b'R', b'N', b'A', b'J', b'E', 0, 0,
        // bytes 8..12: u32 big-endian format version 1
        0, 0, 0, 1, // bytes 12..28: raw `ParameterId` `...11`
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11,
        // bytes 28..44: raw resolved `std.json.Value` `TypeId` `...11`
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11,
    ];

    #[test]
    fn round_trip_is_exact_canonical_payload() {
        let plan = JsonEncodePlan::new(PARAMETER, VALUE_TYPE)
            .expect("any opaque identities form a valid model");
        assert_eq!(plan.parameter(), PARAMETER);
        assert_eq!(plan.value_type(), VALUE_TYPE);

        let encoded = plan.encode().expect("canonical model encodes");
        assert_eq!(encoded.len(), PAYLOAD_LEN);
        assert_eq!(encoded, CANONICAL);

        let decoded = JsonEncodePlan::decode(&encoded, PARAMETER, VALUE_TYPE)
            .expect("canonical payload decodes with pinned identities");
        assert_eq!(decoded, plan);
    }

    #[test]
    fn decode_rejects_wrong_magic() {
        let mut bytes = CANONICAL;
        bytes[0] = b'X';
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::InvalidMagic)
        );

        let mut bytes = CANONICAL;
        bytes[7] = b'X';
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::InvalidMagic)
        );
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let mut bytes = CANONICAL;
        bytes[11] = 2;
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::UnsupportedVersion(2))
        );

        let mut bytes = CANONICAL;
        bytes[8..12].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::UnsupportedVersion(0))
        );
    }

    #[test]
    fn decode_rejects_truncation_at_every_prefix() {
        for length in 0..PAYLOAD_LEN {
            assert_eq!(
                JsonEncodePlan::decode(&CANONICAL[..length], PARAMETER, VALUE_TYPE),
                Err(JsonEncodePlanError::Truncated),
                "prefix length {length} must be truncated"
            );
        }
    }

    #[test]
    fn decode_rejects_excess_bytes() {
        let mut bytes = Vec::from(CANONICAL);
        bytes.push(0);
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::TrailingBytes)
        );

        let mut bytes = Vec::from(CANONICAL);
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::TrailingBytes)
        );
    }

    #[test]
    fn decode_rejects_wrong_parameter_identity() {
        let mut bytes = CANONICAL;
        bytes[27] = 0x21;
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::UnexpectedParameter {
                actual: OTHER_PARAMETER,
                expected: PARAMETER,
            })
        );
    }

    #[test]
    fn decode_rejects_wrong_type_identity() {
        let mut bytes = CANONICAL;
        bytes[43] = 0x22;
        assert_eq!(
            JsonEncodePlan::decode(&bytes, PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::UnexpectedType {
                actual: OTHER_TYPE,
                expected: VALUE_TYPE,
            })
        );
    }

    #[test]
    fn decode_rejects_size_limit() {
        assert_eq!(
            JsonEncodePlan::decode(&vec![0; MAX_ARTIFACT_BYTES + 1], PARAMETER, VALUE_TYPE),
            Err(JsonEncodePlanError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );
    }

    #[test]
    fn error_display_messages_are_stable() {
        let messages = [
            (
                JsonEncodePlanError::InvalidMagic,
                "invalid orna.server-json-encode artifact magic",
            ),
            (
                JsonEncodePlanError::UnsupportedVersion(2),
                "unsupported orna.server-json-encode artifact version 2",
            ),
            (
                JsonEncodePlanError::Truncated,
                "truncated orna.server-json-encode artifact",
            ),
            (
                JsonEncodePlanError::TrailingBytes,
                "trailing bytes after orna.server-json-encode artifact",
            ),
            (
                JsonEncodePlanError::Internal("unreachable"),
                "invalid server json-encode model: unreachable",
            ),
        ];
        for (error, message) in messages {
            assert_eq!(error.to_string(), message);
        }
        assert!(
            JsonEncodePlanError::UnexpectedParameter {
                actual: OTHER_PARAMETER,
                expected: PARAMETER,
            }
            .to_string()
            .contains("parameter")
        );
        assert!(
            JsonEncodePlanError::UnexpectedType {
                actual: OTHER_TYPE,
                expected: VALUE_TYPE,
            }
            .to_string()
            .contains("value type")
        );
        assert!(
            JsonEncodePlanError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            }
            .to_string()
            .contains("size")
        );
    }
}
