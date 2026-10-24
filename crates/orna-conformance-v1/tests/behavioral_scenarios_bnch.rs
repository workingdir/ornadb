//! Batch-three evidence execution for reference scenario ordinals 61–90.
//!
//! The batch IDs and the 1.1.0 scenario total (279) are pinned as literals, as
//! in the 91-144 batch test, so a change to the vendored corpus fails here
//! instead of silently moving the batch. This exercises the checked-in bounded
//! runtime scenario adapter. It does not claim full Orna-engine execution: the
//! corpus labels these as implementation scenarios with
//! `implementation_status = not-executed`, and ORNA-TEST-004 requires
//! specified, implemented, and passed evidence to remain distinct.

use orna_conformance_v1::Corpus;
use serde_json::Value;
use std::process::Command;

const CORPUS_SCENARIO_COUNT: usize = 279;
const BATCH_61_90: &[&str] = &[
    "LIVE-004",
    "LIVE-FALLBACK-001",
    "MIGRATE-SURFACE-091",
    "MODULE-001",
    "MODULE-002",
    "MONEY-001",
    "MONEY-002",
    "NOMINAL-PRIVATE-091",
    "NS-PORTABLE-001",
    "NUM-001",
    "OBJECT-001",
    "PIPE-001",
    "PIPE-002",
    "PRES-001",
    "PRES-002",
    "PROJECT-001",
    "PUB-001",
    "PUB-002",
    "PUB-003",
    "PUB-004",
    "PUB-005",
    "PUB-006",
    "RECOVERY-FAILURE-091",
    "RECOVERY-REFAIL-091",
    "RECOVERY-SUCCESS-091",
    "REF-001",
    "REPL-001",
    "REPL-002",
    "REPL-003",
    "ROW-001",
];

fn ids(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            value["id"]
                .as_str()
                .expect("scenario ID is text")
                .to_owned()
        })
        .collect()
}

#[test]
fn scenarios_61_through_90_report_specification_and_bounded_execution_separately() {
    let corpus = Corpus::load_default().expect("frozen reference corpus loads");
    let scenarios = corpus.scenarios["scenarios"]
        .as_array()
        .expect("validated scenario list");
    let declared = corpus.scenarios["count"]
        .as_u64()
        .expect("declared scenario count") as usize;
    assert_eq!(declared, CORPUS_SCENARIO_COUNT, "1.1.0 corpus declares 279 scenarios");
    assert_eq!(scenarios.len(), declared, "frozen corpus scenario count");
    let batch = &scenarios[60..90];
    let batch_ids = ids(batch);
    assert_eq!(batch_ids, BATCH_61_90, "batch 61-90 IDs are pinned to the 1.1.0 corpus");
    assert!(batch.iter().all(|scenario| {
        scenario["implementation_status"] == "not-executed"
    }));

    // Exercise the shipped composite profile rather than substituting a test-
    // only adapter. Exit 1 is valid evidence when the full report has skipped
    // scenarios outside this batch; the report itself is decoded below.
    let output = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .args(["--profile", "bounded-expression-runtime"])
        .env("ORNA_REFERENCE_DIR", &corpus.root)
        .output()
        .expect("bounded-expression-runtime profile launches");
    let child_exit = output.status.code().expect("CLI exits with a code");
    assert!(matches!(child_exit, 0 | 1), "unexpected CLI exit {child_exit}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "bounded profile emitted invalid JSON ({error}); stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let outcomes = report["scenarios"]
        .as_array()
        .expect("CLI report has scenario outcomes");
    assert_eq!(outcomes.len(), declared);
    let outcomes = &outcomes[60..90];

    println!("bounded-expression-runtime CLI exit code: {child_exit}");
    for (ordinal, (expected_id, result)) in batch_ids.iter().zip(outcomes).enumerate() {
        assert_eq!(result["scenario"], *expected_id);
        let status = result["status"].as_str().expect("status is text");
        let class = result["class"].as_str().expect("class is text");
        let contract = match status {
            "skipped" | "specified" => "absent",
            "passed" | "failed" | "cancelled" => "present",
            other => panic!("unrecognized ORNA-TEST-004 status {other}"),
        };
        println!(
            "scenario {:02}: {} | specified=yes | bounded execution contract={} | outcome={} | class={} | detail={} | diagnostic={}",
            ordinal + 61,
            expected_id,
            contract,
            status,
            class,
            result["detail"].as_str().unwrap_or(""),
            result["diagnostic"],
        );
    }
    println!(
        "follow-up scenarios 91-{declared} (specified, not executed by this test): {}",
        ids(&scenarios[90..]).join(", ")
    );
}
