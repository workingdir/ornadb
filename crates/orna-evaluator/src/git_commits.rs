//! The `sys.git.commits` rows: the `sys.Commit` projection of the commits reachable
//! from the commit the host attached at (ORNA-SYS-111, ORNA-SYS-112).
//!
//! A page of commits is one `git log` walk from the pinned commit, so a page is one
//! observation of the object store. Its cursor is the walk position of the last commit
//! it served, then that commit's id. A later page re-reads the walk from that position
//! and refuses with `sys.snapshot.incomplete` unless the same commit still stands
//! there, so a rewritten history never resumes at a different commit silently.
//!
//! A commit's `parents` relation is served from a source that carries the parents' ids
//! (`PARENTS_PREFIX`), read as one `git log --no-walk` observation per page. A shallow
//! boundary commit has no parents Git will print, so its relation is `UNAVAILABLE` and
//! reading it refuses with `sys.snapshot.incomplete`.

use std::collections::{BTreeMap, HashSet};

use orna_repository::{CommittedCommit, NativeObjectId, Repository};
use orna_sys::SysHostContext;
use orna_value::{commit_reference, git_object_reference};

use crate::git_rows::{
    GitPage, RowPin, incomplete, object_id, person_identity, pinned_commit, repository,
};
use crate::relation::RelationPlan;
use crate::{EvaluationError, Value};

/// The source of a commit's `parents` relation when the commit's parents are listed:
/// this prefix, then the parents' hexadecimal ids, comma separated.
pub(crate) const PARENTS_PREFIX: &str = "sys.git.commits/parents:";

/// The source of the `parents` relation of a shallow boundary commit. Git prints no
/// parents for it, so reading the relation refuses with `sys.snapshot.incomplete`.
pub(crate) const UNAVAILABLE: &str = "sys.git.commits/unavailable";

/// The walk position past which a read refuses with `sys.snapshot.incomplete`, rather
/// than serving a prefix of a history longer than this.
const MAX_COMMITS: u64 = 100_000;

/// The page of `sys.git.commits` that follows the commit at the position `after`
/// names, at most `rows` commits, in `git log` order from the pinned commit. A host
/// with no commit pin refuses with `sys.snapshot.incomplete`, except over an unborn
/// `HEAD`, which names no commit and so serves none; a page of no rows reads nothing
/// and pins nothing.
pub(crate) fn commits_page(
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
            // An unborn `HEAD` names no commit: there is nothing to serve and nothing to pin.
            if context.head_commit.is_none() && matches!(repo.head(), Ok(None)) {
                return Ok((Vec::new(), None));
            }
            return Err(error);
        }
    };
    let (first, listing) = match after {
        None => (0, walk(&repo, &head, 0, rows.saturating_add(1))?),
        Some(cursor) => {
            let (index, oid) = decode_cursor(cursor)?;
            let mut listing = walk(&repo, &head, index, rows.saturating_add(2))?;
            // The walk's first commit is the cursor's own commit: the same id at the
            // same position shows the walk still stands where the page left it.
            let anchored = match listing.first() {
                Some(anchor) => object_id(anchor.commit().as_str())?.as_bytes() == oid,
                None => false,
            };
            if !anchored {
                return Err(incomplete());
            }
            listing.remove(0);
            (index.checked_add(1).ok_or_else(incomplete)?, listing)
        }
    };
    let served = rows.min(listing.len());
    let more = listing.len() > rows;
    let shallow = repo.shallow_boundaries().map_err(|_| incomplete())?;
    let pin = RowPin::of(context)?;
    let mut values = Vec::with_capacity(served);
    let mut last = None;
    for (offset, commit) in listing.iter().take(served).enumerate() {
        let position = first
            .checked_add(u64::try_from(offset).map_err(|_| incomplete())?)
            .filter(|position| *position < MAX_COMMITS)
            .ok_or_else(incomplete)?;
        let oid = object_id(commit.commit().as_str())?;
        values.push(commit_row(&pin, commit, &shallow)?);
        last = Some(encode_cursor(position, oid.as_bytes()));
    }
    Ok((values, if more { last } else { None }))
}

