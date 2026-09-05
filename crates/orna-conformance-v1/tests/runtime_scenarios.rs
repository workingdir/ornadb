use orna_conformance_v1::{
    BoundedEvaluator, Corpus, EvidenceStatus, Harness, RuntimeAdapter, RuntimeEvaluator, Scenario,
    StageOutcome,
};
use orna_evaluator_v1::Limits;

fn scenario(id: &str) -> Scenario {
    let corpus = Corpus::load_default().expect("frozen corpus loads");
    serde_json::from_value(
        corpus.scenarios["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|scenario| scenario["id"] == id)
            .unwrap()
            .clone(),
    )
    .unwrap()
}

#[test]
fn pipeline_insertion_executes_ordinary_function_argument_order() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("PIPE-001"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    let mut runtime = BoundedEvaluator::new(Limits {
        max_steps: 1,
        ..Limits::default()
    });
    assert!(matches!(
        runtime.run_scenario(&scenario("PIPE-001")),
        StageOutcome::Failed(_)
    ));
    let mut changed = scenario("PIPE-001");
    changed
        .when
        .push("an additional lowering obligation".into());
    assert!(matches!(
        BoundedEvaluator::default().run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn let_rebinding_executes_exact_runtime_checks_and_migration_diagnostic() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("LET-REBIND-091"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn scenario_limit_failure_is_not_a_pass() {
    for limits in [
        Limits {
            max_source_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_steps: 1,
            ..Limits::default()
        },
    ] {
        let mut runtime = BoundedEvaluator::new(limits);
        let outcome = runtime.run_scenario(&scenario("LET-REBIND-091"));
        assert!(
            matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
            "{outcome:?}"
        );
    }
}

#[test]
fn changed_or_unimplemented_scenario_contracts_are_not_reported_as_executed() {
    let mut changed = scenario("LET-REBIND-091");
    changed
        .then
        .push("an additional unimplemented obligation".into());
    let mut runtime = BoundedEvaluator::default();
    assert!(matches!(
        runtime.run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
    assert!(matches!(
        runtime.run_scenario(&scenario("TXN-001")),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn harness_distinguishes_executed_rebinding_from_unimplemented_scenarios() {
    let report = Harness::new(Corpus::load_default().unwrap())
        .run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    let executed = report
        .scenarios
        .iter()
        .filter(|scenario| scenario.status == EvidenceStatus::Passed)
        .collect::<Vec<_>>();
    assert_eq!(
        executed
            .iter()
            .map(|scenario| scenario.scenario.as_str())
            .collect::<Vec<_>>(),
        ["LET-REBIND-091", "PIPE-001"]
    );
    assert_eq!(
        report
            .scenarios
            .iter()
            .filter(|scenario| scenario.status == EvidenceStatus::Skipped)
            .count(),
        142
    );
}
