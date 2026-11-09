//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Git-resolved local paths for one worktree's private Orna runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub(crate) root: PathBuf,
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
    /// The runtime-initialized Ed25519 public key used only to authenticate
    /// compact completion receipts. Runtime initialization creates this fixed
    /// private-state record; repository code never provisions or replaces it.
    pub fn compact_runtime_receipt_public_key(&self) -> PathBuf {
        self.root.join("compact-runtime-receipt-public-key-v1")
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
    /// Creates a normalized, repository-relative path for a write, a stage, or
    /// a committed-tree entry.
    /// Absolute paths, `.git` administration components at any depth, tracked
    /// `.orna/cache.db`, parent traversal, backslashes, NUL, and empty
    /// components are rejected before they reach Git. No length limit applies
    /// here: Git stores committed names of any length, so an overlong committed
    /// entry must still list. The filesystem reports an overlong name when Orna
    /// creates it.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        Self::normalize(path.as_ref(), true).map(Self)
    }

    /// Wraps a path Git reports for the local worktree (untracked, ignored, or
    /// changed). A user-created directory such as `Sub/.GIT/` is ordinary local
    /// data that Git lists, so this keeps the structural checks but not the Git
    /// metadata refusal. It only feeds conflict classification, never a write.
    pub(crate) fn from_worktree_listing(path: &str) -> Result<Self, RepositoryError> {
        Self::normalize(Path::new(path), false).map(Self)
    }

    fn normalize(path: &Path, strict: bool) -> Result<PathBuf, RepositoryError> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        if strict {
            // Git treats `\` as ordinary, but Windows checkouts and sync tools do
            // not, so `..\x` would traverse there. NUL cannot name a file at all.
            // Empty components (`a//b`, `a/`) are refused as Git's verify_path does.
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.iter().any(|byte| matches!(*byte, b'\\' | 0))
                || bytes.split(|byte| *byte == b'/').any(<[u8]>::is_empty)
            {
                return Err(RepositoryError::UnsafeManagedPath);
            }
        }
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    if strict
                        && orna_foundation::is_git_metadata_component(value.as_encoded_bytes())
                    {
                        return Err(RepositoryError::UnsafeManagedPath);
                    }
                    normalized.push(value);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
            }
        }
        if normalized.components().next().is_none() || normalized == Path::new(".orna/cache.db") {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        Ok(normalized)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Git materialisation mode observed from local configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitRepositoryMode {
    /// No sparse-checkout or partial/promisor configuration was observed.
    Ordinary,
    /// Git's working-tree sparsity is enabled without partial/promisor state.
    SparseCheckout,
    /// Git's object store is configured for partial/promisor operation without
    /// sparse-checkout state.
    PartialClone,
    /// Both working-tree sparsity and partial/promisor state are enabled.
    Combined,
    /// The relevant local Git configuration could not be interpreted safely.
    Malformed,
}

/// A redacted, read-only observation of a repository's Git materialisation
/// capabilities. Remote URLs and filesystem paths are intentionally absent;
/// only configured remote names are retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRepositoryCapabilities {
    pub(crate) mode: GitRepositoryMode,
    pub(crate) sparse_checkout: bool,
    pub(crate) partial_clone: bool,
    pub(crate) promisor_remotes: Vec<String>,
}

impl GitRepositoryCapabilities {
    /// The combined sparse/partial classification.
    pub const fn mode(&self) -> GitRepositoryMode {
        self.mode
    }

    /// Whether Git has enabled sparse worktree materialisation.
    pub const fn sparse_checkout(&self) -> bool {
        self.sparse_checkout
    }

    /// Whether partial/promisor object configuration was observed.
    pub const fn partial_clone(&self) -> bool {
        self.partial_clone
    }

    /// Configured promisor remote names. URLs and credentials are never
    /// returned by this observation.
    pub fn promisor_remotes(&self) -> &[String] {
        &self.promisor_remotes
    }
}

/// The fixed Git object kinds accepted by the capability observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitObjectKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

/// Safe local availability information for one complete native Git object ID.
/// A promised object is known only when a local promisor walk proves that the
/// missing ID is reachable; no fetch or lazy hydration is attempted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitObjectState {
    Materialized { kind: GitObjectKind, size: u64 },
    Promised,
    Unavailable,
    Malformed,
}

/// Read-only completeness classification for a caller-declared set of native
/// Git object IDs. A complete result means every supplied ID is either locally
/// materialized or proven reachable as a promised object. It does not infer
/// whether the caller supplied every object required by a logical snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitDeclaredObjectSetState {
    Complete,
    Incomplete,
    Malformed,
}

/// A validated Orna-managed remote reference.  Only refs in `refs/orna/*`
/// participate in continuity checks; ordinary Git refs remain outside this
/// observer's authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OrnaInternalRef(String);

impl OrnaInternalRef {
    /// Validates one fully qualified internal reference name.
    pub fn new(reference: impl Into<String>) -> Result<Self, RepositoryError> {
        let reference = reference.into();
        if !valid_orna_internal_ref(&reference) {
            return Err(RepositoryError::InvalidInternalRef);
        }
        Ok(Self(reference))
    }

    /// The fully qualified `refs/orna/*` name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated native Git object ID supplied to a remote-continuity check.
/// The observer additionally verifies that its length agrees with the local
/// repository's native object format before contacting a remote.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeObjectId(String);

impl NativeObjectId {
    /// Accepts complete SHA-1 or SHA-256 hexadecimal object IDs only.
    pub fn new(object_id: impl Into<String>) -> Result<Self, RepositoryError> {
        let mut object_id = object_id.into();
        if !valid_native_object_id(&object_id) {
            return Err(RepositoryError::InvalidObjectId);
        }
        object_id.make_ascii_lowercase();
        Ok(Self(object_id))
    }

    /// The native hexadecimal object ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One remote internal ref whose exact native object ID is required before an
/// allocator or checkpoint can claim continuity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredInternalRef {
    pub(crate) reference: OrnaInternalRef,
    pub(crate) expected: NativeObjectId,
}

impl RequiredInternalRef {
    pub fn new(reference: OrnaInternalRef, expected: NativeObjectId) -> Self {
        Self {
            reference,
            expected,
        }
    }

    pub fn reference(&self) -> &OrnaInternalRef {
        &self.reference
    }

    pub fn expected(&self) -> &NativeObjectId {
        &self.expected
    }
}

/// Read-only continuity evidence for a configured remote's `refs/orna/*`
/// namespace. `InvalidEvidence` and `Unverifiable` are deliberately
/// fail-closed: callers must not allocate IDs or claim checkpoint/allocator
/// continuity from either outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteContinuity {
    Continuous,
    Missing,
    Stale,
    /// Caller-supplied required refs cannot form one unambiguous native
    /// continuity claim, so no remote observation was attempted.
    InvalidEvidence,
    Unverifiable,
}

impl RemoteContinuity {
    /// Whether this observation is sufficient to claim remote allocator or
    /// checkpoint continuity. Every non-continuous observation is fail-closed.
    pub const fn permits_continuity_claim(self) -> bool {
        matches!(self, Self::Continuous)
    }
}
