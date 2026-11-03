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

/// Normalises a `KEY_TEXT` argument: a JSON string literal is decoded, any other
/// text is taken verbatim.
fn key_text(key: &str) -> String {
    serde_json::from_str::<String>(key).unwrap_or_else(|_| key.to_owned())
}

/// Resolves one logical worktree row through the project loader. `Err` reports a
/// failure that is not a missing row, such as an unreadable project or an
/// ambiguous match; `Ok(None)` means no worktree row matches, which the caller
/// explains, because only the caller knows whether rows have files at all.
fn find_worktree_row(
    repository: &Repository,
    table: &str,
    key: &str,
) -> Result<Option<(String, Vec<u8>)>, Diagnostic> {
    let project = orna_project_v1::ProjectLoader::default()
        .load(repository)
        .map_err(|e| error("project rows could not be loaded", format!("{e:?}")))?;
    let mut candidates = project
        .loose_rows()
        .iter()
        .filter(|row| row.table_path() == table && matches_key_path(row.key_path(), key));
    let Some(candidate) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Err(error(
            "logical row is ambiguous",
            format!("more than one worktree row matches table {table:?} and key path {key:?}"),
        ));
    }
    Ok(Some((
        candidate.logical_path().to_owned(),
        candidate.source_bytes().to_vec(),
    )))
}

/// Whether `HEAD`, or the metadata of an unborn branch, records format 3.
/// `subject` names what is being refused in the diagnostic.
///
/// Rows have no physical Git paths in format 3: they live only in
/// `.orna/store` (ORNA-REPO-004), and the row adapter stages a row into the
/// index store (ORNA-STAGE-001), which this build cannot do yet. A loose row
/// file staged into a format-3 index would make the next commit mix formats
/// (ORNA-META-001; Gate A, "unknown/mixed formats fail early"), so it is
/// refused before the index is touched, as is staging into a `HEAD` whose
/// metadata does not admit. Only a format-1/2 repository, or a tree without
/// Orna metadata, keeps row files.
fn is_format3(repository: &Repository, subject: &str) -> Result<bool, Diagnostic> {
    match repository
        .head()
        .map_err(|e| error("Git HEAD could not be inspected", format!("{e:?}")))?
    {
        Some(_) => match repository.open_format_context() {
            Ok(context) => Ok(context.repository_format() == RepositoryFormat::Format3),
            Err(FormatContextError::MetadataUnavailable) => Ok(false),
            Err(refusal) => Err(Diagnostic {
                code: refusal.code(),
                title: Cow::Owned(format!("{subject} cannot be staged: {refusal}")),
                help: "repair the committed `.orna` metadata, then retry; nothing was staged",
                exit: Exit::Target,
                detail: None,
                body: None,
            }),
        },
        // An unborn branch has no committed format yet: its first commit
        // takes the format-3 metadata `orna init` wrote.
        None => Ok(matches!(
            orna_repository_v1::inspect_metadata(repository),
            Ok(Some(_))
        )),
    }
}

/// Refuses to stage a row file by its path.
fn require_row_file_layout(repository: &Repository, logical: &str) -> Result<(), Diagnostic> {
    if !is_format3(repository, &format!("row {logical}"))? {
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

/// Refuses a row command that names a row this repository cannot stage by path.
/// The row may exist in the store, so the refusal says so instead of denying the
/// row: the operator's next step is a database change and a publish.
fn store_only_row_refusal(table: &str, key: &str, verb: &str) -> Diagnostic {
    Diagnostic {
        code: FormatContextError::MixedFormats {
            path: format!("{table}#{key}"),
        }
        .code(),
        title: Cow::Owned(format!(
            "row {table} {key:?} cannot be {verb}: a format-3 repository stores rows only in .orna/store"
        )),
        help: "change the row through the database, for example in `orna repl`, and publish it with `orna publish`; nothing was staged",
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

/// Stage exactly the source bytes captured in the resolved worktree row.
pub(super) fn add_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let key = key_text(key);
    let repository = repository(endpoint)?;
    let permitted = !is_format3(&repository, &format!("row {table} {key:?}"))?;
    let Some((logical, source)) = find_worktree_row(&repository, table, &key)? else {
        return Err(if permitted {
            error(
                "logical row was not found",
                format!("no worktree row matches table {table:?} and key path {key:?}"),
            )
        } else {
            store_only_row_refusal(table, &key, "staged")
        });
    };
    require_row_file_layout(&repository, &logical)?;
    let path =
        ManagedPath::new(&logical).map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
    let index = repository
        .index_generation()
        .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
    repository
        .stage_managed_contents(&index, &path, &source)
        .map_err(|e| error("row could not be staged", e.to_string()))?;
    println!("staged row {table} {key:?}: {logical}");
    Ok(())
}

/// Restore one logical row's index entry to its committed (HEAD) state, the
/// inverse of `add_row` (ORNA-STAGE-002).
pub(super) fn unstage_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let key = key_text(key);
    let repository = repository(endpoint)?;
    let permitted = !is_format3(&repository, &format!("row {table} {key:?}"))?;
    let Some((logical, _source)) = find_worktree_row(&repository, table, &key)? else {
        return Err(if permitted {
            error(
                "logical row was not found",
                format!("no worktree row matches table {table:?} and key path {key:?}"),
            )
        } else {
            store_only_row_refusal(table, &key, "unstaged")
        });
    };
    let path =
        ManagedPath::new(&logical).map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
    let index = repository
        .index_generation()
        .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
    repository
        .unstage_managed(&index, std::slice::from_ref(&path))
        .map_err(|e| error("row could not be unstaged", e.to_string()))?;
    println!("unstaged row {table} {key:?}: {logical}");
    Ok(())
}
