//! Executable audit counterexamples for ornadb-gov5.17.12.
//!
//! Confirmed external reference: https://github.com/workingdir/ornadb/issues/780
//! Normative references below use the frozen specification's requirement IDs.
//! These tests assert required rejection, never acceptance of a known gap.
//! The candidate intentionally retains residuals for generic substitution and
//! bounds, imported protocol members, full runtime conversion dispatch, and
//! arbitrary default equivalence. The three former ignored probes are active
//! regressions.
//!
//! Inputs use in-memory modules where compact setup helps and checked-in `.orna`
//! fixtures for source programs whose exact text is under review.

use std::collections::{BTreeMap, BTreeSet};
fn render_source_fixture(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut source = template.to_owned();
    for (name, value) in replacements {
        source = source.replace(&format!("{{{name}}}"), value);
    }
    source
}

use orna_semantic_v1::{
    Analysis, AssertionOwner, Catalogue, DIAG_ANNOTATION, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT,
    DIAG_DUPLICATE, DIAG_TYPE, DIAG_UNRESOLVED, DIAG_UNSUPPORTED, EffectSummary, ModuleHeader,
    ModuleInput, Namespace, Symbol, SymbolKind, Type, analyze,
};
use orna_syntax_v1::parse_module_with_file;

fn analyze_main(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("main.orna", source)])
}

fn expect_diagnostics(result: &Analysis, expected_codes: &[&str]) {
    // These are the public API's semantic classifications, not new spec codes.
    // Requiring only the expected classifications excludes accidental parse,
    // import, and unsupported-syntax failures from satisfying a regression.
    assert!(
        !result.is_ok(),
        "expected semantic rejection with {expected_codes:?}, got no diagnostics"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| expected_codes.contains(&diagnostic.code())),
        "expected only {expected_codes:?}, got {:#?}",
        result.diagnostics
    );
}

fn expect_accepted(result: &Analysis) {
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

// Algorithm INFER-1 steps 3-6:
// constrain contextual types and assignments, solve, reject incompatibility.
#[test]
fn local_annotated_initializer_mismatch_requires_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/536091da5514.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn compatible_local_annotated_initializers_are_accepted() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/650facfcbeca.orna"
    ));
    expect_accepted(&result);
}

// ORNA-INFER-002/-007 and ORNA-S020-ANNOTATION: a field shape inferred only
// from an unconstrained lambda body cannot export an internal Type::Error.
#[test]
fn underconstrained_lambda_field_inference_requires_annotation() {
    let underconstrained =
        analyze_main(include_str!("fixtures/underconstrained-lambda-field.orna"));
    expect_diagnostics(&underconstrained, &[DIAG_ANNOTATION]);
    assert_eq!(
        underconstrained
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DIAG_ANNOTATION)
            .count(),
        1
    );
    assert!(
        !underconstrained
            .modules
            .get(&Namespace(Vec::new()))
            .expect("main module")
            .exports
            .contains_key("getter")
    );

    let constrained = analyze_main(include_str!("fixtures/constrained-lambda.orna"));
    expect_accepted(&constrained);

    let known_row = analyze_main(include_str!("fixtures/known-row-lambda.orna"));
    expect_accepted(&known_row);
}

// ORNA-INFER-002 and -007: an unsupported annotation must reject at its
// declaration boundary rather than become an internal wildcard that permits
// incompatible function bodies or calls.
#[test]
fn unsupported_product_annotation_requires_type_diagnostic() {
    let result = analyze_main(include_str!("fixtures/unsupported-product-annotation.orna"));
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message() == "type product is not a supported static type"));
}

// Diagnostic control for the same Int/Str conflict at a checked boundary.
#[test]
fn incompatible_function_return_reports_type_diagnostic() {
    let result = analyze_main(include_str!("fixtures/incompatible-function-return.orna"));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn annotated_direct_return_is_checked_and_ends_the_function_body() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/70159a60b3af.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn direct_return_ends_body_before_later_statement() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/fe0cf9ed6d33.orna"
    ));
    expect_accepted(&result);
    let symbol = result
        .modules
        .values()
        .next()
        .and_then(|module| module.symbols.get("integer"))
        .expect("inferred direct-return function");
    assert!(matches!(
        &symbol.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn nested_return_in_fallthrough_control_flow_is_supported() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/26126de3f0e9.orna"
    ));
    expect_accepted(&result);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNSUPPORTED),
        include_str!("fixtures/inline-v1_review_regressions/61675b394167.orna"),
        result.diagnostics
    );
    let symbol = result
        .modules
        .values()
        .next()
        .and_then(|module| module.symbols.get("nested"))
        .expect("inferred nested-return function");
    assert!(matches!(
        &symbol.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn direct_return_mismatch_reports_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/9f0266efa9a9.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-MODULE-003 and Algorithm INFER-1 step 4: resolve exported signatures.
#[test]
fn undeclared_annotation_name_requires_unresolved_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/cc8e2c3372f9.orna"
    ));
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

