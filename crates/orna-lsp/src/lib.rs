//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: diagnostics,
//! document symbols and formatting, semantic highlighting, hover, definition,
//! references, rename and completion. Documents inside an Orna project get the
//! same diagnostics `orna check` reports for that project, computed from the
//! open editor buffers; other documents get syntax diagnostics. The server
//! needs no running database. It writes only read-only copies of standard
//! library sources below the user cache directory, for go-to-definition.

mod analysis;
mod diagnostics;
mod documents;
mod editor_ranges;
mod formatting;
mod hierarchy;
mod hover;
mod inlay;
mod names;
mod project;
mod rename;
mod requests;
mod semantic;
mod standard;
mod server;
mod views;
mod workspace;

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
