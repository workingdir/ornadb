use orna_conformance_v1::{
    BoundedEvaluator, Corpus, EvidenceStatus, Harness, ProjectUnit, RuntimeAdapter,
    RuntimeEvaluator, Scenario, SemanticAdapter, SourceUnit, StageOutcome,
};
use orna_foundation_v1::Diagnostic;

#[test]
fn semantic_adapter_executes_the_v1_analyzer_with_logical_fixture_names() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("minimal fixture");
    // The analyzer really runs, and honestly exposes that its current v1
    // contract has no `std` prelude/catalogue adapter for this corpus.
    assert_eq!(fixture.stages[1].status, EvidenceStatus::Failed);
    assert_eq!(
        fixture.stages[1].diagnostic.as_ref().unwrap()["code"],
        "ORNA-S012-UNRESOLVED"
    );
    assert!(!report.semantic_evidence.is_empty());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Skipped)
    );
    assert!(report.scenarios.iter().all(|scenario| {
        scenario.detail.contains("runtime-v1") && !scenario.detail.contains("/home/")
    }));
}

#[derive(Default)]
struct RecordingRuntime {
    calls: usize,
}
impl RuntimeEvaluator for RecordingRuntime {
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
}

#[test]
fn runtime_adapter_has_an_executable_seam_but_lazy_semantic_failures_precede_it() {
    let mut adapter = RuntimeAdapter::new(RecordingRuntime::default());
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut adapter);
    let runtime = adapter.into_runtime();
    assert!(runtime.calls >= report.scenarios.len());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Passed)
    );
}

#[test]
fn bounded_evaluator_executes_expression_units_and_redacts_failures() {
    let mut evaluator = BoundedEvaluator::default();
    let valid = SourceUnit {
        fixture_id: "test-valid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: std.math.increment(1) }".into(),
    };
    assert_eq!(evaluator.evaluate(&valid), StageOutcome::Passed);

    let invalid = SourceUnit {
        fixture_id: "test-invalid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: missing }".into(),
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&invalid) else {
        panic!("unknown name must fail");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-NAME");
    assert_eq!(diagnostic.message(), "<redacted>");
}
