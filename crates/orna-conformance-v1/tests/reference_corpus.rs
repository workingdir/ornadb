use orna_conformance_v1::*;

#[test]
fn loads_the_complete_unchanged_reference_corpus() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    assert_eq!(corpus.manifest.fixtures.len(), 167);
    assert_eq!(corpus.invalid_metadata.fixtures.len(), 80);
    assert_eq!(corpus.vectors.len(), 6);
    assert_eq!(corpus.requirements.len(), 870);
    assert!(corpus.diagnostics.values().all(|diagnostic| {
        diagnostic.version == "1.0.0" && diagnostic.status == "expected-not-executed"
    }));
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
fn requirement_mapping_is_stage_scoped_and_does_not_promote_skips() {
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SOURCE-001")
        .expect("source requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SOURCE-001",
            "fixture": "valid/minimal-root.orna",
            "stage": "parse",
        }));

    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("representative fixture is reported");
    let parse = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Parse))
        .expect("parse stage is reported");
    assert_eq!(
        parse.requirements,
        vec!["ORNA-SOURCE-001".to_string()]
    );
    assert!(matches!(
        &parse.requirement_mapping,
        RequirementMapping::Mapped { .. }
    ));
    assert_eq!(parse.status, EvidenceStatus::Skipped);
    let serialized = serde_json::to_value(&report).expect("run report serializes");
    let serialized_fixture = serialized["fixtures"]
        .as_array()
        .expect("serialized fixture array")
        .iter()
        .find(|fixture| fixture["fixture"] == "valid/minimal-root.orna")
        .expect("serialized representative fixture");
    let serialized_parse = serialized_fixture["stages"]
        .as_array()
        .expect("serialized stage array")
        .first()
        .expect("serialized parse stage");
    assert_eq!(serialized_parse["status"], "skipped");
    assert_eq!(serialized_parse["requirement_mapping"]["status"], "mapped");
    assert_eq!(serialized["coverage"]["mapped_stage_evidence"], 0);

    let resolve = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Resolve))
        .expect("resolve stage is reported");
    assert!(resolve.requirements.is_empty());
    assert!(matches!(
        &resolve.requirement_mapping,
        RequirementMapping::Unmapped { reason } if reason.contains("resolve")
    ));

    let unrelated = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/affine-max.orna")
        .expect("unrelated fixture is reported");
    assert!(matches!(
        &unrelated.stages[0].requirement_mapping,
        RequirementMapping::Unmapped { .. }
    ));
    assert_eq!(report.coverage.mapped_stage_evidence, 0);
    assert!(report.coverage.unmapped_stage_evidence > 1);
}

#[test]
fn malformed_requirement_links_are_rejected_or_reported_unmapped() {
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    let entry = corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SOURCE-001")
        .expect("source requirement exists");
    entry.tests.push(serde_json::json!({
        "kind": "ordinary source fixture",
        "status": "planned",
        "subject": 7,
        "fixture": "valid/minimal-root.orna",
        "stage": "parse",
    }));
    assert_eq!(
        corpus.validate().expect_err("non-string subject is invalid").to_string(),
        "requirement evidence test subject must match requirement"
    );
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    let parse = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("representative fixture")
        .stages
        .first()
        .expect("parse stage");
    assert!(matches!(
        &parse.requirement_mapping,
        RequirementMapping::Unmapped { reason } if reason.contains("malformed")
    ));

    let mut conflict = Corpus::load_default().expect("reference corpus reloads");
    conflict
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SOURCE-001")
        .expect("source requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SOURCE-001",
            "fixture": "valid/minimal-root.orna",
            "stage": "parse",
            "stages": ["resolve"],
        }));
    assert_eq!(
        conflict
            .validate()
            .expect_err("conflicting stage fields are invalid")
            .to_string(),
        "requirement evidence stage and stages are mutually exclusive"
    );

    let mut unknown = Corpus::load_default().expect("reference corpus reloads");
    unknown
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SOURCE-001")
        .expect("source requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SOURCE-001",
            "fixture": "valid/minimal-root.orna",
            "stages": ["parse", "bogus-stage"],
        }));
    assert_eq!(
        unknown
            .validate()
            .expect_err("unknown stage spelling is invalid")
            .to_string(),
        "requirement evidence stages must contain known stages"
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
fn invoke_report(
    fixture: Fixture,
    source: &str,
    requirement: &str,
) -> orna_conformance_v1::RunReport {
    let root = tempfile::tempdir().expect("invoke fixture root");
    std::fs::write(root.path().join(&fixture.path), source).expect("invoke fixture writes");

    let fixture_id = fixture.id.clone();
    let fixture_path = fixture.path.clone();
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus.root = root.path().to_path_buf();
    corpus.manifest.fixtures = vec![fixture];
    corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == requirement)
        .unwrap_or_else(|| panic!("{requirement} requirement exists"))
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": requirement,
            "fixture": fixture_id,
            "path": fixture_path,
            "stage": "typecheck",
        }));

    Harness::new(corpus).run(&mut SemanticAdapter::default())
}

fn typed_invoke_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    invoke_report(fixture, source, "ORNA-SYS-132")
}

fn erased_invoke_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    invoke_report(fixture, source, "ORNA-SYS-077")
}

fn erased_start_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    invoke_report(fixture, source, "ORNA-SYS-081")
}

fn typed_start_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    let root = tempfile::tempdir().expect("typed start fixture root");
    std::fs::write(root.path().join(&fixture.path), source).expect("typed start fixture writes");

    let fixture_id = fixture.id.clone();
    let fixture_path = fixture.path.clone();
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus.root = root.path().to_path_buf();
    corpus.manifest.fixtures = vec![fixture];
    corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SYS-132")
        .expect("ORNA-SYS-132 requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SYS-132",
            "fixture": fixture_id,
            "path": fixture_path,
            "stage": "typecheck",
        }));

    Harness::new(corpus).run(&mut SemanticAdapter::default())
}

