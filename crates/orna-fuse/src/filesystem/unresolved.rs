use super::*;

impl WorkspaceFilesystem {
    /// The unresolved drafts of this mount, one per path. Both draft-list xattrs
    /// render these, so the unmount refusal and `mount status` name the same drafts.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn unresolved_drafts_of(
        draft_state: &DraftState,
        state: &FsBuilder,
        for_unmount: bool,
    ) -> Vec<UnresolvedDraft> {
        // The generated `sys` code of an internal cause, when the cause names one.
        fn spec_code(cause: &'static str) -> Option<&'static str> {
            SysFailureCode::parse(cause).map(SysFailureCode::as_str)
        }
        let draft_retained = SysFailureCode::VfsDraftRetained.as_str();
        // inode -> (code, draft id, detail). The code is the spec code of the internal
        // cause; otherwise `draft_retained` when a candidate was kept; otherwise empty.
        let mut unresolved: BTreeMap<u64, (&'static str, String, String)> = BTreeMap::new();
        // The path each refusal captured when recorded; the refused node may leave the tree first.
        let mut captured: HashMap<u64, String> = HashMap::new();
        for (inode, refusal) in &draft_state.rejected_documents {
            // Unmount refuses only drafts the mount can still resolve (profiles/vfs-1.md:144). A
            // candidate recovery storage kept whose document can no longer be opened is already
            // explicitly retained there, and the mount cannot serve its row to resolve it.
            if for_unmount && refusal_is_unservable_retained(state, *inode, refusal) {
                continue;
            }
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.draft.as_ref().map(|_| draft_retained))
                .unwrap_or("");
            let (draft, detail) = match &refusal.draft {
                Some(draft) => (
                    draft.clone(),
                    format!(
                        "not accepted ({}); the file shows the current row; candidate retained as draft {draft}",
                        refusal.cause
                    ),
                ),
                None => (
                    String::new(),
                    format!("not accepted ({}); candidate not retained", refusal.cause),
                ),
            };
            captured.insert(*inode, refusal.path.clone());
            unresolved.insert(*inode, (code, draft, detail));
        }
        for (inode, source) in &draft_state.source_drafts {
            let Some(refusal) = source.refusal() else {
                if source.is_unsynced() {
                    unresolved.entry(*inode).or_insert_with(|| {
                        (
                            "",
                            String::new(),
                            "unsynced writes in an open file".to_owned(),
                        )
                    });
                }
                continue;
            };
            // A candidate kept as a draft whose node left the mount has nothing left to resolve
            // (ORNA-VFS-009).
            if refusal.draft.is_some() && state.path_of(*inode).is_none() {
                continue;
            }
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.draft.as_ref().map(|_| draft_retained))
                .unwrap_or("");
            let (draft, detail) = match &refusal.draft {
                Some(draft) => (
                    draft.clone(),
                    format!(
                        "not accepted ({}); candidate retained as draft {draft}",
                        refusal.cause
                    ),
                ),
                None => (
                    String::new(),
                    format!("not accepted ({}); candidate not retained", refusal.cause),
                ),
            };
            unresolved.entry(*inode).or_insert((code, draft, detail));
        }
        for (inode, refusal) in &draft_state.rejected_scratch {
            captured.insert(*inode, refusal.path.clone());
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.kept.then_some(draft_retained))
                .unwrap_or("");
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    code,
                    String::new(),
                    format!("scratch rename refused ({})", refusal.cause),
                )
            });
        }
        for inode in &draft_state.retained_scratch {
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    draft_retained,
                    String::new(),
                    "scratch file not renamed".to_owned(),
                )
            });
        }
        for inode in draft_state.unresolved_inodes() {
            unresolved.entry(inode).or_insert_with(|| {
                (
                    "",
                    String::new(),
                    "unsynced writes in an open file".to_owned(),
                )
            });
        }
        for (inode, size) in &draft_state.pending_truncates {
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    "",
                    String::new(),
                    format!("truncate to {size} bytes not synced"),
                )
            });
        }
        let mut drafts = Vec::with_capacity(unresolved.len());
        for (inode, (code, draft, detail)) in unresolved {
            drafts.push(UnresolvedDraft {
                path: state
                    .path_through(inode, true)
                    .or_else(|| captured.get(&inode).cloned())
                    .unwrap_or_else(|| state.display_path(inode)),
                code,
                draft,
                detail,
            });
        }
        drafts
    }

    pub(crate) fn unresolved_drafts(&self) -> Vec<UnresolvedDraft> {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::unresolved_drafts_of(&draft_state, &state, false)
    }

    /// One `path<TAB>detail` line per unresolved draft, for an unmount refusal.
    pub(crate) fn unresolved_draft_report(&self) -> String {
        self.render_unresolved_drafts(|draft| format!("{}\t{}", draft.path, draft.detail))
    }

    /// One `path<TAB>code<TAB>draft<TAB>detail` line per unresolved draft, for
    /// `mount status`. `code` is the `sys.*` Orna cause (or `sys.vfs.draft_retained`
    /// for a kept candidate with no other cause) and `draft` the retained draft id;
    /// each is empty when the draft has none.
    pub(crate) fn unresolved_draft_fields(&self) -> String {
        self.render_unresolved_drafts(|draft| {
            format!(
                "{}\t{}\t{}\t{}",
                draft.path, draft.code, draft.draft, draft.detail
            )
        })
    }

    /// Renders the drafts as sorted lines, bounded by both a line count and a byte
    /// budget so the xattr value stays small enough for the kernel to return. A draft
    /// that does not fit is not listed; it is counted on one final `\tomitted\tN` line.
    /// That line has an empty first field, which no draft path can have, so it is
    /// never parsed as a draft.
    pub(crate) fn render_unresolved_drafts(
        &self,
        line: impl Fn(&UnresolvedDraft) -> String,
    ) -> String {
        const LISTED: usize = 64;
        const BYTE_BUDGET: usize = 32 * 1024;
        // Room for the final `\tomitted\tN` line (its count is at most 20 digits).
        const OMITTED_RESERVE: usize = 32;
        let mut lines: Vec<String> = self.unresolved_drafts().iter().map(line).collect();
        lines.sort();
        let total = lines.len();
        let mut kept = Vec::with_capacity(total.min(LISTED));
        let mut bytes = 0_usize;
        for text in lines {
            if kept.len() == LISTED || bytes + text.len() + 1 + OMITTED_RESERVE > BYTE_BUDGET {
                break;
            }
            bytes += text.len() + 1;
            kept.push(text);
        }
        let omitted = total - kept.len();
        if omitted > 0 {
            kept.push(format!("\tomitted\t{omitted}"));
        }
        kept.join("\n")
    }

    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_lines)]
    /// Stop-time retention (ORNA-VFS-024): no unresolved draft is silently
    /// discarded when the mount stops. Refused documents are already in
    /// recovery storage; unsynced document writes and truncates are added to
    /// it; scratch files are persisted to the scratch store, which the next
    /// mount restores. Every kept draft is reported with
    /// `sys.vfs.draft_retained`. A candidate that recovery storage refuses keeps
    /// its bytes as an unresolved refusal, so a later attempt retries it.
    /// Returns the errno of the refusal that kept a draft, or `None` when every draft was retained.
    pub(crate) fn retain_all_drafts(&self, draft_state: &mut DraftState) -> Option<Errno> {
        let source_refused = self.retain_unsynced_sources(draft_state);
        let open_files = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut reports = std::collections::BTreeSet::new();
        let mut candidates: Vec<(
            u64,
            Option<u64>,
            &'static str,
            Arc<RowSaveContext>,
            CandidateSource,
        )> = Vec::new();
        // A path truncate cuts the newest writer draft of its inode. That cut is
        // the record the draft is retained as, so the same draft's untruncated
        // copy is superseded and is not retained a second time.
        let mut truncations = Vec::new();
        for (inode, size) in &draft_state.pending_truncates {
            if let Some(NodeKind::Document { save, .. }) =
                state.nodes.get(inode).map(|node| &node.kind)
            {
                // A path truncate cuts the newest writer draft: the open writer's,
                // else the candidate a released writer's refusal still holds.
                let latest = draft_state
                    .unresolved_handles()
                    .filter(|(_, dirty_inode)| *dirty_inode == *inode)
                    .filter_map(|(handle, _)| match open_files.get(&handle) {
                        Some(OpenFile::Document(_, draft, _)) => Some((handle, Arc::clone(draft))),
                        _ => None,
                    })
                    .max_by_key(|(handle, _)| *handle)
                    .map(|(_, draft)| draft)
                    .or_else(|| {
                        draft_state
                            .rejected_documents
                            .get(inode)
                            .and_then(|refusal| refusal.candidate.clone())
                    });
                truncations.push((*inode, *size, Arc::clone(save), latest));
            }
        }
        for (handle, inode) in draft_state.unresolved_handles() {
            if let Some(OpenFile::Document(save, draft, _)) = open_files.get(&handle) {
                let cut_by_truncate = truncations.iter().any(|(truncated, _, _, latest)| {
                    *truncated == inode
                        && latest
                            .as_ref()
                            .is_some_and(|latest| Arc::ptr_eq(latest, draft))
                });
                if cut_by_truncate {
                    continue;
                }
                candidates.push((
                    inode,
                    Some(handle),
                    "unsynced",
                    Arc::clone(save),
                    CandidateSource::Draft(Arc::clone(draft)),
                ));
            }
        }
        for (inode, size, save, latest) in &truncations {
            candidates.push((
                *inode,
                None,
                "unsynced",
                Arc::clone(save),
                CandidateSource::Truncated {
                    size: *size,
                    latest: latest.clone(),
                },
            ));
        }
        let mut unresolved: HashMap<u64, DraftRefusal> = HashMap::new();
        let mut refused: Option<Errno> = None;
        let mut superseded: HashMap<u64, (String, String)> = HashMap::new();
        for (inode, refusal) in std::mem::take(&mut draft_state.rejected_documents) {
            // A refusal whose candidate a path truncate cuts is superseded by the
            // truncated record once that record is retained.
            let cut_by_truncate = truncations.iter().any(|(truncated, _, _, latest)| {
                *truncated == inode
                    && match (latest, refusal.candidate.as_ref()) {
                        (Some(latest), Some(candidate)) => Arc::ptr_eq(latest, candidate),
                        // Neither a writer nor a refusal candidate: the cut is of
                        // the refusal itself, so its untruncated copy is superseded.
                        (None, None) => true,
                        _ => false,
                    }
            });
            for (save, source) in refusal.unretained {
                // A cut candidate is retained once, as its truncated record above; its
                // untruncated copy is not a second record (ORNA-VFS-009, EDIT-1 step 4).
                if !cut_by_truncate {
                    candidates.push((inode, refusal.handle, refusal.cause, save, source));
                }
            }
            if let Some(draft) = refusal.draft {
                let path = state.display_path(inode);
                let report = format!(
                    "retained draft {draft} for {path} ({}; sys.vfs.draft_retained)",
                    refusal.cause
                );
                if cut_by_truncate {
                    superseded.insert(inode, (draft, report));
                } else {
                    reports.insert(report);
                }
            }
        }
        for (inode, handle, cause, save, source) in candidates {
            let path = state.display_path(inode);
            let len = match &source {
                CandidateSource::Draft(draft) => {
                    save.writer.runtime.block_on(draft.candidate_len())
                }
                CandidateSource::Truncated { size, .. } => *size,
            };
            let truncated = matches!(source, CandidateSource::Truncated { .. });
            let mut read_chunk = |offset: u64, length: usize| match &source {
                CandidateSource::Draft(draft) => save
                    .writer
                    .runtime
                    .block_on(draft.read_range(offset, length))
                    .map_err(|error| RetentionError::from(candidate_unreadable(error))),
                CandidateSource::Truncated { size, latest } => match latest {
                    Some(draft) => truncated_window(
                        save.writer.runtime.block_on(draft.candidate_len()),
                        *size,
                        offset,
                        length,
                        |start, count| {
                            save.writer
                                .runtime
                                .block_on(draft.read_range(start, count))
                                .map_err(|error| RetentionError::from(candidate_unreadable(error)))
                        },
                    ),
                    None => match state.nodes.get(&inode).map(|node| &node.kind) {
                        Some(NodeKind::Document { bytes, .. }) => truncated_window(
                            bytes.len() as u64,
                            *size,
                            offset,
                            length,
                            |start, count| Ok(stashed_range(bytes, start, count)),
                        ),
                        _ => Err(RetentionError::from(format!(
                            "{path} has no accepted baseline to truncate"
                        ))),
                    },
                },
            };
            let mut candidate = match &source {
                CandidateSource::Draft(draft) => {
                    save.writer.runtime.block_on(draft.take_candidate_charge())
                }
                CandidateSource::Truncated { .. } => {
                    QuotaCharge::empty(&save.writer.retained_drafts.quota)
                }
            };
            let retained = save.writer.retained_drafts.retain(
                &path,
                cause,
                len,
                &mut candidate,
                &mut read_chunk,
            );
            if let CandidateSource::Draft(draft) = &source {
                save.writer
                    .runtime
                    .block_on(draft.restore_candidate_charge(candidate));
            }
            match retained {
                Ok(retained) => {
                    reports.insert(format!(
                        "retained draft {} for {path} ({cause}; sys.vfs.draft_retained) in {}",
                        retained.id,
                        retained.record.display()
                    ));
                    if truncated
                        && let Some((old, _)) = superseded.remove(&inode)
                        && old != retained.id
                        && let Err(error) = save.writer.retained_drafts.remove(&old)
                    {
                        reports.insert(format!(
                            "superseded draft {old} for {path} could not be removed: {error}"
                        ));
                    }
                }
                Err(error) => {
                    refused = worst_retention_errno(refused, error.errno);
                    reports.insert(format!(
                        "draft for {path} ({cause}) could not be retained: {error}"
                    ));
                    if truncated && let Some((_, report)) = superseded.remove(&inode) {
                        reports.insert(report);
                    }
                    // Recovery storage refused the candidate: it stays unresolved, and its
                    // live source is retried at the next retention rather than copied here.
                    let live = source.live_draft();
                    match unresolved.entry(inode) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            // A second refused candidate of this inode: both stay unresolved, so
                            // neither the earlier unretained source nor its live draft is dropped.
                            let kept = entry.get_mut();
                            kept.unretained.push((save, source));
                            if live.is_some() {
                                kept.candidate = live;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(DraftRefusal {
                                cause,
                                path,
                                draft: None,
                                handle,
                                candidate: live,
                                unretained: vec![(save, source)],
                            });
                        }
                    }
                }
            }
        }
        let scratch = draft_state
            .retained_scratch
            .iter()
            .chain(draft_state.rejected_scratch.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        // Scratch whose bytes could not be kept stays unresolved and keeps the mount up.
        let mut unkept_scratch = std::collections::HashSet::new();
        for inode in scratch {
            let Some(NodeKind::Temporary {
                save,
                scratch,
                identity,
                schema_identity,
                baseline,
                ..
            }) = state.nodes.get(&inode).map(|node| &node.kind)
            else {
                continue;
            };
            let path = state.display_path(inode);
            match save.fsync_temporary(scratch, identity, schema_identity, baseline.as_deref()) {
                Ok(()) => {
                    reports.insert(format!(
                        "kept scratch {path} for the next mount (sys.vfs.draft_retained)"
                    ));
                }
                Err(error) => {
                    refused = worst_retention_errno(refused, Errno::EIO);
                    reports.insert(format!("scratch {path} could not be kept: {error}"));
                    unkept_scratch.insert(inode);
                }
            }
        }
        for report in reports {
            eprintln!("{report}");
        }
        draft_state.clear_unresolved();
        draft_state.pending_truncates.clear();
        draft_state
            .retained_scratch
            .retain(|inode| unkept_scratch.contains(inode));
        draft_state
            .rejected_scratch
            .retain(|inode, _| unkept_scratch.contains(inode));
        draft_state.rejected_documents = unresolved;
        if let Some(errno) = source_refused {
            refused = worst_retention_errno(refused, errno);
        }
        refused
    }
}
