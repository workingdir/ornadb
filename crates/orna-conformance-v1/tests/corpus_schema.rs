use orna_conformance_v1::Corpus;

#[test]
fn corpus_rejects_duplicate_fixture_paths_before_adapter_execution() {
    let mut corpus = Corpus::load_default().expect("published corpus loads");
    corpus.manifest.fixtures[1].path = corpus.manifest.fixtures[0].path.clone();

    assert_eq!(
        corpus
            .validate()
            .expect_err("duplicate paths are not an executable corpus")
            .to_string(),
        "fixture paths must be unique"
    );
}

#[test]
fn corpus_rejects_invalid_metadata_that_does_not_exactly_cover_invalid_fixtures() {
    let mut corpus = Corpus::load_default().expect("published corpus loads");
    corpus.invalid_metadata.fixtures[0].path = "examples/invalid/not-published.orna".into();

    assert_eq!(
        corpus
            .validate()
            .expect_err("invalid metadata cannot name a different corpus")
            .to_string(),
        "invalid metadata and expected diagnostics must exactly cover invalid fixtures"
    );
}

#[test]
fn corpus_rejects_unknown_stage_expectations_before_reporting_evidence() {
    let mut corpus = Corpus::load_default().expect("published corpus loads");
    corpus.manifest.fixtures[0]
        .expect
        .insert("invented-stage".into(), "pass".into());

    assert_eq!(
        corpus
            .validate()
            .expect_err("unknown stage must not acquire a conformance claim")
            .to_string(),
        format!(
            "fixture has an invalid stage expectation: {}",
            corpus.manifest.fixtures[0].path
        )
    );
}
