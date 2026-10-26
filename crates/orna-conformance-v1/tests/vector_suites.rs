use orna_conformance_v1::{
    Corpus, EvidenceStatus, Harness, SkippingAdapter, VECTOR_FILES, VectorSuiteEvidence,
    run_vector_suites,
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Release `SHA256SUMS` values of the Orna 1.1.0 vector files the corpus
/// vendors byte for byte.
const RELEASE_VECTOR_FILES: [(&str, &[u8], &str); 6] = [
    (
        "float-vectors.json",
        include_bytes!("fixtures/reference/tests/float-vectors.json"),
        "2719675ea3dfa68d5b710bc238c1ac66e73588a74e8814b720c2232c9343c015",
    ),
    (
        "numeric-vectors.json",
        include_bytes!("fixtures/reference/tests/numeric-vectors.json"),
        "16b73a224efe971fa9b6190a860dac80611e987416872d1f6035c2994d247c12",
    ),
    (
        "path-vectors.json",
        include_bytes!("fixtures/reference/tests/path-vectors.json"),
        "1b3e0be29c662f0eaed9d7ad849da4acd7237979fb68c43749a42bc7c401f054",
    ),
    (
        "protocol-vectors.json",
        include_bytes!("fixtures/reference/tests/protocol-vectors.json"),
        "beebf327b31fcc6e35413f9839af09849c4ca528f2736c118b9abed73ff12aa7",
    ),
    (
        "snapshot-vectors.json",
        include_bytes!("fixtures/reference/tests/snapshot-vectors.json"),
        "1facde5fb2faae65d5ff121811d14c826647086a504dcbd0de3ccb1a3b0e1b2a",
    ),
    (
        "value-vectors.json",
        include_bytes!("fixtures/reference/tests/value-vectors.json"),
        "b48369756a137544a9d3888f17ad10a97e7a2010618ec084a5b8b3b603844095",
    ),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn suite<'a>(suites: &'a [VectorSuiteEvidence], name: &str) -> &'a VectorSuiteEvidence {
    suites
        .iter()
        .find(|suite| suite.suite == name)
        .unwrap_or_else(|| panic!("{name} has no result"))
}

#[test]
fn vendored_vector_files_match_the_release() {
    assert_eq!(
        RELEASE_VECTOR_FILES.map(|(name, _, _)| name),
        VECTOR_FILES,
        "every harness vector suite is pinned"
    );
    for (name, bytes, release_sha256) in RELEASE_VECTOR_FILES {
        assert_eq!(hex(&Sha256::digest(bytes)), release_sha256, "{name}");
    }
}

/// ORNA-TEST-011: suites with a production implementation pass through it;
/// the others are explicit known gaps naming their tracking bead.
#[test]
fn published_vector_suites_run_through_production_or_name_their_gap() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let suites = run_vector_suites(&corpus);
    assert_eq!(
        suites
            .iter()
            .map(|suite| suite.suite.as_str())
            .collect::<Vec<_>>(),
        VECTOR_FILES
    );

    for (name, cases) in [
        ("path-vectors.json", 41),
        ("protocol-vectors.json", 13),
        ("snapshot-vectors.json", 4),
        ("value-vectors.json", 17),
    ] {
        let result = suite(&suites, name);
        assert_eq!(result.status, EvidenceStatus::Passed, "{result:?}");
        assert_eq!(result.executed_cases, cases, "{name}");
        assert!(result.failures.is_empty(), "{result:?}");
        assert_eq!(result.known_gap, None, "{name}");
    }

    for (name, bead) in [
        ("float-vectors.json", "ornadb-5il1.188"),
        ("numeric-vectors.json", "ornadb-5il1.190"),
    ] {
        let result = suite(&suites, name);
        assert_eq!(result.status, EvidenceStatus::Skipped, "{name}");
        assert_eq!(result.known_gap.as_deref(), Some(bead), "{name}");
        assert_eq!(result.executed_cases, 0, "{name}");
        assert!(!result.detail.is_empty(), "{name}");
    }
}

#[test]
fn harness_reports_vector_suites_apart_from_adapter_evidence() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let expected = run_vector_suites(&corpus);
    let report = Harness::new(corpus).run(&mut SkippingAdapter);
    assert_eq!(report.vector_suites, expected);
    // No adapter ran, so production vector execution is not adapter
    // runtime evidence, and the vectors are no longer reported as merely
    // loaded model data.
    assert!(report.runtime_evidence.is_empty());
    assert!(
        report
            .model_evidence
            .iter()
            .all(|evidence| !evidence.subject.contains("vector"))
    );
    let serialized = serde_json::to_value(&report).expect("report serializes");
    let float = serialized["vector_suites"]
        .as_array()
        .expect("serialized vector suites")
        .iter()
        .find(|suite| suite["suite"] == "float-vectors.json")
        .expect("serialized float suite");
    assert_eq!(float["status"], "skipped");
    assert_eq!(float["known_gap"], "ornadb-5il1.188");
}

/// Each executed suite really runs its cases: one disagreeing published case
/// fails that suite and names the case.
#[test]
fn a_disagreeing_vector_fails_its_suite() {
    let mutations: [(&str, fn(&mut serde_json::Value), &str); 4] = [
        (
            "value-vectors.json",
            |vectors| vectors[0]["canonical_roundtrip"] = json!(false),
            "[0] zero",
        ),
        (
            "snapshot-vectors.json",
            |vectors| vectors["invalid_bare_cwd"] = vectors["cwd_1"].clone(),
            "invalid_bare_cwd",
        ),
        (
            "protocol-vectors.json",
            // A non-minimal map-length head is not canonical CBOR.
            |vectors| {
                let hex = vectors[0]["hex"].as_str().expect("protocol hex").to_owned();
                vectors[0]["hex"] = json!(format!("b805{}", &hex[2..]));
            },
            "[0] subscribe",
        ),
        (
            "path-vectors.json",
            |vectors| vectors["round_trip"][0]["encoded"] = json!("alice_smith"),
            "round_trip[0]",
        ),
    ];
    for (name, mutate, case) in mutations {
        let mut corpus = Corpus::load_default().expect("reference corpus loads");
        mutate(corpus.vectors.get_mut(name).expect("suite is loaded"));
        let suites = run_vector_suites(&corpus);
        let result = suite(&suites, name);
        assert_eq!(result.status, EvidenceStatus::Failed, "{name}");
        assert_eq!(result.failures.len(), 1, "{result:?}");
        assert!(result.failures[0].starts_with(case), "{result:?}");
    }
}

#[test]
fn an_unknown_or_empty_suite_fails_instead_of_passing() {
    let mut corpus = Corpus::load_default().expect("reference corpus loads");
    corpus
        .vectors
        .insert("extra-vectors.json".into(), json!([]));
    corpus
        .vectors
        .insert("value-vectors.json".into(), json!([]));
    let suites = run_vector_suites(&corpus);

    let unknown = suite(&suites, "extra-vectors.json");
    assert_eq!(unknown.status, EvidenceStatus::Failed);
    assert_eq!(unknown.known_gap, None);

    let empty = suite(&suites, "value-vectors.json");
    assert_eq!(empty.status, EvidenceStatus::Failed);
    assert_eq!(empty.executed_cases, 0);
}
