// Backend shell operations return the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
//! The host-only native PostgreSQL escape hatch for the local server.

use std::{
    fmt,
    io::{self, BufRead, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use crate::{EmbeddedHostError, inspect_current_embedded_host};

#[path = "backend_protocol.rs"]
mod backend_protocol;

use backend_protocol::BackendSession;

const EMPTY_PROMPT: &str = "orna=> ";
const CONTINUED_PROMPT: &str = "orna-> ";
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// A failure that prevents or ends the host-only backend shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendShellError {
    /// One or more standard streams are not interactive terminals.
    TerminalRequired,
    /// The default local instance has not been started.
    InstanceNotInstalled,
    /// The local instance or its readiness evidence is inconsistent.
    InstanceInvalid,
    /// The running executable cannot supply the instance's embedded engine.
    EngineInvalid,
    /// The fixed private Unix-socket connection could not be established.
    AttachFailed,
    /// Terminal or PostgreSQL protocol handling failed after attachment.
    SessionFailed,
}

impl fmt::Display for BackendShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TerminalRequired => "orna: backend-shell must be run in an interactive terminal",
            Self::InstanceNotInstalled => "orna: the default Orna instance is not installed",
            Self::InstanceInvalid => "orna: the default Orna instance is invalid",
            Self::EngineInvalid => "orna: the embedded PostgreSQL engine is not valid",
            Self::AttachFailed => "orna: could not attach the backend shell",
            Self::SessionFailed => "orna: backend-shell session failed",
        })
    }
}

impl std::error::Error for BackendShellError {}

/// Attaches a native simple-query terminal to the running embedded PostgreSQL host.
pub fn run_backend_shell() -> Result<(), BackendShellError> {
    if !terminals_are_interactive() {
        return Err(BackendShellError::TerminalRequired);
    }
    let host = inspect_current_embedded_host().map_err(map_host_error)?;
    let _interrupt_handler = InterruptHandler::install()?;
    let mut session = BackendSession::connect(host.config())?;
    terminal_loop(&mut session)
}

fn terminals_are_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
}

fn map_host_error(error: EmbeddedHostError) -> BackendShellError {
    match error {
        EmbeddedHostError::Io(source) if source.kind() == io::ErrorKind::NotFound => {
            BackendShellError::InstanceNotInstalled
        }
        _ => BackendShellError::InstanceInvalid,
    }
}

fn terminal_loop(session: &mut BackendSession) -> Result<(), BackendShellError> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut buffer = String::new();
    let mut line = String::new();

    loop {
        output
            .write_all(if buffer.is_empty() {
                EMPTY_PROMPT.as_bytes()
            } else {
                CONTINUED_PROMPT.as_bytes()
            })
            .and_then(|()| output.flush())
            .map_err(|_| BackendShellError::SessionFailed)?;
        line.clear();
        let length = match input.read_line(&mut line) {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                let _ = interrupt_requested();
                buffer.clear();
                output
                    .write_all(b"\n")
                    .map_err(|_| BackendShellError::SessionFailed)?;
                continue;
            }
            Err(_) => return Err(BackendShellError::SessionFailed),
        };
        if interrupt_requested() {
            buffer.clear();
            output
                .write_all(b"\n")
                .map_err(|_| BackendShellError::SessionFailed)?;
            continue;
        }
        if length == 0 {
            return if buffer.is_empty() {
                session.terminate()
            } else {
                Err(BackendShellError::SessionFailed)
            };
        }
        let control = line
            .strip_suffix('\n')
            .and_then(|line| line.strip_suffix('\r').or(Some(line)));
        match control {
            Some("\\q") => return session.terminate(),
            Some("\\g") if buffer.is_empty() => continue,
            Some("\\g") => {
                session.execute(&buffer, &mut input, &mut output)?;
                buffer.clear();
            }
            _ => buffer.push_str(&line),
        }
    }
}

struct InterruptHandler {
    previous: SigAction,
}

impl InterruptHandler {
    fn install() -> Result<Self, BackendShellError> {
        INTERRUPTED.store(false, Ordering::SeqCst);
        let action = SigAction::new(
            SigHandler::Handler(record_interrupt),
            SaFlags::empty(),
            SigSet::empty(),
        );
        // SAFETY: the handler performs only one lock-free atomic store.
        let previous = unsafe { sigaction(Signal::SIGINT, &action) }
            .map_err(|_| BackendShellError::SessionFailed)?;
        Ok(Self { previous })
    }
}

impl Drop for InterruptHandler {
    fn drop(&mut self) {
        // SAFETY: this restores the action returned by the successful installation.
        let _ = unsafe { sigaction(Signal::SIGINT, &self.previous) };
    }
}

extern "C" fn record_interrupt(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub(super) fn interrupt_requested() -> bool {
    INTERRUPTED.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_have_the_exact_human_readable_lines() {
        let cases = [
            (
                BackendShellError::TerminalRequired,
                "orna: backend-shell must be run in an interactive terminal",
            ),
            (
                BackendShellError::InstanceNotInstalled,
                "orna: the default Orna instance is not installed",
            ),
            (
                BackendShellError::InstanceInvalid,
                "orna: the default Orna instance is invalid",
            ),
            (
                BackendShellError::EngineInvalid,
                "orna: the embedded PostgreSQL engine is not valid",
            ),
            (
                BackendShellError::AttachFailed,
                "orna: could not attach the backend shell",
            ),
            (
                BackendShellError::SessionFailed,
                "orna: backend-shell session failed",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn terminal_escaping_is_unambiguous() {
        let mut output = Vec::new();
        backend_protocol::write_escaped("a\\b\t\r\n\u{1b}\u{7f}\0é", &mut output).unwrap();
        assert_eq!(output, b"a\\\\b\\t\\r\\n\\e\\x7f\\u{0}\xc3\xa9");
    }
}
