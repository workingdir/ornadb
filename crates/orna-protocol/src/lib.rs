//! Canonical runtime values and the bounded authenticated raw-call protocol.

mod frame;

pub use frame::{
    CallArgument, CallFailure, Channel, ClientAction, ClientFrame, ConnectionError, Event,
    EventRecord, FrameCodecError, MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection, RawCall,
    ServerAction, ServerFrame, decode_catalogue_client_frame, decode_catalogue_server_frame,
    decode_client_frame, decode_server_frame, encode_catalogue_client_frame,
    encode_catalogue_server_frame, encode_client_frame, encode_server_frame,
};

use std::{error::Error, fmt};

use orna_core::{
    ObjectId, TypeId,
    catalogue::CatalogueSnapshot,
    types::{ResolvedType, StandardScalar},
    value::{EnumValue, EnumValueError, RuntimeFloat, RuntimeValue},
};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    FLOAT_TYPE_ID, INTEGER_TYPE_ID, STANDARD_TYPE_IDS,
};

const MARKER: &[u8; 4] = b"ORV1";
const CATALOGUE_MARKER: &[u8; 4] = b"ORV2";
const HEADER_LENGTH: usize = 25;
const NULL_SCALAR_TAG: u8 = 0x00;
const NULL_REFERENCE_TAG: u8 = 0x01;
const BOOLEAN_TAG: u8 = 0x02;
const INTEGER_TAG: u8 = 0x03;
const BIGINT_TAG: u8 = 0x04;
const FLOAT_TAG: u8 = 0x05;
const TEXT_TAG: u8 = 0x06;
const BYTES_TAG: u8 = 0x07;
const REFERENCE_TAG: u8 = 0x08;
const NULL_ENUM_TAG: u8 = 0x09;
const ENUM_TAG: u8 = 0x0a;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const SUPPORTED_SCALAR_TYPES: [(TypeId, StandardScalar); 6] = [
    (BOOLEAN_TYPE_ID, StandardScalar::Boolean),
    (INTEGER_TYPE_ID, StandardScalar::Integer),
    (BIGINT_TYPE_ID, StandardScalar::BigInt),
    (FLOAT_TYPE_ID, StandardScalar::Float),
    (
        CHARACTER_LARGE_OBJECT_TYPE_ID,
        StandardScalar::CharacterLargeObject,
    ),
    (
        BINARY_LARGE_OBJECT_TYPE_ID,
        StandardScalar::BinaryLargeObject,
    ),
];

/// An error from canonical runtime value encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueCodecError {
    /// The runtime value category is not defined by codec version 1.
    UnsupportedValue,
    /// The encoded value does not contain the complete fixed header.
    TruncatedHeader {
        /// The total number of available bytes.
        actual: usize,
    },
    /// The encoded value does not start with the version-1 marker.
    InvalidMarker,
    /// The value tag is not defined by codec version 1.
    UnknownTag {
        /// The unrecognised wire tag.
        tag: u8,
    },
    /// The value tag and stable type identity do not agree.
    WrongType {
        /// The recognised wire tag.
        tag: u8,
        /// The incompatible type identity.
        actual: TypeId,
    },
    /// The encoded payload is shorter than its declared length.
    TruncatedPayload {
        /// The length declared by the header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// Bytes occur after the declared payload.
    TrailingBytes {
        /// The length declared by the header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// A fixed-width payload has the wrong length.
    WrongPayloadLength {
        /// The recognised wire tag.
        tag: u8,
        /// The only valid payload length for this tag.
        expected: usize,
        /// The declared and available payload length.
        actual: usize,
    },
    /// A Boolean payload is not the canonical zero or one byte.
    InvalidBoolean {
        /// The invalid payload byte.
        value: u8,
    },
    /// A float payload is non-finite or is the non-canonical negative zero.
    NonCanonicalFloat,
    /// A supplied or declared payload exceeds the version-1 limit.
    PayloadTooLarge {
        /// The supplied or declared payload length.
        actual: usize,
        /// The maximum version-1 payload length.
        maximum: usize,
    },
    /// A text payload is not valid UTF-8.
    InvalidUtf8,
    /// A stable standard scalar identity was used as a reference target.
    StandardTypeAsReference {
        /// The stable scalar identity used as a reference target.
        target: TypeId,
    },
    /// The active catalogue does not contain the supplied enum type.
    InactiveEnumType {
        /// The inactive enum type identity.
        enum_type: TypeId,
    },
    /// The active enum type does not declare the encoded exact label.
    UndeclaredEnumLabel {
        /// The active enum type identity.
        enum_type: TypeId,
        /// The undeclared label.
        label: String,
    },
}

impl fmt::Display for ValueCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedValue => {
                formatter.write_str("runtime value is not supported by codec version 1")
            }
            Self::TruncatedHeader { .. } => {
                formatter.write_str("runtime value header is truncated")
            }
            Self::InvalidMarker => formatter.write_str("runtime value marker is invalid"),
            Self::UnknownTag { .. } => formatter.write_str("runtime value tag is unknown"),
            Self::WrongType { .. } => {
                formatter.write_str("runtime value tag and type identity do not agree")
            }
            Self::TruncatedPayload { .. } => {
                formatter.write_str("runtime value payload is truncated")
            }
            Self::TrailingBytes { .. } => formatter.write_str("runtime value has trailing bytes"),
            Self::WrongPayloadLength { .. } => {
                formatter.write_str("runtime value payload has the wrong length")
            }
            Self::InvalidBoolean { .. } => {
                formatter.write_str("BOOLEAN payload must be zero or one")
            }
            Self::NonCanonicalFloat => {
                formatter.write_str("FLOAT payload is not canonical and finite")
            }
            Self::PayloadTooLarge { .. } => {
                formatter.write_str("runtime value payload exceeds the codec limit")
            }
            Self::InvalidUtf8 => formatter.write_str("text payload is not valid UTF-8"),
            Self::StandardTypeAsReference { .. } => {
                formatter.write_str("stable standard scalar cannot be a reference target")
            }
            Self::InactiveEnumType { .. } => {
                formatter.write_str("enum type is not active for the canonical value")
            }
            Self::UndeclaredEnumLabel { .. } => {
                formatter.write_str("enum label is not declared by the active type")
            }
        }
    }
}

