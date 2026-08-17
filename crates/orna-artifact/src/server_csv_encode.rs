//! Canonical `orna.server-csv-encode` artifact format.
//!
//! The version-1 byte order is:
//!
//! ```text
//! magic[8] = ORNACSV\0
//! version: u32 big-endian = 1
//! parameter: [u8; 16]
//! value_type: [u8; 16]
//! ```
//!
//! The canonical payload is exactly 44 bytes. The parameter identity is the
//! fixed ADR 0067 `std.csv.encode.p_rows` identity (`...13`) and the value
//! type identity is the resolved `std.data.Rows` identity (`...12`); both are
//! validated against the expected identities supplied by the caller at decode
//! time. This format contains no SQL text, expression tree, function name,
//! default, object identifier, predicate, cast, value literal, or general
//! parameter-selection feature.

use std::fmt;

use orna_core::{ParameterId, TypeId};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.server-csv-encode";
/// The Orna language version whose semantics this artifact version executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The version used by all server csv-encode artifacts.
pub const FORMAT_VERSION: u32 = 1;
/// The exact first eight bytes of every server csv-encode artifact.
pub const MAGIC: [u8; 8] = *b"ORNACSV\0";
/// The exact encoded length in bytes of one version-1 artifact.
pub const PAYLOAD_LEN: usize = 44;
/// The maximum accepted encoded artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// A checked server csv-encode artifact.
///
/// The model carries exactly the pinned parameter identity and the resolved
/// value type identity. The identities are pinned by the caller at
/// construction and validated against the expected identities at decode time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CsvEncodePlan {
    parameter: ParameterId,
    value_type: TypeId,
}

