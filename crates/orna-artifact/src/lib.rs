//! Canonical executable artifacts for OrnaDB.
//!
//! Artifact bytes are independent of the compiler, source syntax, and storage
//! backend. Each artifact decoder validates its complete input before it
//! returns an executable representation.

pub mod constant_expression;
pub mod server_mutation_plan;
pub mod server_plan;
