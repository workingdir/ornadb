//! The bounded, canonical `ORNA-INSPECT/1` carrier envelope.
//!
//! This module knows only the *shape* of ORV5 values. Active-catalogue and
//! opaque-codec validation remains at the protocol/server boundary.

use std::{error::Error, fmt};

use crate::{CatalogueRevisionId, InvocationId, SourceRevisionId, TypeId};

pub const INSPECT_CARRIER_MAGIC: &[u8; 15] = b"ORNA-INSPECT/1 ";
pub const INSPECT_CARRIER_VERSION: u16 = 1;
pub const MAX_INSPECT_CARRIER_ROWS: usize = 65_536;
pub const MAX_INSPECT_CARRIER_ROW_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INSPECT_CARRIER_BYTES: usize = 16 * 1024 * 1024;

const ORV5_MARKER: &[u8; 4] = b"ORV5";
const ORV5_HEADER_BYTES: usize = 25;
const ORV5_PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const RECORD_FIELD_HEADER_BYTES: usize = 20;
const MAX_CONSTRUCTED_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InspectProjection {
    InvocationNodes,
    Calls,
    Resources,
    StateCells,
    UiNodes,
    PresentationCandidates,
    RuntimeBindings,
    SecurityDecisions,
}

impl InspectProjection {
    pub const fn tag(self) -> u8 {
        match self {
            Self::InvocationNodes => 2,
            Self::Calls => 3,
            Self::Resources => 4,
            Self::StateCells => 5,
            Self::UiNodes => 6,
            Self::PresentationCandidates => 7,
            Self::RuntimeBindings => 8,
            Self::SecurityDecisions => 9,
        }
    }

    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            2 => Some(Self::InvocationNodes),
            3 => Some(Self::Calls),
            4 => Some(Self::Resources),
            5 => Some(Self::StateCells),
            6 => Some(Self::UiNodes),
            7 => Some(Self::PresentationCandidates),
            8 => Some(Self::RuntimeBindings),
            9 => Some(Self::SecurityDecisions),
            _ => None,
        }
    }

    pub const fn carrier_kind(self) -> InspectCarrierKind {
        match self {
            Self::InvocationNodes => InspectCarrierKind::InvocationNodes,
            Self::Calls => InspectCarrierKind::Calls,
            Self::Resources => InspectCarrierKind::Resources,
            Self::StateCells => InspectCarrierKind::StateCells,
            Self::UiNodes => InspectCarrierKind::UiNodes,
            Self::PresentationCandidates => InspectCarrierKind::PresentationCandidates,
            Self::RuntimeBindings => InspectCarrierKind::RuntimeBindings,
            Self::SecurityDecisions => InspectCarrierKind::SecurityDecisions,
        }
    }

    pub const fn type_id(self) -> TypeId {
        self.carrier_kind().type_id()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InspectCarrierKind {
    Snapshot,
    InvocationNodes,
    Calls,
    Resources,
    StateCells,
    UiNodes,
    PresentationCandidates,
    RuntimeBindings,
    SecurityDecisions,
}

impl From<InspectProjection> for InspectCarrierKind {
    fn from(value: InspectProjection) -> Self {
        value.carrier_kind()
    }
}

impl InspectCarrierKind {
    pub const fn tag(self) -> u8 {
        match self {
            Self::Snapshot => 1,
            Self::InvocationNodes => 2,
            Self::Calls => 3,
            Self::Resources => 4,
            Self::StateCells => 5,
            Self::UiNodes => 6,
            Self::PresentationCandidates => 7,
            Self::RuntimeBindings => 8,
            Self::SecurityDecisions => 9,
        }
    }

    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Snapshot),
            2 => Some(Self::InvocationNodes),
            3 => Some(Self::Calls),
            4 => Some(Self::Resources),
            5 => Some(Self::StateCells),
            6 => Some(Self::UiNodes),
            7 => Some(Self::PresentationCandidates),
            8 => Some(Self::RuntimeBindings),
            9 => Some(Self::SecurityDecisions),
            _ => None,
        }
    }

    pub const fn type_id(self) -> TypeId {
        use crate::system::*;
        match self {
            Self::Snapshot => SYS_INSPECT_SNAPSHOT_TYPE_ID,
            Self::InvocationNodes => SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            Self::Calls => SYS_INSPECT_CALLS_TYPE_ID,
            Self::Resources => SYS_INSPECT_RESOURCES_TYPE_ID,
            Self::StateCells => SYS_INSPECT_STATE_CELLS_TYPE_ID,
            Self::UiNodes => SYS_INSPECT_UI_NODES_TYPE_ID,
            Self::PresentationCandidates => SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            Self::RuntimeBindings => SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            Self::SecurityDecisions => SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        }
    }

    pub fn from_type_id(id: TypeId) -> Option<Self> {
        (1..=9)
            .filter_map(Self::from_tag)
            .find(|kind| kind.type_id() == id)
    }

    pub const fn projection(self) -> Option<InspectProjection> {
        match self {
            Self::Snapshot => None,
            Self::InvocationNodes => Some(InspectProjection::InvocationNodes),
            Self::Calls => Some(InspectProjection::Calls),
            Self::Resources => Some(InspectProjection::Resources),
            Self::StateCells => Some(InspectProjection::StateCells),
            Self::UiNodes => Some(InspectProjection::UiNodes),
            Self::PresentationCandidates => Some(InspectProjection::PresentationCandidates),
            Self::RuntimeBindings => Some(InspectProjection::RuntimeBindings),
            Self::SecurityDecisions => Some(InspectProjection::SecurityDecisions),
        }
    }
}

