use super::*;

impl WorkspaceFilesystem {
    /// Accepts a dirty document candidate (EDIT-1 steps 1-2). The caller holds
    /// the draft-state lock, so acceptance serializes with writes (ORNA-VFS-008).
    /// A refused candidate is retained before its error is returned.
    pub(crate) fn accept_document(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        inode: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
        opened: &Arc<Mutex<DocumentBaseline>>,
    ) -> Result<(), DocumentCommitError> {
        // A path truncate issued while this writer was open is recorded, not applied to
        // the draft. It is applied before the candidate is judged, so the accepted or
        // refused candidate reflects the cut (ORNA-VFS-009; POSIX arrival order).
        if let Some(size) = draft_state.pending_truncates.get(&inode).copied() {
            save.writer
                .runtime
                .block_on(draft.truncate(size))
                .map_err(|error| {
                    DocumentCommitError::Failed(format!(
                        "candidate could not be cut to {size} bytes: {error:?}"
                    ))
                })?;
            draft_state.pending_truncates.remove(&inode);
        }
        // The handle commits against the row it captured at open and advances only with its
        // own accepted candidate (ORNA-VFS-011), so a row another writer accepted since then
        // fails the compare. A handle pinned to a superseded generation whose draft holds a
        // candidate is refused before the compare: its generation is retired (see
        // `replace_document_generation`). A clean superseded handle has no candidate to refuse,
        // so it reaches the commit, which reports NoCandidate; fsync acknowledges that.
        let (based_on, schema_identity) = {
            let baseline = opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (baseline.row.clone(), baseline.schema_identity.clone())
        };
        let superseded = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_superseded(inode);
        let refuse_superseded = superseded && save.writer.runtime.block_on(draft.has_candidate());
        let outcome = if refuse_superseded {
            Err(DocumentCommitError::Stale)
        } else {
            save.commit_document_against(draft, true, based_on, &schema_identity)
        };
        let accepted_row = match outcome {
            Ok(accepted_row) => accepted_row,
            Err(error) => {
                // NoCandidate: the handle submitted no bytes, so there is no candidate to
                // retain and nothing blocks unmount (ORNA-VFS-009, EDIT-1 step 2).
                if matches!(error, DocumentCommitError::NoCandidate) {
                    return Err(error);
                }
                // A candidate recovery storage cannot keep reports the retention errno
                // and stays unresolved (ORNA-VFS-009).
                let retained =
                    self.refuse_document(draft_state, inode, handle, save, draft, &error);
                return Err(match retained {
                    Some(errno) => DocumentCommitError::Unretained {
                        errno,
                        cause: error.cause(),
                    },
                    None => error,
                });
            }
        };
        if let Some(row) = accepted_row {
            let mut baseline = opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            baseline.row = Some(row);
        }
        let bytes = save
            .writer
            .runtime
            .block_on(draft.candidate_bytes())
            .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A generation superseded while this commit ran keeps the bytes it captured
        // (profiles/vfs-1.md:126-133), so only a live generation takes the accepted row.
        let retired = state.is_superseded(inode);
        if let Some(NodeKind::Document {
            bytes: current,
            save: current_save,
        }) = state
            .nodes
            .get_mut(&inode)
            .map(|node| &mut node.kind)
            .filter(|_| !retired)
        {
            *current = bytes.to_vec();
            // A handle opened before a refresh saves through its own
            // context; new opens now continue from the row it accepted.
            if !Arc::ptr_eq(current_save, save) {
                *current_save = Arc::clone(save);
            }
        }
        // The document shows the accepted row, so a Str over 64 KiB is named by its `.txt`.
        if !retired && let Err(error) = state.project_accepted_document(inode, save) {
            eprintln!("the accepted row document could not be re-projected: {error}");
        }
        let accepted_path = state.display_path(inode);
        drop(state);
        draft_state.take_unresolved(handle);
        draft_state.clear_unaccepted_write(handle);
        // The accepted save supersedes the refusals it replaces (ORNA-VFS-009, ORNA-VFS-024).
        draft_state.supersede_refusals(inode, &accepted_path);
        Ok(())
    }

