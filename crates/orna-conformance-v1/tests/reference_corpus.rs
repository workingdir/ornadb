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
fn requirement_evidence_keeps_all_authoritative_not_executed_markers() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(corpus.root.join("tests/requirement-evidence.json"))
            .expect("requirement evidence reads"),
    )
    .expect("requirement evidence is valid JSON");
    let entries = value["requirements"]
        .as_array()
        .expect("requirement evidence entries are an array");

    assert_eq!(entries.len(), 870);
    assert!(entries.iter().all(|entry| {
        entry["implementation_result"] == "not executed"
            && entry["full_implementation_coverage_claimed"] == false
            && entry["tests"].as_array().is_some_and(|tests| {
                tests.iter().all(|test| {
                    test["status"] == "planned"
                        && !test.as_object().is_some_and(|test| {
                            test.contains_key("fixture") || test.contains_key("path")
                        })
                })
            })
    }));

    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(report.coverage.mapped_stage_evidence, 0);
    assert!(
        report
            .implementation_claim
            .executed_scenario_contracts
            .is_empty()
    );
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
fn reference_project_runtime_adapter_reports_exactly_separate_evidence() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let evidence = run_reference_project_runtime_adapter(&corpus);

    assert_eq!(evidence.classification, EvidenceClass::RuntimeAdapter);
    assert_eq!(evidence.specification_version, "1.0.0");
    assert_eq!(evidence.profile, "reference-project-runtime-adapter");
    assert_eq!(evidence.implementation_execution, "not executed");
    assert!(!evidence.compiler_artifact_execution);
    assert!(!evidence.full_orna_engine_conformance);
    assert_eq!(evidence.status, EvidenceStatus::Passed, "{evidence:?}");
    assert_eq!(
        evidence
            .invocations
            .iter()
            .map(|invocation| invocation.invoke.as_str())
            .collect::<Vec<_>>(),
        [
            "main.seed",
            "main.exercise",
            "sensors.ingest",
            "sensors.ingest"
        ]
    );
    assert!(
        evidence
            .invocations
            .iter()
            .all(|invocation| invocation.status == EvidenceStatus::Passed
                && !invocation.checks.is_empty())
    );
    let expected_negative_cases = serde_json::json!([
        {
            "invoke": "library.lend",
            "args": ["missing-book", "reader-2"],
            "expect": "cross-table assertion failure; no new loan"
        },
        {
            "invoke": "warehouse.transfer",
            "args": ["north", "south", "pencil", 100],
            "expect": "assertion failure; both stock rows unchanged"
        },
        {
            "invoke": "library.lend",
            "args": ["book-1", "reader-2"],
            "expect": "duplicate key; existing loan unchanged"
        }
    ]);
    let actual_negative_cases = evidence
        .negative_cases
        .iter()
        .map(|case| {
            serde_json::json!({
                "invoke": case.invoke,
                "args": case.args,
                "expect": case.expected,
                "status": case.status,
                "rollback_verified": case.rollback_verified
            })
        })
        .collect::<Vec<_>>();
    let expected_negative_cases = expected_negative_cases
        .as_array()
        .expect("negative-case oracle is an array")
        .iter()
        .cloned()
        .map(|mut case| {
            let object = case
                .as_object_mut()
                .expect("negative-case oracle entries are objects");
            object.insert("status".into(), serde_json::json!(EvidenceStatus::Passed));
            object.insert("rollback_verified".into(), serde_json::json!(true));
            case
        })
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::Value::Array(actual_negative_cases),
        serde_json::Value::Array(expected_negative_cases)
    );

    // The distinct report type/classification and explicit false flags keep
    // this adapter evidence outside the EngineWitnesses API and its claims.
    assert_ne!(evidence.classification, EvidenceClass::Runtime);
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
        implementation_ref: "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into(),
        test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::engine_witnesses_require_an_exact_expectation_satisfied_fixture_stage".into(),
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
        implementation_ref: "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into(),
        test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::engine_witnesses_require_an_exact_expectation_satisfied_fixture_stage".into(),
    };
    assert!(
        harness
            .engine_witnesses(&report, std::slice::from_ref(&project_evaluation))
            .is_err()
    );
}