fn typed_await_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    let root = tempfile::tempdir().expect("typed await fixture root");
    std::fs::write(root.path().join(&fixture.path), source).expect("typed await fixture writes");

    let fixture_id = fixture.id.clone();
    let fixture_path = fixture.path.clone();
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus.root = root.path().to_path_buf();
    corpus.manifest.fixtures = vec![fixture];
    corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SYS-082")
        .expect("ORNA-SYS-082 requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SYS-082",
            "fixture": fixture_id,
            "path": fixture_path,
            "stage": "typecheck",
        }));

    Harness::new(corpus).run(&mut SemanticAdapter::default())
}
fn typed_cancel_report(fixture: Fixture, source: &str) -> orna_conformance_v1::RunReport {
    let root = tempfile::tempdir().expect("typed cancel fixture root");
    std::fs::write(root.path().join(&fixture.path), source).expect("typed cancel fixture writes");

    let fixture_id = fixture.id.clone();
    let fixture_path = fixture.path.clone();
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus.root = root.path().to_path_buf();
    corpus.manifest.fixtures = vec![fixture];
    corpus
        .requirement_evidence
        .requirements
        .iter_mut()
        .find(|entry| entry.requirement == "ORNA-SYS-083")
        .expect("ORNA-SYS-083 requirement exists")
        .tests
        .push(serde_json::json!({
            "kind": "ordinary source fixture",
            "status": "planned",
            "subject": "ORNA-SYS-083",
            "fixture": fixture_id,
            "path": fixture_path,
            "stage": "typecheck",
        }));

    Harness::new(corpus).run(&mut SemanticAdapter::default())
}

fn typed_cancel_fixture(
    id: &str,
    path: &str,
    expect: &[(&str, &str)],
    failing_phase: Option<&str>,
    diagnostic: Option<&str>,
    message_contains: Option<&str>,
) -> Fixture {
    Fixture {
        id: id.into(),
        kind: if failing_phase.is_some() {
            "invalid".into()
        } else {
            "valid".into()
        },
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: expect
            .iter()
            .map(|(stage, result)| ((*stage).into(), (*result).into()))
            .collect(),
        failing_phase: failing_phase.map(str::to_owned),
        diagnostic: diagnostic.map(str::to_owned),
        message_contains: message_contains.map(str::to_owned),
        expected_diagnostic: None,
        environment: None,
    }
}


fn typed_await_fixture(
    id: &str,
    path: &str,
    expect: &[(&str, &str)],
    failing_phase: Option<&str>,
    diagnostic: Option<&str>,
    message_contains: Option<&str>,
) -> Fixture {
    Fixture {
        id: id.into(),
        kind: if failing_phase.is_some() {
            "invalid".into()
        } else {
            "valid".into()
        },
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: expect
            .iter()
            .map(|(stage, result)| ((*stage).into(), (*result).into()))
            .collect(),
        failing_phase: failing_phase.map(str::to_owned),
        diagnostic: diagnostic.map(str::to_owned),
        message_contains: message_contains.map(str::to_owned),
        expected_diagnostic: None,
        environment: None,
    }
}


fn typed_invoke_fixture(
    id: &str,
    path: &str,
    expect: &[(&str, &str)],
    failing_phase: Option<&str>,
    diagnostic: Option<&str>,
    message_contains: Option<&str>,
) -> Fixture {
    Fixture {
        id: id.into(),
        kind: if failing_phase.is_some() {
            "invalid".into()
        } else {
            "valid".into()
        },
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: expect
            .iter()
            .map(|(stage, result)| ((*stage).into(), (*result).into()))
            .collect(),
        failing_phase: failing_phase.map(str::to_owned),
        diagnostic: diagnostic.map(str::to_owned),
        message_contains: message_contains.map(str::to_owned),
        expected_diagnostic: None,
        environment: None,
    }
}
fn typed_start_fixture(
    id: &str,
    path: &str,
    expect: &[(&str, &str)],
    failing_phase: Option<&str>,
    diagnostic: Option<&str>,
    message_contains: Option<&str>,
) -> Fixture {
    Fixture {
        id: id.into(),
        kind: if failing_phase.is_some() {
            "invalid".into()
        } else {
            "valid".into()
        },
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: expect
            .iter()
            .map(|(stage, result)| ((*stage).into(), (*result).into()))
            .collect(),
        failing_phase: failing_phase.map(str::to_owned),
        diagnostic: diagnostic.map(str::to_owned),
        message_contains: message_contains.map(str::to_owned),
        expected_diagnostic: None,
        environment: None,
    }
}


