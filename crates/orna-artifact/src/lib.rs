//! Canonical executable artifacts for OrnaDB.
//!
//! Artifact bytes are independent of the compiler, source syntax, and storage
//! backend. Each artifact decoder validates its complete input before it
//! returns an executable representation.

mod artifact_codec;
pub mod client_plan;
pub mod constant_expression;
mod parameter_artifact;
pub mod server_csv_encode;
pub mod server_json_encode;
pub mod server_mutation_plan;
pub mod server_parameter_echo;
pub mod server_plan;
pub mod server_terminal_table;
