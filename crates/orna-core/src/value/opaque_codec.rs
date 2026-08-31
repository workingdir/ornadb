//! Registration and validation for bounded opaque runtime values.

use std::{collections::HashSet, error::Error, fmt};

use super::{
    ACTION_BODY_PREFIX_BYTES, ACTION_DOMAIN_CLIENT, ACTION_DOMAIN_SERVER, ACTION_IDENTITY_BYTES,
    ACTION_IDENTITY_FIELDS, ActiveDatabaseRevision, InspectCarrierEnvelope,
    MAX_OPAQUE_CODEC_ACTION_ARGUMENTS, MAX_OPAQUE_CODEC_MAGIC_LENGTH,
    MAX_OPAQUE_CODEC_PAYLOAD_LENGTH, MAX_ROWS_CELLS, MAX_ROWS_COLUMNS, MAX_ROWS_PAYLOAD_LENGTH,
    MAX_ROWS_ROWS, MAX_RUNTIME_VALUE_NODES, ORV3_HEADER_BYTES, ORV3_MARKER, QualifiedSemanticName,
    ROWS_FRAME_VERSION, SYS_SOURCE_FUNCTION_TYPE_ID, TypeId, ValueTypeKind, ValueTypeMutability,
    ValueTypePersistence, VerifiedStandardLibrarySnapshot, inspect_carrier_codec_by_type_id,
};

/// The canonical payload contract of one checked-in opaque codec.
///
/// The contract is inert data supplied by linked code. It fixes the exact
/// canonical byte form the codec accepts and rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
enum OpaquePayloadContract {
    /// The canonical form is the complete input bytes with exactly this length.
    FixedLength {
        /// The exact payload length.
        payload_length: usize,
    },
    /// `MAGIC <len:u32 be> <utf-8 bytes>`: a fixed ASCII magic prefix, then a
    /// big-endian `u32` body length, then exactly that many UTF-8 bytes and no
    /// trailing bytes.
    LengthPrefixedUtf8 {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `MAGIC <len:u32 be> <utf-8 bytes>` with the terminal-document text
    /// invariants: a final newline and no control codes except line feeds.
    TerminalDocument {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `MAGIC <len:u32 be> <bytes>`: a fixed ASCII magic prefix, then a
    /// big-endian `u32` body length, then exactly that many bytes and no
    /// trailing bytes.
    LengthPrefixedBytes {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `MAGIC <len:u32 be> <canonical ORNA-ACTION/1 descriptor>`: a fixed
    /// ASCII magic prefix, an exact body length, and one structurally valid
    /// action descriptor. Target and catalogue semantics remain client-owned.
    Action {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `MAGIC <len:u32 be> <canonical JSON UTF-8 bytes>`: a fixed ASCII magic
    /// prefix, then a big-endian `u32` body length, then exactly that many
    /// canonical JSON UTF-8 bytes and no trailing bytes.
    LengthPrefixedCanonicalJson {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `ORNA-ROWS/1` with bounded ordered column metadata and cell framing.
    Rows {
        /// The exact ASCII magic prefix, including its trailing space.
        magic: String,
    },
    /// `MAGIC <len:u32 be> <canonical std.ui.UI JSON UTF-8 bytes>`: a fixed
    /// ASCII magic prefix, then a big-endian `u32` body length, then exactly
    /// that many canonical JSON UTF-8 bytes representing one closed UI value.
    LengthPrefixedUiValue {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
    /// `MAGIC <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>`: a
    /// fixed ASCII magic prefix, a big-endian `u32` media-type length, the
    /// non-empty media-type bytes, a big-endian `u32` body length, then
    /// exactly that many bytes and no trailing bytes.
    MediaTypeFramed {
        /// The exact ASCII magic prefix, including any separating space.
        magic: String,
    },
}

/// One checked-in identity codec registration for an opaque standard value type.
///
/// The registration is inert data supplied by linked code. It cannot name a
/// dynamic library, executable, filesystem path, environment value, or source
/// declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueCodecRegistration {
    opaque_type: TypeId,
    semantic_name: QualifiedSemanticName,
    representation_contract: String,
    contract: OpaquePayloadContract,
}

impl OpaqueCodecRegistration {
    /// Declares one bounded codec whose canonical form is the complete input bytes.
    pub fn fixed_length_identity(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        payload_length: usize,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        if payload_length == 0 || payload_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
            return Err(OpaqueCodecRegistryError::InvalidPayloadLength {
                opaque_type,
                payload_length,
            });
        }
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract: representation_contract.into(),
            contract: OpaquePayloadContract::FixedLength { payload_length },
        })
    }

    /// Declares a framed codec whose canonical form is
    /// `MAGIC <len:u32 be> <utf-8 bytes>` with exactly that many UTF-8 body
    /// bytes and no trailing bytes.
    pub fn length_prefixed_utf8(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        let representation_contract = representation_contract.into();
        validate_codec_magic(opaque_type, &magic)?;
        let contract =
            if is_terminal_document_codec(&semantic_name, &representation_contract, &magic) {
                OpaquePayloadContract::TerminalDocument { magic }
            } else {
                OpaquePayloadContract::LengthPrefixedUtf8 { magic }
            };
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract,
            contract,
        })
    }

    /// Declares a framed codec whose canonical form is
    /// `MAGIC <len:u32 be> <bytes>` with exactly that many body bytes and no
    /// trailing bytes.
    pub fn length_prefixed_bytes(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        validate_codec_magic(opaque_type, &magic)?;
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract: representation_contract.into(),
            contract: OpaquePayloadContract::LengthPrefixedBytes { magic },
        })
    }

    /// Declares a structurally checked `ORNA-ACTION/1` descriptor codec.
    ///
    /// This validates only the authority-free descriptor shape and canonical
    /// active-value frame structure. Target resolution, revision pinning, and
    /// result/argument type compatibility remain the CLIENT boundary contract.
    pub fn length_prefixed_action(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        validate_codec_magic(opaque_type, &magic)?;
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract: representation_contract.into(),
            contract: OpaquePayloadContract::Action { magic },
        })
    }

    /// Declares a framed codec whose canonical form is
    /// `MAGIC <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>`.
    pub fn media_type_framed(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        validate_codec_magic(opaque_type, &magic)?;
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract: representation_contract.into(),
            contract: OpaquePayloadContract::MediaTypeFramed { magic },
        })
    }

    /// Declares a framed codec whose canonical form is
    /// `MAGIC <len:u32 be> <canonical JSON UTF-8 bytes>` with exactly that
    /// many canonical JSON UTF-8 body bytes and no trailing bytes.
    pub fn length_prefixed_canonical_json(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        validate_codec_magic(opaque_type, &magic)?;
        let representation_contract = representation_contract.into();
        let contract = if is_ui_value_codec(&semantic_name, &representation_contract, &magic) {
            OpaquePayloadContract::LengthPrefixedUiValue { magic }
        } else {
            OpaquePayloadContract::LengthPrefixedCanonicalJson { magic }
        };
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract,
            contract,
        })
    }
    /// Declares a bounded `ORNA-ROWS/1` opaque codec.
    ///
    /// Structural framing is validated by the core registration; active
    /// catalogue type resolution and cell semantics remain protocol-owned.
    pub fn rows(
        opaque_type: TypeId,
        semantic_name: QualifiedSemanticName,
        representation_contract: impl Into<String>,
        magic: impl Into<String>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let magic = magic.into();
        validate_codec_magic(opaque_type, &magic)?;
        Ok(Self {
            opaque_type,
            semantic_name,
            representation_contract: representation_contract.into(),
            contract: OpaquePayloadContract::Rows { magic },
        })
    }
}

