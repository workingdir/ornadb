//! Bounded, fixture-backed evidence for ten high-impact traceability rows.
//!
//! Verified frozen-reference anchors:
//! - `source/14-pages.md:46,52`: ORNA-WIRE-001/004.
//! - `source/07-tables.md:342,368`: ORNA-PATH-006/012.
//! - `source/07-tables.md:391,393`: ORNA-ROW-002/003.
//! - `source/07-tables.md:444,448`: ORNA-OBJECT-003/005.
//! - `source/07-tables.md:492,496`: ORNA-PROJECT-001/003.
//!
//! Each result below is a passed typecheck-stage boundary for the named real
//! `.orna` fixture. The report must keep each requirement partially executed
//! because this stage does not prove the full requirement.

use orna_conformance_v1::{
    BoundedEvaluator, Corpus, FixtureStageBinding, Harness, RuntimeAdapter, Stage,
};
use orna_traceability_v1::{generate_with_engine_witnesses, Status};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const IMPLEMENTATION_REF: &str =
    "crates/orna-conformance-v1/src/lib.rs::Harness::run_fixture";
const TEST_REF: &str =
    "crates/orna-traceability-v1/tests/traceability_gap_evidence_bh1i.rs::ten_fixture_typecheck_boundaries_remain_per_requirement";
const PUBLICATION_DIGEST: &str =
    "d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d";

// These are exact frozen-reference examples, compiled into this new test and
// loaded by the conformance Harness after being copied byte-for-byte.
const FIXTURE_SOURCES: &[(&str, &str)] = &[
    (
        "examples/valid/presentation-watch.orna",
        include_str!("fixtures/reference/examples/valid/presentation-watch.orna"),
    ),
    (
        "examples/valid/table-composite-key.orna",
        include_str!("fixtures/reference/examples/valid/table-composite-key.orna"),
    ),
    (
        "examples/valid/table-reference.orna",
        include_str!("fixtures/reference/examples/valid/table-reference.orna"),
    ),
    (
        "examples/valid/row-body.orna",
        include_str!("fixtures/reference/examples/valid/row-body.orna"),
    ),
    (
        "examples/valid/explicit-rekey.orna",
        include_str!("fixtures/reference/examples/valid/explicit-rekey.orna"),
    ),
    (
        "examples/valid/table-explicit-key.orna",
        include_str!("fixtures/reference/examples/valid/table-explicit-key.orna"),
    ),
    (
        "examples/reference/main.orna",
        include_str!("fixtures/reference/examples/reference/main.orna"),
    ),
    (
        "examples/reference/library.orna",
        include_str!("fixtures/reference/examples/reference/library.orna"),
    ),
    (
        "examples/reference/sensors.orna",
        include_str!("fixtures/reference/examples/reference/sensors.orna"),
    ),
    (
        "examples/reference/values.orna",
        include_str!("fixtures/reference/examples/reference/values.orna"),
    ),
    (
        "examples/reference/warehouse.orna",
        include_str!("fixtures/reference/examples/reference/warehouse.orna"),
    ),
];

struct Row {
    requirement: &'static str,
    fixture_id: &'static str,
    fixture_path: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        requirement: "ORNA-WIRE-001",
        fixture_id: "valid/presentation-watch.orna",
        fixture_path: "examples/valid/presentation-watch.orna",
    },
    Row {
        requirement: "ORNA-WIRE-004",
        fixture_id: "valid/presentation-watch.orna",
        fixture_path: "examples/valid/presentation-watch.orna",
    },
    Row {
        requirement: "ORNA-PATH-006",
        fixture_id: "valid/table-composite-key.orna",
        fixture_path: "examples/valid/table-composite-key.orna",
    },
    Row {
        requirement: "ORNA-PATH-012",
        fixture_id: "valid/table-reference.orna",
        fixture_path: "examples/valid/table-reference.orna",
    },
    Row {
        requirement: "ORNA-ROW-002",
        fixture_id: "valid/row-body.orna",
        fixture_path: "examples/valid/row-body.orna",
    },
    Row {
        requirement: "ORNA-ROW-003",
        fixture_id: "valid/row-body.orna",
        fixture_path: "examples/valid/row-body.orna",
    },
    Row {
        requirement: "ORNA-OBJECT-003",
        fixture_id: "valid/explicit-rekey.orna",
        fixture_path: "examples/valid/explicit-rekey.orna",
    },
    Row {
        requirement: "ORNA-OBJECT-005",
        fixture_id: "valid/table-explicit-key.orna",
        fixture_path: "examples/valid/table-explicit-key.orna",
    },
    Row {
        requirement: "ORNA-PROJECT-001",
        fixture_id: "PROJECT-REFERENCE",
        fixture_path: "examples/reference",
    },
    Row {
        requirement: "ORNA-PROJECT-003",
        fixture_id: "PROJECT-REFERENCE",
        fixture_path: "examples/reference",
    },
];

