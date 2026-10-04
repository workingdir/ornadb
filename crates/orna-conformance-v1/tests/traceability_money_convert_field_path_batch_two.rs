//! Bounded, fixture-backed witnesses for ten money/conversion/field/path rows.
//!
//! Every source case is a checked-in `.orna` file, including the two reference
//! key/path fixtures. A passed row below names only the observed analyzer or
//! path-codec boundary; it is not a claim of complete language conformance.

use orna_semantic_v1::{Catalogue, DIAG_TYPE, ModuleInput, Type, analyze, analyze_with_catalogue};
use orna_storage_v1::LoosePath;

fn analyzed(name: &str, source: &str) -> orna_semantic_v1::Analysis {
    analyze(&[ModuleInput::new(name, source)])
}

fn passed(requirement: &str, boundary: &str) {
    println!(
        "{requirement} specified=yes exists=yes passed=yes boundary={boundary} full_requirement_pass=no"
    );
}

fn type_error(result: &orna_semantic_v1::Analysis) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == DIAG_TYPE)
}

// Specified: ORNA-MONEY-001, frozen source/05-types.md:276.
#[test]
fn money_001_exact_decimal_constructor_keeps_nominal_currency_type() {
    let result = analyzed(
        "exact-money.orna",
        include_str!("fixtures/semantic/traceability-money-exact-decimal.orna"),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let amount = &result.modules.values().next().unwrap().exports["amount"];
    assert!(matches!(
        &amount.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("exact-money.GBP".into())],
            }
    ));
    passed("ORNA-MONEY-001", "exact-decimal-constructor-typecheck");
}

// Specified: ORNA-MONEY-002, frozen source/05-types.md:278.
#[test]
fn money_002_cross_currency_addition_is_rejected() {
    let result = analyzed(
        "cross-currency.orna",
        include_str!("fixtures/semantic/traceability-money-cross-currency-add.orna"),
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "cannot add different currencies without conversion"
    }));
    passed("ORNA-MONEY-002", "cross-currency-addition-type-error");
}

// Specified: ORNA-MONEY-003, frozen source/05-types.md:280.
#[test]
fn money_003_exact_rate_algebra_admits_decimal_and_rejects_float() {
    let exact = analyzed(
        "exact-energy-rate.orna",
        include_str!("fixtures/semantic/traceability-money-exact-energy-rate.orna"),
    );
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);

    let inexact = analyzed(
        "float-energy-rate.orna",
        include_str!("fixtures/semantic/traceability-money-float-energy-rate.orna"),
    );
    assert!(inexact.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "binary Float cannot enter an exact Money calculation implicitly"
    }));
    passed("ORNA-MONEY-003", "decimal-rate-typecheck-and-float-rejection");
}

// Specified: ORNA-MONEY-004, frozen source/05-types.md:282.
#[test]
fn money_004_currency_protocol_requires_static_properties() {
    let complete = analyzed(
        "currency-complete.orna",
        include_str!("fixtures/semantic/traceability-money-currency-protocol-valid.orna"),
    );
    assert!(complete.is_ok(), "{:?}", complete.diagnostics);

    let incomplete = analyzed(
        "currency-incomplete.orna",
        include_str!("fixtures/semantic/traceability-money-currency-protocol-incomplete.orna"),
    );
    assert!(type_error(&incomplete), "{:?}", incomplete.diagnostics);
    passed("ORNA-MONEY-004", "currency-protocol-shape-typecheck");
}

// Specified: ORNA-CONVERT-001, frozen source/05-types.md:432.
#[test]
fn convert_001_explicit_target_from_selects_nested_implementation() {
    let result = analyzed(
        "explicit-from.orna",
        include_str!("fixtures/semantic/traceability-convert-explicit-from.orna"),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let parse_email = &result.modules.values().next().unwrap().exports["parse_email"];
    assert!(matches!(
        &parse_email.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("explicit-from.EmailAddress".into())
    ));
    passed("ORNA-CONVERT-001", "explicit-target-from-dispatch-typecheck");
}

// Specified: ORNA-CONVERT-006, frozen source/05-types.md:442.
#[test]
fn convert_006_from_is_not_applied_implicitly() {
    let result = analyzed(
        "implicit-from.orna",
        include_str!("fixtures/semantic/traceability-convert-no-implicit.orna"),
    );
    assert!(type_error(&result), "{:?}", result.diagnostics);
    passed("ORNA-CONVERT-006", "implicit-from-type-error");
}

// Specified: ORNA-FIELD-006, frozen source/07-tables.md:128.
#[test]
fn field_006_computed_field_cannot_be_supplied_to_insert() {
    let result = analyzed(
        "computed-insert.orna",
        include_str!("fixtures/semantic/traceability-field-computed-insert.orna"),
    );
    assert!(type_error(&result), "{:?}", result.diagnostics);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("computed field")
    }));
    passed("ORNA-FIELD-006", "computed-field-insert-type-error");
}

// Specified: ORNA-FIELD-008, frozen source/07-tables.md:132.
#[test]
fn field_008_computed_field_rejects_io_effect() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "computed-effect.orna",
            include_str!("fixtures/semantic/traceability-field-computed-effect.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "computed field must be deterministic and row-local"
    }));
    passed("ORNA-FIELD-008", "computed-field-effect-type-error");
}

// Specified: ORNA-PATH-007, frozen source/07-tables.md:351.
#[test]
fn path_007_composite_key_path_uses_declared_order_and_final_suffix() {
    let source = include_str!("fixtures/traceability-tail-path-composite.orna");
    let result = analyzed("path-composite.orna", source);
    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let path = LoosePath::for_key("Pair", &["north".into(), "17".into()]).unwrap();
    assert_eq!(path.as_managed_path().as_path().to_string_lossy(), "Pair/north/17.orna");
    passed("ORNA-PATH-007", "fixture-typecheck-and-composite-path-order");
}

// Specified: ORNA-PATH-011, frozen source/07-tables.md:362.
#[test]
fn path_011_noncanonical_alias_is_rejected_during_discovery() {
    let source = include_str!("fixtures/traceability-tail-path-composite.orna");
    let result = analyzed("path-composite.orna", source);
    assert!(result.is_ok(), "{:?}", result.diagnostics);

    for component in ["~61lice.orna", "~FF.orna", "~e.orna"] {
        assert!(
            LoosePath::from_encoded_key("Pair", &[component.into()]).is_err(),
            "noncanonical encoded component must be rejected: {component}"
        );
    }
    passed("ORNA-PATH-011", "fixture-typecheck-and-noncanonical-path-rejection");
}
