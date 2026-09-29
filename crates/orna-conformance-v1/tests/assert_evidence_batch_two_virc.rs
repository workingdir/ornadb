use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_semantic_v1::{
    DIAG_ASSERTION_ONE_TABLE, DIAG_ASSERTION_SCOPE, ModuleInput, analyze,
};

const RUNTIME_FIXTURE: &str = include_str!("fixtures/assert-evidence-batch-two/runtime-boundaries.orna");

fn execute(
    entry: &str,
) -> (StageOutcome<orna_foundation_v1::Diagnostic>, TransactionalEvaluator) {
    let mut runtime = TransactionalEvaluator::new(entry, Limits::default());
    let unit = SourceUnit {
        fixture_id: format!("assert-evidence-batch-two-{entry}"),
        source_id: "assert-evidence-batch-two/runtime-boundaries.orna".into(),
        parse_as: "module_unit".into(),
        source: RUNTIME_FIXTURE.into(),
    };
    let outcome = runtime.execute_source(&unit);
    (outcome, runtime)
}

#[test]
fn assert_004_checks_the_complete_unpublished_candidate_relation() {
    let (outcome, runtime) = execute("mutate_candidate");
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Note", &Value::int(3.into())).is_some());
    assert!(runtime.committed_row("Note", &Value::int(1.into())).is_none());
}

#[test]
fn assert_005_includes_insert_update_delete_and_rekey_in_candidate_evidence() {
    let (outcome, runtime) = execute("mutate_candidate");
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    assert!(runtime.committed_row("Note", &Value::int(3.into())).is_some());
    assert!(runtime.committed_row("Note", &Value::int(2.into())).is_none());
    assert!(runtime.committed_row("Note", &Value::int(1.into())).is_none());
}

#[test]
fn assert_013_false_table_assertion_blocks_candidate_visibility() {
    let (outcome, runtime) = execute("candidate_failure");
    assert!(matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"));
    for id in [4, 5] {
        assert!(runtime.committed_row("Note", &Value::int(id.into())).is_none(), "candidate row {id} became visible");
    }
}

#[test]
fn assert_014_table_assertion_failure_rolls_back_all_candidate_writes() {
    let (outcome, runtime) = execute("candidate_failure");
    assert!(matches!(outcome, StageOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(runtime.committed_row("Note", &Value::int(4.into())), None);
    assert_eq!(runtime.committed_row("Note", &Value::int(5.into())), None);
}

#[test]
fn assert_021_valid_module_assertion_resolves_two_distinct_tables() {
    let source = include_str!("fixtures/reference/examples/valid/cross-table-assertion.orna");
    let analysis = analyze(&[ModuleInput::new("cross-table-assertion.orna", source)]);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    let plans = analysis.assertions.values().flat_map(|plans| plans.iter()).collect::<Vec<_>>();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].dependencies.len(), 2, "the plan names two distinct table dependencies");
}

#[test]
fn assert_022_module_assertion_with_one_table_dependency_is_rejected() {
    let source = include_str!("fixtures/reference/examples/invalid/module-single-table-assertion.orna");
    let analysis = analyze(&[ModuleInput::new("module-single-table-assertion.orna", source)]);
    assert!(analysis.diagnostics.iter().any(|diagnostic| diagnostic.code() == DIAG_ASSERTION_ONE_TABLE), "{:?}", analysis.diagnostics);
}

#[test]
fn assert_023_module_assertion_without_table_dependencies_is_rejected() {
    let source = include_str!("fixtures/reference/examples/invalid/module-zero-table-assertion.orna");
    let analysis = analyze(&[ModuleInput::new("module-zero-table-assertion.orna", source)]);
    assert!(analysis.diagnostics.iter().any(|diagnostic| diagnostic.code() == DIAG_ASSERTION_SCOPE), "{:?}", analysis.diagnostics);
}

#[test]
fn assert_026_changed_dependency_tables_trigger_the_module_assertion() {
    let (outcome, runtime) = execute("module_failure");
    assert!(matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"), "{outcome:?}");
    assert_eq!(runtime.committed_row("Book", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Loan", &Value::int(8.into())), None);
}

#[test]
fn assert_027_table_assertions_fail_before_module_assertions() {
    let (outcome, runtime) = execute("table_and_module_failure");
    assert!(matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"), "first failure must be the table-owned assertion: {outcome:?}");
    assert_eq!(runtime.committed_row("Book", &Value::int(10.into())), None);
    assert_eq!(runtime.committed_row("Loan", &Value::int(11.into())), None);
}

#[test]
fn assert_029_false_module_assertion_rolls_back_the_complete_database_candidate() {
    let (outcome, runtime) = execute("module_failure");
    assert!(matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"), "{outcome:?}");
    assert_eq!(runtime.committed_row("Book", &Value::int(7.into())), None);
    assert_eq!(runtime.committed_row("Loan", &Value::int(8.into())), None);
}
