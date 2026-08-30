//! Backend-independent runtime values, typed function arguments, and ordered
//! SERVER results.
//!
//! This module defines the initial runtime subset only. General runtime-value
//! wire encoding remains a protocol concern; registered opaque contracts may
//! still enforce their own bounded, authority-free payload framing.

use std::{cmp::Ordering, error::Error, fmt};

use crate::{
    FieldId, ObjectId, ParameterId, TypeId,
    catalogue::{
        CatalogueSnapshot, QualifiedSemanticName, ValueTypeKind, ValueTypeMutability,
        ValueTypePersistence,
    },
    inspect_carrier::InspectCarrierEnvelope,
    revision::{
        ActiveDatabaseRevision, RecordValueFieldDescriptorClass, VerifiedStandardLibrarySnapshot,
        classify_record_value_field_descriptor,
    },
    system::{
        SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT, SYS_INSPECT_CALLS_TYPE_ID,
        SYS_INSPECT_CALLS_TYPE_NAME, SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT,
        SYS_INSPECT_INVOCATION_NODES_TYPE_ID, SYS_INSPECT_INVOCATION_NODES_TYPE_NAME,
        SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
        SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_NAME,
        SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT, SYS_INSPECT_RESOURCES_TYPE_ID,
        SYS_INSPECT_RESOURCES_TYPE_NAME, SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_RUNTIME_BINDINGS_TYPE_NAME,
        SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_TYPE_NAME,
        SYS_INSPECT_SNAPSHOT_REPRESENTATION_CONTRACT, SYS_INSPECT_SNAPSHOT_TYPE_ID,
        SYS_INSPECT_SNAPSHOT_TYPE_NAME, SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_STATE_CELLS_TYPE_NAME,
        SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT, SYS_INSPECT_UI_NODES_TYPE_ID,
        SYS_INSPECT_UI_NODES_TYPE_NAME, SYS_SOURCE_FUNCTION_TYPE_ID,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
};

mod opaque_codec;

pub use opaque_codec::{
    OpaqueCodecRegistration, OpaqueCodecRegistry, OpaqueCodecRegistryError, OpaqueValue,
    OpaqueValueError,
};

/// The maximum payload length accepted by every registered opaque codec.
pub const MAX_OPAQUE_CODEC_PAYLOAD_LENGTH: usize = 16 * 1024 * 1024;

/// The largest number of argument frames accepted by the generic action
/// descriptor codec. The semantic target and parameter checks remain in the
/// CLIENT action decoder.
pub const MAX_OPAQUE_CODEC_ACTION_ARGUMENTS: usize = 64;

const ACTION_DOMAIN_CLIENT: u8 = 1;
const ACTION_DOMAIN_SERVER: u8 = 2;
const ACTION_IDENTITY_BYTES: usize = 16;
const ACTION_IDENTITY_FIELDS: usize = 5;
const ACTION_BODY_PREFIX_BYTES: usize = 1 + (ACTION_IDENTITY_FIELDS * ACTION_IDENTITY_BYTES) + 4;
const ORV3_HEADER_BYTES: usize = 25;
const ORV3_MARKER: &[u8; 4] = b"ORV3";

/// The largest accepted framed-codec magic prefix length in bytes.
const MAX_OPAQUE_CODEC_MAGIC_LENGTH: usize = 64;

/// The largest accepted number of runtime-value nodes.
pub const MAX_RUNTIME_VALUE_NODES: usize = 65_536;
/// The exact ASCII magic prefix of the canonical `std.data.Rows` payload.
pub const ROWS_MAGIC: &[u8; 12] = b"ORNA-ROWS/1 ";

/// The only supported canonical `std.data.Rows` frame version.
pub const ROWS_FRAME_VERSION: u16 = 1;

/// The maximum number of ordered columns in one materialised Rows value.
pub const MAX_ROWS_COLUMNS: usize = 1_000_000;

/// The maximum number of ordered rows in one materialised Rows value.
pub const MAX_ROWS_ROWS: usize = 10_000;

/// The maximum number of cells in one materialised Rows value.
pub const MAX_ROWS_CELLS: usize = 1_000_000;

/// The maximum complete payload length of one materialised Rows value.
pub const MAX_ROWS_PAYLOAD_LENGTH: usize = MAX_OPAQUE_CODEC_PAYLOAD_LENGTH;

/// One immutable checked-in contract for a sealed `sys.inspect` carrier.
///
/// These registrations are deliberately separate from `OpaqueCodecRegistry`.
/// Inspector carriers are sealed system values and do not belong to an
/// application or verified standard-library catalogue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectCarrierCodecRegistration {
    opaque_type: TypeId,
    semantic_name: &'static str,
    representation_contract: &'static str,
}

impl InspectCarrierCodecRegistration {
    const fn new(
        opaque_type: TypeId,
        semantic_name: &'static str,
        representation_contract: &'static str,
    ) -> Self {
        Self {
            opaque_type,
            semantic_name,
            representation_contract,
        }
    }

    /// Returns the sealed carrier type identity.
    pub const fn opaque_type(self) -> TypeId {
        self.opaque_type
    }

    /// Returns the sealed carrier semantic name.
    pub const fn semantic_name(self) -> &'static str {
        self.semantic_name
    }

    /// Returns the immutable carrier representation contract.
    pub const fn representation_contract(self) -> &'static str {
        self.representation_contract
    }
}

/// The complete deterministic registration set for the nine sealed Inspector
/// snapshot and projection result carriers. No caller may extend this set.
pub const INSPECT_CARRIER_CODEC_REGISTRATIONS: &[InspectCarrierCodecRegistration] = &[
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_SNAPSHOT_TYPE_ID,
        SYS_INSPECT_SNAPSHOT_TYPE_NAME,
        SYS_INSPECT_SNAPSHOT_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        SYS_INSPECT_INVOCATION_NODES_TYPE_NAME,
        SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_CALLS_TYPE_ID,
        SYS_INSPECT_CALLS_TYPE_NAME,
        SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_RESOURCES_TYPE_ID,
        SYS_INSPECT_RESOURCES_TYPE_NAME,
        SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_STATE_CELLS_TYPE_ID,
        SYS_INSPECT_STATE_CELLS_TYPE_NAME,
        SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_UI_NODES_TYPE_ID,
        SYS_INSPECT_UI_NODES_TYPE_NAME,
        SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
        SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_NAME,
        SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_NAME,
        SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
    ),
    InspectCarrierCodecRegistration::new(
        SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
        SYS_INSPECT_SECURITY_DECISIONS_TYPE_NAME,
        SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
    ),
];

/// Returns the fixed registration for one sealed Inspector carrier type.
pub fn inspect_carrier_codec_by_type_id(
    opaque_type: TypeId,
) -> Option<InspectCarrierCodecRegistration> {
    INSPECT_CARRIER_CODEC_REGISTRATIONS
        .iter()
        .copied()
        .find(|registration| registration.opaque_type == opaque_type)
}

/// A borrowed exact type view of one runtime value.
///
/// `Flat` preserves the existing compatibility `ResolvedType`. `Constructed`
/// borrows the complete descriptor retained by a constructed runtime value.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeType<'a> {
    /// One existing flat runtime type.
    Flat(ResolvedType),
    /// One complete constructed descriptor borrowed from its runtime value.
    Constructed(&'a TypeDescriptor),
}

/// One immutable checked constructed runtime value.
#[derive(Clone, Debug)]
pub struct ConstructedValue {
    descriptor: TypeDescriptor,
    kind: ConstructedValueData,
    node_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum ConstructedValueData {
    Option(Option<Box<RuntimeValue>>),
    List(Vec<RuntimeValue>),
    Set(Vec<RuntimeValue>),
    Map(Vec<(RuntimeValue, RuntimeValue)>),
}

impl PartialEq for ConstructedValue {
    fn eq(&self, other: &Self) -> bool {
        self.descriptor == other.descriptor && self.kind == other.kind
    }
}

impl ConstructedValue {
    /// Returns the complete retained type descriptor.
    pub const fn descriptor(&self) -> &TypeDescriptor {
        &self.descriptor
    }

    /// Returns an immutable borrowed view of the constructed contents.
    pub fn kind(&self) -> ConstructedValueKind<'_> {
        match &self.kind {
            ConstructedValueData::Option(value) => ConstructedValueKind::Option(value.as_deref()),
            ConstructedValueData::List(values) => ConstructedValueKind::List(values),
            ConstructedValueData::Set(values) => ConstructedValueKind::Set(values),
            ConstructedValueData::Map(entries) => ConstructedValueKind::Map(entries),
        }
    }
}

