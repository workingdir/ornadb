//! Resolved Orna type descriptors.

use std::{error::Error, fmt};

use crate::TypeId;

/// The maximum accepted number of nested constructed-type levels.
pub const MAX_TYPE_DESCRIPTOR_DEPTH: usize = 32;

/// The maximum accepted number of nodes in one constructed type descriptor.
pub const MAX_TYPE_DESCRIPTOR_NODES: usize = 256;

/// One bounded canonical type descriptor.
///
/// Checked constructors own all recursive limit accounting. A descriptor does
/// not by itself admit the type in a catalogue or execution position.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TypeDescriptor {
    node: TypeDescriptorNode,
    depth: usize,
    node_count: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum TypeDescriptorNode {
    Named(TypeId),
    Reference(TypeId),
    List(Box<TypeDescriptor>),
    Set(Box<TypeDescriptor>),
    Map {
        key: Box<TypeDescriptor>,
        value: Box<TypeDescriptor>,
    },
    Option(Box<TypeDescriptor>),
    Stream(Box<TypeDescriptor>),
}

/// A borrowed view of one descriptor node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeDescriptorKind<'descriptor> {
    /// One resolved by-value catalogue type identity.
    Named(TypeId),
    /// One resolved durable object-reference target identity.
    Reference(TypeId),
    /// An ordered collection descriptor.
    List(&'descriptor TypeDescriptor),
    /// A logically unique collection descriptor.
    Set(&'descriptor TypeDescriptor),
    /// A key/value collection descriptor.
    Map {
        /// The key descriptor.
        key: &'descriptor TypeDescriptor,
        /// The value descriptor.
        value: &'descriptor TypeDescriptor,
    },
    /// An optional value descriptor.
    Option(&'descriptor TypeDescriptor),
    /// An execution-time stream descriptor.
    Stream(&'descriptor TypeDescriptor),
}

impl TypeDescriptor {
    /// Creates one resolved by-value catalogue leaf.
    pub const fn named(type_id: TypeId) -> Self {
        Self {
            node: TypeDescriptorNode::Named(type_id),
            depth: 0,
            node_count: 1,
        }
    }

    /// Creates one resolved durable object-reference leaf.
    pub const fn reference(target: TypeId) -> Self {
        Self {
            node: TypeDescriptorNode::Reference(target),
            depth: 0,
            node_count: 1,
        }
    }

    /// Creates one bounded `LIST` descriptor.
    pub fn list(element: Self) -> Result<Self, TypeDescriptorError> {
        Self::unary(element, TypeDescriptorNode::List)
    }

    /// Creates one bounded `SET` descriptor.
    pub fn set(element: Self) -> Result<Self, TypeDescriptorError> {
        Self::unary(element, TypeDescriptorNode::Set)
    }

    /// Creates one bounded `MAP` descriptor.
    pub fn map(key: Self, value: Self) -> Result<Self, TypeDescriptorError> {
        let depth = key.depth.max(value.depth) + 1;
        let node_count = key
            .node_count
            .checked_add(value.node_count)
            .and_then(|count| count.checked_add(1))
            .ok_or(TypeDescriptorError::TooLarge {
                maximum: MAX_TYPE_DESCRIPTOR_NODES,
                actual: usize::MAX,
            })?;
        Self::checked(
            TypeDescriptorNode::Map {
                key: Box::new(key),
                value: Box::new(value),
            },
            depth,
            node_count,
        )
    }

    fn checked(
        node: TypeDescriptorNode,
        depth: usize,
        node_count: usize,
    ) -> Result<Self, TypeDescriptorError> {
        if depth > MAX_TYPE_DESCRIPTOR_DEPTH {
            return Err(TypeDescriptorError::TooDeep {
                maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                actual: depth,
            });
        }
        if node_count > MAX_TYPE_DESCRIPTOR_NODES {
            return Err(TypeDescriptorError::TooLarge {
                maximum: MAX_TYPE_DESCRIPTOR_NODES,
                actual: node_count,
            });
        }
        Ok(Self {
            node,
            depth,
            node_count,
        })
    }

    /// Creates one bounded `OPTION` descriptor.
    pub fn option(value: Self) -> Result<Self, TypeDescriptorError> {
        Self::unary(value, TypeDescriptorNode::Option)
    }

    /// Creates one bounded `STREAM` descriptor.
    pub fn stream(value: Self) -> Result<Self, TypeDescriptorError> {
        Self::unary(value, TypeDescriptorNode::Stream)
    }

    /// Returns a borrowed view of this descriptor's outer node.
    pub const fn kind(&self) -> TypeDescriptorKind<'_> {
        match &self.node {
            TypeDescriptorNode::Named(type_id) => TypeDescriptorKind::Named(*type_id),
            TypeDescriptorNode::Reference(target) => TypeDescriptorKind::Reference(*target),
            TypeDescriptorNode::List(element) => TypeDescriptorKind::List(element),
            TypeDescriptorNode::Set(element) => TypeDescriptorKind::Set(element),
            TypeDescriptorNode::Map { key, value } => TypeDescriptorKind::Map { key, value },
            TypeDescriptorNode::Option(value) => TypeDescriptorKind::Option(value),
            TypeDescriptorNode::Stream(value) => TypeDescriptorKind::Stream(value),
        }
    }

    /// Returns the number of nested constructed-type levels.
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the complete descriptor node count.
    pub const fn node_count(&self) -> usize {
        self.node_count
    }

    fn unary(
        child: Self,
        node: impl FnOnce(Box<TypeDescriptor>) -> TypeDescriptorNode,
    ) -> Result<Self, TypeDescriptorError> {
        let depth = child.depth + 1;
        let node_count = child
            .node_count
            .checked_add(1)
            .ok_or(TypeDescriptorError::TooLarge {
                maximum: MAX_TYPE_DESCRIPTOR_NODES,
                actual: usize::MAX,
            })?;
        Self::checked(node(Box::new(child)), depth, node_count)
    }
}

/// A structural constructed-type descriptor failure.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeDescriptorError {
    /// The descriptor exceeds the accepted recursive depth.
    TooDeep {
        /// The accepted maximum depth.
        maximum: usize,
        /// The rejected actual depth.
        actual: usize,
    },
    /// The descriptor exceeds the accepted node count.
    TooLarge {
        /// The accepted maximum node count.
        maximum: usize,
        /// The rejected actual node count.
        actual: usize,
    },
}

impl fmt::Display for TypeDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooDeep { .. } => formatter.write_str("type descriptor is too deep"),
            Self::TooLarge { .. } => formatter.write_str("type descriptor has too many nodes"),
        }
    }
}

