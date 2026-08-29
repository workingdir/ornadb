//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: compiler
//! diagnostics, document symbols, semantic highlighting, hover, definition,
//! references, and completion. It reuses the offline Orna compiler, so it
//! needs no running database and never writes to disk.

mod analysis;
mod documents;
mod function;
mod hover;
mod reference;
mod semantic;
mod server;

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
