//! Source-module writes in the workspace mount (ORNA-VFS-020; 41-vfs.md:97;
//! profiles/vfs-1.md:66-80). A write-intent open of a projected source module
//! takes a private candidate seeded from its committed bytes. Writes stay in
//! memory and are never evaluated. fsync and close commit every dirty candidate
//! as one group: the project must validate with the candidates as overlays, each
//! disk file must still hold the baseline its candidate was derived from, and the
//! files are replaced together or not at all. A refused candidate is retained
//! (ORNA-VFS-009) and stays unresolved until a later commit accepts it.

use super::*;

/// Refusal causes recorded with a retained source candidate. Spec-silent names,
/// chosen here; they appear in the unmount listing and the retained draft record.
const SOURCE_INVALID: &str = "invalid_document";
const SOURCE_STALE: &str = "stale_edit";
const SOURCE_GROUP: &str = "group_refused";
const SOURCE_WRITE_FAILED: &str = "write_failed";
const SOURCE_UNSYNCED: &str = "unsynced";

/// A source module's private candidate. `baseline` is the committed content the
/// candidate was derived from; commit compares it with the disk file.
pub(super) struct SourceDraft {
    pub(super) baseline: Vec<u8>,
    pub(super) candidate: Vec<u8>,
    /// The candidate holds bytes that no commit has accepted.
    pub(super) dirty: bool,
    /// Why the candidate was not accepted, and its retained draft id when kept.
    pub(super) refusal: Option<SourceRefusal>,
}

pub(super) struct SourceRefusal {
    pub(super) cause: &'static str,
    pub(super) draft: Option<String>,
}

impl SourceDraft {
    fn seeded(committed: Vec<u8>) -> Self {
        Self {
            baseline: committed.clone(),
            candidate: committed,
            dirty: false,
            refusal: None,
        }
    }

    pub(super) fn is_unresolved(&self) -> bool {
        self.dirty || self.refusal.is_some()
    }
}

impl WorkspaceFilesystem {
    /// The committed bytes of a projected source module, or None for any other node.
    fn source_committed_bytes(&self, inode: u64) -> Option<Vec<u8>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.source_nodes.contains(&inode) {
            return None;
        }
        match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Bytes(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }

