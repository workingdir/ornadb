//! The `sys.git.worktree` rows: the `sys.WorktreeEntry` projection of the physical
//! status of the working tree of the repository the host attached (ORNA-SYS-111,
//! ORNA-SYS-112).
//!
//! One row per path the status reports, in path order: `index_status` and
//! `worktree_status` are the `sys.ChangeKind` of the path in the index and in the
//! working tree (absent where the path does not differ there), and `ignored` and
//! `untracked` say what Git makes of the path. [`crate::git_rows`] holds what every Git
//! member shares: the repository read, the commit pin and the failure when either is
//! unavailable.

use std::collections::BTreeMap;

use orna_repository::{StatusChange, WorktreeStatusEntry};
use orna_sys::{SysEnum, SysHostContext};
use orna_value::{
    PathFlavour, enum_type_id, enum_variant_id, path_value, worktree_entry_reference,
};

use crate::git_rows::{GitPage, RowPin, incomplete, page_rows, repository};
use crate::{EvaluationError, Value, object_id_raw};

/// Most entries one read lists. A worktree with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
pub(crate) const MAX_ENTRIES: usize = 100_000;

/// Serves the page of `sys.git.worktree` that follows the path named by `after`, from
/// the repository `context` attached.
pub(crate) fn worktree_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let mut entries = repository(context)?
        .worktree_status_entries(MAX_ENTRIES)
        .map_err(|_| incomplete())?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    page_rows(
        context,
        &entries,
        |entry| entry.path(),
        after,
        rows,
        worktree_row,
    )
}

/// The `sys.WorktreeEntry` row: `{ reference, path, index_status, worktree_status,
/// ignored, untracked }` (34-system-reference.md, `sys.WorktreeEntry`).
fn worktree_row(pin: &RowPin, entry: &WorktreeStatusEntry) -> Result<Value, EvaluationError> {
    let path = entry.path();
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| worktree_entry_reference(database, snapshot, path))?,
        ),
        (
            "path".to_owned(),
            Value::Path(path_value(PathFlavour::Repo, path).ok_or_else(incomplete)?),
        ),
        (
            "index_status".to_owned(),
            change_option(entry.index_status()),
        ),
        (
            "worktree_status".to_owned(),
            change_option(entry.worktree_status()),
        ),
        ("ignored".to_owned(), Value::Bool(entry.ignored())),
        ("untracked".to_owned(), Value::Bool(entry.untracked())),
    ])))
}

/// An optional `sys.ChangeKind`, absent when the path does not differ there.
fn change_option(change: Option<StatusChange>) -> Value {
    Value::Option(change.map(|change| Box::new(change_kind(change))))
}

/// The `sys.ChangeKind` value of `change`, named as the spec names its variants.
///
/// The type and the variant are the canonical object ids of the enum and of the
/// variant, the identity every host derives from the qualified enum name.
fn change_kind(change: StatusChange) -> Value {
    let variant = match change {
        StatusChange::Add => "add",
        StatusChange::Modify => "modify",
        StatusChange::Delete => "delete",
        StatusChange::Rename => "rename",
    };
    let enum_name = SysEnum::ChangeKind.name();
    Value::Enum {
        type_id: object_id_raw(enum_type_id(enum_name)),
        variant_id: object_id_raw(enum_variant_id(enum_name, variant)),
        payload: None,
    }
}
