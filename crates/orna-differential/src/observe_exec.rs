//! The executor's observer for stage H4: the outcome of running a case's entry
//! over its lowered HIR, compared with the old evaluator's outcome.
//!
//! `docs/design/hir.md` section 6.3 counts what the executor does not run yet as
//! "not yet on HIR" rather than skipping it silently. [`run_case`] returns
//! either the entry's outcome or that reason, and the ratchet test counts both.
//!
//! The executor runs only a case the HIR lowers with no owned diagnostic, that
//! is evaluated, that has no table rows or arguments, and whose entry is a
//! source function. Its owned modules are then type-checked. When every owned
//! diagnostic is one money, affine or quantity code (E5001-E5004, E5103), that
//! code is the case's outcome; a set that mixes two such codes, or one such
//! code with another diagnostic, is not run. Other type-check diagnostics leave
//! the run as it is, and the agreement ratchet checks each run against the old
//! pipeline. A step-budget exhaustion is not compared: the executor counts its
//! own steps, and the step parity is a later stage's measurement.

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

/// The money, affine and quantity rejections the checker reports (E5001-E5004,
/// E5103). The old pipeline reports one of them as a case's outcome.
const CHECKER_CODES: [&str; 5] = [
    orna_hir::diag::QUANTITY_DIMENSIONS,
    orna_hir::diag::AFFINE_POINT_OPERATOR,
    orna_hir::diag::MONEY_CURRENCY_MISMATCH,
    orna_hir::diag::MONEY_FROM_FLOAT,
    orna_hir::diag::MONEY_BINARY_FLOAT,
];

/// What the executor did with one case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecOutcome {
    /// The case ran to its outcome, in the same shape as [`crate::Observation::outcome`]: `None` when the
    /// last input binds a name and produces no value, otherwise canonical bytes or the error code.
    Ran {
        /// `None` when the case produces no value; otherwise canonical bytes or the error code.
        outcome: Option<Result<Vec<u8>, String>>,
        /// The legacy evaluator steps the run charged, per node weight. A REPL session charges none: the
        /// old REPL records no step count, so it has no step parity to measure.
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
    if !case.rows.is_empty() || !case.arguments.is_empty() {
        return ExecOutcome::NotYet("the case binds rows or arguments (H5)".to_owned());
    }
    if case.kind == CaseKind::Repl {
        return run_repl(case, cancel);
    }
    if !case.inputs.is_empty() {
        return ExecOutcome::NotYet("the case binds inputs outside a REPL session".to_owned());
    }
    let mut lowered = match observe_new::lower_case_admitted(case) {
        Ok(lowered) => lowered,
        Err(error) => return ExecOutcome::NotYet(error),
    };
    if let Some(code) = admission_rejection(&lowered) {
        return ExecOutcome::Ran {
            outcome: Some(Err(code.to_owned())),
            steps: 0,
        };
    }
    if owned_diagnostics(&lowered) {
        return ExecOutcome::NotYet("the owned modules lower with a diagnostic".to_owned());
    }
    // The checker's money, affine and quantity rejections are the case's outcome
    // (docs/design/hir-host.md B8), and so are the table-write rejections (E2002, E2003, E3010):
    // `typeck` reports every one of them as an owned diagnostic, and the run is lowered and checked once.
    orna_hir::typeck(&mut lowered.program);
    if let Some(Some(code)) = static_rejection(&lowered) {
        return ExecOutcome::Ran {
            outcome: Err(code.to_owned()),
            steps: 0,
        };
    }
    if let Some(code) = checker_rejection(&lowered) {
        return ExecOutcome::Ran {
            outcome: Err(code.to_owned()),
            steps: 0,
        };
    }
    if checker_coded(&lowered) {
        return ExecOutcome::NotYet(
            "the owned modules have mixed type-check rejections".to_owned(),
        );
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
            outcome: Some(match value.to_raw() {
                Ok(raw) => encode(raw),
                Err(code) => Err(code.to_owned()),
            }),
            steps,
        },
        Err(ExecError::Code(STEP_LIMIT_CODE)) => ExecOutcome::NotYet(
            "the step budget ends the run; step parity is not measured yet".to_owned(),
        ),
        Err(ExecError::Code(code)) => ExecOutcome::Ran {
            outcome: Some(Err(code.to_owned())),
            steps,
        },
        Err(ExecError::NotYet(reason)) => ExecOutcome::NotYet(reason.to_owned()),
        Err(ExecError::Cancelled) => ExecOutcome::Ran {
            outcome: Some(Err("ORNA-EVAL-CANCELLED".to_owned())),
            steps,
        },
    }
}