#[test]
fn typed_sys_invoke_report_maps_orna_sys_132_to_semantic_pass_evidence() {
    let report = typed_invoke_report(
        typed_invoke_fixture(
            "typed-invoke-valid",
            "typed-invoke-valid.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "pass"), ("evaluate", "not-run")],
            None,
            None,
            None,
        ),
        r#"
            pub fn invoke(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.invoke<Int>(function, arguments, as: Int);
        "#,
    );
    let fixture = &report.fixtures[0];
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed invoke typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-132".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-132".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-invoke-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(fixture.passed, "{:?}", fixture.stages);
}

#[test]
fn typed_sys_invoke_report_retains_mismatched_witness_diagnostic() {
    let report = typed_invoke_report(
        typed_invoke_fixture(
            "typed-invoke-mismatch",
            "typed-invoke-mismatch.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "fail"), ("evaluate", "not-run")],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("sys.invoke explicit type argument must match the as: witness"),
        ),
        r#"
            pub fn invoke(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.invoke<Str>(function, arguments, as: Int);
        "#,
    );
    let typecheck = report.fixtures[0]
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed invoke mismatch typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-132".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-132".to_string()]
    ));
    assert_eq!(
        typecheck.diagnostic,
        Some(serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        }))
    );
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-invoke-mismatch"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Failed
            && evidence.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic["code"] == "ORNA-S021-TYPE"
            })
    }));

    let serialized = serde_json::to_value(&report).expect("typed invoke report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized typed invoke mismatch typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["diagnostic"],
        serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        })
    );
    assert_eq!(
        serialized["semantic_evidence"]
            .as_array()
            .expect("serialized semantic evidence")
            .iter()
            .find(|evidence| {
                evidence["subject"] == "typed-invoke-mismatch"
                    && evidence["stage"] == "typecheck"
            })
            .expect("serialized semantic mismatch evidence")["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
}

#[test]
fn erased_sys_invoke_report_maps_orna_sys_077_to_semantic_pass_evidence() {
    let report = erased_invoke_report(
        typed_invoke_fixture(
            "erased-invoke-valid",
            "erased-invoke-valid.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "pass"),
                ("evaluate", "not-run"),
            ],
            None,
            None,
            None,
        ),
        r#"
            pub fn erased(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.invoke(function, arguments);
        "#,
    );
    let typecheck = report.fixtures[0]
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("erased invoke typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-077".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-077".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "erased-invoke-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(report.fixtures[0].passed, "{:?}", report.fixtures[0].stages);
}

#[test]
fn erased_sys_invoke_report_serializes_missing_witness_diagnostic() {
    let report = erased_invoke_report(
        typed_invoke_fixture(
            "erased-invoke-missing-witness",
            "erased-invoke-missing-witness.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "fail"),
                ("evaluate", "not-run"),
            ],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("typed sys.invoke requires an explicit as: T witness"),
        ),
        r#"
            pub fn missing(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.invoke<Int>(function, arguments);
        "#,
    );
    let serialized = serde_json::to_value(&report).expect("erased invoke report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized erased invoke typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-077"])
    );
    assert_eq!(
        serialized_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert!(serialized["semantic_evidence"]
        .as_array()
        .expect("serialized semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == "erased-invoke-missing-witness"
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "failed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-077"])
                && evidence["diagnostic"]["code"] == "ORNA-S021-TYPE"
        }));
}

#[test]
fn erased_sys_start_report_maps_orna_sys_081_to_semantic_pass_evidence() {
    let report = erased_start_report(
        typed_invoke_fixture(
            "erased-start-valid",
            "erased-start-valid.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "pass"),
                ("evaluate", "not-run"),
            ],
            None,
            None,
            None,
        ),
        r#"
            pub fn start(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.start(function, arguments);
        "#,
    );
    let fixture = &report.fixtures[0];
    let parse = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Parse))
        .expect("erased start parse stage");
    assert_eq!(parse.status, EvidenceStatus::Passed);
    assert!(parse.requirements.is_empty());
    assert_eq!(parse.diagnostic, None);
    let resolve = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Resolve))
        .expect("erased start resolve stage");
    assert_eq!(resolve.status, EvidenceStatus::Passed);
    assert!(resolve.requirements.is_empty());
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("erased start typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-081".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-081".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "erased-start-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(fixture.passed, "{:?}", fixture.stages);

    let serialized = serde_json::to_value(&report).expect("erased start report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized erased start typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "passed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-081"])
    );
    assert!(serialized["semantic_evidence"]
        .as_array()
        .expect("serialized semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == "erased-start-valid"
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "passed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-081"])
        }));
}

#[test]
fn erased_sys_start_report_serializes_missing_witness_diagnostic() {
    let report = erased_start_report(
        typed_invoke_fixture(
            "erased-start-missing-witness",
            "erased-start-missing-witness.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "fail"),
                ("evaluate", "not-run"),
            ],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("typed sys.start requires an explicit as: T witness"),
        ),
        r#"
            pub fn missing(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.start<Int>(function, arguments);
        "#,
    );
    let fixture = &report.fixtures[0];
    let parse = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Parse))
        .expect("erased start missing-witness parse stage");
    assert_eq!(parse.status, EvidenceStatus::Passed);
    assert_eq!(parse.diagnostic, None);
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("erased start missing-witness typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-081".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-081".to_string()]
    ));
    assert_eq!(
        typecheck.diagnostic,
        Some(serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        }))
    );
    assert!(fixture.passed, "{:?}", fixture.stages);

    let serialized = serde_json::to_value(&report).expect("erased start report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized erased start missing-witness typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-081"])
    );
    assert_eq!(
        serialized_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert!(serialized["semantic_evidence"]
        .as_array()
        .expect("serialized semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == "erased-start-missing-witness"
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "failed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-081"])
                && evidence["diagnostic"]["code"] == "ORNA-S021-TYPE"
        }));
}

#[test]
fn typed_sys_start_report_maps_orna_sys_132_to_semantic_pass_evidence() {
    let report = typed_start_report(
        typed_start_fixture(
            "typed-start-valid",
            "typed-start-valid.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "pass"),
                ("evaluate", "not-run"),
            ],
            None,
            None,
            None,
        ),
        r#"
            pub fn start(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.start<Int>(function, arguments, as: Int);
        "#,
    );
    let fixture = &report.fixtures[0];
    let parse = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Parse))
        .expect("typed start parse stage");
    assert_eq!(parse.status, EvidenceStatus::Passed);
    assert!(parse.requirements.is_empty());
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed start typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-132".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-132".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-start-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(fixture.passed, "{:?}", fixture.stages);
}