/// Identifies the accepted standard terminal-document codec without changing
/// the generic length-prefixed UTF-8 constructor's public API.
fn is_terminal_document_codec(
    semantic_name: &QualifiedSemanticName,
    representation_contract: &str,
    magic: &str,
) -> bool {
    magic == "ORNA-TERMINAL-DOCUMENT/1 "
        && representation_contract == "orna.std.value.terminal-document@1"
        && semantic_name
            .parts()
            .iter()
            .map(String::as_str)
            .eq(["std", "terminal", "document"])
}

/// Identifies the accepted standard UI codec without changing the generic
/// length-prefixed canonical-JSON constructor's public API.
fn is_ui_value_codec(
    semantic_name: &QualifiedSemanticName,
    representation_contract: &str,
    magic: &str,
) -> bool {
    magic == "ORNA-UI/1 "
        && representation_contract == "orna.std.value.ui@1"
        && semantic_name
            .parts()
            .iter()
            .map(String::as_str)
            .eq(["std", "ui", "ui"])
}

/// Rejects an empty, non-ASCII, or oversized framed-codec magic prefix.
fn validate_codec_magic(opaque_type: TypeId, magic: &str) -> Result<(), OpaqueCodecRegistryError> {
    if magic.is_empty() || !magic.is_ascii() || magic.len() > MAX_OPAQUE_CODEC_MAGIC_LENGTH {
        return Err(OpaqueCodecRegistryError::InvalidMagic { opaque_type });
    }
    Ok(())
}

/// An immutable set of checked-in codecs bound to one verified standard snapshot.
#[derive(Clone, Debug)]
pub struct OpaqueCodecRegistry {
    standard: VerifiedStandardLibrarySnapshot,
    registrations: Vec<OpaqueCodecRegistration>,
}

impl OpaqueCodecRegistry {
    /// Validates a complete checked-in registration set against one standard snapshot.
    pub fn new(
        standard: &VerifiedStandardLibrarySnapshot,
        registrations: impl IntoIterator<Item = OpaqueCodecRegistration>,
    ) -> Result<Self, OpaqueCodecRegistryError> {
        let registrations = registrations.into_iter().collect::<Vec<_>>();
        if registrations.is_empty() {
            return Err(OpaqueCodecRegistryError::EmptyRegistry);
        }

        for (index, registration) in registrations.iter().enumerate() {
            for earlier in &registrations[..index] {
                if earlier.opaque_type == registration.opaque_type {
                    return Err(OpaqueCodecRegistryError::DuplicateType {
                        opaque_type: registration.opaque_type,
                    });
                }
                if earlier.semantic_name == registration.semantic_name {
                    return Err(OpaqueCodecRegistryError::DuplicateName {
                        semantic_name: registration.semantic_name.clone(),
                    });
                }
                if earlier.representation_contract == registration.representation_contract {
                    return Err(OpaqueCodecRegistryError::DuplicateContract {
                        representation_contract: registration.representation_contract.clone(),
                    });
                }
            }
            validate_opaque_registration(standard, registration)?;
        }

        if let Some(definition) = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| {
                definition.kind() == ValueTypeKind::Opaque
                    && !registrations
                        .iter()
                        .any(|registration| registration.opaque_type == definition.id())
            })
        {
            return Err(OpaqueCodecRegistryError::UnregisteredOpaqueDefinition {
                opaque_type: definition.id(),
            });
        }

        Ok(Self {
            standard: standard.clone(),
            registrations,
        })
    }

    fn construct(
        &self,
        active: &ActiveDatabaseRevision,
        opaque_type: TypeId,
        payload: &[u8],
    ) -> Result<OpaqueValue, OpaqueValueError> {
        let active_standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or(OpaqueValueError::ActiveStandardRequired)?;
        if !same_standard_snapshot(&self.standard, active_standard) {
            return Err(OpaqueValueError::ActiveStandardMismatch);
        }
        let registration = self
            .registrations
            .iter()
            .find(|registration| registration.opaque_type == opaque_type)
            .ok_or(OpaqueValueError::UnregisteredType { opaque_type })?;
        validate_opaque_registration(active_standard, registration)
            .map_err(|_| OpaqueValueError::InactiveRegistration { opaque_type })?;
        validate_opaque_payload(opaque_type, &registration.contract, payload)?;
        Ok(OpaqueValue {
            opaque_type,
            canonical_payload: payload.to_vec(),
        })
    }
}

