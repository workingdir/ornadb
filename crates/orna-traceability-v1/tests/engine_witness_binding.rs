use orna_conformance_v1::{
    BoundedEvaluator, Corpus, FixtureStageBinding, Harness, RuntimeAdapter, Stage,
};
use orna_traceability_v1::{generate_with_engine_witnesses, Status};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const FIXTURE_ID: &str = "valid/minimal-root.orna";
const FIXTURE_PATH: &str = "examples/valid/minimal-root.orna";
const IMPLEMENTATION_REF: &str =
    "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses";
const TEST_REF: &str =
    "crates/orna-traceability-v1/tests/engine_witness_binding.rs::engine_witness_accepts_requirement_declared_for_exact_fixture_and_path";

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0"))
}

fn copied_reference(label: &str) -> PathBuf {
    let target = std::env::temp_dir().join(format!(
        "orna-traceability-engine-witness-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&target);
    copy_dir(&reference_root(), &target);
    target
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create reference copy directory");
    for entry in fs::read_dir(from).expect("read authoritative reference directory") {
        let entry = entry.expect("read reference entry");
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("read reference entry type").is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy authoritative reference file");
        }
    }
}

fn declare_fixture_evidence(root: &Path, requirement_id: &str) {
    let evidence_path = root.join("tests/requirement-evidence.json");
    let mut evidence: Value = serde_json::from_str(
        &fs::read_to_string(&evidence_path).expect("read authoritative requirement evidence"),
    )
    .expect("parse authoritative requirement evidence");
    let requirement = evidence["requirements"]
        .as_array_mut()
        .expect("requirement evidence array")
        .iter_mut()
        .find(|entry| entry["requirement"] == requirement_id)
        .expect("declared requirement exists in authoritative evidence");
    requirement["tests"][0]["fixture"] = Value::String(FIXTURE_ID.into());
    requirement["tests"][0]["path"] = Value::String(FIXTURE_PATH.into());
    fs::write(
        evidence_path,
        serde_json::to_vec(&evidence).expect("serialize requirement evidence"),
    )
    .expect("write copied requirement evidence");
}

fn witnesses_for(root: &Path, requirement_id: &str) -> orna_conformance_v1::EngineWitnesses {
    let corpus = Corpus::load(root).expect("authoritative corpus loads");
    let harness = Harness::new(corpus);
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let report = harness.run(&mut adapter);
    let binding = FixtureStageBinding {
        requirement_id: requirement_id.into(),
        fixture_id: FIXTURE_ID.into(),
        fixture_path: FIXTURE_PATH.into(),
        stage: Stage::Parse,
        implementation_ref: IMPLEMENTATION_REF.into(),
        test_ref: TEST_REF.into(),
    };
    harness
        .engine_witnesses(&report, std::slice::from_ref(&binding))
        .expect("executed fixture stage becomes a digest-bound witness")
}

#[test]
fn engine_witness_accepts_requirement_declared_for_exact_fixture_and_path() {
    let root = copied_reference("declared");
    declare_fixture_evidence(&root, "ORNA-SOURCE-001");
    let witnesses = witnesses_for(&root, "ORNA-SOURCE-001");

    let report = generate_with_engine_witnesses(&root, &witnesses)
        .expect("requirement evidence declaration binds the engine witness");
    let requirement = report
        .requirements
        .iter()
        .find(|requirement| requirement.requirement_id == "ORNA-SOURCE-001")
        .expect("witnessed requirement appears in report");
    assert_eq!(requirement.status, Status::PartiallyExecuted);
    assert!(requirement.boundaries.iter().any(|boundary| {
        boundary.kind == "engine-witness"
            && boundary.status == Status::Executed
            && boundary.implementation_ref.as_deref() == Some(IMPLEMENTATION_REF)
            && boundary.test_ref.as_deref() == Some(TEST_REF)
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn engine_witness_rejects_unrelated_globally_known_requirement() {
    let root = copied_reference("unrelated");
    declare_fixture_evidence(&root, "ORNA-SOURCE-001");
    let witnesses = witnesses_for(&root, "ORNA-CONF-001");

    let error = generate_with_engine_witnesses(&root, &witnesses)
        .expect_err("a globally known but unrelated requirement must be rejected");
    assert!(error
        .to_string()
        .contains("engine witness requirement evidence does not declare fixture"));

    let _ = fs::remove_dir_all(root);
}
