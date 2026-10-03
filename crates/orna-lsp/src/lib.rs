//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: syntax-v1
//! diagnostics, document symbols, semantic highlighting, hover, definition,
//! references, and completion. It derives analysis from the frozen Orna 1.0
//! syntax frontend, so it needs no running database and never writes to disk.

mod analysis;
mod documents;
mod hover;
mod semantic;
mod server;

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