/// Provenance copied from trusted kernel/server facts.
///
/// `server_epoch_id` is the epoch encoded by ORNA-INSPECT/1. The distinct
/// client execution epoch is intentionally not part of this core envelope;
/// the client binds it externally while validating the decoded carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectCarrierProvenance {
    /// The server-side snapshot epoch represented by the carrier.
    server_epoch_id: u64,
    /// The invocation captured by the server epoch, when the carrier was
    /// constructed from target-bound facts. Legacy constructors leave this
    /// absent because ORNA-INSPECT/1 does not encode it.
    target_invocation_id: Option<InvocationId>,
    source_revision_id: SourceRevisionId,
    catalogue_revision_id: CatalogueRevisionId,
}

impl InspectCarrierProvenance {
    /// Explicitly marks the supplied facts as trusted kernel/server evidence.
    pub const fn trusted(
        server_epoch_id: u64,
        source_revision_id: SourceRevisionId,
        catalogue_revision_id: CatalogueRevisionId,
    ) -> Self {
        Self {
            server_epoch_id,
            target_invocation_id: None,
            source_revision_id,
            catalogue_revision_id,
        }
    }

    /// Explicitly marks target-bound server facts as trusted evidence.
    pub const fn trusted_for_target(
        server_epoch_id: u64,
        target_invocation_id: InvocationId,
        source_revision_id: SourceRevisionId,
        catalogue_revision_id: CatalogueRevisionId,
    ) -> Self {
        Self {
            server_epoch_id,
            target_invocation_id: Some(target_invocation_id),
            source_revision_id,
            catalogue_revision_id,
        }
    }

    /// Copies provenance from an immutable captured epoch.
    pub fn from_snapshot_epoch(epoch: &crate::inspect::InspectSnapshotEpoch) -> Self {
        let bytes = epoch.id().to_bytes();
        Self::trusted_for_target(
            u64::from_be_bytes(bytes[8..].try_into().expect("epoch identity width")),
            epoch.invocation_id(),
            epoch.source_revision_id(),
            epoch.catalogue_revision_id(),
        )
    }

    /// Returns the server-side epoch identity.
    pub const fn server_epoch_id(self) -> u64 {
        self.server_epoch_id
    }

    /// Returns the target invocation when this provenance is target-bound.
    pub const fn target_invocation_id(self) -> Option<InvocationId> {
        self.target_invocation_id
    }

    /// Binds trusted legacy provenance to a target, rejecting a conflicting
    /// target already present in the provenance.
    pub fn bind_target(
        self,
        target_invocation_id: InvocationId,
    ) -> Result<Self, InspectCarrierError> {
        if let Some(expected) = self.target_invocation_id
            && expected != target_invocation_id
        {
            return Err(InspectCarrierError::TargetInvocationMismatch {
                expected,
                actual: target_invocation_id,
            });
        }
        Ok(Self {
            target_invocation_id: Some(target_invocation_id),
            ..self
        })
    }

