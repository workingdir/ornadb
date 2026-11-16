use crate::*;
use std::process::{Child, ChildStdin, ChildStdout};

impl Repository {
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
            &git_lock,
        )
    }

    pub(crate) fn reconcile_published_index_locked(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        paths: &[ManagedPath],
        base_index: &[u8],
        _git_lock: &GitIndexLock,
    ) -> Result<IndexGeneration, RepositoryError> {
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        let index = self.git_path("index")?;
        let mut unique = HashSet::new();
        if paths.iter().any(|path| !unique.insert(path.clone())) {
            return Err(RepositoryError::UnsafeManagedPath);
        }

        let candidate_index = StagedFile::create(
            index
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?
                .join(format!("index-candidate-{}", Uuid::new_v4())),
            base_index,
        )?;
        for path in paths {
            let encoded = path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::UnsafeManagedPath)?;
            match self.candidate_tree_entry(candidate, path)? {
                Some((mode, object)) => bind_tree_entry_in_index(
                    || self.command(),
                    candidate_index.path(),
                    encoded,
                    &mode,
                    &object,
                )?,
                None => {
                    let mut update = self.command();
                    update
                        .env("GIT_INDEX_FILE", candidate_index.path())
                        .args(["update-index", "--force-remove", "--"])
                        .arg(path.as_path());
                    self.run(update)?;
                }
            }
        }
        // Reconcile store leaves before atomically installing the candidate index.
        self.reconcile_store_worktree_transition(expected_index, candidate, true)?;
        // Gate D boundary: the store worktree has been rewritten but the
        // candidate index is not yet installed (ORNA-PUB-005/PUB-016).
        crate::publication_crash_barrier("store-worktree");
        self.configure_store_tree_skip_worktree(candidate_index.path())?;
        fs::File::open(candidate_index.path())
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let current_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if current_index != base_index || self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        fs::rename(candidate_index.path(), &index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::File::open(
            index
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(IndexGeneration {
            head: self.head()?,
            tree: self.index_tree_from_file(&index)?,
        })
    }

    /// OGS envelope metadata stays materialized, but OGB-2 raw chunks remain
    /// only in Git's object store instead of being copied into the worktree.
    pub(crate) fn configure_store_tree_skip_worktree(
        &self,
        index: &Path,
    ) -> Result<(), RepositoryError> {
        let mut list = self.command();
        list.env("GIT_INDEX_FILE", index)
            .args(["ls-files", "-t", "-z", "--", ".orna/store"]);
        let output = self.run(list)?.stdout;
        let mut skip_paths = Vec::new();
        let mut clear_paths = Vec::new();
        for record in output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            if record.len() < 3 || record[1] != b' ' {
                return Err(RepositoryError::GitOperationFailed);
            }
            let path = std::str::from_utf8(&record[2..])
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path)?;
            let should_skip = Self::is_store_payload_blob_path(path);
            if (record[0] == b'S') == should_skip {
                continue;
            }
            if should_skip {
                skip_paths.push(path.to_owned());
            } else {
                clear_paths.push(path.to_owned());
            }
        }
        self.update_store_skip_worktree_paths(index, false, &clear_paths)?;
        self.update_store_skip_worktree_paths(index, true, &skip_paths)
    }

    pub(crate) fn update_store_skip_worktree_paths(
        &self,
        index: &Path,
        skip: bool,
        paths: &[String],
    ) -> Result<(), RepositoryError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut encoded_paths = Vec::new();
        for path in paths {
            encoded_paths.extend_from_slice(path.as_bytes());
            encoded_paths.push(0);
        }
        let mut update = self.command();
        update
            .env("GIT_INDEX_FILE", index)
            .args([
                "update-index",
                if skip {
                    "--skip-worktree"
                } else {
                    "--no-skip-worktree"
                },
                "-z",
                "--stdin",
            ])
            .stdin(Stdio::piped());
        let mut child = update
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .write_all(&encoded_paths)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }
    /// Keeps newly introduced compact files out of ordinary worktree diffs while
    /// their post-ref recovery journal is pending. The worktree bytes remain
    /// checked directly by recovery, and the bits are cleared once reconciliation
    /// succeeds. The caller holds the publisher's Git index lock.
    pub(crate) fn set_compact_recovery_skip_worktree_locked(
        &self,
        journal: &PublicationJournal,
        skip: bool,
    ) -> Result<(), RepositoryError> {
        if journal.compact_manifest().is_none() {
            return Ok(());
        }

        let mut encoded_paths = Vec::new();
        for entry in journal.entries() {
            if entry.expected().is_none() && entry.next().is_some() {
                encoded_paths
                    .extend_from_slice(entry.path.as_path().as_os_str().as_encoded_bytes());
                encoded_paths.push(0);
            }
        }
        if encoded_paths.is_empty() {
            return Ok(());
        }

        self.ensure_atomic_index_install_supported()?;
        let index = self.git_path("index")?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let parent = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let candidate_index = StagedFile::create(
            parent.join(format!(".orna-index-recovery-{}", Uuid::new_v4())),
            &base_index,
        )?;
        let mut update = self.command();
        update
            .env("GIT_INDEX_FILE", candidate_index.path())
            .args([
                "update-index",
                if skip {
                    "--skip-worktree"
                } else {
                    "--no-skip-worktree"
                },
                "-z",
                "--stdin",
            ])
            .stdin(Stdio::piped());
        let mut child = update
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .write_all(&encoded_paths)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        fs::File::open(candidate_index.path())
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let current_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if current_index != base_index || self.head()?.as_ref() != Some(journal.new_head()) {
            return Err(RepositoryError::StaleHead);
        }
        fs::rename(candidate_index.path(), &index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }
    /// Enumerates regular blobs in a committed native-store tree.
    /// Tree entries are validated before they can affect the worktree.
    pub(crate) fn native_store_tree_blobs(
        &self,
        treeish: &str,
    ) -> Result<BTreeMap<String, String>, RepositoryError> {
        self.objects().check_expansion(treeish, ".orna/store")?;
        let mut list = self.command();
        list.env("GIT_NO_LAZY_FETCH", "1").args([
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            treeish,
            "--",
            ".orna/store",
        ]);
        let output = self.run(list)?.stdout;
        let mut entries = BTreeMap::new();
        for record in output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let record = GitTreeRecord::parse(record).ok_or(RepositoryError::GitOperationFailed)?;
            if record.mode != b"100644" || record.kind != b"blob" {
                return Err(RepositoryError::GitOperationFailed);
            }
            let path = String::from_utf8(record.path.to_vec())
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path.as_str())?;
            let object = String::from_utf8(record.object.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            if entries.insert(path, object).is_some() {
                return Err(RepositoryError::GitOperationFailed);
            }
        }
        Ok(entries)
    }

    pub(crate) fn store_skip_worktree_paths(
        &self,
        index: &Path,
    ) -> Result<BTreeSet<String>, RepositoryError> {
        let mut list = self.command();
        list.env("GIT_INDEX_FILE", index)
            .args(["ls-files", "-t", "-z", "--", ".orna/store"]);
        let output = self.run(list)?.stdout;
        let mut paths = BTreeSet::new();
        for record in output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            if record.len() < 3 || record[0] != b'S' || record[1] != b' ' {
                continue;
            }
            let path = std::str::from_utf8(&record[2..])
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path)?;
            paths.insert(path.to_owned());
        }
        Ok(paths)
    }
    /// In an OGS envelope, regular leaves named data are bounded node
    /// metadata. Other regular leaves are OGB-2 chunks named by their Git OID.
    pub(crate) fn is_store_payload_blob_path(path: &str) -> bool {
        path.strip_prefix(".orna/store/")
            .and_then(|relative| relative.rsplit('/').next())
            .is_some_and(|name| name != "data")
    }

    pub(crate) fn reconcile_store_payload_worktree_file(
        &self,
        path: &ManagedPath,
        object: &str,
        blobs: &mut StoreBlobBatch<'_>,
        target: &Path,
        quarantine: &Path,
        install: bool,
    ) -> Result<(), RepositoryError> {
        let current = self.read_managed_file(target)?;
        let displaced = self.read_managed_file(quarantine)?;
        if current.is_some() && displaced.is_some() {
            return Err(RepositoryError::ManagedContentConflict);
        }
        let expected = if current.is_some() || displaced.is_some() {
            Some(blobs.read(object)?)
        } else {
            None
        };
        if current
            .as_deref()
            .is_some_and(|bytes| Some(bytes) != expected.as_deref())
            || displaced
                .as_deref()
                .is_some_and(|bytes| Some(bytes) != expected.as_deref())
        {
            return Err(RepositoryError::ManagedContentConflict);
        }
        if !install {
            return Ok(());
        }
        let Some(expected) = expected.as_deref() else {
            return Ok(());
        };
        if current.is_some() {
            self.materialize_managed_file_impl_with_quarantine_locked(
                path,
                Some(expected),
                None,
                Some(quarantine),
                None,
                None,
            )
        } else {
            let parent = target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?;
            self.remove_verified_quarantine(parent, quarantine, target, Some(expected), None)
        }
    }
    pub(crate) fn reconcile_store_worktree_transition(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        install: bool,
    ) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        let skipped_paths = self.store_skip_worktree_paths(&index)?;
        let old = expected_index
            .tree()
            .map(|tree| self.native_store_tree_blobs(tree.as_str()))
            .transpose()?
            .unwrap_or_default();
        let new = self.native_store_tree_blobs(candidate.commit().as_str())?;
        let mut blobs = StoreBlobBatch::new(self);
        let paths = old.keys().chain(new.keys()).collect::<BTreeSet<_>>();
        for relative in paths {
            let relative = relative.as_str();
            let old_object = old.get(relative);
            let new_object = new.get(relative);
            let unchanged = old_object == new_object;
            let skipped = skipped_paths.contains(relative);
            let is_payload = Self::is_store_payload_blob_path(relative);
            if unchanged && !skipped && !is_payload {
                // Node metadata is not rewritten here, but an edit to it is still an external change
                // and must be refused rather than left in place (ORNA-PUB-010).
                let path = ManagedPath::new(relative)?;
                let (target, _) = self.store_reconciliation_paths(&path, candidate)?;
                if let Some(current) = self.read_managed_file(&target)? {
                    let object = new_object.ok_or(RepositoryError::GitOperationFailed)?;
                    let next = blobs.read(object)?;
                    if current != next {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                }
                continue;
            }

            let path = ManagedPath::new(relative)?;
            let (target, quarantine) = self.store_reconciliation_paths(&path, candidate)?;
            if is_payload && let Some(object) = new_object {
                self.reconcile_store_payload_worktree_file(
                    &path,
                    object,
                    &mut blobs,
                    &target,
                    &quarantine,
                    install,
                )?;
                continue;
            }
            let current = self.read_managed_file(&target)?;
            // Older publications could leave skipped leaves absent; restore them before
            // clearing those bits so reconciliation does not invent deletions.
            if unchanged {
                let displaced = self.read_managed_file(&quarantine)?;
                let next = new_object
                    .map(|object| blobs.read(object))
                    .transpose()?
                    .ok_or(RepositoryError::GitOperationFailed)?;
                if current
                    .as_deref()
                    .is_some_and(|bytes| bytes != next.as_slice())
                {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                if current.is_none() || displaced.is_some() {
                    if displaced
                        .as_deref()
                        .is_some_and(|bytes| bytes != next.as_slice())
                    {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    if install {
                        let expected = displaced.as_deref().map(|_| next.as_slice());
                        self.materialize_store_worktree_file(
                            &path,
                            expected,
                            Some(&next),
                            candidate,
                        )?;
                    }
                }
                continue;
            }

            let old_bytes = old_object.map(|object| blobs.read(object)).transpose()?;
            let new_bytes = new_object.map(|object| blobs.read(object)).transpose()?;
            let displaced = self.read_managed_file(&quarantine)?;
            if displaced.as_deref().is_some_and(|bytes| {
                Some(bytes) != old_bytes.as_deref()
                    || current
                        .as_deref()
                        .is_some_and(|current_bytes| Some(current_bytes) != new_bytes.as_deref())
            }) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            let matches_old = current.as_deref() == old_bytes.as_deref();
            let matches_new = current.as_deref() == new_bytes.as_deref();
            let recovering_quarantine = !matches_old
                && !matches_new
                && current.is_none()
                && old_bytes.is_some()
                && new_bytes.is_some()
                && displaced.as_deref() == old_bytes.as_deref();
            let skipped_missing = skipped && current.is_none() && displaced.is_none();
            if !matches_old && !matches_new && !recovering_quarantine && !skipped_missing {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if install {
                let expected = if skipped_missing {
                    None
                } else {
                    old_bytes.as_deref()
                };
                self.materialize_store_worktree_file(
                    &path,
                    expected,
                    new_bytes.as_deref(),
                    candidate,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn store_reconciliation_paths(
        &self,
        path: &ManagedPath,
        candidate: &PrivateCommit,
    ) -> Result<(PathBuf, PathBuf), RepositoryError> {
        let target = self.managed_target(path)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(RepositoryError::UnsafeManagedPath)?;
        let quarantine = parent.join(format!(
            ".orna-store-reconcile-{}-{name}",
            candidate.commit().as_str()
        ));
        Ok((target, quarantine))
    }

    pub(crate) fn materialize_store_worktree_file(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let (target, quarantine) = self.store_reconciliation_paths(path, candidate)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let current = self.read_managed_file(&target)?;
        let displaced = self.read_managed_file(&quarantine)?;

        if let Some(displaced) = displaced.as_deref() {
            if Some(displaced) != expected {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if current.as_deref() == next {
                return self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    expected,
                    next,
                );
            }
            if current.is_none() {
                if let Some(next) = next {
                    self.materialize_managed_file_impl_with_quarantine_locked(
                        path,
                        None,
                        Some(next),
                        None,
                        None,
                        None,
                    )?;
                }
                return self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    expected,
                    next,
                );
            }
            return Err(RepositoryError::ManagedContentConflict);
        }

        if current.as_deref() == next {
            return Ok(());
        }
        if current.as_deref() != expected {
            return Err(RepositoryError::ManagedContentConflict);
        }
        let quarantine_path = expected.map(|_| quarantine.as_path());
        self.materialize_managed_file_impl_with_quarantine_locked(
            path,
            expected,
            next,
            quarantine_path,
            None,
            None,
        )
    }
}

/// Reads store blobs through one `git cat-file --batch` process, started on the
/// first read, so reconciling a store starts the same Git processes whatever its
/// size. Each read honours replace refs and refuses lazy fetches, as
/// [`Repository::git_bytes_no_lazy_fetch`] does.
pub(crate) struct StoreBlobBatch<'a> {
    repository: &'a Repository,
    child: Option<BatchChild>,
}

impl<'a> StoreBlobBatch<'a> {
    pub(crate) fn new(repository: &'a Repository) -> Self {
        Self {
            repository,
            child: None,
        }
    }

    /// Returns the content of blob `object`. A failed read drops the process, so
    /// a broken response stream is never read again.
    pub(crate) fn read(&mut self, object: &str) -> Result<Vec<u8>, RepositoryError> {
        let mut child = match self.child.take() {
            Some(child) => child,
            None => BatchChild::start(self.repository)?,
        };
        let bytes = child.read_blob(object)?;
        self.child = Some(child);
        Ok(bytes)
    }
}

/// The live `cat-file --batch` process behind a [`StoreBlobBatch`].
struct BatchChild {
    child: Child,
    requests: ChildStdin,
    responses: BufReader<ChildStdout>,
}

impl BatchChild {
    fn start(repository: &Repository) -> Result<Self, RepositoryError> {
        let mut command = repository.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let (Some(requests), Some(responses)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RepositoryError::GitOperationFailed);
        };
        Ok(Self {
            child,
            requests,
            responses: BufReader::new(responses),
        })
    }

    fn read_blob(&mut self, object: &str) -> Result<Vec<u8>, RepositoryError> {
        let failed = |_: std::io::Error| RepositoryError::GitOperationFailed;
        writeln!(self.requests, "{object}").map_err(failed)?;
        let mut header = Vec::new();
        self.responses
            .read_until(b'\n', &mut header)
            .map_err(failed)?;
        // A present blob answers `<oid> blob <size>`, then its content and a
        // newline. A missing, ambiguous or non-blob object answers differently.
        let header = header
            .strip_suffix(b"\n")
            .ok_or(RepositoryError::GitOperationFailed)?;
        let header =
            std::str::from_utf8(header).map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut fields = header.split(' ');
        let (Some(oid), Some("blob"), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(RepositoryError::GitOperationFailed);
        };
        if oid != object {
            return Err(RepositoryError::GitOperationFailed);
        }
        let size = size
            .parse::<usize>()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut bytes = vec![0_u8; size];
        self.responses.read_exact(&mut bytes).map_err(failed)?;
        let mut terminator = [0_u8; 1];
        self.responses.read_exact(&mut terminator).map_err(failed)?;
        if terminator != *b"\n" {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(bytes)
    }
}

impl Drop for BatchChild {
    fn drop(&mut self) {
        // Git may be blocked writing a response nobody will read, so kill it
        // rather than wait for a clean exit. A failed kill means it has exited.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod batch_child_tests {
    use super::*;
    use std::path::Path;

    /// Dropping a batch process kills and reaps it: its PID is gone from `/proc`
    /// when `drop` returns, so no zombie is left behind.
    #[test]
    fn dropping_a_batch_child_reaps_its_git_process() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());
        let repository = Repository::discover(root.path()).unwrap();
        let batch = BatchChild::start(&repository).unwrap();
        let proc_entry = Path::new("/proc").join(batch.child.id().to_string());
        assert!(proc_entry.exists());
        drop(batch);
        assert!(!proc_entry.exists());
    }
}
