//! The executor-agreement allowances of stage H4 (`docs/design/hir.md` section 6.3).
//!
//! `tests/exec_share.rs` (the ratchet) and the shadow report both classify an executor run that
//! differs from the old pipeline through [`ExecAllowances`], so a difference is either explained by
//! the same allowance in both or it is a mismatch in both. Each allowance records the case it covers;
//! an allowance whose case no longer differs is stale and must be removed.

use crate::case::{Case, Family};
use crate::compare;

/// Why an executor run is allowed to differ from the old pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The H0 observer has no table store, so the old pipeline cannot reach the table.
    Harness,
    /// The old pipeline answers `ORNA-EVAL-VALUE` for a result the executor returns as a raw blob.
    Representation,
    /// A listed executor error and a listed old error, for one case.
    ExecutorGap,
    /// The executor charges a different number of steps than the old pipeline for an agreeing run.
    Step,
    /// The old pipeline answers `ORNA-EVAL-TYPE` for a currency value; the executor evaluates `Money` per spec.
    Currency,
}

impl Kind {
    /// The name the shadow report uses for this class.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::Representation => "representation",
            Self::ExecutorGap => "executor-gap",
            Self::Step => "step",
            Self::Currency => "currency",
        }
    }
}

/// The loaded allowances, with the use of each recorded during a run.
pub struct ExecAllowances {
    harness: Vec<compare::HarnessAllowance>,
    harness_used: Vec<bool>,
    representation: Vec<compare::HarnessAllowance>,
    representation_used: Vec<bool>,
    executor_gap: Vec<compare::ExecutorGapAllowance>,
    executor_gap_used: Vec<bool>,
    step: Vec<compare::StepAllowance>,
    currency: Vec<compare::HarnessAllowance>,
    currency_used: Vec<bool>,
    not_yet: Vec<compare::HarnessAllowance>,
    not_yet_used: Vec<bool>,
    step_used: Vec<bool>,
}

impl ExecAllowances {
    /// Loads every allowance table of `tests/differential/`.
    pub fn load() -> Result<Self, String> {
        let harness =
            compare::load_harness_allowances().map_err(|e| format!("harness allowances: {e}"))?;
        let representation = compare::load_representation_allowances()
            .map_err(|e| format!("representation allowances: {e}"))?;
        let executor_gap = compare::load_executor_gap_allowances()
            .map_err(|e| format!("executor-gap allowances: {e}"))?;
        let currency =
            compare::load_currency_allowances().map_err(|e| format!("currency allowances: {e}"))?;
        let step = compare::load_step_allowances().map_err(|e| format!("step allowances: {e}"))?;
        let not_yet =
            compare::load_not_yet_allowances().map_err(|e| format!("not-yet allowances: {e}"))?;
        Ok(Self {
            harness_used: vec![false; harness.len()],
            harness,
            representation_used: vec![false; representation.len()],
            representation,
            executor_gap_used: vec![false; executor_gap.len()],
            executor_gap,
            currency_used: vec![false; currency.len()],
            currency,
            not_yet_used: vec![false; not_yet.len()],
            not_yet,
            step_used: vec![false; step.len()],
            step,
        })
    }

    /// Classifies an outcome that differs from the old pipeline's. `None` is a mismatch.
    ///
    /// The order is the ratchet's: harness environment, then representation, then executor gap, then currency.
    pub fn outcome_allowed(
        &mut self,
        case: &Case,
        executor: &Result<Vec<u8>, String>,
        old: Option<&Result<Vec<u8>, String>>,
    ) -> Option<Kind> {
        if harness_environment_allowed(case, executor, old, &self.harness, &mut self.harness_used) {
            return Some(Kind::Harness);
        }
        if representation_allowed(
            case,
            old,
            executor,
            &self.representation,
            &mut self.representation_used,
        ) {
            return Some(Kind::Representation);
        }
        if executor_gap_allowed(
            case,
            executor,
            old,
            &self.executor_gap,
            &mut self.executor_gap_used,
        ) {
            return Some(Kind::ExecutorGap);
        }
        if currency_allowed(case, executor, old, &self.currency, &mut self.currency_used) {
            return Some(Kind::Currency);
        }
        None
    }

    /// Whether an agreeing run's step counts differ only as a listed step allowance covers.
    pub fn step_allowed(&mut self, case: &str, old: u64, executor: u64) -> bool {
        match self
            .step
            .iter()
            .position(|allowance| allowance.matches(case, old, executor))
        {
            Some(index) => {
                self.step_used[index] = true;
                true
            }
            None => false,
        }
    }

