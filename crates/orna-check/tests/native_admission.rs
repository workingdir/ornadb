use orna_check::{DIAG_DUPLICATE, ModuleInput, analyze};

fn module(path: &str, source: &str) -> ModuleInput {
    ModuleInput::new(path, source)
}

fn codes(result: &orna_check::Analysis) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code())
        .collect()
}

/// ORNA-NATIVE-004 (amended 2026-11-10): a `native fn` in project source is admitted. Its binding is decided at link,
/// not by the checker, so admission reports nothing.
#[test]
fn native_declaration_in_user_module_is_admitted() {
    let result = analyze(&[module(
        "lib.orna",
        "pub native fn clock(): Int effect clock;",
    )]);
    assert!(codes(&result).is_empty(), "{:?}", codes(&result));
}

/// Natives in several project modules are each admitted; each module owns its own binding keys.
#[test]
fn natives_in_several_user_modules_are_admitted() {
    let result = analyze(&[
        module("a.orna", "pub native fn a(): Int effect none;"),
        module("b.orna", "pub native fn a(): Int effect none;"),
    ]);
    assert!(codes(&result).is_empty(), "{:?}", codes(&result));
}

/// ORNA-NATIVE-005: two native declarations with one binding key in one module are rejected as duplicates.
#[test]
fn duplicate_native_in_one_module_is_rejected() {
    let result = analyze(&[module(
        "lib.orna",
        "pub native fn a(): Int effect none;\npub native fn a(): Int effect none;",
    )]);
    assert!(
        codes(&result).contains(&DIAG_DUPLICATE),
        "{:?}",
        codes(&result)
    );
}

/// Ordinary functions are unaffected by native admission.
#[test]
fn ordinary_function_is_admitted_without_diagnostics() {
    let result = analyze(&[module("lib.orna", "pub fn clock(): Int = 1;")]);
    assert!(codes(&result).is_empty(), "{:?}", codes(&result));
}
