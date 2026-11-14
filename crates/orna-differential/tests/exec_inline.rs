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
        ExecOutcome::Ran { outcome, .. } => outcome.expect("a module case has an outcome"),
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
            ExecOutcome::Ran { outcome: Some(Err(code)), .. } if code == "ORNA-EVAL-CANCELLED"
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
        matches!(
            outcome,
            ExecOutcome::Ran {
                outcome: Some(Ok(_)),
                ..
            }
        ),
        "unexpected outcome {outcome:?}"
    );
}

#[test]
fn the_family_of_an_inline_expression_is_expression() {
    assert_eq!(expression_case("family", "1").family, Family::Expression);
}

fn module_case(id: &str, text: &str) -> Case {
    Case::module(
        id.to_owned(),
        Family::Standard,
        Source {
            path: format!("inline/{id}.orna"),
            logical_path: format!("inline/{id}.orna"),
            text: text.to_owned(),
        },
    )
}

fn executor_outcome(case: &Case) -> Result<Vec<u8>, String> {
    match observe_exec::run_case(case) {
        ExecOutcome::Ran { outcome, .. } => {
            outcome.unwrap_or_else(|| panic!("{}: a module case has no outcome", case.id))
        }
        ExecOutcome::NotYet(reason) => {
            panic!("{}: the executor did not run the case: {reason}", case.id)
        }
    }
}

fn old_outcome(case: &Case) -> Option<Result<Vec<u8>, String>> {
    observe_old::observe(case)
        .expect("the old pipeline observes the case")
        .outcome
}

// D10 (docs/language/DECIDED.md): a closure captures the variable's slot, so the executor sees
// reassignments made after capture. The old pipeline copied the value at closure creation or rejected
// the assignment. These are the expected differences, recorded under bead ornadb-csenl.10.
// The cases are modules: the old pipeline parses a block only inside a function body.

#[test]
fn d10_a_closure_reads_the_slot_reassigned_after_capture() {
    let text = "fn main() { let seed = 1; let read = () => seed; seed = 9; read() }";
    let executor = executor_outcome(&module_case("d10-read-after-reassignment", text));
    let old = old_outcome(&module_case("d10-read-after-reassignment-old", text));
    assert_eq!(
        executor,
        executor_outcome(&module_case(
            "d10-read-after-reassignment-value",
            "fn main() { let seed = 9; seed }"
        )),
        "D10: the closure reads the reassigned slot"
    );
    assert_eq!(
        old,
        old_outcome(&module_case(
            "d10-read-after-reassignment-old-copy",
            "fn main() { let seed = 1; seed }"
        )),
        "the old pipeline copied the captured value at creation"
    );
    assert_ne!(Some(executor), old, "the difference is real");
}

// Design 03, "Concurrent bodies capture snapshots": assigning to an outer variable inside a body started
// by `parallel` is a compile error. The executor must not run such a program, so the lowering diagnostic
// keeps it at `NotYet`.
#[test]
fn assigning_an_outer_variable_in_a_parallel_body_is_a_lowering_error() {
    let case = module_case(
        "concurrent-assign-outer",
        "fn main() { let count = 0; parallel([() => { count += 1; }]); count }",
    );
    assert!(
        matches!(observe_exec::run_case(&case), ExecOutcome::NotYet(_)),
        "the executor must not run a concurrent body that assigns an outer variable"
    );
}

#[test]
fn d10_a_closure_mutates_the_captured_slot() {
    let text = "fn main() { let seed = 1; let mutate = () => { let bump = 1; seed = seed + bump; seed }; mutate() }";
    assert_eq!(
        executor_outcome(&module_case("d10-mutate-capture", text)),
        executor_outcome(&module_case(
            "d10-mutate-capture-value",
            "fn main() { let seed = 2; seed }"
        )),
        "D10: the closure assigns the shared slot"
    );
    assert_eq!(
        old_outcome(&module_case("d10-mutate-capture-old", text)),
        Some(Err("ORNA-EVAL-IMMUTABLE-CAPTURE".to_owned())),
        "the old pipeline rejected assignment to a captured variable"
    );
}
