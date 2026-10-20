use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits,
    invoke_named_with_effects_and_reuse_report,
};
use orna_syntax_v1::{Declaration, Expr};
use orna_value_v1::Value;

/// Stands in for the relation write and read path. Relation writes reach the
/// evaluator as `Note.insert(..)` through `EffectHandler::handle`, and reads as
/// `Note.count()`.
#[derive(Default)]
struct NoteEffects {
    calls: Vec<Vec<Value>>,
}

impl EffectHandler for NoteEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text != "Note" {
            return Ok(None);
        }
        if name == "count" {
            return Ok(Some(Value::int(self.calls.len().into())));
        }
        if name != "insert" {
            return Ok(None);
        }
        self.calls.push(arguments.to_vec());
        Ok(Some(Value::int(arguments.len().into())))
    }
}

fn functions_from_source(source: &str) -> Functions {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let Declaration::Function { signature, body } = item.declaration else {
                panic!("function expected")
            };
            (
                signature.name,
                orna_evaluator_v1::PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

/// ORNA-CALL-001: equal pure calls in one activation return equal values, and
/// the second call is served from the activation memo.
#[test]
fn equal_pure_calls_in_one_activation_reuse_the_stored_value() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + helper(41);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(84.into()));
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.stores, 1);
    assert_eq!(stats.invalidations, 0);
    assert!(effects.calls.is_empty());
}

/// ORNA-CALL-001: reuse is keyed by the supplied arguments, so different
/// arguments never observe each other's result.
#[test]
fn calls_with_different_arguments_do_not_reuse() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + helper(42);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(85.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
}

/// ORNA-CALL-002: a relation write between two equal pure calls is a boundary,
/// so the second call must be evaluated again rather than reused.
#[test]
fn relation_write_between_equal_calls_invalidates_reuse() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + Note.insert(1) + helper(41);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(85.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.invalidations, 1);
    assert_eq!(effects.calls.len(), 1);
}

/// ORNA-QUERY-005: a write earlier in the activation is observed by a later
/// read of the changed table, even when the reading call has equal arguments.
#[test]
fn later_read_observes_earlier_write_in_same_activation() {
    let functions = functions_from_source(
        "fn read_count(value: Int) = Note.count() + value;\nfn entry() = read_count(0) + Note.insert(1) + read_count(0);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    // First read sees 0 rows, the insert returns 1, and the second read must
    // see the inserted row: 0 + 1 + 1.
    assert_eq!(value, Value::int(2.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(effects.calls.len(), 1);
}

/// ORNA-CALL-002: a function whose body performs an effect is never stored, so
/// equal source and arguments do not collapse its effects.
#[test]
fn effectful_function_calls_are_not_collapsed() {
    let functions = functions_from_source(
        "fn record(value: Int) = Note.insert(value);\nfn entry() = record(1) + record(1);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(2.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
    assert_eq!(effects.calls.len(), 2);
}

/// Counts the clock and identity intrinsics that reach the effect boundary.
/// Each read returns a fresh integer, so a reused result would be observable.
#[derive(Default)]
struct IntrinsicEffects {
    reads: i64,
}

impl EffectHandler for IntrinsicEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        _arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Name { text, .. } = callee else {
            return Ok(None);
        };
        if text != "now" && text != "uuid7" {
            return Ok(None);
        }
        self.reads += 1;
        Ok(Some(Value::int(self.reads.into())))
    }
}

/// ORNA-CALL-002: `now()` is an external read, so a pure-looking call that
/// performs it must not be stored and reused by an equal second call.
#[test]
fn now_boundary_prevents_reuse_of_the_enclosing_call() {
    let functions = functions_from_source(
        "fn pick(x: Int) = x + now();\nfn entry() = pick(1) + pick(1);",
    );
    let mut effects = IntrinsicEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    // Two reads, 1 + 1 and 1 + 2: a reused second call would yield 4.
    assert_eq!(value, Value::int(5.into()));
    assert_eq!(effects.reads, 2);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}

/// ORNA-CALL-002: `uuid7()` is an external identity source with the same
/// boundary as `now()`.
#[test]
fn uuid7_boundary_prevents_reuse_of_the_enclosing_call() {
    let functions = functions_from_source(
        "fn pick(x: Int) = x + uuid7();\nfn entry() = pick(1) + pick(1);",
    );
    let mut effects = IntrinsicEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(5.into()));
    assert_eq!(effects.reads, 2);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}

/// ORNA-CALL-002: a call whose body forks a concurrent child (`race`) crosses
/// the fork boundary; the guard must stop the parent from reusing its result.
#[test]
fn race_fork_does_not_collapse_later_equal_calls() {
    let functions = functions_from_source(
        "fn pick(x: Int) = std.concurrent.race([() => x, () => x + 1]);\nfn entry() = [pick(1), pick(1)];",
    );
    let mut effects = NoteEffects::default();

    let (_value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}

/// ORNA-CALL-002: `parallel` is a fork boundary like `race`.
#[test]
fn parallel_fork_does_not_collapse_later_equal_calls() {
    let functions = functions_from_source(
        "fn pick(x: Int) = std.concurrent.parallel([() => x, () => x + 1]);\nfn entry() = [pick(1), pick(1)];",
    );
    let mut effects = NoteEffects::default();

    let (_value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}

/// ORNA-CALL-002: `timeout` runs its body as a fork child under a deadline and
/// is a fork boundary like `race`.
#[test]
fn timeout_fork_does_not_collapse_later_equal_calls() {
    let functions = functions_from_source(
        "fn pick(x: Int) = std.concurrent.timeout(() => x, 1.s);\nfn entry() = [pick(1), pick(1)];",
    );
    let mut effects = NoteEffects::default();

    let (_value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}

/// Closure arguments are never admitted to the memo: a closure carries live
/// evaluation state, so equal-looking calls with closures must both run.
#[test]
fn closure_arguments_are_not_reused() {
    let functions = functions_from_source(
        "fn apply(f) = f(1);\nfn entry() = apply(y => y + 1) + apply(y => y + 1);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(4.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
}
