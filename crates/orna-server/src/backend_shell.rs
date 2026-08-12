//! The host-only native PostgreSQL escape hatch.

use std::{
    fmt,
    io::{self, BufRead, IsTerminal, Write},
};

use tokio_postgres::{NoTls, SimpleQueryMessage};

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

const EMPTY_PROMPT: &str = "orna=> ";
const CONTINUED_PROMPT: &str = "orna-> ";

/// A failure that prevents or ends the host-only backend shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendShellError {
    TerminalRequired,
    ServiceAccountRequired,
    PackageIncomplete,
    InstanceNotInstalled,
    InstanceInvalid,
    EngineInvalid,
    AttachFailed,
    SessionFailed,
}

impl fmt::Display for BackendShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TerminalRequired => "orna: backend-shell must be run in an interactive terminal",
            Self::ServiceAccountRequired => {
                "orna: backend-shell must run as the orna service account"
            }
            Self::PackageIncomplete => "orna: package maintenance is incomplete",
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
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| BackendShellError::SessionFailed)?;
    runtime.block_on(run_session(host.config().clone()))
}

fn terminals_are_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
}

fn map_host_error(error: EmbeddedHostError) -> BackendShellError {
    match error {
        EmbeddedHostError::InvalidServiceIdentity => BackendShellError::ServiceAccountRequired,
        EmbeddedHostError::InvalidPackageState => BackendShellError::PackageIncomplete,
        EmbeddedHostError::Engine(_) => BackendShellError::EngineInvalid,
        EmbeddedHostError::Io(ref source) if source.kind() == io::ErrorKind::NotFound => {
            BackendShellError::InstanceNotInstalled
        }
        _ => BackendShellError::InstanceInvalid,
    }
}

async fn run_session(config: tokio_postgres::Config) -> Result<(), BackendShellError> {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .map_err(|_| BackendShellError::AttachFailed)?;
    let driver = tokio::spawn(connection);
    let session_result = terminal_loop(&client).await;
    drop(client);
    let driver_result = driver.await;
    session_result?;
    match driver_result {
        Ok(Ok(())) => Ok(()),
        _ => Err(BackendShellError::SessionFailed),
    }
}

async fn terminal_loop(client: &tokio_postgres::Client) -> Result<(), BackendShellError> {
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
                buffer.clear();
                output
                    .write_all(b"\n")
                    .map_err(|_| BackendShellError::SessionFailed)?;
                continue;
            }
            Err(_) => return Err(BackendShellError::SessionFailed),
        };
        if length == 0 {
            return if buffer.is_empty() {
                Ok(())
            } else {
                Err(BackendShellError::SessionFailed)
            };
        }
        let control = line
            .strip_suffix('\n')
            .and_then(|line| line.strip_suffix('\r').or(Some(line)));
        match control {
            Some("\\q") => return Ok(()),
            Some("\\g") if buffer.is_empty() => continue,
            Some("\\g") => {
                render_query(client, &buffer, &mut output).await?;
                buffer.clear();
            }
            _ => buffer.push_str(&line),
        }
    }
}

async fn render_query(
    client: &tokio_postgres::Client,
    query: &str,
    output: &mut impl Write,
) -> Result<(), BackendShellError> {
    match client.simple_query(query).await {
        Ok(messages) => {
            for message in messages {
                match message {
                    SimpleQueryMessage::RowDescription(columns) => {
                        render_fields(columns.iter().map(|column| Some(column.name())), output)?;
                    }
                    SimpleQueryMessage::Row(row) => {
                        render_fields((0..row.len()).map(|index| row.get(index)), output)?;
                    }
                    SimpleQueryMessage::CommandComplete(rows) => {
                        writeln!(output, "({rows} rows)")
                            .map_err(|_| BackendShellError::SessionFailed)?;
                    }
                    _ => return Err(BackendShellError::SessionFailed),
                }
            }
        }
        Err(error) => render_database_error(&error, output)?,
    }
    output.flush().map_err(|_| BackendShellError::SessionFailed)
}

fn render_fields<'a>(
    fields: impl IntoIterator<Item = Option<&'a str>>,
    output: &mut impl Write,
) -> Result<(), BackendShellError> {
    let mut separator = "";
    for field in fields {
        output
            .write_all(separator.as_bytes())
            .map_err(|_| BackendShellError::SessionFailed)?;
        match field {
            Some(value) => write_escaped(value, output)?,
            None => output
                .write_all(b"<NULL>")
                .map_err(|_| BackendShellError::SessionFailed)?,
        }
        separator = "\t";
    }
    output
        .write_all(b"\n")
        .map_err(|_| BackendShellError::SessionFailed)
}

fn render_database_error(
    error: &tokio_postgres::Error,
    output: &mut impl Write,
) -> Result<(), BackendShellError> {
    let Some(error) = error.as_db_error() else {
        return Err(BackendShellError::SessionFailed);
    };
    write!(output, "ERROR {}: ", error.code().code())
        .map_err(|_| BackendShellError::SessionFailed)?;
    write_escaped(error.message(), output)?;
    output
        .write_all(b"\n")
        .map_err(|_| BackendShellError::SessionFailed)?;
    for (label, value) in [("DETAIL", error.detail()), ("HINT", error.hint())] {
        if let Some(value) = value {
            write!(output, "{label}: ").map_err(|_| BackendShellError::SessionFailed)?;
            write_escaped(value, output)?;
            output
                .write_all(b"\n")
                .map_err(|_| BackendShellError::SessionFailed)?;
        }
    }
    Ok(())
}

fn write_escaped(value: &str, output: &mut impl Write) -> Result<(), BackendShellError> {
    for character in value.chars() {
        match character {
            '\\' => output.write_all(b"\\\\"),
            '\t' => output.write_all(b"\\t"),
            '\r' => output.write_all(b"\\r"),
            '\n' => output.write_all(b"\\n"),
            '\u{1b}' => output.write_all(b"\\e"),
            '\u{7f}' => output.write_all(b"\\x7f"),
            character if character.is_control() => {
                write!(output, "\\u{{{:x}}}", character as u32)
            }
            character => write!(output, "{character}"),
        }
        .map_err(|_| BackendShellError::SessionFailed)?;
    }
    Ok(())
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
                BackendShellError::ServiceAccountRequired,
                "orna: backend-shell must run as the orna service account",
            ),
            (
                BackendShellError::PackageIncomplete,
                "orna: package maintenance is incomplete",
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
        write_escaped("a\\b\t\r\n\u{1b}\u{7f}\0é", &mut output).unwrap();
        assert_eq!(output, b"a\\\\b\\t\\r\\n\\e\\x7f\\u{0}\xc3\xa9");
    }
}
