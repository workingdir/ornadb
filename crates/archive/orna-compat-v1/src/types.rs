//! Resolved Orna type descriptors.

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, DeserializeSeed, EnumAccess, VariantAccess},
};
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
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct TypeDescriptor {
    node: TypeDescriptorNode,
    depth: usize,
    node_count: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
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

    fn from_node(node: TypeDescriptorNode) -> Result<Self, TypeDescriptorError> {
        match node {
            TypeDescriptorNode::Named(type_id) => Ok(Self::named(type_id)),
            TypeDescriptorNode::Reference(target) => Ok(Self::reference(target)),
            TypeDescriptorNode::List(element) => Self::list(*element),
            TypeDescriptorNode::Set(element) => Self::set(*element),
            TypeDescriptorNode::Map { key, value } => Self::map(*key, *value),
            TypeDescriptorNode::Option(value) => Self::option(*value),
            TypeDescriptorNode::Stream(value) => Self::stream(*value),
        }
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

impl<'de> Deserialize<'de> for TypeDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut budget = TypeDescriptorBudget::default();
        TypeDescriptorSeed {
            constructed_depth: 0,
            budget: &mut budget,
        }
        .deserialize(deserializer)
    }
}

#[derive(Default)]
struct TypeDescriptorBudget {
    node_count: usize,
}

impl TypeDescriptorBudget {
    fn claim_node<E>(&mut self) -> Result<(), E>
    where
        E: de::Error,
    {
        let actual = self.node_count.saturating_add(1);
        if actual > MAX_TYPE_DESCRIPTOR_NODES {
            return Err(E::custom(TypeDescriptorError::TooLarge {
                maximum: MAX_TYPE_DESCRIPTOR_NODES,
                actual,
            }));
        }
        self.node_count = actual;
        Ok(())
    }
}

struct TypeDescriptorSeed<'budget> {
    constructed_depth: usize,
    budget: &'budget mut TypeDescriptorBudget,
}

impl<'de, 'budget> de::DeserializeSeed<'de> for TypeDescriptorSeed<'budget> {
    type Value = TypeDescriptor;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.budget.claim_node()?;
        deserializer.deserialize_struct(
            "TypeDescriptor",
            &["node", "depth", "node_count"],
            TypeDescriptorVisitor {
                constructed_depth: self.constructed_depth,
                budget: self.budget,
            },
        )
    }
}

struct TypeDescriptorVisitor<'budget> {
    constructed_depth: usize,
    budget: &'budget mut TypeDescriptorBudget,
}

impl<'de, 'budget> de::Visitor<'de> for TypeDescriptorVisitor<'budget> {
    type Value = TypeDescriptor;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded type descriptor")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: de::MapAccess<'de>,
    {
        let mut node = None;
        let mut depth: Option<usize> = None;
        let mut node_count: Option<usize> = None;

        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "node" => {
                    if node.is_some() {
                        return Err(de::Error::duplicate_field("node"));
                    }
                    node = Some(map.next_value_seed(TypeDescriptorNodeSeed {
                        constructed_depth: self.constructed_depth,
                        budget: self.budget,
                    })?);
                }
                "depth" => {
                    if depth.is_some() {
                        return Err(de::Error::duplicate_field("depth"));
                    }
                    depth = Some(map.next_value()?);
                }
                "node_count" => {
                    if node_count.is_some() {
                        return Err(de::Error::duplicate_field("node_count"));
                    }
                    node_count = Some(map.next_value()?);
                }
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        self.finish(
            node.ok_or_else(|| de::Error::missing_field("node"))?,
            depth.ok_or_else(|| de::Error::missing_field("depth"))?,
            node_count.ok_or_else(|| de::Error::missing_field("node_count"))?,
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let node = sequence
            .next_element_seed(TypeDescriptorNodeSeed {
                constructed_depth: self.constructed_depth,
                budget: self.budget,
            })?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let depth = sequence
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        let node_count = sequence
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(2, &self))?;
        self.finish(node, depth, node_count)
    }
}

