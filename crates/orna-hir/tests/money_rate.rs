//! `Rate<C, U>` and the money-rate dimensional algebra (spec ORNA-MONEY-003; units.md section 8.3; hir-host.md B1).
//!
//! `Money<GBP> / kWh` is a `Rate<GBP, kWh>`, and `Decimal<kWh> * Rate<GBP, kWh>` is a `Money<GBP>`. Both operand orders
//! of the product are the same exact amount, because the rate's impl is found for a quantity on either side.

use orna_hir::typeck::typeck;
use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

/// The corpus case `valid/money-rate`, with the fixture units it reaches through `use fixture.units.*` supplied inline.
const MONEY_RATE: &str = "\
pub enum Energy {
    J,
    kWh = 3600000 J,
    impl Unit {}
}
pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}
pub table Tariff(effective_from: Date) {
    rate: Money<GBP> / kWh,
}

pub fn cost(energy: Decimal<kWh>, tariff: Tariff) =
    energy * tariff.rate;
";

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "money-rate.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn lower_one(source: &str) -> Program {
    let tree = parse(source);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }]);
    let mut program = program;
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "checked program must validate");
    program
}

/// The diagnostics of the user module `main`, as code and message.
fn user_diagnostics(program: &Program) -> Vec<String> {
    program
        .modules
        .iter()
        .filter(|module| module.path == ModulePath::new([Name::new("main")]))
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{} {}", diagnostic.code, diagnostic.message))
        })
        .collect()
}

#[test]
fn the_money_rate_corpus_case_lowers_without_diagnostics() {
    let program = lower_one(MONEY_RATE);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// `Decimal<kWh> * Rate<GBP, kWh>` is `Money<GBP>`, so a function declared to return `Money<GBP>` checks.
#[test]
fn the_energy_times_rate_is_money_in_the_rate_currency() {
    let source = MONEY_RATE.replace(
        "pub fn cost(energy: Decimal<kWh>, tariff: Tariff) =",
        "pub fn cost(energy: Decimal<kWh>, tariff: Tariff): Money<GBP> =",
    );
    let program = lower_one(&source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// `Decimal<m> / s` in type position is the quantity in the combined key `m/s`, and the quotient of two quantities
/// with those keys has that type.
#[test]
fn a_quotient_of_unit_quantities_combines_their_keys() {
    let source = "\
pub enum Length {
    m,
    impl Unit {}
}
pub enum Time {
    s,
    impl Unit {}
}
pub fn speed(distance: Decimal<m>, time: Decimal<s>): Decimal<m> / s =
    distance / time;
";
    let program = lower_one(source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}
