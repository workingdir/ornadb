//! The local slots of one active body.
//!
//! A slot is a [`Cell`]: the variable itself, shared by every closure that
//! captured it (`docs/language/reference/03-bindings-and-expressions.md`,
//! "Closures share the slot"). A binding creates a fresh cell; an assignment
//! writes the cell the name already refers to.

use std::cell::RefCell;
use std::rc::Rc;

use orna_hir::{Idx, LocalId};

use crate::error::Stop;
use crate::value::{Cell, Value};

/// One call's local slots, indexed by [`LocalId`].
#[derive(Debug)]
pub(crate) struct Frame {
    slots: Vec<Option<Cell>>,
}

impl Frame {
    /// A frame with `count` empty slots.
    pub(crate) fn new(count: usize) -> Self {
        Self {
            slots: vec![None; count],
        }
    }

    /// Reads a bound slot.
    pub(crate) fn get(&self, slot: LocalId) -> Result<Value, Stop> {
        let cell = self.cell(slot)?;
        let value = cell.borrow().clone();
        Ok(value)
    }

    /// The variable in a bound slot, for a closure to capture.
    pub(crate) fn cell(&self, slot: LocalId) -> Result<Cell, Stop> {
        match self.slots.get(slot.index()) {
            Some(Some(cell)) => Ok(Rc::clone(cell)),
            _ => Err(Stop::NotYet("read of an unbound local")),
        }
    }

    /// Creates a new variable for a binding: a `let`, a parameter or a pattern
    /// variable. Each execution of a binding gets its own slot, so a closure
    /// that captured an earlier one keeps it.
    pub(crate) fn bind(&mut self, slot: LocalId, value: Value) -> Result<(), Stop> {
        match self.slots.get_mut(slot.index()) {
            Some(cell) => {
                *cell = Some(Rc::new(RefCell::new(value)));
                Ok(())
            }
            None => Err(Stop::NotYet("local slot outside the frame")),
        }
    }

    /// Replaces the value of an existing variable, seen by every closure that
    /// shares its slot.
    pub(crate) fn assign(&mut self, slot: LocalId, value: Value) -> Result<(), Stop> {
        match self.slots.get(slot.index()) {
            Some(Some(cell)) => {
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
                *entry = Some(cell);
                Ok(())
            }
            None => Err(Stop::NotYet("local slot outside the frame")),
        }
    }
}
