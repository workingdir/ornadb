//! Git-valid repository state for Orna 1.0.
//!
//! This crate intentionally models the repository boundary. It can initialize
//! canonical `.orna/` metadata, but never advances an ordinary ref. In particular,
//! [`Repository::stage_managed`] delegates to Git's ordinary index and touches
//! only the supplied worktree-relative paths.

use std::{
    collections::HashSet,
    fmt, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

use fs2::FileExt;
use sha2::{Digest, Sha256};

mod init;

pub use init::{
    DatabaseId, RepositoryInitError, RepositoryInitialization, RepositoryMetadata,
    initialize_repository, inspect_metadata,
};

/// A verified native Git commit ID. It is intentionally Git-local: the
/// shared foundation owns the portable Orna `SnapshotRef` row identity and
/// canonical committed snapshot representation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitCommitRef(String);

impl GitCommitRef {
    fn from_verified_commit(
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

/// A verified Git tree ID produced from the ordinary Git index.
///
/// This is intentionally distinct from [`GitCommitRef`]: a tree must never be
/// accepted where a committed historical database snapshot is required.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IndexTreeRef(String);

impl IndexTreeRef {
    fn from_verified_tree(value: String, object_id_length: usize) -> Result<Self, RepositoryError> {
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
    path: ManagedPath,
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
    tree: IndexTreeRef,
    commit: GitCommitRef,
}

const JOURNAL_MAGIC: &[u8] = b"ORNA-PUB-JOURNAL\0";
const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;

/// The recovery stages persisted for one publication attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationJournalStage {
    Prepared,
    RefAdvanced,
    IndexReconciled,
    WorktreeReconciled,
    RuntimeCompleted,
    Complete,
}

impl PublicationJournalStage {
    fn code(self) -> u8 {
        match self {
            Self::Prepared => 1,
            Self::RefAdvanced => 2,
            Self::IndexReconciled => 3,
            Self::WorktreeReconciled => 4,
            Self::RuntimeCompleted => 5,
            Self::Complete => 6,
        }
    }

    fn from_code(code: u8) -> Result<Self, RepositoryError> {
        match code {
            1 => Ok(Self::Prepared),
            2 => Ok(Self::RefAdvanced),
            3 => Ok(Self::IndexReconciled),
            4 => Ok(Self::WorktreeReconciled),
            5 => Ok(Self::RuntimeCompleted),
            6 => Ok(Self::Complete),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }
}

/// One managed path's captured and desired worktree bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournalEntry {
    path: ManagedPath,
    expected: Option<Vec<u8>>,
    next: Option<Vec<u8>>,
}

impl PublicationJournalEntry {
    pub fn new(path: ManagedPath, expected: Option<Vec<u8>>, next: Option<Vec<u8>>) -> Self {
        Self {
            path,
            expected,
            next,
        }
    }

    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    pub fn expected(&self) -> Option<&[u8]> {
        self.expected.as_deref()
    }

    pub fn next(&self) -> Option<&[u8]> {
        self.next.as_deref()
    }
}

/// A restart-safe publication record stored in the private runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournal {
    old_head: GitCommitRef,
    new_head: GitCommitRef,
    base_index_tree: Option<IndexTreeRef>,
    runtime_intent_id: Option<[u8; 16]>,
    entries: Vec<PublicationJournalEntry>,
    stage: PublicationJournalStage,
}

impl PublicationJournal {
    pub fn new(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, None, entries)
    }

    pub fn new_with_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)
    }

    pub fn new_with_runtime_intent(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        runtime_intent_id: [u8; 16],
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        let mut journal =
            Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)?;
        if runtime_intent_id == [0; 16] {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        journal.runtime_intent_id = Some(runtime_intent_id);
        Ok(journal)
    }

    fn new_with_base_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: Option<IndexTreeRef>,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        if entries.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let mut paths = HashSet::new();
        if entries.iter().any(|entry| {
            entry.path.as_path().to_str().is_none() || !paths.insert(entry.path.clone())
        }) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id: None,
            entries,
            stage: PublicationJournalStage::Prepared,
        })
    }

    pub fn old_head(&self) -> &GitCommitRef {
        &self.old_head
    }

    pub fn new_head(&self) -> &GitCommitRef {
        &self.new_head
    }

    pub fn base_index_tree(&self) -> Option<&IndexTreeRef> {
        self.base_index_tree.as_ref()
    }

    pub fn entries(&self) -> &[PublicationJournalEntry] {
        &self.entries
    }

    pub const fn runtime_intent_id(&self) -> Option<[u8; 16]> {
        self.runtime_intent_id
    }

    pub const fn stage(&self) -> PublicationJournalStage {
        self.stage
    }

    pub fn advance(&mut self, next: PublicationJournalStage) -> Result<(), RepositoryError> {
        if next.code() != self.stage.code() + 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.stage = next;
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, RepositoryError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(JOURNAL_MAGIC);
        bytes.push(2);
        put_string(&mut bytes, self.old_head.as_str())?;
        put_string(&mut bytes, self.new_head.as_str())?;
        put_optional_string(
            &mut bytes,
            self.base_index_tree.as_ref().map(IndexTreeRef::as_str),
        )?;
        match self.runtime_intent_id {
            Some(intent_id) => {
                bytes.push(1);
                bytes.extend_from_slice(&intent_id);
            }
            None => bytes.push(0),
        }
        bytes.push(self.stage.code());
        put_u32(&mut bytes, self.entries.len())?;
        for entry in &self.entries {
            let path = entry
                .path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::InvalidPublicationJournal)?;
            put_string(&mut bytes, path)?;
            put_optional_bytes(&mut bytes, entry.expected.as_deref())?;
            put_optional_bytes(&mut bytes, entry.next.as_deref())?;
        }
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8], object_id_length: usize) -> Result<Self, RepositoryError> {
        if bytes.len() > MAX_JOURNAL_BYTES || !bytes.starts_with(JOURNAL_MAGIC) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut cursor = JOURNAL_MAGIC.len();
        if take_byte(bytes, &mut cursor)? != 2 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let old_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let new_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let base_index_tree = take_optional_string(bytes, &mut cursor)?
            .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
            .transpose()?;
        let runtime_intent_id = match take_byte(bytes, &mut cursor)? {
            0 => None,
            1 => Some(take_fixed_array::<16>(bytes, &mut cursor)?),
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let stage = PublicationJournalStage::from_code(take_byte(bytes, &mut cursor)?)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        if count == 0 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut entries = Vec::with_capacity(count.min(1024));
        let mut paths = HashSet::new();
        for _ in 0..count {
            let path = ManagedPath::new(take_string(bytes, &mut cursor)?)?;
            let expected = take_optional_bytes(bytes, &mut cursor)?;
            let next = take_optional_bytes(bytes, &mut cursor)?;
            if !paths.insert(path.clone()) {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            entries.push(PublicationJournalEntry::new(path, expected, next));
        }
        if cursor != bytes.len() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id,
            entries,
            stage,
        })
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

/// A generation supplied by the embedded runtime state layer.
///
/// The repository layer does not open `state.db`: doing so would conflate the
/// Git/index boundary with the single-writer runtime ownership boundary.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Git's current index generation.  Both fields are retained so a `HEAD`
/// change is observable even when the index tree happens to be identical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexGeneration {
    head: Option<GitCommitRef>,
    tree: Option<IndexTreeRef>,
}

impl IndexGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn tree(&self) -> Option<&IndexTreeRef> {
        self.tree.as_ref()
    }
}

/// A lossless, deterministic representation of Git's ordinary worktree
/// status.  It deliberately is not a content hash; callers can compare it but
/// must not treat it as a cryptographic integrity assertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeState(Vec<u8>);

impl WorktreeState {
    pub fn as_porcelain_v2_z(&self) -> &[u8] {
        &self.0
    }
    pub fn is_clean(&self) -> bool {
        self.0.is_empty()
    }
}

/// The four independently observable parts of an Orna CWD generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwdGeneration {
    head: Option<GitCommitRef>,
    branch: Option<String>,
    index: IndexGeneration,
    worktree: WorktreeState,
    runtime: RuntimeGeneration,
}

impl CwdGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    pub fn index(&self) -> &IndexGeneration {
        &self.index
    }
    pub fn worktree(&self) -> &WorktreeState {
        &self.worktree
    }
    pub const fn runtime(&self) -> RuntimeGeneration {
        self.runtime
    }
}

/// The immutable target selected by a checkout preview. A local branch keeps
/// its attachment; every other accepted selector is a detached commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckoutTarget {
    Branch { name: String, commit: GitCommitRef },
    Detached { commit: GitCommitRef },
}

impl CheckoutTarget {
    pub fn commit(&self) -> &GitCommitRef {
        match self {
            Self::Branch { commit, .. } | Self::Detached { commit } => commit,
        }
    }

