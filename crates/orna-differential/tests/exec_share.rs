//! The H4 executor agreement ratchet (`docs/design/hir.md` section 6.3, stage H4).
//!
//! Every corpus case the executor runs must produce the old pipeline's outcome
//! byte-for-byte: canonical result bytes or the same error code. A case the
//! executor does not run is counted by the reason it names, not skipped. The
//! agreed count is recorded in `tests/differential/exec-share.toml` and may only
//! grow. The allowances that explain a disagreement live in
//! `orna_differential::exec_allowance`, shared with the shadow report.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::compare;
use orna_differential::exec_allowance::ExecAllowances;
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
    let mut allowances = ExecAllowances::load().expect("the executor allowances parse");
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
                if observation.outcome != outcome {
                    let allowed = outcome.as_ref().and_then(|executor| {
                        allowances.outcome_allowed(case, executor, observation.outcome.as_ref())
                    });
                    if allowed.is_none() {
                        mismatches.push(format!(
                            "{}: executor {outcome:?}, old {:?}",
                            case.id, observation.outcome
                        ));
                    }
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
    for (id, old, executor) in step_differences {
        if !allowances.step_allowed(&id, old, executor) {
            mismatches.push(format!(
                "{id}: executor charges {executor} steps, old pipeline {old}"
            ));
        }
    }
    mismatches.extend(allowances.stale());
    let old_harness_environment = allowances.harness_environment_count();
    println!(
        "old-harness environment: {old_harness_environment} run cases differ only by the H0 observer's admitted-table environment"
    );
    println!(
        "executor agreement: {agreed}/{ran} run cases agree with the old pipeline ({} cases in the corpus)",
        corpus.cases.len()
    );
    println!(
        "exact step parity: {steps_exact}/{agreed} agreeing run cases charge the old pipeline's steps"
    );
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
        old_harness_environment,
    };
    let failures = compare::exec_share_ratchet(recorded, observed);
    assert!(
        failures.is_empty(),
        "the exec-share ratchet failed:\n{}",
        failures.join("\n")
    );
}
