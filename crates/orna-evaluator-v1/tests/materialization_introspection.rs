use num_bigint::BigInt;
use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction, RelationPage,
    StepBudget, invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Expr, parse_expression};
use orna_value_v1::Raw;

/// Steps the instant handler charges for reading the clock.
const CLOCK_STEPS: u64 = 5;

fn integer(value: i64) -> Raw {
    Raw::Int(BigInt::from(value))
}

fn instant() -> CanonicalValue {
    CanonicalValue::new(Raw::Tag(
        60002,
        Box::new(Raw::Array(vec![
            Raw::Int(1_700_000_000.into()),
            Raw::Int(7.into()),
        ])),
    ))
    .expect("instant is canonical")
}

/// How the host answers `now` for the activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Clock {
    /// No handler resolves `now`.
    Absent,
    /// A real instant, charging `CLOCK_STEPS` against the clock's budget.
    Instant,
    /// The clock read exhausts its budget and fails.
    Exhausted,
    /// The clock answers with a value that is not an instant.
    NotAnInstant,
}

fn is_now(callee: &Expr) -> bool {
    matches!(callee, Expr::Name { text, .. } if text == "now")
}

#[derive(Clone)]
struct StorageEffects {
    rows: Vec<i64>,
    clock: Clock,
}

impl EffectHandler for StorageEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if !is_now(callee) {
            return Ok(None);
        }
        assert!(arguments.is_empty());
        match self.clock {
            Clock::Absent | Clock::Exhausted => Ok(None),
            Clock::Instant => Ok(Some(instant())),
            Clock::NotAnInstant => Ok(Some(
                CanonicalValue::new(integer(7)).expect("row is canonical"),
            )),
        }
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if is_now(callee) {
            match self.clock {
                Clock::Instant => budget.debit(CLOCK_STEPS)?,
                Clock::Exhausted => budget.debit(u64::MAX)?,
                Clock::Absent | Clock::NotAnInstant => {}
            }
        }
        self.handle(callee, arguments)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        _limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if source != "sys.Storage" {
            return Ok(None);
        }
        budget.debit(1)?;
        let index = after.map_or(0, |cursor| usize::from(cursor[0]));
        let Some(row) = self.rows.get(index) else {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        };
        let row = CanonicalValue::new(integer(*row)).expect("row is canonical");
        Ok(Some(RelationPage {
            rows: vec![row],
            next: (index + 1 < self.rows.len()).then(|| vec![(index + 1) as u8]),
        }))
    }

    fn fork_task_child(&mut self, _child_index: usize) -> Option<Box<dyn EffectHandler + Send>> {
        Some(Box::new(self.clone()))
    }
}

fn run(source: &str, rows: Vec<i64>) -> CanonicalValue {
    run_with(source, rows, Clock::Instant)
}

fn run_with(source: &str, rows: Vec<i64>, clock: Clock) -> CanonicalValue {
    run_limited(source, rows, clock, Limits::default()).expect("query should evaluate")
}

fn run_limited(
    source: &str,
    rows: Vec<i64>,
    clock: Clock,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(source);
    assert!(
        parsed.is_ok(),
        "fixture should parse: {source}: {:?}",
        parsed.diagnostics
    );
    let functions = Functions::from([(
        "query".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    let mut effects = StorageEffects { rows, clock };
    invoke_named_with_effects(
        "query",
        &functions,
        &Environment::new(),
        limits,
        &mut effects,
    )
}

#[test]
fn materialization_is_empty_before_any_buffered_decision() {
    assert_eq!(
        run("sys.Materialization | count()", vec![2, 1]).raw(),
        &integer(0)
    );
}

#[test]
fn each_buffered_decision_is_visible_to_later_introspection_in_the_same_activation() {
    // List elements evaluate left to right: each sum buffers one decision over
    // sys.Storage before the introspection count reads the activation log.
    let result = run(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
    );

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![integer(3), integer(3), integer(1)])
    );
}

#[test]
fn materialization_digest_is_deterministic_for_equal_rows_and_differs_otherwise() {
    // The digest is observed through a projection after one buffered decision.
    let query = "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | project(m => m.digest) | first()]";
    let same_a = run(query, vec![2, 1]);
    let same_b = run(query, vec![2, 1]);
    let different = run(query, vec![5, 1]);

    assert_eq!(
        same_a, same_b,
        "equal buffered rows must yield equal digests"
    );
    assert_ne!(
        same_a, different,
        "different buffered rows must yield different digests"
    );
}

#[test]
fn distinct_stages_over_the_same_relation_get_distinct_keys() {
    // Equal outputs from different stages must not collapse: the natural key
    // is object (stages) + snapshot (output), so each stage is its own row.
    let result = run(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Storage | filter(value => value >= 1) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
    );

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![integer(3), integer(3), integer(2)])
    );
}

#[test]
fn parallel_child_decisions_reach_the_parent_log() {
    let result = run(
        "[std.concurrent.parallel([() => sys.Storage | filter(value => value >= 0) | sum()]),
          sys.Materialization | count()]",
        vec![2, 1],
    );

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![Raw::Array(vec![integer(3)]), integer(1)])
    );
}

#[test]
fn materialization_created_is_the_activation_instant() {
    let result = run(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | project(m => m.created) | first()]",
        vec![2, 1],
    );

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![
            integer(3),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![integer(1), instant().raw().clone()])),
            ),
        ])
    );
}

#[test]
fn materialization_without_a_clock_is_not_recorded() {
    // ORNA-QUERY-006 / ORNA-CALL-004: a decision without an activation instant
    // is not inspectable, so it is not recorded. The query result is unchanged.
    let result = run_with(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
        Clock::Absent,
    );

    assert_eq!(result.raw(), &Raw::Array(vec![integer(3), integer(0)]));
}

#[test]
fn materialization_with_a_non_instant_clock_is_not_recorded() {
    let result = run_with(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
        Clock::NotAnInstant,
    );

    assert_eq!(result.raw(), &Raw::Array(vec![integer(3), integer(0)]));
}

#[test]
fn failing_clock_read_is_not_recorded_and_leaves_the_query_result_unchanged() {
    let result = run_with(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
        Clock::Exhausted,
    );

    assert_eq!(result.raw(), &Raw::Array(vec![integer(3), integer(0)]));
}

#[test]
fn recording_a_decision_does_not_consume_the_query_step_budget() {
    // ORNA-PLAN-001: recording must not move a pure result to a step limit.
    // The clock charges CLOCK_STEPS, so a clock debited from the query budget
    // would fail the query at its own minimum. The recording must not do so.
    let source = "sys.Storage | filter(value => value >= 0) | sum()";
    let minimum = (0..=1_000)
        .find(|&max_steps| {
            run_limited(
                source,
                vec![2, 1],
                Clock::Absent,
                Limits {
                    max_steps,
                    ..Limits::default()
                },
            )
            .is_ok()
        })
        .expect("query fits within 1000 steps");
    assert!(
        minimum >= CLOCK_STEPS,
        "the clock read must fit the activation budget for a decision to be recorded"
    );

    let limits = Limits {
        max_steps: minimum,
        ..Limits::default()
    };
    let result = run_limited(
        source,
        vec![2, 1],
        Clock::Instant,
        limits,
    )
    .expect("recording must not add a step-limit failure to a pure query");

    assert_eq!(result.raw(), &integer(3));
}