#[test]
fn typed_sys_start_report_retains_mismatched_witness_diagnostic() {
    let report = typed_start_report(
        typed_start_fixture(
            "typed-start-mismatch",
            "typed-start-mismatch.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "fail"),
                ("evaluate", "not-run"),
            ],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("sys.start explicit type argument must match the as: witness"),
        ),
        r#"
            pub fn start_wrong(function: sys.FunctionRef, arguments: sys.ArgumentMap) =
                sys.start<Str>(function, arguments, as: Int);
        "#,
    );
    let fixture = &report.fixtures[0];
    let parse = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Parse))
        .expect("typed start mismatch parse stage");
    assert_eq!(parse.status, EvidenceStatus::Passed);
    assert_eq!(parse.diagnostic, None);
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed start mismatch typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-132".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-132".to_string()]
    ));
    assert_eq!(
        typecheck.diagnostic,
        Some(serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        }))
    );
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-start-mismatch"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Failed
            && evidence.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic["code"] == "ORNA-S021-TYPE"
            })
    }));

    let serialized = serde_json::to_value(&report).expect("typed start report serializes");
    let serialized_stages = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized typed start stage array");
    let serialized_parse = serialized_stages
        .iter()
        .find(|stage| stage["stage"] == "parse")
        .expect("serialized typed start parse stage");
    assert_eq!(serialized_parse["status"], "passed");
    assert_eq!(serialized_parse["diagnostic"], serde_json::Value::Null);
    let serialized_typecheck = serialized_stages
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized typed start mismatch typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert_eq!(
        serialized["semantic_evidence"]
            .as_array()
            .expect("serialized semantic evidence")
            .iter()
            .find(|evidence| {
                evidence["subject"] == "typed-start-mismatch"
                    && evidence["stage"] == "typecheck"
            })
            .expect("serialized typed start mismatch evidence")["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
}

#[test]
fn typed_sys_await_report_maps_orna_sys_082_to_semantic_pass_evidence() {
    let report = typed_await_report(
        typed_await_fixture(
            "typed-await-valid",
            "typed-await-valid.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "pass"), ("evaluate", "not-run")],
            None,
            None,
            None,
        ),
        r#"
            pub fn await_timeout(job: sys.InvocationHandle<Int>) =
                sys.await<Int>(job, timeout: 1.s);
        "#,
    );
    let fixture = &report.fixtures[0];
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed await typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-082".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-082".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-await-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(fixture.passed, "{:?}", fixture.stages);
}

#[test]
fn typed_sys_await_report_retains_mismatched_witness_diagnostic() {
    let report = typed_await_report(
        typed_await_fixture(
            "typed-await-mismatch",
            "typed-await-mismatch.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "fail"), ("evaluate", "not-run")],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("sys.await explicit type argument must match the invocation handle result type"),
        ),
        r#"
            pub fn await_wrong(job: sys.InvocationHandle<Int>) =
                sys.await<Str>(job, timeout: 1.s);
        "#,
    );
    let typecheck = report.fixtures[0]
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed await mismatch typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-082".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-082".to_string()]
    ));
    assert_eq!(
        typecheck.diagnostic,
        Some(serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        }))
    );
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-await-mismatch"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Failed
            && evidence.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic["code"] == "ORNA-S021-TYPE"
            })
    }));

    let serialized = serde_json::to_value(&report).expect("typed await report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized typed await mismatch typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert_eq!(
        serialized["semantic_evidence"]
            .as_array()
            .expect("serialized semantic evidence")
            .iter()
            .find(|evidence| {
                evidence["subject"] == "typed-await-mismatch"
                    && evidence["stage"] == "typecheck"
            })
            .expect("serialized semantic mismatch evidence")["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
}
#[test]
fn typed_sys_cancel_report_maps_orna_sys_083_to_semantic_pass_evidence() {
    let report = typed_cancel_report(
        typed_cancel_fixture(
            "typed-cancel-valid",
            "typed-cancel-valid.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "pass"), ("evaluate", "not-run")],
            None,
            None,
            None,
        ),
        r#"
            pub fn inferred_cancel(job: sys.InvocationHandle<Int>) =
                sys.cancel(job);
            pub fn explicit_cancel(job: sys.InvocationHandle<Int>) =
                sys.cancel<Int>(job, reason: "stop");
        "#,
    );
    let fixture = &report.fixtures[0];
    let typecheck = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed cancel typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-083".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-083".to_string()]
    ));
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-cancel-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed
    }));
    assert!(fixture.passed, "{:?}", fixture.stages);
}

