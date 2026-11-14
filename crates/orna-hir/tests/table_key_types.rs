//! A table primary-key component may not be `Float` (ORNA-KEY-003, ORNA-FLOAT-003), directly or as a tuple element.
//! The rejection is `E3003`, the code the checker publishes for the same rule.

use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, diag::PRIMARY_KEY_FLOAT, lower};
use orna_syntax::parse_module_with_file;

/// Lowers `source` as one module.
fn lowered(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree = parsed.value;
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    lower(&units)
}

/// Every diagnostic code the program reports.
fn codes(program: &Program) -> Vec<&str> {
    program
        .modules
        .iter()
        .flat_map(|module| module.diagnostics.iter())
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn a_float_primary_key_is_e3003() {
    let program = lowered("pub table Bad(value: Float) { text: Str, }\n");
    assert_eq!(codes(&program), vec![PRIMARY_KEY_FLOAT]);
}

#[test]
fn a_float_element_of_a_tuple_key_is_e3003() {
    let program = lowered("pub table Bad(value: (Int, Float)) { text: Str, }\n");
    assert_eq!(codes(&program), vec![PRIMARY_KEY_FLOAT]);
}

#[test]
fn an_int_or_str_key_is_clean() {
    let program = lowered("pub table Good(id: Int, label: Str) { text: Str, }\n");
    assert!(codes(&program).is_empty(), "{:?}", codes(&program));
}
