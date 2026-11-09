//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// A verified native Git commit ID. It is intentionally Git-local: the
/// shared foundation owns the portable Orna `SnapshotRef` row identity and
/// canonical committed snapshot representation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitCommitRef(pub(crate) String);

impl GitCommitRef {
    pub(crate) fn from_verified_commit(
        value: String,
        object_id_length: usize,
    ) -> Result<Self, RepositoryError> {
        if value.len() != object_id_length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RepositoryError::InvalidObjectId);
        }
        Ok(Self(value))
    }

    /// The object ID in Git's native hexadecimal spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GitCommitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One record discovered from the reachable Git commit history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedLogEntry {
    pub(crate) commit: GitCommitRef,
    pub(crate) committed_at: String,
    pub(crate) author: String,
    pub(crate) subject: String,
}

impl CommittedLogEntry {
    /// The immutable Git commit represented by this history row.
    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }

    /// The commit timestamp in Git's ISO 8601 spelling.
    pub fn committed_at(&self) -> &str {
        &self.committed_at
    }

    /// The author recorded in the Git commit.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The first line of the commit message.
    pub fn subject(&self) -> &str {
        &self.subject
    }
}

/// One revision of a legacy (format-1/2) row path, as read by
/// [`Repository::committed_revision_snapshots`].
///
/// A format-1/2 row unit is a committed file, so a revision of that row is a
/// commit whose tree does or does not carry the file. The record therefore
/// carries the same coordinates the native row walk reports (ORNA-UPGRADE-002,
/// ORNA-ROW-013): the commit, its root tree, the author, the commit time and
/// the migration coordinate a journalled migration commit records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRevisionSnapshot {
    pub(crate) commit: GitCommitRef,
    pub(crate) tree: String,
    pub(crate) author: String,
    pub(crate) committed_at: u64,
    pub(crate) migration: Option<String>,
}

impl CommittedRevisionSnapshot {
    /// The immutable Git commit this revision is.
    pub const fn commit(&self) -> &GitCommitRef {
        &self.commit
    }

    /// The commit's root tree object id.
    pub fn tree(&self) -> &str {
        &self.tree
    }

    /// Commit author as `Name <email>`.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// When the commit was created, in seconds since the Unix epoch.
    pub const fn committed_at(&self) -> u64 {
        self.committed_at
    }

    /// The migration coordinate this commit records, when it is a journalled
    /// format-1/2 to format-3 migration commit rather than an ordinary one.
    pub fn migration(&self) -> Option<&str> {
        self.migration.as_deref()
    }
}

/// Splits one `git log` record's author-and-trailer field into the author and
/// the migration coordinate, for [`Repository::committed_revision_snapshots`].
///
/// An empty trailer value is reported as no migration rather than as an empty
/// coordinate, the same distinction the native revision walk keeps.
pub(crate) fn split_committed_revision_record(record: &str) -> (&str, Option<&str>) {
    match record.split_once('\t') {
        Some((author, marker)) if !marker.is_empty() => (author, Some(marker)),
        Some((author, _)) => (author, None),
        None => (record, None),
    }
}

/// Most commits one [`Repository::committed_revision_snapshots`] walk reads.
/// It matches the native graph's revision bound, so both history paths refuse
/// an over-large `--limit` at the same coordinate.
pub(crate) const MAX_COMMITTED_REVISION_WALK: usize = 4096;

/// One local Git branch and the commit currently named by its ref.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedBranch {
    pub(crate) name: String,
    pub(crate) commit: GitCommitRef,
}
impl CommittedBranch {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}

/// One recursively listed entry in an immutable committed Git tree.
///
/// This deliberately retains only the repository-relative path and Git entry
/// kind. Callers that need bytes must use [`Repository::read_committed_file`]
/// so each body has an explicit size bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTreeEntry {
    pub(crate) path: ManagedPath,
    pub(crate) kind: CommittedTreeEntryKind,
}

impl CommittedTreeEntry {
    /// The normalized repository-relative path of this entry.
    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    /// The entry's Git object kind and supported file mode.
    pub const fn kind(&self) -> CommittedTreeEntryKind {
        self.kind
    }
}