#[test]
fn undeclared_table_and_nominal_field_types_require_unresolved_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/abbc65c3b32d.orna"
    ));
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

#[test]
fn declared_nominal_annotation_name_is_accepted() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/b6dae601fd0e.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn qualified_annotation_names_use_imported_module_scope() {
    let accepted = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/bcff0b106e1b.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/c7ee78772255.orna"),
        ),
    ]);
    expect_accepted(&accepted);

    let undeclared = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/bcff0b106e1b.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/337750482f80.orna"),
        ),
    ]);
    expect_diagnostics(&undeclared, &[DIAG_UNRESOLVED]);

    let not_imported = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/bcff0b106e1b.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/06d7a49fb38a.orna"),
        ),
    ]);
    expect_diagnostics(&not_imported, &[DIAG_UNRESOLVED]);
}

#[test]
fn generic_and_function_type_annotations_remain_resolvable() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/f79e48c1615f.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn dimensional_annotation_arguments_must_resolve() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/2d47b6bd31ce.orna"
    ));
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

// Diagnostic control: unresolved value names already have a classification.
#[test]
fn undeclared_value_name_reports_unresolved_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/8bc1c9cda62b.orna"
    ));
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

// ORNA-NOMINAL-002 and -005 through -007:
// nominal identity survives inference; private-by-default fields are not
// accessible to unrelated modules, including through inferred factory returns.
//
// This preserves the reviewed counterexample while keeping construction valid
// in the owning module. Rejection must come from private field access in the
// importing module.
#[test]
fn private_nominal_field_through_factory_requires_semantic_diagnostic() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/a3b77bf5d588.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/7dfe3be1c768.orna"),
        ),
    ]);
    // A private representation may be rejected as an invalid type operation
    // or as a field unavailable to resolution; no exact wording is prescribed.
    expect_diagnostics(&result, &[DIAG_TYPE, DIAG_UNRESOLVED]);
}

// The only semantic change from the negative input is field visibility.
// This validates module imports, nominal construction, factory inference,
// and chained access without depending on an internal signature encoding.
#[test]
fn public_nominal_field_through_factory_is_accepted() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/bf3122a39c0a.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/7dfe3be1c768.orna"),
        ),
    ]);
    expect_accepted(&result);
}

// ORNA-NOMINAL-005/-007: an imported constructor sees only public fields,
// while declaration-backed required-field metadata prevents it from
// manufacturing a value whose private representation is incomplete.
#[test]
fn imported_nominal_constructor_requires_private_fields() {
    let incomplete = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/e7299a7c8ea0.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/1ba51d4260a4.orna"),
        ),
    ]);
    expect_diagnostics(&incomplete, &[DIAG_TYPE]);

    let supplied_private = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/e7299a7c8ea0.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/36d85be41bce.orna"),
        ),
    ]);
    expect_diagnostics(&supplied_private, &[DIAG_TYPE]);

    let qualified_incomplete = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/e7299a7c8ea0.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/72ba29c5970b.orna"),
        ),
    ]);
    expect_diagnostics(&qualified_incomplete, &[DIAG_TYPE]);

    let qualified_shadowed = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/2dbe4cd1e9aa.orna"),
        ),
    ]);
    expect_diagnostics(&qualified_shadowed, &[DIAG_UNRESOLVED]);
}

