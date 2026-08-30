//! Shared codec for fixed server artifacts that pin one parameter and value type.

use crate::artifact_codec::{DecodeError, Reader, Writer};
use orna_core::{ParameterId, TypeId};

pub(crate) const PAYLOAD_LEN: usize = 8 + size_of::<u32>() + 16 + 16;
pub(crate) const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// Format-specific constants for one pinned-parameter artifact family.
pub(crate) trait ParameterArtifactFormat {
    const MAGIC: [u8; 8];
    const VERSION: u32;
}

/// The identities decoded from one canonical artifact payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PinnedParameterArtifact {
    parameter: ParameterId,
    value_type: TypeId,
}

impl PinnedParameterArtifact {
    pub(crate) const fn parameter(self) -> ParameterId {
        self.parameter
    }

    pub(crate) const fn value_type(self) -> TypeId {
        self.value_type
    }
}

/// Format-neutral failures produced by the shared wire codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParameterArtifactCodecError {
    InvalidMagic,
    UnsupportedVersion(u32),
    UnexpectedParameter {
        actual: ParameterId,
        expected: ParameterId,
    },
    UnexpectedType {
        actual: TypeId,
        expected: TypeId,
    },
    ArtifactSizeLimit {
        size: usize,
        maximum: usize,
    },
    Truncated,
    TrailingBytes,
}

impl From<DecodeError> for ParameterArtifactCodecError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Truncated => Self::Truncated,
            DecodeError::TrailingBytes => Self::TrailingBytes,
        }
    }
}

pub(crate) fn encode<F: ParameterArtifactFormat>(
    parameter: ParameterId,
    value_type: TypeId,
) -> Result<Vec<u8>, ParameterArtifactCodecError> {
    let mut writer = Writer::with_capacity(PAYLOAD_LEN);
    writer.bytes(&F::MAGIC);
    writer.u32(F::VERSION);
    writer.parameter_id(parameter);
    writer.type_id(value_type);
    let bytes = writer.finish();
    validate_artifact_size(bytes.len())?;
    Ok(bytes)
}

pub(crate) fn decode<F: ParameterArtifactFormat>(
    bytes: &[u8],
    expected_parameter: ParameterId,
    expected_type: TypeId,
) -> Result<PinnedParameterArtifact, ParameterArtifactCodecError> {
    validate_artifact_size(bytes.len())?;
    let mut reader = Reader::new(bytes);
    if reader.array::<8>()? != F::MAGIC {
        return Err(ParameterArtifactCodecError::InvalidMagic);
    }
    let version = reader.u32()?;
    if version != F::VERSION {
        return Err(ParameterArtifactCodecError::UnsupportedVersion(version));
    }
    let parameter = reader.parameter_id()?;
    if parameter != expected_parameter {
        return Err(ParameterArtifactCodecError::UnexpectedParameter {
            actual: parameter,
            expected: expected_parameter,
        });
    }
    let value_type = reader.type_id()?;
    if value_type != expected_type {
        return Err(ParameterArtifactCodecError::UnexpectedType {
            actual: value_type,
            expected: expected_type,
        });
    }
    reader.require_finished()?;
    Ok(PinnedParameterArtifact {
        parameter,
        value_type,
    })
}

fn validate_artifact_size(size: usize) -> Result<(), ParameterArtifactCodecError> {
    if size > MAX_ARTIFACT_BYTES {
        Err(ParameterArtifactCodecError::ArtifactSizeLimit {
            size,
            maximum: MAX_ARTIFACT_BYTES,
        })
    } else {
        Ok(())
    }
}