/// Validates one complete canonical payload against its codec contract.
fn validate_opaque_payload(
    opaque_type: TypeId,
    contract: &OpaquePayloadContract,
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    match contract {
        OpaquePayloadContract::FixedLength { payload_length } => {
            if payload.len() != *payload_length {
                return Err(OpaqueValueError::WrongPayloadLength {
                    opaque_type,
                    expected: *payload_length,
                    actual: payload.len(),
                });
            }
            Ok(())
        }
        OpaquePayloadContract::LengthPrefixedUtf8 { magic } => {
            validate_length_prefixed_utf8(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::TerminalDocument { magic } => {
            validate_terminal_document(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::LengthPrefixedBytes { magic } => {
            validate_length_prefixed_bytes(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::Action { magic } => {
            validate_action_frame(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::LengthPrefixedCanonicalJson { magic } => {
            validate_length_prefixed_canonical_json(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::LengthPrefixedUiValue { magic } => {
            validate_length_prefixed_ui_value(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::MediaTypeFramed { magic } => {
            validate_media_type_framed(opaque_type, magic.as_bytes(), payload)
        }
        OpaquePayloadContract::Rows { magic } => {
            validate_rows_payload(opaque_type, magic.as_bytes(), payload)
        }
    }
}
fn validate_rows_payload(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let invalid = || OpaqueValueError::InvalidRowsFrame { opaque_type };
    if payload.len() > MAX_ROWS_PAYLOAD_LENGTH || !payload.starts_with(magic) {
        return Err(invalid());
    }

    let mut cursor = magic.len();
    let version = take_rows_u16(payload, &mut cursor).ok_or_else(invalid)?;
    if version != ROWS_FRAME_VERSION {
        return Err(invalid());
    }
    let column_count = take_rows_u32(payload, &mut cursor)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(invalid)?;
    if !(1..=MAX_ROWS_COLUMNS).contains(&column_count) {
        return Err(invalid());
    }
    let minimum_columns = column_count
        .checked_mul(23)
        .and_then(|bytes| bytes.checked_add(4));
    if minimum_columns.is_none_or(|minimum| payload.len().saturating_sub(cursor) < minimum) {
        return Err(invalid());
    }

    let mut names = HashSet::with_capacity(column_count);
    for _ in 0..column_count {
        let name_length = take_rows_u32(payload, &mut cursor)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(invalid)?;
        let name = take_rows_bytes(payload, &mut cursor, name_length).ok_or_else(invalid)?;
        if std::str::from_utf8(name).is_err() || name.is_empty() || !names.insert(name) {
            return Err(invalid());
        }
        let type_form = take_rows_bytes(payload, &mut cursor, 1)
            .and_then(|bytes| bytes.first().copied())
            .ok_or_else(invalid)?;
        let type_id = take_rows_bytes(payload, &mut cursor, 16).ok_or_else(invalid)?;
        if !(0x01..=0x04).contains(&type_form)
            || (type_form == 0x01 && !is_rows_standard_scalar_type_id(type_id))
        {
            return Err(invalid());
        }
        let nullable = take_rows_bytes(payload, &mut cursor, 1)
            .and_then(|bytes| bytes.first().copied())
            .ok_or_else(invalid)?;
        if nullable > 1 {
            return Err(invalid());
        }
    }

    let row_count = take_rows_u32(payload, &mut cursor)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(invalid)?;
    if row_count > MAX_ROWS_ROWS
        || row_count
            .checked_mul(column_count)
            .is_none_or(|cells| cells > MAX_ROWS_CELLS)
    {
        return Err(invalid());
    }

    for _ in 0..row_count {
        let cell_count = take_rows_u32(payload, &mut cursor)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(invalid)?;
        if cell_count != column_count {
            return Err(invalid());
        }
        for _ in 0..cell_count {
            let length = take_rows_u32(payload, &mut cursor)
                .and_then(|length| usize::try_from(length).ok())
                .ok_or_else(invalid)?;
            if length > MAX_ROWS_PAYLOAD_LENGTH {
                return Err(invalid());
            }
            let cell = take_rows_bytes(payload, &mut cursor, length).ok_or_else(invalid)?;
            if validate_rows_orv5_value(cell, 0).is_err() {
                return Err(invalid());
            }
        }
    }

    if cursor == payload.len() {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn take_rows_bytes<'a>(payload: &'a [u8], cursor: &mut usize, length: usize) -> Option<&'a [u8]> {
    let end = cursor.checked_add(length)?;
    let bytes = payload.get(*cursor..end)?;
    *cursor = end;
    Some(bytes)
}

fn take_rows_u16(payload: &[u8], cursor: &mut usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        take_rows_bytes(payload, cursor, 2)?.try_into().ok()?,
    ))
}

fn take_rows_u32(payload: &[u8], cursor: &mut usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        take_rows_bytes(payload, cursor, 4)?.try_into().ok()?,
    ))
}
fn is_rows_standard_scalar_type_id(type_id: &[u8]) -> bool {
    type_id.len() == 16
        && type_id[..15].iter().all(|byte| *byte == 0)
        && matches!(type_id[15], 0x01 | 0x02 | 0x03 | 0x04 | 0x06 | 0x07)
}
fn is_rows_standard_scalar_type_id_for_tag(type_id: &[u8], tag: u8) -> bool {
    is_rows_standard_scalar_type_id(type_id) && type_id[15] == tag
}
fn validate_rows_orv5_value(bytes: &[u8], depth: usize) -> Result<(), ()> {
    const HEADER: usize = 25;
    const MARKER: &[u8; 4] = b"ORV5";
    if depth > 32 {
        return Err(());
    }
    if bytes.len() < HEADER || &bytes[..4] != MARKER {
        return Err(());
    }
    let tag = bytes[4];
    let declared = u32::from_be_bytes(bytes[21..25].try_into().map_err(|_| ())?) as usize;
    if declared > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
        return Err(());
    }
    let actual = bytes.len() - HEADER;
    if declared != actual {
        return Err(());
    }
    let type_identity = &bytes[5..21];
    let has_type_identity = type_identity.iter().any(|byte| *byte != 0);
    let payload = &bytes[HEADER..];
    match tag {
        0x00 | 0x01 | 0x09 => payload.is_empty().then_some(()).ok_or(()),
        0x02 => (payload.len() == 1 && matches!(payload[0], 0 | 1))
            .then_some(())
            .ok_or(()),
        0x03 => (payload.len() == 4).then_some(()).ok_or(()),
        0x04 | 0x05 => (payload.len() == 8).then_some(()).ok_or(()),
        0x06 => (is_rows_standard_scalar_type_id_for_tag(type_identity, 0x06)
            && std::str::from_utf8(payload).is_ok())
            .then_some(())
            .ok_or(()),
        0x07 => is_rows_standard_scalar_type_id_for_tag(type_identity, 0x07)
            .then_some(())
            .ok_or(()),
        0x0a => (has_type_identity && std::str::from_utf8(payload).is_ok())
            .then_some(())
            .ok_or(()),
        0x0c => Err(()),
        0x08 => (payload.len() == 16).then_some(()).ok_or(()),
        0x0b => validate_rows_record_payload(payload, depth),
        0x0d => validate_rows_constructed_payload(
            bytes[5..21].try_into().map_err(|_| ())?,
            payload,
            depth,
        ),
        _ => Err(()),
    }
}
fn validate_rows_record_payload(payload: &[u8], depth: usize) -> Result<(), ()> {
    if depth > 32 {
        return Err(());
    }
    let mut cursor = 0;
    let count = take_rows_u32(payload, &mut cursor)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(())?;
    if count > payload.len().saturating_sub(cursor) / 20 {
        return Err(());
    }
    let mut field_ids = HashSet::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let field_id = take_rows_bytes(payload, &mut cursor, 16).ok_or(())?;
        if !field_ids.insert(field_id) || previous.is_some_and(|prior: &[u8]| prior >= field_id) {
            return Err(());
        }
        previous = Some(field_id);
        let length = take_rows_u32(payload, &mut cursor)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(())?;
        let nested = take_rows_bytes(payload, &mut cursor, length).ok_or(())?;
        validate_rows_orv5_value(nested, depth + 1)?;
    }
    (cursor == payload.len()).then_some(()).ok_or(())
}

fn validate_rows_constructed_payload(
    type_id: [u8; 16],
    payload: &[u8],
    depth: usize,
) -> Result<(), ()> {
    if type_id != [0; 16] || depth >= 32 {
        return Err(());
    }
    let mut cursor = 0;
    let descriptor_length = usize::from(u16::from_be_bytes(
        take_rows_bytes(payload, &mut cursor, 2)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ));
    if descriptor_length == 0 {
        return Err(());
    }
    let descriptor_bytes = take_rows_bytes(payload, &mut cursor, descriptor_length).ok_or(())?;
    let (descriptor, consumed) = validate_rows_descriptor(descriptor_bytes, 0, 0)?;
    if consumed != descriptor_bytes.len() {
        return Err(());
    }
    validate_rows_constructor_content(descriptor, &payload[cursor..], depth + 1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowsDescriptor {
    Named,
    Reference,
    List,
    Map,
    Option,
}

fn validate_rows_descriptor(
    bytes: &[u8],
    offset: usize,
    depth: usize,
) -> Result<(RowsDescriptor, usize), ()> {
    if depth > 32 {
        return Err(());
    }
    let tag = *bytes.get(offset).ok_or(())?;
    let cursor = offset.checked_add(1).ok_or(())?;
    match tag {
        0 | 1 => {
            let end = cursor.checked_add(16).ok_or(())?;
            if end > bytes.len() {
                return Err(());
            }
            Ok((
                if tag == 0 {
                    RowsDescriptor::Named
                } else {
                    RowsDescriptor::Reference
                },
                end,
            ))
        }
        2..=4 => {
            let (_child, end) = validate_rows_descriptor(bytes, cursor, depth + 1)?;
            if tag == 3 {
                let (_value, end) = validate_rows_descriptor(bytes, end, depth + 1)?;
                Ok((RowsDescriptor::Map, end))
            } else {
                Ok((
                    match tag {
                        2 => RowsDescriptor::List,
                        4 => RowsDescriptor::Option,
                        _ => unreachable!(),
                    },
                    end,
                ))
            }
        }
        _ => Err(()),
    }
}

fn validate_rows_constructor_content(
    descriptor: RowsDescriptor,
    content: &[u8],
    depth: usize,
) -> Result<(), ()> {
    if depth > 32 {
        return Err(());
    }
    match descriptor {
        RowsDescriptor::Option => {
            let presence = *content.first().ok_or(())?;
            match presence {
                0 if content.len() == 1 => Ok(()),
                1 => {
                    let mut cursor = 1;
                    let length = take_rows_u32(content, &mut cursor)
                        .and_then(|length| usize::try_from(length).ok())
                        .ok_or(())?;
                    let nested = take_rows_bytes(content, &mut cursor, length).ok_or(())?;
                    if cursor != content.len() {
                        return Err(());
                    }
                    validate_rows_orv5_value(nested, depth)
                }
                _ => Err(()),
            }
        }
        RowsDescriptor::List => validate_rows_repeated_content(content, depth),
        RowsDescriptor::Map => {
            let mut cursor = 0;
            let count = take_rows_u32(content, &mut cursor)
                .and_then(|count| usize::try_from(count).ok())
                .ok_or(())?;
            for _ in 0..count {
                let key_length = take_rows_u32(content, &mut cursor)
                    .and_then(|length| usize::try_from(length).ok())
                    .ok_or(())?;
                let key = take_rows_bytes(content, &mut cursor, key_length).ok_or(())?;
                validate_rows_orv5_value(key, depth)?;
                let value_length = take_rows_u32(content, &mut cursor)
                    .and_then(|length| usize::try_from(length).ok())
                    .ok_or(())?;
                let value = take_rows_bytes(content, &mut cursor, value_length).ok_or(())?;
                validate_rows_orv5_value(value, depth)?;
            }
            (cursor == content.len()).then_some(()).ok_or(())
        }
        RowsDescriptor::Named | RowsDescriptor::Reference => Err(()),
    }
}

fn validate_rows_repeated_content(content: &[u8], depth: usize) -> Result<(), ()> {
    let mut cursor = 0;
    let count = take_rows_u32(content, &mut cursor)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(())?;
    for _ in 0..count {
        let length = take_rows_u32(content, &mut cursor)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(())?;
        let nested = take_rows_bytes(content, &mut cursor, length).ok_or(())?;
        validate_rows_orv5_value(nested, depth)?;
    }
    (cursor == content.len()).then_some(()).ok_or(())
}

/// Parses and validates one canonical JSON frame, returning its body value for
/// contracts that apply a schema-specific check after the generic framing.
fn canonical_json_body(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<serde_json::Value, OpaqueValueError> {
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let body_length = u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || payload.len() != prefix_length + body_length
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    let body = &payload[prefix_length..];
    if std::str::from_utf8(body).is_err() {
        return Err(OpaqueValueError::InvalidUtf8Body { opaque_type });
    }
    let value = serde_json::from_slice::<serde_json::Value>(body)
        .map_err(|_| OpaqueValueError::InvalidJsonBody { opaque_type })?;
    let canonical_body = serde_json::to_vec(&value)
        .map_err(|_| OpaqueValueError::InvalidJsonBody { opaque_type })?;
    if canonical_body != body {
        return Err(OpaqueValueError::InvalidJsonBody { opaque_type });
    }
    Ok(value)
}

/// Validates `MAGIC <len:u32 be> <canonical JSON UTF-8 bytes>` with exactly
/// `len` canonical JSON body bytes and no trailing bytes.
fn validate_length_prefixed_canonical_json(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    canonical_json_body(opaque_type, magic, payload).map(|_| ())
}

/// Validates the canonical frame and then the closed `std.ui.UI` JSON shape.
fn validate_length_prefixed_ui_value(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let value = canonical_json_body(opaque_type, magic, payload)?;
    let mut state = UiValueValidationState { node_count: 0 };
    validate_ui_value(opaque_type, &value, &mut state)
}

struct UiValueValidationState {
    node_count: usize,
}

fn invalid_ui_value(opaque_type: TypeId) -> Result<(), OpaqueValueError> {
    Err(OpaqueValueError::InvalidJsonBody { opaque_type })
}

fn validate_ui_value(
    opaque_type: TypeId,
    value: &serde_json::Value,
    state: &mut UiValueValidationState,
) -> Result<(), OpaqueValueError> {
    // Walk the recursive schema iteratively. The node bound and frame length
    // remain the resource limits without adding an unrelated depth limit.
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        state.node_count = state
            .node_count
            .checked_add(1)
            .ok_or(OpaqueValueError::InvalidJsonBody { opaque_type })?;
        if state.node_count > MAX_RUNTIME_VALUE_NODES {
            return invalid_ui_value(opaque_type);
        }

        let Some(object) = value.as_object() else {
            return invalid_ui_value(opaque_type);
        };
        match object.get("kind").and_then(serde_json::Value::as_str) {
            Some("empty") if object.len() == 1 => {}
            Some("fragment") => {
                if object.len() != 2 {
                    return invalid_ui_value(opaque_type);
                }
                let Some(children) = object.get("children").and_then(serde_json::Value::as_array)
                else {
                    return invalid_ui_value(opaque_type);
                };
                pending.extend(children.iter());
            }
            Some("node") => {
                if !(5..=9).contains(&object.len())
                    || object.keys().any(|key| {
                        !matches!(
                            key.as_str(),
                            "kind"
                                | "contract"
                                | "call_site_id"
                                | "function_instance_id"
                                | "key"
                                | "properties"
                                | "slots"
                                | "actions"
                                | "source_origin"
                        )
                    })
                {
                    return invalid_ui_value(opaque_type);
                }
                if !object.get("contract").is_some_and(valid_ui_contract)
                    || !object
                        .get("call_site_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                    || !object
                        .get("function_instance_id")
                        .is_none_or(|id| id.is_null() || id.is_string())
                    || !object.get("properties").is_some_and(valid_ui_properties)
                    || !object.get("slots").is_some_and(valid_ui_slots)
                    || !object.get("actions").is_some_and(valid_ui_actions)
                    || !object
                        .get("source_origin")
                        .is_none_or(valid_ui_source_origin)
                {
                    return invalid_ui_value(opaque_type);
                }
                let Some(slots) = object.get("slots").and_then(serde_json::Value::as_object) else {
                    return invalid_ui_value(opaque_type);
                };
                for children in slots.values() {
                    let Some(children) = children.as_array() else {
                        return invalid_ui_value(opaque_type);
                    };
                    pending.extend(children.iter());
                }
            }
            _ => return invalid_ui_value(opaque_type),
        }
    }
    Ok(())
}

fn valid_ui_contract(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == 3
        && object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && object
            .get("version")
            .and_then(serde_json::Value::as_str)
            .is_some()
}

fn valid_ui_typed_value(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == 2
        && object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && object.contains_key("value")
}

fn valid_ui_properties(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.values().all(valid_ui_typed_value))
}

fn valid_ui_slots(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.values().all(serde_json::Value::is_array))
}

fn valid_ui_actions(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.values().all(|action| {
            let Some(action) = action.as_object() else {
                return false;
            };
            action
                .get("action_id")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && action
                    .get("input_type")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                && action
                    .get("debug_kind")
                    .is_none_or(|kind| kind.is_null() || kind.is_string())
        })
    })
}

fn valid_ui_source_origin(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return value.is_null();
    };
    object
        .keys()
        .all(|key| matches!(key.as_str(), "source_unit_id" | "start" | "end"))
        && object
            .get("source_unit_id")
            .is_none_or(serde_json::Value::is_string)
        && object
            .get("start")
            .is_none_or(|value| value.as_i64().is_some())
        && object
            .get("end")
            .is_none_or(|value| value.as_i64().is_some())
}

/// Validates the canonical terminal-document framing and text invariants.
fn validate_terminal_document(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let body_length = u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || payload.len() != prefix_length + body_length
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    let body = &payload[prefix_length..];
    let text =
        std::str::from_utf8(body).map_err(|_| OpaqueValueError::InvalidUtf8Body { opaque_type })?;
    if !body.ends_with(b"\n") || text.chars().any(is_document_control) {
        return Err(OpaqueValueError::InvalidDocumentBody { opaque_type });
    }
    Ok(())
}

/// Validates `MAGIC <len:u32 be> <utf-8 bytes>` with exactly `len` body bytes.
fn validate_length_prefixed_utf8(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let body_length = u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || payload.len() != prefix_length + body_length
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    let body = &payload[prefix_length..];
    if std::str::from_utf8(body).is_err() {
        return Err(OpaqueValueError::InvalidUtf8Body { opaque_type });
    }
    Ok(())
}

/// Validates the canonical action descriptor framing without resolving any
/// target or catalogue identity.
fn validate_action_frame(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let body_length = u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    let body_end = prefix_length
        .checked_add(body_length)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH
        || payload.len() > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH
        || payload.len() != body_end
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }

    let body = &payload[prefix_length..body_end];
    if body.len() < ACTION_BODY_PREFIX_BYTES {
        return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
    }
    if !matches!(body[0], ACTION_DOMAIN_CLIENT | ACTION_DOMAIN_SERVER) {
        return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
    }
    for identity_index in 0..ACTION_IDENTITY_FIELDS {
        let identity_start = 1 + (identity_index * ACTION_IDENTITY_BYTES);
        let identity_end = identity_start + ACTION_IDENTITY_BYTES;
        if body[identity_start..identity_end]
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
        }
    }

    let mut offset = ACTION_BODY_PREFIX_BYTES;
    let argument_count = u32::from_be_bytes(
        body[offset - 4..offset]
            .try_into()
            .expect("the action argument count is exactly four bytes"),
    ) as usize;
    if argument_count > MAX_OPAQUE_CODEC_ACTION_ARGUMENTS {
        return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
    }

    let mut previous_parameter: Option<&[u8]> = None;
    for _ in 0..argument_count {
        let parameter_end = offset
            .checked_add(ACTION_IDENTITY_BYTES)
            .ok_or(OpaqueValueError::InvalidActionFrame { opaque_type })?;
        let frame_length_end = parameter_end
            .checked_add(4)
            .ok_or(OpaqueValueError::InvalidActionFrame { opaque_type })?;
        if frame_length_end > body.len() {
            return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
        }
        let parameter = &body[offset..parameter_end];
        if parameter.iter().all(|byte| *byte == 0) {
            return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
        }
        if previous_parameter.is_some_and(|previous| previous >= parameter) {
            return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
        }
        previous_parameter = Some(parameter);

        let frame_length = u32::from_be_bytes(
            body[parameter_end..frame_length_end]
                .try_into()
                .expect("the action argument frame length is exactly four bytes"),
        ) as usize;
        let frame_start = frame_length_end;
        let frame_end = frame_start
            .checked_add(frame_length)
            .ok_or(OpaqueValueError::InvalidActionFrame { opaque_type })?;
        if frame_end > body.len() || validate_orv3_frame(&body[frame_start..frame_end]).is_err() {
            return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
        }
        offset = frame_end;
    }
    if offset != body.len() {
        return Err(OpaqueValueError::InvalidActionFrame { opaque_type });
    }
    Ok(())
}

/// Validates one complete canonical ORV3 active-value frame without resolving
/// its type identity against a catalogue. Duplicate field identities are rejected
/// structurally; declaration order and semantic field/type checks stay in the
/// protocol/client decoder, while this keeps malformed bytes out of an opaque
/// value before trigger.
fn validate_orv3_frame(encoded: &[u8]) -> Result<(), ()> {
    let mut pending = vec![encoded];
    let mut node_count = 0usize;
    while let Some(frame) = pending.pop() {
        if frame.len() < ORV3_HEADER_BYTES || !frame.starts_with(ORV3_MARKER) {
            return Err(());
        }
        let declared = u32::from_be_bytes(
            frame[21..25]
                .try_into()
                .expect("the ORV3 header is exactly twenty-five bytes"),
        ) as usize;
        let frame_end = ORV3_HEADER_BYTES.checked_add(declared).ok_or(())?;
        if declared > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || frame.len() != frame_end {
            return Err(());
        }
        node_count = node_count.checked_add(1).ok_or(())?;
        if node_count > MAX_RUNTIME_VALUE_NODES {
            return Err(());
        }
        let tag = frame[4];
        let body = &frame[ORV3_HEADER_BYTES..];
        match tag {
            0x00 | 0x01 | 0x09 if body.is_empty() => {}
            0x02 if body.len() == 1 && matches!(body[0], 0 | 1) => {}
            0x03 if body.len() == 4 => {}
            0x04 if body.len() == 8 => {}
            0x05 if body.len() == 8 => {
                let bits =
                    u64::from_be_bytes(body.try_into().expect("float payload is eight bytes"));
                let value = f64::from_bits(bits);
                if bits == (-0.0_f64).to_bits() || !value.is_finite() {
                    return Err(());
                }
            }
            0x06 | 0x0a if std::str::from_utf8(body).is_ok() => {}
            0x07 => {}
            0x08 if body.len() == ACTION_IDENTITY_BYTES => {}
            0x0b => {
                if body.len() < 4 {
                    return Err(());
                }
                let field_count = u32::from_be_bytes(
                    body[..4]
                        .try_into()
                        .expect("the record field count is exactly four bytes"),
                ) as usize;
                if field_count >= MAX_RUNTIME_VALUE_NODES {
                    return Err(());
                }
                let minimum = field_count
                    .checked_mul(ACTION_IDENTITY_BYTES + 4 + ORV3_HEADER_BYTES)
                    .and_then(|length| 4usize.checked_add(length))
                    .ok_or(())?;
                if minimum > body.len() {
                    return Err(());
                }
                let mut cursor = 4usize;
                let mut field_identities: HashSet<[u8; ACTION_IDENTITY_BYTES]> = HashSet::new();
                for _ in 0..field_count {
                    let length_start = cursor.checked_add(ACTION_IDENTITY_BYTES).ok_or(())?;
                    if length_start > body.len() {
                        return Err(());
                    }
                    let field_identity: [u8; ACTION_IDENTITY_BYTES] = body[cursor..length_start]
                        .try_into()
                        .expect("the record field identity is sixteen bytes");
                    if !field_identities.insert(field_identity) {
                        return Err(());
                    }
                    let frame_start = length_start.checked_add(4).ok_or(())?;
                    if frame_start > body.len() {
                        return Err(());
                    }
                    let length = u32::from_be_bytes(
                        body[length_start..frame_start]
                            .try_into()
                            .expect("the record field frame length is exactly four bytes"),
                    ) as usize;
                    let frame_end = frame_start.checked_add(length).ok_or(())?;
                    if length < ORV3_HEADER_BYTES || frame_end > body.len() {
                        return Err(());
                    }
                    pending.push(&body[frame_start..frame_end]);
                    cursor = frame_end;
                }
                if cursor != body.len() {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
    }
    Ok(())
}

/// Validates `MAGIC <len:u32 be> <bytes>` with exactly `len` body bytes.
fn validate_length_prefixed_bytes(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let body_length = u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || payload.len() != prefix_length + body_length
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    Ok(())
}

/// Validates
/// `MAGIC <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>`.
fn validate_media_type_framed(
    opaque_type: TypeId,
    magic: &[u8],
    payload: &[u8],
) -> Result<(), OpaqueValueError> {
    let magic_end = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < magic_end || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength { opaque_type }
        } else {
            OpaqueValueError::InvalidMagic { opaque_type }
        });
    }
    let media_type_length = u32::from_be_bytes(
        payload[magic.len()..magic_end]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if media_type_length == 0 {
        return Err(OpaqueValueError::InvalidMediaType { opaque_type });
    }
    let media_type_end = magic_end
        .checked_add(media_type_length)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    let body_length_start = media_type_end
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength { opaque_type })?;
    if payload.len() < body_length_start {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    let body_length = u32::from_be_bytes(
        payload[media_type_end..body_length_start]
            .try_into()
            .expect("the length prefix is exactly four bytes"),
    ) as usize;
    if body_length > MAX_OPAQUE_CODEC_PAYLOAD_LENGTH
        || payload.len() != body_length_start + body_length
    {
        return Err(OpaqueValueError::InvalidFrameLength { opaque_type });
    }
    Ok(())
}

/// Returns whether `ch` is forbidden in a terminal document body. Newline is
/// the only permitted control character because it is the canonical separator.
fn is_document_control(ch: char) -> bool {
    ch != '\n' && matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}'..='\u{009F}')
}

fn validate_opaque_registration(
    standard: &VerifiedStandardLibrarySnapshot,
    registration: &OpaqueCodecRegistration,
) -> Result<(), OpaqueCodecRegistryError> {
    let Some(definition) = standard
        .catalogue()
        .type_definition_by_id(registration.opaque_type)
    else {
        return Err(OpaqueCodecRegistryError::MissingDefinition {
            opaque_type: registration.opaque_type,
        });
    };
    let Some(definition) = definition.as_opaque_value() else {
        return Err(OpaqueCodecRegistryError::WrongDefinitionKind {
            opaque_type: registration.opaque_type,
        });
    };
    if definition.name() != &registration.semantic_name {
        return Err(OpaqueCodecRegistryError::SemanticNameMismatch {
            opaque_type: registration.opaque_type,
        });
    }
    if definition.representation_contract() != registration.representation_contract {
        return Err(OpaqueCodecRegistryError::ContractMismatch {
            opaque_type: registration.opaque_type,
        });
    }
    if definition.mutability() != ValueTypeMutability::Immutable
        || definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(OpaqueCodecRegistryError::DefinitionPolicyMismatch {
            opaque_type: registration.opaque_type,
        });
    }
    Ok(())
}

fn same_standard_snapshot(
    expected: &VerifiedStandardLibrarySnapshot,
    actual: &VerifiedStandardLibrarySnapshot,
) -> bool {
    expected.revision() == actual.revision()
        && expected.digest_version() == actual.digest_version()
        && expected.source().id() == actual.source().id()
        && expected.source().revision_hash() == actual.source().revision_hash()
        && expected.catalogue().revision() == actual.catalogue().revision()
        && expected.digest() == actual.digest()
}

/// An opaque runtime value accepted by one active revision and codec registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueValue {
    opaque_type: TypeId,
    canonical_payload: Vec<u8>,
}

