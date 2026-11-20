//! The native objects the object store holds, and the ones a partial clone
//! promises, as `sys.git.objects` projects them: each object's id, kind and where
//! its payload is (ORNA-SYS-052).
//!
//! One `git cat-file --batch-all-objects --batch-check` run enumerates the local
//! store, so one listing is one observation. Git lists an object once for each
//! store that holds it, so an id listed twice with the same kind and size is one
//! object, and the same id listed with a different kind, size or availability
//! refuses the listing. In a partial clone, one `git rev-list --objects
//! --missing=print-info` run names each reachable object the local store does not
//! hold. Such an object is represented as promised by the promisor remote that
//! serves it, and has no size: its payload is neither fetched nor invented. A page
//! holds only its own objects, so the contradiction check compares only the ids the
//! page holds: an id the page has already let go is not compared again. Nothing is
//! guessed: a line that does not have the shape Git writes, an id of the wrong
//! width, a kind Git does not name, a size that is not a decimal number, a missing
//! object that is not a blob, a line past the per-line bound, or a promisor remote
//! that cannot be named refuses the whole listing.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use crate::{
    ConfigValue, GitObjectKind, GitRepositoryMode, NativeObjectId, Repository, RepositoryError,
    for_each_listing_line, require_native_object_id,
};

/// Where one object's payload is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitObjectAvailability {
    /// The local store holds the object, and its stored size is `size` bytes.
    Local { size: u64 },
    /// The local store does not hold the object. A partial clone promises it, and
    /// `remote` is the promisor remote that serves it.
    Promised { remote: String },
}

/// One object the repository holds, or promises.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedGitObject {
    oid: NativeObjectId,
    kind: GitObjectKind,
    availability: GitObjectAvailability,
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

    /// Where the object's payload is.
    pub fn availability(&self) -> &GitObjectAvailability {
        &self.availability
    }
}

/// The `git cat-file` format of one object: its id, kind and size, each field
/// separated by one space and each record ended by a newline.
const OBJECT_FORMAT: &str = "--batch-check=%(objectname) %(objecttype) %(objectsize)";

/// The objects one page holds, keyed by id: the `count` smallest ids after the cursor,
/// so a page never holds more than `count` objects however large the store.
struct ObjectWindow<'a> {
    after: Option<&'a [u8]>,
    count: usize,
    objects: BTreeMap<NativeObjectId, (GitObjectKind, GitObjectAvailability)>,
}

impl<'a> ObjectWindow<'a> {
    fn new(after: Option<&'a [u8]>, count: usize) -> Self {
        Self {
            after,
            count,
            objects: BTreeMap::new(),
        }
    }

    /// Records one listed object. An id at or before the cursor is not on the page and
    /// is skipped. An id the page already holds is the same object when its kind and
    /// availability agree, and a contradiction refuses the listing otherwise. A new id
    /// is held while the page has room; once it is full, a new id smaller than the
    /// largest held one replaces that one, and any other id lies beyond the page.
    fn record(
        &mut self,
        oid: NativeObjectId,
        kind: GitObjectKind,
        availability: GitObjectAvailability,
    ) -> Result<(), RepositoryError> {
        if self
            .after
            .is_some_and(|after| oid.as_str().as_bytes() <= after)
        {
            return Ok(());
        }
        if let Some((known_kind, known_availability)) = self.objects.get(&oid) {
            return if *known_kind == kind && *known_availability == availability {
                Ok(())
            } else {
                Err(RepositoryError::GitOperationFailed)
            };
        }
        if self.objects.len() < self.count {
            self.objects.insert(oid, (kind, availability));
        } else if self
            .objects
            .last_key_value()
            .is_some_and(|(largest, _)| oid < *largest)
        {
            self.objects.pop_last();
            self.objects.insert(oid, (kind, availability));
        }
        Ok(())
    }

    /// The held objects in object-id order.
    fn into_objects(self) -> Vec<CommittedGitObject> {
        self.objects
            .into_iter()
            .map(|(oid, (kind, availability))| CommittedGitObject {
                oid,
                kind,
                availability,
            })
            .collect()
    }
}

