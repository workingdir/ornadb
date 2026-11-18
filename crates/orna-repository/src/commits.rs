//! The commits of a repository's history, as `sys.Commit` projects them: the
//! commit each id names, its tree, the parents `git log` prints for it, its
//! author and committer, its message and when each was recorded.
//!
//! A history page is one `git log` run over the walk from one commit, and a
//! parent window is one `git log --no-walk` run over ids, so each listing is
//! one observation of the object store. Nothing is guessed: a record that does
//! not have the shape Git writes, an object id of the wrong width, a time that
//! is not a number or a commit Git cannot read refuses the whole listing.
//!
//! Git prints no parents for a commit listed in the repository's `shallow`
//! file, though the commit object still names them. [`Repository::shallow_boundaries`]
//! names those commits so a caller can report their parents as unavailable.

use std::collections::{HashMap, HashSet};

use crate::{
    GitPerson, NativeObjectId, Repository, RepositoryError, listing_text, parse_unix_seconds,
    require_native_object_id, run_bounded_listing,
};

/// One commit as `sys.Commit` projects it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedCommit {
    commit: NativeObjectId,
    tree: NativeObjectId,
    parents: Vec<NativeObjectId>,
    author: GitPerson,
    authored: i64,
    committer: GitPerson,
    committed: i64,
    message: String,
}

impl CommittedCommit {
    /// The commit's own object id.
    pub fn commit(&self) -> &NativeObjectId {
        &self.commit
    }

    /// The root tree the commit records.
    pub fn tree(&self) -> &NativeObjectId {
        &self.tree
    }

    /// The parents `git log` prints, in the order the commit records them.
    /// Empty for a root commit, and for a commit in the shallow file (see the
    /// module documentation): an empty list there is not the commit's parents.
    pub fn parents(&self) -> &[NativeObjectId] {
        &self.parents
    }

    /// The author: who wrote the change.
    pub fn author(&self) -> &GitPerson {
        &self.author
    }

    /// When the author wrote the change, in seconds since the Unix epoch.
    pub fn authored_unix_seconds(&self) -> i64 {
        self.authored
    }

    /// The committer: who recorded the commit.
    pub fn committer(&self) -> &GitPerson {
        &self.committer
    }

    /// When the commit was recorded, in seconds since the Unix epoch.
    pub fn committed_unix_seconds(&self) -> i64 {
        self.committed
    }

    /// The raw commit message (`git log --format=%B`): the subject, the body
    /// and any trailing newline, exactly as Git stores them.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Fields of one record of the listings behind [`Repository::committed_commit_page`]
/// and [`Repository::committed_commits`].
const COMMIT_FIELDS: usize = 10;

/// The `git log` format of one commit: the commit id, the parent ids (space
/// separated, empty for a root), the tree id, the author's name, address and
/// date, the committer's name, address and date, and the raw message. Each
/// field ends at its NUL terminator; the message is the last field.
const COMMIT_FORMAT: &str = concat!(
    "--format=",
    "%H%x00",
    "%P%x00",
    "%T%x00",
    "%an%x00",
    "%ae%x00",
    "%at%x00",
    "%cn%x00",
    "%ce%x00",
    "%ct%x00",
    "%B%x00",
);

impl Repository {
    /// Lists up to `max_count` commits of the walk from `from`, skipping the
    /// first `skip`. The walk is `git log`'s default order, a function of the
    /// commit graph alone, so the same `from` lists the same commits at every
    /// `skip`.
    ///
    /// A listing larger than the bounded-listing limit backs off to a shorter
    /// run, so the result holds fewer than `max_count` commits only when the
    /// history ends before them. A commit Git cannot read refuses the listing.
    pub fn committed_commit_page(
        &self,
        from: &NativeObjectId,
        skip: u64,
        max_count: usize,
    ) -> Result<Vec<CommittedCommit>, RepositoryError> {
        let object_id_length = self.native_object_id_length()?;
        let mut commits = Vec::new();
        let mut next = skip;
        let mut size = max_count;
        while commits.len() < max_count {
            let requested = size.min(max_count - commits.len());
            let listing = match self.committed_commit_page_listing(from, next, requested) {
                Ok(listing) => listing,
                Err(_) if requested > 1 => {
                    size = requested / 2;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let chunk = parse_committed_commits(&listing, object_id_length)?;
            let exhausted = chunk.len() < requested;
            let advanced = u64::try_from(chunk.len())
                .ok()
                .and_then(|count| next.checked_add(count))
                .ok_or(RepositoryError::GitOperationFailed)?;
            next = advanced;
            commits.extend(chunk);
            if exhausted {
                break;
            }
        }
        Ok(commits)
    }

    /// The commits `oids` name, read in one `git log --no-walk` run and keyed
    /// by id. A repeated id is read once. An id Git cannot read, or one the
    /// listing omits, refuses the whole read.
    pub fn committed_commits(
        &self,
        oids: &[NativeObjectId],
    ) -> Result<HashMap<NativeObjectId, CommittedCommit>, RepositoryError> {
        let distinct: HashSet<&NativeObjectId> = oids.iter().collect();
        // With no revision `git log` walks HEAD, so an empty read is answered here.
        if distinct.is_empty() {
            return Ok(HashMap::new());
        }
        let object_id_length = self.native_object_id_length()?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["-c", "log.showSignature=false", "log", "--no-walk=unsorted"])
            .arg(COMMIT_FORMAT);
        for oid in &distinct {
            command.arg(oid.as_str());
        }
        let listing = run_bounded_listing(command)?;
        let mut commits = HashMap::with_capacity(distinct.len());
        for commit in parse_committed_commits(&listing, object_id_length)? {
            commits.insert(commit.commit.clone(), commit);
        }
        if commits.len() != distinct.len() || !distinct.iter().all(|oid| commits.contains_key(*oid))
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(commits)
    }

    /// The commits the repository's `shallow` file lists: the boundary of a
    /// shallow clone. Git prints no parents for them, though the commit object
    /// names them, so a caller reports their parents as unavailable. A
    /// repository with no `shallow` file has none.
    pub fn shallow_boundaries(&self) -> Result<HashSet<NativeObjectId>, RepositoryError> {
        // `--git-path` names the file Git itself reads, wherever it lives; it is
        // relative to the worktree the command runs in.
        let location = self.git(["rev-parse", "--git-path", "shallow"])?;
        let text = match std::fs::read_to_string(self.worktree.join(location)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HashSet::new());
            }
            Err(_) => return Err(RepositoryError::GitOperationFailed),
        };
        text.lines().map(NativeObjectId::new).collect()
    }

