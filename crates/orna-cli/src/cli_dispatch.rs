//! Translation from parsed commands to the existing runtime and repository handlers.

use super::*;
use crate::cli_args::LegendFormat;
use orna_repository::relation_admission::RelationAdmissionError;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
};

#[allow(clippy::too_many_lines)]
pub(super) fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    match parsed.command.clone() {
        Command::Help => {
            cli_help::print_help();
            Ok(())
        }
        Command::Version => {
            println!("orna 0.1.0");
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
            print!("{}", orna_syntax::editor::semantic_legend_json());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Markdown,
            ..
        } => {
            print!("{}", orna_syntax::editor::semantic_legend_markdown());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Csv,
            ..
        } => {
            print!("{}", orna_syntax::editor::semantic_legend_csv());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Count,
            ..
        } => {
            let (token_types, token_modifiers) = orna_syntax::editor::semantic_legend_counts();
            println!("token_types {token_types}");
            println!("token_modifiers {token_modifiers}");
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Table,
            ..
        } => {
            print!("{}", orna_syntax::editor::semantic_legend_table());
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Version,
            ..
        } => {
            println!(
                "orna-syntax {}",
                orna_syntax::editor::SEMANTIC_LEGEND_VERSION
            );
            Ok(())
        }
        Command::SemanticLegend {
            format: LegendFormat::Quiet,
            limit,
        } => {
            for token_type in
                orna_syntax::editor::legend_token_types().take(limit.unwrap_or(usize::MAX))
            {
                println!("{token_type}");
            }
            for modifier in orna_syntax::editor::TOKEN_MODIFIERS {
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
                orna_syntax::editor::legend_token_types().take(limit.unwrap_or(usize::MAX))
            {
                println!("  {token_type}");
            }
            println!("Token modifiers:");
            for modifier in orna_syntax::editor::TOKEN_MODIFIERS {
                println!("  {modifier}");
            }
            Ok(())
        }
        Command::Serve { port, allow_push } => cli_serve::run(&parsed.endpoint, port, allow_push),
        Command::Mount {
            ref mountpoint,
            ref selector,
        } => cli_mount::run_mount(&parsed.endpoint, mountpoint, selector),
        Command::MountCwd { ref mountpoint } => {
            cli_mount::run_mount_cwd(&parsed.endpoint, mountpoint)
        }
        Command::MountStatus { json } => cli_mount::run_mount_status(&parsed.endpoint, json),
        Command::Unmount { ref mountpoint } => cli_mount::run_unmount(&parsed.endpoint, mountpoint),
        Command::Edit { ref path, row } => cli_mount::run_edit(&parsed.endpoint, path, row),
        Command::Diff(ref arguments) => run_git_passthrough(&parsed.endpoint, "diff", arguments),
        Command::Log(ref arguments) => run_git_passthrough(&parsed.endpoint, "log", arguments),
        Command::GitPassthrough {
            name,
            ref arguments,
        } => run_git_passthrough(&parsed.endpoint, name, arguments),
        Command::BranchCreate { ref name } => {
            cli_checkout::run_branch_create(&parsed.endpoint, name)
        }
        Command::Checkout { ref name } => cli_checkout::run_checkout(&parsed.endpoint, name),
        Command::RestoreRow { ref table, ref key } => {
            cli_stage::unstage_row(&parsed.endpoint, table, key)
        }
        Command::History(arguments) => cli_history::run(&parsed.endpoint, &arguments),
        Command::Query(arguments) => cli_query::run(&parsed.endpoint, &arguments),
        Command::Import(ref arguments) => cli_import::run(arguments),
        Command::Export(ref arguments) => cli_export::run(
            std::path::Path::new(local_project_path(&parsed.endpoint)?),
            arguments,
        ),
        Command::Migrate(ref arguments) => run_migrate(&parsed.endpoint, arguments),
        Command::Upgrade(arguments) => run_upgrade(&parsed.endpoint, &arguments),
        Command::Publish(ref arguments) => cli_publish::run(&parsed.endpoint, arguments),
        Command::Commit(ref arguments) => cli_commit::run(&parsed.endpoint, arguments),
        Command::Annotate(arguments) => cli_annotate::run(&arguments),
        Command::AddRow { ref table, ref key } => cli_stage::add_row(&parsed.endpoint, table, key),
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

/// Renames one table in source and moves its identity in the staged store
/// inventory (ORNA-OBJECT-003). Like `git mv`, the rename is staged for
/// review and nothing is committed: the worktree module and its staged copy
/// get the same declaration-name edit, and the inventory is restaged in the
/// same index update, so the next `orna commit` keeps the table and field
/// `ObjectIds` and with them the table's relation. Other unstaged edits stay
/// unstaged. A run interrupted after the source write resumes from the
/// renamed source.
#[allow(
    clippy::too_many_lines,
    reason = "the source edit and the staged inventory update must remain one auditable sequence"
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
    if let Ok(repository) = orna_repository::Repository::discover(local_project_path(endpoint)?)
        && repository
            .capture_staged_candidate()
            .is_ok_and(|staged| staged.store_root().is_some())
    {
        return run_store_mv(
            endpoint,
            &repository,
            &old_qualified,
            &new_qualified,
            color_enabled,
        );
    }

    let project = load_project_without_standard_rejection(endpoint)?;
    let repository =
        orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree could not be discovered",
                "run mv inside a Git worktree or provide a local project path",
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
    let original_source = renamed_table_source(&modules[module_index].source, old_leaf, new_leaf)
        .map_err(|title| {
            Diagnostic::target("E2101", title, "fix the source module, then retry mv")
        })?
        .map(|updated| std::mem::replace(&mut modules[module_index].source, updated));
    let source_already_renamed = original_source.is_none();

    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic::analyze_with_providers(
        &modules,
        &catalogue,
        orna_semantic::SystemProviders::DeclaredSurface,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "table rename would leave the source project invalid",
            "update affected source references and retry mv",
        ));
    }
    let source_path =
        checked_project_source_path(repository.worktree(), &modules[module_index].logical_path)?;
    let rename = orna_project::ObjectIdentityRename::new(
        orna_project::ObjectKind::Table,
        old_qualified,
        new_qualified,
    );

    repository
        .refuse_unretained_committed_store()
        .map_err(mv_staging_error)?;
    let staged = repository
        .capture_staged_candidate()
        .map_err(mv_staging_error)?;
    let logical_path = modules[module_index].logical_path.clone();
    let staged_source = repository
        .read_private_candidate_file(
            staged.candidate(),
            &logical_path,
            orna_project::ProjectLimits::default().max_source_bytes,
        )
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| mv_staged_source_error("the table's source module is not staged"))?;
    // The staged module gets the same declaration-name edit as the
    // worktree, so the index stays a consistent candidate, as after `git mv`.
    // Other unstaged edits to the module stay unstaged.
    let staged_rename =
        renamed_table_source(&staged_source, old_leaf, new_leaf).map_err(mv_staged_source_error)?;
    let previous = orna_project::ObjectIdentityManifest::read_staged(&repository, &staged)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "staged object identities could not be read",
                "restore the staged `.orna/store`, then retry `mv`",
                format!("{error:?}"),
            )
        })?
        .ok_or_else(mv_identity_missing_error)?;
    if previous
        .id(orna_project::ObjectKind::Table, rename.old_qualified_name())
        .is_none()
        || (source_already_renamed
            && previous
                .id(orna_project::ObjectKind::Table, rename.new_qualified_name())
                .is_some())
    {
        return Err(Diagnostic::target(
            "E2101",
            "table identity was not found in the expected rename state",
            "use an active table declaration recorded in the staged object identities",
        ));
    }
    let next = previous
        .with_renames(&modules, std::slice::from_ref(&rename))
        .map_err(|_| {
            Diagnostic::target(
                "E2101",
                "table rename conflicts with the staged object identities",
                "choose an unused table name and retry mv",
            )
        })?;

    if let Some(original_source) = original_source.as_deref()
        && write_source_atomically(
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
            "check source-file permissions and retry mv",
        ));
    }

    let staged_sources = staged_rename
        .as_deref()
        .map(|updated| (logical_path.as_str(), updated.as_bytes()))
        .into_iter()
        .collect::<Vec<_>>();
    if let Err(error) =
        repository.stage_object_identities(&staged, &next.to_stored(), &staged_sources)
    {
        let source_restored = original_source.as_deref().is_none_or(|original| {
            restore_source_after_failed_write(
                &source_path,
                original.as_bytes(),
                modules[module_index].source.as_bytes(),
            )
        });
        if !source_restored {
            return Err(mv_partial_update_error());
        }
        return Err(mv_staging_error(error));
    }

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
        "invalid rename names",
        "use `mv OLD NEW` with a qualified name such as `Music` or `Music.title`, renaming only its last part",
    )
}

