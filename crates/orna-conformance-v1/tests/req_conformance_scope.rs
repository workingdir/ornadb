use orna_conformance_v1::{Corpus, EvidenceStatus, Harness, SemanticAdapter, SkippingAdapter};

/// ORNA-CONF-002 and ORNA-CONF-003: reports distinguish test execution from static specification evidence.
#[test]
fn skipped_implementation_is_not_reported_as_conformant() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(report.static_evidence[0].status, EvidenceStatus::Passed);
    assert!(!report.skipped_evidence.is_empty());
    assert!(report.runtime_evidence.is_empty());
    assert!(report.fixtures.iter().all(|fixture| !fixture.passed));
}

/// ORNA-CONF-004: the implementation preserves the meaning of published valid source fixtures.
#[test]
fn semantic_adapter_executes_valid_published_fixtures() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    let mut adapter = SemanticAdapter::default();
    let report = Harness::new(corpus).run(&mut adapter);
    assert!(report.fixtures.iter().any(|fixture| fixture.passed));
}

/// ORNA-CONF-006: full-runtime claims require executed scenarios, not static corpus checks.
#[test]
fn skipped_runtime_scenarios_are_never_promoted_to_passes() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert!(report.scenarios.iter().all(|scenario| scenario.status != EvidenceStatus::Passed));
    assert!(report.scenarios.iter().any(|scenario| scenario.status == EvidenceStatus::Skipped));
}

/// ORNA-TEST-001 and ORNA-TEST-002: valid and invalid fixtures carry phase-specific expectations.
#[test]
fn corpus_indexes_valid_fixtures_and_stable_invalid_diagnostics() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    assert!(!corpus.manifest.fixtures.is_empty());
    assert!(!corpus.invalid_metadata.fixtures.is_empty());
    assert!(corpus.invalid_metadata.fixtures.iter().all(|fixture| !fixture.failing_phase.is_empty() && !fixture.diagnostic.is_empty()));
}

/// ORNA-TEST-003: project-runner evidence is distinct from full engine conformance.
#[test]
fn project_adapter_does_not_claim_full_engine_conformance() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    let evidence = orna_conformance_v1::run_reference_project_runtime_adapter(&corpus);
    assert_eq!(evidence.status, EvidenceStatus::Passed);
    assert!(!evidence.invocations.is_empty());
    assert!(!evidence.full_orna_engine_conformance);
}

/// ORNA-TEST-005: the corpus includes valid and deliberately invalid fixtures.
#[test]
fn corpus_covers_valid_and_invalid_syntax_forms() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    assert!(corpus.manifest.fixtures.iter().any(|fixture| fixture.path.starts_with("examples/valid/")));
    assert!(corpus.manifest.fixtures.iter().any(|fixture| fixture.path.starts_with("examples/invalid/")));
}

/// ORNA-TEST-006: requirement evidence is a generated reporting layer over ordinary tests.
#[test]
fn requirement_coverage_has_nonempty_test_mappings() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    assert!(!corpus.requirement_evidence.requirements.is_empty());
    assert!(corpus.requirement_evidence.requirements.iter().all(|entry| !entry.tests.is_empty()));
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert_eq!(report.coverage.mapped_stage_evidence, 0);
    assert!(report.coverage.unmapped_stage_evidence > 0);
    assert!(!report.skipped_evidence.is_empty());
}

/// ORNA-TEST-007 and ORNA-TEST-008: harness unit tests run in-process without a server or a mocks ban.
#[test]
fn harness_executes_in_process() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    let mut adapter = SkippingAdapter;
    let report = Harness::new(corpus).run(&mut adapter);
    assert!(!report.fixtures.is_empty());
}

/// ORNA-TEST-009: a complete-project fixture must explicitly index legacy row loading.
#[test]
fn project_manifest_requires_explicit_row_admission() {
    let corpus = Corpus::load_default().expect("published corpus validates");
    assert!(corpus.manifest.fixtures.iter().any(|fixture| fixture.kind == "project"));
    assert!(corpus.manifest.fixtures.iter().filter(|fixture| fixture.kind == "project").all(|fixture| fixture.expect.contains_key("load_rows")));
}