impl Error for TypeDescriptorError {}

/// A standard scalar representation.
///
/// This enum models compatibility representations only. It does not resolve
/// source spellings or identify catalogue types.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StandardScalar {
    Boolean,
    Integer,
    BigInt,
    Float,
    Decimal,
    CharacterLargeObject,
    BinaryLargeObject,
    Uuid,
    Date,
    Time,
    Timestamp,
    Duration,
    Void,
}

impl StandardScalar {
    /// The initial standard scalar set.
    pub const ALL: [Self; 13] = [
        Self::Boolean,
        Self::Integer,
        Self::BigInt,
        Self::Float,
        Self::Decimal,
        Self::CharacterLargeObject,
        Self::BinaryLargeObject,
        Self::Uuid,
        Self::Date,
        Self::Time,
        Self::Timestamp,
        Self::Duration,
        Self::Void,
    ];
}

/// A type descriptor after name resolution.
///
/// This initial form deliberately excludes source syntax and deferred type
/// constructors. Named types, references, and value types carry resolved
/// `TypeId` values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedType {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference {
        target: TypeId,
    },
    /// A durable resolved standard value-type identity.
    Value(TypeId),
}

impl ResolvedType {
    /// Creates a descriptor for a standard scalar type.
    pub const fn scalar(scalar: StandardScalar) -> Self {
        Self::Scalar(scalar)
    }

    /// Creates a descriptor for a resolved non-scalar named type.
    pub const fn named(type_id: TypeId) -> Self {
        Self::Named(type_id)
    }

