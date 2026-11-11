//! The machine: active-call accounting and the entry point.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orna_hir::{Body, BodyId, DefId, DefKind, FnBody, Program, Ty};

use crate::dispatch::ProtocolMembers;
use crate::error::{ExecError, Stop};
use crate::frame::Frame;
use crate::link;
use crate::natives::NativeFn;
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
    /// Elements allowed in one collection a native builds or reads before `ORNA-EVAL-LIMIT`.
    pub max_collection_items: usize,
    /// Bytes allowed in one string value before `ORNA-EVAL-LIMIT`.
    pub max_string_bytes: usize,
    /// Decimal digits allowed in one integer value before `ORNA-EVAL-LIMIT`.
    pub max_integer_digits: usize,
}

/// The outcome of one measured run: its value and the deterministic counters
/// the benchmarks gate on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measured {
    /// The value the entry point returned.
    pub value: Value,
    /// Expression evaluations charged against [`ExecLimits::max_steps`].
    pub steps: u64,
    /// Calls executed, counting the entry point itself. Unlike the old
    /// evaluator, nothing is memoised, so this counts every source-level call.
    pub calls: u64,
}

/// The machine state of one run.
pub(crate) struct Machine<'p> {
    pub(crate) program: &'p Program,
    pub(crate) limits: ExecLimits,
    /// The ids of every source enum variant, keyed by its definition.
    variants: OnceCell<HashMap<DefId, VariantIds>>,
    /// The owner's cancellation request. Once set, the run ends with
    /// [`Stop::Cancelled`] at its next step.
    cancel: Option<Arc<AtomicBool>>,
    /// The engine natives this program's declarations bind to, built on the first native call.
    pub(crate) natives: OnceCell<HashMap<DefId, NativeFn>>,
    /// The empty type environment, shared by every call that binds no type parameters.
    pub(crate) empty_env: Rc<[Ty]>,
    /// The protocol each protocol member belongs to, built on first dispatch.
    pub(crate) protocols: ProtocolMembers,
    /// The ids of `core.Ordering`'s variants, built on first engine comparison.
    pub(crate) ordering: OnceCell<[VariantIds; 3]>,
    steps: u64,
    calls: u64,
    depth: usize,
}

impl<'p> Machine<'p> {
    fn new(program: &'p Program, limits: ExecLimits, cancel: Option<Arc<AtomicBool>>) -> Self {
        Self {
            program,
            limits,
            variants: OnceCell::new(),
            cancel,
            natives: OnceCell::new(),
            empty_env: Rc::from(Vec::new()),
            protocols: OnceCell::new(),
            ordering: OnceCell::new(),
            steps: 0,
            calls: 0,
            depth: 0,
        }
    }

    /// The ids of the source enum variant `variant`. The map is built on first
    /// use, so a run that never constructs or matches a variant pays nothing.
    pub(crate) fn variant_ids_of(&self, variant: DefId) -> Option<VariantIds> {
        self.variants
            .get_or_init(|| variant_ids(self.program))
            .get(&variant)
            .copied()
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
        env: Rc<[Ty]>,
    ) -> Result<Value, Stop> {
        if self.depth >= self.limits.max_depth {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        self.calls += 1;
        let program = self.program;
        let body: &'p Body = &program.bodies[body_id];
        let mut frame = Frame::new(body.locals.len(), env);
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

fn run(
    program: &Program,
    entry: DefId,
    limits: ExecLimits,
    cancel: Option<Arc<AtomicBool>>,
) -> Run {
    let mut machine = Machine::new(program, limits, cancel);
    let result = start(&mut machine, program, entry);
    Run {
        result,
        steps: machine.steps,
    }
}

fn start(machine: &mut Machine<'_>, program: &Program, entry: DefId) -> Result<Value, ExecError> {
    link::check_abi(program).map_err(ExecError::NotYet)?;
    let DefKind::Fn(function) = &program.defs[entry].kind else {
        return Err(ExecError::NotYet("entry is not a function"));
    };
    if !function.params.is_empty() {
        return Err(ExecError::NotYet("entry takes arguments"));
    }
    let FnBody::Source(body_id) = function.body else {
        return Err(ExecError::NotYet("entry has no source body"));
    };
    machine
        .invoke(body_id, Vec::new(), &[], Rc::clone(&machine.empty_env))
        .map_err(ExecError::from)
}

/// Runs the function `entry` with one argument per parameter, in declaration
/// order, and reports its step and call counters.
///
/// # Errors
///
/// [`ExecError::Code`] when the program ends with a language error, and
/// [`ExecError::NotYet`] when the entry is not a source function or it reaches
/// a construct this stage does not run.
pub fn measure_entry(
    program: &Program,
    entry: DefId,
    arguments: Vec<Value>,
    limits: ExecLimits,
) -> Result<Measured, ExecError> {
    link::check_abi(program).map_err(ExecError::NotYet)?;
    let DefKind::Fn(function) = &program.defs[entry].kind else {
        return Err(ExecError::NotYet("entry is not a function"));
    };
    let FnBody::Source(body_id) = function.body else {
        return Err(ExecError::NotYet("entry has no source body"));
    };
    if function.params.len() != arguments.len() {
        return Err(ExecError::Code("ORNA-EVAL-ARGUMENT"));
    }
    let arguments = arguments.into_iter().enumerate().collect();
    let mut machine = Machine::new(program, limits, None);
    let value = machine
        .invoke(body_id, arguments, &[], Rc::clone(&machine.empty_env))
        .map_err(ExecError::from)?;
    Ok(Measured {
        value,
        steps: machine.steps,
        calls: machine.calls,
    })
}