#[test]
fn imported_nominal_constructor_accepts_complete_public_fields_and_defaults() {
    let complete = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/36d85be41bce.orna"),
        ),
    ]);
    expect_accepted(&complete);

    let defaulted = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/19ab22a1fa6d.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/1ba51d4260a4.orna"),
        ),
    ]);
    expect_accepted(&defaulted);

    let local = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/f44dbd332f97.orna"
    ));
    expect_accepted(&local);

    let qualified = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/3ee8a9d68fd0.orna"),
        ),
    ]);
    expect_accepted(&qualified);

    let aliased = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/bb5cadcf2ab8.orna"),
        ),
    ]);
    expect_accepted(&aliased);

    let colliding_short_names = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/a91e8af79025.orna"),
        ),
        ModuleInput::new(
            "beta.orna",
            include_str!("fixtures/inline-v1_review_regressions/77f004ddc05b.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/565f8601f51a.orna"),
        ),
    ]);
    expect_accepted(&colliding_short_names);

    let private_default_supplied = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/7288fe6c6e54.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/36d85be41bce.orna"),
        ),
    ]);
    expect_diagnostics(&private_default_supplied, &[DIAG_TYPE]);

    let local_private_required = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/69d7872c4490.orna"
    ));
    expect_diagnostics(&local_private_required, &[DIAG_TYPE]);

    let exported = analyze(&[ModuleInput::new(
        "vault.orna",
        include_str!("fixtures/inline-v1_review_regressions/e7299a7c8ea0.orna"),
    )]);
    let vault = exported
        .modules
        .get(&Namespace(vec!["vault".into()]))
        .and_then(|module| module.exports.get("Vault"))
        .expect("exported nominal type");
    let schema = vault.table_schema.as_ref().expect("nominal schema");
    let admission = schema.admission.as_ref().expect("nominal admission");
    assert!(!schema.fields.contains_key("value"));
    assert!(!admission.required.contains("value"));
    assert!(admission.private_required);
}

#[test]
fn nominal_field_defaults_use_earlier_owner_local_fields() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/3b49f8a13cb2.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn incompatible_nominal_field_default_requires_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/9e9477f3a305.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-NOMINAL-002 and ORNA-IMPORT-001/-004: distinct declarations remain
// distinct even when their short source names and public representations match.
#[test]
fn same_short_name_nominals_from_distinct_modules_are_incompatible() {
    let result = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/376cd3f540df.orna"),
        ),
        ModuleInput::new(
            "beta.orna",
            include_str!("fixtures/inline-v1_review_regressions/df55822deaca.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/7da857d11947.orna"),
        ),
    ]);
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-NOMINAL-002 and ORNA-IMPORT-001/-004: named, qualified, and aliased
// references to one exported declaration share one compiler-local identity.
#[test]
fn named_qualified_and_aliased_nominal_references_agree() {
    let result = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/92dd6f19509b.orna"),
        ),
    ]);
    expect_accepted(&result);
}

// ORNA-SYNTAX-001 and ORNA-NOMINAL-002: a qualified constructor produces the
// declaration selected by the same qualified spelling in its result annotation.
#[test]
fn qualified_nominal_constructor_satisfies_qualified_annotation() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/2ea7d609726a.orna"),
        ),
    ]);
    expect_accepted(&result);
}

// ORNA-INFER-011 and ORNA-NOMINAL-002: an inferred factory retains the
// defining module's nominal identity and cannot satisfy another module's type.
#[test]
fn inferred_nominal_factory_identity_cannot_cross_assign() {
    let result = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/e5d3d1311247.orna"),
        ),
        ModuleInput::new(
            "beta.orna",
            include_str!("fixtures/inline-v1_review_regressions/869dd9d55642.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/e31302fa8f59.orna"),
        ),
    ]);
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-NOMINAL-002 and ORNA-IMPORT-004: same-short-name imports keep their
// declaration-specific row metadata instead of selecting the first row seen.
#[test]
fn same_short_name_nominal_rows_do_not_mix_between_modules() {
    let accepted = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/a91e8af79025.orna"),
        ),
        ModuleInput::new(
            "beta.orna",
            include_str!("fixtures/inline-v1_review_regressions/051af6bfc0a5.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/cf2e59f13ebb.orna"),
        ),
    ]);
    expect_accepted(&accepted);

    let mixed = analyze(&[
        ModuleInput::new(
            "alpha.orna",
            include_str!("fixtures/inline-v1_review_regressions/a91e8af79025.orna"),
        ),
        ModuleInput::new(
            "beta.orna",
            include_str!("fixtures/inline-v1_review_regressions/051af6bfc0a5.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/18a90e3e6789.orna"),
        ),
    ]);
    expect_diagnostics(&mixed, &[DIAG_TYPE]);
}

