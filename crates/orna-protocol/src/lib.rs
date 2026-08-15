//! Canonical runtime values and the bounded authenticated raw-call protocol.

mod frame;

pub use frame::{
    CallArgument, CallFailure, Channel, ClientAction, ClientFrame, ConnectionError, Event,
    EventRecord, FrameCodecError, MAX_CHANNEL_WINDOW, MAX_FRAME_PAYLOAD_LENGTH, ProtocolConnection,
    RawCall, RawCallClient, RawCallClientError, RawCallClientResponse, ServerAction, ServerFrame,
    decode_active_client_frame, decode_active_server_frame, decode_catalogue_client_frame,
    decode_catalogue_server_frame, decode_client_frame, decode_constructed_client_frame,
    decode_constructed_server_frame, decode_registered_client_frame,
    decode_registered_server_frame, decode_server_frame, encode_active_client_frame,
    encode_active_server_frame, encode_catalogue_client_frame, encode_catalogue_server_frame,
    encode_client_frame, encode_constructed_client_frame, encode_constructed_server_frame,
    encode_registered_client_frame, encode_registered_server_frame, encode_server_frame,
};

use std::{error::Error, fmt};

use orna_core::{
    FieldId, ObjectId, TypeId,
    catalogue::CatalogueSnapshot,
    revision::ActiveDatabaseRevision,
    types::{
        MAX_TYPE_DESCRIPTOR_DEPTH, ResolvedType, StandardScalar, TypeDescriptor,
        TypeDescriptorError, TypeDescriptorKind,
    },
    value::{
        CollectionValueError, CollectionValuePathSegment, ConstructedValueKind, EnumValue,
        EnumValueError, MAX_RUNTIME_VALUE_NODES, OpaqueCodecRegistry, OpaqueValue,
        OpaqueValueError, RecordValue, RuntimeFloat, RuntimeType, RuntimeValue,
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
const CONSTRUCTED_MARKER: &[u8; 4] = b"ORV5";
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
const CONSTRUCTED_TAG: u8 = 0x0d;
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
    /// A constructed value does not use the required all-zero identity sentinel.
    ConstructedTypeIdentityNotZero {
        /// The non-zero identity from the constructed value header.
        identity: TypeId,
    },
    /// A constructed payload does not contain its two-byte descriptor length.
    TruncatedConstructedHeader {
        /// The available constructed payload bytes.
        actual: usize,
    },
    /// A constructed payload has an empty descriptor region.
    EmptyConstructedDescriptor,
    /// The declared descriptor region exceeds the constructed payload.
    TruncatedConstructedDescriptor {
        /// The declared descriptor length.
        declared: usize,
        /// The available descriptor bytes.
        available: usize,
    },
    /// A descriptor node ends before its required bytes occur.
    TruncatedConstructedDescriptorNode {
        /// The zero-based descriptor offset of the incomplete node.
        offset: usize,
        /// The minimum bytes required at this node.
        required: usize,
        /// The available bytes from this node.
        available: usize,
    },
    /// Bytes remain after one complete descriptor tree.
    TrailingConstructedDescriptor {
        /// The unconsumed descriptor bytes.
        remaining: usize,
    },
    /// A descriptor byte is not defined by ORV5.
    UnknownConstructedDescriptorTag {
        /// The unrecognised descriptor byte.
        tag: u8,
    },
    /// A descriptor exceeds the core structural bounds.
    InvalidConstructedDescriptor {
        /// The core descriptor failure.
        source: TypeDescriptorError,
    },
    /// A descriptor is structurally valid but is not admitted for collections.
    UnsupportedConstructedDescriptor {
        /// The rejected complete descriptor.
        descriptor: TypeDescriptor,
    },
    /// A constructed OPTION presence byte is not zero or one.
    InvalidOptionPresence {
        /// The invalid presence byte.
        value: u8,
    },
    /// A collection entry ends before its length or value is complete.
    TruncatedCollectionEntry {
        /// The first incomplete collection path.
        path: Vec<CollectionValuePathSegment>,
    },
    /// One isolated complete child value is invalid.
    ConstructedChild {
        /// The child path from the constructed root.
        path: Vec<CollectionValuePathSegment>,
        /// The child codec failure.
        source: Box<ValueCodecError>,
    },
    /// MAP entries are not already in canonical key order.
    NonCanonicalMapOrder {
        /// The first non-canonical wire entry index.
        index: usize,
    },
    /// The core checked collection constructor rejected a value.
    CollectionValue {
        /// The core collection failure.
        source: CollectionValueError,
    },
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
    /// A record field value does not use its declared wire type.
    WrongRecordFieldType {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The field descriptor required by the active definition.
        expected: TypeDescriptor,
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
            Self::ConstructedTypeIdentityNotZero { .. } => {
                formatter.write_str("constructed runtime value identity must be zero")
            }
            Self::TruncatedConstructedHeader { .. } => {
                formatter.write_str("constructed runtime value header is truncated")
            }
            Self::EmptyConstructedDescriptor => {
                formatter.write_str("constructed runtime value descriptor is empty")
            }
            Self::TruncatedConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is truncated")
            }
            Self::TruncatedConstructedDescriptorNode { .. } => {
                formatter.write_str("constructed runtime value descriptor node is truncated")
            }
            Self::TrailingConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor has trailing bytes")
            }
            Self::UnknownConstructedDescriptorTag { .. } => {
                formatter.write_str("constructed runtime value descriptor tag is unknown")
            }
            Self::InvalidConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is invalid")
            }
            Self::UnsupportedConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is not accepted")
            }
            Self::InvalidOptionPresence { .. } => {
                formatter.write_str("constructed OPTION presence is invalid")
            }
            Self::TruncatedCollectionEntry { .. } => {
                formatter.write_str("constructed runtime value entry is truncated")
            }
            Self::ConstructedChild { .. } => {
                formatter.write_str("constructed runtime value child is invalid")
            }
            Self::NonCanonicalMapOrder { .. } => {
                formatter.write_str("constructed MAP entries are not in canonical key order")
            }
            Self::CollectionValue { .. } => {
                formatter.write_str("constructed runtime value is invalid")
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
            Self::InvalidConstructedDescriptor { source } => Some(source),
            Self::ConstructedChild { source, .. } => Some(source),
            Self::CollectionValue { source } => Some(source),
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
    encode_registered_value_with_marker(active, registry, value, REGISTERED_MARKER)
}

fn encode_registered_value_with_marker(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
    marker: &[u8; 4],
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
                marker,
                OPAQUE_TAG,
                checked.opaque_type(),
                checked.canonical_payload(),
            ))
        }
        RuntimeValue::Record(value) => encode_record_value_with_marker(active, value, marker),
        _ => encode_catalogue_value(active.catalogue(), value).map(|mut encoded| {
            encoded[..marker.len()].copy_from_slice(marker);
            encoded
        }),
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
    decode_registered_value_with_marker(active, registry, encoded, REGISTERED_MARKER)
}

fn decode_registered_value_with_marker(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, marker)?;
    match tag {
        RECORD_TAG => decode_record_value_with_marker(active, type_id, payload, marker),
        OPAQUE_TAG => OpaqueValue::new(active, registry, type_id, payload)
            .map(RuntimeValue::Opaque)
            .map_err(|source| ValueCodecError::OpaqueValue { source }),
        _ => decode_active_non_record_value(active, tag, type_id, payload),
    }
}

/// Encodes one complete ORV5 runtime value.
///
/// ORV5 retains every ORV4 value and adds checked constructed values.
pub fn encode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    encode_orv5_value(active, registry, value)
}

/// Decodes one complete ORV5 runtime value.
///
/// The decoder validates the whole structural tree before materialising any
/// value. This preserves the authoritative global node-limit precedence.
pub fn decode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, CONSTRUCTED_MARKER)?;
    if tag == CONSTRUCTED_TAG && type_id.to_bytes() != [0; 16] {
        return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
    }
    if tag == CONSTRUCTED_TAG {
        let (descriptor, body) = decode_constructed_descriptor(payload)?;
        preflight_constructed_descriptor(active, &descriptor)?;
        let mut nodes = 0_usize;
        preflight_orv5_tree(payload, tag, &mut nodes, &mut Vec::new())?;
        return decode_constructed_parts(active, registry, descriptor, body);
    }
    let mut nodes = 0_usize;
    preflight_orv5_tree(payload, tag, &mut nodes, &mut Vec::new())?;
    decode_orv5_parts(active, registry, tag, type_id, payload)
}

fn encode_orv5_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    let RuntimeValue::Constructed(constructed) = value else {
        return encode_registered_value_with_marker(active, registry, value, CONSTRUCTED_MARKER);
    };
    let descriptor = constructed.descriptor().clone();
    let mut descriptor_bytes = Vec::new();
    encode_constructed_descriptor(&descriptor, &mut descriptor_bytes)?;
    let descriptor_length = u16::try_from(descriptor_bytes.len()).map_err(|_| {
        ValueCodecError::InvalidConstructedDescriptor {
            source: TypeDescriptorError::TooLarge {
                maximum: u16::MAX as usize,
                actual: descriptor_bytes.len(),
            },
        }
    })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&descriptor_length.to_be_bytes());
    payload.extend_from_slice(&descriptor_bytes);

    match constructed.kind() {
        ConstructedValueKind::Option(value) => {
            RuntimeValue::option(active, descriptor.clone(), value.cloned())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            match value {
                None => payload.push(0),
                Some(value) => {
                    payload.push(1);
                    append_orv5_child(active, registry, &mut payload, value)?;
                }
            }
        }
        ConstructedValueKind::List(values) => {
            RuntimeValue::list(active, descriptor.clone(), values.to_vec())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            append_count(&mut payload, values.len())?;
            for child in values {
                append_orv5_child(active, registry, &mut payload, child)?;
            }
        }
        ConstructedValueKind::Map(entries) => {
            RuntimeValue::map(active, descriptor.clone(), entries.to_vec())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            append_count(&mut payload, entries.len())?;
            for (key, mapped) in entries {
                append_orv5_child(active, registry, &mut payload, key)?;
                append_orv5_child(active, registry, &mut payload, mapped)?;
            }
        }
        _ => return Err(ValueCodecError::UnsupportedValue),
    }
    require_payload_limit(payload.len())?;
    Ok(encode_with_marker(
        CONSTRUCTED_MARKER,
        CONSTRUCTED_TAG,
        TypeId::from_bytes([0; 16]),
        &payload,
    ))
}

fn append_count(payload: &mut Vec<u8>, count: usize) -> Result<(), ValueCodecError> {
    let count = u32::try_from(count).map_err(|_| ValueCodecError::PayloadTooLarge {
        actual: usize::MAX,
        maximum: PAYLOAD_LIMIT,
    })?;
    payload.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_orv5_child(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &mut Vec<u8>,
    value: &RuntimeValue,
) -> Result<(), ValueCodecError> {
    let encoded = encode_orv5_value(active, registry, value)?;
    let length = u32::try_from(encoded.len()).map_err(|_| ValueCodecError::PayloadTooLarge {
        actual: encoded.len(),
        maximum: PAYLOAD_LIMIT,
    })?;
    let next = payload
        .len()
        .checked_add(4)
        .and_then(|length| length.checked_add(encoded.len()))
        .ok_or(ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    require_payload_limit(next)?;
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&encoded);
    Ok(())
}

fn encode_constructed_descriptor(
    descriptor: &TypeDescriptor,
    encoded: &mut Vec<u8>,
) -> Result<(), ValueCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) => {
            encoded.push(0);
            encoded.extend_from_slice(&type_id.to_bytes());
        }
        TypeDescriptorKind::Reference(type_id) => {
            encoded.push(1);
            encoded.extend_from_slice(&type_id.to_bytes());
        }
        TypeDescriptorKind::List(child) => {
            encoded.push(2);
            encode_constructed_descriptor(child, encoded)?;
        }
        TypeDescriptorKind::Map { key, value } => {
            encoded.push(3);
            encode_constructed_descriptor(key, encoded)?;
            encode_constructed_descriptor(value, encoded)?;
        }
        TypeDescriptorKind::Option(child) => {
            encoded.push(4);
            encode_constructed_descriptor(child, encoded)?;
        }
        TypeDescriptorKind::Set(_) | TypeDescriptorKind::Stream(_) => {
            return Err(ValueCodecError::UnsupportedConstructedDescriptor {
                descriptor: descriptor.clone(),
            });
        }
    }
    Ok(())
}

fn decode_orv5_parts(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    if tag == CONSTRUCTED_TAG {
        if type_id.to_bytes() != [0; 16] {
            return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
        }
        return decode_constructed_payload(active, registry, payload);
    }
    let encoded = encode_with_marker(CONSTRUCTED_MARKER, tag, type_id, payload);
    decode_registered_value_with_marker(active, registry, &encoded, CONSTRUCTED_MARKER)
}

fn decode_constructed_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (descriptor, body) = decode_constructed_descriptor(payload)?;
    preflight_constructed_descriptor(active, &descriptor)?;
    decode_constructed_parts(active, registry, descriptor, body)
}

