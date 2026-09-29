use std::process::Command;

const REFERENCE_SCENARIOS: &str = include_str!("../../../../reference/Orna-1.0.0/tests/scenarios.json");
const BATCH_31_60: &[&str] = &[
    "CP-003",
    "CP-004",
    "CURRENCY-FORMAT-091",
    "CURRENCY-PROTOCOL-091",
    "EVAL-001",
    "EVAL-002",
    "EVAL-003",
    "EXT-001",
    "FAIL-001",
    "FAIL-002",
    "FAIL-003",
    "FAIL-004",
    "FAIL-005",
    "FAIL-006",
    "FAILURE-PROPAGATE-091",
    "FAILURE-ROLLBACK-091",
    "FIELD-001",
    "FIELD-002",
    "FLOAT-STATS-001",
    "FLOAT-TOTAL-001",
    "GENERIC-BOUND-091",
    "GIT-001",
    "GIT-002",
    "HIST-001",
    "IMPL-NESTED-091",
    "IMPORT-001",
    "INFER-DIAGNOSTIC-091",
    "INFER-METADATA-091",
    "INFER-PRINCIPAL-091",
    "KEY-001",
];
const FOLLOW_UP_61_144: &[&str] = &[
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
fn executes_batch_two_and_pins_unexecuted_follow_up() {
    let reference: serde_json::Value = serde_json::from_str(REFERENCE_SCENARIOS).unwrap();
    let reference_scenarios = reference["scenarios"].as_array().unwrap();
    assert_eq!(reference_scenarios.len(), 144);

    let batch_ids = reference_scenarios[30..60]
        .iter()
        .map(|scenario| scenario["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(batch_ids, BATCH_31_60);

    let follow_up_ids = reference_scenarios[60..]
        .iter()
        .map(|scenario| scenario["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(follow_up_ids, FOLLOW_UP_61_144);

    let output = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .args(["--profile", "bounded-expression-runtime"])
        .env("ORNA_REFERENCE_DIR", "/home/pbox/dev/ornadb/reference/Orna-1.0.0")
        .output()
        .expect("bounded conformance runner starts");
    let cli_exit_code = output.status.code().expect("runner exits normally");
    println!("bounded CLI exit code: {cli_exit_code}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("runner stdout is not a conformance report JSON: {error}; stderr: {}", String::from_utf8_lossy(&output.stderr)));
    let reported_scenarios = report["scenarios"].as_array().expect("scenario report array");
    assert_eq!(reported_scenarios.len(), 144);

    let mut passed = 0;
    let mut passed_ids = Vec::new();
    let mut failed = 0;
    let mut absent = 0;
    for (offset, expected_id) in BATCH_31_60.iter().enumerate() {
        let index = offset + 31;
        let result = &reported_scenarios[index - 1];
        assert_eq!(result["scenario"], *expected_id, "reference index {index}");
        let status = result["status"].as_str().expect("scenario status");
        let detail = result["detail"].as_str().unwrap_or("");
        // ORNA-TEST-004 distinguishes an absent execution from a passed test.
        match status {
            "passed" => {
                passed += 1;
                passed_ids.push(*expected_id);
                println!("{index:02} {expected_id}: passed — {detail}");
                assert_eq!(detail, "scenario execution satisfied its adapter contract");
            }
            "failed" => {
                failed += 1;
                println!("{index:02} {expected_id}: failed — {detail}");
            }
            "skipped" => {
                absent += 1;
                println!("{index:02} {expected_id}: absent from execution — {detail}");
                assert!(detail.starts_with("scenario execution skipped:"));
            }
            other => panic!("{index:02} {expected_id}: unrecognized status {other:?}"),
        }
    }
    println!("batch 31-60 totals: passed={passed} failed={failed} absent={absent}");
    println!("FOLLOW-UP 61-144: {}", FOLLOW_UP_61_144.join(", "));
    assert_eq!(passed, 2, "only witnessed scenarios may be reported passed");
    assert_eq!(passed_ids, ["EVAL-003", "FAIL-001"]);
    assert_eq!(failed, 0, "scenario failures require a fixture-level repair");
    assert_eq!(absent, 28, "all non-witnessed scenarios remain absent");
    assert_eq!(cli_exit_code, 1, "the full bounded report remains incomplete");
}