/// An immutable borrowed view of one constructed runtime value.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConstructedValueKind<'a> {
    /// One optional child value.
    Option(Option<&'a RuntimeValue>),
    /// Ordered child values.
    List(&'a [RuntimeValue]),
    /// Canonically ordered unique child values.
    Set(&'a [RuntimeValue]),
    /// Canonically ordered key/value entries.
    Map(&'a [(RuntimeValue, RuntimeValue)]),
}

/// One immutable path through a collection descriptor or runtime value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionValuePath(Vec<CollectionValuePathSegment>);

impl CollectionValuePath {
    /// Returns the immutable ordered path segments.
    pub fn segments(&self) -> &[CollectionValuePathSegment] {
        &self.0
    }
}

/// One location in a collection descriptor or runtime value.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionValuePathSegment {
    /// The child of an optional runtime value.
    OptionChild,
    /// One ordered list runtime value.
    ListElement(usize),
    /// One canonical set runtime value.
    SetElement(usize),
    /// One map runtime key.
    MapKey(usize),
    /// One map runtime value.
    MapValue(usize),
    /// One declared immutable record field.
    RecordField(FieldId),
    /// The child descriptor of a list descriptor.
    ListChild,
    /// The child descriptor of a set descriptor.
    SetChild,
    /// The key descriptor of a map descriptor.
    MapKeyChild,
    /// The value descriptor of a map descriptor.
    MapValueChild,
}

/// One supported constructed runtime-value outer kind.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionKind {
    /// An optional value.
    Option,
    /// An ordered collection value.
    List,
    /// A logically unique collection value.
    Set,
    /// A key/value collection value.
    Map,
}

/// An error from checking one constructed runtime value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollectionValueError {
    /// The supplied descriptor has a different outer constructor.
    WrongConstructor {
        /// The required outer constructor.
        expected: CollectionKind,
        /// The complete supplied descriptor.
        descriptor: TypeDescriptor,
    },
    /// One complete descriptor is outside the admitted collection subset.
    UnsupportedDescriptor {
        /// The unsupported descriptor location.
        path: CollectionValuePath,
        /// The exact unsupported descriptor.
        descriptor: TypeDescriptor,
    },
    /// One named descriptor identity is present in both catalogue snapshots.
    AmbiguousNamedType {
        /// The ambiguous descriptor location.
        path: CollectionValuePath,
        /// The ambiguous type identity.
        type_id: TypeId,
    },
    /// A runtime value has more than the accepted number of nodes.
    TooManyNodes {
        /// The accepted maximum node count.
        maximum: usize,
    },
    /// A legacy typed null occurred in a constructed value.
    NullValueNotAccepted {
        /// The null runtime-value location.
        path: CollectionValuePath,
    },
    /// A runtime value does not have its exact declared type.
    ValueTypeMismatch {
        /// The mismatched runtime-value location.
        path: CollectionValuePath,
    },
    /// A runtime value is not active in the supplied revision.
    InactiveValue {
        /// The inactive runtime-value location.
        path: CollectionValuePath,
    },
    /// Two map keys compare equal after canonical ordering.
    DuplicateMapKey {
        /// The lower original input index of the selected equal pair.
        first: usize,
        /// The higher original input index of the selected equal pair.
        duplicate: usize,
    },
    /// Two set elements compare equal after canonical ordering.
    DuplicateSetElement {
        /// The lower original input index of the selected equal pair.
        first: usize,
        /// The higher original input index of the selected equal pair.
        duplicate: usize,
    },
}

impl fmt::Display for CollectionValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongConstructor { .. } => {
                formatter.write_str("collection descriptor has the wrong outer constructor")
            }
            Self::UnsupportedDescriptor { .. } => {
                formatter.write_str("collection descriptor is not supported")
            }
            Self::AmbiguousNamedType { .. } => formatter.write_str(
                "collection descriptor type is present in both application and standard catalogues",
            ),
            Self::TooManyNodes { .. } => formatter.write_str("runtime value has too many nodes"),
            Self::NullValueNotAccepted { .. } => {
                formatter.write_str("collection values cannot contain legacy typed NULL")
            }
            Self::ValueTypeMismatch { .. } => {
                formatter.write_str("collection value has a type mismatch")
            }
            Self::InactiveValue { .. } => formatter.write_str("collection value is not active"),
            Self::DuplicateMapKey { .. } => formatter.write_str("map contains a duplicate key"),
            Self::DuplicateSetElement { .. } => {
                formatter.write_str("set contains a duplicate element")
            }
        }
    }
}

impl Error for CollectionValueError {}

/// One checked core runtime value.
///
/// Existing function and SERVER result positions accept only their closed flat
/// runtime subsets.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeValue {
    /// A typed null value. The type remains available when the value is null.
    Null(NullValue),
    /// A BOOLEAN value.
    Boolean(bool),
    /// An INTEGER value.
    Integer(i32),
    /// A BIGINT value.
    BigInt(i64),
    /// A FLOAT value.
    Float(RuntimeFloat),
    /// A TEXT or CHARACTER LARGE OBJECT value.
    Text(String),
    /// A BYTES or BINARY LARGE OBJECT value.
    Bytes(Vec<u8>),
    /// A typed durable object reference.
    Reference { target: TypeId, object: ObjectId },
    /// A catalogue-validated enum value.
    Enum(EnumValue),
    /// A catalogue-validated named immutable record value.
    Record(RecordValue),
    /// A registered, catalogue-validated opaque value.
    Opaque(OpaqueValue),
    /// A checked immutable constructed runtime value.
    Constructed(ConstructedValue),
    /// One checked sealed invocation typed value.
    InvokeValue(crate::invocation::InvokeValue),
    /// One checked sealed root invocation request.
    InvokeRequest(crate::invocation::InvokeRequest),
    /// One checked sealed invocation event.
    InvokeEvent(crate::invocation::InvokeEvent),
}

impl RuntimeValue {
    /// Returns the exact runtime type without erasing a constructed descriptor.
    ///
    /// Constructed values retain their complete descriptor. Flat values retain
    /// their compatible resolved type.
    pub fn runtime_type(&self) -> RuntimeType<'_> {
        match self {
            Self::Null(value) => RuntimeType::Flat(value.resolved_type),
            Self::Boolean(_) => RuntimeType::Flat(ResolvedType::scalar(StandardScalar::Boolean)),
            Self::Integer(_) => RuntimeType::Flat(ResolvedType::scalar(StandardScalar::Integer)),
            Self::BigInt(_) => RuntimeType::Flat(ResolvedType::scalar(StandardScalar::BigInt)),
            Self::Float(_) => RuntimeType::Flat(ResolvedType::scalar(StandardScalar::Float)),
            Self::Text(_) => {
                RuntimeType::Flat(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
            }
            Self::Bytes(_) => {
                RuntimeType::Flat(ResolvedType::scalar(StandardScalar::BinaryLargeObject))
            }
            Self::Reference { target, .. } => RuntimeType::Flat(ResolvedType::reference(*target)),
            Self::Enum(value) => RuntimeType::Flat(ResolvedType::named(value.enum_type)),
            Self::Record(value) => RuntimeType::Flat(ResolvedType::named(value.record_type)),
            Self::Opaque(value) => RuntimeType::Flat(ResolvedType::value(value.opaque_type())),
            Self::Constructed(value) => RuntimeType::Constructed(value.descriptor()),
            Self::InvokeValue(_) => {
                RuntimeType::Flat(ResolvedType::value(crate::system::SYS_INVOKE_VALUE_TYPE_ID))
            }
            Self::InvokeRequest(_) => RuntimeType::Flat(ResolvedType::value(
                crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
            )),
            Self::InvokeEvent(_) => {
                RuntimeType::Flat(ResolvedType::value(crate::system::SYS_INVOKE_EVENT_TYPE_ID))
            }
        }
    }

    /// Creates a typed null in the initial supported runtime subset.
    pub fn null(resolved_type: ResolvedType) -> Result<Self, ResultRowsError> {
        require_supported_runtime_type(resolved_type)?;
        Ok(Self::Null(NullValue { resolved_type }))
    }

