use crate::*;

impl Repository {
    pub(crate) fn git_at<const N: usize>(
        directory: &Path,
        args: [&str; N],
    ) -> Result<String, RepositoryError> {
        let mut command = git_command_in(directory);
        command.args(args);
        let output = run_command(command)?;
        Ok(trim_output(&output.stdout))
    }
    pub(crate) fn run(&self, command: Command) -> Result<Output, RepositoryError> {
        run_command(command)
    }

    pub(crate) fn commit_optional(
        &self,
        expression: &str,
    ) -> Result<Option<GitCommitRef>, RepositoryError> {
        let mut command = self.command();
        command.args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            expression,
        ]);
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

    pub(crate) fn commit_required(
        &self,
        expression: &str,
    ) -> Result<GitCommitRef, RepositoryError> {
        let snapshot = self
            .commit_optional(expression)?
            .ok_or(RepositoryError::SnapshotNotFound)?;
        if self.head()?.as_ref() == Some(&snapshot) || self.is_reachable_from_ref(&snapshot)? {
            Ok(snapshot)
        } else {
            Err(RepositoryError::SnapshotNotReachable)
        }
    }

    pub(crate) fn is_reachable_from_ref(
        &self,
        snapshot: &GitCommitRef,
    ) -> Result<bool, RepositoryError> {
        // A ref need not name a commit: the internal `refs/orna/pins/*`
        // publication pins name trees. Ancestry is only defined between
        // commits, so a ref that cannot peel to a commit is skipped rather than
        // failing the resolution of a snapshot that is otherwise valid
        // (`orna export DEST --at SELECTOR` freezes and verifies the selected
        // snapshot closure).
        let refs = self.git([
            "for-each-ref",
            "--format=%(objecttype)%00%(*objecttype)%00%(refname)",
        ])?;
        for line in refs.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split('\0');
            let object_type = fields.next().unwrap_or_default();
            let peeled_type = fields.next().unwrap_or_default();
            let Some(reference) = fields.next().filter(|reference| !reference.is_empty()) else {
                continue;
            };
            if object_type != "commit" && peeled_type != "commit" {
                continue;
            }
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

    pub(crate) fn resolve_checkout_target(
        &self,
        selector: &str,
    ) -> Result<CheckoutTarget, RepositoryError> {
        let branch =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/heads/{selector}"))?;
        let tag =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/tags/{selector}"))?;
        let other_ref = if branch {
            [
                format!("refs/{selector}"),
                format!("refs/remotes/{selector}"),
                format!("refs/remotes/{selector}/HEAD"),
            ]
            .into_iter()
            .try_fold(false, |found, reference| {
                if found || reference == format!("refs/heads/{selector}") {
                    Ok(found)
                } else {
                    self.ref_exists(&reference)
                }
            })?
        } else {
            false
        };
        if branch && (tag || other_ref) {
            return Err(RepositoryError::InvalidSelector);
        }
        // A full object ID and a local branch name are both valid selectors,
        // but their attachment semantics differ. Reject the collision before
        // resolving the target so callers cannot accidentally select a branch
        // merely because Git gave it precedence during revision parsing.
        if branch && self.is_full_commit_object_id(selector)? {
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

    pub(crate) fn is_full_commit_object_id(&self, selector: &str) -> Result<bool, RepositoryError> {
        let length = self.native_object_id_length()?;
        if selector.len() != length || !selector.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(false);
        }
        Ok(self
            .commit_optional(&format!("{selector}^{{commit}}"))?
            .is_some())
    }

    pub(crate) fn ref_exists(&self, reference: &str) -> Result<bool, RepositoryError> {
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

    pub(crate) fn snapshot_from_native_oid(
        &self,
        oid: String,
    ) -> Result<GitCommitRef, RepositoryError> {
        let length = self.native_object_id_length()?;
        GitCommitRef::from_verified_commit(oid, length)
    }

    pub(crate) fn index_tree_from_native_oid(
        &self,
        oid: String,
    ) -> Result<IndexTreeRef, RepositoryError> {
        IndexTreeRef::from_verified_tree(oid, self.native_object_id_length()?)
    }

    pub(crate) fn native_object_id_length(&self) -> Result<usize, RepositoryError> {
        Ok(self.objects.algorithm()?.width() * 2)
    }

    /// The local branch currently attached to `HEAD`, for callers that must
    /// name it as a transport ref. `None` for a detached `HEAD`.
    pub fn checked_out_branch(&self) -> Result<Option<String>, RepositoryError> {
        self.current_branch()
    }

    /// Runs a repository operation while the worktree coordination lock is held.
    ///
    /// The lock is acquired before invoking the callback and released on return,
    /// including when the callback fails or panics.
    pub fn with_coordination_lock<T, E>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<RepositoryError>,
    {
        let _lock = self.acquire_coordination_lock().map_err(E::from)?;
        operation()
    }

    pub(crate) fn acquire_coordination_lock(&self) -> Result<CoordinationLock, RepositoryError> {
        let file = self.open_coordination_lock_file()?;
        file.try_lock_exclusive()
            .map_err(|_| RepositoryError::RepositoryBusy)?;
        Ok(CoordinationLock { file })
    }

    /// Acquires the worktree publication lock as a typed lease (PUB-3 step 2
    /// and step 7).
    ///
    /// Publication-state writes below the CLI require this lease, so callers
    /// cannot freeze or complete a publication without joining the worktree's
    /// publication boundary. A held lock is reported as `RepositoryBusy` once
    /// the bounded wait elapses; it never blocks indefinitely.
    pub fn acquire_publication_lease(&self) -> Result<PublicationLease, RepositoryError> {
        self.acquire_coordination_lock_for_publication()
    }

    /// Acquires the coordination lock for publication, waiting at most
    /// `COORDINATION_LOCK_WAIT`. A publisher that loses a race queues briefly
    /// behind the winner and then fails its compare-and-set (ORNA-PUB-009).
    /// A held or hung lock, or a same-thread re-entry, is reported as
    /// `RepositoryBusy` once the bound elapses; it never blocks indefinitely.
    pub(crate) fn acquire_coordination_lock_for_publication(
        &self,
    ) -> Result<PublicationLease, RepositoryError> {
        let file = self.open_coordination_lock_file()?;
        let deadline = std::time::Instant::now() + self.coordination_lock_wait;
        Ok(PublicationLease {
            file: lock_before_deadline(file, deadline, |file| file.try_lock_exclusive())?,
            runtime_root: self.runtime.root().to_path_buf(),
        })
    }

    pub(crate) fn open_coordination_lock_file(&self) -> Result<fs::File, RepositoryError> {
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        open_lock_file(&locks.join("coordination.lock"))
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }

    /// Runs `operation` inside the declared maintenance boundary (ORNA-CAP-006).
    ///
    /// Locks are taken in one fixed order: the exclusive maintenance lease, then
    /// the coordination lock. Capture and prepare hold the shared maintenance
    /// lease while they write objects that no ref names yet, so a Git `gc` or
    /// `repack` run here cannot prune them. A held boundary or lease, in this
    /// process or another, is reported as `RepositoryBusy` once the bounded wait
    /// elapses; the call never blocks indefinitely.
    ///
    /// Git GC and repack are supported only through this entry. Concurrent
    /// destructive prune that bypasses the boundary, such as `git prune --expire
    /// now` run by another tool, is unsupported and is not advertised as safe.
    pub fn with_maintenance_boundary<T, E>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<RepositoryError>,
    {
        let _maintenance = self
            .acquire_maintenance_lease(MaintenanceMode::Exclusive)
            .map_err(E::from)?;
        let _coordination = self
            .acquire_coordination_lock_for_publication()
            .map_err(E::from)?;
        operation()
    }

    /// Holds the shared maintenance lease for the duration of a capture or a
    /// prepare. Shared leases coexist with each other; a held exclusive boundary
    /// is reported as `RepositoryBusy` once the bounded wait elapses (ORNA-CAP-006).
    pub fn hold_maintenance_lease(&self) -> Result<MaintenanceLease, RepositoryError> {
        self.acquire_maintenance_lease(MaintenanceMode::Shared)
    }

    pub(crate) fn acquire_maintenance_lease(
        &self,
        mode: MaintenanceMode,
    ) -> Result<MaintenanceLease, RepositoryError> {
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let file = open_lock_file(&locks.join("maintenance.lock"))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let deadline = std::time::Instant::now() + self.coordination_lock_wait;
        let file = lock_before_deadline(file, deadline, |file| match mode {
            MaintenanceMode::Shared => FileExt::try_lock_shared(file),
            MaintenanceMode::Exclusive => file.try_lock_exclusive(),
        })?;
        Ok(MaintenanceLease { file })
    }

    /// Acquires the process-lifetime owner lock for this worktree's local
    /// runtime. The lock is separate from short-lived repository coordination
    /// locks and remains held for the runtime lifetime.
    ///
    /// Current and departed identities are written as one atomic state update.
    /// Replacing the current identity therefore cannot erase a prior owner
    /// before manifest validation and guarded reclamation succeed.
    pub fn acquire_runtime_owner_lock(
        &self,
        owner_id: [u8; 16],
    ) -> Result<RuntimeOwnerLock, RepositoryError> {
        if owner_id == [0; 16] {
            return Err(RepositoryError::InvalidInternalRef);
        }
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let mut file = open_lock_file(&locks.join("runtime-owner.lock"))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.try_lock_exclusive()
            .map_err(|_| RepositoryError::RepositoryBusy)?;
        let state_path = locks.join("runtime-owner.state");
        let (current_owner, mut departed_owners) =
            read_runtime_owner_state(&state_path, &mut file)?;
        let previous_owner = (current_owner != [0; 16]).then_some(current_owner);
        if let Some(previous_owner) = previous_owner
            && !departed_owners.contains(&previous_owner)
        {
            departed_owners.push(previous_owner);
        }
        persist_runtime_owner_state(&state_path, owner_id, &departed_owners)?;
        // Migrate the legacy lock-file payload only after the atomic state is
        // durable; the file itself remains the stable advisory-lock inode.
        file.set_len(0)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.sync_all()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(RuntimeOwnerLock {
            file,
            repository: self.clone(),
            previous_owner,
            current_owner: owner_id,
            departed_owners,
            owner_state_path: state_path,
        })
    }

    pub(crate) fn managed_target(&self, path: &ManagedPath) -> Result<PathBuf, RepositoryError> {
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

    pub(crate) fn read_managed_file(
        &self,
        target: &Path,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
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

    pub(crate) fn ensure_managed_contents_match(
        &self,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<(), RepositoryError> {
        let target = self.managed_target(path)?;
        if self.read_managed_file(&target)?.as_deref() == Some(contents) {
            Ok(())
        } else {
            Err(RepositoryError::ManagedContentConflict)
        }
    }

    pub(crate) fn managed_index_matches_head(
        &self,
        expected_head: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<bool, RepositoryError> {
        let mut command = self.command();
        scrub_git_routing_environment(&mut command);
        command
            .args([
                "diff",
                "--cached",
                "--quiet",
                "--exit-code",
                expected_head.as_str(),
                "--",
            ])
            .arg(path.as_path());
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    pub(crate) fn committed_managed_file_bytes(
        &self,
        expected_head: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let mut command = self.command();
        command
            .args(["ls-tree", "-z", "--full-tree", expected_head.as_str(), "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let Some(entry) = output.split(|byte| *byte == 0).next() else {
            return Ok(None);
        };
        if entry.is_empty() {
            return Ok(None);
        }
        let record = GitTreeRecord::parse(entry).ok_or(RepositoryError::GitOperationFailed)?;
        if record.path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        if record.kind != b"blob" || !matches!(record.mode, b"100644" | b"100755") {
            return Err(RepositoryError::ManagedContentConflict);
        }
        let object = String::from_utf8(record.object.to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        require_native_object_id(&object, self.native_object_id_length()?)?;

        let mut command = self.command();
        command.args(["cat-file", "blob", &object]);
        Ok(Some(self.run(command)?.stdout))
    }

    pub(crate) fn validate_managed_parent(&self, parent: &Path) -> Result<(), RepositoryError> {
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

    pub(crate) fn update_managed_index(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        operation: ManagedIndexOperation<'_>,
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
        let candidate = StagedFile::create(
            index
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?
                .join(format!("index-candidate-{}", Uuid::new_v4())),
            &base_index,
        )?;
        let permissions = fs::metadata(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?
            .permissions();
        fs::set_permissions(candidate.path(), permissions)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let mut command = self.command();
        command.env("GIT_INDEX_FILE", candidate.path());
        match operation {
            ManagedIndexOperation::Add | ManagedIndexOperation::AddContents(_) => {
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
        self.run(command)?;
        if let ManagedIndexOperation::AddContents(contents) = operation {
            self.prepare_index_contents(candidate.path(), &paths[0], contents)?;
        }
        if let Some(hook) = before_install.as_mut() {
            hook();
        }
        fs::File::open(candidate.path())
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;

        // Git itself serializes normal index writers through this pathname.
        // The candidate is prepared off-index; only an unchanged observed index
        // is atomically replaced while this lock exists.
        let git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
        let current_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let current_head = self.head()?;
        if current_index != base_index || current_head != expected.head {
            drop(git_lock);
            let actual = self.index_generation()?;
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        if let ManagedIndexOperation::AddContents(contents) = operation {
            let path = paths.first().ok_or(RepositoryError::NoManagedPaths)?;
            self.ensure_managed_contents_match(path, contents)?;
        }
        fs::rename(candidate.path(), &index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
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
    pub(crate) fn index_file_mode(
        &self,
        index: &Path,
        path: &ManagedPath,
    ) -> Result<&'static str, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", index)
            .args(["--literal-pathspecs", "ls-files", "--stage", "-z", "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let entry = output
            .split(|byte| *byte == 0)
            .find(|entry| !entry.is_empty())
            .ok_or(RepositoryError::GitOperationFailed)?;
        let record = GitListRecord::parse(entry).ok_or(RepositoryError::GitOperationFailed)?;
        let mode = record.field(0).ok_or(RepositoryError::GitOperationFailed)?;
        match mode {
            b"100644" => Ok("100644"),
            b"100755" => Ok("100755"),
            _ => Err(RepositoryError::UnsafeManagedPath),
        }
    }

    pub(crate) fn prepare_index_contents(
        &self,
        index: &Path,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<(), RepositoryError> {
        let mode = self.index_file_mode(index, path)?;
        let object = self.hash_object_at_path(contents, path)?;
        let path = path
            .as_path()
            .to_str()
            .ok_or(RepositoryError::UnsafeManagedPath)?;
        let cacheinfo = format!("{mode},{object},{path}");
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", index)
            .args([
                "--literal-pathspecs",
                "update-index",
                "--add",
                "--cacheinfo",
            ])
            .arg(cacheinfo);
        self.run(command)?;
        Ok(())
    }

    /// Resolves one Git path for this worktree. Git prints paths as bytes; one
    /// that is not UTF-8 cannot be returned as a `PathBuf` here without
    /// silently mangling it, so it is refused instead of lossily replaced.
    pub(crate) fn git_path(&self, name: &str) -> Result<PathBuf, RepositoryError> {
        let output = self.git_bytes(["rev-parse", "--git-path", name])?;
        let value = std::str::from_utf8(output.trim_ascii_end())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let path = PathBuf::from(value);
        Ok(if path.is_absolute() {
            path
        } else {
            self.worktree.join(path)
        })
    }
    pub(crate) fn ensure_no_git_operation_in_progress(&self) -> Result<(), RepositoryError> {
        for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
            let path = self.git_path(marker)?;
            match fs::symlink_metadata(path) {
                Ok(_) => return Err(RepositoryError::RepositoryBusy),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_no_git_index_lock(&self) -> Result<(), RepositoryError> {
        if self.git_path("index")?.with_extension("lock").exists() {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    pub(crate) fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    pub(crate) fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    pub(crate) fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    pub(crate) fn update_ref_transaction(
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

    pub(crate) fn advance_ref_transaction(
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