    /// Creates a descriptor for a durable resolved standard value-type identity.
    pub const fn value(type_id: TypeId) -> Self {
        Self::Value(type_id)
    }

    /// Creates a descriptor for a typed object reference.
    pub const fn reference(target: TypeId) -> Self {
        Self::Reference { target }
    }

    /// Returns the legacy standard scalar representation, when present.
    ///
    /// This accessor does not provide scalar naming or identity authority.
    pub const fn legacy_scalar(self) -> Option<StandardScalar> {
        match self {
            Self::Scalar(scalar) => Some(scalar),
            Self::Named(_) | Self::Reference { .. } | Self::Value(_) => None,
        }
    }

    /// Returns the resolved named type identity, when present.
    pub const fn named_type(self) -> Option<TypeId> {
        match self {
            Self::Scalar(_) | Self::Reference { .. } | Self::Value(_) => None,
            Self::Named(type_id) => Some(type_id),
        }
    }

    /// Returns the resolved value type identity, when present.
    ///
    /// This is a durable identity. This and the other inspection accessors do
    /// not validate catalogue membership or a representation contract.
    pub const fn value_type(self) -> Option<TypeId> {
        match self {
            Self::Scalar(_) | Self::Named(_) | Self::Reference { .. } => None,
            Self::Value(type_id) => Some(type_id),
        }
    }