/// The identity kinds `orna mv` can rename, in lookup order.
const RENAMEABLE_KINDS: [orna_project::ObjectKind; 7] = [
    orna_project::ObjectKind::Table,
    orna_project::ObjectKind::Field,
    orna_project::ObjectKind::Type,
    orna_project::ObjectKind::Variant,
    orna_project::ObjectKind::TypeField,
    orna_project::ObjectKind::VariantField,
    orna_project::ObjectKind::Unit,
];

#[allow(clippy::too_many_lines)]
/// `orna mv OLD NEW` over the identity inventory a format-3 index stages
/// in `.orna/store` (ORNA-OBJECT-003): renames one table, field, type,
/// variant, type or variant field, or unit declaration in its source file and
/// moves its `ObjectId` to the new name in the staged inventory, so the next
/// `orna commit` keeps the identity and evolves the stored rows by it. Like
/// `git mv`, the rename is staged for review and nothing is committed: the
/// worktree module and its staged copy get the same declaration-name edit,
/// and the inventory is restaged in the same index update. Other unstaged
/// edits stay unstaged. A run interrupted after the source write resumes from
/// the renamed source. A failed staging restores the source.
fn run_store_mv(
    endpoint: &Endpoint,
    repository: &orna_repository::Repository,
    old: &str,
    new: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    repository
        .refuse_unretained_committed_store()
        .map_err(mv_staging_error)?;
    let pending = repository
        .read_publication_journal()
        .map_err(|error| mv_staging_error(RelationAdmissionError::Repository(error)))?;
    if pending.is_some() {
        return Err(mv_staging_error(RelationAdmissionError::Repository(
            orna_repository::RepositoryError::PublicationPending,
        )));
    }
    let identity_error = |error: orna_project::ObjectIdentityError| {
        Diagnostic::target_with_detail(
            "E2101",
            "the rename could not keep the object identity",
            "fix the reported condition, then retry `orna mv`; the source was left unchanged",
            error.to_string(),
        )
    };
    let inventory = orna_project::ObjectIdentityManifest::read_index(repository)
        .map_err(identity_error)?
        .unwrap_or_default();
    let Some(kind) = RENAMEABLE_KINDS
        .into_iter()
        .find(|kind| inventory.id(*kind, old).is_some())
    else {
        return Err(Diagnostic::target(
            "E2101",
            "no committed or staged definition has that name",
            "name a table, field, type, variant or unit by its qualified name, such as `Music.title`; a definition that was never committed has no identity yet, so rename it in source",
        ));
    };
    let new_leaf = new.rsplit('.').next().unwrap_or(new);
    let project = load_project_without_standard_rejection(endpoint)?;
    let mut modules = project.modules().to_vec();
    // After an interrupted run the worktree already declares the new name:
    // the source is left as it is and only the index is staged.
    let (module_index, original_source) = if let Some((module_index, span)) =
        declaration_name_span(&modules, kind, old)
    {
        let original_source = std::mem::take(&mut modules[module_index].source);
        modules[module_index].source = renamed_name_source(&original_source, &span, new_leaf);
        (module_index, Some(original_source))
    } else {
        let (module_index, _) = declaration_name_span(&modules, kind, new).ok_or_else(|| {
            Diagnostic::target(
                "E2101",
                "the declaration to rename was not found in the worktree source",
                "restore the declaration under its committed name, then retry `orna mv`",
            )
        })?;
        (module_index, None)
    };
    let catalogue = semantic_catalogue(&project)?;
    if !orna_semantic::analyze_with_catalogue(&modules, &catalogue).is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "the rename would leave the source project invalid",
            "update the source that refers to the old name, then retry `orna mv`",
        ));
    }
    let source_path =
        checked_project_source_path(repository.worktree(), &modules[module_index].logical_path)?;
    let logical_path = modules[module_index].logical_path.clone();
    let staged = repository
        .capture_staged_candidate()
        .map_err(mv_staging_error)?;
    let staged_source = repository
        .read_private_candidate_file(
            staged.candidate(),
            &logical_path,
            orna_project::ProjectLimits::default().max_source_bytes,
        )
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| mv_staged_source_error("the declaration's source module is not staged"))?;
    // The staged copy gets the same declaration-name edit as the worktree, so
    // the index stays a consistent candidate, as after `git mv`. Other
    // unstaged edits to the module stay unstaged.
    let mut staged_module = modules[module_index].clone();
    staged_module.source = staged_source;
    let staged_modules = std::slice::from_ref(&staged_module);
    let staged_edit = match declaration_name_span(staged_modules, kind, old) {
        Some((_, span)) => Some(renamed_name_source(&staged_module.source, &span, new_leaf)),
        None if declaration_name_span(staged_modules, kind, new).is_some() => None,
        None => {
            return Err(mv_staged_source_error(
                "the staged source module cannot take the rename",
            ));
        }
    };
    let rename = orna_project::ObjectIdentityRename::new(kind, old, new);
    if let Some(original_source) = original_source.as_deref()
        && write_source_atomically(
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
            "the source could not be updated",
            "check source-file permissions and retry `orna mv`",
        ));
    }
    let staged_sources = staged_edit
        .as_deref()
        .map(|updated| (logical_path.as_str(), updated.as_bytes()))
        .into_iter()
        .collect::<Vec<_>>();
    match orna_project::ObjectIdentityManifest::stage_renames(
        repository,
        &modules,
        std::slice::from_ref(&rename),
        &staged_sources,
    ) {
        Ok(Some(_)) => write_success_line(
            &format!(
                "renamed {old} to {new}; the rename and the identity move are staged: review with `git status`, then run `orna commit`"
            ),
            color_enabled,
        ),
        failed => {
            if let Some(original_source) = original_source.as_deref()
                && !restore_source_after_failed_write(
                    &source_path,
                    original_source.as_bytes(),
                    modules[module_index].source.as_bytes(),
                )
            {
                return Err(mv_partial_update_error());
            }
            Err(match failed {
                Err(error) => identity_error(error),
                Ok(_) => identity_error(orna_project::ObjectIdentityError::MissingIdentity),
            })
        }
    }
}