    pub fn branch_name(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } => Some(name),
            Self::Detached { .. } => None,
        }
    }
}

/// Checkout preconditions. The bounded same-commit operation can consume this
/// plan, but forceful/divergent checkout, logical validation, and durable
/// recovery remain separate boundaries owned by higher repository layers.
/// Capturing it may initialize private coordination metadata, but never
/// changes visible Git state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutPreflight {
    target: CheckoutTarget,
    expected_head: Option<GitCommitRef>,
    cwd: CwdGeneration,
    git: CheckoutGitSubplan,
}

/// The read-only Git portion of a checkout plan. Paths are repository-relative
/// and classified from the current worktree and target commit without
/// changing the ref, index, or worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutGitSubplan {
    affected_paths: Vec<ManagedPath>,
    conflicting_paths: Vec<ManagedPath>,
    discardable_paths: Vec<ManagedPath>,
}

impl CheckoutGitSubplan {
    pub fn affected_paths(&self) -> &[ManagedPath] {
        &self.affected_paths
    }

    pub fn conflicting_paths(&self) -> &[ManagedPath] {
        &self.conflicting_paths
    }

    pub fn discardable_paths(&self) -> &[ManagedPath] {
        &self.discardable_paths
    }
}

/// A domain-separated authorization witness for one exact checkout
/// preflight. It is opaque so callers cannot assemble a force token without
/// first observing the repository state it authorizes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CheckoutPlanToken([u8; 32]);

impl CheckoutPlanToken {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An opaque admission capability for one exact force-discard checkout.
///
/// The capability can only be produced after the locked preflight, force
/// witness, logical validation, and canonical discard set all agree. Its
/// private contents prevent callers from replacing any of those bindings
/// before a future journaled executor consumes the capability.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCheckoutDiscard {
    preflight: CheckoutPreflight,
    force_token: CheckoutPlanToken,
    discard_paths: Vec<ManagedPath>,
}

/// The failure boundary for a checkout that validates its logical candidate
/// while the repository mutation lock is held.
#[derive(Debug)]
pub enum CheckoutExecutionError<E> {
    Repository(RepositoryError),
    Validation(E),
}

impl CheckoutPreflight {
    pub fn target(&self) -> &CheckoutTarget {
        &self.target
    }

    pub fn expected_head(&self) -> Option<&GitCommitRef> {
        self.expected_head.as_ref()
    }

    pub fn cwd(&self) -> &CwdGeneration {
        &self.cwd
    }

    pub fn git(&self) -> &CheckoutGitSubplan {
        &self.git
    }

    /// Derives a stable authorization witness over the complete preflight,
    /// including target attachment kind, Git state, raw worktree status, and
    /// runtime generation.
    pub fn force_token(&self) -> CheckoutPlanToken {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ORNA-CHECKOUT-PLAN-TOKEN\0");
        bytes.push(1);
        match &self.target {
            CheckoutTarget::Branch { name, commit } => {
                bytes.push(1);
                token_string(&mut bytes, name);
                token_string(&mut bytes, commit.as_str());
            }
            CheckoutTarget::Detached { commit } => {
                bytes.push(2);
                token_string(&mut bytes, commit.as_str());
            }
        }
        token_optional_string(
            &mut bytes,
            self.expected_head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(&mut bytes, self.cwd.branch.as_deref());
        token_optional_string(
            &mut bytes,
            self.cwd.index.head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(
            &mut bytes,
            self.cwd.index.tree.as_ref().map(IndexTreeRef::as_str),
        );
        token_bytes(&mut bytes, self.cwd.worktree.as_porcelain_v2_z());
        bytes.extend_from_slice(&self.cwd.runtime.get().to_be_bytes());
        token_paths(&mut bytes, &self.git.affected_paths);
        token_paths(&mut bytes, &self.git.conflicting_paths);
        token_paths(&mut bytes, &self.git.discardable_paths);
        CheckoutPlanToken(Sha256::digest(bytes).into())
    }

    /// Requires explicit force authorization for this exact, still-current
    /// preflight. A missing or stale witness never permits checkout mutation.
    pub fn authorize_force(
        &self,
        force: bool,
        token: Option<&CheckoutPlanToken>,
    ) -> Result<(), RepositoryError> {
        if force && token == Some(&self.force_token()) {
            Ok(())
        } else {
            Err(RepositoryError::CheckoutPlanStale)
        }
    }
}

fn token_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn token_string(output: &mut Vec<u8>, value: &str) {
    token_bytes(output, value.as_bytes());
}

fn token_optional_string(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            token_string(output, value);
        }
        None => output.push(0),
    }
}

fn token_paths(output: &mut Vec<u8>, paths: &[ManagedPath]) {
    output.extend_from_slice(&(paths.len() as u64).to_be_bytes());
    for path in paths {
        token_string(
            output,
            path.as_path()
                .to_str()
                .expect("validated checkout path is UTF-8"),
        );
    }
}

fn sorted_paths(paths: HashSet<ManagedPath>) -> Vec<ManagedPath> {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| left.as_path().cmp(right.as_path()));
    paths
}

/// Git-resolved local paths for one worktree's private Orna runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    root: PathBuf,
}

impl RuntimePaths {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn state_db(&self) -> PathBuf {
        self.root.join("state.db")
    }
    pub fn cache_db(&self) -> PathBuf {
        self.root.join("cache.db")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("runtime.sock")
    }
    pub fn locks(&self) -> PathBuf {
        self.root.join("locks")
    }

    /// Creates the private local directory, never a tracked `.orna/` path.
    pub fn ensure_exists(&self) -> Result<(), RepositoryError> {
        fs::create_dir_all(&self.root).map_err(|_| RepositoryError::LocalStateUnavailable)
    }
}

/// A path whose index mutation is deliberately limited to that one path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ManagedPath(PathBuf);