#[test]
fn engine_witnesses_reject_non_repository_provenance_references() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads"));
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);
    let mut binding = FixtureStageBinding {
        requirement_id: "ORNA-SOURCE-001".into(),
        fixture_id: "valid/minimal-root.orna".into(),
        fixture_path: "examples/valid/minimal-root.orna".into(),
        stage: Stage::Parse,
        implementation_ref: "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into(),
        test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::engine_witnesses_reject_non_repository_provenance_references".into(),
    };

    for (field, invalid) in [
        (
            "implementation",
            "/tmp/implementation.rs::Harness::engine_witnesses",
        ),
        (
            "test",
            "crates/orna-conformance-v1/tests/../tests/reference_corpus.rs::regression",
        ),
    ] {
        if field == "implementation" {
            binding.implementation_ref = invalid.into();
        } else {
            binding.test_ref = invalid.into();
        }
        let error = harness
            .engine_witnesses(&report, std::slice::from_ref(&binding))
            .expect_err("engine witness provenance must remain repository-relative");
        assert_eq!(
            error,
            format!("invalid repository-relative {field} reference")
        );
        binding.implementation_ref =
            "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into();
        binding.test_ref =
            "crates/orna-conformance-v1/tests/reference_corpus.rs::engine_witnesses_reject_non_repository_provenance_references".into();
    }
}

fn date_range_implementation_bindings(
    publication_digests: &std::collections::BTreeMap<String, String>,
) -> Vec<ImplementationEvidenceBinding> {
    [
        (
            "ORNA-RANGE-001",
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds",
        ),
        (
            "ORNA-RANGE-002",
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds",
        ),
        (
            "ORNA-RANGE-003",
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds",
        ),
        (
            "ORNA-RANGE-004",
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_allow_empty_values_but_reject_mixed_bounds_and_iteration",
        ),
        (
            "ORNA-RANGE-005",
            "crates/orna-evaluator-v1/src/lib.rs::compare_values",
            "crates/orna-evaluator-v1/tests/evaluator.rs::range_ordering_validates_unbounded_endpoint_types_before_lexicographic_ordering",
        ),
        (
            "ORNA-RANGE-006",
            "crates/orna-evaluator-v1/src/lib.rs::eval_infix",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds",
        ),
        (
            "ORNA-RANGE-006",
            "crates/orna-evaluator-v1/src/lib.rs::finite_range_values",
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_allow_empty_values_but_reject_mixed_bounds_and_iteration",
        ),
    ]
    .into_iter()
    .map(
        |(requirement_id, implementation_ref, test_ref)| ImplementationEvidenceBinding {
            requirement_id: requirement_id.into(),
            publication_digests: publication_digests.clone(),
            implementation_ref: implementation_ref.into(),
            test_ref: test_ref.into(),
            source: ImplementationEvidenceSource::ProductionUnit,
            subject: test_ref.into(),
            command: "CARGO_BUILD_JOBS=6 CARGO_INCREMENTAL=0 RUSTFLAGS='-C debuginfo=0' cargo test -p orna-evaluator-v1 --test evaluator".into(),
            result: "99 passed; 0 failed".into(),
            observed_status: EvidenceStatus::Passed,
        },
    )
    .collect()
}