    /// Creates one checked immutable `OPTION` value.
    pub fn option(
        active: &ActiveDatabaseRevision,
        descriptor: TypeDescriptor,
        value: Option<RuntimeValue>,
    ) -> Result<Self, CollectionValueError> {
        if !matches!(descriptor.kind(), TypeDescriptorKind::Option(_)) {
            return Err(CollectionValueError::WrongConstructor {
                expected: CollectionKind::Option,
                descriptor,
            });
        }
        let mut path = Vec::new();
        preflight_collection_descriptor(active, &descriptor, &mut path)?;

        let mut node_count = 1;
        if let Some(value) = &value {
            count_runtime_value_nodes(value, &mut node_count)?;
        }

        let TypeDescriptorKind::Option(child) = descriptor.kind() else {
            unreachable!("checked option descriptor must retain its option child");
        };
        if let Some(value) = &value {
            path.push(CollectionValuePathSegment::OptionChild);
            validate_collection_runtime_value(active, child, value, &mut path)?;
        }

        Ok(Self::Constructed(ConstructedValue {
            descriptor,
            kind: ConstructedValueData::Option(value.map(Box::new)),
            node_count,
        }))
    }

    /// Creates one checked immutable `LIST` value.
    pub fn list(
        active: &ActiveDatabaseRevision,
        descriptor: TypeDescriptor,
        values: Vec<RuntimeValue>,
    ) -> Result<Self, CollectionValueError> {
        if !matches!(descriptor.kind(), TypeDescriptorKind::List(_)) {
            return Err(CollectionValueError::WrongConstructor {
                expected: CollectionKind::List,
                descriptor,
            });
        }
        let mut path = Vec::new();
        preflight_collection_descriptor(active, &descriptor, &mut path)?;

        let mut node_count = 1;
        check_runtime_value_lower_bound(values.len().checked_add(1))?;
        for value in &values {
            count_runtime_value_nodes(value, &mut node_count)?;
        }

        let TypeDescriptorKind::List(child) = descriptor.kind() else {
            unreachable!("checked list descriptor must retain its list child");
        };
        for (index, value) in values.iter().enumerate() {
            path.push(CollectionValuePathSegment::ListElement(index));
            validate_collection_runtime_value(active, child, value, &mut path)?;
            path.pop();
        }

        Ok(Self::Constructed(ConstructedValue {
            descriptor,
            kind: ConstructedValueData::List(values),
            node_count,
        }))
    }
    /// Creates one checked immutable canonically ordered `SET` value.
    pub fn set(
        active: &ActiveDatabaseRevision,
        descriptor: TypeDescriptor,
        values: Vec<RuntimeValue>,
    ) -> Result<Self, CollectionValueError> {
        if !matches!(descriptor.kind(), TypeDescriptorKind::Set(_)) {
            return Err(CollectionValueError::WrongConstructor {
                expected: CollectionKind::Set,
                descriptor,
            });
        }
        let mut path = Vec::new();
        preflight_collection_descriptor(active, &descriptor, &mut path)?;

        let mut node_count = 1;
        check_runtime_value_lower_bound(values.len().checked_add(1))?;
        for value in &values {
            count_runtime_value_nodes(value, &mut node_count)?;
        }

        let TypeDescriptorKind::Set(child) = descriptor.kind() else {
            unreachable!("checked set descriptor must retain its set child");
        };
        for (index, value) in values.iter().enumerate() {
            path.push(CollectionValuePathSegment::SetElement(index));
            validate_collection_runtime_value(active, child, value, &mut path)?;
            path.pop();
        }

        let mut indexed = values.into_iter().enumerate().collect::<Vec<_>>();
        indexed.sort_by(|(left_index, left), (right_index, right)| {
            compare_map_key(active, child, left, right).then(left_index.cmp(right_index))
        });
        for pair in indexed.windows(2) {
            let (left_index, left) = &pair[0];
            let (right_index, right) = &pair[1];
            if compare_map_key(active, child, left, right) == Ordering::Equal {
                return Err(CollectionValueError::DuplicateSetElement {
                    first: *left_index,
                    duplicate: *right_index,
                });
            }
        }
        let values = indexed.into_iter().map(|(_, value)| value).collect();

        Ok(Self::Constructed(ConstructedValue {
            descriptor,
            kind: ConstructedValueData::Set(values),
            node_count,
        }))
    }

    /// Creates one checked immutable canonically ordered `MAP` value.
    pub fn map(
        active: &ActiveDatabaseRevision,
        descriptor: TypeDescriptor,
        entries: Vec<(RuntimeValue, RuntimeValue)>,
    ) -> Result<Self, CollectionValueError> {
        if !matches!(descriptor.kind(), TypeDescriptorKind::Map { .. }) {
            return Err(CollectionValueError::WrongConstructor {
                expected: CollectionKind::Map,
                descriptor,
            });
        }
        let mut path = Vec::new();
        preflight_collection_descriptor(active, &descriptor, &mut path)?;

        let mut node_count = 1;
        check_runtime_value_lower_bound(
            entries
                .len()
                .checked_mul(2)
                .and_then(|count| count.checked_add(1)),
        )?;
        for (key, value) in &entries {
            count_runtime_value_nodes(key, &mut node_count)?;
            count_runtime_value_nodes(value, &mut node_count)?;
        }

        let TypeDescriptorKind::Map { key, value } = descriptor.kind() else {
            unreachable!("checked map descriptor must retain its map children");
        };
        for (index, (entry_key, entry_value)) in entries.iter().enumerate() {
            path.push(CollectionValuePathSegment::MapKey(index));
            validate_collection_runtime_value(active, key, entry_key, &mut path)?;
            path.pop();
            path.push(CollectionValuePathSegment::MapValue(index));
            validate_collection_runtime_value(active, value, entry_value, &mut path)?;
            path.pop();
        }

        let mut indexed = entries.into_iter().enumerate().collect::<Vec<_>>();
        indexed.sort_by(|(left_index, (left, _)), (right_index, (right, _))| {
            compare_map_key(active, key, left, right).then(left_index.cmp(right_index))
        });
        for pair in indexed.windows(2) {
            let (left_index, (left, _)) = &pair[0];
            let (right_index, (right, _)) = &pair[1];
            if compare_map_key(active, key, left, right) == Ordering::Equal {
                return Err(CollectionValueError::DuplicateMapKey {
                    first: *left_index,
                    duplicate: *right_index,
                });
            }
        }
        let entries = indexed.into_iter().map(|(_, entry)| entry).collect();

        Ok(Self::Constructed(ConstructedValue {
            descriptor,
            kind: ConstructedValueData::Map(entries),
            node_count,
        }))
    }

    /// Reports whether this value is null.
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null(_))
    }
}

fn collection_value_path(path: &[CollectionValuePathSegment]) -> CollectionValuePath {
    CollectionValuePath(path.to_vec())
}

fn too_many_runtime_value_nodes() -> CollectionValueError {
    CollectionValueError::TooManyNodes {
        maximum: MAX_RUNTIME_VALUE_NODES,
    }
}

fn add_runtime_value_nodes(
    total: &mut usize,
    additional: usize,
) -> Result<(), CollectionValueError> {
    let next = total
        .checked_add(additional)
        .ok_or_else(too_many_runtime_value_nodes)?;
    if next > MAX_RUNTIME_VALUE_NODES {
        return Err(too_many_runtime_value_nodes());
    }
    *total = next;
    Ok(())
}

fn check_runtime_value_lower_bound(minimum: Option<usize>) -> Result<(), CollectionValueError> {
    let minimum = minimum.ok_or_else(too_many_runtime_value_nodes)?;
    if minimum > MAX_RUNTIME_VALUE_NODES {
        return Err(too_many_runtime_value_nodes());
    }
    Ok(())
}