/// The Git tree entry kinds a committed snapshot loader must classify.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommittedTreeEntryKind {
    /// An ordinary regular file. `executable` records Git's executable bit.
    File { executable: bool },
    /// A symbolic-link blob, which snapshot source loaders must not follow.
    Symlink,
    /// A gitlink/submodule entry, which is not source content in this tree.
    Submodule,
}

/// A verified Git tree ID produced from the ordinary Git index.
///
/// This is intentionally distinct from [`GitCommitRef`]: a tree must never be
/// accepted where a committed historical database snapshot is required.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IndexTreeRef(pub(crate) String);

impl IndexTreeRef {
    pub(crate) fn from_verified_tree(
        value: String,
        object_id_length: usize,
    ) -> Result<Self, RepositoryError> {
        if value.len() != object_id_length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RepositoryError::InvalidObjectId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One managed file entry for a private publication candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedFileChange {
    pub(crate) path: ManagedPath,
    bytes: Option<Vec<u8>>,
}

impl ManagedFileChange {
    pub fn new(path: ManagedPath, bytes: Option<Vec<u8>>) -> Self {
        Self { path, bytes }
    }

    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
}

/// The Git objects for a candidate publication.  The candidate is private:
/// building it never changes the ordinary index, worktree, or ref namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateCommit {
    pub(crate) tree: IndexTreeRef,
    pub(crate) commit: GitCommitRef,
}

/// Refuses a private candidate commit before it becomes a commit. The
/// repository crate cannot evaluate table or module assertions itself, so each
/// reviewed commit path (`commit_reviewed_candidate` and its stage-hook variant)
/// asks this gate over the serialized candidate before accepting it
/// (ORNA-ASSERT-042). The publish entry points (`publish_candidate`,
/// `publish_candidate_with_stage_hook`, `publish_legacy_format_migration`) take
/// no gate: the CLI runs its gate before calling them, so an assertion is
/// evaluated before graph publication (ORNA-ASSERT-045). The production
/// implementation is `AssertionGate` in `orna-cli`'s `assertion_gate`
/// module; the test-only `AcceptAllAssertions` accepts without evaluating them.
pub trait CandidateAssertionGate {
    fn validate(
        &self,
        repository: &Repository,
        candidate: &PrivateCommit,
    ) -> Result<(), CandidateAssertionRefusal>;
}

/// Why the assertion gate refused a candidate commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateAssertionRefusal {
    /// A table or module assertion evaluated false over the serialized candidate.
    /// Carries the module assertion's site; a table assertion carries none.
    AssertionFalse(Option<String>),
    /// Validation could not complete, so the candidate is refused whole. The
    /// string is the cause, which the E2300 title carries (ORNA-ASSERT-049).
    Unvalidated(String),
}

/// Accepts every candidate. Used only by the repository's commit-path tests
/// (`lib.rs` and `relation_admission.rs`), which stage no assertion.
#[cfg(test)]
pub(crate) struct AcceptAllAssertions;

#[cfg(test)]
impl CandidateAssertionGate for AcceptAllAssertions {
    fn validate(&self, _: &Repository, _: &PrivateCommit) -> Result<(), CandidateAssertionRefusal> {
        Ok(())
    }
}

/// Accepts every checkout target. Used only by the repository's checkout-path
/// unit tests, which stage no assertion.
#[cfg(test)]
pub(crate) struct AcceptAllCheckoutAssertions;

#[cfg(test)]
impl CheckoutAssertionGate for AcceptAllCheckoutAssertions {
    fn validate(
        &self,
        _: &Repository,
        _: &CheckoutPreflight,
    ) -> Result<(), CandidateAssertionRefusal> {
        Ok(())
    }
}

/// Refuses a checkout target whose logical state would become current while it
/// violates an admitted assertion (ORNA-ASSERT-044). The repository crate cannot
/// evaluate table or module assertions itself, so both checkout executors ask
/// this gate over the immutable preflight while the coordination lock is held.
/// Production gates accept without evaluating a target that changes no logical
/// state; a refusal names the same codes as the commit path.
pub trait CheckoutAssertionGate {
    fn validate(
        &self,
        repository: &Repository,
        plan: &CheckoutPreflight,
    ) -> Result<(), CandidateAssertionRefusal>;
}

