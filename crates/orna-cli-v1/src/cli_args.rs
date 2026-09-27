//! Argument grammar and the typed command model for the bounded CLI.
//!
//! Parsing lives apart from execution so callers can inspect a command without
//! opening a repository or starting a runtime.

use std::path::PathBuf;

use super::{ColorMode, Diagnostic, Endpoint};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Invocation {
    Seed,
    Exercise,
    SensorsIngest,
    LibraryLend { book_id: String, borrower: String },
    ProjectFunction(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusFormat {
    Human,
    Porcelain,
    Short,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Command {
    Repl(Option<String>),
    Init(Option<PathBuf>),
    Status { format: StatusFormat },
    Fetch { remote: String, branch: String },
    Check,
    Explain(String),
    Invoke(String),
    Run(Invocation),
    Help,
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Parsed {
    pub(super) endpoint: Endpoint,
    pub(super) command: Command,
    pub(super) color: ColorMode,
}

pub(super) fn requested_color_mode(arguments: &[String]) -> ColorMode {
    let mut color = ColorMode::Auto;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while let Some(option) = words.peek().copied() {
        match option {
            "--db" => {
                words.next();
                if words.next().is_none() {
                    break;
                }
            }
            "--color" => {
                words.next();
                let Some(value) = words.next() else {
                    break;
                };
                let Some(mode) = ColorMode::parse(value) else {
                    break;
                };
                color = mode;
            }
            _ => break,
        }
    }
    color
}

#[allow(
    clippy::too_many_lines,
    reason = "the command grammar deliberately keeps validation precedence in one auditable parser"
)]
pub(super) fn parse_cli(arguments: &[String]) -> Result<Parsed, Diagnostic> {
    let mut endpoint = Endpoint::ManagedLocal;
    let mut has_explicit_endpoint = false;
    let mut color = ColorMode::Auto;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while matches!(words.peek(), Some(&"--db") | Some(&"--color")) {
        match words.next() {
            Some("--db") => {
                has_explicit_endpoint = true;
                endpoint = Endpoint::parse(words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "option `--db` needs a value",
                        "supply an endpoint after `--db`",
                    )
                })?)?;
            }
            Some("--color") => {
                let value = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "option `--color` needs a value",
                        "choose `auto`, `always`, or `never` after `--color`",
                    )
                })?;
                color = ColorMode::parse(value).ok_or_else(|| {
                    Diagnostic::usage(
                        "E1002",
                        "unsupported value for `--color`",
                        "choose `auto`, `always`, or `never`",
                    )
                })?;
            }
            _ => unreachable!("only global options enter this loop"),
        }
    }
    let command = match words.next() {
        None => Command::Repl(None),
        Some("repl") => Command::Repl(words.next().map(str::to_owned)),
        Some("init") => {
            let first = words.next();
            let literal_target = first == Some("--");
            let target = if literal_target { words.next() } else { first }.map(PathBuf::from);
            if !literal_target
                && let Some(target) = target.as_deref()
                && target.as_os_str().to_string_lossy().starts_with('-')
            {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`init` option is not supported",
                    "use `init` or `init DIRECTORY` without Git options",
                ));
            }
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1003",
                    "`init` accepts at most one directory",
                    "use `init` or `init DIRECTORY`",
                ));
            }
            if has_explicit_endpoint {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`init` does not accept `--db`",
                    "use `init` or `init DIRECTORY` to choose a local repository",
                ));
            }
            Command::Init(target)
        }
        Some("status") => match words.next() {
            Some("--porcelain") => Command::Status {
                format: StatusFormat::Porcelain,
            },
            Some("--short") => Command::Status {
                format: StatusFormat::Short,
            },
            Some(_) => {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`status` supports no option, `--porcelain`, or `--short`",
                    "use `status`, `status --porcelain`, or `status --short`",
                ));
            }
            None => Command::Status {
                format: StatusFormat::Human,
            },
        },
        Some("fetch") => Command::Fetch {
            remote: words.next().unwrap_or("origin").to_owned(),
            branch: words.next().unwrap_or("main").to_owned(),
        },
        Some("explain") => Command::Explain(
            words
                .next()
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`explain` needs a diagnostic code",
                        "supply a diagnostic code after `explain`, such as `ORNA-S010-IMPORT`",
                    )
                })?
                .to_owned(),
        ),
        Some("check") => Command::Check,
        Some("invoke") => Command::Invoke(
            words
                .next()
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`invoke` needs a function target",
                        "supply a reachable zero-argument pure function name after `invoke`",
                    )
                })?
                .to_owned(),
        ),
        Some("--help" | "-h" | "help") => Command::Help,
        Some("--version" | "-V") => Command::Version,
        Some("run") => match words.next() {
            Some("seed") => Command::Run(Invocation::Seed),
            Some("exercise") => Command::Run(Invocation::Exercise),
            Some("sensors.ingest") => Command::Run(Invocation::SensorsIngest),
            Some("library.lend") => {
                let book_id = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`run library.lend` needs a book ID",
                        "supply the book ID and borrower after `run library.lend`",
                    )
                })?;
                let borrower = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`run library.lend` needs a borrower",
                        "supply the book ID and borrower after `run library.lend`",
                    )
                })?;
                Command::Run(Invocation::LibraryLend {
                    book_id: book_id.to_owned(),
                    borrower: borrower.to_owned(),
                })
            }
            Some(target) => Command::Run(Invocation::ProjectFunction(target.to_owned())),
            None => Command::Run(Invocation::ProjectFunction("main.main".into())),
        },
        Some(_) => {
            return Err(Diagnostic::usage(
                "E1002",
                "unknown Orna command",
                "use `--help` to list supported commands",
            ));
        }
    };
    if words.next().is_some() {
        return Err(Diagnostic::usage(
            "E1003",
            "command has unexpected arguments",
            "this bounded slice accepts no extra arguments",
        ));
    }
    Ok(Parsed {
        endpoint,
        command,
        color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parser_keeps_primary_commands_and_explicit_recovery_routes() {
        assert_eq!(parse_cli(&args(&[])).unwrap().command, Command::Repl(None));
        assert_eq!(
            parse_cli(&args(&["check"])).unwrap().command,
            Command::Check
        );
        assert_eq!(
            parse_cli(&args(&["run"])).unwrap().command,
            Command::Run(Invocation::ProjectFunction("main.main".into()))
        );
        assert_eq!(
            parse_cli(&args(&["fetch"])).unwrap().command,
            Command::Fetch {
                remote: "origin".into(),
                branch: "main".into(),
            }
        );
        assert_eq!(
            parse_cli(&args(&["status", "--porcelain"]))
                .unwrap()
                .command,
            Command::Status {
                format: StatusFormat::Porcelain,
            }
        );
    }

    #[test]
    fn parser_preserves_option_and_argument_diagnostic_text() {
        let error = parse_cli(&args(&["--color", "invalid", "status"]))
            .expect_err("invalid color mode is rejected");
        assert_eq!(
            (error.code, error.title, error.help),
            (
                "E1002",
                "unsupported value for `--color`",
                "choose `auto`, `always`, or `never`"
            )
        );

        let error = parse_cli(&args(&["serve"])).expect_err("unknown command is rejected");
        assert_eq!(
            (error.code, error.title, error.help),
            (
                "E1002",
                "unknown Orna command",
                "use `--help` to list supported commands"
            )
        );
    }
}
