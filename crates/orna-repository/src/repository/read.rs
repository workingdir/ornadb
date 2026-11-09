use crate::*;

impl Repository {
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

    /// Resolves one exact native Git object ID as a reachable immutable
    /// commit. This accepts only the repository's native object format and
    /// never creates or hydrates Git objects.
    pub fn resolve_committed_oid(
        &self,
        algorithm: orna_foundation::GitHash,
        oid: &[u8],
    ) -> Result<GitCommitRef, RepositoryError> {
        let native_length = self.native_object_id_length()?;
        let (expected_algorithm, expected_bytes) = match native_length {
            40 => (orna_foundation::GitHash::Sha1, 20),
            64 => (orna_foundation::GitHash::Sha256, 32),
            _ => return Err(RepositoryError::UnsupportedObjectFormat),
        };
        if algorithm != expected_algorithm || oid.len() != expected_bytes {
            return Err(RepositoryError::InvalidObjectId);
        }

        let mut object_id = String::with_capacity(native_length);
        for byte in oid {
            use std::fmt::Write as _;
            write!(object_id, "{byte:02x}").map_err(|_| RepositoryError::GitOperationFailed)?;
        }
        self.commit_required(&format!("{object_id}^{{commit}}"))
    }

    /// Reads one bounded regular file from an already-resolved reachable
    /// commit without consulting or changing the worktree, index, or refs.
    ///
    /// Git tree entries that are directories, symlinks, or submodules are not
    /// source files and are rejected at this boundary. A size limit is
    /// required so a historical snapshot cannot make a caller allocate an
    /// unbounded source buffer.
    pub fn read_committed_file(
        &self,
        commit: &GitCommitRef,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.read_tree_file(commit.as_str(), path, max_bytes)
    }

    /// Returns the exact commit recorded by one gitlink in a committed tree.
    /// The returned ID belongs to the attached repository; callers must open
    /// that repository and resolve the object there before reading its files.
    pub fn committed_submodule_commit(
        &self,
        commit: &GitCommitRef,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.read_submodule_commit(commit.as_str(), path)
    }

    /// Returns the gitlink commit from a private publication candidate.
    pub fn private_candidate_submodule_commit(
        &self,
        candidate: &PrivateCommit,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.read_submodule_commit(candidate.tree.as_str(), path)
    }

    pub(crate) fn read_submodule_commit(
        &self,
        treeish: &str,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        let path = ManagedPath::new(path)?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
            .arg(path.as_path());
        let output = self.run(command)?;
        let mut entries = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty());
        let entry = entries.next().ok_or(RepositoryError::GitOperationFailed)?;
        if entries.next().is_some() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let record = GitTreeRecord::parse(entry).ok_or(RepositoryError::GitOperationFailed)?;
        if record.path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        if record.mode != b"160000"
            || record.kind != b"commit"
            || record.object.len() != self.native_object_id_length()?
            || !record.object.iter().all(u8::is_ascii_hexdigit)
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object = String::from_utf8(record.object.to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        GitCommitRef::from_verified_commit(object, self.native_object_id_length()?)
    }

    /// Reads one bounded regular file from the exact immutable tree carried by
    /// a private candidate. The opaque `PrivateCommit` capability is required:
    /// raw unreachable commit IDs do not become valid committed snapshots.
    pub fn read_private_candidate_file(
        &self,
        candidate: &PrivateCommit,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.read_tree_file(candidate.tree.as_str(), path, max_bytes)
    }