/// Runs one REPL case through the orna-repl session, input by input, and reports the last input's outcome: the
/// value it produces (`None` for a name binding), or the error code of its rejection. Every input is submitted, so
/// a rejected input does not stop the session, as the old REPL's `AdmittedReplSession` does not.
fn run_repl(case: &Case, cancel: Option<Arc<AtomicBool>>) -> ExecOutcome {
    if cancel.is_some() {
        return ExecOutcome::NotYet("a cancelled REPL session is not run on HIR yet".to_owned());
    }
    let mut session = orna_repl::Session::new();
    let mut last = None;
    for input in &case.inputs {
        last = Some(match orna_repl::submit(&mut session, input) {
            Ok(None) => None,
            Ok(Some(value)) => Some(match value.to_raw() {
                Ok(raw) => encode(raw),
                Err(code) => Err(code.to_owned()),
            }),
            Err(error) => Some(Err(error.code().to_owned())),
        });
    }
    ExecOutcome::Ran {
        outcome: last.flatten(),
        steps: 0,
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

/// The lowering rejections the old pipeline reports as its outcome before evaluation, each under the
/// spec's own code: an unlinked `sys` member (`sys.version.incompatible`, ORNA-SYS-008), the renamed
/// `sys.runtime` (ORNA-SYS-005), a `sys.storage` call (ORNA-SYS-120) and a `sys` row mutator (E7001).
const STATIC_REJECTIONS: [&str; 4] = [
    orna_hir::diag::SYS_VERSION_INCOMPATIBLE,
    orna_hir::diag::LEGACY_SYS_RUNTIME,
    orna_hir::diag::SYS_STORAGE_CALL,
    orna_hir::diag::SYS_COMMIT_READ_ONLY,
];

/// The code of an owned-module lowering rejection the old pipeline also reports before evaluation.
/// The old observer reports the first sorted code, which equals this code only when it is the sole
/// distinct code, so a set of two different codes, or any other diagnostic, has no shared outcome and
/// is left to `owned_diagnostics`.
fn admission_rejection(lowered: &LoweredProgram) -> Option<&'static str> {
    let mut rejection: Option<&'static str> = None;
    for (module, found) in lowered.program.modules.iter_enumerated() {
        if !lowered.owned[module.index()] {
            continue;
        }
        for diagnostic in &found.diagnostics {
            let code = STATIC_REJECTIONS
                .into_iter()
                .find(|code| *code == diagnostic.code)?;
            match rejection {
                None => rejection = Some(code),
                Some(first) if first != code => return None,
                Some(_) => {}
            }
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

/// The checker codes a table write answers before evaluation: a missing required field (E2002), an unknown
/// field (E2003), and a computed field written on insert (E3010).
const STATIC_TABLE_WRITE_CODES: [&str; 3] = ["E2002", "E2003", "E3010"];

/// The static rejection the owned modules' type-pass diagnostics carry, when every one of them is a table-write
/// code. `Some(None)` is no diagnostic; `Some(Some(code))` is the first sorted code, which is the old pipeline's
/// outcome for the same set; `None` is a diagnostic outside the table-write codes.
fn static_rejection(lowered: &LoweredProgram) -> Option<Option<&'static str>> {
    let mut first: Option<&'static str> = None;
    for (module, found) in lowered.program.modules.iter_enumerated() {
        if !lowered.owned[module.index()] {
            continue;
        }
        for diagnostic in &found.diagnostics {
            let code = STATIC_TABLE_WRITE_CODES
                .into_iter()
                .find(|known| *known == diagnostic.code)?;
            first = Some(first.map_or(code, |current| current.min(code)));
        }
    }
    Some(first)
}

/// The code of the type-check rejection the old pipeline also reports as the
/// case's outcome: the one money, affine or quantity code (E5001-E5004, E5103)
/// that every owned diagnostic carries. The old observer reports the first
/// sorted code, which equals this code only when it is the sole code, so a
/// set with two such codes, or with one such code and another diagnostic, has
/// no shared outcome (see `checker_coded`).
fn checker_rejection(lowered: &LoweredProgram) -> Option<&'static str> {
    let mut rejection = None;
    for (module, found) in lowered.program.modules.iter_enumerated() {
        if !lowered.owned[module.index()] {
            continue;
        }
        for diagnostic in &found.diagnostics {
            let code = CHECKER_CODES
                .iter()
                .copied()
                .find(|known| *known == diagnostic.code)?;
            if rejection.is_some_and(|sole| sole != code) {
                return None;
            }
            rejection = Some(code);
        }
    }
    rejection
}

/// Whether any owned module carries a money, affine or quantity rejection.
fn checker_coded(lowered: &LoweredProgram) -> bool {
    for (module, found) in lowered.program.modules.iter_enumerated() {
        if !lowered.owned[module.index()] {
            continue;
        }
        for diagnostic in &found.diagnostics {
            if CHECKER_CODES.contains(&diagnostic.code) {
                return true;
            }
        }
    }
    false
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