#[test]
fn date_range_implementation_evidence_is_pinned_partial_and_does_not_promote_the_plan() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let frozen_range_plan = corpus
        .requirement_evidence
        .requirements
        .iter()
        .find(|entry| entry.requirement == "ORNA-RANGE-006")
        .expect("range plan exists")
        .tests
        .clone();
    let bindings = date_range_implementation_bindings(&corpus.publication_digests);
    let harness = Harness::new(corpus);

    let overlay = harness
        .implementation_evidence_overlay(&bindings)
        .expect("reviewed Date range production evidence is accepted");
    assert_eq!(overlay.evidence().len(), 7);
    assert_eq!(
        overlay.aggregate(),
        ImplementationEvidenceAggregate::PartiallyExecuted
    );
    assert!(
        overlay
            .evidence()
            .iter()
            .all(|entry| entry.observed_status() == &EvidenceStatus::Passed)
    );
    assert_eq!(
        overlay.evidence()[5].test_ref(),
        "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds"
    );
    assert_eq!(
        overlay.evidence()[6].test_ref(),
        "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_allow_empty_values_but_reject_mixed_bounds_and_iteration"
    );
    let serialized = serde_json::to_value(&overlay).expect("overlay serializes");
    assert_eq!(
        serde_json::to_vec(&overlay).expect("overlay serializes deterministically"),
        serde_json::to_vec(&overlay).expect("overlay serializes deterministically")
    );
    assert_eq!(
        serialized["evidence"][0]["source"],
        serde_json::json!("production-unit")
    );
    assert_eq!(
        serialized["evidence"][0]["subject"],
        serde_json::json!(
            "crates/orna-evaluator-v1/tests/evaluator.rs::date_ranges_are_canonical_membership_values_with_optional_bounds"
        )
    );
    assert_eq!(
        serialized["evidence"][0]["command"],
        serde_json::json!(
            "CARGO_BUILD_JOBS=6 CARGO_INCREMENTAL=0 RUSTFLAGS='-C debuginfo=0' cargo test -p orna-evaluator-v1 --test evaluator"
        )
    );
    assert_eq!(
        serialized["evidence"][0]["result"],
        serde_json::json!("99 passed; 0 failed")
    );
    assert_eq!(
        serialized["evidence"][0]["observed-status"],
        serde_json::json!("passed")
    );
    assert_eq!(
        serialized["publication-digests"],
        serde_json::to_value(overlay.publication_digests()).expect("digests serialize")
    );

    let reloaded = Corpus::load_default().expect("reference corpus reloads");
    let reloaded_range_plan = reloaded
        .requirement_evidence
        .requirements
        .iter()
        .find(|entry| entry.requirement == "ORNA-RANGE-006")
        .expect("range plan remains")
        .tests
        .clone();
    assert_eq!(reloaded_range_plan, frozen_range_plan);
    assert_eq!(reloaded_range_plan[0]["status"], "planned");
}

#[test]
fn implementation_evidence_overlay_rejects_empty_bindings() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads"));

    assert!(
        harness.implementation_evidence_overlay(&[]).is_err(),
        "an empty overlay must not advertise partial execution"
    );
}

#[test]
fn implementation_evidence_overlay_rejects_unpinned_nonproduction_and_engine_inputs() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let mut binding = date_range_implementation_bindings(&corpus.publication_digests)
        .into_iter()
        .next()
        .expect("Date binding exists");
    let harness = Harness::new(corpus);

    binding.requirement_id = "ORNA-NOT-A-REQUIREMENT".into();
    assert!(
        harness
            .implementation_evidence_overlay(std::slice::from_ref(&binding))
            .is_err()
    );

    binding.requirement_id = "ORNA-RANGE-001".into();
    let digest = binding
        .publication_digests
        .values_mut()
        .next()
        .expect("publication inventory is populated");
    let replacement = if digest.starts_with('0') { "1" } else { "0" };
    digest.replace_range(..1, replacement);
    assert!(
        harness
            .implementation_evidence_overlay(std::slice::from_ref(&binding))
            .is_err()
    );

    let publication_digests = Corpus::load_default()
        .expect("reference corpus reloads")
        .publication_digests;
    binding.publication_digests = publication_digests;
    for source in [
        ImplementationEvidenceSource::Model,
        ImplementationEvidenceSource::Skipped,
        ImplementationEvidenceSource::EngineWitness,
    ] {
        binding.source = source;
        assert!(
            harness
                .implementation_evidence_overlay(std::slice::from_ref(&binding))
                .is_err()
        );
    }

    binding.source = ImplementationEvidenceSource::ProductionUnit;
    for invalid_status in [EvidenceStatus::Skipped, EvidenceStatus::Specified] {
        binding.observed_status = invalid_status;
        assert!(
            harness
                .implementation_evidence_overlay(std::slice::from_ref(&binding))
                .is_err()
        );
    }
    binding.observed_status = EvidenceStatus::Failed;
    assert_eq!(
        harness
            .implementation_evidence_overlay(std::slice::from_ref(&binding))
            .expect("failed production-unit evidence remains representable")
            .aggregate(),
        ImplementationEvidenceAggregate::PartiallyExecuted
    );
    binding.observed_status = EvidenceStatus::Passed;
    assert!(
        harness
            .implementation_evidence_overlay(&[binding.clone(), binding])
            .is_err()
    );
}