impl OpaqueValue {
    /// Validates and constructs one complete opaque payload.
    pub fn new(
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        opaque_type: TypeId,
        payload: impl AsRef<[u8]>,
    ) -> Result<Self, OpaqueValueError> {
        registry.construct(active, opaque_type, payload.as_ref())
    }

    /// Validates and constructs one sealed `sys.inspect` snapshot or projection carrier.
    ///
    /// Inspector carriers intentionally bypass [`OpaqueCodecRegistry`]: their
    /// identities and contracts are sealed system facts, not definitions in
    /// the active application or verified standard-library catalogue. The
    /// canonical envelope still carries the active source/catalogue provenance
    /// and is decoded before the opaque value is retained.
    pub fn new_inspect_carrier(
        active: &ActiveDatabaseRevision,
        opaque_type: TypeId,
        payload: impl AsRef<[u8]>,
    ) -> Result<Self, OpaqueValueError> {
        if inspect_carrier_codec_by_type_id(opaque_type).is_none() {
            return Err(OpaqueValueError::UnregisteredType { opaque_type });
        }

        let payload = payload.as_ref();
        let envelope = InspectCarrierEnvelope::decode(payload)
            .map_err(|_| OpaqueValueError::InvalidInspectCarrierEnvelope { opaque_type })?;
        if envelope.carrier_kind().type_id() != opaque_type {
            return Err(OpaqueValueError::InspectCarrierTypeMismatch { opaque_type });
        }
        let pair = active.pair();
        if envelope.source_revision_id() != pair.source()
            || envelope.catalogue_revision_id() != pair.catalogue()
        {
            return Err(OpaqueValueError::InspectCarrierRevisionMismatch { opaque_type });
        }

        Ok(Self {
            opaque_type,
            canonical_payload: payload.to_vec(),
        })
    }

