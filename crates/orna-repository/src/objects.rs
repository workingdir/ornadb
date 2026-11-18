//! The native objects the object store holds, as `sys.git.objects` projects
//! them: each object's id, kind and size (ORNA-SYS-052).
//!
//! One `git cat-file --batch-all-objects --batch-check` run enumerates the
//! store, so one listing is one observation. Git lists an object once for each
//! store that holds it, so an id listed twice with the same kind and size is one
//! object, and the same id listed with a different kind or size refuses the
//! listing. A partial clone defers payloads that Git does not enumerate, so its
//! listing would not be the whole object set; it is refused rather than served as
//! complete. Nothing is guessed: a line that does not have the shape Git writes,
//! an id of the wrong width, a kind Git does not name or a size that is not a
//! decimal number refuses the whole listing.

use crate::{
    GitObjectKind, GitRepositoryMode, NativeObjectId, Repository, RepositoryError, listing_text,
    require_native_object_id, run_bounded_listing,
};

/// One object the object store holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedGitObject {
    oid: NativeObjectId,
    kind: GitObjectKind,
    size: u64,
}

impl CommittedGitObject {
    /// The object's id, in Git's hexadecimal spelling.
    pub fn oid(&self) -> &NativeObjectId {
        &self.oid
    }

    /// The kind Git reads the object as.
    pub fn kind(&self) -> GitObjectKind {
        self.kind
    }

    /// The object's size in bytes, as Git reports it.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// The `git cat-file` format of one object: its id, kind and size, each field
/// separated by one space and each record ended by a newline.
const OBJECT_FORMAT: &str = "--batch-check=%(objectname) %(objecttype) %(objectsize)";

impl Repository {
    /// Lists every object the object store holds, ordered by object id.
    ///
    /// One `git cat-file --batch-all-objects` run enumerates the store with
    /// lazy fetch and replacement objects disabled, so the kind and size read
    /// are the stored object's own. A partial clone or a repository whose
    /// configuration cannot be interpreted is refused, and so is a listing with
    /// more than `max_objects` distinct objects or one past the bounded-listing
    /// limit; the listing is never served truncated.
    pub fn committed_git_objects(
        &self,
        max_objects: usize,
    ) -> Result<Vec<CommittedGitObject>, RepositoryError> {
        let capabilities = self.observe_git_capabilities()?;
        if capabilities.partial_clone() || capabilities.mode() == GitRepositoryMode::Malformed {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(["cat-file", "--batch-all-objects"])
            .arg(OBJECT_FORMAT);
        let output = run_bounded_listing(command)?;

        let objects = parse_committed_git_objects(&output, self.native_object_id_length()?)?;
        if objects.len() > max_objects {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(objects)
    }
}

/// Parses the whole listing: one `<object> <kind> <size>` line per object, each
/// ended by a newline. The objects come back ordered by id, with an object listed
/// more than once collapsed to the one record every listing of it agrees on.
fn parse_committed_git_objects(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedGitObject>, RepositoryError> {
    if !listing.is_empty() && !listing.ends_with(b"\n") {
        return Err(RepositoryError::GitOperationFailed);
    }
    let mut objects = listing_text(listing)?
        .split_terminator('\n')
        .map(|line| parse_committed_git_object(line, object_id_length))
        .collect::<Result<Vec<_>, _>>()?;
    objects.sort_by(|left, right| left.oid.as_str().cmp(right.oid.as_str()));

    let mut unique: Vec<CommittedGitObject> = Vec::with_capacity(objects.len());
    for object in objects {
        if let Some(last) = unique.last()
            && last.oid == object.oid
        {
            if *last != object {
                return Err(RepositoryError::GitOperationFailed);
            }
            continue;
        }
        unique.push(object);
    }
    Ok(unique)
}

/// Reads one record: the object id, its kind and its size.
fn parse_committed_git_object(
    line: &str,
    object_id_length: usize,
) -> Result<CommittedGitObject, RepositoryError> {
    let mut fields = line.split(' ');
    let (Some(oid), Some(kind), Some(size), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(RepositoryError::GitOperationFailed);
    };
    require_native_object_id(oid, object_id_length)?;
    let kind =
        GitObjectKind::from_cat_file_name(kind).ok_or(RepositoryError::GitOperationFailed)?;
    if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(CommittedGitObject {
        oid: NativeObjectId::new(oid)?,
        kind,
        size: size
            .parse::<u64>()
            .map_err(|_| RepositoryError::GitOperationFailed)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1_WIDTH: usize = 40;
    const FIRST: &str = "1111111111111111111111111111111111111111";
    const SECOND: &str = "2222222222222222222222222222222222222222";

    fn parse(listing: &str) -> Result<Vec<CommittedGitObject>, RepositoryError> {
        parse_committed_git_objects(listing.as_bytes(), SHA1_WIDTH)
    }

    #[test]
    fn objects_are_served_in_object_id_order_with_their_kind_and_size() {
        let objects = parse(&format!("{SECOND} blob 7\n{FIRST} tree 33\n")).expect("listing");
        let ids: Vec<&str> = objects.iter().map(|object| object.oid().as_str()).collect();
        assert_eq!(ids, [FIRST, SECOND]);
        assert_eq!(objects[0].kind(), GitObjectKind::Tree);
        assert_eq!(objects[0].size(), 33);
        assert_eq!(objects[1].kind(), GitObjectKind::Blob);
        assert_eq!(objects[1].size(), 7);
    }

    #[test]
    fn an_empty_store_lists_no_object() {
        assert!(parse("").expect("empty listing").is_empty());
    }

    #[test]
    fn an_object_listed_twice_with_the_same_kind_and_size_is_one_object() {
        let objects = parse(&format!("{FIRST} blob 7\n{FIRST} blob 7\n")).expect("listing");
        assert_eq!(objects.len(), 1);
    }

    #[test]
    fn an_object_listed_with_two_kinds_refuses_the_listing() {
        assert!(matches!(
            parse(&format!("{FIRST} blob 7\n{FIRST} tree 7\n")),
            Err(RepositoryError::GitOperationFailed)
        ));
    }

    #[test]
    fn a_malformed_listing_refuses_the_whole_listing() {
        let short_id = "1".repeat(39);
        for listing in [
            format!("{FIRST} blob 7"),
            format!("{FIRST} blob\n"),
            format!("{FIRST} blob 7 extra\n"),
            format!("{FIRST} blob +7\n"),
            format!("{FIRST} missing\n"),
            format!("{FIRST} blob 7\n\n"),
        ] {
            assert!(
                matches!(parse(&listing), Err(RepositoryError::GitOperationFailed)),
                "{listing:?}"
            );
        }
        // A short id fails the object-id width check every listing shares, so it is
        // refused as InvalidObjectId.
        assert!(matches!(
            parse(&format!("{short_id} blob 7\n")),
            Err(RepositoryError::InvalidObjectId)
        ));
    }
}