// ORNA-IMPL-005: a local protocol implementation belongs to its canonical
// nominal owner and must not satisfy the same-short-name imported type.
#[test]
fn protocol_ownership_uses_canonical_nominal_identity() {
    let result = analyze(&[
        ModuleInput::new(
            "foreign.orna",
            include_str!("fixtures/inline-v1_review_regressions/bcff0b106e1b.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/22e1db4ee32f.orna"),
        ),
    ]);
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-INFER-011: block-local annotations use the same identity resolver as
// function signatures and nominal constructors.
#[test]
fn block_local_nominal_annotations_use_canonical_identity() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/f988cc999a1f.orna"),
        ),
    ]);
    expect_accepted(&result);
}

// ORNA-INFER-011: finite-list callback annotations agree with the resolved
// element identity in both named and aliased forms.
#[test]
fn finite_list_callback_annotations_use_canonical_identity() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            include_str!("fixtures/inline-v1_review_regressions/11dad0c479b9.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/c8bb17ed2472.orna"),
        ),
    ]);
    expect_accepted(&result);
}

// ORNA-INFER-011: generic money constructor arguments use the same canonical
// nominal currency identity as their contextual result type.
#[test]
fn generic_money_constructor_uses_canonical_currency_identity() {
    let result = analyze(&[
        ModuleInput::new(
            "currency.orna",
            include_str!("fixtures/inline-v1_review_regressions/a063e01f93b5.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/660cce5c7c85.orna"),
        ),
    ]);
    expect_accepted(&result);
}

#[test]
fn nominal_constructor_rejects_duplicate_fields() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/19fa3af641ed.orna"
    ));
    expect_diagnostics(&result, &[DIAG_DUPLICATE]);

    let first_value_is_checked = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/be11306a3c89.orna"
    ));
    expect_diagnostics(&first_value_is_checked, &[DIAG_DUPLICATE, DIAG_TYPE]);
}

// ORNA-GENERIC-011: every required member
// must have exactly one compatible implementation in the nested impl.
#[test]
fn missing_nested_impl_member_requires_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/2c862894f5b8.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn unresolved_or_nonprotocol_nested_impl_identity_requires_type_diagnostic() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/1ef06ccd1b6a.orna"),
        include_str!("fixtures/inline-v1_review_regressions/46e3ebde3622.orna"),
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }

    // Display is the narrow builtin presentation residual; it has no local
    // protocol member AST for this validator to resolve.
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/93d14ed4884c.orna"
    )));
}

// ORNA-GENERIC-011. The body matches its own Str annotation, so the
// conflict is specifically between the member and the protocol signature.
#[test]
fn wrong_nested_impl_return_signature_requires_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/01ec62a3ef99.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-GENERIC-011. Return types and the body agree; the input type alone
// differs, so checking only the implementation's return type is insufficient.
#[test]
fn wrong_nested_impl_parameter_signature_requires_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/f97db473ceb2.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn compatible_nested_impl_signatures_are_accepted() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/f61a59e6a239.orna"),
        include_str!("fixtures/inline-v1_review_regressions/04724ed44a4a.orna"),
        include_str!("fixtures/inline-v1_review_regressions/6180e3453a61.orna"),
        include_str!("fixtures/inline-v1_review_regressions/16b9ff823429.orna"),
        include_str!("fixtures/inline-v1_review_regressions/e656b153fbec.orna"),
        include_str!("fixtures/inline-v1_review_regressions/75ed9a4a4a57.orna"),
    ] {
        expect_accepted(&analyze_main(source));
    }
}

#[test]
fn nested_impl_direct_return_uses_member_result_context() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/de5a76607214.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn nested_impl_direct_return_mismatch_reports_one_type_diagnostic() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/3163b349bda6.orna"
    ));
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected one diagnostic, got {:#?}",
        result.diagnostics
    );
    assert_eq!(result.diagnostics[0].code(), DIAG_TYPE);
}

#[test]
fn unreachable_break_and_tail_after_direct_return_are_ignored() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/c3d5b112dc50.orna"
    ));
    expect_accepted(&result);
}

#[test]
fn unconstrained_nonself_nested_impl_parameter_requires_type_diagnostic() {
    let rejected = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/c5b78f27bcd0.orna"
    ));
    // The annotated result does not constrain an otherwise untyped parameter.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let constrained = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/6180e3453a61.orna"
    ));
    expect_accepted(&constrained);
}

