//! The `core.stats` median native over lowered source, against the old evaluator's outcomes: the odd and even lengths,
//! the exact decimal average, the half-even rounding to a scale, the empty list, and the refusals of mixed rows and of
//! malformed options.

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
fn median_of_an_odd_length_is_the_middle_element_after_sorting() {
    assert_eq!(
        call_stats("stats.median([9, 1, 5])"),
        Ok(Value::Int(5.into()))
    );
}

#[test]
fn median_of_an_even_length_is_the_exact_average_of_the_middle_pair() {
    assert_eq!(call_stats("stats.median([1, 2])"), call_stats("1.5"));
    assert_eq!(call_stats("stats.median([1, 3])"), Ok(Value::Int(2.into())));
    assert_eq!(call_stats("stats.median([1, 2, 3, 4])"), call_stats("2.5"));
    assert_eq!(call_stats("stats.median([1.0, 2.0])"), call_stats("1.5"));
}

#[test]
fn median_sorts_decimal_rows_by_exact_value() {
    assert_eq!(
        call_stats("stats.median([3.0, 1.5, 2.0])"),
        call_stats("2.0")
    );
    assert_eq!(
        call_stats("stats.median([1.0, 4.0, 2.5, 3.0])"),
        call_stats("2.75")
    );
}

#[test]
fn median_rounds_half_even_only_when_a_scale_is_given() {
    assert_eq!(
        call_stats("stats.median([1.0, 4.0], scale: 0, rounding: \"half_even\")"),
        call_stats("2.0")
    );
    assert_eq!(
        call_stats("stats.median([1.0, 2.0], scale: 0, rounding: \"half_even\")"),
        call_stats("2.0")
    );
    assert_eq!(
        call_stats("stats.median([1, 2], scale: 0, rounding: \"half_even\")"),
        Ok(Value::Int(2.into()))
    );
}

#[test]
fn median_of_no_rows_is_null() {
    assert_eq!(
        call_stats("stats.median(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
}

#[test]
fn median_of_one_row_is_that_row() {
    assert_eq!(call_stats("stats.median([3])"), Ok(Value::Int(3.into())));
}

#[test]
fn median_refuses_mixed_rows_and_malformed_options_with_the_old_codes() {
    assert_eq!(
        call_stats("stats.median([1, 1.0])"),
        Err(ExecError::Code("ORNA-EVAL-UNSUPPORTED"))
    );
    assert_eq!(
        call_stats("stats.median([1, 2], scale: -1, rounding: \"half_even\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.median([1, 2], scale: 0, rounding: \"half_up\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.median([1, 2], scale: 2000, rounding: \"half_even\")"),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
