//! The `core.collection` window natives over lowered source, against the old evaluator's outcomes on the same inputs:
//! per-window rates, trapezoidal areas and derivatives over complete overlapping windows in source order, the windows
//! of a sparse series, the refusals for a non-positive size or step and for a window of fewer than two points, and the
//! strict time-order check.

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

fn call_collection(body: &str) -> Result<Value, ExecError> {
    run(&format!("use core.collection;\npub fn run() = {body};\n"))
}

fn instant(unix_seconds: i64) -> Value {
    Value::Instant {
        unix_seconds,
        nanosecond: 0,
    }
}

fn int(value: i64) -> Value {
    Value::Int(value.into())
}

fn some(value: Value) -> Value {
    Value::Option(Some(Rc::new(value)))
}

fn pair(first: Value, second: Value) -> Value {
    Value::Tuple(Rc::from(vec![first, second]))
}

fn list(items: Vec<Value>) -> Value {
    Value::List(Rc::from(items))
}

#[test]
fn window_rate_and_integrate_are_per_complete_window_in_source_order() {
    // The pkbay fixtures: windows of three points, one apart.
    let points = "[(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5), (1970-01-01T00:00:04Z, 9), (1970-01-01T00:00:06Z, 13)]";
    assert_eq!(
        call_collection(&format!("collection.__window_rate({points}, 3, 1)")),
        Ok(list(vec![some(int(2)), some(int(2))]))
    );
    assert_eq!(
        call_collection(&format!("collection.__window_integrate({points}, 3, 1)")),
        Ok(list(vec![some(int(20)), some(int(36))]))
    );
}

#[test]
fn sparse_windows_keep_elapsed_time_across_gaps() {
    let points = "[(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:01Z, 3), (1970-01-01T00:00:03Z, 7), (1970-01-01T00:00:06Z, 13)]";
    assert_eq!(
        call_collection(&format!("collection.__window_rate({points}, 2, 2)")),
        Ok(list(vec![some(int(2)), some(int(2))]))
    );
    assert_eq!(
        call_collection(&format!("collection.__window_integrate({points}, 2, 2)")),
        Ok(list(vec![some(int(2)), some(int(30))]))
    );
}

#[test]
fn deep_gap_windows_skip_positions_and_omit_the_short_tail() {
    // Size 2 step 2 over five points: starts 0 and 2, the start 4 has no second point.
    let rate_points = "[(1970-01-01T00:00:00Z, 0), (1970-01-01T00:00:01Z, 2), (1970-01-01T00:00:03Z, 6), (1970-01-01T00:00:07Z, 14), (1970-01-01T00:00:11Z, 22)]";
    assert_eq!(
        call_collection(&format!("collection.__window_rate({rate_points}, 2, 2)")),
        Ok(list(vec![some(int(2)), some(int(2))]))
    );

    // Size 2 step 3 over five points: starts 0 and 3.
    let integrate_points = "[(1970-01-01T00:00:00Z, 0), (1970-01-01T00:00:01Z, 2), (1970-01-01T00:00:03Z, 6), (1970-01-01T00:00:07Z, 14), (1970-01-01T00:00:10Z, 20)]";
    assert_eq!(
        call_collection(&format!(
            "collection.__window_integrate({integrate_points}, 2, 3)"
        )),
        Ok(list(vec![some(int(1)), some(int(51))]))
    );
}

#[test]
fn window_rate_integrate_pairs_the_rate_and_area_of_each_window() {
    // Size 3 step 2 over seven points: windows start at 0, 2 and 4.
    let points = "[(1970-01-01T00:00:00Z, 0), (1970-01-01T00:00:02Z, 4), (1970-01-01T00:00:06Z, 18), (1970-01-01T00:00:10Z, 22), (1970-01-01T00:00:12Z, 30), (1970-01-01T00:00:16Z, 36), (1970-01-01T00:00:18Z, 48)]";
    assert_eq!(
        call_collection(&format!(
            "collection.__window_rate_integrate({points}, 3, 2)"
        )),
        Ok(list(vec![
            pair(some(int(3)), some(int(48))),
            pair(some(int(2)), some(int(132))),
            pair(some(int(3)), some(int(216))),
        ]))
    );
}

#[test]
fn window_derivative_times_each_pair_at_its_later_point() {
    let points = "[(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5), (1970-01-01T00:00:04Z, 9), (1970-01-01T00:00:06Z, 13)]";
    assert_eq!(
        call_collection(&format!("collection.__window_derivative({points}, 3, 1)")),
        Ok(list(vec![
            list(vec![pair(instant(2), int(2)), pair(instant(4), int(2)),]),
            list(vec![pair(instant(4), int(2)), pair(instant(6), int(2)),]),
        ]))
    );
}

#[test]
fn a_window_longer_than_the_series_yields_no_windows() {
    assert_eq!(
        call_collection(
            "collection.__window_rate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)], 3, 1)"
        ),
        Ok(list(Vec::new()))
    );
}

#[test]
fn a_non_positive_size_or_step_is_a_value_error() {
    for body in [
        "collection.__window_rate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)], 0, 1)",
        "collection.__window_rate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)], 2, 0)",
        "collection.__window_integrate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)], 2, -1)",
    ] {
        assert_eq!(
            call_collection(body),
            Err(ExecError::Code("ORNA-EVAL-VALUE")),
            "{body}"
        );
    }
}

#[test]
fn a_window_of_one_point_is_a_value_error() {
    assert_eq!(
        call_collection(
            "collection.__window_rate([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)], 1, 1)"
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn strictly_ordered_time_series_rejects_equal_and_reversed_times() {
    assert_eq!(
        call_collection(
            "collection.__strictly_ordered_time_series([(1970-01-01T00:00:00Z, 1), (1970-01-01T00:00:02Z, 5)])"
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        call_collection(
            "collection.__strictly_ordered_time_series([(1970-01-01T00:00:02Z, 1), (1970-01-01T00:00:02Z, 5)])"
        ),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        call_collection(
            "collection.__strictly_ordered_time_series([(1970-01-01T00:00:04Z, 1), (1970-01-01T00:00:02Z, 5)])"
        ),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        call_collection("collection.__strictly_ordered_time_series([(1970-01-01T00:00:00Z, 1)])"),
        Ok(Value::Bool(true))
    );
}
