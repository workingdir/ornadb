use std::collections::BTreeMap;

use orna_evaluator_v1::{Environment, EvaluationError, Limits, evaluate_expression, evaluate_repl};
use orna_value_v1::{Raw, Value};

fn evaluate(source: &str) -> Value {
    evaluate_expression(source, &Environment::new(), Limits::default()).unwrap()
}
fn code(result: Result<Value, EvaluationError>) -> String {
    result.unwrap_err().code().to_owned()
}

#[test]
fn evaluates_literals_collections_bindings_and_math() {
    let value = evaluate(
        "if true { let point = (std.math.increment(1), 2.5, 3.0f); { label: \"ok\", values: [point, null, true] } }",
    );
    assert_eq!(
        value.raw(),
        &Raw::Map(vec![
            (Raw::Text("label".into()), Raw::Text("ok".into())),
            (
                Raw::Text("values".into()),
                Raw::Array(vec![
                    Raw::Array(vec![
                        Raw::Int(2.into()),
                        Raw::Tag(
                            60000,
                            Box::new(Raw::Array(vec![Raw::Int(25.into()), Raw::Int((-1).into())]))
                        ),
                        Raw::Float(3.0f64.to_bits())
                    ]),
                    Raw::Null,
                    Raw::Bool(true),
                ])
            ),
        ])
    );
}

#[test]
fn uses_environment_and_short_circuiting_deterministically() {
    let mut environment = BTreeMap::new();
    environment.insert("count".into(), Value::int(41.into()));
    assert_eq!(
        evaluate_expression("count + 1", &environment, Limits::default()).unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        evaluate("false && missing"),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate("true || missing"),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn supports_comparison_boolean_and_allowlisted_math() {
    assert_eq!(
        evaluate(
            "std.math.clamp(9, 0, 5) == std.math.max(3, 5) && std.math.is_zero(std.math.decrement(1))"
        ),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate("1.25 + 0.75"),
        Value::decimal(2.into(), 0.into()).unwrap()
    );
    assert_eq!(
        evaluate("{ alphabet: 1, z: 2 }").raw(),
        &Raw::Map(vec![
            (Raw::Text("z".into()), Raw::Int(2.into())),
            (Raw::Text("alphabet".into()), Raw::Int(1.into())),
        ])
    );
    assert_eq!(
        evaluate_repl("std.math.min(7, 3)", &Environment::new(), Limits::default()).unwrap(),
        Value::int(3.into())
    );
}

#[test]
fn rejects_fail_closed_cases_with_redacted_stable_diagnostics() {
    for (source, expected) in [
        ("unknown", "ORNA-EVAL-NAME"),
        ("std.math.abs(1)", "ORNA-EVAL-UNSUPPORTED"),
        ("1 / 0", "ORNA-EVAL-DIVIDE-BY-ZERO"),
        ("{ let x = 1; x = 2; x }", "ORNA-EVAL-PARSE"),
        ("fn x() = 1", "ORNA-EVAL-PARSE"),
        ("2026-09-05", "ORNA-EVAL-UNSUPPORTED"),
    ] {
        let failure =
            evaluate_expression(source, &Environment::new(), Limits::default()).unwrap_err();
        assert_eq!(failure.code(), expected, "{source}");
        assert_eq!(failure.diagnostic().message(), "<redacted>");
        assert!(!failure.diagnostic().message().contains(source));
    }
}

#[test]
fn rejects_resource_limits_before_work_can_expand() {
    let limits = Limits {
        max_steps: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("1 + 2", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_source_bytes: 3,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("1234", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression("[1, 2]", &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
}