    /// Validates and constructs a source metadata carrier.
    pub fn new_source_metadata_carrier(
        active: &ActiveDatabaseRevision,
        opaque_type: TypeId,
        payload: impl AsRef<[u8]>,
    ) -> Result<Self, OpaqueValueError> {
        if opaque_type != SYS_SOURCE_FUNCTION_TYPE_ID {
            return Err(OpaqueValueError::UnregisteredType { opaque_type });
        }
        let payload = payload.as_ref();
        let metadata = crate::source_metadata::SourceFunctionMetadata::decode(payload)
            .map_err(|_| OpaqueValueError::InvalidSourceMetadata { opaque_type })?;
        let Some(function) = active.catalogue().function_by_id(metadata.function()) else {
            return Err(OpaqueValueError::SourceFunctionUnavailable { opaque_type });
        };
        if function.current_revision() != metadata.function_revision() {
            return Err(OpaqueValueError::SourceRevisionMismatch { opaque_type });
        }
        Ok(Self {
            opaque_type,
            canonical_payload: payload.to_vec(),
        })
    }

    /// Returns the nominal opaque value-type identity.
    pub const fn opaque_type(&self) -> TypeId {
        self.opaque_type
    }

    /// Returns the complete bounded canonical codec payload.
    pub fn canonical_payload(&self) -> &[u8] {
        &self.canonical_payload
    }
}

