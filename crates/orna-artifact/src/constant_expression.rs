//! Canonical `orna.constant-expression` artifact format, version 1.
//!
//! The format stores the closed constant set accepted by the first compiler
//! slice. It is independent of source spelling, compiler types, and backend
//! storage:
//!
//! ```text
//! magic[8] = ORNACE\0\0
//! version: u32 big-endian = 1
//! kind: u8
//! payload: kind-specific bytes
//! ```

use std::{fmt, str};

/// The stable public identity of this artifact format.
pub const FORMAT_IDENTITY: &str = "orna.constant-expression";
/// The only supported constant-expression artifact version.
pub const FORMAT_VERSION: u32 = 1;
/// The exact first eight bytes of every constant-expression artifact.
pub const MAGIC: [u8; 8] = *b"ORNACE\0\0";
/// The maximum accepted encoded artifact size.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// A source-independent constant expression value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantExpression {
    /// The SQL null value. Its resolved target type belongs to the catalogue.
    Null,
    /// A BOOLEAN constant.
    Boolean(bool),
    /// A signed 64-bit INTEGER constant.
    Integer(i64),
    /// An exact UTF-8 character string constant.
    Text(String),
}

impl ConstantExpression {
    /// Encodes this value into canonical version-1 bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ConstantExpressionError> {
        let encoded_size = self.encoded_size()?;
        validate_artifact_size(encoded_size)?;
        let mut bytes = Vec::with_capacity(encoded_size);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        match self {
            Self::Null => bytes.push(1),
            Self::Boolean(value) => {
                bytes.push(2);
                bytes.push(u8::from(*value));
            }
            Self::Integer(value) => {
                bytes.push(3);
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            Self::Text(value) => {
                bytes.push(4);
                let length = u32::try_from(value.len()).map_err(|_| {
                    ConstantExpressionError::TextSizeLimit {
                        size: value.len(),
                        maximum: u32::MAX as usize,
                    }
                })?;
                bytes.extend_from_slice(&length.to_be_bytes());
                bytes.extend_from_slice(value.as_bytes());
            }
        }
        debug_assert_eq!(bytes.len(), encoded_size);
        Ok(bytes)
    }

    fn encoded_size(&self) -> Result<usize, ConstantExpressionError> {
        let payload_size = match self {
            Self::Null => 0,
            Self::Boolean(_) => 1,
            Self::Integer(_) => size_of::<i64>(),
            Self::Text(value) => {
                u32::try_from(value.len()).map_err(|_| ConstantExpressionError::TextSizeLimit {
                    size: value.len(),
                    maximum: u32::MAX as usize,
                })?;
                size_of::<u32>() + value.len()
            }
        };
        Ok(MAGIC.len() + size_of::<u32>() + size_of::<u8>() + payload_size)
    }

    /// Decodes exactly one canonical version-1 artifact.
    pub fn decode(bytes: &[u8]) -> Result<Self, ConstantExpressionError> {
        validate_artifact_size(bytes.len())?;
        let mut reader = Reader::new(bytes);
        if reader.array::<8>()? != MAGIC {
            return Err(ConstantExpressionError::InvalidMagic);
        }
        let version = reader.u32()?;
        if version != FORMAT_VERSION {
            return Err(ConstantExpressionError::UnsupportedVersion(version));
        }
        let value = match reader.u8()? {
            1 => Self::Null,
            2 => Self::Boolean(reader.boolean()?),
            3 => Self::Integer(reader.i64()?),
            4 => {
                let length = reader.u32()? as usize;
                let value = reader.bytes(length)?;
                Self::Text(
                    str::from_utf8(value)
                        .map_err(|_| ConstantExpressionError::InvalidUtf8)?
                        .to_owned(),
                )
            }
            tag => return Err(ConstantExpressionError::InvalidKindTag(tag)),
        };
        reader.require_finished()?;
        Ok(value)
    }
}

/// An error returned for an invalid or unsupported constant artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantExpressionError {
    /// The artifact does not start with the version-1 magic bytes.
    InvalidMagic,
    /// The artifact version is not supported.
    UnsupportedVersion(u32),
    /// The value-kind tag is not defined by version 1.
    InvalidKindTag(u8),
    /// A BOOLEAN payload was not zero or one.
    InvalidBoolean(u8),
    /// A TEXT payload is not valid UTF-8.
    InvalidUtf8,
    /// A TEXT value cannot fit the format's length prefix.
    TextSizeLimit {
        /// The supplied UTF-8 byte length.
        size: usize,
        /// The largest representable byte length.
        maximum: usize,
    },
    /// The encoded artifact exceeds the version-1 resource limit.
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
}