    fn source_writer(&self) -> Option<Arc<MountWriter>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .clone()
    }

    fn source_path(&self, inode: u64) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path_of(inode)
    }

    /// Replaces the committed bytes of a source node (accepted save or re-projection).
    fn set_source_node_bytes(&self, inode: u64, bytes: Vec<u8>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(node) = state.nodes.get_mut(&inode) {
            node.kind = NodeKind::Bytes(bytes);
        }
    }

    /// The inode of a source module open through `handle`.
    pub(super) fn source_handle_inode(&self, handle: u64) -> Option<u64> {
        match self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle)
        {
            Some(OpenFile::Source(inode)) => Some(*inode),
            _ => None,
        }
    }

    /// Whether `inode` is a projected source module, the only node a write-intent open can
    /// turn into a private candidate (ORNA-VFS-020).
    pub(super) fn is_source_module(&self, inode: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .source_nodes
            .contains(&inode)
    }

    /// The draft for `inode`. A clean draft is re-seeded from the committed bytes,
    /// so it never carries content the disk no longer holds.
    fn source_candidate_mut<'a>(
        &self,
        draft_state: &'a mut DraftState,
        inode: u64,
    ) -> Result<&'a mut SourceDraft, Errno> {
        let clean = !matches!(draft_state.source_drafts.get(&inode), Some(draft) if draft.dirty);
        if clean {
            let committed = self.source_committed_bytes(inode).ok_or(Errno::EBADF)?;
            draft_state
                .source_drafts
                .insert(inode, SourceDraft::seeded(committed));
        }
        draft_state.source_drafts.get_mut(&inode).ok_or(Errno::EBADF)
    }

    fn mark_source_handle(draft_state: &mut DraftState, handle: u64, inode: u64) {
        draft_state.unresolved_handles.insert(handle, inode);
        draft_state.dirty_handles.insert(handle);
    }

    /// Opens a source module for writing (ORNA-VFS-020). A mount without a writer
    /// has nothing to commit through, so the open is refused with EPERM.
    pub(super) fn source_open_write(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        truncate: bool,
        pid: u32,
    ) -> Result<u64, Errno> {
        self.source_committed_bytes(inode).ok_or(Errno::EPERM)?;
        self.source_writer().ok_or(Errno::EPERM)?;
        let draft = self.source_candidate_mut(draft_state, inode)?;
        if truncate {
            draft.candidate.clear();
            draft.dirty = true;
            draft.refusal = None;
        }
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Source(inode));
        draft_state.writer_owners.insert(handle, pid);
        if truncate {
            Self::mark_source_handle(draft_state, handle, inode);
        }
        Ok(handle)
    }

    /// Applies a write to a source candidate. The candidate is never evaluated here.
    pub(super) fn source_write(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        inode: u64,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Errno> {
        if data.is_empty() {
            return Ok(());
        }
        let end = offset.checked_add(data.len() as u64).ok_or(Errno::EFBIG)?;
        if end > DRAFT_FILE_BOUND_BYTES as u64 {
            return Err(Errno::EDQUOT);
        }
        let start = usize::try_from(offset).map_err(|_| Errno::EFBIG)?;
        let end = usize::try_from(end).map_err(|_| Errno::EFBIG)?;
        let draft = self.source_candidate_mut(draft_state, inode)?;
        if draft.candidate.len() < end {
            draft.candidate.resize(end, 0);
        }
        draft.candidate[start..end].copy_from_slice(data);
        draft.dirty = true;
        draft.refusal = None;
        Self::mark_source_handle(draft_state, handle, inode);
        Ok(())
    }

    /// Sets the candidate length of an open source handle (ftruncate).
    pub(super) fn source_truncate(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        inode: u64,
        size: u64,
    ) -> Result<(), Errno> {
        if size > DRAFT_FILE_BOUND_BYTES as u64 {
            return Err(Errno::EDQUOT);
        }
        let size = usize::try_from(size).map_err(|_| Errno::EFBIG)?;
        let draft = self.source_candidate_mut(draft_state, inode)?;
        draft.candidate.resize(size, 0);
        draft.dirty = true;
        draft.refusal = None;
        Self::mark_source_handle(draft_state, handle, inode);
        Ok(())
    }

    /// Serves a read of an open source handle from its private candidate.
    pub(super) fn source_read(&self, inode: u64, offset: u64, size: u32) -> Vec<u8> {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let committed;
        let bytes: &[u8] = match draft_state.source_drafts.get(&inode) {
            Some(draft) => &draft.candidate,
            None => {
                committed = self.source_committed_bytes(inode).unwrap_or_default();
                &committed
            }
        };
        let len = bytes.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(len);
        let end = start.saturating_add(size as usize).min(len);
        bytes[start..end].to_vec()
    }

    /// The candidate length of a source module, for attributes of an open handle.
    pub(super) fn source_candidate_len(&self, inode: u64) -> u64 {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(draft) = draft_state.source_drafts.get(&inode) {
            return draft.candidate.len() as u64;
        }
        self.source_committed_bytes(inode)
            .map_or(0, |bytes| bytes.len() as u64)
    }

    /// Forgets the unresolved and dirty state of every handle open on `inodes`.
    fn clear_source_handles(&self, draft_state: &mut DraftState, inodes: &[u64]) {
        let handles: Vec<u64> = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(handle, open)| match open {
                OpenFile::Source(inode) if inodes.contains(inode) => Some(*handle),
                _ => None,
            })
            .collect();
        for handle in handles {
            draft_state.unresolved_handles.remove(&handle);
            draft_state.dirty_handles.remove(&handle);
        }
    }

    /// Records a refusal for `inode`, retaining its candidate. `keep_dirty` leaves the
    /// candidate pending so a later commit can accept it. Returns the retention errno
    /// when the candidate could not be kept.
    fn refuse_source(
        &self,
        draft_state: &mut DraftState,
        writer: &MountWriter,
        inode: u64,
        path: &str,
        cause: &'static str,
        keep_dirty: bool,
    ) -> Option<Errno> {
        let draft = draft_state.source_drafts.get_mut(&inode)?;
        let (draft_id, failure) = match retain_source_candidate(writer, path, cause, &draft.candidate) {
            Ok(id) => (Some(id), None),
            Err(error) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate not retained: {}",
                    error.message
                );
                (None, Some(error.errno))
            }
        };
        draft.refusal = Some(SourceRefusal {
            cause,
            draft: draft_id,
        });
        draft.dirty = keep_dirty;
        failure
    }

    /// Refuses every candidate of a group with one cause. Returns the worst retention errno.
    fn refuse_source_group(
        &self,
        draft_state: &mut DraftState,
        writer: &MountWriter,
        group: &[(u64, String)],
        cause: &'static str,
    ) -> Option<Errno> {
        let mut worst = None;
        for (inode, path) in group {
            if let Some(errno) = self.refuse_source(draft_state, writer, *inode, path, cause, true) {
                worst = worst_retention_errno(worst, errno);
            }
        }
        worst
    }

    /// Commits every dirty source candidate as one group (fsync or close). A refused
    /// group keeps its candidates dirty and retained; an accepted group removes its drafts.
    pub(super) fn source_commit(
        &self,
        draft_state: &mut DraftState,
    ) -> Result<(), DocumentCommitError> {
        let dirty: Vec<u64> = draft_state
            .source_drafts
            .iter()
            .filter(|(_, draft)| draft.dirty)
            .map(|(inode, _)| *inode)
            .collect();
        if dirty.is_empty() {
            return Ok(());
        }
        let writer = self
            .source_writer()
            .ok_or_else(|| DocumentCommitError::Failed("mount has no writer".to_owned()))?;
        let group = dirty
            .iter()
            .map(|inode| {
                self.source_path(*inode)
                    .map(|path| (*inode, path))
                    .ok_or_else(|| {
                        DocumentCommitError::Failed("source module path is unavailable".to_owned())
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Admission: the whole project must validate with the group as overlays.
        if let Err(detail) = admit_source_group(&writer, draft_state, &group) {
            eprintln!("source save refused ({SOURCE_INVALID}): {detail}");
            return Err(
                match self.refuse_source_group(draft_state, &writer, &group, SOURCE_INVALID) {
                    Some(errno) => DocumentCommitError::Unretained {
                        errno,
                        cause: SOURCE_INVALID,
                    },
                    None => DocumentCommitError::Rejected(detail),
                },
            );
        }

        // Compare-and-swap: each disk file must still hold the baseline its candidate came from.
        let root = writer.repository.worktree().to_path_buf();
        let mut stale = Vec::new();
        let mut disk_bytes = Vec::new();
        for (inode, path) in &group {
            let baseline = &draft_state.source_drafts[inode].baseline;
            let disk = checked_project_source_path(&root, path)
                .ok()
                .and_then(|disk| std::fs::read(disk).ok());
            if disk.as_deref() != Some(baseline.as_slice()) {
                stale.push(*inode);
            }
            disk_bytes.push((*inode, disk));
        }
        if !stale.is_empty() {
            // A stale file keeps its retained candidate and re-projects the disk content.
            // The rest of the group is refused as a unit and stays pending. The handles
            // stay unresolved, so a later fsync or close cannot acknowledge the stale write.
            let mut worst = None;
            for (inode, path) in &group {
                let (cause, keep_dirty) = if stale.contains(inode) {
                    (SOURCE_STALE, false)
                } else {
                    (SOURCE_GROUP, true)
                };
                if let Some(errno) =
                    self.refuse_source(draft_state, &writer, *inode, path, cause, keep_dirty)
                {
                    worst = worst_retention_errno(worst, errno);
                }
            }
            for (inode, disk) in disk_bytes {
                if let (true, Some(bytes)) = (stale.contains(&inode), disk) {
                    self.set_source_node_bytes(inode, bytes);
                }
            }
            return Err(match worst {
                Some(errno) => DocumentCommitError::Unretained {
                    errno,
                    cause: SOURCE_STALE,
                },
                None => DocumentCommitError::Stale,
            });
        }

        // Replacement: every file is written, or the whole group is restored.
        for (index, (inode, path)) in group.iter().enumerate() {
            let draft = &draft_state.source_drafts[inode];
            let result = match checked_project_source_path(&root, path) {
                Ok(disk) => write_source_atomically(&disk, &draft.baseline, &draft.candidate),
                Err(_) => Err(std::io::Error::other("source path is unavailable")),
            };
            if let Err(error) = result {
                for (done, done_path) in group[..index].iter().rev() {
                    let done_draft = &draft_state.source_drafts[done];
                    let restored = checked_project_source_path(&root, done_path).is_ok_and(|disk| {
                        restore_source_after_failed_write(
                            &disk,
                            &done_draft.baseline,
                            &done_draft.candidate,
                        )
                    });
                    if !restored {
                        eprintln!(
                            "source {done_path} could not be restored after a failed group save"
                        );
                    }
                }
                let refused =
                    self.refuse_source_group(draft_state, &writer, &group, SOURCE_WRITE_FAILED);
                return Err(match refused {
                    Some(errno) => DocumentCommitError::Unretained {
                        errno,
                        cause: SOURCE_WRITE_FAILED,
                    },
                    None => DocumentCommitError::Failed(format!(
                        "source save for {path} failed: {error}"
                    )),
                });
            }
        }

        let inodes: Vec<u64> = group.iter().map(|(inode, _)| *inode).collect();
        for inode in &inodes {
            if let Some(draft) = draft_state.source_drafts.remove(inode) {
                self.set_source_node_bytes(*inode, draft.candidate);
            }
        }
        self.clear_source_handles(draft_state, &inodes);
        Ok(())
    }

    /// Handles the close of a source handle (release). When no handle remains open
    /// on the module, an unaccepted candidate is retained as `unsynced` and stays
    /// pending; a clean draft is dropped.
    pub(super) fn source_release(&self, draft_state: &mut DraftState, inode: u64) {
        let still_open = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|open| matches!(open, OpenFile::Source(open_inode) if *open_inode == inode));
        if still_open {
            return;
        }
        let (dirty, refused) = match draft_state.source_drafts.get(&inode) {
            Some(draft) => (draft.dirty, draft.refusal.is_some()),
            None => return,
        };
        if !dirty && !refused {
            draft_state.source_drafts.remove(&inode);
            return;
        }
        if dirty && !refused {
            let (Some(writer), Some(path)) = (self.source_writer(), self.source_path(inode)) else {
                return;
            };
            self.refuse_source(draft_state, &writer, inode, &path, SOURCE_UNSYNCED, true);
        }
    }

    /// Retains dirty source candidates that no commit has refused yet, at quiesce
    /// (ORNA-VFS-009). Returns the retention errno when a candidate could not be kept.
    pub(super) fn retain_unsynced_sources(&self, draft_state: &mut DraftState) -> Option<Errno> {
        let writer = self.source_writer()?;
        let pending: Vec<(u64, String)> = draft_state
            .source_drafts
            .iter()
            .filter(|(_, draft)| draft.dirty && draft.refusal.is_none())
            .filter_map(|(inode, _)| self.source_path(*inode).map(|path| (*inode, path)))
            .collect();
        let mut worst = None;
        for (inode, path) in pending {
            let Some(draft) = draft_state.source_drafts.get_mut(&inode) else {
                continue;
            };
            match retain_source_candidate(&writer, &path, SOURCE_UNSYNCED, &draft.candidate) {
                Ok(id) => {
                    draft.refusal = Some(SourceRefusal {
                        cause: SOURCE_UNSYNCED,
                        draft: Some(id),
                    });
                }
                Err(error) => worst = worst_retention_errno(worst, error.errno),
            }
        }
        worst
    }
}

/// Validates the project with the group's candidates as overlays, the same check
/// `check` runs on a saved tree. An invalid candidate is refused before any file is written.
fn admit_source_group(
    writer: &MountWriter,
    draft_state: &DraftState,
    group: &[(u64, String)],
) -> Result<(), String> {
    let mut overlays = BTreeMap::new();
    for (inode, path) in group {
        let candidate = &draft_state.source_drafts[inode].candidate;
        let text = std::str::from_utf8(candidate).map_err(|_| format!("{path} is not UTF-8"))?;
        overlays.insert(path.clone(), text.to_owned());
    }
    let project = orna_project_v1::ProjectLoader::default()
        .load_with_overlays(&writer.repository, &overlays)
        .map_err(|error| format!("{error:?}"))?;
    let catalogue = project
        .production_catalogue()
        .map_err(|error| format!("{error:?}"))?;
    let analysis = orna_semantic_v1::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic_v1::SystemProviders::Production,
    );
    if analysis.is_ok() {
        return Ok(());
    }
    Err(analysis.diagnostics.first().map_or_else(
        || "semantic analysis failed".to_owned(),
        |diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()),
    ))
}

/// Keeps a refused source candidate in recovery storage under a stable draft id (ORNA-VFS-009).
fn retain_source_candidate(
    writer: &MountWriter,
    path: &str,
    cause: &str,
    candidate: &[u8],
) -> Result<String, RetentionError> {
    let mut read_chunk = |offset: u64, length: usize| -> Result<Vec<u8>, RetentionError> {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(candidate.len());
        let end = start.saturating_add(length).min(candidate.len());
        Ok(candidate[start..end].to_vec())
    };
    let retained = writer
        .retained_drafts
        .retain(path, cause, candidate.len() as u64, &mut read_chunk)?;
    eprintln!(
        "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
        retained.id,
        retained.record.display()
    );
    Ok(retained.id)
}
