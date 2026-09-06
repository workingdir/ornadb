use orna_conformance_v1::*;

#[test]
fn loads_the_complete_unchanged_reference_corpus() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    assert_eq!(corpus.manifest.fixtures.len(), 167);
    assert_eq!(corpus.invalid_metadata.fixtures.len(), 80);
    assert_eq!(corpus.vectors.len(), 6);
    assert_eq!(corpus.requirements.len(), 870);
    assert_eq!(
        corpus.diagnostics["examples/invalid/unsafe-row-key-repeat.orna"].failing_phase,
        "row-validation"
    );
    assert_eq!(corpus.publication_digests.len(), 46);
}

#[test]
fn no_adapter_cannot_create_runtime_passes() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(report.fixtures.len(), 167);
    assert!(report.runtime_evidence.is_empty());
    assert!(!report.skipped_evidence.is_empty());
    assert!(
        report
            .model_evidence
            .iter()
            .all(|e| e.status == EvidenceStatus::Specified)
    );
    assert_eq!(report.scenarios.len(), 144);
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Skipped)
    );
}

struct ParseFail;
impl ConformanceAdapter for ParseFail {
    type Diagnostic = serde_json::Value;
    fn diagnostic_code(&self, diagnostic: &Self::Diagnostic) -> String {
        diagnostic["code"].as_str().unwrap().into()
    }
    fn diagnostic_message(&self, diagnostic: &Self::Diagnostic) -> String {
        diagnostic["message"].as_str().unwrap().into()
    }
    fn parse(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Failed(
            serde_json::json!({"code": "WRONG", "message": "source-only-should-never-escape", "span": {"start": 0}}),
        )
    }
    fn resolve(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn typecheck(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn parse_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn resolve_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn typecheck_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
}

#[test]
fn row_validation_is_a_real_distinct_stage() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    let case = report
        .fixtures
        .iter()
        .find(|case| case.fixture == "invalid/unsafe-row-key-repeat.orna")
        .unwrap();
    assert!(
        case.stages
            .iter()
            .any(|stage| stage.stage == Some(Stage::RowValidation))
    );
}

#[test]
fn primary_diagnostic_mismatch_is_evidence_not_a_pass() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = ParseFail;
    let report = Harness::new(corpus).run(&mut adapter);
    let case = report
        .fixtures
        .iter()
        .find(|case| case.fixture == "invalid/affine-addition.orna")
        .unwrap();
    assert!(!case.passed);
    assert!(case.stages[0].detail.contains("NOT satisfied"));
}

struct FailBeforeResolve {
    later_calls: usize,
}
impl ConformanceAdapter for FailBeforeResolve {
    type Diagnostic = serde_json::Value;
    fn diagnostic_code(&self, _: &Self::Diagnostic) -> String {
        "E".into()
    }
    fn diagnostic_message(&self, _: &Self::Diagnostic) -> String {
        "failure".into()
    }
    fn parse(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Failed(serde_json::json!({"code":"E"}))
    }
    fn resolve(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        self.later_calls += 1;
        StageOutcome::Passed
    }
    fn typecheck(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        self.later_calls += 1;
        StageOutcome::Passed
    }
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        self.later_calls += 1;
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        self.later_calls += 1;
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        self.later_calls += 1;
        StageOutcome::Passed
    }
}

#[test]
fn stages_are_lazy_after_the_first_failure() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = FailBeforeResolve { later_calls: 0 };
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(adapter.later_calls, 0);
    assert!(report.fixtures.iter().all(|fixture| !fixture.passed));
}

#[test]
fn skipped_required_work_is_not_a_pass() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = SkippingAdapter;
    assert!(
        Harness::new(corpus)
            .run(&mut adapter)
            .fixtures
            .iter()
            .all(|fixture| !fixture.passed)
    );
}