    /// One `git log` run over the walk from `from`, `skip` commits in.
    fn committed_commit_page_listing(
        &self,
        from: &NativeObjectId,
        skip: u64,
        count: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            // A configured `log.showSignature` would print a verification
            // report before each signed commit of the walk.
            .args(["-c", "log.showSignature=false", "log"])
            .arg(COMMIT_FORMAT)
            .arg(format!("--skip={skip}"))
            .arg(format!("--max-count={count}"))
            .arg(from.as_str());
        run_bounded_listing(command)
    }
}

/// Parses a whole listing: records of [`COMMIT_FIELDS`] NUL-terminated fields,
/// each record followed by the newline `git log` ends its entries with.
/// Anything else is refused.
fn parse_committed_commits(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedCommit>, RepositoryError> {
    let mut rest = listing;
    let mut commits = Vec::new();
    while !rest.is_empty() {
        let mut fields: [&[u8]; COMMIT_FIELDS] = [&[]; COMMIT_FIELDS];
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
        commits.push(parse_committed_commit(fields, object_id_length)?);
    }
    Ok(commits)
}

/// Reads one record, its fields in the order of [`COMMIT_FORMAT`].
fn parse_committed_commit(
    fields: [&[u8]; COMMIT_FIELDS],
    object_id_length: usize,
) -> Result<CommittedCommit, RepositoryError> {
    let [
        commit,
        parents,
        tree,
        author_name,
        author_email,
        authored,
        committer_name,
        committer_email,
        committed,
        message,
    ] = fields;
    let parents = listing_text(parents)?;
    let parents: Vec<NativeObjectId> = if parents.is_empty() {
        Vec::new()
    } else {
        parents
            .split(' ')
            .map(|parent| object_id(parent, object_id_length))
            .collect::<Result<Vec<NativeObjectId>, _>>()?
    };
    Ok(CommittedCommit {
        commit: object_id(listing_text(commit)?, object_id_length)?,
        tree: object_id(listing_text(tree)?, object_id_length)?,
        parents,
        author: GitPerson::new(
            listing_text(author_name)?.to_owned(),
            address(author_email)?,
        ),
        authored: parse_unix_seconds(listing_text(authored)?)?,
        committer: GitPerson::new(
            listing_text(committer_name)?.to_owned(),
            address(committer_email)?,
        ),
        committed: parse_unix_seconds(listing_text(committed)?)?,
        message: listing_text(message)?.to_owned(),
    })
}

/// An object id field, checked to be a complete id of this repository's width.
fn object_id(text: &str, object_id_length: usize) -> Result<NativeObjectId, RepositoryError> {
    require_native_object_id(text, object_id_length)?;
    NativeObjectId::new(text)
}

/// The address of a person field: `None` when the identity records none.
fn address(field: &[u8]) -> Result<Option<String>, RepositoryError> {
    let text = listing_text(field)?;
    Ok((!text.is_empty()).then(|| text.to_owned()))
}
