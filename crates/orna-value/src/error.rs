//! The payload-free OVB error taxonomy and its stable codes.

use crate::blob_role::{BlobRole, MediaFamily};
use std::fmt;

/// A deliberately payload-free failure.  Do not use `Debug` output of input
/// values in diagnostics or logs at this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    TrailingBytes,
    Limit,
    Unsupported,
    NonCanonical,
    InvalidUtf8,
    InvalidTag,
    InvalidValue,
    DuplicateOrUnorderedMapKey,
    ProtectedValue,
    InvalidPath,
    InvalidSchema,
    DecimalLimit,
    DivisionByZero,
    NonFiniteDecimal,
    InvalidMediaType,
    InvalidSuffix,
    IncompatibleSuffix,
    InvalidProfile,
    InvalidOid,
    ContentUnavailable,
    ContentDigestMismatch,
    ContentCollision,
    InvalidRange,
    MissingDescriptor,
    InvalidContext,
    /// The Blob's MIME family is a closed set (see `MediaFamily`), so naming it
    /// does not echo any caller-supplied media type.
    /// `length` is the Blob's byte length from its identity, so naming it
    /// reads no content.
    /// `expected` is the role the annotation selects, so the message can
    /// suggest a fix without echoing any caller-supplied value.
    RoleMismatch {
        role: BlobRole,
        family: Option<MediaFamily>,
        length: u64,
        expected: BlobRole,
        digest: [u8; 32],
    },
    OwnerExpired,
    Cancelled,
    QuotaExceeded,
    SinkFailed,
}

/// Stable, machine-readable codes for dispatch failures. The string form is
/// part of the contract, so a variant rename must not change it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    RoleMismatch,
    UnsupportedValueProfile,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoleMismatch => "OVB2_ROLE_MISMATCH",
            Self::UnsupportedValueProfile => "sys.codec.unsupported_value_profile",
        }
    }
}

impl Error {
    /// The stable code for this failure, when it has one.
    pub const fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::RoleMismatch { .. } => Some(ErrorCode::RoleMismatch),
            Self::InvalidProfile => Some(ErrorCode::UnsupportedValueProfile),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RoleMismatch {
                role,
                family,
                length,
                expected,
                digest,
            } => {
                let code = ErrorCode::RoleMismatch.as_str();
                let digest = orna_base::hex(digest);
                let family = family.map_or("unknown", MediaFamily::name);
                write!(
                    f,
                    "{code}: OVB-2 Blob role {role:?} does not admit MIME family {family} ({length} bytes, sha256 {digest}); admit as {expected:?} or Opaque"
                )
            }
            _ => write!(f, "OVB-1 {self:?}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<orna_cbor::Error> for Error {
    fn from(error: orna_cbor::Error) -> Self {
        match error {
            orna_cbor::Error::Truncated => Self::Truncated,
            orna_cbor::Error::NonCanonical => Self::NonCanonical,
            orna_cbor::Error::Unsupported => Self::Unsupported,
            orna_cbor::Error::InvalidUtf8 => Self::InvalidUtf8,
            orna_cbor::Error::DuplicateOrUnorderedMapKey => Self::DuplicateOrUnorderedMapKey,
            orna_cbor::Error::Limit => Self::Limit,
            orna_cbor::Error::BignumPayload => Self::InvalidValue,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
