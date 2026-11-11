//! The `core.stats.mean` native over lowered source, against the old evaluator's outcomes: whole integer means, exact
//! decimal means, half-even rounding to a scale, float means, the empty list, and the refusals of a bad option.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.stats;\n";

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    let user = ModulePath::new([Name::new("main")]);
    let diagnostics: Vec<String> = program
        .modules
        .iter()
        .filter(|module| module.path == user)
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{diagnostic:?}"))
        })
        .collect();
    assert!(
        diagnostics.is_empty(),
        "lowering diagnostics: {diagnostics:?}"
    );
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn run(source: &str) -> Result<Value, ExecError> {
    let program = lower_one(source);
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

/// The value of `run`, the body of which is one expression over `core.stats` and a typed empty list helper.
fn call(body: &str) -> Result<Value, ExecError> {
    run(&format!(
        "{HEAD}pub fn empty(): [Int] = [];\npub fn run() = {body};\n"
    ))
}

/// The value of a literal expression, lowered the same way as the call under test.
fn literal(body: &str) -> Value {
    run(&format!("pub fn run() = {body};\n")).expect("literal evaluates")
}

#[test]
fn whole_integer_means_are_integers() {
    assert_eq!(call("stats.mean([1, 2, 3])"), Ok(Value::Int(2.into())));
    assert_eq!(call("stats.mean([10, 20])"), Ok(Value::Int(15.into())));
}

#[test]
fn an_exact_non_integer_mean_stays_decimal_without_a_scale() {
    assert_eq!(call("stats.mean([1, 2])"), Ok(literal("1.5")));
}

#[test]
fn an_inexact_mean_without_a_scale_is_a_value_error() {
    assert_eq!(
        call("stats.mean([1, 2, 4])"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn a_scale_rounds_an_inexact_mean_half_to_even() {
    assert_eq!(
        call("stats.mean([1, 2, 4], 2, \"half_even\")"),
        Ok(literal("2.33"))
    );
    assert_eq!(
        call("stats.mean([1, 2], 0, \"half_even\")"),
        Ok(Value::Int(2.into()))
    );
    assert_eq!(
        call("stats.mean([1, 2, 4], 0, \"half_even\")"),
        Ok(Value::Int(2.into()))
    );
}

#[test]
fn decimal_means_keep_their_decimal_form() {
    assert_eq!(call("stats.mean([1.5, 2.5])"), Ok(literal("2.0")));
    assert_eq!(
        call("stats.mean([1.0, 2.0, 2.0], 2, \"half_even\")"),
        Ok(literal("1.67"))
    );
}

#[test]
fn float_means_average_in_order() {
    assert_eq!(
        call("stats.mean([1.0f, 2.0f])"),
        Ok(Value::Float(1.5f64.to_bits()))
    );
}

#[test]
fn a_mean_of_no_rows_is_null() {
    assert_eq!(call("stats.mean(empty())"), Ok(Value::Null));
}

#[test]
fn a_bad_rounding_option_is_refused() {
    assert_eq!(
        call("stats.mean([1, 2, 4], 0, \"half_up\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call("stats.mean([1, 2], -1, \"half_even\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}
