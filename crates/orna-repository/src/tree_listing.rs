//! The entries of a committed tree, as `sys.TreeEntry` projects them: every entry
//! under one root tree, recursively, trees included, each with the path it is
//! filed under relative to that root.
//!
//! One `git ls-tree -r -t` run reads the tree objects of one root. A root names
//! immutable objects, so the entries of one listing are one observation. A listing
//! refuses the whole result rather than serving part of it: an entry whose path is
//! not UTF-8, whose mode is not six octal digits, whose kind Git does not name or
//! whose object id is not of the repository's width fails closed, and so does a
//! listing larger than the bounded-listing limit or with more entries than the
//! caller allows.

use crate::{
    GitObjectKind, GitTreeRecord, NativeObjectId, Repository, RepositoryError, listing_text,
    require_native_object_id, run_bounded_listing,
};

/// The digits of a mode as Git writes it (`%06o` in `ls-tree`).
const MODE_DIGITS: usize = 6;

/// One entry of a committed tree listing: its mode, the kind of object it names,
/// that object and the path it is filed under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTreeListingEntry {
    mode: u32,
    kind: GitObjectKind,
    object: NativeObjectId,
    path: String,
}

impl CommittedTreeListingEntry {
    /// The mode as a number: Git writes it as six octal digits, so a regular file
    /// (`100644`) is `33188`.
    pub fn mode(&self) -> u32 {
        self.mode
    }

    /// The kind of object the entry names, as Git lists it.
    pub fn kind(&self) -> GitObjectKind {
        self.kind
    }

    /// The object the entry names.
    pub fn object(&self) -> &NativeObjectId {
        &self.object
    }