/// The source with the name at `span` replaced by `new_leaf`.
fn renamed_name_source(source: &str, span: &orna_syntax::SyntaxSpan, new_leaf: &str) -> String {
    let mut updated = String::with_capacity(source.len() + new_leaf.len());
    updated.push_str(&source[..span.start]);
    updated.push_str(new_leaf);
    updated.push_str(&source[span.end..]);
    updated
}

#[allow(clippy::too_many_lines)]
/// The module and byte span of the name in the declaration `qualified`
/// names: a table, type, enum or unit declaration, or a table field or key,
/// an enum variant, a type field or a variant payload field.
fn declaration_name_span(
    modules: &[orna_semantic::ModuleInput],
    kind: orna_project::ObjectKind,
    qualified: &str,
) -> Option<(usize, orna_syntax::SyntaxSpan)> {
    use orna_project::ObjectKind;
    use orna_syntax::{Declaration, Keyword, TableMember, TypeMember, TypeRepresentation};
    let segments = qualified.split('.').collect::<Vec<_>>();
    let members = match kind {
        ObjectKind::Table | ObjectKind::Type | ObjectKind::Unit => 0,
        ObjectKind::Field | ObjectKind::Variant | ObjectKind::TypeField => 1,
        ObjectKind::VariantField => 2,
    };
    let declaration_index = segments.len().checked_sub(members + 1)?;
    let namespace = segments[..declaration_index].join(".");
    let declaration = segments[declaration_index];
    let module_index = modules
        .iter()
        .position(|module| module_namespace_matches(&module.logical_path, &namespace))?;
    let source = &modules[module_index].source;
    let parsed = orna_syntax::parse_module(source);
    if !parsed.is_ok() {
        return None;
    }
    let tokens = orna_syntax::lex(source).ok()?;
    // The identifier `name` that follows `keyword` inside `span`, or the first
    // identifier `name` inside `span` when no keyword introduces it.
    let name_in = |span: &orna_syntax::SyntaxSpan, keyword: Option<Keyword>, name: &str| {
        let inside = tokens
            .iter()
            .filter(|token| token.span.start >= span.start && token.span.end <= span.end)
            .collect::<Vec<_>>();
        let start = match keyword {
            Some(keyword) => {
                inside
                    .iter()
                    .position(|token| token.kind == orna_syntax::TokenKind::Keyword(keyword))?
                    + 1
            }
            None => 0,
        };
        inside[start..]
            .iter()
            .find(|token| {
                matches!(
                    &token.kind,
                    orna_syntax::TokenKind::Identifier { normalized } if normalized == name
                )
            })
            .filter(|token| keyword.is_none() || std::ptr::eq(**token, inside[start]))
            .map(|token| token.span.clone())
    };
    let leaf = segments.last()?;
    let span = parsed
        .value
        .items
        .iter()
        .find_map(|item| match (&item.declaration, kind) {
            (Declaration::Table { name, .. }, ObjectKind::Table) if name == declaration => {
                name_in(&item.span, Some(Keyword::Table), name)
            }
            (Declaration::Type { name, .. }, ObjectKind::Type) if name == declaration => {
                name_in(&item.span, Some(Keyword::Type), name)
            }
            (Declaration::Enum { name, .. }, ObjectKind::Type) if name == declaration => {
                name_in(&item.span, Some(Keyword::Enum), name)
            }
            (Declaration::Unit { name, .. }, ObjectKind::Unit) if name == declaration => {
                name_in(&item.span, Some(Keyword::Unit), name)
            }
            (
                Declaration::Table {
                    name,
                    keys,
                    members,
                },
                ObjectKind::Field,
            ) if name == declaration => keys
                .iter()
                .find_map(|key| match &key.pattern {
                    orna_syntax::Pattern::Name(key_name, span) if key_name == leaf => {
                        Some(span.clone())
                    }
                    _ => None,
                })
                .or_else(|| {
                    members.iter().find_map(|member| match member {
                        TableMember::Field { name, span, .. } if name == leaf => {
                            name_in(span, None, name)
                        }
                        _ => None,
                    })
                }),
            (Declaration::Enum { name, variants, .. }, ObjectKind::Variant)
                if name == declaration =>
            {
                variants
                    .iter()
                    .find(|variant| variant.name == *leaf)
                    .and_then(|variant| name_in(&variant.span, None, &variant.name))
            }
            (Declaration::Enum { name, variants, .. }, ObjectKind::VariantField)
                if name == declaration =>
            {
                let variant = variants
                    .iter()
                    .find(|variant| variant.name == segments[declaration_index + 1])?;
                let field = variant.fields.iter().find(|field| field.name == *leaf)?;
                name_in(&field.span, None, &field.name)
            }
            (
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Nominal { members },
                    ..
                },
                ObjectKind::TypeField,
            ) if name == declaration => members.iter().find_map(|member| match member {
                TypeMember::Field { name, span, .. } if name == leaf => name_in(span, None, name),
                _ => None,
            }),
            _ => None,
        })?;
    Some((module_index, span))
}

/// The module source with table `old_leaf` renamed to `new_leaf`, or `None`
/// when it already declares `new_leaf` and no `old_leaf`, as after an
/// interrupted `mv`. The error is the diagnostic title.
fn renamed_table_source(
    source: &str,
    old_leaf: &str,
    new_leaf: &str,
) -> Result<Option<String>, &'static str> {
    let parsed = orna_syntax::parse_module(source);
    if !parsed.is_ok() {
        return Err("table source could not be parsed");
    }
    let tables = |leaf: &str| {
        parsed
            .value
            .items
            .iter()
            .filter(|item| {
                matches!(
                    &item.declaration,
                    orna_syntax::Declaration::Table { name, .. } if name == leaf
                )
            })
            .collect::<Vec<_>>()
    };
    match tables(old_leaf).as_slice() {
        [item] => {
            let name_span = table_declaration_name_span(source, &item.span, old_leaf)
                .ok_or("table declaration name could not be located")?;
            let mut updated = String::with_capacity(
                source.len() - (name_span.end - name_span.start) + new_leaf.len(),
            );
            updated.push_str(&source[..name_span.start]);
            updated.push_str(new_leaf);
            updated.push_str(&source[name_span.end..]);
            Ok(Some(updated))
        }
        [] => match tables(new_leaf).len() {
            1 => Ok(None),
            0 => Err("source table was not found"),
            _ => Err("source table name is ambiguous"),
        },
        _ => Err("source table name is ambiguous"),
    }
}

