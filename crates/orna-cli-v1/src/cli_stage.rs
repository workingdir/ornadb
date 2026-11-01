//! Row-scoped staging against the ordinary Git index.

use std::borrow::Cow;

use super::{Diagnostic, Endpoint, Exit, local_project_path};
use orna_repository_v1::{FormatContextError, ManagedPath, Repository, RepositoryFormat};

fn error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2300",
        title,
        "run this command inside a local Orna Git worktree",
        detail,
    )
}

pub(super) fn repository(endpoint: &Endpoint) -> Result<Repository, Diagnostic> {
    let root = local_project_path(endpoint)?;
    Repository::discover(root)
        .map_err(|e| error("Git worktree could not be discovered", format!("{e:?}")))
}

fn matches_key_path(components: &[String], key: &str) -> bool {
    let requested = key.split('/');
    if components.len() != requested.clone().count() {
        return false;
    }
    components
        .iter()
        .zip(requested)
        .enumerate()
        .all(|(index, (component, expected))| {
            if index + 1 == components.len() {
                component.strip_suffix(".orna") == Some(expected)
            } else {
                component == expected
            }
        })
}

/// Normalises a KEY_TEXT argument: a JSON string literal is decoded, any other
/// text is taken verbatim.
fn key_text(key: &str) -> String {
    serde_json::from_str::<String>(key).unwrap_or_else(|_| key.to_owned())
}

/// Resolves one logical worktree row through the project loader and passes its
/// logical path and captured source bytes to `action`. `key` is already
/// normalised by `key_text`.
fn with_worktree_row<T>(
    endpoint: &Endpoint,
    table: &str,
    key: &str,
    action: impl FnOnce(&Repository, &str, &[u8]) -> Result<T, Diagnostic>,
) -> Result<T, Diagnostic> {
    let repository = repository(endpoint)?;
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|e| error("project rows could not be loaded", format!("{e:?}")))?;
    let mut candidates = project
        .loose_rows()
        .iter()
        .filter(|row| row.table_path() == table && matches_key_path(row.key_path(), key));
    let candidate = candidates.next().ok_or_else(|| {
        error(
            "logical row was not found",
            format!("no worktree row matches table {table:?} and key path {key:?}"),
        )
    })?;
    if candidates.next().is_some() {
        return Err(error(
            "logical row is ambiguous",
            format!("more than one worktree row matches table {table:?} and key path {key:?}"),
        ));
    }
    action(&repository, candidate.logical_path(), candidate.source_bytes())
}

/// Refuses to stage a row file into a repository whose rows are not files.
///
/// Rows have no physical Git paths in format 3: they live only in
/// `.orna/store` (ORNA-REPO-004), and the row adapter stages a row into the
/// index store (ORNA-STAGE-001), which this build cannot do yet. A loose row
/// file staged into a format-3 index would make the next commit mix formats
/// (ORNA-META-001; Gate A, "unknown/mixed formats fail early"), so it is
/// refused by path before the index is touched, as is staging into a `HEAD`
/// whose metadata does not admit. Only a format-1/2 repository, or a tree
/// without Orna metadata, keeps row files.
fn require_row_file_layout(repository: &Repository, logical: &str) -> Result<(), Diagnostic> {
    let format3 = match repository
        .head()
        .map_err(|e| error("Git HEAD could not be inspected", format!("{e:?}")))?
    {
        Some(_) => match repository.open_format_context() {
            Ok(context) => context.repository_format() == RepositoryFormat::Format3,
            Err(FormatContextError::MetadataUnavailable) => false,
            Err(refusal) => {
                return Err(Diagnostic {
                    code: refusal.code(),
                    title: Cow::Owned(format!("row {logical} cannot be staged: {refusal}")),
                    help: "repair the committed `.orna` metadata, then retry; nothing was staged",
                    exit: Exit::Target,
                    detail: None,
                    body: None,
                });
            }
        },
        // An unborn branch has no committed format yet: its first commit
        // takes the format-3 metadata `orna init` wrote.
        None => matches!(
            orna_repository_v1::inspect_metadata(repository),
            Ok(Some(_))
        ),
    };
    if !format3 {
        return Ok(());
    }
    Err(Diagnostic {
        code: FormatContextError::MixedFormats {
            path: logical.to_owned(),
        }
        .code(),
        title: Cow::Owned(format!(
            "row file {logical} cannot be staged: a format-3 repository stores rows only in .orna/store"
        )),
        help: "change the row through the database, for example in `orna repl`, and publish it with `orna publish`; nothing was staged",
        exit: Exit::Target,
        detail: None,
        body: None,
    })
}

/// Stage exactly the source bytes captured in the resolved worktree row.
pub(super) fn add_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let key = key_text(key);
    with_worktree_row(endpoint, table, &key, |repository, logical, source| {
        require_row_file_layout(repository, logical)?;
        let path = ManagedPath::new(logical)
            .map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
        let index = repository
            .index_generation()
            .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
        repository
            .stage_managed_contents(&index, &path, source)
            .map_err(|e| error("row could not be staged", e.to_string()))?;
        println!("staged row {table} {key:?}: {logical}");
        Ok(())
    })
}

/// Restore one logical row's index entry to its committed (HEAD) state, the
/// inverse of `add_row` (ORNA-STAGE-002).
pub(super) fn unstage_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let key = key_text(key);
    with_worktree_row(endpoint, table, &key, |repository, logical, _source| {
        let path = ManagedPath::new(logical)
            .map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
        let index = repository
            .index_generation()
            .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
        repository
            .unstage_managed(&index, std::slice::from_ref(&path))
            .map_err(|e| error("row could not be unstaged", e.to_string()))?;
        println!("unstaged row {table} {key:?}: {logical}");
        Ok(())
    })
}

