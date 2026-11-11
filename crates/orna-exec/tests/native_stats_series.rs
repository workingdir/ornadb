//! The `core.stats` series natives `rate`, `derivative` and `integrate` over lowered source, against the old evaluator's
//! outcomes: slopes and areas per elapsed second, derivatives timed at the later point, float series in binary64, and
//! the refusals for fewer than two points, non-increasing times and a non-positive span.

use std::rc::Rc;

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
    run(&format!("use core.stats;\npub fn run() = {body};\n"))
}

fn instant(unix_seconds: i64) -> Value {
    Value::Instant {
        unix_seconds,
        nanosecond: 0,
    }
}

fn float(value: f64) -> Value {
    Value::Float(value.to_bits())
}

#[test]
fn rate_of_floats_is_the_slope_between_the_first_and_last_points() {
    assert_eq!(
        call_stats("stats.rate([(1970-01-01T00:00:00Z, 1.0f), (1970-01-01T00:00:10Z, 3.0f)])"),
        Ok(float(0.2))
    );
    assert_eq!(
        call_stats(
            "stats.rate([(1970-01-01T00:00:00Z, 1.0f), (1970-01-01T00:00:10Z, 9.0f), (1970-01-01T00:00:20Z, 3.0f)])"
        ),
        Ok(float(0.1))
    );
}

#[test]
fn rate_of_decimals_keeps_a_decimal_result() {
    assert!(matches!(
        call_stats("stats.rate([(1970-01-01T00:00:00Z, 1.5), (1970-01-01T00:00:10Z, 2.5)])"),
        Ok(Value::Decimal(_))
    ));
}

#[test]
fn derivative_of_floats_is_timed_at_the_later_point_of_each_pair() {
    assert_eq!(
        call_stats(
            "stats.derivative([(1970-01-01T00:00:00Z, 1.0f), (1970-01-01T00:00:10Z, 3.0f), (1970-01-01T00:00:30Z, 3.0f)])"
        ),
        Ok(Value::List(Rc::from(vec![
            Value::Tuple(Rc::from(vec![instant(10), float(0.2)])),
            Value::Tuple(Rc::from(vec![instant(30), float(0.0)])),
        ])))
    );
}

#[test]
fn integrate_of_floats_is_the_trapezoidal_area() {
    // (1 + 3) / 2 * 10 seconds.
    assert_eq!(
        call_stats("stats.integrate([(1970-01-01T00:00:00Z, 1.0f), (1970-01-01T00:00:10Z, 3.0f)])"),
        Ok(float(20.0))
    );
}

#[test]
fn integrate_of_integers_is_an_exact_area_normalised_to_an_int() {
    // (1 + 3) / 2 * 10 seconds = 20; a whole-valued decimal normalises to Int.
    assert_eq!(
        call_stats("stats.integrate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:10Z, 3)])"),
        Ok(Value::Int(20.into()))
    );
}

#[test]
fn series_with_fewer_than_two_points_is_a_value_error() {
    for body in [
        "stats.rate([(1970-01-01T00:00:00Z, 1.0f)])",
        "stats.derivative([(1970-01-01T00:00:00Z, 1.0f)])",
        "stats.integrate([(1970-01-01T00:00:00Z, 1.0f)])",
    ] {
        assert_eq!(
            call_stats(body),
            Err(ExecError::Code("ORNA-EVAL-VALUE")),
            "{body}"
        );
    }
}

#[test]
fn series_times_must_be_strictly_increasing() {
    for body in [
        "stats.rate([(1970-01-01T00:00:10Z, 1.0f), (1970-01-01T00:00:00Z, 3.0f)])",
        "stats.integrate([(1970-01-01T00:00:10Z, 1.0f), (1970-01-01T00:00:10Z, 3.0f)])",
        "stats.derivative([(1970-01-01T00:00:00Z, 1.0f), (1970-01-01T00:00:10Z, 3.0f), (1970-01-01T00:00:05Z, 4.0f)])",
    ] {
        assert_eq!(
            call_stats(body),
            Err(ExecError::Code("ORNA-EVAL-VALUE")),
            "{body}"
        );
    }
}