impl Error for ValueCodecError {}

/// Encodes one runtime value as canonical version-1 bytes.
///
/// # Errors
///
/// Returns [`ValueCodecError::PayloadTooLarge`] for text or byte payloads over
/// 16 MiB. Returns [`ValueCodecError::StandardTypeAsReference`] when a
/// reference uses a stable standard scalar identity. Returns
/// [`ValueCodecError::UnsupportedValue`] when the non-exhaustive core value
/// model contains a category that version 1 does not define.
pub fn encode_value(value: &RuntimeValue) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Boolean(value) => {
            let payload = [u8::from(*value)];
            Ok(encode(BOOLEAN_TAG, BOOLEAN_TYPE_ID, &payload))
        }
        RuntimeValue::Integer(value) => {
            Ok(encode(INTEGER_TAG, INTEGER_TYPE_ID, &value.to_be_bytes()))
        }
        RuntimeValue::BigInt(value) => Ok(encode(BIGINT_TAG, BIGINT_TYPE_ID, &value.to_be_bytes())),
        RuntimeValue::Float(value) => {
            let value = if value.value() == 0.0 {
                0.0
            } else {
                value.value()
            };
            Ok(encode(
                FLOAT_TAG,
                FLOAT_TYPE_ID,
                &value.to_bits().to_be_bytes(),
            ))
        }
        RuntimeValue::Text(value) => {
            encode_variable(TEXT_TAG, CHARACTER_LARGE_OBJECT_TYPE_ID, value.as_bytes())
        }
        RuntimeValue::Bytes(value) => {
            encode_variable(BYTES_TAG, BINARY_LARGE_OBJECT_TYPE_ID, value)
        }
        RuntimeValue::Null(value) => {
            let resolved_type = value.resolved_type();
            if let Some(target) = resolved_type.reference_target() {
                require_reference_target(target)?;
                Ok(encode(NULL_REFERENCE_TAG, target, &[]))
            } else if let Some(scalar) = resolved_type.legacy_scalar() {
                let type_id =
                    supported_scalar_type_id(scalar).ok_or(ValueCodecError::UnsupportedValue)?;
                Ok(encode(NULL_SCALAR_TAG, type_id, &[]))
            } else {
                Err(ValueCodecError::UnsupportedValue)
            }
        }
        RuntimeValue::Reference { target, object } => {
            require_reference_target(*target)?;
            Ok(encode(REFERENCE_TAG, *target, &object.to_bytes()))
        }
        _ => Err(ValueCodecError::UnsupportedValue),
    }
}

