use orna_semantic_v1::{DIAG_AMBIGUOUS, DIAG_ASSERTION_SCOPE, ModuleInput, analyze};

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