/// An error from validating an immutable opaque codec registry.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpaqueCodecRegistryError {
    /// The checked-in registry contains no codec.
    EmptyRegistry,
    /// A fixed-length identity codec has an invalid payload bound.
    InvalidPayloadLength {
        /// The opaque type named by the invalid registration.
        opaque_type: TypeId,
        /// The invalid exact payload length.
        payload_length: usize,
    },
    /// A framed codec has an empty, non-ASCII, or oversized magic prefix.
    InvalidMagic {
        /// The opaque type named by the invalid registration.
        opaque_type: TypeId,
    },
    /// Two registrations name the same type identity.
    DuplicateType {
        /// The duplicated opaque type identity.
        opaque_type: TypeId,
    },
    /// Two registrations name the same semantic type.
    DuplicateName {
        /// The duplicated qualified semantic name.
        semantic_name: QualifiedSemanticName,
    },
    /// Two registrations select the same representation contract.
    DuplicateContract {
        /// The duplicated representation contract.
        representation_contract: String,
    },
    /// A registered type identity is absent from the standard snapshot.
    MissingDefinition {
        /// The absent opaque type identity.
        opaque_type: TypeId,
    },
    /// A registered identity resolves to a non-opaque definition.
    WrongDefinitionKind {
        /// The mismatched type identity.
        opaque_type: TypeId,
    },
    /// The registered semantic name differs from the standard definition.
    SemanticNameMismatch {
        /// The mismatched opaque type identity.
        opaque_type: TypeId,
    },
    /// The registered contract differs from the standard definition.
    ContractMismatch {
        /// The mismatched opaque type identity.
        opaque_type: TypeId,
    },
    /// The standard definition is not immutable and transient.
    DefinitionPolicyMismatch {
        /// The mismatched opaque type identity.
        opaque_type: TypeId,
    },
    /// The standard snapshot contains an opaque definition with no codec.
    UnregisteredOpaqueDefinition {
        /// The unregistered opaque type identity.
        opaque_type: TypeId,
    },
}

