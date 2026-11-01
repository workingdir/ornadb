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
    Status {
        format: StatusFormat,
    },
    AddRow {
        table: String,
        key: String,
    },
    Fetch {
        remote: String,
        branch: String,
    },
    /// `push [REMOTE] [BRANCH]`: publish the ordinary branch ref together with
    /// the continuity refs an ordinary push synchronizes (ORNA-GIT-008).
    Push {
        remote: String,
        branch: Option<String>,
    },
    Mv {
        old: String,
        new: String,
    },
    Edit {
        path: String,
        row: bool,
    },
    SemanticLegend {
        format: LegendFormat,
        limit: Option<usize>,
    },
    /// `serve [--port PORT] [--allow-push]`: loopback Git transport, listing
    /// pages and live sessions. Smart-HTTP push is refused unless
    /// `--allow-push` is given.
    Serve {
        port: u16,
        allow_push: bool,
    },
    /// `mount DIR --at SELECTOR`: one read-only view of an exactly-resolved
    /// snapshot (VFS-1).
    Mount {
        mountpoint: PathBuf,
        selector: String,
    },
    /// mount DIR --cwd: writable current-workspace view with validated row saves.
    MountCwd {
        mountpoint: PathBuf,
    },

    /// `mount status [--json]`: the attached mount views.
    MountStatus {
        json: bool,
    },
    /// `unmount DIR`: release the view's record, nothing else.
    Unmount {
        mountpoint: PathBuf,
    },
    Diff(Vec<String>),
    Log(Vec<String>),
    /// Git-native operation run against the backing repository: `git -C DB NAME ...`
    /// for a local `--db` path, unchanged `git NAME ...` otherwise (ORNA-CLI-001).
    GitPassthrough {
        name: &'static str,
        arguments: Vec<String>,
    },
    /// `branch NAME`: create a branch at HEAD with no commit, leaving the index
    /// and worktree unchanged (ORNA-BRANCH-001).
    BranchCreate {
        name: String,
    },
    /// `checkout NAME`: select a branch or commit whose snapshot is HEAD
    /// (ORNA-BRANCH-002); other targets are refused until the CHECKOUT-3 plan lands.
    Checkout {
        name: String,
    },
    /// `restore --staged --row TABLE KEY`: restore one logical row's index entry to HEAD.
    RestoreRow {
        table: String,
        key: String,
    },
    History(Vec<String>),
    Query(Vec<String>),
    Import(Vec<String>),
    Export(Vec<String>),
    Publish(Vec<String>),
    Commit(Vec<String>),
    Migrate(Vec<String>),
    Annotate(Vec<String>),
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
        value
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| {
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

/// Refusal for a Git-compatible or Orna-specific operation this build does not
/// implement. It is decided at parse time, before any repository is opened, so a
/// refused command never mutates anything.
fn refused_command(name: &str) -> Diagnostic {
    let (title, help) = match name {
        "switch" => (
            "`switch` is not available in this build",
            "use `branch` to list branches; switching the Orna workspace is not available in this build",
        ),
        "merge" => (
            "`merge` is not available in this build",
            "merging rows is not available in this build; record changes with `commit`",
        ),
        "reset" => (
            "`reset` is not available in this build",
            "use `restore --staged --row TABLE KEY_TEXT` to unstage one row",
        ),
        "pull" => (
            "`pull` is not available in this build",
            "use `fetch REMOTE BRANCH`; fetching does not merge in this build",
        ),
        "stash" => (
            "`stash` is not available in this build",
            "staged rows cannot be stashed in this build; commit them or use `restore --staged --row TABLE KEY_TEXT`",
        ),
        "fmt" => (
            "`fmt` is not available in this build",
            "source formatting is not available in this build; use `check` to validate",
        ),
        "verify" => (
            "`verify` is not available in this build",
            "use `check` to validate the project in this build",
        ),
        "prune" => (
            "`prune` is not available in this build",
            "removing unreachable data is not available in this build; nothing was deleted",
        ),
        "upgrade" => (
            "`upgrade` is not available in this build",
            "format upgrades are not available in this build; nothing was changed",
        ),
        "restore" => (
            "`restore` is not available in this build",
            "use `restore --staged --row TABLE KEY_TEXT` to unstage one row; restoring worktree files is not available in this build",
        ),
        _ => (
            "command is not available in this build",
            "use `--help` to list supported commands",
        ),
    };
    Diagnostic::usage("E1002", title, help)
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
        Some("add") => {
            if words.next() != Some("--row") {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`add` requires `--row`",
                    "use `add --row TABLE KEY` to stage one logical row",
                ));
            }
            let table = words.next().ok_or_else(|| {
                Diagnostic::usage("E1001", "`add --row` needs a table", "use `add --row TABLE KEY`")
            })?;
            let key = words.next().ok_or_else(|| {
                Diagnostic::usage("E1001", "`add --row` needs a key", "use `add --row TABLE KEY`")
            })?;
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unexpected trailing arguments for `add`",
                    "use `add --row TABLE KEY` with exactly one table and key",
                ));
            }
            Command::AddRow { table: table.to_owned(), key: key.to_owned() }
        }
        Some("restore") => {
            if words.next() != Some("--staged") || words.next() != Some("--row") {
                return Err(refused_command("restore"));
            }
            let table = words.next().ok_or_else(|| {
                Diagnostic::usage(
                    "E1001",
                    "`restore --staged --row` needs a table",
                    "use `restore --staged --row TABLE KEY_TEXT`",
                )
            })?;
            let key = words.next().ok_or_else(|| {
                Diagnostic::usage(
                    "E1001",
                    "`restore --staged --row` needs a key",
                    "use `restore --staged --row TABLE KEY_TEXT`",
                )
            })?;
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unexpected trailing arguments for `restore`",
                    "use `restore --staged --row TABLE KEY_TEXT` with exactly one table and key",
                ));
            }
            Command::RestoreRow {
                table: table.to_owned(),
                key: key.to_owned(),
            }
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
                format: local_format
                    .or(output_format)
                    .unwrap_or(StatusFormat::Human),
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
        Some("mv") => {
            let old = words.next().ok_or_else(|| {
                Diagnostic::usage("E1001", "`mv` needs a source name", "use `mv OLD NEW`")
            })?;
            let new = words.next().ok_or_else(|| {
                Diagnostic::usage("E1001", "`mv` needs a target name", "use `mv OLD NEW`")
            })?;
            Command::Mv {
                old: old.to_owned(),
                new: new.to_owned(),
            }
        }
        Some("edit") => {
            let row = words.peek() == Some(&"--row");
            if row { words.next(); }
            let path = words.next().ok_or_else(|| Diagnostic::usage("E1001", "`edit` needs a path", "use `edit PATH` or `edit --row PATH`"))?;
            if words.next().is_some() {
                return Err(Diagnostic::usage("E1002", "unexpected trailing arguments for `edit`", "use `edit PATH` or `edit --row PATH`"));
            }
            Command::Edit { path: path.to_owned(), row }
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
                let mut cwd = false;
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
                            if selector.is_some() || cwd {
                                return Err(Diagnostic::usage(
                                    "E1001",
                                    "`mount` names one snapshot",
                                    "use `mount DIR --at SELECTOR`",
                                ));
                            }
                            selector = Some(value.to_owned());
                        }
                        "--cwd" => {
                            if cwd || selector.is_some() {
                                return Err(Diagnostic::usage(
                                    "E1001",
                                    "mount accepts exactly one view mode",
                                    "use mount DIR --cwd or mount DIR --at SELECTOR",
                                ));
                            }
                            cwd = true;
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
                if cwd {
                    Command::MountCwd { mountpoint }
                } else {
                    let selector = selector.ok_or_else(|| {
                        Diagnostic::usage(
                            "E1001",
                            "mount needs a view mode",
                            "use mount DIR --cwd or mount DIR --at SELECTOR",
                        )
                    })?;
                    Command::Mount {
                        mountpoint,
                        selector,
                    }
                }
            }
        }
        Some("unmount") => {
            let mountpoint = words.next().ok_or_else(|| {
                Diagnostic::usage("E1001", "`unmount` needs a mountpoint", "use `unmount DIR`")
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
        Some("annotate") => Command::Annotate(words.by_ref().map(str::to_owned).collect()),
        Some("import") => Command::Import(words.by_ref().map(str::to_owned).collect()),
        Some("export") => Command::Export(words.by_ref().map(str::to_owned).collect()),
        Some("publish") => Command::Publish(words.by_ref().map(str::to_owned).collect()),
        Some("commit") => Command::Commit(words.by_ref().map(str::to_owned).collect()),
        Some("migrate") => Command::Migrate(words.by_ref().map(str::to_owned).collect()),
        Some("serve") => {
            let mut port = 8080;
            let mut allow_push = false;
            while let Some(option) = words.next() {
                match option {
                    "--allow-push" => allow_push = true,
                    "--port" => {
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
                    _ => {
                        return Err(Diagnostic::usage(
                            "E1002",
                            "unsupported `serve` option",
                            "use `serve [--port PORT] [--allow-push]`; the listener stays on loopback",
                        ));
                    }
                }
            }
            Command::Serve { port, allow_push }
        }
        Some("diff") => {
            let mut arguments = Vec::new();
            while let Some(argument) = words.next() {
                arguments.push(argument.to_owned());
            }
            Command::Diff(arguments)
        }
        Some("log") => Command::Log(words.by_ref().map(str::to_owned).collect()),
        Some("branch") => {
            let arguments: Vec<String> = words.by_ref().map(str::to_owned).collect();
            let create = match arguments.as_slice() {
                [name] if !name.starts_with('-') => Some(name.clone()),
                _ => None,
            };
            match create {
                Some(name) => Command::BranchCreate { name },
                None => Command::GitPassthrough {
                    name: "branch",
                    arguments,
                },
            }
        }
        Some("checkout") => {
            let arguments: Vec<&str> = words.by_ref().collect();
            match arguments.as_slice() {
                [name] if !name.starts_with('-') => Command::Checkout {
                    name: (*name).to_owned(),
                },
                [] => {
                    return Err(Diagnostic::usage(
                        "E1001",
                        "`checkout` needs a branch or commit name",
                        "supply the branch or commit to select, such as `orna checkout main`",
                    ));
                }
                _ => {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "`checkout` takes exactly one branch or commit name",
                        "use `orna checkout NAME`; options are not available in this build",
                    ));
                }
            }
        }
        Some("tag") => Command::GitPassthrough {
            name: "tag",
            arguments: words.by_ref().map(str::to_owned).collect(),
        },
        Some("show") => Command::GitPassthrough {
            name: "show",
            arguments: words.by_ref().map(str::to_owned).collect(),
        },
        Some("blame") => Command::GitPassthrough {
            name: "blame",
            arguments: words.by_ref().map(str::to_owned).collect(),
        },
        Some("remote") => Command::GitPassthrough {
            name: "remote",
            arguments: words.by_ref().map(str::to_owned).collect(),
        },
        Some("clone") => Command::GitPassthrough {
            name: "clone",
            arguments: words.by_ref().map(str::to_owned).collect(),
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
            None => Command::Run(Invocation::ProjectMain),
        },
        Some(name @ ("switch" | "merge" | "reset" | "pull" | "stash" | "fmt" | "verify" | "prune" | "upgrade")) => {
            return Err(refused_command(name));
        }
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

    /// ORNA-TRUST-003, A13-live-serving-09: smart-HTTP push is an explicit
    /// operator opt-in; plain `serve` refuses it.
    #[test]
    fn serve_git_push_is_an_explicit_opt_in() {
        assert_eq!(
            parse_cli(&args(&["serve"])).unwrap().command,
            Command::Serve {
                port: 8080,
                allow_push: false,
            }
        );
        for arguments in [
            ["serve", "--allow-push", "--port", "0"],
            ["serve", "--port", "0", "--allow-push"],
        ] {
            assert_eq!(
                parse_cli(&args(&arguments)).unwrap().command,
                Command::Serve {
                    port: 0,
                    allow_push: true,
                }
            );
        }
        assert_eq!(
            parse_cli(&args(&["serve", "--push"])).unwrap_err().code,
            "E1002"
        );
    }

    #[test]
    fn semantic_legend_limit_is_positive_and_text_or_quiet_only() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--limit", "3"]))
                .unwrap()
                .command,
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
            parse_cli(&args(&["semantic-legend", "--count"]))
                .unwrap()
                .command,
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
            parse_cli(&args(&["semantic-legend", "--format", "table"]))
                .unwrap()
                .command,
            Command::SemanticLegend {
                format: LegendFormat::Table,
                limit: None,
            }
        );
        assert!(
            parse_cli(&args(&[
                "semantic-legend",
                "--format",
                "table",
                "--limit",
                "2"
            ]))
            .is_err()
        );
    }

    #[test]
    fn semantic_legend_version_flag_selects_version_output() {
        assert_eq!(
            parse_cli(&args(&["semantic-legend", "--version"]))
                .unwrap()
                .command,
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
            parse_cli(&args(&["semantic-legend", "--quiet"]))
                .unwrap()
                .command,
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
            (
                invalid_format.code,
                invalid_format.title.as_ref(),
                invalid_format.help
            ),
            (
                "E1002",
                "unsupported value for `status --format`",
                "choose `human`, `short`, or `json` after `status --format`"
            )
        );
    }

    #[test]
    fn parser_accepts_exact_mv_source_and_target() {
        assert_eq!(
            parse_cli(&args(&["mv", "Media", "Archive"]))
                .expect("mv parses")
                .command,
            Command::Mv {
                old: "Media".into(),
                new: "Archive".into(),
            }
        );
        assert_eq!(
            parse_cli(&args(&["mv"]))
                .expect_err("missing source is rejected")
                .code,
            "E1001"
        );
        assert_eq!(
            parse_cli(&args(&["mv", "Media"]))
                .expect_err("missing target is rejected")
                .code,
            "E1001"
        );
        assert_eq!(
            parse_cli(&args(&["mv", "Media", "Archive", "Extra"]))
                .expect_err("extra arguments are rejected")
                .code,
            "E1003"
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
            (error.code, error.title.as_ref(), error.help),
            (
                "E1002",
                "unsupported value for `--color`",
                "choose `auto`, `always`, or `never`"
            )
        );

        let error = parse_cli(&args(&["unknown"])).expect_err("unknown command is rejected");
        assert_eq!(
            (error.code, error.title.as_ref(), error.help),
            (
                "E1002",
                "unknown Orna command",
                "use `--help` to list supported commands"
            )
        );
    }
}