#[test]
fn nested_impl_body_can_read_private_target_fields() {
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/1d8c5b26efdc.orna"
    )));
}

#[test]
fn nested_impl_cannot_construct_other_nominal_private_fields() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/7e5d2bb18a70.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn table_nested_impl_body_can_read_private_row_fields() {
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/a9c68d98dade.orna"
    )));
}

#[test]
fn table_display_and_present_writes_require_type_diagnostics() {
    for (protocol, member) in [
        (
            include_str!("fixtures/inline-v1_review_regressions/34e108c0896d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/dfbb889cf19b.orna"),
        ),
        (
            include_str!("fixtures/inline-v1_review_regressions/43f9b89c0b9d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/4d4c7eee2e28.orna"),
        ),
    ] {
        let source = render_source_fixture(
            include_str!("fixtures/inline-v1_review_regressions/8b311ae59082.orna"),
            &[("protocol", &protocol), ("member", &member)],
        );
        expect_diagnostics(&analyze_main(&source), &[DIAG_TYPE]);
    }
}

#[test]
fn presentation_direct_return_terminates_write_scan() {
    for (protocol, member) in [
        (
            include_str!("fixtures/inline-v1_review_regressions/34e108c0896d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/dfbb889cf19b.orna"),
        ),
        (
            include_str!("fixtures/inline-v1_review_regressions/43f9b89c0b9d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/4d4c7eee2e28.orna"),
        ),
    ] {
        let accepted = render_source_fixture(
            include_str!("fixtures/inline-v1_review_regressions/eca9c0c90cf4.orna"),
            &[("protocol", &protocol), ("member", &member)],
        );
        expect_accepted(&analyze_main(&accepted));

        let rejected = render_source_fixture(
            include_str!("fixtures/inline-v1_review_regressions/dd5e785cd9c3.orna"),
            &[("protocol", &protocol), ("member", &member)],
        );
        expect_diagnostics(&analyze_main(&rejected), &[DIAG_TYPE]);
    }
}

#[test]
fn presentation_nested_control_return_does_not_terminate_write_scan() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/90beb981c84d.orna"
    ));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        include_str!("fixtures/inline-v1_review_regressions/2b18ac9922cb.orna"),
        result.diagnostics
    );
}

#[test]
fn presentation_lambda_block_return_does_not_terminate_write_scan() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/14c61480a6fd.orna"
    ));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        include_str!("fixtures/inline-v1_review_regressions/be4c29c7125d.orna"),
        result.diagnostics
    );
}

#[test]
fn presentation_effect_summaries_propagate_through_helpers() {
    let indirect_write = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/719b99d4ee69.orna"
    ));
    expect_diagnostics(&indirect_write, &[DIAG_TYPE]);

    // The spelling `insert` is not a write when scope resolution identifies a
    // local function with a proven empty effect summary.
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/fb1350657dc3.orna"
    )));

    let imported_write = analyze(&[
        ModuleInput::new(
            "helpers.orna",
            include_str!("fixtures/inline-v1_review_regressions/6533b5e42f42.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/ca8d532ef690.orna"),
        ),
    ]);
    expect_diagnostics(&imported_write, &[DIAG_TYPE]);

    let imported_pure = analyze(&[
        ModuleInput::new(
            "helpers.orna",
            include_str!("fixtures/inline-v1_review_regressions/55a6ba82ea35.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/678ad3a13467.orna"),
        ),
    ]);
    expect_accepted(&imported_pure);

    // Failure is a separate channel from effects. A pure helper may still
    // fail (here through finite-list cardinality) and remains valid in a
    // presenter; only its effect set makes the implementation a write.
    for (protocol, member) in [
        (
            include_str!("fixtures/inline-v1_review_regressions/34e108c0896d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/dfbb889cf19b.orna"),
        ),
        (
            include_str!("fixtures/inline-v1_review_regressions/43f9b89c0b9d.orna"),
            include_str!("fixtures/inline-v1_review_regressions/4d4c7eee2e28.orna"),
        ),
    ] {
        let source = render_source_fixture(
            include_str!("fixtures/inline-v1_review_regressions/8e46d6a91cc3.orna"),
            &[("protocol", &protocol), ("member", &member)],
        );
        expect_accepted(&analyze_main(&source));
    }
}

