//! The per-code parity of every diagnostic (`docs/design/hir.md` section 6.3, H3).
//!
//! For every module case the old checker analyses, the set of diagnostic codes
//! the checker reports is compared with the set HIR reports on the case's
//! source modules after lowering and typing. Each code is counted per case as
//! agreeing, checker-only or HIR-only.
//!
//! The cases whose code sets differ are ratcheted exactly: the measured set must
//! equal the `## expected` section of `diagnostic_codes_differing.txt`. A new
//! difference fails the test, and a listed case that now agrees fails it as
//! stale. Every listed row carries a reason field, and every differing code
//! carries a recorded reason. The measurement and the reasons live in
//! `orna_differential::diagnostics_parity`, shared with the shadow report.

use std::collections::BTreeSet;

use orna_differential::case::Corpus;
use orna_differential::diagnostics_parity::{
    DIFFERING, difference_line, expected_differences, measure, recorded_reason, row_reason,
};

#[test]
fn every_diagnostic_code_matches_the_checker_per_case() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let measured = measure(&corpus);
    let differing: BTreeSet<String> = measured
        .differing
        .iter()
        .map(|(case, (old, new))| difference_line(case, old, new))
        .collect();
    let expected = expected_differences(DIFFERING);
    println!(
        "diagnostic codes over {} cases ({} skipped); columns: agree, checker-only, hir-only; \
         {} cases differ (expected {})",
        measured.compared,
        measured.skipped,
        differing.len(),
        expected.len()
    );
    for (code, row) in &measured.table {
        let reason = if row[1] + row[2] == 0 {
            "agrees on every case"
        } else {
            recorded_reason(code).unwrap_or("UNRECORDED")
        };
        println!(
            "{code}: agree {} checker-only {} hir-only {} :: {reason}",
            row[0], row[1], row[2]
        );
    }
    let unrecorded: Vec<&String> = measured
        .table
        .iter()
        .filter(|(code, row)| (row[1] + row[2] > 0) && recorded_reason(code).is_none())
        .map(|(code, _)| code)
        .collect();
    assert!(
        unrecorded.is_empty(),
        "differing codes without a recorded reason: {unrecorded:?}"
    );
    let unexpected: Vec<&String> = differing.difference(&expected).collect();
    let stale: Vec<&String> = expected.difference(&differing).collect();
    assert!(
        unexpected.is_empty(),
        "code differences not listed in diagnostic_codes_differing.txt:\n{}",
        unexpected
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        stale.is_empty(),
        "listed code differences that now agree (remove them):\n{}",
        stale
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let without_reason: Vec<&String> = expected
        .iter()
        .filter(|line| row_reason(line).is_none())
        .collect();
    assert!(
        without_reason.is_empty(),
        "listed code differences without a reason field (`<case> :: <checker> => <HIR> :: <reason>`):\n{}",
        without_reason
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}
