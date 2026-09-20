use orna_conformance_v1::{
    EvidenceStatus, Harness, ImplementationEvidenceAggregate, ImplementationEvidenceBinding,
    ImplementationEvidenceSource, Corpus,
};
use orna_traceability_v1::{generate, generate_with_implementation_evidence_overlay, Status};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const REQUIREMENT_ID: &str = "ORNA-RANGE-001";
const FIXTURE_ID: &str = "valid/minimal-root.orna";
const PUBLICATION_DIGEST: &str =
    "d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d";

struct TemporaryReference(PathBuf);

impl TemporaryReference {
    fn new(label: &str) -> Self {
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!(
                "orna-traceability-status-boundaries-{label}-{}",
                std::process::id()
            ));
        let _ = fs::remove_dir_all(&target);
        copy_dir(&reference_root(), &target);
        Self(target)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryReference {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0"))
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create temporary reference directory");
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

fn production_binding(
    publication_digests: BTreeMap<String, String>,
    implementation_ref: &str,
    test_ref: &str,
    observed_status: EvidenceStatus,
) -> ImplementationEvidenceBinding {
    ImplementationEvidenceBinding {
        requirement_id: REQUIREMENT_ID.into(),
        publication_digests,
        implementation_ref: implementation_ref.into(),
        test_ref: test_ref.into(),
        source: ImplementationEvidenceSource::ProductionUnit,
        subject: format!("{FIXTURE_ID} bounded production observation"),
        command: "cargo test -p orna-evaluator-v1 --test evaluator".into(),
        result: "bounded result recorded without Orna-engine execution".into(),
        observed_status,
    }
}

#[test]
fn bounded_evidence_cannot_promote_mixed_requirement_to_executed() {
    let root = TemporaryReference::new("mixed");
    let corpus = Corpus::load(root.path()).expect("copied authoritative corpus loads");
    let publication_digests = corpus.publication_digests.clone();
    let harness = Harness::new(corpus);
    let bindings = [
        production_binding(
            publication_digests.clone(),
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::range_passes",
            EvidenceStatus::Passed,
        ),
        production_binding(
            publication_digests,
            "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
            "crates/orna-evaluator-v1/tests/evaluator.rs::range_fails",
            EvidenceStatus::Failed,
        ),
    ];
    let overlay = harness
        .implementation_evidence_overlay(&bindings)
        .expect("bounded production bindings are accepted");
    assert_eq!(overlay.aggregate(), ImplementationEvidenceAggregate::PartiallyExecuted);

    let report = generate_with_implementation_evidence_overlay(root.path(), &overlay)
        .expect("bounded evidence report is accepted");
    assert_eq!(
        report.publication_digests.get("Orna-1.0.0.md").map(String::as_str),
        Some(PUBLICATION_DIGEST)
    );
    assert!(report
        .requirements
        .iter()
        .all(|requirement| requirement.status != Status::Executed));

    let requirement = report
        .requirements
        .iter()
        .find(|requirement| requirement.requirement_id == REQUIREMENT_ID)
        .expect("corpus requirement exists");
    assert_eq!(requirement.status, Status::PartiallyExecuted);
    assert!(requirement
        .boundaries
        .iter()
        .any(|boundary| boundary.status == Status::JustifiedGap));
    let bounded = requirement
        .boundaries
        .iter()
        .filter(|boundary| boundary.kind == "bounded-production-evidence")
        .collect::<Vec<_>>();
    assert_eq!(bounded.len(), 2);
    assert!(bounded
        .iter()
        .all(|boundary| boundary.status == Status::PartiallyExecuted));
    let frozen_report = generate(root.path()).expect("copied model evidence report is valid");
    let frozen_requirement = frozen_report
        .requirements
        .iter()
        .find(|requirement| requirement.requirement_id == REQUIREMENT_ID)
        .expect("corpus requirement exists in model evidence report");
    assert_ne!(frozen_requirement.status, Status::Executed);
    assert!(frozen_requirement
        .boundaries
        .iter()
        .all(|boundary| boundary.status != Status::Executed));
    assert!(report
        .bounded_production_evidence
        .iter()
        .all(|evidence| evidence.status == Status::PartiallyExecuted));
}

#[test]
fn overlay_rejects_model_unknown_and_duplicate_bindings() {
    let root = TemporaryReference::new("reject");
    let corpus = Corpus::load(root.path()).expect("copied authoritative corpus loads");
    let publication_digests = corpus.publication_digests.clone();
    let harness = Harness::new(corpus);

    let mut model = production_binding(
        publication_digests.clone(),
        "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
        "crates/orna-evaluator-v1/tests/evaluator.rs::range_model",
        EvidenceStatus::Passed,
    );
    model.source = ImplementationEvidenceSource::Model;
    let error = harness
        .implementation_evidence_overlay(&[model])
        .expect_err("model-only evidence cannot enter bounded overlay");
    assert!(error.contains("model evidence cannot enter implementation overlay"));

    let mut unknown = production_binding(
        publication_digests.clone(),
        "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
        "crates/orna-evaluator-v1/tests/evaluator.rs::range_unknown",
        EvidenceStatus::Passed,
    );
    unknown.requirement_id = "ORNA-NOT-IN-CORPUS".into();
    let error = harness
        .implementation_evidence_overlay(&[unknown])
        .expect_err("unknown requirement evidence cannot be bound");
    assert!(error.contains("unknown implementation evidence requirement"));

    let duplicate = production_binding(
        publication_digests,
        "crates/orna-evaluator-v1/src/lib.rs::Value::Range",
        "crates/orna-evaluator-v1/tests/evaluator.rs::range_duplicate",
        EvidenceStatus::Passed,
    );
    let error = harness
        .implementation_evidence_overlay(&[duplicate.clone(), duplicate])
        .expect_err("duplicate evidence bindings must be rejected");
    assert!(error.contains("duplicate implementation evidence binding"));
}
