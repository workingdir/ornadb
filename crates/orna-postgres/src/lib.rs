//! Private PostgreSQL implementation for OrnaDB.
//!
//! The linked engine and private SQL kernel are internal modules of one owned
//! PostgreSQL implementation.

mod kernel;
mod storage;

pub use kernel::*;

pub(crate) use kernel::{
    bootstrap, decode, physical, recovery, server_execution, server_runtime, storage,
};

#[cfg(feature = "embedded")]
mod engine;

#[cfg(feature = "embedded")]
pub use engine::*;