#[test]
fn implementation_evidence_overlay_rejects_malformed_repository_references_and_text() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let binding = date_range_implementation_bindings(&corpus.publication_digests)
        .into_iter()
        .next()
        .expect("Date binding exists");
    let harness = Harness::new(corpus);

    for malformed in [
        "/crates/orna-evaluator-v1/src/lib.rs::eval_infix",
        "C:/crates/orna-evaluator-v1/src/lib.rs::eval_infix",
        "crates/../orna-evaluator-v1/src/lib.rs::eval_infix",
        "https://example.invalid/lib.rs::eval_infix",
        "crates/orna-evaluator-v1/src/lib.rs::",
        "crates/orna-evaluator-v1/src/lib.rs::eval_infix::",
        "crates//orna-evaluator-v1/src/lib.rs::eval_infix",
        "crates/orna-evaluator-v1/src/lib.rs::eval/infix",
        "crates/orna-evaluator-v1/src/lib.rs::eval\ninfix",
        "crates/\u{1b}orna-evaluator-v1/src/lib.rs::eval_infix",
        "crates/orna-evaluator-v1/src/lib.rs",
    ] {
        let mut malformed_implementation = binding.clone();
        malformed_implementation.implementation_ref = malformed.into();
        assert!(
            harness
                .implementation_evidence_overlay(std::slice::from_ref(&malformed_implementation))
                .is_err(),
            "implementation reference must be rejected: {malformed:?}"
        );

        let mut malformed_test = binding.clone();
        malformed_test.test_ref = malformed.into();
        assert!(
            harness
                .implementation_evidence_overlay(std::slice::from_ref(&malformed_test))
                .is_err(),
            "test reference must be rejected: {malformed:?}"
        );
    }

    for (kind, invalid) in [
        ("subject", ""),
        ("command", "cargo test\n-p orna-evaluator-v1"),
        ("result", "97 passed\u{1b}[0m"),
    ] {
        let mut malformed = binding.clone();
        match kind {
            "subject" => malformed.subject = invalid.into(),
            "command" => malformed.command = invalid.into(),
            "result" => malformed.result = invalid.into(),
            _ => unreachable!("test cases enumerate all evidence text fields"),
        }
        assert!(
            harness
                .implementation_evidence_overlay(std::slice::from_ref(&malformed))
                .is_err(),
            "{kind} must reject control or empty text"
        );
    }
}

