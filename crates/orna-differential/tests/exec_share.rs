//! The H4 executor agreement ratchet (`docs/design/hir.md` section 6.3, stage H4).
//!
//! Every corpus case the executor runs must produce the old pipeline's outcome
//! byte-for-byte: canonical result bytes or the same error code. A case the
//! executor does not run is counted by the reason it names, not skipped. The
//! agreed count is recorded in `tests/differential/exec-share.toml` and may only
//! grow.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::compare;
use orna_differential::observe_exec::{self, ExecOutcome};
use orna_differential::observe_old;

#[test]
fn executor_agreement_with_the_old_pipeline_only_grows() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let recorded = compare::load_exec_share().expect("the exec-share ratchet parses");
    let evaluable = corpus.cases.iter().filter(|case| case.evaluate).count();
    let mut ran = 0usize;
    let mut agreed = 0usize;
    let mut mismatches = Vec::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for case in &corpus.cases {
        match observe_exec::run_case(case) {
            ExecOutcome::NotYet(reason) => *reasons.entry(reason).or_insert(0) += 1,
            ExecOutcome::Ran(outcome) => {
                ran += 1;
                let old = match observe_old::observe(case) {
                    Ok(observation) => observation.outcome,
                    Err(error) => {
                        mismatches.push(format!("{}: the old pipeline failed: {error}", case.id));
                        continue;
                    }
                };
                if old.as_ref() == Some(&outcome) {
                    agreed += 1;
                } else {
                    mismatches.push(format!("{}: executor {outcome:?}, old {old:?}", case.id));
                }
            }
        }
    }
    println!(
        "executor agreement: {agreed}/{ran} run cases agree with the old pipeline ({} cases in the corpus)",
        corpus.cases.len()
    );
    for (reason, count) in &reasons {
        println!("  {count:>5} {reason}");
    }
    assert!(
        mismatches.is_empty(),
        "the executor disagrees with the old pipeline:\n{}",
        mismatches.join("\n")
    );
    let observed = compare::ExecShare { agreed, evaluable };
    let failures = compare::exec_share_ratchet(recorded, observed);
    assert!(
        failures.is_empty(),
        "the exec-share ratchet failed:\n{}",
        failures.join("\n")
    );
}