    /// Commits the pending state of an open handle: a document candidate is
    /// accepted, and a scratch file is persisted to the scratch store.
    pub(crate) fn commit_open_handle(
        &self,
        draft_state: &mut DraftState,
        inode: INodeNo,
        handle: u64,
    ) -> Result<(), DocumentCommitError> {
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle)
            .cloned();
        match open_file {
            Some(OpenFile::Document(save, draft, opened)) => {
                self.accept_document(draft_state, handle, inode.0, &save, &draft, &opened)
            }
            Some(OpenFile::Temporary(save, target, scratch, Some(field)))
                if draft_state.has_blob(handle) =>
            {
                self.commit_blob_candidate(draft_state, handle, &save, &target, &scratch, &field)
            }
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match state.nodes.get(&inode.0).map(|node| &node.kind) {
                    Some(NodeKind::Temporary {
                        identity,
                        baseline,
                        schema_identity,
                        ..
                    }) => save
                        .fsync_temporary(&scratch, identity, schema_identity, baseline.as_deref())
                        .map_err(DocumentCommitError::Failed),
                    // Rename already accepted and removed this scratch node.
                    None => Ok(()),
                    _ => Err(DocumentCommitError::Failed(
                        "open scratch node is unavailable".to_owned(),
                    )),
                }
            }
            Some(OpenFile::Source(_)) => Err(DocumentCommitError::Failed(
                "source module commits go through source_sync".to_owned(),
            )),
            None => Err(DocumentCommitError::Failed(
                "file handle is not open".to_owned(),
            )),
        }
    }

    /// The parent inode and text flag of a candidate node, or `None` when the node is gone and
    /// there is no candidate left to accept.
    fn candidate_node(&self, inode: u64) -> Result<Option<(u64, bool)>, DocumentCommitError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.nodes.get(&inode) {
            Some(node)
                if matches!(
                    node.kind,
                    NodeKind::Blob {
                        candidate: Some(_),
                        ..
                    } | NodeKind::BlobBytes(_)
                        | NodeKind::Text {
                            candidate: Some(_),
                            ..
                        }
                ) =>
            {
                Ok(Some((
                    node.parent,
                    matches!(node.kind, NodeKind::Text { .. }),
                )))
            }
            None => Ok(None),
            Some(_) => Err(DocumentCommitError::Failed(
                "open Blob candidate node is unavailable".to_owned(),
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    /// Accepts a Blob write-open's candidate as the row's replacement (41-vfs.md EDIT-1).
    /// The baseline is the one captured when the handle opened (ORNA-VFS-011), so an accept by a
    /// sibling handle since then fails the row CAS and the candidate is refused. After an accept,
    /// the handle's next candidate starts from the accepted bytes (EDIT-1 step 3).
    pub(crate) fn commit_blob_candidate(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        save: &Arc<RowSaveContext>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &Arc<TemporarySave<RowMapSnapshot>>,
        field: &BlobField,
    ) -> Result<(), DocumentCommitError> {
        let Some(blob) = draft_state.blob(handle) else {
            return Err(DocumentCommitError::Failed(
                "open Blob candidate has no captured baseline".to_owned(),
            ));
        };
        let inode = blob.generation;
        let baseline = blob.baseline.clone();
        let schema_identity = blob.schema_identity.clone();
        let path = blob.path.clone();
        // The node is gone, so there is no candidate left to accept.
        let Some((parent, text_field)) = self.candidate_node(inode)? else {
            return Ok(());
        };
        let accepted = match save.commit_blob_temporary(
            target,
            scratch,
            field,
            baseline,
            schema_identity.clone(),
            true,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                return Err(retain_refused_scratch(
                    draft_state,
                    save,
                    scratch,
                    inode,
                    &path,
                    error,
                ));
            }
        };
        // The accepted image is the one the vfs published; it is not read back from scratch.
        let bytes = accepted.bytes;
        let generation = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace_blob_generation(
                parent,
                OsStr::new(&field.file_name),
                inode,
                Arc::clone(&bytes),
            )
            .map_err(DocumentCommitError::Failed)?;
        if text_field {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Ok(Some(document)) = state.lookup_child(parent, OsStr::new("data.orna"))
                && let Err(error) = state.project_accepted_document(document, save)
            {
                eprintln!("the accepted row document could not be re-projected: {error}");
            }
        }
        // The accepted candidate is no longer draft state (same as an accepted rename).
        draft_state.retained_scratch.remove(&inode);
        draft_state.rejected_scratch.remove(&inode);
        draft_state.supersede_refusals(inode, &path);
        // The next candidate starts from the accepted bytes, and its baseline is the accepted row.
        let accepted_baseline = save
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let next_target = save
            .temporary_target(Some(field))
            .map_err(DocumentCommitError::Failed)?;
        let next_scratch = Arc::new(
            save.writer
                .runtime
                .block_on(next_target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
        );
        // The next candidate is seeded one bounded chunk at a time; the accepted bytes are never
        // copied whole.
        seed_next_candidate(save, &bytes, &next_scratch)?;
        let next_path = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.track_open(generation);
            state.track_release(inode);
            state.display_path(generation)
        };
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle,
                OpenFile::Temporary(
                    Arc::clone(save),
                    next_target,
                    next_scratch,
                    Some(field.clone()),
                ),
            );
        draft_state.set_blob(
            handle,
            BlobHandle {
                generation,
                baseline: accepted_baseline,
                path: next_path,
                schema_identity,
            },
        );
        Ok(())
    }

    /// Keeps the candidate of a document released while dirty (EDIT-1 step 5).
    /// Release only cleans up, so the candidate is retained and reported, never
    /// accepted.
    pub(crate) fn retain_unsynced_document(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        handle: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
    ) {
        // A candidate already refused by fsync or flush keeps its refusal cause
        // (ORNA-VFS-009); only a candidate never refused is recorded as unsynced.
        let cause = release_retention_cause(draft_state.rejected_documents.get(&inode));
        self.retain_candidate(draft_state, inode, handle, save, draft, cause, false);
    }

    #[allow(clippy::too_many_arguments)]
    /// Writes a candidate that was not accepted to recovery storage and records
    /// it against the file for unmount reporting (ORNA-VFS-009). A stale save
    /// also re-projects the row, so a new open shows and saves against the row
    /// another writer accepted (W15-03).
    pub(crate) fn retain_candidate(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        handle: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
        cause: &'static str,
        refresh: bool,
    ) -> Option<Errno> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = state.display_path(inode);
        let len = save.writer.runtime.block_on(draft.candidate_len());
        let mut candidate = save.writer.runtime.block_on(draft.take_candidate_charge());
        let retained = {
            let mut read_chunk = |offset: u64, length: usize| {
                save.writer
                    .runtime
                    .block_on(draft.read_range(offset, length))
                    .map_err(|error| RetentionError::from(candidate_unreadable(error)))
            };
            save.writer
                .retained_drafts
                .retain(&path, cause, len, &mut candidate, &mut read_chunk)
        };
        save.writer
            .runtime
            .block_on(draft.restore_candidate_charge(candidate));
        let (retained_draft, unretained, failure) = match retained {
            Ok(retained) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
                    retained.id,
                    retained.record.display()
                );
                (Some(retained.id), Vec::new(), None)
            }
            Err(retain_error) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate could not be retained: {retain_error}"
                );
                // Recovery storage refused the candidate: the live draft is the only copy
                // left, so it stays unresolved and is retried at the next retention.
                (
                    None,
                    vec![(Arc::clone(save), CandidateSource::Draft(Arc::clone(draft)))],
                    Some(retain_error.errno),
                )
            }
        };
        let refusal = DraftRefusal {
            cause,
            path: path.clone(),
            handle: Some(handle),
            candidate: Some(Arc::clone(draft)),
            draft: retained_draft,
            unretained,
        };
        match draft_state.rejected_documents.entry(inode) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let kept = entry.get_mut();
                if kept.unretained.is_empty() {
                    *kept = refusal;
                } else {
                    // An earlier refusal of this inode still holds an unretained source: keep it
                    // alongside this one rather than replacing it, so neither live draft is lost.
                    kept.unretained.extend(refusal.unretained);
                    kept.handle = refusal.handle;
                    kept.candidate = refusal.candidate;
                    if kept.draft.is_none() {
                        kept.draft = refusal.draft;
                    }
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(refusal);
            }
        }
        if refresh {
            state.report_refresh(inode, save, &path);
        }
        failure
    }
}

/// Seeds the next Blob candidate from the accepted bytes one bounded chunk at a
/// time, so the accepted image is never copied whole.
fn seed_next_candidate(
    save: &Arc<RowSaveContext>,
    bytes: &CandidateSnapshot,
    next_scratch: &TemporarySave<RowMapSnapshot>,
) -> Result<(), DocumentCommitError> {
    let accepted_length = bytes.len();
    let mut seeded = 0_u64;
    while seeded < accepted_length {
        let chunk = bytes
            .read_range(seeded, CANDIDATE_CHUNK_BYTES)
            .map_err(|error| {
                DocumentCommitError::Failed(format!(
                    "next Blob candidate could not be read: {error}"
                ))
            })?;
        if chunk.is_empty() {
            return Err(DocumentCommitError::Failed(
                "next Blob candidate ended before its recorded length".into(),
            ));
        }
        save.writer
            .runtime
            .block_on(next_scratch.write_at(seeded, &chunk))
            .map_err(|error| {
                DocumentCommitError::Failed(format!(
                    "next Blob candidate could not be seeded: {error:?}"
                ))
            })?;
        seeded += chunk.len() as u64;
    }
    Ok(())
}