#[test]
fn normative_digest_mismatch_and_load_errors_do_not_expose_machine_paths() {
    let root = std::env::temp_dir().join(format!("orna-conformance-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("member"), b"actual").unwrap();
    let digests = std::collections::BTreeMap::from([("member".into(), "0".repeat(64))]);
    assert_eq!(
        verify_normative_members(&root, &digests)
            .unwrap_err()
            .to_string(),
        "normative member digest mismatch: member"
    );
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(
        Corpus::load("/definitely-not-an-authoritative-reference-tree")
            .unwrap_err()
            .to_string(),
        "cannot read reference JSON: tests/conformance-manifest.json"
    );
}

#[test]
fn report_redacts_adapter_diagnostics_even_when_an_adapter_returns_source() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut ParseFail);
    let serialized = serde_json::to_string(&report).expect("report serializes");
    assert!(!serialized.contains("source-only-should-never-escape"));
    assert!(!serialized.contains("/home/"));
    let diagnostic = report.fixtures[0].stages[0]
        .diagnostic
        .as_ref()
        .expect("failed stage has a redacted diagnostic");
    assert_eq!(diagnostic["redacted"], true);
    assert_eq!(diagnostic["spans"], serde_json::json!([]));
}

struct ProjectProbe {
    project: Option<(usize, usize, usize)>,
}
impl ConformanceAdapter for ProjectProbe {
    type Diagnostic = serde_json::Value;
    fn diagnostic_code(&self, _: &Self::Diagnostic) -> String {
        String::new()
    }
    fn diagnostic_message(&self, _: &Self::Diagnostic) -> String {
        String::new()
    }
    fn parse_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn resolve_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn typecheck_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn parse(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn resolve(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn typecheck(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        self.project = Some((
            project.modules.len(),
            project.loose_rows.len(),
            project.expectations.steps.len(),
        ));
        assert!(!project.expectations.environment.network);
        assert!(!project.expectations.environment.credentials);
        StageOutcome::Passed
    }
}

#[test]
fn project_adapter_receives_reachable_modules_rows_and_typed_expectations() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut adapter = ProjectProbe { project: None };
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(adapter.project, Some((5, 0, 4)));
    let project = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "PROJECT-REFERENCE")
        .unwrap();
    assert!(project.stages.iter().all(|stage| matches!(
        stage.requirement_mapping,
        RequirementMapping::Unmapped { .. }
    )));
    assert!(report.coverage.unmapped_stage_evidence > 0);
}

#[test]
fn engine_witnesses_require_an_exact_expectation_satisfied_fixture_stage() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads"));
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);
    let binding = FixtureStageBinding {
        requirement_id: "ORNA-SOURCE-001".into(),
        fixture_id: "valid/minimal-root.orna".into(),
        fixture_path: "examples/valid/minimal-root.orna".into(),
        stage: Stage::Parse,
        implementation_ref: "orna.syntax.module-entrypoint".into(),
        test_ref: "conformance.reference_corpus.engine_witnesses".into(),
    };
    let witnesses = harness
        .engine_witnesses(&report, std::slice::from_ref(&binding))
        .expect("reviewed executed stage becomes a witness");
    assert_eq!(witnesses.witnesses().len(), 1);
    assert_eq!(
        witnesses.witnesses()[0].fixture_path(),
        "examples/valid/minimal-root.orna"
    );

    let mut bad_path = binding;
    bad_path.fixture_path = "examples/valid/not-the-fixture.orna".into();
    assert!(
        harness
            .engine_witnesses(&report, std::slice::from_ref(&bad_path))
            .is_err()
    );

    let project_evaluation = FixtureStageBinding {
        requirement_id: "ORNA-STREAM-002".into(),
        fixture_id: "PROJECT-REFERENCE".into(),
        fixture_path: "examples/reference".into(),
        stage: Stage::Evaluate,
        implementation_ref: "orna.project-runtime".into(),
        test_ref: "conformance.project_runtime".into(),
    };
    assert!(
        harness
            .engine_witnesses(&report, std::slice::from_ref(&project_evaluation))
            .is_err()
    );
}
