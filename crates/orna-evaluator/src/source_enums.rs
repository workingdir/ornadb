//! Evaluator construction plans for source enum declarations.
//!
//! ORNA-ENUM-001..003: an enum is a closed nominal type whose values are
//! `Enum.variant` or `Enum.variant { field: value }`. Every host admits a
//! project's enums through this one builder, so payload-free references,
//! payload construction and `case` patterns agree on one identity.

use orna_syntax::{Declaration, Item};
use orna_value::{enum_type_id, enum_variant_id, qualified_name};

use crate::{NominalDefinition, NominalDefinitions, NominalVariant};

/// Builds the construction plans of the enums declared by one module.
///
/// Plans are keyed by the qualified declaration name (`main.Level`), or by
/// the bare name in an unnamed root module. The product identity manifest
/// does not allocate enum or variant ObjectIds, so both identities are
/// derived from that qualified name with domain separation (`orna_value`
/// owns the derivation, shared with the executor): every host derives the
/// same identity for the same declaration. An enum without variants has no
/// value to construct and receives no plan.
#[must_use]
pub fn source_enum_definitions(namespace: Option<&str>, items: &[Item]) -> NominalDefinitions {
    let mut definitions = NominalDefinitions::new();
    for item in items {
        let Declaration::Enum { name, variants, .. } = &item.declaration else {
            continue;
        };
        if variants.is_empty() {
            continue;
        }
        let identity = qualified_name(namespace, name);
        let variants = variants
            .iter()
            .map(|variant| {
                NominalVariant::new(
                    enum_variant_id(&identity, &variant.name),
                    variant.name.clone(),
                )
                .with_payload_fields(
                    variant
                        .fields
                        .iter()
                        .map(|field| field.name.clone())
                        .collect(),
                )
            })
            .collect();
        let namespace = namespace.filter(|namespace| !namespace.is_empty());
        let definition = NominalDefinition::new(
            enum_type_id(&identity),
            namespace.map(str::to_owned),
            Vec::new(),
        )
        .with_enum_variants(variants);
        definitions.insert(identity, definition);
    }
    definitions
}

#[cfg(test)]
mod tests {
    use super::source_enum_definitions;
    use orna_syntax::parse_module;

    #[test]
    fn qualified_declarations_receive_distinct_stable_plans() {
        let parsed = parse_module(
            "pub enum Level { low, high, } pub enum SyncResult { ok { count: Int }, } pub enum Never {}",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let main = source_enum_definitions(Some("main"), &parsed.value.items);
        assert_eq!(
            main.keys().collect::<Vec<_>>(),
            ["main.Level", "main.SyncResult"]
        );
        assert_eq!(
            main,
            source_enum_definitions(Some("main"), &parsed.value.items)
        );

        let other = source_enum_definitions(Some("other"), &parsed.value.items);
        assert_ne!(main["main.Level"], other["other.Level"]);
        let root = source_enum_definitions(None, &parsed.value.items);
        assert_eq!(root.keys().collect::<Vec<_>>(), ["Level", "SyncResult"]);
    }

    #[test]
    fn enum_ids_keep_the_domain_separated_derivation() {
        use sha2::{Digest, Sha256};
        // The length-prefixed SHA-256 over the domain tag and each part, as
        // the evaluator derived these ids before the derivation moved to
        // orna_value. A change here would silently re-key every enum value.
        let expect = |parts: &[&[u8]]| {
            let mut digest = Sha256::new();
            for part in parts {
                digest.update((part.len() as u64).to_be_bytes());
                digest.update(part);
            }
            let mut id = [0_u8; 16];
            id.copy_from_slice(&digest.finalize()[..16]);
            id
        };
        let parsed = parse_module("pub enum Level { low, }");
        let definitions = source_enum_definitions(Some("main"), &parsed.value.items);
        let level = &definitions["main.Level"];
        assert_eq!(
            level.type_id,
            crate::object_id_raw(expect(&[b"orna.enum.type", b"main.Level"]))
        );
        assert_eq!(
            level.variants[0].variant_id,
            crate::object_id_raw(expect(&[b"orna.enum.variant", b"main.Level", b"low"]))
        );
    }
}
