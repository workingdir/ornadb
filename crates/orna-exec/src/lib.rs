//! `orna-exec`: the executor over the typed HIR (stage H4 of
//! `docs/design/hir.md`).
//!
//! The executor walks a [`orna_hir::Program`] directly. It is additive: the
//! old pipeline (`orna-check`, `orna-evaluator`) stays authoritative, and the
//! differential harness compares the two. A construct this stage does not run
//! yet is reported as [`ExecError::NotYet`] with the construct named, never
//! guessed at, so the harness can count what has been executed.

mod binders;
mod decimal;
mod dispatch;
mod engine_order;
mod error;
mod eval;
mod float;
mod frame;
mod link;
mod machine;
mod natives;
mod ops;
mod raw_value;
mod relation;
mod store;
mod table;
mod time;
mod unit_variant;
mod value;
mod variants;

pub use error::ExecError;
pub use machine::{
    ExecLimits, Measured, Run, measure_entry, measure_entry_with_store, run_entry,
    run_entry_cancellable,
};
pub use store::{
    CursorId, Direction, Expect, KeyBounds, MemoryStore, RowRef, StoreError, TableId, TableStore,
};
pub use value::Value;
