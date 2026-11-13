//! The `core.money` engine natives over lowered source (hir-host.md B6): a `Money<GBP>` value is built by its constructor,
//! its amount is read by `.amount`, and its arithmetic runs through the engine natives. The currency id is the id the
//! host gives the currency's module-qualified name, and it is stable across two activations.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
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

fn run(source: &str) -> Result<Value, ExecError> {
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
    run_entry(&program, run_def, LIMITS).result
}

fn money_parts(value: Value) -> ([u8; 16], Value) {
    match value {
        Value::Money { amount, currency } => (currency, Value::Decimal(amount)),
        other => panic!("expected a Money value, got {other:?}"),
    }
}

#[test]
fn a_sum_of_two_same_currency_amounts_is_their_exact_total() {
    let sum = run("pub fn run(): Money<GBP> = gbp(12.34) + gbp(1.00);\n")
        .expect("sum runs");
    let total = run("pub fn run(): Money<GBP> = gbp(13.34);\n").expect("literal runs");
    assert_eq!(money_parts(sum), money_parts(total));
}

#[test]
fn the_amount_field_reads_the_exact_decimal() {
    let value =
        run("pub fn run(): Decimal = (gbp(12.34)).amount;\n").expect("amount runs");
    let literal = run("pub fn run(): Decimal = 12.34;\n").expect("literal runs");
    assert_eq!(value, literal);
}

#[test]
fn negate_and_subtract_keep_the_currency() {
    let negated = run("pub fn run(): Money<GBP> = -gbp(3.00);\n").expect("negate runs");
    let minus_three = run("pub fn run(): Money<GBP> = gbp(0.00) - gbp(3.00);\n")
        .expect("sub runs");
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