    pub(crate) fn read_tree_file(
        &self,
        treeish: &str,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let path = ManagedPath::new(path)?;
        let mut tree = self.command();
        tree.env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
            .arg(path.as_path());
        let tree_output = self.run(tree)?;
        let entry = tree_output
            .stdout
            .split(|byte| *byte == 0)
            .find(|entry| !entry.is_empty())
            .ok_or(RepositoryError::GitOperationFailed)?;
        let record = GitTreeRecord::parse(entry).ok_or(RepositoryError::GitOperationFailed)?;
        if record.path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        if record.mode != b"100644" && record.mode != b"100755" || record.kind != b"blob" {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object = String::from_utf8(record.object.to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        require_native_object_id(&object, self.native_object_id_length()?)?;

        let mut size_command = self.command();
        size_command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-s", object.as_str()]);
        let size = trim_output(&self.run(size_command)?.stdout)
            .parse::<usize>()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if size > max_bytes {
            return Err(RepositoryError::GitOperationFailed);
        }

        let mut read_command = self.command();
        read_command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "blob", object.as_str()])
            .stdout(Stdio::piped());
        let mut child = read_command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let mut bytes = Vec::with_capacity(size);
        let stdout = child
            .stdout
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?;
        stdout
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() || bytes.len() > max_bytes || bytes.len() != size {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(bytes)
    }

    /// Lists a committed tree recursively without consulting or changing the
    /// worktree, index, or refs.
    ///
    /// The caller supplies an entry limit before Git output is accumulated.
    /// Symlinks and submodules are returned for explicit rejection by the
    /// caller; malformed or unsupported tree modes fail closed. File bytes
    /// remain unavailable through this listing and require
    /// [`Self::read_committed_file`] with a separate byte bound.
    pub fn list_committed_tree(
        &self,
        commit: &GitCommitRef,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.list_tree_entries(commit.as_str(), max_entries)
    }

    /// Lists the exact immutable tree carried by a private candidate without
    /// consulting the worktree, index, or refs. The candidate capability is
    /// bound to both its unreachable commit and root tree; callers cannot
    /// authorize arbitrary unreachable objects by selecting an object ID.
    pub fn list_private_candidate_tree(
        &self,
        candidate: &PrivateCommit,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.list_tree_entries(candidate.tree.as_str(), max_entries)
    }

    pub(crate) fn verify_private_candidate(
        &self,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", candidate.commit.as_str());
        let resolved = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::GitOperationFailed)?;
        if resolved != candidate.commit {
            return Err(RepositoryError::GitOperationFailed);
        }

        let tree_expression = format!("{}^{{tree}}", candidate.commit.as_str());
        let mut tree_command = self.command();
        tree_command.args(["rev-parse", "--verify", &tree_expression]);
        let resolved_tree =
            self.index_tree_from_native_oid(trim_output(&self.run(tree_command)?.stdout))?;
        if resolved_tree != candidate.tree {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(())
    }

    pub(crate) fn list_tree_entries(
        &self,
        treeish: &str,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        const MAX_ENTRY_BYTES: usize = 4 * 1024;

        self.objects().check_expansion(treeish, "")?;
        let object_id_length = self.native_object_id_length()?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-r", "-z", "--full-tree", treeish])
            .stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let listed = {
            let stdout = child
                .stdout
                .take()
                .ok_or(RepositoryError::GitOperationFailed)?;
            let mut stdout = BufReader::new(stdout);
            let mut entries = Vec::new();
            (|| {
                loop {
                    let mut raw_entry = Vec::new();
                    let bytes = (&mut stdout)
                        .take((MAX_ENTRY_BYTES + 1) as u64)
                        .read_until(b'\0', &mut raw_entry)
                        .map_err(|_| RepositoryError::GitOperationFailed)?;
                    if bytes == 0 {
                        break;
                    }
                    if raw_entry.len() > MAX_ENTRY_BYTES || raw_entry.last() != Some(&b'\0') {
                        return Err(RepositoryError::GitOperationFailed);
                    }
                    if entries.len() == max_entries {
                        return Err(RepositoryError::GitOperationFailed);
                    }
                    raw_entry.pop();
                    entries.push(parse_committed_tree_entry(&raw_entry, object_id_length)?);
                }
                Ok(entries)
            })()
        };
        let entries = match listed {
            Ok(entries) => entries,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
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

    /// Stages the exact managed-file bytes already captured by a caller.
    ///
    /// The candidate index is built off-index and atomically installed only if
    /// the observed index generation remains current and the worktree bytes
    /// still match the supplied contents. Git path filters and the file mode
    /// are preserved while the staged blob comes from those contents, not a
    /// second worktree read.
    pub fn stage_managed_contents(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<IndexGeneration, RepositoryError> {
        self.stage_managed_contents_inner(expected, path, contents, None)
    }

    /// Test-only interleaving seam for worktree changes between candidate
    /// construction and installation.
    #[doc(hidden)]
    pub fn stage_managed_contents_with_test_hook(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        self.stage_managed_contents_inner(expected, path, contents, Some(hook))
    }

    pub(crate) fn stage_managed_contents_inner(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
        before_install: Option<&mut dyn FnMut()>,
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.ensure_managed_contents_match(path, contents)?;
        self.update_managed_index(
            expected,
            std::slice::from_ref(path),
            ManagedIndexOperation::AddContents(contents),
            before_install,
        )
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

    /// Test-only interleaving seam. Production callers use stage_managed or
    /// stage_managed_contents; this makes a normal Git writer race deterministic
    /// without weakening the production protocol.
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
}
