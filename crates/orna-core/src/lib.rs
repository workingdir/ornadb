//! Core types shared by OrnaDB components.
//!
//! The public text form of each identifier is opaque and type-tagged. The
//! random 128-bit representation is an implementation detail, not a UUID API.

pub mod canonical_hash;
pub mod catalogue;
pub mod physical;
pub mod revision;
pub mod security;
pub mod source;
pub mod system;
pub mod types;
pub mod value;

use std::{fmt, str::FromStr};

const BASE32_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const ENCODED_BYTES_LEN: usize = 26;

/// An error returned when an identifier is not in canonical Orna text form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCanonicalId;

impl fmt::Display for InvalidCanonicalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid canonical Orna identifier")
    }
}

impl std::error::Error for InvalidCanonicalId {}

fn encode_id(bytes: [u8; 16]) -> [u8; ENCODED_BYTES_LEN] {
    let mut encoded = [0; ENCODED_BYTES_LEN];

    for (character_index, character) in encoded.iter_mut().enumerate() {
        let mut value = 0_u8;
        for bit_in_character in 0..5 {
            let bit_index = character_index * 5 + bit_in_character;
            if bit_index < 128 {
                value = (value << 1) | ((bytes[bit_index / 8] >> (7 - bit_index % 8)) & 1);
            } else {
                value <<= 1;
            }
        }
        *character = BASE32_ALPHABET[value as usize];
    }

    encoded
}

fn decode_id(encoded: &str) -> Result<[u8; 16], InvalidCanonicalId> {
    let bytes = encoded.as_bytes();
    if bytes.len() != ENCODED_BYTES_LEN {
        return Err(InvalidCanonicalId);
    }

    let mut decoded = [0; 16];
    for (character_index, character) in bytes.iter().copied().enumerate() {
        let value = BASE32_ALPHABET
            .iter()
            .position(|candidate| *candidate == character)
            .ok_or(InvalidCanonicalId)? as u8;
        if character_index == ENCODED_BYTES_LEN - 1 && value & 0b11 != 0 {
            return Err(InvalidCanonicalId);
        }

        for bit_in_character in 0..5 {
            let bit_index = character_index * 5 + bit_in_character;
            if bit_index < 128 {
                let bit = (value >> (4 - bit_in_character)) & 1;
                decoded[bit_index / 8] |= bit << (7 - bit_index % 8);
            }
        }
    }

    Ok(decoded)
}

macro_rules! define_id_common {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("An opaque Orna ", stringify!($name), ".")]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Creates an identifier from its persisted opaque bytes.
            ///
            /// This supports catalog recovery and protected import/restore.
            /// It does not expose a UUID type or UUID text format.
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Returns the bytes used by Orna-owned durable storage and codecs.
            pub const fn to_bytes(self) -> [u8; 16] {
                self.0
            }

            /// Returns the canonical opaque Orna text form.
            pub fn canonical(self) -> String {
                self.to_string()
            }

            /// Parses the canonical opaque Orna text form.
            pub fn from_canonical(value: &str) -> Result<Self, InvalidCanonicalId> {
                value.parse()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let encoded = encode_id(self.0);
                formatter.write_str($prefix)?;
                formatter.write_str(":")?;
                formatter
                    .write_str(std::str::from_utf8(&encoded).expect("base32 alphabet is UTF-8"))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!(stringify!($name), "({})"), self)
            }
        }

        impl FromStr for $name {
            type Err = InvalidCanonicalId;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let encoded = value
                    .strip_prefix(concat!($prefix, ":"))
                    .ok_or(InvalidCanonicalId)?;
                decode_id(encoded).map(Self)
            }
        }
    };
}

