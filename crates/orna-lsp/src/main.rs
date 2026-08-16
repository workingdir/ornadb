//! The Orna language server binary entry point.
//!
//! The server speaks the Language Server Protocol over standard input and
//! output. Editor integrations launch this binary and communicate through
//! framed JSON-RPC messages.

use std::process::ExitCode;

fn main() -> ExitCode {
    match orna_lsp::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orna-lsp: {error}");
            ExitCode::FAILURE
        }
    }
}
