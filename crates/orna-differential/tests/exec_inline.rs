//! Inline differential cases for the executor's failure paths (`fail`, `|?`)
//! and cancellation, compared with the old pipeline. The corpus carries none
//! of these constructs, so each case is built here from one expression.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use orna_differential::case::{Case, Family, Source};
use orna_differential::observe_exec::{self, ExecOutcome};
use orna_differential::observe_old;

fn expression_case(id: &str, text: &str) -> Case {
    Case::expression(Source {
        path: format!("inline/{id}.orna"),
        logical_path: format!("inline/{id}.orna"),
        text: text.to_owned(),
    })
}

fn assert_agrees(id: &str, text: &str) {
    let case = expression_case(id, text);
    let executor = match observe_exec::run_case(&case) {
        ExecOutcome::Ran { outcome, .. } => outcome,
        ExecOutcome::NotYet(reason) => panic!("{id}: the executor did not run the case: {reason}"),
    };
    let old = observe_old::observe(&case)
        .expect("the old pipeline observes the case")
        .outcome;
    assert_eq!(
        executor,
        old.expect("the old pipeline has an outcome"),
        "{id}: the executor disagrees with the old pipeline"
    );
}

#[test]
fn fail_with_a_value_matches_the_old_pipeline() {
    assert_agrees("fail-value", "fail(\"ORNA-EVAL-TEST\")");
}

#[test]
fn recover_catches_a_division_failure_like_the_old_pipeline() {
    assert_agrees("recover-divide", "((1 / 0) |? (failure => 7)) + 2");
}

#[test]
fn recover_success_skips_the_handler_like_the_old_pipeline() {
    assert_agrees(
        "recover-success",
        "((1 / 0) |? (failure => 7)) |? (next_failure => 99)",
    );
}

#[test]
fn a_failure_reraised_from_a_handler_matches_the_old_pipeline() {
    assert_agrees(
        "fail-from-handler",
        "(1 / 0) |? (failure => fail(failure)) |? (failure => 7)",
    );
}

#[test]
fn a_cancelled_run_reports_cancellation_not_a_value() {
    let case = expression_case("cancelled", "1 + 1");
    let flag = Arc::new(AtomicBool::new(true));
    let outcome = observe_exec::run_case_cancellable(&case, flag);
    assert!(
        matches!(
            &outcome,
            ExecOutcome::Ran { outcome: Err(code), .. } if code == "ORNA-EVAL-CANCELLED"
        ),
        "unexpected outcome {outcome:?}"
    );
}

#[test]
fn a_run_whose_flag_is_clear_is_not_cancelled() {
    let case = expression_case("not-cancelled", "1 + 1");
    let flag = Arc::new(AtomicBool::new(false));
    let outcome = observe_exec::run_case_cancellable(&case, flag);
    assert!(
        matches!(outcome, ExecOutcome::Ran { outcome: Ok(_), .. }),
        "unexpected outcome {outcome:?}"
    );
}

#[test]
fn the_family_of_an_inline_expression_is_expression() {
    assert_eq!(expression_case("family", "1").family, Family::Expression);
}
