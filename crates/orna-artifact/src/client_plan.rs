//! Canonical `orna.client-plan` artefact format, version 1.
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
//! The format contains no source text, source locations, Orna names, or
//! backend values.

use std::fmt;

/// The stable public identity of this artefact format.
pub const FORMAT_IDENTITY: &str = "orna.client-plan";
/// The Orna language version whose semantics this artefact executes.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";
/// The only supported client-plan artefact version.
pub const FORMAT_VERSION: u32 = 1;
/// The exact first eight bytes of every client-plan artefact.
pub const MAGIC: [u8; 8] = *b"ORNACP\0\0";

const RETURN_BOOLEAN_OPERATION: u8 = 1;
const ENCODED_LENGTH: usize = MAGIC.len() + size_of::<u32>() + 2;

/// A checked CLIENT plan that returns one Boolean constant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientPlan {
    returned_boolean: bool,
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
    /// The artefact does not start with the version-1 magic bytes.
    InvalidMagic,
    /// The artefact version is not supported.
    UnsupportedVersion(u32),
    /// The operation tag is not defined by version 1.
    InvalidOperation(u8),
    /// The Boolean payload is not zero or one.
    InvalidBoolean(u8),
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