fn decode_constructed_parts(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Option(_) => decode_option_chain(active, registry, descriptor, body),
        TypeDescriptorKind::List(_) => decode_list_body(active, registry, descriptor, body),
        TypeDescriptorKind::Map { .. } => decode_map_body(active, registry, descriptor, body),
        _ => Err(ValueCodecError::UnsupportedConstructedDescriptor { descriptor }),
    }
}

fn decode_constructed_descriptor(
    payload: &[u8],
) -> Result<(TypeDescriptor, &[u8]), ValueCodecError> {
    if payload.len() < 2 {
        return Err(ValueCodecError::TruncatedConstructedHeader {
            actual: payload.len(),
        });
    }
    let length = u16::from_be_bytes(payload[..2].try_into().expect("length checked")) as usize;
    if length == 0 {
        return Err(ValueCodecError::EmptyConstructedDescriptor);
    }
    let available = payload.len() - 2;
    if available < length {
        return Err(ValueCodecError::TruncatedConstructedDescriptor {
            declared: length,
            available,
        });
    }
    let bytes = &payload[2..2 + length];
    let (descriptor, consumed) = parse_constructed_descriptor(bytes, 0)?;
    if consumed != bytes.len() {
        return Err(ValueCodecError::TrailingConstructedDescriptor {
            remaining: bytes.len() - consumed,
        });
    }
    Ok((descriptor, &payload[2 + length..]))
}

fn parse_constructed_descriptor(
    encoded: &[u8],
    offset: usize,
) -> Result<(TypeDescriptor, usize), ValueCodecError> {
    enum Pending {
        List,
        Option,
        MapKey,
        MapValue(TypeDescriptor),
    }

    let mut cursor = offset;
    let mut pending = Vec::new();
    let mut complete = None;
    loop {
        if let Some(mut descriptor) = complete.take() {
            loop {
                match pending.pop() {
                    Some(Pending::List) => {
                        descriptor = TypeDescriptor::list(descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    Some(Pending::Option) => {
                        descriptor = TypeDescriptor::option(descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    Some(Pending::MapKey) => {
                        pending.push(Pending::MapValue(descriptor));
                        break;
                    }
                    Some(Pending::MapValue(key)) => {
                        descriptor = TypeDescriptor::map(key, descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    None => return Ok((descriptor, cursor)),
                }
            }
            continue;
        }

        let available = encoded.len().saturating_sub(cursor);
        let tag =
            *encoded
                .get(cursor)
                .ok_or(ValueCodecError::TruncatedConstructedDescriptorNode {
                    offset: cursor,
                    required: 1,
                    available,
                })?;
        match tag {
            0 | 1 => {
                if available < 17 {
                    return Err(ValueCodecError::TruncatedConstructedDescriptorNode {
                        offset: cursor,
                        required: 17,
                        available,
                    });
                }
                let type_id = TypeId::from_bytes(
                    encoded[cursor + 1..cursor + 17]
                        .try_into()
                        .expect("descriptor leaf length checked"),
                );
                cursor += 17;
                complete = Some(if tag == 0 {
                    TypeDescriptor::named(type_id)
                } else {
                    TypeDescriptor::reference(type_id)
                });
            }
            2..=4 => {
                let depth = pending.len() + 1;
                if depth > MAX_TYPE_DESCRIPTOR_DEPTH {
                    return Err(ValueCodecError::InvalidConstructedDescriptor {
                        source: TypeDescriptorError::TooDeep {
                            maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                            actual: depth,
                        },
                    });
                }
                cursor += 1;
                pending.push(match tag {
                    2 => Pending::List,
                    3 => Pending::MapKey,
                    4 => Pending::Option,
                    _ => unreachable!("constructor tag was checked"),
                });
            }
            tag => return Err(ValueCodecError::UnknownConstructedDescriptorTag { tag }),
        }
    }
}

fn preflight_constructed_descriptor(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
) -> Result<(), ValueCodecError> {
    let result = match descriptor.kind() {
        TypeDescriptorKind::Option(_) => RuntimeValue::option(active, descriptor.clone(), None),
        TypeDescriptorKind::List(_) => RuntimeValue::list(active, descriptor.clone(), Vec::new()),
        TypeDescriptorKind::Map { .. } => RuntimeValue::map(active, descriptor.clone(), Vec::new()),
        _ => {
            return Err(ValueCodecError::UnsupportedConstructedDescriptor {
                descriptor: descriptor.clone(),
            });
        }
    };
    result
        .map(|_| ())
        .map_err(|source| ValueCodecError::CollectionValue { source })
}

fn decode_option_chain(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    mut descriptor: TypeDescriptor,
    mut body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let mut parents = Vec::new();
    loop {
        let Some((&presence, remaining)) = body.split_first() else {
            return Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() });
        };
        match presence {
            0 => {
                if !remaining.is_empty() {
                    return Err(ValueCodecError::TrailingBytes {
                        declared: 1,
                        actual: body.len(),
                    });
                }
                let tail = RuntimeValue::option(active, descriptor, None)
                    .map_err(|source| ValueCodecError::CollectionValue { source })?;
                return rebuild_option_chain(active, parents, tail);
            }
            1 => {
                let child_path = [CollectionValuePathSegment::OptionChild];
                let (encoded, consumed) = take_constructed_child(remaining, 0, &child_path)?;
                if consumed != remaining.len() {
                    return Err(ValueCodecError::TrailingBytes {
                        declared: consumed + 1,
                        actual: body.len(),
                    });
                }
                let (tag, type_id, payload) = decode_envelope(encoded, CONSTRUCTED_MARKER)
                    .map_err(|source| ValueCodecError::ConstructedChild {
                        path: vec![CollectionValuePathSegment::OptionChild],
                        source: Box::new(source),
                    })?;
                if tag == CONSTRUCTED_TAG {
                    if type_id.to_bytes() != [0; 16] {
                        return Err(ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(ValueCodecError::ConstructedTypeIdentityNotZero {
                                identity: type_id,
                            }),
                        });
                    }
                    let (child_descriptor, child_body) = decode_constructed_descriptor(payload)
                        .map_err(|source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        })?;
                    preflight_constructed_descriptor(active, &child_descriptor).map_err(
                        |source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        },
                    )?;
                    if matches!(child_descriptor.kind(), TypeDescriptorKind::Option(_)) {
                        parents.push(descriptor);
                        descriptor = child_descriptor;
                        body = child_body;
                        continue;
                    }
                    let child = decode_constructed_payload(active, registry, payload).map_err(
                        |source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        },
                    )?;
                    parents.push(descriptor);
                    return rebuild_option_chain(active, parents, child);
                }
                let child = decode_orv5_parts(active, registry, tag, type_id, payload).map_err(
                    |source| ValueCodecError::ConstructedChild {
                        path: vec![CollectionValuePathSegment::OptionChild],
                        source: Box::new(source),
                    },
                )?;
                parents.push(descriptor);
                return rebuild_option_chain(active, parents, child);
            }
            value => return Err(ValueCodecError::InvalidOptionPresence { value }),
        }
    }
}

fn rebuild_option_chain(
    active: &ActiveDatabaseRevision,
    parents: Vec<TypeDescriptor>,
    mut value: RuntimeValue,
) -> Result<RuntimeValue, ValueCodecError> {
    for (index, descriptor) in parents.into_iter().enumerate().rev() {
        value = match RuntimeValue::option(active, descriptor, Some(value)) {
            Ok(value) => value,
            Err(source) if index == 0 => return Err(ValueCodecError::CollectionValue { source }),
            Err(source) => {
                return Err(ValueCodecError::ConstructedChild {
                    path: vec![CollectionValuePathSegment::OptionChild; index],
                    source: Box::new(ValueCodecError::CollectionValue { source }),
                });
            }
        };
    }
    Ok(value)
}

fn decode_list_body(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let count = decode_constructed_count(body)?;
    let mut cursor = 4;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let path = vec![CollectionValuePathSegment::ListElement(index)];
        let (encoded, consumed) = take_constructed_child(body, cursor, &path)?;
        values.push(decode_orv5_child(active, registry, encoded, path)?);
        cursor = consumed;
    }
    if cursor != body.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: body.len(),
        });
    }
    RuntimeValue::list(active, descriptor, values)
        .map_err(|source| ValueCodecError::CollectionValue { source })
}

fn decode_map_body(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let count = decode_constructed_count(body)?;
    let mut cursor = 4;
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let key_path = vec![CollectionValuePathSegment::MapKey(index)];
        let (encoded_key, key_end) = take_constructed_child(body, cursor, &key_path)?;
        let key = decode_orv5_child(active, registry, encoded_key, key_path)?;
        let value_path = vec![CollectionValuePathSegment::MapValue(index)];
        let (encoded_value, value_end) = take_constructed_child(body, key_end, &value_path)?;
        let value = decode_orv5_child(active, registry, encoded_value, value_path)?;
        entries.push((key, value));
        cursor = value_end;
    }
    if cursor != body.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: body.len(),
        });
    }
    let wire_entries = entries.clone();
    let value = RuntimeValue::map(active, descriptor, entries)
        .map_err(|source| ValueCodecError::CollectionValue { source })?;
    let RuntimeValue::Constructed(constructed) = &value else {
        unreachable!("checked MAP construction returns a constructed value");
    };
    let ConstructedValueKind::Map(canonical) = constructed.kind() else {
        unreachable!("checked MAP construction retains MAP contents");
    };
    if canonical != wire_entries.as_slice() {
        let index = canonical
            .iter()
            .zip(&wire_entries)
            .position(|(left, right)| left != right)
            .unwrap_or(0);
        return Err(ValueCodecError::NonCanonicalMapOrder { index });
    }
    Ok(value)
}

fn decode_constructed_count(body: &[u8]) -> Result<usize, ValueCodecError> {
    if body.len() < 4 {
        return Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() });
    }
    Ok(u32::from_be_bytes(body[..4].try_into().expect("count length checked")) as usize)
}

fn take_constructed_child<'a>(
    body: &'a [u8],
    cursor: usize,
    path: &[CollectionValuePathSegment],
) -> Result<(&'a [u8], usize), ValueCodecError> {
    let remaining =
        body.get(cursor..)
            .ok_or_else(|| ValueCodecError::TruncatedCollectionEntry {
                path: path.to_vec(),
            })?;
    if remaining.len() < 4 {
        return Err(ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        });
    }
    let declared = u32::from_be_bytes(remaining[..4].try_into().expect("length checked")) as usize;
    let available = remaining.len() - 4;
    if declared < HEADER_LENGTH || declared > available {
        return Err(ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        });
    }
    let end = cursor
        .checked_add(4)
        .and_then(|start| start.checked_add(declared))
        .ok_or_else(|| ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        })?;
    Ok((&body[cursor + 4..end], end))
}

fn decode_orv5_child(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
    path: Vec<CollectionValuePathSegment>,
) -> Result<RuntimeValue, ValueCodecError> {
    decode_constructed_value(active, registry, encoded).map_err(|source| {
        ValueCodecError::ConstructedChild {
            path,
            source: Box::new(source),
        }
    })
}

fn preflight_orv5_tree(
    payload: &[u8],
    tag: u8,
    nodes: &mut usize,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), ValueCodecError> {
    increment_orv5_node(nodes)?;
    match tag {
        CONSTRUCTED_TAG => {
            let (descriptor, body) = decode_constructed_descriptor(payload)?;
            match descriptor.kind() {
                TypeDescriptorKind::Option(_) => preflight_orv5_option_chain(body, nodes, path)?,
                TypeDescriptorKind::List(_) => {
                    let count = decode_constructed_count(body)?;
                    let mut cursor = 4;
                    for index in 0..count {
                        let child_path = [CollectionValuePathSegment::ListElement(index)];
                        let (child, end) = take_constructed_child(body, cursor, &child_path)?;
                        path.push(CollectionValuePathSegment::ListElement(index));
                        let result = preflight_orv5_child(child, nodes, path);
                        path.pop();
                        result?;
                        cursor = end;
                    }
                    if cursor != body.len() {
                        return Err(ValueCodecError::TrailingBytes {
                            declared: cursor,
                            actual: body.len(),
                        });
                    }
                }
                TypeDescriptorKind::Map { .. } => {
                    let count = decode_constructed_count(body)?;
                    let mut cursor = 4;
                    for index in 0..count {
                        let key_path = [CollectionValuePathSegment::MapKey(index)];
                        let (key, key_end) = take_constructed_child(body, cursor, &key_path)?;
                        path.push(CollectionValuePathSegment::MapKey(index));
                        let key_result = preflight_orv5_child(key, nodes, path);
                        path.pop();
                        key_result?;
                        let value_path = [CollectionValuePathSegment::MapValue(index)];
                        let (value, value_end) =
                            take_constructed_child(body, key_end, &value_path)?;
                        path.push(CollectionValuePathSegment::MapValue(index));
                        let value_result = preflight_orv5_child(value, nodes, path);
                        path.pop();
                        value_result?;
                        cursor = value_end;
                    }
                    if cursor != body.len() {
                        return Err(ValueCodecError::TrailingBytes {
                            declared: cursor,
                            actual: body.len(),
                        });
                    }
                }
                _ => return Err(ValueCodecError::UnsupportedConstructedDescriptor { descriptor }),
            }
        }
        RECORD_TAG => preflight_orv5_record_tree(payload, nodes, path)?,
        _ => {}
    }
    Ok(())
}

