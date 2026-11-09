use crate::*;

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
            objects: std::sync::Arc::new(object_reader::ObjectReader::new(worktree.clone())),
            worktree,
            runtime: RuntimePaths { root: runtime },
            coordination_lock_wait: COORDINATION_LOCK_WAIT,
            section_cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Overrides how long publication waits for the coordination lock before
    /// reporting `RepositoryBusy`. Production uses `COORDINATION_LOCK_WAIT`;
    /// tests use this to keep contended-lock cases short or to allow a slow
    /// winner on a loaded runner.
    #[doc(hidden)]
    #[must_use]
    pub fn with_coordination_lock_wait(mut self, wait: std::time::Duration) -> Self {
        self.coordination_lock_wait = wait;
        self
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

    /// The content of the single index entry for `path`, or `None` when the
    /// path is not in the index. A conflicted path (several stages) is an error.
    pub fn index_file_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, RepositoryError> {
        let mut command = self.command();
        command
            .args(["--literal-pathspecs", "ls-files", "--stage", "-z", "--"])
            .arg(path);
        let output = self.run(command)?.stdout;
        let records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        match records.as_slice() {
            [] => Ok(None),
            [record] => {
                let record =
                    GitListRecord::parse(record).ok_or(RepositoryError::GitOperationFailed)?;
                let oid = record.field(1).ok_or(RepositoryError::GitOperationFailed)?;
                let oid =
                    std::str::from_utf8(oid).map_err(|_| RepositoryError::GitOperationFailed)?;
                Ok(Some(self.git_bytes(["cat-file", "blob", oid])?))
            }
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// The content of `path` in `HEAD`, or `None` when `HEAD` has no such path
    /// or the repository has no commit yet.
    pub fn head_file_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, RepositoryError> {
        if self.head()?.is_none() {
            return Ok(None);
        }
        let output = self.git_bytes(["ls-tree", "-z", "HEAD", "--", path])?;
        let Some(record) = output
            .split(|byte| *byte == 0)
            .find(|record| !record.is_empty())
        else {
            return Ok(None);
        };
        let record = GitListRecord::parse(record).ok_or(RepositoryError::GitOperationFailed)?;
        let oid = record.field(2).ok_or(RepositoryError::GitOperationFailed)?;
        let oid = std::str::from_utf8(oid).map_err(|_| RepositoryError::GitOperationFailed)?;
        Ok(Some(self.git_bytes(["cat-file", "blob", oid])?))
    }

    /// Lists the newest commits reachable from `HEAD`, newest first.
    ///
    /// The count and output size are bounded before Git output is accumulated.
    /// An unborn `HEAD` produces an empty history.
    pub fn committed_history(
        &self,
        max_commits: usize,
    ) -> Result<Vec<CommittedLogEntry>, RepositoryError> {
        if max_commits == 0 || self.head()?.is_none() {
            return Ok(Vec::new());
        }

        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["log", "-z"])
            .arg(format!("--max-count={max_commits}"))
            .arg("--format=format:%H%x00%cI%x00%an%x00%s%x00")
            .arg("HEAD");
        let output = run_bounded_listing(command)?;

        let object_id_length = self.native_object_id_length()?;
        let mut fields = output.split(|byte| *byte == 0);
        let mut entries = Vec::new();
        while let Some(object_id) = fields.next() {
            let committed_at = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let author = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let subject = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let record_end = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            if !record_end.is_empty() {
                return Err(RepositoryError::GitOperationFailed);
            }
            let object_id = String::from_utf8(object_id.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            entries.push(CommittedLogEntry {
                commit: GitCommitRef::from_verified_commit(object_id, object_id_length)?,
                committed_at: String::from_utf8_lossy(committed_at).into_owned(),
                author: String::from_utf8_lossy(author).into_owned(),
                subject: String::from_utf8_lossy(subject).into_owned(),
            });
        }
        if fields.any(|field| !field.is_empty()) || entries.len() > max_commits {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
    }

    /// Lists up to `max_commits` commits reachable from `start`, newest first,
    /// each paired with its root tree, author and recorded migration
    /// coordinate.
    ///
    /// This is the revision walk a legacy (format-1/2) row reader needs
    /// (ORNA-UPGRADE-002, ORNA-ROW-013). A format-1/2 commit has no native row
    /// map, so its rows cannot be walked through the native graph's
    /// `list_row_revisions`; the walk starts from a commit name (`HEAD` or a
    /// pinned commit id) and reads commit headers only, so no blob is opened.
    /// The count and output size are bounded before Git output is accumulated.
    pub fn committed_revision_snapshots(
        &self,
        start: &str,
        max_commits: usize,
    ) -> Result<Vec<CommittedRevisionSnapshot>, RepositoryError> {
        if max_commits > MAX_COMMITTED_REVISION_WALK {
            return Err(RepositoryError::SourceTreeLimit);
        }
        if max_commits == 0 {
            return Ok(Vec::new());
        }

        let object_id_length = self.native_object_id_length()?;
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "log",
            &format!("--max-count={max_commits}"),
            "--format=%H %T%x09%ct%x09%an <%ae>%x09%(trailers:key=Orna-Migration,valueonly)",
            start,
        ]);
        let output = run_bounded_listing(command)?;
        let text = std::str::from_utf8(&output).map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut snapshots = Vec::new();
        for line in text.lines() {
            let Some((header, rest)) = line.split_once('\t') else {
                return Err(RepositoryError::GitOperationFailed);
            };
            let Some((committed_at, rest)) = rest.split_once('\t') else {
                return Err(RepositoryError::GitOperationFailed);
            };
            let committed_at = committed_at
                .parse::<u64>()
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            let (author, migration) = split_committed_revision_record(rest);
            let mut fields = header.split(' ');
            let (Some(commit), Some(tree), None) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(RepositoryError::GitOperationFailed);
            };
            snapshots.push(CommittedRevisionSnapshot {
                commit: GitCommitRef::from_verified_commit(commit.to_owned(), object_id_length)?,
                tree: tree.to_owned(),
                author: author.to_owned(),
                committed_at,
                migration: migration.map(str::to_owned),
            });
        }
        if snapshots.len() > max_commits {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(snapshots)
    }

    /// Reports whether `path` is a tracked leaf of `commit`'s tree, without
    /// reading its bytes.
    ///
    /// A legacy row unit is a committed file, so its presence in a revision is
    /// exactly what a legacy row's history reports (ORNA-UPGRADE-002). The
    /// lookup is one `ls-tree` of that literal path, and the pathspec is not
    /// interpreted as a pattern, so a key component that spells a glob
    /// character names only that component.
    pub fn committed_path_present(
        &self,
        commit: &GitCommitRef,
        path: &str,
    ) -> Result<bool, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--full-tree",
                commit.as_str(),
                "--",
            ])
            .arg(path);
        let output = self.run(command)?.stdout;
        Ok(output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .filter_map(|record| {
                let record = GitListRecord::parse(record)?;
                Some((record.header(), record.name()))
            })
            .any(|(metadata, named)| named == path.as_bytes() && !metadata.starts_with(b"040000 ")))
    }

    /// Lists local branches in Git's ref order, bounded by `max_branches`.
    pub fn local_branches(
        &self,
        max_branches: usize,
    ) -> Result<Vec<CommittedBranch>, RepositoryError> {
        if max_branches == 0 {
            return Ok(Vec::new());
        }

        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["for-each-ref", "--count"])
            .arg(max_branches.to_string())
            .arg("--format=%(refname:strip=2)%00%(objectname)")
            .arg("refs/heads");
        let output = run_bounded_listing(command)?;

        let object_id_length = self.native_object_id_length()?;
        let mut branches = Vec::new();
        for record in output
            .split(|byte| *byte == b'\n')
            .filter(|record| !record.is_empty())
        {
            let separator = record
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(RepositoryError::GitOperationFailed)?;
            let (name, object_id_with_separator) = record.split_at(separator);
            let object_id = &object_id_with_separator[1..];
            let name = String::from_utf8(name.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            if !valid_branch_name(&name) {
                return Err(RepositoryError::GitOperationFailed);
            }
            let object_id = String::from_utf8(object_id.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            branches.push(CommittedBranch {
                name,
                commit: GitCommitRef::from_verified_commit(object_id, object_id_length)?,
            });
        }
        if branches.len() > max_branches {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(branches)
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
        self.build_private_commit_inner(expected_head, changes, None, message, None)
    }

    /// Builds a private format-3 candidate from a validated native store root.
    /// Format-3 rows are persisted only in the store tree, so this operation
    /// cannot accept loose-row file projections.
    pub fn build_private_commit_with_store_root(
        &self,
        expected_head: &GitCommitRef,
        graph: &NativeGraphContext,
        store_root: &NativeOid,
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        self.build_private_commit_inner(
            expected_head,
            &[],
            Some(StoreBinding::Validated {
                graph,
                root: store_root,
            }),
            message,
            None,
        )
    }

    /// Builds a private commit whose author and committer time is `timestamp`
    /// (seconds since the Unix epoch, recorded at `+0000`) instead of the wall
    /// clock.
    ///
    /// A caller that must reproduce the *same* commit object from the same
    /// inputs cannot use the current time: two runs a second apart would record
    /// different objects for an identical tree, parent and message. Naming the
    /// time explicitly makes the object a function of its content alone.
    /// Nothing else about the commit changes; the identity still comes from the
    /// repository's configured or pinned author and committer.
    pub fn build_private_commit_at(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
        message: &str,
        timestamp: i64,
    ) -> Result<PrivateCommit, RepositoryError> {
        self.build_private_commit_inner(expected_head, changes, None, message, Some(timestamp))
    }

    pub(crate) fn build_private_commit_inner(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
        store_projection: Option<StoreBinding<'_>>,
        message: &str,
        commit_time: Option<i64>,
    ) -> Result<PrivateCommit, RepositoryError> {
        if changes.is_empty() && store_projection.is_none() {
            return Err(RepositoryError::NoManagedPaths);
        }
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        if let Some(StoreBinding::Validated { graph, root }) = store_projection
            && root.algorithm() != graph.algorithm()
        {
            return Err(RepositoryError::Graph(GraphError::InvalidOidWidth {
                expected: graph.algorithm().width(),
                actual: root.as_bytes().len(),
            }));
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
        self.objects().check_expansion(expected_head.as_str(), "")?;
        self.reject_final_loose_path_collisions(expected_head, changes)?;

        let index = self.private_index()?;
        let mut read_tree = self.command();
        read_tree
            .env("GIT_INDEX_FILE", index.path())
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
                update.env("GIT_INDEX_FILE", index.path()).args([
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &cacheinfo,
                ]);
                self.run(update)?;
            } else {
                let mut update = self.command();
                update
                    .env("GIT_INDEX_FILE", index.path())
                    .args(["update-index", "--force-remove", "--"])
                    .arg(change.path.as_path());
                self.run(update)?;
            }
        }
        match store_projection {
            None => {}
            Some(StoreBinding::Validated { graph, root }) => {
                graph
                    .pin_store_subtree_in_index(index.path(), root)
                    .map_err(RepositoryError::Graph)?;
            }
            Some(StoreBinding::Decoded(root)) => {
                bind_tree_entry_in_index(
                    || self.command(),
                    index.path(),
                    STORE_PATH,
                    "040000",
                    &root.to_hex(),
                )?;
            }
        }
        self.commit_private_index(index.path(), expected_head, message, commit_time)
    }

    /// Builds one publication candidate `N = H + P` from the captured base and
    /// the frozen native store root (ORNA-PUB-010).
    ///
    /// The candidate is built entirely in a private index seeded from
    /// `expected_head`; the ordinary index and worktree are never read, so no
    /// human staged or unstaged content can reach the candidate. The frozen
    /// batch contributes its encoded store root, staged at the fixed
    /// `.orna/store` position the format-3 reader resolves. Because the
    /// argument is an encoded OGS-1 store tree rather than a set of managed
    /// file bytes, a caller cannot smuggle an unrelated edit into a
    /// publication commit.
    ///
    /// Every written object stays unreachable until the returned commit names
    /// it, and the commit is a direct child of the captured `HEAD`. The
    /// symbolic branch and ordinary index are untouched; publication advances
    /// them later under its own lock and compare-and-set.
    pub fn build_frozen_publication_candidate(
        &self,
        expected_head: &GitCommitRef,
        graph: &NativeGraphContext,
        store_root: &NativeOid,
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        self.objects().check_expansion(expected_head.as_str(), "")?;
        if store_root.algorithm() != graph.algorithm() {
            return Err(RepositoryError::Graph(GraphError::InvalidOidWidth {
                expected: graph.algorithm().width(),
                actual: store_root.as_bytes().len(),
            }));
        }
        let index = self.private_index()?;
        let mut read_tree = self.command();
        read_tree
            .env("GIT_INDEX_FILE", index.path())
            .args(["read-tree", expected_head.as_str()]);
        self.run(read_tree)?;

        // The store root must be a real decodable OGS-1 tree, so a raw
        // object ID cannot be planted at the store position.
        graph
            .pin_store_subtree_in_index(index.path(), store_root)
            .map_err(RepositoryError::Graph)?;
        self.commit_private_index(index.path(), expected_head, message, None)
    }

    /// A fresh private index path under the runtime locks directory, for
    /// building a candidate commit without touching the ordinary index. Git
    /// creates the file itself; the returned guard removes it.
    pub(crate) fn private_index(&self) -> Result<StagedFile, RepositoryError> {
        self.runtime.ensure_exists()?;
        fs::create_dir_all(self.runtime.locks())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let path = self.runtime.locks().join(format!(
            "private-index-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        // `create_new` proves the name is unused before Git claims it.
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(StagedFile::reserve(path))
    }

    /// Writes the private `index` as a tree and commits it as a child of
    /// `expected_head`. `commit_time` pins the time of both identities when
    /// the object must be reproducible. The commit is refused if `HEAD` moved
    /// while it was built.
    pub(crate) fn commit_private_index(
        &self,
        index: &Path,
        expected_head: &GitCommitRef,
        message: &str,
        commit_time: Option<i64>,
    ) -> Result<PrivateCommit, RepositoryError> {
        let mut write_tree = self.command();
        write_tree.env("GIT_INDEX_FILE", index).args(["write-tree"]);
        let tree = self.index_tree_from_native_oid(trim_output(&self.run(write_tree)?.stdout))?;

        let mut commit_tree = self.command();
        commit_tree.args([
            "commit-tree",
            tree.as_str(),
            "-p",
            expected_head.as_str(),
            "-m",
        ]);
        if let Some(timestamp) = commit_time {
            // One offset for both identities keeps the object independent
            // of the host's time zone as well as its clock.
            let date = format!("@{timestamp} +0000");
            commit_tree.env("GIT_AUTHOR_DATE", &date);
            commit_tree.env("GIT_COMMITTER_DATE", &date);
        }
        commit_tree.arg(message);
        let commit = self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?;
        if self.head()?.as_ref() != Some(expected_head) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(PrivateCommit { tree, commit })
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
        self.advance_current_ref_bound(expected_head, candidate, None)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(())
    }

    pub(crate) fn symbolic_head_ref(&self) -> Result<String, RepositoryError> {
        let reference = self
            .git(["symbolic-ref", "--quiet", "HEAD"])
            .map_err(|_| RepositoryError::DetachedHead)?;
        if !reference.starts_with("refs/heads/") {
            return Err(RepositoryError::DetachedHead);
        }
        Ok(reference)
    }

    /// Resolves the branch a publication may advance. `HEAD` must still name
    /// `expected_head` through an attached `refs/heads/*` symbolic ref, and a
    /// branch bound by the journal must be that ref (ORNA-PUB-009).
    pub(crate) fn advance_target_ref(
        &self,
        expected_head: &GitCommitRef,
        expected_ref: Option<&str>,
    ) -> Result<String, RepositoryError> {
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        let reference = self.symbolic_head_ref()?;
        if expected_ref.is_some_and(|expected| expected != reference) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(reference)
    }

    pub(crate) fn advance_current_ref_bound(
        &self,
        expected_head: &GitCommitRef,
        candidate: &PrivateCommit,
        expected_ref: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let reference = self.advance_target_ref(expected_head, expected_ref)?;
        // ORNA-PUB-005: every object the candidate adds is durable before the
        // ref names it, and the ref is durable before a journal stage or the
        // runtime's tail consumption relies on it. A ref backend whose refs
        // cannot be flushed is refused before the ref moves.
        native_graph::require_files_ref_backend(self).map_err(RepositoryError::Graph)?;
        self.sync_candidate_objects(Some(expected_head), candidate.commit())?;
        self.advance_ref_transaction(&reference, expected_head, candidate.commit())?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(())
    }

    /// Flushes every object `candidate` adds over `base`: the commit, its root
    /// tree, and each tree and blob on a changed path. Without a base (a root
    /// commit) every tree and blob of the candidate is flushed. `write-tree`
    /// and `commit-tree` leave their loose objects unflushed under Git's
    /// default `core.fsync`, so without this a crash after the ref names them
    /// could lose or truncate them.
    pub(crate) fn sync_candidate_objects(
        &self,
        base: Option<&GitCommitRef>,
        candidate: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let algorithm = match self.native_object_id_length()? {
            40 => GitHashAlgorithm::Sha1,
            64 => GitHashAlgorithm::Sha256,
            _ => return Err(RepositoryError::UnsupportedObjectFormat),
        };
        let oid = |hex: &str| NativeOid::from_hex(algorithm, hex).map_err(RepositoryError::Graph);
        let root_tree = format!("{}^{{tree}}", candidate.as_str());
        let mut objects = BTreeSet::from([
            oid(candidate.as_str())?,
            oid(&self.git(["rev-parse", "--verify", &root_tree])?)?,
        ]);
        self.objects().check_expansion(candidate.as_str(), "")?;
        if let Some(base) = base {
            self.objects().check_expansion(base.as_str(), "")?;
        }
        let changes = match base {
            Some(base) => self.git_bytes_no_lazy_fetch([
                "diff-tree",
                "-r",
                "-t",
                "-z",
                "--no-renames",
                base.as_str(),
                candidate.as_str(),
            ])?,
            // `--root` reports every path of a parentless commit as added.
            None => self.git_bytes_no_lazy_fetch([
                "diff-tree",
                "--root",
                "--no-commit-id",
                "-r",
                "-t",
                "-z",
                "--no-renames",
                candidate.as_str(),
            ])?,
        };
        // Raw `-z` output alternates `:<old mode> <new mode> <old> <new> <status>`
        // records with their paths.
        let mut fields = changes
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        while let Some(record) = fields.next() {
            fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let record =
                std::str::from_utf8(record).map_err(|_| RepositoryError::GitOperationFailed)?;
            let mut parts = record
                .strip_prefix(':')
                .ok_or(RepositoryError::GitOperationFailed)?
                .split(' ');
            let new_mode = parts.nth(1).ok_or(RepositoryError::GitOperationFailed)?;
            let new_object = parts.nth(1).ok_or(RepositoryError::GitOperationFailed)?;
            // A deletion names no object, and a gitlink names a commit in
            // another repository.
            if new_mode != "000000" && new_mode != "160000" {
                objects.insert(oid(new_object)?);
            }
        }
        let objects_dir = self.git_path("objects")?;
        native_graph::sync_object_files(&objects_dir, &objects, || Ok(()))
            .map_err(RepositoryError::Graph)
    }

    /// Persists the ref `HEAD` resolves through: its branch when attached,
    /// otherwise `HEAD` itself.
    pub(crate) fn sync_head_ref(&self) -> Result<(), RepositoryError> {
        let reference = self.git(["rev-parse", "--symbolic-full-name", "HEAD"])?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)
    }

    /// Whether `HEAD` names `commit` or descends from it. A commit that is not
    /// in the object database is contained by no ref.
    pub fn head_contains_commit(&self, commit: &GitCommitRef) -> Result<bool, RepositoryError> {
        let Some(head) = self.head()? else {
            return Ok(false);
        };
        if &head == commit {
            return Ok(true);
        }
        if self
            .commit_optional(&format!("{}^{{commit}}", commit.as_str()))?
            .is_none()
        {
            return Ok(false);
        }
        let mut command = self.command();
        command.args([
            "merge-base",
            "--is-ancestor",
            commit.as_str(),
            head.as_str(),
        ]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// Whether a local branch names `commit` or descends from it. A commit
    /// that is not in the object database is contained by no branch.
    pub(crate) fn branch_contains_commit(
        &self,
        commit: &GitCommitRef,
    ) -> Result<bool, RepositoryError> {
        if self
            .commit_optional(&format!("{}^{{commit}}", commit.as_str()))?
            .is_none()
        {
            return Ok(false);
        }
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
            commit.as_str(),
            "refs/heads/",
        ]);
        Ok(self
            .run(command)?
            .stdout
            .iter()
            .any(|byte| !byte.is_ascii_whitespace()))
    }
}