    /// Compatibility alias for the pre-target-bound carrier API.
    pub const fn epoch_id(self) -> u64 {
        self.server_epoch_id()
    }
    pub const fn source_revision_id(self) -> SourceRevisionId {
        self.source_revision_id
    }
    pub const fn catalogue_revision_id(self) -> CatalogueRevisionId {
        self.catalogue_revision_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectCarrierEnvelope {
    kind: InspectCarrierKind,
    provenance: InspectCarrierProvenance,
    rows: Vec<Vec<u8>>,
}

impl InspectCarrierEnvelope {
    /// Constructs a carrier from trusted kernel/server provenance facts.
    /// Prefer [`Self::from_snapshot_epoch`] or [`Self::new_with_target`]
    /// when the target invocation is available at the call site.
    pub fn new<K: Into<InspectCarrierKind>>(
        kind: K,
        epoch_id: u64,
        source_revision_id: SourceRevisionId,
        catalogue_revision_id: CatalogueRevisionId,
        rows: Vec<Vec<u8>>,
    ) -> Result<Self, InspectCarrierError> {
        Self::new_with_provenance(
            kind,
            InspectCarrierProvenance::trusted(epoch_id, source_revision_id, catalogue_revision_id),
            rows,
        )
    }

    pub fn new_with_provenance<K: Into<InspectCarrierKind>>(
        kind: K,
        provenance: InspectCarrierProvenance,
        rows: Vec<Vec<u8>>,
    ) -> Result<Self, InspectCarrierError> {
        let value = Self {
            kind: kind.into(),
            provenance,
            rows,
        };
        validate_rows(&value.rows)?;
        let length = value.encoded_len()?;
        if length > MAX_INSPECT_CARRIER_BYTES {
            return Err(InspectCarrierError::EnvelopeTooLarge {
                actual: length,
                maximum: MAX_INSPECT_CARRIER_BYTES,
            });
        }
        Ok(value)
    }

    /// Constructs a carrier after binding trusted provenance to its target
    /// invocation. The target is retained in memory; ORNA-INSPECT/1 does not
    /// have a target field, so a wire decode cannot recover this binding.
    pub fn new_with_target<K: Into<InspectCarrierKind>>(
        kind: K,
        target_invocation_id: InvocationId,
        provenance: InspectCarrierProvenance,
        rows: Vec<Vec<u8>>,
    ) -> Result<Self, InspectCarrierError> {
        Self::new_with_provenance(kind, provenance.bind_target(target_invocation_id)?, rows)
    }

    pub fn from_snapshot_epoch<K: Into<InspectCarrierKind>>(
        kind: K,
        epoch: &crate::inspect::InspectSnapshotEpoch,
        rows: Vec<Vec<u8>>,
    ) -> Result<Self, InspectCarrierError> {
        Self::new_with_provenance(
            kind,
            InspectCarrierProvenance::from_snapshot_epoch(epoch),
            rows,
        )
    }

    pub const fn carrier_kind(&self) -> InspectCarrierKind {
        self.kind
    }
    pub const fn kind(&self) -> InspectCarrierKind {
        self.kind
    }
    pub const fn projection(&self) -> Option<InspectProjection> {
        self.kind.projection()
    }
    pub const fn epoch_id(&self) -> u64 {
        self.server_epoch_id()
    }
    pub const fn server_epoch_id(&self) -> u64 {
        self.provenance.server_epoch_id()
    }
    pub const fn target_invocation_id(&self) -> Option<InvocationId> {
        self.provenance.target_invocation_id()
    }
    pub const fn source_revision_id(&self) -> SourceRevisionId {
        self.provenance.source_revision_id()
    }
    pub const fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        self.provenance.catalogue_revision_id()
    }
    pub const fn provenance(&self) -> InspectCarrierProvenance {
        self.provenance
    }
    pub fn rows(&self) -> &[Vec<u8>] {
        &self.rows
    }

    pub fn encode(&self) -> Result<Vec<u8>, InspectCarrierError> {
        validate_rows(&self.rows)?;
        let length = self.encoded_len()?;
        if length > MAX_INSPECT_CARRIER_BYTES {
            return Err(InspectCarrierError::EnvelopeTooLarge {
                actual: length,
                maximum: MAX_INSPECT_CARRIER_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(INSPECT_CARRIER_MAGIC);
        bytes.extend_from_slice(&INSPECT_CARRIER_VERSION.to_be_bytes());
        bytes.push(self.kind.tag());
        bytes.extend_from_slice(&self.epoch_id().to_be_bytes());
        bytes.extend_from_slice(&self.source_revision_id().to_bytes());
        bytes.extend_from_slice(&self.catalogue_revision_id().to_bytes());
        bytes.extend_from_slice(&(self.rows.len() as u32).to_be_bytes());
        for row in &self.rows {
            bytes.extend_from_slice(&(row.len() as u32).to_be_bytes());
            bytes.extend_from_slice(row);
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InspectCarrierError> {
        if bytes.len() > MAX_INSPECT_CARRIER_BYTES {
            return Err(InspectCarrierError::EnvelopeTooLarge {
                actual: bytes.len(),
                maximum: MAX_INSPECT_CARRIER_BYTES,
            });
        }
        let mut reader = Reader { bytes, position: 0 };
        if reader.take(INSPECT_CARRIER_MAGIC.len())? != INSPECT_CARRIER_MAGIC {
            return Err(InspectCarrierError::InvalidMagic);
        }
        let version = reader.u16()?;
        if version != INSPECT_CARRIER_VERSION {
            return Err(InspectCarrierError::UnsupportedVersion(version));
        }
        let tag = reader.u8()?;
        let kind = InspectCarrierKind::from_tag(tag)
            .ok_or(InspectCarrierError::UnknownProjectionTag(tag))?;
        let epoch_id = reader.u64()?;
        let source_revision_id = SourceRevisionId::from_bytes(reader.array::<16>()?);
        let catalogue_revision_id = CatalogueRevisionId::from_bytes(reader.array::<16>()?);
        let count = reader.u32()? as usize;
        if count > MAX_INSPECT_CARRIER_ROWS {
            return Err(InspectCarrierError::RowCountExceeded {
                actual: count,
                maximum: MAX_INSPECT_CARRIER_ROWS,
            });
        }
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            let length = reader.u32()? as usize;
            if length > MAX_INSPECT_CARRIER_ROW_BYTES {
                return Err(InspectCarrierError::RowTooLarge {
                    actual: length,
                    maximum: MAX_INSPECT_CARRIER_ROW_BYTES,
                });
            }
            rows.push(reader.take(length)?.to_vec());
        }
        if reader.remaining() != 0 {
            return Err(InspectCarrierError::TrailingBytes);
        }
        Self::new(
            kind,
            epoch_id,
            source_revision_id,
            catalogue_revision_id,
            rows,
        )
    }

    fn encoded_len(&self) -> Result<usize, InspectCarrierError> {
        let rows_len = self.rows.iter().try_fold(0usize, |total, row| {
            total
                .checked_add(4)
                .and_then(|total| total.checked_add(row.len()))
                .ok_or(InspectCarrierError::EnvelopeTooLarge {
                    actual: usize::MAX,
                    maximum: MAX_INSPECT_CARRIER_BYTES,
                })
        })?;
        62usize
            .checked_add(rows_len)
            .ok_or(InspectCarrierError::EnvelopeTooLarge {
                actual: usize::MAX,
                maximum: MAX_INSPECT_CARRIER_BYTES,
            })
    }
}

/// Validates ORV5 framing and structural payload shape. Active revision and
/// registered-opaque checks remain the protocol/server boundary's job.
pub fn validate_orv5_row_frame(row: &[u8]) -> Result<(), InspectRowError> {
    validate_orv5_value(row, 0)
}

/// Validates row count/size, ORV5 shape, and strict byte ordering.
pub fn validate_inspect_rows(rows: &[Vec<u8>]) -> Result<(), InspectCarrierError> {
    validate_rows(rows)
}

fn validate_rows(rows: &[Vec<u8>]) -> Result<(), InspectCarrierError> {
    if rows.len() > MAX_INSPECT_CARRIER_ROWS {
        return Err(InspectCarrierError::RowCountExceeded {
            actual: rows.len(),
            maximum: MAX_INSPECT_CARRIER_ROWS,
        });
    }
    let mut previous: Option<&[u8]> = None;
    for row in rows {
        if row.len() > MAX_INSPECT_CARRIER_ROW_BYTES {
            return Err(InspectCarrierError::RowTooLarge {
                actual: row.len(),
                maximum: MAX_INSPECT_CARRIER_ROW_BYTES,
            });
        }
        validate_orv5_row_frame(row).map_err(InspectCarrierError::InvalidRow)?;
        if previous.is_some_and(|previous| previous >= row.as_slice()) {
            return Err(InspectCarrierError::NonCanonicalRowOrder);
        }
        previous = Some(row);
    }
    Ok(())
}

fn validate_orv5_value(bytes: &[u8], depth: usize) -> Result<(), InspectRowError> {
    if bytes.len() < ORV5_HEADER_BYTES {
        return Err(InspectRowError::TruncatedHeader {
            actual: bytes.len(),
        });
    }
    if &bytes[..4] != ORV5_MARKER {
        return Err(InspectRowError::InvalidMarker);
    }
    let tag = bytes[4];
    let type_id = TypeId::from_bytes(bytes[5..21].try_into().expect("ORV5 header width"));
    let declared =
        u32::from_be_bytes(bytes[21..25].try_into().expect("ORV5 header width")) as usize;
    if declared > ORV5_PAYLOAD_LIMIT {
        return Err(InspectRowError::PayloadTooLarge {
            actual: declared,
            maximum: ORV5_PAYLOAD_LIMIT,
        });
    }
    let actual = bytes.len() - ORV5_HEADER_BYTES;
    if actual < declared {
        return Err(InspectRowError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(InspectRowError::TrailingBytes { declared, actual });
    }
    let payload = &bytes[ORV5_HEADER_BYTES..];
    match tag {
        0x00 | 0x01 | 0x09 => require_empty(tag, payload),
        0x02 => {
            require_fixed(tag, payload, 1)?;
            match payload[0] {
                0 | 1 => Ok(()),
                value => Err(InspectRowError::InvalidBoolean { value }),
            }
        }
        0x03 => require_fixed(tag, payload, 4),
        0x04..=0x05 => require_fixed(tag, payload, 8),
        0x06..=0x07 | 0x0a | 0x0c => Ok(()),
        0x08 => require_fixed(tag, payload, 16),
        0x0b => {
            if depth >= MAX_CONSTRUCTED_DEPTH {
                return Err(InspectRowError::ConstructedDepthExceeded);
            }
            validate_record_payload(payload, depth)
        }
        0x0d => validate_constructed_payload(type_id, payload, depth),
        tag => Err(InspectRowError::UnknownTag { tag }),
    }
}

fn require_empty(tag: u8, payload: &[u8]) -> Result<(), InspectRowError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(InspectRowError::WrongPayloadLength {
            tag,
            expected: 0,
            actual: payload.len(),
        })
    }
}

fn require_fixed(tag: u8, payload: &[u8], expected: usize) -> Result<(), InspectRowError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(InspectRowError::WrongPayloadLength {
            tag,
            expected,
            actual: payload.len(),
        })
    }
}

fn validate_record_payload(payload: &[u8], depth: usize) -> Result<(), InspectRowError> {
    if payload.len() < 4 {
        return Err(InspectRowError::TruncatedRecord {
            actual: payload.len(),
        });
    }
    let count = u32::from_be_bytes(payload[..4].try_into().expect("record count width")) as usize;
    let mut cursor = 4;
    for _ in 0..count {
        if payload.len().saturating_sub(cursor) < RECORD_FIELD_HEADER_BYTES {
            return Err(InspectRowError::TruncatedRecord {
                actual: payload.len().saturating_sub(cursor),
            });
        }
        cursor += 16;
        let length = u32::from_be_bytes(
            payload[cursor..cursor + 4]
                .try_into()
                .expect("record field length width"),
        ) as usize;
        cursor += 4;
        let remaining = payload.len().saturating_sub(cursor);
        if length < ORV5_HEADER_BYTES || length > remaining {
            return Err(InspectRowError::InvalidNestedLength {
                declared: length,
                actual: remaining,
            });
        }
        validate_orv5_value(&payload[cursor..cursor + length], depth + 1)?;
        cursor += length;
    }
    if cursor == payload.len() {
        Ok(())
    } else {
        Err(InspectRowError::TrailingBytes {
            declared: cursor,
            actual: payload.len(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Descriptor {
    Named,
    Reference,
    List(Box<Self>),
    Map(Box<Self>, Box<Self>),
    Option(Box<Self>),
}

fn validate_constructed_payload(
    type_id: TypeId,
    payload: &[u8],
    depth: usize,
) -> Result<(), InspectRowError> {
    if type_id.to_bytes() != [0; 16] {
        return Err(InspectRowError::ConstructedTypeIdentityNotZero);
    }
    if depth >= MAX_CONSTRUCTED_DEPTH {
        return Err(InspectRowError::ConstructedDepthExceeded);
    }
    if payload.len() < 2 {
        return Err(InspectRowError::TruncatedConstructedHeader);
    }
    let descriptor_length = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if descriptor_length == 0 {
        return Err(InspectRowError::EmptyConstructedDescriptor);
    }
    let descriptor_end = 2usize
        .checked_add(descriptor_length)
        .ok_or(InspectRowError::TruncatedConstructedDescriptor)?;
    if descriptor_end > payload.len() {
        return Err(InspectRowError::TruncatedConstructedDescriptor);
    }
    let mut cursor = 0;
    let descriptor = parse_descriptor(&payload[2..descriptor_end], &mut cursor, depth + 1)?;
    if cursor != descriptor_length {
        return Err(InspectRowError::TrailingConstructedDescriptor);
    }
    if matches!(descriptor, Descriptor::Named | Descriptor::Reference) {
        return Err(InspectRowError::UnsupportedConstructedRoot);
    }
    validate_constructor_content(&descriptor, &payload[descriptor_end..], depth + 1)
}

fn parse_descriptor(
    bytes: &[u8],
    cursor: &mut usize,
    depth: usize,
) -> Result<Descriptor, InspectRowError> {
    if depth > MAX_CONSTRUCTED_DEPTH {
        return Err(InspectRowError::ConstructedDepthExceeded);
    }
    let tag = *bytes
        .get(*cursor)
        .ok_or(InspectRowError::TruncatedConstructedDescriptor)?;
    *cursor += 1;
    match tag {
        0x00 | 0x01 => {
            let end = cursor
                .checked_add(16)
                .ok_or(InspectRowError::TruncatedConstructedDescriptor)?;
            if end > bytes.len() {
                return Err(InspectRowError::TruncatedConstructedDescriptor);
            }
            *cursor = end;
            Ok(if tag == 0x00 {
                Descriptor::Named
            } else {
                Descriptor::Reference
            })
        }
        0x02 => Ok(Descriptor::List(Box::new(parse_descriptor(
            bytes,
            cursor,
            depth + 1,
        )?))),
        0x03 => Ok(Descriptor::Map(
            Box::new(parse_descriptor(bytes, cursor, depth + 1)?),
            Box::new(parse_descriptor(bytes, cursor, depth + 1)?),
        )),
        0x04 => Ok(Descriptor::Option(Box::new(parse_descriptor(
            bytes,
            cursor,
            depth + 1,
        )?))),
        _ => Err(InspectRowError::UnknownDescriptorTag { tag }),
    }
}

fn validate_constructor_content(
    descriptor: &Descriptor,
    content: &[u8],
    depth: usize,
) -> Result<(), InspectRowError> {
    if depth > MAX_CONSTRUCTED_DEPTH {
        return Err(InspectRowError::ConstructedDepthExceeded);
    }
    match descriptor {
        Descriptor::Option(_child) => {
            let Some(&presence) = content.first() else {
                return Err(InspectRowError::TruncatedConstructedContent);
            };
            match presence {
                0 if content.len() == 1 => Ok(()),
                1 if content.len() >= 5 => {
                    let length = u32::from_be_bytes(content[1..5].try_into().expect("child length"))
                        as usize;
                    if length != content.len() - 5 {
                        return Err(InspectRowError::InvalidNestedLength {
                            declared: length,
                            actual: content.len() - 5,
                        });
                    }
                    validate_orv5_value(&content[5..], depth + 1)
                }
                _ => Err(InspectRowError::InvalidOptionPresence { value: presence }),
            }
        }
        Descriptor::List(child) => validate_repeated_content(child, content, depth),
        Descriptor::Map(key, value) => {
            if content.len() < 4 {
                return Err(InspectRowError::TruncatedConstructedContent);
            }
            let count = u32::from_be_bytes(content[..4].try_into().expect("map count")) as usize;
            let mut cursor = 4;
            for _ in 0..count {
                cursor = validate_child_at(content, cursor, depth, key)?;
                cursor = validate_child_at(content, cursor, depth, value)?;
            }
            if cursor == content.len() {
                Ok(())
            } else {
                Err(InspectRowError::TrailingBytes {
                    declared: cursor,
                    actual: content.len(),
                })
            }
        }
        Descriptor::Named | Descriptor::Reference => {
            Err(InspectRowError::UnsupportedConstructedRoot)
        }
    }
}

fn validate_repeated_content(
    child: &Descriptor,
    content: &[u8],
    depth: usize,
) -> Result<(), InspectRowError> {
    if content.len() < 4 {
        return Err(InspectRowError::TruncatedConstructedContent);
    }
    let count = u32::from_be_bytes(content[..4].try_into().expect("list count")) as usize;
    let mut cursor = 4;
    for _ in 0..count {
        cursor = validate_child_at(content, cursor, depth, child)?;
    }
    if cursor == content.len() {
        Ok(())
    } else {
        Err(InspectRowError::TrailingBytes {
            declared: cursor,
            actual: content.len(),
        })
    }
}

fn validate_child_at(
    content: &[u8],
    cursor: usize,
    depth: usize,
    _descriptor: &Descriptor,
) -> Result<usize, InspectRowError> {
    if content.len().saturating_sub(cursor) < 4 {
        return Err(InspectRowError::TruncatedConstructedContent);
    }
    let length = u32::from_be_bytes(
        content[cursor..cursor + 4]
            .try_into()
            .expect("child length width"),
    ) as usize;
    let value_start = cursor + 4;
    let remaining = content.len().saturating_sub(value_start);
    if length < ORV5_HEADER_BYTES || length > remaining {
        return Err(InspectRowError::InvalidNestedLength {
            declared: length,
            actual: remaining,
        });
    }
    validate_orv5_value(&content[value_start..value_start + length], depth + 1)?;
    Ok(value_start + length)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], InspectCarrierError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(InspectCarrierError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(InspectCarrierError::Truncated)?;
        self.position = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, InspectCarrierError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, InspectCarrierError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("width")))
    }
    fn u32(&mut self) -> Result<u32, InspectCarrierError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("width")))
    }
    fn u64(&mut self) -> Result<u64, InspectCarrierError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("width")))
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], InspectCarrierError> {
        Ok(self.take(N)?.try_into().expect("width"))
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectCarrierError {
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownProjectionTag(u8),
    Truncated,
    TrailingBytes,
    RowCountExceeded { actual: usize, maximum: usize },
    RowTooLarge { actual: usize, maximum: usize },
    NonCanonicalRowOrder,
    InvalidRow(InspectRowError),
    EnvelopeTooLarge { actual: usize, maximum: usize },
    TargetInvocationMismatch {
        expected: InvocationId,
        actual: InvocationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectRowError {
    InvalidMarker,
    TruncatedHeader {
        actual: usize,
    },
    UnknownTag {
        tag: u8,
    },
    PayloadTooLarge {
        actual: usize,
        maximum: usize,
    },
    TruncatedPayload {
        declared: usize,
        actual: usize,
    },
    TrailingBytes {
        declared: usize,
        actual: usize,
    },
    WrongPayloadLength {
        tag: u8,
        expected: usize,
        actual: usize,
    },
    InvalidBoolean {
        value: u8,
    },
    TruncatedRecord {
        actual: usize,
    },
    InvalidNestedLength {
        declared: usize,
        actual: usize,
    },
    ConstructedTypeIdentityNotZero,
    TruncatedConstructedHeader,
    EmptyConstructedDescriptor,
    TruncatedConstructedDescriptor,
    TrailingConstructedDescriptor,
    UnknownDescriptorTag {
        tag: u8,
    },
    UnsupportedConstructedRoot,
    ConstructedDepthExceeded,
    TruncatedConstructedContent,
    InvalidOptionPresence {
        value: u8,
    },
}

impl fmt::Display for InspectCarrierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("invalid ORNA-INSPECT/1 carrier magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported inspect carrier version {version}")
            }
            Self::UnknownProjectionTag(tag) => {
                write!(formatter, "unknown inspect carrier projection tag {tag}")
            }
            Self::Truncated => formatter.write_str("truncated inspect carrier envelope"),
            Self::TrailingBytes => {
                formatter.write_str("trailing bytes after inspect carrier envelope")
            }
            Self::RowCountExceeded { actual, maximum } => write!(
                formatter,
                "inspect carrier row count {actual} exceeds maximum {maximum}"
            ),
            Self::RowTooLarge { actual, maximum } => write!(
                formatter,
                "inspect carrier row length {actual} exceeds maximum {maximum}"
            ),
            Self::NonCanonicalRowOrder => {
                formatter.write_str("inspect carrier rows are not in canonical order")
            }
            Self::InvalidRow(error) => write!(formatter, "invalid ORV5 inspect row: {error}"),
            Self::EnvelopeTooLarge { actual, maximum } => write!(
                formatter,
                "inspect carrier envelope length {actual} exceeds maximum {maximum}"
            ),
            Self::TargetInvocationMismatch { expected, actual } => write!(
                formatter,
                "inspect carrier target invocation {actual} does not match provenance target {expected}"
            ),
        }
    }
}

