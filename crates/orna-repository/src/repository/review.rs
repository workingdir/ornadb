use crate::*;

impl Repository {
    /// Reviews the exact index candidate using identity, generation, snapshot, and
    /// format observations supplied by the durable runtime and this repository.
    pub fn review_current_candidate(
        &self,
        capture: &orna_foundation::CwdCapture,
    ) -> Result<ReviewedCandidateReview, RepositoryError> {
        let digits = capture.generation().to_u64_digits().1;
        if digits.len() > 1 {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        let generation = digits.first().copied().unwrap_or(0);
        let repository_format = match self.head()? {
            Some(head) => self
                .open_pinned_format_context(head.as_str())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .repository_format(),
            // An unborn branch has no committed format: its first commit takes
            // the format its staged metadata declares.
            None => self.staged_repository_format()?,
        };
        let descriptor = capture
            .snapshot()
            .encode()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.review_staged_candidate(
            Uuid::from_bytes(capture.database_id()),
            Uuid::from_bytes(capture.runtime_id()),
            generation,
            repository_format,
            RuntimeGeneration::new(generation),
            descriptor,
        )
    }
    /// Captures the actual index tree and CWD observation without mutating
    /// the ordinary index, worktree, or branch.
    pub(crate) fn review_staged_candidate(
        &self,
        database_id: Uuid,
        runtime_id: Uuid,
        pending_generation: u64,
        repository_format: RepositoryFormat,
        runtime: RuntimeGeneration,
        cwd_snapshot_descriptor: Vec<u8>,
    ) -> Result<ReviewedCandidateReview, RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        let cwd = self.cwd_generation_locked(runtime)?;
        let index_tree = cwd
            .index
            .tree()
            .cloned()
            .ok_or(RepositoryError::UnbornHead)?;
        let index = self.git_path("index")?;
        let index_bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if self.index_tree_from_file(&index)? != Some(index_tree.clone())
            || self.head()? != cwd.head().cloned()
            || self.current_branch()? != cwd.branch().map(str::to_owned)
        {
            return Err(RepositoryError::StaleCwd);
        }
        self.objects().check_expansion(index_tree.as_str(), "")?;
        if let Some(head) = cwd.head() {
            self.objects().check_expansion(head.as_str(), "")?;
        }
        let mut path_diff = self.command();
        match cwd.head() {
            Some(head) => path_diff
                .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "-z"])
                .arg(head.as_str())
                .arg(index_tree.as_str()),
            // On an unborn branch every staged path is new.
            None => path_diff
                .args(["ls-tree", "-r", "--name-only", "-z", "--full-tree"])
                .arg(index_tree.as_str()),
        };
        let candidate_paths = self
            .run(path_diff)?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                String::from_utf8(path.to_vec()).map_err(|_| RepositoryError::UnsafeManagedPath)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut review = ReviewedCandidateReview {
            database_id,
            runtime_id,
            pending_generation,
            repository_format,
            head_ref: cwd.branch().map(str::to_owned),
            cwd,
            cwd_snapshot_descriptor,
            candidate_paths,
            index_bytes_digest: Sha256::digest(&index_bytes).into(),
            index_tree,
            token: [0; 32],
        };
        review.token = reviewed_candidate_token(&review);
        Ok(review)
    }

    pub(crate) fn validate_candidate_assertions(
        &self,
        gate: &dyn CandidateAssertionGate,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        gate.validate(self, candidate)
            .map_err(RepositoryError::CandidateAssertionRefused)
    }

    pub fn commit_reviewed_candidate(
        &self,
        review: &ReviewedCandidateReview,
        message: &str,
        gate: &dyn CandidateAssertionGate,
    ) -> Result<GitCommitRef, RepositoryError> {
        self.commit_reviewed_candidate_with_stage_hook(review, message, gate, |_| {})
    }

    /// Records the reviewed result of a publication whose batch reached
    /// `WorktreeReconciled`, then marks its runtime intent consumed. Reviewed
    /// publication and migration activation both complete through this path.
    pub(crate) fn complete_reviewed_publication(
        &self,
        runtime_intent_id: [u8; 16],
        result_path: &Path,
        record: &str,
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        fs::write(result_path, record).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.mark_runtime_complete(runtime_intent_id, journal)
    }

    pub(crate) fn resume_reviewed_candidate_publication(
        &self,
        review: &ReviewedCandidateReview,
        runtime_intent_id: [u8; 16],
        result_path: &Path,
        intent_hex: &str,
        message_digest_hex: &str,
    ) -> Result<Option<GitCommitRef>, RepositoryError> {
        let Some(mut journal) = self.read_publication_journal()? else {
            return Ok(None);
        };
        if journal.runtime_intent_id() != Some(runtime_intent_id) {
            return Ok(None);
        }
        if journal.base_index_tree() != Some(&review.index_tree) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        if matches!(
            journal.stage(),
            PublicationJournalStage::Prepared
                | PublicationJournalStage::RefAdvanced
                | PublicationJournalStage::IndexReconciled
        ) {
            match self.recover_publication() {
                Ok(_) | Err(RepositoryError::RuntimeCompletionRequired) => {}
                Err(error) => return Err(error),
            }
            let Some(recovered) = self.read_publication_journal()? else {
                return Ok(None);
            };
            journal = recovered;
        }
        let commit = journal.new_head().clone();
        let record = format!("{intent_hex}\n{message_digest_hex}\n{}", commit.as_str());
        match journal.stage() {
            PublicationJournalStage::WorktreeReconciled => {
                self.complete_reviewed_publication(
                    runtime_intent_id,
                    result_path,
                    &record,
                    &mut journal,
                )?;
            }
            PublicationJournalStage::RuntimeCompleted | PublicationJournalStage::Complete => {
                fs::write(result_path, &record)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                self.recover_publication()?;
            }
            _ => return Err(RepositoryError::RuntimeCompletionRequired),
        }
        Ok(Some(commit))
    }
    pub(crate) fn commit_reviewed_candidate_with_stage_hook(
        &self,
        review: &ReviewedCandidateReview,
        message: &str,
        gate: &dyn CandidateAssertionGate,
        mut on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<GitCommitRef, RepositoryError> {
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        if review.token != reviewed_candidate_token(review) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let runtime_intent_id = reviewed_candidate_runtime_intent_id(review, message);
        let intent_hex = runtime_intent_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let message_digest_hex = Sha256::digest(message.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let result_path = self
            .runtime
            .root()
            .join(format!("reviewed-result-{intent_hex}"));
        let recorded_commit = if let Ok(recorded) = fs::read_to_string(&result_path) {
            let mut fields = recorded.lines();
            let Some(recorded_intent) = fields.next() else {
                return Err(RepositoryError::LocalStateUnavailable);
            };
            let Some(recorded_message_digest) = fields.next() else {
                return Err(RepositoryError::LocalStateUnavailable);
            };
            let Some(recorded_commit) = fields.next() else {
                return Err(RepositoryError::StaleReviewedCandidate);
            };
            if fields.next().is_some() {
                return Err(RepositoryError::LocalStateUnavailable);
            }
            if recorded_intent != intent_hex || recorded_message_digest != message_digest_hex {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
            Some(self.snapshot_from_native_oid(recorded_commit.trim().to_owned())?)
        } else {
            None
        };
        if let Some(recovered) = self.resume_reviewed_candidate_publication(
            review,
            runtime_intent_id,
            &result_path,
            &intent_hex,
            &message_digest_hex,
        )? {
            if recorded_commit
                .as_ref()
                .is_some_and(|recorded| recorded != &recovered)
            {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
            return Ok(recovered);
        }
        if let Some(recorded) = recorded_commit {
            return Ok(recorded);
        }
        if review.candidate_paths.is_empty() {
            return Err(RepositoryError::SysGitNothingToCommit);
        }
        let actual_cwd = self.cwd_generation(review.cwd.runtime())?;
        let index = self.git_path("index")?;
        let index_bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if actual_cwd != review.cwd
            || self.current_branch()? != review.head_ref
            || Sha256::digest(&index_bytes).as_slice() != review.index_bytes_digest
        {
            return Err(RepositoryError::StaleReviewedCandidate);
        }

        // 16-administration `commit`: a missing identity fails before any commit
        // object exists. The table assertions are evaluated over the serialized
        // candidate before its journal or any ref names it, so a refused
        // candidate leaves at most an unreachable commit object and no ref.
        self.require_configured_commit_identity()?;
        if review.repository_format != RepositoryFormat::Format3 {
            return Err(RepositoryError::InvalidCommitCandidate);
        }
        self.validate_format3_index_tree(&review.index_tree)
            .map_err(commit_gate_refusal)?;
        let Some(old_head) = review.cwd.head().cloned() else {
            return self.commit_reviewed_root_candidate(
                review,
                message,
                gate,
                &result_path,
                &intent_hex,
                &message_digest_hex,
            );
        };
        let mut commit_tree = self.configured_identity_command();
        commit_tree
            .args([
                "commit-tree",
                review.index_tree.as_str(),
                "-p",
                old_head.as_str(),
                "-m",
            ])
            .arg(message);
        // Like every other publication candidate, the commit stays unreachable
        // until the branch compare-and-set names it; no private ref anchors it
        // (ORNA-PUB-005, ORNA-GIT-008).
        let candidate = PrivateCommit {
            tree: review.index_tree.clone(),
            commit: self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?,
        };
        self.validate_candidate_assertions(gate, &candidate)?;
        let mut diff = self.command();
        diff.args(["diff", "--no-renames", "--name-status", "-z"])
            .arg(old_head.as_str())
            .arg(candidate.commit().as_str());
        let changed_paths = self.run(diff)?.stdout;
        let mut fields = changed_paths
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let mut entries = Vec::new();
        while let Some(status) = fields.next() {
            let raw_path = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let path =
                std::str::from_utf8(raw_path).map_err(|_| RepositoryError::UnsafeManagedPath)?;
            // The staged store is bound and reconciled as the one
            // `.orna/store` tree entry, like a publication's store root: its
            // node files are not managed files, and payload chunks are never
            // materialized in the worktree. A path the candidate deletes is
            // already absent from the reviewed index, and the worktree keeps
            // its unstaged state. Neither is a journal entry.
            if path.starts_with(".orna/store/") || status == b"D" {
                continue;
            }
            let path = ManagedPath::new(path)?;
            let target = self.managed_target(&path)?;
            let current = self.read_managed_file(&target)?;
            entries.push(PublicationJournalEntry::new(path, current.clone(), current));
        }
        if entries.is_empty() {
            // A candidate that only changes the store or deletes paths keeps
            // the unchanged metadata file as its durable journal anchor.
            let marker = ManagedPath::new(".orna/database.orna")?;
            let current = self.read_managed_file(&self.managed_target(&marker)?)?;
            entries.push(PublicationJournalEntry::new(
                marker,
                current.clone(),
                current,
            ));
        }
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            review.index_tree.clone(),
            runtime_intent_id,
            entries,
        )?;
        self.publish_candidate_impl_with_migration(
            &review.cwd.index,
            &candidate,
            &mut journal,
            false,
            Some(review),
            &mut on_stage,
        )?;
        on_stage(PublicationJournalStage::WorktreeReconciled);
        fs::write(
            &result_path,
            format!(
                "{intent_hex}\n{message_digest_hex}\n{}",
                candidate.commit().as_str()
            ),
        )
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.mark_runtime_complete(runtime_intent_id, &mut journal)?;
        Ok(candidate.commit().clone())
    }

    /// Commits a reviewed candidate on an unborn branch. "The first actual
    /// commit creates that branch ref" (16-administration, unborn branches):
    /// the commit names exactly the reviewed index tree with no parent, and
    /// the symbolic branch moves from absent to it by compare-and-set. The
    /// ordinary index already equals the new commit's tree and the worktree
    /// keeps its unstaged differences, so there is no index or worktree
    /// reconciliation to journal. Like every other ref advance, the commit
    /// and all of its objects are flushed before the ref names them, and the
    /// ref is flushed before the result is recorded (ORNA-PUB-005).
    pub(crate) fn commit_reviewed_root_candidate(
        &self,
        review: &ReviewedCandidateReview,
        message: &str,
        gate: &dyn CandidateAssertionGate,
        result_path: &Path,
        intent_hex: &str,
        message_digest_hex: &str,
    ) -> Result<GitCommitRef, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        // The index is not rewritten, so Git's index lock is not taken: a
        // concurrent `git add` after this check stays staged against the new
        // commit, and the branch itself moves only by compare-and-set.
        self.verify_reviewed_candidate_locked(review)?;
        if self.head()?.is_some() {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let reference = self.symbolic_head_ref()?;

        let mut commit_tree = self.configured_identity_command();
        commit_tree
            .args(["commit-tree", review.index_tree.as_str(), "-m"])
            .arg(message);
        let candidate = PrivateCommit {
            tree: review.index_tree.clone(),
            commit: self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?,
        };
        let context = self
            .open_private_candidate_format_context(&candidate)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        if context.repository_format() != review.repository_format
            || context.repository_format() != RepositoryFormat::Format3
            || context.validate_schema_root().is_err()
            || context.validate_store_root().is_err()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.validate_candidate_assertions(gate, &candidate)?;
        // PUB-3 step 3: complete objects and their durability barriers
        // precede the visible ref. A ref backend whose refs cannot be
        // flushed is refused before the branch is created.
        native_graph::require_files_ref_backend(self).map_err(RepositoryError::Graph)?;
        self.sync_candidate_objects(None, candidate.commit())?;
        let absent = "0".repeat(self.native_object_id_length()?);
        let mut create = self.command();
        create.args([
            "update-ref",
            reference.as_str(),
            candidate.commit().as_str(),
            absent.as_str(),
        ]);
        self.run(create)?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        fs::write(
            result_path,
            format!(
                "{intent_hex}\n{message_digest_hex}\n{}",
                candidate.commit().as_str()
            ),
        )
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(candidate.commit().clone())
    }

    /// A Git command that records only a configured identity: Git refuses to
    /// guess an author or committer from `EMAIL`, the account name or the
    /// host name (`user.useConfigOnly`). Configuration and Git's explicit
    /// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` variables still apply.
    pub(crate) fn configured_identity_command(&self) -> Command {
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true"]);
        command
    }

    /// A Git command that records a private staged-identity carrier (the
    /// unreachable commit `capture_staged_candidate` names for loading). A
    /// carrier is never ref-advanced, so it is written on Orna's behalf and
    /// never reads the ambient identity. A machine or CI runner without
    /// `user.name`/`user.email` therefore builds it the same way. The identity
    /// is fixed here and never written to Git config; timestamps remain Git's.
    /// Publication candidates ref-advance the user's branch and keep
    /// `command()`, so they record the configured identity (ORNA-PUB-010).
    pub(crate) fn private_commit_command(&self) -> Command {
        const NAME: &str = "Orna Internal";
        const EMAIL: &str = "orna-internal@localhost.invalid";
        let mut command = self.command();
        command
            .env("GIT_AUTHOR_NAME", NAME)
            .env("GIT_AUTHOR_EMAIL", EMAIL)
            .env("GIT_COMMITTER_NAME", NAME)
            .env("GIT_COMMITTER_EMAIL", EMAIL);
        command
    }

    /// Requires the author and committer identity a reviewed commit records
    /// to be configured. 16-administration: "Default author information comes
    /// from configured Git identity; missing identity fails before creating a
    /// commit." Both identities are resolved by Git itself, exactly as the
    /// following `commit-tree` resolves them.
    pub fn require_configured_commit_identity(&self) -> Result<(), RepositoryError> {
        for identity in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            let mut command = self.configured_identity_command();
            command
                .args(["var", identity])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let status = command
                .status()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if !status.success() {
                return Err(RepositoryError::MissingGitIdentity);
            }
        }
        Ok(())
    }

    pub(crate) fn verify_reviewed_candidate_locked(
        &self,
        review: &ReviewedCandidateReview,
    ) -> Result<(), RepositoryError> {
        if review.token != reviewed_candidate_token(review) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let actual = self.cwd_generation_locked(review.cwd.runtime())?;
        let index = self.git_path("index")?;
        let bytes = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if actual != review.cwd
            || self.current_branch()? != review.head_ref
            || actual.index.tree() != Some(&review.index_tree)
            || Sha256::digest(bytes).as_slice() != review.index_bytes_digest
        {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        Ok(())
    }
}