#[test]
fn scenario_witnesses_bind_only_declared_passed_implementation_scenarios() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads"));
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);
    let bindings = [
        ScenarioExecutionBinding {
            requirement_id: "ORNA-VALUE-006".into(),
            scenario_id: "LET-REBIND-091".into(),
            implementation_ref: "orna.bounded-expression-runtime.let-rebinding".into(),
            test_ref: "conformance.runtime_scenarios.let_rebinding".into(),
        },
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-001".into(),
            scenario_id: "PIPE-001".into(),
            implementation_ref: "orna.bounded-expression-runtime.pipeline-insertion".into(),
            test_ref: "conformance.runtime_scenarios.pipeline_insertion".into(),
        },
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-002".into(),
            scenario_id: "PIPE-002".into(),
            implementation_ref: "orna.bounded-expression-runtime.pipeline-precedence".into(),
            test_ref: "conformance.runtime_scenarios.pipeline_precedence".into(),
        },
    ];

    assert!(
        harness
            .scenario_execution_witnesses(&report, &bindings)
            .is_err()
    );

    let declared_harness = Harness::new(Corpus::load_default().expect("reference corpus loads"))
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "bounded-expression-runtime".into(),
            command: "orna-conformance --profile bounded-expression-runtime".into(),
            environment: std::collections::BTreeMap::new(),
            executed_scenario_contracts: vec![
                "LET-REBIND-091".into(),
                "PIPE-001".into(),
                "PIPE-002".into(),
                "REPL-001".into(),
                "TXN-001".into(),
                "TXN-002".into(),
            ],
        });
    let declared_report = declared_harness.run(&mut adapter);
    let bindings = [
        ScenarioExecutionBinding {
            requirement_id: "ORNA-VALUE-006".into(),
            scenario_id: "LET-REBIND-091".into(),
            implementation_ref: "crates/orna-conformance-v1/src/main.rs::let_rebinding_contract".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::scenario_witnesses_bind_only_declared_passed_implementation_scenarios".into(),
        },
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-001".into(),
            scenario_id: "PIPE-001".into(),
            implementation_ref: "crates/orna-conformance-v1/src/main.rs::pipeline_insertion_contract".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::scenario_witnesses_bind_only_declared_passed_implementation_scenarios".into(),
        },
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-002".into(),
            scenario_id: "PIPE-002".into(),
            implementation_ref: "crates/orna-conformance-v1/src/main.rs::pipeline_precedence_contract".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::scenario_witnesses_bind_only_declared_passed_implementation_scenarios".into(),
        },
    ];
    let witnesses = declared_harness
        .scenario_execution_witnesses(&declared_report, &bindings)
        .expect("declared passed scenarios become implementation witnesses");
    assert_eq!(
        witnesses
            .witnesses()
            .iter()
            .map(ScenarioExecutionWitness::scenario_id)
            .collect::<Vec<_>>(),
        ["LET-REBIND-091", "PIPE-001", "PIPE-002"]
    );
}

#[test]
fn report_reconciles_claimed_scenarios_with_passed_runtime_evidence() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads")).with_claim(
        ImplementationClaim {
            implementation_id: "test-runner".into(),
            profile: "test".into(),
            command: "test-runner".into(),
            environment: std::collections::BTreeMap::new(),
            executed_scenario_contracts: vec![
                "TXN-001".into(),
                "PIPE-001".into(),
                "PIPE-001".into(),
                "unknown-scenario".into(),
            ],
        },
    );
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);
    let executed = report
        .implementation_claim
        .executed_scenario_contracts
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        executed,
        ["PIPE-001"].into_iter().map(String::from).collect()
    );
}

struct FailedScenario;
impl ConformanceAdapter for FailedScenario {
    type Diagnostic = serde_json::Value;

    fn diagnostic_code(&self, _: &Self::Diagnostic) -> String {
        "E-TEST".into()
    }

    fn diagnostic_message(&self, _: &Self::Diagnostic) -> String {
        "scenario failed".into()
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

    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        StageOutcome::Passed
    }

    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Self::Diagnostic> {
        match scenario.id.as_str() {
            "PIPE-001" => StageOutcome::Failed(serde_json::json!({"code": "E-TEST"})),
            "PIPE-002" => StageOutcome::Passed,
            _ => StageOutcome::Skipped {
                reason: "scenario omitted by focused predicate test".into(),
            },
        }
    }
}

#[test]
fn report_reconciliation_excludes_failed_runtime_claims() {
    let harness = Harness::new(Corpus::load_default().expect("reference corpus loads")).with_claim(
        ImplementationClaim {
            implementation_id: "test-runner".into(),
            profile: "test".into(),
            command: "test-runner".into(),
            environment: std::collections::BTreeMap::new(),
            executed_scenario_contracts: vec!["PIPE-001".into(), "PIPE-002".into()],
        },
    );
    let mut adapter = FailedScenario;
    let report = harness.run(&mut adapter);

    assert_eq!(
        report.implementation_claim.executed_scenario_contracts,
        vec!["PIPE-002"]
    );
    assert_eq!(
        report
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == "PIPE-001")
            .expect("failed scenario is reported")
            .status,
        EvidenceStatus::Failed
    );
}
