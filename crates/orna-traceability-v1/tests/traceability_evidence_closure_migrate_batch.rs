use orna_traceability_v1::{Status, generate};
use std::path::PathBuf;

// This real source fixture is parsed by the companion conformance test. Its
// presence here does not turn the frozen report into execution evidence.
const SOURCE_FIXTURE: &str =
    include_str!("fixtures/traceability-evidence-closure-migrate.orna");

const REQUIREMENTS: [&str; 10] = [
    "ORNA-TEST-004",
    "ORNA-TEST-010",
    "ORNA-TEST-011",
    "ORNA-EVIDENCE-001",
    "ORNA-EVIDENCE-002",
    "ORNA-CLOSURE-002",
    "ORNA-CLOSURE-003",
    "ORNA-CLOSURE-004",
    "ORNA-MIGRATE-003",
    "ORNA-MIGRATE-004",
];

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference")
        })
}

#[test]
fn frozen_traceability_report_keeps_all_ten_planned_rows_unexecuted() {
    assert!(SOURCE_FIXTURE.contains("=>"));
    let report = generate(reference_root()).expect("frozen reference report validates");

    for id in REQUIREMENTS {
        let requirement = report
            .requirements
            .iter()
            .find(|requirement| requirement.requirement_id == id)
            .unwrap_or_else(|| panic!("frozen report contains specified row {id}"));
        assert_eq!(requirement.status, Status::JustifiedGap, "{id} remains unexecuted");
        assert!(!requirement.boundaries.is_empty(), "{id} preserves per-row boundaries");
        assert!(
            requirement
                .boundaries
                .iter()
                .all(|boundary| boundary.status != Status::Executed),
            "the report must not infer execution for {id} from its plan or fixture"
        );
    }
}