fn increment_orv5_node(nodes: &mut usize) -> Result<(), ValueCodecError> {
    *nodes = nodes
        .checked_add(1)
        .ok_or(ValueCodecError::CollectionValue {
            source: CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            },
        })?;
    if *nodes > MAX_RUNTIME_VALUE_NODES {
        return Err(ValueCodecError::CollectionValue {
            source: CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            },
        });
    }
    Ok(())
}

fn preflight_orv5_option_chain(
    mut body: &[u8],
    nodes: &mut usize,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), ValueCodecError> {
    let path_start = path.len();
    loop {
        let Some((&presence, remaining)) = body.split_first() else {
            return Err(option_chain_body_error(
                path,
                path_start,
                ValueCodecError::TruncatedCollectionEntry { path: path.clone() },
            ));
        };
        match presence {
            0 => {
                if !remaining.is_empty() {
                    return Err(option_chain_body_error(
                        path,
                        path_start,
                        ValueCodecError::TrailingBytes {
                            declared: 1,
                            actual: body.len(),
                        },
                    ));
                }
                path.truncate(path_start);
                return Ok(());
            }
            1 => {
                let mut child_path = path.clone();
                child_path.push(CollectionValuePathSegment::OptionChild);
                let (child, consumed) = take_constructed_child(remaining, 0, &child_path)
                    .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                if consumed != remaining.len() {
                    return Err(option_chain_body_error(
                        path,
                        path_start,
                        ValueCodecError::TrailingBytes {
                            declared: consumed + 1,
                            actual: body.len(),
                        },
                    ));
                }
                let (tag, type_id, payload) = decode_envelope(child, CONSTRUCTED_MARKER)
                    .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                if tag == CONSTRUCTED_TAG {
                    if type_id.to_bytes() != [0; 16] {
                        return Err(wrap_preflight_child_error(
                            &child_path,
                            ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id },
                        ));
                    }
                    let (descriptor, child_body) = decode_constructed_descriptor(payload)
                        .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                    if matches!(descriptor.kind(), TypeDescriptorKind::Option(_)) {
                        increment_orv5_node(nodes)?;
                        path.push(CollectionValuePathSegment::OptionChild);
                        body = child_body;
                        continue;
                    }
                }
                path.push(CollectionValuePathSegment::OptionChild);
                let result = preflight_orv5_child(child, nodes, path);
                path.truncate(path_start);
                return result;
            }
            value => {
                return Err(option_chain_body_error(
                    path,
                    path_start,
                    ValueCodecError::InvalidOptionPresence { value },
                ));
            }
        }
    }
}

fn option_chain_body_error(
    path: &[CollectionValuePathSegment],
    path_start: usize,
    source: ValueCodecError,
) -> ValueCodecError {
    if path.len() == path_start {
        source
    } else {
        wrap_preflight_child_error(path, source)
    }
}

fn wrap_preflight_child_error(
    path: &[CollectionValuePathSegment],
    source: ValueCodecError,
) -> ValueCodecError {
    if is_global_node_limit(&source) || matches!(source, ValueCodecError::ConstructedChild { .. }) {
        source
    } else {
        ValueCodecError::ConstructedChild {
            path: path.to_vec(),
            source: Box::new(source),
        }
    }
}

fn preflight_orv5_child(
    encoded: &[u8],
    nodes: &mut usize,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), ValueCodecError> {
    match preflight_orv5_envelope(encoded, nodes, path) {
        Err(error)
            if is_global_node_limit(&error)
                || matches!(error, ValueCodecError::ConstructedChild { .. }) =>
        {
            Err(error)
        }
        Err(source) => Err(ValueCodecError::ConstructedChild {
            path: path.clone(),
            source: Box::new(source),
        }),
        Ok(()) => Ok(()),
    }
}

fn is_global_node_limit(error: &ValueCodecError) -> bool {
    matches!(
        error,
        ValueCodecError::CollectionValue {
            source: CollectionValueError::TooManyNodes { .. },
        }
    )
}

fn preflight_orv5_envelope(
    encoded: &[u8],
    nodes: &mut usize,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, CONSTRUCTED_MARKER)?;
    if tag == CONSTRUCTED_TAG && type_id.to_bytes() != [0; 16] {
        return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
    }
    preflight_orv5_tree(payload, tag, nodes, path)
}

fn preflight_orv5_record_tree(
    payload: &[u8],
    nodes: &mut usize,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), ValueCodecError> {
    if payload.len() < 4 {
        return Err(ValueCodecError::TruncatedPayload {
            declared: 4,
            actual: payload.len(),
        });
    }
    let count = u32::from_be_bytes(payload[..4].try_into().expect("count length checked")) as usize;
    let mut cursor = 4;
    for ordinal in 0..count {
        let remaining = payload.len() - cursor;
        if remaining < RECORD_FIELD_HEADER_LENGTH {
            return Err(ValueCodecError::TruncatedRecordFieldHeader {
                ordinal,
                actual: remaining,
            });
        }
        let field = FieldId::from_bytes(
            payload[cursor..cursor + 16]
                .try_into()
                .expect("field header length checked"),
        );
        let declared = u32::from_be_bytes(
            payload[cursor + 16..cursor + RECORD_FIELD_HEADER_LENGTH]
                .try_into()
                .expect("field header length checked"),
        ) as usize;
        cursor += RECORD_FIELD_HEADER_LENGTH;
        let remaining = payload.len() - cursor;
        if declared < HEADER_LENGTH || declared > remaining {
            return Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal,
                declared,
                remaining,
            });
        }
        let end = cursor + declared;
        path.push(CollectionValuePathSegment::RecordField(field));
        let result = preflight_orv5_child(&payload[cursor..end], nodes, path);
        path.pop();
        result?;
        cursor = end;
    }
    if cursor != payload.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: payload.len(),
        });
    }
    Ok(())
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
    let checked = RecordValue::new(
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
    if checked != *value {
        return Err(ValueCodecError::RecordValueNotActive {
            record_type: value.record_type(),
        });
    }

    let field_count =
        u32::try_from(definition.fields().len()).map_err(|_| ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&field_count.to_be_bytes());
    for (field, value) in definition.fields().iter().zip(value.fields()) {
        let encoded =
            encode_record_field_value(active, definition.id(), field.descriptor(), value, marker)?;
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
        require_record_field_wire_type(
            active,
            definition_field.descriptor(),
            ordinal,
            tag,
            type_id,
        )?;
        let value = decode_record_field_value(
            active,
            definition_field.descriptor(),
            tag,
            type_id,
            field_payload,
            marker,
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
    expected: &TypeDescriptor,
    ordinal: usize,
    tag: u8,
    actual: TypeId,
) -> Result<(), ValueCodecError> {
    let expected_type = match expected.kind() {
        TypeDescriptorKind::Named(type_id) => type_id,
        TypeDescriptorKind::Reference(_)
        | TypeDescriptorKind::List(_)
        | TypeDescriptorKind::Set(_)
        | TypeDescriptorKind::Map { .. }
        | TypeDescriptorKind::Option(_)
        | TypeDescriptorKind::Stream(_) => {
            return Err(ValueCodecError::WrongRecordFieldType {
                ordinal,
                expected: expected.clone(),
                tag,
                actual,
            });
        }
    };
    let matches = if application_record_field_target(active, expected).is_some() {
        actual == expected_type && tag == RECORD_TAG
    } else {
        active
            .record_value_field_descriptor_runtime_type(expected)
            .is_some_and(|runtime| {
                actual == expected_type
                    && runtime.legacy_scalar().map_or(tag == ENUM_TAG, |scalar| {
                        supported_scalar_tag_from_scalar(scalar) == Some(tag)
                    })
            })
    };
    if matches {
        Ok(())
    } else {
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal,
            expected: expected.clone(),
            tag,
            actual,
        })
    }
}