/// The staged copy of the table's module cannot take the rename: it is not
/// staged, or it does not declare the table under either name.
fn mv_staged_source_error(title: &'static str) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2101",
        "the staged source module cannot take the rename",
        "stage the table's source module with `git add`, then retry mv",
        title.to_owned(),
    )
}

fn split_qualified_name(name: &str) -> Option<(&str, &str)> {
    let (namespace, leaf) = name.rsplit_once('.').unwrap_or(("", name));
    if leaf.is_empty() || name.split('.').any(str::is_empty) || !name.split('.').all(is_identifier)
    {
        return None;
    }
    Some((namespace, leaf))
}

fn is_identifier(value: &str) -> bool {
    let Ok(tokens) = orna_syntax::lex(value) else {
        return false;
    };
    let mut identifiers = tokens
        .iter()
        .filter(|token| !matches!(&token.kind, orna_syntax::TokenKind::Eof));
    let Some(token) = identifiers.next() else {
        return false;
    };
    if identifiers.next().is_some() {
        return false;
    }
    matches!(
        &token.kind,
        orna_syntax::TokenKind::Identifier { normalized }
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
        None if without_extension != "main" => without_extension,
        None => "",
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
    item_span: &orna_syntax::SyntaxSpan,
    expected_name: &str,
) -> Option<orna_syntax::SyntaxSpan> {
    let tokens = orna_syntax::lex(source).ok()?;
    let keyword = tokens.iter().position(|token| {
        token.span.start >= item_span.start
            && token.span.end <= item_span.end
            && token.kind == orna_syntax::TokenKind::Keyword(orna_syntax::Keyword::Table)
    })?;
    let name = tokens.get(keyword.checked_add(1)?)?;
    if !matches!(
        &name.kind,
        orna_syntax::TokenKind::Identifier { normalized } if normalized == expected_name
    ) {
        return None;
    }
    Some(name.span.clone())
}

pub(crate) fn checked_project_source_path(
    root: &Path,
    logical_path: &str,
) -> Result<PathBuf, Diagnostic> {
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

pub(crate) fn write_source_atomically(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
) -> io::Result<()> {
    write_source_atomically_before_replace(path, expected, replacement, || Ok(()))
}

fn write_source_atomically_before_replace(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
    before_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
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
    let mut temp_created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        temp_created = true;
        // Copy only the permission bits: never carry setuid/setgid/sticky onto
        // the replacement source (W62-08).
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt as _;
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o777)
        };
        #[cfg(not(unix))]
        let permissions = metadata.permissions();
        file.set_permissions(permissions)?;
        file.write_all(replacement)?;
        file.sync_all()?;
        drop(file);
        before_replace()?;
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
    if result.is_err() && temp_created {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn restore_source_after_failed_write(
    path: &Path,
    original: &[u8],
    updated: &[u8],
) -> bool {
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

/// The index stages no object identities yet: a database before its first
/// `orna commit` has no inventory, and a rename without identity metadata
/// would be a delete plus create (ORNA-OBJECT-004).
fn mv_identity_missing_error() -> Diagnostic {
    Diagnostic::target(
        "E2101",
        "no staged object identities record the table",
        "commit the table with `orna commit -m MESSAGE` before using mv",
    )
}

/// The staged inventory could not be captured or restaged; the source is
/// unchanged.
fn mv_staging_error(error: RelationAdmissionError) -> Diagnostic {
    use orna_repository::RepositoryError;
    match error {
        RelationAdmissionError::EmptyIndex => mv_identity_missing_error(),
        RelationAdmissionError::SourceNotStaged { .. } => {
            mv_staged_source_error("the table's source module is not staged")
        }
        RelationAdmissionError::StaleIndex => Diagnostic::target(
            "E2100",
            "the index changed while mv was staging object identities",
            "review the staged changes, then retry `mv`",
        ),
        RelationAdmissionError::Repository(RepositoryError::PublicationPending) => {
            Diagnostic::target(
                "E2100",
                "an interrupted publication has not been recovered",
                "run `orna publish` to finish recovery first, then retry `mv`",
            )
        }
        other => Diagnostic::target_with_detail(
            "E2100",
            "staged object identities could not be updated",
            "restore the staged `.orna/store`, then retry `mv`",
            other.to_string(),
        ),
    }
}

fn mv_partial_update_error() -> Diagnostic {
    Diagnostic::target(
        "E2100",
        "table rename could not be fully rolled back",
        "inspect the source module and the staged `.orna/store` before retrying `mv`",
    )
}

/// Names why the migration source failed format admission. The title carries
/// the `FormatContextError` message, so the cause is user-visible; the stable
/// code is kept in the detail.
#[allow(clippy::needless_pass_by_value)]
fn migration_source_admission_diagnostic(error: orna_repository::FormatContextError) -> Diagnostic {
    let title = format!("migration source could not be admitted: {error}");
    Diagnostic {
        code: "E2100",
        title: std::borrow::Cow::Owned(title),
        help: "verify the source worktree and retry with the same migration coordinates",
        exit: Exit::Target,
        detail: Some(format!("{} ({error:?})", error.code())),
        body: None,
    }
}

#[allow(clippy::too_many_lines)]
fn run_migrate(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let usage = || {
        Diagnostic::usage(
            "E1002",
            "unsupported `migrate` arguments",
            "use `migrate copy DEST [--annotation-defaults N] --no-persistent-consumers` or `migrate abort DEST`",
        )
    };
    let mut arguments = arguments.iter().map(String::as_str);
    let subcommand = arguments.next();
    if subcommand == Some("abort") {
        return run_migrate_abort(endpoint, arguments);
    }
    if subcommand != Some("copy") {
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
            "supply `--no-persistent-consumers` to acknowledge that persistent consumer checkpoints are carried under recorded identity correspondence",
        ));
    }
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "legacy migration source could not be opened",
            "run the command from a local format-1 or format-2 worktree",
            format!("{error:?}"),
        )
    })?;
    let prepared = prepare_migration_copy(
        &repository,
        std::path::Path::new(destination),
        annotation_defaults,
    )?;
    print_prepared_copy(&prepared);
    Ok(())
}

fn print_prepared_copy(prepared: &PreparedMigrationCopy) {
    let copy = &prepared.copy;
    println!(
        "prepared or recovered format-3 migration copy: source_format={} source_commit={} candidate_commit={} destination={} carried_checkpoints={} carried_history={}",
        copy.source_format_number(),
        copy.source_commit().as_str(),
        copy.candidate_journal().new_head().as_str(),
        copy.destination().display(),
        prepared.checkpoints,
        prepared.history,
    );
}

