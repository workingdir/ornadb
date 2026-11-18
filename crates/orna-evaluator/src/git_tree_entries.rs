//! The `sys.git.tree_entries` rows: the `sys.TreeEntry` projection of every entry under
//! the root tree of the commit the host attached at (ORNA-SYS-111, ORNA-SYS-112).
//!
//! `tree` is that root tree and `path` each entry's path relative to it, so the pair
//! names one entry (the relation's key). Every entry is a row, a directory included,
//! and the entries under it follow it. `object` is the `sys.GitObjectRef` of the object
//! the entry names, `kind` the kind of that object, and `mode` the entry's mode as a
//! number (`100644` is `33188`). [`crate::git_rows`] holds what every Git member shares.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository::{CommittedTreeListingEntry, GitObjectKind, NativeObjectId};
use orna_sys::{SysEnum, SysHostContext};
use orna_value::{
    GitObjectId, PathFlavour, git_object_reference, path_value, tree_entry_reference,
};

use crate::git_rows::{
    GitPage, RowPin, incomplete, object_id, page_rows, pinned_commit, repository,
};
use crate::source_enums::system_enum_value;
use crate::{EvaluationError, Value};

/// Most entries one read lists. A tree with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_TREE_ENTRIES: usize = 100_000;

/// The page of `sys.git.tree_entries` that follows the entry whose path is `after`,
/// at most `rows` entries, in path order under the root tree of the pinned commit. A
/// host with no commit pin refuses with `sys.snapshot.incomplete`, except over an
/// unborn `HEAD`, which names no commit and so serves none; a page of no rows reads
/// nothing and pins nothing.
pub(crate) fn tree_entries_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    if rows == 0 {
        return Ok((Vec::new(), None));
    }
    let repo = repository(context)?;
    let head = match pinned_commit(context) {
        Ok(oid) => NativeObjectId::new(oid.to_hex()).map_err(|_| incomplete())?,
        Err(error) => {
            // An unborn `HEAD` names no commit, so there is no tree to list.
            if context.head_commit.is_none() && matches!(repo.head(), Ok(None)) {
                return Ok((Vec::new(), None));
            }
            return Err(error);
        }
    };
    let root = repo
        .committed_commits(std::slice::from_ref(&head))
        .map_err(|_| incomplete())?
        .remove(&head)
        .ok_or_else(incomplete)?
        .tree()
        .clone();
    let root_id = object_id(root.as_str())?;
    let mut entries = repo
        .committed_tree_listing(&root, MAX_TREE_ENTRIES)
        .map_err(|_| incomplete())?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    page_rows(
        context,
        &entries,
        |entry| entry.path(),
        after,
        rows,
        |pin, entry| tree_entry_row(pin, &root_id, entry),
    )
}

/// The `sys.TreeEntry` row of one entry of the root tree `root`: `{ reference, tree,
/// path, mode, object, kind }` (34-system-reference.md, `sys.TreeEntry`).
fn tree_entry_row(
    pin: &RowPin,
    root: &GitObjectId,
    entry: &CommittedTreeListingEntry,
) -> Result<Value, EvaluationError> {
    let path = path_value(PathFlavour::Repo, entry.path()).ok_or_else(incomplete)?;
    let object = object_id(entry.object().as_str())?;
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| {
                tree_entry_reference(database, snapshot, root.clone(), entry.path())
            })?,
        ),
        (
            "tree".to_owned(),
            pin.reference(|database, snapshot| {
                git_object_reference(database, snapshot, root.clone())
            })?,
        ),
        ("path".to_owned(), Value::Path(path)),
        ("mode".to_owned(), Value::Int(BigInt::from(entry.mode()))),
        (
            "object".to_owned(),
            pin.reference(|database, snapshot| git_object_reference(database, snapshot, object))?,
        ),
        ("kind".to_owned(), kind_value(entry.kind())),
    ])))
}

/// The `sys.GitObjectKind` value of the kind of object an entry names, by the label the
/// kind carries in the system inventory.
fn kind_value(kind: GitObjectKind) -> Value {
    let label = match kind {
        GitObjectKind::Commit => "commit",
        GitObjectKind::Tree => "tree",
        GitObjectKind::Blob => "blob",
        GitObjectKind::Tag => "tag",
    };
    system_enum_value(SysEnum::GitObjectKind.name(), label)
}
