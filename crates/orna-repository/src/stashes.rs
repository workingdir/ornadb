//! The stash entries of a repository, as `sys.Stash` projects them: the stash
//! commit each entry names, its message and when it was made.
//!
//! One `git stash list` run reads the stash reflog, so the entries of one
//! listing are one observation. A stash entry is what Git lists as
//! `stash@{N}`: the newest entry is index `0`, and an index is the position in
//! the list (the reflog of `refs/stash`), which is why a stash dropped between
//! two listings renumbers the entries after it. A repository with no stash has
//! no entry. Nothing is guessed: a record that does not have the shape Git
//! writes, an object id of the wrong width, a time that is not a number or a
//! stash commit Git cannot read refuses the whole listing.

use crate::{
    NativeObjectId, Repository, RepositoryError, listing_text, parse_unix_seconds,
    require_native_object_id, run_bounded_listing,
};

/// One entry of the stash list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedStash {
    index: u64,
    commit: NativeObjectId,
    message: String,
    created: i64,
}

impl CommittedStash {
    /// The `N` of `stash@{N}`: `0` for the newest entry.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// The stash commit the entry names.
    pub fn commit(&self) -> &NativeObjectId {
        &self.commit
    }

    /// The message Git lists the entry with (`git stash list`): the reflog
    /// subject, such as `WIP on main: 1a2b3c4 subject` or `On main: message`.
    /// A reflog message is one line, so a stash message of several lines reads
    /// as Git flattened it.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// When the stash commit was made, in seconds since the Unix epoch: its
    /// committer date.
    pub fn created_unix_seconds(&self) -> i64 {
        self.created
    }
}

/// Fields of one record of the listing behind [`Repository::committed_stashes`].
const STASH_FIELDS: usize = 3;

/// The `git stash list` format of one entry: the stash commit id, the reflog
/// subject and the committer date as a Unix timestamp, each field ended by a
/// NUL.
///
/// The reflog selector (`%gd`) is not requested: it reads `stash@{<date>}`
/// instead of `stash@{<index>}` when `log.date` is configured, so the index is
/// the position in the listing, which is what the selector counts. Git reads
/// every field as a C string, so no field can hold a NUL byte and the NUL that
/// ends each one is an unambiguous terminator.
const STASH_FORMAT: &str = concat!("--format=", "%H%x00", "%gs%x00", "%ct%x00");

impl Repository {
    /// Lists the stash entries, newest first (`stash@{0}` first).
    ///
    /// One `git stash list` run reads the reflog of `refs/stash`; a repository
    /// with no stash lists none. More than `max_stashes` entries, or a listing
    /// larger than the bounded-listing limit, is refused rather than served
    /// truncated, and so is an entry whose commit Git cannot read.
    pub fn committed_stashes(
        &self,
        max_stashes: usize,
    ) -> Result<Vec<CommittedStash>, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            // A configured `log.showSignature` would print a verification
            // report before each signed commit of the log the list runs.
            .args(["-c", "log.showSignature=false", "stash", "list"])
            .arg(format!("--max-count={}", max_stashes.saturating_add(1)))
            .arg(STASH_FORMAT);
        let output = run_bounded_listing(command)?;

        let stashes = parse_committed_stashes(&output, self.native_object_id_length()?)?;
        if stashes.len() > max_stashes {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(stashes)
    }
}

