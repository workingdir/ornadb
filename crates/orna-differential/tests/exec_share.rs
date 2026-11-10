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
    let mut steps_exact = 0usize;
    let mut mismatches = Vec::new();
    let mut step_differences: Vec<(String, u64, u64)> = Vec::new();
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for case in &corpus.cases {
        match observe_exec::run_case(case) {
            ExecOutcome::NotYet(reason) => {
                if reason.ends_with("(H4)") && std::env::var_os("EXEC_SHARE_IDS").is_some() {
                    let old = observe_old::observe(case).map(|observation| observation.outcome);
                    println!("OLD {} => {old:?}", case.id);
                }
                reasons.entry(reason).or_default().push(case.id.clone());
            }
            ExecOutcome::Ran { outcome, steps } => {
                ran += 1;
                let observation = match observe_old::observe(case) {
                    Ok(observation) => observation,
                    Err(error) => {
                        mismatches.push(format!("{}: the old pipeline failed: {error}", case.id));
                        continue;
                    }
                };
                if observation.outcome.as_ref() != Some(&outcome) {
                    mismatches.push(format!("{}: executor {outcome:?}, old {:?}", case.id, observation.outcome));
                    continue;
                }
                agreed += 1;
                match observation.steps {
                    Some(old) if old == steps => steps_exact += 1,
                    Some(old) => step_differences.push((case.id.clone(), old, steps)),
                    None => {}
                }
            }
        }
    }
    let allowances = compare::load_step_allowances().expect("the step allowances parse");
    let mut used = vec![false; allowances.len()];
    for (id, old, executor) in step_differences {
        match allowances.iter().position(|allowance| allowance.matches(&id, old, executor)) {
            Some(index) => used[index] = true,
            None => mismatches.push(format!("{id}: executor charges {executor} steps, old pipeline {old}")),
        }
    }
    for (allowance, used) in allowances.iter().zip(&used) {
        if !used {
            mismatches.push(format!(
                "{}: step allowance for old {} and executor {} is stale; remove or update it",
                allowance.case, allowance.old, allowance.executor
            ));
        }
    }
    println!(
        "executor agreement: {agreed}/{ran} run cases agree with the old pipeline ({} cases in the corpus)",
        corpus.cases.len()
    );
    println!("exact step parity: {steps_exact}/{agreed} agreeing run cases charge the old pipeline's steps");
    for (reason, ids) in &reasons {
        println!("  {:>5} {reason}", ids.len());
        if std::env::var_os("EXEC_SHARE_IDS").is_some() {
            for id in ids {
                println!("          {id}");
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "the executor disagrees with the old pipeline:\n{}",
        mismatches.join("\n")
    );
    let observed = compare::ExecShare {
        agreed,
        evaluable,
        steps_exact,
    };
    let failures = compare::exec_share_ratchet(recorded, observed);
    assert!(
        failures.is_empty(),
        "the exec-share ratchet failed:\n{}",
        failures.join("\n")
    );
}
