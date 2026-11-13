//! Differential check of the repl-inline fixtures against the old evaluator's REPL.
//!
//! The fixtures carry no expected-stdout files; their expectations are asserts in `orna-evaluator` and
//! `orna-project-repl`. Each fixture that needs only a standalone session is run through the new session and
//! through a fresh old `ReplSession`, and the canonical result or the error code must match. Fixtures that need
//! a repository, a snapshot, a watch or persistence are listed in `repl_inline_exclusions.txt` with a reason.
//! Runnable fixtures that disagree are listed with a reason in `repl_inline_differing.txt`; the disagreement set must
//! match that list exactly. `AGREED_FLOOR` is a ratchet: the agreed count must not drop, and it is raised when it rises.
//! The fixtures, the ledgers and both outcomes live in `tests/common/repl_inline.rs`, shared with the shadow report
//! (`tests/shadow_report.rs`).

#[allow(dead_code)]
mod common;

use common::repl_inline::{
    collect_fixtures, new_outcome, old_outcome, parse_exclusions, parse_expected,
};

const AGREED_FLOOR: usize = 15;

#[test]
fn repl_inline_fixtures_agree_with_the_old_repl() {
    let fixtures = collect_fixtures();
    let exclusions = parse_exclusions();
    for path in exclusions.keys() {
        assert!(fixtures.contains_key(path), "stale exclusion: {path}");
    }

    let expected = parse_expected();
    let mut agreed = 0;
    let mut disagreed = Vec::new();
    for (path, source) in &fixtures {
        if exclusions.contains_key(path) {
            continue;
        }
        let (new, old) = (new_outcome(source), old_outcome(source));
        if new == old {
            agreed += 1;
        } else {
            eprintln!("disagree: {path}\n  new={new:?}\n  old={old:?}");
            disagreed.push(path.clone());
        }
    }
    for path in expected.keys() {
        assert!(fixtures.contains_key(path), "stale expectation: {path}");
        assert!(
            disagreed.contains(path),
            "stale expectation, now agrees with the old REPL: {path}"
        );
    }
    for path in &disagreed {
        assert!(
            expected.contains_key(path),
            "unclassified disagreement, add it to repl_inline_differing.txt with a reason: {path}"
        );
    }
    let runnable = fixtures.len() - exclusions.len();
    eprintln!(
        "repl-inline differential: agreed {agreed} of {runnable} runnable, {} total, {} expected disagreements",
        fixtures.len(),
        expected.len()
    );
    assert!(
        agreed >= AGREED_FLOOR,
        "agreed {agreed} dropped below the ratchet floor {AGREED_FLOOR}"
    );
}