impl Repository {
    /// Lists the objects the local store holds, and in a partial clone the reachable
    /// objects it does not hold, in object-id order, from the first one after `after`
    /// (or the first of all when `after` is `None`) and at most `count` of them. Each
    /// page reads both listings through to their end, so every page reads the whole store
    /// again and holds only the objects it returns.
    ///
    /// Both listings run with lazy fetch and replacement objects disabled, so the kind
    /// and size read are the stored object's own and nothing is fetched. A repository
    /// whose configuration cannot be interpreted is refused, and so is a listing with a
    /// line past the per-line bound; the listing is never served truncated. Each listing
    /// is read line by line and never held whole, so only the page's objects are held,
    /// and those are bounded by `count`.
    pub fn committed_git_objects_after(
        &self,
        after: Option<&[u8]>,
        count: usize,
    ) -> Result<Vec<CommittedGitObject>, RepositoryError> {
        let capabilities = self.observe_git_capabilities()?;
        if capabilities.mode() == GitRepositoryMode::Malformed {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object_id_length = self.native_object_id_length()?;
        let mut objects = ObjectWindow::new(after, count);
        for_each_listing_line(self.stored_listing_command(), |line| {
            record_stored_object(line, object_id_length, &mut objects)
        })?;
        if capabilities.partial_clone() {
            // The promisor remote is named only once a promised object is found.
            let mut promisor: Option<String> = None;
            for_each_listing_line(self.reachable_listing_command(), |line| {
                let Some((oid, kind)) = parse_reachable_line(line, object_id_length)? else {
                    return Ok(());
                };
                let remote = match &promisor {
                    Some(remote) => remote.clone(),
                    None => {
                        let remote = promised_object_remote(
                            self.local_config_value("extensions.partialClone")?,
                            capabilities.promisor_remotes(),
                        )?;
                        promisor = Some(remote.clone());
                        remote
                    }
                };
                objects.record(oid, kind, GitObjectAvailability::Promised { remote })
            })?;
        }
        Ok(objects.into_objects())
    }

    /// The local store's listing: one `git cat-file --batch-all-objects` run, read
    /// line by line.
    fn stored_listing_command(&self) -> Command {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .stderr(Stdio::null())
            .args(["cat-file", "--batch-all-objects"])
            .arg(OBJECT_FORMAT);
        command
    }

    /// The reachable listing of a partial clone: one `git rev-list` run that names
    /// each object the local store does not hold, read line by line.
    fn reachable_listing_command(&self) -> Command {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .stderr(Stdio::null())
            .args([
                "rev-list",
                "--objects",
                "--missing=print-info",
                "--no-object-names",
                "--all",
            ]);
        command
    }
}

/// Records one line of the local listing: `<object> <kind> <size>`.
fn record_stored_object(
    line: &str,
    object_id_length: usize,
    objects: &mut ObjectWindow<'_>,
) -> Result<(), RepositoryError> {
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
    let size = size
        .parse::<u64>()
        .map_err(|_| RepositoryError::GitOperationFailed)?;
    objects.record(
        NativeObjectId::new(oid)?,
        kind,
        GitObjectAvailability::Local { size },
    )
}

/// Reads one line of a partial clone's reachable listing. An object the local store
/// holds is its id alone and is skipped, since the local listing already holds it;
/// a missing object is read by [`parse_missing_object`] and returned. Git does not
/// walk beneath a missing tree or commit, so only blobs may be missing: any other
/// kind refuses the listing.
fn parse_reachable_line(
    line: &str,
    object_id_length: usize,
) -> Result<Option<(NativeObjectId, GitObjectKind)>, RepositoryError> {
    if !line.starts_with('?') {
        require_native_object_id(line, object_id_length)?;
        return Ok(None);
    }
    let (oid, kind) = parse_missing_object(line, object_id_length)?;
    if kind != GitObjectKind::Blob {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(Some((oid, kind)))
}

/// Reads one missing object: `?<object>`, then the path Git names it at when it names
/// one, then `type=<kind>` as the last field. A path may itself contain ` type=`, so
/// the kind is the text after the last one.
fn parse_missing_object(
    line: &str,
    object_id_length: usize,
) -> Result<(NativeObjectId, GitObjectKind), RepositoryError> {
    let oid = line
        .strip_prefix('?')
        .and_then(|marked| marked.split(' ').next())
        .ok_or(RepositoryError::GitOperationFailed)?;
    let (_, kind) = line
        .rsplit_once(" type=")
        .ok_or(RepositoryError::GitOperationFailed)?;
    require_native_object_id(oid, object_id_length)?;
    let kind =
        GitObjectKind::from_cat_file_name(kind).ok_or(RepositoryError::GitOperationFailed)?;
    Ok((NativeObjectId::new(oid)?, kind))
}

/// The promisor remote that serves a partial clone's missing objects: the one
/// `extensions.partialClone` names, which must be a promisor remote, or the only
/// promisor remote when that setting is absent. Any other choice refuses.
fn promised_object_remote(
    configured: ConfigValue,
    promisors: &[String],
) -> Result<String, RepositoryError> {
    match configured {
        ConfigValue::Value(name) if promisors.contains(&name) => Ok(name),
        ConfigValue::Missing => match promisors {
            [only] => Ok(only.clone()),
            _ => Err(RepositoryError::GitOperationFailed),
        },
        ConfigValue::Value(_) | ConfigValue::Malformed => Err(RepositoryError::GitOperationFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Repository, for_each_line_of, test_support};

    const SHA1_WIDTH: usize = 40;
    const FIRST: &str = "1111111111111111111111111111111111111111";
    const SECOND: &str = "2222222222222222222222222222222222222222";
    const THIRD: &str = "3333333333333333333333333333333333333333";

    fn parse(listing: &str) -> Result<Vec<CommittedGitObject>, RepositoryError> {
        let mut objects = ObjectWindow::new(None, usize::MAX);
        for_each_line_of(listing.as_bytes(), |line| {
            record_stored_object(line, SHA1_WIDTH, &mut objects)
        })?;
        Ok(objects.into_objects())
    }

    fn reachable(listing: &str) -> Result<Vec<(NativeObjectId, GitObjectKind)>, RepositoryError> {
        let mut missing = Vec::new();
        for_each_line_of(listing.as_bytes(), |line| {
            missing.extend(parse_reachable_line(line, SHA1_WIDTH)?);
            Ok(())
        })?;
        Ok(missing)
    }

    #[test]
    fn objects_are_served_in_object_id_order_with_their_kind_and_size() {
        let objects = parse(&format!("{SECOND} blob 7\n{FIRST} tree 33\n")).expect("listing");
        let ids: Vec<&str> = objects.iter().map(|object| object.oid().as_str()).collect();
        assert_eq!(ids, [FIRST, SECOND]);
        assert_eq!(objects[0].kind(), GitObjectKind::Tree);
        assert_eq!(
            objects[0].availability(),
            &GitObjectAvailability::Local { size: 33 }
        );
        assert_eq!(objects[1].kind(), GitObjectKind::Blob);
        assert_eq!(
            objects[1].availability(),
            &GitObjectAvailability::Local { size: 7 }
        );
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
    fn an_object_listed_as_local_and_as_promised_refuses_the_listing() {
        let mut objects = ObjectWindow::new(None, usize::MAX);
        let oid = NativeObjectId::new(FIRST).expect("an id");
        objects
            .record(
                oid.clone(),
                GitObjectKind::Blob,
                GitObjectAvailability::Local { size: 7 },
            )
            .expect("a first record");
        assert!(matches!(
            objects.record(
                oid,
                GitObjectKind::Blob,
                GitObjectAvailability::Promised {
                    remote: "origin".to_owned(),
                },
            ),
            Err(RepositoryError::GitOperationFailed)
        ));
    }

    /// The ids a window holds after `listed` is recorded in order, each a blob held
    /// locally at seven bytes.
    fn window_of(
        after: Option<&[u8]>,
        count: usize,
        listed: &[&str],
    ) -> Result<Vec<String>, RepositoryError> {
        let mut window = ObjectWindow::new(after, count);
        for &oid in listed {
            window.record(
                NativeObjectId::new(oid)?,
                GitObjectKind::Blob,
                GitObjectAvailability::Local { size: 7 },
            )?;
        }
        Ok(window
            .into_objects()
            .iter()
            .map(|object| object.oid().as_str().to_owned())
            .collect())
    }

    #[test]
    fn a_page_holds_its_count_of_the_smallest_ids_after_its_cursor() {
        assert_eq!(
            window_of(None, 2, &[THIRD, SECOND, FIRST]).expect("a page"),
            [FIRST, SECOND]
        );
    }

    #[test]
    fn a_page_skips_the_ids_at_or_before_its_cursor() {
        assert_eq!(
            window_of(Some(SECOND.as_bytes()), usize::MAX, &[FIRST, SECOND, THIRD])
                .expect("a page"),
            [THIRD]
        );
    }

    #[test]
    fn a_page_of_no_objects_holds_nothing() {
        assert!(window_of(None, 0, &[FIRST]).expect("a page").is_empty());
    }

    #[test]
    fn an_object_the_page_holds_listed_with_another_kind_refuses_the_page() {
        let mut window = ObjectWindow::new(None, 1);
        let held = GitObjectAvailability::Local { size: 7 };
        window
            .record(
                NativeObjectId::new(FIRST).expect("an id"),
                GitObjectKind::Blob,
                held.clone(),
            )
            .expect("a first record");
        assert!(matches!(
            window.record(
                NativeObjectId::new(FIRST).expect("an id"),
                GitObjectKind::Tree,
                held,
            ),
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

    #[test]
    fn a_missing_blob_is_read_by_its_kind_whatever_path_it_appears_at() {
        // A path may itself contain ` type=`; the kind is the last field.
        let listing = format!("{FIRST}\n?{SECOND} path=notes type=tree/x type=blob\n");
        let missing = reachable(&listing).expect("listing");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0.as_str(), SECOND);
        assert_eq!(missing[0].1, GitObjectKind::Blob);

        let pathless = reachable(&format!("?{FIRST} type=blob\n")).expect("listing");
        assert_eq!(pathless[0].0.as_str(), FIRST);
    }

    #[test]
    fn a_missing_tree_or_commit_refuses_the_listing() {
        for listing in [
            format!("?{SECOND} type=commit\n"),
            format!("?{SECOND} path=src type=tree\n"),
        ] {
            assert!(
                matches!(
                    reachable(&listing),
                    Err(RepositoryError::GitOperationFailed)
                ),
                "{listing:?}"
            );
        }
    }

    #[test]
    fn a_missing_object_without_a_known_kind_refuses_the_listing() {
        for listing in [
            format!("?{SECOND} path=a\n"),
            format!("?{SECOND} type=widget\n"),
            format!("?{SECOND} type=blob extra\n"),
            format!("?{SECOND} type=blob"),
            format!("?{SECOND}\n"),
            format!("{FIRST} extra\n"),
        ] {
            assert!(reachable(&listing).is_err(), "{listing:?}");
        }
        let short_id = "2".repeat(39);
        assert!(matches!(
            reachable(&format!("?{short_id} type=blob\n")),
            Err(RepositoryError::InvalidObjectId)
        ));
    }

    #[test]
    fn a_promised_object_names_the_promisor_remote_that_serves_it() {
        let origin = ["origin".to_owned()];
        assert_eq!(
            promised_object_remote(ConfigValue::Missing, &origin).expect("a remote"),
            "origin"
        );
        assert_eq!(
            promised_object_remote(ConfigValue::Value("origin".to_owned()), &origin)
                .expect("a remote"),
            "origin"
        );
    }

    #[test]
    fn a_promised_object_with_no_nameable_promisor_remote_refuses_the_listing() {
        let origin = ["origin".to_owned()];
        let two = ["origin".to_owned(), "mirror".to_owned()];
        assert!(promised_object_remote(ConfigValue::Missing, &two).is_err());
        assert!(promised_object_remote(ConfigValue::Missing, &[]).is_err());
        assert!(promised_object_remote(ConfigValue::Value("mirror".to_owned()), &origin).is_err());
        assert!(promised_object_remote(ConfigValue::Malformed, &origin).is_err());
    }

    /// Past the former fifty-thousand-object limit: every object is served once, in
    /// object-id order, when the store is paged twenty thousand at a time.
    #[test]
    fn a_store_past_fifty_thousand_objects_pages_through_each_object_once() {
        const OBJECTS: usize = 50_001;
        const PAGE: usize = 20_000;
        let directory = tempfile::tempdir().expect("repository directory");
        test_support::initialize_fixture_repository(directory.path());
        let mut stream = String::new();
        for index in 0..OBJECTS {
            let content = format!("object {index}\n");
            stream.push_str(&format!("blob\ndata {}\n{content}\n", content.len()));
        }
        test_support::git_with_input(directory.path(), &["fast-import", "--quiet"], &stream);
        let repository = Repository::discover(directory.path()).expect("a Git worktree");

        let everything = repository
            .committed_git_objects_after(None, usize::MAX)
            .expect("every object");
        let mut paged = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = repository
                .committed_git_objects_after(after.as_deref().map(str::as_bytes), PAGE)
                .expect("a page of the store");
            let Some(last) = page.last() else {
                break;
            };
            after = Some(last.oid().as_str().to_owned());
            paged.extend(page.iter().map(|object| object.oid().as_str().to_owned()));
        }
        let listed: Vec<String> = everything
            .iter()
            .map(|object| object.oid().as_str().to_owned())
            .collect();
        assert_eq!(listed.len(), OBJECTS);
        assert_eq!(paged, listed);
    }
}
