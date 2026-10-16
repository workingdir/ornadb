//! Translation from parsed commands to the existing runtime and repository handlers.

use super::*;
use crate::cli_args::LegendFormat;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
};

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
        Command::Push {
            ref remote,
            ref branch,
        } => run_push(
            &parsed.endpoint,
            remote,
            branch.as_deref(),
            parsed.color.stdout_enabled(),
        ),
        Command::Mv { ref old, ref new } => {
            run_mv(&parsed.endpoint, old, new, parsed.color.stdout_enabled())
        }
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
            println!(
                "orna-syntax-v1 {}",
                orna_syntax_v1::editor::SEMANTIC_LEGEND_VERSION
            );
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Quiet,
            limit,
        } => {
            for token_type in
                orna_syntax_v1::editor::legend_token_types().take(limit.unwrap_or(usize::MAX))
            {
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
            for token_type in
                orna_syntax_v1::editor::legend_token_types().take(limit.unwrap_or(usize::MAX))
            {
                println!("  {token_type}");
            }
            println!("Token modifiers:");
            for modifier in orna_syntax_v1::editor::TOKEN_MODIFIERS {
                println!("  {modifier}");
            }
            Ok(())
        }
        Command::Serve { port } => cli_serve::run(&parsed.endpoint, port),
        Command::Mount {
            ref mountpoint,
            ref selector,
        } => cli_mount::run_mount(&parsed.endpoint, mountpoint, selector),
        Command::MountStatus { json } => cli_mount::run_mount_status(&parsed.endpoint, json),
        Command::Unmount { ref mountpoint } => cli_mount::run_unmount(&parsed.endpoint, mountpoint),
        Command::Diff(ref arguments) => run_git_diff(arguments),
        Command::Log(ref arguments) => run_git_log(arguments),
        Command::History(arguments) => cli_history::run(&parsed.endpoint, &arguments),
        Command::Query(arguments) => cli_query::run(&parsed.endpoint, &arguments),
        Command::Import(ref arguments) => cli_import::run(arguments),
        Command::Export(ref arguments) => cli_export::run(
            std::path::Path::new(local_project_path(&parsed.endpoint)?),
            arguments,
        ),
        Command::Migrate(ref arguments) => run_migrate(&parsed.endpoint, arguments),
        Command::Publish(ref arguments) => cli_publish::run(&parsed.endpoint, arguments),
        Command::Annotate(arguments) => cli_annotate::run(&arguments),
        Command::Status {
            format: StatusFormat::Human,
        } => run_status_human(&parsed.endpoint, parsed.color.stdout_enabled()),
        Command::Status {
            format: StatusFormat::Porcelain,
        } => run_status(&parsed.endpoint),
        Command::Status {
            format: StatusFormat::Short,
        } => run_status_short_with_runtime_summary(&parsed.endpoint, parsed.color.stdout_enabled()),
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

#[allow(
    clippy::too_many_lines,
    reason = "the source and manifest update must remain one auditable coordination-lock sequence"
)]
fn run_mv(
    endpoint: &Endpoint,
    old: &str,
    new: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let Some((old_namespace, old_leaf)) = split_qualified_name(old) else {
        return Err(mv_usage());
    };
    let Some((new_namespace, new_leaf)) = split_qualified_name(new) else {
        return Err(mv_usage());
    };
    let old_qualified = if old_namespace.is_empty() {
        old_leaf.to_owned()
    } else {
        format!("{old_namespace}.{old_leaf}")
    };
    let new_qualified = if new.contains('.') {
        if new_namespace != old_namespace {
            return Err(mv_usage());
        }
        new.to_owned()
    } else if old_namespace.is_empty() {
        new_leaf.to_owned()
    } else {
        format!("{old_namespace}.{new_leaf}")
    };
    if old_qualified == new_qualified {
        return Err(mv_usage());
    }

    let project = load_project_without_standard_rejection(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(local_project_path(endpoint)?)
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree could not be discovered",
                "run `mv` inside a Git worktree or provide a local project path",
            )
        })?;
    let mut modules = project.modules().to_vec();
    let module_index = modules
        .iter()
        .position(|module| module_namespace_matches(&module.logical_path, old_namespace))
        .ok_or_else(|| {
            Diagnostic::target(
                "E2101",
                "source table was not found",
                "use the table's qualified name from the current source project",
            )
        })?;
    let source = &modules[module_index].source;
    let parsed = orna_syntax_v1::parse_module(source);
    if !parsed.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "table source could not be parsed",
            "fix the source module, then retry `mv`",
        ));
    }
    let mut matching_items = parsed.value.items.iter().filter(|item| {
        matches!(
            &item.declaration,
            orna_syntax_v1::Declaration::Table { name, .. } if name == old_leaf
        )
    });
    let item = matching_items.next().ok_or_else(|| {
        Diagnostic::target(
            "E2101",
            "source table was not found",
            "use an active table declaration recorded in the object identity manifest",
        )
    })?;
    if matching_items.next().is_some() {
        return Err(Diagnostic::target(
            "E2101",
            "source table name is ambiguous",
            "use a project with one table declaration for the selected qualified name",
        ));
    }
    let name_span = table_declaration_name_span(source, &item.span, old_leaf).ok_or_else(|| {
        Diagnostic::target(
            "E2101",
            "table declaration name could not be located",
            "fix the source module, then retry `mv`",
        )
    })?;
    let original_source = std::mem::take(&mut modules[module_index].source);
    let mut updated_source = String::with_capacity(
        original_source.len() - (name_span.end - name_span.start) + new_leaf.len(),
    );
    updated_source.push_str(&original_source[..name_span.start]);
    updated_source.push_str(new_leaf);
    updated_source.push_str(&original_source[name_span.end..]);
    modules[module_index].source = updated_source;

    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(&modules, &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "table rename would leave the source project invalid",
            "update affected source references and retry `mv`",
        ));
    }
    let source_path =
        checked_project_source_path(repository.worktree(), &modules[module_index].logical_path)?;
    let rename = orna_project_v1::ObjectIdentityRename::new(
        orna_project_v1::ObjectKind::Table,
        old_qualified,
        new_qualified,
    );

    repository.with_coordination_lock(|| {
        let previous = orna_project_v1::ObjectIdentityManifest::read_worktree(&repository)
            .map_err(|_| mv_manifest_io_error())?
            .ok_or_else(|| {
                Diagnostic::target(
                    "E2101",
                    "object identity manifest is missing",
                    "reconcile and commit object identities before using `mv`",
                )
            })?;
        if previous
            .id(
                orna_project_v1::ObjectKind::Table,
                rename.old_qualified_name(),
            )
            .is_none()
        {
            return Err(Diagnostic::target(
                "E2101",
                "table identity was not found",
                "use an active table declaration recorded in the object identity manifest",
            ));
        }
        let next = orna_project_v1::ObjectIdentityManifest::reconcile_with_renames(
            &modules,
            Some(&previous),
            std::slice::from_ref(&rename),
        )
        .map_err(|_| {
            Diagnostic::target(
                "E2101",
                "table rename conflicts with the object identity manifest",
                "choose an unused table name and retry `mv`",
            )
        })?;
        if write_source_atomically(
            &source_path,
            original_source.as_bytes(),
            modules[module_index].source.as_bytes(),
        )
        .is_err()
        {
            if !restore_source_after_failed_write(
                &source_path,
                original_source.as_bytes(),
                modules[module_index].source.as_bytes(),
            ) {
                return Err(mv_partial_update_error());
            }
            return Err(Diagnostic::target(
                "E2100",
                "table source could not be updated",
                "check source-file permissions and retry `mv`",
            ));
        }
        if next.write_worktree(&repository).is_err() {
            let manifest_restored =
                match orna_project_v1::ObjectIdentityManifest::read_worktree(&repository) {
                    Ok(Some(current)) if current == next => {
                        previous.write_worktree(&repository).is_ok()
                    }
                    Ok(Some(current)) if current == previous => true,
                    _ => false,
                };
            let source_restored = restore_source_after_failed_write(
                &source_path,
                original_source.as_bytes(),
                modules[module_index].source.as_bytes(),
            );
            if !manifest_restored || !source_restored {
                return Err(mv_partial_update_error());
            }
            return Err(mv_manifest_io_error());
        }
        Ok(())
    })?;

    write_success_line(
        &format!(
            "renamed {} to {}",
            rename.old_qualified_name(),
            rename.new_qualified_name(),
        ),
        color_enabled,
    )
}

