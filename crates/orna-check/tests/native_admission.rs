use orna_check::{DIAG_NATIVE_ADMISSION, ModuleInput, analyze};

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

/// D12: `native fn` is admitted only in the engine-embedded core and sys modules; user source gets ORNA-NATIVE-004.
#[test]
fn native_declaration_in_user_module_is_rejected() {
    let result = analyze(&[module(
        "lib.orna",
        "pub native fn clock(): Int effect clock;",
    )]);
    assert_eq!(codes(&result), vec![DIAG_NATIVE_ADMISSION]);
}

/// Every native declaration in user source is reported, one diagnostic each.
#[test]
fn each_native_declaration_is_reported() {
    let result = analyze(&[module(
        "lib.orna",
        "pub native fn a(): Int effect none;\npub native fn b(): Int effect none;",
    )]);
    assert_eq!(
        codes(&result),
        vec![DIAG_NATIVE_ADMISSION, DIAG_NATIVE_ADMISSION]
    );
}

/// Ordinary functions are unaffected by the native admission rule.
#[test]
fn ordinary_function_has_no_native_admission_diagnostic() {
    let result = analyze(&[module("lib.orna", "pub fn clock(): Int = 1;")]);
    assert!(!codes(&result).contains(&DIAG_NATIVE_ADMISSION));
}