impl<'budget> TypeDescriptorVisitor<'budget> {
    fn finish<E>(
        self,
        node: TypeDescriptorNode,
        supplied_depth: usize,
        supplied_node_count: usize,
    ) -> Result<TypeDescriptor, E>
    where
        E: de::Error,
    {
        let descriptor = TypeDescriptor::from_node(node).map_err(E::custom)?;

        if descriptor.depth != supplied_depth {
            return Err(E::custom(format_args!(
                "type descriptor depth metadata does not match its structure: expected {}, got {}",
                descriptor.depth, supplied_depth
            )));
        }
        if descriptor.node_count != supplied_node_count {
            return Err(E::custom(format_args!(
                "type descriptor node count metadata does not match its structure: expected {}, got {}",
                descriptor.node_count, supplied_node_count
            )));
        }
        Ok(descriptor)
    }
}

struct TypeDescriptorNodeSeed<'budget> {
    constructed_depth: usize,
    budget: &'budget mut TypeDescriptorBudget,
}

impl<'de, 'budget> de::DeserializeSeed<'de> for TypeDescriptorNodeSeed<'budget> {
    type Value = TypeDescriptorNode;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_enum(
            "TypeDescriptorNode",
            &[
                "Named",
                "Reference",
                "List",
                "Set",
                "Map",
                "Option",
                "Stream",
            ],
            TypeDescriptorNodeVisitor {
                constructed_depth: self.constructed_depth,
                budget: self.budget,
            },
        )
    }
}

#[derive(Deserialize)]
enum TypeDescriptorNodeVariant {
    Named,
    Reference,
    List,
    Set,
    Map,
    Option,
    Stream,
}

struct TypeDescriptorNodeVisitor<'budget> {
    constructed_depth: usize,
    budget: &'budget mut TypeDescriptorBudget,
}

impl<'de, 'budget> de::Visitor<'de> for TypeDescriptorNodeVisitor<'budget> {
    type Value = TypeDescriptorNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an externally tagged type descriptor node")
    }

    fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
    where
        A: EnumAccess<'de>,
    {
        let (variant, access) = data.variant::<TypeDescriptorNodeVariant>()?;
        match variant {
            TypeDescriptorNodeVariant::Named => {
                Ok(TypeDescriptorNode::Named(access.newtype_variant()?))
            }
            TypeDescriptorNodeVariant::Reference => {
                Ok(TypeDescriptorNode::Reference(access.newtype_variant()?))
            }
            TypeDescriptorNodeVariant::List => {
                let child_depth = self.next_constructed_depth()?;
                Ok(TypeDescriptorNode::List(Box::new(
                    access.newtype_variant_seed(TypeDescriptorSeed {
                        constructed_depth: child_depth,
                        budget: self.budget,
                    })?,
                )))
            }
            TypeDescriptorNodeVariant::Set => {
                let child_depth = self.next_constructed_depth()?;
                Ok(TypeDescriptorNode::Set(Box::new(
                    access.newtype_variant_seed(TypeDescriptorSeed {
                        constructed_depth: child_depth,
                        budget: self.budget,
                    })?,
                )))
            }
            TypeDescriptorNodeVariant::Option => {
                let child_depth = self.next_constructed_depth()?;
                Ok(TypeDescriptorNode::Option(Box::new(
                    access.newtype_variant_seed(TypeDescriptorSeed {
                        constructed_depth: child_depth,
                        budget: self.budget,
                    })?,
                )))
            }
            TypeDescriptorNodeVariant::Stream => {
                let child_depth = self.next_constructed_depth()?;
                Ok(TypeDescriptorNode::Stream(Box::new(
                    access.newtype_variant_seed(TypeDescriptorSeed {
                        constructed_depth: child_depth,
                        budget: self.budget,
                    })?,
                )))
            }
            TypeDescriptorNodeVariant::Map => {
                let child_depth = self.next_constructed_depth()?;
                let payload = access.struct_variant(
                    &["key", "value"],
                    TypeDescriptorMapVisitor {
                        constructed_depth: child_depth,
                        budget: self.budget,
                    },
                )?;
                Ok(TypeDescriptorNode::Map {
                    key: Box::new(payload.key),
                    value: Box::new(payload.value),
                })
            }
        }
    }
}

