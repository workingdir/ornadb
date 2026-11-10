//! The local slots of one active body.

use orna_hir::{Idx, LocalId};

use crate::error::Stop;
use crate::value::Value;

/// One call's local slots, indexed by [`LocalId`].
#[derive(Debug)]
pub(crate) struct Frame {
    slots: Vec<Option<Value>>,
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
        match self.slots.get(slot.index()) {
            Some(Some(value)) => Ok(value.clone()),
            _ => Err(Stop::NotYet("read of an unbound local")),
        }
    }

    /// Binds or rebinds a slot.
    pub(crate) fn set(&mut self, slot: LocalId, value: Value) -> Result<(), Stop> {
        match self.slots.get_mut(slot.index()) {
            Some(cell) => {
                *cell = Some(value);
                Ok(())
            }
            None => Err(Stop::NotYet("local slot outside the frame")),
        }
    }
}
