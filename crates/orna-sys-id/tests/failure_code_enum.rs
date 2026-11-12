//! Parity of the hand-written `FailureCode` enums with the frozen failure-code tables (sys-as-native section 3.4,
//! decision F1).
//!
//! `sys.orna` holds the 67 portable codes and `sys/engine.orna` holds the 61 engine codes. Both files are parsed with
//! `orna-syntax`, never scanned as text. A variant spells its code with `__` for each dot, so `object__not_found` is
//! `sys.object.not_found`. The test then requires the variants, in source order, to equal the frozen catalogue lists.

use orna_syntax::{Declaration, parse_interface_with_file};
use serde_json::Value;

const SYS_ORNA: &str = include_str!("../../orna-hir/interface/sys.orna");
const ENGINE_ORNA: &str = include_str!("../../orna-hir/interface/sys/engine.orna");
fn catalogue() -> Value {
    orna_sys_interface::catalogue_json(
        &orna_sys_interface::native_raw_document().expect("sys.orna decodes"),
    )
}
const ENGINE: &str = include_str!("../catalogue/engine-1.1.0.json");

/// The variant names of the single `FailureCode` enum in `source`, in source order.
fn declared_variants(source: &str, file: &str) -> Vec<String> {
    let parsed = parse_interface_with_file(source, file);
    assert!(
        parsed.diagnostics.is_empty(),
        "{file} must parse without diagnostics: {:?}",
        parsed.diagnostics
    );
    let mut enums = parsed
        .value
        .items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Enum { name, variants, .. } if name == "FailureCode" => Some(
                variants
                    .iter()
                    .map(|variant| variant.name.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        });
    let variants = enums
        .next()
        .unwrap_or_else(|| panic!("{file} declares no FailureCode enum"));
    assert!(
        enums.next().is_none(),
        "{file} declares more than one FailureCode enum"
    );
    variants
}

/// The wire codes that a variant spells: `host__filesystem__not_found` becomes `sys.host.filesystem.not_found`.
fn spelled_codes(variants: &[String]) -> Vec<String> {
    variants
        .iter()
        .map(|variant| format!("sys.{}", variant.replace("__", ".")))
        .collect()
}

/// The `failure_codes` array of the sys catalogue rendered from `sys.orna`.
fn frozen_codes(root: &Value) -> Vec<String> {
    root["failure_codes"]
        .as_array()
        .expect("failure_codes is an array")
        .iter()
        .map(|code| code.as_str().expect("failure code is a string").to_owned())
        .collect()
}

#[test]
fn portable_failure_code_enum_is_the_frozen_sys_set_in_order() {
    let variants = declared_variants(SYS_ORNA, "sys.orna");
    assert_eq!(spelled_codes(&variants), frozen_codes(&catalogue()));
    assert_eq!(variants.len(), 67);
}

#[test]
fn engine_failure_code_enum_is_the_frozen_engine_set_in_order() {
    let variants = declared_variants(ENGINE_ORNA, "sys/engine.orna");
    let engine: Value = serde_json::from_str(ENGINE).expect("engine catalogue is JSON");
    assert_eq!(spelled_codes(&variants), frozen_codes(&engine));
    assert_eq!(variants.len(), 61);
}

#[test]
fn every_variant_spelling_is_invertible_and_the_two_sets_are_disjoint() {
    let variants: Vec<String> = declared_variants(SYS_ORNA, "sys.orna")
        .into_iter()
        .chain(declared_variants(ENGINE_ORNA, "sys/engine.orna"))
        .collect();
    let codes = spelled_codes(&variants);
    for (variant, code) in variants.iter().zip(&codes) {
        assert_eq!(
            &code["sys.".len()..].replace('.', "__"),
            variant,
            "{code} does not invert to {variant}"
        );
    }
    let mut distinct = codes.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        128,
        "the 67 portable and 61 engine codes are 128 distinct codes"
    );
}
