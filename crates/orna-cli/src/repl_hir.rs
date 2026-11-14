//! `ORNA_REPL_HIR=1`: the `orna repl` console served by the `orna-repl` session.
//!
//! The default console runs the project-backed admitted session. This driver
//! serves the same line protocol from `orna_repl::Session`, for a REPL outside
//! any Git worktree. A REPL bound to a repository needs host state this driver
//! does not own yet: stage and commit against the project's tables, `:at`
//! snapshots and `:watch` sources. Such a REPL fails closed rather than
//! answering from a session that lacks that state.

use std::io::{self, BufRead, BufReader, Write};

use crate::repl::{
    AnsiColor, ReadSubmission, parse_help_command, parse_snapshot_target, parse_watch_source,
    read_submission, write_rendered_error, write_rendered_value, write_repl_error, write_styled,
};
use crate::repl_run::repl_failure_diagnostic;
use crate::repl_session::repl_repository;
use crate::{ColorMode, Diagnostic, Endpoint};

const ENV: &str = "ORNA_REPL_HIR";

const WATCH_UNSUPPORTED: &str = "error[ORNA-REPL-HIR-UNSUPPORTED]: `:watch` needs the project REPL\nhelp: unset ORNA_REPL_HIR to use live watches";

/// Whether `ORNA_REPL_HIR=1` selects this driver. Any other value keeps the project REPL.
pub(crate) fn enabled() -> bool {
    std::env::var_os(ENV).is_some_and(|value| value == "1")
}

pub(crate) fn run_repl_hir(
    endpoint: &Endpoint,
    expression: Option<&str>,
    color_mode: ColorMode,
) -> Result<(), Diagnostic> {
    if repl_repository(endpoint)?.is_some() {
        return Err(Diagnostic::target(
            "E2100",
            "ORNA_REPL_HIR serves only a REPL outside a Git worktree",
            "run `orna repl` outside the project worktree, or unset ORNA_REPL_HIR for the project REPL",
        ));
    }
    let color_enabled = color_mode.stdout_enabled();
    let mut session = orna_repl::Session::new();
    if let Some(source) = expression {
        return run_expression(
            &mut session,
            source,
            &mut io::stdout().lock(),
            color_enabled,
        );
    }
    run_console(
        &mut BufReader::new(io::stdin().lock()),
        &mut io::stdout().lock(),
        &mut session,
        color_enabled,
    )
    .map_err(|_| console_failed())
}

fn console_failed() -> Diagnostic {
    Diagnostic::target(
        "E2200",
        "REPL console I/O failed",
        "check the terminal input and output streams, then start a new session",
    )
}

fn run_expression<W: Write>(
    session: &mut orna_repl::Session,
    source: &str,
    writer: &mut W,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    if source.trim() == ":quit" {
        return Ok(());
    }
    match orna_repl::submit(session, source) {
        Ok(Some(value)) => {
            write_rendered_value(writer, &orna_repl::render_result(&value), color_enabled)
                .map_err(|_| console_failed())
        }
        Ok(None) => Ok(()),
        Err(error) => Err(repl_failure_diagnostic(error.code(), None)),
    }
}

fn run_console<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    session: &mut orna_repl::Session,
    color_enabled: bool,
) -> io::Result<()> {
    loop {
        write_styled(writer, AnsiColor::Dim, b"> ", color_enabled)?;
        writer.flush()?;
        let source = match read_submission(reader)? {
            ReadSubmission::Eof => return Ok(()),
            ReadSubmission::TooLong => {
                write_repl_error(writer, "ORNA-REPL-INPUT-LIMIT", color_enabled)?;
                continue;
            }
            ReadSubmission::InvalidUtf8 => {
                write_repl_error(writer, "ORNA-REPL-INPUT-UTF8", color_enabled)?;
                continue;
            }
            ReadSubmission::Source(source) => source,
        };
        let command = source.trim();
        if command == ":quit" {
            return Ok(());
        }
        if let Some(help) = parse_help_command(command) {
            match help {
                Ok(text) => {
                    write_styled(writer, AnsiColor::Cyan, text.as_bytes(), color_enabled)?;
                    writer.write_all(b"\n")?;
                }
                Err(()) => write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?,
            }
        } else if parse_snapshot_target(command).is_some() {
            // Snapshots load from a repository, and this console has none.
            write_repl_error(writer, "ORNA-REPL-AT", color_enabled)?;
        } else if parse_watch_source(command).is_some() {
            write_rendered_error(writer, WATCH_UNSUPPORTED, color_enabled)?;
        } else if source.trim_start().starts_with(':') {
            write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?;
        } else {
            match orna_repl::submit(session, &source) {
                Ok(Some(value)) => {
                    write_rendered_value(writer, &orna_repl::render_result(&value), color_enabled)?;
                }
                Ok(None) => {}
                Err(error) => write_repl_error(writer, error.code(), color_enabled)?,
            }
        }
    }
}
