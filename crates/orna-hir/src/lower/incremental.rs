//! Incremental lowering: a program grows one batch of units at a time. Each batch
//! declares its modules and items, then lowers only the items not yet lowered.
//! Definitions are numbered in declaration order, so a single batch lowers to the
//! same program as a batch of many.

use super::declare::Builder;
use super::{PreludeExports, SourceUnit};
use crate::Program;

/// A program being lowered. [`Lowering::extend`] adds units and lowers only the
/// items they declare, so a lowered item is never lowered again.
#[derive(Clone)]
pub struct Lowering {
    builder: Builder,
}

impl Lowering {
    /// Starts a lowering that holds the engine-embedded `core` and `sys` modules (D12).
    /// Their items are declared here and lowered by the first [`Lowering::extend`] or
    /// [`Lowering::into_program`].
    #[must_use]
    pub fn new(prelude: &PreludeExports) -> Self {
        let embedded: Vec<SourceUnit<'_>> = crate::embedded::parsed()
            .iter()
            .enumerate()
            .map(|(index, module)| SourceUnit {
                source: crate::embedded::source_id(index),
                tree: &module.tree,
                path: module.path.clone(),
            })
            .collect();
        let mut builder = Builder::new(prelude);
        builder.declare_units(&embedded);
        Self { builder }
    }

    /// Adds `units`: declares their modules and items, then lowers every item not
    /// yet lowered.
    pub fn extend(&mut self, units: &[SourceUnit<'_>]) {
        self.builder.declare_units(units);
        self.builder.resolve_imports();
        self.builder.lower_items();
    }

    /// The number of items lowered so far, across every [`Lowering::extend`].
    #[must_use]
    pub fn lowered_items(&self) -> usize {
        self.builder.lowered
    }

    /// The finished program as it stands now. This lowering is unchanged, so more
    /// units can still be added to it.
    #[must_use]
    pub fn snapshot(&self) -> Program {
        self.clone().into_program()
    }

    /// Lowers whatever is still pending, then runs the passes that complete the program.
    #[must_use]
    pub fn into_program(mut self) -> Program {
        self.builder.resolve_imports();
        self.builder.lower_items();
        let mut program = self.builder.finish();
        crate::units::eval::run(&mut program);
        crate::effect_pass::run(&mut program);
        crate::rules::run(&mut program);
        program
    }
}
