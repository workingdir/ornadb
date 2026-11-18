//! The Git facts `sys.GitRepository` and `sys.WorktreeEntry` serve: where the
//! repository's Git directory and worktree are, whether it is bare, its object
//! format and partial-clone state, and the status of every path Git reports as
//! changed, untracked or ignored.
//!
//! Each observation is one Git process run under `GIT_NO_LAZY_FETCH=1`, so a
//! read never fetches a promised object, and the status run passes
//! `--no-optional-locks`, so it never takes the index lock. A record of a shape
//! these queries do not ask Git for refuses the whole observation rather than
//! serving part of it.

use std::path::{Path, PathBuf};

use crate::{GitHashAlgorithm, Repository, RepositoryError, run_bounded_listing};

/// Where a repository's Git directory and worktree are, and what kind of
/// repository it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRepositoryFacts {
    git_dir: PathBuf,
    worktree: Option<PathBuf>,
    bare: bool,
    object_format: GitHashAlgorithm,
    partial_clone: bool,
}

impl GitRepositoryFacts {
    /// The absolute Git directory of the selected worktree. A linked worktree
    /// reports its own administrative directory, not the common one.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The top of the worktree, or `None` for a bare repository.
    pub fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }

    /// Whether the repository has no worktree.
    pub const fn bare(&self) -> bool {
        self.bare
    }

    /// The object format the repository writes its object ids in.
    pub const fn object_format(&self) -> GitHashAlgorithm {
        self.object_format
    }

    /// Whether partial or promisor object configuration was observed.
    pub const fn partial_clone(&self) -> bool {
        self.partial_clone
    }
}

/// One change Git's status reports for a path on one side: the index against
/// `HEAD`, or the worktree against the index. A type change (`T`) is a
/// modification, and a rename is reported at the path it now has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusChange {
    Add,
    Modify,
    Delete,
    Rename,
}

/// One path `git status` reports: its change on each side, and whether it is
/// untracked or ignored. An untracked path has no index change and reads as
/// added in the worktree; an ignored path has no change on either side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeStatusEntry {
    path: String,
    index_status: Option<StatusChange>,
    worktree_status: Option<StatusChange>,
    ignored: bool,
    untracked: bool,
}

impl WorktreeStatusEntry {
    /// Builds an entry, refusing an empty path, which Git never writes.
    fn new(
        path: &str,
        index_status: Option<StatusChange>,
        worktree_status: Option<StatusChange>,
        ignored: bool,
        untracked: bool,
    ) -> Result<Self, RepositoryError> {
        if path.is_empty() {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(Self {
            path: path.to_owned(),
            index_status,
            worktree_status,
            ignored,
            untracked,
        })
    }

    /// The path relative to the worktree top, as Git writes it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The change between `HEAD` and the index, or `None` when they agree.
    pub const fn index_status(&self) -> Option<StatusChange> {
        self.index_status
    }

    /// The change between the index and the worktree, or `None` when they agree.
    pub const fn worktree_status(&self) -> Option<StatusChange> {
        self.worktree_status
    }

    /// Whether the path matches an ignore rule.
    pub const fn ignored(&self) -> bool {
        self.ignored
    }

    /// Whether the path is in the worktree but not in the index.
    pub const fn untracked(&self) -> bool {
        self.untracked
    }
}

impl Repository {
    /// Observes where the repository's Git directory and worktree are, whether
    /// it is bare, its object format and whether it is a partial clone.
    pub fn git_repository_facts(&self) -> Result<GitRepositoryFacts, RepositoryError> {
        let (git_dir, bare) = parse_location(&self.git([
            "rev-parse",
            "--absolute-git-dir",
            "--is-bare-repository",
        ])?)?;
        let worktree = if bare {
            None
        } else {
            Some(PathBuf::from(self.git(["rev-parse", "--show-toplevel"])?))
        };
        Ok(GitRepositoryFacts {
            git_dir,
            worktree,
            bare,
            object_format: self.object_hash_algorithm()?,
            partial_clone: self.observe_git_capabilities()?.partial_clone(),
        })
    }

    /// Lists the paths `git status` reports (changes on either side, untracked
    /// paths and ignored paths) in one status run. Rename detection is named
    /// explicitly, so the listing does not depend on `status.renames`.
    ///
    /// More than `max_entries` paths, or a listing past the bounded-listing
    /// limit, is refused rather than served truncated.
    pub fn worktree_status_entries(
        &self,
        max_entries: usize,
    ) -> Result<Vec<WorktreeStatusEntry>, RepositoryError> {
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--ignored=matching",
            "--untracked-files=all",
            "--find-renames",
        ]);
        let entries = parse_worktree_status(&run_bounded_listing(command)?)?;
        if entries.len() > max_entries {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
    }
}