/// A prepared or recovered format-3 migration copy and the stream state carried
/// into it. The one-step upgrade publishes `copy` once its candidate is confirmed.
struct PreparedMigrationCopy {
    copy: orna_repository::migration_copy::MigratedCopy,
    checkpoints: usize,
    history: usize,
}

/// Prepares (or recovers) the format-3 migration copy of `repository` at
/// `destination`, carrying its persistent stream checkpoints. Nothing is
/// published here; activation is a separate step.
// A declared count is an assertion against the count the decoded legacy rows
// carry; an omitted one is the decoded count itself (ORNA-UPGRADE-004).
#[allow(clippy::too_many_lines)]
fn prepare_migration_copy(
    repository: &orna_repository::Repository,
    destination: &std::path::Path,
    annotation_defaults: Option<u32>,
) -> Result<PreparedMigrationCopy, Diagnostic> {
    let declared_annotation_defaults = annotation_defaults
        .map(orna_repository::MigrationAnnotationDefaults::new)
        .transpose()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration annotation metadata was invalid",
                "supply the count derived from decoded legacy metadata, or omit the flag",
                format!("{error:?}"),
            )
        })?;
    // A source whose runtime holds unpublished mutations has rows the copy would
    // not receive, while its stream checkpoints may already cover them. Refuse
    // before any destination is created so the source keeps its tail (ORNA-UPGRADE-006).
    refuse_unpublished_tail(repository)?;
    // Persistent stream checkpoints are carried into the prepared destination
    // under the identity correspondence recorded there before any row is
    // imported. Live per-attempt stream state is not carried: a source holding
    // it refuses before a copy is built and its state is left unchanged.
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "migration source runtime could not be inspected",
                "retry `migrate copy` once the local runtime can start",
            )
        })?;
    let carry = source_runtime
        .block_on(orna_runtime::read_stream_carry(repository))
        .map_err(|error| match error {
            orna_runtime::StreamCarryError::NotCarriable(tables) => {
                Diagnostic::target_with_detail(
                    "E2100",
                    "migration source holds live per-attempt stream state",
                    "let the live stream attempts finish or be cancelled, then retry; no copy was prepared and the source is unchanged",
                    format!("tables={}", tables.join(", ")),
                )
            }
            orna_runtime::StreamCarryError::Runtime(error) => Diagnostic::target_with_detail(
                "E2100",
                "migration source stream state could not be read",
                "retry `migrate copy` once the source runtime can be read; no copy was prepared",
                format!("{error:?}"),
            ),
        })?;
    let continuity = carry.continuity_record().map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "migration consumer continuity could not be represented",
            "retry with the migration continuity inventory available",
            format!("{error:?}"),
        )
    })?;
    let copy = orna_repository::migration_copy::prepare_format3_migration(
        repository,
        destination,
        continuity,
        declared_annotation_defaults,
        &|staged: &orna_repository::Repository,
          database: orna_repository::DatabaseId|
         -> Result<
            orna_repository::migration_copy::MigrationStore,
            orna_repository::RepositoryError,
        > {
            let store = orna_application::build_migration_store(staged, database)?;
            // The declared count is an assertion: it must equal the Blob values the
            // decoded rows carry. A mismatch refuses before any candidate is built.
            if let Some(declared) = declared_annotation_defaults {
                let decoded = store.annotation_default_entries;
                if declared.entry_count() != decoded {
                    return Err(
                        orna_repository::RepositoryError::MigrationAnnotationDefaultsMismatch {
                            declared: declared.entry_count(),
                            decoded,
                        },
                    );
                }
            }
            // The store was built from a source that could still move. A source
            // that gained unpublished runtime mutations in that window keeps its
            // tail: refuse before the candidate is built or pinned.
            let unpublished = crate::source_unpublished_mutation_count(repository)
                .map_err(|_| orna_repository::RepositoryError::PublicationRecoveryConflict)?;
            if unpublished != 0 {
                return Err(orna_repository::RepositoryError::PublicationRecoveryConflict);
            }
            Ok(store)
        },
    )
    .map_err(|error| match error {
        orna_repository::RepositoryError::FormatContext(context) => {
            migration_source_admission_diagnostic(context)
        }
        orna_repository::RepositoryError::LegacyMigrationCopy => Diagnostic::target(
            "E2100",
            "prepared migration copy predates the migration-record removal",
            "discard it with `orna migrate abort DEST`, then run `orna migrate copy` again",
        ),
        orna_repository::RepositoryError::Format2CompactUnimplemented => Diagnostic::target(
            "E2100",
            "format-2 source holds compact-storage-v2 tables, which have no decoder yet",
            "see MIGRATION.md and bead ornadb-5il1.486; no copy was prepared and the source is unchanged",
        ),
        orna_repository::RepositoryError::MigrationAnnotationDefaultsMismatch { declared, decoded } => {
            Diagnostic::target_with_detail(
                "E2100",
                "declared annotation defaults do not match the decoded legacy rows",
                "remove `--annotation-defaults` to use the decoded count, or pass the decoded Blob count; no copy was prepared and the source is unchanged",
                format!("declared={declared} decoded={decoded}"),
            )
        }
        other => Diagnostic::target_with_detail(
            "E2100",
            "migration copy could not be prepared or recovered",
            "verify the source worktree and retry with the same migration coordinates",
            format!("{other:?}"),
        ),
    })?;
    let destination_repository = orna_repository::Repository::discover(copy.destination())
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration destination could not be opened",
                "verify the prepared destination and retry with the same migration coordinates",
                format!("{error:?}"),
            )
        })?;
    let recorded_journal = destination_repository
        .read_publication_journal()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration destination journal could not be read",
                "verify the prepared destination and retry with the same migration coordinates",
                format!("{error:?}"),
            )
        })?;
    let recorded_continuity = recorded_journal
        .as_ref()
        .and_then(|journal| journal.migration_continuity());
    source_runtime
        .block_on(orna_runtime::import_stream_carry(
            &destination_repository,
            &carry,
            recorded_continuity,
        ))
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration stream checkpoints could not be carried",
                "verify the destination continuity record; a destination already holding different carried checkpoints is refused and no checkpoint was imported",
                format!("{error:?}"),
            )
        })?;
    // Boundary: the carry is committed in the destination runtime and the
    // command has not yet reported it. A restart re-imports the same carry,
    // which the import recognises as already present.
    orna_repository::publication_crash_barrier("migration-carried");
    Ok(PreparedMigrationCopy {
        copy,
        checkpoints: carry.checkpoint_count(),
        history: carry.history_count(),
    })
}