/// Opaque, repository-local review of one exact staged index generation.
/// Construction is restricted to this crate so callers cannot forge a review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewedCandidateReview {
    pub(crate) database_id: Uuid,
    pub(crate) runtime_id: Uuid,
    pub(crate) pending_generation: u64,
    pub(crate) repository_format: RepositoryFormat,
    pub(crate) head_ref: Option<String>,
    pub(crate) cwd: CwdGeneration,
    pub(crate) cwd_snapshot_descriptor: Vec<u8>,
    pub(crate) candidate_paths: Vec<String>,
    pub(crate) index_bytes_digest: [u8; 32],
    pub(crate) index_tree: IndexTreeRef,
    pub(crate) token: [u8; 32],
}

impl ReviewedCandidateReview {
    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn index_tree(&self) -> &IndexTreeRef {
        &self.index_tree
    }

    pub const fn pending_generation(&self) -> u64 {
        self.pending_generation
    }
}
fn append_cbor_optional_text(bytes: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => cbor_text(bytes, value),
        None => bytes.push(0xf6),
    }
}

pub(crate) fn reviewed_candidate_token(review: &ReviewedCandidateReview) -> [u8; 32] {
    let mut payload = Vec::with_capacity(256 + review.cwd_snapshot_descriptor.len());
    cbor_head(&mut payload, 4, 11);
    cbor_bytes(&mut payload, review.database_id.as_bytes());
    cbor_bytes(&mut payload, review.runtime_id.as_bytes());
    append_cbor_optional_text(&mut payload, review.cwd.head().map(GitCommitRef::as_str));
    append_cbor_optional_text(&mut payload, review.head_ref.as_deref());
    cbor_text(&mut payload, review.index_tree.as_str());
    cbor_bytes(&mut payload, &review.index_bytes_digest);
    cbor_head(&mut payload, 4, review.candidate_paths.len() as u64);
    for path in &review.candidate_paths {
        cbor_text(&mut payload, path);
    }
    cbor_bytes(&mut payload, &review.cwd_snapshot_descriptor);
    cbor_head(&mut payload, 0, review.pending_generation);
    cbor_head(
        &mut payload,
        0,
        u64::from(review.repository_format.number()),
    );
    let mut digest = Sha256::new();
    digest.update(b"orna.commit-review-worktree.v1\0");
    digest.update(payload);
    digest.finalize().into()
}

pub(crate) fn reviewed_candidate_runtime_intent_id(
    review: &ReviewedCandidateReview,
    message: &str,
) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(b"orna.reviewed-candidate-runtime-intent.v1\0");
    digest.update(review.database_id.as_bytes());
    digest.update(review.runtime_id.as_bytes());
    let tree = review.index_tree.as_str();
    digest.update((tree.len() as u64).to_be_bytes());
    digest.update(tree.as_bytes());
    digest.update(review.pending_generation.to_be_bytes());
    digest.update(Sha256::digest(message.as_bytes()));
    let digest: [u8; 32] = digest.finalize().into();
    let mut intent = [0; 16];
    intent.copy_from_slice(&digest[..16]);
    intent
}

/// The commit gate's refusal as the commit's failure. A candidate that mixes
/// formats keeps its format error, which names the tracked path that does not
/// belong to format 3 (ORNA-META-001, ORNA-REPO-CONTEXT-004); any other
/// invalid candidate is `InvalidCommitCandidate` (`sys.admin.assertion_failed`).
pub(crate) fn commit_gate_refusal(error: FormatContextError) -> RepositoryError {
    match error {
        mixed @ FormatContextError::MixedFormats { .. } => RepositoryError::FormatContext(mixed),
        _ => RepositoryError::InvalidCommitCandidate,
    }
}

impl PrivateCommit {
    pub fn tree(&self) -> &IndexTreeRef {
        &self.tree
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}
