//! Private PostgreSQL implementation for OrnaDB.
//!
//! The linked engine and private SQL kernel are internal modules of one owned
//! PostgreSQL implementation.

include!("kernel.rs");

#[cfg(feature = "embedded")]
mod engine;

#[cfg(feature = "embedded")]
pub use engine::*;
