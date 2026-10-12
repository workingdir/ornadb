use orna_evaluator_v1::{Environment, Limits, evaluate_expression};
use orna_value_v1::{Raw, Value};

fn evaluate(source: &str, limits: Limits) -> Result<Value, orna_evaluator_v1::EvaluationError> {
    evaluate_expression(source.trim(), &Environment::new(), limits)
}

#[test]
fn power_is_right_associative_and_parentheses_override_it() {
    assert_eq!(
        evaluate(
            include_str!("fixtures/power_operator/right_associative.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(512.into())
    );
    assert_eq!(
        evaluate(
            include_str!("fixtures/power_operator/parenthesized_left.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(64.into())
    );
}

#[test]
fn integer_power_checks_digit_budget_and_rejects_negative_exponent() {
    let limits = Limits {
        max_integer_digits: 2,
        ..Limits::default()
    };
    assert_eq!(
        evaluate(
            include_str!("fixtures/power_operator/integer_overflow.orna"),
            limits,
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate(
            include_str!("fixtures/power_operator/integer_negative_exponent.orna"),
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn float_power_uses_real_exponentiation_and_rejects_negative_base() {
    match evaluate(
        include_str!("fixtures/power_operator/float_power.orna"),
        Limits::default(),
    )
    .unwrap()
    .raw()
    {
        Raw::Float(bits) => assert!((f64::from_bits(*bits) - 8.0).abs() < 1e-12),
        other => panic!("expected float result, got {other:?}"),
    }
    assert_eq!(
        evaluate(
            include_str!("fixtures/power_operator/float_negative_base.orna"),
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-TYPE"
    );
}
