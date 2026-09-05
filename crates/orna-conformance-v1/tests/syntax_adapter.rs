use orna_conformance_v1::{Corpus, EvidenceStatus, Harness, Stage, SyntaxAdapter};

fn report() -> orna_conformance_v1::RunReport {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    Harness::new(corpus).run(&mut SyntaxAdapter)
}

#[test]
fn valid_fixtures_pass_the_parse_stage() {
    let report = report();
    let valid = report
        .fixtures
        .iter()
        .filter(|fixture| fixture.fixture.starts_with("valid/"))
        .collect::<Vec<_>>();
    assert_eq!(valid.len(), 86);
    assert!(valid.iter().all(|fixture| {
        fixture.stages[0].stage == Some(Stage::Parse)
            && fixture.stages[0].status == EvidenceStatus::Passed
    }));
}

#[test]
fn invalid_parse_fixtures_fail_with_the_expected_primary_code() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus.clone()).run(&mut SyntaxAdapter);
    for expected in corpus
        .manifest
        .fixtures
        .iter()
        .filter(|fixture| fixture.failing_phase.as_deref() == Some("parse"))
    {
        let actual = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == expected.id)
            .expect("fixture result");
        assert_eq!(
            actual.stages[0].status,
            EvidenceStatus::Failed,
            "{}",
            expected.id
        );
        assert_eq!(
            actual.stages[0]
                .diagnostic
                .as_ref()
                .and_then(|value| value["code"].as_str()),
            expected.diagnostic.as_deref(),
            "{}",
            expected.id
        );
        assert!(
            actual.stages[0].detail.contains("expectation satisfied"),
            "{}: {}",
            expected.id,
            actual.stages[0].detail
        );
    }
}

#[test]
fn semantic_and_runtime_stages_remain_explicitly_skipped_without_source_disclosure() {
    let report = report();
    let valid = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("valid fixture");
    assert_eq!(valid.stages[0].status, EvidenceStatus::Passed);
    assert!(
        valid.stages[1..]
            .iter()
            .all(|stage| stage.status == EvidenceStatus::Skipped)
    );
    assert!(valid.stages[1].detail.contains("NOT satisfied"));

    let invalid = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "invalid/legacy-var.orna")
        .expect("parse-invalid fixture");
    let diagnostic = invalid.stages[0]
        .diagnostic
        .as_ref()
        .expect("parse diagnostic");
    let serialized = serde_json::to_string(diagnostic).expect("diagnostic serializes");
    assert!(!serialized.contains("/home/"));
    assert!(!serialized.contains("var thing"));
    assert_eq!(diagnostic["spans"], serde_json::json!([]));
    assert_eq!(diagnostic["redacted"], true);
}
