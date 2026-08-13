//! Private PostgreSQL implementation for OrnaDB.
//!
//! During the package consolidation this crate forwards the existing linked
//! engine and SQL kernel interfaces without changing their behaviour.

pub use orna_kernel_postgres::*;

#[cfg(feature = "embedded")]
pub use orna_postgres_engine::*;
