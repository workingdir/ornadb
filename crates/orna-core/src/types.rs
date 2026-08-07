//! Resolved Orna type descriptors.

use crate::TypeId;

/// A standard scalar type with a stable catalogue identity.
///
/// Source spelling is intentionally absent. The lossless syntax tree retains
/// it before semantic resolution selects one of these canonical values.
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

    /// Resolves a standard scalar spelling without retaining that spelling.
    pub fn from_source_spelling(source: &str) -> Result<Self, ScalarResolutionError> {
        let source = source.trim();

        for scalar in Self::ALL {
            if scalar.matches_source_spelling(source) {
                return Ok(scalar);
            }
        }

        Err(ScalarResolutionError::UnknownStandardScalar)
    }

    /// Returns the canonical Orna spelling for this scalar type.
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Boolean => "BOOLEAN",
            Self::Integer => "INTEGER",
            Self::BigInt => "BIGINT",
            Self::Float => "FLOAT",
            Self::Decimal => "DECIMAL",
            Self::CharacterLargeObject => "CHARACTER LARGE OBJECT",
            Self::BinaryLargeObject => "BINARY LARGE OBJECT",
            Self::Uuid => "UUID",
            Self::Date => "DATE",
            Self::Time => "TIME",
            Self::Timestamp => "TIMESTAMP",
            Self::Duration => "DURATION",
            Self::Void => "VOID",
        }
    }

    /// Returns this standard scalar's stable Orna type identity.
    pub const fn type_id(self) -> TypeId {
        let discriminator = match self {
            Self::Boolean => 1,
            Self::Integer => 2,
            Self::BigInt => 3,
            Self::Float => 4,
            Self::Decimal => 5,
            Self::CharacterLargeObject => 6,
            Self::BinaryLargeObject => 7,
            Self::Uuid => 8,
            Self::Date => 9,
            Self::Time => 10,
            Self::Timestamp => 11,
            Self::Duration => 12,
            Self::Void => 13,
        };

        TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, discriminator])
    }

    fn matches_source_spelling(self, source: &str) -> bool {
        match self {
            Self::Boolean => matches_one_of(source, &["BOOLEAN", "BOOL"]),
            Self::Integer => matches_one_of(source, &["INTEGER", "INT"]),
            Self::BigInt => matches_one_of(source, &["BIGINT"]),
            Self::Float => matches_one_of(source, &["FLOAT"]),
            Self::Decimal => matches_one_of(source, &["DECIMAL"]),
            Self::CharacterLargeObject => {
                matches_one_of(source, &["CHARACTER LARGE OBJECT", "CLOB", "TEXT"])
            }
            Self::BinaryLargeObject => {
                matches_one_of(source, &["BINARY LARGE OBJECT", "BLOB", "BYTES"])
            }
            Self::Uuid => matches_one_of(source, &["UUID"]),
            Self::Date => matches_one_of(source, &["DATE"]),
            Self::Time => matches_one_of(source, &["TIME"]),
            Self::Timestamp => matches_one_of(source, &["TIMESTAMP"]),
            Self::Duration => matches_one_of(source, &["DURATION"]),
            Self::Void => matches_one_of(source, &["VOID"]),
        }
    }
}

fn matches_one_of(source: &str, spellings: &[&str]) -> bool {
    spellings
        .iter()
        .any(|spelling| source.eq_ignore_ascii_case(spelling))
}

/// An error returned when a spelling is not an Orna standard scalar type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarResolutionError {
    UnknownStandardScalar,
}

impl std::fmt::Display for ScalarResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("unknown Orna standard scalar type")
    }
}

impl std::error::Error for ScalarResolutionError {}

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
    fn aliases_resolve_to_their_canonical_scalar_identity() {
        for (source, canonical) in [
            ("BOOL", StandardScalar::Boolean),
            ("BOOLEAN", StandardScalar::Boolean),
            ("INT", StandardScalar::Integer),
            ("INTEGER", StandardScalar::Integer),
            ("TEXT", StandardScalar::CharacterLargeObject),
            ("CLOB", StandardScalar::CharacterLargeObject),
            (
                "CHARACTER LARGE OBJECT",
                StandardScalar::CharacterLargeObject,
            ),
            ("BYTES", StandardScalar::BinaryLargeObject),
            ("BLOB", StandardScalar::BinaryLargeObject),
            ("BINARY LARGE OBJECT", StandardScalar::BinaryLargeObject),
        ] {
            assert_eq!(StandardScalar::from_source_spelling(source), Ok(canonical));
            assert_eq!(
                StandardScalar::from_source_spelling(&source.to_lowercase()),
                Ok(canonical)
            );
        }

        assert_eq!(
            StandardScalar::Boolean.type_id(),
            StandardScalar::from_source_spelling("bool")
                .unwrap()
                .type_id()
        );
    }

    #[test]
    fn canonical_scalar_names_and_ids_cover_the_initial_standard_set() {
        let scalars = StandardScalar::ALL;

        assert_eq!(scalars.len(), 13);
        for scalar in scalars.iter().copied() {
            assert_eq!(
                StandardScalar::from_source_spelling(scalar.canonical_name()),
                Ok(scalar)
            );
        }
        assert_eq!(
            scalars
                .iter()
                .map(|scalar| scalar.type_id())
                .collect::<HashSet<_>>()
                .len(),
            scalars.len()
        );
        assert_eq!(StandardScalar::Boolean.canonical_name(), "BOOLEAN");
        assert_eq!(StandardScalar::Integer.canonical_name(), "INTEGER");
        assert_eq!(
            StandardScalar::CharacterLargeObject.canonical_name(),
            "CHARACTER LARGE OBJECT"
        );
        assert_eq!(
            StandardScalar::BinaryLargeObject.canonical_name(),
            "BINARY LARGE OBJECT"
        );
    }

    #[test]
    fn postgresql_only_spellings_do_not_resolve() {
        for spelling in ["BYTEA", "SERIAL", "JSONB", "TIMESTAMPTZ"] {
            assert!(StandardScalar::from_source_spelling(spelling).is_err());
        }
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