impl ManagedPath {
    /// Creates a normalized, repository-relative path.
    ///
    /// Absolute paths, `.git` administration paths, parent traversal, and an
    /// empty path are rejected before they can be passed to Git.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => normalized.push(value),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
            }
        }
        if normalized.components().next().is_none()
            || normalized.components().next() == Some(Component::Normal(".git".as_ref()))
        {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        Ok(Self(normalized))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A normal Git repository and its selected worktree.
#[derive(Clone, Debug)]
pub struct Repository {
    worktree: PathBuf,
    runtime: RuntimePaths,
}

impl Repository {
    /// Discovers the Git worktree containing `path` and resolves the *per
    /// worktree* local runtime directory using `git rev-parse --git-path`.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let input = path.as_ref();
        let directory = if input.is_dir() {
            input
        } else {
            input.parent().ok_or(RepositoryError::NotAWorktree)?
        };
        let worktree = PathBuf::from(Self::git_at(
            directory,
            ["rev-parse", "--show-toplevel"],
            true,
        )?);
        let runtime = PathBuf::from(Self::git_at(
            &worktree,
            ["rev-parse", "--git-path", "orna"],
            true,
        )?);
        // Git intentionally prints a path relative to its current directory
        // for an ordinary worktree, but a linked worktree may resolve through
        // an absolute administrative path. Store one absolute filesystem path
        // in either case so later local I/O never depends on the caller's CWD.
        let runtime = if runtime.is_absolute() {
            runtime
        } else {
            worktree.join(runtime)
        };
        Ok(Self {
            worktree,
            runtime: RuntimePaths { root: runtime },
        })
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }
    pub fn runtime_paths(&self) -> &RuntimePaths {
        &self.runtime
    }

    /// The actual selected committed Git snapshot. `None` represents an
    /// unborn `HEAD`; it never substitutes CWD/runtime state.
    pub fn head(&self) -> Result<Option<GitCommitRef>, RepositoryError> {
        self.commit_optional("HEAD^{commit}")
    }

    /// Builds a real Git tree and commit from `expected_head` plus the supplied
    /// managed-file changes, using a private temporary index. The ordinary
    /// index and worktree are never inputs to this operation and the ordinary
    /// refs are not advanced; unreachable candidate objects are permitted when
    /// a later publication step fails.
    pub fn build_private_commit(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        if changes.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        let mut paths = HashSet::new();
        for change in changes {
            if !paths.insert(change.path.clone()) || change.path.as_path().to_str().is_none() {
                return Err(RepositoryError::UnsafeManagedPath);
            }
        }

        self.runtime.ensure_exists()?;
        fs::create_dir_all(self.runtime.locks())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let index = self.runtime.locks().join(format!(
            "private-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::remove_file(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;

        let result = (|| {
            let mut read_tree = self.command();
            read_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", expected_head.as_str()]);
            self.run(read_tree)?;

            for change in changes {
                if let Some(bytes) = change.bytes() {
                    let object = self.hash_object(bytes)?;
                    let cacheinfo = format!(
                        "100644,{object},{}",
                        change
                            .path
                            .as_path()
                            .to_str()
                            .ok_or(RepositoryError::UnsafeManagedPath)?
                    );
                    let mut update = self.command();
                    update.env("GIT_INDEX_FILE", &index).args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &cacheinfo,
                    ]);
                    self.run(update)?;
                } else {
                    let mut update = self.command();
                    update
                        .env("GIT_INDEX_FILE", &index)
                        .args(["update-index", "--force-remove", "--"])
                        .arg(change.path.as_path());
                    self.run(update)?;
                }
            }

            let mut write_tree = self.command();
            write_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["write-tree"]);
            let tree =
                self.index_tree_from_native_oid(trim_output(&self.run(write_tree)?.stdout))?;

            let mut commit_tree = self.command();
            commit_tree
                .args([
                    "commit-tree",
                    tree.as_str(),
                    "-p",
                    expected_head.as_str(),
                    "-m",
                ])
                .arg(message);
            let commit =
                self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?;
            if self.head()?.as_ref() != Some(expected_head) {
                return Err(RepositoryError::StaleHead);
            }
            Ok(PrivateCommit { tree, commit })
        })();
        let _ = fs::remove_file(&index);
        result
    }

    /// Advances the symbolic current branch to a previously built private
    /// candidate using Git's compare-and-set ref transaction. It does not
    /// reconcile the ordinary index or worktree; callers must keep the
    /// publication journal until that separate recovery boundary is complete.
    pub fn advance_current_ref(
        &self,
        expected_head: &GitCommitRef,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        let reference = self
            .git(["symbolic-ref", "--quiet", "HEAD"])
            .map_err(|_| RepositoryError::DetachedHead)?;
        self.advance_ref_transaction(&reference, expected_head, candidate.commit())?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(())
    }

    /// Installs the ordinary index image for a published candidate. Only the
    /// supplied managed paths are replaced from the candidate tree; unrelated
    /// staged entries remain in the captured index. The worktree is not
    /// modified, so managed file replacement is a later recovery boundary.
    pub fn reconcile_published_index(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_atomic_index_install_supported()?;
        self.ensure_no_git_index_lock()?;
        let index = self.git_path("index")?;
        let base_index = self.capture_index_for_expected(expected_index, &index)?;
        let git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
        self.reconcile_published_index_locked(
            expected_index,
            candidate,
            paths,
            &base_index,
            git_lock,
        )
    }

    fn reconcile_published_index_locked(
        &self,
        _expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        paths: &[ManagedPath],
        base_index: &[u8],
        git_lock: GitIndexLock,
    ) -> Result<IndexGeneration, RepositoryError> {
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        let index = self.git_path("index")?;
        let mut unique = HashSet::new();
        if paths.iter().any(|path| !unique.insert(path.clone())) {
            return Err(RepositoryError::UnsafeManagedPath);
        }

        let candidate_index = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .join(format!(
                "index-candidate-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .as_nanos()
            ));
        fs::write(&candidate_index, base_index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let result = (|| {
            for path in paths {
                let mut update = self.command();
                update.env("GIT_INDEX_FILE", &candidate_index);
                if let Some((mode, object)) = self.candidate_tree_entry(candidate, path)? {
                    let cacheinfo = format!(
                        "{mode},{object},{}",
                        path.as_path()
                            .to_str()
                            .ok_or(RepositoryError::UnsafeManagedPath)?
                    );
                    update.args(["update-index", "--add", "--cacheinfo", &cacheinfo]);
                } else {
                    update
                        .args(["update-index", "--force-remove", "--"])
                        .arg(path.as_path());
                }
                self.run(update)?;
            }
            fs::File::open(&candidate_index)
                .and_then(|file| file.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let current_index =
                fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if current_index != base_index || self.head()?.as_ref() != Some(candidate.commit()) {
                return Err(RepositoryError::StaleHead);
            }
            fs::rename(&candidate_index, &index)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(
                index
                    .parent()
                    .ok_or(RepositoryError::LocalStateUnavailable)?,
            )
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            drop(git_lock);
            self.index_generation()
        })();
        let _ = fs::remove_file(&candidate_index);
        result
    }

    /// Atomically persists the current publication journal in private runtime
    /// state. The ordinary Git namespace is not touched.
    pub fn write_publication_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let encoded = journal.encode()?;
        let _lock = self.acquire_coordination_lock()?;
        self.runtime.ensure_exists()?;
        let path = self.runtime.root().join("publication-journal.bin");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let temporary = self.runtime.root().join(format!(
            ".publication-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.write_all(&encoded)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.sync_all()
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::rename(&temporary, &path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(self.runtime.root())
                .and_then(|directory| directory.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Reads the last atomically persisted publication journal, if present.
    pub fn read_publication_journal(&self) -> Result<Option<PublicationJournal>, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let path = self.runtime.root().join("publication-journal.bin");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(Some(PublicationJournal::decode(
            &bytes,
            self.native_object_id_length()?,
        )?))
    }

    /// Removes a completed or abandoned journal idempotently and flushes the
    /// containing private runtime directory.
    pub fn clear_publication_journal(&self) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let path = self.runtime.root().join("publication-journal.bin");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(self.runtime.root())
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        Ok(())
    }

    /// Executes the non-crashing PUB-1 publication path over one prepared
    /// journal. Every durable stage is written before the next boundary; a
    /// returned error leaves the journal available for recovery.
    pub fn publish_candidate(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
    ) -> Result<IndexGeneration, RepositoryError> {
        if journal.stage() != PublicationJournalStage::Prepared
            || journal.runtime_intent_id().is_none()
            || expected_index.head() != Some(journal.old_head())
            || journal.new_head() != candidate.commit()
            || journal.base_index_tree() != expected_index.tree()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let paths = journal
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        self.ensure_atomic_index_install_supported()?;
        let index = self.git_path("index")?;
        let base_index = self.capture_index_for_expected(expected_index, &index)?;
        // Own Git's writer lock before exposing the new ref. This closes the
        // post-ref/pre-index race where an ordinary Git writer could publish
        // the captured, stale index.
        let git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
        self.write_publication_journal(journal)?;
        self.advance_current_ref(journal.old_head(), candidate)?;

        journal.advance(PublicationJournalStage::RefAdvanced)?;
        self.write_publication_journal(journal)?;
        let reconciled = self.reconcile_published_index_locked(
            expected_index,
            candidate,
            &paths,
            &base_index,
            git_lock,
        )?;

        journal.advance(PublicationJournalStage::IndexReconciled)?;
        self.write_publication_journal(journal)?;
        for entry in journal.entries() {
            let current = self.managed_file_bytes(&entry.path)?;
            if current.as_deref() == entry.next() {
                continue;
            }
            if current.as_deref() != entry.expected() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            self.materialize_managed_file(&entry.path, entry.expected(), entry.next())?;
        }

        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal(journal)?;
        Ok(reconciled)
    }

    /// Records that the separately owned runtime transaction consumed the
    /// exact frozen intent named by the journal. The caller must invoke this
    /// only after runtime completion has committed successfully.
    pub fn mark_runtime_complete(
        &self,
        runtime_intent_id: [u8; 16],
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        if journal.stage() != PublicationJournalStage::WorktreeReconciled
            || journal.runtime_intent_id() != Some(runtime_intent_id)
        {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        // A normal Git writer may have moved HEAD after reconciliation but
        // before the separately durable runtime completion.  Do not discard
        // the journal in that state: PUB-1 recovery must retain it to
        // reconcile the newer ref rather than treating cleanup as complete.
        if self.head()?.as_ref() != Some(journal.new_head()) {
            return Err(RepositoryError::StaleHead);
        }
        journal.advance(PublicationJournalStage::RuntimeCompleted)?;
        self.write_publication_journal(journal)?;
        journal.advance(PublicationJournalStage::Complete)?;
        self.write_publication_journal(journal)?;
        self.clear_publication_journal()
    }

    /// Resumes a persisted publication after a process interruption. The
    /// journal is advanced only after each observable boundary is verified;
    /// unexpected ref, index, or worktree state remains a typed conflict.
    pub fn recover_publication(&self) -> Result<Option<IndexGeneration>, RepositoryError> {
        let Some(mut journal) = self.read_publication_journal()? else {
            return Ok(None);
        };
        let candidate = self.candidate_from_journal(&journal)?;
        let base_tree = journal
            .base_index_tree()
            .cloned()
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let expected_index = IndexGeneration {
            head: Some(journal.old_head().clone()),
            tree: Some(base_tree),
        };

        if journal.stage() == PublicationJournalStage::Prepared {
            match self.head()? {
                Some(head) if &head == journal.old_head() => {
                    let actual = self.index_generation()?;
                    if actual != expected_index {
                        return Err(RepositoryError::StaleIndex {
                            expected: expected_index,
                            actual,
                        });
                    }
                    return Err(RepositoryError::PublicationPending);
                }
                Some(head) if &head == journal.new_head() => {
                    journal.advance(PublicationJournalStage::RefAdvanced)?;
                    self.write_publication_journal(&journal)?;
                }
                _ => return Err(RepositoryError::StaleHead),
            }
        }

        if journal.stage() == PublicationJournalStage::RefAdvanced {
            if self.head()?.as_ref() != Some(journal.new_head()) {
                return Err(RepositoryError::StaleHead);
            }
            let current = self.index_generation()?;
            let _reconciled = if current.tree() == expected_index.tree() {
                let paths = journal
                    .entries()
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect::<Vec<_>>();
                self.reconcile_published_index(&expected_index, &candidate, &paths)?
            } else if self.index_entries_match_candidate(&candidate, journal.entries())? {
                current
            } else {
                return Err(RepositoryError::StaleIndex {
                    expected: expected_index,
                    actual: current,
                });
            };
            journal.advance(PublicationJournalStage::IndexReconciled)?;
            self.write_publication_journal(&journal)?;
            self.recover_publication_worktree(&mut journal)?;
            return Err(RepositoryError::RuntimeCompletionRequired);
        }

        if journal.stage() == PublicationJournalStage::IndexReconciled {
            if self.head()?.as_ref() != Some(journal.new_head()) {
                return Err(RepositoryError::StaleHead);
            }
            self.recover_publication_worktree(&mut journal)?;
        }
        if journal.stage() == PublicationJournalStage::WorktreeReconciled {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        if journal.stage() == PublicationJournalStage::RuntimeCompleted {
            journal.advance(PublicationJournalStage::Complete)?;
            self.write_publication_journal(&journal)?;
        }
        if journal.stage() == PublicationJournalStage::Complete {
            self.clear_publication_journal()?;
            return Ok(Some(self.index_generation()?));
        }
        Err(RepositoryError::InvalidPublicationJournal)
    }

    fn candidate_from_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<PrivateCommit, RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let commit = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        if &commit != journal.new_head() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        // Recovery must reconstruct the same private H + P candidate that
        // publication prepared. An existing commit is not enough: accepting
        // a journal whose new head is not the sole child of the recorded old
        // head could reconcile the ordinary index and worktree against an
        // unrelated history after a crash.
        let parents = self.git(["show", "-s", "--format=%P", journal.new_head().as_str()])?;
        let parents = parents.split_ascii_whitespace().collect::<Vec<_>>();
        if parents.len() != 1 || parents[0] != journal.old_head().as_str() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let tree_expression = format!("{}^{{tree}}", journal.new_head().as_str());
        let tree = self.index_tree_from_native_oid(self.git([
            "rev-parse",
            "--verify",
            &tree_expression,
        ])?)?;
        let candidate = PrivateCommit { tree, commit };
        if !self.candidate_entries_match_journal(&candidate, journal.entries())? {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(candidate)
    }

    fn recover_publication_worktree(
        &self,
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        for entry in journal.entries() {
            let current = self.managed_file_bytes(&entry.path)?;
            if current.as_deref() == entry.next() {
                continue;
            }
            if current.as_deref() != entry.expected() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            self.materialize_managed_file(&entry.path, entry.expected(), entry.next())?;
        }
        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal(journal)?;
        Ok(())
    }

    /// Observes the ordinary Git index without modifying it.
    pub fn index_generation(&self) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_no_git_index_lock()?;
        self.index_generation_while_locked()
    }

    fn index_generation_while_locked(&self) -> Result<IndexGeneration, RepositoryError> {
        Ok(IndexGeneration {
            head: self.head()?,
            tree: self.git_optional_tree(["write-tree"])?,
        })
    }

    fn capture_index_for_expected(
        &self,
        expected: &IndexGeneration,
        index: &Path,
    ) -> Result<Vec<u8>, RepositoryError> {
        let base_index = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let actual = self.index_generation()?;
        let after_capture = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if base_index != after_capture || actual.tree() != expected.tree() {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(base_index)
    }

    /// Captures Git's ordinary worktree/index status in machine-readable form.
    pub fn worktree_state(&self) -> Result<WorktreeState, RepositoryError> {
        Ok(WorktreeState(self.git_bytes([
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ])?))
    }

    /// Observes CWD. The runtime generation must come from the runtime owner,
    /// keeping `state.db` access outside the repository abstraction.
    pub fn cwd_generation(
        &self,
        runtime: RuntimeGeneration,
    ) -> Result<CwdGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.cwd_generation_locked(runtime)
    }

    fn cwd_generation_locked(
        &self,
        runtime: RuntimeGeneration,
    ) -> Result<CwdGeneration, RepositoryError> {
        // Do not return a torn CWD if another Git process changes the index
        // while status is being read.  The lock serializes Orna writers; the
        // bounded recheck detects external Git writers.
        for _ in 0..3 {
            let before = self.index_generation()?;
            let branch = self.current_branch()?;
            let worktree = self.worktree_state()?;
            let after = self.index_generation()?;
            let branch_after = self.current_branch()?;
            let worktree_after = self.worktree_state()?;
            if before == after && branch == branch_after && worktree == worktree_after {
                return Ok(CwdGeneration {
                    head: before.head.clone(),
                    branch,
                    index: before,
                    worktree,
                    runtime,
                });
            }
        }
        Err(RepositoryError::StaleCwd)
    }

    /// Returns the local branch currently attached to `HEAD`, if any. A
    /// detached `HEAD` is distinct CWD state even when it names the same
    /// commit as a local branch.
    fn current_branch(&self) -> Result<Option<String>, RepositoryError> {
        let mut command = self.command();
        command.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            let branch = trim_output(&output.stdout);
            if valid_branch_name(&branch) {
                Ok(Some(branch))
            } else {
                Err(RepositoryError::GitOperationFailed)
            }
        } else if output.status.code() == Some(1) && output.stderr.is_empty() {
            Ok(None)
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    /// Rechecks an earlier CWD observation before an operation that depends on
    /// both staged and unstaged state.  A worktree-only edit is stale too.
    pub fn verify_cwd(&self, expected: &CwdGeneration) -> Result<(), RepositoryError> {
        let actual = self.cwd_generation(expected.runtime)?;
        if &actual == expected {
            Ok(())
        } else {
            Err(RepositoryError::StaleCwd)
        }
    }

    /// Resolves a checkout target and captures all Git CWD preconditions while
    /// holding the repository coordination lock. This operation never writes
    /// refs, the index, the worktree, or the private runtime area.
    pub fn plan_checkout(
        &self,
        selector: &str,
        runtime: RuntimeGeneration,
    ) -> Result<CheckoutPreflight, RepositoryError> {
        if selector.is_empty() || selector.starts_with('-') || selector.contains('\0') {
            return Err(RepositoryError::InvalidSelector);
        }
        let _lock = self.acquire_coordination_lock()?;
        let target = self.resolve_checkout_target(selector)?;
        let cwd = self.cwd_generation_locked(runtime)?;
        let git = self.checkout_git_subplan(cwd.head.as_ref(), target.commit())?;
        Ok(CheckoutPreflight {
            expected_head: cwd.head.clone(),
            target,
            cwd,
            git,
        })
    }

    /// Revalidates a previously captured checkout preflight while holding the
    /// same coordination lock future mutation will require. No ref, index,
    /// worktree, or runtime state is written.
    pub fn verify_checkout_preflight(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_checkout_preflight_locked(plan)
    }

    fn verify_checkout_preflight_locked(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let current_cwd = self.cwd_generation_locked(plan.cwd.runtime)?;
        if current_cwd != plan.cwd {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        if self.checkout_git_subplan(current_cwd.head.as_ref(), plan.target.commit())? != plan.git {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        let selector = plan
            .target
            .branch_name()
            .map_or_else(|| plan.target.commit().as_str().to_owned(), str::to_owned);
        let current_target = self.resolve_checkout_target(&selector)?;
        if current_target != plan.target {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        Ok(())
    }

    /// Switches only between attachments of the already-selected commit.
    ///
    /// This narrow checkout form cannot replace target files, so Git retains
    /// ordinary staged, unstaged, and untracked paths. It deliberately accepts
    /// neither a force flag nor a token: divergent checkout remains outside
    /// this API until its discard set and recovery journal are implemented.
    pub fn execute_same_commit_checkout(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_checkout_preflight_locked(plan)?;
        if plan.expected_head.as_ref() != Some(plan.target.commit()) {
            return Err(RepositoryError::CheckoutExecutionUnsafe);
        }

        let mut command = self.command();
        match plan.target() {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command)?;
        if self.head()?.as_ref() != Some(plan.target.commit()) {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(())
    }

    /// Executes the Git carry-forward phase for a divergent checkout whose
    /// locked preflight found no conflicting local paths.
    ///
    /// This is intentionally narrower than a complete Orna checkout: callers
    /// must validate the target's logical state and coordinate runtime
    /// activation before invoking this repository phase.  It never accepts a
    /// force witness or discard set; a conflict fails before Git is asked to
    /// change the CWD.
    pub fn execute_nonconflicting_git_checkout(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        self.execute_nonconflicting_git_checkout_with_validation(plan, |_repository, _plan| {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            CheckoutExecutionError::Repository(error) => error,
            CheckoutExecutionError::Validation(never) => match never {},
        })
    }

    /// Executes the safe Git carry-forward phase only after `validate` has
    /// accepted the isolated logical candidate under the same mutation lock.
    ///
    /// The validator may inspect the target commit and its logical state using
    /// the repository and immutable preflight. A rejected candidate leaves
    /// `HEAD`, the index, and the worktree untouched. This does not authorize
    /// conflicting or forced checkout; journaled destructive recovery remains
    /// a separate higher-level boundary.
    pub fn execute_nonconflicting_git_checkout_with_validation<E, F>(
        &self,
        plan: &CheckoutPreflight,
        validate: F,
    ) -> Result<(), CheckoutExecutionError<E>>
    where
        F: FnOnce(&Repository, &CheckoutPreflight) -> Result<(), E>,
    {
        let _lock = self
            .acquire_coordination_lock()
            .map_err(CheckoutExecutionError::Repository)?;
        self.verify_checkout_preflight_locked(plan)
            .map_err(CheckoutExecutionError::Repository)?;
        if plan.expected_head.as_ref() == Some(plan.target.commit())
            || !plan.git.conflicting_paths().is_empty()
        {
            return Err(CheckoutExecutionError::Repository(
                RepositoryError::CheckoutExecutionUnsafe,
            ));
        }

        validate(self, plan).map_err(CheckoutExecutionError::Validation)?;

        let mut command = self.command();
        match plan.target() {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command)
            .map_err(CheckoutExecutionError::Repository)?;
        if self
            .head()
            .map_err(CheckoutExecutionError::Repository)?
            .as_ref()
            != Some(plan.target.commit())
        {
            return Err(CheckoutExecutionError::Repository(
                RepositoryError::GitOperationFailed,
            ));
        }
        Ok(())
    }

    fn checkout_git_subplan(
        &self,
        current_head: Option<&GitCommitRef>,
        target: &GitCommitRef,
    ) -> Result<CheckoutGitSubplan, RepositoryError> {
        let target_paths = match current_head {
            Some(current) => self.git_nul_paths(&[
                "diff".into(),
                "--name-only".into(),
                "-z".into(),
                current.as_str().into(),
                target.as_str().into(),
                "--".into(),
            ])?,
            None => self.git_nul_paths(&[
                "ls-tree".into(),
                "-r".into(),
                "--name-only".into(),
                "-z".into(),
                target.as_str().into(),
                "--".into(),
            ])?,
        };
        let mut local_paths = HashSet::new();
        for arguments in [
            vec![
                "diff".into(),
                "--name-only".into(),
                "-z".into(),
                "--".into(),
            ],
            vec![
                "diff".into(),
                "--cached".into(),
                "--name-only".into(),
                "-z".into(),
                "--".into(),
            ],
            vec![
                "ls-files".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "-z".into(),
                "--".into(),
            ],
        ] {
            local_paths.extend(self.git_nul_paths(&arguments)?);
        }
        let affected_paths = target_paths
            .union(&local_paths)
            .cloned()
            .collect::<HashSet<_>>();
        let conflicting_paths = target_paths
            .intersection(&local_paths)
            .cloned()
            .collect::<HashSet<_>>();
        let affected_paths = sorted_paths(affected_paths);
        let conflicting_paths = sorted_paths(conflicting_paths);
        let discardable_paths = conflicting_paths.clone();
        Ok(CheckoutGitSubplan {
            affected_paths,
            conflicting_paths,
            discardable_paths,
        })
    }

    /// Revalidates and authorizes the exact preflight for a force-capable
    /// checkout operation. The witness alone is insufficient after any Git
    /// or runtime generation drift.
    pub fn authorize_checkout_force(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
    ) -> Result<(), RepositoryError> {
        self.verify_checkout_preflight(plan)?;
        plan.authorize_force(force, token)
    }

    /// Admits the exact precomputed discard set for a later divergent checkout.
    ///
    /// This is deliberately only an admission boundary: it holds the same
    /// coordination lock as checkout execution, revalidates the complete
    /// preflight, and requires the canonical force witness, but does not alter
    /// the ref, index, worktree, or runtime state. A future journaled executor
    /// must consume this validated set rather than infer a replacement set.
    pub fn validate_checkout_discard_set(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
        discard_paths: &[ManagedPath],
    ) -> Result<ValidatedCheckoutDiscard, RepositoryError> {
        self.validate_checkout_discard_set_with_validation(
            plan,
            force,
            token,
            discard_paths,
            |_repository, _plan| Ok::<(), std::convert::Infallible>(()),
        )
        .map_err(|error| match error {
            CheckoutExecutionError::Repository(error) => error,
            CheckoutExecutionError::Validation(never) => match never {},
        })
    }

    /// Admits a force discard set only after `validate` accepts the isolated
    /// logical candidate while the checkout mutation lock is held.
    ///
    /// This remains an admission boundary: it never changes the ref, index,
    /// worktree, or runtime state. A rejected logical candidate cannot be
    /// converted into a force-authorized plan, and a future journaled executor
    /// must still consume this exact validated discard set.
    pub fn validate_checkout_discard_set_with_validation<E, F>(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
        discard_paths: &[ManagedPath],
        validate: F,
    ) -> Result<ValidatedCheckoutDiscard, CheckoutExecutionError<E>>
    where
        F: FnOnce(&Repository, &CheckoutPreflight) -> Result<(), E>,
    {
        let _lock = self
            .acquire_coordination_lock()
            .map_err(CheckoutExecutionError::Repository)?;
        self.verify_checkout_preflight_locked(plan)
            .map_err(CheckoutExecutionError::Repository)?;
        validate(self, plan).map_err(CheckoutExecutionError::Validation)?;
        plan.authorize_force(force, token)
            .map_err(CheckoutExecutionError::Repository)?;
        if discard_paths == plan.git.discardable_paths() {
            Ok(ValidatedCheckoutDiscard {
                preflight: plan.clone(),
                force_token: *token.expect("authorized force checkout has a token"),
                discard_paths: discard_paths.to_vec(),
            })
        } else {
            Err(CheckoutExecutionError::Repository(
                RepositoryError::CheckoutDiscardSetMismatch,
            ))
        }
    }

    /// Explicitly resolves a Git selector to an immutable commit. This is the
    /// repository primitive behind `sys.snapshot(selector)`, not new source
    /// grammar for bare branch expressions.
    pub fn resolve_snapshot(&self, selector: &str) -> Result<GitCommitRef, RepositoryError> {
        if selector.is_empty() || selector.starts_with('-') {
            return Err(RepositoryError::InvalidSelector);
        }
        let expression = format!("{selector}^{{commit}}");
        self.commit_required(&expression)
    }

    /// Stages exactly `paths` through normal Git index semantics.
    ///
    /// It checks the caller's observed index generation first, so stale
    /// logical-stage records fail rather than silently being rebased. Git's
    /// pathspec separator ensures no path beginning with `-` is interpreted as
    /// an option. No worktree file is written by this operation.
    pub fn stage_managed(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.update_managed_index(expected, paths, ManagedIndexOperation::Add, None)
    }

    /// Unstages exactly `paths`, retaining their worktree bytes.  This is the
    /// inverse scoped index primitive and is intentionally not `reset --hard`.
    pub fn unstage_managed(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.update_managed_index(expected, paths, ManagedIndexOperation::Unstage, None)
    }

    /// Test-only interleaving seam. Production callers must use
    /// [`Self::stage_managed`]; this makes a normal Git writer race
    /// deterministic without weakening the production protocol.
    #[doc(hidden)]
    pub fn stage_managed_with_test_hook(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.update_managed_index(expected, paths, ManagedIndexOperation::Add, Some(hook))
    }

    /// Test-only equivalent of [`Self::stage_managed_with_test_hook`].
    #[doc(hidden)]
    pub fn unstage_managed_with_test_hook(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.update_managed_index(expected, paths, ManagedIndexOperation::Unstage, Some(hook))
    }

    /// Creates `name` at the selected commit using Git's nonexistence CAS.
    /// It makes no commit and does not modify the index or worktree.
    pub fn create_branch_at_head(&self, name: &str) -> Result<GitCommitRef, RepositoryError> {
        if !valid_branch_name(name) {
            return Err(RepositoryError::InvalidBranchName);
        }
        let _lock = self.acquire_coordination_lock()?;
        let head = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        let reference = format!("refs/heads/{name}");
        self.update_ref_transaction(&head, &reference)?;
        Ok(head)
    }

    /// Lists ordinary configured remote names. Internal refs are not published
    /// here; publication is deliberately owned by a later layer.
    pub fn remote_names(&self) -> Result<Vec<String>, RepositoryError> {
        let output = self.git(["remote"])?;
        Ok(output
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Atomically materialises one repository-managed file while preserving
    /// an ordinary editor's later change. The expected bytes are checked both
    /// before and immediately before replacement; `None` removes an existing
    /// regular file and is a no-op when the file is already absent.
    pub fn materialize_managed_file(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        self.ensure_atomic_worktree_install_supported()?;
        let _lock = self.acquire_coordination_lock()?;
        let target = self.managed_target(path)?;
        let current = self.read_managed_file(&target)?;
        if current.as_deref() != expected {
            return Err(RepositoryError::ManagedContentConflict);
        }
        if let Some(bytes) = next {
            let parent = target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?;
            fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            self.validate_managed_parent(parent)?;
            let candidate = parent.join(format!(
                ".orna-materialize-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .as_nanos()
            ));
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&candidate)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                file.write_all(bytes)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                file.sync_all()
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                if self.read_managed_file(&target)?.as_deref() != expected {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                fs::rename(&candidate, &target)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&candidate);
            }
            result
        } else {
            if target.exists() {
                if !fs::symlink_metadata(&target)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .is_file()
                {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
                fs::remove_file(&target).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                if let Some(parent) = target.parent() {
                    fs::File::open(parent)
                        .and_then(|directory| directory.sync_all())
                        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                }
            }
            Ok(())
        }
    }

    /// Observes one managed regular file without exposing host paths. The
    /// observation is serialized with Orna's repository coordination lock;
    /// callers that later materialise content must still pass the observed
    /// bytes back as the expected value for the final revalidation.
    pub fn managed_file_bytes(
        &self,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let target = self.managed_target(path)?;
        self.read_managed_file(&target)
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.worktree);
        command
    }
    fn git<const N: usize>(&self, args: [&str; N]) -> Result<String, RepositoryError> {
        Self::git_at(&self.worktree, args, false)
    }
    fn git_optional_tree<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<Option<IndexTreeRef>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            return self
                .index_tree_from_native_oid(trim_output(&output.stdout))
                .map(Some);
        }
        // `write-tree` failure (including exit 128) is a corrupt or
        // unmerged index, never an absent tree.
        Err(RepositoryError::GitOperationFailed)
    }
    fn git_bytes<const N: usize>(&self, args: [&str; N]) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        Ok(self.run(command)?.stdout)
    }
    fn git_nul_paths(&self, args: &[String]) -> Result<HashSet<ManagedPath>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        self.run(command)?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = String::from_utf8(path.to_vec())
                    .map_err(|_| RepositoryError::UnsafeManagedPath)?;
                ManagedPath::new(path)
            })
            .collect()
    }

    fn hash_object(&self, bytes: &[u8]) -> Result<String, RepositoryError> {
        let mut command = self.command();
        command.args(["hash-object", "-w", "--stdin"]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(bytes)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if output.status.success() {
            Ok(trim_output(&output.stdout))
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    fn candidate_tree_entry(
        &self,
        candidate: &PrivateCommit,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        let mut command = self.command();
        command
            .args([
                "ls-tree",
                "-z",
                "--full-tree",
                candidate.commit.as_str(),
                "--",
            ])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let Some(entry) = output.split(|byte| *byte == 0).next() else {
            return Ok(None);
        };
        if entry.is_empty() {
            return Ok(None);
        }
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(RepositoryError::GitOperationFailed)?;
        let metadata = &entry[..tab];
        let encoded_path = &entry[tab + 1..];
        if encoded_path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = String::from_utf8(fields.next().unwrap_or_default().to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let kind = fields.next().unwrap_or_default();
        let object = String::from_utf8(fields.next().unwrap_or_default().to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if kind != b"blob" || fields.next().is_some() {
            return Err(RepositoryError::GitOperationFailed);
        }
        GitCommitRef::from_verified_commit(object.clone(), self.native_object_id_length()?)?;
        Ok(Some((mode, object)))
    }

    fn index_entries_match_candidate(
        &self,
        candidate: &PrivateCommit,
        entries: &[PublicationJournalEntry],
    ) -> Result<bool, RepositoryError> {
        for entry in entries {
            let mut command = self.command();
            command
                .args(["ls-files", "--stage", "-z", "--"])
                .arg(entry.path.as_path());
            let output = self.run(command)?.stdout;
            let records = output
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .collect::<Vec<_>>();
            let expected = self.candidate_tree_entry(candidate, &entry.path)?;
            match (expected, records.as_slice()) {
                (None, []) => {}
                (Some((mode, object)), [record]) => {
                    let tab = record
                        .iter()
                        .position(|byte| *byte == b'\t')
                        .ok_or(RepositoryError::GitOperationFailed)?;
                    let metadata = &record[..tab];
                    let encoded_path = &record[tab + 1..];
                    if encoded_path != entry.path.as_path().as_os_str().as_encoded_bytes() {
                        return Ok(false);
                    }
                    let mut fields = metadata.split(|byte| *byte == b' ');
                    let actual_mode = fields.next().unwrap_or_default();
                    let actual_object = fields.next().unwrap_or_default();
                    let stage = fields.next().unwrap_or_default();
                    if actual_mode != mode.as_bytes()
                        || actual_object != object.as_bytes()
                        || stage != b"0"
                        || fields.next().is_some()
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Verifies that recovery's persisted loose-file intent is exactly the
    /// content committed by the private publication candidate. Without this
    /// binding, a malformed journal could reconcile the index to one blob and
    /// materialise different bytes after the ref-advance crash boundary.
    fn candidate_entries_match_journal(
        &self,
        candidate: &PrivateCommit,
        entries: &[PublicationJournalEntry],
    ) -> Result<bool, RepositoryError> {
        for entry in entries {
            match (
                entry.next(),
                self.candidate_tree_entry(candidate, &entry.path)?,
            ) {
                (None, None) => {}
                (Some(next), Some((_mode, object)))
                    if self.git_bytes(["cat-file", "blob", &object])? == next => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
    fn git_at<const N: usize>(
        directory: &Path,
        args: [&str; N],
        scrub_routing_environment: bool,
    ) -> Result<String, RepositoryError> {
        let mut command = Command::new("git");
        command.current_dir(directory).args(args);
        if scrub_routing_environment {
            scrub_git_routing_environment(&mut command);
        }
        let output = run_command(command)?;
        Ok(trim_output(&output.stdout))
    }
    fn run(&self, command: Command) -> Result<Output, RepositoryError> {
        run_command(command)
    }

    fn commit_optional(&self, expression: &str) -> Result<Option<GitCommitRef>, RepositoryError> {
        let mut command = self.command();
        command.args(["rev-parse", "--verify", "--quiet", expression]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            return self
                .snapshot_from_native_oid(trim_output(&output.stdout))
                .map(Some);
        }
        // This exact quiet rev-parse probe is the sole absence case. All
        // other exit statuses fail closed instead of treating 128 as absence.
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(RepositoryError::GitOperationFailed)
    }

    fn commit_required(&self, expression: &str) -> Result<GitCommitRef, RepositoryError> {
        let snapshot = self
            .commit_optional(expression)?
            .ok_or(RepositoryError::SnapshotNotFound)?;
        if self.head()?.as_ref() == Some(&snapshot) || self.is_reachable_from_ref(&snapshot)? {
            Ok(snapshot)
        } else {
            Err(RepositoryError::SnapshotNotReachable)
        }
    }

    fn is_reachable_from_ref(&self, snapshot: &GitCommitRef) -> Result<bool, RepositoryError> {
        let refs = self.git(["for-each-ref", "--format=%(refname)"])?;
        for reference in refs.lines().filter(|reference| !reference.is_empty()) {
            let mut command = self.command();
            command.args(["merge-base", "--is-ancestor", snapshot.as_str(), reference]);
            let output = command
                .output()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if output.status.success() {
                return Ok(true);
            }
            if output.status.code() != Some(1) {
                return Err(RepositoryError::GitOperationFailed);
            }
        }
        Ok(false)
    }

    fn resolve_checkout_target(&self, selector: &str) -> Result<CheckoutTarget, RepositoryError> {
        let branch =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/heads/{selector}"))?;
        let tag =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/tags/{selector}"))?;
        if branch && tag {
            return Err(RepositoryError::InvalidSelector);
        }
        let commit = self.resolve_snapshot(selector)?;
        if branch {
            Ok(CheckoutTarget::Branch {
                name: selector.to_owned(),
                commit,
            })
        } else {
            Ok(CheckoutTarget::Detached { commit })
        }
    }

    fn ref_exists(&self, reference: &str) -> Result<bool, RepositoryError> {
        let mut command = self.command();
        command
            .args(["show-ref", "--verify", "--quiet", "--"])
            .arg(reference);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) && output.stderr.is_empty() {
            Ok(false)
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    fn snapshot_from_native_oid(&self, oid: String) -> Result<GitCommitRef, RepositoryError> {
        let length = self.native_object_id_length()?;
        GitCommitRef::from_verified_commit(oid, length)
    }

    fn index_tree_from_native_oid(&self, oid: String) -> Result<IndexTreeRef, RepositoryError> {
        IndexTreeRef::from_verified_tree(oid, self.native_object_id_length()?)
    }

    fn native_object_id_length(&self) -> Result<usize, RepositoryError> {
        match self.git(["rev-parse", "--show-object-format"])?.as_str() {
            "sha1" => Ok(40),
            "sha256" => Ok(64),
            _ => Err(RepositoryError::UnsupportedObjectFormat),
        }
    }

    fn acquire_coordination_lock(&self) -> Result<CoordinationLock, RepositoryError> {
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let path = locks.join("coordination.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.try_lock_exclusive()
            .map_err(|_| RepositoryError::RepositoryBusy)?;
        Ok(CoordinationLock { file })
    }

    fn managed_target(&self, path: &ManagedPath) -> Result<PathBuf, RepositoryError> {
        let target = self.worktree.join(path.as_path());
        self.validate_managed_parent(
            target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )?;
        if let Ok(metadata) = fs::symlink_metadata(&target)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        Ok(target)
    }

    fn read_managed_file(&self, target: &Path) -> Result<Option<Vec<u8>>, RepositoryError> {
        match fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                Err(RepositoryError::UnsafeManagedPath)
            }
            Ok(_) => fs::read(target)
                .map(Some)
                .map_err(|_| RepositoryError::LocalStateUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn validate_managed_parent(&self, parent: &Path) -> Result<(), RepositoryError> {
        let relative = parent
            .strip_prefix(&self.worktree)
            .map_err(|_| RepositoryError::UnsafeManagedPath)?;
        let mut current = self.worktree.clone();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(RepositoryError::UnsafeManagedPath);
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Ok(())
    }

    fn update_managed_index(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        operation: ManagedIndexOperation,
        mut before_install: Option<&mut dyn FnMut()>,
    ) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_atomic_index_install_supported()?;
        self.ensure_no_git_index_lock()?;
        let index = self.git_path("index")?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let captured = self.index_generation()?;
        let base_after_capture =
            fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if &captured != expected || base_index != base_after_capture {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual: captured,
            });
        }
        let candidate = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .join(format!(
                "index-candidate-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .as_nanos()
            ));
        fs::write(&candidate, &base_index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let permissions = fs::metadata(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?
            .permissions();
        fs::set_permissions(&candidate, permissions)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let mut command = self.command();
        command.env("GIT_INDEX_FILE", &candidate);
        match operation {
            ManagedIndexOperation::Add => {
                command.arg("add");
            }
            ManagedIndexOperation::Unstage => {
                command.args(["restore", "--staged"]);
            }
        }
        command.arg("--");
        for path in paths {
            command.arg(path.as_path());
        }
        if let Err(error) = self.run(command) {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
        if let Some(hook) = before_install.as_mut() {
            hook();
        }
        fs::File::open(&candidate)
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;

        // Git itself serializes normal index writers through this pathname.
        // The candidate is prepared off-index; only an unchanged observed index
        // is atomically replaced while this lock exists.
        let git_lock = match GitIndexLock::acquire(index.with_extension("lock")) {
            Ok(lock) => lock,
            Err(error) => {
                let _ = fs::remove_file(&candidate);
                return Err(error);
            }
        };
        let current_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let current_head = self.head()?;
        if current_index != base_index || current_head != expected.head {
            let _ = fs::remove_file(&candidate);
            drop(git_lock);
            let actual = self.index_generation()?;
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        fs::rename(&candidate, &index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::File::open(
            index
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        drop(git_lock);
        self.index_generation()
    }

    fn git_path(&self, name: &str) -> Result<PathBuf, RepositoryError> {
        let path = PathBuf::from(self.git(["rev-parse", "--git-path", name])?);
        Ok(if path.is_absolute() {
            path
        } else {
            self.worktree.join(path)
        })
    }

    fn ensure_no_git_index_lock(&self) -> Result<(), RepositoryError> {
        if self.git_path("index")?.with_extension("lock").exists() {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    #[cfg(windows)]
    fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    fn update_ref_transaction(
        &self,
        head: &GitCommitRef,
        reference: &str,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let input = format!(
            "start\nverify HEAD {}\ncreate {reference} {}\nprepare\ncommit\n",
            head.as_str(),
            head.as_str()
        );
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(input.as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if status.success() {
            Ok(())
        } else {
            Err(RepositoryError::StaleHead)
        }
    }

    fn advance_ref_transaction(
        &self,
        reference: &str,
        old: &GitCommitRef,
        new: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let input = format!(
            "start\nupdate {reference} {} {}\nprepare\ncommit\n",
            new.as_str(),
            old.as_str()
        );
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(input.as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if status.success() {
            Ok(())
        } else {
            Err(RepositoryError::StaleHead)
        }
    }
}

#[derive(Clone, Copy)]
enum ManagedIndexOperation {
    Add,
    Unstage,
}

struct CoordinationLock {
    file: fs::File,
}

impl Drop for CoordinationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct GitIndexLock {
    path: PathBuf,
    _file: fs::File,
}
impl GitIndexLock {
    fn acquire(path: PathBuf) -> Result<Self, RepositoryError> {
        fs::File::create_new(&path)
            .map(|file| Self { path, _file: file })
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    // Never remove an existing Git lock: it may belong to a
                    // live ordinary Git writer or require Git's own recovery.
                    RepositoryError::GitIndexLockPresent
                } else {
                    RepositoryError::LocalStateUnavailable
                }
            })
    }
}
impl Drop for GitIndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn trim_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), RepositoryError> {
    let value = u32::try_from(value).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), RepositoryError> {
    put_u32(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) -> Result<(), RepositoryError> {
    match value {
        Some(value) => put_string(bytes, value),
        None => {
            bytes.extend_from_slice(&u32::MAX.to_le_bytes());
            Ok(())
        }
    }
}

fn put_optional_bytes(bytes: &mut Vec<u8>, value: Option<&[u8]>) -> Result<(), RepositoryError> {
    match value {
        Some(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(value);
        }
        None => bytes.extend_from_slice(&u32::MAX.to_le_bytes()),
    }
    Ok(())
}

fn take_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, RepositoryError> {
    let value = *bytes
        .get(*cursor)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor += 1;
    Ok(value)
}

fn take_fixed_array<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<[u8; N], RepositoryError> {
    let end = cursor
        .checked_add(N)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    value
        .try_into()
        .map_err(|_| RepositoryError::InvalidPublicationJournal)
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, RepositoryError> {
    let end = cursor
        .checked_add(4)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?,
    ))
}

fn take_string(bytes: &[u8], cursor: &mut usize) -> Result<String, RepositoryError> {
    let length = usize::try_from(take_u32(bytes, cursor)?)
        .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    String::from_utf8(value.to_vec()).map_err(|_| RepositoryError::InvalidPublicationJournal)
}

fn take_optional_string(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<String>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length > MAX_JOURNAL_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    Ok(Some(
        String::from_utf8(value.to_vec())
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?,
    ))
}

fn take_optional_bytes(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length > MAX_JOURNAL_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?
        .to_vec();
    *cursor = end;
    Ok(Some(value))
}
fn run_command(mut command: Command) -> Result<Output, RepositoryError> {
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(RepositoryError::GitOperationFailed)
    }
}

/// Prevent inherited Git routing from selecting a repository, worktree, or
/// index other than the explicitly supplied command directory.
pub(crate) fn scrub_git_routing_environment(command: &mut Command) {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(variable);
    }
}
fn valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && !name.bytes().any(|byte| {
            byte <= b' ' || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
}

/// Safe public repository-boundary failures. Internal Git command lines,
/// worktree paths, and Git stderr are intentionally not exposed here.
#[derive(Debug)]
pub enum RepositoryError {
    GitUnavailable,
    GitOperationFailed,
    LocalStateUnavailable,
    NotAWorktree,
    InvalidObjectId,
    UnsupportedObjectFormat,
    SnapshotNotFound,
    SnapshotNotReachable,
    InvalidSelector,
    InvalidBranchName,
    InvalidCommitMessage,
    DetachedHead,
    InvalidPublicationJournal,
    UnsafeManagedPath,
    NoManagedPaths,
    UnbornHead,
    StaleIndex {
        expected: IndexGeneration,
        actual: IndexGeneration,
    },
    StaleCwd,
    ManagedContentConflict,
    RepositoryBusy,
    GitIndexLockPresent,
    StaleHead,
    PublicationPending,
    CheckoutPlanStale,
    CheckoutDiscardSetMismatch,
    CheckoutExecutionUnsafe,
    RuntimeCompletionRequired,
    /// This profile implements atomic index replacement only on POSIX
    /// filesystems. Windows requires a separately validated replacement path.
    PlatformUnsupported,
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitUnavailable => f.write_str("Git is unavailable"),
            Self::GitOperationFailed => f.write_str("Git repository operation failed"),
            Self::LocalStateUnavailable => f.write_str("local Orna state is unavailable"),
            Self::NotAWorktree => f.write_str("not inside a Git worktree"),
            Self::InvalidObjectId => f.write_str("invalid native Git object ID"),
            Self::UnsupportedObjectFormat => f.write_str("unsupported Git object format"),
            Self::SnapshotNotFound => f.write_str("committed snapshot was not found"),
            Self::SnapshotNotReachable => {
                f.write_str("committed snapshot is not reachable from this repository")
            }
            Self::InvalidSelector => f.write_str("invalid Git snapshot selector"),
            Self::InvalidBranchName => f.write_str("invalid Git branch name"),
            Self::InvalidCommitMessage => f.write_str("invalid Git commit message"),
            Self::DetachedHead => f.write_str("publication requires a symbolic Git HEAD"),
            Self::InvalidPublicationJournal => f.write_str("invalid publication journal"),
            Self::UnsafeManagedPath => f.write_str("unsafe managed path"),
            Self::NoManagedPaths => f.write_str("at least one managed path is required"),
            Self::UnbornHead => f.write_str("cannot create a branch from an unborn HEAD"),
            Self::StaleIndex { .. } => {
                f.write_str("Git index changed since the expected generation")
            }
            Self::StaleCwd => f.write_str("Git CWD changed during observation"),
            Self::ManagedContentConflict => {
                f.write_str("managed worktree content changed since capture")
            }
            Self::RepositoryBusy => {
                f.write_str("another local Orna operation owns the repository lock")
            }
            Self::GitIndexLockPresent => {
                f.write_str("Git index lock is present; resolve it with Git before retrying")
            }
            Self::StaleHead => f.write_str("Git HEAD changed during repository operation"),
            Self::PublicationPending => {
                f.write_str("publication remains pending before Git ref advancement")
            }
            Self::CheckoutPlanStale => f.write_str("checkout preflight is stale"),
            Self::CheckoutDiscardSetMismatch => {
                f.write_str("checkout discard set does not match the preflight")
            }
            Self::CheckoutExecutionUnsafe => {
                f.write_str("checkout target does not match the current commit")
            }
            Self::RuntimeCompletionRequired => {
                f.write_str("runtime publication completion is required")
            }
            Self::PlatformUnsupported => {
                f.write_str("atomic Git index replacement is unsupported on this platform")
            }
        }
    }
}
impl std::error::Error for RepositoryError {}

/// Bridges real Git observation to the shared Orna CWD contract. The runtime
/// store is the sole authority for database/runtime identity and monotonic
/// logical generation; a Git commit selector stays a Git `SnapshotRef`.
pub struct OrnaRepositoryAdapter<S> {
    repository: Repository,
    runtime: S,
}
impl<S> OrnaRepositoryAdapter<S> {
    pub fn new(repository: Repository, runtime: S) -> Self {
        Self {
            repository,
            runtime,
        }
    }
}
#[derive(Debug)]
pub enum OrnaRepositoryAdapterError<E> {
    Runtime(E),
    Repository(RepositoryError),
    Bare,
    Identity,
    NonMonotonicGeneration,
}
impl<E: fmt::Display> fmt::Display for OrnaRepositoryAdapterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(f, "runtime identity store: {error}"),
            Self::Repository(error) => error.fmt(f),
            Self::Bare => f.write_str("a bare repository has no CWD"),
            Self::Identity => f.write_str("CWD capture belongs to a different database or runtime"),
            Self::NonMonotonicGeneration => {
                f.write_str("CWD publication must strictly advance logical generation")
            }
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for OrnaRepositoryAdapterError<E> {}
impl<S: orna_foundation_v1::RuntimeIdentityStore> OrnaRepositoryAdapter<S> {
    fn identity(
        &self,
    ) -> Result<orna_foundation_v1::RepositoryIdentity, OrnaRepositoryAdapterError<S::Error>> {
        Ok(orna_foundation_v1::RepositoryIdentity {
            database_id: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            repository_id: self
                .runtime
                .repository_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
        })
    }
    fn cwd(&self) -> Result<orna_foundation_v1::CwdCapture, OrnaRepositoryAdapterError<S::Error>> {
        let _ = self
            .repository
            .cwd_generation(RuntimeGeneration::new(0))
            .map_err(OrnaRepositoryAdapterError::Repository)?;
        let capture = self
            .runtime
            .capture_cwd()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        self.validate_capture(&capture)?;
        Ok(capture)
    }
    fn validate_capture(
        &self,
        capture: &orna_foundation_v1::CwdCapture,
    ) -> Result<(), OrnaRepositoryAdapterError<S::Error>> {
        let database_id = self
            .runtime
            .database_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        let runtime_id = self
            .runtime
            .runtime_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        if capture.database_id() != database_id || capture.runtime_id() != runtime_id {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        Ok(())
    }
}
impl<S: orna_foundation_v1::RuntimeIdentityStore> orna_foundation_v1::RepositoryGenerationAdapter
    for OrnaRepositoryAdapter<S>
{
    type Error = OrnaRepositoryAdapterError<S::Error>;
    fn require_cwd(&self) -> Result<orna_foundation_v1::RepositoryIdentity, Self::Error> {
        self.identity()
    }
    fn profile(&self) -> Result<orna_foundation_v1::RepositoryProfile, Self::Error> {
        Ok(orna_foundation_v1::RepositoryProfile { bare: false })
    }
    fn committed_snapshot(
        &self,
    ) -> Result<Option<orna_foundation_v1::CanonicalSnapshot>, Self::Error> {
        let Some(head) = self
            .repository
            .head()
            .map_err(OrnaRepositoryAdapterError::Repository)?
        else {
            return Ok(None);
        };
        let oid =
            decode_object_id(head.as_str()).map_err(OrnaRepositoryAdapterError::Repository)?;
        let algorithm = match oid.len() {
            20 => orna_foundation_v1::GitHash::Sha1,
            32 => orna_foundation_v1::GitHash::Sha256,
            _ => {
                return Err(OrnaRepositoryAdapterError::Repository(
                    RepositoryError::InvalidObjectId,
                ));
            }
        };
        Ok(Some(orna_foundation_v1::CanonicalSnapshot::Commit {
            database: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            algorithm,
            oid,
        }))
    }
    fn capture_cwd(
        &self,
        identity: orna_foundation_v1::RepositoryIdentity,
    ) -> Result<orna_foundation_v1::CwdCapture, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        self.cwd()
    }
    fn compare_and_set_cwd(
        &self,
        identity: orna_foundation_v1::RepositoryIdentity,
        expected: &orna_foundation_v1::CwdCapture,
        next: &orna_foundation_v1::CwdCapture,
    ) -> Result<orna_foundation_v1::CwdCas, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        let current = self.cwd()?;
        self.validate_capture(expected)?;
        self.validate_capture(next)?;
        if &current != expected {
            return Ok(orna_foundation_v1::CwdCas::Stale { current });
        }
        if next.generation() <= expected.generation() {
            return Err(OrnaRepositoryAdapterError::NonMonotonicGeneration);
        }
        self.runtime
            .compare_and_set_cwd(expected, next)
            .map_err(OrnaRepositoryAdapterError::Runtime)
    }
}

fn decode_object_id(value: &str) -> Result<Vec<u8>, RepositoryError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RepositoryError::InvalidObjectId);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::{Repository, RepositoryError, RuntimeGeneration};

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn checkout_preflight_rejects_same_commit_branch_attachment_drift() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        git(
            root.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(root.path(), &["config", "user.name", "Repository test"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);
        git(root.path(), &["branch", "target"]);
        git(root.path(), &["branch", "interleaved"]);

        let repository = Repository::discover(root.path()).unwrap();
        let plan = repository
            .plan_checkout("target", RuntimeGeneration::new(41))
            .unwrap();
        let token = plan.force_token();

        git(root.path(), &["switch", "interleaved"]);

        assert!(matches!(
            repository.verify_checkout_preflight(&plan),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert!(matches!(
            repository.authorize_checkout_force(&plan, true, Some(&token)),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert_eq!(
            Command::new("git")
                .current_dir(root.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
            b"interleaved\n"
        );
    }
}