    /// The path of the entry relative to the root tree, with `/` between its
    /// components.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Repository {
    /// Lists every entry under the tree `tree`, recursively: each tree entry, then
    /// the entries under it, in Git's order (`git ls-tree -r -t`). `tree` names a
    /// tree object, such as the root tree a commit records.
    ///
    /// More than `max_entries` entries, or a listing larger than the bounded-listing
    /// limit, is refused rather than served truncated, and so is an entry Git cannot
    /// read. Nothing is fetched: a tree that is not available locally fails closed.
    pub fn committed_tree_listing(
        &self,
        tree: &NativeObjectId,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeListingEntry>, RepositoryError> {
        self.objects().check_expansion(tree.as_str(), "")?;
        let object_id_length = self.native_object_id_length()?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-r", "-t", "-z", "--full-tree"])
            .arg(tree.as_str());
        let listing = run_bounded_listing(command)?;
        let entries = parse_committed_tree_listing(&listing, object_id_length)?;
        if entries.len() > max_entries {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
    }
}

/// Parses the whole listing: one NUL-terminated record per entry, each of the form
/// `<mode> SP <kind> SP <object> TAB <path>`. An empty listing is a tree with no
/// entries; anything else that Git did not write is refused.
fn parse_committed_tree_listing(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedTreeListingEntry>, RepositoryError> {
    if listing.is_empty() {
        return Ok(Vec::new());
    }
    let body = listing
        .strip_suffix(b"\0")
        .ok_or(RepositoryError::GitOperationFailed)?;
    body.split(|byte| *byte == b'\0')
        .map(|record| parse_committed_tree_listing_entry(record, object_id_length))
        .collect()
}

/// Parses one record of the listing.
fn parse_committed_tree_listing_entry(
    record: &[u8],
    object_id_length: usize,
) -> Result<CommittedTreeListingEntry, RepositoryError> {
    let record = GitTreeRecord::parse(record).ok_or(RepositoryError::GitOperationFailed)?;
    let mode = parse_mode(record.mode)?;
    let kind = match record.kind {
        b"blob" => GitObjectKind::Blob,
        b"tree" => GitObjectKind::Tree,
        b"commit" => GitObjectKind::Commit,
        b"tag" => GitObjectKind::Tag,
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    let object = listing_text(record.object)?;
    require_native_object_id(object, object_id_length)?;
    let object = NativeObjectId::new(object)?;
    let path =
        String::from_utf8(record.path.to_vec()).map_err(|_| RepositoryError::GitOperationFailed)?;
    Ok(CommittedTreeListingEntry {
        mode,
        kind,
        object,
        path,
    })
}

/// A mode as Git writes it: exactly six octal digits. Six octal digits never
/// overflow a `u32`.
fn parse_mode(text: &[u8]) -> Result<u32, RepositoryError> {
    if text.len() != MODE_DIGITS || !text.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(text
        .iter()
        .fold(0, |mode, byte| mode * 8 + u32::from(byte - b'0')))
}

#[cfg(test)]
mod tests {
    use super::{CommittedTreeListingEntry, parse_committed_tree_listing};
    use crate::GitObjectKind;

    const TREE: &str = "2222222222222222222222222222222222222222";
    const BLOB: &str = "1111111111111111111111111111111111111111";
    const COMMIT: &str = "3333333333333333333333333333333333333333";

    fn listing(records: &[String]) -> Vec<u8> {
        records
            .iter()
            .flat_map(|record| record.bytes().chain(std::iter::once(0)))
            .collect()
    }

    fn parsed(records: &[String]) -> Vec<CommittedTreeListingEntry> {
        parse_committed_tree_listing(&listing(records), 40).expect("a listing Git wrote")
    }

    #[test]
    fn an_empty_listing_lists_no_entry() {
        assert!(
            parse_committed_tree_listing(b"", 40)
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn every_entry_carries_its_octal_mode_kind_object_and_relative_path() {
        let entries = parsed(&[
            format!("040000 tree {TREE}\tdir"),
            format!("100644 blob {BLOB}\tdir/a.txt"),
            format!("100755 blob {BLOB}\tbin"),
            format!("120000 blob {BLOB}\tlink"),
            format!("160000 commit {COMMIT}\tsub"),
        ]);
        let shape = entries
            .iter()
            .map(|entry| (entry.mode(), entry.kind(), entry.path()))
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            [
                (0o040000, GitObjectKind::Tree, "dir"),
                (0o100644, GitObjectKind::Blob, "dir/a.txt"),
                (0o100755, GitObjectKind::Blob, "bin"),
                (0o120000, GitObjectKind::Blob, "link"),
                (0o160000, GitObjectKind::Commit, "sub"),
            ]
        );
        assert_eq!(entries[1].mode(), 33188);
        assert_eq!(entries[1].object().as_str(), BLOB);
    }

    #[test]
    fn a_record_git_did_not_write_fails_closed() {
        let refused = [
            format!("10064 blob {BLOB}\tshort-mode"),
            format!("100648 blob {BLOB}\tnot-octal"),
            format!("100644 link {BLOB}\tunknown-kind"),
            format!("100644 blob {}\tshort-object", &BLOB[..39]),
            format!("100644 blob {BLOB}"),
        ];
        for record in refused {
            assert!(
                parse_committed_tree_listing(&listing(std::slice::from_ref(&record)), 40).is_err(),
                "refused {record:?}"
            );
        }
    }

    #[test]
    fn a_path_that_is_not_utf8_fails_closed() {
        let mut record = format!("100644 blob {BLOB}\t").into_bytes();
        record.push(0xff);
        record.push(0);
        assert!(parse_committed_tree_listing(&record, 40).is_err());
    }

    #[test]
    fn a_listing_without_its_final_terminator_fails_closed() {
        let record = format!("100644 blob {BLOB}\tfile");
        assert!(parse_committed_tree_listing(record.as_bytes(), 40).is_err());
    }

    #[test]
    fn an_object_of_another_width_fails_closed() {
        let record = format!("100644 blob {BLOB}\tfile");
        assert!(parse_committed_tree_listing(&listing(&[record]), 64).is_err());
    }
}
