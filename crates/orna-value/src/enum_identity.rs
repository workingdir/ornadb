//! Stable ObjectIds of source enums and their variants.
//!
//! The old pipeline and the executor must agree on the identity of every enum
//! value. Both derive it from the qualified declaration name (`main.Level`, or
//! the bare name in an unnamed root module) with domain separation, so every
//! host derives the same id for the same declaration.

use sha2::{Digest as _, Sha256};

/// The ObjectId of an enum type, derived from its qualified name.
#[must_use]
pub fn enum_type_id(qualified_name: &str) -> [u8; 16] {
    derived_id(&[b"orna.enum.type", qualified_name.as_bytes()])
}

/// The ObjectId of one variant of an enum, derived from the enum's qualified
/// name and the variant's bare name.
#[must_use]
pub fn enum_variant_id(qualified_name: &str, variant: &str) -> [u8; 16] {
    derived_id(&[
        b"orna.enum.variant",
        qualified_name.as_bytes(),
        variant.as_bytes(),
    ])
}

/// Joins a namespace and a bare declaration name into its qualified name.
///
/// An empty or absent namespace leaves the bare name unqualified, as the
/// unnamed root module does.
#[must_use]
pub fn qualified_name(namespace: Option<&str>, name: &str) -> String {
    namespace
        .filter(|namespace| !namespace.is_empty())
        .map_or_else(
            || name.to_owned(),
            |namespace| format!("{namespace}.{name}"),
        )
}

fn derived_id(parts: &[&[u8]]) -> [u8; 16] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    digest.finalize()[..16]
        .try_into()
        .expect("truncated digest has the ObjectId width")
}

#[cfg(test)]
mod tests {
    use super::{enum_type_id, enum_variant_id, qualified_name};

    #[test]
    fn qualified_name_drops_an_empty_namespace() {
        assert_eq!(qualified_name(None, "Level"), "Level");
        assert_eq!(qualified_name(Some(""), "Level"), "Level");
        assert_eq!(qualified_name(Some("main"), "Level"), "main.Level");
    }

    #[test]
    fn variant_ids_separate_domains_and_variants() {
        let level = enum_type_id("main.Level");
        assert_ne!(level, enum_type_id("main.Other"));
        assert_ne!(
            enum_variant_id("main.Level", "Low"),
            enum_variant_id("main.Level", "High")
        );
        assert_ne!(enum_variant_id("main.Level", "Low"), level);
    }
}
