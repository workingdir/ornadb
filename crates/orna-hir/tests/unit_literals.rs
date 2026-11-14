//! Unit literals are typed as quantities (units.md section 6, ORNA-UNIT-008): `n.name` with a unit of that name in
//! scope is `Quantity<N, U>`, so the operator rules of the kernel apply to it. A suffix no unit, literal target or
//! built-in postfix names is ORNA-UNIT-UNRESOLVED (ORNA-UNIT-008 step 3). Only numeric literals take a unit suffix.

use orna_hir::{
    ModulePath, Name, Program, SourceId, SourceUnit, diag::UNIT_UNRESOLVED, lower, typeck::typeck,
};
use orna_syntax::parse_module_with_file;

const FAMILIES: &str = "pub enum Length {\n    metre,\n    kilometre = 1000 metre,\n    impl Unit {}\n}\n\
pub enum Mass {\n    kilogram,\n    impl Unit {}\n}\n";

/// Lowers `source` as one module and runs the type checker over it.
fn checked(source: &str) -> Program {
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
    let mut program = lower(&units);
    typeck(&mut program);
    program
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
fn a_literal_of_a_declared_unit_adds_to_a_literal_of_the_same_dimension() {
    let program = checked(&format!("{FAMILIES}pub fn ok() = 5.kilometre + 3.metre;\n"));
    assert!(codes(&program).is_empty(), "{:?}", codes(&program));
}

#[test]
fn a_literal_of_a_declared_unit_subtracts_from_a_literal_of_the_same_dimension() {
    let program = checked(&format!("{FAMILIES}pub fn ok() = 5.kilometre - 3.metre;\n"));
    assert!(codes(&program).is_empty(), "{:?}", codes(&program));
}

#[test]
fn adding_literals_of_different_dimensions_is_e5001() {
    let program = checked(&format!(
        "{FAMILIES}pub fn bad() = 5.kilometre + 4.kilogram;\n"
    ));
    assert!(codes(&program).contains(&"E5001"), "{:?}", codes(&program));
}

#[test]
fn subtracting_literals_of_different_dimensions_is_e5001() {
    let program = checked(&format!(
        "{FAMILIES}pub fn bad() = 5.kilometre - 4.kilogram;\n"
    ));
    assert!(codes(&program).contains(&"E5001"), "{:?}", codes(&program));
}

#[test]
fn an_undeclared_suffix_on_a_numeric_literal_is_unit_unresolved() {
    let program = checked(&format!("{FAMILIES}pub fn bad() = 20.Zzz;\n"));
    assert!(
        codes(&program).contains(&UNIT_UNRESOLVED),
        "{:?}",
        codes(&program)
    );
}

#[test]
fn a_declared_suffix_on_a_numeric_literal_is_not_unit_unresolved() {
    let program = checked(&format!("{FAMILIES}pub fn ok() = 5.metre;\n"));
    assert!(
        !codes(&program).contains(&UNIT_UNRESOLVED),
        "{:?}",
        codes(&program)
    );
}

#[test]
fn a_suffix_on_a_value_is_not_a_unit_suffix() {
    // ORNA-UNIT-008 applies to numeric literals only; a value's unknown member is not a unit diagnostic.
    let program = checked(&format!("{FAMILIES}pub fn bad(x: Int) = x.Zzz;\n"));
    assert!(
        !codes(&program).contains(&UNIT_UNRESOLVED),
        "{:?}",
        codes(&program)
    );
}
