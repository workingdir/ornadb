//! The shadow report (`docs/design/hir-cutover.md`, section 4.2): the whole corpus through the old and
//! HIR pipelines, written as markdown. The report is printed on stdout and, when `ORNA_SHADOW_REPORT` names a
//! path, written to that file.
//!
//! ```sh
//! ORNA_SHADOW_REPORT=shadow-report.md cargo test -p orna-differential --test shadow_report --locked -- --ignored --nocapture
//! ```
//!
//! The test fails when a difference is unexplained or a ledger entry is stale.

#[allow(dead_code)]
mod common;

use std::fs;

use common::shadow;
use orna_differential::case::Corpus;

#[test]
#[ignore = "runs the whole corpus through both pipelines; run with --ignored"]
fn shadow_report() {
    let corpus = Corpus::load().unwrap_or_else(|error| panic!("the corpus does not load: {error}"));
    let report =
        shadow::run(&corpus).unwrap_or_else(|error| panic!("the shadow run failed: {error}"));
    let markdown = report.render();
    print!("{markdown}");
    if let Ok(path) = std::env::var("ORNA_SHADOW_REPORT") {
        fs::write(&path, &markdown).unwrap_or_else(|error| panic!("cannot write {path}: {error}"));
    }
    assert!(
        report.is_clean(),
        "shadow report: {} unexplained difference(s), {} stale ledger entr(y/ies)",
        report.unexplained_count(),
        report.stale_count(),
    );
}