fn encode_record_field_value(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    declared: &TypeDescriptor,
    value: &RuntimeValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    let TypeDescriptorKind::Named(type_id) = declared.kind() else {
        return Err(ValueCodecError::UnsupportedValue);
    };
    if let Some(expected_record_type) = application_record_field_target(active, declared) {
        let RuntimeValue::Record(value) = value else {
            return Err(ValueCodecError::RecordValueNotActive { record_type });
        };
        if value.record_type() != expected_record_type {
            return Err(ValueCodecError::RecordValueNotActive { record_type });
        }
        return encode_record_value_with_marker(active, value, marker);
    }
    let expected = active
        .record_value_field_descriptor_runtime_type(declared)
        .ok_or(ValueCodecError::UnsupportedValue)?;
    match expected {
        ResolvedType::Scalar(_) => {
            if value.runtime_type() != RuntimeType::Flat(expected) {
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
        ResolvedType::Value(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
    }
}

fn decode_record_field_value(
    active: &ActiveDatabaseRevision,
    declared: &TypeDescriptor,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    if application_record_field_target(active, declared).is_some() {
        return decode_record_value_with_marker(active, type_id, payload, marker);
    }
    let expected = active
        .record_value_field_descriptor_runtime_type(declared)
        .ok_or(ValueCodecError::UnsupportedValue)?;
    match expected {
        ResolvedType::Scalar(scalar) => {
            let canonical_type =
                supported_scalar_type_id(scalar).ok_or(ValueCodecError::UnsupportedValue)?;
            decode_non_enum_value(tag, canonical_type, payload)
        }
        ResolvedType::Named(enum_type) => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_active_enum_value(active, enum_type, label).map(RuntimeValue::Enum)
        }
        ResolvedType::Value(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
    }
}

fn application_record_field_target(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
) -> Option<TypeId> {
    let TypeDescriptorKind::Named(type_id) = descriptor.kind() else {
        return None;
    };
    active
        .catalogue()
        .record_value_type_by_id(type_id)
        .is_some()
        .then_some(type_id)
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
            verify_standard_library_snapshot as verify_core_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, SourceOrigin,
            StoredSourceRevision, StoredSourceUnit,
        },
        types::{ResolvedType, StandardScalar, TypeDescriptor},
        value::{EnumValue, OpaqueValue, RecordValue, RuntimeFloat, RuntimeValue},
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
        active_record_revision_with_second_type(TypeDescriptor::named(ENUM_TYPE))
    }

    fn active_revision_without_standard() -> ActiveDatabaseRevision {
        let schema = SchemaId::from_bytes([0x75; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x76; 16]);
        let catalogue = CatalogueSnapshot::new(
            catalogue_revision,
            vec![SchemaDefinition::new(
                schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            Vec::new(),
        )
        .unwrap();
        let source_unit_id = SourceUnitId::from_bytes([0x77; 16]);
        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "app/schema.orna",
            "a",
            source_unit_content_digest("a").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source_revision = SourceRevisionId::from_bytes([0x78; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x79; 16]),
            source_revision,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x79; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema),
            SourceOrigin::new(source_unit_id, 0, 1).unwrap(),
        )];
        let context = CatalogueHashContext::version_one();
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source_revision, catalogue_revision),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn alternate_verified_standard() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let accepted = retained_standard_library_snapshot().unwrap();
        let alternate = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            accepted.source().clone(),
            "orna.language/2",
            accepted.catalogue().clone(),
            accepted.origins().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([
                0x19, 0x65, 0xe6, 0xcb, 0xeb, 0x68, 0x77, 0xa6, 0xab, 0xea, 0x13, 0x14, 0xe9, 0x12,
                0xbe, 0xc5, 0xef, 0x12, 0xa9, 0x5b, 0xd3, 0x57, 0xdc, 0xee, 0xc9, 0xef, 0xb4, 0x54,
                0xf8, 0x4a, 0x98, 0xb2,
            ]),
        )
        .unwrap();
        verify_core_standard_library_snapshot(alternate).unwrap()
    }

    fn active_revision_with_standard_named_collision() -> ActiveDatabaseRevision {
        let standard =
            verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap())
                .unwrap();
        let schema = SchemaId::from_bytes([0x7a; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x7b; 16]);
        let catalogue = CatalogueSnapshot::new_with_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            Vec::new(),
            vec![ValueTypeDefinition::primitive(
                OPAQUE_TOKEN_TYPE_ID,
                QualifiedSemanticName::new(["crm", "collision"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.crm.value.collision@1",
            )],
            Vec::new(),
        )
        .unwrap();
        let source_unit_id = SourceUnitId::from_bytes([0x7c; 16]);
        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "app/collision.orna",
            "ab",
            source_unit_content_digest("ab").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source_revision = SourceRevisionId::from_bytes([0x7d; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x7e; 16]),
            source_revision,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x7e; 16]),
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
                DefinitionIdentity::ValueType(OPAQUE_TOKEN_TYPE_ID),
                SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
            ),
        ];
        let context = CatalogueHashContext::version_two(standard);
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source_revision, catalogue_revision),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn active_record_revision_with_second_type(
        second_field_type: TypeDescriptor,
    ) -> ActiveDatabaseRevision {
        active_record_revision_with_types(TypeDescriptor::named(BOOLEAN_TYPE_ID), second_field_type)
    }

    fn active_record_revision_with_types(
        first_field_type: TypeDescriptor,
        second_field_type: TypeDescriptor,
    ) -> ActiveDatabaseRevision {
        let standard =
            verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap())
                .unwrap();
        active_record_revision_with_types_and_standard(
            first_field_type,
            second_field_type,
            standard,
        )
    }

    fn active_record_revision_with_types_and_standard(
        first_field_type: TypeDescriptor,
        second_field_type: TypeDescriptor,
        standard: orna_core::revision::VerifiedStandardLibrarySnapshot,
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
                    RecordValueFieldDefinition::try_new_descriptor(
                        record_field,
                        "enabled",
                        0,
                        first_field_type,
                    )
                    .unwrap(),
                    RecordValueFieldDefinition::try_new_descriptor(
                        second_record_field,
                        "verified",
                        1,
                        second_field_type,
                    )
                    .unwrap(),
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

    fn active_nested_record_revision() -> ActiveDatabaseRevision {
        active_nested_record_revision_with_fields(vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "value",
                0,
                TypeDescriptor::named(BOOLEAN_TYPE_ID),
            )
            .unwrap(),
        ])
    }

    fn active_nested_record_revision_with_fields(
        inner_fields: Vec<RecordValueFieldDefinition>,
    ) -> ActiveDatabaseRevision {
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let inner_field_ids = inner_fields
            .iter()
            .map(|field| field.id())
            .collect::<Vec<_>>();
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
            vec![],
            vec![
                RecordValueTypeDefinition::new(
                    inner_type,
                    QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
                    inner_fields,
                ),
                RecordValueTypeDefinition::new(
                    outer_type,
                    QualifiedSemanticName::new(["crm", "outer"]).unwrap(),
                    vec![
                        RecordValueFieldDefinition::try_new_descriptor(
                            outer_field,
                            "payload",
                            0,
                            TypeDescriptor::named(inner_type),
                        )
                        .unwrap(),
                    ],
                ),
            ],
            vec![],
        )
        .unwrap();
        let source_unit_id = SourceUnitId::from_bytes([0x4b; 16]);
        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "app/types.orna",
            "abcdef",
            source_unit_content_digest("abcdef").unwrap(),
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
        let mut identities = vec![
            DefinitionIdentity::Schema(schema),
            DefinitionIdentity::ValueType(inner_type),
        ];
        identities.extend(
            inner_field_ids
                .iter()
                .map(|&field| DefinitionIdentity::Field {
                    owner: inner_type,
                    field,
                }),
        );
        identities.push(DefinitionIdentity::ValueType(outer_type));
        identities.push(DefinitionIdentity::Field {
            owner: outer_type,
            field: outer_field,
        });
        let origins = identities
            .into_iter()
            .enumerate()
            .map(|(index, identity)| {
                DefinitionOrigin::new(
                    identity,
                    SourceOrigin::new(source_unit_id, index as u32, index as u32 + 1).unwrap(),
                )
            })
            .collect::<Vec<_>>();
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
    fn tracer_bullet_active_codec_encodes_nested_immutable_records() {
        let active = active_nested_record_revision();
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let inner = RecordValue::new(
            &active,
            inner_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the inner record must construct");
        let outer = RecordValue::new(
            &active,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(inner))],
        )
        .expect("the outer record must construct");
        let value = RuntimeValue::Record(outer);

        let mut boolean_envelope = b"ORV3".to_vec();
        boolean_envelope.push(0x02);
        boolean_envelope.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        boolean_envelope.extend_from_slice(&1_u32.to_be_bytes());
        boolean_envelope.push(1);

        let mut inner_payload = 1_u32.to_be_bytes().to_vec();
        inner_payload.extend_from_slice(&inner_field.to_bytes());
        inner_payload.extend_from_slice(&(boolean_envelope.len() as u32).to_be_bytes());
        inner_payload.extend_from_slice(&boolean_envelope);
        let mut inner_envelope = b"ORV3".to_vec();
        inner_envelope.push(0x0b);
        inner_envelope.extend_from_slice(&inner_type.to_bytes());
        inner_envelope.extend_from_slice(&(inner_payload.len() as u32).to_be_bytes());
        inner_envelope.extend_from_slice(&inner_payload);

        let mut outer_payload = 1_u32.to_be_bytes().to_vec();
        outer_payload.extend_from_slice(&outer_field.to_bytes());
        outer_payload.extend_from_slice(&(inner_envelope.len() as u32).to_be_bytes());
        outer_payload.extend_from_slice(&inner_envelope);
        let mut expected = b"ORV3".to_vec();
        expected.push(0x0b);
        expected.extend_from_slice(&outer_type.to_bytes());
        expected.extend_from_slice(&(outer_payload.len() as u32).to_be_bytes());
        expected.extend_from_slice(&outer_payload);

        assert_eq!(
            encode_active_value(&active, &value),
            Ok(expected.clone()),
            "the active codec must encode a nested immutable record"
        );
        assert_eq!(
            decode_active_value(&active, &expected),
            Ok(value),
            "the active codec must round-trip a nested immutable record"
        );
    }

    #[test]
    fn tracer_bullet_active_codec_rejects_stale_inner_field_identity() {
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            0,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let field_b = RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3c; 16]),
            "b",
            1,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let old = active_nested_record_revision_with_fields(vec![field_a, field_b]);
        let current = active_nested_record_revision_with_fields(vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                0,
                TypeDescriptor::named(BOOLEAN_TYPE_ID),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                1,
                TypeDescriptor::named(BOOLEAN_TYPE_ID),
            )
            .unwrap(),
        ]);
        let old_child = RecordValue::new(
            &old,
            inner_type,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(false)),
            ],
        )
        .expect("the child must construct under the old revision");

        let value = RuntimeValue::Record(old_child);
        assert_eq!(
            encode_active_value(&current, &value),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: inner_type,
            }),
            "the encoder must reject a stale inner field identity"
        );
        let standard = current.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        assert_eq!(
            encode_registered_value(&current, &registry, &value),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: inner_type,
            }),
            "the registered encoder must reject a stale inner field identity"
        );
    }

    #[test]
    fn stale_replaced_field_identity_fails_both_encoders() {
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            0,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let replaced_a = RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3d; 16]),
            "a",
            0,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let old = active_nested_record_revision_with_fields(vec![field_a]);
        let current = active_nested_record_revision_with_fields(vec![replaced_a]);
        let old_child = RecordValue::new(
            &old,
            inner_type,
            [(String::from("a"), RuntimeValue::Boolean(true))],
        )
        .expect("the child must construct under the old revision");
        let value = RuntimeValue::Record(old_child);
        assert_eq!(
            encode_active_value(&current, &value.clone()),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: inner_type,
            })
        );
        let standard = current.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        assert_eq!(
            encode_registered_value(&current, &registry, &value),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: inner_type,
            })
        );
    }

    fn nested_record_value(active: &ActiveDatabaseRevision) -> RuntimeValue {
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let inner = RecordValue::new(
            active,
            inner_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .unwrap();
        let outer = RecordValue::new(
            active,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(inner))],
        )
        .unwrap();
        RuntimeValue::Record(outer)
    }

    fn nested_envelope(marker: &[u8; 4], tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
        let mut bytes = marker.to_vec();
        bytes.push(tag);
        bytes.extend_from_slice(&type_id.to_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn nested_record_payload(fields: &[(FieldId, Vec<u8>)]) -> Vec<u8> {
        let mut payload = (fields.len() as u32).to_be_bytes().to_vec();
        for (field, encoded) in fields {
            payload.extend_from_slice(&field.to_bytes());
            payload.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
            payload.extend_from_slice(encoded);
        }
        payload
    }

    fn assemble_nested_envelope(
        marker: &[u8; 4],
        inner_tag: u8,
        inner_type: TypeId,
        inner_field: FieldId,
        inner_count: u32,
        inner_field_length: u32,
        inner_extra_payload: &[u8],
    ) -> Vec<u8> {
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let boolean = nested_envelope(marker, 0x02, BOOLEAN_TYPE_ID, &[1]);
        let mut inner_payload = inner_count.to_be_bytes().to_vec();
        inner_payload.extend_from_slice(&inner_field.to_bytes());
        inner_payload.extend_from_slice(&inner_field_length.to_be_bytes());
        inner_payload.extend_from_slice(&boolean);
        inner_payload.extend_from_slice(inner_extra_payload);
        let inner = nested_envelope(marker, inner_tag, inner_type, &inner_payload);
        let outer_payload = nested_record_payload(&[(outer_field, inner)]);
        nested_envelope(marker, 0x0b, outer_type, &outer_payload)
    }

    #[test]
    fn registered_codec_has_exact_nested_record_bytes_and_round_trips() {
        let active = active_nested_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let value = nested_record_value(&active);
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        let expected = assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);

        assert_eq!(
            encode_registered_value(&active, &registry, &value),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_registered_value(&active, &registry, &expected),
            Ok(value.clone())
        );
        assert_eq!(&expected[0..4], b"ORV4", "the outer marker must be ORV4");

        assert_eq!(
            encode_value(&value),
            Err(ValueCodecError::UnsupportedValue),
            "version one must stay closed for nested records"
        );
        assert_eq!(
            encode_catalogue_value(active.catalogue(), &value),
            Err(ValueCodecError::UnsupportedValue),
            "version two must stay closed for nested records"
        );
        assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
        assert_eq!(
            decode_catalogue_value(active.catalogue(), &expected),
            Err(ValueCodecError::InvalidMarker)
        );
    }

    #[test]
    fn nested_codec_rejects_inner_marker_crossing() {
        let active = active_nested_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let value = nested_record_value(&active);
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        let inner_offset = 25 + 4 + 16 + 4;

        let active_bytes =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[]);
        assert_eq!(
            encode_active_value(&active, &value),
            Ok(active_bytes.clone())
        );
        let mut wrong_active = active_bytes.clone();
        wrong_active[inner_offset..inner_offset + 4].copy_from_slice(b"ORV4");
        assert_eq!(
            decode_active_value(&active, &wrong_active),
            Err(ValueCodecError::InvalidMarker),
            "an ORV4 inner envelope must be rejected by the ORV3 decoder"
        );

        let registered_bytes =
            assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
        let mut wrong_registered = registered_bytes.clone();
        wrong_registered[inner_offset..inner_offset + 4].copy_from_slice(b"ORV3");
        assert_eq!(
            decode_registered_value(&active, &registry, &wrong_registered),
            Err(ValueCodecError::InvalidMarker),
            "an ORV3 inner envelope must be rejected by the ORV4 decoder"
        );
    }

    #[test]
    fn nested_codec_rejects_wrong_inner_tag_and_type() {
        let active = active_nested_record_revision();
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        for inner_tag in [0x0a, 0x02] {
            let corrupted =
                assemble_nested_envelope(b"ORV3", inner_tag, inner_type, inner_field, 1, 26, &[]);
            assert_eq!(
                decode_active_value(&active, &corrupted),
                Err(ValueCodecError::WrongRecordFieldType {
                    ordinal: 0,
                    expected: TypeDescriptor::named(inner_type),
                    tag: inner_tag,
                    actual: inner_type,
                })
            );
        }
        let replaced_type = TypeId::from_bytes([0x99; 16]);
        let corrupted =
            assemble_nested_envelope(b"ORV3", 0x0b, replaced_type, inner_field, 1, 26, &[]);
        assert_eq!(
            decode_active_value(&active, &corrupted),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: TypeDescriptor::named(inner_type),
                tag: 0x0b,
                actual: replaced_type,
            })
        );
    }

    #[test]
    fn nested_codec_rejects_inner_structure_corruption() {
        let active = active_nested_record_revision();
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);

        let wrong_count =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 2, 26, &[]);
        assert_eq!(
            decode_active_value(&active, &wrong_count),
            Err(ValueCodecError::WrongRecordFieldCount {
                expected: 1,
                actual: 2,
            })
        );

        let wrong_field = FieldId::from_bytes([0x99; 16]);
        let wrong_identity =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, wrong_field, 1, 26, &[]);
        assert_eq!(
            decode_active_value(&active, &wrong_identity),
            Err(ValueCodecError::WrongRecordFieldIdentity {
                ordinal: 0,
                expected: inner_field,
                actual: wrong_field,
            })
        );

        let wrong_length =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 10, &[]);
        assert_eq!(
            decode_active_value(&active, &wrong_length),
            Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal: 0,
                declared: 10,
                remaining: 26,
            })
        );

        let trailing =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[0xaa, 0xbb]);
        assert_eq!(
            decode_active_value(&active, &trailing),
            Err(ValueCodecError::TrailingBytes {
                declared: 50,
                actual: 52,
            })
        );
    }

    #[test]
    fn nested_codec_checks_inner_payload_limit_before_truncation() {
        let active = active_nested_record_revision();
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let mut inner_header = b"ORV3".to_vec();
        inner_header.push(0x0b);
        inner_header.extend_from_slice(&inner_type.to_bytes());
        inner_header.extend_from_slice(&((PAYLOAD_LIMIT as u32) + 1).to_be_bytes());
        assert_eq!(
            inner_header.len(),
            25,
            "inner envelope must carry no payload"
        );

        let mut outer_payload = 1_u32.to_be_bytes().to_vec();
        outer_payload.extend_from_slice(&outer_field.to_bytes());
        outer_payload.extend_from_slice(&(inner_header.len() as u32).to_be_bytes());
        outer_payload.extend_from_slice(&inner_header);
        let mut expected = b"ORV3".to_vec();
        expected.push(0x0b);
        expected.extend_from_slice(&outer_type.to_bytes());
        expected.extend_from_slice(&(outer_payload.len() as u32).to_be_bytes());
        expected.extend_from_slice(&outer_payload);

        assert_eq!(
            decode_active_value(&active, &expected),
            Err(ValueCodecError::PayloadTooLarge {
                actual: PAYLOAD_LIMIT + 1,
                maximum: PAYLOAD_LIMIT,
            }),
            "the declared inner payload limit must be checked before truncation"
        );
    }

    #[test]
    fn nested_record_values_delegate_unchanged_through_frames() {
        let active = active_nested_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let value = nested_record_value(&active);
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        let parameter = ParameterId::from_bytes([0x5f; 16]);
        let active_envelope =
            assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[]);
        let registered_envelope =
            assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
        assert_eq!(active_envelope.len(), 124);
        assert_eq!(registered_envelope.len(), 124);

        let argument = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: value.clone(),
        };
        let mut expected_argument = b"ORF3\x02\0".to_vec();
        expected_argument.extend_from_slice(&7_u64.to_be_bytes());
        expected_argument.extend_from_slice(&140_u32.to_be_bytes());
        expected_argument.extend_from_slice(&parameter.to_bytes());
        expected_argument.extend_from_slice(&active_envelope);
        assert_eq!(
            encode_active_client_frame(&active, &argument),
            Ok(expected_argument.clone())
        );
        assert_eq!(
            decode_active_client_frame(&active, &expected_argument),
            Ok(argument)
        );
        assert_eq!(
            decode_client_frame(&expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_client_frame(active.catalogue(), &expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );

        let event_batch = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value.clone()),
            }],
        };
        let mut expected_batch = b"ORF3\x82\0".to_vec();
        expected_batch.extend_from_slice(&7_u64.to_be_bytes());
        expected_batch.extend_from_slice(&140_u32.to_be_bytes());
        expected_batch.push(0x01);
        expected_batch.extend_from_slice(&1_u16.to_be_bytes());
        expected_batch.extend_from_slice(&1_u64.to_be_bytes());
        expected_batch.push(0x01);
        expected_batch.extend_from_slice(&124_u32.to_be_bytes());
        expected_batch.extend_from_slice(&active_envelope);
        assert_eq!(
            encode_active_server_frame(&active, &event_batch),
            Ok(expected_batch.clone())
        );
        assert_eq!(
            decode_active_server_frame(&active, &expected_batch),
            Ok(event_batch)
        );

        let registered_batch = ServerFrame::EventBatch {
            stream: 8,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        };
        let mut expected_registered = b"ORF4\x82\0".to_vec();
        expected_registered.extend_from_slice(&8_u64.to_be_bytes());
        expected_registered.extend_from_slice(&140_u32.to_be_bytes());
        expected_registered.push(0x01);
        expected_registered.extend_from_slice(&1_u16.to_be_bytes());
        expected_registered.extend_from_slice(&1_u64.to_be_bytes());
        expected_registered.push(0x01);
        expected_registered.extend_from_slice(&124_u32.to_be_bytes());
        expected_registered.extend_from_slice(&registered_envelope);
        assert_eq!(
            encode_registered_server_frame(&active, &registry, &registered_batch),
            Ok(expected_registered.clone())
        );
        assert_eq!(
            decode_registered_server_frame(&active, &registry, &expected_registered),
            Ok(registered_batch)
        );
        assert_eq!(
            decode_active_server_frame(&active, &expected_registered),
            Err(FrameCodecError::InvalidMarker),
            "the ORV4 frame must be rejected by the active frame decoder"
        );
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
        let changed =
            active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));

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

        let mut record_tag_in_scalar_field = encoded.clone();
        record_tag_in_scalar_field[53] = 0x0b;
        assert_eq!(
            decode_active_value(&active, &record_tag_in_scalar_field),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
                tag: 0x0b,
                actual: BOOLEAN_TYPE_ID,
            })
        );

        let mut wrong_field_type = encoded.clone();
        wrong_field_type[53] = 0x06;
        wrong_field_type[54..70].copy_from_slice(&CHARACTER_LARGE_OBJECT_TYPE_ID.to_bytes());
        assert_eq!(
            decode_active_value(&active, &wrong_field_type),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
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
                expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
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
                expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
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
            TypeDescriptor::named(BINARY_LARGE_OBJECT_TYPE_ID),
            TypeDescriptor::named(ENUM_TYPE),
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
        let changed =
            active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));

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
        fn arbitrary_version_five_constructed_bytes_never_panic(
            descriptor in prop::collection::vec(any::<u8>(), 0..=4_096),
            body in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let registry = registered_opaque_codecs(
                active.catalogue_hash_context().standard().unwrap(),
            ).unwrap();
            let mut payload = (descriptor.len() as u16).to_be_bytes().to_vec();
            payload.extend_from_slice(&descriptor);
            payload.extend_from_slice(&body);
            let _ = decode_constructed_value(&active, &registry, &orv5_constructed(payload));
        }

        #[test]
        fn arbitrary_version_five_untrusted_envelopes_never_panic(
            tag in any::<u8>(),
            type_bytes in any::<[u8; 16]>(),
            declared in any::<u32>(),
            payload in prop::collection::vec(any::<u8>(), 0..=4_096),
        ) {
            let active = active_record_revision();
            let registry = registered_opaque_codecs(
                active.catalogue_hash_context().standard().unwrap(),
            ).unwrap();
            let mut encoded = b"ORV5".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&type_bytes);
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_constructed_value(&active, &registry, &encoded);
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

        #[test]
        fn arbitrary_version_five_frame_envelopes_never_panic(
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
            let mut encoded = b"ORF5".to_vec();
            encoded.push(tag);
            encoded.push(flags);
            encoded.extend_from_slice(&stream.to_be_bytes());
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_constructed_client_frame(&active, &registry, &encoded);
            let _ = decode_constructed_server_frame(&active, &registry, &encoded);
        }
    }

    fn constructed_collection_values(active: &ActiveDatabaseRevision) -> Vec<RuntimeValue> {
        let option = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let list = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let map = TypeDescriptor::map(
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        vec![
            RuntimeValue::option(active, option, Some(RuntimeValue::Boolean(true))).unwrap(),
            RuntimeValue::list(active, list, vec![RuntimeValue::Boolean(true)]).unwrap(),
            RuntimeValue::map(
                active,
                map,
                vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(true))],
            )
            .unwrap(),
        ]
    }

    #[test]
    fn constructed_collection_values_stay_closed_to_the_legacy_orv_encoders() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        for value in constructed_collection_values(&active) {
            assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
            assert_eq!(
                encode_catalogue_value(active.catalogue(), &value),
                Err(ValueCodecError::UnsupportedValue)
            );
            assert_eq!(
                encode_active_value(&active, &value),
                Err(ValueCodecError::UnsupportedValue)
            );
            assert_eq!(
                encode_registered_value(&active, &registry, &value),
                Err(ValueCodecError::UnsupportedValue)
            );
        }
    }

    #[test]
    fn orv5_round_trips_a_checked_option_with_independent_exact_bytes() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let value = RuntimeValue::option(
            &active,
            descriptor.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();

        let mut expected = b"ORV5".to_vec();
        expected.push(0x0d);
        expected.extend_from_slice(&[0; 16]);
        expected.extend_from_slice(&51_u32.to_be_bytes());
        expected.extend_from_slice(&18_u16.to_be_bytes());
        expected.push(0x04);
        expected.push(0x00);
        expected.extend_from_slice(&[0; 15]);
        expected.push(0x01);
        expected.push(0x01);
        expected.extend_from_slice(&26_u32.to_be_bytes());
        expected.extend_from_slice(b"ORV5");
        expected.push(0x02);
        expected.extend_from_slice(&[0; 15]);
        expected.push(0x01);
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.push(0x01);

        let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
        assert_eq!(encoded, expected);

        let decoded = decode_constructed_value(&active, &registry, &encoded).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn orv5_round_trips_all_admitted_constructors_and_rejects_hostile_option_bytes() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        for value in constructed_collection_values(&active) {
            let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
            assert_eq!(&encoded[..4], b"ORV5");
            assert_eq!(
                decode_constructed_value(&active, &registry, &encoded),
                Ok(value)
            );
        }

        let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let option =
            RuntimeValue::option(&active, descriptor, Some(RuntimeValue::Boolean(true))).unwrap();
        let encoded = encode_constructed_value(&active, &registry, &option).unwrap();

        let mut identity = encoded.clone();
        identity[5] = 1;
        assert_eq!(
            decode_constructed_value(&active, &registry, &identity),
            Err(ValueCodecError::ConstructedTypeIdentityNotZero {
                identity: TypeId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            })
        );

        let mut descriptor_tag = encoded.clone();
        descriptor_tag[27] = 0xff;
        assert_eq!(
            decode_constructed_value(&active, &registry, &descriptor_tag),
            Err(ValueCodecError::UnknownConstructedDescriptorTag { tag: 0xff })
        );

        let mut presence = encoded.clone();
        presence[45] = 2;
        assert_eq!(
            decode_constructed_value(&active, &registry, &presence),
            Err(ValueCodecError::InvalidOptionPresence { value: 2 })
        );

        let mut child_marker = encoded;
        child_marker[50..54].copy_from_slice(b"ORV4");
        assert_eq!(
            decode_constructed_value(&active, &registry, &child_marker),
            Err(ValueCodecError::ConstructedChild {
                path: vec![CollectionValuePathSegment::OptionChild],
                source: Box::new(ValueCodecError::InvalidMarker),
            })
        );
    }

    #[test]
    fn orv5_admits_the_descriptor_before_the_body_and_wraps_nested_option_body_errors() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        let mut inactive_payload = Vec::new();
        inactive_payload.extend_from_slice(&18_u16.to_be_bytes());
        inactive_payload.push(0x04);
        inactive_payload.push(0x00);
        inactive_payload.extend_from_slice(&[0xfe; 16]);
        inactive_payload.push(0x02);
        let inactive = orv5_constructed(inactive_payload);
        let error = decode_constructed_value(&active, &registry, &inactive).unwrap_err();
        assert!(matches!(
            error,
            ValueCodecError::CollectionValue {
                source: CollectionValueError::UnsupportedDescriptor { .. },
            }
        ));

        let mut inner_payload = Vec::new();
        inner_payload.extend_from_slice(&18_u16.to_be_bytes());
        inner_payload.push(0x04);
        inner_payload.push(0x00);
        inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        inner_payload.push(0x02);
        let inner = orv5_constructed(inner_payload);

        let mut outer_payload = Vec::new();
        outer_payload.extend_from_slice(&19_u16.to_be_bytes());
        outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
        outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        outer_payload.push(0x01);
        outer_payload.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        outer_payload.extend_from_slice(&inner);
        let outer = orv5_constructed(outer_payload);
        assert_eq!(
            decode_constructed_value(&active, &registry, &outer),
            Err(ValueCodecError::ConstructedChild {
                path: vec![CollectionValuePathSegment::OptionChild],
                source: Box::new(ValueCodecError::InvalidOptionPresence { value: 2 }),
            })
        );

        let mut valid_inner_payload = Vec::new();
        valid_inner_payload.extend_from_slice(&18_u16.to_be_bytes());
        valid_inner_payload.push(0x04);
        valid_inner_payload.push(0x00);
        valid_inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        valid_inner_payload.push(0x00);
        let valid_inner = orv5_constructed(valid_inner_payload);
        let mut trailing_outer_payload = Vec::new();
        trailing_outer_payload.extend_from_slice(&19_u16.to_be_bytes());
        trailing_outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
        trailing_outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        trailing_outer_payload.push(0x01);
        trailing_outer_payload.extend_from_slice(&(valid_inner.len() as u32).to_be_bytes());
        trailing_outer_payload.extend_from_slice(&valid_inner);
        trailing_outer_payload.push(0xff);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(trailing_outer_payload),
            ),
            Err(ValueCodecError::TrailingBytes {
                declared: 51,
                actual: 52,
            })
        );

        let boolean = orv5_boolean(true);
        let mut trailing_inner_payload = Vec::new();
        trailing_inner_payload.extend_from_slice(&18_u16.to_be_bytes());
        trailing_inner_payload.push(0x04);
        trailing_inner_payload.push(0x00);
        trailing_inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        trailing_inner_payload.push(0x01);
        trailing_inner_payload.extend_from_slice(&(boolean.len() as u32).to_be_bytes());
        trailing_inner_payload.extend_from_slice(&boolean);
        trailing_inner_payload.push(0xff);
        let trailing_inner = orv5_constructed(trailing_inner_payload);
        let mut contained_outer_payload = Vec::new();
        contained_outer_payload.extend_from_slice(&19_u16.to_be_bytes());
        contained_outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
        contained_outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        contained_outer_payload.push(0x01);
        contained_outer_payload.extend_from_slice(&(trailing_inner.len() as u32).to_be_bytes());
        contained_outer_payload.extend_from_slice(&trailing_inner);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(contained_outer_payload),
            ),
            Err(ValueCodecError::ConstructedChild {
                path: vec![CollectionValuePathSegment::OptionChild],
                source: Box::new(ValueCodecError::TrailingBytes {
                    declared: 31,
                    actual: 32,
                }),
            })
        );
    }

    #[test]
    fn orv5_public_tracers_retain_empty_nested_and_registered_values() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let option = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let list = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let map = TypeDescriptor::map(
            TypeDescriptor::named(INTEGER_TYPE_ID),
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let nested = TypeDescriptor::list(option.clone()).unwrap();
        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
        );
        let values = vec![
            RuntimeValue::option(&active, option.clone(), None).unwrap(),
            RuntimeValue::list(&active, list, Vec::new()).unwrap(),
            RuntimeValue::map(&active, map, Vec::new()).unwrap(),
            RuntimeValue::list(
                &active,
                nested,
                vec![
                    RuntimeValue::option(&active, option.clone(), None).unwrap(),
                    RuntimeValue::option(&active, option, Some(RuntimeValue::Boolean(true)))
                        .unwrap(),
                ],
            )
            .unwrap(),
            RuntimeValue::Integer(-7),
            opaque,
        ];
        for value in values {
            let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
            assert_eq!(
                decode_constructed_value(&active, &registry, &encoded),
                Ok(value)
            );
        }
    }

    #[test]
    fn orv5_has_independent_list_and_map_goldens_and_rejects_noncanonical_map_order() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let list = RuntimeValue::list(
            &active,
            list_descriptor,
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
        )
        .unwrap();
        let mut expected_list = b"ORV5".to_vec();
        expected_list.push(0x0d);
        expected_list.extend_from_slice(&[0; 16]);
        expected_list.extend_from_slice(&84_u32.to_be_bytes());
        expected_list.extend_from_slice(&18_u16.to_be_bytes());
        expected_list.extend_from_slice(&[0x02, 0x00]);
        expected_list.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        expected_list.extend_from_slice(&2_u32.to_be_bytes());
        for value in [true, false] {
            let child = orv5_boolean(value);
            expected_list.extend_from_slice(&(child.len() as u32).to_be_bytes());
            expected_list.extend_from_slice(&child);
        }
        assert_eq!(
            encode_constructed_value(&active, &registry, &list),
            Ok(expected_list.clone())
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &expected_list),
            Ok(list)
        );

        let map_descriptor = TypeDescriptor::map(
            TypeDescriptor::named(INTEGER_TYPE_ID),
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let map = RuntimeValue::map(
            &active,
            map_descriptor,
            vec![
                (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
                (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();
        let first = orv5_map_entry(orv5_integer(1), orv5_boolean(true));
        let second = orv5_map_entry(orv5_integer(2), orv5_boolean(false));
        let mut expected_map = b"ORV5".to_vec();
        expected_map.push(0x0d);
        expected_map.extend_from_slice(&[0; 16]);
        expected_map.extend_from_slice(&167_u32.to_be_bytes());
        expected_map.extend_from_slice(&35_u16.to_be_bytes());
        expected_map.push(0x03);
        expected_map.push(0x00);
        expected_map.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
        expected_map.push(0x00);
        expected_map.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        expected_map.extend_from_slice(&2_u32.to_be_bytes());
        expected_map.extend_from_slice(&first);
        expected_map.extend_from_slice(&second);
        assert_eq!(
            encode_constructed_value(&active, &registry, &map),
            Ok(expected_map.clone())
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &expected_map),
            Ok(map)
        );

        let mut noncanonical =
            expected_map[..expected_map.len() - first.len() - second.len()].to_vec();
        noncanonical.extend_from_slice(&second);
        noncanonical.extend_from_slice(&first);
        assert_eq!(
            decode_constructed_value(&active, &registry, &noncanonical),
            Err(ValueCodecError::NonCanonicalMapOrder { index: 0 })
        );
    }

    #[test]
    fn orv5_enforces_descriptor_and_value_node_limits_before_later_body_failures() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        let mut deep_payload = Vec::new();
        deep_payload.extend_from_slice(&50_u16.to_be_bytes());
        deep_payload.extend(std::iter::repeat_n(0x04, 33));
        deep_payload.push(0x00);
        deep_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        deep_payload.push(0x00);
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(deep_payload)),
            Err(ValueCodecError::InvalidConstructedDescriptor {
                source: TypeDescriptorError::TooDeep {
                    maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                    actual: MAX_TYPE_DESCRIPTOR_DEPTH + 1,
                },
            })
        );

        let leaf = orv5_boolean(false);
        let mut at_limit_payload = orv5_boolean_list_prefix(65_535);
        for _ in 0..65_535 {
            at_limit_payload.extend_from_slice(&(leaf.len() as u32).to_be_bytes());
            at_limit_payload.extend_from_slice(&leaf);
        }
        assert!(
            decode_constructed_value(&active, &registry, &orv5_constructed(at_limit_payload),)
                .is_ok()
        );

        let mut over_limit_payload = orv5_boolean_list_prefix(65_537);
        for _ in 0..65_536 {
            over_limit_payload.extend_from_slice(&(leaf.len() as u32).to_be_bytes());
            over_limit_payload.extend_from_slice(&leaf);
        }
        over_limit_payload.extend_from_slice(&25_u32.to_be_bytes());
        over_limit_payload.extend_from_slice(b"ORV5");
        over_limit_payload.push(0x02);
        over_limit_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        over_limit_payload.extend_from_slice(&1_u32.to_be_bytes());
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(over_limit_payload),),
            Err(ValueCodecError::CollectionValue {
                source: CollectionValueError::TooManyNodes {
                    maximum: MAX_RUNTIME_VALUE_NODES,
                },
            })
        );
    }

    #[test]
    fn orv5_reports_each_constructed_structure_failure_exactly() {
        let active = active_record_revision();
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(Vec::new())),
            Err(ValueCodecError::TruncatedConstructedHeader { actual: 0 })
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(vec![0])),
            Err(ValueCodecError::TruncatedConstructedHeader { actual: 1 })
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 0])),
            Err(ValueCodecError::EmptyConstructedDescriptor)
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 3, 0x04])),
            Err(ValueCodecError::TruncatedConstructedDescriptor {
                declared: 3,
                available: 1,
            })
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 1, 0x00])),
            Err(ValueCodecError::TruncatedConstructedDescriptorNode {
                offset: 0,
                required: 17,
                available: 1,
            })
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 1, 0x04])),
            Err(ValueCodecError::TruncatedConstructedDescriptorNode {
                offset: 1,
                required: 1,
                available: 0,
            })
        );
        let mut trailing_descriptor = orv5_named_descriptor(BOOLEAN_TYPE_ID);
        trailing_descriptor.push(0xff);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&trailing_descriptor, &[])),
            ),
            Err(ValueCodecError::TrailingConstructedDescriptor { remaining: 1 })
        );

        let mut list_descriptor = vec![0x02];
        list_descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
        let mut map_descriptor = vec![0x03];
        map_descriptor.extend_from_slice(&orv5_named_descriptor(INTEGER_TYPE_ID));
        map_descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
        for (descriptor, child_path) in [
            (
                list_descriptor.as_slice(),
                vec![CollectionValuePathSegment::ListElement(0)],
            ),
            (
                map_descriptor.as_slice(),
                vec![CollectionValuePathSegment::MapKey(0)],
            ),
        ] {
            assert_eq!(
                decode_constructed_value(
                    &active,
                    &registry,
                    &orv5_constructed(orv5_descriptor_payload(descriptor, &[])),
                ),
                Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() })
            );
            let mut truncated_header = 1_u32.to_be_bytes().to_vec();
            truncated_header.extend_from_slice(&[0; 3]);
            assert_eq!(
                decode_constructed_value(
                    &active,
                    &registry,
                    &orv5_constructed(orv5_descriptor_payload(descriptor, &truncated_header)),
                ),
                Err(ValueCodecError::TruncatedCollectionEntry {
                    path: child_path.clone(),
                })
            );
            let mut truncated_region = 1_u32.to_be_bytes().to_vec();
            truncated_region.extend_from_slice(&26_u32.to_be_bytes());
            truncated_region.extend_from_slice(&[0; 25]);
            assert_eq!(
                decode_constructed_value(
                    &active,
                    &registry,
                    &orv5_constructed(orv5_descriptor_payload(descriptor, &truncated_region)),
                ),
                Err(ValueCodecError::TruncatedCollectionEntry { path: child_path })
            );
        }

        let mut map_value_header = 1_u32.to_be_bytes().to_vec();
        map_value_header.extend_from_slice(&orv5_map_entry(orv5_integer(1), Vec::new()));
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&map_descriptor, &map_value_header)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: vec![CollectionValuePathSegment::MapValue(0)],
            })
        );
        let mut map_value_region = 1_u32.to_be_bytes().to_vec();
        map_value_region.extend_from_slice(&(orv5_integer(1).len() as u32).to_be_bytes());
        map_value_region.extend_from_slice(&orv5_integer(1));
        map_value_region.extend_from_slice(&26_u32.to_be_bytes());
        map_value_region.extend_from_slice(&[0; 25]);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&map_descriptor, &map_value_region)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: vec![CollectionValuePathSegment::MapValue(0)],
            })
        );

        let mut short_child = 1_u32.to_be_bytes().to_vec();
        short_child.extend_from_slice(&24_u32.to_be_bytes());
        short_child.extend_from_slice(&[0; 24]);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &short_child)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: vec![CollectionValuePathSegment::ListElement(0)],
            })
        );
        let mut maximum_child = 1_u32.to_be_bytes().to_vec();
        maximum_child.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &maximum_child)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: vec![CollectionValuePathSegment::ListElement(0)],
            })
        );

        let maximum_count = u32::MAX.to_be_bytes().to_vec();
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &maximum_count)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: vec![CollectionValuePathSegment::ListElement(0)],
            })
        );
        let mut oversized_header = b"ORV5".to_vec();
        oversized_header.push(0x0d);
        oversized_header.extend_from_slice(&[0; 16]);
        oversized_header.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode_constructed_value(&active, &registry, &oversized_header),
            Err(ValueCodecError::PayloadTooLarge {
                actual: u32::MAX as usize,
                maximum: PAYLOAD_LIMIT,
            })
        );
    }

    #[test]
    fn orv5_marker_substitution_covers_every_accepted_orv4_value_family() {
        let active = active_record_revision();
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
        let reference_target = TypeId::from_bytes([0x41; 16]);
        let values = vec![
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(-7),
            RuntimeValue::BigInt(-9),
            RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
            RuntimeValue::Text(String::from("literal ORV4 text payload")),
            RuntimeValue::Bytes(b"literal ORV4 byte payload".to_vec()),
            RuntimeValue::null(ResolvedType::reference(reference_target)).unwrap(),
            RuntimeValue::Reference {
                target: reference_target,
                object: ObjectId::from_bytes([0x42; 16]),
            },
            RuntimeValue::null(ResolvedType::named(ENUM_TYPE)).unwrap(),
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap()),
            RuntimeValue::Opaque(
                OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
            ),
        ];
        for value in values {
            assert_orv4_to_orv5_flat_marker_substitution(&active, &registry, value);
        }

        let nested_active = active_nested_record_revision();
        let nested_registry =
            registered_opaque_codecs(nested_active.catalogue_hash_context().standard().unwrap())
                .unwrap();
        let nested_value = nested_record_value(&nested_active);
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let inner_field = FieldId::from_bytes([0x3a; 16]);
        let version_four =
            assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
        let expected = assemble_nested_envelope(b"ORV5", 0x0b, inner_type, inner_field, 1, 26, &[]);
        assert_eq!(
            encode_registered_value(&nested_active, &nested_registry, &nested_value),
            Ok(version_four)
        );
        assert_eq!(
            encode_constructed_value(&nested_active, &nested_registry, &nested_value),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_constructed_value(&nested_active, &nested_registry, &expected),
            Ok(nested_value)
        );
    }

    #[test]
    fn orv5_rechecks_stale_enum_reference_standard_and_opaque_authorities() {
        let active = active_record_revision();
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();

        let enum_value =
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());
        let mut stale_enum = encode_constructed_value(&active, &registry, &enum_value).unwrap();
        stale_enum[25..34].copy_from_slice(b"obsolete!");
        assert_eq!(
            decode_constructed_value(&active, &registry, &stale_enum),
            Err(ValueCodecError::UndeclaredEnumLabel {
                enum_type: ENUM_TYPE,
                label: String::from("obsolete!"),
            })
        );

        let stale_reference_target = TypeId::from_bytes([0x74; 16]);
        let mut reference_descriptor = vec![0x04, 0x01];
        reference_descriptor.extend_from_slice(&stale_reference_target.to_bytes());
        let reference_error = decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&reference_descriptor, &[0])),
        )
        .unwrap_err();
        let ValueCodecError::CollectionValue {
            source: CollectionValueError::UnsupportedDescriptor { path, descriptor },
        } = reference_error
        else {
            panic!("a stale reference target must fail collection admission");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
        assert_eq!(
            descriptor,
            TypeDescriptor::reference(stale_reference_target)
        );

        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
        );
        let encoded_opaque = encode_constructed_value(&active, &registry, &opaque).unwrap();
        assert_eq!(
            decode_constructed_value(
                &active_revision_without_standard(),
                &registry,
                &encoded_opaque
            ),
            Err(ValueCodecError::OpaqueValue {
                source: OpaqueValueError::ActiveStandardRequired,
            })
        );
        let alternate_active = active_record_revision_with_types_and_standard(
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
            TypeDescriptor::named(ENUM_TYPE),
            alternate_verified_standard(),
        );
        assert_eq!(
            decode_constructed_value(&alternate_active, &registry, &encoded_opaque),
            Err(ValueCodecError::OpaqueValue {
                source: OpaqueValueError::ActiveStandardMismatch,
            })
        );
        let invalid_registration =
            orna_core::value::OpaqueCodecRegistration::fixed_length_identity(
                OPAQUE_TOKEN_TYPE_ID,
                QualifiedSemanticName::new(["std", "types", "opaque_token"]).unwrap(),
                "orna.std.value.opaque-token@2",
                16,
            )
            .unwrap();
        assert!(matches!(
            OpaqueCodecRegistry::new(
                active.catalogue_hash_context().standard().unwrap(),
                [invalid_registration],
            ),
            Err(
                orna_core::value::OpaqueCodecRegistryError::ContractMismatch {
                    opaque_type: OPAQUE_TOKEN_TYPE_ID,
                }
            )
        ));
        let mut wrong_contract = encoded_opaque;
        wrong_contract[21..25].copy_from_slice(&15_u32.to_be_bytes());
        wrong_contract.pop();
        assert_eq!(
            decode_constructed_value(&active, &registry, &wrong_contract),
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
    fn orv5_cross_catalogue_collision_precedes_opaque_category_rejection() {
        let active = active_revision_with_standard_named_collision();
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
        let mut descriptor = vec![0x02];
        descriptor.extend_from_slice(&orv5_named_descriptor(OPAQUE_TOKEN_TYPE_ID));
        let error = decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&descriptor, &0_u32.to_be_bytes())),
        )
        .unwrap_err();
        let ValueCodecError::CollectionValue {
            source: CollectionValueError::AmbiguousNamedType { path, type_id },
        } = error
        else {
            panic!("cross-catalogue identity collision must precede opaque rejection");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(type_id, OPAQUE_TOKEN_TYPE_ID);
    }

    #[test]
    fn orv5_map_permutations_encode_to_the_same_canonical_bytes() {
        let active = active_record_revision();
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
        let descriptor = TypeDescriptor::map(
            TypeDescriptor::named(INTEGER_TYPE_ID),
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap();
        let canonical = RuntimeValue::map(
            &active,
            descriptor.clone(),
            vec![
                (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
                (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap();
        let permuted = RuntimeValue::map(
            &active,
            descriptor,
            vec![
                (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
                (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();
        assert_eq!(
            encode_constructed_value(&active, &registry, &canonical),
            encode_constructed_value(&active, &registry, &permuted)
        );
    }

    #[test]
    fn orv5_retains_legacy_bytes_and_keeps_markers_closed() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        let legacy = RuntimeValue::Boolean(true);
        let version_four = encode_registered_value(&active, &registry, &legacy).unwrap();
        let version_five = encode_constructed_value(&active, &registry, &legacy).unwrap();
        assert_eq!(&version_four[..4], b"ORV4");
        assert_eq!(&version_five[..4], b"ORV5");
        assert_eq!(&version_five[4..], &version_four[4..]);
        assert_eq!(
            decode_constructed_value(&active, &registry, &version_five),
            Ok(legacy)
        );

        assert_eq!(
            decode_constructed_value(&active, &registry, &version_four),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_registered_value(&active, &registry, &version_five),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_value(&version_five),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_value(active.catalogue(), &version_five),
            Err(ValueCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_value(&active, &version_five),
            Err(ValueCodecError::InvalidMarker)
        );
        for marker in [b"ORV1", b"ORV2", b"ORV3", b"ORV4"] {
            let mut crossed = version_five.clone();
            crossed[..4].copy_from_slice(marker);
            assert_eq!(
                decode_constructed_value(&active, &registry, &crossed),
                Err(ValueCodecError::InvalidMarker)
            );
        }

        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
        );
        let opaque_version_four = encode_registered_value(&active, &registry, &opaque).unwrap();
        let opaque_version_five = encode_constructed_value(&active, &registry, &opaque).unwrap();
        assert_eq!(&opaque_version_five[4..], &opaque_version_four[4..]);
        assert_eq!(
            decode_constructed_value(&active, &registry, &opaque_version_five),
            Ok(opaque)
        );
    }

    #[test]
    fn orv5_accepts_exact_depth_and_parses_the_256_node_descriptor_before_rejection() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();

        let mut depth_bytes = vec![0x04; MAX_TYPE_DESCRIPTOR_DEPTH];
        let mut depth_descriptor = TypeDescriptor::named(BOOLEAN_TYPE_ID);
        for _ in 0..MAX_TYPE_DESCRIPTOR_DEPTH {
            depth_descriptor = TypeDescriptor::option(depth_descriptor).unwrap();
        }
        depth_bytes.push(0x00);
        depth_bytes.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&depth_bytes, &[0])),
            ),
            Ok(RuntimeValue::option(&active, depth_descriptor, None).unwrap())
        );

        let mut tree_bytes = orv5_named_descriptor(BOOLEAN_TYPE_ID);
        let mut tree_descriptor = TypeDescriptor::named(BOOLEAN_TYPE_ID);
        for _ in 0..7 {
            let child_bytes = tree_bytes.clone();
            tree_bytes = vec![0x03];
            tree_bytes.extend_from_slice(&child_bytes);
            tree_bytes.extend_from_slice(&child_bytes);
            tree_descriptor =
                TypeDescriptor::map(tree_descriptor.clone(), tree_descriptor).unwrap();
        }
        let mut maximum_bytes = vec![0x04];
        maximum_bytes.extend_from_slice(&tree_bytes);
        let _maximum_descriptor = TypeDescriptor::option(tree_descriptor).unwrap();
        assert_eq!(maximum_bytes.len(), 2_304);
        let maximum_error = decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&maximum_bytes, &[0])),
        )
        .unwrap_err();
        let ValueCodecError::CollectionValue {
            source: CollectionValueError::UnsupportedDescriptor { path, .. },
        } = maximum_error
        else {
            panic!("the 256-node descriptor must parse before collection admission rejects it");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::OptionChild,
                CollectionValuePathSegment::MapKeyChild,
            ]
        );

        let mut too_large_bytes = vec![0x04];
        too_large_bytes.extend_from_slice(&maximum_bytes);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&too_large_bytes, &[0])),
            ),
            Err(ValueCodecError::InvalidConstructedDescriptor {
                source: TypeDescriptorError::TooLarge {
                    maximum: 256,
                    actual: 257,
                },
            })
        );
    }

    #[test]
    fn orv5_map_duplicate_keys_keep_original_wire_indexes() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let mut descriptor = vec![0x03];
        descriptor.extend_from_slice(&orv5_named_descriptor(INTEGER_TYPE_ID));
        descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
        let mut body = 3_u32.to_be_bytes().to_vec();
        body.extend_from_slice(&orv5_map_entry(orv5_integer(2), orv5_boolean(false)));
        body.extend_from_slice(&orv5_map_entry(orv5_integer(1), orv5_boolean(true)));
        body.extend_from_slice(&orv5_map_entry(orv5_integer(2), orv5_boolean(true)));

        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(&descriptor, &body)),
            ),
            Err(ValueCodecError::CollectionValue {
                source: CollectionValueError::DuplicateMapKey {
                    first: 0,
                    duplicate: 2,
                },
            })
        );
    }

    #[test]
    fn orv5_revalidates_stale_records_and_rejects_unregistered_opaque_values() {
        let original = active_record_revision();
        let record = &original.catalogue().record_value_types()[0];
        let stale = RuntimeValue::Record(
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
        let active = active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));
        let registry =
            registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
        assert_eq!(
            encode_constructed_value(&active, &registry, &stale),
            Err(ValueCodecError::RecordValueNotActive {
                record_type: record.id(),
            })
        );

        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
        );
        let mut encoded = encode_constructed_value(&active, &registry, &opaque).unwrap();
        encoded[5..21].fill(0x72);
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Err(ValueCodecError::OpaqueValue {
                source: OpaqueValueError::UnregisteredType {
                    opaque_type: TypeId::from_bytes([0x72; 16]),
                },
            })
        );

        let mut opaque_list_descriptor = vec![0x02];
        opaque_list_descriptor.extend_from_slice(&orv5_named_descriptor(OPAQUE_TOKEN_TYPE_ID));
        let opaque_child = encode_constructed_value(&active, &registry, &opaque).unwrap();
        let mut opaque_list_body = 1_u32.to_be_bytes().to_vec();
        opaque_list_body.extend_from_slice(&(opaque_child.len() as u32).to_be_bytes());
        opaque_list_body.extend_from_slice(&opaque_child);
        let error = decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(
                &opaque_list_descriptor,
                &opaque_list_body,
            )),
        )
        .unwrap_err();
        let ValueCodecError::CollectionValue {
            source: CollectionValueError::UnsupportedDescriptor { path, descriptor },
        } = error
        else {
            panic!("opaque collection leaves must stay closed");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(descriptor, TypeDescriptor::named(OPAQUE_TOKEN_TYPE_ID));
    }

    #[test]
    fn constructed_collection_values_stay_closed_to_both_orf_value_paths() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let parameter = ParameterId::from_bytes([0x5f; 16]);
        for value in constructed_collection_values(&active) {
            let argument = ClientFrame::CallArgument {
                stream: 7,
                parameter,
                value: value.clone(),
            };
            assert_eq!(
                encode_client_frame(&argument),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_catalogue_client_frame(active.catalogue(), &argument),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_active_client_frame(&active, &argument),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_registered_client_frame(&active, &registry, &argument),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );

            let batch = ServerFrame::EventBatch {
                stream: 7,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Value(value),
                }],
            };
            assert_eq!(
                encode_server_frame(&batch),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_catalogue_server_frame(active.catalogue(), &batch),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_active_server_frame(&active, &batch),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
            assert_eq!(
                encode_registered_server_frame(&active, &registry, &batch),
                Err(FrameCodecError::Value {
                    source: ValueCodecError::UnsupportedValue,
                })
            );
        }
    }

    #[test]
    fn supported_flat_values_prove_the_orf_value_rejection_is_causal() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let parameter = ParameterId::from_bytes([0x5f; 16]);
        let argument = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: RuntimeValue::Boolean(true),
        };
        assert!(encode_client_frame(&argument).is_ok());
        assert!(encode_catalogue_client_frame(active.catalogue(), &argument).is_ok());
        assert!(encode_active_client_frame(&active, &argument).is_ok());
        assert!(encode_registered_client_frame(&active, &registry, &argument).is_ok());

        let batch = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        };
        assert!(encode_server_frame(&batch).is_ok());
        assert!(encode_catalogue_server_frame(active.catalogue(), &batch).is_ok());
        assert!(encode_active_server_frame(&active, &batch).is_ok());
        assert!(encode_registered_server_frame(&active, &registry, &batch).is_ok());
    }

    #[test]
    fn orf5_retains_orf4_frames_and_embeds_orv5_values() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let parameter = ParameterId::from_bytes([0x71; 16]);
        let argument = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: RuntimeValue::Boolean(true),
        };
        let value = orv5_boolean(true);
        let mut expected_argument = b"ORF5\x02\0".to_vec();
        expected_argument.extend_from_slice(&7_u64.to_be_bytes());
        expected_argument.extend_from_slice(&42_u32.to_be_bytes());
        expected_argument.extend_from_slice(&parameter.to_bytes());
        expected_argument.extend_from_slice(&value);
        assert_eq!(
            encode_constructed_client_frame(&active, &registry, &argument),
            Ok(expected_argument.clone())
        );
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &expected_argument),
            Ok(argument)
        );
        assert_eq!(
            decode_registered_client_frame(&active, &registry, &expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );

        let event_frame = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        };
        let mut expected_events = b"ORF5\x82\0".to_vec();
        expected_events.extend_from_slice(&7_u64.to_be_bytes());
        expected_events.extend_from_slice(&42_u32.to_be_bytes());
        expected_events.push(0x01);
        expected_events.extend_from_slice(&1_u16.to_be_bytes());
        expected_events.extend_from_slice(&1_u64.to_be_bytes());
        expected_events.push(0x01);
        expected_events.extend_from_slice(&26_u32.to_be_bytes());
        expected_events.extend_from_slice(&value);
        assert_eq!(
            encode_constructed_server_frame(&active, &registry, &event_frame),
            Ok(expected_events.clone())
        );
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &expected_events),
            Ok(event_frame)
        );
        assert_eq!(
            decode_registered_server_frame(&active, &registry, &expected_events),
            Err(FrameCodecError::InvalidMarker)
        );

        let client_non_value = ClientFrame::Ping {
            token: [1, 2, 3, 4, 5, 6, 7, 8],
        };
        let client_expected = orf5_frame(0x06, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            encode_constructed_client_frame(&active, &registry, &client_non_value),
            Ok(client_expected.clone())
        );
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &client_expected),
            Ok(client_non_value)
        );

        let server_non_value = ServerFrame::CallAccepted {
            stream: 1,
            invocation: InvocationId::from_bytes([0x72; 16]),
        };
        let server_expected = orf5_frame(0x81, 1, &[0x72; 16]);
        assert_eq!(
            encode_constructed_server_frame(&active, &registry, &server_non_value),
            Ok(server_expected.clone())
        );
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &server_expected),
            Ok(server_non_value)
        );

        let enum_frame = ServerFrame::EventBatch {
            stream: 9,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                )),
            }],
        };
        let mut enum_value = b"ORV5".to_vec();
        enum_value.push(0x0a);
        enum_value.extend_from_slice(&ENUM_TYPE.to_bytes());
        enum_value.extend_from_slice(&4_u32.to_be_bytes());
        enum_value.extend_from_slice(b"lead");
        let mut enum_payload = vec![0x01];
        enum_payload.extend_from_slice(&1_u16.to_be_bytes());
        enum_payload.extend_from_slice(&1_u64.to_be_bytes());
        enum_payload.push(0x01);
        enum_payload.extend_from_slice(&(enum_value.len() as u32).to_be_bytes());
        enum_payload.extend_from_slice(&enum_value);
        let enum_expected = orf5_frame(0x82, 9, &enum_payload);
        assert_eq!(
            encode_constructed_server_frame(&active, &registry, &enum_frame),
            Ok(enum_expected.clone())
        );
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &enum_expected),
            Ok(enum_frame)
        );

        for marker in [b"ORF1", b"ORF2", b"ORF3", b"ORF4"] {
            let mut crossed = expected_argument.clone();
            crossed[..4].copy_from_slice(marker);
            assert_eq!(
                decode_constructed_client_frame(&active, &registry, &crossed),
                Err(FrameCodecError::InvalidMarker)
            );
        }
        for marker in [b"ORF1", b"ORF2", b"ORF3", b"ORF4"] {
            let mut crossed = expected_events.clone();
            crossed[..4].copy_from_slice(marker);
            assert_eq!(
                decode_constructed_server_frame(&active, &registry, &crossed),
                Err(FrameCodecError::InvalidMarker)
            );
        }

        assert_eq!(
            decode_client_frame(&expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_client_frame(active.catalogue(), &expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_client_frame(&active, &expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_registered_client_frame(&active, &registry, &expected_argument),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_server_frame(&expected_events),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_catalogue_server_frame(active.catalogue(), &expected_events),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_active_server_frame(&active, &expected_events),
            Err(FrameCodecError::InvalidMarker)
        );
        assert_eq!(
            decode_registered_server_frame(&active, &registry, &expected_events),
            Err(FrameCodecError::InvalidMarker)
        );
    }

    #[test]
    fn orf5_rejects_constructed_arguments_and_events_after_value_validation() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let value = RuntimeValue::option(
            &active,
            descriptor.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();
        let parameter = ParameterId::from_bytes([0x74; 16]);
        let argument = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: value.clone(),
        };
        let expected_error = FrameCodecError::ConstructedValueNotAccepted {
            descriptor: descriptor.clone(),
        };
        assert_eq!(
            encode_constructed_client_frame(&active, &registry, &argument),
            Err(expected_error.clone())
        );
        assert_eq!(
            expected_error.to_string(),
            "constructed runtime values are not accepted by protocol 5 frames"
        );
        assert!(std::error::Error::source(&expected_error).is_none());

        let mut option_payload = 18_u16.to_be_bytes().to_vec();
        option_payload.extend_from_slice(&[0x04, 0x00]);
        option_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        option_payload.push(1);
        option_payload.extend_from_slice(&26_u32.to_be_bytes());
        option_payload.extend_from_slice(&orv5_boolean(true));
        let encoded_value = orv5_constructed(option_payload);
        let mut encoded_argument = b"ORF5\x02\0".to_vec();
        encoded_argument.extend_from_slice(&7_u64.to_be_bytes());
        encoded_argument.extend_from_slice(
            &(u32::try_from(parameter.to_bytes().len() + encoded_value.len()).unwrap())
                .to_be_bytes(),
        );
        encoded_argument.extend_from_slice(&parameter.to_bytes());
        encoded_argument.extend_from_slice(&encoded_value);
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &encoded_argument),
            Err(expected_error.clone())
        );

        let mut malformed_argument = encoded_argument.clone();
        malformed_argument[79] = 2;
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &malformed_argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::InvalidOptionPresence { value: 2 },
            })
        );

        let event = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        };
        assert_eq!(
            encode_constructed_server_frame(&active, &registry, &event),
            Err(expected_error.clone())
        );

        let mut encoded_event = b"ORF5\x82\0".to_vec();
        encoded_event.extend_from_slice(&7_u64.to_be_bytes());
        encoded_event.extend_from_slice(
            &(u32::try_from(1 + 2 + 8 + 1 + 4 + encoded_value.len()).unwrap()).to_be_bytes(),
        );
        encoded_event.push(0x01);
        encoded_event.extend_from_slice(&1_u16.to_be_bytes());
        encoded_event.extend_from_slice(&1_u64.to_be_bytes());
        encoded_event.push(0x01);
        encoded_event.extend_from_slice(&(encoded_value.len() as u32).to_be_bytes());
        encoded_event.extend_from_slice(&encoded_value);
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &encoded_event),
            Err(expected_error.clone())
        );

        let mut malformed_event = encoded_event;
        malformed_event[79] = 2;
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &malformed_event),
            Err(FrameCodecError::Value {
                source: ValueCodecError::InvalidOptionPresence { value: 2 },
            })
        );
    }

    #[test]
    fn orf5_accepts_opaque_results_and_rejects_opaque_arguments() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let payload = [0x73; 16];
        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
        );
        let mut encoded_value = b"ORV5".to_vec();
        encoded_value.push(0x0c);
        encoded_value.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
        encoded_value.extend_from_slice(&16_u32.to_be_bytes());
        encoded_value.extend_from_slice(&payload);

        let result = ServerFrame::EventBatch {
            stream: 8,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(opaque.clone()),
            }],
        };
        let mut result_payload = vec![0x01];
        result_payload.extend_from_slice(&1_u16.to_be_bytes());
        result_payload.extend_from_slice(&1_u64.to_be_bytes());
        result_payload.push(0x01);
        result_payload.extend_from_slice(&(encoded_value.len() as u32).to_be_bytes());
        result_payload.extend_from_slice(&encoded_value);
        let expected_result = orf5_frame(0x82, 8, &result_payload);
        assert_eq!(
            encode_constructed_server_frame(&active, &registry, &result),
            Ok(expected_result.clone())
        );
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &expected_result),
            Ok(result)
        );

        let parameter = ParameterId::from_bytes([0x78; 16]);
        let argument = ClientFrame::CallArgument {
            stream: 8,
            parameter,
            value: opaque.clone(),
        };
        let opaque_error = FrameCodecError::OpaqueArgumentNotAccepted {
            opaque_type: OPAQUE_TOKEN_TYPE_ID,
        };
        assert_eq!(
            encode_constructed_client_frame(&active, &registry, &argument),
            Err(opaque_error.clone())
        );
        let mut argument_payload = parameter.to_bytes().to_vec();
        argument_payload.extend_from_slice(&encoded_value);
        let expected_argument = orf5_frame(0x02, 8, &argument_payload);
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &expected_argument),
            Err(opaque_error.clone())
        );

        let function = FunctionId::from_bytes([0x79; 16]);
        let mut connection = ProtocolConnection::new();
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallRawStart {
                    stream: 8,
                    function,
                },
            )
            .unwrap();
        let before_argument = connection.clone();
        assert_eq!(
            connection.receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgument {
                    stream: 8,
                    parameter,
                    value: opaque,
                },
            ),
            Err(ConnectionError::InvalidFrame {
                source: opaque_error,
            })
        );
        assert_eq!(connection, before_argument);
    }

    #[test]
    fn orf5_constructed_rejection_preserves_connection_state_and_credit() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = registered_opaque_codecs(standard).unwrap();
        let function = FunctionId::from_bytes([0x75; 16]);
        let parameter = ParameterId::from_bytes([0x76; 16]);
        let descriptor = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
        let constructed = RuntimeValue::list(
            &active,
            descriptor.clone(),
            vec![RuntimeValue::Boolean(true)],
        )
        .unwrap();
        let rejection = FrameCodecError::ConstructedValueNotAccepted { descriptor };
        let mut connection = ProtocolConnection::new();
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
            )
            .unwrap();
        let before_argument = connection.clone();
        assert_eq!(
            connection.receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter,
                    value: constructed.clone(),
                },
            ),
            Err(ConnectionError::InvalidFrame {
                source: rejection.clone(),
            })
        );
        assert_eq!(connection, before_argument);

        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 4096,
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .receive_constructed(
                    &active,
                    &registry,
                    ClientFrame::CallArgumentsComplete { stream: 1 },
                )
                .unwrap(),
            Some(ClientAction::Dispatch {
                stream: 1,
                call: RawCall {
                    function,
                    arguments: vec![],
                },
            })
        );
        connection
            .apply_constructed(
                &active,
                &registry,
                ServerAction::Accepted {
                    stream: 1,
                    invocation: InvocationId::from_bytes([0x77; 16]),
                },
            )
            .unwrap();

        let before_event = connection.clone();
        assert_eq!(
            connection.apply_constructed(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 1,
                    events: vec![Event::Value(constructed)],
                },
            ),
            Err(ConnectionError::InvalidFrame { source: rejection })
        );
        assert_eq!(connection, before_event);

        assert_eq!(
            connection
                .apply_constructed(
                    &active,
                    &registry,
                    ServerAction::Events {
                        stream: 1,
                        events: vec![Event::Value(RuntimeValue::Boolean(true))],
                    },
                )
                .unwrap(),
            ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Value(RuntimeValue::Boolean(true)),
                }],
            }
        );
    }

    fn orv5_constructed(payload: Vec<u8>) -> Vec<u8> {
        let mut encoded = b"ORV5".to_vec();
        encoded.push(0x0d);
        encoded.extend_from_slice(&[0; 16]);
        encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&payload);
        encoded
    }

    fn orf5_frame(tag: u8, stream: u64, payload: &[u8]) -> Vec<u8> {
        let mut encoded = b"ORF5".to_vec();
        encoded.push(tag);
        encoded.push(0);
        encoded.extend_from_slice(&stream.to_be_bytes());
        encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        encoded.extend_from_slice(payload);
        encoded
    }

    fn orv5_descriptor_payload(descriptor: &[u8], body: &[u8]) -> Vec<u8> {
        let mut payload = (descriptor.len() as u16).to_be_bytes().to_vec();
        payload.extend_from_slice(descriptor);
        payload.extend_from_slice(body);
        payload
    }

    fn orv5_named_descriptor(type_id: TypeId) -> Vec<u8> {
        let mut descriptor = vec![0x00];
        descriptor.extend_from_slice(&type_id.to_bytes());
        descriptor
    }

    fn assert_orv4_to_orv5_flat_marker_substitution(
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        value: RuntimeValue,
    ) {
        let version_four = encode_registered_value(active, registry, &value).unwrap();
        let mut expected = version_four;
        assert_eq!(&expected[..4], b"ORV4");
        expected[..4].copy_from_slice(b"ORV5");
        if matches!(
            &value,
            RuntimeValue::Text(text) if text.as_bytes().windows(4).any(|window| window == b"ORV4")
        ) || matches!(
            &value,
            RuntimeValue::Bytes(bytes) if bytes.windows(4).any(|window| window == b"ORV4")
        ) {
            assert!(expected.windows(4).any(|window| window == b"ORV4"));
        }
        assert_eq!(
            encode_constructed_value(active, registry, &value),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_constructed_value(active, registry, &expected),
            Ok(value)
        );
    }

    fn orv5_integer(value: i32) -> Vec<u8> {
        let mut encoded = b"ORV5".to_vec();
        encoded.push(0x03);
        encoded.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
        encoded.extend_from_slice(&4_u32.to_be_bytes());
        encoded.extend_from_slice(&value.to_be_bytes());
        encoded
    }

    fn orv5_boolean(value: bool) -> Vec<u8> {
        let mut encoded = b"ORV5".to_vec();
        encoded.push(0x02);
        encoded.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        encoded.push(u8::from(value));
        encoded
    }

    fn orv5_boolean_list_prefix(count: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&18_u16.to_be_bytes());
        payload.extend_from_slice(&[0x02, 0x00]);
        payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
        payload.extend_from_slice(&count.to_be_bytes());
        payload
    }

    fn orv5_map_entry(key: Vec<u8>, value: Vec<u8>) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend_from_slice(&(key.len() as u32).to_be_bytes());
        entry.extend_from_slice(&key);
        entry.extend_from_slice(&(value.len() as u32).to_be_bytes());
        entry.extend_from_slice(&value);
        entry
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
