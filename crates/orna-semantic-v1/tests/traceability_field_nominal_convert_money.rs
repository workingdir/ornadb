//! Focused traceability evidence for selected field, nominal, conversion, and
//! money gaps. The frozen requirement register remains unchanged. These tests
//! run the retained semantic analyzer over checked-in source fixtures; they do
//! not claim full compiler or Orna-engine execution.

use orna_semantic_v1::{
    Catalogue, DIAG_TYPE, ModuleInput, Type, analyze, analyze_with_catalogue,
};

fn has_type_diagnostic(result: &orna_semantic_v1::Analysis) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == DIAG_TYPE)
}

// Specified: ORNA-FIELD-006, source/07-tables.md:128.
// Exists: this fixture-backed table-write analysis.
// Passed is bounded to rejecting a supplied computed selector at insert.
#[test]
fn computed_field_cannot_be_supplied_to_insert() {
    let result = analyze(&[ModuleInput::new(
        "computed-insert.orna",
        include_str!("fixtures/traceability-field-computed-insert.orna"),
    )]);

    assert!(has_type_diagnostic(&result), "{:?}", result.diagnostics);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message().contains("computed field")
    }));
}

// Specified: ORNA-FIELD-008, source/07-tables.md:132.
// Exists: this fixture-backed effect analysis.
// Passed is bounded to rejecting an effectful computed expression.
#[test]
fn computed_field_rejects_io_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "computed-effect.orna",
            include_str!("fixtures/traceability-field-computed-effect.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "computed field must be deterministic and row-local"
    }));
}

// Specified: ORNA-NOMINAL-002, source/05-types.md:368.
// Exists: this fixture-backed structural-to-nominal type analysis.
// Passed is bounded to rejecting that implicit structural assignment.
#[test]
fn structural_record_does_not_satisfy_nominal_type() {
    let result = analyze(&[ModuleInput::new(
        "nominal-record.orna",
        include_str!("fixtures/traceability-nominal-structural-record.orna"),
    )]);

    assert!(has_type_diagnostic(&result), "{:?}", result.diagnostics);
}

// Specified: ORNA-NOMINAL-005, source/05-types.md:374.
// Exists: private and public field fixtures analyzed independently.
// Passed is bounded to visibility checking of direct field access.
#[test]
fn nominal_field_visibility_controls_representation_access() {
    let private = analyze(&[ModuleInput::new(
        "private-token.orna",
        include_str!("fixtures/traceability-nominal-private-field.orna"),
    )]);
    assert!(has_type_diagnostic(&private), "{:?}", private.diagnostics);

    let public = analyze(&[ModuleInput::new(
        "public-token.orna",
        include_str!("fixtures/traceability-nominal-public-field.orna"),
    )]);
    assert!(public.is_ok(), "{:?}", public.diagnostics);
}

// Specified: ORNA-CONVERT-001, source/05-types.md:432.
// Exists: this fixture-backed nested From dispatch analysis.
// Passed is bounded to the explicit Target.from(Source) call's inferred type.
#[test]
fn target_from_selects_its_nested_source_implementation() {
    let result = analyze(&[ModuleInput::new(
        "explicit-from.orna",
        include_str!("fixtures/traceability-convert-explicit-from.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let parse_email = &result.modules.values().next().unwrap().exports["parse_email"];
    assert!(matches!(
        &parse_email.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("explicit-from.EmailAddress".into())
    ), "unexpected conversion result type: {:?}", parse_email.ty);
}

// Specified: ORNA-CONVERT-006, source/05-types.md:442.
// Exists: this fixture-backed implicit-conversion analysis.
// Passed is bounded to refusing an implicit From application.
#[test]
fn from_implementation_is_not_applied_implicitly() {
    let result = analyze(&[ModuleInput::new(
        "implicit-from.orna",
        include_str!("fixtures/traceability-convert-no-implicit.orna"),
    )]);

    assert!(has_type_diagnostic(&result), "{:?}", result.diagnostics);
}

// Specified: ORNA-MONEY-001, source/05-types.md:276.
// Exists: exact-decimal and inexact-Float constructor fixtures.
// Passed is bounded to exact Money construction and Float rejection.
#[test]
fn money_construction_keeps_decimal_exactness() {
    let exact = analyze(&[ModuleInput::new(
        "exact-money.orna",
        include_str!("fixtures/traceability-money-exact-decimal.orna"),
    )]);
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);
    let amount = &exact.modules.values().next().unwrap().exports["amount"];
    assert!(matches!(
        &amount.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("exact-money.GBP".into())],
            }
    ), "unexpected exact-money result type: {:?}", amount.ty);

    let inexact = analyze(&[ModuleInput::new(
        "inexact-money.orna",
        include_str!("fixtures/traceability-money-float-constructor.orna"),
    )]);
    assert!(inexact.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message()
                == "Money cannot be constructed from an inexact Float without explicit rounding"
    }));
}

// Specified: ORNA-MONEY-002, source/05-types.md:278.
// Exists: this fixture-backed cross-currency addition analysis.
// Passed is bounded to rejection of unconverted GBP + USD.
#[test]
fn different_currency_amounts_require_explicit_conversion() {
    let result = analyze(&[ModuleInput::new(
        "cross-currency.orna",
        include_str!("fixtures/traceability-money-cross-currency-add.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "cannot add different currencies without conversion"
    }));
}

// Specified: ORNA-MONEY-003, source/05-types.md:280.
// Exists: exact Decimal and binary Float dimensional fixtures.
// Passed is bounded to exact Decimal * Money/rate admission and Float rejection.
#[test]
fn exact_energy_rate_algebra_excludes_binary_float() {
    let exact = analyze(&[ModuleInput::new(
        "exact-energy-rate.orna",
        include_str!("fixtures/traceability-money-exact-energy-rate.orna"),
    )]);
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);

    let inexact = analyze(&[ModuleInput::new(
        "float-energy-rate.orna",
        include_str!("fixtures/traceability-money-float-energy-rate.orna"),
    )]);
    assert!(inexact.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message()
                == "binary Float cannot enter an exact Money calculation implicitly"
    }));
}

// Specified: ORNA-MONEY-004, source/05-types.md:282.
// Exists: complete and incomplete Currency implementation fixtures.
// Passed is bounded to checking required static property implementation shape.
#[test]
fn currency_requires_code_and_minor_digits_properties() {
    let complete = analyze(&[ModuleInput::new(
        "currency-complete.orna",
        include_str!("fixtures/traceability-money-currency-protocol-valid.orna"),
    )]);
    assert!(complete.is_ok(), "{:?}", complete.diagnostics);

    let incomplete = analyze(&[ModuleInput::new(
        "currency-incomplete.orna",
        include_str!("fixtures/traceability-money-currency-protocol-incomplete.orna"),
    )]);
    assert!(has_type_diagnostic(&incomplete), "{:?}", incomplete.diagnostics);
}
