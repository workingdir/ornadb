//! Canonical runtime values and the bounded authenticated raw-call protocol.

mod frame;

pub use frame::{
    CallArgument, CallFailure, Channel, ClientAction, ClientFrame, ConnectionError, Event,
    EventRecord, FrameCodecError, MAX_CHANNEL_WINDOW, MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection,
    RawCall, RawCallClient, RawCallClientError, RawCallClientResponse, ServerAction, ServerFrame,
    decode_active_client_frame, decode_active_server_frame, decode_catalogue_client_frame,
    decode_catalogue_server_frame, decode_client_frame, decode_registered_client_frame,
    decode_registered_server_frame, decode_server_frame, encode_active_client_frame,
    encode_active_server_frame, encode_catalogue_client_frame, encode_catalogue_server_frame,
    encode_client_frame, encode_registered_client_frame, encode_registered_server_frame,
    encode_server_frame,
};

use std::{error::Error, fmt};

use orna_core::{
    FieldId, ObjectId, TypeId,
    catalogue::CatalogueSnapshot,
    revision::ActiveDatabaseRevision,
    types::{ResolvedType, StandardScalar},
    value::{
        EnumValue, EnumValueError, OpaqueCodecRegistry, OpaqueValue, OpaqueValueError, RecordValue,
        RuntimeFloat, RuntimeValue,
    },
};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    FLOAT_TYPE_ID, INTEGER_TYPE_ID, STANDARD_TYPE_IDS,
};

const MARKER: &[u8; 4] = b"ORV1";
const CATALOGUE_MARKER: &[u8; 4] = b"ORV2";
const ACTIVE_MARKER: &[u8; 4] = b"ORV3";
const REGISTERED_MARKER: &[u8; 4] = b"ORV4";
const HEADER_LENGTH: usize = 25;
const RECORD_FIELD_HEADER_LENGTH: usize = 20;
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
const RECORD_TAG: u8 = 0x0b;
const OPAQUE_TAG: u8 = 0x0c;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const SUPPORTED_SCALAR_TYPES: [(TypeId, StandardScalar, u8); 6] = [
    (BOOLEAN_TYPE_ID, StandardScalar::Boolean, BOOLEAN_TAG),
    (INTEGER_TYPE_ID, StandardScalar::Integer, INTEGER_TAG),
    (BIGINT_TYPE_ID, StandardScalar::BigInt, BIGINT_TAG),
    (FLOAT_TYPE_ID, StandardScalar::Float, FLOAT_TAG),
    (
        CHARACTER_LARGE_OBJECT_TYPE_ID,
        StandardScalar::CharacterLargeObject,
        TEXT_TAG,
    ),
    (
        BINARY_LARGE_OBJECT_TYPE_ID,
        StandardScalar::BinaryLargeObject,
        BYTES_TAG,
    ),
];

/// An error from canonical runtime value encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueCodecError {
    /// The runtime value category is not defined by the selected codec version.
    UnsupportedValue,
    /// The encoded value does not contain the complete fixed header.
    TruncatedHeader {
        /// The total number of available bytes.
        actual: usize,
    },
    /// The encoded value does not start with the selected codec marker.
    InvalidMarker,
    /// The value tag is not defined by the selected codec version.
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
    /// A supplied or declared payload exceeds the shared codec limit.
    PayloadTooLarge {
        /// The supplied or declared payload length.
        actual: usize,
        /// The shared canonical value payload limit.
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
    /// The active revision does not contain the supplied record type.
    InactiveRecordType {
        /// The inactive record type identity.
        record_type: TypeId,
    },
    /// A checked record value is not valid against the supplied active revision.
    RecordValueNotActive {
        /// The incompatible record type identity.
        record_type: TypeId,
    },
    /// The encoded field count differs from the active record definition.
    WrongRecordFieldCount {
        /// The field count required by the active definition.
        expected: usize,
        /// The field count declared by the encoded payload.
        actual: usize,
    },
    /// An encoded field identity differs from the active declaration ordinal.
    WrongRecordFieldIdentity {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The stable field identity required at this ordinal.
        expected: FieldId,
        /// The stable field identity found in the encoded payload.
        actual: FieldId,
    },
    /// The record payload ends before one complete field-entry header.
    TruncatedRecordFieldHeader {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The bytes available for the field-entry header.
        actual: usize,
    },
    /// An encoded complete field-value length cannot fit in the record payload.
    InvalidRecordFieldLength {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The complete field-value length declared by the entry.
        declared: usize,
        /// The bytes available after the entry header.
        remaining: usize,
    },
    /// A record field contains the deferred nested-record shape.
    NestedRecordValue {
        /// The zero-based declaration ordinal.
        ordinal: usize,
    },
    /// A record field value does not use its declared wire type.
    WrongRecordFieldType {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The resolved field type required by the active definition.
        expected: ResolvedType,
        /// The encoded value tag.
        tag: u8,
        /// The encoded stable type identity.
        actual: TypeId,
    },
    /// A registered opaque value is invalid for the supplied revision or registry.
    OpaqueValue {
        /// The opaque value validation failure.
        source: OpaqueValueError,
    },
}

impl fmt::Display for ValueCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedValue => {
                formatter.write_str("runtime value is not supported by the selected codec version")
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
            Self::InactiveRecordType { .. } => {
                formatter.write_str("record type is not active for the canonical value")
            }
            Self::RecordValueNotActive { .. } => {
                formatter.write_str("record value is not valid for the active revision")
            }
            Self::WrongRecordFieldCount { .. } => {
                formatter.write_str("record field count does not match the active definition")
            }
            Self::WrongRecordFieldIdentity { .. } => {
                formatter.write_str("record field identity does not match its declaration ordinal")
            }
            Self::TruncatedRecordFieldHeader { .. } => {
                formatter.write_str("record field-entry header is truncated")
            }
            Self::InvalidRecordFieldLength { .. } => {
                formatter.write_str("record field length is invalid")
            }
            Self::NestedRecordValue { .. } => {
                formatter.write_str("nested record values are not supported")
            }
            Self::WrongRecordFieldType { .. } => {
                formatter.write_str("record field value does not match its declared type")
            }
            Self::OpaqueValue { .. } => {
                formatter.write_str("opaque value is not valid for the active registry")
            }
        }
    }
}

