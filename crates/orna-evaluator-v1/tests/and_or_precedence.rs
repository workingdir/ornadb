use orna_evaluator_v1::{Environment, Limits, evaluate_expression};
use orna_value_v1::{Raw, Value};

#[test]
fn and_binds_tighter_than_or() {
    // Left-to-right at equal precedence would give (true || false) && false.
    let source = include_str!("fixtures/and_or_precedence/and_binds_tighter.orna");
    assert_eq!(
        evaluate_expression(source.trim(), &Environment::new(), Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
}