    /// Returns the target type identity for a typed reference, when present.
    pub const fn reference_target(self) -> Option<TypeId> {
        match self {
            Self::Scalar(_) | Self::Named(_) | Self::Value(_) => None,
            Self::Reference { target } => Some(target),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::{
        MAX_TYPE_DESCRIPTOR_DEPTH, MAX_TYPE_DESCRIPTOR_NODES, ResolvedType, StandardScalar,
        TypeDescriptor, TypeDescriptorError, TypeDescriptorKind,
    };
    use crate::TypeId;

    #[test]
    fn standard_scalar_all_is_the_unique_initial_representation_set() {
        let scalars = StandardScalar::ALL;

        assert_eq!(
            scalars,
            [
                StandardScalar::Boolean,
                StandardScalar::Integer,
                StandardScalar::BigInt,
                StandardScalar::Float,
                StandardScalar::Decimal,
                StandardScalar::CharacterLargeObject,
                StandardScalar::BinaryLargeObject,
                StandardScalar::Uuid,
                StandardScalar::Date,
                StandardScalar::Time,
                StandardScalar::Timestamp,
                StandardScalar::Duration,
                StandardScalar::Void,
            ]
        );
        assert_eq!(
            scalars.iter().copied().collect::<HashSet<_>>().len(),
            scalars.len()
        );
    }

    #[test]
    fn resolved_types_model_scalar_named_value_and_typed_reference_identities() {
        let named = TypeId::from_bytes([42; 16]);

        assert_eq!(
            ResolvedType::scalar(StandardScalar::Date),
            ResolvedType::Scalar(StandardScalar::Date)
        );
        assert_eq!(ResolvedType::named(named), ResolvedType::Named(named));
        assert_eq!(ResolvedType::value(named), ResolvedType::Value(named));
        assert_eq!(
            ResolvedType::reference(named),
            ResolvedType::Reference { target: named }
        );
    }

    #[test]
    fn resolved_type_inspection_accessors_report_the_current_variant_matrix() {
        const TYPE_ID: TypeId = TypeId::from_bytes([42; 16]);
        const SCALAR: ResolvedType = ResolvedType::scalar(StandardScalar::Date);
        const NAMED: ResolvedType = ResolvedType::named(TYPE_ID);
        const VALUE: ResolvedType = ResolvedType::value(TYPE_ID);
        const REFERENCE: ResolvedType = ResolvedType::reference(TYPE_ID);

        const SCALAR_LEGACY_SCALAR: Option<StandardScalar> = SCALAR.legacy_scalar();
        const NAMED_NAMED_TYPE: Option<TypeId> = NAMED.named_type();
        const VALUE_VALUE_TYPE: Option<TypeId> = VALUE.value_type();
        const REFERENCE_TARGET: Option<TypeId> = REFERENCE.reference_target();
        const SCALAR_VALUE_TYPE: Option<TypeId> = SCALAR.value_type();
        const NAMED_VALUE_TYPE: Option<TypeId> = NAMED.value_type();
        const REFERENCE_VALUE_TYPE: Option<TypeId> = REFERENCE.value_type();

        assert_eq!(SCALAR_LEGACY_SCALAR, Some(StandardScalar::Date));
        assert_eq!(SCALAR.named_type(), None);
        assert_eq!(SCALAR_VALUE_TYPE, None);
        assert_eq!(SCALAR.reference_target(), None);

        assert_eq!(NAMED.legacy_scalar(), None);
        assert_eq!(NAMED_NAMED_TYPE, Some(TYPE_ID));
        assert_eq!(NAMED_VALUE_TYPE, None);
        assert_eq!(NAMED.reference_target(), None);

        assert_eq!(VALUE.legacy_scalar(), None);
        assert_eq!(VALUE.named_type(), None);
        assert_eq!(VALUE_VALUE_TYPE, Some(TYPE_ID));
        assert_eq!(VALUE.reference_target(), None);

        assert_eq!(REFERENCE.legacy_scalar(), None);
        assert_eq!(REFERENCE.named_type(), None);
        assert_eq!(REFERENCE_VALUE_TYPE, None);
        assert_eq!(REFERENCE_TARGET, Some(TYPE_ID));

        for resolved in [SCALAR, NAMED, VALUE, REFERENCE] {
            assert_eq!(
                [
                    resolved.legacy_scalar().is_some(),
                    resolved.named_type().is_some(),
                    resolved.value_type().is_some(),
                    resolved.reference_target().is_some(),
                ]
                .into_iter()
                .filter(|present| *present)
                .count(),
                1
            );
        }
    }

    #[test]
    fn constructed_type_descriptors_retain_exact_recursive_structure() {
        let key_type = TypeId::from_bytes([41; 16]);
        let value_type = TypeId::from_bytes([42; 16]);
        let descriptor = TypeDescriptor::map(
            TypeDescriptor::reference(key_type),
            TypeDescriptor::list(TypeDescriptor::named(value_type)).unwrap(),
        )
        .unwrap();

        let TypeDescriptorKind::Map { key, value } = descriptor.kind() else {
            panic!("expected a map descriptor");
        };
        assert_eq!(key.kind(), TypeDescriptorKind::Reference(key_type));
        let TypeDescriptorKind::List(element) = value.kind() else {
            panic!("expected a list value descriptor");
        };
        assert_eq!(element.kind(), TypeDescriptorKind::Named(value_type));
        assert_eq!(descriptor.depth(), 2);
        assert_eq!(descriptor.node_count(), 4);

        let set = TypeDescriptor::set(TypeDescriptor::named(value_type)).unwrap();
        assert_eq!(
            set.kind(),
            TypeDescriptorKind::Set(&TypeDescriptor::named(value_type))
        );
        let option = TypeDescriptor::option(TypeDescriptor::named(value_type)).unwrap();
        assert_eq!(
            option.kind(),
            TypeDescriptorKind::Option(&TypeDescriptor::named(value_type))
        );
        let stream = TypeDescriptor::stream(TypeDescriptor::named(value_type)).unwrap();
        assert_eq!(
            stream.kind(),
            TypeDescriptorKind::Stream(&TypeDescriptor::named(value_type))
        );
    }

    #[test]
    fn constructed_type_descriptor_depth_is_exact_and_fail_closed() {
        let mut descriptor = TypeDescriptor::named(TypeId::from_bytes([43; 16]));
        for _ in 0..MAX_TYPE_DESCRIPTOR_DEPTH {
            descriptor = TypeDescriptor::option(descriptor).unwrap();
        }
        assert_eq!(descriptor.depth(), 32);
        assert_eq!(descriptor.node_count(), 33);

        assert_eq!(
            TypeDescriptor::option(descriptor).unwrap_err(),
            TypeDescriptorError::TooDeep {
                maximum: 32,
                actual: 33,
            }
        );
    }

    #[test]
    fn constructed_type_descriptor_size_is_exact_and_fail_closed() {
        let mut descriptor = TypeDescriptor::named(TypeId::from_bytes([44; 16]));
        for _ in 0..7 {
            descriptor = TypeDescriptor::map(descriptor.clone(), descriptor).unwrap();
        }
        assert_eq!(descriptor.node_count(), 255);
        let descriptor = TypeDescriptor::list(descriptor).unwrap();
        assert_eq!(descriptor.node_count(), MAX_TYPE_DESCRIPTOR_NODES);

        assert_eq!(
            TypeDescriptor::set(descriptor).unwrap_err(),
            TypeDescriptorError::TooLarge {
                maximum: 256,
                actual: 257,
            }
        );

        let mut left = TypeDescriptor::named(TypeId::from_bytes([47; 16]));
        for _ in 0..6 {
            left = TypeDescriptor::map(left.clone(), left).unwrap();
        }
        assert_eq!(left.node_count(), 127);
        let right = TypeDescriptor::list(left.clone()).unwrap();
        assert_eq!(right.node_count(), 128);
        let exact = TypeDescriptor::map(left.clone(), right.clone()).unwrap();
        assert_eq!(exact.node_count(), 256);
        let oversized_right = TypeDescriptor::option(right).unwrap();
        assert_eq!(
            TypeDescriptor::map(left, oversized_right).unwrap_err(),
            TypeDescriptorError::TooLarge {
                maximum: 256,
                actual: 257,
            }
        );
    }

    #[test]
    fn constructed_type_descriptor_equality_is_structural() {
        let first = TypeDescriptor::named(TypeId::from_bytes([45; 16]));
        let second = TypeDescriptor::named(TypeId::from_bytes([46; 16]));
        let descriptors = [
            TypeDescriptor::list(first.clone()).unwrap(),
            TypeDescriptor::set(first.clone()).unwrap(),
            TypeDescriptor::map(first.clone(), second.clone()).unwrap(),
            TypeDescriptor::map(second.clone(), first.clone()).unwrap(),
            TypeDescriptor::option(first.clone()).unwrap(),
            TypeDescriptor::stream(first.clone()).unwrap(),
            first.clone(),
            second,
            TypeDescriptor::reference(TypeId::from_bytes([45; 16])),
        ];

        assert_eq!(descriptors.iter().collect::<HashSet<_>>().len(), 9);
        assert_eq!(
            TypeDescriptor::list(first.clone()).unwrap(),
            TypeDescriptor::list(first).unwrap()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_constructed_type_operations_never_panic(
            operations in prop::collection::vec((0_u8..7, any::<[u8; 16]>()), 0..1024),
        ) {
            let mut descriptors = Vec::new();
            for (operation, bytes) in operations {
                let leaf = TypeDescriptor::named(TypeId::from_bytes(bytes));
                let result = match operation {
                    0 => Ok(leaf),
                    1 => Ok(TypeDescriptor::reference(TypeId::from_bytes(bytes))),
                    2 => TypeDescriptor::list(descriptors.pop().unwrap_or(leaf)),
                    3 => TypeDescriptor::set(descriptors.pop().unwrap_or(leaf)),
                    4 => TypeDescriptor::option(descriptors.pop().unwrap_or(leaf)),
                    5 => TypeDescriptor::stream(descriptors.pop().unwrap_or(leaf)),
                    _ => {
                        let value = descriptors.pop().unwrap_or_else(|| leaf.clone());
                        let key = descriptors.pop().unwrap_or(leaf);
                        TypeDescriptor::map(key, value)
                    }
                };
                match result {
                    Ok(descriptor) => {
                        prop_assert!(descriptor.depth() <= MAX_TYPE_DESCRIPTOR_DEPTH);
                        prop_assert!(descriptor.node_count() <= MAX_TYPE_DESCRIPTOR_NODES);
                        descriptors.push(descriptor);
                    }
                    Err(TypeDescriptorError::TooDeep { .. } | TypeDescriptorError::TooLarge { .. }) => {
                        descriptors.clear();
                    }
                }
            }
        }
    }
}
