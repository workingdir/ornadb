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
    ProjectMain,
    ProjectFunction(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LegendFormat {
    Text,
    Json,
    Markdown,
    Csv,
    Quiet,
    Version,
    Table,
    Count,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusFormat {
    Human,
    Porcelain,
    Short,
    Json,
}

impl StatusFormat {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "human" => Self::Human,
            "short" => Self::Short,
            "json" => Self::Json,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Command {
    Repl(Option<String>),
    Init(Option<PathBuf>),
    Status { format: StatusFormat },
    Fetch { remote: String, branch: String },
    /// `push [REMOTE] [BRANCH]`: publish the ordinary branch ref together with
    /// the continuity refs an ordinary push synchronizes (ORNA-GIT-008).
    Push {
        remote: String,
        branch: Option<String>,
    },
    SemanticLegend {
        format: LegendFormat,
        limit: Option<usize>,
    },
    Serve { port: u16 },
    /// `mount DIR --at SELECTOR`: one read-only view of an exactly-resolved
    /// snapshot (VFS-1).
    Mount {
        mountpoint: PathBuf,
        selector: String,
    },
    /// `mount status [--json]`: the attached read-only views.
    MountStatus { json: bool },
    /// `unmount DIR`: release the view's record, nothing else.
    Unmount { mountpoint: PathBuf },
    Diff(Vec<String>),
    Log(Vec<String>),
    History(Vec<String>),
    Query(Vec<String>),
    Import(Vec<String>),
    Export(Vec<String>),
    Publish(Vec<String>),
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
            "--debug" => {
                words.next();
            }
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
            "--format" => {
                words.next();
                if words.next().is_none() {
                    break;
                }
            }
            _ => break,
        }
    }
    color
}

pub(super) fn requested_debug_mode(arguments: &[String]) -> bool {
    let mut enabled = false;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while let Some(option) = words.peek().copied() {
        match option {
            "--debug" => {
                words.next();
                enabled = true;
            }
            "--db" | "--color" | "--format" => {
                words.next();
                if words.next().is_none() {
                    break;
                }
            }
            _ => break,
        }
    }
    enabled
}

#[allow(
    clippy::too_many_lines,
    reason = "the command grammar deliberately keeps validation precedence in one auditable parser"
)]
/// Options for `semantic-legend`. `--limit N` caps the token-type rows of the
/// plain text and `--quiet` listings; the JSON, Markdown and CSV artifacts
/// always carry the full legend.
fn parse_legend_args(args: &[&str]) -> Result<(LegendFormat, Option<usize>), Diagnostic> {
    let usage = || {
        Diagnostic::usage(
            "E1002",
            "unsupported `semantic-legend` option",
            "use `semantic-legend`, `semantic-legend --json`, `semantic-legend --markdown`, `semantic-legend --format csv`, `semantic-legend --quiet`, `semantic-legend --limit N`, `semantic-legend --version`, `semantic-legend --format table` or `semantic-legend --count`",
        )
    };
    let limit = |value: &str| {
        value.parse::<usize>().ok().filter(|count| *count > 0).ok_or_else(|| {
            Diagnostic::usage(
                "E1002",
                "invalid `semantic-legend --limit` value",
                "use a positive integer, such as `--limit 5`",
            )
        })
    };
    match args {
        [] => Ok((LegendFormat::Text, None)),
        ["--json"] => Ok((LegendFormat::Json, None)),
        ["--markdown"] => Ok((LegendFormat::Markdown, None)),
        ["--format", "csv"] => Ok((LegendFormat::Csv, None)),
        ["--quiet"] => Ok((LegendFormat::Quiet, None)),
        ["--version"] => Ok((LegendFormat::Version, None)),
        ["--format", "table"] => Ok((LegendFormat::Table, None)),
        ["--count"] => Ok((LegendFormat::Count, None)),
        ["--limit", count] => Ok((LegendFormat::Text, Some(limit(count)?))),
        ["--quiet", "--limit", count] | ["--limit", count, "--quiet"] => {
            Ok((LegendFormat::Quiet, Some(limit(count)?)))
        }
        _ => Err(usage()),
    }
}

