//! The `core.money` engine natives over lowered source (hir-host.md B6): a `Money<GBP>` value is built by its constructor,
//! its amount is read by `.amount`, and its arithmetic runs through the engine natives. The currency id is the id the
//! host gives the currency's module-qualified name, and it is stable across two activations.

use orna_exec::{ExecError, ExecLimits, Run, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const GBP: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\npub fn gbp(amount: Decimal): Money<GBP> = Money<GBP>(amount);\n";

fn lower_typed(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let mut program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn user_diagnostics(program: &Program) -> Vec<String> {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .map(|(_, diagnostic)| format!("{} {}", diagnostic.code, diagnostic.message))
        .collect()
}

fn lower_run(source: &str) -> (Program, DefId) {
    let program = lower_typed(&format!("{GBP}{source}"));
    let diagnostics = user_diagnostics(&program);
    assert!(
        diagnostics.is_empty(),
        "the user program must type clean: {diagnostics:?}"
    );
    let run_def: DefId = program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`");
    (program, run_def)
}

fn run_with(source: &str, limits: ExecLimits) -> Run {
    let (program, run_def) = lower_run(source);
    run_entry(&program, run_def, limits)
}

fn run(source: &str) -> Result<Value, ExecError> {
    run_with(source, LIMITS).result
}

fn money_parts(value: Value) -> ([u8; 16], Value) {
    match value {
        Value::Money { amount, currency } => (currency, Value::Decimal(amount)),
        other => panic!("expected a Money value, got {other:?}"),
    }
}

#[test]
fn a_sum_of_two_same_currency_amounts_is_their_exact_total() {
    let sum = run("pub fn run(): Money<GBP> = gbp(12.34) + gbp(1.00);\n").expect("sum runs");
    let total = run("pub fn run(): Money<GBP> = gbp(13.34);\n").expect("literal runs");
    assert_eq!(money_parts(sum), money_parts(total));
}

#[test]
fn the_amount_field_reads_the_exact_decimal() {
    let value = run("pub fn run(): Decimal = (gbp(12.34)).amount;\n").expect("amount runs");
    let literal = run("pub fn run(): Decimal = 12.34;\n").expect("literal runs");
    assert_eq!(value, literal);
}

#[test]
fn negate_and_subtract_keep_the_currency() {
    let negated = run("pub fn run(): Money<GBP> = -gbp(3.00);\n").expect("negate runs");
    let minus_three = run("pub fn run(): Money<GBP> = gbp(0.00) - gbp(3.00);\n").expect("sub runs");
    assert_eq!(money_parts(negated), money_parts(minus_three));
}

#[test]
fn the_currency_id_is_the_host_id_of_the_qualified_name_and_stable_across_activations() {
    let first = run("pub fn run(): Money<GBP> = gbp(1.00);\n").expect("first activation");
    let second = run("pub fn run(): Money<GBP> = gbp(1.00);\n").expect("second activation");
    let (first_currency, _) = money_parts(first);
    let (second_currency, _) = money_parts(second);
    assert_eq!(first_currency, second_currency);
    assert_eq!(first_currency, orna_value::enum_type_id("main.GBP"));
}

#[test]
fn ordering_compares_the_exact_amounts_of_one_currency() {
    let less = run("pub fn run(): Bool = gbp(1.00) < gbp(2.00);\n").expect("less runs");
    let greater = run("pub fn run(): Bool = gbp(2.00) < gbp(1.00);\n").expect("greater runs");
    assert_eq!(less, Value::Bool(true));
    assert_eq!(greater, Value::Bool(false));
}

#[test]
fn amounts_written_at_different_scales_are_equal() {
    // Amounts are normalised decimals, so `1.00` and `1.0` are one amount and compare equal in both directions.
    let forward = run("pub fn run(): Bool = gbp(1.00) == gbp(1.0);\n").expect("forward runs");
    let backward = run("pub fn run(): Bool = gbp(1.0) == gbp(1.00);\n").expect("backward runs");
    let unequal = run("pub fn run(): Bool = gbp(1.00) != gbp(1.0);\n").expect("unequal runs");
    assert_eq!(forward, Value::Bool(true));
    assert_eq!(backward, Value::Bool(true));
    assert_eq!(unequal, Value::Bool(false));
}

#[test]
fn different_amounts_in_one_currency_are_not_equal() {
    let equal = run("pub fn run(): Bool = gbp(1.00) == gbp(2.00);\n").expect("equal runs");
    let unequal = run("pub fn run(): Bool = gbp(1.00) != gbp(2.00);\n").expect("unequal runs");
    assert_eq!(equal, Value::Bool(false));
    assert_eq!(unequal, Value::Bool(true));
}

#[test]
fn scaling_by_a_decimal_is_exact() {
    let scaled = run("pub fn run(): Money<GBP> = gbp(12.34) * 2.5;\n").expect("scale runs");
    let literal = run("pub fn run(): Money<GBP> = gbp(30.85);\n").expect("literal runs");
    assert_eq!(money_parts(scaled), money_parts(literal));
}

#[test]
fn dividing_by_a_decimal_is_exact_or_a_named_error() {
    let halved = run("pub fn run(): Money<GBP> = gbp(10.00) / 4.00;\n").expect("divide runs");
    let literal = run("pub fn run(): Money<GBP> = gbp(2.50);\n").expect("literal runs");
    assert_eq!(money_parts(halved), money_parts(literal));
    let inexact = run("pub fn run(): Money<GBP> = gbp(10.00) / 3.00;\n");
    assert_eq!(inexact, Err(ExecError::Code("InexactDivision")));
    let zero = run("pub fn run(): Money<GBP> = gbp(10.00) / 0.00;\n");
    assert_eq!(zero, Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO")));
}

#[test]
fn a_currency_suffix_literal_is_the_constructor_of_its_exact_amount() {
    let suffixed = run("pub fn run(): Money<GBP> = 12.34.GBP;\n").expect("suffix runs");
    let constructed = run("pub fn run(): Money<GBP> = gbp(12.34);\n").expect("constructor runs");
    assert_eq!(money_parts(suffixed), money_parts(constructed));
}

#[test]
fn the_step_budget_admits_exactly_the_steps_the_run_charges() {
    let source = "pub fn run(): Money<GBP> = gbp(12.34) + gbp(1.00);\n";
    let measured = run_with(source, LIMITS);
    assert!(measured.result.is_ok(), "{:?}", measured.result);
    let steps = measured.steps;
    let exact = ExecLimits {
        max_steps: steps,
        ..LIMITS
    };
    assert!(
        run_with(source, exact).result.is_ok(),
        "{steps} steps admit the run"
    );
    let short = ExecLimits {
        max_steps: steps - 1,
        ..LIMITS
    };
    assert_eq!(
        run_with(source, short).result,
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
