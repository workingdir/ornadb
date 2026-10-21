//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: diagnostics,
//! document symbols and formatting, semantic highlighting, hover, definition,
//! references, rename and completion. Documents inside an Orna project get the
//! same diagnostics `orna check` reports for that project, computed from the
//! open editor buffers; other documents get syntax diagnostics. The server
//! needs no running database.

mod analysis;
mod diagnostics;
mod documents;
mod editor_ranges;
mod formatting;
mod hover;
mod inlay;
mod locals;
mod project;
mod semantic;
mod server;

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
