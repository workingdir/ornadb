//! `orna mv OLD NEW`: rename one declaration in source and move its identity in
//! the staged inventory.

use super::*;
use crate::cli_source_edit::{
    checked_project_source_path, restore_source_after_failed_write, write_source_atomically,
};
use orna_repository::relation_admission::{RelationAdmissionError, StagedCandidate};

/// The worktree source after the declaration-name edit, with the bytes a later
/// failure must put back. A run interrupted after the write leaves the
/// worktree already renamed, so it has no bytes to write or restore.
struct SourceEdit<'a> {
    path: std::path::PathBuf,
    before: Option<&'a str>,
    after: &'a str,
}

impl SourceEdit<'_> {
    /// Writes the renamed source. A failed write restores the previous bytes;
    /// when even that restore fails the rename is reported as partially
    /// applied, never as unchanged.
    fn write(&self, title: &'static str, help: &'static str) -> Result<(), Diagnostic> {
        let Some(before) = self.before else {
            return Ok(());
        };
        if write_source_atomically(&self.path, before.as_bytes(), self.after.as_bytes()).is_ok() {
            return Ok(());
        }
        if !restore_source_after_failed_write(&self.path, before.as_bytes(), self.after.as_bytes())
        {
            return Err(mv_partial_update_error());
        }
        Err(Diagnostic::target("E2100", title, help))
    }

    /// Restores the previous source after the inventory update failed. A
    /// restore that cannot prove the bytes are still ours is a partially
    /// applied rename.
    fn roll_back(&self) -> Result<(), Diagnostic> {
        let Some(before) = self.before else {
            return Ok(());
        };
        if restore_source_after_failed_write(&self.path, before.as_bytes(), self.after.as_bytes()) {
            return Ok(());
        }
        Err(mv_partial_update_error())
    }
}

/// The staged copy of `logical_path`, which the index update rewrites with the
/// same declaration-name edit as the worktree. A module that is not staged, or
/// whose bytes are not text, cannot take the rename.
fn staged_module_source(
    repository: &orna_repository::Repository,
    staged: &StagedCandidate,
    logical_path: &str,
    title: &'static str,
) -> Result<String, Diagnostic> {
    repository
        .read_private_candidate_file(
            staged.candidate(),
            logical_path,
            orna_project::ProjectLimits::default().max_source_bytes,
        )
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| mv_staged_source_error(title))
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
pub(super) fn run_mv(
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
    let staged_source = staged_module_source(
        &repository,
        &staged,
        &logical_path,
        "the table's source module is not staged",
    )?;
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

    let source_edit = SourceEdit {
        path: source_path,
        before: original_source.as_deref(),
        after: &modules[module_index].source,
    };
    source_edit.write(
        "table source could not be updated",
        "check source-file permissions and retry mv",
    )?;

    let staged_sources = staged_rename
        .as_deref()
        .map(|updated| (logical_path.as_str(), updated.as_bytes()))
        .into_iter()
        .collect::<Vec<_>>();
    if let Err(error) =
        repository.stage_object_identities(&staged, &next.to_stored(), &staged_sources)
    {
        source_edit.roll_back()?;
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
    let staged_source = staged_module_source(
        repository,
        &staged,
        &logical_path,
        "the declaration's source module is not staged",
    )?;
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
    let source_edit = SourceEdit {
        path: source_path,
        before: original_source.as_deref(),
        after: &modules[module_index].source,
    };
    source_edit.write(
        "the source could not be updated",
        "check source-file permissions and retry `orna mv`",
    )?;
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
            source_edit.roll_back()?;
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
