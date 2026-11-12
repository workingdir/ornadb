//! The REPL session core: an incremental Orna module over the HIR and the executor.
//!
//! [`Session::stage`] lowers one input against the session's accumulated source, typechecks it and, for an
//! expression or a `let`, evaluates it. Nothing changes until [`Session::commit`] applies the staged input.
//! [`Session::rollback`] discards a staged input and leaves the session as it was.
//!
//! The standard library is lowered once per process. Each stage lowers only the session module over a copy of it,
//! and the session keeps its accumulated definitions as source (see `base`). Redefinition, shadowing and `$_` follow
//! the old REPL's rules; `tests/session.rs` pins them.
//!
//! [`run_lines`] drives a session from a reader, one submission per non-empty line; the `orna-repl-session`
//! example is its stdin front end.

#![forbid(unsafe_code)]

mod base;
mod check;
mod compose;
mod driver;
mod error;
mod render;
mod session;
mod standard;

pub use driver::{run_lines, submit};
pub use error::ReplError;
pub use session::{Session, Staged};
