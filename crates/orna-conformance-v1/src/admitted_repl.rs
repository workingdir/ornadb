//! Compatibility export for the production typed REPL boundary.
//!
//! Conformance owns corpus orchestration and adapter evidence. Source
//! admission and execution live in `orna-evaluator-v1` so production hosts do
//! not depend on the conformance harness.

pub use orna_evaluator_v1::{AdmittedReplSession, ReplError};
