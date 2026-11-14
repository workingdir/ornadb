//! Unit suffix precedence (units.md name resolution): an embedded core unit is the lowest precedence, so a unit a
//! user module declares with the same name wins over it; two user declarations of one name stay ambiguous.

use orna_hir::{
    ModulePath, Name, Program, SourceId, SourceUnit,
    diag::AMBIGUOUS,
    lower,
    typeck::typeck,
};
use orna_syntax::parse_module_with_file;

/// Lowers each `(path, source)` as its own module and runs the type checker over the program.
fn checked(modules: &[(&str, &str)]) -> Program {
    let trees: Vec<_> = modules
        .iter()
        .map(|(_, source)| {
            let parsed = parse_module_with_file(source, "test.orna");
            assert!(parsed.is_ok(), "test source must parse: {:?}", parsed.diagnostics);
            parsed.value
        })
        .collect();
    let units: Vec<SourceUnit<'_>> = modules
        .iter()
        .zip(&trees)
        .enumerate()
        .map(|(index, ((path, _), tree))| SourceUnit {
            source: SourceId::new(index as u32),
            tree,
            path: ModulePath::new([Name::new(path)]),
        })
        .collect();
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
fn a_user_unit_shadows_the_core_unit_of_the_same_name() {
    // `m` is declared by core.units and by the `lib` module. `main` has no own `m`, so both candidates are
    // program-wide; the core one must drop out and `1.m` must not be ambiguous.
    let lib = "pub dim Length;\npub unit m : Length base;\n";
    let main = "pub fn run() = 1.m;\n";
    let program = checked(&[("lib", lib), ("main", main)]);
    assert!(!codes(&program).contains(&AMBIGUOUS), "{:?}", codes(&program));
}

#[test]
fn two_user_modules_declaring_one_unit_stay_ambiguous() {
    let first = "pub dim Length;\npub unit q : Length base;\n";
    let second = "pub dim Length;\npub unit q : Length base;\n";
    let main = "pub fn run() = 1.q;\n";
    let program = checked(&[("first", first), ("second", second), ("main", main)]);
    assert!(codes(&program).contains(&AMBIGUOUS), "{:?}", codes(&program));
}
