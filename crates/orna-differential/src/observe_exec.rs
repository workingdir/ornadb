//! The executor's observer for stage H4: the outcome of running a case's entry
//! over its lowered HIR, compared with the old evaluator's outcome.
//!
//! `docs/design/hir.md` section 6.3 counts what the executor does not run yet as
//! "not yet on HIR" rather than skipping it silently. [`run_case`] returns
//! either the entry's outcome or that reason, and the ratchet test counts both.
//!
//! The executor runs only a case the HIR lowers with no owned diagnostic, that
//! is evaluated, that has no table rows or arguments, and whose entry is a
//! source function. A step-budget exhaustion is not compared: the executor
//! counts its own steps, and the step parity is a later stage's measurement.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use orna_exec::{ExecError, ExecLimits, run_entry, run_entry_cancellable};
use orna_hir::Idx;
use orna_value::CanonicalValue;

use crate::case::{Case, CaseKind};
use crate::observe_new::{self, LoweredProgram};

/// The old evaluator's step ceiling for one run (`orna-evaluator` `STEPS`), so
/// the executor is bounded like the old pipeline it is compared with.
const EXEC_LIMITS: ExecLimits = ExecLimits {
    max_steps: 2048 * 200 + 10_000,
    max_depth: 64,
    max_collection_items: 2048,
    max_string_bytes: 16_384,
    max_integer_digits: 1024,
};

/// The step-budget code the executor reports when its own budget ends a run.
const STEP_LIMIT_CODE: &str = "ORNA-EVAL-LIMIT";

/// What the executor did with one case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecOutcome {
    /// The entry ran to a value or to a language error, in the same shape as
    /// [`crate::Observation::outcome`]: canonical bytes or the error code.
    Ran {
        /// Canonical bytes or the error code, in the shape of [`crate::Observation::outcome`].
        outcome: Result<Vec<u8>, String>,
        /// The legacy evaluator steps the run charged, per node weight.
        steps: u64,
    },
    /// The case is outside what this stage runs; the reason names the stage or
    /// construct that owns it.
    NotYet(String),
}

/// Runs one case's entry over its lowered HIR, to its outcome.
#[must_use]
pub fn run_case(case: &Case) -> ExecOutcome {
    run_with(case, None)
}

/// Runs one case as a cancelled run: the executor ends it when `cancel` is set,
/// and the outcome is the old pipeline's cancellation code.
pub fn run_case_cancellable(case: &Case, cancel: Arc<AtomicBool>) -> ExecOutcome {
    run_with(case, Some(cancel))
}

fn run_with(case: &Case, cancel: Option<Arc<AtomicBool>>) -> ExecOutcome {
    if !case.evaluate {
        return ExecOutcome::NotYet("the case is not evaluated".to_owned());
    }
    if !case.rows.is_empty() || !case.arguments.is_empty() || !case.inputs.is_empty() {
        return ExecOutcome::NotYet("the case binds rows, arguments or inputs (H5/H8)".to_owned());
    }
    let lowered = match observe_new::lower_case_admitted(case) {
        Ok(lowered) => lowered,
        Err(error) => return ExecOutcome::NotYet(error),
    };
    if let Some(code) = admission_rejection(&lowered) {
        return ExecOutcome::Ran {
            outcome: Err(code.to_owned()),
            steps: 0,
        };
    }
    if owned_diagnostics(&lowered) {
        return ExecOutcome::NotYet("the owned modules lower with a diagnostic".to_owned());
    }
    let Some(entry) = entry_name(case) else {
        return ExecOutcome::NotYet("the module case has no entry function".to_owned());
    };
    let Some(def) = entry_def(&lowered, case, entry) else {
        return ExecOutcome::NotYet(format!("the entry `{entry}` is not a function of the case"));
    };
    let run = match cancel {
        Some(flag) => run_entry_cancellable(&lowered.program, def, EXEC_LIMITS, flag),
        None => run_entry(&lowered.program, def, EXEC_LIMITS),
    };
    let steps = run.steps;
    match run.result {
        Ok(value) => ExecOutcome::Ran {
            outcome: match value.to_raw() {
                Ok(raw) => encode(raw),
                Err(code) => Err(code.to_owned()),
            },
            steps,
        },
        Err(ExecError::Code(STEP_LIMIT_CODE)) => ExecOutcome::NotYet(
            "the step budget ends the run; step parity is not measured yet".to_owned(),
        ),
        Err(ExecError::Code(code)) => ExecOutcome::Ran {
            outcome: Err(code.to_owned()),
            steps,
        },
        Err(ExecError::NotYet(reason)) => ExecOutcome::NotYet(reason.to_owned()),
        Err(ExecError::Cancelled) => ExecOutcome::Ran {
            outcome: Err("ORNA-EVAL-CANCELLED".to_owned()),
            steps,
        },
    }
}

/// The canonical bytes of a raw value, as the old pipeline encodes a result.
fn encode(raw: orna_value::Raw) -> Result<Vec<u8>, String> {
    CanonicalValue::new(raw)
        .and_then(|value| value.encode())
        .map_err(|error| format!("canonical encoding failed: {error:?}"))
}

/// The entry's name: `run` for an expression case, the case's entry otherwise.
fn entry_name(case: &Case) -> Option<&str> {
    match case.kind {
        CaseKind::Expression => Some("run"),
        CaseKind::Module | CaseKind::StandardBundle => case.entry.as_deref(),
        CaseKind::Repl => None,
    }
}

/// The code of an owned-module lowering rejection the old pipeline also
/// reports before evaluation: an unlinked `sys` member
/// (`sys.version.incompatible`, ORNA-SYS-008). The old observer reports the
/// first sorted code, which equals this code only when it is the sole code, so
/// a mixed set has no shared outcome and is left to `owned_diagnostics`.
fn admission_rejection(lowered: &LoweredProgram) -> Option<&'static str> {
    let mut rejection = None;
    for (module, found) in lowered.program.modules.iter_enumerated() {
        if !lowered.owned[module.index()] {
            continue;
        }
        for diagnostic in &found.diagnostics {
            if diagnostic.code != orna_hir::diag::SYS_VERSION_INCOMPATIBLE {
                return None;
            }
            rejection = Some(orna_hir::diag::SYS_VERSION_INCOMPATIBLE);
        }
    }
    rejection
}

/// Whether any owned module carries a lowering diagnostic.
fn owned_diagnostics(lowered: &LoweredProgram) -> bool {
    lowered
        .program
        .modules
        .iter_enumerated()
        .any(|(module, found)| lowered.owned[module.index()] && !found.diagnostics.is_empty())
}

/// The definition `entry` names in the case's primary module.
fn entry_def(lowered: &LoweredProgram, case: &Case, entry: &str) -> Option<orna_hir::DefId> {
    let primary = case.sources.first()?.logical_path.as_str();
    let (module, _) = lowered
        .program
        .modules
        .iter_enumerated()
        .find(|(module, _)| lowered.logical_paths[module.index()].as_deref() == Some(primary))?;
    lowered.program.modules[module]
        .items
        .iter()
        .copied()
        .find(|def| lowered.program.defs[*def].name.as_str() == entry)
}