/// The page of a commit's parents that follows the parent at the position `after`
/// names, at most `rows` parents. `hex` is the comma-separated ids Git prints for the
/// commit, in the order the commit records them.
pub(crate) fn parents_page(
    context: &SysHostContext,
    hex: &str,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    if rows == 0 || hex.is_empty() {
        return Ok((Vec::new(), None));
    }
    let parents = hex
        .split(',')
        .map(|id| NativeObjectId::new(id).map_err(|_| incomplete()))
        .collect::<Result<Vec<_>, _>>()?;
    let start = match after {
        None => 0,
        Some(cursor) => {
            let (index, oid) = decode_cursor(cursor)?;
            let index = usize::try_from(index).map_err(|_| incomplete())?;
            let parent = parents.get(index).ok_or_else(incomplete)?;
            if object_id(parent.as_str())?.as_bytes() != oid {
                return Err(incomplete());
            }
            index + 1
        }
    };
    let end = start.saturating_add(rows).min(parents.len());
    let window = &parents[start.min(end)..end];
    if window.is_empty() {
        return Ok((Vec::new(), None));
    }
    let repo = repository(context)?;
    let commits = repo.committed_commits(window).map_err(|_| incomplete())?;
    let shallow = repo.shallow_boundaries().map_err(|_| incomplete())?;
    let pin = RowPin::of(context)?;
    let values = window
        .iter()
        .map(|parent| {
            let commit = commits.get(parent).ok_or_else(incomplete)?;
            commit_row(&pin, commit, &shallow)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next = if end < parents.len() {
        let index = end - 1;
        let oid = object_id(parents[index].as_str())?;
        Some(encode_cursor(
            u64::try_from(index).map_err(|_| incomplete())?,
            oid.as_bytes(),
        ))
    } else {
        None
    };
    Ok((values, next))
}

/// One `git log` walk from `head`, `skip` commits in, at most `count` commits.
fn walk(
    repo: &Repository,
    head: &NativeObjectId,
    skip: u64,
    count: usize,
) -> Result<Vec<CommittedCommit>, EvaluationError> {
    repo.committed_commit_page(head, skip, count)
        .map_err(|_| incomplete())
}

/// A cursor: the eight big-endian bytes of a position, then the id of the commit or
/// parent at it, so cursors order by position and not by the spelling of an id.
fn encode_cursor(position: u64, oid: &[u8]) -> Vec<u8> {
    let mut cursor = position.to_be_bytes().to_vec();
    cursor.extend_from_slice(oid);
    cursor
}

/// The position and id an [`encode_cursor`] cursor carries.
fn decode_cursor(cursor: &[u8]) -> Result<(u64, &[u8]), EvaluationError> {
    if cursor.len() < 8 {
        return Err(incomplete());
    }
    let (position, oid) = cursor.split_at(8);
    let position = u64::from_be_bytes(position.try_into().map_err(|_| incomplete())?);
    Ok((position, oid))
}

/// The `sys.Commit` row of one commit: `{ reference, oid, snapshot, parents, author,
/// committer, message, authored_at, committed_at, tree }` (34-system-reference.md,
/// `sys.Commit`). The parents of a shallow boundary commit are unavailable.
fn commit_row(
    pin: &RowPin,
    commit: &CommittedCommit,
    shallow: &HashSet<NativeObjectId>,
) -> Result<Value, EvaluationError> {
    let oid = object_id(commit.commit().as_str())?;
    let tree = object_id(commit.tree().as_str())?;
    let parents = if shallow.contains(commit.commit()) {
        Value::Relation(RelationPlan::new(UNAVAILABLE.to_owned()))
    } else {
        let ids = commit
            .parents()
            .iter()
            .map(NativeObjectId::as_str)
            .collect::<Vec<_>>();
        Value::Relation(RelationPlan::with_known_rows(
            format!("{PARENTS_PREFIX}{}", ids.join(",")),
            ids.len(),
        ))
    };
    let author = commit.author();
    let committer = commit.committer();
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| commit_reference(database, snapshot, oid.clone()))?,
        ),
        ("oid".to_owned(), Value::GitOid(oid.clone())),
        ("snapshot".to_owned(), pin.commit_snapshot(&oid)?),
        ("parents".to_owned(), parents),
        (
            "author".to_owned(),
            person_identity(author.name(), author.email()),
        ),
        (
            "committer".to_owned(),
            person_identity(committer.name(), committer.email()),
        ),
        (
            "message".to_owned(),
            Value::String(commit.message().to_owned()),
        ),
        (
            "authored_at".to_owned(),
            Value::Instant {
                unix_seconds: commit.authored_unix_seconds(),
                nanosecond: 0,
            },
        ),
        (
            "committed_at".to_owned(),
            Value::Instant {
                unix_seconds: commit.committed_unix_seconds(),
                nanosecond: 0,
            },
        ),
        (
            "tree".to_owned(),
            pin.reference(|database, snapshot| git_object_reference(database, snapshot, tree))?,
        ),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{decode_cursor, encode_cursor};

    #[test]
    fn a_cursor_carries_its_position_and_its_id() {
        let oid = [7_u8; 20];
        let encoded = encode_cursor(258, &oid);
        assert_eq!(decode_cursor(&encoded).ok(), Some((258, &oid[..])));
    }

    #[test]
    fn a_cursor_without_a_whole_position_is_refused() {
        assert!(decode_cursor(&[0_u8; 7]).is_err());
    }
}
