//! `Rate<C, U>` and the money-rate dimensional algebra (spec ORNA-MONEY-003; units.md section 8.3; hir-host.md B1).
//!
//! `Money<GBP> / kWh` is a `Rate<GBP, kWh>`, and `Decimal<kWh> * Rate<GBP, kWh>` is a `Money<GBP>`. Both operand orders
//! of the product are the same exact amount, because the rate's impl is found for a quantity on either side.

use orna_hir::diag::{INFER_CONFLICT, MONEY_BINARY_FLOAT, UNIT_DIM, UNIT_INEXACT};
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

/// The units, currency and tariff of the money-rate fixture, plus the `Wh` unit and the time unit `s` that the
/// mixed-unit tests reach. Each test appends the functions it checks to this source.
const RATE_FIXTURE: &str = "\
pub enum Energy {
    J,
    Wh = 3600 J,
    kWh = 3600000 J,
    impl Unit {}
}
pub enum Time {
    s,
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
";

/// [`RATE_FIXTURE`] followed by `functions`.
fn rate_source(functions: &str) -> String {
    format!("{RATE_FIXTURE}{functions}")
}

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

/// The codes of the diagnostics of the user module `main`.
fn user_codes(program: &Program) -> Vec<&str> {
    program
        .modules
        .iter()
        .filter(|module| module.path == ModulePath::new([Name::new("main")]))
        .flat_map(|module| module.diagnostics.iter().map(|diagnostic| diagnostic.code))
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

/// `Int<kWh> * Rate<GBP, kWh>` and `Rate<GBP, kWh> * Int<kWh>` are both `Money<GBP>`: the quantity already has the
/// rate's unit, so its conversion is the identity ratio (ORNA-UNIT-015).
#[test]
fn an_integer_energy_times_rate_is_money_in_either_order() {
    let source = rate_source(
        "pub fn cost(energy: Int<kWh>, tariff: Tariff): Money<GBP> = energy * tariff.rate;\n\
         pub fn reversed(energy: Int<kWh>, tariff: Tariff): Money<GBP> = tariff.rate * energy;\n",
    );
    let program = lower_one(&source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// `Rate<GBP, kWh> * Decimal<Wh>` is `Money<GBP>` too: the rate on the left, and the `Decimal<Wh>` quantity converts to
/// `kWh` by the same exact factor 1/1000 (ORNA-MONEY-003, ORNA-UNIT-016).
#[test]
fn a_watt_hour_energy_times_rate_is_money_with_the_rate_on_the_left() {
    let source = rate_source(
        "pub fn reversed(energy: Decimal<Wh>, tariff: Tariff): Money<GBP> = tariff.rate * energy;\n",
    );
    let program = lower_one(&source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// `Decimal<Wh>` converts to `kWh` by the factor 1/1000, a finite decimal, so the product is `Money<GBP>`
/// (ORNA-UNIT-016).
#[test]
fn a_watt_hour_energy_converts_exactly_to_the_rate_unit() {
    let source = rate_source(
        "pub fn cost(energy: Decimal<Wh>, tariff: Tariff): Money<GBP> = energy * tariff.rate;\n",
    );
    let program = lower_one(&source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// `Decimal<J>` converts to `kWh` by the factor 1/3600000, which has no finite decimal form, so the product is
/// rejected as `UNIT_INEXACT` (ORNA-UNIT-016).
#[test]
fn a_joule_energy_times_rate_is_rejected_as_inexact() {
    let source =
        rate_source("pub fn cost(energy: Decimal<J>, tariff: Tariff) = energy * tariff.rate;\n");
    let program = lower_one(&source);
    assert!(
        user_codes(&program).contains(&UNIT_INEXACT),
        "{:?}",
        user_diagnostics(&program)
    );
}

/// `Decimal<s>` has the dimension of time, not energy, so no conversion to the rate's unit exists: `UNIT_DIM`.
#[test]
fn a_time_quantity_times_rate_is_a_dimension_mismatch() {
    let source =
        rate_source("pub fn cost(time: Decimal<s>, tariff: Tariff) = time * tariff.rate;\n");
    let program = lower_one(&source);
    assert!(
        user_codes(&program).contains(&UNIT_DIM),
        "{:?}",
        user_diagnostics(&program)
    );
}

/// `Money<GBP> / Decimal<kWh>` is `Rate<GBP, kWh>` (units.md section 8.3), the type the tariff's `Money<GBP> / kWh` declares.
#[test]
fn money_over_energy_is_the_rate_per_unit() {
    let source = rate_source(
        "pub fn per_unit(cost: Money<GBP>, energy: Decimal<kWh>): Money<GBP> / kWh = cost / energy;\n",
    );
    let program = lower_one(&source);
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

/// The quotient `Money<GBP> / Decimal<kWh>` is a rate, not money, so it does not check as `Money<GBP>`.
#[test]
fn money_over_energy_is_not_money() {
    let source = rate_source(
        "pub fn per_unit(cost: Money<GBP>, energy: Decimal<kWh>): Money<GBP> = cost / energy;\n",
    );
    let program = lower_one(&source);
    assert_eq!(
        user_codes(&program),
        vec![INFER_CONFLICT],
        "{:?}",
        user_diagnostics(&program)
    );
}

/// A `Float` quantity never enters the money-rate product (ORNA-MONEY-003), so the operator is `E5103`.
#[test]
fn a_float_energy_times_rate_is_rejected() {
    let source =
        rate_source("pub fn cost(energy: Float<kWh>, tariff: Tariff) = energy * tariff.rate;\n");
    let program = lower_one(&source);
    assert!(
        user_codes(&program).contains(&MONEY_BINARY_FLOAT),
        "{:?}",
        user_diagnostics(&program)
    );
}

/// Two rates have no product: `Rate<C, U> * Rate<C, U>` is not a money-rate product, and is rejected.
#[test]
fn a_rate_times_a_rate_is_rejected() {
    let source = rate_source("pub fn square(tariff: Tariff) = tariff.rate * tariff.rate;\n");
    let program = lower_one(&source);
    assert_eq!(
        user_codes(&program),
        vec![INFER_CONFLICT],
        "{:?}",
        user_diagnostics(&program)
    );
}

/// `Decimal<kWh> / Money<GBP>` has no rate form: only `Money<C> / U` is a rate (units.md section 8.3).
#[test]
fn an_energy_over_money_is_rejected() {
    let source =
        rate_source("pub fn bad(energy: Decimal<kWh>, cost: Money<GBP>) = energy / cost;\n");
    let program = lower_one(&source);
    assert_eq!(
        user_codes(&program),
        vec![INFER_CONFLICT],
        "{:?}",
        user_diagnostics(&program)
    );
}
