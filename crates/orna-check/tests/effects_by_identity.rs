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

/// A secret value as the standard library produces it. Presenting it is
/// rejected by the secret-presentation boundary (ORNA-PRES-002).
const SECRET_OPEN: &str = r#"std.secret.open(std.secret.ref("fixture.credential"), as: Str)"#;

fn secret_presentation_count(analysis: &Analysis) -> usize {
    analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message() == "secret values cannot be presented")
        .count()
}

#[test]
fn secret_presentation_through_std_module_alias_is_rejected() {
    let source =
        format!("use std.terminal as t;\npub fn shown() = t.present_table({SECRET_OPEN});");
    let analysis = analyze(&[ModuleInput::new("consumer.orna", &source)]);

    assert_eq!(
        secret_presentation_count(&analysis),
        1,
        "a std module alias must present as the spelled std path:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn secret_presentation_through_std_root_alias_is_rejected() {
    let source =
        format!("use std as core;\npub fn shown() = core.terminal.present_table({SECRET_OPEN});");
    let analysis = analyze(&[ModuleInput::new("consumer.orna", &source)]);

    assert_eq!(
        secret_presentation_count(&analysis),
        1,
        "a std root alias must present as the spelled std path:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn secret_presentation_through_named_std_import_is_rejected() {
    let source = format!(
        "use std.terminal.{{present_table}};\npub fn shown() = present_table({SECRET_OPEN});"
    );
    let analysis = analyze(&[ModuleInput::new("consumer.orna", &source)]);

    assert_eq!(
        secret_presentation_count(&analysis),
        1,
        "a named std import must present as the spelled std path:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn secret_presentation_through_spelled_std_path_is_rejected() {
    let source = format!("pub fn shown() = std.terminal.present_table({SECRET_OPEN});");
    let analysis = analyze(&[ModuleInput::new("consumer.orna", &source)]);

    assert_eq!(
        secret_presentation_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_std_root_alias_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std as core;
pub table User(id: Uuid) { name: Str, assert core.net.http.get("https://example.com") == "ok"; }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_std_module_alias_to_secret_access_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std.secret as s;
pub table User(id: Uuid) { name: Str, assert s.open(s.ref("fixture.credential"), as: Str) == "x"; }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn helper_through_std_root_alias_carries_its_effect() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std as core;
pub fn fetch(): Str = core.net.http.get("https://example.com");"#,
    )]);

    assert!(
        export_effects(&analysis, "consumer", "fetch").is_effectful(),
        "a helper calling through a std root alias must export the operation's effect:\n{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_through_helper_using_std_module_alias_is_rejected() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"use std.net.http as http;
pub fn fetch(): Str = http.get("https://example.com");
pub table User(id: Uuid) { name: Str, assert fetch() == "ok"; }"#,
    )]);

    assert_eq!(
        assertion_effect_count(&analysis),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn symbol_rename_import_is_not_an_alias_of_a_std_symbol() {
    // `use a.b.c as x` binds a namespace only (ORNA-IMPORT-002); the symbol
    // `present_table` is not a module, so the rename is rejected rather than
    // silently aliasing the operation.
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        format!("use std.terminal.present_table as p;\npub fn shown() = p({SECRET_OPEN});"),
    )]);

    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == orna_check::DIAG_IMPORT),
        "a rename of a symbol must not be accepted silently:\n{}",
        diagnostic_lines(&analysis)
    );
}
