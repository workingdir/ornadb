//! Translation from parsed commands to the existing runtime and repository handlers.

use super::*;
use crate::cli_args::LegendFormat;

pub(super) fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    match parsed.command.clone() {
        Command::Help => {
            cli_help::print_help();
            Ok(())
        }
        Command::Version => {
            println!("orna-cli-v1 0.1.0");
            Ok(())
        }
        Command::Init(ref target) => {
            initialize_repository(target.as_deref(), parsed.color.stdout_enabled())
        }
        Command::Fetch {
            ref remote,
            ref branch,
        } => run_fetch(
            &parsed.endpoint,
            remote,
            branch,
            parsed.color.stdout_enabled(),
        ),
        Command::SemanticLegend {
            format: LegendFormat::Json,
            ..
        } => {
            print!("{}", orna_syntax_v1::editor::semantic_legend_json());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Markdown,
            ..
        } => {
            print!("{}", orna_syntax_v1::editor::semantic_legend_markdown());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Csv,
            ..
        } => {
            print!("{}", orna_syntax_v1::editor::semantic_legend_csv());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Count,
            ..
        } => {
            let (token_types, token_modifiers) = orna_syntax_v1::editor::semantic_legend_counts();
            println!("token_types {token_types}");
            println!("token_modifiers {token_modifiers}");
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Table,
            ..
        } => {
            print!("{}", orna_syntax_v1::editor::semantic_legend_table());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Version,
            ..
        } => {
            println!("orna-syntax-v1 {}", orna_syntax_v1::editor::SEMANTIC_LEGEND_VERSION);
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Quiet,
            limit,
        } => {
            for token_type in orna_syntax_v1::editor::legend_token_types().take(limit.unwrap_or(usize::MAX)) {
                println!("{token_type}");
            }
            for modifier in orna_syntax_v1::editor::TOKEN_MODIFIERS {
                println!("{modifier}");
            }
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Text,
            limit,
        } => {
            println!("Token types:");
            for token_type in orna_syntax_v1::editor::legend_token_types().take(limit.unwrap_or(usize::MAX)) {
                println!("  {token_type}");
            }
            println!("Token modifiers:");
            for modifier in orna_syntax_v1::editor::TOKEN_MODIFIERS {
                println!("  {modifier}");
            }
            Ok(())
        }
        Command::Serve { port } => cli_serve::run(&parsed.endpoint, port),
        Command::Diff(ref arguments) => run_git_diff(arguments),
        Command::History(arguments) => cli_history::run(&parsed.endpoint, &arguments),
        Command::Query(arguments) => cli_query::run(&parsed.endpoint, &arguments),
        Command::Import(ref arguments) => cli_import::run(arguments),
        Command::Export(ref arguments) => {
            cli_export::run(std::path::Path::new(local_project_path(&parsed.endpoint)?), arguments)
        }
        Command::Status {
            format: StatusFormat::Human,
        } => run_status_human(&parsed.endpoint, parsed.color.stdout_enabled()),
        Command::Status {
            format: StatusFormat::Porcelain,
        } => run_status(&parsed.endpoint),
        Command::Status {
            format: StatusFormat::Short,
        } => run_status_short_with_runtime_summary(
            &parsed.endpoint,
            parsed.color.stdout_enabled(),
        ),
        Command::Status {
            format: StatusFormat::Json,
        } => run_status_json(&parsed.endpoint),
        Command::Check => check_project(&parsed.endpoint, parsed.color.stdout_enabled()),
        Command::Invoke(ref target) => {
            run_pure_invocation(&parsed.endpoint, target, parsed.color.stdout_enabled())
        }
        Command::Explain(code) => {
            println!("{}", explain_diagnostic(&code)?);
            Ok(())
        }
        Command::Repl(ref expression) => {
            run_repl(&parsed.endpoint, expression.as_deref(), parsed.color)
        }
        Command::Run(Invocation::Seed) => {
            run_project_invocation(&parsed.endpoint, "main.seed", parsed.color.stdout_enabled())
        }
        Command::Run(Invocation::Exercise) => run_project_invocation(
            &parsed.endpoint,
            "main.exercise",
            parsed.color.stdout_enabled(),
        ),
        Command::Run(Invocation::SensorsIngest) => run_project_stream_invocation(
            &parsed.endpoint,
            "sensors.ingest",
            parsed.color.stdout_enabled(),
        ),
        Command::Run(Invocation::LibraryLend {
            ref book_id,
            ref borrower,
        }) => {
            let arguments = Environment::from([
                (
                    "book_id".into(),
                    Value::new(OvbRaw::Text(book_id.clone())).map_err(|_| {
                        Diagnostic::usage(
                            "E1002",
                            "`run library.lend` received an invalid book ID",
                            "supply a valid Orna string value for the book ID",
                        )
                    })?,
                ),
                (
                    "borrower".into(),
                    Value::new(OvbRaw::Text(borrower.clone())).map_err(|_| {
                        Diagnostic::usage(
                            "E1002",
                            "`run library.lend` received an invalid borrower",
                            "supply a valid Orna string value for the borrower",
                        )
                    })?,
                ),
            ]);
            run_project_invocation_with_arguments(
                &parsed.endpoint,
                "library.lend",
                &arguments,
                parsed.color.stdout_enabled(),
            )
        }
        Command::Run(Invocation::ProjectMain) => {
            run_default_project_main(&parsed.endpoint, parsed.color.stdout_enabled())
        }
        Command::Run(Invocation::ProjectFunction(ref target)) => {
            run_public_project_function(&parsed.endpoint, target, parsed.color.stdout_enabled())
        }
    }
}

fn run_status_short_with_runtime_summary(
    endpoint: &Endpoint,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    run_status_short(endpoint, color_enabled)?;
    writeln!(io::stdout().lock(), "{}", short_runtime_summary(unpublished_count)).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --short`",
        )
    })?;
    Ok(())
}

fn short_runtime_summary(count: usize) -> String {
    let noun = if count == 1 {
        "mutation"
    } else {
        "mutations"
    };
    format!("RM {count} runtime {noun}")
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

fn run_git_diff(arguments: &[String]) -> Result<(), Diagnostic> {
    let status = std::process::Command::new("git")
        .arg("diff")
        .args(arguments)
        .status()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2000",
                "Git diff could not be started",
                "check that Git is installed and available on PATH",
                error.to_string(),
            )
        })?;
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    std::process::exit(128)
}