impl fmt::Display for ConstantExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => {
                formatter.write_str("invalid orna.constant-expression artifact magic")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orna.constant-expression artifact version {version}"
            ),
            Self::InvalidKindTag(tag) => {
                write!(formatter, "invalid constant-expression kind tag {tag}")
            }
            Self::InvalidBoolean(value) => {
                write!(
                    formatter,
                    "invalid constant-expression boolean byte {value}"
                )
            }
            Self::InvalidUtf8 => formatter.write_str("constant TEXT payload is not UTF-8"),
            Self::TextSizeLimit { size, maximum } => write!(
                formatter,
                "constant TEXT size {size} exceeds format limit {maximum}"
            ),
            Self::ArtifactSizeLimit { size, maximum } => write!(
                formatter,
                "constant-expression artifact size {size} exceeds version-1 limit {maximum}"
            ),
            Self::Truncated => formatter.write_str("truncated constant-expression artifact"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after constant-expression artifact")
            }
        }
    }
}

impl std::error::Error for ConstantExpressionError {}

fn validate_artifact_size(size: usize) -> Result<(), ConstantExpressionError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(ConstantExpressionError::ArtifactSizeLimit {
            size,
            maximum: MAX_ARTIFACT_BYTES,
        })
    } else {
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], ConstantExpressionError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ConstantExpressionError::Truncated)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ConstantExpressionError::Truncated)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], ConstantExpressionError> {
        self.bytes(LENGTH)?
            .try_into()
            .map_err(|_| ConstantExpressionError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, ConstantExpressionError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ConstantExpressionError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, ConstantExpressionError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    fn boolean(&mut self) -> Result<bool, ConstantExpressionError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(ConstantExpressionError::InvalidBoolean(value)),
        }
    }

    fn require_finished(&self) -> Result<(), ConstantExpressionError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ConstantExpressionError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_golden_values_and_round_trips_every_kind() {
        let null = ConstantExpression::Null.encode().unwrap();
        assert_eq!(
            null,
            [
                MAGIC.as_slice(),
                FORMAT_VERSION.to_be_bytes().as_slice(),
                &[1],
            ]
            .concat()
        );

        for value in [
            ConstantExpression::Null,
            ConstantExpression::Boolean(false),
            ConstantExpression::Boolean(true),
            ConstantExpression::Integer(i64::MIN),
            ConstantExpression::Integer(42),
            ConstantExpression::Text(String::new()),
            ConstantExpression::Text("cafe\u{301}\r\n".to_owned()),
        ] {
            let encoded = value.encode().unwrap();
            assert_eq!(ConstantExpression::decode(&encoded), Ok(value));
        }
    }

    #[test]
    fn uses_big_endian_integer_and_utf8_text_payloads() {
        let integer = ConstantExpression::Integer(0x0102_0304_0506_0708)
            .encode()
            .unwrap();
        assert_eq!(&integer[13..], &0x0102_0304_0506_0708_i64.to_be_bytes());

        let text = ConstantExpression::Text("é".to_owned()).encode().unwrap();
        assert_eq!(&text[13..17], &2_u32.to_be_bytes());
        assert_eq!(&text[17..], "é".as_bytes());
    }

    #[test]
    fn rejects_corruption_truncation_and_trailing_bytes() {
        let valid = ConstantExpression::Boolean(true).encode().unwrap();

        let mut bad_magic = valid.clone();
        bad_magic[0] ^= 0xff;
        assert_eq!(
            ConstantExpression::decode(&bad_magic),
            Err(ConstantExpressionError::InvalidMagic)
        );

        let mut bad_version = valid.clone();
        bad_version[8..12].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            ConstantExpression::decode(&bad_version),
            Err(ConstantExpressionError::UnsupportedVersion(2))
        );

        let mut bad_tag = valid.clone();
        bad_tag[12] = 9;
        assert_eq!(
            ConstantExpression::decode(&bad_tag),
            Err(ConstantExpressionError::InvalidKindTag(9))
        );

        let mut bad_boolean = valid.clone();
        bad_boolean[13] = 2;
        assert_eq!(
            ConstantExpression::decode(&bad_boolean),
            Err(ConstantExpressionError::InvalidBoolean(2))
        );

        assert_eq!(
            ConstantExpression::decode(&valid[..valid.len() - 1]),
            Err(ConstantExpressionError::Truncated)
        );
        let mut trailing = valid;
        trailing.push(0);
        assert_eq!(
            ConstantExpression::decode(&trailing),
            Err(ConstantExpressionError::TrailingBytes)
        );
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_artifacts() {
        let invalid_utf8 = [
            MAGIC.as_slice(),
            FORMAT_VERSION.to_be_bytes().as_slice(),
            &[4],
            1_u32.to_be_bytes().as_slice(),
            &[0xff],
        ]
        .concat();
        assert_eq!(
            ConstantExpression::decode(&invalid_utf8),
            Err(ConstantExpressionError::InvalidUtf8)
        );

        let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
        assert_eq!(
            ConstantExpression::decode(&oversized),
            Err(ConstantExpressionError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 1,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );

        let oversized_text = ConstantExpression::Text("x".repeat(MAX_ARTIFACT_BYTES));
        assert_eq!(
            oversized_text.encode(),
            Err(ConstantExpressionError::ArtifactSizeLimit {
                size: MAX_ARTIFACT_BYTES + 17,
                maximum: MAX_ARTIFACT_BYTES,
            })
        );
    }
}