/// Parses the two lines `rev-parse --absolute-git-dir --is-bare-repository`
/// writes: the absolute Git directory, then `true` or `false`.
fn parse_location(text: &str) -> Result<(PathBuf, bool), RepositoryError> {
    let mut lines = text.lines();
    let (Some(git_dir), Some(bare), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(RepositoryError::GitOperationFailed);
    };
    let bare = match bare {
        "true" => true,
        "false" => false,
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    let git_dir = PathBuf::from(git_dir);
    if !git_dir.is_absolute() {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok((git_dir, bare))
}

/// Parses a `status --porcelain=v2 -z` listing: NUL-terminated records, each
/// one of the shapes Git writes for these queries. A rename's origin path is
/// the field after its record and is not reported. Any other shape, and a path
/// reported twice, refuses the whole listing.
fn parse_worktree_status(listing: &[u8]) -> Result<Vec<WorktreeStatusEntry>, RepositoryError> {
    if listing.is_empty() {
        return Ok(Vec::new());
    }
    let body = listing
        .strip_suffix(b"\0")
        .ok_or(RepositoryError::GitOperationFailed)?;
    let mut fields = body.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(field) = fields.next() {
        let record = std::str::from_utf8(field).map_err(|_| RepositoryError::GitOperationFailed)?;
        let (entry, has_origin) = parse_status_record(record)?;
        if has_origin {
            fields.next().ok_or(RepositoryError::GitOperationFailed)?;
        }
        entries.push(entry);
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(entries)
}

/// Parses one record, and whether a rename's origin path follows it as the next
/// field. Unmerged (`u`) and header (`#`) records are not asked for, so they are
/// refused with every other shape.
fn parse_status_record(record: &str) -> Result<(WorktreeStatusEntry, bool), RepositoryError> {
    let (kind, rest) = record
        .split_once(' ')
        .ok_or(RepositoryError::GitOperationFailed)?;
    match kind {
        // `1 XY sub mH mI mW hH hI path`: the path may hold spaces.
        "1" => {
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            let [xy, _, _, _, _, _, _, path] = fields.as_slice() else {
                return Err(RepositoryError::GitOperationFailed);
            };
            Ok((changed_entry(xy, path)?, false))
        }
        // `2 XY sub mH mI mW hH hI score path`, then the origin path.
        "2" => {
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            let [xy, _, _, _, _, _, _, _, path] = fields.as_slice() else {
                return Err(RepositoryError::GitOperationFailed);
            };
            Ok((changed_entry(xy, path)?, true))
        }
        "?" => Ok((
            WorktreeStatusEntry::new(rest, None, Some(StatusChange::Add), false, true)?,
            false,
        )),
        "!" => Ok((
            WorktreeStatusEntry::new(rest, None, None, true, false)?,
            false,
        )),
        _ => Err(RepositoryError::GitOperationFailed),
    }
}

/// The entry for a changed path, from its two-letter `XY` status.
fn changed_entry(xy: &str, path: &str) -> Result<WorktreeStatusEntry, RepositoryError> {
    let [index, worktree] = xy.as_bytes() else {
        return Err(RepositoryError::GitOperationFailed);
    };
    let index_status = status_change(*index)?;
    let worktree_status = status_change(*worktree)?;
    if index_status.is_none() && worktree_status.is_none() {
        return Err(RepositoryError::GitOperationFailed);
    }
    WorktreeStatusEntry::new(path, index_status, worktree_status, false, false)
}

/// The change one status letter names. `.` is no change. A letter this module
/// does not map (`C` copy, `U` unmerged) refuses the listing.
fn status_change(letter: u8) -> Result<Option<StatusChange>, RepositoryError> {
    Ok(match letter {
        b'.' => None,
        b'A' => Some(StatusChange::Add),
        b'M' | b'T' => Some(StatusChange::Modify),
        b'D' => Some(StatusChange::Delete),
        b'R' => Some(StatusChange::Rename),
        _ => return Err(RepositoryError::GitOperationFailed),
    })
}

#[cfg(test)]
mod tests {
    use super::{StatusChange, WorktreeStatusEntry, parse_location, parse_worktree_status};
    use crate::RepositoryError;
    use std::path::Path;

    const ZERO: &str = "0000000000000000000000000000000000000000";

    /// The NUL-terminated listing `status -z` writes for `records`.
    fn listing<S: AsRef<str>>(records: &[S]) -> Vec<u8> {
        records
            .iter()
            .flat_map(|record| record.as_ref().bytes().chain([0]))
            .collect()
    }

    /// An ordinary-change record for `path` with the status letters `xy`.
    fn changed(xy: &str, path: &str) -> String {
        format!("1 {xy} N... 100644 100644 100644 {ZERO} {ZERO} {path}")
    }

    fn find<'a>(entries: &'a [WorktreeStatusEntry], path: &str) -> &'a WorktreeStatusEntry {
        entries
            .iter()
            .find(|entry| entry.path() == path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    /// The four facts an entry serves, as one comparable tuple.
    fn facts(
        entry: &WorktreeStatusEntry,
    ) -> (Option<StatusChange>, Option<StatusChange>, bool, bool) {
        (
            entry.index_status(),
            entry.worktree_status(),
            entry.ignored(),
            entry.untracked(),
        )
    }

    #[test]
    fn an_empty_listing_is_an_empty_status() {
        assert!(parse_worktree_status(b"").unwrap().is_empty());
    }

    #[test]
    fn each_record_maps_to_its_change_labels() {
        let rows = [
            changed(".M", "modified.txt"),
            changed("M.", "staged.txt"),
            changed("AM", "added.txt"),
            changed(".D", "gone.txt"),
            changed(".T", "link.txt"),
            changed("A.", "dir/with space.txt"),
            "? new.txt".to_owned(),
            "! target/out.bin".to_owned(),
        ];
        let entries = parse_worktree_status(&listing(&rows)).unwrap();

        assert_eq!(entries.len(), 8);
        assert_eq!(
            facts(find(&entries, "modified.txt")),
            (None, Some(StatusChange::Modify), false, false)
        );
        assert_eq!(
            facts(find(&entries, "staged.txt")),
            (Some(StatusChange::Modify), None, false, false)
        );
        assert_eq!(
            facts(find(&entries, "added.txt")),
            (
                Some(StatusChange::Add),
                Some(StatusChange::Modify),
                false,
                false
            )
        );
        assert_eq!(
            facts(find(&entries, "gone.txt")),
            (None, Some(StatusChange::Delete), false, false)
        );
        assert_eq!(
            facts(find(&entries, "link.txt")),
            (None, Some(StatusChange::Modify), false, false)
        );
        assert_eq!(
            facts(find(&entries, "dir/with space.txt")),
            (Some(StatusChange::Add), None, false, false)
        );
        assert_eq!(
            facts(find(&entries, "new.txt")),
            (None, Some(StatusChange::Add), false, true)
        );
        assert_eq!(
            facts(find(&entries, "target/out.bin")),
            (None, None, true, false)
        );
    }

    #[test]
    fn a_rename_is_reported_at_its_new_path_and_its_origin_is_skipped() {
        let rows = [
            format!("2 R. N... 100644 100644 100644 {ZERO} {ZERO} R100 new name.txt"),
            "old name.txt".to_owned(),
        ];
        let entries = parse_worktree_status(&listing(&rows)).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            facts(find(&entries, "new name.txt")),
            (Some(StatusChange::Rename), None, false, false)
        );
    }

    #[test]
    fn a_record_git_did_not_write_refuses_the_listing() {
        let refused: Vec<(&str, Vec<u8>)> = vec![
            ("a missing terminator", b"? new.txt".to_vec()),
            ("an empty record", b"\0".to_vec()),
            ("an unknown record kind", listing(&["x new.txt"])),
            ("a record without a path", listing(&["? "])),
            (
                "an unmerged record",
                listing(&[format!(
                    "u UU N... 100644 100644 100644 100644 {ZERO} {ZERO} {ZERO} conflicted.txt"
                )]),
            ),
            ("a header record", listing(&["# branch.oid 0000"])),
            (
                "a copy record",
                listing(&[
                    format!("2 C. N... 100644 100644 100644 {ZERO} {ZERO} C100 copy.txt"),
                    "source.txt".to_owned(),
                ]),
            ),
            (
                "a rename without its origin field",
                listing(&[format!(
                    "2 R. N... 100644 100644 100644 {ZERO} {ZERO} R100 new.txt"
                )]),
            ),
            (
                "an unknown status letter",
                listing(&[changed("C.", "a.txt")]),
            ),
            (
                "a change on neither side",
                listing(&[changed("..", "a.txt")]),
            ),
            (
                "a short ordinary record",
                listing(&["1 .M N... 100644 a.txt"]),
            ),
            ("a path that is not text", b"? \xff\0".to_vec()),
            (
                "a path reported twice",
                listing(&[changed(".M", "a.txt"), changed("M.", "a.txt")]),
            ),
        ];
        for (what, bytes) in refused {
            assert!(
                matches!(
                    parse_worktree_status(&bytes),
                    Err(RepositoryError::GitOperationFailed)
                ),
                "{what}"
            );
        }
    }

    #[test]
    fn the_location_is_an_absolute_git_directory_and_a_bare_flag() {
        assert_eq!(
            parse_location("/repo/.git\nfalse\n").unwrap(),
            (Path::new("/repo/.git").to_owned(), false)
        );
        assert_eq!(
            parse_location("/srv/bare.git\ntrue\n").unwrap(),
            (Path::new("/srv/bare.git").to_owned(), true)
        );
        for refused in [
            "relative/.git\nfalse\n",
            "/repo/.git\nmaybe\n",
            "/repo/.git\n",
            "/repo/.git\nfalse\nextra\n",
            "\nfalse\n",
        ] {
            assert!(
                matches!(
                    parse_location(refused),
                    Err(RepositoryError::GitOperationFailed)
                ),
                "{refused:?}"
            );
        }
    }
}
