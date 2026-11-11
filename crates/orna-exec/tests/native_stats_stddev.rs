//! The `core.stats` standard deviation native over lowered source, against the old evaluator's outcomes: one value has
//! zero spread, empty input is null, exact roots are kept, a root with no finite expansion is rounded half-even to the
//! scale, and an inexact root without a scale and malformed rows are refused with the old codes.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_string_bytes: 1 << 20,
    max_collection_items: 2_048,
    max_integer_digits: 1_024,
};

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

fn call_stats(body: &str) -> Result<Value, ExecError> {
    run(&format!(
        "use core.stats;\nuse core.collection;\npub fn run() = {body};\n"
    ))
}

#[test]
fn standard_deviation_of_one_value_is_zero_and_empty_input_is_null() {
    assert_eq!(
        call_stats("stats.standard_deviation([7])"),
        Ok(Value::Int(0.into()))
    );
    assert_eq!(
        call_stats("stats.standard_deviation(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
}

#[test]
fn standard_deviation_keeps_an_exact_root() {
    assert_eq!(
        call_stats("stats.standard_deviation([1, 3])"),
        Ok(Value::Int(1.into()))
    );
    assert_eq!(
        call_stats("stats.standard_deviation([1.0, 3.0])"),
        call_stats("1.0")
    );
}

#[test]
fn standard_deviation_rounds_half_even_when_the_root_is_inexact() {
    assert_eq!(
        call_stats("stats.standard_deviation([1, 2, 3], scale: 2, rounding: \"half_even\")"),
        call_stats("0.82")
    );
    assert_eq!(
        call_stats("stats.standard_deviation([1, 2, 3])"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn standard_deviation_refuses_non_numeric_rows_and_malformed_options_with_the_old_codes() {
    assert_eq!(
        call_stats("stats.standard_deviation([\"a\"])"),
        Err(ExecError::Code("ORNA-EVAL-TYPE"))
    );
    assert_eq!(
        call_stats("stats.standard_deviation([1, 2], scale: -1, rounding: \"half_even\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}
