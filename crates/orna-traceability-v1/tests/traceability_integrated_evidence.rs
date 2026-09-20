use orna_conformance_v1::{BoundedEvaluator, Corpus, FixtureStageBinding, Harness, RuntimeAdapter, Stage};
use orna_traceability_v1::{generate_with_engine_witnesses, Status};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const REQUIREMENT_ID: &str = "ORNA-SOURCE-001";
const FIXTURE_ID: &str = "valid/minimal-root.orna";
const FIXTURE_PATH: &str = "examples/valid/minimal-root.orna";
const IMPLEMENTATION_REF: &str =
    "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses";
const TEST_REF: &str = "crates/orna-traceability-v1/tests/traceability_integrated_evidence.rs::declared_parse_witness_preserves_publication_identity_and_partial_requirement_boundary";
const CANONICAL_MARKDOWN_DIGEST: &str =
    "d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d";

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0"))
}

struct CopiedReference {
    path: PathBuf,
}

impl Drop for CopiedReference {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn copied_reference() -> CopiedReference {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.scratch")
        .join(format!("orna-traceability-integrated-{nonce}"));
    copy_dir(&reference_root(), &path);
    CopiedReference { path }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copied reference directory");
    for entry in fs::read_dir(from).expect("read authoritative reference directory") {
        let entry = entry.expect("read authoritative reference entry");
        let destination = to.join(entry.file_name());
        if entry
            .file_type()
            .expect("read authoritative reference entry type")
            .is_dir()
        {
            copy_dir(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy authoritative reference file");
        }
    }
}

fn declare_fixture_evidence(root: &Path) {
    let evidence_path = root.join("tests/requirement-evidence.json");
    let mut evidence: Value = serde_json::from_str(
        &fs::read_to_string(&evidence_path).expect("read copied requirement evidence"),
    )
    .expect("parse copied requirement evidence");
    let requirement = evidence["requirements"]
        .as_array_mut()
        .expect("requirement evidence array")
        .iter_mut()
        .find(|entry| entry["requirement"] == REQUIREMENT_ID)
        .expect("requirement exists in authoritative evidence");
    requirement["tests"][0]["fixture"] = Value::String(FIXTURE_ID.into());
    requirement["tests"][0]["path"] = Value::String(FIXTURE_PATH.into());
    fs::write(
        evidence_path,
        serde_json::to_vec(&evidence).expect("serialize copied requirement evidence"),
    )
    .expect("write copied requirement evidence");
}

#[test]
fn declared_parse_witness_preserves_publication_identity_and_partial_requirement_boundary() {
    let copied = copied_reference();
    declare_fixture_evidence(&copied.path);

    let corpus = Corpus::load(&copied.path).expect("copied authoritative corpus loads");
    let expected_digests = corpus.publication_digests.clone();
    assert_eq!(
        expected_digests.get("Orna-1.0.0.md").map(String::as_str),
        Some(CANONICAL_MARKDOWN_DIGEST),
        "copied corpus keeps the canonical publication identity"
    );

    let harness = Harness::new(corpus);
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let conformance_report = harness.run(&mut adapter);
    let binding = FixtureStageBinding {
        requirement_id: REQUIREMENT_ID.into(),
        fixture_id: FIXTURE_ID.into(),
        fixture_path: FIXTURE_PATH.into(),
        stage: Stage::Parse,
        implementation_ref: IMPLEMENTATION_REF.into(),
        test_ref: TEST_REF.into(),
    };
    let witnesses = harness
        .engine_witnesses(&conformance_report, std::slice::from_ref(&binding))
        .expect("declared passing parse stage becomes an engine witness");
    assert_eq!(witnesses.publication_digests(), &expected_digests);

    let report = generate_with_engine_witnesses(&copied.path, &witnesses)
        .expect("public traceability consumer accepts the declared witness");
    assert_eq!(report.publication_digests, expected_digests);

    let requirement = report
        .requirements
        .iter()
        .find(|requirement| requirement.requirement_id == REQUIREMENT_ID)
        .expect("witnessed requirement appears in report");
    assert_eq!(requirement.status, Status::PartiallyExecuted);
    assert!(requirement.boundaries.iter().any(|boundary| {
        boundary.kind == "engine-witness"
            && boundary.logical_id == format!("{FIXTURE_ID}:parse")
            && boundary.status == Status::Executed
            && boundary.implementation_ref.as_deref() == Some(IMPLEMENTATION_REF)
            && boundary.test_ref.as_deref() == Some(TEST_REF)
    }));
}