fn mv_usage() -> Diagnostic {
    Diagnostic::usage(
        "E1002",
        "invalid table rename names",
        "use `mv TABLE NEW_NAME` within the table's existing module namespace",
    )
}

fn split_qualified_name(name: &str) -> Option<(&str, &str)> {
    let (namespace, leaf) = name.rsplit_once('.').unwrap_or(("", name));
    if leaf.is_empty()
        || name.split('.').any(|part| part.is_empty())
        || !name.split('.').all(is_identifier)
    {
        return None;
    }
    Some((namespace, leaf))
}

fn is_identifier(value: &str) -> bool {
    let Ok(tokens) = orna_syntax_v1::lex(value) else {
        return false;
    };
    let mut identifiers = tokens
        .iter()
        .filter(|token| !matches!(&token.kind, orna_syntax_v1::TokenKind::Eof));
    let Some(token) = identifiers.next() else {
        return false;
    };
    if identifiers.next().is_some() {
        return false;
    }
    matches!(
        &token.kind,
        orna_syntax_v1::TokenKind::Identifier { normalized }
            if normalized == value
                && token.text == value
                && token.span.start == 0
                && token.span.end == value.len()
    )
}

fn module_namespace_matches(path: &str, expected: &str) -> bool {
    let Some(without_extension) = path.strip_suffix(".orna") else {
        return false;
    };
    let module_path = match without_extension.rsplit_once('/') {
        Some((directory, "main")) => directory,
        Some(_) => without_extension,
        None if without_extension == "main" => "",
        None => without_extension,
    };
    if module_path.is_empty() {
        return expected.is_empty();
    }
    let mut module_parts = module_path.split('/');
    let mut namespace_parts = expected.split('.');
    loop {
        match (module_parts.next(), namespace_parts.next()) {
            (Some(module_part), Some(namespace_part)) if module_part == namespace_part => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn table_declaration_name_span(
    source: &str,
    item_span: &orna_syntax_v1::SyntaxSpan,
    expected_name: &str,
) -> Option<orna_syntax_v1::SyntaxSpan> {
    let tokens = orna_syntax_v1::lex(source).ok()?;
    let keyword = tokens.iter().position(|token| {
        token.span.start >= item_span.start
            && token.span.end <= item_span.end
            && token.kind == orna_syntax_v1::TokenKind::Keyword(orna_syntax_v1::Keyword::Table)
    })?;
    let name = tokens.get(keyword.checked_add(1)?)?;
    if !matches!(
        &name.kind,
        orna_syntax_v1::TokenKind::Identifier { normalized } if normalized == expected_name
    ) {
        return None;
    }
    Some(name.span.clone())
}

fn checked_project_source_path(root: &Path, logical_path: &str) -> Result<PathBuf, Diagnostic> {
    let relative = Path::new(logical_path);
    let component_count = relative.components().count();
    if relative.is_absolute()
        || component_count == 0
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(mv_source_path_error());
    }
    let root_metadata = fs::symlink_metadata(root).map_err(|_| mv_source_path_error())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(mv_source_path_error());
    }
    let mut path = root.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|_| mv_source_path_error())?;
        if metadata.file_type().is_symlink()
            || (index + 1 == component_count && !metadata.is_file())
            || (index + 1 != component_count && !metadata.is_dir())
        {
            return Err(mv_source_path_error());
        }
    }
    Ok(path)
}

