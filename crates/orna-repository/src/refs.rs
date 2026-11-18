//! The refs of a repository, as `sys.Ref` projects them: every ref under
//! `refs/` with the object it names and, for a symbolic ref, the ref it points
//! at.
//!
//! One `for-each-ref` run lists the refs together with the objects they name,
//! so the refs of one listing and what each names are one observation. Nothing
//! is guessed: a record that does not have the shape Git writes, a name that is
//! not a ref name, an object id of the wrong width, or a ref whose object is
//! missing refuses the whole listing. A symbolic ref that points at a ref which
//! does not exist names no object, so Git lists none and neither does this.
//! `HEAD` and the pseudo-refs are not under `refs/` and are not listed.

use crate::{
    NativeObjectId, Repository, RepositoryError, listing_text, require_native_object_id,
    run_bounded_listing, valid_branch_name,
};

/// One ref under `refs/`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRef {
    name: String,
    target: NativeObjectId,
    symbolic_target: Option<String>,
}

impl CommittedRef {
    /// The full ref name, `refs/heads/main` and not `main`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The object the ref names. For a symbolic ref it is the object the ref it
    /// points at names, as Git resolves it; for an annotated tag it is the tag
    /// object, not the commit the tag object tags.
    pub fn target(&self) -> &NativeObjectId {
        &self.target
    }

    /// The full name of the ref a symbolic ref points at, or `None` for a ref
    /// that names an object directly.
    pub fn symbolic_target(&self) -> Option<&str> {
        self.symbolic_target.as_deref()
    }

    /// Whether the ref lives in the namespace Orna keeps its own state in
    /// (`refs/orna/`: allocator watermarks, checkpoints, pins and the private
    /// publication candidates). They are ordinary, valid Git refs that no
    /// branch or tag listing shows; `sys.Ref.hidden` makes them visible
    /// (ORNA-SYS-050).
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with(HIDDEN_NAMESPACE)
    }

    /// Whether the ref lives under `refs/remotes/`, where a fetch records the
    /// branches of a remote.
    pub fn is_remote_tracking(&self) -> bool {
        self.name.starts_with(REMOTE_NAMESPACE)
    }

    /// The remote this ref tracks a branch of: the one of `remotes` (remote
    /// names) whose name the ref lives under (`refs/remotes/<remote>/...`), or
    /// `None` for a ref outside `refs/remotes/` or under a remote that is not
    /// among `remotes`. A remote name may hold slashes, so `refs/remotes/a/b/c`
    /// belongs to remote `a/b` rather than `a` when both exist: the longest
    /// name wins.
    pub fn remote<'a>(&self, remotes: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
        let rest = self.name.strip_prefix(REMOTE_NAMESPACE)?;
        remotes
            .into_iter()
            .filter(|remote| {
                rest.strip_prefix(remote)
                    .is_some_and(|tail| tail.starts_with('/') && tail.len() > 1)
            })
            .max_by_key(|remote| remote.len())
    }
}

/// The namespace of the refs Orna keeps its own state in.
const HIDDEN_NAMESPACE: &str = "refs/orna/";

/// The namespace of remote-tracking refs.
const REMOTE_NAMESPACE: &str = "refs/remotes/";

/// Fields of one record of the listing behind [`Repository::committed_refs`].
const REF_FIELDS: usize = 4;

/// The `for-each-ref` format of one ref record: the full ref name, the type and
/// id of the object the ref names and the ref a symbolic ref points at (empty
/// for a ref that names an object directly), each field ended by a NUL.
///
/// The object type is requested so Git must read the object: a ref whose object
/// is missing then fails the listing instead of being served. Git reads every
/// field as a C string, so no field can hold a NUL byte and the NUL that ends
/// each one is an unambiguous terminator.
const REF_FORMAT: &str = concat!(
    "--format=",
    "%(refname)%00",
    "%(objecttype)%00",
    "%(objectname)%00",
    "%(symref)%00",
);

impl Repository {
    /// Lists every ref under `refs/` in name order: the object each names and,
    /// for a symbolic ref, the ref it points at.
    ///
    /// One `for-each-ref` run observes the refs and the objects they name
    /// together. More than `max_refs` refs, or a listing larger than the
    /// bounded-listing limit, is refused rather than served truncated, and so
    /// is a ref whose object is missing: nothing is reported for a ref that
    /// cannot be read.
    pub fn committed_refs(&self, max_refs: usize) -> Result<Vec<CommittedRef>, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["for-each-ref", "--sort=refname", "--count"])
            .arg(max_refs.saturating_add(1).to_string())
            .arg(REF_FORMAT);
        let output = run_bounded_listing(command)?;

        let refs = parse_committed_refs(&output, self.native_object_id_length()?)?;
        if refs.len() > max_refs {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(refs)
    }
}

