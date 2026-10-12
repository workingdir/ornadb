//! Batch-three evidence execution for reference scenario ordinals 61–90.
//!
//! This exercises the checked-in bounded runtime scenario adapter. It does not
//! claim full Orna-engine execution: the frozen corpus labels these as
//! implementation scenarios, and ORNA-TEST-004 requires specified,
//! implemented, and passed evidence to remain distinct.

use orna_conformance_v1::Corpus;
use serde_json::Value;
use std::process::Command;

const BATCH_THREE: [&str; 30] = [
    "KEY-002",
    "KEY-003",
    "KEY-004",
    "KEY-005",
    "LET-REBIND-091",
    "LEX-QUESTION-001",
    "LIVE-001",
    "LIVE-002",
    "LIVE-003",
    "LIVE-004",
    "LIVE-FALLBACK-001",
    "MERGE-001",
    "MERGE-002",
    "MERGE-003",
    "MERGE-004",
    "MERGE-PRUNE-001",
    "MIGRATE-SURFACE-091",
    "MODULE-001",
    "MODULE-002",
    "MONEY-001",
    "MONEY-002",
    "NOMINAL-PRIVATE-091",
    "NS-PORTABLE-001",
    "NUM-001",
    "OBJECT-001",
    "ORDER-001",
    "PATH-CASE-001",
    "PATH-EMPTY-001",
    "PIPE-001",
    "PIPE-002",
];

// Ordinals 91–144 are deliberately pinned as the follow-up batch. They are
// reported as specified only; this test does not execute them.
const FOLLOW_UP_91_144: [&str; 54] = [
    "PRES-001",
    "PRES-002",
    "PROFILE-001",
    "PROJECT-001",
    "PROJECT-LIFE-001",
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
    "ROW-002",
    "SEC-001",
    "SECRET-001",
    "SERVER-001",
    "SERVER-002",
    "STATE-001",
    "STATE-002",
    "STORAGE-DISJOINT-001",
    "STREAM-001",
    "STREAM-002",
    "SYNTAX-ARROW-001",
    "SYNTAX-RECORD-001",
    "SYNTAX-RETURN-091",
    "SYS-001",
    "SYS-002",
    "SYS-AWAIT-TIMEOUT-100",
    "SYS-CHECKPOINT-CAS-100",
    "SYS-FAILURE-ADMIN-100",
    "SYS-GROUPED-RELATION-100",
    "SYS-INVOKE-RESULT-TYPE-100",
    "SYS-INVOKE-TYPED-100",
    "SYS-REDACTION-100",
    "SYS-ROW-REF-100",
    "SYS-RT-RENAME-100",
    "SYS-SNAPSHOT-OVERLOADS-100",
    "SYS-SNAPSHOT-PIN-100",
    "SYS-SOURCE-HISTORY-100",
    "SYS-STORAGE-ADMIN-100",
    "SYS-SUPPORT-TYPES-100",
    "TIME-001",
    "TXN-001",
    "TXN-002",
    "TXN-003",
    "UNIT-001",
    "UNIT-002",
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
    assert_eq!(scenarios.len(), 144, "frozen corpus scenario count");
    assert_eq!(ids(&scenarios[60..90]), BATCH_THREE.map(str::to_owned));
    assert_eq!(
        ids(&scenarios[90..144]),
        FOLLOW_UP_91_144.map(str::to_owned),
        "keep the next 54 scenarios as the explicit follow-up batch"
    );
    assert!(scenarios[60..90].iter().all(|scenario| {
        scenario["evidence_level"]
            == "implementation scenario, not executed by an Orna engine"
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
    assert_eq!(outcomes.len(), 144);
    let outcomes = &outcomes[60..90];

    println!("bounded-expression-runtime CLI exit code: {child_exit}");
    for (ordinal, (expected_id, result)) in BATCH_THREE.iter().zip(outcomes).enumerate() {
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
        "follow-up scenarios 91-144 (specified, not executed by this test): {}",
        FOLLOW_UP_91_144.join(", ")
    );
}