fn count_runtime_value_nodes(
    value: &RuntimeValue,
    total: &mut usize,
) -> Result<(), CollectionValueError> {
    match value {
        RuntimeValue::Constructed(value) => add_runtime_value_nodes(total, value.node_count),
        RuntimeValue::Record(value) => {
            add_runtime_value_nodes(total, 1)?;
            for field in value.fields() {
                count_runtime_value_nodes(field, total)?;
            }
            Ok(())
        }
        RuntimeValue::Null(_)
        | RuntimeValue::Boolean(_)
        | RuntimeValue::Integer(_)
        | RuntimeValue::BigInt(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Text(_)
        | RuntimeValue::Bytes(_)
        | RuntimeValue::Reference { .. }
        | RuntimeValue::Enum(_)
        | RuntimeValue::Opaque(_)
        | RuntimeValue::InvokeValue(_)
        | RuntimeValue::InvokeRequest(_)
        | RuntimeValue::InvokeEvent(_) => add_runtime_value_nodes(total, 1),
    }
}

/// Counts ordinary runtime nodes for one invocation carrier and rejects any
/// nested carrier before it can enter a checked carrier tree.
pub(crate) fn count_invocation_runtime_value_nodes(
    value: &RuntimeValue,
) -> Result<usize, crate::invocation::InvocationCarrierConstructionError> {
    use crate::invocation::{InvocationCarrierConstructionError, MAX_INVOCATION_CARRIER_NODES};

    fn add(total: &mut usize, additional: usize) -> Result<(), InvocationCarrierConstructionError> {
        let next = total.checked_add(additional).ok_or(
            InvocationCarrierConstructionError::TooManyNodes {
                maximum: MAX_INVOCATION_CARRIER_NODES,
            },
        )?;
        if next > MAX_INVOCATION_CARRIER_NODES {
            return Err(InvocationCarrierConstructionError::TooManyNodes {
                maximum: MAX_INVOCATION_CARRIER_NODES,
            });
        }
        *total = next;
        Ok(())
    }

    fn count(
        value: &RuntimeValue,
        total: &mut usize,
    ) -> Result<(), InvocationCarrierConstructionError> {
        match value {
            RuntimeValue::Constructed(value) => {
                add(total, 1)?;
                match value.kind() {
                    ConstructedValueKind::Option(value) => {
                        if let Some(value) = value {
                            count(value, total)?;
                        }
                    }
                    ConstructedValueKind::List(values) => {
                        for value in values {
                            count(value, total)?;
                        }
                    }
                    ConstructedValueKind::Set(values) => {
                        for value in values {
                            count(value, total)?;
                        }
                    }
                    ConstructedValueKind::Map(entries) => {
                        for (key, value) in entries {
                            count(key, total)?;
                            count(value, total)?;
                        }
                    }
                }
                Ok(())
            }
            RuntimeValue::Record(value) => {
                add(total, 1)?;
                for field in value.fields() {
                    count(field, total)?;
                }
                Ok(())
            }
            RuntimeValue::InvokeValue(_) => {
                Err(InvocationCarrierConstructionError::NestedCarrier {
                    carrier: crate::system::SYS_INVOKE_VALUE_TYPE_ID,
                })
            }
            RuntimeValue::InvokeRequest(_) => {
                Err(InvocationCarrierConstructionError::NestedCarrier {
                    carrier: crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
                })
            }
            RuntimeValue::InvokeEvent(_) => {
                Err(InvocationCarrierConstructionError::NestedCarrier {
                    carrier: crate::system::SYS_INVOKE_EVENT_TYPE_ID,
                })
            }
            RuntimeValue::Null(_)
            | RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
            | RuntimeValue::Enum(_)
            | RuntimeValue::Opaque(_) => add(total, 1),
        }
    }

    let mut total = 0;
    count(value, &mut total)?;
    Ok(total)
}

fn preflight_collection_descriptor(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), CollectionValueError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(_) => {
            classify_collection_named_descriptor(active, descriptor, path).map(|_| ())
        }
        TypeDescriptorKind::Reference(target) => {
            if active.catalogue().object_type_by_id(target).is_some()
                || target == crate::system::SYS_SECURITY_PRINCIPAL_TYPE_ID
            {
                Ok(())
            } else {
                Err(CollectionValueError::UnsupportedDescriptor {
                    path: collection_value_path(path),
                    descriptor: descriptor.clone(),
                })
            }
        }
        TypeDescriptorKind::Option(child) => {
            path.push(CollectionValuePathSegment::OptionChild);
            let result = preflight_collection_descriptor(active, child, path);
            if result.is_ok() {
                path.pop();
            }
            result
        }
        TypeDescriptorKind::List(child) => {
            path.push(CollectionValuePathSegment::ListChild);
            let result = preflight_collection_descriptor(active, child, path);
            if result.is_ok() {
                path.pop();
            }
            result
        }
        TypeDescriptorKind::Map { key, value } => {
            path.push(CollectionValuePathSegment::MapKeyChild);
            if matches!(
                key.kind(),
                TypeDescriptorKind::Named(type_id)
                    if type_id == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
            ) {
                return Err(CollectionValueError::UnsupportedDescriptor {
                    path: collection_value_path(path),
                    descriptor: key.clone(),
                });
            }
            if !matches!(
                key.kind(),
                TypeDescriptorKind::Named(_) | TypeDescriptorKind::Reference(_)
            ) {
                return Err(CollectionValueError::UnsupportedDescriptor {
                    path: collection_value_path(path),
                    descriptor: key.clone(),
                });
            }
            preflight_collection_descriptor(active, key, path)?;
            path.pop();
            path.push(CollectionValuePathSegment::MapValueChild);
            let result = if matches!(
                value.kind(),
                TypeDescriptorKind::Named(type_id)
                    if type_id == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
            ) {
                Err(CollectionValueError::UnsupportedDescriptor {
                    path: collection_value_path(path),
                    descriptor: value.clone(),
                })
            } else {
                preflight_collection_descriptor(active, value, path)
            };
            if result.is_ok() {
                path.pop();
            }
            result
        }
        TypeDescriptorKind::Set(child) => {
            if !path.is_empty()
                || !matches!(
                    child.kind(),
                    TypeDescriptorKind::Named(_) | TypeDescriptorKind::Reference(_)
                )
            {
                return Err(CollectionValueError::UnsupportedDescriptor {
                    path: collection_value_path(path),
                    descriptor: descriptor.clone(),
                });
            }
            path.push(CollectionValuePathSegment::SetChild);
            let result = preflight_collection_descriptor(active, child, path);
            if result.is_ok() {
                path.pop();
            }
            result
        }
        TypeDescriptorKind::Stream(_) => Err(CollectionValueError::UnsupportedDescriptor {
            path: collection_value_path(path),
            descriptor: descriptor.clone(),
        }),
    }
}

fn classify_collection_named_descriptor(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
    path: &[CollectionValuePathSegment],
) -> Result<RecordValueFieldDescriptorClass, CollectionValueError> {
    let TypeDescriptorKind::Named(type_id) = descriptor.kind() else {
        return Err(CollectionValueError::UnsupportedDescriptor {
            path: collection_value_path(path),
            descriptor: descriptor.clone(),
        });
    };
    if type_id == crate::system::SYS_SOURCE_FUNCTION_TYPE_ID
        && ((!path.is_empty()
            && path.iter().all(|segment| {
                matches!(
                    segment,
                    CollectionValuePathSegment::OptionChild | CollectionValuePathSegment::ListChild
                )
            }))
            || matches!(path, [CollectionValuePathSegment::SetChild]))
    {
        return Ok(RecordValueFieldDescriptorClass::SealedSourceMetadata);
    }
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(CollectionValueError::UnsupportedDescriptor {
            path: collection_value_path(path),
            descriptor: descriptor.clone(),
        });
    };
    let catalogue = active.catalogue();
    let standard = standard.catalogue();
    if catalogue.type_definition_by_id(type_id).is_some()
        && standard.type_definition_by_id(type_id).is_some()
    {
        return Err(CollectionValueError::AmbiguousNamedType {
            path: collection_value_path(path),
            type_id,
        });
    }
    classify_record_value_field_descriptor(catalogue, standard, descriptor).map_err(|_| {
        CollectionValueError::UnsupportedDescriptor {
            path: collection_value_path(path),
            descriptor: descriptor.clone(),
        }
    })
}

