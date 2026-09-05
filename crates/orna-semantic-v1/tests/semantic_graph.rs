use orna_semantic_v1::{
    Catalogue, DIAG_AMBIGUOUS, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT, DIAG_ASSERTION_SCOPE,
    DIAG_RESERVED, DIAG_UNRESOLVED, ModuleInput, analyze, analyze_with_catalogue,
};

fn has(result: &orna_semantic_v1::Analysis, code: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

#[test]
fn unicode_nfkc_casefold_sibling_collision_is_rejected() {
    let result = analyze(&[
        ModuleInput::new("ff/left.orna", ""),
        ModuleInput::new("ﬀ/right.orna", ""),
    ]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "ORNA-S002-NAMESPACE")
    );
}

#[test]
fn graph_resolution_keeps_explicit_imports_over_globs_and_rejects_module_assertion_execution() {
    let result = analyze(&[
        ModuleInput::new("left.orna", "pub fn pick(): Int = 1;"),
        ModuleInput::new("right.orna", "pub fn pick(): Int = 2;"),
        ModuleInput::new(
            "consumer.orna",
            "use sys as system; use left.{pick}; use right.*; fn chosen() = pick(); assert true;",
        ),
    ]);

    assert!(!has(&result, DIAG_AMBIGUOUS));
    assert!(has(&result, DIAG_ASSERTION_SCOPE));
}

#[test]
fn table_assertion_rejects_authoritative_std_net_effect() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "pub table User(id: Uuid) { name: Str, assert std.net.http.get(\"https://example.com\") == \"ok\"; }",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_ASSERTION_EFFECT));
}

#[test]
fn table_assertion_rejects_owner_type_mismatch() {
    let result = analyze(&[ModuleInput::new(
        "books.orna",
        "pub table User(id: Uuid) { name: Str, assert >= 0; }",
    )]);

    assert!(has(&result, DIAG_ASSERTION));
}

#[test]
fn authoritative_core_catalogue_resolves_prelude_types_and_common_functions() {
    let profile = Catalogue::authoritative_core();
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "use std as _; use std.math.{increment, is_zero}; use std.ui.{text}; use std.json.{encode}; fn next(value: INTEGER): INTEGER = increment(value); fn zero(): BOOLEAN = is_zero(0); fn view(): UI = text(\"hello\"); fn bytes(value: JsonValue): ByteStream = encode(value);",
        )],
        &profile,
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn catalogue_is_closed_world_and_diagnostics_remain_redacted_and_stable() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret.orna",
            "use std as _; fn f() = definitely_not_in_the_catalogue;",
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_UNRESOLVED));
    let json = serde_json::to_string(&result.diagnostics).unwrap();
    assert!(!json.contains("secret.orna"));
    assert!(!json.contains("definitely_not_in_the_catalogue"));
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.code())
            .collect::<Vec<_>>(),
        vec![DIAG_UNRESOLVED]
    );
}

#[test]
fn catalogue_does_not_relax_reserved_source_roots() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new("std/main.orna", "")],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_RESERVED));
}
