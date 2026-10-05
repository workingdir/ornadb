//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: compiler
//! diagnostics, document symbols and formatting, semantic highlighting,
//! hover, definition, references, and completion. It derives analysis from
//! the frozen Orna 1.0 syntax frontend, including semantic tokens and inlay
//! hints, so it needs no running database and never writes to disk.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

mod analysis;
pub mod browser;
mod documents;
mod editor_ranges;
#[cfg(not(target_arch = "wasm32"))]
mod formatting;
mod hover;
mod inlay;
mod locals;
mod semantic;
#[cfg(not(target_arch = "wasm32"))]
mod server;

/// Runs the server until the client exits.
#[cfg(not(target_arch = "wasm32"))]
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
