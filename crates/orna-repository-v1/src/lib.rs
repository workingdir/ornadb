//! Git-valid repository state for Orna 1.0.
//!
//! This crate intentionally models only the repository boundary.  It does not
//! write `.orna/` metadata or advance an ordinary ref.  In particular,
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
    index: IndexGeneration,
    worktree: WorktreeState,
    runtime: RuntimeGeneration,
}

impl CwdGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
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
        let worktree = PathBuf::from(Self::git_at(directory, ["rev-parse", "--show-toplevel"])?);
        let runtime = PathBuf::from(Self::git_at(
            &worktree,
            ["rev-parse", "--git-path", "orna"],
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
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        let index = self.git_path("index")?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let actual = self.index_generation()?;
        if actual.tree() != expected_index.tree() {
            return Err(RepositoryError::StaleIndex {
                expected: expected_index.clone(),
                actual,
            });
        }
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
        fs::write(&candidate_index, &base_index)
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
            let git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
            let current_index =
                fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if current_index != base_index || self.head()?.as_ref() != Some(candidate.commit()) {
                drop(git_lock);
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

    /// Observes the ordinary Git index without modifying it.
    pub fn index_generation(&self) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_no_git_index_lock()?;
        Ok(IndexGeneration {
            head: self.head()?,
            tree: self.git_optional_tree(["write-tree"])?,
        })
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
        // Do not return a torn CWD if another Git process changes the index
        // while status is being read.  The lock serializes Orna writers; the
        // bounded recheck detects external Git writers.
        for _ in 0..3 {
            let before = self.index_generation()?;
            let worktree = self.worktree_state()?;
            let after = self.index_generation()?;
            let worktree_after = self.worktree_state()?;
            if before == after && worktree == worktree_after {
                return Ok(CwdGeneration {
                    head: before.head.clone(),
                    index: before,
                    worktree,
                    runtime,
                });
            }
        }
        Err(RepositoryError::StaleCwd)
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
        Self::git_at(&self.worktree, args)
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
    fn git_at<const N: usize>(
        directory: &Path,
        args: [&str; N],
    ) -> Result<String, RepositoryError> {
        let mut command = Command::new("git");
        command.current_dir(directory).args(args);
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
