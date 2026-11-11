//! Unit diagnostics of the typing kernel (units.md ORNA-UNIT-010, -013, -016, -019): a family whose variants mix
//! dimensions, a power with a non-literal exponent, a power beyond the exponent budget, and a conversion with no exact
//! decimal form.

use orna_hir::{
    ModulePath, Name, Program, SourceId, SourceUnit,
    diag::{UNIT_DIM, UNIT_INEXACT, UNIT_LIMIT, UNIT_POW},
    lower,
    typeck::typeck,
};
use orna_syntax::parse_module_with_file;

const LENGTH_AND_TIME: &str = "pub enum Length {\n    metre,\n    kilometre = 1000 metre,\n    impl Unit {}\n}\n\
pub enum Duration {\n    second,\n    minute = 60 second,\n    impl Unit {}\n}\n";

const SPEED: &str = "pub enum Speed {\n    kpm = Length.kilometre / Duration.minute,\n\
mps = Length.metre / Duration.second,\n    impl Unit {}\n}\n";

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
fn a_family_whose_variants_mix_dimensions_is_unit_dim() {
    let source = format!(
        "{LENGTH_AND_TIME}pub enum Mixed {{\n    m = Length.metre,\n    s = Duration.second,\n    impl Unit {{}}\n}}\n"
    );
    let program = checked(&source);
    assert!(codes(&program).contains(&UNIT_DIM), "{:?}", codes(&program));
}

#[test]
fn a_quantity_power_with_a_non_literal_exponent_is_unit_pow() {
    let source = format!("{LENGTH_AND_TIME}pub fn run(n: Int) = 2.metre ^ n;\n");
    let program = checked(&source);
    assert!(codes(&program).contains(&UNIT_POW), "{:?}", codes(&program));
}

#[test]
fn a_quantity_power_beyond_the_exponent_budget_is_unit_limit() {
    let source = format!("{LENGTH_AND_TIME}pub fn run() = 2.metre ^ 40;\n");
    let program = checked(&source);
    assert!(
        codes(&program).contains(&UNIT_LIMIT),
        "{:?}",
        codes(&program)
    );
}

#[test]
fn a_conversion_without_an_exact_decimal_form_is_unit_inexact() {
    // `kpm` has scale 1000 / 60 = 50/3, whose denominator has a prime factor other than 2 and 5, so converting a
    // value of `kpm` to the finer `mps` (scale 1) has no finite decimal form.
    let source = format!("{LENGTH_AND_TIME}{SPEED}pub fn run() = 2.kpm + 1.mps;\n");
    let program = checked(&source);
    assert!(
        codes(&program).contains(&UNIT_INEXACT),
        "{:?}",
        codes(&program)
    );
}

#[test]
fn a_top_level_affine_unit_is_unsupported_not_a_kernel_clause() {
    // units.md 8.5 and C6: an offset and the affine flag are library types, not kernel clauses.
    let source = "pub dim Temperature;\npub unit K : Temperature base;\npub unit C : Temperature = K offset 273.15 affine;\n";
    let program = checked(source);
    assert!(
        codes(&program).contains(&orna_hir::diag::UNSUPPORTED),
        "an affine unit must report UNSUPPORTED: {:?}",
        codes(&program)
    );
}
