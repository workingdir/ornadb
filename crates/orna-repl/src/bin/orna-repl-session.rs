//! Reads submissions from stdin, one per non-empty line, and runs them in one session.
//!
//! Exit status: 0 when every submission is accepted, 1 when any is rejected, 2 on an I/O failure.

#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    let mut err = io::stderr().lock();
    match orna_repl::run_lines(stdin.lock(), &mut out, &mut err) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            let _ = writeln!(err, "orna-repl-session: {error}");
            ExitCode::from(2)
        }
    }
}
