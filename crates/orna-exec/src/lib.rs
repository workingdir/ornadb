//! `orna-exec`: the executor over the typed HIR (stage H4 of
//! `docs/design/hir.md`).
//!
//! The executor walks a [`orna_hir::Program`] directly. It is additive: the
//! old pipeline (`orna-check`, `orna-evaluator`) stays authoritative, and the
//! differential harness compares the two. A construct this stage does not run
//! yet is reported as [`ExecError::NotYet`] with the construct named, never
//! guessed at, so the harness can count what has been executed.

mod binders;
mod error;
mod eval;
mod frame;
mod machine;
mod ops;
mod value;
mod variants;

pub use error::ExecError;
pub use machine::{ExecLimits, Measured, Run, measure_entry, run_entry, run_entry_cancellable};
pub use value::Value;
