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
    Upgrade(Vec<String>),
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
            "--db" | "--format" => {
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

/// The words of one command line, consumed left to right.
struct Words<'a> {
    words: &'a [String],
    position: usize,
}

impl<'a> Words<'a> {
    fn new(words: &'a [String]) -> Self {
        Self { words, position: 0 }
    }

    fn peek(&self) -> Option<&'a str> {
        self.words.get(self.position).map(String::as_str)
    }

    /// The next word, or the `E1001` refusal that names what is missing.
    fn require(&mut self, title: &'static str, help: &'static str) -> Result<&'a str, Diagnostic> {
        self.next().ok_or(Diagnostic::usage("E1001", title, help))
    }

    /// Refuses (`E1002`) any word left over.
    fn reject_trailing(
        &mut self,
        title: &'static str,
        help: &'static str,
    ) -> Result<(), Diagnostic> {
        match self.next() {
            Some(_) => Err(Diagnostic::usage("E1002", title, help)),
            None => Ok(()),
        }
    }

    /// Every remaining word, owned: the arguments a verb parses itself.
    fn rest(&mut self) -> Vec<String> {
        self.by_ref().map(str::to_owned).collect()
    }
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let word = self.words.get(self.position)?;
        self.position += 1;
        Some(word.as_str())
    }
}

/// The options that precede the command.
struct GlobalOptions {
    endpoint: Endpoint,
    has_explicit_endpoint: bool,
    color: ColorMode,
    output_format: Option<StatusFormat>,
}

/// Git operations run against the backing repository unchanged (ORNA-CLI-001).
const GIT_PASSTHROUGH: [&str; 5] = ["tag", "show", "blame", "remote", "clone"];

/// Operations this build refuses at parse time, before any repository is opened.
const REFUSED_COMMANDS: [&str; 8] = [
    "switch", "merge", "reset", "pull", "stash", "fmt", "verify", "prune",
];

pub(super) fn parse_cli(arguments: &[String]) -> Result<Parsed, Diagnostic> {
    let mut words = Words::new(arguments);
    let globals = parse_global_options(&mut words)?;
    let command = parse_command(&mut words, &globals)?;
    if globals.output_format.is_some() && !matches!(&command, Command::Status { .. }) {
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
        endpoint: globals.endpoint,
        command,
        color: globals.color,
    })
}

fn parse_global_options(words: &mut Words<'_>) -> Result<GlobalOptions, Diagnostic> {
    let mut globals = GlobalOptions {
        endpoint: Endpoint::ManagedLocal,
        has_explicit_endpoint: false,
        color: ColorMode::Auto,
        output_format: None,
    };
    while let Some(option) = words.peek() {
        match option {
            "--debug" => {
                words.next();
            }
            "--db" => {
                words.next();
                globals.has_explicit_endpoint = true;
                globals.endpoint = Endpoint::parse(words.require(
                    "option `--db` needs a value",
                    "supply an endpoint after `--db`",
                )?)?;
            }
            "--color" => {
                words.next();
                let value = words.require(
                    "option `--color` needs a value",
                    "choose `auto`, `always`, or `never` after `--color`",
                )?;
                globals.color = ColorMode::parse(value).ok_or(Diagnostic::usage(
                    "E1002",
                    "unsupported value for `--color`",
                    "choose `auto`, `always`, or `never`",
                ))?;
            }
            "--format" => {
                words.next();
                let value = words.require(
                    "option `--format` needs a value",
                    "choose `human`, `short`, or `json` after `--format`",
                )?;
                globals.output_format =
                    Some(StatusFormat::parse(value).ok_or(Diagnostic::usage(
                        "E1002",
                        "unsupported value for `--format`",
                        "choose `human`, `short`, or `json`",
                    ))?);
            }
            _ => break,
        }
    }
    Ok(globals)
}

