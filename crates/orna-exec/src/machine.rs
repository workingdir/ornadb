//! The machine: active-call accounting and the entry point.

use orna_hir::{Body, BodyId, DefId, DefKind, FnBody, Program};

use crate::error::{ExecError, Stop};
use crate::frame::Frame;
use crate::value::Value;

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
    steps: u64,
    calls: u64,
    depth: usize,
}

impl<'p> Machine<'p> {
    fn new(program: &'p Program, limits: ExecLimits) -> Self {
        Self {
            program,
            limits,
            steps: 0,
            calls: 0,
            depth: 0,
        }
    }

    /// Charges one expression evaluation against the step limit.
    pub(crate) fn tick(&mut self) -> Result<(), Stop> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        Ok(())
    }

    /// Runs one body with its argument slots and capture slots already bound.
    /// A `return` ends the call with its value.
    pub(crate) fn invoke(
        &mut self,
        body_id: BodyId,
        arguments: Vec<(usize, Value)>,
        captured: &[Value],
    ) -> Result<Value, Stop> {
        if self.depth >= self.limits.max_depth {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        self.calls += 1;
        let program = self.program;
        let body: &'p Body = &program.bodies[body_id];
        let mut frame = Frame::new(body.locals.len());
        for (index, value) in arguments {
            let Some(&local) = body.params.get(index) else {
                return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
            };
            frame.set(local, value)?;
        }
        for (capture, value) in body.captures.iter().zip(captured) {
            frame.set(capture.inner, value.clone())?;
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
pub fn run_entry(program: &Program, entry: DefId, limits: ExecLimits) -> Result<Value, ExecError> {
    let DefKind::Fn(function) = &program.defs[entry].kind else {
        return Err(ExecError::NotYet("entry is not a function"));
    };
    if !function.params.is_empty() {
        return Err(ExecError::NotYet("entry takes arguments"));
    }
    measure_entry(program, entry, Vec::new(), limits).map(|measured| measured.value)
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
    let mut machine = Machine::new(program, limits);
    let value = machine
        .invoke(body_id, arguments, &[])
        .map_err(ExecError::from)?;
    Ok(Measured {
        value,
        steps: machine.steps,
        calls: machine.calls,
    })
}