fn validate_collection_runtime_value(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
    value: &RuntimeValue,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), CollectionValueError> {
    if value.is_null() {
        return Err(CollectionValueError::NullValueNotAccepted {
            path: collection_value_path(path),
        });
    }

    match descriptor.kind() {
        TypeDescriptorKind::Named(_) => {
            let class =
                classify_collection_named_descriptor(active, descriptor, path).map_err(|_| {
                    CollectionValueError::InactiveValue {
                        path: collection_value_path(path),
                    }
                })?;
            let expected = if class == RecordValueFieldDescriptorClass::SealedSourceMetadata {
                ResolvedType::value(crate::system::SYS_SOURCE_FUNCTION_TYPE_ID)
            } else {
                let Some(expected) = active.record_value_field_descriptor_runtime_type(descriptor)
                else {
                    return Err(CollectionValueError::InactiveValue {
                        path: collection_value_path(path),
                    });
                };
                expected
            };
            if value.runtime_type() != RuntimeType::Flat(expected) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            match class {
                RecordValueFieldDescriptorClass::ApplicationEnum(type_id) => {
                    validate_collection_enum_label(active, type_id, value, path)
                }
                RecordValueFieldDescriptorClass::StandardEnum(type_id) => {
                    validate_collection_enum_label(active, type_id, value, path)
                }
                RecordValueFieldDescriptorClass::ApplicationRecord(_) => {
                    let RuntimeValue::Record(record) = value else {
                        return Err(CollectionValueError::ValueTypeMismatch {
                            path: collection_value_path(path),
                        });
                    };
                    validate_record_value_semantics(active, record, path)
                        .map_err(|failure| collection_error_from_record_failure(failure, path))
                }
                RecordValueFieldDescriptorClass::StandardPrimitive(_)
                | RecordValueFieldDescriptorClass::SealedSourceMetadata => Ok(()),
            }
        }
        TypeDescriptorKind::Reference(target) => {
            if value.runtime_type() != RuntimeType::Flat(ResolvedType::reference(target)) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            if active.catalogue().object_type_by_id(target).is_none()
                && target != crate::system::SYS_SECURITY_PRINCIPAL_TYPE_ID
            {
                return Err(CollectionValueError::InactiveValue {
                    path: collection_value_path(path),
                });
            }
            Ok(())
        }
        TypeDescriptorKind::Option(child) => {
            if value.runtime_type() != RuntimeType::Constructed(descriptor) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            let RuntimeValue::Constructed(value) = value else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            let ConstructedValueKind::Option(value) = value.kind() else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            if let Some(value) = value {
                path.push(CollectionValuePathSegment::OptionChild);
                let result = validate_collection_runtime_value(active, child, value, path);
                if result.is_ok() {
                    path.pop();
                }
                result?;
            }
            Ok(())
        }
        TypeDescriptorKind::List(child) => {
            if value.runtime_type() != RuntimeType::Constructed(descriptor) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            let RuntimeValue::Constructed(value) = value else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            let ConstructedValueKind::List(values) = value.kind() else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            for (index, value) in values.iter().enumerate() {
                path.push(CollectionValuePathSegment::ListElement(index));
                validate_collection_runtime_value(active, child, value, path)?;
                path.pop();
            }
            Ok(())
        }
        TypeDescriptorKind::Map {
            key,
            value: map_value,
        } => {
            if value.runtime_type() != RuntimeType::Constructed(descriptor) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            let RuntimeValue::Constructed(value) = value else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            let ConstructedValueKind::Map(entries) = value.kind() else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            for (index, (entry_key, entry_value)) in entries.iter().enumerate() {
                path.push(CollectionValuePathSegment::MapKey(index));
                validate_collection_runtime_value(active, key, entry_key, path)?;
                path.pop();
                path.push(CollectionValuePathSegment::MapValue(index));
                validate_collection_runtime_value(active, map_value, entry_value, path)?;
                path.pop();
            }
            Ok(())
        }
        TypeDescriptorKind::Set(child) => {
            if value.runtime_type() != RuntimeType::Constructed(descriptor) {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            }
            let RuntimeValue::Constructed(value) = value else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            let ConstructedValueKind::Set(values) = value.kind() else {
                return Err(CollectionValueError::ValueTypeMismatch {
                    path: collection_value_path(path),
                });
            };
            for (index, value) in values.iter().enumerate() {
                path.push(CollectionValuePathSegment::SetElement(index));
                validate_collection_runtime_value(active, child, value, path)?;
                path.pop();
            }
            Ok(())
        }
        TypeDescriptorKind::Stream(_) => Err(CollectionValueError::InactiveValue {
            path: collection_value_path(path),
        }),
    }
}

fn validate_collection_enum_label(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
    value: &RuntimeValue,
    path: &[CollectionValuePathSegment],
) -> Result<(), CollectionValueError> {
    let RuntimeValue::Enum(value) = value else {
        return Err(CollectionValueError::ValueTypeMismatch {
            path: collection_value_path(path),
        });
    };
    let standard = active
        .catalogue_hash_context()
        .standard()
        .map(VerifiedStandardLibrarySnapshot::catalogue);
    let definition = active
        .catalogue()
        .enum_type_by_id(type_id)
        .or_else(|| standard.and_then(|standard| standard.enum_type_by_id(type_id)));
    if definition.is_some_and(|definition| {
        definition
            .labels()
            .iter()
            .any(|label| label == value.label())
    }) {
        Ok(())
    } else {
        Err(CollectionValueError::InactiveValue {
            path: collection_value_path(path),
        })
    }
}

fn compare_map_key(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
    left: &RuntimeValue,
    right: &RuntimeValue,
) -> Ordering {
    match descriptor.kind() {
        TypeDescriptorKind::Reference(_) => {
            let (
                RuntimeValue::Reference { object: left, .. },
                RuntimeValue::Reference { object: right, .. },
            ) = (left, right)
            else {
                return Ordering::Equal;
            };
            left.to_bytes().cmp(&right.to_bytes())
        }
        TypeDescriptorKind::Named(_) => {
            let Some(standard) = active.catalogue_hash_context().standard() else {
                return Ordering::Equal;
            };
            let Ok(class) = classify_record_value_field_descriptor(
                active.catalogue(),
                standard.catalogue(),
                descriptor,
            ) else {
                return Ordering::Equal;
            };
            match class {
                RecordValueFieldDescriptorClass::ApplicationEnum(_)
                | RecordValueFieldDescriptorClass::StandardEnum(_) => {
                    let (RuntimeValue::Enum(left), RuntimeValue::Enum(right)) = (left, right)
                    else {
                        return Ordering::Equal;
                    };
                    left.label().as_bytes().cmp(right.label().as_bytes())
                }
                RecordValueFieldDescriptorClass::ApplicationRecord(type_id) => {
                    let (RuntimeValue::Record(left), RuntimeValue::Record(right)) = (left, right)
                    else {
                        return Ordering::Equal;
                    };
                    let Some(definition) = active.catalogue().record_value_type_by_id(type_id)
                    else {
                        return Ordering::Equal;
                    };
                    for (field, (left, right)) in definition
                        .fields()
                        .iter()
                        .zip(left.fields().iter().zip(right.fields()))
                    {
                        let ordering = compare_map_key(active, field.descriptor(), left, right);
                        if ordering != Ordering::Equal {
                            return ordering;
                        }
                    }
                    Ordering::Equal
                }
                RecordValueFieldDescriptorClass::StandardPrimitive(_) => {
                    compare_standard_primitive_map_key(left, right)
                }
                RecordValueFieldDescriptorClass::SealedSourceMetadata => {
                    let (RuntimeValue::Opaque(left), RuntimeValue::Opaque(right)) = (left, right)
                    else {
                        return Ordering::Equal;
                    };
                    left.canonical_payload().cmp(right.canonical_payload())
                }
            }
        }
        TypeDescriptorKind::List(_)
        | TypeDescriptorKind::Set(_)
        | TypeDescriptorKind::Map { .. }
        | TypeDescriptorKind::Option(_)
        | TypeDescriptorKind::Stream(_) => Ordering::Equal,
    }
}

fn compare_standard_primitive_map_key(left: &RuntimeValue, right: &RuntimeValue) -> Ordering {
    match (left, right) {
        (RuntimeValue::Boolean(left), RuntimeValue::Boolean(right)) => left.cmp(right),
        (RuntimeValue::Integer(left), RuntimeValue::Integer(right)) => left.cmp(right),
        (RuntimeValue::BigInt(left), RuntimeValue::BigInt(right)) => left.cmp(right),
        (RuntimeValue::Float(left), RuntimeValue::Float(right)) => left
            .value()
            .partial_cmp(&right.value())
            .unwrap_or(Ordering::Equal),
        (RuntimeValue::Text(left), RuntimeValue::Text(right)) => {
            left.as_bytes().cmp(right.as_bytes())
        }
        (RuntimeValue::Bytes(left), RuntimeValue::Bytes(right)) => left.cmp(right),
        _ => Ordering::Equal,
    }
}

/// One named immutable record value validated against an active catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordValue {
    record_type: TypeId,
    field_ids: Vec<FieldId>,
    fields: Vec<RuntimeValue>,
}