#[test]
fn typed_sys_cancel_report_retains_mismatched_witness_diagnostic() {
    let report = typed_cancel_report(
        typed_cancel_fixture(
            "typed-cancel-mismatch",
            "typed-cancel-mismatch.orna",
            &[("parse", "pass"), ("resolve", "pass"), ("typecheck", "fail"), ("evaluate", "not-run")],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            Some("sys.cancel explicit type argument must match the invocation handle result type"),
        ),
        r#"
            pub fn cancel_wrong(job: sys.InvocationHandle<Int>) =
                sys.cancel<Str>(job);
        "#,
    );
    let typecheck = report.fixtures[0]
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("typed cancel mismatch typecheck stage");
    assert_eq!(typecheck.class, EvidenceClass::Semantic);
    assert_eq!(typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        typecheck.requirements,
        vec!["ORNA-SYS-083".to_string()]
    );
    assert!(matches!(
        &typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-083".to_string()]
    ));
    assert_eq!(
        typecheck.diagnostic,
        Some(serde_json::json!({
            "code": "ORNA-S021-TYPE",
            "spans": [],
            "redacted": true,
        }))
    );
    assert!(report.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "typed-cancel-mismatch"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Failed
            && evidence.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic["code"] == "ORNA-S021-TYPE"
            })
    }));

    let serialized = serde_json::to_value(&report).expect("typed cancel report serializes");
    let serialized_typecheck = serialized["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized typed cancel mismatch typecheck stage");
    assert_eq!(serialized_typecheck["class"], "semantic");
    assert_eq!(serialized_typecheck["status"], "failed");
    assert_eq!(
        serialized_typecheck["requirement_mapping"]["status"],
        "mapped"
    );
    assert_eq!(
        serialized_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert_eq!(
        serialized["semantic_evidence"]
            .as_array()
            .expect("serialized semantic evidence")
            .iter()
            .find(|evidence| {
                evidence["subject"] == "typed-cancel-mismatch"
                    && evidence["stage"] == "typecheck"
            })
            .expect("serialized typed cancel mismatch evidence")["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
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
    // Reviewed bindings are caller ordered, but the published bounded
    // implementation-scenario evidence is canonicalized by requirement and
    // scenario identity. This only makes the serialized evidence
    // reproducible; it does not turn it into an Orna-engine witness.
    assert_eq!(
        witnesses
            .witnesses()
            .iter()
            .map(ScenarioExecutionWitness::scenario_id)
            .collect::<Vec<_>>(),
        ["PIPE-001", "PIPE-002", "LET-REBIND-091"]
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

#[test]
fn report_canonicalizes_equivalent_claim_order_for_reproducibility() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let claim = |executed_scenario_contracts| ImplementationClaim {
        implementation_id: "test-runner".into(),
        profile: "test".into(),
        command: "test-runner".into(),
        environment: std::collections::BTreeMap::new(),
        executed_scenario_contracts,
    };
    let mut first_adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let first = Harness::new(corpus.clone())
        .with_claim(claim(vec![
            "PIPE-002".into(),
            "PIPE-001".into(),
            "PIPE-002".into(),
        ]))
        .run(&mut first_adapter);
    let mut second_adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let second = Harness::new(corpus)
        .with_claim(claim(vec!["PIPE-001".into(), "PIPE-002".into()]))
        .run(&mut second_adapter);

    assert_eq!(
        first.implementation_claim.executed_scenario_contracts,
        ["PIPE-001", "PIPE-002"]
    );
    assert_eq!(
        serde_json::to_vec(&first).expect("first report serializes"),
        serde_json::to_vec(&second).expect("second report serializes"),
        "claim ordering and duplication must not change report bytes"
    );
}

#[test]
fn reviewed_evidence_bindings_canonicalize_equivalent_input_order() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let harness = Harness::new(corpus.clone());
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);

    let engine_bindings = vec![
        FixtureStageBinding {
            requirement_id: "ORNA-SOURCE-002".into(),
            fixture_id: "valid/minimal-root.orna".into(),
            fixture_path: "examples/valid/minimal-root.orna".into(),
            stage: Stage::Resolve,
            implementation_ref: "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::reviewed_evidence_bindings_canonicalize_equivalent_input_order".into(),
        },
        FixtureStageBinding {
            requirement_id: "ORNA-SOURCE-001".into(),
            fixture_id: "valid/minimal-root.orna".into(),
            fixture_path: "examples/valid/minimal-root.orna".into(),
            stage: Stage::Parse,
            implementation_ref: "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::reviewed_evidence_bindings_canonicalize_equivalent_input_order".into(),
        },
    ];
    let first_engine = harness
        .engine_witnesses(&report, &engine_bindings)
        .expect("approved engine witness bindings are accepted");
    let mut reversed_engine = engine_bindings;
    reversed_engine.reverse();
    let second_engine = harness
        .engine_witnesses(&report, &reversed_engine)
        .expect("reordered engine witness bindings are accepted");
    assert_eq!(
        serde_json::to_vec(&first_engine).expect("engine witnesses serialize"),
        serde_json::to_vec(&second_engine).expect("engine witnesses serialize"),
    );

    let mut production_bindings = date_range_implementation_bindings(&corpus.publication_digests);
    let first_production = harness
        .implementation_evidence_overlay(&production_bindings)
        .expect("approved production evidence is accepted");
    production_bindings.reverse();
    let second_production = harness
        .implementation_evidence_overlay(&production_bindings)
        .expect("reordered production evidence is accepted");
    assert_eq!(
        serde_json::to_vec(&first_production).expect("production evidence serializes"),
        serde_json::to_vec(&second_production).expect("production evidence serializes"),
    );

    let declared_harness = Harness::new(corpus).with_claim(ImplementationClaim {
        implementation_id: "test-runner".into(),
        profile: "bounded-expression-runtime".into(),
        command: "test-runner".into(),
        environment: std::collections::BTreeMap::new(),
        executed_scenario_contracts: vec!["PIPE-001".into(), "PIPE-002".into()],
    });
    let declared_report = declared_harness.run(&mut adapter);
    let scenario_bindings = vec![
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-002".into(),
            scenario_id: "PIPE-002".into(),
            implementation_ref: "crates/orna-conformance-v1/src/main.rs::pipeline_precedence_contract".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::reviewed_evidence_bindings_canonicalize_equivalent_input_order".into(),
        },
        ScenarioExecutionBinding {
            requirement_id: "ORNA-PIPE-001".into(),
            scenario_id: "PIPE-001".into(),
            implementation_ref: "crates/orna-conformance-v1/src/main.rs::pipeline_insertion_contract".into(),
            test_ref: "crates/orna-conformance-v1/tests/reference_corpus.rs::reviewed_evidence_bindings_canonicalize_equivalent_input_order".into(),
        },
    ];
    let first_scenarios = declared_harness
        .scenario_execution_witnesses(&declared_report, &scenario_bindings)
        .expect("approved scenario witnesses are accepted");
    let mut reversed_scenarios = scenario_bindings;
    reversed_scenarios.reverse();
    let second_scenarios = declared_harness
        .scenario_execution_witnesses(&declared_report, &reversed_scenarios)
        .expect("reordered scenario witnesses are accepted");
    assert_eq!(
        serde_json::to_vec(&first_scenarios).expect("scenario witnesses serialize"),
        serde_json::to_vec(&second_scenarios).expect("scenario witnesses serialize"),
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
#[test]
fn harness_report_retains_imported_generic_sys_meta_evidence() {
    let root = tempfile::tempdir().expect("temporary conformance corpus");
    let manifest_root = root.path().join("tests");
    std::fs::create_dir_all(&manifest_root).expect("manifest directory");
    std::fs::write(
        manifest_root.join("project-manifest.json"),
        serde_json::json!({
            "project": "imported-generic",
            "entry": "consumer.orna",
            "modules": ["library.orna", "consumer.orna"],
            "expected": "semantic-only",
            "implementation_execution": "not executed"
        })
        .to_string(),
    )
    .expect("project manifest");

    let accepted_path = root.path().join("accepted-imported-generic");
    let rejected_path = root.path().join("rejected-imported-generic");
    for project_path in [&accepted_path, &rejected_path] {
        std::fs::create_dir_all(project_path).expect("project directory");
        std::fs::write(
            project_path.join("library.orna"),
            "pub fn lookup<T>(value: T) = sys.meta<T>(value);",
        )
        .expect("generic library module");
    }
    std::fs::write(
        accepted_path.join("consumer.orna"),
        "use library; pub fn read(value: Int) = library.lookup<Int>(value);",
    )
    .expect("accepted consumer module");
    std::fs::write(
        rejected_path.join("consumer.orna"),
        "use library; pub fn too_many(value: Int) = library.lookup<Int, Str>(value);",
    )
    .expect("rejected consumer module");

    let fixture = |id: &str, path: &str, failing: bool| Fixture {
        id: id.into(),
        kind: "project".into(),
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: std::collections::BTreeMap::from([
            ("parse".into(), "pass".into()),
            ("resolve".into(), "pass".into()),
            (
                "typecheck".into(),
                if failing { "fail".into() } else { "pass".into() },
            ),
        ]),
        failing_phase: failing.then(|| "typecheck".into()),
        diagnostic: failing.then(|| "ORNA-S021-TYPE".into()),
        message_contains: None,
        expected_diagnostic: None,
        environment: None,
    };
    let accepted_id = "valid/imported-generic-sys-meta.orna";
    let rejected_id = "invalid/imported-generic-sys-meta-type-arguments.orna";
    let requirement_test = |fixture: &str| {
        serde_json::json!({
            "kind": "imported generic sys.meta<T> report witness",
            "status": "planned",
            "subject": "ORNA-GENERIC-001",
            "fixture": fixture,
            "stage": "typecheck",
        })
    };
    let corpus = Corpus {
        root: root.path().to_path_buf(),
        manifest: Manifest {
            version: "1.0.0".into(),
            counts: ManifestCounts {
                valid: 1,
                invalid: 1,
                project: 2,
                total: 2,
            },
            fixtures: vec![
                fixture(accepted_id, "accepted-imported-generic", false),
                fixture(rejected_id, "rejected-imported-generic", true),
            ],
        },
        invalid_metadata: InvalidMetadata {
            version: "1.0.0".into(),
            count: 0,
            fixtures: Vec::new(),
        },
        diagnostics: std::collections::BTreeMap::new(),
        vectors: std::collections::BTreeMap::new(),
        scenarios: serde_json::json!({"scenarios": []}),
        requirements: vec![Requirement {
            id: "ORNA-GENERIC-001".into(),
            chapter: "expressions".into(),
            source: "source/06-expressions.md".into(),
            text: "Generic parameters are explicit in public source when a generic abstraction is intended; local type arguments may be inferred.".into(),
        }],
        requirement_evidence: RequirementEvidence {
            meaning: "focused report-stage witness".into(),
            requirements: vec![RequirementEvidenceEntry {
                requirement: "ORNA-GENERIC-001".into(),
                tests: vec![requirement_test(accepted_id), requirement_test(rejected_id)],
            }],
        },
        project_expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
        publication_digests: std::collections::BTreeMap::new(),
    };

    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let accepted = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == accepted_id)
        .expect("accepted imported generic fixture");
    assert!(accepted.passed, "{:?}", accepted.stages);
    let accepted_stage = accepted
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("accepted typecheck evidence");
    assert_eq!(accepted_stage.class, EvidenceClass::Semantic);
    assert_eq!(accepted_stage.status, EvidenceStatus::Passed);
    assert!(accepted_stage.expectation_satisfied);
    assert_eq!(
        accepted_stage.requirements,
        vec!["ORNA-GENERIC-001".to_string()]
    );
    assert!(matches!(
        accepted_stage.requirement_mapping,
        RequirementMapping::Mapped { .. }
    ));

    let rejected = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == rejected_id)
        .expect("rejected imported generic fixture");
    assert!(rejected.passed, "{:?}", rejected.stages);
    let rejected_stage = rejected
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("rejected typecheck evidence");
    assert_eq!(rejected_stage.class, EvidenceClass::Semantic);
    assert_eq!(rejected_stage.status, EvidenceStatus::Failed);
    assert!(rejected_stage.expectation_satisfied);
    assert_eq!(
        rejected_stage.requirements,
        vec!["ORNA-GENERIC-001".to_string()]
    );
    assert!(matches!(
        rejected_stage.requirement_mapping,
        RequirementMapping::Mapped { .. }
    ));
    assert_eq!(
        rejected_stage
            .diagnostic
            .as_ref()
            .expect("rejected diagnostic is retained")["code"],
        "ORNA-S021-TYPE"
    );

    let serialized = serde_json::to_value(&report).expect("report serializes");
    let semantic = serialized["semantic_evidence"]
        .as_array()
        .expect("semantic evidence array");
    assert!(semantic.iter().any(|evidence| {
        evidence["subject"] == accepted_id
            && evidence["status"] == "passed"
            && evidence["requirement_mapping"]["status"] == "mapped"
    }));
    assert!(semantic.iter().any(|evidence| {
        evidence["subject"] == rejected_id
            && evidence["status"] == "failed"
            && evidence["diagnostic"]["code"] == "ORNA-S021-TYPE"
    }));
}