impl Error for ValueCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OpaqueValue { source } => Some(source),
            _ => None,
        }
    }
}

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
        RuntimeValue::Null(value) if value.resolved_type().named_type().is_some() => {
            let enum_type = value
                .resolved_type()
                .named_type()
                .expect("named type checked");
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
    decode_catalogue_value_parts(catalogue, tag, type_id, payload)
}

/// Encodes one runtime value against an active revision as canonical
/// version-3 bytes.
///
/// Version 3 retains every version-2 scalar and enum shape under the `ORV3`
/// marker and adds named immutable record values.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates an earlier codec rule
/// or is not valid against the supplied active revision.
pub fn encode_active_value(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Record(value) => encode_record_value(active, value),
        _ => encode_catalogue_value(active.catalogue(), value).map(with_active_marker),
    }
}

/// Decodes one complete canonical version-3 value against an active revision.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-2 byte shape and for
/// a record that does not match the active nominal definition. It never
/// returns a partial value.
pub fn decode_active_value(
    active: &ActiveDatabaseRevision,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, ACTIVE_MARKER)?;
    if tag == RECORD_TAG {
        decode_record_value(active, type_id, payload)
    } else {
        decode_active_non_record_value(active, tag, type_id, payload)
    }
}

/// Encodes one runtime value against an active revision and immutable opaque
/// codec registry as canonical version-4 bytes.
///
/// Version 4 retains every version-3 value shape under the `ORV4` marker and
/// adds non-null registered opaque values.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates an earlier codec rule,
/// is not valid against the supplied active revision, or is not accepted by
/// the supplied opaque codec registry.
pub fn encode_registered_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Opaque(value) => {
            let checked = OpaqueValue::new(
                active,
                registry,
                value.opaque_type(),
                value.canonical_payload(),
            )
            .map_err(|source| ValueCodecError::OpaqueValue { source })?;
            Ok(encode_with_marker(
                REGISTERED_MARKER,
                OPAQUE_TAG,
                checked.opaque_type(),
                checked.canonical_payload(),
            ))
        }
        RuntimeValue::Record(value) => {
            encode_record_value_with_marker(active, value, REGISTERED_MARKER)
        }
        _ => encode_catalogue_value(active.catalogue(), value).map(with_registered_marker),
    }
}

/// Decodes one complete canonical version-4 value against an active revision
/// and immutable opaque codec registry.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-3 byte shape and for
/// an opaque value rejected by the supplied active revision or registry. It
/// never returns a partial value.
pub fn decode_registered_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, REGISTERED_MARKER)?;
    match tag {
        RECORD_TAG => decode_record_value_with_marker(active, type_id, payload, REGISTERED_MARKER),
        OPAQUE_TAG => OpaqueValue::new(active, registry, type_id, payload)
            .map(RuntimeValue::Opaque)
            .map_err(|source| ValueCodecError::OpaqueValue { source }),
        _ => decode_active_non_record_value(active, tag, type_id, payload),
    }
}

fn encode_record_value(
    active: &ActiveDatabaseRevision,
    value: &RecordValue,
) -> Result<Vec<u8>, ValueCodecError> {
    encode_record_value_with_marker(active, value, ACTIVE_MARKER)
}