#[test]
fn omitted_nested_impl_annotations_are_checked_contextually() {
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/65e7813109ff.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/f3331b57e1ee.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/5fe50a9599fe.orna"
        )),
        &[DIAG_TYPE],
    );

    // Same type is not enough for an omitted argument: the protocol's
    // literal default is part of the member behaviour.
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/9bc527798ab7.orna"
        )),
        &[DIAG_TYPE],
    );

    // With no protocol result annotation, the implementation's result is
    // still an effective contextual type for its body.
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/4bc6b3f4dd8e.orna"
        )),
        &[DIAG_TYPE],
    );
}

#[test]
fn differing_nonliteral_protocol_defaults_fail_closed() {
    let rejected = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/b6bc3d9e7920.orna"
    ));
    // The implementation may happen to compute the same value, but the
    // validator has no canonical evaluator. Different ASTs are rejected.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let identical = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/810f32f422a3.orna"
    ));
    expect_accepted(&identical);
}

#[test]
fn transparent_aliases_resolve_for_protocol_signatures_without_erasing_nominals() {
    let accepted = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/0e89dd6f5d98.orna"
    ));
    expect_accepted(&accepted);

    let rejected = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/348aefe7bcaf.orna"
    ));
    // Transparent aliases may resolve to Int; a nominal Token must remain
    // distinct from Int.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);
}

#[test]
fn local_protocol_aliases_resolve_to_the_declared_protocol_surface() {
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/156f0678934e.orna"
    )));

    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/2d487a79bb7f.orna"
        )),
        &[DIAG_TYPE],
    );
}

#[test]
fn unsupported_generic_protocol_instantiations_fail_closed() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/41f8c8638d3a.orna"),
        include_str!("fixtures/inline-v1_review_regressions/30b84d807343.orna"),
        include_str!("fixtures/inline-v1_review_regressions/b0fbbe36aba4.orna"),
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

// ORNA-GENERIC-011 (source/06-expressions.md:181): required generic and
// non-generic members each need exactly one compatible implementation.
#[test]
fn mixed_generic_protocol_members_keep_their_checkable_surface() {
    let accepted = analyze_main(include_str!(
        "fixtures/mixed-generic-protocol-accepted.orna"
    ));
    expect_accepted(&accepted);

    let rejected = analyze_main(include_str!("fixtures/mixed-generic-protocol-missing.orna"));
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let generic_implementation = analyze_main(include_str!(
        "fixtures/generic-implementation-does-not-satisfy.orna"
    ));
    // A generic implementation member cannot satisfy a non-generic required
    // member merely by sharing its name; otherwise substitution is being
    // guessed instead of checked.
    expect_diagnostics(&generic_implementation, &[DIAG_TYPE]);
}

#[test]
fn local_generic_protocol_bounds_accept_a_satisfying_call_and_substitute_result() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/9e313601b80b.orna"
    ));
    expect_accepted(&result);
    let module = result.modules.values().next().expect("generic module");
    assert_eq!(
        module
            .symbols
            .get("accepted")
            .expect("accepted function")
            .ty,
        orna_semantic_v1::Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(orna_semantic_v1::Type::Named("Ranked".into())),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn self_result_keeps_nominal_target_identity() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/3cebe3658152.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "static types are incompatible" })
    );
}

#[test]
fn local_generic_protocol_bounds_reject_a_missing_implementation() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/367a93526490.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "generic type argument does not satisfy its protocol bound"
    }));
}

#[test]
fn generic_protocol_overlap_rejection_is_independent_of_source_order() {
    for implementations in [
        include_str!("fixtures/inline-v1_review_regressions/ea1d3ad994e9.orna"),
        include_str!("fixtures/inline-v1_review_regressions/616ec7d2e809.orna"),
    ] {
        let result = analyze_main(&render_source_fixture(
            include_str!("fixtures/inline-v1_review_regressions/9ee5c163a769.orna"),
            &[("implementations", &implementations)],
        ));
        expect_diagnostics(&result, &[DIAG_TYPE]);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message()
                == include_str!("fixtures/inline-v1_review_regressions/529e5f255207.orna")
        }));
    }
}

#[test]
fn colon_generic_bounds_receive_the_frozen_legacy_diagnostic() {
    let parsed = parse_module_with_file(
        include_str!("fixtures/inline-v1_review_regressions/6d4d7e0d847b.orna"),
        "legacy-bound.orna",
    );
    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "ORNA091-E-BOUND-COLON"
            && diagnostic.message
                == include_str!("fixtures/inline-v1_review_regressions/ffeb6888b210.orna")
    }));
}

