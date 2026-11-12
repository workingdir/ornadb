//! The H4 executor agreement ratchet (`docs/design/hir.md` section 6.3, stage H4).
//!
//! Every corpus case the executor runs must produce the old pipeline's outcome
//! byte-for-byte: canonical result bytes or the same error code. A case the
//! executor does not run is counted by the reason it names, not skipped. The
//! agreed count is recorded in `tests/differential/exec-share.toml` and may only
//! grow.

use std::collections::BTreeMap;

use orna_differential::case::{Case, Corpus, Family};
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
    let harness_allowances =
        compare::load_harness_allowances().expect("the harness allowances parse");
    let mut harness_used = vec![false; harness_allowances.len()];
    let representation_allowances =
        compare::load_representation_allowances().expect("the representation allowances parse");
    let mut representation_used = vec![false; representation_allowances.len()];
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
                    if harness_environment_allowed(
                        case,
                        &outcome,
                        observation.outcome.as_ref(),
                        &harness_allowances,
                        &mut harness_used,
                    ) {
                        continue;
                    }
                    if representation_allowed(
                        case,
                        observation.outcome.as_ref(),
                        &outcome,
                        &representation_allowances,
                        &mut representation_used,
                    ) {
                        continue;
                    }
                    mismatches.push(format!(
                        "{}: executor {outcome:?}, old {:?}",
                        case.id, observation.outcome
                    ));
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
    mismatches.extend(harness_stale(&harness_allowances, &harness_used));
    mismatches.extend(representation_stale(
        &representation_allowances,
        &representation_used,
    ));
    let old_harness_environment = harness_used.iter().filter(|used| **used).count();
    println!(
        "old-harness environment: {old_harness_environment} run cases differ only by the H0 observer's missing table store"
    );
    let allowances = compare::load_step_allowances().expect("the step allowances parse");
    let mut used = vec![false; allowances.len()];
    for (id, old, executor) in step_differences {
        match allowances
            .iter()
            .position(|allowance| allowance.matches(&id, old, executor))
        {
            Some(index) => used[index] = true,
            None => mismatches.push(format!(
                "{id}: executor charges {executor} steps, old pipeline {old}"
            )),
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

/// Whether `case`'s disagreement is the H0 observer's missing table store. Either the old outcome
/// is `ORNA-EVAL-NAME` for a case that reaches a table (declared in its source, or an attached
/// fixture table: the reference family imports `fixture.attached.*` at load time), or the executor
/// reports `ORNA-EVAL-TABLE-ROW` for a case the old pipeline rejects. Both need a harness allowance.
fn harness_environment_allowed(
    case: &Case,
    executor: &Result<Vec<u8>, String>,
    old: Option<&Result<Vec<u8>, String>>,
    allowances: &[compare::HarnessAllowance],
    used: &mut [bool],
) -> bool {
    let reaches_table = declares_table(case) || case.family == Family::Reference;
    let missing_name = matches!(old, Some(Err(code)) if code == "ORNA-EVAL-NAME") && reaches_table;
    let missing_store = matches!(executor, Err(code) if code == "ORNA-EVAL-TABLE-ROW")
        && matches!(old, Some(Err(_)));
    if !missing_name && !missing_store {
        return false;
    }
    match allowances
        .iter()
        .position(|allowance| allowance.case == case.id)
    {
        Some(index) => {
            used[index] = true;
            true
        }
        None => false,
    }
}

/// Whether `case`'s disagreement is the old pipeline's raw-blob representation gap: the old
/// pipeline answers `ORNA-EVAL-VALUE` for a result the executor returns as a raw `Blob`, and the
/// case is listed in `[[representation-allowance]]`. Any other disagreement is still a mismatch.
fn representation_allowed(
    case: &Case,
    old: Option<&Result<Vec<u8>, String>>,
    executor: &Result<Vec<u8>, String>,
    allowances: &[compare::HarnessAllowance],
    used: &mut [bool],
) -> bool {
    let value_gap = matches!(old, Some(Err(code)) if code == "ORNA-EVAL-VALUE");
    if !value_gap || !executor.is_ok() {
        return false;
    }
    match allowances
        .iter()
        .position(|allowance| allowance.case == case.id)
    {
        Some(index) => {
            used[index] = true;
            true
        }
        None => false,
    }
}

/// The representation allowances that no longer cover a run: each must be removed.
fn representation_stale(allowances: &[compare::HarnessAllowance], used: &[bool]) -> Vec<String> {
    allowances
        .iter()
        .zip(used)
        .filter(|(_, used)| !**used)
        .map(|(allowance, _)| {
            format!(
                "{}: representation allowance is stale; the case no longer differs, remove it",
                allowance.case
            )
        })
        .collect()
}

/// Whether a source of `case` declares a table, the shape the H0 observer has no store for.
fn declares_table(case: &Case) -> bool {
    case.sources.iter().any(|source| {
        source
            .text
            .lines()
            .map(str::trim_start)
            .any(|line| line.starts_with("table ") || line.starts_with("pub table "))
    })
}

/// The harness allowances that no longer cover a run: each must be removed.
fn harness_stale(allowances: &[compare::HarnessAllowance], used: &[bool]) -> Vec<String> {
    allowances
        .iter()
        .zip(used)
        .filter(|(_, used)| !**used)
        .map(|(allowance, _)| {
            format!(
                "{}: harness allowance is stale; the case no longer differs, remove it",
                allowance.case
            )
        })
        .collect()
}
