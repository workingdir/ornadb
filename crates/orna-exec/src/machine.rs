//! The machine: active-call accounting and the entry point.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orna_hir::{Body, BodyId, DefId, DefKind, FnBody, Program};

use crate::dispatch::ProtocolMembers;
use crate::error::{ExecError, Stop};
use crate::frame::{Frame, TypeEnv};
use crate::globals::Globals;
use crate::host::{Host, StoreHost};
use crate::link;
use crate::natives::NativeFn;
use crate::store::TableStore;
use crate::table::store_invariant;
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
    /// Rows the table store handed to a scan during the run. A `take` that stops early leaves the rest unread.
    pub rows_read: u64,
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
    /// The protocol each protocol member belongs to, built on first dispatch.
    pub(crate) protocols: ProtocolMembers,
    /// The ids of `core.Ordering`'s variants, built on first engine comparison.
    pub(crate) ordering: OnceCell<[VariantIds; 3]>,
    /// The host the run reads its tables, clock, entropy and `sys` members from. Each entry-point activation opens
    /// one store activation on its store, committed or rolled back.
    pub(crate) host: &'p mut dyn Host,
    /// The session's evaluated top-level `let`s. Taken from the caller for the run and handed back after it.
    pub(crate) globals: Globals,
    /// Whether this run's store starts empty and no other writer shares it, so a row absent from the store is
    /// absent from the activation. Only the entry points that create their own empty store set it.
    pub(crate) empty_snapshot: bool,
    steps: u64,
    calls: u64,
    depth: usize,
}

impl<'p> Machine<'p> {
    pub(crate) fn new(
        program: &'p Program,
        limits: ExecLimits,
        cancel: Option<Arc<AtomicBool>>,
        host: &'p mut dyn Host,
    ) -> Self {
        Self {
            program,
            limits,
            variants: OnceCell::new(),
            cancel,
            natives: OnceCell::new(),
            protocols: OnceCell::new(),
            ordering: OnceCell::new(),
            host,
            globals: Globals::new(),
            empty_snapshot: false,
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
    /// A failing activation drops its frame. The store is rolled back by [`Machine::run_activation`] at the
    /// entry boundary, which owns the store activation.
    pub(crate) fn invoke(
        &mut self,
        body_id: BodyId,
        arguments: Vec<(usize, Value)>,
        captured: &[Cell],
        env: TypeEnv,
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

    /// Runs the activation of `body_id` inside one store activation. The writes commit when it returns a value
    /// and roll back when it stops with any error, so a failed entry leaves the store unchanged.
    ///
    /// # Errors
    ///
    /// The activation's own stop, or [`Stop::NotYet`] when the store breaks its activation contract.
    pub(crate) fn run_activation(
        &mut self,
        body_id: BodyId,
        arguments: Vec<(usize, Value)>,
    ) -> Result<Value, Stop> {
        self.host
            .store()
            .begin()
            .map_err(|error| store_invariant(&error))?;
        match self.invoke(body_id, arguments, &[], None) {
            Ok(value) => {
                self.host
                    .store()
                    .commit()
                    .map_err(|error| store_invariant(&error))?;
                Ok(value)
            }
            Err(stop) => {
                self.host
                    .store()
                    .rollback()
                    .map_err(|error| store_invariant(&error))?;
                Err(stop)
            }
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
    run(program, entry, limits, None, &mut StoreHost::empty(), true)
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
    run(
        program,
        entry,
        limits,
        Some(cancel),
        &mut StoreHost::empty(),
        true,
    )
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
    host: &mut dyn Host,
    empty_snapshot: bool,
) -> Run {
    let mut machine = Machine::new(program, limits, cancel, host);
    machine.empty_snapshot = empty_snapshot;
    let result = start(&mut machine, program, entry);
    Run {
        result,
        steps: machine.steps,
    }
}

/// Runs the zero-parameter function `entry` to its value, reading and writing through `host`: its store, clock,
/// entropy and `sys` members.
///
/// # Errors
///
/// As [`run_entry`].
pub fn run_entry_with_host(
    program: &Program,
    entry: DefId,
    limits: ExecLimits,
    host: &mut dyn Host,
) -> Run {
    run(program, entry, limits, None, host, false)
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
        .run_activation(body_id, Vec::new())
        .map_err(ExecError::from)
}

/// Runs the function `entry` with one argument per parameter, in declaration
/// order, and reports its step, call and row-read counters.
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
    let mut host = StoreHost::empty();
    let mut machine = Machine::new(program, limits, None, &mut host);
    machine.empty_snapshot = true;
    measure(&mut machine, program, entry, arguments)
}

/// As [`measure_entry`], but the run reads and writes through `host`: its store, clock, entropy and `sys` members.
///
/// # Errors
///
/// As [`measure_entry`].
pub fn measure_entry_with_host(
    program: &Program,
    entry: DefId,
    arguments: Vec<Value>,
    limits: ExecLimits,
    host: &mut dyn Host,
) -> Result<Measured, ExecError> {
    let mut machine = Machine::new(program, limits, None, host);
    measure(&mut machine, program, entry, arguments)
}

/// As [`measure_entry`], but the run reads and writes `store`. The store comes back with the run's writes
/// applied, or rolled back when the run fails, so a later run sees the rows an earlier run committed.
pub fn measure_entry_with_store(
    program: &Program,
    entry: DefId,
    arguments: Vec<Value>,
    limits: ExecLimits,
    store: Box<dyn TableStore>,
) -> (Result<Measured, ExecError>, Box<dyn TableStore>) {
    let mut host = StoreHost::new(store);
    let result = {
        let mut machine = Machine::new(program, limits, None, &mut host);
        measure(&mut machine, program, entry, arguments)
    };
    (result, host.into_store())
}

/// As [`measure_entry_with_store`], but the run also reads and extends the session `globals`: a top-level `let` already
/// evaluated by an earlier run is read from the cache, not evaluated again.
///
/// # Errors
///
/// As [`measure_entry`].
pub fn measure_entry_in(
    program: &Program,
    entry: DefId,
    arguments: Vec<Value>,
    limits: ExecLimits,
    store: Box<dyn TableStore>,
    globals: &mut Globals,
) -> (Result<Measured, ExecError>, Box<dyn TableStore>) {
    let mut host = StoreHost::new(store);
    let result = {
        let mut machine = Machine::new(program, limits, None, &mut host);
        machine.globals = std::mem::take(globals);
        let result = measure(&mut machine, program, entry, arguments);
        *globals = std::mem::take(&mut machine.globals);
        result
    };
    (result, host.into_store())
}

fn measure(
    machine: &mut Machine<'_>,
    program: &Program,
    entry: DefId,
    arguments: Vec<Value>,
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
    let rows_before = machine.host.store().rows_read();
    let value = machine
        .run_activation(body_id, arguments)
        .map_err(ExecError::from)?;
    Ok(Measured {
        value,
        steps: machine.steps,
        calls: machine.calls,
        rows_read: machine.host.store().rows_read() - rows_before,
    })
}