macro_rules! define_id {
    ($name:ident, $prefix:literal) => {
        define_id_common!($name, $prefix);

        impl $name {
            /// Creates a new collision-resistant identifier.
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().into_bytes())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

macro_rules! define_derived_id {
    ($name:ident, $prefix:literal) => {
        define_id_common!($name, $prefix);
    };
}

define_id!(TypeId, "type");
define_derived_id!(TypeBindingId, "type-binding");
define_id!(FieldId, "field");
define_id!(SchemaId, "schema");
define_id!(CatalogueRevisionId, "catalogue-revision");
define_derived_id!(StandardLibraryRevisionId, "standard-library-revision");
define_id!(SourceBundleId, "source-bundle");
define_id!(SourceUnitId, "source-unit");
define_id!(SourceRevisionId, "source-revision");
define_id!(ExpressionId, "expression");
define_id!(ObjectId, "object");
define_id!(FunctionId, "function");
define_id!(ParameterId, "parameter");
define_id!(FunctionRevisionId, "function-revision");
define_id!(StateSlotId, "state-slot");
define_id!(CallSiteId, "call-site");
define_id!(InvocationId, "invocation");
define_id!(PrincipalId, "principal");
define_id!(SecurityAuditEventId, "security-audit-event");

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, str::FromStr};

    use super::*;

    #[test]
    fn canonical_text_is_typed_opaque_and_round_trips() {
        let id = ObjectId::from_bytes([0x12; 16]);
        let canonical = id.canonical();

        assert_eq!(canonical, "object:289144gj289144gj289144gj28");
        assert_eq!(ObjectId::from_canonical(&canonical), Ok(id));
        assert_eq!(canonical.parse::<ObjectId>(), Ok(id));
        assert!(!canonical.contains('-'));
    }

    #[test]
    fn catalogue_identifiers_use_their_own_canonical_type_tags() {
        let schema = SchemaId::from_bytes([0x12; 16]);
        let revision = CatalogueRevisionId::from_bytes([0x12; 16]);
        let expression = ExpressionId::from_bytes([0x12; 16]);

        assert_eq!(schema.canonical(), "schema:289144gj289144gj289144gj28");
        assert_eq!(
            revision.canonical(),
            "catalogue-revision:289144gj289144gj289144gj28"
        );
        assert_eq!(
            expression.canonical(),
            "expression:289144gj289144gj289144gj28"
        );
        assert_eq!(SchemaId::from_canonical(&schema.canonical()), Ok(schema));
        assert_eq!(
            CatalogueRevisionId::from_canonical(&revision.canonical()),
            Ok(revision)
        );
        assert_eq!(
            ExpressionId::from_canonical(&expression.canonical()),
            Ok(expression)
        );
    }

    #[test]
    fn debug_renders_the_type_and_canonical_value() {
        let id = FunctionId::from_bytes([0x12; 16]);

        assert_eq!(
            format!("{id:?}"),
            "FunctionId(function:289144gj289144gj289144gj28)"
        );
    }

    #[test]
    fn canonical_input_rejects_other_types_and_noncanonical_values() {
        let id = TypeId::from_bytes([0; 16]);
        let canonical = id.canonical();

        assert_eq!(FieldId::from_str(&canonical), Err(InvalidCanonicalId));
        assert_eq!(
            TypeId::from_str("type:0000000000000000000000000i"),
            Err(InvalidCanonicalId)
        );
        assert_eq!(
            TypeId::from_str("type:00000000000000000000000001"),
            Err(InvalidCanonicalId)
        );
        assert_eq!(
            TypeId::from_str("type:00000000000000000000000000".to_uppercase().as_str()),
            Err(InvalidCanonicalId)
        );
    }

    #[test]
    fn generated_ids_are_unique_in_a_sample() {
        let generated = (0..10_000)
            .map(|_| InvocationId::new())
            .collect::<HashSet<_>>();

        assert_eq!(generated.len(), 10_000);
    }

    #[test]
    fn identifiers_of_different_types_cannot_be_substituted() {
        fn accepts_type_id(_: TypeId) {}

        accepts_type_id(TypeId::new());
        let field_id = FieldId::new();
        assert!(field_id.canonical().starts_with("field:"));
    }

    #[test]
    fn durable_source_and_schema_identifiers_are_typed_and_round_trip() {
        let bytes = [0x34; 16];
        let schema = SchemaId::from_bytes(bytes);
        let bundle = SourceBundleId::from_bytes(bytes);
        let unit = SourceUnitId::from_bytes(bytes);
        let revision = SourceRevisionId::from_bytes(bytes);

        assert_eq!(schema.canonical(), "schema:6gt38d1m6gt38d1m6gt38d1m6g");
        assert_eq!(
            bundle.canonical(),
            "source-bundle:6gt38d1m6gt38d1m6gt38d1m6g"
        );
        assert_eq!(unit.canonical(), "source-unit:6gt38d1m6gt38d1m6gt38d1m6g");
        assert_eq!(
            revision.canonical(),
            "source-revision:6gt38d1m6gt38d1m6gt38d1m6g"
        );
        assert_eq!(SchemaId::from_canonical(&schema.canonical()), Ok(schema));
        assert_eq!(
            SourceBundleId::from_canonical(&bundle.canonical()),
            Ok(bundle)
        );
        assert_eq!(SourceUnitId::from_canonical(&unit.canonical()), Ok(unit));
        assert_eq!(
            SourceRevisionId::from_canonical(&revision.canonical()),
            Ok(revision)
        );
        assert_eq!(
            SchemaId::from_canonical(&bundle.canonical()),
            Err(InvalidCanonicalId)
        );
    }
}