impl RecordValue {
    /// Validates a complete named field set and stores values in declaration order.
    pub fn new(
        active: &ActiveDatabaseRevision,
        record_type: TypeId,
        fields: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Result<Self, RecordValueError> {
        let catalogue = active.catalogue();
        let definition = catalogue
            .record_value_type_by_id(record_type)
            .ok_or(RecordValueError::UnknownType { record_type })?;
        let mut ordered = vec![None; definition.fields().len()];

        for (name, value) in fields {
            let field = definition
                .field_by_name(&name)
                .ok_or(RecordValueError::UnknownField { record_type, name })?;
            let index = usize::try_from(field.ordinal())
                .expect("validated record field ordinal must fit usize");
            if ordered[index].is_some() {
                return Err(RecordValueError::DuplicateField {
                    record_type,
                    field: field.id(),
                });
            }
            if value.is_null() {
                return Err(RecordValueError::NullField {
                    record_type,
                    field: field.id(),
                });
            }
            let descriptor = field.descriptor();
            let expected = active
                .record_value_field_descriptor_runtime_type(descriptor)
                .ok_or_else(|| RecordValueError::UnsupportedFieldType {
                    record_type,
                    field: field.id(),
                    descriptor: descriptor.clone(),
                })?;
            if let RuntimeValue::Constructed(value) = &value {
                return Err(RecordValueError::ConstructedValueNotAccepted {
                    record_type,
                    field: field.id(),
                    descriptor: value.descriptor().clone(),
                });
            }
            let RuntimeType::Flat(actual) = value.runtime_type() else {
                unreachable!("non-constructed runtime values have a flat runtime type");
            };
            if actual != expected {
                return Err(RecordValueError::FieldTypeMismatch {
                    record_type,
                    field: field.id(),
                    expected,
                    actual,
                });
            }
            let mut path = vec![CollectionValuePathSegment::RecordField(field.id())];
            if let Err(failure) =
                validate_record_field_semantics(active, descriptor, &value, &mut path)
            {
                return Err(record_value_error_from_field_semantic_failure(
                    active,
                    record_type,
                    field.id(),
                    descriptor,
                    &value,
                    failure,
                ));
            }
            ordered[index] = Some(value);
        }

        let fields = definition
            .fields()
            .iter()
            .zip(ordered)
            .map(|(field, value)| {
                value.ok_or(RecordValueError::MissingField {
                    record_type,
                    field: field.id(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let value = Self {
            record_type,
            field_ids: definition.fields().iter().map(|field| field.id()).collect(),
            fields,
        };
        Ok(value)
    }

    /// Returns the stable identity of the nominal record type.
    pub const fn record_type(&self) -> TypeId {
        self.record_type
    }

    /// Returns values in declaration ordinal order.
    pub fn fields(&self) -> &[RuntimeValue] {
        &self.fields
    }
}

fn application_record_field_target(
    catalogue: &CatalogueSnapshot,
    descriptor: &TypeDescriptor,
) -> Option<TypeId> {
    let crate::types::TypeDescriptorKind::Named(type_id) = descriptor.kind() else {
        return None;
    };
    catalogue
        .record_value_type_by_id(type_id)
        .map(|definition| definition.id())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordValueSemanticFailure {
    Null,
    TypeMismatch,
    Inactive,
}

fn validate_record_value_semantics(
    active: &ActiveDatabaseRevision,
    value: &RecordValue,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), RecordValueSemanticFailure> {
    let catalogue = active.catalogue();
    let Some(definition) = catalogue.record_value_type_by_id(value.record_type) else {
        return Err(RecordValueSemanticFailure::Inactive);
    };
    for (index, field) in definition.fields().iter().enumerate() {
        if value.field_ids.get(index) != Some(&field.id()) || value.fields.get(index).is_none() {
            path.push(CollectionValuePathSegment::RecordField(field.id()));
            return Err(RecordValueSemanticFailure::Inactive);
        }
    }
    let current_field_count = definition.fields().len();
    if value.field_ids.len() > current_field_count || value.fields.len() > current_field_count {
        let retained_field = value
            .field_ids
            .get(current_field_count)
            .expect("record values retain one field identity for each retained field value");
        path.push(CollectionValuePathSegment::RecordField(*retained_field));
        return Err(RecordValueSemanticFailure::Inactive);
    }
    if value.field_ids.len() != current_field_count || value.fields.len() != current_field_count {
        return Err(RecordValueSemanticFailure::Inactive);
    }

    for (field, field_value) in definition.fields().iter().zip(&value.fields) {
        path.push(CollectionValuePathSegment::RecordField(field.id()));
        let result = validate_record_field_semantics(active, field.descriptor(), field_value, path);
        if result.is_ok() {
            path.pop();
        }
        result?;
    }
    Ok(())
}

fn validate_record_field_semantics(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
    value: &RuntimeValue,
    path: &mut Vec<CollectionValuePathSegment>,
) -> Result<(), RecordValueSemanticFailure> {
    if value.is_null() {
        return Err(RecordValueSemanticFailure::Null);
    }
    let Some(expected) = active.record_value_field_descriptor_runtime_type(descriptor) else {
        return Err(RecordValueSemanticFailure::Inactive);
    };
    if value.runtime_type() != RuntimeType::Flat(expected) {
        return Err(RecordValueSemanticFailure::TypeMismatch);
    }

    if let Some(nested_record_type) =
        application_record_field_target(active.catalogue(), descriptor)
    {
        let RuntimeValue::Record(nested) = value else {
            return Err(RecordValueSemanticFailure::TypeMismatch);
        };
        if nested.record_type() != nested_record_type {
            return Err(RecordValueSemanticFailure::TypeMismatch);
        }
        return validate_record_value_semantics(active, nested, path);
    }
    if matches!(value, RuntimeValue::Record(_)) {
        return Err(RecordValueSemanticFailure::TypeMismatch);
    }
    if let RuntimeValue::Enum(enum_value) = value {
        let Some(standard) = active.catalogue_hash_context().standard() else {
            return Err(RecordValueSemanticFailure::Inactive);
        };
        let active_enum = active
            .catalogue()
            .enum_type_by_id(enum_value.enum_type())
            .or_else(|| standard.catalogue().enum_type_by_id(enum_value.enum_type()));
        if !active_enum.is_some_and(|enum_type| {
            enum_type
                .labels()
                .iter()
                .any(|label| label == enum_value.label())
        }) {
            return Err(RecordValueSemanticFailure::Inactive);
        }
    }
    Ok(())
}

fn collection_error_from_record_failure(
    failure: RecordValueSemanticFailure,
    path: &[CollectionValuePathSegment],
) -> CollectionValueError {
    match failure {
        RecordValueSemanticFailure::Null => CollectionValueError::NullValueNotAccepted {
            path: collection_value_path(path),
        },
        RecordValueSemanticFailure::TypeMismatch => CollectionValueError::ValueTypeMismatch {
            path: collection_value_path(path),
        },
        RecordValueSemanticFailure::Inactive => CollectionValueError::InactiveValue {
            path: collection_value_path(path),
        },
    }
}

fn record_value_error_from_field_semantic_failure(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    field: FieldId,
    descriptor: &TypeDescriptor,
    value: &RuntimeValue,
    failure: RecordValueSemanticFailure,
) -> RecordValueError {
    if let RuntimeValue::Enum(enum_value) = value
        && matches!(failure, RecordValueSemanticFailure::Inactive)
    {
        return RecordValueError::InactiveEnumLabel {
            record_type,
            field,
            enum_type: enum_value.enum_type(),
            label: enum_value.label().to_owned(),
        };
    }
    if matches!(failure, RecordValueSemanticFailure::Inactive)
        && let Some(nested_record_type) =
            application_record_field_target(active.catalogue(), descriptor)
    {
        return RecordValueError::InactiveNestedRecord {
            record_type,
            field,
            nested_record_type,
        };
    }

    match failure {
        RecordValueSemanticFailure::Null => RecordValueError::NullField { record_type, field },
        RecordValueSemanticFailure::TypeMismatch => RecordValueError::FieldTypeMismatch {
            record_type,
            field,
            expected: active
                .record_value_field_descriptor_runtime_type(descriptor)
                .expect("a newly constructed field descriptor must remain admitted"),
            actual: match value.runtime_type() {
                RuntimeType::Flat(actual) => actual,
                RuntimeType::Constructed(_) => {
                    unreachable!("constructed values are rejected before record semantic checks")
                }
            },
        },
        RecordValueSemanticFailure::Inactive => RecordValueError::UnsupportedFieldType {
            record_type,
            field,
            descriptor: descriptor.clone(),
        },
    }
}

/// An error from validating a named immutable record value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordValueError {
    /// The active catalogue does not contain the supplied record type.
    UnknownType {
        /// The unknown record type identity.
        record_type: TypeId,
    },
    /// The active record type does not declare the supplied exact field name.
    UnknownField {
        /// The active record type identity.
        record_type: TypeId,
        /// The unknown exact field name.
        name: String,
    },
    /// One declared field was supplied more than once.
    DuplicateField {
        /// The active record type identity.
        record_type: TypeId,
        /// The duplicated field identity.
        field: FieldId,
    },
    /// One required declared field was not supplied.
    MissingField {
        /// The active record type identity.
        record_type: TypeId,
        /// The missing field identity.
        field: FieldId,
    },
    /// A record field was supplied as a typed null value.
    NullField {
        /// The active record type identity.
        record_type: TypeId,
        /// The field that received NULL.
        field: FieldId,
    },
    /// A constructed value is outside the current record-field subset.
    ConstructedValueNotAccepted {
        /// The active record type identity.
        record_type: TypeId,
        /// The field that received the constructed value.
        field: FieldId,
        /// The rejected complete constructed descriptor.
        descriptor: TypeDescriptor,
    },
    /// A declared field type is not available through the selected context.
    UnsupportedFieldType {
        /// The active record type identity.
        record_type: TypeId,
        /// The unsupported field identity.
        field: FieldId,
        /// The unsupported declared descriptor.
        descriptor: TypeDescriptor,
    },
    /// A field value does not have the exact declared runtime type.
    FieldTypeMismatch {
        /// The active record type identity.
        record_type: TypeId,
        /// The mismatched field identity.
        field: FieldId,
        /// The runtime type required by the declaration.
        expected: ResolvedType,
        /// The runtime type supplied by the caller.
        actual: ResolvedType,
    },
    /// A nested record field value is not valid for the active revision.
    InactiveNestedRecord {
        /// The active record type identity.
        record_type: TypeId,
        /// The nested record field identity.
        field: FieldId,
        /// The declared nested record type identity.
        nested_record_type: TypeId,
    },
    /// An enum field label is not present in the active enum definition.
    InactiveEnumLabel {
        /// The active record type identity.
        record_type: TypeId,
        /// The enum field identity.
        field: FieldId,
        /// The active enum type identity.
        enum_type: TypeId,
        /// The inactive label supplied by the caller.
        label: String,
    },
}

impl fmt::Display for RecordValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { .. } => formatter.write_str("record value type is not active"),
            Self::UnknownField { .. } => {
                formatter.write_str("record field is not declared by the active type")
            }
            Self::DuplicateField { .. } => formatter.write_str("record field is duplicated"),
            Self::MissingField { .. } => formatter.write_str("record field is missing"),
            Self::NullField { .. } => formatter.write_str("record field cannot be NULL"),
            Self::ConstructedValueNotAccepted { .. } => {
                formatter.write_str("constructed record field values are not accepted")
            }
            Self::UnsupportedFieldType { .. } => {
                formatter.write_str("record field type is not available in the active context")
            }
            Self::FieldTypeMismatch { .. } => {
                formatter.write_str("record field value has a type mismatch")
            }
            Self::InactiveNestedRecord { .. } => {
                formatter.write_str("nested record field value is not active")
            }
            Self::InactiveEnumLabel { .. } => {
                formatter.write_str("record enum field label is not active")
            }
        }
    }
}

impl Error for RecordValueError {}

/// One enum label validated against an active catalogue snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumValue {
    enum_type: TypeId,
    label: String,
}

impl EnumValue {
    /// Creates an enum value only when the active type declares the exact label.
    pub fn new(
        catalogue: &CatalogueSnapshot,
        enum_type: TypeId,
        label: impl Into<String>,
    ) -> Result<Self, EnumValueError> {
        let definition = catalogue
            .enum_type_by_id(enum_type)
            .ok_or(EnumValueError::UnknownType { enum_type })?;
        let label = label.into();
        if !definition
            .labels()
            .iter()
            .any(|declared| declared == &label)
        {
            return Err(EnumValueError::UndeclaredLabel { enum_type, label });
        }
        Ok(Self { enum_type, label })
    }

    /// Returns the stable identity of the declaring enum type.
    pub const fn enum_type(&self) -> TypeId {
        self.enum_type
    }

    /// Returns the exact declared label.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// An error from validating an enum runtime value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumValueError {
    /// The active catalogue does not contain the supplied enum type.
    UnknownType {
        /// The unknown enum type identity.
        enum_type: TypeId,
    },
    /// The active enum type does not declare the supplied exact label.
    UndeclaredLabel {
        /// The active enum type identity.
        enum_type: TypeId,
        /// The undeclared label.
        label: String,
    },
}

impl fmt::Display for EnumValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { .. } => formatter.write_str("enum type is not active"),
            Self::UndeclaredLabel { .. } => {
                formatter.write_str("enum label is not declared by the active type")
            }
        }
    }
}

