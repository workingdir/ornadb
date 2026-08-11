//! Resolved Orna type descriptors.

use crate::TypeId;

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

    use super::{ResolvedType, StandardScalar};
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
}
