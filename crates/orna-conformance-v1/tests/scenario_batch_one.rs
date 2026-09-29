use std::process::Command;

use orna_conformance_v1::Corpus;
use serde_json::Value;

const FIRST_SCENARIO_COUNT: usize = 30;

#[test]
fn first_scenario_batch_reports_only_the_frozen_prefix_and_never_promotes_skips() {
    let corpus = Corpus::load_default().expect("frozen Orna corpus loads");
    let expected = corpus.scenarios["scenarios"]
        .as_array()
        .expect("scenario list")
        .iter()
        .take(FIRST_SCENARIO_COUNT)
        .map(|scenario| scenario["id"].as_str().expect("scenario id"))
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), FIRST_SCENARIO_COUNT);

    let output = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .args([
            "--profile",
            "bounded-expression-runtime",
            "--first-scenarios",
            "30",
        ])
        .output()
        .expect("bounded first-30 scenario runner starts");
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| {
            panic!(
                "scenario report JSON: {error}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let results = report["scenarios"].as_array().expect("scenario results");
    assert_eq!(results.len(), FIRST_SCENARIO_COUNT);
    assert_eq!(
        report["implementation_claim"]["command"],
        "orna-conformance --profile bounded-expression-runtime --first-scenarios 30"
    );

    let mut failed = Vec::new();
    for (index, (result, expected_id)) in results.iter().zip(expected).enumerate() {
        assert_eq!(result["scenario"].as_str(), Some(expected_id));
        let status = result["status"].as_str().expect("scenario status");
        let disposition = match status {
            "passed" => "passed",
            "failed" | "cancelled" => {
                failed.push(expected_id);
                "failed"
            }
            "skipped" | "specified" => "absent",
            other => panic!("unexpected scenario evidence status {other}"),
        };
        let witness = if disposition == "absent" { "no" } else { "yes" };
        println!(
            "{:02} {} specified=yes executable_witness={witness} outcome={disposition}",
            index + 1,
            expected_id
        );
    }

    assert!(failed.is_empty(), "scenario mismatches: {failed:?}");
    assert!(
        output.status.success() || output.status.code() == Some(1),
        "unexpected runner exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