impl Error for EnumValueError {}

/// One typed argument supplied to a server function.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionArgument {
    parameter: ParameterId,
    value: RuntimeValue,
}

impl FunctionArgument {
    /// Creates one argument, rejecting typed null values.
    pub fn new(parameter: ParameterId, value: RuntimeValue) -> Result<Self, FunctionArgumentError> {
        match &value {
            RuntimeValue::Null(null) => Err(FunctionArgumentError::NullValue {
                parameter,
                resolved_type: null.resolved_type(),
            }),
            RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
            | RuntimeValue::Enum(_)
            | RuntimeValue::Opaque(_) => Ok(Self { parameter, value }),
            RuntimeValue::Constructed(value) => {
                Err(FunctionArgumentError::ConstructedValueNotAccepted {
                    parameter,
                    descriptor: value.descriptor().clone(),
                })
            }
            RuntimeValue::Record(value) => Err(FunctionArgumentError::RecordValueNotAccepted {
                parameter,
                record_type: value.record_type(),
            }),
            RuntimeValue::InvokeValue(_)
            | RuntimeValue::InvokeRequest(_)
            | RuntimeValue::InvokeEvent(_) => {
                Err(FunctionArgumentError::InvocationCarrierNotAccepted {
                    parameter,
                    carrier: crate::invocation::invocation_carrier_kind(&value)
                        .expect("invocation runtime values have a carrier kind"),
                })
            }
        }
    }

    /// Returns the parameter identity bound to this argument.
    pub const fn parameter(&self) -> ParameterId {
        self.parameter
    }

    /// Returns the runtime value bound to this argument.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }
}

/// An error from constructing a typed server-function argument.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionArgumentError {
    /// A typed null value is not a valid function argument in this slice.
    NullValue {
        /// The parameter identity supplied with the null value.
        parameter: ParameterId,
        /// The resolved type carried by the null value.
        resolved_type: ResolvedType,
    },
    /// A constructed value is outside the current executable argument subset.
    ConstructedValueNotAccepted {
        /// The parameter identity supplied with the constructed value.
        parameter: ParameterId,
        /// The rejected complete constructed descriptor.
        descriptor: TypeDescriptor,
    },
    /// A record value is outside the current executable argument subset.
    RecordValueNotAccepted {
        /// The parameter identity supplied with the record value.
        parameter: ParameterId,
        /// The record type carried by the value.
        record_type: TypeId,
    },
    /// A sealed invocation carrier is outside ordinary function arguments.
    InvocationCarrierNotAccepted {
        /// The parameter identity supplied with the carrier.
        parameter: ParameterId,
        /// The rejected carrier kind.
        carrier: crate::system::InvocationCarrierKind,
    },
}

impl fmt::Display for FunctionArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NullValue { .. } => formatter.write_str("function argument value cannot be NULL"),
            Self::ConstructedValueNotAccepted { .. } => {
                formatter.write_str("constructed function arguments are not accepted")
            }
            Self::RecordValueNotAccepted { .. } => {
                formatter.write_str("record function arguments are not accepted")
            }
            Self::InvocationCarrierNotAccepted { .. } => {
                formatter.write_str("invocation carrier function arguments are not accepted")
            }
        }
    }
}

impl Error for FunctionArgumentError {}

/// An opaque typed null value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NullValue {
    resolved_type: ResolvedType,
}

impl NullValue {
    /// Returns the exact supported type carried by this null value.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
}

/// A finite FLOAT value with reflexive numeric equality.
///
/// `+0.0` and `-0.0` compare equal. Non-finite IEEE values are not runtime
/// values in this initial subset.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFloat(f64);

