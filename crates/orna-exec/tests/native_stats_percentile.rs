//! The `core.stats` percentile native over lowered source, against the old evaluator's outcomes: the endpoints are exact,
//! each interpolation picks its neighbour by the fraction, a scale rounds half-even, float rows interpolate as floats,
//! and a bad probability, interpolation or mixed row kind is refused with the old codes.

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
fn percentile_of_one_value_is_that_value_and_empty_input_is_null() {
    assert_eq!(
        call_stats("stats.percentile([7], 0.5, \"linear\")"),
        Ok(Value::Int(7.into()))
    );
    assert_eq!(
        call_stats("stats.percentile(collection.drop([1], 1), 0.5, \"linear\")"),
        Ok(Value::Null)
    );
}

#[test]
fn percentile_picks_the_endpoints_exactly() {
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0, \"linear\")"),
        Ok(Value::Int(1.into()))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 1, \"linear\")"),
        Ok(Value::Int(4.into()))
    );
}

#[test]
fn percentile_interpolates_integers_by_the_named_method() {
    // The median position of [1, 2, 3, 4] is 1.5, between the second and third rows.
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"linear\")"),
        call_stats("2.5")
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"lower\")"),
        Ok(Value::Int(2.into()))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"higher\")"),
        Ok(Value::Int(3.into()))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"nearest\")"),
        Ok(Value::Int(3.into()))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"midpoint\")"),
        call_stats("2.5")
    );
}

#[test]
fn percentile_rounds_half_even_to_the_scale() {
    // 1.3 rounds down to 1; the tie 2.5 rounds half-even to 2.
    assert_eq!(
        call_stats(
            "stats.percentile([1, 2, 3, 4], 0.1, \"linear\", scale: 0, rounding: \"half_even\")"
        ),
        Ok(Value::Int(1.into()))
    );
    assert_eq!(
        call_stats(
            "stats.percentile([1, 2, 3, 4], 0.5, \"linear\", scale: 0, rounding: \"half_even\")"
        ),
        Ok(Value::Int(2.into()))
    );
}

#[test]
fn percentile_of_floats_interpolates_in_float() {
    assert_eq!(
        call_stats("stats.percentile([1.0f, 2.0f, 3.0f, 4.0f], 0.5, \"linear\")"),
        Ok(Value::Float(2.5_f64.to_bits()))
    );
}

#[test]
fn percentile_refuses_bad_probability_interpolation_and_mixed_rows_with_the_old_codes() {
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 2, \"linear\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], -1, \"linear\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.percentile([1, 2, 3, 4], 0.5, \"bogus\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    // The probability range is checked before the empty-list check.
    assert_eq!(
        call_stats("stats.percentile(collection.drop([1], 1), 2, \"linear\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call_stats("stats.percentile([\"a\"], 0.5, \"linear\")"),
        Err(ExecError::Code("ORNA-EVAL-UNSUPPORTED"))
    );
}