#[test]
fn generic_calls_retain_callee_effect_and_failure_summaries() {
    let filesystem = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/be79a57479d0.orna"
    ));
    assert!(filesystem.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "declaration assertion uses forbidden filesystem effect"
    }));
    let reads = filesystem
        .modules
        .values()
        .next()
        .expect("filesystem module")
        .symbols
        .get("reads")
        .expect("generic filesystem function");
    assert!(reads.effects.effects.contains("filesystem"));
    assert!(reads.effects.may_fail);

    let fallible = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/4a6f31b2f192.orna"
    ));
    assert!(fallible.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "assertion has forbidden effects or failure"
    }));
    let maybe = fallible
        .modules
        .values()
        .next()
        .expect("fallible module")
        .symbols
        .get("maybe")
        .expect("generic fallible function");
    assert!(maybe.effects.may_fail);
}

#[test]
fn table_assertion_admits_pure_relation_function_value_and_retains_plan() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/d506f9b39df0.orna"
    ));
    expect_accepted(&result);

    let module = result
        .modules
        .values()
        .next()
        .expect("pure relation assertion module");
    assert!(matches!(
        &module.symbols["valid_notes"].ty,
        Type::Function { parameters, result, .. }
            if parameters == &vec![Type::Relation(Box::new(Type::Named("Note".into())))]
                && result.as_ref() == &Type::Bool
    ));
    assert!(module.symbols["valid_notes"].effects.effects.is_empty());
    assert!(!module.symbols["valid_notes"].effects.may_fail);

    let plans = result.assertions.values().next().expect(include_str!(
        "fixtures/inline-v1_review_regressions/7c322615c8a7.orna"
    ));
    assert_eq!(plans.len(), 3);
    assert!(
        plans
            .iter()
            .all(|plan| plan.owner == AssertionOwner::Table("Note".into()))
    );
    assert!(
        plans
            .iter()
            .all(|plan| plan.effects == EffectSummary::default())
    );
}

#[test]
fn table_assertion_rejects_database_read_and_write_function_values() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/8330a5a31e46.orna"),
        include_str!("fixtures/inline-v1_review_regressions/4f29b24b1e51.orna"),
    ] {
        let result = analyze_main(source);
        expect_diagnostics(&result, &[DIAG_ASSERTION_EFFECT]);
    }
}

#[test]
fn table_assertion_preserves_evaluation_failure_for_the_boundary() {
    let result = analyze(&[ModuleInput::new(
        "assertion-failure.orna",
        include_str!("fixtures/assertion-failure/failable_predicate.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let plan = result
        .assertions
        .values()
        .flatten()
        .find(|plan| plan.owner == AssertionOwner::Table("Note".into()))
        .expect(include_str!(
            "fixtures/inline-v1_review_regressions/7c322615c8a7.orna"
        ));
    assert!(plan.effects.may_fail);
    assert!(plan.effects.effects.is_empty());
}

#[test]
fn table_assertion_rejects_explicit_relation_read_for_pure_predicate() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/9dedb83b9c03.orna"
    ));
    expect_diagnostics(&result, &[DIAG_ASSERTION, DIAG_ASSERTION_EFFECT]);
}

#[test]
fn imported_qualified_protocol_members_remain_a_documented_residual() {
    let result = analyze(&[
        ModuleInput::new(
            "traits.orna",
            include_str!("fixtures/inline-v1_review_regressions/756be8c646b4.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/139e0286a537.orna"),
        ),
    ]);
    // The resolved scope retains module/header symbols but not the imported
    // protocol's member AST or stable protocol identity. These forms therefore
    // fail closed; this is not a conformance proof for ORNA-GENERIC-011, and
    // imported-member conformance remains a residual until that data is exposed.
    expect_diagnostics(&result, &[DIAG_TYPE]);

    let named_import = analyze(&[
        ModuleInput::new(
            "traits.orna",
            include_str!("fixtures/inline-v1_review_regressions/756be8c646b4.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/00b32f5a7785.orna"),
        ),
    ]);
    expect_diagnostics(&named_import, &[DIAG_TYPE]);
}

#[test]
fn transparent_aliases_participate_in_protocol_overlap_identity() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/20c36a433818.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn refined_aliases_resolve_transparent_record_shape_for_private_self_access() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/865343a68f0d.orna"
    ));
    // Box remains nominal; only its transparent record representation is used
    // for the nested implementation's private self.field access.
    expect_accepted(&result);
}