fn run_migrate_abort<'a>(
    endpoint: &Endpoint,
    mut arguments: impl Iterator<Item = &'a str>,
) -> Result<(), Diagnostic> {
    let destination = arguments.next().ok_or_else(|| {
        Diagnostic::usage(
            "E1001",
            "`migrate abort` needs a destination",
            "supply DEST after `migrate abort`",
        )
    })?;
    if arguments.next().is_some() {
        return Err(Diagnostic::usage(
            "E1002",
            "unsupported `migrate abort` arguments",
            "use `migrate abort DEST`",
        ));
    }
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "legacy migration source could not be opened",
            "run the command from a local format-1 or format-2 worktree",
            format!("{error:?}"),
        )
    })?;
    let destination = std::path::PathBuf::from(destination);
    let outcome = orna_repository::migration_copy::discard_prepared_format3_migration(
        &repository,
        &destination,
    )
    .map_err(|error| match error {
        orna_repository::RepositoryError::MigrationCopyAdvanced => Diagnostic::target(
            "E2100",
            "migration copy has begun activation and cannot be discarded",
            "finish activation with `orna upgrade --to-format 3 DEST --yes-activate`; nothing was removed",
        ),
        other => Diagnostic::target_with_detail(
            "E2100",
            "migration copy could not be discarded",
            "verify that DEST is the prepared copy of this source; the source worktree is unchanged",
            format!("{other:?}"),
        ),
    })?;
    match outcome {
        orna_repository::migration_copy::DiscardOutcome::Discarded => println!(
            "discarded prepared format-3 migration copy: destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::DiscardOutcome::NothingToDiscard => println!(
            "nothing to discard: no prepared format-3 migration copy at destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::DiscardOutcome::AlreadyActivated => println!(
            "nothing discarded: destination is an activated format-3 repository, destination={}",
            destination.display()
        ),
    }
    Ok(())
}

/// `orna upgrade --to-format 3`, in two forms.
///
/// Without DEST, the one-step form prepares the candidate in a disposable copy
/// beside the worktree, prints its review, and publishes it only after explicit
/// confirmation of that candidate. Publication swaps the activated copy in for
/// the worktree path and leaves the old snapshot at the copy's path.
///
/// With DEST and `--yes-activate`, the two-step form publishes a copy that
/// `orna migrate copy DEST` already prepared. The source is never changed.
fn run_upgrade(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let arguments = parse_upgrade_arguments(arguments)?;
    match arguments.destination {
        Some(destination) => run_upgrade_prepared(endpoint, destination, arguments.confirm),
        None => run_upgrade_in_place(endpoint, &arguments),
    }
}

/// `orna upgrade --to-format 3 DEST --yes-activate [--confirm ID]`: activates the
/// prepared format-3 copy at DEST by publishing its candidate. Without a matching
/// `--confirm` the candidate id is still printed and a warning is written.
fn run_upgrade_prepared(
    endpoint: &Endpoint,
    destination: &str,
    confirm: Option<&str>,
) -> Result<(), Diagnostic> {
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "upgrade source could not be opened",
            "run the command from the local format-1 or format-2 worktree that the copy was prepared from",
            format!("{error:?}"),
        )
    })?;
    let destination = std::path::PathBuf::from(destination);
    let candidate =
        orna_repository::migration_copy::prepared_candidate(&destination).map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "prepared migration copy could not be read for activation",
                "verify that DEST is the prepared copy of this source; nothing was published",
                format!("{error:?}"),
            )
        })?;
    match confirm {
        Some(id) if id != candidate.as_str() => {
            return Err(Diagnostic::target_with_detail(
                "E2100",
                "confirmation names a different candidate than the prepared copy",
                "re-run with the candidate_commit printed by the review; nothing was published",
                format!("confirmed={id} candidate={}", candidate.as_str()),
            ));
        }
        Some(_) => {}
        None => eprintln!(
            "warning: publishing candidate_commit={} without `--confirm {}`; a confirmed run binds activation to the reviewed candidate",
            candidate.as_str(),
            candidate.as_str(),
        ),
    }
    eprintln!(
        "reviewed prepared candidate: candidate_commit={} destination={}",
        candidate.as_str(),
        destination.display()
    );
    match activate_prepared(&repository, &destination, &candidate)? {
        orna_repository::migration_copy::ActivationOutcome::Activated => println!(
            "activated format-3 migration copy: destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::ActivationOutcome::AlreadyActivated => println!(
            "already activated format-3 migration copy: destination={}",
            destination.display()
        ),
    }
    Ok(())
}

/// Publishes `candidate` at `destination`. Both forms of `orna upgrade` share it.
fn activate_prepared(
    repository: &orna_repository::Repository,
    destination: &std::path::Path,
    candidate: &orna_repository::GitCommitRef,
) -> Result<orna_repository::migration_copy::ActivationOutcome, Diagnostic> {
    let store_for = |staged: &orna_repository::Repository,
                     database: orna_repository::DatabaseId| {
        activation_store(repository, staged, database)
    };
    orna_repository::migration_copy::activate_prepared_format3_migration(
        repository,
        destination,
        candidate,
        crate::invocation_owner_id(),
        &store_for,
    )
    .map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "prepared migration copy could not be activated",
            "verify that DEST is the prepared copy of this source and re-run the same upgrade to resume; the source worktree is unchanged",
            format!("{error:?}"),
        )
    })
}

