//! Translation from parsed commands to the existing runtime and repository handlers.

use super::*;
use crate::cli_args::LegendFormat;
use crate::cli_migrate::{run_migrate, run_upgrade};
use crate::cli_mv::run_mv;
use std::{
    io::{self, Write as _},
    path::Path,
};

pub(super) fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    let endpoint = &parsed.endpoint;
    let color = parsed.color.stdout_enabled();
    match &parsed.command {
        Command::Help => {
            cli_help::print_help();
            Ok(())
        }
        Command::Version => {
            println!("orna 0.1.0");
            Ok(())
        }
        Command::Init(target) => initialize_repository(target.as_deref(), color),
        Command::Fetch { remote, branch } => run_fetch(endpoint, remote, branch, color),
        Command::Push { remote, branch } => run_push(endpoint, remote, branch.as_deref(), color),
        Command::Mv { old, new } => run_mv(endpoint, old, new, color),
        Command::SemanticLegend { format, limit } => {
            print_semantic_legend(*format, *limit);
            Ok(())
        }
        Command::Serve { port, allow_push } => cli_serve::run(endpoint, *port, *allow_push),
        Command::Mount {
            mountpoint,
            selector,
        } => cli_mount::run_mount(endpoint, mountpoint, selector),
        Command::MountCwd { mountpoint } => cli_mount::run_mount_cwd(endpoint, mountpoint),
        Command::MountStatus { json } => cli_mount::run_mount_status(endpoint, *json),
        Command::Unmount { mountpoint } => cli_mount::run_unmount(endpoint, mountpoint),
        Command::Edit { path, row } => cli_mount::run_edit(endpoint, path, *row),
        Command::Diff(arguments) => run_git_passthrough(endpoint, "diff", arguments),
        Command::Log(arguments) => run_git_passthrough(endpoint, "log", arguments),
        Command::GitPassthrough { name, arguments } => {
            run_git_passthrough(endpoint, name, arguments)
        }
        Command::BranchCreate { name } => cli_checkout::run_branch_create(endpoint, name),
        Command::Checkout { name } => cli_checkout::run_checkout(endpoint, name),
        Command::RestoreRow { table, key } => cli_stage::unstage_row(endpoint, table, key),
        Command::History(arguments) => cli_history::run(endpoint, arguments),
        Command::Query(arguments) => cli_query::run(endpoint, arguments),
        Command::Import(arguments) => cli_import::run(arguments),
        Command::Export(arguments) => {
            cli_export::run(Path::new(local_project_path(endpoint)?), arguments)
        }
        Command::Migrate(arguments) => run_migrate(endpoint, arguments),
        Command::Upgrade(arguments) => run_upgrade(endpoint, arguments),
        Command::Publish(arguments) => cli_publish::run(endpoint, arguments),
        Command::Commit(arguments) => cli_commit::run(endpoint, arguments),
        Command::Annotate(arguments) => cli_annotate::run(arguments),
        Command::AddRow { table, key } => cli_stage::add_row(endpoint, table, key),
        Command::Status { format } => match format {
            StatusFormat::Human => run_status_human(endpoint, color),
            StatusFormat::Porcelain => run_status(endpoint),
            StatusFormat::Short => run_status_short_with_runtime_summary(endpoint, color),
            StatusFormat::Json => run_status_json(endpoint),
        },
        Command::Check => check_project(endpoint, color),
        Command::Invoke(target) => run_pure_invocation(endpoint, target, color),
        Command::Explain(code) => {
            println!("{}", explain_diagnostic(code)?);
            Ok(())
        }
        Command::Repl(expression) => run_repl(endpoint, expression.as_deref(), parsed.color),
        Command::Run(invocation) => run_invocation(endpoint, invocation, color),
    }
}

