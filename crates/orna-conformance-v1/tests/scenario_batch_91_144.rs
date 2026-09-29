use std::process::Command;

const REFERENCE_SCENARIOS: &str =
    include_str!("fixtures/reference/tests/scenarios.json");
const BATCH_91_144: &[&str] = &[
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

#[test]
fn executes_final_scenario_batch_and_dispositions_all_144_entries() {
    let reference: serde_json::Value = serde_json::from_str(REFERENCE_SCENARIOS).unwrap();
    let scenarios = reference["scenarios"].as_array().unwrap();
    assert_eq!(reference["count"], 144);
    assert_eq!(scenarios.len(), 144);

    let batch_ids = scenarios[90..144]
        .iter()
        .map(|scenario| scenario["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(batch_ids, BATCH_91_144);
    assert!(scenarios[90..144].iter().all(|scenario| {
        scenario["evidence_level"] == "implementation scenario, not executed by an Orna engine"
    }));

    let output = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .args(["--profile", "bounded-expression-runtime"])
        .env(
            "ORNA_REFERENCE_DIR",
            "/home/pbox/dev/ornadb/reference/Orna-1.0.0",
        )
        .output()
        .expect("bounded conformance runner starts");
    let cli_exit_code = output.status.code().expect("runner exits normally");
    println!("bounded CLI exit code: {cli_exit_code}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "runner stdout is not a conformance report JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let reported = report["scenarios"].as_array().expect("scenario report array");
    assert_eq!(reported.len(), 144);

    let (mut corpus_passed, mut corpus_failed, mut corpus_absent) = (0, 0, 0);
    for (index, result) in reported.iter().enumerate() {
        assert_eq!(result["scenario"], scenarios[index]["id"]);
        match result["status"].as_str().expect("scenario status") {
            "passed" => corpus_passed += 1,
            "failed" => corpus_failed += 1,
            "skipped" => corpus_absent += 1,
            status => panic!("scenario {} has unknown status {status:?}", index + 1),
        }
    }

    let (mut passed, mut failed, mut absent) = (0, 0, 0);
    for (offset, expected_id) in BATCH_91_144.iter().enumerate() {
        let index = offset + 91;
        let result = &reported[index - 1];
        assert_eq!(result["scenario"], *expected_id, "reference index {index}");
        let status = result["status"].as_str().expect("scenario status");
        let detail = result["detail"].as_str().unwrap_or("");
        match status {
            "passed" => {
                passed += 1;
                println!(
                    "{index:03} {expected_id}: specified=yes test_exists=yes passed=yes executable_witness=bounded-adapter-contract orna_engine_execution=no — {detail}"
                );
                assert_eq!(detail, "scenario execution satisfied its adapter contract");
            }
            "failed" => {
                failed += 1;
                println!(
                    "{index:03} {expected_id}: specified=yes test_exists=yes passed=no executable_witness=adapter-failed orna_engine_execution=no — {detail}"
                );
            }
            "skipped" => {
                absent += 1;
                println!(
                    "{index:03} {expected_id}: specified=yes test_exists=no passed=no executable_witness=none orna_engine_execution=no — {detail}"
                );
                assert!(detail.starts_with("scenario execution skipped:"));
            }
            other => panic!("{index:03} {expected_id}: unrecognized status {other:?}"),
        }
    }

    println!("batch 91-144 totals: specified=54 test_exists={} passed={passed} failed={failed} absent={absent} orna_engine_executions=0", passed + failed);
    println!("final corpus totals: dispositioned=144/144 passed={corpus_passed} failed={corpus_failed} absent={corpus_absent}; bounded adapter results are not Orna-engine execution");
    assert_eq!(passed, 4, "only adapter-witnessed scenarios may be reported passed");
    assert_eq!(failed, 0, "scenario failures require a fixture-level repair");
    assert_eq!(absent, 50, "all non-witnessed scenarios remain absent");
    assert_eq!((corpus_passed, corpus_failed, corpus_absent), (12, 0, 132));
    assert_eq!(cli_exit_code, 1, "the full bounded report remains incomplete");
}