/// `orna upgrade --to-format 3 [--annotation-defaults N] --no-persistent-consumers
/// [--confirm ID]`: the one-step form. It prepares the candidate in a sibling
/// `<worktree>-format3-candidate` copy, reviews it, and publishes it in place only
/// after the candidate is confirmed. A refused confirmation keeps the prepared copy
/// (abort it with `orna migrate abort`) and leaves the worktree untouched.
fn run_upgrade_in_place(
    endpoint: &Endpoint,
    arguments: &UpgradeArguments<'_>,
) -> Result<(), Diagnostic> {
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "upgrade source could not be opened",
            "run the command from the local format-1 or format-2 worktree to upgrade",
            format!("{error:?}"),
        )
    })?;
    let root = repository.worktree().canonicalize().map_err(|_| {
        Diagnostic::target(
            "E2100",
            "upgrade source worktree could not be resolved",
            "run the command from the local format-1 or format-2 worktree to upgrade",
        )
    })?;
    let git_metadata = std::fs::symlink_metadata(root.join(".git")).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "upgrade source has no Git directory",
            "run the command from a standalone worktree whose `.git` is a directory; nothing was changed",
        )
    })?;
    if !git_metadata.is_dir() {
        return Err(Diagnostic::target(
            "E2100",
            "in-place upgrade needs a standalone worktree",
            "run it from the worktree whose `.git` is a directory; a linked worktree is refused and nothing was changed",
        ));
    }
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return Err(Diagnostic::target(
            "E2100",
            "upgrade source worktree has no parent directory",
            "move the worktree under a directory the candidate copy can be created in; nothing was changed",
        ));
    };
    let destination = parent.join(format!("{}-format3-candidate", name.to_string_lossy()));
    if repository.open_format_context().is_ok_and(|context| {
        context.repository_format() == orna_repository::RepositoryFormat::Format3
    }) {
        return Err(Diagnostic::target(
            "E2100",
            "in-place upgrade already completed; nothing changed",
            "the worktree is already format-3; the sibling copy holds the previous snapshot; nothing was changed",
        ));
    }
    let activated = orna_repository::migration_copy::finished_activation_head(&destination)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "prepared migration copy could not be inspected",
                "remove the sibling copy with `orna migrate abort` and re-run; the worktree is unchanged",
                format!("{error:?}"),
            )
        })?;
    // A finished activation whose exchange did not complete is resumed. Its
    // candidate is re-derived from the source in a verification copy, and only
    // a source that reproduces the activated candidate may publish it.
    let review_destination = if activated.is_some() {
        parent.join(format!("{}-format3-verify", name.to_string_lossy()))
    } else {
        destination.clone()
    };
    let prepared = prepare_migration_copy(
        &repository,
        &review_destination,
        arguments.annotation_defaults,
    )?;
    print_prepared_copy(&prepared);
    let candidate = prepared.copy.candidate_journal().new_head().clone();
    print_in_place_review(&root, &destination, activated.is_some());
    confirm_and_activate(
        &repository,
        &candidate,
        activated.as_ref(),
        &destination,
        &review_destination,
        arguments.confirm,
    )?;
    // The exchange is the last point the source can still move: refuse a tail
    // that appeared since the copy was prepared, leaving the worktree untouched.
    refuse_unpublished_tail(&repository)?;
    orna_repository::migration_copy::exchange_activated_copy(
        &repository,
        &prepared.copy,
        &destination,
    )
    .map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "activated candidate could not be published in place",
            "the worktree is unchanged because the exchange is atomic; the activated copy is kept at destination; re-run the same command to resume the exchange",
            format!("{error:?}"),
        )
    })?;
    println!(
        "published format-3 repository in place: worktree={} candidate_commit={} rollback_snapshot={}",
        root.display(),
        candidate.as_str(),
        destination.display()
    );
    Ok(())
}

/// Prints the resume note and the publication review: the candidate replaces
/// the worktree, and the old snapshot stays at the destination as the rollback
/// copy until it is removed.
fn print_in_place_review(root: &std::path::Path, destination: &std::path::Path, resumed: bool) {
    if resumed {
        println!(
            "resume: destination={} is already activated; the candidate re-derived from the source is verified against it before the exchange",
            destination.display()
        );
    }
    println!(
        "review: publication replaces worktree={} with the activated candidate; the old snapshot stays at destination={} as the rollback copy",
        root.display(),
        destination.display()
    );
    println!(
        "review: abort before publication with `orna migrate abort {}`; the worktree is unchanged until then",
        destination.display()
    );
}

/// Confirms the reviewed candidate and activates it in the sibling copy. A
/// resumed run already has an activated sibling, so after the confirmation it
/// only discards its verification copy.
fn confirm_and_activate(
    repository: &orna_repository::Repository,
    candidate: &orna_repository::GitCommitRef,
    activated: Option<&orna_repository::GitCommitRef>,
    destination: &std::path::Path,
    review_destination: &std::path::Path,
    confirm: Option<&str>,
) -> Result<(), Diagnostic> {
    let confirmed = match activated {
        Some(head) if head != candidate => Err(Diagnostic::target_with_detail(
            "E2100",
            "activated sibling was built from a different candidate",
            "the source changed since the sibling was activated; inspect and remove the sibling copy by hand, then re-run; the worktree is unchanged",
            format!(
                "activated={} candidate={}",
                head.as_str(),
                candidate.as_str()
            ),
        )),
        _ => confirm_candidate(confirm, candidate),
    };
    if let Err(error) = confirmed {
        if activated.is_some() {
            // The verification copy is not the activated sibling; removing it
            // leaves the sibling in place for the next run.
            let _ = discard_verification_copy(repository, review_destination);
        }
        return Err(error);
    }
    if activated.is_none() {
        refuse_unpublished_tail(repository)?;
        activate_prepared(repository, destination, candidate)?;
    } else {
        discard_verification_copy(repository, review_destination)?;
    }
    Ok(())
}

/// Confirms the reviewed candidate: `--confirm` must name it, or an interactive
/// terminal must type it back.
fn confirm_candidate(
    confirm: Option<&str>,
    candidate: &orna_repository::GitCommitRef,
) -> Result<(), Diagnostic> {
    match confirm {
        Some(id) if id != candidate.as_str() => Err(Diagnostic::target_with_detail(
            "E2100",
            "confirmation names a different candidate than the reviewed one",
            "re-run with the candidate_commit printed above; the worktree is unchanged and the sibling copy is kept",
            format!("confirmed={id} candidate={}", candidate.as_str()),
        )),
        Some(_) => Ok(()),
        None if confirm_interactively(candidate.as_str())? => Ok(()),
        None => Err(Diagnostic::target_with_detail(
            "E2100",
            "publication was not confirmed",
            "re-run with `--confirm <candidate_commit>` shown above, or confirm at an interactive terminal; the worktree is unchanged and the sibling copy is kept",
            format!("candidate={}", candidate.as_str()),
        )),
    }
}

/// Refuses while the source holds unpublished runtime mutations: its rows would
/// miss them while its stream checkpoints may already cover them (ORNA-UPGRADE-006).
fn refuse_unpublished_tail(repository: &orna_repository::Repository) -> Result<(), Diagnostic> {
    let unpublished_count = crate::source_unpublished_mutation_count(repository)?;
    if unpublished_count != 0 {
        return Err(Diagnostic::target_with_detail(
            "E2100",
            "migration source holds unpublished runtime mutations",
            "make the source's unpublished runtime mutations durable where its HEAD allows a publish, or discard them, then retry; the source is unchanged",
            format!("unpublished_mutations={unpublished_count}"),
        ));
    }
    Ok(())
}

/// Removes the verification copy a resumed run prepared; the activated sibling
/// and the worktree are not touched.
fn discard_verification_copy(
    repository: &orna_repository::Repository,
    verification: &std::path::Path,
) -> Result<(), Diagnostic> {
    let detail = match orna_repository::migration_copy::discard_prepared_format3_migration(
        repository,
        verification,
    ) {
        Ok(orna_repository::migration_copy::DiscardOutcome::Discarded) => return Ok(()),
        Ok(outcome) => format!("outcome={outcome:?}"),
        Err(error) => format!("{error:?}"),
    };
    Err(Diagnostic::target_with_detail(
        "E2100",
        "verification copy could not be removed",
        "remove the verification copy with `orna migrate abort` and re-run; the worktree and the activated sibling are unchanged",
        detail,
    ))
}

