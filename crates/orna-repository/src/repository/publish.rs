use crate::*;

impl Repository {
    /// Publishes a prepared candidate through the ref, index and worktree
    /// boundaries. A refusal before the ref CAS leaves no journal behind; an
    /// error after the CAS leaves the journal available for recovery.
    pub fn publish_candidate(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
    ) -> Result<IndexGeneration, RepositoryError> {
        self.publish_candidate_impl_with_migration(
            expected_index,
            candidate,
            journal,
            false,
            None,
            |_| {},
        )
    }
    /// Executes publication and observes each durable journal stage, from
    /// the Prepared intent through the reconciled index.
    pub fn publish_candidate_with_stage_hook(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        self.publish_candidate_impl_with_migration(
            expected_index,
            candidate,
            journal,
            false,
            None,
            on_stage,
        )
    }

    /// Explicitly publishes a caller-built format-3 migration from a captured
    /// format-1/2 snapshot through the shared durable PUB-3 journal.
    ///
    /// This method never converts or relabels legacy data. The caller must
    /// provide the complete private candidate and prepared publication
    /// journal. The candidate must be a direct child of the captured legacy
    /// commit and admit as format 3 with valid source and store roots. Until
    /// the runtime owner completes the journal's intent, the old commit and
    /// runtime tail remain available for recovery.
    pub fn publish_legacy_format_migration(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
    ) -> Result<IndexGeneration, RepositoryError> {
        if journal.migration_continuity().is_none()
            || journal.migration_annotation_defaults().is_none()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        let old_head = expected_index
            .head()
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if journal.old_head() != old_head || journal.new_head() != candidate.commit() {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let legacy_snapshot = self
            .pin_snapshot(old_head.as_str())
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        let legacy_context = self
            .open_format_context_at(&legacy_snapshot)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        if !matches!(
            legacy_context.repository_format(),
            RepositoryFormat::Legacy1 | RepositoryFormat::Legacy2
        ) {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let candidate_context = self
            .open_private_candidate_format_context(candidate)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        let candidate_format3 = candidate_context.repository_format() == RepositoryFormat::Format3;
        let candidate_schema_valid = candidate_context.validate_schema_root().is_ok();
        let candidate_store_valid = candidate_context.validate_store_root().is_ok();
        if !candidate_format3 || !candidate_schema_valid || !candidate_store_valid {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let parent = self.run({
            let mut command = self.command();
            command.args([
                "rev-list",
                "--parents",
                "--no-walk",
                candidate.commit().as_str(),
            ]);
            command
        })?;
        let parent_line = trim_output(&parent.stdout);
        let parents = parent_line
            .split_ascii_whitespace()
            .skip(1)
            .collect::<Vec<_>>();
        if parents.as_slice() != [old_head.as_str()] {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        self.publish_candidate_impl_with_migration(
            expected_index,
            candidate,
            journal,
            true,
            None,
            |_| {},
        )
    }

    /// The common PUB-1 ref/index/worktree machinery. A compact journal is
    /// refused: its retired writer has no completion path in this build.
    /// Pins adopted by an attempt that no journal names are released before
    /// the error is returned (ORNA-GIT-010).
    pub(crate) fn publish_candidate_impl_with_migration(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        permit_legacy_migration: bool,
        review: Option<&ReviewedCandidateReview>,
        on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        let published = self.publish_candidate_body(
            expected_index,
            candidate,
            journal,
            permit_legacy_migration,
            review,
            on_stage,
        );
        if published.is_err() {
            // The refusal is the caller's error. If the release itself fails,
            // the pins remain for the next recovery to sweep.
            let _ = self.release_unjournaled_publication_pins();
        }
        published
    }

    /// The body of [`Self::publish_candidate_impl_with_migration`]. It takes
    /// the coordination lock itself and releases it before returning.
    pub(crate) fn publish_candidate_body(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        permit_legacy_migration: bool,
        review: Option<&ReviewedCandidateReview>,
        mut on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        if journal.migration_continuity().is_some() != permit_legacy_migration
            || journal.migration_annotation_defaults().is_some() != permit_legacy_migration
            || (permit_legacy_migration && journal.compact_manifest().is_some())
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        if migration_copy::journal_lists_migration_record(journal)? {
            return Err(RepositoryError::LegacyMigrationCopy);
        }
        if journal.stage() != PublicationJournalStage::Prepared
            || journal.runtime_intent_id().is_none()
            || expected_index.head() != Some(journal.old_head())
            || journal.new_head() != candidate.commit()
            || journal.base_index_tree() != expected_index.tree()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if journal.compact_manifest().is_some() {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        let _coordination_lock = self.acquire_coordination_lock_for_publication()?;
        if let Some(review) = review {
            self.verify_reviewed_candidate_locked(review)?;
        }
        self.require_no_pending_checkout_recovery_locked()?;
        self.ensure_no_git_operation_in_progress()?;
        // A retained PUB-1 journal is the sole recovery authority for its
        // candidate. Never replace it with another publisher's prepared
        // journal: doing so would discard the first candidate's exact
        // ref/index/worktree evidence before recovery can reconcile it.
        let resumes_retained_journal = match self.read_publication_journal_locked()? {
            // Compact publication persists this exact prepared journal before
            // it invokes the common PUB-1 boundary, so an exact retry is the
            // same recovery operation, not a second publisher. Any differing
            // journal remains fenced rather than replacing its evidence.
            Some(retained) if retained != *journal => {
                // Ordering (ORNA-PUB-009 before recovery signal): a head that has
                // already moved past this publisher's expected head is an ordinary
                // lost CAS -> StaleHead (retriable). The retained batch is untouched.
                // Only an unmoved head with a foreign retained journal means the
                // earlier publication has not advanced the ref: PublicationPending
                // (ORNA-PUB-006 recovery required).
                if self.head()?.as_ref() != Some(journal.old_head()) {
                    return Err(RepositoryError::StaleHead);
                }
                return Err(RepositoryError::PublicationPending);
            }
            retained => retained.is_some(),
        };
        let paths = self.publication_index_paths(candidate, journal)?;
        self.ensure_atomic_index_install_supported()?;
        let index = self.git_path("index")?;
        let base_index = self.capture_index_for_expected(expected_index, &index)?;
        self.adopt_publication_pins_locked(journal, PublicationPinPolicy::RequireObjects)?;
        let lock_binding = journal.lock_binding()?;
        // A publisher killed after taking Git's lock leaves an Orna-marked lock bound
        // to this journal. Reclaim exactly that lock, and only once its owner is
        // proven dead; a live or differently bound lock still refuses below.
        GitIndexLock::reclaim_abandoned(index.with_extension("lock"), lock_binding)?;
        // Own Git's writer lock before persisting a prepared intent so lock contention
        // cannot strand a timestamped journal that blocks a fresh retry. This closes the
        // post-ref/pre-index race where an ordinary Git writer could publish
        // the captured, stale index.
        let git_lock = GitIndexLock::acquire_owned(index.with_extension("lock"), lock_binding)?;
        // ORNA-PUB-006/PUB-016: the Orna-marked index lock now exists but no
        // Prepared journal does; recovery must reclaim it without a binding.
        crate::publication_crash_barrier("index-lock");
        // PUB-3 step 7: every precondition of the ref CAS is revalidated under
        // the lock before the Prepared journal exists. A refusal here leaves
        // no record that could fence the pending batch (ORNA-PUB-006).
        self.ensure_no_git_operation_in_progress()?;
        let selected_ref = journal
            .compact_manifest()
            .map(|witness| witness.selected_ref());
        self.advance_target_ref(journal.old_head(), selected_ref)?;
        if let Some(review) = review {
            let bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if Sha256::digest(bytes).as_slice() != review.index_bytes_digest
                || self.head()? != review.cwd.head().cloned()
                || self.current_branch()? != review.head_ref
                || self.worktree_state()? != *review.cwd.worktree()
            {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
        }
        // A managed file may have changed after preparation but before the
        // publication boundary.  Refuse that known conflict before advancing
        // the ref; the later worktree check still protects the unavoidable
        // race with an editor that writes during publication.
        for entry in journal.entries() {
            let target = self.managed_target(&entry.path)?;
            let current = self.read_managed_file(&target)?;
            if current.as_deref() == entry.expected() || current.as_deref() == entry.next() {
                continue;
            }
            // A retired legacy file the migration deletes must still hold the
            // committed bytes it was read from. A local edit to it is not an
            // unrelated change the migration may replace, so it is named.
            if permit_legacy_migration && entry.next().is_none() {
                return Err(RepositoryError::LegacyPathChanged(
                    entry.path().as_path().display().to_string(),
                ));
            }
            return Err(RepositoryError::ManagedContentConflict);
        }
        self.reconcile_store_worktree_transition(expected_index, candidate, false)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::Prepared);
        // PUB-3 step 7: the reconciled index is built from `base_index`, so
        // the ordinary index must still hold exactly those bytes when the ref
        // moves. A write that landed after capture, including one just before
        // this attempt took Git's lock, is refused here rather than replaced
        // by an index that stages the inverse of the batch (ORNA-PUB-011,
        // ORNA-PUB-017).
        let advanced = self
            .require_index_unchanged(expected_index, &index, &base_index)
            .and_then(|()| {
                self.advance_current_ref_bound(journal.old_head(), candidate, selected_ref)
            });
        if let Err(error) = advanced {
            // After a refused or lost CAS, a candidate that neither HEAD nor
            // any branch contains shows that the journal this attempt wrote
            // names nothing that was published: discard it and keep the batch
            // pending. A journal retained from an earlier attempt stays with
            // recovery.
            if !resumes_retained_journal
                && !self.head_contains_commit(candidate.commit())?
                && !self.branch_contains_commit(candidate.commit())?
            {
                self.clear_publication_journal_locked()?;
            }
            return Err(error);
        }
        // Gate D boundary: the ref CAS has succeeded but the RefAdvanced
        // journal stage is not yet durable. Recovery sees a Prepared journal
        // whose HEAD already names the candidate (ORNA-PUB-009, ORNA-PUB-005).
        crate::publication_crash_barrier("ref-cas-pre-journal");

        journal.advance(PublicationJournalStage::RefAdvanced)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::RefAdvanced);
        let reconciled = self.reconcile_published_index_locked(
            expected_index,
            candidate,
            &paths,
            &base_index,
            &git_lock,
        )?;
        // Gate D boundary: the reconciled index is installed but its stage is
        // not yet durable, so recovery must recognise the installed index.
        crate::publication_crash_barrier("index-install");
        journal.advance(PublicationJournalStage::IndexReconciled)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::IndexReconciled);
        for index in 0..journal.entries().len() {
            self.materialize_publication_entry_locked(journal, index)?;
        }
        // Gate D boundary: every managed entry has been materialised but the
        // WorktreeReconciled stage is not yet recorded (ORNA-PUB-016).
        crate::publication_crash_barrier("worktree");

        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal_locked(journal)?;
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
        if journal.compact_manifest().is_some()
            || journal.stage() != PublicationJournalStage::WorktreeReconciled
            || journal.runtime_intent_id() != Some(runtime_intent_id)
        {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        let _coordination_lock = self.acquire_coordination_lock()?;
        self.require_no_pending_checkout_recovery_locked()?;
        // ORNA-PUB-009 and recovery row "Ref no longer H or N": a normal Git
        // writer may advance HEAD past the candidate after reconciliation.
        // Ancestry proves the batch is present, so completion proceeds without
        // rewriting the ref, index or worktree. Any HEAD that does not contain
        // the candidate is a recovery conflict.
        if !self.head_contains_commit(journal.new_head())? {
            return Err(RepositoryError::StaleHead);
        }
        self.verify_index_at_candidate(journal)?;
        journal.advance(PublicationJournalStage::RuntimeCompleted)?;
        self.write_publication_journal_locked(journal)?;
        journal.advance(PublicationJournalStage::Complete)?;
        self.write_publication_journal_locked(journal)?;
        self.clear_publication_journal_locked()
    }

    /// Resumes a persisted publication after a process interruption. The
    /// journal is advanced only after each observable boundary is verified;
    /// unexpected ref, index, or worktree state remains a typed conflict. When
    /// `HEAD` does not contain a prepared publication's candidate, the journal
    /// is discarded and its batch stays pending in the runtime.
    pub fn recover_publication(&self) -> Result<Option<IndexGeneration>, RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        self.require_no_pending_checkout_recovery_locked()?;
        let Some(mut journal) = self.read_publication_journal_locked()? else {
            self.sweep_publication_pins_locked(&[])?;
            self.reclaim_orphan_publication_index_lock()?;
            return Ok(None);
        };
        if journal.stage() == PublicationJournalStage::Prepared
            && migration_copy::journal_lists_migration_record(&journal)?
        {
            return Err(RepositoryError::LegacyMigrationCopy);
        }
        self.adopt_publication_pins_locked(&journal, PublicationPinPolicy::AcceptLostObjects)?;
        self.reclaim_abandoned_publication_index_lock(&journal)?;
        let base_tree = journal
            .base_index_tree()
            .cloned()
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let expected_index = IndexGeneration {
            head: Some(journal.old_head().clone()),
            tree: Some(base_tree),
        };
        // Recovery row "Ref still at H": Orna changes the ordinary index and
        // worktree only after RefAdvanced is durable. While neither HEAD nor
        // any branch contains the candidate, a Prepared publication journal
        // therefore records nothing that was published. Drop it and keep the
        // batch pending in the runtime, whether or not the unreferenced
        // candidate still exists and whatever the user staged since
        // (ORNA-PUB-006, ORNA-PUB-012). A candidate that a branch holds while
        // HEAD does not may already be published there, so the journal stays
        // as a recovery conflict (row "Ref no longer H or N"). Compact and
        // legacy-migration journals carry evidence their publishers resume
        // from, so they keep their own boundaries below.
        if journal.stage() == PublicationJournalStage::Prepared
            && journal.compact_manifest().is_none()
            && journal.migration_continuity().is_none()
            && !self.head_contains_commit(journal.new_head())?
        {
            if self.branch_contains_commit(journal.new_head())? {
                return Err(RepositoryError::StaleHead);
            }
            self.clear_publication_journal_locked()?;
            return Ok(Some(self.index_generation()?));
        }
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let candidate_commit = self.commit_optional(&commit_expression)?;
        if journal.stage() == PublicationJournalStage::Prepared
            && candidate_commit.is_none()
            && self.head()?.as_ref() == Some(journal.old_head())
        {
            let actual = self.index_generation()?;
            if actual == expected_index {
                self.clear_publication_journal_locked()?;
                return Ok(Some(actual));
            }
        }
        let candidate = self.candidate_from_journal_with_commit(
            &journal,
            candidate_commit.ok_or(RepositoryError::InvalidPublicationJournal)?,
        )?;

        if journal.stage() == PublicationJournalStage::Prepared
            && self.head()?.as_ref() == Some(journal.old_head())
        {
            let actual = self.index_generation()?;
            if actual != expected_index {
                return Err(RepositoryError::StaleIndex {
                    expected: expected_index,
                    actual,
                });
            }
            return Err(RepositoryError::PublicationPending);
        }

        if matches!(
            journal.stage(),
            PublicationJournalStage::Prepared
                | PublicationJournalStage::RefAdvanced
                | PublicationJournalStage::IndexReconciled
        ) {
            if self.head()?.as_ref() != Some(journal.new_head()) {
                return Err(RepositoryError::StaleHead);
            }
            let index = self.git_path("index")?;
            let git_lock =
                GitIndexLock::acquire_owned(index.with_extension("lock"), journal.lock_binding()?)?;

            if journal.stage() == PublicationJournalStage::Prepared {
                // The publisher may have stopped between its CAS and the
                // barriers after it, so the candidate and the ref must be
                // durable before RefAdvanced is recorded (ORNA-PUB-005).
                self.sync_candidate_objects(Some(journal.old_head()), journal.new_head())?;
                self.sync_head_ref()?;
                journal.advance(PublicationJournalStage::RefAdvanced)?;
                self.write_publication_journal_locked(&journal)?;
            }
            let mut store_verified = false;
            if journal.stage() == PublicationJournalStage::RefAdvanced {
                self.reconcile_store_worktree_transition(&expected_index, &candidate, false)?;
                store_verified = true;
                let current = IndexGeneration {
                    head: self.head()?,
                    tree: self.index_tree_from_file(&index)?,
                };
                let _reconciled = if current.tree() == expected_index.tree() {
                    let paths = self.publication_index_paths(&candidate, &journal)?;
                    let base_index =
                        fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                    self.reconcile_published_index_locked(
                        &expected_index,
                        &candidate,
                        &paths,
                        &base_index,
                        &git_lock,
                    )?
                } else if self.index_entries_match_candidate(&candidate, &journal)? {
                    let paths = self.publication_index_paths(&candidate, &journal)?;
                    let base_index =
                        fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                    self.reconcile_published_index_locked(
                        &expected_index,
                        &candidate,
                        &paths,
                        &base_index,
                        &git_lock,
                    )?
                } else {
                    return Err(RepositoryError::StaleIndex {
                        expected: expected_index,
                        actual: current,
                    });
                };
                journal.advance(PublicationJournalStage::IndexReconciled)?;
                self.write_publication_journal_locked(&journal)?;
            }
            if journal.stage() == PublicationJournalStage::IndexReconciled {
                // The store worktree was reconciled before IndexReconciled. An edit to a store path made since
                // then differs from the recorded candidate state and must be preserved as a conflict, not
                // overwritten by materialisation (recovery table row 6; ORNA-PUB-010).
                if !store_verified {
                    self.reconcile_store_worktree_transition(&expected_index, &candidate, false)?;
                }
                self.recover_publication_worktree_locked(&mut journal)?;
            }
            drop(git_lock);
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        if journal.stage() == PublicationJournalStage::WorktreeReconciled {
            if !self.head_contains_commit(journal.new_head())? {
                return Err(RepositoryError::StaleHead);
            }
            self.verify_reconciled_index(&journal)?;
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        if matches!(
            journal.stage(),
            PublicationJournalStage::RuntimeCompleted | PublicationJournalStage::Complete
        ) && !self.head_contains_commit(journal.new_head())?
        {
            return Err(RepositoryError::StaleHead);
        }
        self.verify_reconciled_index(&journal)?;
        if journal.stage() == PublicationJournalStage::RuntimeCompleted {
            journal.advance(PublicationJournalStage::Complete)?;
            self.write_publication_journal_locked(&journal)?;
        }
        if journal.stage() == PublicationJournalStage::Complete {
            self.clear_publication_journal_locked()?;
            return Ok(Some(self.index_generation()?));
        }
        Err(RepositoryError::InvalidPublicationJournal)
    }

    /// Verifies, under `index.lock`, that the ordinary index still holds the
    /// managed entries the reconciled candidate recorded. Finalization changes
    /// neither ref, index nor worktree, so an index that moved since
    /// reconciliation is a typed conflict that keeps the journal (ORNA-PUB-011,
    /// ORNA-PUB-017; recovery row "Index or affected path differs from both
    /// recorded states").
    pub fn verify_reconciled_index(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let candidate = self.candidate_from_journal(journal)?;
        self.verify_index_matches_candidate(&candidate, journal)
    }

    /// Index-only form of `verify_reconciled_index` for finalizing a journal
    /// whose worktree may hold unstaged edits the candidate does not carry
    /// (reviewed candidates, ORNA-PUB-017). It checks the candidate's identity
    /// and that the index still holds the candidate's entries; it does not
    /// require the journal's recorded worktree contents to equal the commit.
    pub(crate) fn verify_index_at_candidate(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let commit = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let candidate = self.candidate_identity_from_journal(journal, commit)?;
        self.verify_index_matches_candidate(&candidate, journal)
    }

    pub(crate) fn verify_index_matches_candidate(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        let _git_lock =
            GitIndexLock::acquire_owned(index.with_extension("lock"), journal.lock_binding()?)?;
        if self.index_entries_match_candidate(candidate, journal)? {
            return Ok(());
        }
        Err(RepositoryError::StaleIndex {
            expected: IndexGeneration {
                head: Some(journal.new_head().clone()),
                tree: Some(candidate.tree().clone()),
            },
            actual: self.index_generation_while_locked()?,
        })
    }

    pub(crate) fn candidate_from_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<PrivateCommit, RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let commit = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        self.candidate_from_journal_with_commit(journal, commit)
    }

    pub(crate) fn candidate_from_journal_with_commit(
        &self,
        journal: &PublicationJournal,
        commit: GitCommitRef,
    ) -> Result<PrivateCommit, RepositoryError> {
        let candidate = self.candidate_identity_from_journal(journal, commit)?;
        if !self.candidate_entries_match_journal(&candidate, journal)? {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if journal.compact_manifest().is_some() {
            self.verify_compact_publication_binding(journal, &candidate)?;
        }
        Ok(candidate)
    }

    /// The private H + P identity of the journal's candidate: the commit is
    /// the journal's new head, its only parent is the recorded old head, and
    /// its tree is that commit's tree. Entry contents are checked separately
    /// by `candidate_from_journal_with_commit`.
    pub(crate) fn candidate_identity_from_journal(
        &self,
        journal: &PublicationJournal,
        commit: GitCommitRef,
    ) -> Result<PrivateCommit, RepositoryError> {
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
        Ok(PrivateCommit { tree, commit })
    }

    pub(crate) fn recover_publication_worktree_locked(
        &self,
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        self.set_compact_recovery_skip_worktree_locked(journal, true)?;
        for index in 0..journal.entries().len() {
            self.materialize_publication_entry_locked(journal, index)?;
        }
        self.set_compact_recovery_skip_worktree_locked(journal, false)?;
        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal_locked(journal)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn materialize_publication_entry(
        &self,
        journal: &mut PublicationJournal,
        index: usize,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.materialize_publication_entry_locked(journal, index)
    }

    pub(crate) fn materialize_publication_entry_locked(
        &self,
        journal: &mut PublicationJournal,
        index: usize,
    ) -> Result<(), RepositoryError> {
        let (path, removes_file) = {
            let entry = journal
                .entries()
                .get(index)
                .ok_or(RepositoryError::InvalidPublicationJournal)?;
            (entry.path().clone(), entry.next().is_none())
        };
        self.materialize_publication_entry_phases_locked(journal, index)?;
        if removes_file {
            self.prune_empty_parents(&self.managed_target(&path)?);
        }
        Ok(())
    }

    /// Removes the directories a deleted managed file left empty, from its
    /// parent up to, never including, the worktree root. `remove_dir` only
    /// removes an empty directory, so a directory another file still uses ends
    /// the walk, and a symlink is never followed.
    pub(crate) fn prune_empty_parents(&self, target: &Path) {
        let mut directory = target.parent();
        while let Some(current) = directory {
            if current == self.worktree.as_path()
                || !current.starts_with(&self.worktree)
                || fs::remove_dir(current).is_err()
            {
                break;
            }
            directory = current.parent();
        }
    }

    /// The error for a managed file whose bytes changed under the publication.
    /// A migration-retired legacy file the migration deletes is named as
    /// `LegacyPathChanged`; any other managed file stays a managed-content
    /// conflict.
    pub(crate) fn managed_content_conflict(
        journal: &PublicationJournal,
        entry: &PublicationJournalEntry,
    ) -> RepositoryError {
        if entry.next().is_none() && journal.migration_continuity().is_some() {
            RepositoryError::LegacyPathChanged(entry.path().as_path().display().to_string())
        } else {
            RepositoryError::ManagedContentConflict
        }
    }

    pub(crate) fn materialize_publication_entry_phases_locked(
        &self,
        journal: &mut PublicationJournal,
        index: usize,
    ) -> Result<(), RepositoryError> {
        let entry = journal
            .entries()
            .get(index)
            .ok_or(RepositoryError::InvalidPublicationJournal)?
            .clone();
        let quarantine_name = journal.quarantine_name(index)?;
        let target = self.managed_target(&entry.path)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.validate_managed_parent(parent)?;
        let quarantine = parent.join(&quarantine_name);
        let quarantine_bytes = self.read_managed_file(&quarantine)?;
        let current = self.read_managed_file(&target)?;
        let phase = entry.materialization().phase();
        let step = publication_materialization_step(
            &entry,
            phase,
            quarantine_bytes.as_deref(),
            current.as_deref(),
        )
        .map_err(|conflict| match conflict {
            PublicationMaterializationConflict::Plain => RepositoryError::ManagedContentConflict,
            PublicationMaterializationConflict::Entry => {
                Self::managed_content_conflict(journal, &entry)
            }
        })?;

        match step {
            PublicationMaterializationStep::Done => return Ok(()),
            PublicationMaterializationStep::PrepareQuarantine => {
                journal.set_materialization(
                    index,
                    PublicationMaterializationPhase::QuarantinePrepared,
                )?;
                self.write_publication_journal_locked(journal)?;
            }
            PublicationMaterializationStep::DiscardQuarantine => {
                self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    entry.expected(),
                    entry.next(),
                )?;
                journal.set_materialization(index, PublicationMaterializationPhase::Clean)?;
                self.write_publication_journal_locked(journal)?;
                return Ok(());
            }
            PublicationMaterializationStep::InstallAbsent => {
                if self.read_managed_file(&target)?.is_some()
                    || self.read_managed_file(&quarantine)?.as_deref() != entry.expected()
                {
                    return Err(Self::managed_content_conflict(journal, &entry));
                }
            }
            PublicationMaterializationStep::InstallNext => {
                self.materialize_managed_file_impl_with_quarantine_locked(
                    &entry.path,
                    None,
                    entry.next(),
                    None,
                    None,
                    None,
                )?;
                if self.read_managed_file(&target)?.as_deref() != entry.next()
                    || self.read_managed_file(&quarantine)?.as_deref() != entry.expected()
                {
                    return Err(Self::managed_content_conflict(journal, &entry));
                }
            }
            PublicationMaterializationStep::AdoptInstalled => {
                journal.set_materialization(index, PublicationMaterializationPhase::Clean)?;
                self.write_publication_journal_locked(journal)?;
                return Ok(());
            }
            PublicationMaterializationStep::Materialize => {}
        }

        if matches!(
            step,
            PublicationMaterializationStep::InstallAbsent
                | PublicationMaterializationStep::InstallNext
        ) {
            journal.set_materialization(index, PublicationMaterializationPhase::Installed)?;
            self.write_publication_journal_locked(journal)?;
            self.remove_verified_quarantine(
                parent,
                &quarantine,
                &target,
                entry.expected(),
                entry.next(),
            )?;
            journal.set_materialization(index, PublicationMaterializationPhase::Clean)?;
            self.write_publication_journal_locked(journal)?;
            return Ok(());
        }

        let mut after_boundary = |phase| {
            journal.set_materialization(index, phase)?;
            self.write_publication_journal_locked(journal)
        };
        self.materialize_managed_file_impl_with_quarantine_locked(
            &entry.path,
            entry.expected(),
            entry.next(),
            Some(&quarantine),
            None,
            Some(&mut after_boundary),
        )?;
        journal.set_materialization(index, PublicationMaterializationPhase::Clean)?;
        self.write_publication_journal_locked(journal)
    }

    pub(crate) fn remove_verified_quarantine(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_with_boundary(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn remove_verified_quarantine_with_boundary(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_inner(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            after_final_verification,
        )
    }

    #[cfg(not(test))]
    pub(crate) fn remove_verified_quarantine_with_boundary(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        _after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_inner(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            None,
        )
    }

    pub(crate) fn remove_verified_quarantine_inner(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        mut after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        if self.read_managed_file(quarantine)?.as_deref() != expected_quarantine
            || self.read_managed_file(target)?.as_deref() != expected_target
        {
            return Err(RepositoryError::ManagedContentConflict);
        }
        self.runtime.ensure_exists()?;
        let cleanup_parent = self.runtime.root().join("publication-cleanup");
        fs::create_dir_all(&cleanup_parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let cleanup = (0..8)
            .map(|_| {
                cleanup_parent.join(format!(
                    ".orna-quarantine-cleanup-{}-{}",
                    std::process::id(),
                    Uuid::new_v4()
                ))
            })
            .find(|path| !path.exists())
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        match Self::rename_path_without_replacement(quarantine, &cleanup) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RepositoryError::PublicationRecoveryConflict);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(RepositoryError::PublicationRecoveryConflict);
            }
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        let moved = self.read_managed_file(&cleanup)?;
        if moved.as_deref() != expected_quarantine {
            let _ = Self::rename_path_without_replacement(&cleanup, quarantine);
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
        if fs::symlink_metadata(quarantine).is_ok()
            || self.read_managed_file(target)?.as_deref() != expected_target
        {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }

        // The cleanup object is now in the private runtime area.  Its final
        // removal cannot act on a worktree pathname, so a replacement at the
        // old quarantine name is preserved.  The hook is test-only and
        // represents that replacement at the final public-path boundary.
        if let Some(hook) = after_final_verification.as_mut() {
            hook()?;
        }
        let public_replacement = fs::symlink_metadata(quarantine).is_ok();
        fs::remove_file(&cleanup).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RepositoryError::PublicationRecoveryConflict
            } else {
                RepositoryError::LocalStateUnavailable
            }
        })?;
        if public_replacement {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }

    /// Captures the affected managed worktree bytes only when the selected
    /// HEAD and ordinary index still match the caller's generation and every
    /// affected path agrees with the committed base. This is the PUB-1
    /// capture gate: staged or unstaged human edits on managed paths are a
    /// conflict, while unrelated Git state remains outside this check.
    pub fn capture_managed_publication_state(
        &self,
        expected_head: &GitCommitRef,
        expected_index: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<Vec<Option<Vec<u8>>>, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        self.ensure_no_git_index_lock()?;
        let actual_head = self.head()?;
        if actual_head.as_ref() != Some(expected_head) {
            return Err(RepositoryError::StaleHead);
        }
        let actual_index = self.index_generation_while_locked()?;
        if &actual_index != expected_index {
            return Err(RepositoryError::StaleIndex {
                expected: expected_index.clone(),
                actual: actual_index,
            });
        }
        self.reject_loose_path_collisions(expected_head, paths)?;

        let mut captured = Vec::with_capacity(paths.len());
        for path in paths {
            let target = self.managed_target(path)?;
            if !self.managed_index_matches_head(expected_head, path)? {
                return Err(RepositoryError::ManagedContentConflict);
            }
            let committed = self.committed_managed_file_bytes(expected_head, path)?;
            let actual = self.read_managed_file(&target)?;
            if actual != committed {
                return Err(RepositoryError::ManagedContentConflict);
            }
            captured.push(actual);
        }
        Ok(captured)
    }

    pub(crate) fn tracked_loose_path_components(
        &self,
        expected_head: &GitCommitRef,
    ) -> Result<Vec<Vec<Vec<u8>>>, RepositoryError> {
        self.objects().check_expansion(expected_head.as_str(), "")?;
        let tracked = self.git_bytes([
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "--full-tree",
            expected_head.as_str(),
            "--",
        ])?;
        let mut tracked_paths = Vec::new();
        for path in tracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let components = path
                .split(|byte| *byte == b'/')
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if !is_loose_path_components(&components) {
                continue;
            }
            tracked_paths.push(components);
        }
        Ok(tracked_paths)
    }

    pub(crate) fn reject_loose_path_collisions(
        &self,
        expected_head: &GitCommitRef,
        paths: &[ManagedPath],
    ) -> Result<(), RepositoryError> {
        let tracked_paths = self.tracked_loose_path_components(expected_head)?;
        let tracked_set = tracked_paths.iter().cloned().collect::<BTreeSet<_>>();
        let planned_paths = paths
            .iter()
            .map(|path| {
                path.as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<Vec<_>>();
        let planned_set = planned_paths.iter().cloned().collect::<BTreeSet<_>>();
        let mut known_components = BTreeMap::<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>::new();
        for components in &tracked_paths {
            if !planned_set.contains(components) {
                record_case_folded_path_components(&mut known_components, components);
            }
        }

        for components in planned_paths {
            if tracked_set.contains(&components) {
                continue;
            }
            if case_folded_path_collides(&known_components, &components) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            record_case_folded_path_components(&mut known_components, &components);
        }
        Ok(())
    }

    pub(crate) fn reject_final_loose_path_collisions(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
    ) -> Result<(), RepositoryError> {
        let planned = changes
            .iter()
            .filter(|change| change.bytes().is_some())
            .map(|change| {
                change
                    .path()
                    .as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<Vec<_>>();
        if planned.is_empty() {
            return Ok(());
        }
        let deleted = changes
            .iter()
            .filter(|change| change.bytes().is_none())
            .map(|change| {
                change
                    .path()
                    .as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<BTreeSet<_>>();
        let mut known_components = BTreeMap::<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>::new();
        for components in self.tracked_loose_path_components(expected_head)? {
            if !deleted.contains(&components) {
                record_case_folded_path_components(&mut known_components, &components);
            }
        }
        for components in planned {
            if case_folded_path_collides(&known_components, &components) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            record_case_folded_path_components(&mut known_components, &components);
        }
        Ok(())
    }
}