fn encode_record_value_with_marker(
    active: &ActiveDatabaseRevision,
    value: &RecordValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    let definition = active
        .catalogue()
        .record_value_type_by_id(value.record_type())
        .ok_or(ValueCodecError::InactiveRecordType {
            record_type: value.record_type(),
        })?;
    if definition.fields().len() != value.fields().len() {
        return Err(ValueCodecError::RecordValueNotActive {
            record_type: value.record_type(),
        });
    }
    RecordValue::new(
        active,
        value.record_type(),
        definition
            .fields()
            .iter()
            .zip(value.fields())
            .map(|(field, value)| (field.name().to_owned(), value.clone())),
    )
    .map_err(|_| ValueCodecError::RecordValueNotActive {
        record_type: value.record_type(),
    })?;

    let field_count =
        u32::try_from(definition.fields().len()).map_err(|_| ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&field_count.to_be_bytes());
    for (field, value) in definition.fields().iter().zip(value.fields()) {
        let encoded = encode_record_field_value(
            active,
            definition.id(),
            field.resolved_type(),
            value,
            marker,
        )?;
        let encoded_length =
            u32::try_from(encoded.len()).map_err(|_| ValueCodecError::PayloadTooLarge {
                actual: encoded.len(),
                maximum: PAYLOAD_LIMIT,
            })?;
        let next_length = payload
            .len()
            .checked_add(RECORD_FIELD_HEADER_LENGTH)
            .and_then(|length| length.checked_add(encoded.len()))
            .ok_or(ValueCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
        require_payload_limit(next_length)?;
        payload.reserve(RECORD_FIELD_HEADER_LENGTH + encoded.len());
        payload.extend_from_slice(&field.id().to_bytes());
        payload.extend_from_slice(&encoded_length.to_be_bytes());
        payload.extend_from_slice(&encoded);
    }
    Ok(encode_with_marker(
        marker,
        RECORD_TAG,
        value.record_type(),
        &payload,
    ))
}

fn decode_record_value(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    decode_record_value_with_marker(active, record_type, payload, ACTIVE_MARKER)
}

fn decode_record_value_with_marker(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    payload: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    let definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or(ValueCodecError::InactiveRecordType { record_type })?;
    if payload.len() < 4 {
        return Err(ValueCodecError::TruncatedPayload {
            declared: 4,
            actual: payload.len(),
        });
    }
    let field_count = u32::from_be_bytes(payload[..4].try_into().expect("length checked")) as usize;
    if field_count != definition.fields().len() {
        return Err(ValueCodecError::WrongRecordFieldCount {
            expected: definition.fields().len(),
            actual: field_count,
        });
    }
    let minimum = 4_usize
        .checked_add(
            field_count
                .checked_mul(RECORD_FIELD_HEADER_LENGTH + HEADER_LENGTH)
                .ok_or(ValueCodecError::PayloadTooLarge {
                    actual: usize::MAX,
                    maximum: PAYLOAD_LIMIT,
                })?,
        )
        .ok_or(ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    if payload.len() < minimum {
        return Err(ValueCodecError::TruncatedPayload {
            declared: minimum,
            actual: payload.len(),
        });
    }

    let mut cursor = 4;
    let mut fields = Vec::with_capacity(field_count);
    for (ordinal, definition_field) in definition.fields().iter().enumerate() {
        let remaining = payload.len() - cursor;
        if remaining < RECORD_FIELD_HEADER_LENGTH {
            return Err(ValueCodecError::TruncatedRecordFieldHeader {
                ordinal,
                actual: remaining,
            });
        }
        let field_end = cursor + 16;
        let field = FieldId::from_bytes(
            payload[cursor..field_end]
                .try_into()
                .expect("minimum record entry length checked"),
        );
        if field != definition_field.id() {
            return Err(ValueCodecError::WrongRecordFieldIdentity {
                ordinal,
                expected: definition_field.id(),
                actual: field,
            });
        }
        cursor = field_end;
        let length_end = cursor + 4;
        let declared = u32::from_be_bytes(
            payload[cursor..length_end]
                .try_into()
                .expect("minimum record entry length checked"),
        ) as usize;
        cursor = length_end;
        let remaining = payload.len() - cursor;
        if declared < HEADER_LENGTH || declared > remaining {
            return Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal,
                declared,
                remaining,
            });
        }
        let encoded_end = cursor + declared;
        let encoded = &payload[cursor..encoded_end];
        let (tag, type_id, field_payload) = decode_envelope(encoded, marker)?;
        if tag == RECORD_TAG {
            return Err(ValueCodecError::NestedRecordValue { ordinal });
        }
        require_record_field_wire_type(
            active,
            definition_field.resolved_type(),
            ordinal,
            tag,
            type_id,
        )?;
        let value = decode_record_field_value(
            active,
            definition_field.resolved_type(),
            tag,
            field_payload,
        )?;
        fields.push((definition_field.name().to_owned(), value));
        cursor = encoded_end;
    }
    if cursor != payload.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: payload.len(),
        });
    }
    RecordValue::new(active, record_type, fields)
        .map(RuntimeValue::Record)
        .map_err(|_| ValueCodecError::RecordValueNotActive { record_type })
}

fn decode_active_non_record_value(
    active: &ActiveDatabaseRevision,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    decode_catalogue_value_parts(active.catalogue(), tag, type_id, payload)
}

fn decode_catalogue_value_parts(
    catalogue: &CatalogueSnapshot,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match tag {
        NULL_ENUM_TAG => {
            require_empty_payload(tag, payload)?;
            require_active_enum_type(catalogue, type_id)?;
            RuntimeValue::null(ResolvedType::named(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        ENUM_TAG => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_enum_value(catalogue, type_id, label).map(RuntimeValue::Enum)
        }
        _ => decode_non_enum_value(tag, type_id, payload),
    }
}

fn require_record_field_wire_type(
    active: &ActiveDatabaseRevision,
    expected: ResolvedType,
    ordinal: usize,
    tag: u8,
    actual: TypeId,
) -> Result<(), ValueCodecError> {
    let matches = match expected {
        ResolvedType::Value(expected) => active
            .record_value_field_runtime_type(ResolvedType::value(expected))
            .and_then(ResolvedType::legacy_scalar)
            .is_some_and(|scalar| {
                actual == expected && supported_scalar_tag_from_scalar(scalar) == Some(tag)
            }),
        ResolvedType::Named(expected) => actual == expected && tag == ENUM_TAG,
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => false,
    };
    if matches {
        Ok(())
    } else {
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal,
            expected,
            tag,
            actual,
        })
    }
}

fn encode_record_field_value(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    declared: ResolvedType,
    value: &RuntimeValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    match declared {
        ResolvedType::Value(type_id) => {
            let expected = active
                .record_value_field_runtime_type(declared)
                .ok_or(ValueCodecError::UnsupportedValue)?;
            if value.resolved_type() != expected {
                return Err(ValueCodecError::RecordValueNotActive { record_type });
            }
            let mut encoded = encode_value(value)?;
            encoded[..marker.len()].copy_from_slice(marker);
            encoded[5..21].copy_from_slice(&type_id.to_bytes());
            Ok(encoded)
        }
        ResolvedType::Named(enum_type) => {
            let RuntimeValue::Enum(value) = value else {
                return Err(ValueCodecError::RecordValueNotActive { record_type });
            };
            validate_active_enum_value(active, enum_type, value.label())?;
            encode_variable(ENUM_TAG, enum_type, value.label().as_bytes()).map(|mut encoded| {
                encoded[..marker.len()].copy_from_slice(marker);
                encoded
            })
        }
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
    }
}