impl fmt::Display for OpaqueCodecRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRegistry => formatter.write_str("opaque codec registry is empty"),
            Self::InvalidPayloadLength { .. } => {
                formatter.write_str("opaque codec payload length is invalid")
            }
            Self::InvalidMagic { .. } => {
                formatter.write_str("opaque codec magic prefix is invalid")
            }
            Self::DuplicateType { .. } => {
                formatter.write_str("opaque codec type identity is duplicated")
            }
            Self::DuplicateName { .. } => {
                formatter.write_str("opaque codec semantic name is duplicated")
            }
            Self::DuplicateContract { .. } => {
                formatter.write_str("opaque codec representation contract is duplicated")
            }
            Self::MissingDefinition { .. } => {
                formatter.write_str("opaque codec definition is missing")
            }
            Self::WrongDefinitionKind { .. } => {
                formatter.write_str("opaque codec definition has the wrong kind")
            }
            Self::SemanticNameMismatch { .. } => {
                formatter.write_str("opaque codec semantic name does not match")
            }
            Self::ContractMismatch { .. } => {
                formatter.write_str("opaque codec representation contract does not match")
            }
            Self::DefinitionPolicyMismatch { .. } => {
                formatter.write_str("opaque codec definition policy does not match")
            }
            Self::UnregisteredOpaqueDefinition { .. } => {
                formatter.write_str("standard opaque definition has no registered codec")
            }
        }
    }
}