pub(super) fn parse_cli(arguments: &[String]) -> Result<Parsed, Diagnostic> {
    let mut endpoint = Endpoint::ManagedLocal;
    let mut has_explicit_endpoint = false;
    let mut color = ColorMode::Auto;
    let mut output_format = None;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while matches!(
        words.peek(),
        Some(&"--db") | Some(&"--color") | Some(&"--format") | Some(&"--debug")
    ) {
        match words.next() {
            Some("--debug") => {}
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
            Some("--format") => {
                let value = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "option `--format` needs a value",
                        "choose `human`, `short`, or `json` after `--format`",
                    )
                })?;
                output_format = Some(StatusFormat::parse(value).ok_or_else(|| {
                    Diagnostic::usage(
                        "E1002",
                        "unsupported value for `--format`",
                        "choose `human`, `short`, or `json`",
                    )
                })?);
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
        Some("status") => {
            let local_format = match words.next() {
                Some("--porcelain") => Some(StatusFormat::Porcelain),
                Some("--short") => Some(StatusFormat::Short),
                Some("--format") => {
                    let value = words.next().ok_or_else(|| {
                        Diagnostic::usage(
                            "E1001",
                            "`status --format` needs a value",
                            "use `status --format human`, `status --format short`, or `status --format json`",
                        )
                    })?;
                    Some(StatusFormat::parse(value).ok_or_else(|| {
                        Diagnostic::usage(
                            "E1002",
                            "unsupported value for `status --format`",
                            "choose `human`, `short`, or `json` after `status --format`",
                        )
                    })?)
                }
                Some(_) => {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "unsupported `status` option or option placement",
                        "use `status`, `status --porcelain`, `status --short`, `status --format human|short|json`, or `--format human|short|json status`",
                    ));
                }
                None => None,
            };
            if output_format.is_some() && local_format.is_some() {
                return Err(Diagnostic::usage(
                    "E1002",
                    "status output format was specified twice",
                    "choose either `--format VALUE status` or `status --format VALUE`",
                ));
            }
            Command::Status {
                format: local_format.or(output_format).unwrap_or(StatusFormat::Human),
            }
        }
        Some("semantic-legend") => {
            let rest = words.by_ref().collect::<Vec<_>>();
            let (format, limit) = parse_legend_args(&rest)?;
            Command::SemanticLegend { format, limit }
        }
        Some("fetch") => Command::Fetch {
            remote: words.next().unwrap_or("origin").to_owned(),
            branch: words.next().unwrap_or("main").to_owned(),
        },
        Some("push") => {
            let remote = words.next().unwrap_or("origin").to_owned();
            let branch = words.next().map(str::to_owned);
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unsupported `push` argument",
                    "use `push`, `push REMOTE` or `push REMOTE BRANCH`",
                ));
            }
            Command::Push { remote, branch }
        }
        Some("history") => Command::History(words.by_ref().map(str::to_owned).collect()),
        Some("query") => Command::Query(words.by_ref().map(str::to_owned).collect()),
        Some("mount") => {
            let first = words.next().ok_or_else(|| {
                Diagnostic::usage(
                    "E1001",
                    "`mount` needs a mountpoint",
                    "use `mount DIR --at SELECTOR`, `mount status [--json]`",
                )
            })?;
            if first == "status" {
                let json = match words.next() {
                    None => false,
                    Some("--json") => true,
                    Some(_) => {
                        return Err(Diagnostic::usage(
                            "E1002",
                            "unsupported `mount status` option",
                            "use `mount status` or `mount status --json`",
                        ));
                    }
                };
                if words.next().is_some() {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "unsupported `mount status` option",
                        "use `mount status` or `mount status --json`",
                    ));
                }
                Command::MountStatus { json }
            } else {
                let mountpoint = PathBuf::from(first);
                let mut selector = None;
                while let Some(option) = words.next() {
                    match option {
                        "--at" => {
                            let value = words.next().ok_or_else(|| {
                                Diagnostic::usage(
                                    "E1001",
                                    "`mount --at` needs a value",
                                    "supply a commit or ref that resolves to one snapshot",
                                )
                            })?;
                            if selector.is_some() {
                                return Err(Diagnostic::usage(
                                    "E1001",
                                    "`mount` names one snapshot",
                                    "use `mount DIR --at SELECTOR`",
                                ));
                            }
                            selector = Some(value.to_owned());
                        }
                        // `--cwd` is the writable workspace view; this slice
                        // implements the always-read-only `--at` view only.
                        "--cwd" | "--read-only" => {
                            return Err(Diagnostic::usage(
                                "E1002",
                                "`mount --cwd` is not implemented",
                                "use `mount DIR --at SELECTOR`, which is always read-only",
                            ));
                        }
                        _ => {
                            return Err(Diagnostic::usage(
                                "E1002",
                                "unsupported `mount` option",
                                "use `mount DIR --at SELECTOR`",
                            ));
                        }
                    }
                }
                let selector = selector.ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`mount` needs a snapshot selector",
                        "use `mount DIR --at SELECTOR`; exactly one selector is required",
                    )
                })?;
                Command::Mount {
                    mountpoint,
                    selector,
                }
            }
        }
        Some("unmount") => {
            let mountpoint = words.next().ok_or_else(|| {
                Diagnostic::usage(
                    "E1001",
                    "`unmount` needs a mountpoint",
                    "use `unmount DIR`",
                )
            })?;
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unsupported `unmount` option",
                    "use `unmount DIR`",
                ));
            }
            Command::Unmount {
                mountpoint: PathBuf::from(mountpoint),
            }
        }
        Some("import") => Command::Import(words.by_ref().map(str::to_owned).collect()),
        Some("export") => Command::Export(words.by_ref().map(str::to_owned).collect()),
        Some("publish") => Command::Publish(words.by_ref().map(str::to_owned).collect()),
        Some("serve") => {
            let mut port = 8080;
            while let Some(option) = words.next() {
                if option != "--port" {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "unsupported `serve` option",
                        "use `serve` or `serve --port PORT`; the listener stays on loopback",
                    ));
                }
                let value = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`serve --port` needs a value",
                        "supply a port from 0 through 65535 after `--port`",
                    )
                })?;
                port = value.parse().map_err(|_| {
                    Diagnostic::usage(
                        "E1002",
                        "`serve --port` is invalid",
                        "supply a port from 0 through 65535 after `--port`",
                    )
                })?;
            }
            Command::Serve { port }
        }
        Some("diff") => {
            let mut arguments = Vec::new();
            while let Some(argument) = words.next() {
                arguments.push(argument.to_owned());
            }
            Command::Diff(arguments)
        }
        Some("log") => Command::Log(words.by_ref().map(str::to_owned).collect()),
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
            None => Command::Run(Invocation::ProjectMain),
        },
        Some(_) => {
            return Err(Diagnostic::usage(
                "E1002",
                "unknown Orna command",
                "use `--help` to list supported commands",
            ));
        }
    };
    if output_format.is_some() && !matches!(&command, Command::Status { .. }) {
        return Err(Diagnostic::usage(
            "E1002",
            "`--format` is supported only by `status`",
            "use `--format human|short|json status` or `status --format human|short|json`",
        ));
    }
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
    fn semantic_legend_limit_is_positive_and_text_or_quiet_only() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--limit", "3"])).unwrap().command,
            Command::SemanticLegend {
                format: LegendFormat::Text,
                limit: Some(3),
            }
        );
        assert!(parse_cli(&args(&["semantic-legend", "--limit", "0"])).is_err());
        assert!(parse_cli(&args(&["semantic-legend", "--limit", "x"])).is_err());
        assert!(parse_cli(&args(&["semantic-legend", "--json", "--limit", "3"])).is_err());
    }

    #[test]
    fn semantic_legend_count_flag_selects_counts() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--count"])).unwrap().command,
            Command::SemanticLegend {
                format: LegendFormat::Count,
                limit: None,
            }
        );
        assert!(parse_cli(&args(&["semantic-legend", "--count", "--json"])).is_err());
    }

    #[test]
    fn semantic_legend_format_table_is_a_distinct_mode() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--format", "table"])).unwrap().command,
            Command::SemanticLegend {
                format: LegendFormat::Table,
                limit: None,
            }
        );
        assert!(parse_cli(&args(&["semantic-legend", "--format", "table", "--limit", "2"])).is_err());
    }

    #[test]
    fn semantic_legend_version_flag_selects_version_output() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--version"])).unwrap().command,
            Command::SemanticLegend {
                format: LegendFormat::Version,
                limit: None,
            }
        );
        assert!(parse_cli(&args(&["semantic-legend", "--version", "--limit", "2"])).is_err());
    }

    #[test]
    fn semantic_legend_quiet_flag_selects_bare_names() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--quiet"])).unwrap().command,
            Command::SemanticLegend {
                format: LegendFormat::Quiet,
                limit: None,
            }
        );
        assert!(parse_cli(&args(&["semantic-legend", "--quiet", "extra"])).is_err());
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
            Command::Run(Invocation::ProjectMain)
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
        for invocation in [
            args(&["status", "--format", "json"]),
            args(&["--format", "json", "status"]),
        ] {
            assert_eq!(
                parse_cli(&invocation).expect("JSON status parses").command,
                Command::Status {
                    format: StatusFormat::Json,
                }
            );
        }
        let misplaced = parse_cli(&args(&["--format", "json", "check"]))
            .expect_err("format is limited to status");
        assert_eq!(misplaced.code, "E1002");

        let invalid_format = parse_cli(&args(&["status", "--format", "xml"]))
            .expect_err("unsupported output format is rejected");
        assert_eq!(
            (invalid_format.code, invalid_format.title, invalid_format.help),
            (
                "E1002",
                "unsupported value for `status --format`",
                "choose `human`, `short`, or `json` after `status --format`"
            )
        );
    }

    #[test]
    fn parser_preserves_git_diff_argument_boundaries_and_options() {
        let parsed = parse_cli(&args(&[
            "diff",
            "--no-color",
            "--exit-code",
            "--",
            "changed path.orna",
        ]))
        .expect("diff arguments parse");
        assert_eq!(
            parsed.command,
            Command::Diff(vec![
                "--no-color".into(),
                "--exit-code".into(),
                "--".into(),
                "changed path.orna".into(),
            ])
        );
    }

    #[test]
    fn parser_preserves_git_log_argument_boundaries_and_options() {
        let parsed = parse_cli(&args(&[
            "log",
            "--oneline",
            "-n",
            "3",
            "--",
            "changed path.orna",
        ]))
        .expect("log arguments parse");
        assert_eq!(
            parsed.command,
            Command::Log(vec![
                "--oneline".into(),
                "-n".into(),
                "3".into(),
                "--".into(),
                "changed path.orna".into(),
            ])
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

        let error = parse_cli(&args(&["unknown"])).expect_err("unknown command is rejected");
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