/// Encodes one runtime value against the active catalogue as canonical
/// version-2 bytes.
///
/// Version 2 retains every version-1 tag and payload unchanged under the
/// `ORV2` marker. It adds catalogue enum values and typed enum nulls.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates the version-1 rules or
/// when an enum type or label is absent from the active catalogue.
pub fn encode_catalogue_value(
    catalogue: &CatalogueSnapshot,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Enum(value) => {
            validate_enum_value(catalogue, value.enum_type(), value.label())?;
            encode_variable(ENUM_TAG, value.enum_type(), value.label().as_bytes())
                .map(with_catalogue_marker)
        }
        RuntimeValue::Null(value) if value.resolved_type().value_type().is_some() => {
            let enum_type = value
                .resolved_type()
                .value_type()
                .expect("value type checked");
            require_active_enum_type(catalogue, enum_type)?;
            Ok(encode_with_marker(
                CATALOGUE_MARKER,
                NULL_ENUM_TAG,
                enum_type,
                &[],
            ))
        }
        _ => encode_value(value).map(with_catalogue_marker),
    }
}

/// Decodes one complete canonical version-1 runtime value.
///
/// # Errors
///
/// Returns a [`ValueCodecError`] for a truncated header or payload, invalid
/// marker, unknown tag, wrong stable type identity, trailing bytes, wrong
/// fixed payload length, invalid Boolean, non-canonical float, oversized
/// declared payload, invalid UTF-8, or stable scalar identity used as a
/// reference target. It never returns a partial value.
pub fn decode_value(encoded: &[u8]) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, MARKER)?;
    decode_non_enum_value(tag, type_id, payload)
}