impl CsvEncodePlan {
    /// Builds a checked server csv-encode model.
    ///
    /// The model itself has no invariant beyond its two opaque identities;
    /// signature pinning happens at [`Self::decode`] time against the
    /// expected identities supplied by the caller.
    pub fn new(parameter: ParameterId, value_type: TypeId) -> Result<Self, CsvEncodePlanError> {
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
    pub fn encode(&self) -> Result<Vec<u8>, CsvEncodePlanError> {
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
    /// The decoder requires the exact magic, version, and 45-byte length. It
    /// consumes all 44 bytes and rejects a wrong magic, a wrong version,
    /// truncation, excess bytes, a parameter identity other than
    /// `expected_parameter`, and a value type identity other than
    /// `expected_type` before it returns an executable representation.
    pub fn decode(
        bytes: &[u8],
        expected_parameter: ParameterId,
        expected_type: TypeId,
    ) -> Result<Self, CsvEncodePlanError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        let magic = reader.array::<8>()?;
        if magic != MAGIC {
            return Err(CsvEncodePlanError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(CsvEncodePlanError::UnsupportedVersion(version));
        }
        let parameter = reader.parameter_id()?;
        if parameter != expected_parameter {
            return Err(CsvEncodePlanError::UnexpectedParameter {
                actual: parameter,
                expected: expected_parameter,
            });
        }
        let value_type = reader.type_id()?;
        if value_type != expected_type {
            return Err(CsvEncodePlanError::UnexpectedType {
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

/// An error returned when a server csv-encode artifact cannot be decoded or
/// encoded safely.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CsvEncodePlanError {
    /// The artifact does not start with the csv-encode magic bytes.
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

impl fmt::Display for CsvEncodePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => {
                formatter.write_str("invalid orna.server-csv-encode artifact magic")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.server-csv-encode artifact version {version}"
            ),
            Self::UnexpectedParameter { actual, expected } => write!(
                formatter,
                "server csv-encode artifact pins parameter {actual}, expected {expected}"
            ),
            Self::UnexpectedType { actual, expected } => write!(
                formatter,
                "server csv-encode artifact pins value type {actual}, expected {expected}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "orna.server-csv-encode artifact size {size} exceeds the limit {maximum}"
            ),
            Self::Truncated => formatter.write_str("truncated orna.server-csv-encode artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after orna.server-csv-encode artifact")
            }
            Self::Internal(reason) => {
                write!(formatter, "invalid server csv-encode model: {reason}")
            }
        }
    }
}

impl std::error::Error for CsvEncodePlanError {}

fn validate_artifact_size(size: usize) -> Result<(), CsvEncodePlanError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(CsvEncodePlanError::ArtifactSizeLimit {
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

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], CsvEncodePlanError> {
        let bytes = self.take(LENGTH)?;
        bytes.try_into().map_err(|_| CsvEncodePlanError::Truncated)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CsvEncodePlanError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CsvEncodePlanError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(CsvEncodePlanError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, CsvEncodePlanError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn parameter_id(&mut self) -> Result<ParameterId, CsvEncodePlanError> {
        Ok(ParameterId::from_bytes(self.array()?))
    }

    fn type_id(&mut self) -> Result<TypeId, CsvEncodePlanError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    fn require_finished(&self) -> Result<(), CsvEncodePlanError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(CsvEncodePlanError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter() -> ParameterId {
        ParameterId::from_bytes([0x13; 16])
    }

    fn rows_type() -> TypeId {
        TypeId::from_bytes([0x12; 16])
    }

    #[test]
    fn encode_and_decode_round_trip_exactly() {
        let plan = CsvEncodePlan::new(parameter(), rows_type()).unwrap();
        let bytes = plan.encode().unwrap();
        assert_eq!(bytes.len(), PAYLOAD_LEN);
        assert_eq!(&bytes[..8], &MAGIC);
        let decoded = CsvEncodePlan::decode(&bytes, parameter(), rows_type()).unwrap();
        assert_eq!(decoded, plan);
    }

    #[test]
    fn decode_rejects_wrong_magic_version_and_identities() {
        let plan = CsvEncodePlan::new(parameter(), rows_type()).unwrap();
        let mut bytes = plan.encode().unwrap();

        let mut wrong_magic = bytes.clone();
        wrong_magic[0] ^= 0xff;
        assert_eq!(
            CsvEncodePlan::decode(&wrong_magic, parameter(), rows_type()),
            Err(CsvEncodePlanError::InvalidMagic)
        );

        bytes[8..12].copy_from_slice(&2u32.to_be_bytes());
        assert_eq!(
            CsvEncodePlan::decode(&bytes, parameter(), rows_type()),
            Err(CsvEncodePlanError::UnsupportedVersion(2))
        );
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());

        let other = ParameterId::from_bytes([0x14; 16]);
        assert_eq!(
            CsvEncodePlan::decode(&bytes, other, rows_type()),
            Err(CsvEncodePlanError::UnexpectedParameter {
                actual: parameter(),
                expected: other
            })
        );

        let other_type = TypeId::from_bytes([0x11; 16]);
        assert_eq!(
            CsvEncodePlan::decode(&bytes, parameter(), other_type),
            Err(CsvEncodePlanError::UnexpectedType {
                actual: rows_type(),
                expected: other_type
            })
        );
    }

    #[test]
    fn decode_rejects_truncation_and_trailing_bytes() {
        let plan = CsvEncodePlan::new(parameter(), rows_type()).unwrap();
        let bytes = plan.encode().unwrap();

        assert_eq!(
            CsvEncodePlan::decode(&bytes[..bytes.len() - 1], parameter(), rows_type()),
            Err(CsvEncodePlanError::Truncated)
        );

        let mut with_trailing = bytes.clone();
        with_trailing.push(0);
        assert_eq!(
            CsvEncodePlan::decode(&with_trailing, parameter(), rows_type()),
            Err(CsvEncodePlanError::TrailingBytes)
        );
    }

    #[test]
    fn error_variants_display_without_panicking() {
        let errors = [
            CsvEncodePlanError::InvalidMagic,
            CsvEncodePlanError::UnsupportedVersion(2),
            CsvEncodePlanError::UnexpectedParameter {
                actual: parameter(),
                expected: ParameterId::from_bytes([0x14; 16]),
            },
            CsvEncodePlanError::UnexpectedType {
                actual: rows_type(),
                expected: TypeId::from_bytes([0x11; 16]),
            },
            CsvEncodePlanError::ArtifactSizeLimit {
                size: 100,
                maximum: 50,
            },
            CsvEncodePlanError::Truncated,
            CsvEncodePlanError::TrailingBytes,
            CsvEncodePlanError::Internal("test"),
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
        }
    }
}
