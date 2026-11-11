//! The `core.collection` engine natives over lowered source, against the old evaluator's outcomes: the whole-list
//! natives, the closure natives, `group_by` and `asof_join`, their error codes, and the registry's coverage of the
//! declarations.

use std::rc::Rc;

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.collection;\n";

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

fn int(value: i64) -> Value {
    Value::Int(value.into())
}

fn ints(values: &[i64]) -> Value {
    Value::List(Rc::from(
        values.iter().map(|value| int(*value)).collect::<Vec<_>>(),
    ))
}

fn some(value: Value) -> Value {
    Value::Option(Some(Rc::new(value)))
}

fn none() -> Value {
    Value::Option(None)
}

fn pair(first: Value, second: Value) -> Value {
    Value::Tuple(Rc::from(vec![first, second]))
}

/// The value of `run` for a body that is one call expression.
fn call(body: &str) -> Result<Value, ExecError> {
    run(&format!("{HEAD}pub fn run() = {body};\n"))
}

#[test]
fn count_union_take_drop_and_distinct_keep_source_order() {
    assert_eq!(call("collection.count([1, 2, 3])"), Ok(int(3)));
    assert_eq!(call("collection.union([1], [2, 3])"), Ok(ints(&[1, 2, 3])));
    assert_eq!(call("collection.take([1, 2, 3], 2)"), Ok(ints(&[1, 2])));
    assert_eq!(call("collection.drop([1, 2, 3], 2)"), Ok(ints(&[3])));
    assert_eq!(call("collection.take([1], 5)"), Ok(ints(&[1])));
    assert_eq!(
        call("collection.distinct([1, 2, 1, 3, 2])"),
        Ok(ints(&[1, 2, 3]))
    );
}

