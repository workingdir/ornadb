//! Call-depth budget and `case` arm slots.
//!
//! ORNA-CLOSURE-004: resource limits may vary only where explicitly permitted
//! and must remain observable. `Limits::max_depth` bounds active calls, and
//! exhausting it fails with the explicit `ORNA-EVAL-LIMIT` code instead of a
//! partial result. Expression nesting inside one body is bounded by the
//! parser and does not consume the call budget.
//!
//! ORNA-CFLOW-005: `let` creates a function-local slot and assignment replaces
//! its value, including from inside a `case` arm.

use orna_evaluator_v1::{
    AdmittedReplSession, Environment, Functions, Limits, PureFunction, evaluate_expression,
    evaluate_with_functions,
};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

const RECURSIVE_MODULE: &str = "
fn depth(n: Int): Int = if n == 0 { 0 } else { 1 + depth(n - 1) };
fn recur(): Int = recur();
fn nested(n: Int): Int = if n == 0 {
    ((((((((1 + 1) * 2) - 1) + 1) * 2) - 1) + 1) * 2)
} else {
    let next = n - 1;
    if next >= 0 { nested(next) } else { 0 }
};
";

/// `((((((((1 + 1) * 2) - 1) + 1) * 2) - 1) + 1) * 2)`.
const NESTED_VALUE: i64 = 16;

fn functions(source: &str) -> Functions {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration else {
                panic!("function expected")
            };
            (
                signature.name,
                PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

fn call(expression: &str, limits: Limits) -> Result<Value, String> {
    let parsed = orna_syntax_v1::parse_expression(expression);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    evaluate_with_functions(
        &parsed.value,
        &Environment::new(),
        &functions(RECURSIVE_MODULE),
        limits,
    )
    .map_err(|error| error.code().to_owned())
}

fn evaluate(source: &str, limits: Limits) -> Result<Value, String> {
    evaluate_expression(source, &Environment::new(), limits)
        .map_err(|error| error.code().to_owned())
}

fn int(value: i64) -> Value {
    Value::int(value.into())
}

fn limit() -> Result<Value, String> {
    Err("ORNA-EVAL-LIMIT".to_owned())
}

#[test]
fn default_call_budget_admits_sixty_four_active_calls() {
    // `depth(63)` keeps 64 calls active at its base case.
    assert_eq!(call("depth(63)", Limits::default()), Ok(int(63)));
    assert_eq!(call("depth(64)", Limits::default()), limit());
}

#[test]
fn configured_depth_counts_calls_not_expression_nesting() {
    let limits = Limits {
        max_depth: 3,
        ..Limits::default()
    };
    // Each activation nests conditionals, blocks and arithmetic well past
    // three levels; only the three active calls count.
    assert_eq!(call("nested(2)", limits), Ok(int(NESTED_VALUE)));
    assert_eq!(call("nested(3)", limits), limit());
    assert_eq!(call("depth(2)", limits), Ok(int(2)));
    assert_eq!(call("depth(3)", limits), limit());
}

#[test]
fn top_level_expression_nesting_does_not_consume_the_call_budget() {
    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    assert_eq!(
        evaluate("((((((((1 + 1) * 2) - 1) + 1) * 2) - 1) + 1) * 2)", limits),
        Ok(int(NESTED_VALUE)),
    );
}

#[test]
fn unbounded_recursion_fails_with_an_explicit_limit() {
    assert_eq!(call("recur()", Limits::default()), limit());
}

#[test]
fn closure_calls_count_against_the_call_budget() {
    let limits = Limits {
        max_depth: 2,
        ..Limits::default()
    };
    assert_eq!(evaluate("(value => value + 1)(1)", limits), Ok(int(2)));
    assert_eq!(
        evaluate("(value => (inner => inner + value)(1))(1)", limits),
        Ok(int(2)),
    );
    assert_eq!(
        evaluate(
            "(value => (inner => (last => last + inner)(value))(1))(1)",
            limits,
        ),
        limit(),
    );
}

const CASE_ARM_ASSIGNMENT: &str = "{
    let total = 0;
    for item in [Some(1), null, Some(3)] {
        case item {
            Some(value): {
                total += value;
            },
            null: {},
        }
    }
    total
}";

const CASE_ARM_SHADOWING: &str = "{
    let value = 10;
    let seen = 0;
    case Some(4) {
        Some(value): {
            seen = value;
        },
        null: {},
    }
    (value, seen)
}";

const GUARDED_CASE_ARM_ASSIGNMENT: &str = "{
    let label = 0;
    case 5 {
        n if n > 10: {
            label = 1;
        },
        _: {
            label = 2;
        },
    }
    label
}";

#[test]
fn case_arm_assignment_replaces_the_enclosing_slot() {
    assert_eq!(evaluate(CASE_ARM_ASSIGNMENT, Limits::default()), Ok(int(4)));
    assert_eq!(
        evaluate(GUARDED_CASE_ARM_ASSIGNMENT, Limits::default()),
        Ok(int(2)),
    );
}

#[test]
fn case_pattern_bindings_stay_local_to_their_arm() {
    assert_eq!(
        evaluate(CASE_ARM_SHADOWING, Limits::default()).map(|value| value.raw().clone()),
        Ok(Raw::Tag(
            60015,
            Box::new(Raw::Array(vec![Raw::Int(10.into()), Raw::Int(4.into())])),
        )),
    );
}

#[test]
fn admitted_session_keeps_case_arm_assignments() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(CASE_ARM_ASSIGNMENT)
            .map_err(|error| error.code().to_owned()),
        Ok(Some(
            CanonicalValue::new(Raw::Int(4.into())).expect("integer is canonical")
        )),
    );
}