impl Error for InspectCarrierError {}

impl fmt::Display for InspectRowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for InspectRowError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceRevisionId {
        SourceRevisionId::from_bytes([0x11; 16])
    }
    fn catalogue() -> CatalogueRevisionId {
        CatalogueRevisionId::from_bytes([0x22; 16])
    }

    fn row(tag: u8, type_id: [u8; 16], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ORV5_HEADER_BYTES + payload.len());
        bytes.extend_from_slice(ORV5_MARKER);
        bytes.push(tag);
        bytes.extend_from_slice(&type_id);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn integer_row(value: i32) -> Vec<u8> {
        row(0x03, [0; 16], &value.to_be_bytes())
    }

    #[test]
    fn all_projection_tags_and_type_identities_are_closed() {
        let projections = [
            InspectProjection::InvocationNodes,
            InspectProjection::Calls,
            InspectProjection::Resources,
            InspectProjection::StateCells,
            InspectProjection::UiNodes,
            InspectProjection::PresentationCandidates,
            InspectProjection::RuntimeBindings,
            InspectProjection::SecurityDecisions,
        ];
        for (index, projection) in projections.into_iter().enumerate() {
            assert_eq!(projection.tag(), index as u8 + 2);
            assert_eq!(
                InspectProjection::from_tag(projection.tag()),
                Some(projection)
            );
            assert_eq!(projection.carrier_kind().tag(), projection.tag());
            assert_eq!(
                InspectCarrierKind::from_type_id(projection.type_id()),
                Some(projection.carrier_kind())
            );
        }
        assert_eq!(InspectCarrierKind::Snapshot.tag(), 1);
        assert_eq!(
            InspectCarrierKind::from_tag(1),
            Some(InspectCarrierKind::Snapshot)
        );
        assert_eq!(InspectCarrierKind::from_tag(0), None);
        assert_eq!(InspectCarrierKind::from_tag(10), None);
    }

    #[test]
    fn every_carrier_kind_round_trips_with_empty_rows() {
        let kinds = [
            InspectCarrierKind::Snapshot,
            InspectCarrierKind::InvocationNodes,
            InspectCarrierKind::Calls,
            InspectCarrierKind::Resources,
            InspectCarrierKind::StateCells,
            InspectCarrierKind::UiNodes,
            InspectCarrierKind::PresentationCandidates,
            InspectCarrierKind::RuntimeBindings,
            InspectCarrierKind::SecurityDecisions,
        ];
        for kind in kinds {
            let carrier = InspectCarrierEnvelope::new(kind, 7, source(), catalogue(), vec![])
                .expect("empty rows are valid");
            let encoded = carrier.encode().expect("carrier encodes");
            assert_eq!(InspectCarrierEnvelope::decode(&encoded), Ok(carrier));
        }
    }

    #[test]
    fn primitive_and_opaque_rows_round_trip_with_revision_evidence() {
        let carrier = InspectCarrierEnvelope::new(
            InspectProjection::Calls,
            0x0102_0304_0506_0708,
            source(),
            catalogue(),
            vec![integer_row(1), row(0x0c, [0xaa; 16], b"opaque")],
        )
        .expect("canonical rows are accepted");
        let encoded = carrier.encode().expect("carrier encodes");
        assert!(encoded.starts_with(INSPECT_CARRIER_MAGIC));
        assert_eq!(encoded[15..17], INSPECT_CARRIER_VERSION.to_be_bytes());
        assert_eq!(encoded[17], InspectProjection::Calls.tag());
        assert_eq!(InspectCarrierEnvelope::decode(&encoded), Ok(carrier));
    }

    #[test]
    fn malformed_truncated_and_trailing_rows_fail_closed() {
        assert_eq!(
            validate_orv5_row_frame(b"row"),
            Err(InspectRowError::TruncatedHeader { actual: 3 })
        );
        let mut truncated = integer_row(1);
        truncated.pop();
        assert!(matches!(
            validate_orv5_row_frame(&truncated),
            Err(InspectRowError::TruncatedPayload { .. })
        ));
        let mut trailing = integer_row(1);
        trailing.push(0);
        assert!(matches!(
            validate_orv5_row_frame(&trailing),
            Err(InspectRowError::TrailingBytes { .. })
        ));
        let mut unknown = integer_row(1);
        unknown[4] = 0xff;
        assert_eq!(
            validate_orv5_row_frame(&unknown),
            Err(InspectRowError::UnknownTag { tag: 0xff })
        );
        let carrier = InspectCarrierEnvelope::new(
            InspectCarrierKind::Snapshot,
            1,
            source(),
            catalogue(),
            vec![integer_row(1)],
        )
        .expect("carrier");
        let encoded = carrier.encode().expect("encode");
        assert_eq!(
            InspectCarrierEnvelope::decode(&encoded[..encoded.len() - 1]),
            Err(InspectCarrierError::Truncated)
        );
        let mut envelope_trailing = encoded;
        envelope_trailing.push(0);
        assert_eq!(
            InspectCarrierEnvelope::decode(&envelope_trailing),
            Err(InspectCarrierError::TrailingBytes)
        );
    }

    #[test]
    fn canonical_row_order_and_bounds_are_enforced() {
        assert_eq!(
            InspectCarrierEnvelope::new(
                InspectProjection::Calls,
                1,
                source(),
                catalogue(),
                vec![integer_row(2), integer_row(1)]
            ),
            Err(InspectCarrierError::NonCanonicalRowOrder)
        );
        assert_eq!(
            InspectCarrierEnvelope::new(
                InspectProjection::Calls,
                1,
                source(),
                catalogue(),
                vec![integer_row(1), integer_row(1)]
            ),
            Err(InspectCarrierError::NonCanonicalRowOrder)
        );
        let mut encoded = InspectCarrierEnvelope::new(
            InspectCarrierKind::Snapshot,
            1,
            source(),
            catalogue(),
            vec![],
        )
        .expect("carrier")
        .encode()
        .expect("encode");
        encoded[58..62].copy_from_slice(&((MAX_INSPECT_CARRIER_ROWS as u32) + 1).to_be_bytes());
        assert_eq!(
            InspectCarrierEnvelope::decode(&encoded),
            Err(InspectCarrierError::RowCountExceeded {
                actual: MAX_INSPECT_CARRIER_ROWS + 1,
                maximum: MAX_INSPECT_CARRIER_ROWS
            })
        );
    }

    #[test]
    fn explicit_provenance_constructor_is_available() {
        let provenance = InspectCarrierProvenance::trusted(9, source(), catalogue());
        let carrier = InspectCarrierEnvelope::new_with_provenance(
            InspectCarrierKind::Snapshot,
            provenance,
            vec![],
        )
        .expect("trusted provenance");
        assert_eq!(carrier.provenance(), provenance);
        assert_eq!(carrier.server_epoch_id(), 9);
        assert_eq!(carrier.target_invocation_id(), None);
    }

    #[test]
    fn target_bound_provenance_round_trips_in_memory_and_rejects_mismatch() {
        let target = InvocationId::from_bytes([0x33; 16]);
        let other_target = InvocationId::from_bytes([0x44; 16]);
        let provenance = InspectCarrierProvenance::trusted_for_target(
            9,
            target,
            source(),
            catalogue(),
        );
        let carrier = InspectCarrierEnvelope::new_with_target(
            InspectCarrierKind::Snapshot,
            target,
            provenance,
            vec![],
        )
        .expect("matching target provenance");
        assert_eq!(carrier.provenance(), provenance);
        assert_eq!(carrier.target_invocation_id(), Some(target));
        assert_eq!(carrier.server_epoch_id(), 9);

        assert_eq!(
            InspectCarrierEnvelope::new_with_target(
                InspectCarrierKind::Snapshot,
                other_target,
                provenance,
                vec![],
            ),
            Err(InspectCarrierError::TargetInvocationMismatch {
                expected: target,
                actual: other_target,
            })
        );

        // The @1 wire envelope intentionally has no target/client-epoch
        // fields. Decoding preserves server epoch and revision evidence but
        // cannot claim a target binding that was not encoded.
        let decoded = InspectCarrierEnvelope::decode(&carrier.encode().expect("encode"))
            .expect("decode");
        assert_eq!(decoded.server_epoch_id(), carrier.server_epoch_id());
        assert_eq!(decoded.source_revision_id(), carrier.source_revision_id());
        assert_eq!(decoded.catalogue_revision_id(), carrier.catalogue_revision_id());
        assert_eq!(decoded.target_invocation_id(), None);
    }
}