struct ReferenceCopy(PathBuf);

impl ReferenceCopy {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "orna-traceability-gap-bh1i-{nonce}"
        ));
        copy_dir(&reference_root(), &path);
        for (logical_path, source) in FIXTURE_SOURCES {
            let fixture_path = path.join(logical_path);
            assert_eq!(
                fs::read_to_string(&fixture_path).expect("frozen fixture is present"),
                *source,
                "compiled fixture matches frozen source: {logical_path}"
            );
            fs::write(fixture_path, source).expect("write compiled fixture to reference copy");
        }
        declare_fixtures(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ReferenceCopy {
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
    fs::create_dir_all(to).expect("create copied reference directory");
    for entry in fs::read_dir(from).expect("read frozen reference directory") {
        let entry = entry.expect("read frozen reference entry");
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("read frozen reference entry type").is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy frozen reference file");
        }
    }
}

fn declare_fixtures(root: &Path) {
    let evidence_path = root.join("tests/requirement-evidence.json");
    let mut evidence: Value = serde_json::from_str(
        &fs::read_to_string(&evidence_path).expect("read copied requirement evidence"),
    )
    .expect("parse copied requirement evidence");
    let entries = evidence["requirements"]
        .as_array_mut()
        .expect("requirement evidence array");
    for row in ROWS {
        let entry = entries
            .iter_mut()
            .find(|entry| entry["requirement"] == row.requirement)
            .expect("selected requirement is declared in frozen evidence");
        let test = entry["tests"]
            .as_array_mut()
            .expect("selected requirement has its planned test row")
            .first_mut()
            .expect("selected requirement has a planned test row");
        test["fixture"] = Value::String(row.fixture_id.into());
        test["path"] = Value::String(row.fixture_path.into());
    }
    fs::write(
        evidence_path,
        serde_json::to_vec(&evidence).expect("serialize copied requirement evidence"),
    )
    .expect("write copied requirement evidence");
}

#[test]
fn ten_fixture_typecheck_boundaries_remain_per_requirement() {
    let copied = ReferenceCopy::new();
    let corpus = Corpus::load(copied.path()).expect("copied frozen corpus loads");
    assert_eq!(
        corpus
            .publication_digests
            .get("Orna-1.0.0.md")
            .map(String::as_str),
        Some(PUBLICATION_DIGEST),
        "fixture-backed evidence stays pinned to the frozen publication"
    );

    let harness = Harness::new(corpus);
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let run = harness.run(&mut adapter);
    let bindings = ROWS
        .iter()
        .map(|row| FixtureStageBinding {
            requirement_id: row.requirement.into(),
            fixture_id: row.fixture_id.into(),
            fixture_path: row.fixture_path.into(),
            stage: Stage::Typecheck,
            implementation_ref: IMPLEMENTATION_REF.into(),
            test_ref: TEST_REF.into(),
        })
        .collect::<Vec<_>>();
    let witnesses = harness
        .engine_witnesses(&run, &bindings)
        .expect("each selected fixture typecheck produced bounded execution evidence");
    let report = generate_with_engine_witnesses(copied.path(), &witnesses)
        .expect("the traceability consumer preserves ten distinct rows");

    for row in ROWS {
        let requirement = report
            .requirements
            .iter()
            .find(|requirement| requirement.requirement_id == row.requirement)
            .expect("selected requirement remains specified");
        assert_eq!(requirement.status, Status::PartiallyExecuted);
        assert!(requirement.boundaries.iter().any(|boundary| {
            boundary.status == Status::JustifiedGap
                && boundary.kind == "implementation-conformance obligation"
        }));
        assert!(requirement.boundaries.iter().any(|boundary| {
            boundary.kind == "engine-witness"
                && boundary.logical_id == format!("{}:typecheck", row.fixture_id)
                && boundary.status == Status::Executed
                && boundary.implementation_ref.as_deref() == Some(IMPLEMENTATION_REF)
                && boundary.test_ref.as_deref() == Some(TEST_REF)
        }));
        println!(
            "{} specified=present exists=present passed=typecheck-boundary requirement=partially-executed remainder=unexecuted",
            row.requirement
        );
    }
    println!(
        "ORNA-TEST-004 rows={} full_requirement_passes=0 frozen_register=unchanged",
        ROWS.len()
    );
}