#[test]
fn unparameterized_from_implementation_fails_closed() {
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/1b866ec5526b.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/4796540a1383.orna"
    )));
}

#[test]
fn from_implementation_requires_valid_source_and_from_member() {
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/a7ec6f4b2c9b.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/bed43f70bf1b.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/49dbfb01fc6b.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/4796540a1383.orna"
    )));
}

#[test]
fn nominal_from_requires_a_nominal_constructor_result() {
    expect_diagnostics(
        &analyze_main(include_str!(
            "fixtures/inline-v1_review_regressions/0c1af74856ec.orna"
        )),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/964a0d136165.orna"
    )));

    // Tables retain their current row construction form: the AST has no
    // nominal table constructor, so a structural row remains supported.
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/26c303bb06d5.orna"
    )));
}

#[test]
fn imported_nested_from_metadata_resolves_qualified_targets_and_effects() {
    let result = analyze(&[
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/imported_nested_conversion/main.orna"),
        ),
        ModuleInput::new(
            "target.orna",
            include_str!("fixtures/imported_nested_conversion/target.orna"),
        ),
        ModuleInput::new(
            "first.orna",
            include_str!("fixtures/imported_nested_conversion/first.orna"),
        ),
    ]);
    expect_accepted(&result);
    let main = result.modules.get(&Namespace(vec![])).expect("main module");
    let convert = main.symbols.get("convert").expect("conversion function");
    assert!(convert.effects.effects.contains("database write"));
    assert!(convert.effects.may_fail);
}

#[test]
fn private_nominal_members_are_not_available_to_unrelated_local_functions() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/9e19a6ab5df8.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn nominal_from_requires_inferred_target_value_even_when_spelling_matches() {
    let result = analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/6e5c1b5301b7.orna"
    ));
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn refined_targets_validate_nested_members_and_overlap() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/4c2c29754269.orna"),
        include_str!("fixtures/inline-v1_review_regressions/2d27dc426ea9.orna"),
        include_str!("fixtures/inline-v1_review_regressions/f64e15264a21.orna"),
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn nested_impl_member_names_and_parameter_counts_are_checked() {
    for source in [
        include_str!("fixtures/inline-v1_review_regressions/dc7db64c28c5.orna"),
        include_str!("fixtures/inline-v1_review_regressions/7f6ae962a292.orna"),
        include_str!("fixtures/inline-v1_review_regressions/ee1c7a5d4818.orna"),
        include_str!("fixtures/inline-v1_review_regressions/025ae42708e1.orna"),
        include_str!("fixtures/inline-v1_review_regressions/2418bbfc2291.orna"),
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn nested_impl_static_properties_are_checked() {
    expect_accepted(&analyze_main(include_str!(
        "fixtures/inline-v1_review_regressions/06176dbdeaa9.orna"
    )));

    for source in [
        include_str!("fixtures/inline-v1_review_regressions/de9299e2249f.orna"),
        include_str!("fixtures/inline-v1_review_regressions/7174b0a6ca53.orna"),
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn historical_snapshot_projects_nested_authority_module_roots() {
    let exports = BTreeMap::from([(
        "read".into(),
        Symbol {
            kind: SymbolKind::Function,
            ty: Type::Function {
                parameters: Vec::new(),
                parameter_names: Some(Vec::new()),
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::Int),
            },
            public: true,
            effects: EffectSummary::default(),
            generic_parameters: Vec::new(),
            enum_variants: BTreeSet::new(),
            table_schema: None,
        },
    )]);
    let historical = ModuleHeader {
        namespace: Namespace(vec!["legacy".into(), "pkg".into()]),
        symbols: exports.clone(),
        exports,
        generic_functions: BTreeMap::new(),
        prelude_exports: BTreeSet::new(),
        implicit: false,
    };
    let catalogue = Catalogue::empty().with_historical_modules([historical]);
    let result = orna_semantic_v1::analyze_with_catalogue(
        &[ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-v1_review_regressions/6818c2ac5862.orna"),
        )],
        &catalogue,
    );
    expect_accepted(&result);
}