impl RuntimeFloat {
    /// Creates one finite FLOAT value.
    pub fn new(value: f64) -> Result<Self, ResultRowsError> {
        if !value.is_finite() {
            return Err(ResultRowsError::NonFiniteFloat);
        }
        Ok(Self(value))
    }

    /// Returns the finite floating-point value.
    pub const fn value(&self) -> f64 {
        self.0
    }
}

impl PartialEq for RuntimeFloat {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// One ordered result column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultColumn {
    name: String,
    resolved_type: ResolvedType,
    nullable: bool,
}

impl ResultColumn {
    /// Creates one result column in the initial supported runtime subset.
    pub fn new(
        name: impl Into<String>,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> Result<Self, ResultRowsError> {
        let name = name.into();
        if name.is_empty() {
            return Err(ResultRowsError::EmptyColumnName);
        }
        require_supported_runtime_type(resolved_type)?;
        Ok(Self {
            name,
            resolved_type,
            nullable,
        })
    }

    /// Returns the exact result column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the resolved result type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    /// Reports whether this result column accepts null values.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// One ordered result row before result-set validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRow {
    values: Vec<RuntimeValue>,
}

impl ResultRow {
    /// Creates one ordered row. [`ResultRows::new`] validates it against columns.
    pub fn new(values: impl IntoIterator<Item = RuntimeValue>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    /// Returns values in result-column order.
    pub fn values(&self) -> &[RuntimeValue] {
        &self.values
    }

    /// Transfers values in result-column order without cloning their payloads.
    pub fn into_values(self) -> Vec<RuntimeValue> {
        self.values
    }
}

/// A validated ordered set of SERVER query result rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRows {
    columns: Vec<ResultColumn>,
    rows: Vec<ResultRow>,
}

impl ResultRows {
    /// Validates and creates one ordered result set.
    pub fn new(
        columns: impl IntoIterator<Item = ResultColumn>,
        rows: impl IntoIterator<Item = ResultRow>,
    ) -> Result<Self, ResultRowsError> {
        let columns = columns.into_iter().collect::<Vec<_>>();
        validate_columns(&columns)?;

        let rows = rows.into_iter().collect::<Vec<_>>();
        for (row_index, row) in rows.iter().enumerate() {
            if row.values.len() != columns.len() {
                return Err(ResultRowsError::RowWidthMismatch {
                    row: row_index,
                    expected: columns.len(),
                    actual: row.values.len(),
                });
            }
            for (column_index, (column, value)) in columns.iter().zip(&row.values).enumerate() {
                if let RuntimeValue::Opaque(value) = value {
                    return Err(ResultRowsError::OpaqueValueNotAccepted {
                        row: row_index,
                        column: column_index,
                        opaque_type: value.opaque_type(),
                    });
                }
                if let Some(carrier) = crate::invocation::invocation_carrier_kind(value) {
                    return Err(ResultRowsError::InvocationCarrierNotAccepted {
                        row: row_index,
                        column: column_index,
                        carrier,
                    });
                }
                if value.is_null() && !column.nullable {
                    return Err(ResultRowsError::NullInNonNullableColumn {
                        row: row_index,
                        column: column_index,
                    });
                }
                if let RuntimeValue::Constructed(value) = value {
                    return Err(ResultRowsError::ConstructedValueNotAccepted {
                        row: row_index,
                        column: column_index,
                        descriptor: value.descriptor().clone(),
                    });
                }
                let RuntimeType::Flat(actual) = value.runtime_type() else {
                    unreachable!("constructed values are rejected before result type validation");
                };
                if actual != column.resolved_type {
                    return Err(ResultRowsError::ValueTypeMismatch {
                        row: row_index,
                        column: column_index,
                        expected: column.resolved_type,
                        actual,
                    });
                }
            }
        }

        Ok(Self { columns, rows })
    }

    /// Returns result columns in their declared order.
    pub fn columns(&self) -> &[ResultColumn] {
        &self.columns
    }

    /// Returns rows in query result order.
    pub fn rows(&self) -> &[ResultRow] {
        &self.rows
    }

    /// Transfers rows in query result order without cloning their payloads.
    pub fn into_rows(self) -> Vec<ResultRow> {
        self.rows
    }
}

/// A structured error from runtime result construction.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultRowsError {
    /// A result set has no result columns.
    EmptyColumns,
    /// A result column name is empty.
    EmptyColumnName,
    /// A type has no representation in the initial runtime subset.
    UnsupportedRuntimeType { resolved_type: ResolvedType },
    /// A FLOAT value is not finite.
    NonFiniteFloat,
    /// Two result columns have the same exact name.
    DuplicateColumnName {
        first: usize,
        duplicate: usize,
        name: String,
    },
    /// A row does not have exactly one value per result column.
    RowWidthMismatch {
        row: usize,
        expected: usize,
        actual: usize,
    },
    /// A null value occurred in a non-nullable result column.
    NullInNonNullableColumn { row: usize, column: usize },
    /// A value type does not equal its result-column type.
    ValueTypeMismatch {
        row: usize,
        column: usize,
        expected: ResolvedType,
        actual: ResolvedType,
    },
    /// A constructed value occurred in a SERVER result row.
    ConstructedValueNotAccepted {
        /// The zero-based result-row position.
        row: usize,
        /// The zero-based result-column position.
        column: usize,
        /// The rejected complete constructed descriptor.
        descriptor: TypeDescriptor,
    },
    /// An opaque value occurred in a SERVER result row.
    OpaqueValueNotAccepted {
        /// The zero-based result-row position.
        row: usize,
        /// The zero-based result-column position.
        column: usize,
        /// The rejected opaque type identity.
        opaque_type: TypeId,
    },
    /// A sealed invocation carrier occurred in a SERVER result row.
    InvocationCarrierNotAccepted {
        /// The zero-based result-row position.
        row: usize,
        /// The zero-based result-column position.
        column: usize,
        /// The rejected carrier kind.
        carrier: crate::system::InvocationCarrierKind,
    },
}

impl fmt::Display for ResultRowsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyColumns => formatter.write_str("result set has no columns"),
            Self::EmptyColumnName => formatter.write_str("result column name is empty"),
            Self::UnsupportedRuntimeType { .. } => {
                formatter.write_str("type is not supported by the runtime subset")
            }
            Self::NonFiniteFloat => formatter.write_str("FLOAT value must be finite"),
            Self::DuplicateColumnName {
                first,
                duplicate,
                name,
            } => write!(
                formatter,
                "result column {duplicate} duplicates result column {first}: {name}"
            ),
            Self::RowWidthMismatch {
                row,
                expected,
                actual,
            } => write!(
                formatter,
                "result row {row} has {actual} values; expected {expected}"
            ),
            Self::NullInNonNullableColumn { row, column } => {
                write!(formatter, "result row {row} column {column} cannot be null")
            }
            Self::ValueTypeMismatch {
                row,
                column,
                expected: _,
                actual: _,
            } => write!(
                formatter,
                "result row {row} column {column} has a type mismatch"
            ),
            Self::ConstructedValueNotAccepted { .. } => {
                formatter.write_str("constructed SERVER result values are not accepted")
            }
            Self::OpaqueValueNotAccepted { row, column, .. } => {
                write!(
                    formatter,
                    "result row {row} column {column} cannot contain an opaque value"
                )
            }
            Self::InvocationCarrierNotAccepted { row, column, .. } => {
                write!(
                    formatter,
                    "result row {row} column {column} cannot contain an invocation carrier"
                )
            }
        }
    }
}

impl Error for ResultRowsError {}

fn validate_columns(columns: &[ResultColumn]) -> Result<(), ResultRowsError> {
    if columns.is_empty() {
        return Err(ResultRowsError::EmptyColumns);
    }
    for (index, column) in columns.iter().enumerate() {
        for (first, earlier) in columns[..index].iter().enumerate() {
            if earlier.name == column.name {
                return Err(ResultRowsError::DuplicateColumnName {
                    first,
                    duplicate: index,
                    name: column.name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn require_supported_runtime_type(resolved_type: ResolvedType) -> Result<(), ResultRowsError> {
    if supports_runtime_value(resolved_type) {
        Ok(())
    } else {
        Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
    }
}

const fn supports_runtime_value(resolved_type: ResolvedType) -> bool {
    resolved_type.reference_target().is_some()
        || resolved_type.value_type().is_some()
        || resolved_type.named_type().is_some()
        || matches!(
            resolved_type.legacy_scalar(),
            Some(
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            )
        )
}

#[cfg(test)]
mod tests;
