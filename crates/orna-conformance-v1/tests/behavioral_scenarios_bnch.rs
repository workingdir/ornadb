//! Batch-three evidence execution for reference scenario ordinals 61–90.
//!
//! The batch and the expected total are derived from the vendored 1.1.0
//! corpus (its declared `count` and scenario order), not from a pinned 1.0.0
//! list. This exercises the checked-in bounded runtime scenario adapter. It
//! does not claim full Orna-engine execution: the corpus labels these as
//! implementation scenarios with `implementation_status = not-executed`, and
//! ORNA-TEST-004 requires specified, implemented, and passed evidence to remain
//! distinct.

use orna_conformance_v1::Corpus;
use serde_json::Value;
use std::process::Command;

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
    assert_eq!(scenarios.len(), declared, "frozen corpus scenario count");
    let batch = &scenarios[60..90];
    let batch_ids = ids(batch);
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