/// Decodes one complete canonical version-2 value against the active
/// catalogue.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-1 byte shape and when
/// an enum type or exact label is absent from the active catalogue.
pub fn decode_catalogue_value(
    catalogue: &CatalogueSnapshot,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, CATALOGUE_MARKER)?;
    match tag {
        NULL_ENUM_TAG => {
            require_empty_payload(tag, payload)?;
            require_active_enum_type(catalogue, type_id)?;
            RuntimeValue::null(ResolvedType::value(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        ENUM_TAG => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            let value = validate_enum_value(catalogue, type_id, label)?;
            Ok(RuntimeValue::Enum(value))
        }
        _ => decode_non_enum_value(tag, type_id, payload),
    }
}

fn decode_non_enum_value(
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match tag {
        NULL_SCALAR_TAG => {
            require_empty_payload(tag, payload)?;
            let scalar =
                supported_scalar_from_type_id(type_id).ok_or(ValueCodecError::WrongType {
                    tag,
                    actual: type_id,
                })?;
            RuntimeValue::null(ResolvedType::scalar(scalar))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        NULL_REFERENCE_TAG => {
            require_empty_payload(tag, payload)?;
            require_reference_target(type_id)?;
            RuntimeValue::null(ResolvedType::reference(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        BOOLEAN_TAG => {
            require_type(tag, type_id, BOOLEAN_TYPE_ID)?;
            let [value] = payload else {
                return Err(ValueCodecError::WrongPayloadLength {
                    tag,
                    expected: 1,
                    actual: payload.len(),
                });
            };
            match value {
                0 => Ok(RuntimeValue::Boolean(false)),
                1 => Ok(RuntimeValue::Boolean(true)),
                value => Err(ValueCodecError::InvalidBoolean { value: *value }),
            }
        }
        INTEGER_TAG => {
            require_type(tag, type_id, INTEGER_TYPE_ID)?;
            let payload = require_fixed_payload::<4>(tag, payload)?;
            Ok(RuntimeValue::Integer(i32::from_be_bytes(payload)))
        }
        BIGINT_TAG => {
            require_type(tag, type_id, BIGINT_TYPE_ID)?;
            let payload = require_fixed_payload::<8>(tag, payload)?;
            Ok(RuntimeValue::BigInt(i64::from_be_bytes(payload)))
        }
        FLOAT_TAG => {
            require_type(tag, type_id, FLOAT_TYPE_ID)?;
            let payload = require_fixed_payload::<8>(tag, payload)?;
            let bits = u64::from_be_bytes(payload);
            let value = f64::from_bits(bits);
            if bits == (-0.0_f64).to_bits() || !value.is_finite() {
                return Err(ValueCodecError::NonCanonicalFloat);
            }
            RuntimeFloat::new(value)
                .map(RuntimeValue::Float)
                .map_err(|_| ValueCodecError::NonCanonicalFloat)
        }
        TEXT_TAG => {
            require_type(tag, type_id, CHARACTER_LARGE_OBJECT_TYPE_ID)?;
            require_payload_limit(payload.len())?;
            String::from_utf8(payload.to_vec())
                .map(RuntimeValue::Text)
                .map_err(|_| ValueCodecError::InvalidUtf8)
        }
        BYTES_TAG => {
            require_type(tag, type_id, BINARY_LARGE_OBJECT_TYPE_ID)?;
            require_payload_limit(payload.len())?;
            Ok(RuntimeValue::Bytes(payload.to_vec()))
        }
        REFERENCE_TAG => {
            require_reference_target(type_id)?;
            let object = require_fixed_payload::<16>(tag, payload)?;
            Ok(RuntimeValue::Reference {
                target: type_id,
                object: ObjectId::from_bytes(object),
            })
        }
        tag => Err(ValueCodecError::UnknownTag { tag }),
    }
}

fn encode(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    encode_with_marker(MARKER, tag, type_id, payload)
}

fn encode_with_marker(marker: &[u8; 4], tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(HEADER_LENGTH + payload.len());
    encoded.extend_from_slice(marker);
    encoded.push(tag);
    encoded.extend_from_slice(&type_id.to_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn with_catalogue_marker(mut encoded: Vec<u8>) -> Vec<u8> {
    encoded[..CATALOGUE_MARKER.len()].copy_from_slice(CATALOGUE_MARKER);
    encoded
}

fn encode_variable(tag: u8, type_id: TypeId, payload: &[u8]) -> Result<Vec<u8>, ValueCodecError> {
    require_payload_limit(payload.len())?;
    Ok(encode(tag, type_id, payload))
}

fn require_payload_limit(actual: usize) -> Result<(), ValueCodecError> {
    if actual <= PAYLOAD_LIMIT {
        Ok(())
    } else {
        Err(ValueCodecError::PayloadTooLarge {
            actual,
            maximum: PAYLOAD_LIMIT,
        })
    }
}

fn decode_envelope<'a>(
    encoded: &'a [u8],
    marker: &[u8; 4],
) -> Result<(u8, TypeId, &'a [u8]), ValueCodecError> {
    if encoded.len() < HEADER_LENGTH {
        return Err(ValueCodecError::TruncatedHeader {
            actual: encoded.len(),
        });
    }
    if &encoded[..marker.len()] != marker {
        return Err(ValueCodecError::InvalidMarker);
    }
    let tag = encoded[4];
    let type_id = TypeId::from_bytes(encoded[5..21].try_into().expect("header length checked"));
    let declared =
        u32::from_be_bytes(encoded[21..25].try_into().expect("header length checked")) as usize;
    require_payload_limit(declared)?;
    let actual = encoded.len() - HEADER_LENGTH;
    if actual < declared {
        return Err(ValueCodecError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(ValueCodecError::TrailingBytes { declared, actual });
    }
    Ok((tag, type_id, &encoded[HEADER_LENGTH..]))
}

fn require_active_enum_type(
    catalogue: &CatalogueSnapshot,
    enum_type: TypeId,
) -> Result<(), ValueCodecError> {
    catalogue
        .enum_type_by_id(enum_type)
        .map(|_| ())
        .ok_or(ValueCodecError::InactiveEnumType { enum_type })
}

fn validate_enum_value(
    catalogue: &CatalogueSnapshot,
    enum_type: TypeId,
    label: &str,
) -> Result<EnumValue, ValueCodecError> {
    EnumValue::new(catalogue, enum_type, label).map_err(|error| match error {
        EnumValueError::UnknownType { enum_type } => {
            ValueCodecError::InactiveEnumType { enum_type }
        }
        EnumValueError::UndeclaredLabel { enum_type, label } => {
            ValueCodecError::UndeclaredEnumLabel { enum_type, label }
        }
        _ => ValueCodecError::UnsupportedValue,
    })
}

fn require_type(tag: u8, actual: TypeId, expected: TypeId) -> Result<(), ValueCodecError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ValueCodecError::WrongType { tag, actual })
    }
}

fn require_empty_payload(tag: u8, payload: &[u8]) -> Result<(), ValueCodecError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(ValueCodecError::WrongPayloadLength {
            tag,
            expected: 0,
            actual: payload.len(),
        })
    }
}

fn require_reference_target(target: TypeId) -> Result<(), ValueCodecError> {
    if STANDARD_TYPE_IDS.contains(&target) {
        Err(ValueCodecError::StandardTypeAsReference { target })
    } else {
        Ok(())
    }
}

fn supported_scalar_type_id(scalar: StandardScalar) -> Option<TypeId> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(type_id, candidate)| (*candidate == scalar).then_some(*type_id))
}

