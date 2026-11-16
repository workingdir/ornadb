//! Effects follow the resolved identity of a callee, not the spelling of its
//! path. A `sys` import alias denotes the reserved namespace it was bound to,
//! so calls through it are classified exactly like the spelled operation
//! (ORNA-IMPORT-002, ORNA-SYS-039, ORNA-ASSERT-011).

use orna_check::{
    Analysis, Catalogue, DIAG_ASSERTION_EFFECT, EffectSummary, ModuleInput, Namespace,
    analyze_with_catalogue,
};

fn analyze(modules: &[ModuleInput]) -> Analysis {
    analyze_with_catalogue(modules, &Catalogue::authoritative_core())
}

fn assertion_effect_count(analysis: &Analysis) -> usize {
    analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DIAG_ASSERTION_EFFECT)
        .count()
}

/// Renders every diagnostic as `code: message`, so a failing assertion shows
/// its cause (the `Debug` form redacts both fields).
fn diagnostic_lines(analysis: &Analysis) -> String {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn export_effects<'a>(analysis: &'a Analysis, module: &str, name: &str) -> &'a EffectSummary {
    &analysis
        .modules
        .get(&Namespace(vec![module.to_owned()]))
        .unwrap_or_else(|| panic!("missing module {module:?}"))
        .exports
        .get(name)
        .unwrap_or_else(|| panic!("missing export {name:?}"))
        .effects
}

#[test]
fn table_assertion_through_spelled_sys_operation_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"pub table User(id: Uuid) { name: Str, assert sys.admin.commit("m") == sys.admin.commit("m"); }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_std_module_alias_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std.net.http as http;
pub table User(id: Uuid) { name: Str, assert http.get("https://example.com") == "ok"; }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_named_std_import_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std.net.http.{get};
pub table User(id: Uuid) { name: Str, assert get("https://example.com") == "ok"; }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_sys_alias_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use sys.admin as ops;
pub table User(id: Uuid) { name: Str, assert ops.commit("m") == ops.commit("m"); }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "a sys alias must classify its calls as the spelled sys operation:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn helper_through_sys_alias_carries_its_effect() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use sys.admin as ops;
pub fn snapshot(): sys.CommitRef = ops.commit("m");"#,
    )]);

    assert!(
        export_effects(&analysis, "consumer", "snapshot").is_effectful(),
        "a helper calling through a sys alias must export the operation's effect:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_helper_using_sys_alias_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use sys.admin as ops;
pub fn same_commit(): Bool = ops.commit("m") == ops.commit("m");
pub table User(id: Uuid) { name: Str, assert same_commit(); }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}
