//! `orna history --count` without a relation or row key is a usage error. It is
//! rejected while parsing arguments, before any repository is discovered, so it
//! must exit 1 with the usage message and print no count, even from a plain
//! directory that holds only an `.orna` source.

use std::path::Path;

use tempfile::TempDir;

#[path = "support/orna_cli_run.rs"]
mod orna_cli_run;
use orna_cli_run::run_in;

const FIXTURE: &str = "catalogue-count-no-args-ogx1.orna";
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[test]
fn history_count_without_relation_or_key_is_a_usage_error() {
    let directory = TempDir::new().unwrap();
    std::fs::copy(Path::new(FIXTURES).join(FIXTURE), directory.path().join(FIXTURE)).unwrap();

    let output = run_in(directory.path(), &["history", "--count"]);

    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "no count may be printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("History expects a relation and a row key"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("Repository could not be opened"), "stderr: {stderr}");
}