fn write_source_atomically(path: &Path, expected: &[u8], replacement: &[u8]) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::other("source path is not a regular file"));
    }
    if fs::read(path)? != expected {
        return Err(io::Error::other("source changed during rename"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("source has no parent directory"))?;
    let temp = parent.join(format!(".orna-mv-{:032x}.tmp", cli_request_nonce()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.set_permissions(metadata.permissions())?;
        file.write_all(replacement)?;
        file.sync_all()?;
        drop(file);
        let current_metadata = fs::symlink_metadata(path)?;
        if current_metadata.file_type().is_symlink()
            || !current_metadata.is_file()
            || fs::read(path)? != expected
        {
            return Err(io::Error::other("source changed during rename"));
        }
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn restore_source_after_failed_write(path: &Path, original: &[u8], updated: &[u8]) -> bool {
    let Ok(current) = fs::symlink_metadata(path) else {
        return false;
    };
    if current.file_type().is_symlink() || !current.is_file() {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) if bytes == original => true,
        Ok(bytes) if bytes == updated => write_source_atomically(path, updated, original).is_ok(),
        _ => false,
    }
}

fn mv_source_path_error() -> Diagnostic {
    Diagnostic::target(
        "E2100",
        "table source path is unavailable or unsafe",
        "restore the source module as a regular worktree file, then retry `mv`",
    )
}

fn mv_manifest_io_error() -> Diagnostic {
    Diagnostic::target(
        "E2100",
        "object identity manifest could not be updated",
        "check repository permissions and retry `mv`",
    )
}

fn mv_partial_update_error() -> Diagnostic {
    Diagnostic::target(
        "E2100",
        "table rename could not be fully rolled back",
        "inspect the source module and object identity manifest before retrying `mv`",
    )
}

fn run_migrate(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let usage = || {
        Diagnostic::usage(
            "E1002",
            "unsupported `migrate copy` arguments",
            "use `migrate copy DEST --annotation-defaults N --no-persistent-consumers`",
        )
    };
    let mut arguments = arguments.iter().map(String::as_str);
    if arguments.next() != Some("copy") {
        return Err(usage());
    }
    let destination = arguments.next().ok_or_else(|| {
        Diagnostic::usage(
            "E1001",
            "`migrate copy` needs a destination",
            "supply DEST after `migrate copy`",
        )
    })?;
    let mut annotation_defaults = None;
    let mut no_persistent_consumers = false;
    while let Some(option) = arguments.next() {
        match option {
            "--annotation-defaults" if annotation_defaults.is_none() => {
                let value = arguments.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--annotation-defaults` needs a count",
                        "supply the legacy metadata entry count after `--annotation-defaults`",
                    )
                })?;
                annotation_defaults = Some(value.parse::<u32>().map_err(|_| {
                    Diagnostic::usage(
                        "E1002",
                        "invalid `--annotation-defaults` count",
                        "supply a nonnegative integer",
                    )
                })?);
            }
            "--no-persistent-consumers" if !no_persistent_consumers => {
                no_persistent_consumers = true;
            }
            _ => return Err(usage()),
        }
    }
    if !no_persistent_consumers {
        return Err(Diagnostic::usage(
            "E1001",
            "`migrate copy` needs an explicit consumer-continuity choice",
            "supply `--no-persistent-consumers` when no persistent consumer state must be carried",
        ));
    }
    let annotation_defaults = annotation_defaults.ok_or_else(|| {
        Diagnostic::usage(
            "E1001",
            "`migrate copy` needs an annotation-default count",
            "supply `--annotation-defaults N` from decoded legacy metadata",
        )
    })?;
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository_v1::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "legacy migration source could not be opened",
            "run the command from a local format-1 or format-2 worktree",
            format!("{error:?}"),
        )
    })?;
    let annotation_defaults = orna_repository_v1::MigrationAnnotationDefaults::new(
        annotation_defaults,
    )
    .map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "migration annotation metadata was invalid",
            "supply the count derived from decoded legacy metadata",
            format!("{error:?}"),
        )
    })?;
    let continuity =
        orna_repository_v1::MigrationContinuityRecord::new(Vec::new()).map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration consumer continuity could not be represented",
                "retry with the migration continuity inventory available",
                format!("{error:?}"),
            )
        })?;
    let copy = orna_repository_v1::migration_copy::prepare_format3_migration(
        &repository,
        &std::path::PathBuf::from(destination),
        continuity,
        annotation_defaults,
    )
    .map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "migration copy could not be prepared or recovered",
            "verify the source worktree and retry with the same migration coordinates",
            format!("{error:?}"),
        )
    })?;
    println!(
        "prepared or recovered format-3 migration copy: source_format={} source_commit={} candidate_commit={} destination={}",
        copy.source_format_number(),
        copy.source_commit().as_str(),
        copy.candidate_journal().new_head().as_str(),
        copy.destination().display(),
    );
    Ok(())
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

fn run_git_log(arguments: &[String]) -> Result<(), Diagnostic> {
    let status = std::process::Command::new("git")
        .arg("log")
        .args(arguments)
        .status()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2000",
                "Git log could not be started",
                "check that Git is installed and available on PATH",
                error.to_string(),
            )
        })?;
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    std::process::exit(128)
}