impl Error for OpaqueCodecRegistryError {}

/// An error from constructing a registered opaque runtime value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpaqueValueError {
    /// The active revision does not pin a verified standard snapshot.
    ActiveStandardRequired,
    /// The active revision pins a different verified standard snapshot.
    ActiveStandardMismatch,
    /// The requested opaque type has no checked-in codec registration.
    UnregisteredType {
        /// The unregistered opaque type identity.
        opaque_type: TypeId,
    },
    /// The active standard no longer matches the checked registration.
    InactiveRegistration {
        /// The inactive opaque type identity.
        opaque_type: TypeId,
    },
    /// The sealed Inspector carrier envelope is malformed or non-canonical.
    InvalidInspectCarrierEnvelope {
        /// The carrier type whose envelope was rejected.
        opaque_type: TypeId,
    },
    /// The envelope projection does not match the requested sealed carrier.
    InspectCarrierTypeMismatch {
        /// The carrier type whose projection was rejected.
        opaque_type: TypeId,
    },
    /// The envelope is pinned to a different active source/catalogue pair.
    InspectCarrierRevisionMismatch {
        /// The carrier type whose provenance was rejected.
        opaque_type: TypeId,
    },
    /// The source metadata carrier envelope is malformed or non-canonical.
    InvalidSourceMetadata {
        /// The carrier type whose metadata was rejected.
        opaque_type: TypeId,
    },
    /// The source function is not present in the active catalogue.
    SourceFunctionUnavailable {
        /// The carrier type whose function was rejected.
        opaque_type: TypeId,
    },
    /// The source metadata names a stale function revision.
    SourceRevisionMismatch {
        /// The carrier type whose revision was rejected.
        opaque_type: TypeId,
    },
    /// The complete opaque payload has the wrong exact length.
    WrongPayloadLength {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
        /// The codec's required payload length.
        expected: usize,
        /// The supplied complete payload length.
        actual: usize,
    },
    /// A framed payload does not start with the codec's exact magic prefix.
    InvalidMagic {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A framed payload declares a length inconsistent with its remaining bytes.
    InvalidFrameLength {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// An action descriptor has malformed or non-canonical structure.
    InvalidActionFrame {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A length-prefixed UTF-8 payload body is not valid UTF-8.
    InvalidUtf8Body {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A terminal-document body is empty, lacks a final newline, or contains
    /// a forbidden control character.
    InvalidDocumentBody {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A canonical JSON payload body is invalid or not in canonical form.
    InvalidJsonBody {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A media-type framed payload carries an empty media type.
    InvalidMediaType {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
    /// A bounded `ORNA-ROWS/1` payload is malformed or non-canonical.
    InvalidRowsFrame {
        /// The opaque type whose payload was rejected.
        opaque_type: TypeId,
    },
}

impl fmt::Display for OpaqueValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveStandardRequired => {
                formatter.write_str("opaque value requires an active standard snapshot")
            }
            Self::ActiveStandardMismatch => {
                formatter.write_str("opaque codec registry does not match the active standard")
            }
            Self::UnregisteredType { .. } => {
                formatter.write_str("opaque value type has no registered codec")
            }
            Self::InactiveRegistration { .. } => {
                formatter.write_str("opaque codec registration is not active")
            }
            Self::InvalidInspectCarrierEnvelope { .. } => {
                formatter.write_str("inspect carrier envelope is invalid")
            }
            Self::InspectCarrierTypeMismatch { .. } => {
                formatter.write_str("inspect carrier envelope type does not match")
            }
            Self::InspectCarrierRevisionMismatch { .. } => {
                formatter.write_str("inspect carrier envelope revision does not match")
            }
            Self::InvalidSourceMetadata { .. } => {
                formatter.write_str("source metadata carrier is invalid")
            }
            Self::SourceFunctionUnavailable { .. } => {
                formatter.write_str("source metadata function is not active")
            }
            Self::SourceRevisionMismatch { .. } => {
                formatter.write_str("source metadata function revision does not match")
            }
            Self::WrongPayloadLength { .. } => {
                formatter.write_str("opaque value payload has the wrong length")
            }
            Self::InvalidMagic { .. } => {
                formatter.write_str("opaque value payload has the wrong magic prefix")
            }
            Self::InvalidFrameLength { .. } => {
                formatter.write_str("opaque value payload has an inconsistent frame length")
            }
            Self::InvalidActionFrame { .. } => {
                formatter.write_str("opaque value action frame is malformed or non-canonical")
            }
            Self::InvalidUtf8Body { .. } => {
                formatter.write_str("opaque value payload body is not valid UTF-8")
            }
            Self::InvalidDocumentBody { .. } => {
                formatter.write_str("terminal document payload body is invalid")
            }
            Self::InvalidJsonBody { .. } => {
                formatter.write_str("opaque value payload body is not valid canonical JSON")
            }
            Self::InvalidMediaType { .. } => {
                formatter.write_str("opaque value payload has an empty media type")
            }
            Self::InvalidRowsFrame { .. } => {
                formatter.write_str("opaque value Rows frame is malformed or non-canonical")
            }
        }
    }
}

impl Error for OpaqueValueError {}
