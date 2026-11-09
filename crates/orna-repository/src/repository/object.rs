use crate::*;

impl Repository {
    /// Atomically materialises one repository-managed file while preserving
    /// an ordinary editor's later change. The expected bytes are checked both
    /// before and immediately before replacement or removal; `None` removes
    /// an existing regular file and is a no-op when the file is already absent.
    pub fn materialize_managed_file(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        self.materialize_managed_file_impl(path, expected, next, None)
    }

    pub(crate) fn materialize_managed_file_impl(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        before_install: Option<&mut dyn FnMut()>,
    ) -> Result<(), RepositoryError> {
        self.materialize_managed_file_impl_with_quarantine(
            path,
            expected,
            next,
            None,
            before_install,
            None,
        )
    }

    pub(crate) fn materialize_managed_file_impl_with_quarantine(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        quarantine_path: Option<&Path>,
        before_install: Option<&mut dyn FnMut()>,
        after_boundary: Option<
            &mut dyn FnMut(PublicationMaterializationPhase) -> Result<(), RepositoryError>,
        >,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.materialize_managed_file_impl_with_quarantine_locked(
            path,
            expected,
            next,
            quarantine_path,
            before_install,
            after_boundary,
        )
    }

    pub(crate) fn materialize_managed_file_impl_with_quarantine_locked(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        quarantine_path: Option<&Path>,
        mut before_install: Option<&mut dyn FnMut()>,
        mut after_boundary: Option<
            &mut dyn FnMut(PublicationMaterializationPhase) -> Result<(), RepositoryError>,
        >,
    ) -> Result<(), RepositoryError> {
        self.ensure_atomic_worktree_install_supported()?;
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
                managed_candidate_recheck_race();
                if self.read_managed_file(&target)?.as_deref() != expected {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                let quarantine = if expected.is_some() {
                    let quarantine = match quarantine_path {
                        Some(path) => {
                            match fs::symlink_metadata(path) {
                                Ok(_) => return Err(RepositoryError::ManagedContentConflict),
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(_) => {
                                    return Err(RepositoryError::LocalStateUnavailable);
                                }
                            }
                            path.to_path_buf()
                        }
                        None => self.new_managed_quarantine(parent)?,
                    };
                    if let Some(hook) = before_install.as_mut() {
                        hook();
                    }
                    match Self::rename_path_without_replacement(&target, &quarantine) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Err(RepositoryError::ManagedContentConflict);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(RepositoryError::ManagedContentConflict);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                            return Err(RepositoryError::PlatformUnsupported);
                        }
                        Err(_) => {
                            return Err(RepositoryError::LocalStateUnavailable);
                        }
                    }
                    if self.read_managed_file(&quarantine)?.as_deref() != expected {
                        return match self.restore_managed_quarantine(&quarantine, &target) {
                            Ok(()) | Err(RepositoryError::ManagedContentConflict) => {
                                Err(RepositoryError::ManagedContentConflict)
                            }
                            Err(error) => Err(error),
                        };
                    }
                    if let Some(hook) = after_boundary.as_mut() {
                        hook(PublicationMaterializationPhase::Quarantined)?;
                    }
                    Some(quarantine)
                } else {
                    None
                };
                if expected.is_none() {
                    if let Some(hook) = before_install.as_mut() {
                        hook();
                    }
                    if self.read_managed_file(&target)?.as_deref() != expected {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                }
                self.install_managed_candidate(&candidate, &target)?;
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Installed)?;
                }
                if let Some(quarantine) = quarantine {
                    self.remove_verified_quarantine(parent, &quarantine, &target, expected, next)?;
                }
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
                if let Some(hook) = before_install.as_mut() {
                    hook();
                }
                if self.read_managed_file(&target)?.as_deref() != expected {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                let parent = target
                    .parent()
                    .ok_or(RepositoryError::LocalStateUnavailable)?;
                let quarantine = match quarantine_path {
                    Some(path) => {
                        match fs::symlink_metadata(path) {
                            Ok(_) => return Err(RepositoryError::ManagedContentConflict),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
                        }
                        path.to_path_buf()
                    }
                    None => self.new_managed_quarantine(parent)?,
                };
                if let Some(hook) = before_install.as_mut() {
                    hook();
                }
                match Self::rename_path_without_replacement(&target, &quarantine) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                        return Err(RepositoryError::PlatformUnsupported);
                    }
                    Err(_) => {
                        return Err(RepositoryError::LocalStateUnavailable);
                    }
                }
                if self.read_managed_file(&quarantine)?.as_deref() != expected {
                    return match self.restore_managed_quarantine(&quarantine, &target) {
                        Ok(()) | Err(RepositoryError::ManagedContentConflict) => {
                            Err(RepositoryError::ManagedContentConflict)
                        }
                        Err(error) => Err(error),
                    };
                }
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Quarantined)?;
                }
                if fs::symlink_metadata(&target).is_ok() {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Installed)?;
                }
                self.remove_verified_quarantine(parent, &quarantine, &target, expected, None)?;
                fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Ok(())
        }
    }

    pub(crate) fn new_managed_quarantine(&self, parent: &Path) -> Result<PathBuf, RepositoryError> {
        for _ in 0..8 {
            let quarantine = parent.join(format!(
                ".orna-quarantine-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            match fs::symlink_metadata(&quarantine) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(quarantine);
                }
                Ok(_) => continue,
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Err(RepositoryError::LocalStateUnavailable)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn rename_path_without_replacement(from: &Path, to: &Path) -> std::io::Result<()> {
        let from_parent = from
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let to_parent = to
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let from_directory = rustix::fs::openat(
            rustix::fs::CWD,
            from_parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let to_directory = rustix::fs::openat(
            rustix::fs::CWD,
            to_parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        rustix::fs::renameat_with(
            &from_directory,
            from.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing source name")
            })?,
            &to_directory,
            to.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing target name")
            })?,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn rename_path_without_replacement(_from: &Path, _to: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    pub(crate) fn restore_managed_quarantine(
        &self,
        quarantine: &Path,
        target: &Path,
    ) -> Result<(), RepositoryError> {
        self.restore_managed_quarantine_impl(quarantine, target, None)
    }

    pub(crate) fn restore_managed_quarantine_impl(
        &self,
        quarantine: &Path,
        target: &Path,
        mut before_restore: Option<&mut dyn FnMut()>,
    ) -> Result<(), RepositoryError> {
        match fs::symlink_metadata(target) {
            Ok(_) => Err(RepositoryError::ManagedContentConflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(hook) = before_restore.as_mut() {
                    hook();
                }
                match fs::hard_link(quarantine, target) {
                    Ok(()) => {
                        let parent = target
                            .parent()
                            .ok_or(RepositoryError::LocalStateUnavailable)?;
                        let target_bytes = self.read_managed_file(target)?;
                        self.remove_verified_quarantine(
                            parent,
                            quarantine,
                            target,
                            target_bytes.as_deref(),
                            target_bytes.as_deref(),
                        )
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        Err(RepositoryError::ManagedContentConflict)
                    }
                    Err(_) => Err(RepositoryError::LocalStateUnavailable),
                }
            }
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    pub(crate) fn install_managed_candidate(
        &self,
        candidate: &Path,
        target: &Path,
    ) -> Result<(), RepositoryError> {
        match fs::hard_link(candidate, target) {
            Ok(()) => {
                fs::remove_file(candidate).map_err(|_| RepositoryError::LocalStateUnavailable)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(RepositoryError::ManagedContentConflict)
            }
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
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

    pub(crate) fn command(&self) -> Command {
        git_command_in(&self.worktree)
    }

    /// Constructs a Git command for a passive observer.  This intentionally
    /// ignores inherited routing and object-store overrides so every query is
    /// bound to this repository's selected worktree.
    pub(crate) fn observer_command(&self) -> Command {
        self.command()
    }
    pub(crate) fn git<const N: usize>(&self, args: [&str; N]) -> Result<String, RepositoryError> {
        Self::git_at(&self.worktree, args)
    }
    pub(crate) fn git_bytes_no_lazy_fetch<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args(args);
        Ok(self.run(command)?.stdout)
    }
    pub(crate) fn git_bytes<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        Ok(self.run(command)?.stdout)
    }
    /// Parses paths Git reports for a committed tree or diff. Each is a
    /// committed name, so it passes [`ManagedPath::new`] in full.
    pub(crate) fn git_nul_paths(
        &self,
        args: &[String],
    ) -> Result<HashSet<ManagedPath>, RepositoryError> {
        self.git_nul_paths_with(args, |path| ManagedPath::new(path))
    }

    /// Parses paths Git reports for the local worktree, which include untracked
    /// user directories such as `Sub/.GIT/`. Those are ordinary local data, so
    /// they take only the structural check.
    pub(crate) fn git_worktree_nul_paths(
        &self,
        args: &[String],
    ) -> Result<HashSet<ManagedPath>, RepositoryError> {
        self.git_nul_paths_with(args, ManagedPath::from_worktree_listing)
    }

    pub(crate) fn git_nul_paths_with(
        &self,
        args: &[String],
        parse: impl Fn(&str) -> Result<ManagedPath, RepositoryError>,
    ) -> Result<HashSet<ManagedPath>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        self.run(command)?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path =
                    std::str::from_utf8(path).map_err(|_| RepositoryError::UnsafeManagedPath)?;
                parse(path)
            })
            .collect()
    }

    pub(crate) fn hash_object(&self, bytes: &[u8]) -> Result<String, RepositoryError> {
        self.hash_object_with_path(bytes, None)
    }

    pub(crate) fn hash_object_at_path(
        &self,
        bytes: &[u8],
        path: &ManagedPath,
    ) -> Result<String, RepositoryError> {
        self.hash_object_with_path(bytes, Some(path))
    }

    pub(crate) fn hash_object_with_path(
        &self,
        bytes: &[u8],
        path: Option<&ManagedPath>,
    ) -> Result<String, RepositoryError> {
        let mut command = self.command();
        command.args(["hash-object", "-w"]);
        if let Some(path) = path {
            let path = path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::UnsafeManagedPath)?;
            command.arg(format!("--path={path}"));
        }
        command.arg("--stdin");
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

    pub(crate) fn candidate_tree_entry(
        &self,
        candidate: &PrivateCommit,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        self.tree_entry(candidate.commit.as_str(), path)
    }

    /// Reads the entry at `path` in `treeish`. Only a blob, or the tree at
    /// the fixed `.orna/store` position, is admitted.
    pub(crate) fn tree_entry(
        &self,
        treeish: &str,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        let mut command = self.command();
        command
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
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
        let mode = String::from_utf8(record.mode.to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let object = String::from_utf8(record.object.to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if record.kind == b"blob" {
            require_native_object_id(&object, self.native_object_id_length()?)?;
        } else if path.as_path() == Path::new(".orna/store")
            && record.kind == b"tree"
            && mode == "040000"
        {
            IndexTreeRef::from_verified_tree(object.clone(), self.native_object_id_length()?)?;
        } else {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(Some((mode, object)))
    }

    pub(crate) fn committed_store_tree_path(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<Option<ManagedPath>, RepositoryError> {
        let store_path = ManagedPath::new(".orna/store")?;
        if journal
            .entries()
            .iter()
            .any(|entry| entry.path == store_path)
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        match (
            journal.store_root(),
            self.candidate_tree_entry(candidate, &store_path)?,
        ) {
            (Some(expected), Some((mode, object)))
                if mode == "040000" && object == expected.as_str() =>
            {
                Ok(Some(store_path))
            }
            (None, Some((mode, _))) if mode == "040000" => Ok(Some(store_path)),
            (None, None) => Ok(None),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }

    pub(crate) fn publication_index_paths(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<Vec<ManagedPath>, RepositoryError> {
        let mut paths = journal
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        if let Some(store_path) = self.committed_store_tree_path(candidate, journal)? {
            paths.push(store_path);
        }
        Ok(paths)
    }

    pub(crate) fn index_entries_match_candidate(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        for path in self.publication_index_paths(candidate, journal)? {
            let expected = self.candidate_tree_entry(candidate, &path)?;
            if expected.as_ref().is_some_and(|(mode, _)| mode == "040000") {
                // The index holds a bound tree as its leaf entries, never as
                // one `040000` record, so compare the subtree the index writes
                // at this path with the candidate's store root (ORNA-PUB-007).
                let index = self.git_path("index")?;
                let Some(tree) = self.index_tree_from_file(&index)? else {
                    return Ok(false);
                };
                if self.tree_entry(tree.as_str(), &path)? != expected {
                    return Ok(false);
                }
                continue;
            }
            let mut command = self.command();
            command
                .args(["ls-files", "--stage", "-z", "--"])
                .arg(path.as_path());
            let output = self.run(command)?.stdout;
            let records = output
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .collect::<Vec<_>>();
            match (expected, records.as_slice()) {
                (None, []) => {}
                (Some((mode, object)), [record]) => {
                    let record =
                        GitListRecord::parse(record).ok_or(RepositoryError::GitOperationFailed)?;
                    if record.name() != path.as_path().as_os_str().as_encoded_bytes() {
                        return Ok(false);
                    }
                    let actual_mode = record.field(0).unwrap_or_default();
                    let actual_object = record.field(1).unwrap_or_default();
                    let stage = record.field(2).unwrap_or_default();
                    if actual_mode != mode.as_bytes()
                        || actual_object != object.as_bytes()
                        || stage != b"0"
                        || record.field(3).is_some()
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
    /// Verifies recovery's persisted file intents and optional store-root
    /// tree witness against the private publication candidate. This prevents
    /// recovery from reconciling the index to a different file or store tree
    /// after the ref-advance crash boundary.
    pub(crate) fn candidate_entries_match_journal(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        let _store_path = self.committed_store_tree_path(candidate, journal)?;
        for entry in journal.entries() {
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
}