/// Asks for the candidate commit id on a terminal and reports whether it was
/// typed exactly. Without a terminal on both stdin and stderr nothing is asked.
fn confirm_interactively(candidate: &str) -> Result<bool, Diagnostic> {
    use std::io::{BufRead, IsTerminal, Write};
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(false);
    }
    let prompt_failed = || {
        Diagnostic::target(
            "E2100",
            "confirmation prompt could not be read",
            "re-run with `--confirm <candidate_commit>`; nothing was changed",
        )
    };
    eprint!("Type the candidate_commit above to publish in place, or press Enter to abort: ");
    std::io::stderr().flush().map_err(|_| prompt_failed())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| prompt_failed())?;
    Ok(line.trim() == candidate)
}

/// The parsed `orna upgrade --to-format 3` arguments.
struct UpgradeArguments<'a> {
    destination: Option<&'a str>,
    confirm: Option<&'a str>,
    annotation_defaults: Option<u32>,
}

/// Parses both forms of `orna upgrade --to-format 3`. Every refusal happens
/// before the repository is opened, so nothing is changed.
fn parse_upgrade_arguments(arguments: &[String]) -> Result<UpgradeArguments<'_>, Diagnostic> {
    let mut words = arguments.iter().map(String::as_str);
    let mut format_three = false;
    let mut yes_activate = false;
    let mut no_persistent_consumers = false;
    let mut confirm = None;
    let mut annotation_defaults = None;
    let mut destination = None;
    while let Some(word) = words.next() {
        match word {
            "--to-format" if !format_three => {
                let format = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--to-format` needs a format",
                        "supply `--to-format 3` after `upgrade`",
                    )
                })?;
                if format != "3" {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "unsupported `--to-format` target",
                        "only `--to-format 3` is supported; nothing was changed",
                    ));
                }
                format_three = true;
            }
            "--yes-activate" if !yes_activate => yes_activate = true,
            "--no-persistent-consumers" if !no_persistent_consumers => {
                no_persistent_consumers = true;
            }
            "--confirm" if confirm.is_none() => {
                confirm = Some(words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--confirm` needs a candidate commit id",
                        "supply the candidate_commit printed by the review after `--confirm`",
                    )
                })?);
            }
            "--annotation-defaults" if annotation_defaults.is_none() => {
                let value = words.next().ok_or_else(|| {
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
            _ if destination.is_none() && !word.starts_with("--") => destination = Some(word),
            _ => {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unsupported `upgrade` arguments",
                    "use `upgrade --to-format 3 [--annotation-defaults N] --no-persistent-consumers [--confirm ID]`, or `upgrade --to-format 3 DEST --yes-activate [--confirm ID]`; nothing was changed",
                ));
            }
        }
    }
    if !format_three {
        return Err(Diagnostic::usage(
            "E1001",
            "`upgrade` needs `--to-format 3`",
            "supply `--to-format 3` after `upgrade`; nothing was changed",
        ));
    }
    check_upgrade_form(
        destination,
        yes_activate,
        annotation_defaults,
        no_persistent_consumers,
    )?;
    Ok(UpgradeArguments {
        destination,
        confirm,
        annotation_defaults,
    })
}

/// Refuses an `upgrade --to-format 3` argument shape that belongs to neither
/// form. Runs after the arguments are parsed and before the repository is
/// opened, so a refusal changes nothing.
fn check_upgrade_form(
    destination: Option<&str>,
    yes_activate: bool,
    annotation_defaults: Option<u32>,
    no_persistent_consumers: bool,
) -> Result<(), Diagnostic> {
    if destination.is_some() {
        if !yes_activate {
            return Err(Diagnostic::usage(
                "E1002",
                "`upgrade` activates the prepared copy only with `--yes-activate`",
                "re-run with `--yes-activate` to publish the prepared candidate into DEST; nothing was changed",
            ));
        }
        if annotation_defaults.is_some() || no_persistent_consumers {
            return Err(Diagnostic::usage(
                "E1002",
                "`--annotation-defaults` and `--no-persistent-consumers` belong to the preparation",
                "the prepared copy already records them; pass them to `orna migrate copy DEST` or to the one-step `orna upgrade --to-format 3`; nothing was changed",
            ));
        }
    } else {
        if yes_activate {
            return Err(Diagnostic::usage(
                "E1002",
                "`--yes-activate` needs a DEST",
                "the one-step form publishes after `--confirm` or an interactive confirmation; nothing was changed",
            ));
        }
        if !no_persistent_consumers {
            return Err(Diagnostic::usage(
                "E1001",
                "`upgrade --to-format 3` needs an explicit consumer-continuity choice",
                "supply `--no-persistent-consumers` to acknowledge that persistent consumer checkpoints are carried under recorded identity correspondence",
            ));
        }
    }
    Ok(())
}

/// The store a prepared copy must match: exactly what a fresh preparation of
/// `source` builds. The source may move after preparation, so this runs on
/// every re-derivation and refuses, before the copy's ref moves, while the
/// source holds unpublished runtime mutations.
fn activation_store(
    source: &orna_repository::Repository,
    staged: &orna_repository::Repository,
    database: orna_repository::DatabaseId,
) -> Result<orna_repository::migration_copy::MigrationStore, orna_repository::RepositoryError> {
    let store = orna_application::build_migration_store(staged, database)?;
    let unpublished = crate::source_unpublished_mutation_count(source)
        .map_err(|_| orna_repository::RepositoryError::PublicationRecoveryConflict)?;
    if unpublished != 0 {
        return Err(orna_repository::RepositoryError::PublicationRecoveryConflict);
    }
    Ok(store)
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
mod mv_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn source_writer_rejects_stale_expected_bytes_without_creating_a_temp_file() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"current").unwrap();

        let error = write_source_atomically(&source, b"stale", b"replacement").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&source).unwrap(), b"current");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".orna-mv-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn source_writer_refuses_symlinks_without_changing_the_target() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let target = directory.path().join("target.orna");
        let link = directory.path().join("main.orna");
        fs::write(&target, b"target").unwrap();
        symlink(&target, &link).unwrap();

        assert!(write_source_atomically(&link, b"target", b"replacement").is_err());

        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// W62-08: the replacement source must not inherit setuid/setgid/sticky.
    #[cfg(unix)]
    #[test]
    fn source_writer_drops_setuid_setgid_and_sticky_from_the_replaced_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"before").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o7755)).unwrap();

        write_source_atomically(&source, b"before", b"after").unwrap();

        assert_eq!(fs::read(&source).unwrap(), b"after");
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }

    #[test]
    fn source_writer_cleans_its_temp_file_when_the_expected_source_changes() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"before").unwrap();

        let error =
            write_source_atomically_before_replace(&source, b"before", b"replacement", || {
                fs::write(&source, b"external change")
            })
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&source).unwrap(), b"external change");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".orna-mv-")
        }));
    }

    #[test]
    fn source_rollback_restores_only_known_states() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"original").unwrap();

        assert!(restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        fs::write(&source, b"updated").unwrap();
        assert!(restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        fs::write(&source, b"concurrent").unwrap();
        assert!(!restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        assert_eq!(fs::read(&source).unwrap(), b"concurrent");
    }
}