fn decode_record_field_value(
    active: &ActiveDatabaseRevision,
    declared: ResolvedType,
    tag: u8,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match declared {
        ResolvedType::Value(_) => {
            let scalar = active
                .record_value_field_runtime_type(declared)
                .and_then(ResolvedType::legacy_scalar)
                .ok_or(ValueCodecError::UnsupportedValue)?;
            let canonical_type =
                supported_scalar_type_id(scalar).ok_or(ValueCodecError::UnsupportedValue)?;
            decode_non_enum_value(tag, canonical_type, payload)
        }
        ResolvedType::Named(enum_type) => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_active_enum_value(active, enum_type, label).map(RuntimeValue::Enum)
        }
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
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

fn with_active_marker(mut encoded: Vec<u8>) -> Vec<u8> {
    encoded[..ACTIVE_MARKER.len()].copy_from_slice(ACTIVE_MARKER);
    encoded
}

fn with_registered_marker(mut encoded: Vec<u8>) -> Vec<u8> {
    encoded[..REGISTERED_MARKER.len()].copy_from_slice(REGISTERED_MARKER);
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

fn validate_active_enum_value(
    active: &ActiveDatabaseRevision,
    enum_type: TypeId,
    label: &str,
) -> Result<EnumValue, ValueCodecError> {
    if active.catalogue().enum_type_by_id(enum_type).is_some() {
        return validate_enum_value(active.catalogue(), enum_type, label);
    }
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or(ValueCodecError::InactiveEnumType { enum_type })?
        .catalogue();
    validate_enum_value(standard, enum_type, label)
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
        .find_map(|(type_id, candidate, _)| (*candidate == scalar).then_some(*type_id))
}

fn supported_scalar_from_type_id(type_id: TypeId) -> Option<StandardScalar> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(candidate, scalar, _)| (*candidate == type_id).then_some(*scalar))
}

