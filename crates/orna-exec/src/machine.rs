//! The machine: active-call accounting and the entry point.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orna_hir::{Body, BodyId, DefId, DefKind, FnBody, Program};

use crate::error::{ExecError, Stop};
use crate::frame::Frame;
use crate::value::{Cell, Value};
use crate::variants::{VariantIds, variant_ids};

/// The resource limits one run is bounded by. Their defaults in the old
/// evaluator are the parity targets for the differential harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecLimits {
    /// Expression evaluations allowed before `ORNA-EVAL-LIMIT`.
    pub max_steps: u64,
    /// Active calls allowed before `ORNA-EVAL-LIMIT`.
    pub max_depth: usize,
    /// Bytes allowed in one string value before `ORNA-EVAL-LIMIT`.
    pub max_string_bytes: usize,
    /// Decimal digits allowed in one integer value before `ORNA-EVAL-LIMIT`.
    pub max_integer_digits: usize,
}

/// The machine state of one run.
pub(crate) struct Machine<'p> {
    pub(crate) program: &'p Program,
    pub(crate) limits: ExecLimits,
    /// The ids of every source enum variant, keyed by its definition.
    pub(crate) variants: HashMap<DefId, VariantIds>,
    /// The owner's cancellation request. Once set, the run ends with
    /// [`Stop::Cancelled`] at its next step.
    cancel: Option<Arc<AtomicBool>>,
    steps: u64,
    depth: usize,
}

impl<'p> Machine<'p> {
    fn new(program: &'p Program, limits: ExecLimits, cancel: Option<Arc<AtomicBool>>) -> Self {
        Self {
            program,
            limits,
            variants: variant_ids(program),
            cancel,
            steps: 0,
            depth: 0,
        }
    }

    /// Charges one expression evaluation against the step limit, and ends the
    /// run when the owner has requested cancellation.
    pub(crate) fn tick(&mut self, weight: u16) -> Result<(), Stop> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(Stop::Cancelled);
        }
        self.steps += u64::from(weight);
        if self.steps > self.limits.max_steps {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        Ok(())
    }

    /// Runs one body with its argument slots and capture slots already bound.
    /// A `return` ends the call with its value.
    ///
    /// A failing activation drops its frame. The pure core holds no other
    /// state, so a failure has nothing to roll back at the activation boundary.
    pub(crate) fn invoke(
        &mut self,
        body_id: BodyId,
        arguments: Vec<(usize, Value)>,
        captured: &[Cell],
    ) -> Result<Value, Stop> {
        if self.depth >= self.limits.max_depth {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        let program = self.program;
        let body: &'p Body = &program.bodies[body_id];
        let mut frame = Frame::new(body.locals.len());
        for (index, value) in arguments {
            let Some(&local) = body.params.get(index) else {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            };
            frame.bind(local, value)?;
        }
        for (capture, cell) in body.captures.iter().zip(captured) {
            frame.share(capture.inner, cell.clone())?;
        }
        self.depth += 1;
        let result = self.eval(body, &mut frame, body.root);
        self.depth -= 1;
        match result {
            Ok(value) | Err(Stop::Return(value)) => Ok(value),
            Err(stop) => Err(stop),
        }
    }
}

/// Runs the zero-parameter function `entry` to its value.
///
/// # Errors
///
/// [`ExecError::Code`] when the program ends with a language error, and
/// [`ExecError::NotYet`] when it reaches a construct this stage does not run.
pub fn run_entry(program: &Program, entry: DefId, limits: ExecLimits) -> Run {
    run(program, entry, limits, None)
}

/// Runs the zero-parameter function `entry` to its value, ending the run with
/// [`ExecError::Cancelled`] at the first step after `cancel` is set.
///
/// Cancellation is its own completion. No `|?` recovers from it, and it is not
/// reported as a language error.
///
/// # Errors
///
/// As [`run_entry`], plus [`ExecError::Cancelled`].
pub fn run_entry_cancellable(
    program: &Program,
    entry: DefId,
    limits: ExecLimits,
    cancel: Arc<AtomicBool>,
) -> Run {
    run(program, entry, limits, Some(cancel))
}

/// The outcome of one run and the steps it charged.
#[derive(Debug)]
pub struct Run {
    /// The entry's value, or the reason the run ended.
    pub result: Result<Value, ExecError>,
    /// The legacy evaluator steps charged: each node counts its weight.
    pub steps: u64,
}

fn run(program: &Program, entry: DefId, limits: ExecLimits, cancel: Option<Arc<AtomicBool>>) -> Run {
    let mut machine = Machine::new(program, limits, cancel);
    let result = start(&mut machine, program, entry);
    Run {
        result,
        steps: machine.steps,
    }
}

fn start(machine: &mut Machine<'_>, program: &Program, entry: DefId) -> Result<Value, ExecError> {
    let DefKind::Fn(function) = &program.defs[entry].kind else {
        return Err(ExecError::NotYet("entry is not a function"));
    };
    let FnBody::Source(body_id) = function.body else {
        return Err(ExecError::NotYet("entry has no source body"));
    };
    if !function.params.is_empty() {
        return Err(ExecError::NotYet("entry takes arguments"));
    }
    machine
        .invoke(body_id, Vec::new(), &[])
        .map_err(ExecError::from)
}
