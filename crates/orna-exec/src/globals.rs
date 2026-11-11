//! Top-level `let` bindings as session globals.
//!
//! A `let` is evaluated once: the first read runs its initializer and the value is kept in a [`Globals`] cache the
//! caller owns across runs. A later run reads the cached value, so a REPL session's bindings persist from one line to
//! the next.

use std::collections::HashMap;
use std::mem;

use orna_hir::{DefId, DefKind, Program};

use crate::error::{ExecError, Stop};
use crate::link;
use crate::machine::{ExecLimits, Machine};
use crate::store::TableStore;
use crate::value::Value;

/// The evaluated top-level `let` bindings, keyed by their definition.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    values: HashMap<DefId, Value>,
}

impl Globals {
    /// An empty cache: no `let` has been evaluated yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached value of the `let` `def`, if it has been evaluated.
    #[must_use]
    pub fn get(&self, def: DefId) -> Option<&Value> {
        self.values.get(&def)
    }
}

impl Machine<'_> {
    /// Reads the top-level `let` `def`, evaluating its initializer on first use and caching the value.
    pub(crate) fn global(&mut self, def: DefId) -> Result<Value, Stop> {
        if let Some(value) = self.globals.get(def) {
            return Ok(value.clone());
        }
        let program = self.program;
        let DefKind::Let(binding) = &program.defs[def].kind else {
            return Err(Stop::NotYet("global is not a let"));
        };
        let value = self.invoke(binding.body, Vec::new(), &[], None)?;
        self.globals.values.insert(def, value.clone());
        Ok(value)
    }
}

/// Evaluates the top-level `let` `def` in a run of its own, caching the value in `globals`. A value already cached
/// is returned without evaluating the initializer again.
///
/// # Errors
///
/// [`ExecError::Code`] when the initializer ends with a language error, [`ExecError::NotYet`] when `def` is not a
/// `let` or the initializer reaches a construct this stage does not run.
pub fn evaluate_global(
    program: &Program,
    def: DefId,
    limits: ExecLimits,
    store: Box<dyn TableStore>,
    globals: &mut Globals,
) -> (Result<Value, ExecError>, Box<dyn TableStore>) {
    let mut machine = Machine::new(program, limits, None, store);
    machine.globals = mem::take(globals);
    let result = define(&mut machine, program, def);
    *globals = mem::take(&mut machine.globals);
    (result, machine.into_store())
}

fn define(machine: &mut Machine<'_>, program: &Program, def: DefId) -> Result<Value, ExecError> {
    link::check_abi(program).map_err(ExecError::NotYet)?;
    if let Some(value) = machine.globals.get(def) {
        return Ok(value.clone());
    }
    let DefKind::Let(binding) = &program.defs[def].kind else {
        return Err(ExecError::NotYet("global is not a let"));
    };
    let value = machine
        .run_activation(binding.body, Vec::new())
        .map_err(ExecError::from)?;
    machine.globals.values.insert(def, value.clone());
    Ok(value)
}
