//! Focused, executable evidence for the highest-risk uncovered presentation/page claims.
//!
//! Reference anchors are verified against the frozen source:
//! - ORNA-PAGE-001: source/14-pages.md:11 (ordinary page values returned by functions).
//! - ORNA-PAGE-002: source/14-pages.md:13 (widgets/layouts compose as ordinary values).
//! - ORNA-PRES-002: source/13-presentation.md:25 (Inspect structure and secret redaction).
//! - ORNA-PRES-008: source/13-presentation.md:99 (Display/Present are read-only).
//!
//! Evidence grade: the new page fixture is a direct semantic witness (A for its
//! exact fixture, not the entire page surface); the effect tests are direct
//! rejection witnesses (A for attempted writes); the secret test proves that
//! secrets cannot be Displayed, not that every Inspect renderer redacts secrets.
//! The broader audit leaves PRES-001/003/004/005/006/007/009/010 and LIVE/WIRE
//! behavior to their owning codec, Inspect, evaluator, live-host and client
//! suites; this file does not promote their current evidence to complete.

use orna_semantic_v1::{
    Catalogue, DIAG_TYPE, ModuleInput, Type, analyze, analyze_with_catalogue,
};

fn effect_fixture(protocol: &str, member: &str) -> String {
    include_str!("fixtures/inline-v1_review_regressions/8b311ae59082.orna")
        .replace("{protocol}", protocol)
        .replace("{member}", member)
}

#[test]
fn page_is_an_ordinary_function_result() {
    let source = include_str!("fixtures/presentation-pages-y36t/page-composition.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("records.orna", source)],
        &Catalogue::authoritative_core(),
    );
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result.modules.values().next().expect("page module");
    assert!(matches!(
        module.symbols.get("records_page").map(|symbol| &symbol.ty),
        Some(Type::Function { result, .. }) if result.as_ref() == &Type::Named("std.UI".into())
    ));
}

#[test]
fn page_layout_values_compose_through_ordinary_functions() {
    let source = include_str!("fixtures/presentation-pages-y36t/page-composition.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("records.orna", source)],
        &Catalogue::authoritative_core(),
    );
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result.modules.values().next().expect("page module");
    for name in ["heading", "heading_row", "body", "records_page"] {
        assert!(
            module.symbols.contains_key(name),
            "composed page helper {name} remains an ordinary function"
        );
    }
}

#[test]
fn display_implementation_write_is_rejected() {
    let source = effect_fixture(
        include_str!("fixtures/inline-v1_review_regressions/34e108c0896d.orna"),
        include_str!("fixtures/inline-v1_review_regressions/dfbb889cf19b.orna"),
    );
    let result = analyze(&[ModuleInput::new("display-write.orna", &source)]);
    assert!(
        result.diagnostics.iter().any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "Display write must fail as a type/effect diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn present_implementation_write_is_rejected() {
    let source = effect_fixture(
        include_str!("fixtures/inline-v1_review_regressions/43f9b89c0b9d.orna"),
        include_str!("fixtures/inline-v1_review_regressions/4d4c7eee2e28.orna"),
    );
    let result = analyze(&[ModuleInput::new("present-write.orna", &source)]);
    assert!(
        result.diagnostics.iter().any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "Present write must fail as a type/effect diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn secret_value_cannot_be_exposed_through_display() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret-display.orna",
            include_str!("fixtures/inline-semantic_graph/bf3665cb2f18.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "secret values cannot be displayed"
        }),
        "secret display must be rejected without treating that as proof of every Inspect renderer: {:#?}",
        result.diagnostics
    );
}
