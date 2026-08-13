//! Temporary compatibility package for the linked PostgreSQL engine.

#[cfg(feature = "embedded")]
#[path = "../../orna-postgres/src/engine.rs"]
mod engine;

#[cfg(feature = "embedded")]
pub use engine::*;