    /// Checks a case the executor does not run yet against its `[[not-yet-allowance]]`. An entry
    /// matches only its exact case and reason, so a case listed under another reason is a mismatch
    /// until its entry names that reason. A case with no entry is counted by its reason alone.
    pub fn not_yet_check(&mut self, case: &str, reason: &str) -> Result<(), String> {
        let mut listed: Option<&str> = None;
        for (index, allowance) in self.not_yet.iter().enumerate() {
            if allowance.case != case {
                continue;
            }
            if allowance.reason == reason {
                self.not_yet_used[index] = true;
                return Ok(());
            }
            listed = Some(allowance.reason.as_str());
        }
        match listed {
            Some(expected) => Err(format!(
                "{case}: the executor defers it for {reason:?}, not {expected:?}; update the entry"
            )),
            None => Ok(()),
        }
    }

    /// Run cases the harness environment explains, counted for the exec-share ratchet.
    #[must_use]
    pub fn harness_environment_count(&self) -> usize {
        self.harness_used.iter().filter(|used| **used).count()
    }

    /// Every allowance that no run used. Each message says which allowance to remove.
    #[must_use]
    pub fn stale(&self) -> Vec<String> {
        let mut stale = harness_stale(&self.harness, &self.harness_used);
        stale.extend(representation_stale(
            &self.representation,
            &self.representation_used,
        ));
        stale.extend(executor_gap_stale(
            &self.executor_gap,
            &self.executor_gap_used,
        ));
        stale.extend(
            self.step
                .iter()
                .zip(&self.step_used)
                .filter(|(_, used)| !**used)
                .map(|(allowance, _)| {
                    format!(
                    "{}: step allowance for old {} and executor {} is stale; remove or update it",
                    allowance.case, allowance.old, allowance.executor
                )
                }),
        );
        stale.extend(currency_stale(&self.currency, &self.currency_used));
        stale.extend(not_yet_stale(&self.not_yet, &self.not_yet_used));
        stale
    }
}

/// Whether a case's currency disagreement is the old pipeline's `ORNA-EVAL-TYPE` for a currency
/// value, where the executor returns `Ok`. Matches only the exact case id and old outcome.
fn currency_allowed(
    case: &Case,
    executor: &Result<Vec<u8>, String>,
    old: Option<&Result<Vec<u8>, String>>,
    allowances: &[compare::HarnessAllowance],
    used: &mut [bool],
) -> bool {
    let old_type = matches!(old, Some(Err(code)) if code == "ORNA-EVAL-TYPE");
    if !old_type || !executor.is_ok() {
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

/// The currency allowances that no longer cover a run: each must be removed.
fn currency_stale(allowances: &[compare::HarnessAllowance], used: &[bool]) -> Vec<String> {
    allowances
        .iter()
        .zip(used)
        .filter(|(_, used)| !**used)
        .map(|(allowance, _)| {
            format!(
                "{}: currency allowance is stale; the case no longer differs, remove it",
                allowance.case
            )
        })
        .collect()
}

/// Not-yet allowances no run used: the executor now runs the case, or defers it for another reason.
fn not_yet_stale(allowances: &[compare::HarnessAllowance], used: &[bool]) -> Vec<String> {
    allowances
        .iter()
        .zip(used)
        .filter(|(_, used)| !**used)
        .map(|(allowance, _)| {
            format!(
                "{}: not-yet allowance is stale; remove or update it",
                allowance.case
            )
        })
        .collect()
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

/// Executor-gap allowances whose exact pair no longer disagrees are stale and must be removed.
fn executor_gap_stale(allowances: &[compare::ExecutorGapAllowance], used: &[bool]) -> Vec<String> {
    allowances
        .iter()
        .zip(used)
        .filter(|(_, used)| !**used)
        .map(|(allowance, _)| {
            format!(
                "{}: executor-gap allowance for executor {} and old {} is stale; remove it",
                allowance.case, allowance.executor, allowance.old
            )
        })
        .collect()
}

/// Whether `case`'s disagreement is a listed executor gap: the executor answers the listed error
/// code and the old pipeline answers the listed error code. Any other disagreement stays a mismatch.
fn executor_gap_allowed(
    case: &Case,
    executor: &Result<Vec<u8>, String>,
    old: Option<&Result<Vec<u8>, String>>,
    allowances: &[compare::ExecutorGapAllowance],
    used: &mut [bool],
) -> bool {
    let (Err(executor_code), Some(Err(old_code))) = (executor, old) else {
        return false;
    };
    match allowances.iter().position(|allowance| {
        allowance.case == case.id
            && allowance.executor == *executor_code
            && allowance.old == *old_code
    }) {
        Some(index) => {
            used[index] = true;
            true
        }
        None => false,
    }
}
