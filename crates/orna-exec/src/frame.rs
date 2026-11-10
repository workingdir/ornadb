//! The local slots of one active body.
//!
//! A slot holds its value inline until a closure captures it. Only then does
//! the slot become a [`Cell`]: the variable itself, shared by every closure that
//! captured it (`docs/language/reference/03-bindings-and-expressions.md`,
//! "Closures share the slot"). A binding replaces the slot's variable, so each
//! execution of a binding gets its own variable and a closure that captured an
//! earlier one keeps it.

use std::cell::RefCell;
use std::rc::Rc;

use orna_hir::{Idx, LocalId};

use crate::error::Stop;
use crate::value::{Cell, Value};

/// The variable held by one slot.
#[derive(Debug)]
enum Variable {
    /// Not captured: the value lives in the slot itself.
    Value(Value),
    /// Captured by a closure: the value lives in a cell shared with the closure.
    Shared(Cell),
}

/// One call's local slots, indexed by [`LocalId`].
#[derive(Debug)]
pub(crate) struct Frame {
    slots: Vec<Option<Variable>>,
}

impl Frame {
    /// A frame with `count` empty slots.
    pub(crate) fn new(count: usize) -> Self {
        Self {
            slots: std::iter::repeat_with(|| None).take(count).collect(),
        }
    }

    /// Reads a bound slot.
    pub(crate) fn get(&self, slot: LocalId) -> Result<Value, Stop> {
        match self.slots.get(slot.index()) {
            Some(Some(Variable::Value(value))) => Ok(value.clone()),
            Some(Some(Variable::Shared(cell))) => Ok(cell.borrow().clone()),
            _ => Err(Stop::NotYet("read of an unbound local")),
        }
    }

    /// The variable in a bound slot, for a closure to capture. The slot moves to
    /// a shared cell on its first capture and keeps that cell afterwards.
    pub(crate) fn cell(&mut self, slot: LocalId) -> Result<Cell, Stop> {
        let Some(entry) = self.slots.get_mut(slot.index()) else {
            return Err(Stop::NotYet("read of an unbound local"));
        };
        match entry.take() {
            Some(Variable::Shared(cell)) => {
                *entry = Some(Variable::Shared(Rc::clone(&cell)));
                Ok(cell)
            }
            Some(Variable::Value(value)) => {
                let cell = Rc::new(RefCell::new(value));
                *entry = Some(Variable::Shared(Rc::clone(&cell)));
                Ok(cell)
            }
            None => Err(Stop::NotYet("read of an unbound local")),
        }
    }

    /// Creates a new variable for a binding: a `let`, a parameter or a pattern
    /// variable.
    pub(crate) fn bind(&mut self, slot: LocalId, value: Value) -> Result<(), Stop> {
        match self.slots.get_mut(slot.index()) {
            Some(entry) => {
                *entry = Some(Variable::Value(value));
                Ok(())
            }
            None => Err(Stop::NotYet("local slot outside the frame")),
        }
    }

    /// Replaces the value of an existing variable, seen by every closure that
    /// shares its slot.
    pub(crate) fn assign(&mut self, slot: LocalId, value: Value) -> Result<(), Stop> {
        match self.slots.get_mut(slot.index()) {
            Some(Some(Variable::Value(current))) => {
                *current = value;
                Ok(())
            }
            Some(Some(Variable::Shared(cell))) => {
                *cell.borrow_mut() = value;
                Ok(())
            }
            _ => Err(Stop::NotYet("assignment to an unbound local")),
        }
    }

    /// Makes a capture slot refer to the variable the closure captured.
    pub(crate) fn share(&mut self, slot: LocalId, cell: Cell) -> Result<(), Stop> {
        match self.slots.get_mut(slot.index()) {
            Some(entry) => {
                *entry = Some(Variable::Shared(cell));
                Ok(())
            }
            None => Err(Stop::NotYet("local slot outside the frame")),
        }
    }
}
