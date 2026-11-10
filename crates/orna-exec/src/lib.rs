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

pub use error::ExecError;
pub use machine::{ExecLimits, run_entry};
pub use value::Value;
