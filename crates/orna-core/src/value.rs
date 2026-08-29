//! Backend-independent runtime values, typed function arguments, and ordered
//! SERVER results.
//!
//! This module defines the initial runtime subset only. General runtime-value
//! wire encoding remains a protocol concern; registered opaque contracts may
//! still enforce their own bounded, authority-free payload framing.

use std::{cmp::Ordering, collections::HashSet, error::Error, fmt};

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
        SYS_INSPECT_UI_NODES_TYPE_NAME, SYS_SOURCE_FUNCTION_REPRESENTATION_CONTRACT,
        SYS_SOURCE_FUNCTION_TYPE_ID, SYS_SOURCE_FUNCTION_TYPE_NAME,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
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
        SYS_SOURCE_FUNCTION_TYPE_ID,
        SYS_SOURCE_FUNCTION_TYPE_NAME,
        SYS_SOURCE_FUNCTION_REPRESENTATION_CONTRACT,
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
            Self::Opaque(value) => RuntimeType::Flat(ResolvedType::value(value.opaque_type)),
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
            let result = preflight_collection_descriptor(active, value, path);
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
    let catalogue = active.catalogue();
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(CollectionValueError::UnsupportedDescriptor {
            path: collection_value_path(path),
            descriptor: descriptor.clone(),
        });
    };
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
            let Some(expected) = active.record_value_field_descriptor_runtime_type(descriptor)
            else {
                return Err(CollectionValueError::InactiveValue {
                    path: collection_value_path(path),
                });
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
                RecordValueFieldDescriptorClass::StandardPrimitive(_) => Ok(()),
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
        0x06 | 0x0a => (has_type_identity && std::str::from_utf8(payload).is_ok())
            .then_some(())
            .ok_or(()),
        0x07 => has_type_identity.then_some(()).ok_or(()),
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
mod tests {
    use super::*;

    use crate::{
        CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            calculate_standard_library_digest_for_test, catalogue_digest_with_context,
            source_bundle_digest, source_revision_record_digest, source_unit_content_digest,
            verify_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, Sha256Digest,
            SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
            StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
        },
    };

    const TARGET: TypeId = TypeId::from_bytes([0x41; 16]);
    const OBJECT: ObjectId = ObjectId::from_bytes([0x42; 16]);
    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x43; 16]);
    const RECORD_TYPE: TypeId = TypeId::from_bytes([0x47; 16]);
    const STANDARD_BOOLEAN: TypeId = TypeId::from_bytes([0x48; 16]);
    const OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x49; 16]);
    const OTHER_OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x4a; 16]);
    const ENABLED_FIELD: FieldId = FieldId::from_bytes([0x59; 16]);
    const STAGE_FIELD: FieldId = FieldId::from_bytes([0x5a; 16]);
    const OPAQUE_NAME: [&str; 3] = ["std", "types", "opaque_token"];
    const OPAQUE_CONTRACT: &str = "orna.std.value.opaque-token@1";

    /// One active revision with an acyclic nested-record catalogue.
    ///
    /// `outer.payload` declares `inner` as its Named application-record
    /// field, and `inner.value` is a pinned-standard Boolean leaf.
    fn active_nested_record_revision() -> ActiveDatabaseRevision {
        active_nested_record_revision_with_child_fields(vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "value",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ])
    }

    fn active_nested_record_revision_with_child_fields(
        child_fields: Vec<RecordValueFieldDefinition>,
    ) -> ActiveDatabaseRevision {
        active_nested_record_revision_with_seed(child_fields, 0x58, 0x64)
    }

    fn active_nested_record_revision_with_seed(
        child_fields: Vec<RecordValueFieldDefinition>,
        catalogue_revision_byte: u8,
        source_revision_byte: u8,
    ) -> ActiveDatabaseRevision {
        active_nested_record_revision_with_standard_and_seed(
            child_fields,
            verified_standard_with_value_types(vec![standard_boolean_definition()]),
            catalogue_revision_byte,
            source_revision_byte,
        )
    }

    fn active_nested_record_revision_with_standard_and_seed(
        child_fields: Vec<RecordValueFieldDefinition>,
        standard: VerifiedStandardLibrarySnapshot,
        catalogue_revision_byte: u8,
        source_revision_byte: u8,
    ) -> ActiveDatabaseRevision {
        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([catalogue_revision_byte; 16]);
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let child_field_ids = child_fields
            .iter()
            .map(|field| field.id())
            .collect::<Vec<_>>();
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![],
            vec![
                RecordValueTypeDefinition::new(
                    inner_type,
                    QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
                    child_fields,
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
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "abcdef";
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x63; 16]),
            0,
            "app/types.orna",
            application_content,
            source_unit_content_digest(application_content).unwrap(),
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([source_revision_byte; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x65; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x65; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let mut identities = vec![
            DefinitionIdentity::Schema(application_schema),
            DefinitionIdentity::ValueType(inner_type),
        ];
        identities.extend(
            child_field_ids
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
                    SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    /// One active revision with an acyclic chain of 33 application records.
    ///
    /// Record `0x20 + index` holds one field pointing at the next record for
    /// `index < 32`, and the leaf `0x40` holds a pinned-standard Boolean.
    fn active_record_chain_revision() -> ActiveDatabaseRevision {
        let standard = verified_standard_with_value_types(vec![standard_boolean_definition()]);
        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
        let mut records = Vec::new();
        for index in 0..33_u8 {
            let record_type = TypeId::from_bytes([0x20 + index; 16]);
            let field_id = FieldId::from_bytes([0x80 + index; 16]);
            let target = if index == 32 {
                STANDARD_BOOLEAN
            } else {
                TypeId::from_bytes([0x20 + index + 1; 16])
            };
            records.push(RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["crm", &format!("chain_{index}")]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        field_id,
                        if index == 32 { "value" } else { "next" },
                        0,
                        TypeDescriptor::named(target),
                    )
                    .unwrap(),
                ],
            ));
        }
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![],
            records,
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "z".repeat(70);
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x63; 16]),
            0,
            "app/chain.orna",
            &application_content,
            source_unit_content_digest(&application_content).unwrap(),
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([0x64; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x65; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x65; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let mut identities = vec![DefinitionIdentity::Schema(application_schema)];
        for index in 0..33_u8 {
            let record_type = TypeId::from_bytes([0x20 + index; 16]);
            let field_id = FieldId::from_bytes([0x80 + index; 16]);
            identities.push(DefinitionIdentity::ValueType(record_type));
            identities.push(DefinitionIdentity::Field {
                owner: record_type,
                field: field_id,
            });
        }
        let origins = identities
            .into_iter()
            .enumerate()
            .map(|(index, identity)| {
                DefinitionOrigin::new(
                    identity,
                    SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    #[test]
    fn nested_record_value_constructs_against_one_active_revision() {
        let active = active_nested_record_revision();
        let inner_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let inner = RecordValue::new(
            &active,
            inner_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the flat inner record must construct");
        assert_eq!(inner.record_type(), inner_type);
        assert_eq!(inner.fields(), &[RuntimeValue::Boolean(true)]);
        assert_eq!(
            RuntimeValue::Record(inner.clone()).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(inner_type))
        );

        let outer = RecordValue::new(
            &active,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(inner.clone()))],
        )
        .expect("nested application record field must be admitted");
        assert_eq!(outer.record_type(), outer_type);
        let [RuntimeValue::Record(inner_value)] = outer.fields() else {
            panic!("outer payload must hold the inner record value");
        };
        assert_eq!(
            inner_value, &inner,
            "outer must store the equal inner record in declaration order"
        );
        assert_eq!(
            RuntimeValue::Record(outer).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(outer_type))
        );
    }

    #[test]
    fn runtime_type_preserves_every_flat_runtime_variant() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [opaque_registration(
                OPAQUE_TYPE,
                OPAQUE_NAME,
                OPAQUE_CONTRACT,
            )],
        )
        .unwrap();
        let record = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap();
        let opaque = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, [0; 16]).unwrap();

        let cases: [(RuntimeValue, ResolvedType); 11] = [
            (
                RuntimeValue::null(ResolvedType::reference(TARGET)).unwrap(),
                ResolvedType::reference(TARGET),
            ),
            (
                RuntimeValue::Boolean(true),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            (
                RuntimeValue::Integer(-7),
                ResolvedType::scalar(StandardScalar::Integer),
            ),
            (
                RuntimeValue::BigInt(8),
                ResolvedType::scalar(StandardScalar::BigInt),
            ),
            (
                RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
                ResolvedType::scalar(StandardScalar::Float),
            ),
            (
                RuntimeValue::Text("value".into()),
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            ),
            (
                RuntimeValue::Bytes(vec![1, 2, 3]),
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            ),
            (
                RuntimeValue::Reference {
                    target: TARGET,
                    object: OBJECT,
                },
                ResolvedType::reference(TARGET),
            ),
            (
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap(),
                ),
                ResolvedType::named(ENUM_TYPE),
            ),
            (
                RuntimeValue::Record(record),
                ResolvedType::named(RECORD_TYPE),
            ),
            (
                RuntimeValue::Opaque(opaque),
                ResolvedType::value(OPAQUE_TYPE),
            ),
        ];

        for (value, expected) in cases {
            assert_eq!(value.runtime_type(), RuntimeType::Flat(expected));
        }

        let query: for<'a> fn(&'a RuntimeValue) -> RuntimeType<'a> = RuntimeValue::runtime_type;
        assert_eq!(
            query(&RuntimeValue::Boolean(true)),
            RuntimeType::Flat(ResolvedType::scalar(StandardScalar::Boolean))
        );
    }

    #[test]
    fn canonical_nested_list_and_option_values_expose_exact_public_views_and_stay_closed() {
        let active = active_record_revision();
        let option_descriptor =
            TypeDescriptor::option(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
        let list_descriptor = TypeDescriptor::list(option_descriptor.clone()).unwrap();

        let some = RuntimeValue::option(
            &active,
            option_descriptor.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();
        let none = RuntimeValue::option(&active, option_descriptor.clone(), None).unwrap();
        let list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![some.clone(), none.clone()],
        )
        .unwrap();

        assert_eq!(
            list.runtime_type(),
            RuntimeType::Constructed(&list_descriptor)
        );

        let RuntimeValue::Constructed(constructed) = &list else {
            panic!("list value must be a constructed value");
        };
        assert_eq!(constructed.descriptor(), &list_descriptor);
        let ConstructedValueKind::List(elements) = constructed.kind() else {
            panic!("list kind view must expose the elements");
        };
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0], some);
        assert_eq!(elements[1], none);

        let RuntimeValue::Constructed(some_value) = &elements[0] else {
            panic!("first element must be constructed");
        };
        assert_eq!(some_value.descriptor(), &option_descriptor);
        let ConstructedValueKind::Option(inner) = some_value.kind() else {
            panic!("first element kind view must be an option");
        };
        assert_eq!(inner, Some(&RuntimeValue::Boolean(true)));

        let RuntimeValue::Constructed(none_value) = &elements[1] else {
            panic!("second element must be constructed");
        };
        assert_eq!(none_value.descriptor(), &option_descriptor);
        let ConstructedValueKind::Option(inner) = none_value.kind() else {
            panic!("second element kind view must be an option");
        };
        assert_eq!(inner, None);

        let list_again = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![some.clone(), none.clone()],
        )
        .unwrap();
        assert_eq!(list, list_again);

        let parameter = ParameterId::from_bytes([0x4c; 16]);
        let argument_error = FunctionArgument::new(parameter, list.clone()).unwrap_err();
        assert_eq!(
            argument_error,
            FunctionArgumentError::ConstructedValueNotAccepted {
                parameter,
                descriptor: list_descriptor.clone(),
            }
        );
        assert_eq!(
            argument_error.to_string(),
            "constructed function arguments are not accepted"
        );
        assert!(std::error::Error::source(&argument_error).is_none());

        let column = ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        )
        .unwrap();
        let rows_error =
            ResultRows::new(vec![column], vec![ResultRow::new(vec![list.clone()])]).unwrap_err();
        assert_eq!(
            rows_error,
            ResultRowsError::ConstructedValueNotAccepted {
                row: 0,
                column: 0,
                descriptor: list_descriptor.clone(),
            }
        );
        assert_eq!(
            rows_error.to_string(),
            "constructed SERVER result values are not accepted"
        );
        assert!(std::error::Error::source(&rows_error).is_none());

        let record_error = RecordValue::new(
            &active,
            RECORD_TYPE,
            [(String::from("enabled"), list.clone())],
        )
        .unwrap_err();
        assert_eq!(
            record_error,
            RecordValueError::ConstructedValueNotAccepted {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
                descriptor: list_descriptor.clone(),
            }
        );
        assert_eq!(
            record_error.to_string(),
            "constructed record field values are not accepted"
        );
        assert!(std::error::Error::source(&record_error).is_none());
    }

    #[test]
    fn stale_record_with_removed_trailing_field_reports_that_field_path() {
        const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
        const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
        const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);

        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let field_b = RecordValueFieldDefinition::try_new_descriptor(
            B_FIELD,
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let old_active =
            active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
        let old_record = RecordValue::new(
            &old_active,
            INNER_TYPE,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(true)),
            ],
        )
        .expect("the old two-field inner record must construct");

        let current = active_nested_record_revision_with_child_fields(vec![field_a]);
        let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
        let error = RuntimeValue::list(
            &current,
            list_descriptor,
            vec![RuntimeValue::Record(old_record)],
        )
        .unwrap_err();

        let CollectionValueError::InactiveValue { path } = &error else {
            panic!("stale record list must fail as an inactive value: {error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::ListElement(0),
                CollectionValuePathSegment::RecordField(B_FIELD),
            ]
        );
        assert_eq!(error.to_string(), "collection value is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn stale_enum_label_precedes_a_later_unknown_field() {
        let active = active_record_revision();
        let stale_catalogue = enum_catalogue(&["retired"]);
        let stale =
            RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
        let error = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("stage"), stale),
                (String::from("missing"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap_err();
        assert_eq!(
            error,
            RecordValueError::InactiveEnumLabel {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
                enum_type: ENUM_TYPE,
                label: String::from("retired"),
            }
        );
        assert_eq!(error.to_string(), "record enum field label is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn stale_enum_label_precedes_a_missing_required_field() {
        let active = active_record_revision();
        let stale_catalogue = enum_catalogue(&["retired"]);
        let stale =
            RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
        let error =
            RecordValue::new(&active, RECORD_TYPE, [(String::from("stage"), stale)]).unwrap_err();
        assert_eq!(
            error,
            RecordValueError::InactiveEnumLabel {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
                enum_type: ENUM_TYPE,
                label: String::from("retired"),
            }
        );
        assert_eq!(error.to_string(), "record enum field label is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn stale_nested_record_precedes_a_later_unknown_field() {
        const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
        const OUTER_TYPE: TypeId = TypeId::from_bytes([0x30; 16]);
        const OUTER_FIELD: FieldId = FieldId::from_bytes([0x3b; 16]);
        const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
        const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let field_b = RecordValueFieldDefinition::try_new_descriptor(
            B_FIELD,
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let old = active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
        let old_child = RecordValue::new(
            &old,
            INNER_TYPE,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(true)),
            ],
        )
        .expect("the old two-field child must construct");
        let current = active_nested_record_revision_with_child_fields(vec![field_a]);
        let error = RecordValue::new(
            &current,
            OUTER_TYPE,
            [
                (String::from("payload"), RuntimeValue::Record(old_child)),
                (String::from("missing"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap_err();
        assert_eq!(
            error,
            RecordValueError::InactiveNestedRecord {
                record_type: OUTER_TYPE,
                field: OUTER_FIELD,
                nested_record_type: INNER_TYPE,
            }
        );
        assert_eq!(error.to_string(), "nested record field value is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn empty_option_list_and_map_retain_exact_constructed_views_and_equal_contents() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
        let list_desc = TypeDescriptor::list(boolean.clone()).unwrap();
        let map_desc = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

        let option = RuntimeValue::option(&active, option_desc.clone(), None).unwrap();
        assert_eq!(
            option.runtime_type(),
            RuntimeType::Constructed(&option_desc)
        );
        let RuntimeValue::Constructed(option_value) = &option else {
            panic!("empty option must be a constructed value");
        };
        assert_eq!(option_value.descriptor(), &option_desc);
        let ConstructedValueKind::Option(inner) = option_value.kind() else {
            panic!("empty option must expose the option kind");
        };
        assert_eq!(inner, None);

        let list = RuntimeValue::list(&active, list_desc.clone(), vec![]).unwrap();
        assert_eq!(list.runtime_type(), RuntimeType::Constructed(&list_desc));
        let RuntimeValue::Constructed(list_value) = &list else {
            panic!("empty list must be a constructed value");
        };
        assert_eq!(list_value.descriptor(), &list_desc);
        let ConstructedValueKind::List(elements) = list_value.kind() else {
            panic!("empty list must expose the list kind");
        };
        assert_eq!(elements, &[]);

        let map = RuntimeValue::map(&active, map_desc.clone(), vec![]).unwrap();
        assert_eq!(map.runtime_type(), RuntimeType::Constructed(&map_desc));
        let RuntimeValue::Constructed(map_value) = &map else {
            panic!("empty map must be a constructed value");
        };
        assert_eq!(map_value.descriptor(), &map_desc);
        let ConstructedValueKind::Map(entries) = map_value.kind() else {
            panic!("empty map must expose the map kind");
        };
        assert_eq!(entries, &[]);

        let inner_list = RuntimeValue::list(
            &active,
            list_desc.clone(),
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
        )
        .unwrap();
        let nested_option = TypeDescriptor::option(list_desc.clone()).unwrap();
        let nested =
            RuntimeValue::option(&active, nested_option.clone(), Some(inner_list.clone())).unwrap();
        let RuntimeValue::Constructed(nested_value) = &nested else {
            panic!("nested option must be a constructed value");
        };
        let ConstructedValueKind::Option(Some(child)) = nested_value.kind() else {
            panic!("nested option must expose its some child");
        };
        assert_eq!(child, &inner_list);
        assert_eq!(
            nested,
            RuntimeValue::option(&active, nested_option, Some(inner_list.clone())).unwrap()
        );

        let map_entry = RuntimeValue::map(
            &active,
            map_desc.clone(),
            vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))],
        )
        .unwrap();
        let RuntimeValue::Constructed(map_entry_value) = &map_entry else {
            panic!("map entry must be a constructed value");
        };
        let ConstructedValueKind::Map(entries) = map_entry_value.kind() else {
            panic!("map entry must expose the map kind");
        };
        assert_eq!(
            entries,
            &[(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))]
        );
        assert_eq!(
            map_entry,
            RuntimeValue::map(
                &active,
                map_desc,
                vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false),)],
            )
            .unwrap()
        );

        let ordered = RuntimeValue::list(
            &active,
            list_desc.clone(),
            vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
                RuntimeValue::Boolean(true),
            ],
        )
        .unwrap();
        let RuntimeValue::Constructed(ordered_value) = &ordered else {
            panic!("ordered list must be a constructed value");
        };
        let ConstructedValueKind::List(elements) = ordered_value.kind() else {
            panic!("ordered list must expose the list kind");
        };
        assert_eq!(
            elements,
            &[
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
                RuntimeValue::Boolean(true),
            ]
        );
        assert_eq!(
            ordered,
            RuntimeValue::list(
                &active,
                list_desc,
                vec![
                    RuntimeValue::Boolean(true),
                    RuntimeValue::Boolean(false),
                    RuntimeValue::Boolean(true),
                ],
            )
            .unwrap()
        );
    }

    #[test]
    fn set_values_are_canonically_ordered_and_unique() {
        let active = active_record_revision();
        let descriptor = TypeDescriptor::set(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
        let value = RuntimeValue::set(
            &active,
            descriptor.clone(),
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
        )
        .unwrap();
        let RuntimeValue::Constructed(constructed) = &value else {
            panic!("set construction must return a constructed value");
        };
        let ConstructedValueKind::Set(values) = constructed.kind() else {
            panic!("set construction must retain SET values");
        };
        assert_eq!(
            values,
            &[RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)]
        );
        assert_eq!(
            value,
            RuntimeValue::set(
                &active,
                descriptor.clone(),
                vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)],
            )
            .unwrap()
        );
        assert_eq!(
            RuntimeValue::set(
                &active,
                descriptor,
                vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)],
            )
            .unwrap_err(),
            CollectionValueError::DuplicateSetElement {
                first: 0,
                duplicate: 1,
            }
        );
    }

    #[test]
    fn constructed_constructors_reject_wrong_outer_descriptors_exactly() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
        let list_desc = TypeDescriptor::list(boolean.clone()).unwrap();
        let map_desc = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

        let wrong_option = RuntimeValue::option(&active, list_desc.clone(), None).unwrap_err();
        assert_eq!(
            wrong_option,
            CollectionValueError::WrongConstructor {
                expected: CollectionKind::Option,
                descriptor: list_desc.clone(),
            }
        );
        assert_eq!(
            wrong_option.to_string(),
            "collection descriptor has the wrong outer constructor"
        );
        assert!(std::error::Error::source(&wrong_option).is_none());

        let wrong_list = RuntimeValue::list(&active, option_desc.clone(), vec![]).unwrap_err();
        assert_eq!(
            wrong_list,
            CollectionValueError::WrongConstructor {
                expected: CollectionKind::List,
                descriptor: option_desc.clone(),
            }
        );
        assert_eq!(
            wrong_list.to_string(),
            "collection descriptor has the wrong outer constructor"
        );
        assert!(std::error::Error::source(&wrong_list).is_none());

        let wrong_map = RuntimeValue::map(&active, option_desc.clone(), vec![]).unwrap_err();
        assert_eq!(
            wrong_map,
            CollectionValueError::WrongConstructor {
                expected: CollectionKind::Map,
                descriptor: option_desc.clone(),
            }
        );
        assert_eq!(
            wrong_map.to_string(),
            "collection descriptor has the wrong outer constructor"
        );
        assert!(std::error::Error::source(&wrong_map).is_none());

        let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
        let list_of_set = TypeDescriptor::list(set_desc).unwrap();
        let wrong_before_unsupported =
            RuntimeValue::option(&active, list_of_set.clone(), None).unwrap_err();
        assert_eq!(
            wrong_before_unsupported,
            CollectionValueError::WrongConstructor {
                expected: CollectionKind::Option,
                descriptor: list_of_set.clone(),
            }
        );
        assert_eq!(
            wrong_before_unsupported.to_string(),
            "collection descriptor has the wrong outer constructor"
        );
        assert!(std::error::Error::source(&wrong_before_unsupported).is_none());

        let _ = map_desc;
    }

    #[test]
    fn collection_descriptor_preorder_reports_exact_paths_for_unsupported_children() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
        let stream_desc = TypeDescriptor::stream(boolean.clone()).unwrap();

        let option_of_set = TypeDescriptor::option(set_desc.clone()).unwrap();
        let error = RuntimeValue::option(&active, option_of_set.clone(), None).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("option child must fail as an unsupported descriptor: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
        assert_eq!(descriptor, &set_desc);

        let list_of_set = TypeDescriptor::list(set_desc.clone()).unwrap();
        let error = RuntimeValue::list(&active, list_of_set.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("list child must fail as an unsupported descriptor: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(descriptor, &set_desc);

        let nested_list =
            TypeDescriptor::list(TypeDescriptor::list(stream_desc.clone()).unwrap()).unwrap();
        let error = RuntimeValue::list(&active, nested_list.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("nested list child must fail at the deepest path: {error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::ListChild,
                CollectionValuePathSegment::ListChild,
            ]
        );
        assert_eq!(descriptor, &stream_desc);

        let map_stream_key = TypeDescriptor::map(stream_desc.clone(), boolean.clone()).unwrap();
        let error = RuntimeValue::map(&active, map_stream_key.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("map stream key must fail before the value: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
        assert_eq!(descriptor, &stream_desc);

        let map_set_value = TypeDescriptor::map(boolean.clone(), set_desc.clone()).unwrap();
        let error = RuntimeValue::map(&active, map_set_value.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("map set value must fail at the value child: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::MapValueChild]
        );
        assert_eq!(descriptor, &set_desc);

        let map_list_key = TypeDescriptor::map(
            TypeDescriptor::list(boolean.clone()).unwrap(),
            boolean.clone(),
        )
        .unwrap();
        let error = RuntimeValue::map(&active, map_list_key.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("constructed map key must fail at the key child without a deeper path: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
        assert_eq!(descriptor, &TypeDescriptor::list(boolean.clone()).unwrap());

        let option_of_stream = TypeDescriptor::option(stream_desc.clone()).unwrap();
        let error = RuntimeValue::option(&active, option_of_stream.clone(), None).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("option stream child must fail at the option child path: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
        assert_eq!(descriptor, &stream_desc);
    }

    #[test]
    fn collection_descriptor_rejects_missing_and_inactive_leaf_categories_exactly() {
        let active = active_record_revision();
        let missing = TypeDescriptor::named(TypeId::from_bytes([0x6c; 16]));

        let list_of_missing = TypeDescriptor::list(missing.clone()).unwrap();
        let error = RuntimeValue::list(&active, list_of_missing.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("missing named leaf must be unsupported: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(descriptor, &missing);

        let opaque = TypeDescriptor::named(OPAQUE_TYPE);
        let list_of_opaque = TypeDescriptor::list(opaque.clone()).unwrap();
        let error = RuntimeValue::list(&active, list_of_opaque.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("pinned opaque leaf must be unsupported: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(descriptor, &opaque);

        let reference = TypeDescriptor::reference(TARGET);
        let list_of_reference = TypeDescriptor::list(reference.clone()).unwrap();
        let error = RuntimeValue::list(&active, list_of_reference.clone(), vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("inactive reference leaf must be unsupported: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(descriptor, &reference);
    }

    #[test]
    fn ambiguous_application_standard_named_collision_precedes_category_rejection() {
        let active = active_record_revision_with_opaque_contract(
            TypeId::from_bytes([0x49; 16]),
            OPAQUE_CONTRACT,
        );
        let collision = TypeDescriptor::named(TypeId::from_bytes([0x49; 16]));
        let list_of_collision = TypeDescriptor::list(collision.clone()).unwrap();
        let error = RuntimeValue::list(&active, list_of_collision.clone(), vec![]).unwrap_err();
        let CollectionValueError::AmbiguousNamedType { path, type_id } = &error else {
            panic!("application-standard collision must be ambiguous: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(*type_id, TypeId::from_bytes([0x49; 16]));
        assert_eq!(
            error.to_string(),
            "collection descriptor type is present in both application and standard catalogues"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn constructed_constructor_precedence_is_exact() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
        let list_of_set = TypeDescriptor::list(set_desc.clone()).unwrap();
        let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
        let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

        let error = RuntimeValue::list(
            &active,
            list_of_set.clone(),
            vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES],
        )
        .unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, .. } = &error else {
            panic!("descriptor failure must precede node counting: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);

        let mut overflow = vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES];
        overflow.push(RuntimeValue::Text("x".into()));
        let error = RuntimeValue::list(&active, list_boolean.clone(), overflow).unwrap_err();
        assert_eq!(
            error,
            CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            }
        );

        let error = RuntimeValue::list(
            &active,
            list_boolean.clone(),
            vec![
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
                RuntimeValue::Text("x".into()),
            ],
        )
        .unwrap_err();
        let CollectionValueError::NullValueNotAccepted { path } = &error else {
            panic!("null element must precede a later type mismatch: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::ListElement(0)]
        );

        const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
        const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
        const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let field_b = RecordValueFieldDefinition::try_new_descriptor(
            B_FIELD,
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let old = active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
        let stale_child = RecordValue::new(
            &old,
            INNER_TYPE,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(true)),
            ],
        )
        .expect("the old two-field child must construct");
        let current = active_nested_record_revision_with_child_fields(vec![field_a]);
        let list_inner = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
        let error = RuntimeValue::list(
            &current,
            list_inner,
            vec![RuntimeValue::Integer(1), RuntimeValue::Record(stale_child)],
        )
        .unwrap_err();
        let CollectionValueError::ValueTypeMismatch { path } = &error else {
            panic!("type mismatch must precede an inactive element: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::ListElement(0)]
        );

        let error = RuntimeValue::map(
            &active,
            map_boolean.clone(),
            vec![(
                RuntimeValue::Text("k".into()),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
            )],
        )
        .unwrap_err();
        let CollectionValueError::ValueTypeMismatch { path } = &error else {
            panic!("map key mismatch must precede the value failure: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKey(0)]);

        let error = RuntimeValue::map(
            &active,
            map_boolean.clone(),
            vec![
                (RuntimeValue::Boolean(true), RuntimeValue::Text("x".into())),
                (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap_err();
        let CollectionValueError::ValueTypeMismatch { path } = &error else {
            panic!("value semantic failure must precede duplicate detection: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::MapValue(0)]);
    }

    const MAP_BOOLEAN: TypeId = TypeId::from_bytes([0x81; 16]);
    const MAP_INTEGER: TypeId = TypeId::from_bytes([0x82; 16]);
    const MAP_BIGINT: TypeId = TypeId::from_bytes([0x83; 16]);
    const MAP_FLOAT: TypeId = TypeId::from_bytes([0x84; 16]);
    const MAP_TEXT: TypeId = TypeId::from_bytes([0x85; 16]);
    const MAP_BYTES: TypeId = TypeId::from_bytes([0x86; 16]);
    const MAP_STD_ENUM: TypeId = TypeId::from_bytes([0x87; 16]);
    const MAP_APP_ENUM: TypeId = TypeId::from_bytes([0x88; 16]);
    const MAP_OBJECT: TypeId = TypeId::from_bytes([0x89; 16]);
    const MAP_FLAT: TypeId = TypeId::from_bytes([0x8a; 16]);
    const MAP_INNER: TypeId = TypeId::from_bytes([0x8b; 16]);
    const MAP_OUTER: TypeId = TypeId::from_bytes([0x8c; 16]);
    const MAP_STD_ENUM_RECORD: TypeId = TypeId::from_bytes([0x8d; 16]);
    const MAP_A_FIELD: FieldId = FieldId::from_bytes([0xa1; 16]);
    const MAP_B_FIELD: FieldId = FieldId::from_bytes([0xa2; 16]);
    const MAP_LEAF_FIELD: FieldId = FieldId::from_bytes([0xa3; 16]);
    const MAP_FIRST_FIELD: FieldId = FieldId::from_bytes([0xa4; 16]);
    const MAP_TAIL_FIELD: FieldId = FieldId::from_bytes([0xa5; 16]);
    const MAP_STD_ENUM_ENABLED_FIELD: FieldId = FieldId::from_bytes([0xa6; 16]);
    const MAP_STD_ENUM_FIELD: FieldId = FieldId::from_bytes([0xa7; 16]);

    fn map_standard_primitive(type_id: TypeId, name: &str, contract: &str) -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            type_id,
            QualifiedSemanticName::new(["std", name]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            contract,
        )
    }

    fn verified_map_standard() -> VerifiedStandardLibrarySnapshot {
        let value_types = vec![
            map_standard_primitive(MAP_BOOLEAN, "boolean", "orna.kernel.value.boolean@1"),
            map_standard_primitive(MAP_INTEGER, "integer", "orna.kernel.value.integer@1"),
            map_standard_primitive(MAP_BIGINT, "bigint", "orna.kernel.value.bigint@1"),
            map_standard_primitive(MAP_FLOAT, "float", "orna.kernel.value.float@1"),
            map_standard_primitive(
                MAP_TEXT,
                "character_large_object",
                "orna.kernel.value.character-large-object@1",
            ),
            map_standard_primitive(
                MAP_BYTES,
                "binary_large_object",
                "orna.kernel.value.binary-large-object@1",
            ),
        ];
        let enum_types = vec![EnumTypeDefinition::new(
            MAP_STD_ENUM,
            QualifiedSemanticName::new(["std", "mode"]).unwrap(),
            ["alpha", "beta"],
        )];
        let standard_unit_content = "x".repeat(value_types.len() + enum_types.len() + 2);
        let standard_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x50; 16]),
            0,
            "std/types.orna",
            &standard_unit_content,
            source_unit_content_digest(&standard_unit_content).unwrap(),
        )
        .unwrap();
        let standard_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&standard_unit)).unwrap();
        let standard_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x51; 16]),
            SourceRevisionId::from_bytes([0x52; 16]),
            None,
            vec![standard_unit],
            standard_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x51; 16]),
                None,
                standard_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let standard_schema = SchemaId::from_bytes([0x53; 16]);
        let standard_types_schema = SchemaId::from_bytes([0x54; 16]);
        let standard_catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x5b; 16]),
            vec![
                SchemaDefinition::new(
                    standard_schema,
                    QualifiedSemanticName::new(["std"]).unwrap(),
                ),
                SchemaDefinition::new(
                    standard_types_schema,
                    QualifiedSemanticName::new(["std", "types"]).unwrap(),
                ),
            ],
            vec![],
            value_types.clone(),
            enum_types.clone(),
            vec![],
            vec![],
        )
        .unwrap();
        let mut standard_origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(standard_schema),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(standard_types_schema),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 1, 2).unwrap(),
            ),
        ];
        for (index, value_type) in standard_catalogue.value_types().iter().enumerate() {
            let start = u32::try_from(index + 2).unwrap();
            standard_origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(value_type.id()),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
            ));
        }
        for (index, enum_type) in standard_catalogue.enum_types().iter().enumerate() {
            let start = u32::try_from(value_types.len() + index + 2).unwrap();
            standard_origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(enum_type.id()),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
            ));
        }
        let provisional_standard = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0x55; 16]),
            StandardLibraryDigestVersion::Version1,
            standard_source.clone(),
            "orna.language/1",
            standard_catalogue.clone(),
            standard_origins.clone(),
            Sha256Digest::from_bytes([0x56; 32]),
        )
        .unwrap();
        let standard_digest =
            calculate_standard_library_digest_for_test(&provisional_standard).unwrap();
        verify_standard_library_snapshot(
            StandardLibrarySnapshot::new(
                provisional_standard.revision(),
                provisional_standard.digest_version(),
                standard_source,
                provisional_standard.language_version(),
                standard_catalogue,
                standard_origins,
                standard_digest,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn active_map_revision() -> ActiveDatabaseRevision {
        let standard = verified_map_standard();
        let application_schema = SchemaId::from_bytes([0x91; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x92; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                MAP_OBJECT,
                QualifiedSemanticName::new(["app", "item"]).unwrap(),
                vec![],
            )],
            vec![],
            vec![EnumTypeDefinition::new(
                MAP_APP_ENUM,
                QualifiedSemanticName::new(["app", "stage"]).unwrap(),
                ["low", "high"],
            )],
            vec![
                RecordValueTypeDefinition::new(
                    MAP_FLAT,
                    QualifiedSemanticName::new(["app", "flat"]).unwrap(),
                    vec![
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_A_FIELD,
                            "a",
                            0,
                            TypeDescriptor::named(MAP_BOOLEAN),
                        )
                        .unwrap(),
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_B_FIELD,
                            "b",
                            1,
                            TypeDescriptor::named(MAP_INTEGER),
                        )
                        .unwrap(),
                    ],
                ),
                RecordValueTypeDefinition::new(
                    MAP_INNER,
                    QualifiedSemanticName::new(["app", "inner"]).unwrap(),
                    vec![
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_LEAF_FIELD,
                            "leaf",
                            0,
                            TypeDescriptor::named(MAP_BOOLEAN),
                        )
                        .unwrap(),
                    ],
                ),
                RecordValueTypeDefinition::new(
                    MAP_OUTER,
                    QualifiedSemanticName::new(["app", "outer"]).unwrap(),
                    vec![
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_FIRST_FIELD,
                            "first",
                            0,
                            TypeDescriptor::named(MAP_INNER),
                        )
                        .unwrap(),
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_TAIL_FIELD,
                            "tail",
                            1,
                            TypeDescriptor::named(MAP_INTEGER),
                        )
                        .unwrap(),
                    ],
                ),
                RecordValueTypeDefinition::new(
                    MAP_STD_ENUM_RECORD,
                    QualifiedSemanticName::new(["app", "standard_enum_record"]).unwrap(),
                    vec![
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_STD_ENUM_ENABLED_FIELD,
                            "enabled",
                            0,
                            TypeDescriptor::named(MAP_BOOLEAN),
                        )
                        .unwrap(),
                        RecordValueFieldDefinition::try_new_descriptor(
                            MAP_STD_ENUM_FIELD,
                            "mode",
                            1,
                            TypeDescriptor::named(MAP_STD_ENUM),
                        )
                        .unwrap(),
                    ],
                ),
            ],
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "x".repeat(15);
        let application_content_digest = source_unit_content_digest(&application_content).unwrap();
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x93; 16]),
            0,
            "app/types.orna",
            application_content,
            application_content_digest,
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([0x95; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x94; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x94; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x93; 16]);
        let mut origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(application_schema),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(MAP_APP_ENUM),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(MAP_OBJECT),
                SourceOrigin::new(source_unit, 2, 3).unwrap(),
            ),
        ];
        for (index, record) in catalogue.record_value_types().iter().enumerate() {
            let record_start = u32::try_from(3 + index * 3).unwrap();
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record.id()),
                SourceOrigin::new(source_unit, record_start, record_start + 1).unwrap(),
            ));
            for (field_index, field) in record.fields().iter().enumerate() {
                let field_start = record_start + 1 + u32::try_from(field_index).unwrap();
                origins.push(DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: record.id(),
                        field: field.id(),
                    },
                    SourceOrigin::new(source_unit, field_start, field_start + 1).unwrap(),
                ));
            }
        }
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    fn canonical_map_entries(
        active: &ActiveDatabaseRevision,
        key_descriptor: TypeDescriptor,
        entries: Vec<(RuntimeValue, RuntimeValue)>,
    ) -> Vec<(RuntimeValue, RuntimeValue)> {
        let map_descriptor =
            TypeDescriptor::map(key_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap();
        let map = RuntimeValue::map(active, map_descriptor, entries).unwrap();
        let RuntimeValue::Constructed(value) = &map else {
            panic!("map value must be constructed");
        };
        let ConstructedValueKind::Map(entries) = value.kind() else {
            panic!("map value must expose the map kind");
        };
        entries.to_vec()
    }

    fn map_flat_record(active: &ActiveDatabaseRevision, a: bool, b: i32) -> RuntimeValue {
        RuntimeValue::Record(
            RecordValue::new(
                active,
                MAP_FLAT,
                [
                    (String::from("a"), RuntimeValue::Boolean(a)),
                    (String::from("b"), RuntimeValue::Integer(b)),
                ],
            )
            .unwrap(),
        )
    }

    fn map_outer_record(active: &ActiveDatabaseRevision, leaf: bool, tail: i32) -> RuntimeValue {
        let inner = RecordValue::new(
            active,
            MAP_INNER,
            [(String::from("leaf"), RuntimeValue::Boolean(leaf))],
        )
        .unwrap();
        RuntimeValue::Record(
            RecordValue::new(
                active,
                MAP_OUTER,
                [
                    (String::from("first"), RuntimeValue::Record(inner)),
                    (String::from("tail"), RuntimeValue::Integer(tail)),
                ],
            )
            .unwrap(),
        )
    }

    fn map_keys(keys: Vec<RuntimeValue>) -> Vec<(RuntimeValue, RuntimeValue)> {
        keys.into_iter()
            .map(|key| (key, RuntimeValue::Boolean(true)))
            .collect()
    }

    #[test]
    fn map_canonical_order_holds_for_every_admitted_flat_key_family() {
        let active = active_map_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();

        let boolean_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_BOOLEAN),
            map_keys(vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            boolean_keys,
            vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)]
        );

        let integer_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_INTEGER),
            map_keys(vec![
                RuntimeValue::Integer(2),
                RuntimeValue::Integer(0),
                RuntimeValue::Integer(-1),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            integer_keys,
            vec![
                RuntimeValue::Integer(-1),
                RuntimeValue::Integer(0),
                RuntimeValue::Integer(2),
            ]
        );

        let bigint_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_BIGINT),
            map_keys(vec![
                RuntimeValue::BigInt(0),
                RuntimeValue::BigInt(-5),
                RuntimeValue::BigInt(9),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            bigint_keys,
            vec![
                RuntimeValue::BigInt(-5),
                RuntimeValue::BigInt(0),
                RuntimeValue::BigInt(9),
            ]
        );

        let float_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_FLOAT),
            map_keys(vec![
                RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
                RuntimeValue::Float(RuntimeFloat::new(-2.5).unwrap()),
                RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            float_keys,
            vec![
                RuntimeValue::Float(RuntimeFloat::new(-2.5).unwrap()),
                RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
                RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
            ]
        );

        let text_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_TEXT),
            map_keys(vec![
                RuntimeValue::Text("cherry".into()),
                RuntimeValue::Text("apple".into()),
                RuntimeValue::Text("banana".into()),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            text_keys,
            vec![
                RuntimeValue::Text("apple".into()),
                RuntimeValue::Text("banana".into()),
                RuntimeValue::Text("cherry".into()),
            ]
        );

        let bytes_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_BYTES),
            map_keys(vec![
                RuntimeValue::Bytes(vec![2]),
                RuntimeValue::Bytes(vec![1, 0]),
                RuntimeValue::Bytes(vec![1]),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            bytes_keys,
            vec![
                RuntimeValue::Bytes(vec![1]),
                RuntimeValue::Bytes(vec![1, 0]),
                RuntimeValue::Bytes(vec![2]),
            ]
        );

        let reference_keys = canonical_map_entries(
            &active,
            TypeDescriptor::reference(MAP_OBJECT),
            map_keys(vec![
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x02; 16]),
                },
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x01; 16]),
                },
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x03; 16]),
                },
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            reference_keys,
            vec![
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x01; 16]),
                },
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x02; 16]),
                },
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x03; 16]),
                },
            ]
        );

        let app_enum_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_APP_ENUM),
            map_keys(vec![
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap(),
                ),
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), MAP_APP_ENUM, "high").unwrap(),
                ),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            app_enum_keys,
            vec![
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), MAP_APP_ENUM, "high").unwrap(),
                ),
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap(),
                ),
            ]
        );

        let std_enum_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_STD_ENUM),
            map_keys(vec![
                RuntimeValue::Enum(
                    EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta").unwrap(),
                ),
                RuntimeValue::Enum(
                    EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
                ),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            std_enum_keys,
            vec![
                RuntimeValue::Enum(
                    EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
                ),
                RuntimeValue::Enum(
                    EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta").unwrap(),
                ),
            ]
        );

        let flat_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_FLAT),
            map_keys(vec![
                map_flat_record(&active, true, 2),
                map_flat_record(&active, false, 1),
                map_flat_record(&active, true, 1),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            flat_keys,
            vec![
                map_flat_record(&active, false, 1),
                map_flat_record(&active, true, 1),
                map_flat_record(&active, true, 2),
            ]
        );

        let outer_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_OUTER),
            map_keys(vec![
                map_outer_record(&active, true, 1),
                map_outer_record(&active, false, 9),
                map_outer_record(&active, true, 0),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            outer_keys,
            vec![
                map_outer_record(&active, false, 9),
                map_outer_record(&active, true, 0),
                map_outer_record(&active, true, 1),
            ]
        );
    }

    #[test]
    fn map_input_permutations_produce_equal_canonical_maps() {
        let active = active_map_revision();
        let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
        let text_descriptor = TypeDescriptor::named(MAP_TEXT);
        let flat_descriptor = TypeDescriptor::named(MAP_FLAT);

        let forward = RuntimeValue::map(
            &active,
            TypeDescriptor::map(
                integer_descriptor.clone(),
                TypeDescriptor::named(MAP_BOOLEAN),
            )
            .unwrap(),
            map_keys(vec![
                RuntimeValue::Integer(1),
                RuntimeValue::Integer(0),
                RuntimeValue::Integer(2),
            ]),
        )
        .unwrap();
        let reversed = RuntimeValue::map(
            &active,
            TypeDescriptor::map(integer_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            map_keys(vec![
                RuntimeValue::Integer(2),
                RuntimeValue::Integer(1),
                RuntimeValue::Integer(0),
            ]),
        )
        .unwrap();
        assert_eq!(forward, reversed);
        let RuntimeValue::Constructed(forward_value) = &forward else {
            panic!("forward map must be constructed");
        };
        let RuntimeValue::Constructed(reversed_value) = &reversed else {
            panic!("reversed map must be constructed");
        };
        let ConstructedValueKind::Map(forward_entries) = forward_value.kind() else {
            panic!("forward map must expose the map kind");
        };
        let ConstructedValueKind::Map(reversed_entries) = reversed_value.kind() else {
            panic!("reversed map must expose the map kind");
        };
        assert_eq!(forward_entries, reversed_entries);

        let text_forward = RuntimeValue::map(
            &active,
            TypeDescriptor::map(text_descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN))
                .unwrap(),
            map_keys(vec![
                RuntimeValue::Text("b".into()),
                RuntimeValue::Text("a".into()),
                RuntimeValue::Text("c".into()),
            ]),
        )
        .unwrap();
        let text_scrambled = RuntimeValue::map(
            &active,
            TypeDescriptor::map(text_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            map_keys(vec![
                RuntimeValue::Text("c".into()),
                RuntimeValue::Text("b".into()),
                RuntimeValue::Text("a".into()),
            ]),
        )
        .unwrap();
        assert_eq!(text_forward, text_scrambled);

        let flat_forward = RuntimeValue::map(
            &active,
            TypeDescriptor::map(flat_descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN))
                .unwrap(),
            map_keys(vec![
                map_flat_record(&active, false, 1),
                map_flat_record(&active, true, 2),
            ]),
        )
        .unwrap();
        let flat_reversed = RuntimeValue::map(
            &active,
            TypeDescriptor::map(flat_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            map_keys(vec![
                map_flat_record(&active, true, 2),
                map_flat_record(&active, false, 1),
            ]),
        )
        .unwrap();
        assert_eq!(flat_forward, flat_reversed);
    }

    #[test]
    fn map_duplicate_keys_report_exact_original_indices() {
        let active = active_map_revision();
        let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
        let float_descriptor = TypeDescriptor::named(MAP_FLOAT);

        let error = RuntimeValue::map(
            &active,
            TypeDescriptor::map(
                integer_descriptor.clone(),
                TypeDescriptor::named(MAP_BOOLEAN),
            )
            .unwrap(),
            map_keys(vec![RuntimeValue::Integer(5), RuntimeValue::Integer(5)]),
        )
        .unwrap_err();
        assert_eq!(
            error,
            CollectionValueError::DuplicateMapKey {
                first: 0,
                duplicate: 1,
            }
        );

        let three_error = RuntimeValue::map(
            &active,
            TypeDescriptor::map(
                integer_descriptor.clone(),
                TypeDescriptor::named(MAP_BOOLEAN),
            )
            .unwrap(),
            map_keys(vec![
                RuntimeValue::Integer(5),
                RuntimeValue::Integer(5),
                RuntimeValue::Integer(5),
            ]),
        )
        .unwrap_err();
        assert_eq!(
            three_error,
            CollectionValueError::DuplicateMapKey {
                first: 0,
                duplicate: 1,
            }
        );

        let canonical_first_error = RuntimeValue::map(
            &active,
            TypeDescriptor::map(
                integer_descriptor.clone(),
                TypeDescriptor::named(MAP_BOOLEAN),
            )
            .unwrap(),
            vec![
                (RuntimeValue::Integer(5), RuntimeValue::Boolean(true)),
                (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
                (RuntimeValue::Integer(5), RuntimeValue::Boolean(true)),
                (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap_err();
        assert_eq!(
            canonical_first_error,
            CollectionValueError::DuplicateMapKey {
                first: 1,
                duplicate: 3,
            }
        );

        let negative_zero_error = RuntimeValue::map(
            &active,
            TypeDescriptor::map(float_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            map_keys(vec![
                RuntimeValue::Float(RuntimeFloat::new(-0.0).unwrap()),
                RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
            ]),
        )
        .unwrap_err();
        assert_eq!(
            negative_zero_error,
            CollectionValueError::DuplicateMapKey {
                first: 0,
                duplicate: 1,
            }
        );
        assert_eq!(
            negative_zero_error.to_string(),
            "map contains a duplicate key"
        );
        assert!(std::error::Error::source(&negative_zero_error).is_none());
    }

    #[test]
    fn record_map_keys_order_lexicographically_in_declaration_order() {
        let active = active_map_revision();

        let flat_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_FLAT),
            map_keys(vec![
                map_flat_record(&active, true, 2),
                map_flat_record(&active, true, 1),
                map_flat_record(&active, false, 9),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            flat_keys,
            vec![
                map_flat_record(&active, false, 9),
                map_flat_record(&active, true, 1),
                map_flat_record(&active, true, 2),
            ]
        );

        let outer_keys = canonical_map_entries(
            &active,
            TypeDescriptor::named(MAP_OUTER),
            map_keys(vec![
                map_outer_record(&active, true, 1),
                map_outer_record(&active, false, 9),
                map_outer_record(&active, true, 0),
            ]),
        )
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
        assert_eq!(
            outer_keys,
            vec![
                map_outer_record(&active, false, 9),
                map_outer_record(&active, true, 0),
                map_outer_record(&active, true, 1),
            ]
        );
    }

    #[test]
    fn map_and_list_equality_distinguish_descriptors_values_and_order() {
        let active = active_map_revision();
        let boolean_descriptor = TypeDescriptor::named(MAP_BOOLEAN);
        let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
        let map_descriptor = TypeDescriptor::map(
            boolean_descriptor.clone(),
            TypeDescriptor::named(MAP_BOOLEAN),
        )
        .unwrap();

        let value_map = RuntimeValue::map(
            &active,
            map_descriptor.clone(),
            vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))],
        )
        .unwrap();
        let different_value = RuntimeValue::map(
            &active,
            map_descriptor.clone(),
            vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(true))],
        )
        .unwrap();
        assert_ne!(value_map, different_value);

        let different_descriptor = RuntimeValue::map(
            &active,
            TypeDescriptor::map(integer_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            vec![(RuntimeValue::Integer(1), RuntimeValue::Boolean(false))],
        )
        .unwrap();
        assert_ne!(value_map, different_descriptor);

        let list_descriptor = TypeDescriptor::list(boolean_descriptor).unwrap();
        let forward_list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
        )
        .unwrap();
        let reversed_list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)],
        )
        .unwrap();
        assert_ne!(forward_list, reversed_list);

        let duplicate_list = RuntimeValue::list(
            &active,
            list_descriptor.clone(),
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)],
        )
        .unwrap();
        assert_ne!(
            forward_list,
            RuntimeValue::list(&active, list_descriptor, vec![RuntimeValue::Boolean(true)],)
                .unwrap()
        );
        assert_ne!(forward_list, duplicate_list);
    }
    fn active_object_opaque_collision_revision() -> ActiveDatabaseRevision {
        let standard = verified_standard_with_value_types(vec![
            standard_boolean_definition(),
            opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
        ]);
        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![ObjectTypeDefinition::new(
                OPAQUE_TYPE,
                QualifiedSemanticName::new(["crm", "item"]).unwrap(),
                vec![],
            )],
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "ab";
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x63; 16]),
            0,
            "app/types.orna",
            application_content,
            source_unit_content_digest(application_content).unwrap(),
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([0x64; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x65; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x65; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(application_schema),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(OPAQUE_TYPE),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
        ];
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    #[test]
    fn enum_with_record_identity_is_rejected_as_a_field_type_mismatch() {
        const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
        const OUTER_TYPE: TypeId = TypeId::from_bytes([0x30; 16]);
        const OUTER_FIELD: FieldId = FieldId::from_bytes([0x3b; 16]);
        let current = active_nested_record_revision_with_child_fields(vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "value",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ]);
        let identity_catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                INNER_TYPE,
                QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
                ["lead"],
            )],
            vec![],
        )
        .unwrap();
        let identity_enum =
            RuntimeValue::Enum(EnumValue::new(&identity_catalogue, INNER_TYPE, "lead").unwrap());
        let error = RecordValue::new(
            &current,
            OUTER_TYPE,
            [(String::from("payload"), identity_enum)],
        )
        .unwrap_err();
        assert_eq!(
            error,
            RecordValueError::FieldTypeMismatch {
                record_type: OUTER_TYPE,
                field: OUTER_FIELD,
                expected: ResolvedType::named(INNER_TYPE),
                actual: ResolvedType::named(INNER_TYPE),
            }
        );
        assert_eq!(error.to_string(), "record field value has a type mismatch");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn legacy_typed_null_precedes_type_mismatch_at_the_same_list_element() {
        let active = active_record_revision();
        let list_boolean = TypeDescriptor::list(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
        let error = RuntimeValue::list(
            &active,
            list_boolean,
            vec![
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
                    .unwrap(),
                RuntimeValue::Boolean(false),
            ],
        )
        .unwrap_err();
        let CollectionValueError::NullValueNotAccepted { path } = &error else {
            panic!("typed null must precede the type mismatch: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::ListElement(0)]
        );
    }

    #[test]
    fn enum_nominal_mismatch_precedes_label_inactivity_at_the_same_element() {
        let active = active_record_revision();
        let wrong_id_catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes([0x6d; 16]),
                QualifiedSemanticName::new(["crm", "other"]).unwrap(),
                ["retired"],
            )],
            vec![],
        )
        .unwrap();
        let wrong_enum = RuntimeValue::Enum(
            EnumValue::new(
                &wrong_id_catalogue,
                TypeId::from_bytes([0x6d; 16]),
                "retired",
            )
            .unwrap(),
        );
        let list_enum = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
        let error = RuntimeValue::list(&active, list_enum, vec![wrong_enum]).unwrap_err();
        let CollectionValueError::ValueTypeMismatch { path } = &error else {
            panic!("nominal mismatch must precede label inactivity: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::ListElement(0)]
        );
    }

    #[test]
    fn map_descriptor_reports_the_key_child_before_an_unsupported_value() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let set_key = TypeDescriptor::set(boolean.clone()).unwrap();
        let stream_value = TypeDescriptor::stream(boolean).unwrap();
        let map_descriptor = TypeDescriptor::map(set_key.clone(), stream_value).unwrap();
        let error = RuntimeValue::map(&active, map_descriptor, vec![]).unwrap_err();
        let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
            panic!("unsupported map key must precede the unsupported value: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
        assert_eq!(descriptor, &set_key);
    }

    #[test]
    fn object_opaque_shared_identity_is_ambiguous_before_unsupported() {
        let active = active_object_opaque_collision_revision();
        let collision = TypeDescriptor::named(OPAQUE_TYPE);
        let list = TypeDescriptor::list(collision.clone()).unwrap();
        let error = RuntimeValue::list(&active, list, vec![]).unwrap_err();
        let CollectionValueError::AmbiguousNamedType { path, type_id } = &error else {
            panic!("object/opaque identity collision must be ambiguous: {error}");
        };
        assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
        assert_eq!(*type_id, OPAQUE_TYPE);
        assert_eq!(
            error.to_string(),
            "collection descriptor type is present in both application and standard catalogues"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn record_caller_input_order_reports_the_first_supplied_stale_enum_error() {
        let active = active_record_revision();
        let stale_catalogue = enum_catalogue(&["retired"]);
        let stale =
            RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
        let error = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("stage"), stale),
                (String::from("enabled"), RuntimeValue::Integer(1)),
            ],
        )
        .unwrap_err();
        assert_eq!(
            error,
            RecordValueError::InactiveEnumLabel {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
                enum_type: ENUM_TYPE,
                label: String::from("retired"),
            }
        );
        assert_eq!(error.to_string(), "record enum field label is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn runtime_value_node_boundary_is_exact() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let option_boolean = TypeDescriptor::option(boolean.clone()).unwrap();
        let list_of_option = TypeDescriptor::list(option_boolean.clone()).unwrap();
        let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
        let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

        let subtree = RuntimeValue::option(
            &active,
            option_boolean.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();
        let empty_subtree = RuntimeValue::option(&active, option_boolean.clone(), None).unwrap();
        let mut boundary_elements = vec![subtree.clone(); 32_767];
        boundary_elements.push(empty_subtree.clone());
        let at_boundary =
            RuntimeValue::list(&active, list_of_option.clone(), boundary_elements).unwrap();
        let RuntimeValue::Constructed(at_boundary_value) = &at_boundary else {
            panic!("boundary list must be constructed");
        };
        let ConstructedValueKind::List(elements) = at_boundary_value.kind() else {
            panic!("boundary list must expose the list kind");
        };
        assert_eq!(elements.len(), 32_768);
        let RuntimeValue::Constructed(last) = &elements[32_767] else {
            panic!("the final boundary element must be constructed");
        };
        let ConstructedValueKind::Option(None) = last.kind() else {
            panic!("the final boundary element must be the empty option");
        };

        let overflow =
            RuntimeValue::list(&active, list_of_option, vec![subtree; 32_768]).unwrap_err();
        assert_eq!(
            overflow,
            CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            }
        );
        assert_eq!(overflow.to_string(), "runtime value has too many nodes");
        assert!(std::error::Error::source(&overflow).is_none());

        let list_lower_bound = RuntimeValue::list(&active, list_boolean.clone(), {
            let mut entries = vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES];
            entries.push(RuntimeValue::Text("x".into()));
            entries
        })
        .unwrap_err();
        assert_eq!(
            list_lower_bound,
            CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            }
        );

        let map_lower_bound = RuntimeValue::map(&active, map_boolean, {
            let mut entries = vec![
                (RuntimeValue::Boolean(true), RuntimeValue::Boolean(true));
                MAX_RUNTIME_VALUE_NODES / 2
            ];
            entries.push((RuntimeValue::Text("x".into()), RuntimeValue::Boolean(true)));
            entries
        })
        .unwrap_err();
        assert_eq!(
            map_lower_bound,
            CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            }
        );
    }

    #[test]
    fn constructed_equality_ignores_the_construction_route() {
        let active = active_map_revision();
        let boolean = TypeDescriptor::named(MAP_BOOLEAN);
        let option_boolean = TypeDescriptor::option(boolean.clone()).unwrap();

        let first = RuntimeValue::option(
            &active,
            option_boolean.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();
        let second = RuntimeValue::option(
            &active,
            option_boolean.clone(),
            Some(RuntimeValue::Boolean(true)),
        )
        .unwrap();
        assert_eq!(first, second);

        let map_descriptor = TypeDescriptor::map(boolean.clone(), boolean).unwrap();
        let map_first = RuntimeValue::map(
            &active,
            map_descriptor.clone(),
            vec![
                (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
                (RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();
        let map_second = RuntimeValue::map(
            &active,
            map_descriptor,
            vec![
                (RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)),
                (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap();
        assert_eq!(map_first, map_second);
    }

    #[test]
    fn stale_and_identical_revision_semantics_are_exact() {
        const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
        const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
        const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
        let field_a = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let field_b = RecordValueFieldDefinition::try_new_descriptor(
            B_FIELD,
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();

        let old = active_nested_record_revision_with_seed(vec![field_a.clone()], 0x58, 0x64);
        let current = active_nested_record_revision_with_seed(vec![field_a.clone()], 0x59, 0x65);
        let identical = RecordValue::new(
            &old,
            INNER_TYPE,
            [(String::from("a"), RuntimeValue::Boolean(true))],
        )
        .unwrap();

        let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
        assert!(
            RuntimeValue::list(
                &current,
                list_descriptor.clone(),
                vec![RuntimeValue::Record(identical.clone())],
            )
            .is_ok()
        );
        let option_descriptor = TypeDescriptor::option(TypeDescriptor::named(INNER_TYPE)).unwrap();
        assert!(
            RuntimeValue::option(
                &current,
                option_descriptor.clone(),
                Some(RuntimeValue::Record(identical.clone())),
            )
            .is_ok()
        );
        let map_descriptor = TypeDescriptor::map(
            TypeDescriptor::named(INNER_TYPE),
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        assert!(
            RuntimeValue::map(
                &current,
                map_descriptor.clone(),
                vec![(
                    RuntimeValue::Record(identical.clone()),
                    RuntimeValue::Boolean(true)
                )],
            )
            .is_ok()
        );

        let retired_active = active_record_revision();
        let retired_enum = RuntimeValue::Enum(
            EnumValue::new(&enum_catalogue(&["retired"]), ENUM_TYPE, "retired").unwrap(),
        );
        let enum_list = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
        let error = RuntimeValue::list(&retired_active, enum_list, vec![retired_enum]).unwrap_err();
        let CollectionValueError::InactiveValue { path } = &error else {
            panic!("retired enum label must be inactive: {error}");
        };
        assert_eq!(
            path.segments(),
            &[CollectionValuePathSegment::ListElement(0)]
        );

        let old_two =
            active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b.clone()]);
        let removed = active_nested_record_revision_with_child_fields(vec![field_a.clone()]);
        let old_record = RecordValue::new(
            &old_two,
            INNER_TYPE,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();
        let option_error = RuntimeValue::option(
            &removed,
            option_descriptor,
            Some(RuntimeValue::Record(old_record.clone())),
        )
        .unwrap_err();
        let CollectionValueError::InactiveValue { path } = &option_error else {
            panic!("removed trailing field must be inactive in an option: {option_error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::OptionChild,
                CollectionValuePathSegment::RecordField(B_FIELD),
            ]
        );
        let map_error = RuntimeValue::map(
            &removed,
            map_descriptor,
            vec![(
                RuntimeValue::Record(old_record.clone()),
                RuntimeValue::Boolean(true),
            )],
        )
        .unwrap_err();
        let CollectionValueError::InactiveValue { path } = &map_error else {
            panic!("removed trailing field must be inactive as a map key: {map_error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::MapKey(0),
                CollectionValuePathSegment::RecordField(B_FIELD),
            ]
        );
        let map_value_error = RuntimeValue::map(
            &removed,
            TypeDescriptor::map(
                TypeDescriptor::named(STANDARD_BOOLEAN),
                TypeDescriptor::named(INNER_TYPE),
            )
            .unwrap(),
            vec![(
                RuntimeValue::Boolean(true),
                RuntimeValue::Record(old_record.clone()),
            )],
        )
        .unwrap_err();
        let CollectionValueError::InactiveValue { path } = &map_value_error else {
            panic!("removed trailing field must be inactive as a map value: {map_value_error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::MapValue(0),
                CollectionValuePathSegment::RecordField(B_FIELD),
            ]
        );

        let swapped_id_a = RecordValueFieldDefinition::try_new_descriptor(
            B_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let swapped_id_b = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let reordered_current =
            active_nested_record_revision_with_child_fields(vec![swapped_id_a, swapped_id_b]);
        let reordered_error = RuntimeValue::list(
            &reordered_current,
            list_descriptor,
            vec![RuntimeValue::Record(old_record)],
        )
        .unwrap_err();
        let CollectionValueError::InactiveValue { path } = &reordered_error else {
            panic!("reordered field identities must be inactive: {reordered_error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::ListElement(0),
                CollectionValuePathSegment::RecordField(B_FIELD),
            ]
        );

        let integer_standard = verified_standard_with_value_types(vec![
            standard_boolean_definition(),
            map_standard_primitive(MAP_INTEGER, "integer", "orna.kernel.value.integer@1"),
        ]);
        let boolean_field = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap();
        let integer_field = RecordValueFieldDefinition::try_new_descriptor(
            A_FIELD,
            "a",
            0,
            TypeDescriptor::named(MAP_INTEGER),
        )
        .unwrap();
        let boolean_active = active_nested_record_revision_with_standard_and_seed(
            vec![boolean_field],
            integer_standard.clone(),
            0x5a,
            0x66,
        );
        let integer_active = active_nested_record_revision_with_standard_and_seed(
            vec![integer_field],
            integer_standard,
            0x5b,
            0x67,
        );
        let boolean_record = RecordValue::new(
            &boolean_active,
            INNER_TYPE,
            [(String::from("a"), RuntimeValue::Boolean(true))],
        )
        .unwrap();
        let changed_error = RuntimeValue::list(
            &integer_active,
            TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap(),
            vec![RuntimeValue::Record(boolean_record)],
        )
        .unwrap_err();
        let CollectionValueError::ValueTypeMismatch { path } = &changed_error else {
            panic!("changed record field type must be a type mismatch: {changed_error}");
        };
        assert_eq!(
            path.segments(),
            &[
                CollectionValuePathSegment::ListElement(0),
                CollectionValuePathSegment::RecordField(A_FIELD),
            ]
        );
        assert_eq!(
            changed_error.to_string(),
            "collection value has a type mismatch"
        );
        assert!(std::error::Error::source(&changed_error).is_none());
    }

    fn deep_record_chain_revision_error() -> crate::canonical_hash::CanonicalHashError {
        let standard = verified_standard_with_value_types(vec![standard_boolean_definition()]);
        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
        let mut records = Vec::new();
        for index in 0..34_u8 {
            let record_type = TypeId::from_bytes([0x20 + index; 16]);
            let field_id = FieldId::from_bytes([0x80 + index; 16]);
            let target = if index == 33 {
                STANDARD_BOOLEAN
            } else {
                TypeId::from_bytes([0x20 + index + 1; 16])
            };
            records.push(RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["crm", &format!("chain_{index}")]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        field_id,
                        if index == 33 { "value" } else { "next" },
                        0,
                        TypeDescriptor::named(target),
                    )
                    .unwrap(),
                ],
            ));
        }
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![],
            records,
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let mut identities = vec![DefinitionIdentity::Schema(application_schema)];
        for index in 0..34_u8 {
            let record_type = TypeId::from_bytes([0x20 + index; 16]);
            let field_id = FieldId::from_bytes([0x80 + index; 16]);
            identities.push(DefinitionIdentity::ValueType(record_type));
            identities.push(DefinitionIdentity::Field {
                owner: record_type,
                field: field_id,
            });
        }
        let origins = identities
            .into_iter()
            .enumerate()
            .map(|(index, identity)| {
                DefinitionOrigin::new(
                    identity,
                    SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap_err()
    }

    #[test]
    fn descriptor_and_record_nesting_depth_boundaries_are_exact() {
        let active = active_record_revision();
        let mut descriptor = TypeDescriptor::named(STANDARD_BOOLEAN);
        let mut value = RuntimeValue::Boolean(true);
        for _ in 0..32 {
            descriptor = TypeDescriptor::option(descriptor).unwrap();
            value = RuntimeValue::option(&active, descriptor.clone(), Some(value)).unwrap();
        }
        assert_eq!(value.runtime_type(), RuntimeType::Constructed(&descriptor));
        let too_deep = TypeDescriptor::option(descriptor).unwrap_err();
        assert_eq!(
            too_deep,
            crate::types::TypeDescriptorError::TooDeep {
                maximum: 32,
                actual: 33,
            }
        );
        assert_eq!(too_deep.to_string(), "type descriptor is too deep");
        assert!(std::error::Error::source(&too_deep).is_none());

        let error = deep_record_chain_revision_error();
        match error {
            crate::canonical_hash::CanonicalHashError::RecordValueNestingTooDeep {
                record_value_type,
                field,
                nested_record_value_type,
                maximum,
                actual,
            } => {
                assert_eq!(record_value_type, TypeId::from_bytes([0x20 + 32; 16]));
                assert_eq!(field, FieldId::from_bytes([0x80 + 32; 16]));
                assert_eq!(
                    nested_record_value_type,
                    TypeId::from_bytes([0x20 + 33; 16])
                );
                assert_eq!(maximum, 32);
                assert_eq!(actual, 33);
            }
            other => panic!("deep record chain must fail as nesting too deep: {other:?}"),
        }
        assert_eq!(
            error.to_string(),
            "record value nesting exceeds the maximum depth"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn collection_error_variants_preserve_exact_display_and_source() {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
        let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
        let list_enum = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
        let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

        let cases = [
            (
                RuntimeValue::list(&active, option_desc, vec![]).unwrap_err(),
                "collection descriptor has the wrong outer constructor",
            ),
            (
                RuntimeValue::list(
                    &active,
                    TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])))
                        .unwrap(),
                    vec![],
                )
                .unwrap_err(),
                "collection descriptor is not supported",
            ),
            (
                RuntimeValue::list(
                    &active,
                    list_boolean.clone(),
                    vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES],
                )
                .unwrap_err(),
                "runtime value has too many nodes",
            ),
            (
                RuntimeValue::list(
                    &active,
                    list_boolean.clone(),
                    vec![
                        RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
                    ],
                )
                .unwrap_err(),
                "collection values cannot contain legacy typed NULL",
            ),
            (
                RuntimeValue::list(&active, list_boolean, vec![RuntimeValue::Text("x".into())])
                    .unwrap_err(),
                "collection value has a type mismatch",
            ),
            (
                RuntimeValue::list(
                    &active,
                    list_enum,
                    vec![RuntimeValue::Enum(
                        EnumValue::new(&enum_catalogue(&["retired"]), ENUM_TYPE, "retired")
                            .unwrap(),
                    )],
                )
                .unwrap_err(),
                "collection value is not active",
            ),
            (
                RuntimeValue::map(
                    &active,
                    map_boolean,
                    vec![
                        (RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)),
                        (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
                    ],
                )
                .unwrap_err(),
                "map contains a duplicate key",
            ),
        ];
        for (error, expected_display) in cases {
            assert_eq!(error.to_string(), expected_display);
            assert!(std::error::Error::source(&error).is_none());
        }

        let ambiguous = RuntimeValue::list(
            &active_record_revision_with_opaque_contract(
                TypeId::from_bytes([0x49; 16]),
                OPAQUE_CONTRACT,
            ),
            TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0x49; 16]))).unwrap(),
            vec![],
        )
        .unwrap_err();
        assert_eq!(
            ambiguous.to_string(),
            "collection descriptor type is present in both application and standard catalogues"
        );
        assert!(std::error::Error::source(&ambiguous).is_none());
    }

    proptest::proptest! {
        #[test]
        fn constructed_constructors_never_panic_on_bounded_public_input(
            descriptor_choice in 0usize..10,
            value_choice in 0usize..8,
            count in 0usize..6,
        ) {
            let active = active_record_revision();
            let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
            let descriptors = vec![
                TypeDescriptor::option(boolean.clone()).unwrap(),
                TypeDescriptor::list(boolean.clone()).unwrap(),
                TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap(),
                TypeDescriptor::option(TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])))
                    .unwrap(),
                TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap(),
                TypeDescriptor::set(boolean.clone()).unwrap(),
                TypeDescriptor::stream(boolean.clone()).unwrap(),
                TypeDescriptor::option(TypeDescriptor::list(boolean.clone()).unwrap()).unwrap(),
                TypeDescriptor::map(
                    TypeDescriptor::named(ENUM_TYPE),
                    TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])),
                )
                .unwrap(),
                TypeDescriptor::list(
                    TypeDescriptor::option(TypeDescriptor::named(ENUM_TYPE)).unwrap(),
                )
                .unwrap(),
            ];
            let values = [
                RuntimeValue::Boolean(true),
                RuntimeValue::Integer(1),
                RuntimeValue::Text("x".into()),
                RuntimeValue::Bytes(vec![1]),
                RuntimeValue::Enum(
                    EnumValue::new(&enum_catalogue(&["lead"]), ENUM_TYPE, "lead").unwrap(),
                ),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
                RuntimeValue::Reference {
                    target: TypeId::from_bytes([0x41; 16]),
                    object: ObjectId::from_bytes([0x42; 16]),
                },
                RuntimeValue::option(
                    &active,
                    TypeDescriptor::option(boolean).unwrap(),
                    Some(RuntimeValue::Boolean(false)),
                )
                .unwrap(),
            ];
            let descriptor = descriptors[descriptor_choice].clone();
            let value = values[value_choice].clone();
            let _ = RuntimeValue::option(&active, descriptor.clone(), Some(value.clone()));
            let _ = RuntimeValue::list(
                &active,
                descriptor.clone(),
                vec![value.clone(); count],
            );
            let _ = RuntimeValue::map(
                &active,
                descriptor.clone(),
                (0..count)
                    .map(|index| {
                        (
                            values[(value_choice + index) % values.len()].clone(),
                            value.clone(),
                        )
                    })
                    .collect(),
            );
        }
    }

    #[test]
    fn admitted_leaf_values_construct_in_option_list_and_map() {
        let active = active_map_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let record = RecordValue::new(
            &active,
            MAP_FLAT,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Integer(1)),
            ],
        )
        .unwrap();
        let leaf_cases = [
            (
                TypeDescriptor::named(MAP_BOOLEAN),
                RuntimeValue::Boolean(true),
            ),
            (TypeDescriptor::named(MAP_INTEGER), RuntimeValue::Integer(1)),
            (TypeDescriptor::named(MAP_BIGINT), RuntimeValue::BigInt(2)),
            (
                TypeDescriptor::named(MAP_FLOAT),
                RuntimeValue::Float(RuntimeFloat::new(3.5).unwrap()),
            ),
            (
                TypeDescriptor::named(MAP_TEXT),
                RuntimeValue::Text("t".into()),
            ),
            (
                TypeDescriptor::named(MAP_BYTES),
                RuntimeValue::Bytes(vec![1, 2]),
            ),
            (
                TypeDescriptor::named(MAP_APP_ENUM),
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap(),
                ),
            ),
            (
                TypeDescriptor::named(MAP_STD_ENUM),
                RuntimeValue::Enum(
                    EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
                ),
            ),
            (
                TypeDescriptor::named(MAP_FLAT),
                RuntimeValue::Record(record),
            ),
            (
                TypeDescriptor::reference(MAP_OBJECT),
                RuntimeValue::Reference {
                    target: MAP_OBJECT,
                    object: ObjectId::from_bytes([0x01; 16]),
                },
            ),
        ];
        for (descriptor, value) in leaf_cases {
            let option = RuntimeValue::option(
                &active,
                TypeDescriptor::option(descriptor.clone()).unwrap(),
                Some(value.clone()),
            )
            .expect("admitted leaf must construct an option");
            let RuntimeValue::Constructed(option_value) = &option else {
                panic!("admitted leaf option must be constructed");
            };
            let ConstructedValueKind::Option(Some(child)) = option_value.kind() else {
                panic!("admitted leaf option must expose its child");
            };
            assert_eq!(child, &value);

            let list = RuntimeValue::list(
                &active,
                TypeDescriptor::list(descriptor.clone()).unwrap(),
                vec![value.clone()],
            )
            .expect("admitted leaf must construct a list");
            let RuntimeValue::Constructed(list_value) = &list else {
                panic!("admitted leaf list must be constructed");
            };
            let ConstructedValueKind::List(elements) = list_value.kind() else {
                panic!("admitted leaf list must expose elements");
            };
            assert_eq!(elements, std::slice::from_ref(&value));

            let map = RuntimeValue::map(
                &active,
                TypeDescriptor::map(descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN))
                    .unwrap(),
                vec![(value.clone(), RuntimeValue::Boolean(true))],
            )
            .expect("admitted leaf must construct a map key");
            let RuntimeValue::Constructed(map_value) = &map else {
                panic!("admitted leaf map must be constructed");
            };
            let ConstructedValueKind::Map(entries) = map_value.kind() else {
                panic!("admitted leaf map must expose entries");
            };
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, value);
        }
    }

    #[test]
    fn record_value_accepts_a_verified_standard_enum_field_in_declaration_order() {
        let active = active_map_revision();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("the map fixture pins a verified standard library");
        let mode = EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta")
            .expect("the verified standard enum declares beta");

        let record = RecordValue::new(
            &active,
            MAP_STD_ENUM_RECORD,
            [
                (String::from("mode"), RuntimeValue::Enum(mode.clone())),
                (String::from("enabled"), RuntimeValue::Boolean(true)),
            ],
        )
        .expect("a verified standard enum is an admitted record field type");

        assert_eq!(record.record_type(), MAP_STD_ENUM_RECORD);
        assert_eq!(record.fields().len(), 2);
        assert_eq!(record.fields()[0], RuntimeValue::Boolean(true));
        assert_eq!(record.fields()[1], RuntimeValue::Enum(mode.clone()));
        let RuntimeValue::Enum(mode_value) = &record.fields()[1] else {
            panic!("the mode field must retain its enum value");
        };
        assert_eq!(mode_value.enum_type(), MAP_STD_ENUM);
        assert_eq!(mode_value.label(), "beta");
        assert_eq!(
            RuntimeValue::Record(record).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(MAP_STD_ENUM_RECORD))
        );
    }

    #[test]
    fn record_value_rejects_a_stale_verified_standard_enum_label() {
        let active = active_map_revision();
        let stale = RuntimeValue::Enum(
            EnumValue::new(
                &standard_enum_catalogue(&["retired"]),
                MAP_STD_ENUM,
                "retired",
            )
            .expect("the stale standard catalogue declares retired"),
        );

        let error = RecordValue::new(
            &active,
            MAP_STD_ENUM_RECORD,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("mode"), stale),
            ],
        )
        .expect_err("a stale standard enum label must not enter a current record");

        assert_eq!(
            error,
            RecordValueError::InactiveEnumLabel {
                record_type: MAP_STD_ENUM_RECORD,
                field: MAP_STD_ENUM_FIELD,
                enum_type: MAP_STD_ENUM,
                label: String::from("retired"),
            }
        );
    }

    #[test]
    fn verified_standard_enum_value_rejects_an_undeclared_label() {
        let active = active_map_revision();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("the map fixture pins a verified standard library");

        assert_eq!(
            EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "retired"),
            Err(EnumValueError::UndeclaredLabel {
                enum_type: MAP_STD_ENUM,
                label: String::from("retired"),
            })
        );
    }

    #[test]
    fn stale_child_record_value_is_rejected_with_exact_inactive_nested_record_error() {
        let child_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let old = active_nested_record_revision();
        let current = active_nested_record_revision_with_child_fields(vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "value",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "checked",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ]);
        let old_child = RecordValue::new(
            &old,
            child_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the child must construct under the old revision");

        let error = RecordValue::new(
            &current,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(old_child))],
        )
        .expect_err("a stale child must be rejected by the current outer");
        assert_eq!(
            error,
            RecordValueError::InactiveNestedRecord {
                record_type: outer_type,
                field: outer_field,
                nested_record_type: child_type,
            }
        );
        assert_eq!(error.to_string(), "nested record field value is not active");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn nominal_mismatch_precedes_recursive_activity_checking() {
        let active = active_nested_record_revision();
        let child_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let outer_field = FieldId::from_bytes([0x3b; 16]);
        let inner = RecordValue::new(
            &active,
            child_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the flat inner record must construct");
        let outer = RecordValue::new(
            &active,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(inner))],
        )
        .expect("a valid nested record must construct");

        let error = RecordValue::new(
            &active,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(outer))],
        )
        .expect_err("a nominal mismatch must be rejected");
        assert_eq!(
            error,
            RecordValueError::FieldTypeMismatch {
                record_type: outer_type,
                field: outer_field,
                expected: ResolvedType::named(child_type),
                actual: ResolvedType::named(outer_type),
            }
        );
    }

    #[test]
    fn nested_record_value_carries_no_creation_provenance_identity() {
        let child_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "value",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let old = active_nested_record_revision();
        let fresh = active_nested_record_revision_with_seed(child_fields, 0x77, 0x78);
        let child_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let old_child = RecordValue::new(
            &old,
            child_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the child must construct under the old revision");

        let outer = RecordValue::new(
            &fresh,
            outer_type,
            [(
                String::from("payload"),
                RuntimeValue::Record(old_child.clone()),
            )],
        )
        .expect("a semantically identical revision must accept the child");
        assert_eq!(outer.record_type(), outer_type);
        let [RuntimeValue::Record(inner_value)] = outer.fields() else {
            panic!("outer payload must hold the child record value");
        };
        assert_eq!(inner_value, &old_child);
        assert_eq!(
            RuntimeValue::Record(outer).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(outer_type))
        );
    }

    #[test]
    fn nested_record_value_chain_walks_32_edges_to_the_boolean_leaf() {
        let active = active_record_chain_revision();
        let root_type = TypeId::from_bytes([0x20; 16]);
        let leaf_type = TypeId::from_bytes([0x40; 16]);
        let leaf = RecordValue::new(
            &active,
            leaf_type,
            [(String::from("value"), RuntimeValue::Boolean(true))],
        )
        .expect("the leaf record must construct");
        let mut value = RuntimeValue::Record(leaf);
        for index in (0..32).rev() {
            let record_type = TypeId::from_bytes([0x20 + index; 16]);
            value = RuntimeValue::Record(
                RecordValue::new(&active, record_type, [(String::from("next"), value)])
                    .expect("each parent record must construct"),
            );
        }
        let RuntimeValue::Record(root) = value else {
            panic!("the root must be a record value");
        };
        assert_eq!(root.record_type(), root_type);

        let mut current = &root;
        let mut edges = 0;
        loop {
            let [field] = current.fields() else {
                panic!("each chain record must hold exactly one field");
            };
            match field {
                RuntimeValue::Record(next) => {
                    edges += 1;
                    current = next;
                }
                RuntimeValue::Boolean(stored) => {
                    assert!(*stored, "the leaf must hold Boolean true");
                    break;
                }
                other => panic!("unexpected chain leaf value {other:?}"),
            }
        }
        assert_eq!(edges, 32, "the root must reach the leaf through 32 edges");
    }

    #[test]
    fn reversed_child_declaration_order_is_inactive_in_the_current_revision() {
        let child_type = TypeId::from_bytes([0x31; 16]);
        let outer_type = TypeId::from_bytes([0x30; 16]);
        let old_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let current_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let old = active_nested_record_revision_with_child_fields(old_fields);
        let current = active_nested_record_revision_with_child_fields(current_fields);
        let old_child = RecordValue::new(
            &old,
            child_type,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(false)),
            ],
        )
        .expect("the child must construct under the old revision");

        let error = RecordValue::new(
            &current,
            outer_type,
            [(String::from("payload"), RuntimeValue::Record(old_child))],
        )
        .expect_err("reversed declaration order must be inactive in the current revision");
        assert_eq!(
            error,
            RecordValueError::InactiveNestedRecord {
                record_type: outer_type,
                field: FieldId::from_bytes([0x3b; 16]),
                nested_record_type: child_type,
            }
        );
    }

    fn active_record_revision() -> ActiveDatabaseRevision {
        active_record_revision_with_type(RECORD_TYPE)
    }

    fn active_record_revision_with_type(record_type: TypeId) -> ActiveDatabaseRevision {
        active_record_revision_with_opaque_contract(record_type, OPAQUE_CONTRACT)
    }

    fn active_record_revision_with_opaque_contract(
        record_type: TypeId,
        opaque_contract: &str,
    ) -> ActiveDatabaseRevision {
        let standard = verified_standard_with_value_types(vec![
            standard_boolean_definition(),
            opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, opaque_contract),
        ]);

        active_record_revision_with_standard(record_type, standard)
    }

    fn verified_standard_with_value_types(
        value_types: Vec<ValueTypeDefinition>,
    ) -> VerifiedStandardLibrarySnapshot {
        verified_standard_with_value_types_and_schemas(value_types, Vec::new())
    }

    fn verified_standard_with_value_types_and_schemas(
        value_types: Vec<ValueTypeDefinition>,
        extra_schemas: Vec<SchemaDefinition>,
    ) -> VerifiedStandardLibrarySnapshot {
        let standard_unit_content = "x".repeat(value_types.len() + extra_schemas.len() + 2);
        let standard_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x50; 16]),
            0,
            "std/types.orna",
            &standard_unit_content,
            source_unit_content_digest(&standard_unit_content).unwrap(),
        )
        .unwrap();
        let standard_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&standard_unit)).unwrap();
        let standard_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x51; 16]),
            SourceRevisionId::from_bytes([0x52; 16]),
            None,
            vec![standard_unit],
            standard_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x51; 16]),
                None,
                standard_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let standard_schema = SchemaId::from_bytes([0x53; 16]);
        let standard_types_schema = SchemaId::from_bytes([0x54; 16]);
        let mut schemas = vec![
            SchemaDefinition::new(
                standard_schema,
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                standard_types_schema,
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ];
        schemas.extend(extra_schemas);
        let standard_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x5b; 16]),
            schemas,
            vec![],
            value_types,
            vec![],
        )
        .unwrap();
        let mut standard_origins = standard_catalogue
            .schemas()
            .iter()
            .enumerate()
            .map(|(index, schema)| {
                let start = u32::try_from(index).unwrap();
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(schema.id()),
                    SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1)
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        standard_origins.extend(standard_catalogue.value_types().iter().enumerate().map(
            |(index, value_type)| {
                let start = u32::try_from(index + standard_catalogue.schemas().len()).unwrap();
                DefinitionOrigin::new(
                    DefinitionIdentity::ValueType(value_type.id()),
                    SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1)
                        .unwrap(),
                )
            },
        ));
        let provisional_standard = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0x55; 16]),
            StandardLibraryDigestVersion::Version1,
            standard_source.clone(),
            "orna.language/1",
            standard_catalogue.clone(),
            standard_origins.clone(),
            Sha256Digest::from_bytes([0x56; 32]),
        )
        .unwrap();
        let standard_digest =
            calculate_standard_library_digest_for_test(&provisional_standard).unwrap();
        verify_standard_library_snapshot(
            StandardLibrarySnapshot::new(
                provisional_standard.revision(),
                provisional_standard.digest_version(),
                standard_source,
                provisional_standard.language_version(),
                standard_catalogue,
                standard_origins,
                standard_digest,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn active_record_revision_with_standard(
        record_type: TypeId,
        standard: VerifiedStandardLibrarySnapshot,
    ) -> ActiveDatabaseRevision {
        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
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
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        ENABLED_FIELD,
                        "enabled",
                        0,
                        TypeDescriptor::named(STANDARD_BOOLEAN),
                    )
                    .unwrap(),
                    RecordValueFieldDefinition::try_new_descriptor(
                        STAGE_FIELD,
                        "stage",
                        1,
                        TypeDescriptor::named(ENUM_TYPE),
                    )
                    .unwrap(),
                ],
            )],
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "abcde";
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x63; 16]),
            0,
            "app/types.orna",
            application_content,
            source_unit_content_digest(application_content).unwrap(),
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([0x64; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x65; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x65; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(application_schema),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(ENUM_TYPE),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record_type),
                SourceOrigin::new(source_unit, 2, 3).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: ENABLED_FIELD,
                },
                SourceOrigin::new(source_unit, 3, 4).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: STAGE_FIELD,
                },
                SourceOrigin::new(source_unit, 4, 5).unwrap(),
            ),
        ];
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    fn standard_boolean_definition() -> ValueTypeDefinition {
        ValueTypeDefinition::primitive(
            STANDARD_BOOLEAN,
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )
    }

    fn opaque_definition(
        opaque_type: TypeId,
        name: impl IntoIterator<Item = &'static str>,
        contract: &str,
    ) -> ValueTypeDefinition {
        ValueTypeDefinition::opaque(
            opaque_type,
            QualifiedSemanticName::new(name).unwrap(),
            contract,
        )
    }

    fn opaque_registration(
        opaque_type: TypeId,
        name: impl IntoIterator<Item = &'static str>,
        contract: &str,
    ) -> OpaqueCodecRegistration {
        OpaqueCodecRegistration::fixed_length_identity(
            opaque_type,
            QualifiedSemanticName::new(name).unwrap(),
            contract,
            16,
        )
        .unwrap()
    }

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

    fn standard_enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x96; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x97; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                MAP_STD_ENUM,
                QualifiedSemanticName::new(["std", "mode"]).unwrap(),
                labels.iter().copied(),
            )],
            vec![],
        )
        .unwrap()
    }

    fn column(name: &str, resolved_type: ResolvedType, nullable: bool) -> ResultColumn {
        ResultColumn::new(name, resolved_type, nullable).unwrap()
    }

    #[test]
    fn opaque_codec_registry_is_complete_unique_and_exact() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let accepted = opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT);
        assert!(OpaqueCodecRegistry::new(standard, [accepted.clone()]).is_ok());
        assert_eq!(
            OpaqueCodecRegistry::new(standard, Vec::<OpaqueCodecRegistration>::new()).unwrap_err(),
            OpaqueCodecRegistryError::EmptyRegistry
        );

        let duplicate_type = opaque_registration(
            OPAQUE_TYPE,
            ["std", "types", "other_token"],
            "orna.std.value.other-token@1",
        );
        assert_eq!(
            OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_type]).unwrap_err(),
            OpaqueCodecRegistryError::DuplicateType {
                opaque_type: OPAQUE_TYPE,
            }
        );
        let duplicate_name = opaque_registration(
            OTHER_OPAQUE_TYPE,
            OPAQUE_NAME,
            "orna.std.value.other-token@1",
        );
        assert_eq!(
            OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_name]).unwrap_err(),
            OpaqueCodecRegistryError::DuplicateName {
                semantic_name: QualifiedSemanticName::new(OPAQUE_NAME).unwrap(),
            }
        );
        let duplicate_contract = opaque_registration(
            OTHER_OPAQUE_TYPE,
            ["std", "types", "other_token"],
            OPAQUE_CONTRACT,
        );
        assert_eq!(
            OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_contract]).unwrap_err(),
            OpaqueCodecRegistryError::DuplicateContract {
                representation_contract: OPAQUE_CONTRACT.into(),
            }
        );

        for (registration, expected) in [
            (
                opaque_registration(
                    OTHER_OPAQUE_TYPE,
                    ["std", "types", "missing"],
                    "orna.std.value.missing@1",
                ),
                OpaqueCodecRegistryError::MissingDefinition {
                    opaque_type: OTHER_OPAQUE_TYPE,
                },
            ),
            (
                opaque_registration(
                    STANDARD_BOOLEAN,
                    ["std", "boolean"],
                    "orna.kernel.value.boolean@1",
                ),
                OpaqueCodecRegistryError::WrongDefinitionKind {
                    opaque_type: STANDARD_BOOLEAN,
                },
            ),
            (
                opaque_registration(
                    OPAQUE_TYPE,
                    ["std", "types", "wrong_token"],
                    OPAQUE_CONTRACT,
                ),
                OpaqueCodecRegistryError::SemanticNameMismatch {
                    opaque_type: OPAQUE_TYPE,
                },
            ),
            (
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, "orna.std.value.wrong-token@1"),
                OpaqueCodecRegistryError::ContractMismatch {
                    opaque_type: OPAQUE_TYPE,
                },
            ),
        ] {
            assert_eq!(
                OpaqueCodecRegistry::new(standard, [registration]).unwrap_err(),
                expected
            );
        }

        let expanded_standard = verified_standard_with_value_types(vec![
            standard_boolean_definition(),
            opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            opaque_definition(
                OTHER_OPAQUE_TYPE,
                ["std", "types", "other_token"],
                "orna.std.value.other-token@1",
            ),
        ]);
        assert_eq!(
            OpaqueCodecRegistry::new(&expanded_standard, [accepted]).unwrap_err(),
            OpaqueCodecRegistryError::UnregisteredOpaqueDefinition {
                opaque_type: OTHER_OPAQUE_TYPE,
            }
        );
    }

    #[test]
    fn opaque_values_require_the_same_active_standard_and_exact_payload() {
        let active = active_record_revision();
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [opaque_registration(
                OPAQUE_TYPE,
                OPAQUE_NAME,
                OPAQUE_CONTRACT,
            )],
        )
        .unwrap();

        for position in 0..16 {
            for byte in u8::MIN..=u8::MAX {
                let mut payload = [0; 16];
                payload[position] = byte;
                let value = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, payload).unwrap();
                assert_eq!(value.opaque_type(), OPAQUE_TYPE);
                assert_eq!(value.canonical_payload(), payload);
                assert_eq!(
                    RuntimeValue::Opaque(value.clone()).runtime_type(),
                    RuntimeType::Flat(ResolvedType::value(OPAQUE_TYPE))
                );
                assert_eq!(value, value.clone());
            }
        }

        for length in (0..=32).filter(|length| *length != 16) {
            assert_eq!(
                OpaqueValue::new(&active, &registry, OPAQUE_TYPE, vec![0; length]),
                Err(OpaqueValueError::WrongPayloadLength {
                    opaque_type: OPAQUE_TYPE,
                    expected: 16,
                    actual: length,
                })
            );
        }
        assert_eq!(
            OpaqueValue::new(&active, &registry, OTHER_OPAQUE_TYPE, [0; 16]),
            Err(OpaqueValueError::UnregisteredType {
                opaque_type: OTHER_OPAQUE_TYPE,
            })
        );

        let stale = active_record_revision_with_opaque_contract(
            TypeId::from_bytes([0x4b; 16]),
            "orna.std.value.opaque-token@2",
        );
        assert_eq!(
            OpaqueValue::new(&stale, &registry, OPAQUE_TYPE, [0; 16]),
            Err(OpaqueValueError::ActiveStandardMismatch)
        );

        let value = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, [0; 16]).unwrap();
        let runtime_value = RuntimeValue::Opaque(value.clone());
        let parameter = ParameterId::from_bytes([0x4c; 16]);
        assert_eq!(
            FunctionArgument::new(parameter, runtime_value.clone())
                .unwrap()
                .value(),
            &runtime_value
        );
        assert_eq!(
            ResultRows::new(
                [ResultColumn::new("opaque", ResolvedType::value(OPAQUE_TYPE), false).unwrap()],
                [ResultRow::new([runtime_value])],
            ),
            Err(ResultRowsError::OpaqueValueNotAccepted {
                row: 0,
                column: 0,
                opaque_type: OPAQUE_TYPE,
            })
        );
        assert_ne!(
            value,
            OpaqueValue {
                opaque_type: OTHER_OPAQUE_TYPE,
                canonical_payload: vec![0; 16],
            }
        );
    }

    #[test]
    fn action_frame_rejects_zero_target_revision_call_site_result_and_parameter_identities() {
        let mut integer = b"ORV3".to_vec();
        integer.push(0x03);
        integer.extend_from_slice(&[0x12; 16]);
        integer.extend_from_slice(&4_u32.to_be_bytes());
        integer.extend_from_slice(&7_i32.to_be_bytes());

        let mut body = vec![ACTION_DOMAIN_CLIENT];
        for byte in 0x21..=0x25 {
            body.extend_from_slice(&[byte; ACTION_IDENTITY_BYTES]);
        }
        body.extend_from_slice(&1_u32.to_be_bytes());
        body.extend_from_slice(&[0x31; ACTION_IDENTITY_BYTES]);
        body.extend_from_slice(&(integer.len() as u32).to_be_bytes());
        body.extend_from_slice(&integer);
        let mut payload = b"ORNA-ACTION/1 ".to_vec();
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(&body);

        validate_action_frame(OPAQUE_TYPE, b"ORNA-ACTION/1 ", &payload)
            .expect("valid action identities are accepted");
        for offset in [1, 17, 33, 49, 65, 85] {
            let mut corrupted = payload.clone();
            let body_offset = b"ORNA-ACTION/1 ".len() + 4;
            corrupted[body_offset + offset..body_offset + offset + ACTION_IDENTITY_BYTES].fill(0);
            assert!(matches!(
                validate_action_frame(OPAQUE_TYPE, b"ORNA-ACTION/1 ", &corrupted),
                Err(OpaqueValueError::InvalidActionFrame { .. })
            ));
        }
    }

    #[test]
    fn ui_value_codec_enforces_closed_canonical_shape_after_framing() {
        const UI_TYPE: TypeId = TypeId::from_bytes([0x55; 16]);
        const UI_NAME: [&str; 3] = ["std", "ui", "ui"];
        const UI_CONTRACT: &str = "orna.std.value.ui@1";
        const UI_MAGIC: &str = "ORNA-UI/1 ";

        let active = active_record_revision_with_standard(
            RECORD_TYPE,
            verified_standard_with_value_types_and_schemas(
                vec![
                    standard_boolean_definition(),
                    opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                    opaque_definition(UI_TYPE, UI_NAME, UI_CONTRACT),
                ],
                vec![SchemaDefinition::new(
                    SchemaId::from_bytes([0x56; 16]),
                    QualifiedSemanticName::new(["std", "ui"]).unwrap(),
                )],
            ),
        );
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                OpaqueCodecRegistration::length_prefixed_canonical_json(
                    UI_TYPE,
                    QualifiedSemanticName::new(UI_NAME).unwrap(),
                    UI_CONTRACT,
                    UI_MAGIC,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let frame = |body: &[u8]| {
            let mut payload = Vec::from(UI_MAGIC.as_bytes());
            payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
            payload.extend_from_slice(body);
            payload
        };

        for body in [
            br#"{"kind":"empty"}"#.as_slice(),
            br#"{"children":[{"kind":"empty"}],"kind":"fragment"}"#.as_slice(),
            br#"{"actions":{"activate":{"action_id":"activate","debug_kind":null,"input_type":"std.ui.event","trace":true}},"call_site_id":null,"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"function_instance_id":"fn-1","key":{"id":1},"kind":"node","properties":{"title":{"type":"std.text","value":"Hello"}},"slots":{"content":[{"kind":"empty"}]},"source_origin":{"end":2,"source_unit_id":"unit-1","start":1}}"#.as_slice(),
        ] {
            let payload = frame(body);
            let value = OpaqueValue::new(&active, &registry, UI_TYPE, &payload)
                .expect("the closed canonical UI shape constructs");
            assert_eq!(value.canonical_payload(), payload.as_slice());
        }

        for body in [
            br#"{"kind":"not-a-ui-kind"}"#.as_slice(),
            br#"{"actions":{},"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"kind":"node","properties":{},"slots":{},"unknown":null}"#.as_slice(),
            br#"{"children":[{"kind":"not-a-ui-kind"}],"kind":"fragment"}"#.as_slice(),
        ] {
            assert_eq!(
                OpaqueValue::new(&active, &registry, UI_TYPE, frame(body)),
                Err(OpaqueValueError::InvalidJsonBody { opaque_type: UI_TYPE })
            );
        }
        let mut deep = serde_json::json!({"kind": "empty"});
        for _ in 0..40 {
            deep = serde_json::json!({"children": [deep], "kind": "fragment"});
        }
        let deep_body = serde_json::to_vec(&deep).unwrap();
        let deep_value = OpaqueValue::new(&active, &registry, UI_TYPE, frame(&deep_body))
            .expect("schema-valid UI values do not have an arbitrary depth limit");
        assert_eq!(deep_value.canonical_payload(), frame(&deep_body));
    }

    #[test]
    fn framed_codec_constructors_reject_invalid_magic_prefixes() {
        let name = ["std", "terminal", "document"];
        for magic in [
            "",
            "ORNA-TERMINAL-DOCUMENT/1 \u{00e9}",
            "x".repeat(65).as_str(),
        ] {
            assert_eq!(
                OpaqueCodecRegistration::length_prefixed_utf8(
                    OPAQUE_TYPE,
                    QualifiedSemanticName::new(name).unwrap(),
                    OPAQUE_CONTRACT,
                    magic,
                )
                .unwrap_err(),
                OpaqueCodecRegistryError::InvalidMagic {
                    opaque_type: OPAQUE_TYPE,
                }
            );
            assert_eq!(
                OpaqueCodecRegistration::media_type_framed(
                    OPAQUE_TYPE,
                    QualifiedSemanticName::new(name).unwrap(),
                    OPAQUE_CONTRACT,
                    magic,
                )
                .unwrap_err(),
                OpaqueCodecRegistryError::InvalidMagic {
                    opaque_type: OPAQUE_TYPE,
                }
            );
            assert_eq!(
                OpaqueCodecRegistration::length_prefixed_canonical_json(
                    OPAQUE_TYPE,
                    QualifiedSemanticName::new(name).unwrap(),
                    OPAQUE_CONTRACT,
                    magic,
                )
                .unwrap_err(),
                OpaqueCodecRegistryError::InvalidMagic {
                    opaque_type: OPAQUE_TYPE,
                }
            );
        }
        assert!(
            OpaqueCodecRegistration::length_prefixed_utf8(
                OPAQUE_TYPE,
                QualifiedSemanticName::new(name).unwrap(),
                OPAQUE_CONTRACT,
                " ",
            )
            .is_ok()
        );
    }

    #[test]
    fn terminal_document_codec_enforces_canonical_text_payloads() {
        const DOCUMENT_TYPE: TypeId = TypeId::from_bytes([0x4d; 16]);
        const DOCUMENT_MAGIC: &str = "ORNA-TERMINAL-DOCUMENT/1 ";
        const DOCUMENT_NAME: [&str; 3] = ["std", "terminal", "document"];
        const DOCUMENT_CONTRACT: &str = "orna.std.value.terminal-document@1";

        let active = active_record_revision_with_standard(
            RECORD_TYPE,
            verified_standard_with_value_types_and_schemas(
                vec![
                    standard_boolean_definition(),
                    opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                    opaque_definition(DOCUMENT_TYPE, DOCUMENT_NAME, DOCUMENT_CONTRACT),
                ],
                vec![SchemaDefinition::new(
                    SchemaId::from_bytes([0x4f; 16]),
                    QualifiedSemanticName::new(["std", "terminal"]).unwrap(),
                )],
            ),
        );
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                OpaqueCodecRegistration::length_prefixed_utf8(
                    DOCUMENT_TYPE,
                    QualifiedSemanticName::new(DOCUMENT_NAME).unwrap(),
                    DOCUMENT_CONTRACT,
                    DOCUMENT_MAGIC,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let mut payload = Vec::from(DOCUMENT_MAGIC.as_bytes());
        payload.extend_from_slice(&6_u32.to_be_bytes());
        payload.extend_from_slice(b"hello\n");
        let value = OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &payload).unwrap();
        assert_eq!(value.opaque_type(), DOCUMENT_TYPE);
        assert_eq!(value.canonical_payload(), payload);

        let mut empty_body = Vec::from(DOCUMENT_MAGIC.as_bytes());
        empty_body.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &empty_body),
            Err(OpaqueValueError::InvalidDocumentBody {
                opaque_type: DOCUMENT_TYPE,
            })
        );
        let mut missing_final_newline = Vec::from(DOCUMENT_MAGIC.as_bytes());
        missing_final_newline.extend_from_slice(&5_u32.to_be_bytes());
        missing_final_newline.extend_from_slice(b"hello");
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &missing_final_newline),
            Err(OpaqueValueError::InvalidDocumentBody {
                opaque_type: DOCUMENT_TYPE,
            })
        );

        for body in [
            b"\0\n".as_slice(),
            b"\t\n".as_slice(),
            b"\r\n".as_slice(),
            b"\x7f\n".as_slice(),
            "\u{0085}\n".as_bytes(),
        ] {
            let mut control_byte = Vec::from(DOCUMENT_MAGIC.as_bytes());
            control_byte.extend_from_slice(&(body.len() as u32).to_be_bytes());
            control_byte.extend_from_slice(body);
            assert_eq!(
                OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &control_byte),
                Err(OpaqueValueError::InvalidDocumentBody {
                    opaque_type: DOCUMENT_TYPE,
                })
            );
        }

        let mut over_limit = Vec::from(DOCUMENT_MAGIC.as_bytes());
        over_limit.extend_from_slice(
            &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
                .unwrap()
                .to_be_bytes(),
        );
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &over_limit),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: DOCUMENT_TYPE,
            })
        );

        let bad_magic = b"WRONG-DOCUMENT/1 \0\0\0\0".to_vec();
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &bad_magic),
            Err(OpaqueValueError::InvalidMagic {
                opaque_type: DOCUMENT_TYPE,
            })
        );

        let truncated = Vec::from(DOCUMENT_MAGIC.as_bytes());
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &truncated),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: DOCUMENT_TYPE,
            })
        );

        let mut short_body = Vec::from(DOCUMENT_MAGIC.as_bytes());
        short_body.extend_from_slice(&5_u32.to_be_bytes());
        short_body.extend_from_slice(b"hi");
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &short_body),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: DOCUMENT_TYPE,
            })
        );

        let mut invalid_utf8 = Vec::from(DOCUMENT_MAGIC.as_bytes());
        invalid_utf8.extend_from_slice(&2_u32.to_be_bytes());
        invalid_utf8.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &invalid_utf8),
            Err(OpaqueValueError::InvalidUtf8Body {
                opaque_type: DOCUMENT_TYPE,
            })
        );
    }

    #[test]
    fn canonical_json_codec_accepts_canonical_payload() {
        const JSON_TYPE: TypeId = TypeId::from_bytes([0x4f; 16]);
        const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";
        const JSON_NAME: [&str; 3] = ["std", "json", "value"];
        const JSON_CONTRACT: &str = "orna.std.value.json@1";

        let active = active_record_revision_with_standard(
            RECORD_TYPE,
            verified_standard_with_value_types_and_schemas(
                vec![
                    standard_boolean_definition(),
                    opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                    opaque_definition(JSON_TYPE, JSON_NAME, JSON_CONTRACT),
                ],
                vec![SchemaDefinition::new(
                    SchemaId::from_bytes([0x60; 16]),
                    QualifiedSemanticName::new(["std", "json"]).unwrap(),
                )],
            ),
        );
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                OpaqueCodecRegistration::length_prefixed_canonical_json(
                    JSON_TYPE,
                    QualifiedSemanticName::new(JSON_NAME).unwrap(),
                    JSON_CONTRACT,
                    JSON_MAGIC,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let body = br#"{"a":[1,true],"z":"ok"}"#;
        let mut payload = Vec::from(JSON_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(body);
        let value = OpaqueValue::new(&active, &registry, JSON_TYPE, &payload).unwrap();
        assert_eq!(value.opaque_type(), JSON_TYPE);
        assert_eq!(value.canonical_payload(), payload);
    }

    #[test]
    fn canonical_json_codec_rejects_invalid_and_noncanonical_payloads() {
        const JSON_TYPE: TypeId = TypeId::from_bytes([0x4f; 16]);
        const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";
        const JSON_NAME: [&str; 3] = ["std", "json", "value"];
        const JSON_CONTRACT: &str = "orna.std.value.json@1";

        let active = active_record_revision_with_standard(
            RECORD_TYPE,
            verified_standard_with_value_types_and_schemas(
                vec![
                    standard_boolean_definition(),
                    opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                    opaque_definition(JSON_TYPE, JSON_NAME, JSON_CONTRACT),
                ],
                vec![SchemaDefinition::new(
                    SchemaId::from_bytes([0x60; 16]),
                    QualifiedSemanticName::new(["std", "json"]).unwrap(),
                )],
            ),
        );
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                OpaqueCodecRegistration::length_prefixed_canonical_json(
                    JSON_TYPE,
                    QualifiedSemanticName::new(JSON_NAME).unwrap(),
                    JSON_CONTRACT,
                    JSON_MAGIC,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let frame = |body: &[u8]| {
            let mut payload = Vec::from(JSON_MAGIC.as_bytes());
            payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
            payload.extend_from_slice(body);
            payload
        };
        let reject =
            |payload: &[u8]| OpaqueValue::new(&active, &registry, JSON_TYPE, payload).unwrap_err();

        assert_eq!(
            reject(b"WRONG-JSON-VALUE/1 \0\0\0\0null"),
            OpaqueValueError::InvalidMagic {
                opaque_type: JSON_TYPE,
            }
        );
        assert_eq!(
            reject(JSON_MAGIC.as_bytes()),
            OpaqueValueError::InvalidFrameLength {
                opaque_type: JSON_TYPE,
            }
        );

        let mut short_body = frame(br#"null"#);
        short_body.pop();
        assert_eq!(
            reject(&short_body),
            OpaqueValueError::InvalidFrameLength {
                opaque_type: JSON_TYPE,
            }
        );
        let mut trailing = frame(br#"null"#);
        trailing.push(0);
        assert_eq!(
            reject(&trailing),
            OpaqueValueError::InvalidFrameLength {
                opaque_type: JSON_TYPE,
            }
        );
        let invalid_utf8 = frame(&[0xff]);
        assert_eq!(
            reject(&invalid_utf8),
            OpaqueValueError::InvalidUtf8Body {
                opaque_type: JSON_TYPE,
            }
        );

        for body in [
            br#"{"a":}"#.as_slice(),
            br#" null"#.as_slice(),
            br#"{"z":1,"a":2}"#.as_slice(),
            br#"{"a":1,"a":1}"#.as_slice(),
            br#"1e0"#.as_slice(),
        ] {
            assert_eq!(
                reject(&frame(body)),
                OpaqueValueError::InvalidJsonBody {
                    opaque_type: JSON_TYPE,
                }
            );
        }

        let mut oversized = Vec::from(JSON_MAGIC.as_bytes());
        oversized.extend_from_slice(
            &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
                .unwrap()
                .to_be_bytes(),
        );
        assert_eq!(
            reject(&oversized),
            OpaqueValueError::InvalidFrameLength {
                opaque_type: JSON_TYPE,
            }
        );
    }

    #[test]
    fn framed_codecs_validate_media_type_payloads() {
        const BYTE_STREAM_TYPE: TypeId = TypeId::from_bytes([0x4e; 16]);
        const BYTE_STREAM_MAGIC: &str = "ORNA-BYTE-STREAM/1 ";
        const BYTE_STREAM_NAME: [&str; 3] = ["std", "io", "bytestream"];
        const BYTE_STREAM_CONTRACT: &str = "orna.std.value.byte-stream@1";

        let active = active_record_revision_with_standard(
            RECORD_TYPE,
            verified_standard_with_value_types_and_schemas(
                vec![
                    standard_boolean_definition(),
                    opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                    opaque_definition(BYTE_STREAM_TYPE, BYTE_STREAM_NAME, BYTE_STREAM_CONTRACT),
                ],
                vec![SchemaDefinition::new(
                    SchemaId::from_bytes([0x4f; 16]),
                    QualifiedSemanticName::new(["std", "io"]).unwrap(),
                )],
            ),
        );
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registry = OpaqueCodecRegistry::new(
            standard,
            [
                opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                OpaqueCodecRegistration::media_type_framed(
                    BYTE_STREAM_TYPE,
                    QualifiedSemanticName::new(BYTE_STREAM_NAME).unwrap(),
                    BYTE_STREAM_CONTRACT,
                    BYTE_STREAM_MAGIC,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        let mut payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        let media_type = b"application/json";
        payload.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
        payload.extend_from_slice(media_type);
        payload.extend_from_slice(&2_u32.to_be_bytes());
        payload.extend_from_slice(b"{}");
        let value = OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &payload).unwrap();
        assert_eq!(value.opaque_type(), BYTE_STREAM_TYPE);
        assert_eq!(value.canonical_payload(), payload);

        let mut empty_media_type = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        empty_media_type.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &empty_media_type),
            Err(OpaqueValueError::InvalidMediaType {
                opaque_type: BYTE_STREAM_TYPE,
            })
        );

        let mut truncated = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        truncated.extend_from_slice(&3_u32.to_be_bytes());
        truncated.extend_from_slice(b"abc");
        truncated.extend_from_slice(&5_u32.to_be_bytes());
        truncated.extend_from_slice(b"hi");
        assert_eq!(
            OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &truncated),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: BYTE_STREAM_TYPE,
            })
        );

        let bad_magic = b"WRONG-STREAM/1 \0\0\0\0".to_vec();
        assert_eq!(
            OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &bad_magic),
            Err(OpaqueValueError::InvalidMagic {
                opaque_type: BYTE_STREAM_TYPE,
            })
        );
        let mut over_limit = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        over_limit.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
        over_limit.extend_from_slice(media_type);
        over_limit.extend_from_slice(
            &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
                .unwrap()
                .to_be_bytes(),
        );
        over_limit.extend(std::iter::repeat_n(
            0_u8,
            MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1,
        ));
        assert_eq!(
            OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &over_limit),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: BYTE_STREAM_TYPE,
            })
        );
    }

    #[test]
    fn record_values_validate_named_fields_and_store_declaration_order() {
        let active = active_record_revision();
        let stage =
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());

        let record = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("stage"), stage.clone()),
                (String::from("enabled"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();

        assert_eq!(record.record_type(), RECORD_TYPE);
        assert_eq!(
            record.fields(),
            &[RuntimeValue::Boolean(true), stage.clone()]
        );
        assert_eq!(
            RuntimeValue::Record(record).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(RECORD_TYPE))
        );
    }

    #[test]
    fn record_values_require_an_active_nominal_type_and_exact_field_names() {
        let active = active_record_revision();
        let unknown_type = TypeId::from_bytes([0x60; 16]);
        assert_eq!(
            RecordValue::new(&active, unknown_type, Vec::<(String, RuntimeValue)>::new(),),
            Err(RecordValueError::UnknownType {
                record_type: unknown_type,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("Enabled"), RuntimeValue::Boolean(true))],
            ),
            Err(RecordValueError::UnknownField {
                record_type: RECORD_TYPE,
                name: String::from("Enabled"),
            })
        );
    }

    #[test]
    fn record_values_require_every_declared_field_exactly_once() {
        let active = active_record_revision();
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("enabled"), RuntimeValue::Boolean(true))],
            ),
            Err(RecordValueError::MissingField {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (String::from("enabled"), RuntimeValue::Boolean(false)),
                ],
            ),
            Err(RecordValueError::DuplicateField {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
            })
        );
    }

    #[test]
    fn record_values_reject_null_wrong_type_and_stale_enum_fields() {
        let active = active_record_revision();
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(
                    String::from("enabled"),
                    RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
                )],
            ),
            Err(RecordValueError::NullField {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("enabled"), RuntimeValue::Integer(1))],
            ),
            Err(RecordValueError::FieldTypeMismatch {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
                expected: ResolvedType::scalar(StandardScalar::Boolean),
                actual: ResolvedType::scalar(StandardScalar::Integer),
            })
        );

        let stale_catalogue = enum_catalogue(&["retired"]);
        let stale =
            RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (String::from("stage"), stale),
                ],
            ),
            Err(RecordValueError::InactiveEnumLabel {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
                enum_type: ENUM_TYPE,
                label: String::from("retired"),
            })
        );
    }

    #[test]
    fn record_values_enter_server_results_but_not_the_argument_subset() {
        let active = active_record_revision();
        let record = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap();
        let parameter = ParameterId::from_bytes([0x61; 16]);
        assert_eq!(
            FunctionArgument::new(parameter, RuntimeValue::Record(record.clone())),
            Err(FunctionArgumentError::RecordValueNotAccepted {
                parameter,
                record_type: RECORD_TYPE,
            })
        );
        let expected = RuntimeValue::Record(record);
        let rows = ResultRows::new(
            [column("status", ResolvedType::named(RECORD_TYPE), false)],
            [ResultRow::new([expected.clone()])],
        )
        .unwrap();
        assert_eq!(rows.rows()[0].values(), &[expected]);
    }

    #[test]
    fn record_value_equality_is_nominal_across_semantically_identical_revisions() {
        let active = active_record_revision();
        let fields = || {
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ]
        };
        let record = RecordValue::new(&active, RECORD_TYPE, fields()).unwrap();
        assert_eq!(
            RecordValue::new(&active, RECORD_TYPE, fields()).unwrap(),
            record
        );

        let other_type = TypeId::from_bytes([0x62; 16]);
        let other_active = active_record_revision_with_type(other_type);
        let other = RecordValue::new(
            &other_active,
            other_type,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(other_active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap();
        assert_ne!(record, other);

        // Equality must not bind a creation-revision identity: the same
        // nominal type and field sequence compare equal across revisions
        // with different source and catalogue revision IDs.
        let child_type = TypeId::from_bytes([0x31; 16]);
        let single_field = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let old = active_nested_record_revision_with_child_fields(single_field.clone());
        let fresh = active_nested_record_revision_with_seed(single_field, 0x77, 0x78);
        let left = RecordValue::new(
            &old,
            child_type,
            [(String::from("a"), RuntimeValue::Boolean(true))],
        )
        .unwrap();
        let right = RecordValue::new(
            &fresh,
            child_type,
            [(String::from("a"), RuntimeValue::Boolean(true))],
        )
        .unwrap();
        assert_eq!(
            left, right,
            "equality must ignore the creation-revision identity"
        );

        // A reversed field-ID declaration sequence with identical positional
        // Boolean values compares unequal.
        let ab_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let ba_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3a; 16]),
                "a",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let ab = RecordValue::new(
            &active_nested_record_revision_with_child_fields(ab_fields.clone()),
            child_type,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap();
        let ba = RecordValue::new(
            &active_nested_record_revision_with_child_fields(ba_fields),
            child_type,
            [
                (String::from("a"), RuntimeValue::Boolean(false)),
                (String::from("b"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();
        assert_ne!(
            ab, ba,
            "reversed field-ID declaration order must compare unequal"
        );

        // One replaced field ID with the same names, ordinals, and values
        // compares unequal.
        let replaced_fields = vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3d; 16]),
                "a",
                0,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([0x3c; 16]),
                "b",
                1,
                TypeDescriptor::named(STANDARD_BOOLEAN),
            )
            .unwrap(),
        ];
        let original = RecordValue::new(
            &active_nested_record_revision_with_child_fields(ab_fields),
            child_type,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap();
        let replaced = RecordValue::new(
            &active_nested_record_revision_with_child_fields(replaced_fields),
            child_type,
            [
                (String::from("a"), RuntimeValue::Boolean(true)),
                (String::from("b"), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap();
        assert_ne!(
            original, replaced,
            "a replaced field ID must compare unequal"
        );
    }

    #[test]
    fn accepts_every_current_non_null_runtime_value_as_a_function_argument() {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let values = vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(-7),
            RuntimeValue::BigInt(8),
            RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
            RuntimeValue::Text("value".into()),
            RuntimeValue::Bytes(vec![1, 2, 3]),
            RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            },
            RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap()),
        ];

        for (index, value) in values.into_iter().enumerate() {
            let parameter = ParameterId::from_bytes([index as u8; 16]);
            let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
            assert_eq!(argument.parameter(), parameter);
            assert_eq!(argument.value(), &value);
        }
    }

    #[test]
    fn enum_values_require_an_active_type_and_exact_declared_label() {
        let catalogue = enum_catalogue(&["lead", "owner's", "customer"]);
        let value = EnumValue::new(&catalogue, ENUM_TYPE, "owner's").unwrap();

        assert_eq!(value.enum_type(), ENUM_TYPE);
        assert_eq!(value.label(), "owner's");
        assert_eq!(
            RuntimeValue::Enum(value.clone()).runtime_type(),
            RuntimeType::Flat(ResolvedType::named(ENUM_TYPE))
        );
        assert_eq!(value, value.clone());

        let unknown = TypeId::from_bytes([0x46; 16]);
        let error = EnumValue::new(&catalogue, unknown, "lead").unwrap_err();
        assert_eq!(error, EnumValueError::UnknownType { enum_type: unknown });
        assert_eq!(error.to_string(), "enum type is not active");

        let error = EnumValue::new(&catalogue, ENUM_TYPE, "Lead").unwrap_err();
        assert_eq!(
            error,
            EnumValueError::UndeclaredLabel {
                enum_type: ENUM_TYPE,
                label: String::from("Lead"),
            }
        );
        assert_eq!(
            error.to_string(),
            "enum label is not declared by the active type"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn result_rows_accept_exact_enum_values_and_typed_nulls() {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let enum_type = ResolvedType::named(ENUM_TYPE);
        let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
        let rows = ResultRows::new(
            [
                column("stage", enum_type, false),
                column("previous_stage", enum_type, true),
            ],
            [ResultRow::new([
                value.clone(),
                RuntimeValue::null(enum_type).unwrap(),
            ])],
        )
        .unwrap();

        assert_eq!(rows.rows()[0].values()[0], value);
        assert!(rows.rows()[0].values()[1].is_null());
    }

    #[test]
    fn rejects_typed_null_function_arguments_with_parameter_and_type() {
        let parameter = ParameterId::from_bytes([0x43; 16]);
        let resolved_type = ResolvedType::reference(TARGET);
        let value = RuntimeValue::null(resolved_type).unwrap();

        let error = FunctionArgument::new(parameter, value).unwrap_err();
        assert_eq!(
            error,
            FunctionArgumentError::NullValue {
                parameter,
                resolved_type,
            }
        );
        assert_eq!(error.to_string(), "function argument value cannot be NULL");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn function_argument_clone_and_equality_preserve_parameter_and_reference_identity() {
        let parameter = ParameterId::from_bytes([0x44; 16]);
        let value = RuntimeValue::Reference {
            target: TARGET,
            object: OBJECT,
        };
        let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
        let clone = argument.clone();

        assert_eq!(clone, argument);
        assert_eq!(argument.parameter(), parameter);
        assert_eq!(argument.value(), &value);

        let other_parameter = ParameterId::from_bytes([0x45; 16]);
        let other = FunctionArgument::new(other_parameter, value).unwrap();
        assert_ne!(argument, other);
    }

    #[test]
    fn accepts_every_initial_runtime_value_type_and_typed_null() {
        let rows = ResultRows::new(
            [
                column(
                    "boolean",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
                column(
                    "integer",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                ),
                column(
                    "bigint",
                    ResolvedType::scalar(StandardScalar::BigInt),
                    false,
                ),
                column("float", ResolvedType::scalar(StandardScalar::Float), false),
                column(
                    "text",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
                column(
                    "optional_text",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
                column(
                    "bytes",
                    ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                    false,
                ),
                column("reference", ResolvedType::reference(TARGET), false),
            ],
            [ResultRow::new([
                RuntimeValue::Boolean(true),
                RuntimeValue::Integer(7),
                RuntimeValue::BigInt(8),
                RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
                RuntimeValue::Text("value".into()),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
                    .unwrap(),
                RuntimeValue::Bytes(vec![1, 2, 3]),
                RuntimeValue::Reference {
                    target: TARGET,
                    object: OBJECT,
                },
            ])],
        )
        .unwrap();

        assert_eq!(rows.columns().len(), 8);
        assert_eq!(rows.rows()[0].values().len(), 8);
        assert!(rows.rows()[0].values()[5].is_null());
    }

    #[test]
    fn preserves_column_and_row_order() {
        let rows = ResultRows::new(
            [
                column(
                    "second",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                ),
                column(
                    "first",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
            ],
            [
                ResultRow::new([RuntimeValue::Integer(2), RuntimeValue::Boolean(false)]),
                ResultRow::new([RuntimeValue::Integer(1), RuntimeValue::Boolean(true)]),
            ],
        )
        .unwrap();

        assert_eq!(rows.columns()[0].name(), "second");
        assert_eq!(rows.columns()[1].name(), "first");
        assert_eq!(rows.rows()[0].values()[0], RuntimeValue::Integer(2));
        assert_eq!(rows.rows()[1].values()[1], RuntimeValue::Boolean(true));
    }

    #[test]
    fn transfers_rows_and_values_in_order_without_cloning_payloads() {
        let bytes = vec![1_u8, 2, 3];
        let rows = ResultRows::new(
            [column(
                "payload",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                false,
            )],
            [ResultRow::new([RuntimeValue::Bytes(bytes.clone())])],
        )
        .unwrap();

        let rows = rows.into_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows.into_iter().next().unwrap().into_values(),
            [RuntimeValue::Bytes(bytes),]
        );
    }

    #[test]
    fn rejects_empty_duplicate_and_unsupported_columns() {
        assert_eq!(
            ResultColumn::new("", ResolvedType::scalar(StandardScalar::Boolean), false),
            Err(ResultRowsError::EmptyColumnName)
        );
        for resolved_type in [
            ResolvedType::scalar(StandardScalar::Decimal),
            ResolvedType::scalar(StandardScalar::Uuid),
            ResolvedType::scalar(StandardScalar::Date),
            ResolvedType::scalar(StandardScalar::Time),
            ResolvedType::scalar(StandardScalar::Timestamp),
            ResolvedType::scalar(StandardScalar::Duration),
            ResolvedType::scalar(StandardScalar::Void),
        ] {
            assert_eq!(
                ResultColumn::new("unsupported", resolved_type, false),
                Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
            );
            assert_eq!(
                RuntimeValue::null(resolved_type),
                Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
            );
        }
        assert_eq!(
            ResultRows::new(
                [
                    column("same", ResolvedType::scalar(StandardScalar::Boolean), false),
                    column("same", ResolvedType::scalar(StandardScalar::Integer), false),
                ],
                [],
            ),
            Err(ResultRowsError::DuplicateColumnName {
                first: 0,
                duplicate: 1,
                name: "same".into(),
            })
        );
    }

    #[test]
    fn rejects_zero_columns_even_when_rows_have_zero_width() {
        assert_eq!(
            ResultRows::new(Vec::<ResultColumn>::new(), [ResultRow::new([])]),
            Err(ResultRowsError::EmptyColumns)
        );
    }

    #[test]
    fn rejects_non_finite_floats_and_preserves_finite_equality() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                RuntimeFloat::new(value),
                Err(ResultRowsError::NonFiniteFloat)
            );
        }

        let finite = RuntimeFloat::new(2.5).unwrap();
        assert_eq!(finite, finite);
        assert_eq!(finite.value(), 2.5);
        assert_eq!(
            RuntimeFloat::new(0.0).unwrap(),
            RuntimeFloat::new(-0.0).unwrap()
        );
    }

    #[test]
    fn null_values_expose_only_the_checked_type() {
        let value = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap();
        let RuntimeValue::Null(null) = value else {
            panic!("runtime null constructor must create a null value");
        };
        assert_eq!(
            null.resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
    }

    #[test]
    fn rejects_width_nullability_and_type_mismatches() {
        let boolean = column(
            "boolean",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        );
        assert_eq!(
            ResultRows::new([boolean.clone()], [ResultRow::new([])]),
            Err(ResultRowsError::RowWidthMismatch {
                row: 0,
                expected: 1,
                actual: 0,
            })
        );
        assert_eq!(
            ResultRows::new(
                [boolean.clone()],
                [ResultRow::new([RuntimeValue::null(ResolvedType::scalar(
                    StandardScalar::Boolean
                ))
                .unwrap(),])],
            ),
            Err(ResultRowsError::NullInNonNullableColumn { row: 0, column: 0 })
        );
        assert_eq!(
            ResultRows::new([boolean], [ResultRow::new([RuntimeValue::Integer(1)])]),
            Err(ResultRowsError::ValueTypeMismatch {
                row: 0,
                column: 0,
                expected: ResolvedType::scalar(StandardScalar::Boolean),
                actual: ResolvedType::scalar(StandardScalar::Integer),
            })
        );
    }

    #[test]
    fn rejects_references_with_the_wrong_target_type() {
        let expected = TypeId::from_bytes([0x51; 16]);
        let actual = TypeId::from_bytes([0x52; 16]);
        assert_eq!(
            ResultRows::new(
                [column(
                    "reference",
                    ResolvedType::reference(expected),
                    false
                )],
                [ResultRow::new([RuntimeValue::Reference {
                    target: actual,
                    object: OBJECT,
                }])],
            ),
            Err(ResultRowsError::ValueTypeMismatch {
                row: 0,
                column: 0,
                expected: ResolvedType::reference(expected),
                actual: ResolvedType::reference(actual),
            })
        );
    }

    fn inspect_carrier_payload(
        active: &ActiveDatabaseRevision,
        tag: u8,
        rows: &[&[u8]],
    ) -> Vec<u8> {
        let mut payload = b"ORNA-INSPECT/1 ".to_vec();
        payload.extend_from_slice(&1_u16.to_be_bytes());
        payload.push(tag);
        payload.extend_from_slice(&7_u64.to_be_bytes());
        payload.extend_from_slice(&active.pair().source().to_bytes());
        payload.extend_from_slice(&active.pair().catalogue().to_bytes());
        payload.extend_from_slice(&u32::try_from(rows.len()).unwrap().to_be_bytes());
        for row in rows {
            payload.extend_from_slice(&u32::try_from(row.len()).unwrap().to_be_bytes());
            payload.extend_from_slice(row);
        }
        payload
    }

    fn inspect_orv5_integer_row(value: i32) -> Vec<u8> {
        let mut row = b"ORV5".to_vec();
        row.push(0x03);
        row.extend_from_slice(&[0; 16]);
        row.extend_from_slice(&4_u32.to_be_bytes());
        row.extend_from_slice(&value.to_be_bytes());
        row
    }

    #[test]
    fn inspect_opaque_constructor_accepts_all_nine_registered_carriers() {
        let active = active_record_revision();
        let carriers = [
            (1_u8, SYS_INSPECT_SNAPSHOT_TYPE_ID),
            (2_u8, SYS_INSPECT_INVOCATION_NODES_TYPE_ID),
            (3, SYS_INSPECT_CALLS_TYPE_ID),
            (4, SYS_INSPECT_RESOURCES_TYPE_ID),
            (5, SYS_INSPECT_STATE_CELLS_TYPE_ID),
            (6, SYS_INSPECT_UI_NODES_TYPE_ID),
            (7, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID),
            (8, SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID),
            (9, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID),
        ];

        for (tag, opaque_type) in carriers {
            let row = inspect_orv5_integer_row(1);
            let payload = inspect_carrier_payload(&active, tag, &[row.as_slice()]);
            let value = OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload)
                .expect("the fixed Inspector carrier must construct");
            assert_eq!(value.opaque_type(), opaque_type);
            assert_eq!(value.canonical_payload(), payload);
        }
    }

    #[test]
    fn inspect_opaque_constructor_accepts_snapshot_and_rejects_other_reserved_types() {
        let active = active_record_revision();
        let row = inspect_orv5_integer_row(1);
        let payload = inspect_carrier_payload(&active, 1, &[row.as_slice()]);
        let snapshot =
            OpaqueValue::new_inspect_carrier(&active, SYS_INSPECT_SNAPSHOT_TYPE_ID, &payload)
                .expect("the fixed snapshot carrier must construct");
        assert_eq!(snapshot.opaque_type(), SYS_INSPECT_SNAPSHOT_TYPE_ID);

        for opaque_type in [
            crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
            crate::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
            crate::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
        ] {
            assert_eq!(
                OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload),
                Err(OpaqueValueError::UnregisteredType { opaque_type })
            );
        }
    }

    #[test]
    fn inspect_opaque_constructor_rejects_malformed_trailing_and_mismatched_payloads() {
        let active = active_record_revision();
        let opaque_type = SYS_INSPECT_INVOCATION_NODES_TYPE_ID;
        let row = inspect_orv5_integer_row(1);
        let payload = inspect_carrier_payload(&active, 2, &[row.as_slice()]);

        let mut trailing = payload.clone();
        trailing.push(0);
        assert_eq!(
            OpaqueValue::new_inspect_carrier(&active, opaque_type, trailing),
            Err(OpaqueValueError::InvalidInspectCarrierEnvelope { opaque_type })
        );
        assert_eq!(
            OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload[..payload.len() - 1]),
            Err(OpaqueValueError::InvalidInspectCarrierEnvelope { opaque_type })
        );

        let mut wrong_revision = payload.clone();
        let source_offset = b"ORNA-INSPECT/1 ".len() + 2 + 1 + 8;
        wrong_revision[source_offset] ^= 1;
        assert_eq!(
            OpaqueValue::new_inspect_carrier(&active, opaque_type, wrong_revision),
            Err(OpaqueValueError::InspectCarrierRevisionMismatch { opaque_type })
        );

        let unknown_type = TypeId::from_bytes([0xaa; 16]);
        assert_eq!(
            OpaqueValue::new_inspect_carrier(&active, unknown_type, payload),
            Err(OpaqueValueError::UnregisteredType {
                opaque_type: unknown_type,
            })
        );
    }
    #[test]
    fn rows_opaque_registration_accepts_bounded_canonical_zero_row_frame() {
        const ROWS_TYPE: TypeId = TypeId::from_bytes([0x8a; 16]);
        let standard = verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(ROWS_TYPE, ["std", "data", "rows"], "orna.std.value.rows@1"),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x8b; 16]),
                QualifiedSemanticName::new(["std", "data"]).unwrap(),
            )],
        );
        let active = active_record_revision_with_standard(RECORD_TYPE, standard);
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registration = OpaqueCodecRegistration::rows(
            ROWS_TYPE,
            QualifiedSemanticName::new(["std", "data", "rows"]).unwrap(),
            "orna.std.value.rows@1",
            "ORNA-ROWS/1 ",
        )
        .unwrap();
        let registry = OpaqueCodecRegistry::new(standard, [registration]).unwrap();
        let mut payload = b"ORNA-ROWS/1 ".to_vec();
        payload.extend_from_slice(&1_u16.to_be_bytes());
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.push(b'x');
        payload.push(0x01);
        payload.extend_from_slice(&[0; 15]);
        payload.push(0x01);
        payload.push(0);
        payload.extend_from_slice(&0_u32.to_be_bytes());

        let value = OpaqueValue::new(&active, &registry, ROWS_TYPE, payload.clone())
            .expect("the bounded zero-row Rows frame must be structurally valid");
        assert_eq!(value.opaque_type(), ROWS_TYPE);
        assert_eq!(value.canonical_payload(), payload);

        let mut trailing = payload;
        trailing.push(0);
        assert_eq!(
            OpaqueValue::new(&active, &registry, ROWS_TYPE, trailing),
            Err(OpaqueValueError::InvalidRowsFrame {
                opaque_type: ROWS_TYPE,
            })
        );
    }

    #[test]
    fn rows_opaque_registration_rejects_malformed_variable_orv5_cells() {
        const ROWS_TYPE: TypeId = TypeId::from_bytes([0x8a; 16]);
        let standard = verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(ROWS_TYPE, ["std", "data", "rows"], "orna.std.value.rows@1"),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x8b; 16]),
                QualifiedSemanticName::new(["std", "data"]).unwrap(),
            )],
        );
        let active = active_record_revision_with_standard(RECORD_TYPE, standard);
        let standard = active.catalogue_hash_context().standard().unwrap();
        let registration = OpaqueCodecRegistration::rows(
            ROWS_TYPE,
            QualifiedSemanticName::new(["std", "data", "rows"]).unwrap(),
            "orna.std.value.rows@1",
            "ORNA-ROWS/1 ",
        )
        .unwrap();
        let registry = OpaqueCodecRegistry::new(standard, [registration]).unwrap();

        let rows_frame = |cell: &[u8]| {
            let mut payload = b"ORNA-ROWS/1 ".to_vec();
            payload.extend_from_slice(&1_u16.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(b"x");
            payload.push(0x01);
            payload.extend_from_slice(&[0; 15]);
            payload.push(0x01);
            payload.push(0);
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&u32::try_from(cell.len()).unwrap().to_be_bytes());
            payload.extend_from_slice(cell);
            payload
        };
        let orv5 = |tag: u8, type_id: [u8; 16], payload: &[u8]| {
            let mut cell = b"ORV5".to_vec();
            cell.push(tag);
            cell.extend_from_slice(&type_id);
            cell.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
            cell.extend_from_slice(payload);
            cell
        };

        for cell in [
            orv5(0x06, [0x01; 16], &[0xff]),
            orv5(0x0a, [0x02; 16], &[0xff]),
            orv5(0x06, [0; 16], b"text"),
            orv5(0x07, [0; 16], &[0xde, 0xad]),
            orv5(0x0c, [0x03; 16], &[0xde, 0xad]),
        ] {
            assert_eq!(
                OpaqueValue::new(&active, &registry, ROWS_TYPE, rows_frame(&cell)),
                Err(OpaqueValueError::InvalidRowsFrame {
                    opaque_type: ROWS_TYPE,
                })
            );
        }

        let bytes = orv5(0x07, [0x04; 16], &[0xde, 0xad, 0xbe, 0xef]);
        let payload = rows_frame(&bytes);
        let value = OpaqueValue::new(&active, &registry, ROWS_TYPE, payload.clone())
            .expect("bounded arbitrary BYTES cells are structurally valid");
        assert_eq!(value.canonical_payload(), payload);
    }
}
