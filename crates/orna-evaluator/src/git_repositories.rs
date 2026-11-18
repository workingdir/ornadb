//! The `sys.git.repositories` rows: the `sys.GitRepository` projection of the
//! repository the host attached (ORNA-SYS-111, ORNA-SYS-112).
//!
//! The one row describes that repository: `git_dir` is its Git directory, `worktree`
//! its working tree (absent when it is bare), `bare` and `partial_clone` say what kind
//! of repository it is, and `object_format` names its hash algorithm (`sha1` or
//! `sha256`). `reference` and `database` are pinned to the commit the host attached at,
//! as every Git row is; [`crate::git_rows`] holds the repository read and the pin.

use std::collections::BTreeMap;
use std::path::Path;

use orna_repository::{GitHashAlgorithm, GitRepositoryFacts};
use orna_sys::SysHostContext;
use orna_value::{PathFlavour, database_reference, git_repository_reference, path_value};

use crate::git_rows::{GitPage, RowPin, incomplete, page_ordered, repository};
use crate::{EvaluationError, Value};

/// Serves the page of `sys.git.repositories` that follows the row named by `after`,
/// from the repository `context` attached. There is one row, so a cursor is only ever
/// past it.
pub(crate) fn repositories_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let facts = repository(context)?
        .git_repository_facts()
        .map_err(|_| incomplete())?;
    page_ordered(
        context,
        std::slice::from_ref(&facts),
        |_| context.database_id.to_vec(),
        after,
        rows,
        repository_row,
    )
}

/// The `sys.GitRepository` row: `{ reference, database, git_dir, worktree, bare,
/// object_format, partial_clone }` (34-system-reference.md, `sys.GitRepository`).
fn repository_row(pin: &RowPin, facts: &GitRepositoryFacts) -> Result<Value, EvaluationError> {
    let object_format = match facts.object_format() {
        GitHashAlgorithm::Sha1 => "sha1",
        GitHashAlgorithm::Sha256 => "sha256",
    };
    let worktree = match facts.worktree() {
        Some(path) => Value::Option(Some(Box::new(host_path(path)?))),
        None => Value::Option(None),
    };
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| {
                let owner = database_reference(database, snapshot.clone())?.into_row_ref();
                git_repository_reference(database, snapshot, owner)
            })?,
        ),
        ("database".to_owned(), pin.reference(database_reference)?),
        ("git_dir".to_owned(), host_path(facts.git_dir())?),
        ("worktree".to_owned(), worktree),
        ("bare".to_owned(), Value::Bool(facts.bare())),
        (
            "object_format".to_owned(),
            Value::String(object_format.to_owned()),
        ),
        (
            "partial_clone".to_owned(),
            Value::Bool(facts.partial_clone()),
        ),
    ])))
}

/// A host path as a `Path` value of the POSIX flavour. A path that is not UTF-8, or
/// that the flavour refuses, is `sys.snapshot.incomplete` rather than a row without it.
fn host_path(path: &Path) -> Result<Value, EvaluationError> {
    let text = path.to_str().ok_or_else(incomplete)?;
    path_value(PathFlavour::Posix, text)
        .map(Value::Path)
        .ok_or_else(incomplete)
}