impl<'budget> TypeDescriptorNodeVisitor<'budget> {
    fn next_constructed_depth<E>(&self) -> Result<usize, E>
    where
        E: de::Error,
    {
        let depth = self.constructed_depth.saturating_add(1);
        if depth > MAX_TYPE_DESCRIPTOR_DEPTH {
            return Err(E::custom(TypeDescriptorError::TooDeep {
                maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                actual: depth,
            }));
        }
        Ok(depth)
    }
}

struct TypeDescriptorMapValue {
    key: TypeDescriptor,
    value: TypeDescriptor,
}

struct TypeDescriptorMapVisitor<'budget> {
    constructed_depth: usize,
    budget: &'budget mut TypeDescriptorBudget,
}

impl<'de, 'budget> de::Visitor<'de> for TypeDescriptorMapVisitor<'budget> {
    type Value = TypeDescriptorMapValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a map type descriptor payload")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: de::MapAccess<'de>,
    {
        let mut key = None;
        let mut value = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "key" => {
                    if key.is_some() {
                        return Err(de::Error::duplicate_field("key"));
                    }
                    key = Some(map.next_value_seed(TypeDescriptorSeed {
                        constructed_depth: self.constructed_depth,
                        budget: self.budget,
                    })?);
                }
                "value" => {
                    if value.is_some() {
                        return Err(de::Error::duplicate_field("value"));
                    }
                    value = Some(map.next_value_seed(TypeDescriptorSeed {
                        constructed_depth: self.constructed_depth,
                        budget: self.budget,
                    })?);
                }
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }
        Ok(TypeDescriptorMapValue {
            key: key.ok_or_else(|| de::Error::missing_field("key"))?,
            value: value.ok_or_else(|| de::Error::missing_field("value"))?,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let key = sequence
            .next_element_seed(TypeDescriptorSeed {
                constructed_depth: self.constructed_depth,
                budget: self.budget,
            })?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let value = sequence
            .next_element_seed(TypeDescriptorSeed {
                constructed_depth: self.constructed_depth,
                budget: self.budget,
            })?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        Ok(TypeDescriptorMapValue { key, value })
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
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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
    use serde_json::json;

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

    #[test]
    fn type_descriptor_serde_round_trip_preserves_all_node_shapes() {
        let first = TypeDescriptor::named(TypeId::from_bytes([51; 16]));
        let second = TypeDescriptor::reference(TypeId::from_bytes([52; 16]));
        let descriptors = [
            first.clone(),
            second.clone(),
            TypeDescriptor::list(first.clone()).unwrap(),
            TypeDescriptor::set(first.clone()).unwrap(),
            TypeDescriptor::map(first.clone(), second.clone()).unwrap(),
            TypeDescriptor::option(first.clone()).unwrap(),
            TypeDescriptor::stream(second).unwrap(),
        ];

        for descriptor in descriptors {
            let encoded = serde_json::to_value(&descriptor).unwrap();
            let decoded: TypeDescriptor = serde_json::from_value(encoded.clone()).unwrap();
            assert_eq!(decoded, descriptor);
            assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
        }
    }

    #[test]
    fn type_descriptor_serde_ignores_unknown_fields_like_derived_deserialization() {
        let descriptor =
            TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([53; 16]))).unwrap();
        let mut encoded = serde_json::to_value(&descriptor).unwrap();
        encoded["unknown"] = json!("ignored");
        encoded["node"]["List"]["unknown"] = json!(true);

        let decoded: TypeDescriptor = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, descriptor);
    }

    #[test]
    fn type_descriptor_serde_rejects_forged_root_and_nested_metadata() {
        let descriptor =
            TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([54; 16]))).unwrap();

        let mut forged_root = serde_json::to_value(&descriptor).unwrap();
        forged_root["depth"] = json!(0);
        assert!(serde_json::from_value::<TypeDescriptor>(forged_root).is_err());

        let mut forged_nested = serde_json::to_value(&descriptor).unwrap();
        forged_nested["node"]["List"]["node_count"] = json!(2);
        assert!(serde_json::from_value::<TypeDescriptor>(forged_nested).is_err());
    }

    #[test]
    fn type_descriptor_serde_accepts_exact_depth_and_node_boundaries() {
        let mut max_depth = TypeDescriptor::named(TypeId::from_bytes([55; 16]));
        for _ in 0..MAX_TYPE_DESCRIPTOR_DEPTH {
            max_depth = TypeDescriptor::option(max_depth).unwrap();
        }
        let encoded = serde_json::to_value(&max_depth).unwrap();
        assert_eq!(
            serde_json::from_value::<TypeDescriptor>(encoded).unwrap(),
            max_depth
        );

        let mut max_nodes = TypeDescriptor::named(TypeId::from_bytes([56; 16]));
        for _ in 0..7 {
            max_nodes = TypeDescriptor::map(max_nodes.clone(), max_nodes).unwrap();
        }
        max_nodes = TypeDescriptor::list(max_nodes).unwrap();
        assert_eq!(max_nodes.node_count(), MAX_TYPE_DESCRIPTOR_NODES);
        let encoded = serde_json::to_value(&max_nodes).unwrap();
        assert_eq!(
            serde_json::from_value::<TypeDescriptor>(encoded).unwrap(),
            max_nodes
        );
    }

    #[test]
    fn type_descriptor_serde_rejects_structural_depth_and_node_overflows() {
        let mut max_depth = TypeDescriptor::named(TypeId::from_bytes([57; 16]));
        for _ in 0..MAX_TYPE_DESCRIPTOR_DEPTH {
            max_depth = TypeDescriptor::option(max_depth).unwrap();
        }
        let mut over_depth = serde_json::json!({
            "node": { "Option": serde_json::to_value(&max_depth).unwrap() },
            "depth": MAX_TYPE_DESCRIPTOR_DEPTH + 1,
            "node_count": max_depth.node_count() + 1,
        });
        let error = serde_json::from_value::<TypeDescriptor>(over_depth.take()).unwrap_err();
        assert!(error.to_string().contains("type descriptor is too deep"));

        let mut max_nodes = TypeDescriptor::named(TypeId::from_bytes([58; 16]));
        for _ in 0..7 {
            max_nodes = TypeDescriptor::map(max_nodes.clone(), max_nodes).unwrap();
        }
        max_nodes = TypeDescriptor::list(max_nodes).unwrap();
        let over_nodes = serde_json::json!({
            "node": { "List": serde_json::to_value(&max_nodes).unwrap() },
            "depth": max_nodes.depth() + 1,
            "node_count": MAX_TYPE_DESCRIPTOR_NODES + 1,
        });
        let error = serde_json::from_value::<TypeDescriptor>(over_nodes).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("type descriptor has too many nodes")
        );
    }

    #[test]
    fn type_descriptor_serde_rejects_malformed_payloads() {
        let malformed = [
            json!({}),
            json!({"node": {"Named": [1, 2, 3]}, "depth": 0, "node_count": 1}),
            json!({"node": {"Unknown": null}, "depth": 0, "node_count": 1}),
            json!({
                "node": {"Map": {"key": serde_json::to_value(TypeDescriptor::named(TypeId::from_bytes([59; 16]))).unwrap()}},
                "depth": 1,
                "node_count": 2
            }),
        ];

        for payload in malformed {
            assert!(serde_json::from_value::<TypeDescriptor>(payload).is_err());
        }
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