fn supported_scalar_from_type_id(type_id: TypeId) -> Option<StandardScalar> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(candidate, scalar)| (*candidate == type_id).then_some(*scalar))
}

fn require_fixed_payload<const LENGTH: usize>(
    tag: u8,
    payload: &[u8],
) -> Result<[u8; LENGTH], ValueCodecError> {
    payload
        .try_into()
        .map_err(|_| ValueCodecError::WrongPayloadLength {
            tag,
            expected: LENGTH,
            actual: payload.len(),
        })
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, ObjectId, SchemaId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        types::{ResolvedType, StandardScalar},
        value::{EnumValue, RuntimeFloat},
    };
    use orna_standard::{
        BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID,
        CHARACTER_LARGE_OBJECT_TYPE_ID, FLOAT_TYPE_ID, INTEGER_TYPE_ID, STANDARD_TYPE_IDS,
    };
    use proptest::prelude::*;

    use super::*;

    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x43; 16]);

    fn enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                labels.iter().copied(),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn public_frame_payload_limit_matches_the_wire_contract() {
        assert_eq!(MAX_FRAME_PAYLOAD_LENGTH, 16 * 1024 * 1024 + 64);
    }

    #[test]
    fn boolean_has_exact_golden_bytes_and_round_trips() {
        let mut expected = b"ORV1".to_vec();
        expected.push(0x02);
        expected.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.push(1);

        assert_eq!(
            encode_value(&RuntimeValue::Boolean(true)),
            Ok(expected.clone())
        );
        assert_eq!(decode_value(&expected), Ok(RuntimeValue::Boolean(true)));
    }

    #[test]
    fn catalogue_codec_has_exact_enum_bytes_and_preserves_version_one_closure() {
        let catalogue = enum_catalogue(&["lead", "owner's"]);
        let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "owner's").unwrap());
        let mut expected = b"ORV2".to_vec();
        expected.push(0x0a);
        expected.extend_from_slice(&ENUM_TYPE.to_bytes());
        expected.extend_from_slice(&7_u32.to_be_bytes());
        expected.extend_from_slice(b"owner's");

        assert_eq!(
            encode_catalogue_value(&catalogue, &value),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_catalogue_value(&catalogue, &expected),
            Ok(value.clone())
        );
        assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
        assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
    }

    #[test]
    fn catalogue_codec_round_trips_enum_null_and_legacy_values_as_version_two() {
        let catalogue = enum_catalogue(&["lead"]);
        let null = RuntimeValue::null(ResolvedType::value(ENUM_TYPE)).unwrap();
        let expected_null = encoded_catalogue_value(0x09, ENUM_TYPE, &[]);
        assert_eq!(
            encode_catalogue_value(&catalogue, &null),
            Ok(expected_null.clone())
        );
        assert_eq!(decode_catalogue_value(&catalogue, &expected_null), Ok(null));

        let boolean = RuntimeValue::Boolean(true);
        let expected_boolean = encoded_catalogue_value(0x02, BOOLEAN_TYPE_ID, &[1]);
        assert_eq!(
            encode_catalogue_value(&catalogue, &boolean),
            Ok(expected_boolean.clone())
        );
        assert_eq!(
            decode_catalogue_value(&catalogue, &expected_boolean),
            Ok(boolean)
        );
    }

    #[test]
    fn catalogue_codec_rejects_stale_unknown_and_mismatched_enum_labels() {
        let original = enum_catalogue(&["lead", "qualified"]);
        let active = enum_catalogue(&["lead", "customer"]);
        let stale = RuntimeValue::Enum(EnumValue::new(&original, ENUM_TYPE, "qualified").unwrap());
        assert_eq!(
            encode_catalogue_value(&active, &stale),
            Err(ValueCodecError::UndeclaredEnumLabel {
                enum_type: ENUM_TYPE,
                label: String::from("qualified"),
            })
        );

        let unknown = TypeId::from_bytes([0x46; 16]);
        assert_eq!(
            decode_catalogue_value(&active, &encoded_catalogue_value(0x0a, unknown, b"lead")),
            Err(ValueCodecError::InactiveEnumType { enum_type: unknown })
        );
        assert_eq!(
            decode_catalogue_value(
                &active,
                &encoded_catalogue_value(0x0a, ENUM_TYPE, b"qualified")
            ),
            Err(ValueCodecError::UndeclaredEnumLabel {
                enum_type: ENUM_TYPE,
                label: String::from("qualified"),
            })
        );
        assert_eq!(
            decode_catalogue_value(&active, &encoded_catalogue_value(0x0a, ENUM_TYPE, &[0xff])),
            Err(ValueCodecError::InvalidUtf8)
        );
        assert_eq!(
            decode_catalogue_value(&active, &encoded_catalogue_value(0x09, ENUM_TYPE, b"lead")),
            Err(ValueCodecError::WrongPayloadLength {
                tag: 0x09,
                expected: 0,
                actual: 4,
            })
        );
    }

    #[test]
    fn signed_integers_have_exact_big_endian_bytes_and_round_trip() {
        let mut integer = b"ORV1".to_vec();
        integer.push(0x03);
        integer.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
        integer.extend_from_slice(&4_u32.to_be_bytes());
        integer.extend_from_slice(&(-2_i32).to_be_bytes());
        assert_eq!(
            encode_value(&RuntimeValue::Integer(-2)),
            Ok(integer.clone())
        );
        assert_eq!(decode_value(&integer), Ok(RuntimeValue::Integer(-2)));

        let mut bigint = b"ORV1".to_vec();
        bigint.push(0x04);
        bigint.extend_from_slice(&BIGINT_TYPE_ID.to_bytes());
        bigint.extend_from_slice(&8_u32.to_be_bytes());
        bigint.extend_from_slice(&(-3_i64).to_be_bytes());
        assert_eq!(encode_value(&RuntimeValue::BigInt(-3)), Ok(bigint.clone()));
        assert_eq!(decode_value(&bigint), Ok(RuntimeValue::BigInt(-3)));
    }

    #[test]
    fn float_has_exact_bytes_and_normalises_negative_zero() {
        let mut expected = b"ORV1".to_vec();
        expected.push(0x05);
        expected.extend_from_slice(&FLOAT_TYPE_ID.to_bytes());
        expected.extend_from_slice(&8_u32.to_be_bytes());
        expected.extend_from_slice(&1.5_f64.to_bits().to_be_bytes());
        let value = RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap());
        assert_eq!(encode_value(&value), Ok(expected.clone()));
        assert_eq!(decode_value(&expected), Ok(value));

        let positive = RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap());
        let negative = RuntimeValue::Float(RuntimeFloat::new(-0.0).unwrap());
        assert_eq!(encode_value(&negative), encode_value(&positive));
    }

    #[test]
    fn text_and_bytes_preserve_payloads_and_enforce_the_shared_limit() {
        let mut text = b"ORV1".to_vec();
        text.push(0x06);
        text.extend_from_slice(&CHARACTER_LARGE_OBJECT_TYPE_ID.to_bytes());
        text.extend_from_slice(&2_u32.to_be_bytes());
        text.extend_from_slice("é".as_bytes());
        assert_eq!(
            encode_value(&RuntimeValue::Text("é".into())),
            Ok(text.clone())
        );
        assert_eq!(decode_value(&text), Ok(RuntimeValue::Text("é".into())));

        let mut bytes = b"ORV1".to_vec();
        bytes.push(0x07);
        bytes.extend_from_slice(&BINARY_LARGE_OBJECT_TYPE_ID.to_bytes());
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0xff, 1]);
        assert_eq!(
            encode_value(&RuntimeValue::Bytes(vec![0, 0xff, 1])),
            Ok(bytes.clone())
        );
        assert_eq!(
            decode_value(&bytes),
            Ok(RuntimeValue::Bytes(vec![0, 0xff, 1]))
        );

        let oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
        assert_eq!(
            encode_value(&RuntimeValue::Bytes(oversized.clone())),
            Err(ValueCodecError::PayloadTooLarge {
                actual: oversized.len(),
                maximum: 16 * 1024 * 1024,
            })
        );
        assert_eq!(
            encode_value(&RuntimeValue::Text(
                String::from_utf8(oversized).expect("ASCII fixture is UTF-8")
            )),
            Err(ValueCodecError::PayloadTooLarge {
                actual: 16 * 1024 * 1024 + 1,
                maximum: 16 * 1024 * 1024,
            })
        );
    }

    #[test]
    fn typed_nulls_and_references_retain_exact_type_and_object_identity() {
        let boolean_null = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean))
            .expect("BOOLEAN null is supported");
        let mut expected_null = b"ORV1".to_vec();
        expected_null.push(0x00);
        expected_null.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        expected_null.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(encode_value(&boolean_null), Ok(expected_null.clone()));
        assert_eq!(decode_value(&expected_null), Ok(boolean_null));

        let target = TypeId::from_bytes([0x41; 16]);
        let object = ObjectId::from_bytes([0x42; 16]);
        let reference_null = RuntimeValue::null(ResolvedType::reference(target))
            .expect("reference null is supported");
        let mut expected_reference_null = b"ORV1".to_vec();
        expected_reference_null.push(0x01);
        expected_reference_null.extend_from_slice(&target.to_bytes());
        expected_reference_null.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            encode_value(&reference_null),
            Ok(expected_reference_null.clone())
        );
        assert_eq!(decode_value(&expected_reference_null), Ok(reference_null));

        let reference = RuntimeValue::Reference { target, object };
        let mut expected_reference = b"ORV1".to_vec();
        expected_reference.push(0x08);
        expected_reference.extend_from_slice(&target.to_bytes());
        expected_reference.extend_from_slice(&16_u32.to_be_bytes());
        expected_reference.extend_from_slice(&object.to_bytes());
        assert_eq!(encode_value(&reference), Ok(expected_reference.clone()));
        assert_eq!(decode_value(&expected_reference), Ok(reference));
    }

    #[test]
    fn null_scalar_accepts_exactly_the_six_supported_standard_identities() {
        let supported = [
            (StandardScalar::Boolean, BOOLEAN_TYPE_ID),
            (StandardScalar::Integer, INTEGER_TYPE_ID),
            (StandardScalar::BigInt, BIGINT_TYPE_ID),
            (StandardScalar::Float, FLOAT_TYPE_ID),
            (
                StandardScalar::CharacterLargeObject,
                CHARACTER_LARGE_OBJECT_TYPE_ID,
            ),
            (
                StandardScalar::BinaryLargeObject,
                BINARY_LARGE_OBJECT_TYPE_ID,
            ),
        ];
        for (scalar, type_id) in supported {
            let value = RuntimeValue::null(ResolvedType::scalar(scalar)).unwrap();
            let encoded = encode_value(&value).unwrap();
            assert_eq!(encoded, encoded_value(0x00, type_id, &[]));
            assert_eq!(decode_value(&encoded), Ok(value));
        }

        let supported_ids = supported.map(|(_, type_id)| type_id);
        for unsupported in STANDARD_TYPE_IDS
            .into_iter()
            .filter(|candidate| !supported_ids.contains(candidate))
        {
            let mut encoded = b"ORV1".to_vec();
            encoded.push(0x00);
            encoded.extend_from_slice(&unsupported.to_bytes());
            encoded.extend_from_slice(&0_u32.to_be_bytes());
            assert_eq!(
                decode_value(&encoded),
                Err(ValueCodecError::WrongType {
                    tag: 0x00,
                    actual: unsupported,
                })
            );
        }
    }

    #[test]
    fn references_reject_every_stable_standard_scalar_identity() {
        for target in STANDARD_TYPE_IDS {
            let reference = RuntimeValue::Reference {
                target,
                object: ObjectId::from_bytes([0x42; 16]),
            };
            assert_eq!(
                encode_value(&reference),
                Err(ValueCodecError::StandardTypeAsReference { target })
            );
            let null = RuntimeValue::null(ResolvedType::reference(target)).unwrap();
            assert_eq!(
                encode_value(&null),
                Err(ValueCodecError::StandardTypeAsReference { target })
            );

            for tag in [0x01, 0x08] {
                let payload = if tag == 0x08 {
                    &[0x42; 16][..]
                } else {
                    &[][..]
                };
                let mut encoded = b"ORV1".to_vec();
                encoded.push(tag);
                encoded.extend_from_slice(&target.to_bytes());
                encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                encoded.extend_from_slice(payload);
                assert_eq!(
                    decode_value(&encoded),
                    Err(ValueCodecError::StandardTypeAsReference { target })
                );
            }
        }
    }

    #[test]
    fn scalar_tags_accept_only_their_matching_supported_identity() {
        for (tag, expected, payload) in [
            (0x02, BOOLEAN_TYPE_ID, vec![1]),
            (0x03, INTEGER_TYPE_ID, vec![0; 4]),
            (0x04, BIGINT_TYPE_ID, vec![0; 8]),
            (0x05, FLOAT_TYPE_ID, vec![0; 8]),
            (0x06, CHARACTER_LARGE_OBJECT_TYPE_ID, vec![]),
            (0x07, BINARY_LARGE_OBJECT_TYPE_ID, vec![]),
        ] {
            for actual in STANDARD_TYPE_IDS {
                let encoded = encoded_value(tag, actual, &payload);
                if actual == expected {
                    assert!(decode_value(&encoded).is_ok());
                } else {
                    assert_eq!(
                        decode_value(&encoded),
                        Err(ValueCodecError::WrongType { tag, actual })
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_envelopes_and_payloads_fail_closed() {
        for length in 0..25 {
            assert_eq!(
                decode_value(&vec![0; length]),
                Err(ValueCodecError::TruncatedHeader { actual: length })
            );
        }

        let mut bad_marker = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
        bad_marker[0] = b'X';
        assert_eq!(
            decode_value(&bad_marker),
            Err(ValueCodecError::InvalidMarker)
        );

        assert_eq!(
            decode_value(&encoded_value(0xff, BOOLEAN_TYPE_ID, &[])),
            Err(ValueCodecError::UnknownTag { tag: 0xff })
        );

        let mut truncated = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
        truncated[21..25].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(
            decode_value(&truncated),
            Err(ValueCodecError::TruncatedPayload {
                declared: 2,
                actual: 1,
            })
        );

        let mut trailing = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
        trailing[21..25].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            decode_value(&trailing),
            Err(ValueCodecError::TrailingBytes {
                declared: 0,
                actual: 1,
            })
        );

        let mut oversized = encoded_value(0x07, BINARY_LARGE_OBJECT_TYPE_ID, &[]);
        oversized[21..25].copy_from_slice(&(16_u32 * 1024 * 1024 + 1).to_be_bytes());
        assert_eq!(
            decode_value(&oversized),
            Err(ValueCodecError::PayloadTooLarge {
                actual: 16 * 1024 * 1024 + 1,
                maximum: 16 * 1024 * 1024,
            })
        );

        assert_eq!(
            decode_value(&encoded_value(
                0x06,
                CHARACTER_LARGE_OBJECT_TYPE_ID,
                &[0xff]
            )),
            Err(ValueCodecError::InvalidUtf8)
        );
        assert_eq!(
            decode_value(&encoded_value(0x03, INTEGER_TYPE_ID, &[0; 3])),
            Err(ValueCodecError::WrongPayloadLength {
                tag: 0x03,
                expected: 4,
                actual: 3,
            })
        );
        assert_eq!(
            decode_value(&encoded_value(0x00, BOOLEAN_TYPE_ID, &[0])),
            Err(ValueCodecError::WrongPayloadLength {
                tag: 0x00,
                expected: 0,
                actual: 1,
            })
        );
    }

    #[test]
    fn boolean_and_float_payloads_require_canonical_values() {
        for value in 2..=u8::MAX {
            assert_eq!(
                decode_value(&encoded_value(0x02, BOOLEAN_TYPE_ID, &[value])),
                Err(ValueCodecError::InvalidBoolean { value })
            );
        }

        for bits in [
            (-0.0_f64).to_bits(),
            f64::NAN.to_bits(),
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
        ] {
            assert_eq!(
                decode_value(&encoded_value(0x05, FLOAT_TYPE_ID, &bits.to_be_bytes())),
                Err(ValueCodecError::NonCanonicalFloat)
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn arbitrary_bytes_never_panic(
            bytes in prop::collection::vec(any::<u8>(), 0..=65_536),
        ) {
            let _ = decode_value(&bytes);
        }

        #[test]
        fn arbitrary_version_one_envelopes_never_panic(
            tag in any::<u8>(),
            type_bytes in any::<[u8; 16]>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let mut encoded = b"ORV1".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&type_bytes);
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_value(&encoded);
        }

        #[test]
        fn arbitrary_version_two_envelopes_never_panic(
            tag in any::<u8>(),
            type_bytes in any::<[u8; 16]>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let catalogue = enum_catalogue(&["lead", "qualified"]);
            let mut encoded = b"ORV2".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&type_bytes);
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_catalogue_value(&catalogue, &encoded);
        }
    }

    fn encoded_value(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
        let mut encoded = b"ORV1".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_id.to_bytes());
        encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        encoded.extend_from_slice(payload);
        encoded
    }

    fn encoded_catalogue_value(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
        let mut encoded = encoded_value(tag, type_id, payload);
        encoded[..4].copy_from_slice(b"ORV2");
        encoded
    }
}