fn supported_scalar_tag_from_scalar(scalar: StandardScalar) -> Option<u8> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(_, candidate, tag)| (*candidate == scalar).then_some(*tag))
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
        CatalogueRevisionId, FieldId, FunctionId, InvocationId, ObjectId, ParameterId, SchemaId,
        SourceBundleId, SourceRevisionId, SourceUnitId,
        canonical_hash::{
            catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, SourceOrigin,
            StoredSourceRevision, StoredSourceUnit,
        },
        types::{ResolvedType, StandardScalar},
        value::{EnumValue, OpaqueValue, RecordValue, RuntimeFloat},
    };
    use orna_standard::{
        BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID,
        CHARACTER_LARGE_OBJECT_TYPE_ID, FLOAT_TYPE_ID, INTEGER_TYPE_ID, OPAQUE_TOKEN_TYPE_ID,
        STANDARD_TYPE_IDS, registered_opaque_codecs, retained_standard_library_snapshot,
        verify_standard_library_snapshot,
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

    fn active_record_revision() -> ActiveDatabaseRevision {
        active_record_revision_with_second_type(ResolvedType::named(ENUM_TYPE))
    }

    fn active_record_revision_with_second_type(
        second_field_type: ResolvedType,
    ) -> ActiveDatabaseRevision {
        active_record_revision_with_types(ResolvedType::value(BOOLEAN_TYPE_ID), second_field_type)
    }

    fn active_record_revision_with_types(
        first_field_type: ResolvedType,
        second_field_type: ResolvedType,
    ) -> ActiveDatabaseRevision {
        let record_type = TypeId::from_bytes([0x47; 16]);
        let record_field = FieldId::from_bytes([0x48; 16]);
        let second_record_field = FieldId::from_bytes([0x4e; 16]);
        let schema = SchemaId::from_bytes([0x49; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x4a; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                ["lead", "qualified"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["crm", "flag"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::new(record_field, "enabled", 0, first_field_type),
                    RecordValueFieldDefinition::new(
                        second_record_field,
                        "verified",
                        1,
                        second_field_type,
                    ),
                ],
            )],
            vec![],
        )
        .unwrap();
        let source_unit_id = SourceUnitId::from_bytes([0x4b; 16]);
        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "app/types.orna",
            "ab",
            source_unit_content_digest("ab").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source_revision = SourceRevisionId::from_bytes([0x4c; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x4d; 16]),
            source_revision,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x4d; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema),
                SourceOrigin::new(source_unit_id, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record_type),
                SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(ENUM_TYPE),
                SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: record_field,
                },
                SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: second_record_field,
                },
                SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
            ),
        ];
        let standard =
            verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap())
                .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source_revision, catalogue_revision),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
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
    fn version_one_and_two_codecs_reject_record_runtime_values() {
        let active = active_record_revision();
        let record_type = active.catalogue().record_value_types()[0].id();
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record_type,
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );

        assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
        assert_eq!(
            encode_catalogue_value(active.catalogue(), &value),
            Err(ValueCodecError::UnsupportedValue)
        );
    }

    #[test]
    fn active_codec_has_exact_record_bytes_and_round_trips() {
        let active = active_record_revision();
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let mut field_value = b"ORV3".to_vec();
        field_value.push(0x02);
        field_value.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        field_value.extend_from_slice(&1_u32.to_be_bytes());
        field_value.push(1);
        let mut payload = 2_u32.to_be_bytes().to_vec();
        payload.extend_from_slice(&record.fields()[0].id().to_bytes());
        payload.extend_from_slice(&26_u32.to_be_bytes());
        payload.extend_from_slice(&field_value);
        let mut second_field_value = b"ORV3".to_vec();
        second_field_value.push(0x0a);
        second_field_value.extend_from_slice(&ENUM_TYPE.to_bytes());
        second_field_value.extend_from_slice(&4_u32.to_be_bytes());
        second_field_value.extend_from_slice(b"lead");
        payload.extend_from_slice(&record.fields()[1].id().to_bytes());
        payload.extend_from_slice(&29_u32.to_be_bytes());
        payload.extend_from_slice(&second_field_value);
        let mut expected = b"ORV3".to_vec();
        expected.push(0x0b);
        expected.extend_from_slice(&record.id().to_bytes());
        expected.extend_from_slice(&99_u32.to_be_bytes());
        expected.extend_from_slice(&payload);

        assert_eq!(encode_active_value(&active, &value), Ok(expected.clone()));
        assert_eq!(decode_active_value(&active, &expected), Ok(value));
    }

    #[test]
    fn registered_codec_has_exact_opaque_bytes_and_preserves_earlier_closure() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let payload = [0x71; 16];
        let value = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
        );
        let mut expected = b"ORV4".to_vec();
        expected.push(0x0c);
        expected.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(&payload);

        assert_eq!(
            encode_registered_value(&active, &registry, &value),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_registered_value(&active, &registry, &expected),
            Ok(value.clone())
        );
        assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
        assert_eq!(
            encode_catalogue_value(active.catalogue(), &value),
            Err(ValueCodecError::UnsupportedValue)
        );
        assert_eq!(
            encode_active_value(&active, &value),
            Err(ValueCodecError::UnsupportedValue)
        );
        assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
        assert_eq!(
            decode_catalogue_value(active.catalogue(), &expected),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_value(&active, &expected),
            Err(ValueCodecError::InvalidMarker)
        );

        let mut wrong_type = expected.clone();
        wrong_type[5..21].fill(0x72);
        assert_eq!(
            decode_registered_value(&active, &registry, &wrong_type),
            Err(ValueCodecError::OpaqueValue {
                source: OpaqueValueError::UnregisteredType {
                    opaque_type: TypeId::from_bytes([0x72; 16]),
                },
            })
        );
        let mut wrong_length = expected;
        wrong_length[21..25].copy_from_slice(&15_u32.to_be_bytes());
        wrong_length.pop();
        assert_eq!(
            decode_registered_value(&active, &registry, &wrong_length),
            Err(ValueCodecError::OpaqueValue {
                source: OpaqueValueError::WrongPayloadLength {
                    opaque_type: OPAQUE_TOKEN_TYPE_ID,
                    expected: 16,
                    actual: 15,
                },
            })
        );
    }

    #[test]
    fn registered_codec_retains_version_three_shapes_under_its_marker() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let active_bytes = encode_active_value(&active, &value).unwrap();
        let registered_bytes = encode_registered_value(&active, &registry, &value).unwrap();
        assert_eq!(&registered_bytes[..4], b"ORV4");
        assert_eq!(&registered_bytes[4..49], &active_bytes[4..49]);
        assert_eq!(&registered_bytes[49..53], b"ORV4");
        assert_eq!(&registered_bytes[53..95], &active_bytes[53..95]);
        assert_eq!(&registered_bytes[95..99], b"ORV4");
        assert_eq!(&registered_bytes[99..], &active_bytes[99..]);
        assert_eq!(
            decode_registered_value(&active, &registry, &registered_bytes),
            Ok(value)
        );
        assert_eq!(
            decode_active_value(&active, &registered_bytes),
            Err(ValueCodecError::InvalidMarker)
        );
    }

    #[test]
    fn active_client_frame_has_exact_record_bytes_and_round_trips() {
        let active = active_record_revision();
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let parameter = ParameterId::from_bytes([0x5f; 16]);
        let frame = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: value.clone(),
        };
        let mut record_value = b"ORV3".to_vec();
        record_value.push(0x0b);
        record_value.extend_from_slice(&record.id().to_bytes());
        record_value.extend_from_slice(&99_u32.to_be_bytes());
        record_value.extend_from_slice(&2_u32.to_be_bytes());
        record_value.extend_from_slice(&record.fields()[0].id().to_bytes());
        record_value.extend_from_slice(&26_u32.to_be_bytes());
        record_value.extend_from_slice(b"ORV3");
        record_value.push(0x02);
        record_value.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        record_value.extend_from_slice(&1_u32.to_be_bytes());
        record_value.push(1);
        record_value.extend_from_slice(&record.fields()[1].id().to_bytes());
        record_value.extend_from_slice(&29_u32.to_be_bytes());
        record_value.extend_from_slice(b"ORV3");
        record_value.push(0x0a);
        record_value.extend_from_slice(&ENUM_TYPE.to_bytes());
        record_value.extend_from_slice(&4_u32.to_be_bytes());
        record_value.extend_from_slice(b"lead");
        let mut expected = b"ORF3\x02\0".to_vec();
        expected.extend_from_slice(&7_u64.to_be_bytes());
        expected.extend_from_slice(&140_u32.to_be_bytes());
        expected.extend_from_slice(&parameter.to_bytes());
        expected.extend_from_slice(&record_value);

        assert_eq!(
            encode_active_client_frame(&active, &frame),
            Ok(expected.clone())
        );
        assert_eq!(decode_active_client_frame(&active, &expected), Ok(frame));
        assert_eq!(
            decode_client_frame(&expected),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_client_frame(active.catalogue(), &expected),
            Err(FrameCodecError::InvalidMarker)
        );
        let mut wrong_value_marker = expected.clone();
        wrong_value_marker[34..38].copy_from_slice(b"ORV2");
        assert_eq!(
            decode_active_client_frame(&active, &wrong_value_marker),
            Err(FrameCodecError::Value {
                source: ValueCodecError::InvalidMarker,
            })
        );

        let server = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        };
        let mut expected_server = b"ORF3\x82\0".to_vec();
        expected_server.extend_from_slice(&7_u64.to_be_bytes());
        expected_server.extend_from_slice(&140_u32.to_be_bytes());
        expected_server.push(0x01);
        expected_server.extend_from_slice(&1_u16.to_be_bytes());
        expected_server.extend_from_slice(&1_u64.to_be_bytes());
        expected_server.push(0x01);
        expected_server.extend_from_slice(&124_u32.to_be_bytes());
        expected_server.extend_from_slice(&record_value);
        assert_eq!(
            encode_active_server_frame(&active, &server),
            Ok(expected_server.clone())
        );
        assert_eq!(
            decode_active_server_frame(&active, &expected_server),
            Ok(server)
        );

        for marker in [b"ORF1", b"ORF2"] {
            let mut wrong_version = expected.clone();
            wrong_version[..4].copy_from_slice(marker);
            assert_eq!(
                decode_active_client_frame(&active, &wrong_version),
                Err(FrameCodecError::InvalidMarker)
            );
        }
    }

    #[test]
    fn registered_frame_carries_opaque_results_but_rejects_opaque_arguments() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let payload = [0x73; 16];
        let value = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
        );
        let mut encoded_value = b"ORV4".to_vec();
        encoded_value.push(0x0c);
        encoded_value.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
        encoded_value.extend_from_slice(&16_u32.to_be_bytes());
        encoded_value.extend_from_slice(&payload);

        let server = ServerFrame::EventBatch {
            stream: 8,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value.clone()),
            }],
        };
        let mut expected_server = b"ORF4\x82\0".to_vec();
        expected_server.extend_from_slice(&8_u64.to_be_bytes());
        expected_server.extend_from_slice(&57_u32.to_be_bytes());
        expected_server.push(0x01);
        expected_server.extend_from_slice(&1_u16.to_be_bytes());
        expected_server.extend_from_slice(&1_u64.to_be_bytes());
        expected_server.push(0x01);
        expected_server.extend_from_slice(&41_u32.to_be_bytes());
        expected_server.extend_from_slice(&encoded_value);
        assert_eq!(
            encode_registered_server_frame(&active, &registry, &server),
            Ok(expected_server.clone())
        );
        assert_eq!(
            decode_registered_server_frame(&active, &registry, &expected_server),
            Ok(server)
        );
        assert_eq!(
            decode_active_server_frame(&active, &expected_server),
            Err(FrameCodecError::InvalidMarker)
        );

        let parameter = ParameterId::from_bytes([0x74; 16]);
        let argument = ClientFrame::CallArgument {
            stream: 8,
            parameter,
            value: value.clone(),
        };
        assert_eq!(
            encode_registered_client_frame(&active, &registry, &argument),
            Err(FrameCodecError::OpaqueArgumentNotAccepted {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
            })
        );
        let mut encoded_argument = b"ORF4\x02\0".to_vec();
        encoded_argument.extend_from_slice(&8_u64.to_be_bytes());
        encoded_argument.extend_from_slice(&57_u32.to_be_bytes());
        encoded_argument.extend_from_slice(&parameter.to_bytes());
        encoded_argument.extend_from_slice(&encoded_value);
        assert_eq!(
            decode_registered_client_frame(&active, &registry, &encoded_argument),
            Err(FrameCodecError::OpaqueArgumentNotAccepted {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
            })
        );

        let mut connection = ProtocolConnection::default();
        let function = FunctionId::from_bytes([0x75; 16]);
        connection
            .receive_registered(
                &active,
                &registry,
                ClientFrame::CallRawStart {
                    stream: 8,
                    function,
                },
            )
            .unwrap();
        assert_eq!(
            connection.receive_registered(&active, &registry, argument),
            Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::OpaqueArgumentNotAccepted {
                    opaque_type: OPAQUE_TOKEN_TYPE_ID,
                },
            })
        );
        assert_eq!(connection.live_streams(), 1);
        connection
            .receive_registered(
                &active,
                &registry,
                ClientFrame::WindowUpdate {
                    stream: 8,
                    channel: Channel::ResultValues,
                    credit: 57,
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .receive_registered(
                    &active,
                    &registry,
                    ClientFrame::CallArgumentsComplete { stream: 8 },
                )
                .unwrap(),
            Some(ClientAction::Dispatch {
                stream: 8,
                call: RawCall {
                    function,
                    arguments: vec![],
                },
            })
        );
        let invocation = InvocationId::from_bytes([0x76; 16]);
        connection
            .apply_registered(
                &active,
                &registry,
                ServerAction::Accepted {
                    stream: 8,
                    invocation,
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .apply_registered(
                    &active,
                    &registry,
                    ServerAction::Events {
                        stream: 8,
                        events: vec![Event::Value(value.clone())],
                    },
                )
                .unwrap(),
            ServerFrame::EventBatch {
                stream: 8,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Value(value),
                }],
            }
        );
    }

    #[test]
    fn active_frame_codec_round_trips_every_non_value_frame_shape() {
        let active = active_record_revision();
        let function = FunctionId::from_bytes([0x60; 16]);
        let invocation = InvocationId::from_bytes([0x61; 16]);
        let token = [1, 2, 3, 4, 5, 6, 7, 8];
        let client_frames = [
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 4096,
            },
            ClientFrame::CallCancel { stream: 1 },
            ClientFrame::Ping { token },
        ];
        for frame in client_frames {
            let encoded = encode_active_client_frame(&active, &frame).unwrap();
            let version_one = encode_client_frame(&frame).unwrap();
            assert_eq!(&encoded[..4], b"ORF3");
            assert_eq!(&encoded[4..], &version_one[4..]);
            assert_eq!(decode_active_client_frame(&active, &encoded), Ok(frame));
        }

        let server_frames = [
            ServerFrame::CallAccepted {
                stream: 1,
                invocation,
            },
            ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultBytes,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Bytes(vec![1, 2, 3]),
                }],
            },
            ServerFrame::CallCompleted { stream: 1 },
            ServerFrame::CallFailed {
                stream: 1,
                failure: CallFailure::ExecuteDenied,
            },
            ServerFrame::CallCancelled { stream: 1 },
            ServerFrame::Pong { token },
        ];
        for frame in server_frames {
            let encoded = encode_active_server_frame(&active, &frame).unwrap();
            let version_one = encode_server_frame(&frame).unwrap();
            assert_eq!(&encoded[..4], b"ORF3");
            assert_eq!(&encoded[4..], &version_one[4..]);
            assert_eq!(decode_active_server_frame(&active, &encoded), Ok(frame));
        }
    }

    #[test]
    fn active_frame_codec_rejects_a_record_from_an_incompatible_revision() {
        let original = active_record_revision();
        let record = &original.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &original,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(original.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let frame = ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([0x62; 16]),
            value,
        };
        let encoded = encode_active_client_frame(&original, &frame).unwrap();
        let changed = active_record_revision_with_second_type(ResolvedType::value(BIGINT_TYPE_ID));

        assert_eq!(
            encode_active_client_frame(&changed, &frame),
            Err(FrameCodecError::Value {
                source: ValueCodecError::RecordValueNotActive {
                    record_type: record.id(),
                },
            })
        );
        assert!(matches!(
            decode_active_client_frame(&changed, &encoded),
            Err(FrameCodecError::Value {
                source: ValueCodecError::WrongRecordFieldType { ordinal: 1, .. },
            })
        ));
    }

    #[test]
    fn active_connection_carries_record_arguments_and_results() {
        let active = active_record_revision();
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let function = FunctionId::from_bytes([0x63; 16]);
        let parameter = ParameterId::from_bytes([0x64; 16]);
        let invocation = InvocationId::from_bytes([0x65; 16]);
        let mut connection = ProtocolConnection::new();
        connection
            .receive_active(
                &active,
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
            )
            .unwrap();
        connection
            .receive_active(
                &active,
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter,
                    value: value.clone(),
                },
            )
            .unwrap();
        connection
            .receive_active(
                &active,
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 4096,
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .receive_active(&active, ClientFrame::CallArgumentsComplete { stream: 1 })
                .unwrap(),
            Some(ClientAction::Dispatch {
                stream: 1,
                call: RawCall {
                    function,
                    arguments: vec![CallArgument {
                        parameter,
                        value: value.clone(),
                    }],
                },
            })
        );
        connection
            .apply_active(
                &active,
                ServerAction::Accepted {
                    stream: 1,
                    invocation,
                },
            )
            .unwrap();
        let result = connection
            .apply_active(
                &active,
                ServerAction::Events {
                    stream: 1,
                    events: vec![Event::Value(value.clone())],
                },
            )
            .unwrap();
        assert_eq!(
            result,
            ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Value(value),
                }],
            }
        );
    }

    #[test]
    fn active_codec_preserves_earlier_shapes_and_marker_closure() {
        let active = active_record_revision();
        let boolean = RuntimeValue::Boolean(true);
        let mut expected_boolean = b"ORV3".to_vec();
        expected_boolean.push(0x02);
        expected_boolean.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        expected_boolean.extend_from_slice(&1_u32.to_be_bytes());
        expected_boolean.push(1);
        let enum_value =
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());
        let mut expected_enum = b"ORV3".to_vec();
        expected_enum.push(0x0a);
        expected_enum.extend_from_slice(&ENUM_TYPE.to_bytes());
        expected_enum.extend_from_slice(&9_u32.to_be_bytes());
        expected_enum.extend_from_slice(b"qualified");

        assert_eq!(
            encode_active_value(&active, &boolean),
            Ok(expected_boolean.clone())
        );
        assert_eq!(decode_active_value(&active, &expected_boolean), Ok(boolean));
        assert_eq!(
            encode_active_value(&active, &enum_value),
            Ok(expected_enum.clone())
        );
        assert_eq!(decode_active_value(&active, &expected_enum), Ok(enum_value));
        assert_eq!(
            decode_value(&expected_boolean),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_value(active.catalogue(), &expected_boolean),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_value(&active, &encoded_value(0x02, BOOLEAN_TYPE_ID, &[1])),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_value(
                &active,
                &encoded_catalogue_value(0x02, BOOLEAN_TYPE_ID, &[1])
            ),
            Err(ValueCodecError::InvalidMarker)
        );
    }

    #[test]
    fn active_codec_rejects_record_structure_and_value_corruption() {
        let active = active_record_revision();
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let encoded = encode_active_value(&active, &value).unwrap();

        let mut wrong_count = encoded.clone();
        wrong_count[25..29].copy_from_slice(&1_u32.to_be_bytes());
        assert_eq!(
            decode_active_value(&active, &wrong_count),
            Err(ValueCodecError::WrongRecordFieldCount {
                expected: 2,
                actual: 1,
            })
        );

        let mut wrong_identity = encoded.clone();
        wrong_identity[29..45].fill(0xff);
        assert_eq!(
            decode_active_value(&active, &wrong_identity),
            Err(ValueCodecError::WrongRecordFieldIdentity {
                ordinal: 0,
                expected: record.fields()[0].id(),
                actual: FieldId::from_bytes([0xff; 16]),
            })
        );

        let mut unknown_record = encoded.clone();
        unknown_record[5..21].fill(0xfe);
        assert_eq!(
            decode_active_value(&active, &unknown_record),
            Err(ValueCodecError::InactiveRecordType {
                record_type: TypeId::from_bytes([0xfe; 16]),
            })
        );

        let mut unknown_tag = encoded.clone();
        unknown_tag[4] = 0x0c;
        assert_eq!(
            decode_active_value(&active, &unknown_tag),
            Err(ValueCodecError::UnknownTag { tag: 0x0c })
        );

        for declared in [24_u32, u32::MAX] {
            let mut wrong_length = encoded.clone();
            wrong_length[45..49].copy_from_slice(&declared.to_be_bytes());
            assert_eq!(
                decode_active_value(&active, &wrong_length),
                Err(ValueCodecError::InvalidRecordFieldLength {
                    ordinal: 0,
                    declared: declared as usize,
                    remaining: 75,
                })
            );
        }

        let mut nested = encoded.clone();
        nested[53] = 0x0b;
        assert_eq!(
            decode_active_value(&active, &nested),
            Err(ValueCodecError::NestedRecordValue { ordinal: 0 })
        );

        let mut wrong_field_type = encoded.clone();
        wrong_field_type[53] = 0x06;
        wrong_field_type[54..70].copy_from_slice(&CHARACTER_LARGE_OBJECT_TYPE_ID.to_bytes());
        assert_eq!(
            decode_active_value(&active, &wrong_field_type),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: ResolvedType::value(BOOLEAN_TYPE_ID),
                tag: 0x06,
                actual: CHARACTER_LARGE_OBJECT_TYPE_ID,
            })
        );

        let mut stale_enum = encoded.clone();
        stale_enum[120..124].copy_from_slice(b"lost");
        assert_eq!(
            decode_active_value(&active, &stale_enum),
            Err(ValueCodecError::UndeclaredEnumLabel {
                enum_type: ENUM_TYPE,
                label: String::from("lost"),
            })
        );

        let mut wrong_inner_marker = encoded.clone();
        wrong_inner_marker[49..53].copy_from_slice(b"ORV2");
        assert_eq!(
            decode_active_value(&active, &wrong_inner_marker),
            Err(ValueCodecError::InvalidMarker)
        );

        let mut null_field = encoded.clone();
        null_field[45..49].copy_from_slice(&25_u32.to_be_bytes());
        null_field[53] = 0x00;
        null_field[70..74].copy_from_slice(&0_u32.to_be_bytes());
        null_field.remove(74);
        null_field[21..25].copy_from_slice(&98_u32.to_be_bytes());
        assert_eq!(
            decode_active_value(&active, &null_field),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: ResolvedType::value(BOOLEAN_TYPE_ID),
                tag: 0x00,
                actual: BOOLEAN_TYPE_ID,
            })
        );

        let mut reference_field = encoded.clone();
        let reference_type = TypeId::from_bytes([0x51; 16]);
        reference_field[45..49].copy_from_slice(&41_u32.to_be_bytes());
        reference_field[53] = 0x08;
        reference_field[54..70].copy_from_slice(&reference_type.to_bytes());
        reference_field[70..74].copy_from_slice(&16_u32.to_be_bytes());
        reference_field.splice(74..75, [0x52; 16]);
        reference_field[21..25].copy_from_slice(&114_u32.to_be_bytes());
        assert_eq!(
            decode_active_value(&active, &reference_field),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: ResolvedType::value(BOOLEAN_TYPE_ID),
                tag: 0x08,
                actual: reference_type,
            })
        );

        let mut truncated = encoded.clone();
        truncated.pop();
        assert_eq!(
            decode_active_value(&active, &truncated),
            Err(ValueCodecError::TruncatedPayload {
                declared: 99,
                actual: 98,
            })
        );

        let mut trailing = encoded.clone();
        trailing[21..25].copy_from_slice(&100_u32.to_be_bytes());
        trailing.push(0);
        assert_eq!(
            decode_active_value(&active, &trailing),
            Err(ValueCodecError::TrailingBytes {
                declared: 99,
                actual: 100,
            })
        );

        let mut oversized = encoded;
        oversized[21..25].copy_from_slice(&((PAYLOAD_LIMIT as u32) + 1).to_be_bytes());
        assert_eq!(
            decode_active_value(&active, &oversized),
            Err(ValueCodecError::PayloadTooLarge {
                actual: PAYLOAD_LIMIT + 1,
                maximum: PAYLOAD_LIMIT,
            })
        );
    }

    #[test]
    fn active_codec_rejects_a_field_length_that_consumes_the_next_entry() {
        let active = active_record_revision_with_types(
            ResolvedType::value(BINARY_LARGE_OBJECT_TYPE_ID),
            ResolvedType::named(ENUM_TYPE),
        );
        let record = &active.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Bytes(vec![1])),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let mut encoded = encode_active_value(&active, &value).unwrap();
        encoded[45..49].copy_from_slice(&75_u32.to_be_bytes());
        encoded[70..74].copy_from_slice(&50_u32.to_be_bytes());

        assert_eq!(
            decode_active_value(&active, &encoded),
            Err(ValueCodecError::TruncatedRecordFieldHeader {
                ordinal: 1,
                actual: 0,
            })
        );
    }

    #[test]
    fn active_codec_rejects_a_record_from_an_incompatible_active_revision() {
        let original = active_record_revision();
        let record = &original.catalogue().record_value_types()[0];
        let value = RuntimeValue::Record(
            RecordValue::new(
                &original,
                record.id(),
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (
                        String::from("verified"),
                        RuntimeValue::Enum(
                            EnumValue::new(original.catalogue(), ENUM_TYPE, "lead").unwrap(),
                        ),
                    ),
                ],
            )
            .unwrap(),
        );
        let changed = active_record_revision_with_second_type(ResolvedType::value(BIGINT_TYPE_ID));

        assert_eq!(
            encode_active_value(&changed, &value),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: record.id(),
            })
        );
    }

    #[test]
    fn catalogue_codec_round_trips_enum_null_and_legacy_values_as_version_two() {
        let catalogue = enum_catalogue(&["lead"]);
        let null = RuntimeValue::null(ResolvedType::named(ENUM_TYPE)).unwrap();
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

        #[test]
        fn arbitrary_version_three_envelopes_never_panic(
            tag in any::<u8>(),
            type_bytes in any::<[u8; 16]>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let mut encoded = b"ORV3".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&type_bytes);
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_active_value(&active, &encoded);
        }

        #[test]
        fn arbitrary_version_three_frame_envelopes_never_panic(
            tag in any::<u8>(),
            flags in any::<u8>(),
            stream in any::<u64>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let mut encoded = b"ORF3".to_vec();
            encoded.push(tag);
            encoded.push(flags);
            encoded.extend_from_slice(&stream.to_be_bytes());
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_active_client_frame(&active, &encoded);
            let _ = decode_active_server_frame(&active, &encoded);
        }

        #[test]
        fn arbitrary_version_four_envelopes_never_panic(
            tag in any::<u8>(),
            type_bytes in any::<[u8; 16]>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let registry = registered_opaque_codecs(
                active.catalogue_hash_context().standard().unwrap(),
            ).unwrap();
            let mut encoded = b"ORV4".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&type_bytes);
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_registered_value(&active, &registry, &encoded);
        }

        #[test]
        fn arbitrary_version_four_frame_envelopes_never_panic(
            tag in any::<u8>(),
            flags in any::<u8>(),
            stream in any::<u64>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let registry = registered_opaque_codecs(
                active.catalogue_hash_context().standard().unwrap(),
            ).unwrap();
            let mut encoded = b"ORF4".to_vec();
            encoded.push(tag);
            encoded.push(flags);
            encoded.extend_from_slice(&stream.to_be_bytes());
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_registered_client_frame(&active, &registry, &encoded);
            let _ = decode_registered_server_frame(&active, &registry, &encoded);
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
