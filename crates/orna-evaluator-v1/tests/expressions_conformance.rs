//! Evidence-graded expression audit against the frozen 1.0.0 chapter.
//!
//! - `ORNA-OP-001` (`source/06-expressions.md:301-305`) specifies right
//!   associativity for `^`; integer and float edge behavior follows the
//!   evaluator's documented pragmatic choices where the chapter is silent.
//! - **B, coverage gap:** `ORNA-CFLOW-002`
//!   (`source/06-expressions.md:211`) requires left-to-right list and record
//!   expression evaluation. The evaluator has ordered collection evaluation;
//!   these fixtures distinguish it from evaluating a later failing expression
//!   first. Existing function-argument coverage did not cover these forms.
//! - **B, coverage gap:** `ORNA-CASE-003/005`
//!   (`source/06-expressions.md:121-125`) require source-order arm selection
//!   and guards only after a pattern match. The fixtures cover pattern misses,
//!   a successful earlier arm, and a false guard followed by a matching arm.
//! - **B, coverage gap:** `ORNA-COALESCE-002`
//!   (`source/06-expressions.md:346`) specifies right association. Existing
//!   coverage checked laziness, while this fixture distinguishes a chained
//!   expression's grouping.
//! - **B, coverage gap:** `ORNA-PIPE-007/008`
//!   (`source/06-expressions.md:324-326`) and `ORNA-ERR-011/012`
//!   (`source/06-expressions.md:413-415`) define failure short-circuiting,
//!   one-shot recovery, and continuation after successful recovery. Existing
//!   recovery tests covered re-emission and nested boundaries; these fixtures
//!   exercise an enclosing expression and skipped work after recovery.

use orna_evaluator_v1::{Environment, EvaluationError, Limits, evaluate_expression};
use orna_value_v1::Value;

fn evaluate_fixture(
    source: &str,
    environment: &Environment,
) -> Result<Value, EvaluationError> {
    evaluate_expression(source.trim(), environment, Limits::default())
}

fn failure_code(source: &str) -> String {
    evaluate_fixture(source, &Environment::new())
        .expect_err("fixture should fail")
        .code()
        .to_owned()
}

// Coverage gap (partial evidence): ORNA-CFLOW-002 (source/06-expressions.md:211).
#[test]
fn collection_elements_and_record_fields_evaluate_left_to_right() {
    for source in [
        include_str!("fixtures/expressions_conformance/list_left_to_right.orna"),
        include_str!("fixtures/expressions_conformance/tuple_left_to_right.orna"),
        include_str!("fixtures/expressions_conformance/record_left_to_right.orna"),
    ] {
        assert_eq!(failure_code(source), "ORNA-EVAL-DIVIDE-BY-ZERO");
    }
}

// Coverage gap (partial evidence): ORNA-CASE-003/005 (source/06-expressions.md:121-125).
#[test]
fn case_guards_run_after_pattern_match_and_arms_keep_source_order() {
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/case_skips_guard_on_pattern_miss.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(2.into())
    );
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/case_skips_later_arm_after_match.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(1.into())
    );
    assert_eq!(
        failure_code(include_str!(
            "fixtures/expressions_conformance/case_evaluates_next_matching_guard.orna"
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

// Coverage gap (partial evidence): ORNA-COALESCE-002 (source/06-expressions.md:346).
#[test]
fn coalescing_chains_associate_to_the_right() {
    let environment = Environment::from([
        ("left".into(), Value::option(None).unwrap()),
        (
            "middle".into(),
            Value::option(Some(Value::int(7.into()))).unwrap(),
        ),
        ("fallback".into(), Value::int(9.into())),
    ]);
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/coalesce_right_associative.orna"),
            &environment,
        )
        .unwrap(),
        Value::int(7.into())
    );
}

// Coverage gap (partial evidence): ORNA-PIPE-007/008 (source/06-expressions.md:324-330),
// ORNA-ERR-011/012 (source/06-expressions.md:413-415).
#[test]
fn pipelines_preserve_failure_and_successful_recovery_continues_once() {
    assert_eq!(
        failure_code(include_str!(
            "fixtures/expressions_conformance/pipeline_does_not_run_after_failure.orna"
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/recovery_resumes_outer_expression.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(9.into())
    );
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/recovery_success_skips_next_handler.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(7.into())
    );
}

// ORNA-OP-001 (source/06-expressions.md:301-305).
#[test]
fn arithmetic_precedence_and_right_associative_exponentiation_work() {
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/operator_precedence_mixed.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(7.into())
    );
    assert_eq!(
        evaluate_fixture(
            include_str!(
                "fixtures/expressions_conformance/operator_exponent_right_associative.orna"
            ),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(512.into())
    );
    assert_eq!(
        evaluate_fixture(
            include_str!("fixtures/expressions_conformance/operator_multiplication_division_left_associative.orna"),
            &Environment::new(),
        )
        .unwrap(),
        Value::int(8.into())
    );
}
