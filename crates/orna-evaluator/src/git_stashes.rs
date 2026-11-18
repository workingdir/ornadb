//! The `sys.git.stashes` rows: the `sys.Stash` projection of the stash entries of
//! the repository the host attached (ORNA-SYS-111, ORNA-SYS-112).
//!
//! `index` is the `N` of `stash@{N}` (the newest entry is `0`) and the key of the
//! `sys.StashRef` that `reference` holds. `commit` is the `sys.CommitRef` of the
//! stash commit, `message` what `git stash list` prints for the entry and
//! `created` the committer date of the stash commit. [`crate::git_rows`] holds
//! what every Git member shares: the repository read, the commit pin and the
//! failure when either is unavailable.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository::CommittedStash;
use orna_sys::SysHostContext;
use orna_value::{commit_reference, stash_reference};

use crate::git_rows::{GitPage, RowPin, incomplete, object_id, page_ordered, repository};
use crate::{EvaluationError, Value};

/// Most stash entries one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_STASHES: usize = 10_000;

/// Serves the page of `sys.git.stashes` that follows the entry numbered by
/// `after`, from the repository `context` attached. The cursor of a page is the
/// index of the last entry it served, as eight big-endian bytes, so entries
/// order by index and not by the spelling of the number.
pub(crate) fn stashes_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let stashes = repository(context)?
        .committed_stashes(MAX_STASHES)
        .map_err(|_| incomplete())?;
    let facts = stashes.iter().map(StashFacts::of).collect::<Vec<_>>();
    page_ordered(
        context,
        &facts,
        |stash| stash.index.to_be_bytes().to_vec(),
        after,
        rows,
        stash_row,
    )
}

/// What one `sys.Stash` row is built from.
struct StashFacts<'a> {
    index: u64,
    /// The stash commit, in Git's hexadecimal spelling.
    commit: &'a str,
    message: &'a str,
    /// The committer date of the stash commit, in seconds since the Unix epoch.
    created: i64,
}

impl<'a> StashFacts<'a> {
    fn of(stash: &'a CommittedStash) -> Self {
        Self {
            index: stash.index(),
            commit: stash.commit().as_str(),
            message: stash.message(),
            created: stash.created_unix_seconds(),
        }
    }
}

/// The `sys.Stash` row of one entry: `{ reference, index, commit, message,
/// created }` (34-system-reference.md, `sys.Stash`).
fn stash_row(pin: &RowPin, stash: &StashFacts<'_>) -> Result<Value, EvaluationError> {
    let commit = object_id(stash.commit)?;
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| stash_reference(database, snapshot, stash.index))?,
        ),
        ("index".to_owned(), Value::Int(BigInt::from(stash.index))),
        (
            "commit".to_owned(),
            pin.reference(|database, snapshot| commit_reference(database, snapshot, commit))?,
        ),
        (
            "message".to_owned(),
            Value::String(stash.message.to_owned()),
        ),
        (
            "created".to_owned(),
            Value::Option(Some(Box::new(Value::Instant {
                unix_seconds: stash.created,
                nanosecond: 0,
            }))),
        ),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{StashFacts, stash_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, oid, pin, reference_of, row_pin};
    use num_bigint::BigInt;
    use orna_value::{commit_reference, relations, stash_reference};

    const STASH_COMMIT: &str = "00112233445566778899aabbccddeeff00112233";

    fn entry(index: u64) -> StashFacts<'static> {
        StashFacts {
            index,
            commit: STASH_COMMIT,
            message: "WIP on main: 1a2b3c4 base",
            created: 981_173_106,
        }
    }

    #[test]
    fn a_stash_row_carries_its_index_commit_message_and_time() {
        let row = stash_row(&row_pin(), &entry(2)).expect("a row");
        let row = fields(&row);
        assert_eq!(
            row.keys().map(String::as_str).collect::<Vec<_>>(),
            ["commit", "created", "index", "message", "reference"]
        );
        assert_eq!(row["index"], Value::Int(BigInt::from(2)));
        assert_eq!(
            row["message"],
            Value::String("WIP on main: 1a2b3c4 base".to_owned())
        );
        assert_eq!(
            row["created"],
            Value::Option(Some(Box::new(Value::Instant {
                unix_seconds: 981_173_106,
                nanosecond: 0,
            })))
        );

        let reference = reference_of(&row["reference"]);
        assert!(reference.is_of(&relations::STASH));
        assert_eq!(
            reference,
            stash_reference(DATABASE, pin(), 2).expect("a reference")
        );
        let commit = reference_of(&row["commit"]);
        assert!(commit.is_of(&relations::COMMIT));
        assert_eq!(
            commit,
            commit_reference(DATABASE, pin(), oid(STASH_COMMIT)).expect("a reference")
        );
    }

    #[test]
    fn every_entry_has_a_reference_of_its_own() {
        let first = stash_row(&row_pin(), &entry(0)).expect("a row");
        let second = stash_row(&row_pin(), &entry(1)).expect("a row");
        assert_ne!(fields(&first)["reference"], fields(&second)["reference"]);
        assert_eq!(
            fields(&first)["commit"],
            fields(&second)["commit"],
            "two entries naming one commit share the commit reference"
        );
    }

    #[test]
    fn a_time_before_the_epoch_is_an_instant_too() {
        let early = StashFacts {
            created: -86_400,
            ..entry(0)
        };
        let row = stash_row(&row_pin(), &early).expect("a row");
        assert_eq!(
            fields(&row)["created"],
            Value::Option(Some(Box::new(Value::Instant {
                unix_seconds: -86_400,
                nanosecond: 0,
            })))
        );
    }

    #[test]
    fn a_commit_id_of_the_wrong_width_is_refused_not_served() {
        let broken = StashFacts {
            commit: "0123",
            ..entry(0)
        };
        let failure = stash_row(&row_pin(), &broken).expect_err("no commit to name");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }
}