#[test]
fn harness_maps_runtime_info_semantics_and_serializes_runtime_rejection() {
    let root = tempfile::tempdir().expect("temporary runtime-info corpus");
    let valid_path = "examples/valid/sys-runtime-info.orna";
    let invalid_path = "examples/invalid/legacy-sys-runtime.orna";
    for (path, source) in [
        (
            valid_path,
            "pub fn info() = sys.rt.info();",
        ),
        (
            invalid_path,
            "fn active_streams() {\n    sys.runtime.streams\n}",
        ),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        std::fs::write(path, source).expect("fixture source");
    }

    let valid_id = "valid/sys-runtime-info.orna";
    let invalid_id = "invalid/legacy-sys-runtime.orna";
    let fixture = |id: &str,
                   path: &str,
                   kind: &str,
                   expect: [(&str, &str); 3],
                   failing_phase: Option<&str>,
                   diagnostic: Option<&str>,
                   message_contains: Option<&str>| Fixture {
        id: id.into(),
        kind: kind.into(),
        path: path.into(),
        parse_as: "module_unit".into(),
        expect: expect
            .into_iter()
            .map(|(stage, outcome)| (stage.into(), outcome.into()))
            .collect(),
        failing_phase: failing_phase.map(str::to_owned),
        diagnostic: diagnostic.map(str::to_owned),
        message_contains: message_contains.map(str::to_owned),
        expected_diagnostic: diagnostic.map(|_| "tests/expected-diagnostics/runtime-info.json".into()),
        environment: None,
    };
    let corpus = Corpus {
        root: root.path().to_path_buf(),
        manifest: Manifest {
            version: "1.0.0".into(),
            counts: ManifestCounts {
                valid: 1,
                invalid: 1,
                project: 0,
                total: 2,
            },
            fixtures: vec![
                fixture(
                    valid_id,
                    valid_path,
                    "valid",
                    [("parse", "pass"), ("resolve", "pass"), ("typecheck", "pass")],
                    None,
                    None,
                    None,
                ),
                fixture(
                    invalid_id,
                    invalid_path,
                    "invalid",
                    [("parse", "pass"), ("resolve", "fail"), ("typecheck", "not-run")],
                    Some("resolve"),
                    Some("ORNA100-E-SYS-RUNTIME"),
                    Some("`sys.runtime` was renamed to `sys.rt`"),
                ),
            ],
        },
        invalid_metadata: InvalidMetadata {
            version: "1.0.0".into(),
            count: 1,
            fixtures: vec![InvalidFixture {
                path: invalid_path.into(),
                failing_phase: "resolve".into(),
                diagnostic: "ORNA100-E-SYS-RUNTIME".into(),
                message_contains: "`sys.runtime` was renamed to `sys.rt`".into(),
            }],
        },
        diagnostics: std::collections::BTreeMap::from([(
            invalid_path.into(),
            ExpectedDiagnostic {
                version: "1.0.0".into(),
                fixture: invalid_path.into(),
                failing_phase: "resolve".into(),
                primary_diagnostic: "ORNA100-E-SYS-RUNTIME".into(),
                message_contains: "`sys.runtime` was renamed to `sys.rt`".into(),
                status: "expected-not-executed".into(),
            },
        )]),
        vectors: std::collections::BTreeMap::new(),
        scenarios: serde_json::json!({"scenarios": []}),
        requirements: vec![
            Requirement {
                id: "ORNA-SYS-005".into(),
                chapter: "system".into(),
                source: "source/15-system.md".into(),
                text: "The unrecognised member `sys.runtime` MUST produce ORNA100-E-SYS-RUNTIME."
                    .into(),
            },
            Requirement {
                id: "ORNA-SYS-006".into(),
                chapter: "system".into(),
                source: "source/15-system.md".into(),
                text: "A conforming runtime MUST expose the compatibility fields of sys.RuntimeInfo."
                    .into(),
            },
        ],
        requirement_evidence: RequirementEvidence {
            meaning: "focused system semantic Harness witness".into(),
            requirements: vec![
                RequirementEvidenceEntry {
                    requirement: "ORNA-SYS-005".into(),
                    tests: vec![serde_json::json!({
                        "kind": "invalid system runtime spelling Harness witness",
                        "status": "planned",
                        "subject": "ORNA-SYS-005",
                        "fixture": invalid_id,
                        "stage": "resolve",
                    })],
                },
                RequirementEvidenceEntry {
                    requirement: "ORNA-SYS-006".into(),
                    tests: vec![serde_json::json!({
                        "kind": "sys.rt.info semantic pass Harness witness",
                        "status": "planned",
                        "subject": "ORNA-SYS-006",
                        "fixture": valid_id,
                        "stage": "typecheck",
                    })],
                },
            ],
        },
        project_expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
        publication_digests: std::collections::BTreeMap::new(),
    };

    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let valid = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == valid_id)
        .expect("valid runtime-info fixture");
    assert!(valid.passed, "{:?}", valid.stages);
    let valid_typecheck = valid
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("runtime-info typecheck stage");
    assert_eq!(valid_typecheck.class, EvidenceClass::Semantic);
    assert_eq!(valid_typecheck.status, EvidenceStatus::Passed);
    assert!(valid_typecheck.expectation_satisfied);
    assert_eq!(
        valid_typecheck.requirements,
        vec!["ORNA-SYS-006".to_string()]
    );
    assert!(matches!(
        &valid_typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-006".to_string()]
    ));

    let invalid = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == invalid_id)
        .expect("invalid runtime spelling fixture");
    assert!(invalid.passed, "{:?}", invalid.stages);
    let invalid_resolve = invalid
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Resolve))
        .expect("runtime spelling resolve stage");
    assert_eq!(invalid_resolve.class, EvidenceClass::Semantic);
    assert_eq!(invalid_resolve.status, EvidenceStatus::Failed);
    assert!(invalid_resolve.expectation_satisfied);
    assert_eq!(
        invalid_resolve.requirements,
        vec!["ORNA-SYS-005".to_string()]
    );
    assert_eq!(
        invalid_resolve
            .diagnostic
            .as_ref()
            .expect("runtime spelling diagnostic")["code"],
        "ORNA100-E-SYS-RUNTIME"
    );

    // The semantic adapter deliberately skips execution here: this witness
    // does not populate or assert any runtime coordinates.
    assert!(report
        .runtime_evidence
        .iter()
        .all(|evidence| evidence.stage != Some(Stage::Evaluate)));
    assert!(report
        .skipped_evidence
        .iter()
        .any(|evidence| evidence.stage == Some(Stage::Evaluate)));
    assert!(report
        .semantic_evidence
        .iter()
        .any(|evidence| evidence.subject == valid_id
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.status == EvidenceStatus::Passed));
    let serialized = serde_json::to_value(&report).expect("runtime-info report serializes");
    assert!(serialized["runtime_evidence"]
        .as_array()
        .expect("serialized runtime evidence")
        .iter()
        .all(|evidence| evidence["stage"] != "evaluate"));
    let serialized_invalid = serialized["fixtures"]
        .as_array()
        .expect("serialized fixture array")
        .iter()
        .find(|fixture| fixture["fixture"] == invalid_id)
        .expect("serialized invalid runtime fixture");
    let serialized_resolve = serialized_invalid["stages"]
        .as_array()
        .expect("serialized invalid stages")
        .iter()
        .find(|stage| stage["stage"] == "resolve")
        .expect("serialized runtime spelling resolve stage");
    assert_eq!(serialized_resolve["class"], "semantic");
    assert_eq!(serialized_resolve["status"], "failed");
    assert_eq!(
        serialized_resolve["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-005"])
    );
    assert_eq!(
        serialized_resolve["diagnostic"]["code"],
        "ORNA100-E-SYS-RUNTIME"
    );
    assert!(serialized["semantic_evidence"]
        .as_array()
        .expect("serialized semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == valid_id
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "passed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-006"])
        }));
}

