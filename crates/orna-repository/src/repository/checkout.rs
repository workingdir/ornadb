use crate::*;

impl Repository {
    /// Observes the ordinary Git index without modifying it.
    pub fn index_generation(&self) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_no_git_index_lock()?;
        self.index_generation_while_locked()
    }

    pub(crate) fn index_generation_while_locked(&self) -> Result<IndexGeneration, RepositoryError> {
        // Observe a copy: `write-tree` on the live index contends with the
        // `index.lock` the caller holds and fails with exit 128. With no index
        // file there is nothing to contend with, so git derives the empty tree.
        let index = self.git_path("index")?;
        let tree = if index.exists() {
            self.index_tree_from_file(&index)?
        } else {
            let mut command = self.command();
            command.args(["write-tree"]);
            let output = command
                .output()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if !output.status.success() {
                return Err(RepositoryError::GitOperationFailed);
            }
            Some(self.index_tree_from_native_oid(trim_output(&output.stdout))?)
        };
        Ok(IndexGeneration {
            head: self.head()?,
            tree,
        })
    }

    pub(crate) fn index_tree_from_file(
        &self,
        index: &Path,
    ) -> Result<Option<IndexTreeRef>, RepositoryError> {
        let bytes = match fs::read(index) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        let parent = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let temporary = parent.join(format!(
            ".orna-index-observe-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let mut command = self.command();
            command
                .env("GIT_INDEX_FILE", &temporary)
                .args(["write-tree"]);
            let tree = self.index_tree_from_native_oid(trim_output(&self.run(command)?.stdout))?;
            Ok(Some(tree))
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    pub(crate) fn capture_index_for_expected(
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

    /// Refuses publication when the ordinary index no longer holds the bytes
    /// captured as the reconciliation base.
    pub(crate) fn require_index_unchanged(
        &self,
        expected: &IndexGeneration,
        index: &Path,
        base_index: &[u8],
    ) -> Result<(), RepositoryError> {
        let current = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if current == base_index {
            return Ok(());
        }
        Err(RepositoryError::StaleIndex {
            expected: expected.clone(),
            actual: IndexGeneration {
                head: self.head()?,
                tree: self.index_tree_from_file(index)?,
            },
        })
    }

    /// Captures Git's ordinary worktree/index status in machine-readable form.
    pub fn worktree_state(&self) -> Result<WorktreeState, RepositoryError> {
        Ok(self.worktree_scan()?.state)
    }

    /// Captures status and hashes managed content once for this observation.
    pub(crate) fn worktree_scan(&self) -> Result<WorktreeScan, RepositoryError> {
        let status = self.worktree_status()?;
        let (digest, stamps) = self.worktree_content_scan(&[], false)?;
        Ok(WorktreeScan {
            state: WorktreeState(status, digest),
            stamps,
        })
    }

    /// Reports whether `scan` still describes the worktree. Compares status and
    /// managed-path stamps only, so managed bytes are never read a second time.
    pub(crate) fn worktree_scan_stable(
        &self,
        scan: &WorktreeScan,
    ) -> Result<bool, RepositoryError> {
        if self.worktree_status()? != scan.state.0 {
            return Ok(false);
        }
        for managed in &scan.stamps {
            if self.managed_stamp(&managed.path)? != managed.stamp {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn worktree_status(&self) -> Result<Vec<u8>, RepositoryError> {
        self.git_bytes(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
    }

    pub(crate) fn managed_stamp(
        &self,
        path: &ManagedPath,
    ) -> Result<Option<ManagedFileStamp>, RepositoryError> {
        match fs::symlink_metadata(self.worktree.join(path.as_path())) {
            Ok(metadata) => Ok(Some(Self::managed_file_stamp(&metadata))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    pub(crate) fn managed_file_stamp(metadata: &fs::Metadata) -> ManagedFileStamp {
        #[cfg(unix)]
        let change = (
            std::os::unix::fs::MetadataExt::dev(metadata),
            std::os::unix::fs::MetadataExt::ino(metadata),
            std::os::unix::fs::MetadataExt::ctime(metadata),
            std::os::unix::fs::MetadataExt::ctime_nsec(metadata),
            std::os::unix::fs::MetadataExt::mode(metadata),
        );
        ManagedFileStamp {
            file_type: metadata.file_type(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            change,
        }
    }
    #[cfg(unix)]
    pub(crate) fn worktree_executable_marker(metadata: &fs::Metadata) -> u8 {
        use std::os::unix::fs::PermissionsExt;

        u8::from(metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    const fn worktree_executable_marker(_metadata: &fs::Metadata) -> u8 {
        0
    }

    /// Digest for checkout force plans. Includes git-ignored bytes, so a force
    /// plan fences ignored files that the checkout could overwrite.
    pub(crate) fn worktree_content_digest(
        &self,
        extra_paths: &[ManagedPath],
    ) -> Result<[u8; 32], RepositoryError> {
        Ok(self.worktree_content_scan(extra_paths, true)?.0)
    }

    /// Hashes managed worktree content in bounded chunks and returns each path's
    /// stamp, taken before its bytes are read. Git-ignored files are listed and
    /// read only when `include_ignored` is set; CWD observation omits them.
    pub(crate) fn worktree_content_scan(
        &self,
        extra_paths: &[ManagedPath],
        include_ignored: bool,
    ) -> Result<([u8; 32], Vec<ManagedStamp>), RepositoryError> {
        let paths = self.worktree_scan_paths(extra_paths, include_ignored)?;
        // Regular-file sections come from the previous scan when their stamp is
        // unchanged and settled (see `stamp_settled_before`). The cache is
        // replaced by this scan's sections, so it never outlives one observation
        // of each path.
        let previous = self
            .section_cache
            .lock()
            .map(|mut cache| std::mem::take(&mut *cache))
            .unwrap_or_default();
        let mut next = HashMap::new();
        let mut digest = Sha256::new();
        digest.update(b"ORNA-WORKTREE-CONTENT-V2\0");
        let mut stamps = Vec::with_capacity(paths.len());
        for encoded_path in paths {
            let path =
                String::from_utf8(encoded_path).map_err(|_| RepositoryError::UnsafeManagedPath)?;
            // Worktree paths are local user data: untracked `Sub/.GIT/` is legal,
            // so only the structural check applies here.
            let path = ManagedPath::from_worktree_listing(&path)?;
            let scanned = self.scan_worktree_path(&path, &previous, &mut digest)?;
            stamps.push(scanned.stamp);
            if let Some((key, entry)) = scanned.cache_entry {
                next.insert(key, entry);
            }
        }
        if let Ok(mut cache) = self.section_cache.lock() {
            *cache = next;
        }
        Ok((digest.finalize().into(), stamps))
    }

    /// Collects every worktree path one content scan observes: unstaged and
    /// staged changes, untracked files (and ignored ones when asked), plus the
    /// caller's extra paths.
    pub(crate) fn worktree_scan_paths(
        &self,
        extra_paths: &[ManagedPath],
        include_ignored: bool,
    ) -> Result<BTreeSet<Vec<u8>>, RepositoryError> {
        let mut paths = BTreeSet::new();
        for path in self
            .git_bytes(["diff", "--no-renames", "--name-only", "-z", "--"])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        for path in self
            .git_bytes([
                "diff",
                "--cached",
                "--no-renames",
                "--name-only",
                "-z",
                "--",
            ])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        for path in self
            .git_bytes(["ls-files", "--others", "--exclude-standard", "-z", "--"])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        if include_ignored {
            for path in self
                .git_bytes([
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                    "--",
                ])?
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                paths.insert(path.to_vec());
            }
        }
        for path in extra_paths {
            paths.insert(path.as_path().as_os_str().as_encoded_bytes().to_vec());
        }
        Ok(paths)
    }

    /// Hashes one worktree path into `digest` and returns its stamp plus the
    /// cache entry to carry forward when a regular file's section was reused.
    pub(crate) fn scan_worktree_path(
        &self,
        path: &ManagedPath,
        previous: &HashMap<Vec<u8>, CachedFileDigest>,
        digest: &mut Sha256,
    ) -> Result<ScannedWorktreePath, RepositoryError> {
        let encoded_path = path.as_path().as_os_str().as_encoded_bytes();
        digest.update((encoded_path.len() as u64).to_be_bytes());
        digest.update(encoded_path);
        let key = encoded_path.to_vec();
        let target = self.worktree.join(path.as_path());
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        let stamp = ManagedStamp {
            path: path.clone(),
            stamp: metadata.as_ref().map(Self::managed_file_stamp),
        };
        match metadata {
            Some(metadata) if metadata.file_type().is_symlink() => {
                digest.update([1]);
                let link =
                    fs::read_link(&target).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                let link = link.as_os_str().as_encoded_bytes();
                digest.update((link.len() as u64).to_be_bytes());
                digest.update(link);
            }
            Some(metadata) if metadata.is_file() => {
                digest.update([2]);
                digest.update([Self::worktree_executable_marker(&metadata)]);
                let file_stamp = Self::managed_file_stamp(&metadata);
                let (section, entry) = match previous.get(&key) {
                    Some(cached) if cached.stamp == file_stamp => {
                        (cached.digest, Some(cached.clone()))
                    }
                    _ => {
                        let read_started = std::time::SystemTime::now();
                        let section = Self::file_section_digest(&target)?;
                        let settled = Self::stamp_settled_before(&file_stamp, read_started);
                        let entry = settled.then_some(CachedFileDigest {
                            stamp: file_stamp,
                            digest: section,
                        });
                        (section, entry)
                    }
                };
                digest.update(section);
                return Ok(ScannedWorktreePath {
                    stamp,
                    cache_entry: entry.map(|entry| (key, entry)),
                });
            }
            Some(_) => digest.update([3]),
            None => digest.update([0]),
        }
        Ok(ScannedWorktreePath {
            stamp,
            cache_entry: None,
        })
    }

    /// Hashes one regular file's length and bytes as a standalone section, so
    /// the section can be cached against the file's stamp.
    pub(crate) fn file_section_digest(target: &Path) -> Result<[u8; 32], RepositoryError> {
        #[cfg(test)]
        CONTENT_READS.with(|reads| reads.borrow_mut().push(target.to_path_buf()));
        let mut section = Sha256::new();
        Self::digest_file_contents(target, &mut section)?;
        Ok(section.finalize().into())
    }

    /// Reports whether a file's stamp is settled before `read_started`: its mtime
    /// and ctime both precede it by the racy window. Only then can a later write
    /// leave the stamp unchanged, so only settled sections may be reused. A file
    /// written within the window is always read again.
    pub(crate) fn stamp_settled_before(
        stamp: &ManagedFileStamp,
        read_started: std::time::SystemTime,
    ) -> bool {
        let Some(limit) = read_started.checked_sub(CONTENT_RACY_WINDOW) else {
            return false;
        };
        let modified_settled = stamp.modified.is_some_and(|modified| modified <= limit);
        #[cfg(unix)]
        let changed_settled = {
            let (_, _, ctime, ctime_nsec, _) = stamp.change;
            u64::try_from(ctime)
                .ok()
                .zip(u32::try_from(ctime_nsec).ok())
                .and_then(|(secs, nanos)| {
                    std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(secs, nanos))
                })
                .is_some_and(|changed| changed <= limit)
        };
        #[cfg(not(unix))]
        let changed_settled = true;
        modified_settled && changed_settled
    }

    /// Streams a regular file's length and bytes into `digest` through a fixed
    /// chunk. A file that shrinks while it is read fails the observation.
    pub(crate) fn digest_file_contents(
        target: &Path,
        digest: &mut Sha256,
    ) -> Result<(), RepositoryError> {
        let file = fs::File::open(target).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let len = file
            .metadata()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?
            .len();
        digest.update(len.to_be_bytes());
        let mut reader = file.take(len);
        let mut chunk = [0_u8; 64 * 1024];
        let mut copied = 0_u64;
        loop {
            let read = reader
                .read(&mut chunk)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if read == 0 {
                break;
            }
            digest.update(&chunk[..read]);
            copied += read as u64;
        }
        if copied != len {
            return Err(RepositoryError::StaleCwd);
        }
        Ok(())
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

    pub(crate) fn cwd_generation_locked(
        &self,
        runtime: RuntimeGeneration,
    ) -> Result<CwdGeneration, RepositoryError> {
        // Do not return a torn CWD if another Git process changes the index,
        // branch, status, or a managed file while it is observed.  The lock
        // serializes Orna writers; the bounded recheck detects external Git
        // writers by stamp, so managed bytes are hashed once per attempt.
        for _ in 0..3 {
            let before = self.index_generation()?;
            let branch = self.current_branch()?;
            let scan = self.worktree_scan()?;
            let after = self.index_generation()?;
            let branch_after = self.current_branch()?;
            if before == after && branch == branch_after && self.worktree_scan_stable(&scan)? {
                return Ok(CwdGeneration {
                    head: before.head.clone(),
                    branch,
                    index: before,
                    worktree: scan.state,
                    runtime,
                });
            }
        }
        Err(RepositoryError::StaleCwd)
    }

    /// Returns the local branch currently attached to `HEAD`, if any. A
    /// detached `HEAD` is distinct CWD state even when it names the same
    /// commit as a local branch.
    pub(crate) fn current_branch(&self) -> Result<Option<String>, RepositoryError> {
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
        let _lock = self.acquire_coordination_lock()?;
        self.plan_checkout_locked(selector, runtime)
    }

    pub(crate) fn plan_checkout_locked(
        &self,
        selector: &str,
        runtime: RuntimeGeneration,
    ) -> Result<CheckoutPreflight, RepositoryError> {
        if selector.is_empty() || selector.starts_with('-') || selector.contains('\0') {
            return Err(RepositoryError::InvalidSelector);
        }
        let target = self.resolve_checkout_target(selector)?;
        let cwd = self.cwd_generation_locked(runtime)?;
        let git = self.checkout_git_subplan(cwd.head.as_ref(), target.commit())?;
        let dirty_content_digest = self.worktree_content_digest(&git.discardable_paths)?;
        Ok(CheckoutPreflight {
            expected_head: cwd.head.clone(),
            target,
            cwd,
            git,
            dirty_content_digest,
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

    pub(crate) fn verify_checkout_preflight_locked(
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
        if self.worktree_content_digest(&plan.git.discardable_paths)? != plan.dirty_content_digest {
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
        self.require_no_pending_publication_recovery_locked()?;
        self.require_no_pending_checkout_recovery_locked()?;
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
    /// must coordinate runtime activation before invoking this repository
    /// phase. The gate decides whether the target's logical state may become
    /// current (ORNA-ASSERT-044). It runs under the mutation lock after the
    /// target snapshot is admitted and before Git moves; a refusal leaves
    /// `HEAD`, the index, and the worktree untouched. The phase never accepts
    /// a force witness or discard set; a conflict fails before Git is asked to
    /// change the CWD.
    pub fn execute_nonconflicting_git_checkout(
        &self,
        plan: &CheckoutPreflight,
        gate: &dyn CheckoutAssertionGate,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        self.require_no_pending_checkout_recovery_locked()?;
        self.verify_checkout_preflight_locked(plan)?;
        if plan.expected_head.as_ref() == Some(plan.target.commit())
            || !plan.git.conflicting_paths().is_empty()
        {
            return Err(RepositoryError::CheckoutExecutionUnsafe);
        }

        self.validate_checkout_target_context(plan.target.commit())?;

        gate.validate(self, plan)
            .map_err(RepositoryError::CandidateAssertionRefused)?;
        self.verify_checkout_preflight_locked(plan)?;

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

    /// Admits the immutable target snapshot before checkout can make it
    /// visible. Format-3 targets must carry the committed source and native
    /// store roots; legacy formats remain read-only checkout targets.
    pub(crate) fn validate_checkout_target_context(
        &self,
        target: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let snapshot = self
            .pin_snapshot(target.as_str())
            .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        let context = self
            .open_format_context_at(&snapshot)
            .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        if context.supports_writes() {
            context
                .validate_schema_root()
                .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
            context
                .validate_store_root()
                .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        } else if !context.is_read_only() {
            return Err(RepositoryError::CheckoutTargetInvalid);
        }
        Ok(())
    }

    pub(crate) fn checkout_git_subplan(
        &self,
        current_head: Option<&GitCommitRef>,
        target: &GitCommitRef,
    ) -> Result<CheckoutGitSubplan, RepositoryError> {
        self.objects().check_expansion(target.as_str(), "")?;
        if let Some(current) = current_head {
            self.objects().check_expansion(current.as_str(), "")?;
        }
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
            vec![
                "ls-files".into(),
                "--others".into(),
                "--ignored".into(),
                "--exclude-standard".into(),
                "-z".into(),
                "--".into(),
            ],
        ] {
            local_paths.extend(self.git_worktree_nul_paths(&arguments)?);
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
    /// preflight, asks the gate to accept the target's logical state
    /// (ORNA-ASSERT-044), and requires the canonical force witness. It never
    /// changes the ref, index, worktree, or runtime state, and a rejected
    /// logical candidate cannot be converted into a force-authorized plan. A
    /// future journaled executor must consume this exact validated discard set
    /// rather than infer a replacement set.
    pub fn validate_checkout_discard_set(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
        discard_paths: &[ManagedPath],
        gate: &dyn CheckoutAssertionGate,
    ) -> Result<ValidatedCheckoutDiscard, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_checkout_preflight_locked(plan)?;
        gate.validate(self, plan)
            .map_err(RepositoryError::CandidateAssertionRefused)?;
        plan.authorize_force(force, token)?;
        if discard_paths == plan.git.discardable_paths() {
            Ok(ValidatedCheckoutDiscard {
                preflight: plan.clone(),
                force_token: token.copied().ok_or(RepositoryError::CheckoutPlanStale)?,
                discard_paths: discard_paths.to_vec(),
            })
        } else {
            Err(RepositoryError::CheckoutDiscardSetMismatch)
        }
    }

    /// Rechecks an admitted force-discard capability at the destructive
    /// execution boundary.
    ///
    /// Force admission and checkout execution cannot share a lock across
    /// arbitrary runtime coordination. A capability is therefore not a
    /// reusable right to discard: immediately before a journaled executor
    /// changes Git state, it must prove that the captured target, CWD, exact
    /// discard set, and canonical force witness still describe this CWD. Any
    /// intervening ref, index, worktree, or runtime-generation change fails
    /// closed before mutation.
    pub fn verify_validated_checkout_discard(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_validated_checkout_discard_locked(discard)
    }

    pub(crate) fn verify_validated_checkout_discard_locked(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        self.verify_checkout_preflight_locked(&discard.preflight)?;
        if discard.discard_paths != discard.preflight.git.discardable_paths() {
            return Err(RepositoryError::CheckoutDiscardSetMismatch);
        }
        if discard.force_token != discard.preflight.force_token() {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        discard
            .preflight
            .authorize_force(true, Some(&discard.force_token))
    }

    /// A durable force-discard intent means recovery owns the next checkout
    /// transition. No other Git-visible checkout may interleave with it: doing
    /// so would invalidate the before-state that recovery has to compare and
    /// could turn a recoverable intent into a partial switch.
    pub(crate) fn require_no_pending_checkout_recovery_locked(
        &self,
    ) -> Result<(), RepositoryError> {
        if self.read_checkout_recovery_journal_locked()?.is_some() {
            Err(RepositoryError::CheckoutRecoveryRequired)
        } else {
            Ok(())
        }
    }

    /// A publication journal retains the exact ref/index/worktree generation
    /// that its own recovery protocol must reconcile. A force-checkout cannot
    /// journal or mutate a competing before-state until publication recovery
    /// has resolved that durable boundary.
    pub(crate) fn require_no_pending_publication_recovery_locked(
        &self,
    ) -> Result<(), RepositoryError> {
        if self.read_publication_journal_locked()?.is_some() {
            Err(RepositoryError::PublicationPending)
        } else {
            Ok(())
        }
    }

    /// Durably records a verified force-discard capability before a future
    /// destructive executor can alter Git state. The record is intentionally
    /// not an execution command: restart recovery may only clear it after
    /// proving that the exact original preflight still holds.
    pub fn persist_validated_checkout_discard(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        self.verify_validated_checkout_discard_locked(discard)?;
        let journal = CheckoutRecoveryJournal::from_validated(discard);
        match self.read_checkout_recovery_journal_locked()? {
            Some(existing) if !existing.matches_validated(discard) => {
                Err(RepositoryError::CheckoutRecoveryRequired)
            }
            Some(_) => Ok(()),
            None => self.write_checkout_recovery_journal_locked(&journal),
        }
    }

    /// Executes an admitted force-discard checkout through durable,
    /// repository-local recovery boundaries.
    ///
    /// Only the exact consented paths are restored from the selected target;
    /// the subsequent ordinary `git switch` carries unrelated staged,
    /// unstaged, and untracked state using Git's normal compatibility rules.
    /// Before the scoped discard, after it, and after selection are each
    /// journalled as complete CWD generations. Recovery consequently never
    /// uses a blind reset after an interruption.
    pub fn execute_validated_force_checkout(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, None, None)
    }

    /// Test-only interruption seam for the durable discard boundary.
    ///
    /// Production callers must use [`Self::execute_validated_force_checkout`].
    /// The hook runs after the scoped discard generation is durably journalled,
    /// but before Git selects the recorded target. Recovery must resume that
    /// recorded transition without resetting unrelated local state.
    #[doc(hidden)]
    pub fn execute_validated_force_checkout_after_discard_with_test_hook(
        &self,
        discard: &ValidatedCheckoutDiscard,
        hook: &mut dyn FnMut() -> Result<(), RepositoryError>,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, Some(hook), None)
    }

    /// Test-only interruption seam for the post-selection journal boundary.
    ///
    /// Production callers must use [`Self::execute_validated_force_checkout`].
    /// The hook runs after Git has selected the target and its CWD has been
    /// observed, but before that selected generation is durably journalled.
    /// This is the only crash window in which recovery must recognize the
    /// selected attachment without an already-recorded applied generation.
    pub fn execute_validated_force_checkout_with_test_hook(
        &self,
        discard: &ValidatedCheckoutDiscard,
        hook: &mut dyn FnMut() -> Result<(), RepositoryError>,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, None, Some(hook))
    }

    pub(crate) fn execute_validated_force_checkout_impl(
        &self,
        discard: &ValidatedCheckoutDiscard,
        mut after_discard: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
        mut after_applied: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        let mut journal = match self.read_checkout_recovery_journal_locked()? {
            Some(journal)
                if journal.matches_validated(discard)
                    && matches!(journal.state, CheckoutRecoveryState::Prepared { .. }) =>
            {
                journal
            }
            Some(_) | None => return Err(RepositoryError::CheckoutRecoveryRequired),
        };
        self.verify_validated_checkout_discard_locked(discard)?;

        self.restore_checkout_discard_paths(discard)?;
        journal.record_discarded(self.cwd_generation_locked(discard.preflight.cwd.runtime)?)?;
        self.write_checkout_recovery_journal_locked(&journal)?;
        if let Some(hook) = after_discard.as_mut() {
            hook()?;
        }

        self.switch_checkout_target(discard.preflight.target())?;
        let after = self.cwd_generation_locked(discard.preflight.cwd.runtime)?;
        if after.head.as_ref() != Some(discard.preflight.target.commit())
            || after.branch.as_deref() != discard.preflight.target.branch_name()
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        if let Some(hook) = after_applied.as_mut() {
            hook()?;
        }
        journal.record_applied(after)?;
        self.write_checkout_recovery_journal_locked(&journal)?;
        self.clear_checkout_recovery_journal_locked()
    }

    pub(crate) fn restore_checkout_discard_paths(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["restore", "--source"])
            .arg(discard.preflight.target.commit().as_str())
            .args(["--staged", "--worktree", "--"]);
        for path in &discard.discard_paths {
            command.arg(path.as_path());
        }
        self.run(command).map(|_| ())
    }

    pub(crate) fn switch_checkout_target(
        &self,
        target: &CheckoutTarget,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        match target {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command).map(|_| ())
    }

    pub(crate) fn verify_prepared_checkout_journal_locked(
        &self,
        journal: &CheckoutRecoveryJournal,
        verify_force_token: bool,
    ) -> Result<(), RepositoryError> {
        let selector = journal.target.branch_name().map_or_else(
            || journal.target.commit().as_str().to_owned(),
            str::to_owned,
        );
        let plan = self.plan_checkout_locked(&selector, journal.runtime)?;
        if plan.target != journal.target
            || plan.git.discardable_paths != journal.discard_paths
            || (verify_force_token && plan.force_token() != journal.force_token)
        {
            return Err(RepositoryError::CheckoutRecoveryRequired);
        }
        Ok(())
    }

    /// Resolves a force-discard journal after restart.
    ///
    /// A prepared transition preserves the recorded old CWD. A journalled
    /// post-selection CWD completes by releasing the journal. A partial or
    /// externally changed transition is retained and reported instead of
    /// resetting files that no longer match its recorded generations.
    pub fn recover_pre_execution_checkout(&self) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        let Some(mut journal) = self.read_checkout_recovery_journal_locked()? else {
            return Ok(());
        };
        let step = match &journal.state {
            // Version-one journals did not contain enough state to recognize a
            // post-mutation CWD. Retain their historical no-mutation recovery
            // behavior rather than assuming a destructive transition completed.
            CheckoutRecoveryState::PreparedWithoutGeneration => {
                CheckoutRecoveryStep::ReleaseLegacyIntent
            }
            CheckoutRecoveryState::Prepared { before } => {
                if &self.cwd_generation_locked(journal.runtime)? != before {
                    return Err(RepositoryError::CheckoutRecoveryRequired);
                }
                CheckoutRecoveryStep::ReleasePrepared
            }
            CheckoutRecoveryState::Discarded { discarded, .. } => {
                let current = self.cwd_generation_locked(journal.runtime)?;
                if &current == discarded {
                    CheckoutRecoveryStep::SelectTarget
                } else if current.head.as_ref() == Some(journal.target.commit())
                    && current.branch.as_deref() == journal.target.branch_name()
                {
                    // A process may stop after Git has selected the target but
                    // before that observable CWD generation is journalled. The
                    // target attachment proves this is the recorded transition,
                    // while retaining the observed index and worktree state
                    // avoids any broad reset over later local changes.
                    CheckoutRecoveryStep::AdoptSelection(current)
                } else {
                    return Err(RepositoryError::CheckoutRecoveryRequired);
                }
            }
            CheckoutRecoveryState::Applied { after, .. } => {
                if &self.cwd_generation_locked(journal.runtime)? != after {
                    return Err(RepositoryError::CheckoutRecoveryRequired);
                }
                CheckoutRecoveryStep::ReleaseApplied
            }
        };
        match step {
            CheckoutRecoveryStep::ReleaseLegacyIntent => {
                self.verify_prepared_checkout_journal_locked(&journal, false)?;
            }
            CheckoutRecoveryStep::ReleasePrepared => {
                self.verify_prepared_checkout_journal_locked(&journal, true)?;
            }
            CheckoutRecoveryStep::ReleaseApplied => {}
            CheckoutRecoveryStep::SelectTarget => {
                // The target may be a movable branch. Re-resolve it while
                // the recovery lock is held before switching: otherwise a
                // branch update between discard and restart could make
                // recovery mutate HEAD/worktree to a different commit
                // than the one the force witness authorized.
                self.verify_prepared_checkout_journal_locked(&journal, false)?;
                self.switch_checkout_target(&journal.target)?;
                let after = self.cwd_generation_locked(journal.runtime)?;
                if after.head.as_ref() != Some(journal.target.commit())
                    || after.branch.as_deref() != journal.target.branch_name()
                {
                    return Err(RepositoryError::GitOperationFailed);
                }
                journal.record_applied(after)?;
                self.write_checkout_recovery_journal_locked(&journal)?;
            }
            CheckoutRecoveryStep::AdoptSelection(current) => {
                journal.record_applied(current)?;
                self.write_checkout_recovery_journal_locked(&journal)?;
            }
        }
        self.clear_checkout_recovery_journal_locked()
    }

    /// Reports whether restart recovery has a retained pre-execution checkout
    /// intent. This exposes no capability or discard details.
    pub fn has_pending_pre_execution_checkout(&self) -> Result<bool, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        Ok(self.read_checkout_recovery_journal_locked()?.is_some())
    }
}