/// Whether `name` is a full ref name Git could have written: under `refs/`, with
/// a component after it, and the ref-name rules of a branch name applied to the
/// whole (a component may start with a dash, since only the whole name is
/// checked for one).
fn valid_ref_name(name: &str) -> bool {
    name.strip_prefix("refs/")
        .is_some_and(|rest| !rest.is_empty())
        && valid_branch_name(name)
}

/// Parses the whole listing: records of [`REF_FIELDS`] NUL-terminated fields,
/// each followed by the newline `for-each-ref` ends its lines with. Anything
/// else is refused.
fn parse_committed_refs(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedRef>, RepositoryError> {
    let mut rest = listing;
    let mut refs = Vec::new();
    while !rest.is_empty() {
        let mut fields: [&[u8]; REF_FIELDS] = [&[]; REF_FIELDS];
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
        refs.push(parse_committed_ref(fields, object_id_length)?);
    }
    Ok(refs)
}

/// Reads one record. The object type is one Git has; the symbolic target, when
/// there is one, is a ref name.
fn parse_committed_ref(
    fields: [&[u8]; REF_FIELDS],
    object_id_length: usize,
) -> Result<CommittedRef, RepositoryError> {
    let [name, kind, object, symref] = fields;
    let name = listing_text(name)?;
    if !valid_ref_name(name) {
        return Err(RepositoryError::GitOperationFailed);
    }
    if !matches!(kind, b"commit" | b"tree" | b"blob" | b"tag") {
        return Err(RepositoryError::GitOperationFailed);
    }
    let object = listing_text(object)?;
    require_native_object_id(object, object_id_length)?;
    let symbolic_target = match listing_text(symref)? {
        "" => None,
        target if valid_ref_name(target) => Some(target.to_owned()),
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    Ok(CommittedRef {
        name: name.to_owned(),
        target: NativeObjectId::new(object)?,
        symbolic_target,
    })
}

#[cfg(test)]
mod tests {
    use super::{CommittedRef, parse_committed_refs};
    use crate::RepositoryError;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const TAG_OBJECT: &str = "fedcba9876543210fedcba9876543210fedcba98";

    /// One record as `for-each-ref` prints it.
    fn record(fields: [&str; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in fields {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
        }
        bytes.push(b'\n');
        bytes
    }

    fn direct(name: &str, kind: &str, object: &str) -> Vec<u8> {
        record([name, kind, object, ""])
    }

    fn parse(bytes: &[u8]) -> Result<Vec<CommittedRef>, RepositoryError> {
        parse_committed_refs(bytes, 40)
    }

    fn one(bytes: &[u8]) -> CommittedRef {
        let mut refs = parse(bytes).expect("one ref");
        assert_eq!(refs.len(), 1);
        refs.remove(0)
    }

    #[test]
    fn a_ref_names_its_full_name_and_the_object_it_names() {
        for kind in ["commit", "tree", "blob", "tag"] {
            let named = one(&direct("refs/tags/v1", kind, TAG_OBJECT));
            assert_eq!(named.name(), "refs/tags/v1");
            assert_eq!(named.target().as_str(), TAG_OBJECT, "{kind}");
            assert_eq!(named.symbolic_target(), None);
        }
    }

    #[test]
    fn a_symbolic_ref_names_the_ref_it_points_at_and_the_object_that_resolves_to() {
        let symbolic = one(&record([
            "refs/remotes/origin/HEAD",
            "commit",
            COMMIT,
            "refs/remotes/origin/main",
        ]));
        assert_eq!(symbolic.target().as_str(), COMMIT);
        assert_eq!(symbolic.symbolic_target(), Some("refs/remotes/origin/main"));
    }

    #[test]
    fn the_orna_namespace_is_hidden_and_nothing_else_is() {
        for hidden in [
            "refs/orna/ids/00112233",
            "refs/orna/pins/abc/scratch/row-1",
            "refs/orna/x",
        ] {
            assert!(
                one(&direct(hidden, "commit", COMMIT)).is_hidden(),
                "{hidden}"
            );
        }
        for visible in [
            "refs/heads/main",
            "refs/heads/orna/topic",
            "refs/orna",
            "refs/orna-diff-fixture/abc",
            "refs/tags/refs/orna/x",
        ] {
            assert!(
                !one(&direct(visible, "commit", COMMIT)).is_hidden(),
                "{visible}"
            );
        }
    }

    #[test]
    fn a_remote_tracking_ref_belongs_to_the_remote_it_lives_under() {
        let configured = ["fork", "origin"];
        let remote = |name: &str| {
            one(&direct(name, "commit", COMMIT))
                .remote(configured)
                .map(str::to_owned)
        };
        assert_eq!(
            remote("refs/remotes/origin/main").as_deref(),
            Some("origin")
        );
        assert_eq!(
            remote("refs/remotes/origin/feature/x").as_deref(),
            Some("origin"),
            "the branch may hold slashes"
        );
        assert_eq!(remote("refs/remotes/fork/main").as_deref(), Some("fork"));
        assert_eq!(
            remote("refs/remotes/gone/main"),
            None,
            "a remote that is not configured has no row to refer to"
        );
        assert_eq!(
            remote("refs/remotes/originx/main"),
            None,
            "a prefix of the name is not the remote"
        );
        assert_eq!(
            remote("refs/remotes/origin"),
            None,
            "a ref with no branch under the remote belongs to none"
        );
        for outside in [
            "refs/heads/origin/main",
            "refs/tags/origin/v1",
            "refs/orna/x",
        ] {
            assert_eq!(remote(outside), None, "{outside}");
        }
    }

    #[test]
    fn the_longest_remote_name_wins_when_remote_names_hold_slashes() {
        let configured = ["a", "a/b"];
        let named = one(&direct("refs/remotes/a/b/c", "commit", COMMIT));
        assert_eq!(named.remote(configured), Some("a/b"));
        let shallow = one(&direct("refs/remotes/a/c", "commit", COMMIT));
        assert_eq!(shallow.remote(configured), Some("a"));
    }

    #[test]
    fn only_refs_under_refs_remotes_are_remote_tracking() {
        for tracking in ["refs/remotes/origin/main", "refs/remotes/origin/HEAD"] {
            assert!(
                one(&direct(tracking, "commit", COMMIT)).is_remote_tracking(),
                "{tracking}"
            );
        }
        for other in ["refs/heads/main", "refs/remote/origin/main", "refs/orna/x"] {
            assert!(
                !one(&direct(other, "commit", COMMIT)).is_remote_tracking(),
                "{other}"
            );
        }
    }

    #[test]
    fn records_are_read_in_order_and_an_empty_listing_has_no_ref() {
        assert_eq!(parse(b"").expect("an empty listing"), Vec::new());
        let mut listing = direct("refs/heads/a", "commit", COMMIT);
        listing.extend(direct("refs/heads/b", "commit", COMMIT));
        listing.extend(direct("refs/stash", "commit", COMMIT));
        let names = parse(&listing)
            .expect("three refs")
            .iter()
            .map(|named| named.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["refs/heads/a", "refs/heads/b", "refs/stash"]);
    }

    #[test]
    fn a_ref_component_may_start_with_a_dash() {
        assert_eq!(
            one(&direct("refs/tags/-odd", "commit", COMMIT)).name(),
            "refs/tags/-odd"
        );
    }

    #[test]
    fn a_record_git_did_not_write_is_refused() {
        let good = direct("refs/heads/main", "commit", COMMIT);
        let refused: Vec<(&str, Vec<u8>)> = vec![
            ("no final newline", good[..good.len() - 1].to_vec()),
            ("a short record", b"refs/heads/main\0commit\0".to_vec()),
            ("an unterminated field", b"refs/heads/main".to_vec()),
            (
                "an unknown object type",
                direct("refs/heads/main", "snapshot", COMMIT),
            ),
            (
                "an empty object type",
                direct("refs/heads/main", "", COMMIT),
            ),
            (
                "a name outside refs",
                direct("heads/main", "commit", COMMIT),
            ),
            ("the bare namespace", direct("refs/", "commit", COMMIT)),
            (
                "an invalid name",
                direct("refs/heads/a..b", "commit", COMMIT),
            ),
            ("an empty name", direct("", "commit", COMMIT)),
            (
                "an object id of the wrong width",
                direct("refs/heads/main", "commit", "0123"),
            ),
            (
                "an object id that is not hexadecimal",
                direct("refs/heads/main", "commit", &"g".repeat(40)),
            ),
            (
                "a symbolic target that is not a ref name",
                record(["refs/heads/main", "commit", COMMIT, "main"]),
            ),
            (
                "a name that is not text",
                [
                    b"refs/heads/\xff\0commit\0".as_slice(),
                    COMMIT.as_bytes(),
                    b"\0\0\n",
                ]
                .concat(),
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

    /// The whole listing is one Git run, however many refs it lists, so the refs
    /// and the objects they name are one observation.
    #[test]
    fn one_git_run_lists_every_ref() {
        let directory = tempfile::tempdir().expect("repository directory");
        test_support::initialize_fixture_repository(directory.path());
        test_support::commit_initial_module(directory.path());
        for name in ["a", "b", "c"] {
            test_support::git(directory.path(), &["branch", name]);
            test_support::git(directory.path(), &["tag", "-a", "-m", name, name]);
        }
        let repository = Repository::discover(directory.path()).expect("a Git worktree");
        // The object format is read once per handle and not by the listing.
        repository.committed_refs(64).expect("a first listing");

        git_command::reset_git_process_requests();
        let refs = repository.committed_refs(64).expect("a listing");
        assert_eq!(refs.len(), 7, "main, three branches and three tags");
        assert_eq!(git_command::git_process_requests(), 1);
    }
}