fn parse_command(words: &mut Words<'_>, globals: &GlobalOptions) -> Result<Command, Diagnostic> {
    let Some(name) = words.next() else {
        return Ok(Command::Repl(None));
    };
    if let Some(&git) = GIT_PASSTHROUGH.iter().find(|&&git| git == name) {
        return Ok(Command::GitPassthrough {
            name: git,
            arguments: words.rest(),
        });
    }
    if REFUSED_COMMANDS.contains(&name) {
        return Err(refused_command(name));
    }
    match name {
        "repl" => Ok(Command::Repl(words.next().map(str::to_owned))),
        "init" => parse_init(words, globals),
        "add" => parse_add_row(words),
        "restore" => parse_restore_row(words),
        "status" => parse_status(words, globals),
        "semantic-legend" => {
            let rest = words.by_ref().collect::<Vec<_>>();
            let (format, limit) = parse_legend_args(&rest)?;
            Ok(Command::SemanticLegend { format, limit })
        }
        "fetch" => Ok(Command::Fetch {
            remote: words.next().unwrap_or("origin").to_owned(),
            branch: words.next().unwrap_or("main").to_owned(),
        }),
        "push" => parse_push(words),
        "mv" => parse_mv(words),
        "edit" => parse_edit(words),
        "history" => Ok(Command::History(words.rest())),
        "query" => Ok(Command::Query(words.rest())),
        "mount" => parse_mount(words),
        "unmount" => parse_unmount(words),
        "annotate" => Ok(Command::Annotate(words.rest())),
        "import" => Ok(Command::Import(words.rest())),
        "export" => Ok(Command::Export(words.rest())),
        "publish" => Ok(Command::Publish(words.rest())),
        "commit" => Ok(Command::Commit(words.rest())),
        "migrate" => Ok(Command::Migrate(words.rest())),
        "upgrade" => Ok(Command::Upgrade(words.rest())),
        "serve" => parse_serve(words),
        "diff" => Ok(Command::Diff(words.rest())),
        "log" => Ok(Command::Log(words.rest())),
        "branch" => Ok(parse_branch(words)),
        "checkout" => parse_checkout(words),
        "explain" => Ok(Command::Explain(
            words
                .require(
                    "`explain` needs a diagnostic code",
                    "supply a diagnostic code after `explain`, such as `ORNA-S010-IMPORT`",
                )?
                .to_owned(),
        )),
        "check" => Ok(Command::Check),
        "invoke" => Ok(Command::Invoke(
            words
                .require(
                    "`invoke` needs a function target",
                    "supply a reachable zero-argument pure function name after `invoke`",
                )?
                .to_owned(),
        )),
        "--help" | "-h" | "help" => Ok(Command::Help),
        "--version" | "-V" => Ok(Command::Version),
        "run" => parse_run(words),
        _ => Err(Diagnostic::usage(
            "E1002",
            "unknown Orna command",
            "use `--help` to list supported commands",
        )),
    }
}

fn parse_init(words: &mut Words<'_>, globals: &GlobalOptions) -> Result<Command, Diagnostic> {
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
    if globals.has_explicit_endpoint {
        return Err(Diagnostic::usage(
            "E1002",
            "`init` does not accept `--db`",
            "use `init` or `init DIRECTORY` to choose a local repository",
        ));
    }
    Ok(Command::Init(target))
}

fn parse_add_row(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    if words.next() != Some("--row") {
        return Err(Diagnostic::usage(
            "E1002",
            "`add` requires `--row`",
            "use `add --row TABLE KEY` to stage one logical row",
        ));
    }
    let table = words.require("`add --row` needs a table", "use `add --row TABLE KEY`")?;
    let key = words.require("`add --row` needs a key", "use `add --row TABLE KEY`")?;
    words.reject_trailing(
        "unexpected trailing arguments for `add`",
        "use `add --row TABLE KEY` with exactly one table and key",
    )?;
    Ok(Command::AddRow {
        table: table.to_owned(),
        key: key.to_owned(),
    })
}

fn parse_restore_row(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    if words.next() != Some("--staged") || words.next() != Some("--row") {
        return Err(refused_command("restore"));
    }
    let table = words.require(
        "`restore --staged --row` needs a table",
        "use `restore --staged --row TABLE KEY_TEXT`",
    )?;
    let key = words.require(
        "`restore --staged --row` needs a key",
        "use `restore --staged --row TABLE KEY_TEXT`",
    )?;
    words.reject_trailing(
        "unexpected trailing arguments for `restore`",
        "use `restore --staged --row TABLE KEY_TEXT` with exactly one table and key",
    )?;
    Ok(Command::RestoreRow {
        table: table.to_owned(),
        key: key.to_owned(),
    })
}

