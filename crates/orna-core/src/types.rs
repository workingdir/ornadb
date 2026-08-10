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
/// constructors. Named types and references carry resolved `TypeId` values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedType {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference { target: TypeId },
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

    /// Creates a descriptor for a typed object reference.
    pub const fn reference(target: TypeId) -> Self {
        Self::Reference { target }
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
    fn resolved_types_only_model_scalar_named_and_typed_references() {
        let named = TypeId::from_bytes([42; 16]);

        assert_eq!(
            ResolvedType::scalar(StandardScalar::Date),
            ResolvedType::Scalar(StandardScalar::Date)
        );
        assert_eq!(ResolvedType::named(named), ResolvedType::Named(named));
        assert_eq!(
            ResolvedType::reference(named),
            ResolvedType::Reference { target: named }
        );
    }
}