#[test]
fn a_negative_take_or_drop_count_is_the_old_value_code() {
    assert_eq!(
        call("collection.take([1, 2], -1)"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        call("collection.drop([1, 2], -1)"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn first_is_null_when_absent_and_last_min_and_max_are_options() {
    assert_eq!(call("collection.first([4, 5])"), Ok(some(int(4))));
    assert_eq!(call("collection.last([4, 5])"), Ok(some(int(5))));
    assert_eq!(
        call("collection.first(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
    assert_eq!(call("collection.last(collection.drop([1], 1))"), Ok(none()));
    assert_eq!(call("collection.min([3, 1, 2])"), Ok(some(int(1))));
    assert_eq!(call("collection.max([3, 1, 2])"), Ok(some(int(3))));
    assert_eq!(call("collection.min(collection.drop([1], 1))"), Ok(none()));
    assert_eq!(call("collection.max(collection.drop([1], 1))"), Ok(none()));
}

#[test]
fn sum_adds_integers_exactly_and_is_zero_for_no_rows() {
    assert_eq!(call("collection.sum(collection.drop([1], 1))"), Ok(int(0)));
    assert_eq!(
        call("collection.sum([9007199254740993, 1])"),
        Ok(int(9007199254740994))
    );
}

#[test]
fn map_flat_map_and_filter_apply_their_closures_in_order() {
    assert_eq!(
        call("collection.map([1, 2, 3], value => value * 2)"),
        Ok(ints(&[2, 4, 6]))
    );
    assert_eq!(
        call("collection.flat_map([2, 1], value => [value, value + 10])"),
        Ok(ints(&[2, 12, 1, 11]))
    );
    assert_eq!(
        call("collection.filter([1, 2, 3, 4], value => value % 2 == 0)"),
        Ok(ints(&[2, 4]))
    );
}

#[test]
fn every_and_exists_stop_at_the_deciding_row() {
    assert_eq!(
        call("collection.every([2, 4], value => value % 2 == 0)"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        call("collection.every([2, 3], value => value % 2 == 0)"),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        call("collection.exists([1, 2], value => value > 1)"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        call("collection.exists([1, 2], value => value > 5)"),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        call("collection.every([1, 0], value => value > 0)"),
        Ok(Value::Bool(false))
    );
}

#[test]
fn one_returns_the_single_match_and_reports_zero_or_many() {
    assert_eq!(
        call("collection.one([4, 5], value => value == 5)"),
        Ok(int(5))
    );
    assert_eq!(
        call("collection.one([1, 2], value => value > 5)"),
        Err(ExecError::Code("ORNA-EVAL-RELATION-ONE-ZERO"))
    );
    assert_eq!(
        call("collection.one([1, 2], value => value > 0)"),
        Err(ExecError::Code("ORNA-EVAL-RELATION-ONE-MULTIPLE"))
    );
}

#[test]
fn one_propagates_a_callback_failure_after_a_second_match() {
    assert_eq!(
        call(
            "collection.one([1, 2, 3], value => if value < 3 { true } else { 1 / (value - 3) == 0 })"
        ),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
}

#[test]
fn a_callback_failure_stops_map() {
    assert_eq!(
        call("collection.map([1, 0, 2], value => 1 / value)"),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
}

#[test]
fn a_predicate_that_is_not_a_boolean_is_a_type_error() {
    assert_eq!(
        call("collection.filter([1], value => value)"),
        Err(ExecError::Code("ORNA-EVAL-TYPE"))
    );
}

#[test]
fn sort_by_is_stable_and_rank_shares_ties() {
    assert_eq!(
        call("collection.sort_by([3, 2, 1, 4], value => value % 2)"),
        Ok(ints(&[2, 4, 3, 1]))
    );
    assert_eq!(
        call("collection.rank([10, 20, 20, 30], value => value)"),
        Ok(Value::List(Rc::from(vec![
            pair(int(10), int(1)),
            pair(int(20), int(2)),
            pair(int(20), int(2)),
            pair(int(30), int(4)),
        ])))
    );
}

#[test]
fn group_by_keeps_first_seen_rows_and_sorts_the_groups() {
    assert_eq!(
        call("collection.group_by([1, 2, 3, 4], value => value % 2)"),
        Ok(Value::List(Rc::from(vec![
            pair(int(0), ints(&[2, 4])),
            pair(int(1), ints(&[1, 3])),
        ])))
    );
}

#[test]
fn asof_join_pairs_each_row_with_the_latest_earlier_row_or_null() {
    assert_eq!(
        call("collection.asof_join([4], [1, 3, 2], value => value, value => value % 2)"),
        Ok(Value::List(Rc::from(vec![pair(int(4), some(int(2)))])))
    );
    assert_eq!(
        call("collection.asof_join([0], [1], value => value, value => 0)"),
        Ok(Value::List(Rc::from(vec![pair(int(0), Value::Null)])))
    );
    assert_eq!(
        call("collection.asof_join(collection.drop([1], 1), [1], value => value, value => 0)"),
        Ok(Value::List(Rc::from(Vec::<Value>::new())))
    );
}

#[test]
fn a_bucket_by_call_is_not_yet_ported() {
    assert_eq!(
        call("collection.bucket_by([1], 1, \"UTC\")"),
        Err(ExecError::NotYet("engine native not ported yet (H5)"))
    );
}

#[test]
fn an_absent_first_encodes_to_the_canonical_null_the_old_evaluator_produces() {
    // The old evaluator returns `Value::Null` for an absent `first`, which `to_raw` encodes as `Raw::Null`.
    let absent = call("collection.first(collection.drop([1], 1))")
        .expect("first on no rows is not a failure");
    assert_eq!(absent, Value::Null);
    assert_eq!(absent.to_raw(), Ok(orna_value::Raw::Null));
    assert_eq!(Value::Null.to_raw(), Ok(orna_value::Raw::Null));
}

/// The value of `run` for a `core.stats` body that is one call expression.
fn call_stats(body: &str) -> Result<Value, ExecError> {
    run(&format!(
        "use core.stats;\nuse core.collection;\npub fn run() = {body};\n"
    ))
}

#[test]
fn sum_min_max_range_and_mode_match_the_old_stats_fixtures() {
    assert_eq!(call_stats("stats.sum([1, 2, 3])"), Ok(int(6)));
    assert_eq!(call_stats("stats.min([3, 1, 2])"), Ok(some(int(1))));
    assert_eq!(call_stats("stats.max([3, 1, 2])"), Ok(some(int(3))));
    assert_eq!(call_stats("stats.range([3, 1, 2])"), Ok(int(2)));
    assert_eq!(call_stats("stats.mode([3, 1, 3, 2, 2])"), Ok(ints(&[3, 2])));
}

#[test]
fn stats_extremes_range_and_mode_of_no_rows_are_null_or_empty() {
    assert_eq!(
        call_stats("stats.min(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
    assert_eq!(
        call_stats("stats.max(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
    assert_eq!(
        call_stats("stats.range(collection.drop([1], 1))"),
        Ok(Value::Null)
    );
    assert_eq!(
        call_stats("stats.mode(collection.drop([1], 1))"),
        Ok(ints(&[]))
    );
    assert_eq!(call_stats("stats.sum(collection.drop([1], 1))"), Ok(int(0)));
}

#[test]
fn stats_mode_keeps_every_row_tied_for_the_most_occurrences() {
    assert_eq!(call_stats("stats.mode([2, 1, 1, 2])"), Ok(ints(&[2, 1])));
    assert_eq!(call_stats("stats.mode([7])"), Ok(ints(&[7])));
}