/// Prints the semantic legend of the editor tooling in the requested shape.
/// `limit` caps the token-type rows of the plain text and `--quiet` listings;
/// the other shapes always carry the full legend.
fn print_semantic_legend(format: LegendFormat, limit: Option<usize>) {
    use orna_syntax::editor;
    let limited_types = || editor::legend_token_types().take(limit.unwrap_or(usize::MAX));
    match format {
        LegendFormat::Json => print!("{}", editor::semantic_legend_json()),
        LegendFormat::Markdown => print!("{}", editor::semantic_legend_markdown()),
        LegendFormat::Csv => print!("{}", editor::semantic_legend_csv()),
        LegendFormat::Table => print!("{}", editor::semantic_legend_table()),
        LegendFormat::Version => println!("orna-syntax {}", editor::SEMANTIC_LEGEND_VERSION),
        LegendFormat::Count => {
            let (token_types, token_modifiers) = editor::semantic_legend_counts();
            println!("token_types {token_types}");
            println!("token_modifiers {token_modifiers}");
        }
        LegendFormat::Quiet => {
            for token_type in limited_types() {
                println!("{token_type}");
            }
            for modifier in editor::TOKEN_MODIFIERS {
                println!("{modifier}");
            }
        }
        LegendFormat::Text => {
            println!("Token types:");
            for token_type in limited_types() {
                println!("  {token_type}");
            }
            println!("Token modifiers:");
            for modifier in editor::TOKEN_MODIFIERS {
                println!("  {modifier}");
            }
        }
    }
}

/// `orna run seed|exercise|sensors.ingest|library.lend|FUNCTION`.
fn run_invocation(
    endpoint: &Endpoint,
    invocation: &Invocation,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    match invocation {
        Invocation::Seed => run_project_invocation(endpoint, "main.seed", color_enabled),
        Invocation::Exercise => run_project_invocation(endpoint, "main.exercise", color_enabled),
        Invocation::SensorsIngest => {
            run_project_stream_invocation(endpoint, "sensors.ingest", color_enabled)
        }
        Invocation::LibraryLend { book_id, borrower } => {
            let text_argument = |text: &str, title: &'static str, help: &'static str| {
                Value::new(OvbRaw::Text(text.to_owned()))
                    .map_err(|_| Diagnostic::usage("E1002", title, help))
            };
            let arguments = Environment::from([
                (
                    "book_id".into(),
                    text_argument(
                        book_id,
                        "`run library.lend` received an invalid book ID",
                        "supply a valid Orna string value for the book ID",
                    )?,
                ),
                (
                    "borrower".into(),
                    text_argument(
                        borrower,
                        "`run library.lend` received an invalid borrower",
                        "supply a valid Orna string value for the borrower",
                    )?,
                ),
            ]);
            run_project_invocation_with_arguments(
                endpoint,
                "library.lend",
                &arguments,
                color_enabled,
            )
        }
        Invocation::ProjectMain => run_default_project_main(endpoint, color_enabled),
        Invocation::ProjectFunction(target) => {
            run_public_project_function(endpoint, target, color_enabled)
        }
    }
}

fn run_status_short_with_runtime_summary(
    endpoint: &Endpoint,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    run_status_short(endpoint, color_enabled)?;
    writeln!(
        io::stdout().lock(),
        "{}",
        short_runtime_summary(unpublished_count)
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --short`",
        )
    })?;
    Ok(())
}

fn short_runtime_summary(count: usize) -> String {
    let noun = if count == 1 { "mutation" } else { "mutations" };
    format!("RM {count} runtime {noun}")
}

/// Run a Git-native command (ORNA-CLI-001). A local `--db PATH` runs as
/// `git -C PATH NAME ...` so the Git operation targets the database's repository,
/// not the current directory. Managed-local runs in the current worktree. Socket
/// and remote endpoints have no local repository to run Git against.
fn run_git_passthrough(
    endpoint: &Endpoint,
    name: &str,
    arguments: &[String],
) -> Result<(), Diagnostic> {
    let mut command = std::process::Command::new("git");
    match endpoint {
        Endpoint::ManagedLocal => {}
        Endpoint::Path(path) => {
            command.arg("-C").arg(path);
        }
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => {
            return Err(Diagnostic::target(
                "E2100",
                "Git passthrough requires a local repository",
                "run the command against a local worktree or with `--db PATH`",
            ));
        }
    }
    let status = command
        .arg(name)
        .args(arguments)
        .status()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2000",
                "Git command could not be started",
                "check that Git is installed and available on PATH",
                error.to_string(),
            )
        })?;
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    std::process::exit(128)
}

#[cfg(test)]
mod short_runtime_summary_tests {
    use super::short_runtime_summary;

    #[test]
    fn stable_count_format_includes_zero_singular_and_plural() {
        assert_eq!(short_runtime_summary(0), "RM 0 runtime mutations");
        assert_eq!(short_runtime_summary(1), "RM 1 runtime mutation");
        assert_eq!(short_runtime_summary(2), "RM 2 runtime mutations");
    }
}
