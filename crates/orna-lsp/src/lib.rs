//! The Orna language server.
//!
//! `orna-lsp` provides editor features for `.orna` source files: diagnostics,
//! document symbols and formatting, semantic highlighting, hover, definition,
//! references, rename and completion. Documents inside an Orna project get the
//! same diagnostics `orna check` reports for that project, computed from the
//! open editor buffers; other documents get syntax diagnostics. The server
//! needs no running database. It writes only read-only copies of standard
//! library sources below the user cache directory, for go-to-definition.

// `lsp_types::Uri` wraps `fluent_uri::Uri<String>`, which keeps its parse tags
// behind interior mutability. Clippy therefore reports every URI-keyed map in
// this crate as a mutable key type. The URI hashes and compares by its string
// value only, and the server never mutates a key in place, so the allow covers
// the dependency's type rather than a decision made here.
#![allow(clippy::mutable_key_type)]

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
mod server;
mod standard;
mod views;
mod workspace;

/// Runs the server until the client exits.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
