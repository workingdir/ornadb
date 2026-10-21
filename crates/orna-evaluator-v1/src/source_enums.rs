//! Evaluator construction plans for source enum declarations.
//!
//! ORNA-ENUM-001..003: an enum is a closed nominal type whose values are
//! `Enum.variant` or `Enum.variant { field: value }`. Every host admits a
//! project's enums through this one builder, so payload-free references,
//! payload construction and `case` patterns agree on one identity.

use orna_syntax_v1::{Declaration, Item};
use sha2::{Digest, Sha256};

use crate::{NominalDefinition, NominalDefinitions, NominalVariant};

/// Builds the construction plans of the enums declared by one module.
///
/// Plans are keyed by the qualified declaration name (`main.Level`), or by
/// the bare name in an unnamed root module. The product identity manifest
/// does not allocate enum or variant ObjectIds, so both identities are
/// derived from that qualified name with domain separation: every host
/// derives the same identity for the same declaration. An enum without
/// variants has no value to construct and receives no plan.
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
        let namespace = namespace.filter(|namespace| !namespace.is_empty());
        let identity = namespace.map_or_else(|| name.clone(), |namespace| format!("{namespace}.{name}"));
        let variants = variants
            .iter()
            .map(|variant| {
                NominalVariant::new(
                    derived_id(&[b"orna.enum.variant", identity.as_bytes(), variant.name.as_bytes()]),
                    variant.name.clone(),
                )
                .with_payload_fields(variant.fields.iter().map(|field| field.name.clone()).collect())
            })
            .collect();
        let definition = NominalDefinition::new(
            derived_id(&[b"orna.enum.type", identity.as_bytes()]),
            namespace.map(str::to_owned),
            Vec::new(),
        )
        .with_enum_variants(variants);
        definitions.insert(identity, definition);
    }
    definitions
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
    use super::source_enum_definitions;
    use orna_syntax_v1::parse_module;

    #[test]
    fn qualified_declarations_receive_distinct_stable_plans() {
        let parsed = parse_module(
            "pub enum Level { low, high, } pub enum SyncResult { ok { count: Int }, } pub enum Never {}",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let main = source_enum_definitions(Some("main"), &parsed.value.items);
        assert_eq!(main.keys().collect::<Vec<_>>(), ["main.Level", "main.SyncResult"]);
        assert_eq!(main, source_enum_definitions(Some("main"), &parsed.value.items));

        let other = source_enum_definitions(Some("other"), &parsed.value.items);
        assert_ne!(main["main.Level"], other["other.Level"]);
        let root = source_enum_definitions(None, &parsed.value.items);
        assert_eq!(root.keys().collect::<Vec<_>>(), ["Level", "SyncResult"]);
    }
}