#[test]
fn harness_maps_snapshot_selection_and_serializes_snapshot_type_diagnostic() {
    let valid = invoke_report(
        typed_invoke_fixture(
            "snapshot-valid",
            "snapshot-valid.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "pass"),
                ("evaluate", "not-run"),
            ],
            None,
            None,
            None,
        ),
        r#"pub fn before_change() = sys.snapshot("HEAD~3");"#,
        "ORNA-SYS-011",
    );
    let valid_fixture = &valid.fixtures[0];
    assert!(valid_fixture.passed, "{:?}", valid_fixture.stages);
    let valid_typecheck = valid_fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("snapshot typecheck stage");
    assert_eq!(valid_typecheck.class, EvidenceClass::Semantic);
    assert_eq!(valid_typecheck.status, EvidenceStatus::Passed);
    assert_eq!(
        valid_typecheck.requirements,
        vec!["ORNA-SYS-011".to_string()]
    );
    assert!(matches!(
        &valid_typecheck.requirement_mapping,
        RequirementMapping::Mapped { requirements }
            if requirements == &vec!["ORNA-SYS-011".to_string()]
    ));
    assert!(valid.semantic_evidence.iter().any(|evidence| {
        evidence.subject == "snapshot-valid"
            && evidence.stage == Some(Stage::Typecheck)
            && evidence.class == EvidenceClass::Semantic
            && evidence.status == EvidenceStatus::Passed
    }));

    let invalid = invoke_report(
        typed_invoke_fixture(
            "snapshot-non-string",
            "snapshot-non-string.orna",
            &[
                ("parse", "pass"),
                ("resolve", "pass"),
                ("typecheck", "fail"),
                ("evaluate", "not-run"),
            ],
            Some("typecheck"),
            Some("ORNA-S021-TYPE"),
            None,
        ),
        r#"pub fn before_change() = sys.snapshot(42);"#,
        "ORNA-SYS-011",
    );
    let invalid_fixture = &invalid.fixtures[0];
    assert!(invalid_fixture.passed, "{:?}", invalid_fixture.stages);
    let invalid_typecheck = invalid_fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(Stage::Typecheck))
        .expect("snapshot non-string typecheck stage");
    assert_eq!(invalid_typecheck.class, EvidenceClass::Semantic);
    assert_eq!(invalid_typecheck.status, EvidenceStatus::Failed);
    assert_eq!(
        invalid_typecheck.requirements,
        vec!["ORNA-SYS-011".to_string()]
    );
    assert_eq!(
        invalid_typecheck
            .diagnostic
            .as_ref()
            .expect("snapshot type diagnostic")["code"],
        "ORNA-S021-TYPE"
    );

    let serialized_valid = serde_json::to_value(&valid).expect("valid snapshot report serializes");
    let serialized_valid_typecheck = serialized_valid["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized valid stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized valid snapshot typecheck stage");
    assert_eq!(serialized_valid_typecheck["class"], "semantic");
    assert_eq!(serialized_valid_typecheck["status"], "passed");
    assert_eq!(
        serialized_valid_typecheck["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-011"])
    );
    assert!(serialized_valid["semantic_evidence"]
        .as_array()
        .expect("serialized valid semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == "snapshot-valid"
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "passed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-011"])
        }));

    let serialized_invalid =
        serde_json::to_value(&invalid).expect("invalid snapshot report serializes");
    let serialized_invalid_typecheck = serialized_invalid["fixtures"][0]["stages"]
        .as_array()
        .expect("serialized invalid stage array")
        .iter()
        .find(|stage| stage["stage"] == "typecheck")
        .expect("serialized invalid snapshot typecheck stage");
    assert_eq!(serialized_invalid_typecheck["class"], "semantic");
    assert_eq!(serialized_invalid_typecheck["status"], "failed");
    assert_eq!(
        serialized_invalid_typecheck["requirement_mapping"]["requirements"],
        serde_json::json!(["ORNA-SYS-011"])
    );
    assert_eq!(
        serialized_invalid_typecheck["diagnostic"]["code"],
        "ORNA-S021-TYPE"
    );
    assert!(serialized_invalid["semantic_evidence"]
        .as_array()
        .expect("serialized invalid semantic evidence")
        .iter()
        .any(|evidence| {
            evidence["subject"] == "snapshot-non-string"
                && evidence["stage"] == "typecheck"
                && evidence["status"] == "failed"
                && evidence["requirement_mapping"]["requirements"]
                    == serde_json::json!(["ORNA-SYS-011"])
                && evidence["diagnostic"]["code"] == "ORNA-S021-TYPE"
        }));
}
