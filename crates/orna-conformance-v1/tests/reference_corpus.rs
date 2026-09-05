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
            serde_json::json!({"code": "WRONG", "message": "wrong", "span": {"start": 0}}),
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
