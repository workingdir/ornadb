use orna_conformance_v1::{Corpus, EvidenceStatus, Harness, SemanticAdapter, Stage};

#[test]
fn reports_identifiable_language_processor_diagnostic_corpus_results() {
    let corpus = Corpus::load_default().expect("frozen reference corpus loads");
    let metadata = corpus.invalid_metadata.fixtures.clone();
    let report = Harness::new(corpus.clone()).run(&mut SemanticAdapter::default());

    let mut checked = 0;
    let mut matched = 0;
    let mut mismatches = Vec::new();
    for expected in metadata.iter().filter(|fixture| {
        matches!(
            fixture.failing_phase.as_str(),
            "parse" | "resolve" | "typecheck"
        )
    }) {
        let fixture_id = corpus
            .manifest
            .fixtures
            .iter()
            .find(|fixture| fixture.path == expected.path)
            .expect("invalid metadata path is in the manifest")
            .id
            .as_str();
        let actual = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .unwrap_or_else(|| panic!("{}: no harness result", expected.path));
        let stage = match expected.failing_phase.as_str() {
            "parse" => Stage::Parse,
            "resolve" => Stage::Resolve,
            "typecheck" => Stage::Typecheck,
            _ => unreachable!(),
        };
        let evidence = actual
            .stages
            .iter()
            .find(|evidence| evidence.stage.as_ref() == Some(&stage))
            .unwrap_or_else(|| panic!("{}: no {:?} result", expected.path, stage));

        if evidence.status == EvidenceStatus::Failed && evidence.expectation_satisfied {
            matched += 1;
        } else {
            mismatches.push(format!(
                "{}: expected {} / {}, observed {:?} / {:?}",
                expected.path,
                expected.failing_phase,
                expected.diagnostic,
                evidence.status,
                evidence
                    .diagnostic
                    .as_ref()
                    .and_then(|value| value["code"].as_str())
            ));
        }
        checked += 1;
    }

    assert_eq!(
        checked, 78,
        "all parse, resolve, and typecheck corpus forms"
    );
    let mut unsupported = Vec::new();
    for expected in metadata.iter().filter(|fixture| {
        matches!(
            fixture.failing_phase.as_str(),
            "evaluate" | "row-validation"
        )
    }) {
        let fixture_id = corpus
            .manifest
            .fixtures
            .iter()
            .find(|fixture| fixture.path == expected.path)
            .expect("invalid metadata path is in the manifest")
            .id
            .as_str();
        let actual = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .unwrap_or_else(|| panic!("{}: no harness result", expected.path));
        let stage = match expected.failing_phase.as_str() {
            "evaluate" => Stage::Evaluate,
            "row-validation" => Stage::RowValidation,
            _ => unreachable!(),
        };
        let evidence = actual
            .stages
            .iter()
            .find(|evidence| evidence.stage.as_ref() == Some(&stage))
            .unwrap_or_else(|| panic!("{}: no {:?} result", expected.path, stage));
        assert_eq!(
            evidence.status,
            EvidenceStatus::Skipped,
            "{}: {}",
            expected.path,
            evidence.detail
        );
        unsupported.push(format!("{}: {:?}", expected.path, evidence.status));
    }
    assert_eq!(unsupported.len(), 2);
    println!(
        "ORNA-CONF-005 audit: {matched}/{checked} parse, resolve, and typecheck cases matched; {} evaluate/row-validation cases were skipped by this language-processor adapter.",
        unsupported.len()
    );
    for skipped in unsupported {
        println!("  skipped: {skipped}");
    }
    println!("Primary diagnostic mismatches: {}", mismatches.len());
    for mismatch in mismatches {
        println!("  {mismatch}");
    }
}
