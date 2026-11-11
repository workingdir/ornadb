//! The stdin driver: one submission per non-empty line, run in one session.
//!
//! Results go to the output writer in the scalar format of [`crate::render`]; a rejected submission writes
//! `error[<code>]: <message>` to the error writer and does not stop the run.

use std::io::{self, BufRead, Write};

use orna_exec::Value;

use crate::{ReplError, Session, render};

/// Stages and commits one submission. A rejected stage changes nothing in the session.
///
/// # Errors
/// Returns the rejection of the stage or of the commit.
pub fn submit(session: &mut Session, input: &str) -> Result<Option<Value>, ReplError> {
    let staged = session.stage(input)?;
    session.commit(staged)
}

/// Runs every non-empty line of `input` as one submission in a fresh session.
///
/// Returns `true` when every submission was accepted.
///
/// # Errors
/// Returns an I/O error from reading `input` or writing either writer.
pub fn run_lines<R: BufRead, O: Write, E: Write>(input: R, out: &mut O, err: &mut E) -> io::Result<bool> {
    let mut session = Session::new();
    let mut all_accepted = true;
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match submit(&mut session, &line) {
            Ok(Some(value)) => writeln!(out, "{}", render::result(&value))?,
            Ok(None) => {}
            Err(error) => {
                all_accepted = false;
                writeln!(err, "error[{}]: {}", error.code(), error.message())?;
            }
        }
    }
    Ok(all_accepted)
}