fn parse_status(words: &mut Words<'_>, globals: &GlobalOptions) -> Result<Command, Diagnostic> {
    let local_format = match words.next() {
        Some("--porcelain") => Some(StatusFormat::Porcelain),
        Some("--short") => Some(StatusFormat::Short),
        Some("--format") => {
            let value = words.require(
                "`status --format` needs a value",
                "use `status --format human`, `status --format short`, or `status --format json`",
            )?;
            Some(StatusFormat::parse(value).ok_or(Diagnostic::usage(
                "E1002",
                "unsupported value for `status --format`",
                "choose `human`, `short`, or `json` after `status --format`",
            ))?)
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
    if globals.output_format.is_some() && local_format.is_some() {
        return Err(Diagnostic::usage(
            "E1002",
            "status output format was specified twice",
            "choose either `--format VALUE status` or `status --format VALUE`",
        ));
    }
    Ok(Command::Status {
        format: local_format
            .or(globals.output_format)
            .unwrap_or(StatusFormat::Human),
    })
}

fn parse_push(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let remote = words.next().unwrap_or("origin").to_owned();
    let branch = words.next().map(str::to_owned);
    words.reject_trailing(
        "unsupported `push` argument",
        "use `push`, `push REMOTE` or `push REMOTE BRANCH`",
    )?;
    Ok(Command::Push { remote, branch })
}

fn parse_mv(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let old = words.require("`mv` needs a source name", "use `mv OLD NEW`")?;
    let new = words.require("`mv` needs a target name", "use `mv OLD NEW`")?;
    Ok(Command::Mv {
        old: old.to_owned(),
        new: new.to_owned(),
    })
}

fn parse_edit(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let row = words.peek() == Some("--row");
    if row {
        words.next();
    }
    let path = words.require(
        "`edit` needs a path",
        "use `edit PATH` or `edit --row PATH`",
    )?;
    words.reject_trailing(
        "unexpected trailing arguments for `edit`",
        "use `edit PATH` or `edit --row PATH`",
    )?;
    Ok(Command::Edit {
        path: path.to_owned(),
        row,
    })
}

/// The view a `mount DIR` names, decided as its options arrive.
enum MountMode {
    Unset,
    Cwd,
    At(String),
}

fn parse_mount(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let first = words.require(
        "`mount` needs a mountpoint",
        "use `mount DIR --at SELECTOR`, `mount status [--json]`",
    )?;
    if first == "status" {
        return parse_mount_status(words);
    }
    let mountpoint = PathBuf::from(first);
    let mut mode = MountMode::Unset;
    while let Some(option) = words.next() {
        match option {
            "--at" => {
                let value = words.require(
                    "`mount --at` needs a value",
                    "supply a commit or ref that resolves to one snapshot",
                )?;
                if !matches!(mode, MountMode::Unset) {
                    return Err(Diagnostic::usage(
                        "E1001",
                        "`mount` names one snapshot",
                        "use `mount DIR --at SELECTOR`",
                    ));
                }
                mode = MountMode::At(value.to_owned());
            }
            "--cwd" => {
                if !matches!(mode, MountMode::Unset) {
                    return Err(Diagnostic::usage(
                        "E1001",
                        "mount accepts exactly one view mode",
                        "use mount DIR --cwd or mount DIR --at SELECTOR",
                    ));
                }
                mode = MountMode::Cwd;
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
    match mode {
        MountMode::Cwd => Ok(Command::MountCwd { mountpoint }),
        MountMode::At(selector) => Ok(Command::Mount {
            mountpoint,
            selector,
        }),
        MountMode::Unset => Err(Diagnostic::usage(
            "E1001",
            "mount needs a view mode",
            "use mount DIR --cwd or mount DIR --at SELECTOR",
        )),
    }
}

fn parse_mount_status(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let unsupported = Diagnostic::usage(
        "E1002",
        "unsupported `mount status` option",
        "use `mount status` or `mount status --json`",
    );
    let json = match words.next() {
        None => false,
        Some("--json") => true,
        Some(_) => return Err(unsupported),
    };
    if words.next().is_some() {
        return Err(unsupported);
    }
    Ok(Command::MountStatus { json })
}

fn parse_unmount(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let mountpoint = words.require("`unmount` needs a mountpoint", "use `unmount DIR`")?;
    words.reject_trailing("unsupported `unmount` option", "use `unmount DIR`")?;
    Ok(Command::Unmount {
        mountpoint: PathBuf::from(mountpoint),
    })
}

fn parse_serve(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let mut port = 8080;
    let mut allow_push = false;
    while let Some(option) = words.next() {
        match option {
            "--allow-push" => allow_push = true,
            "--port" => {
                let value = words.require(
                    "`serve --port` needs a value",
                    "supply a port from 0 through 65535 after `--port`",
                )?;
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
    Ok(Command::Serve { port, allow_push })
}

/// `branch NAME` creates a branch; every other shape is Git's own `branch`.
fn parse_branch(words: &mut Words<'_>) -> Command {
    let arguments = words.rest();
    match arguments.as_slice() {
        [name] if !name.starts_with('-') => Command::BranchCreate { name: name.clone() },
        _ => Command::GitPassthrough {
            name: "branch",
            arguments,
        },
    }
}

fn parse_checkout(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let arguments: Vec<&str> = words.by_ref().collect();
    match arguments.as_slice() {
        [name] if !name.starts_with('-') => Ok(Command::Checkout {
            name: (*name).to_owned(),
        }),
        [] => Err(Diagnostic::usage(
            "E1001",
            "`checkout` needs a branch or commit name",
            "supply the branch or commit to select, such as `orna checkout main`",
        )),
        _ => Err(Diagnostic::usage(
            "E1002",
            "`checkout` takes exactly one branch or commit name",
            "use `orna checkout NAME`; options are not available in this build",
        )),
    }
}

fn parse_run(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    Ok(Command::Run(match words.next() {
        Some("seed") => Invocation::Seed,
        Some("exercise") => Invocation::Exercise,
        Some("sensors.ingest") => Invocation::SensorsIngest,
        Some("library.lend") => {
            let book_id = words.require(
                "`run library.lend` needs a book ID",
                "supply the book ID and borrower after `run library.lend`",
            )?;
            let borrower = words.require(
                "`run library.lend` needs a borrower",
                "supply the book ID and borrower after `run library.lend`",
            )?;
            Invocation::LibraryLend {
                book_id: book_id.to_owned(),
                borrower: borrower.to_owned(),
            }
        }
        Some(target) => Invocation::ProjectFunction(target.to_owned()),
        None => Invocation::ProjectMain,
    }))
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