/// Parses the whole listing: records of [`STASH_FIELDS`] NUL-terminated fields,
/// each followed by the newline `git log` ends its lines with. Anything else is
/// refused. The entry of a record is numbered by its position.
fn parse_committed_stashes(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedStash>, RepositoryError> {
    let mut rest = listing;
    let mut stashes = Vec::new();
    while !rest.is_empty() {
        let mut fields: [&[u8]; STASH_FIELDS] = [&[]; STASH_FIELDS];
        for field in &mut fields {
            let end = rest
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(RepositoryError::GitOperationFailed)?;
            (*field, rest) = (&rest[..end], &rest[end + 1..]);
        }
        rest = rest
            .strip_prefix(b"\n")
            .ok_or(RepositoryError::GitOperationFailed)?;
        let index =
            u64::try_from(stashes.len()).map_err(|_| RepositoryError::GitOperationFailed)?;
        stashes.push(parse_committed_stash(index, fields, object_id_length)?);
    }
    Ok(stashes)
}

/// Reads one record: the commit id, the message and the commit time.
fn parse_committed_stash(
    index: u64,
    fields: [&[u8]; STASH_FIELDS],
    object_id_length: usize,
) -> Result<CommittedStash, RepositoryError> {
    let [commit, message, created] = fields;
    let commit = listing_text(commit)?;
    require_native_object_id(commit, object_id_length)?;
    Ok(CommittedStash {
        index,
        commit: NativeObjectId::new(commit)?,
        message: listing_text(message)?.to_owned(),
        created: parse_unix_seconds(listing_text(created)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{CommittedStash, parse_committed_stashes};
    use crate::RepositoryError;

    const FIRST: &str = "0123456789abcdef0123456789abcdef01234567";
    const SECOND: &str = "fedcba9876543210fedcba9876543210fedcba98";

    /// One entry as `git stash list` prints it with the listing's format.
    fn record(commit: &str, message: &str, created: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in [commit, message, created] {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
        }
        bytes.push(b'\n');
        bytes
    }

    fn parse(bytes: &[u8]) -> Result<Vec<CommittedStash>, RepositoryError> {
        parse_committed_stashes(bytes, 40)
    }

    #[test]
    fn an_entry_carries_its_commit_message_and_time() {
        let stashes = parse(&record(FIRST, "On main: work", "1795001704")).expect("one entry");
        let [stash] = stashes.as_slice() else {
            panic!("one entry");
        };
        assert_eq!(stash.index(), 0);
        assert_eq!(stash.commit().as_str(), FIRST);
        assert_eq!(stash.message(), "On main: work");
        assert_eq!(stash.created_unix_seconds(), 1_795_001_704);
    }

    #[test]
    fn entries_are_numbered_by_position_newest_first() {
        let mut listing = record(FIRST, "On main: newest", "30");
        listing.extend(record(SECOND, "WIP on main: 1a2b3c4 base", "20"));
        listing.extend(record(FIRST, "On main: oldest", "10"));
        let stashes = parse(&listing).expect("three entries");
        let seen = stashes
            .iter()
            .map(|stash| (stash.index(), stash.message().to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(
            seen,
            [
                (0, "On main: newest".to_owned()),
                (1, "WIP on main: 1a2b3c4 base".to_owned()),
                (2, "On main: oldest".to_owned()),
            ]
        );
        assert_eq!(stashes[1].commit().as_str(), SECOND);
    }

    #[test]
    fn an_empty_listing_has_no_entry() {
        assert_eq!(parse(b"").expect("an empty listing"), Vec::new());
    }

    #[test]
    fn a_message_may_be_empty_or_hold_text_that_looks_like_another_record() {
        let mut listing = record(FIRST, "", "5");
        listing.extend(record(SECOND, &format!("On main: {SECOND} 99"), "6"));
        let stashes = parse(&listing).expect("two entries");
        assert_eq!(stashes[0].message(), "");
        assert_eq!(stashes[1].message(), format!("On main: {SECOND} 99"));
    }

    #[test]
    fn a_time_may_be_before_the_epoch() {
        let stashes = parse(&record(FIRST, "m", "-86400")).expect("an entry");
        assert_eq!(stashes[0].created_unix_seconds(), -86_400);
    }

    #[test]
    fn a_record_git_did_not_write_is_refused() {
        let good = record(FIRST, "On main: work", "1795001704");
        let refused: Vec<(&str, Vec<u8>)> = vec![
            ("no final newline", good[..good.len() - 1].to_vec()),
            ("a short record", b"0123\0m\0".to_vec()),
            ("an unterminated field", FIRST.as_bytes().to_vec()),
            ("an object id of the wrong width", record("0123", "m", "1")),
            (
                "an object id that is not hexadecimal",
                record(&"g".repeat(40), "m", "1"),
            ),
            ("an empty time", record(FIRST, "m", "")),
            ("a time that is not a number", record(FIRST, "m", "soon")),
            ("a time with a plus sign", record(FIRST, "m", "+5")),
            ("a time with a space", record(FIRST, "m", "5 ")),
            ("a lone minus sign", record(FIRST, "m", "-")),
            (
                "a time past 64 bits",
                record(FIRST, "m", "99999999999999999999999"),
            ),
            (
                "a message that is not text",
                [FIRST.as_bytes(), b"\0\xff\xfe\x001\0\n"].concat(),
            ),
        ];
        for (what, bytes) in refused {
            assert!(
                matches!(
                    parse(&bytes),
                    Err(RepositoryError::GitOperationFailed | RepositoryError::InvalidObjectId)
                ),
                "{what}"
            );
        }
    }
}

#[cfg(test)]
mod process_tests {
    use crate::{Repository, git_command, test_support};

    /// The whole listing is one Git run (`git stash list`, which runs its own
    /// log walk), however many entries it lists.
    #[test]
    fn one_git_run_lists_every_stash_entry() {
        let directory = tempfile::tempdir().expect("repository directory");
        let root = directory.path();
        test_support::initialize_fixture_repository(root);
        test_support::commit_initial_module(root);
        for round in 0..3 {
            std::fs::write(root.join("main.orna"), format!("module main; // {round}\n"))
                .expect("a change to stash");
            test_support::git(root, &["stash", "push", "-m", &format!("round {round}")]);
        }
        let repository = Repository::discover(root).expect("a Git worktree");
        // The object format is read once per handle and not by the listing.
        repository.committed_stashes(64).expect("a first listing");

        git_command::reset_git_process_requests();
        let stashes = repository.committed_stashes(64).expect("a listing");
        assert_eq!(stashes.len(), 3);
        assert_eq!(git_command::git_process_requests(), 1);
    }

    /// A repository with no stash has no `refs/stash`, which `git reflog show`
    /// refuses; `git stash list` reports an empty list instead.
    #[test]
    fn a_repository_with_no_stash_lists_none_in_one_git_run() {
        let directory = tempfile::tempdir().expect("repository directory");
        test_support::initialize_fixture_repository(directory.path());
        test_support::commit_initial_module(directory.path());
        let repository = Repository::discover(directory.path()).expect("a Git worktree");
        repository.committed_stashes(64).expect("a first listing");

        git_command::reset_git_process_requests();
        assert!(
            repository
                .committed_stashes(64)
                .expect("a listing")
                .is_empty()
        );
        assert_eq!(git_command::git_process_requests(), 1);
    }
}
