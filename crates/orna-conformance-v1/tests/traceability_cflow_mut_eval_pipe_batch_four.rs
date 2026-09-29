//! Bounded executable witnesses for selected CFLOW/MUT/EVAL/PIPE gap-tail rows.
//! The frozen evidence register is untouched. ORNA-TEST-004 distinguishes a
//! specified clause, an existing test, and the result of executing that test;
//! each row below records only the particular observation made here.

use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::{Environment, Limits, evaluate_expression};
use orna_foundation_v1::{Diagnostic, Value};
use orna_semantic_v1::{DIAG_TYPE, ModuleInput, analyze};
use orna_syntax_v1::parse_module;

fn run_fixture(
    fixture_id: &str,
    source: &str,
) -> (TransactionalEvaluator, StageOutcome<Diagnostic>) {
    let unit = SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: source.into(),
    };
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let outcome = evaluator.execute_source(&unit);
    (evaluator, outcome)
}

// ORNA-CFLOW-005, source/06-expressions.md:217.
#[test]
fn local_let_slot_can_be_reassigned_before_a_table_write() {
    let (runtime, outcome) = run_fixture(
        "traceability-b4-cflow-assignment",
        include_str!("fixtures/traceability-b4-cflow-assignment.orna"),
    );

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(
        runtime
            .committed_row("Note", &Value::int(8.into()))
            .is_some(),
        "the replacement slot value reached the inserted key"
    );
    assert_eq!(
        runtime.committed_row("Note", &Value::int(7.into())),
        None,
        "the initial slot value was not used for the inserted key"
    );
}

// ORNA-CFLOW-006, source/06-expressions.md:219.
#[test]
fn assignment_is_rejected_when_used_as_a_returned_expression() {
    let parsed = parse_module(include_str!(
        "fixtures/traceability-b4-cflow-assignment-expression-invalid.orna"
    ));
    assert!(
        !parsed.diagnostics.is_empty(),
        "assignment in a returned expression must not parse as a valid expression"
    );
}

// ORNA-CFLOW-010, source/06-expressions.md:233.
#[test]
fn braced_if_statement_without_semicolon_parses_before_a_later_item() {
    let parsed = parse_module(include_str!(
        "fixtures/traceability-b4-cflow-control-statement.orna"
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

// ORNA-MUT-004, source/07-tables.md:420.
#[test]
fn ordinary_update_cannot_replace_a_primary_key() {
    let (runtime, outcome) = run_fixture(
        "traceability-b4-mut-key-update-invalid",
        include_str!("fixtures/traceability-b4-mut-key-update-invalid.orna"),
    );

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic) if diagnostic.code() == "ORNA-S021-TYPE"
        ),
        "primary-key patch must be rejected during source admission: {outcome:?}"
    );
    assert_eq!(
        runtime.committed_row("Note", &Value::int(7.into())),
        None,
        "the failed activation published neither the original nor patched key"
    );
    assert_eq!(
        runtime.committed_row("Note", &Value::int(8.into())),
        None,
        "the failed activation did not publish the attempted replacement key"
    );
}

// ORNA-MUT-006, source/07-tables.md:424.
#[test]
fn rekey_collision_fails_without_publishing_candidate_rows() {
    let (runtime, outcome) = run_fixture(
        "traceability-b4-mut-rekey-collision",
        include_str!("fixtures/traceability-b4-mut-rekey-collision.orna"),
    );

    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-DUPLICATE"
        ),
        "rekey collision must report a duplicate-key failure: {outcome:?}"
    );
    for key in [7, 8] {
        assert_eq!(
            runtime.committed_row("Note", &Value::int(key.into())),
            None,
            "candidate row {key} escaped the failed activation"
        );
    }
}

// ORNA-EVAL-003, source/14-pages.md:74.
#[test]
fn bounded_local_eval_witness_commits_one_activation_write() {
    let (runtime, outcome) = run_fixture(
        "traceability-b4-eval-cwd-write",
        include_str!("fixtures/traceability-b4-eval-cwd-write.orna"),
    );

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(
        runtime
            .committed_row("Note", &Value::int(7.into()))
            .is_some()
    );
    // This is TransactionalEvaluator evidence for one local activation. It
    // does not establish production remote-Eval host admission or transport.
}

// ORNA-PIPE-003, source/06-expressions.md:311.
#[test]
fn success_pipeline_inserts_the_value_before_explicit_arguments() {
    let (_, outcome) = run_fixture(
        "traceability-b4-pipe-call-arguments",
        include_str!("fixtures/traceability-b4-pipe-call-arguments.orna"),
    );

    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

// ORNA-PIPE-005, source/06-expressions.md:315.
#[test]
fn direct_anonymous_pipeline_stage_requires_parentheses() {
    let valid = parse_module(include_str!(
        "fixtures/traceability-b4-pipe-anonymous-valid.orna"
    ));
    assert!(valid.diagnostics.is_empty(), "{:?}", valid.diagnostics);

    let invalid = parse_module(include_str!(
        "fixtures/traceability-b4-pipe-anonymous-invalid.orna"
    ));
    assert!(!invalid.diagnostics.is_empty());
}

// ORNA-PIPE-009, source/06-expressions.md:328.
#[test]
fn recovery_value_type_unifies_with_the_success_pipeline_type() {
    let matching = analyze(&[ModuleInput::new(
        "traceability-b4-pipe-recovery-unifies.orna",
        include_str!("fixtures/traceability-b4-pipe-recovery-unifies.orna"),
    )]);
    assert!(matching.is_ok(), "{:?}", matching.diagnostics);

    let mismatched = analyze(&[ModuleInput::new(
        "traceability-b4-pipe-recovery-mismatched.orna",
        include_str!("fixtures/traceability-b4-pipe-recovery-mismatched.orna"),
    )]);
    assert!(
        mismatched
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        mismatched.diagnostics
    );
}

// ORNA-PIPE-010, source/06-expressions.md:330.
#[test]
fn explicit_failure_from_a_recovery_handler_keeps_the_original_failure() {
    let failure = evaluate_expression(
        include_str!("fixtures/traceability-b4-pipe-failure-propagates.orna").trim(),
        &Environment::new(),
        Limits::default(),
    )
    .expect_err("fail(error) must propagate the handled failure");
    assert_eq!(failure.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
}
