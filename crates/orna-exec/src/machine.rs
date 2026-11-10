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

/// The machine state of one run.
pub(crate) struct Machine<'p> {
    pub(crate) program: &'p Program,
    pub(crate) limits: ExecLimits,
    steps: u64,
    depth: usize,
}

impl<'p> Machine<'p> {
    fn new(program: &'p Program, limits: ExecLimits) -> Self {
        Self {
            program,
            limits,
            steps: 0,
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

    /// Runs one function body with its argument slots already bound. A
    /// `return` ends the call with its value.
    pub(crate) fn invoke(
        &mut self,
        body_id: BodyId,
        arguments: Vec<(usize, Value)>,
    ) -> Result<Value, Stop> {
        if self.depth >= self.limits.max_depth {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        let program = self.program;
        let body: &'p Body = &program.bodies[body_id];
        let mut frame = Frame::new(body.locals.len());
        for (index, value) in arguments {
            frame.set(body.params[index], value)?;
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
    let FnBody::Source(body_id) = function.body else {
        return Err(ExecError::NotYet("entry has no source body"));
    };
    if !function.params.is_empty() {
        return Err(ExecError::NotYet("entry takes arguments"));
    }
    let mut machine = Machine::new(program, limits);
    machine.invoke(body_id, Vec::new()).map_err(ExecError::from)
}
