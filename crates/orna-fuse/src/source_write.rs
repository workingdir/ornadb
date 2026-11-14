//! Source-module writes in the workspace mount (ORNA-VFS-020; 41-vfs.md:97;
//! profiles/vfs-1.md:66-80). A write-intent open of a projected source module
//! takes a private candidate seeded from its committed bytes. Writes stay in
//! memory and are never evaluated. fsync and close commit the dirty candidates
//! together: each candidate is judged against the committed tree, and the
//! candidates that fail are refused and retained (ORNA-VFS-009). The remaining
//! candidates are written together or not at all. A refused candidate stays
//! unresolved until a later commit accepts it.
//!
//! The group is the set of dirty candidates, which the spec does not define
//! (profiles/vfs-1.md states no cross-handle trigger). The sync trigger's errno
//! reports only the trigger's own candidate, never another module's refusal.

use super::{
    Arc, BTreeMap, DRAFT_FILE_BOUND_BYTES, DocumentCommitError, DraftState, Errno, MountWriter,
    NodeKind, OpenFile, Ordering, Path, RetentionError, WorkspaceFilesystem,
    checked_project_source_path, restore_source_after_failed_write, worst_retention_errno,
    write_source_atomically,
};

/// Refusal causes recorded with a retained source candidate (ORNA-VFS-009). The
/// `sys.vfs.*` names follow the document causes in `DocumentCommitError::cause`.
const SOURCE_INVALID: &str = orna_sys_id::SysFailureCode::VfsInvalidDocument.as_str();
const SOURCE_STALE: &str = orna_sys_id::SysFailureCode::VfsStaleEdit.as_str();
const SOURCE_UNSYNCED: &str = "unsynced";

/// Bound on candidate and baseline bytes across every source draft (EDQUOT).
const SOURCE_DRAFT_TOTAL_BOUND_BYTES: usize = 4 * DRAFT_FILE_BOUND_BYTES;

/// Validation attempts before a commit gives up because drafts kept changing.
const SOURCE_COMMIT_ATTEMPTS: usize = 3;

/// A source module's private candidate. `baseline` is the committed content the
/// candidate was derived from; commit compares it with the disk file.
pub(super) struct SourceDraft {
    pub(super) baseline: Vec<u8>,
    pub(super) candidate: Vec<u8>,
    pub(super) status: SourceDraftStatus,
}

/// What a source candidate holds relative to the committed baseline.
pub(super) enum SourceDraftStatus {
    /// The candidate equals the committed bytes.
    Clean,
    /// The candidate holds bytes no commit has accepted.
    Unsynced,
    /// A commit refused the candidate; `unsynced` when those bytes still
    /// differ from the accepted row, so a later commit can accept them.
    Refused {
        unsynced: bool,
        refusal: SourceRefusal,
    },
}

impl SourceDraftStatus {
    /// The state a group member records when its commit begins.
    fn member_state(&self) -> MemberState {
        match self {
            Self::Clean => MemberState::Clean,
            Self::Unsynced => MemberState::Unsynced,
            Self::Refused { unsynced, .. } => MemberState::Refused {
                unsynced: *unsynced,
            },
        }
    }
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
            status: SourceDraftStatus::Clean,
        }
    }

    /// Whether the candidate holds bytes no commit has accepted.
    pub(super) fn is_unsynced(&self) -> bool {
        matches!(
            self.status,
            SourceDraftStatus::Unsynced | SourceDraftStatus::Refused { unsynced: true, .. }
        )
    }

    pub(super) fn refusal(&self) -> Option<&SourceRefusal> {
        match &self.status {
            SourceDraftStatus::Refused { refusal, .. } => Some(refusal),
            SourceDraftStatus::Clean | SourceDraftStatus::Unsynced => None,
        }
    }

    pub(super) fn is_refused(&self) -> bool {
        self.refusal().is_some()
    }

    /// Marks the candidate as holding unaccepted writes, clearing an old refusal.
    fn mark_unsynced(&mut self) {
        self.status = SourceDraftStatus::Unsynced;
    }

    /// Records a refusal, keeping the candidate pending only when
    /// `keep_unsynced` says those bytes still differ from the accepted row.
    fn refuse(&mut self, refusal: SourceRefusal, keep_unsynced: bool) {
        self.status = SourceDraftStatus::Refused {
            unsynced: keep_unsynced,
            refusal,
        };
    }
}

/// A group member as it stood when its commit began.
struct SourceMember {
    inode: u64,
    path: String,
    baseline: Vec<u8>,
    candidate: Vec<u8>,
    state: MemberState,
}

/// A group member's draft state when its commit began, without the refusal
/// detail the live draft keeps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemberState {
    Clean,
    Unsynced,
    Refused { unsynced: bool },
}

/// The verdict on one group member.
enum SourceVerdict {
    Accept,
    /// The candidate fails the project check, or is not UTF-8.
    Invalid(String),
    /// The disk file no longer holds the baseline the candidate came from.
    Stale,
}

impl WorkspaceFilesystem {
    /// Runs `read` on the committed bytes of a projected source module. None for any other node.
    fn with_source_committed<R>(&self, inode: u64, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.source_nodes.contains(&inode) {
            return None;
        }
        match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Bytes(bytes)) => Some(read(bytes.as_slice())),
            _ => None,
        }
    }

    fn source_committed_bytes(&self, inode: u64) -> Option<Vec<u8>> {
        self.with_source_committed(inode, <[u8]>::to_vec)
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

    /// Fails with EDQUOT when a candidate of `candidate_len` bytes for `inode` would push
    /// the drafts past the total bound. `rebase` means the draft is reseeded from the
    /// committed bytes (a truncating open of a clean draft).
    fn source_budget_fits(
        &self,
        draft_state: &DraftState,
        inode: u64,
        rebase: bool,
        candidate_len: usize,
    ) -> Result<(), Errno> {
        let others: usize = draft_state
            .source_drafts
            .iter()
            .filter(|(other, _)| **other != inode)
            .map(|(_, draft)| draft.baseline.len() + draft.candidate.len())
            .sum();
        let baseline_len = match draft_state.source_drafts.get(&inode) {
            Some(draft) if !rebase || draft.is_unsynced() => draft.baseline.len(),
            _ => self
                .with_source_committed(inode, <[u8]>::len)
                .ok_or(Errno::EBADF)?,
        };
        if others + baseline_len + candidate_len > SOURCE_DRAFT_TOTAL_BOUND_BYTES {
            return Err(Errno::EDQUOT);
        }
        Ok(())
    }

    /// The draft for `inode`. A draft is seeded from the committed bytes only when none
    /// exists, or when `rebase` asks for a clean draft to restart from the committed bytes.
    /// A held refusal is otherwise kept, so the handle never loses its candidate silently.
    fn source_candidate_mut<'a>(
        &self,
        draft_state: &'a mut DraftState,
        inode: u64,
        rebase: bool,
    ) -> Result<&'a mut SourceDraft, Errno> {
        let reseed = match draft_state.source_drafts.get(&inode) {
            None => true,
            Some(draft) => rebase && !draft.is_unsynced(),
        };
        if reseed {
            let committed = self.source_committed_bytes(inode).ok_or(Errno::EBADF)?;
            draft_state
                .source_drafts
                .insert(inode, SourceDraft::seeded(committed));
        }
        draft_state
            .source_drafts
            .get_mut(&inode)
            .ok_or(Errno::EBADF)
    }

    fn mark_source_handle(draft_state: &mut DraftState, handle: u64, inode: u64) {
        draft_state.set_unresolved(handle, inode);
        draft_state.set_unaccepted_write(handle);
    }

    /// Opens a source module for writing (ORNA-VFS-020). A mount without a writer
    /// has nothing to commit through, so the open is refused with EPERM. A truncating
    /// open starts the candidate empty from the committed bytes.
    pub(super) fn source_open_write(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        truncate: bool,
        pid: u32,
    ) -> Result<u64, Errno> {
        self.with_source_committed(inode, |_| ())
            .ok_or(Errno::EPERM)?;
        self.source_writer().ok_or(Errno::EPERM)?;
        if truncate {
            self.source_budget_fits(&*draft_state, inode, true, 0)?;
        }
        let draft = self.source_candidate_mut(draft_state, inode, truncate)?;
        if truncate {
            draft.candidate.clear();
            draft.mark_unsynced();
        }
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Source(inode));
        draft_state.set_writer_owner(handle, pid);
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
        let current = draft_state
            .source_drafts
            .get(&inode)
            .map_or(0, |draft| draft.candidate.len());
        self.source_budget_fits(&*draft_state, inode, false, end.max(current))?;
        let draft = self.source_candidate_mut(draft_state, inode, false)?;
        if draft.candidate.len() < end {
            draft.candidate.resize(end, 0);
        }
        draft.candidate[start..end].copy_from_slice(data);
        draft.mark_unsynced();
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
        self.source_budget_fits(&*draft_state, inode, false, size)?;
        let draft = self.source_candidate_mut(draft_state, inode, false)?;
        draft.candidate.resize(size, 0);
        draft.mark_unsynced();
        Self::mark_source_handle(draft_state, handle, inode);
        Ok(())
    }

    /// Serves a read of an open source handle from its private candidate.
    pub(super) fn source_read(&self, inode: u64, offset: u64, size: u32) -> Vec<u8> {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = |bytes: &[u8]| {
            let len = bytes.len();
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(len);
            let end = start.saturating_add(size as usize).min(len);
            bytes[start..end].to_vec()
        };
        match draft_state.source_drafts.get(&inode) {
            Some(draft) => window(&draft.candidate),
            None => self
                .with_source_committed(inode, window)
                .unwrap_or_default(),
        }
    }

    /// The candidate length of a source module, for attributes of an open handle.
    pub(super) fn source_candidate_len(&self, inode: u64) -> u64 {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match draft_state.source_drafts.get(&inode) {
            Some(draft) => draft.candidate.len() as u64,
            None => self
                .with_source_committed(inode, |bytes| bytes.len() as u64)
                .unwrap_or(0),
        }
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
            draft_state.take_unresolved(handle);
            draft_state.clear_unaccepted_write(handle);
        }
    }

    #[allow(clippy::unused_self)]
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
        let Some(draft) = draft_state.source_drafts.get_mut(&inode) else {
            eprintln!("[vfs] {path}: not accepted ({cause}); no candidate is held to retain");
            return Some(Errno::EIO);
        };
        let (draft_id, failure) =
            match retain_source_candidate(writer, path, cause, &draft.candidate) {
                Ok(id) => (Some(id), None),
                Err(error) => {
                    eprintln!(
                        "[vfs] {path}: not accepted ({cause}); candidate not retained: {}",
                        error.message
                    );
                    (None, Some(error.errno))
                }
            };
        draft.refuse(
            SourceRefusal {
                cause,
                draft: draft_id,
            },
            keep_dirty,
        );
        failure
    }

    /// Commits the dirty source candidates (fsync or close of a source handle). The
    /// commit gate serializes groups; the draft lock is held only for snapshots and
    /// applied results, so validation does not stall other handles.
    pub(super) fn source_sync(&self, trigger: u64) -> Result<(), Errno> {
        let _gate = self
            .source_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ensure_serving()?;
        self.source_group_commit(trigger).map_err(|error| {
            eprintln!("source commit did not accept the group: {error}");
            error.errno()
        })
    }

    #[allow(clippy::too_many_lines)]
    /// The commit itself, run under the commit gate. `trigger` is the inode of the
    /// handle that asked for the sync. Its errno reports only its own candidate.
    fn source_group_commit(&self, trigger: u64) -> Result<(), DocumentCommitError> {
        let writer = self
            .source_writer()
            .ok_or_else(|| DocumentCommitError::Failed("mount has no writer".to_owned()))?;
        let root = writer.repository.worktree().to_path_buf();
        let mut trigger_in_group = false;
        for _ in 0..SOURCE_COMMIT_ATTEMPTS {
            let (members, siblings) = {
                let draft_state = self
                    .draft_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                self.source_snapshot(&draft_state, trigger)?
            };
            trigger_in_group = members.iter().any(|member| member.inode == trigger);
            if members.is_empty() {
                return Ok(());
            }

            // Validation reads the disk and runs the project check without the draft lock.
            let verdicts = source_verdicts(&writer, &root, &members);
            let sibling_disk: Vec<(u64, String, Option<Vec<u8>>)> = siblings
                .into_iter()
                .map(|(inode, path)| {
                    let bytes = disk_bytes(&root, &path);
                    (inode, path, bytes)
                })
                .collect();

            let mut draft_state = self
                .draft_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A write, release, or refusal since the snapshot invalidates the verdicts.
            if !members
                .iter()
                .all(|member| draft_matches(draft_state.source_drafts.get(&member.inode), member))
            {
                continue;
            }
            // A sibling changed on disk after the project check read it, so the verdicts
            // no longer describe the tree being replaced. Re-project it and stop.
            let mut sibling_changed = false;
            for (inode, path, expected) in &sibling_disk {
                let current = disk_bytes(&root, path);
                if &current != expected {
                    sibling_changed = true;
                    if let Some(bytes) = current {
                        self.set_source_node_bytes(*inode, bytes);
                    }
                }
            }
            if sibling_changed {
                eprintln!("source commit stopped: a sibling source module changed on disk");
                return group_result(trigger_in_group, DocumentCommitError::Stale);
            }

            // Culprits are refused and retained; the rest of the group stays pending.
            if verdicts
                .iter()
                .any(|verdict| !matches!(verdict, SourceVerdict::Accept))
            {
                let mut trigger_error = None;
                for (member, verdict) in members.iter().zip(&verdicts) {
                    let (cause, error) = match verdict {
                        SourceVerdict::Accept => continue,
                        SourceVerdict::Stale => {
                            // The stale file keeps its retained candidate and re-projects the disk.
                            if let Some(bytes) = disk_bytes(&root, &member.path) {
                                self.set_source_node_bytes(member.inode, bytes);
                            }
                            (SOURCE_STALE, DocumentCommitError::Stale)
                        }
                        SourceVerdict::Invalid(detail) => (
                            SOURCE_INVALID,
                            DocumentCommitError::Rejected(detail.clone()),
                        ),
                    };
                    let failure = self.refuse_source(
                        &mut draft_state,
                        &writer,
                        member.inode,
                        &member.path,
                        cause,
                        false,
                    );
                    if member.inode == trigger {
                        trigger_error = Some(match failure {
                            Some(errno) => DocumentCommitError::Unretained { errno, cause },
                            None => error,
                        });
                    }
                }
                return if trigger_in_group {
                    Err(trigger_error.unwrap_or_else(|| {
                        DocumentCommitError::Failed(
                            "source save refused: another candidate in the group was not accepted"
                                .to_owned(),
                        )
                    }))
                } else {
                    Ok(())
                };
            }

            // Every candidate passed: the files are written together or not at all.
            if let Err(detail) = write_source_group(&root, &members) {
                return group_result(trigger_in_group, DocumentCommitError::Failed(detail));
            }
            let inodes: Vec<u64> = members.iter().map(|member| member.inode).collect();
            for inode in &inodes {
                if let Some(draft) = draft_state.source_drafts.remove(inode) {
                    self.set_source_node_bytes(*inode, draft.candidate);
                }
            }
            self.clear_source_handles(&mut draft_state, &inodes);
            return Ok(());
        }
        group_result(trigger_in_group, DocumentCommitError::Stale)
    }

    #[allow(clippy::type_complexity)]
    /// The group for one commit: every dirty draft, plus the trigger's held refusal so
    /// its own commit retries it. Returns the members and the other source modules.
    fn source_snapshot(
        &self,
        draft_state: &DraftState,
        trigger: u64,
    ) -> Result<(Vec<SourceMember>, Vec<(u64, String)>), DocumentCommitError> {
        let member_inodes: Vec<u64> = draft_state
            .source_drafts
            .iter()
            .filter(|(inode, draft)| {
                draft.is_unsynced() || (**inode == trigger && draft.is_refused())
            })
            .map(|(inode, _)| *inode)
            .collect();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut members = Vec::with_capacity(member_inodes.len());
        for inode in &member_inodes {
            let path = state.path_of(*inode).ok_or_else(|| {
                DocumentCommitError::Failed("source module path is unavailable".to_owned())
            })?;
            let draft = &draft_state.source_drafts[inode];
            members.push(SourceMember {
                inode: *inode,
                path,
                baseline: draft.baseline.clone(),
                candidate: draft.candidate.clone(),
                state: draft.status.member_state(),
            });
        }
        let siblings = state
            .source_nodes
            .iter()
            .filter(|inode| !member_inodes.contains(*inode))
            .filter_map(|inode| state.path_of(*inode).map(|path| (*inode, path)))
            .collect();
        Ok((members, siblings))
    }

    #[allow(clippy::type_complexity)]
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
        let (dirty, refused) = if let Some(draft) = draft_state.source_drafts.get(&inode) {
            (draft.is_unsynced(), draft.is_refused())
        } else {
            eprintln!("[vfs] source module {inode}: closed with no candidate; nothing to retain");
            return;
        };
        if !dirty && !refused {
            draft_state.source_drafts.remove(&inode);
            return;
        }
        if dirty && !refused {
            if let (Some(writer), Some(path)) = (self.source_writer(), self.source_path(inode)) {
                self.refuse_source(draft_state, &writer, inode, &path, SOURCE_UNSYNCED, true);
            } else {
                // Nothing can keep the candidate, so it is recorded as refused and unretained.
                eprintln!(
                    "[vfs] source module {inode}: not retained at release (no writer or path)"
                );
                if let Some(draft) = draft_state.source_drafts.get_mut(&inode) {
                    draft.refuse(
                        SourceRefusal {
                            cause: SOURCE_UNSYNCED,
                            draft: None,
                        },
                        false,
                    );
                }
            }
        }
    }

    /// Retains dirty source candidates that no commit has refused yet, at quiesce
    /// (ORNA-VFS-009). Returns the retention errno when a candidate could not be kept.
    pub(super) fn retain_unsynced_sources(&self, draft_state: &mut DraftState) -> Option<Errno> {
        let writer = self.source_writer()?;
        let pending: Vec<(u64, String)> = draft_state
            .source_drafts
            .iter()
            .filter(|(_, draft)| matches!(draft.status, SourceDraftStatus::Unsynced))
            .filter_map(|(inode, _)| self.source_path(*inode).map(|path| (*inode, path)))
            .collect();
        let mut worst = None;
        for (inode, path) in pending {
            let Some(draft) = draft_state.source_drafts.get_mut(&inode) else {
                continue;
            };
            match retain_source_candidate(&writer, &path, SOURCE_UNSYNCED, &draft.candidate) {
                Ok(id) => {
                    draft.refuse(
                        SourceRefusal {
                            cause: SOURCE_UNSYNCED,
                            draft: Some(id),
                        },
                        true,
                    );
                }
                Err(error) => worst = worst_retention_errno(worst, error.errno),
            }
        }
        worst
    }
}

/// The result of a group commit for the sync trigger: an error only when the trigger
/// is a group member. A trigger outside the group has nothing pending to report.
fn group_result(
    trigger_in_group: bool,
    error: DocumentCommitError,
) -> Result<(), DocumentCommitError> {
    if trigger_in_group { Err(error) } else { Ok(()) }
}

/// Whether the draft still holds what the snapshot captured.
fn draft_matches(draft: Option<&SourceDraft>, member: &SourceMember) -> bool {
    draft.is_some_and(|draft| {
        draft.status.member_state() == member.state
            && draft.baseline == member.baseline
            && draft.candidate == member.candidate
    })
}

/// The disk bytes of a source module, or None when the file cannot be read.
fn disk_bytes(root: &Path, path: &str) -> Option<Vec<u8>> {
    checked_project_source_path(root, path)
        .ok()
        .and_then(|disk| std::fs::read(disk).ok())
}

/// Judges each member: the disk must still hold its baseline, and its candidate must
/// pass the project check. A candidate that fails alone is invalid. A group that fails
/// only together is refused as a whole, and the verdict says so on stderr.
fn source_verdicts(
    writer: &MountWriter,
    root: &Path,
    members: &[SourceMember],
) -> Vec<SourceVerdict> {
    let mut verdicts: Vec<SourceVerdict> = members
        .iter()
        .map(|member| {
            if disk_bytes(root, &member.path).as_deref() == Some(member.baseline.as_slice()) {
                SourceVerdict::Accept
            } else {
                SourceVerdict::Stale
            }
        })
        .collect();
    if verdicts
        .iter()
        .any(|verdict| matches!(verdict, SourceVerdict::Stale))
    {
        return verdicts;
    }

    let mut texts = BTreeMap::new();
    for (index, member) in members.iter().enumerate() {
        match std::str::from_utf8(&member.candidate) {
            Ok(text) => {
                texts.insert(member.path.clone(), text.to_owned());
            }
            Err(_) => {
                verdicts[index] = SourceVerdict::Invalid(format!("{} is not UTF-8", member.path));
            }
        }
    }
    if texts.is_empty() {
        return verdicts;
    }
    let Err(detail) = admit_source_overlays(writer, &texts) else {
        return verdicts;
    };

    let mut blamed = false;
    for (index, member) in members.iter().enumerate() {
        if !matches!(verdicts[index], SourceVerdict::Accept) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&member.candidate) else {
            continue;
        };
        let alone = BTreeMap::from([(member.path.clone(), text.to_owned())]);
        if let Err(solo) = admit_source_overlays(writer, &alone) {
            verdicts[index] = SourceVerdict::Invalid(solo);
            blamed = true;
        }
    }
    if !blamed {
        eprintln!(
            "source group refused: {detail}; no candidate fails alone, so every candidate is refused"
        );
        for verdict in &mut verdicts {
            if matches!(verdict, SourceVerdict::Accept) {
                *verdict = SourceVerdict::Invalid(detail.clone());
            }
        }
    }
    verdicts
}

/// Writes every group member's candidate over its baseline. A failure restores the
/// members already written, so the group lands whole or not at all.
fn write_source_group(root: &Path, members: &[SourceMember]) -> Result<(), String> {
    for (index, member) in members.iter().enumerate() {
        let result = match checked_project_source_path(root, &member.path) {
            Ok(disk) => write_source_atomically(&disk, &member.baseline, &member.candidate),
            Err(_) => Err(std::io::Error::other("source path is unavailable")),
        };
        if let Err(error) = result {
            for done in members[..index].iter().rev() {
                let restored = checked_project_source_path(root, &done.path).is_ok_and(|disk| {
                    restore_source_after_failed_write(&disk, &done.baseline, &done.candidate)
                });
                if !restored {
                    eprintln!(
                        "source {} could not be restored after a failed group save",
                        done.path
                    );
                }
            }
            return Err(format!("source save for {} failed: {error}", member.path));
        }
    }
    Ok(())
}

/// Validates the project with the given candidates as overlays, the same check `check`
/// runs on a saved tree.
fn admit_source_overlays(
    writer: &MountWriter,
    overlays: &BTreeMap<String, String>,
) -> Result<(), String> {
    let project = orna_project::ProjectLoader::default()
        .load_with_overlays(&writer.repository, overlays)
        .map_err(|error| format!("{error:?}"))?;
    let catalogue = project
        .production_catalogue()
        .map_err(|error| format!("{error:?}"))?;
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
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
    let mut charge = orna_vfs::QuotaCharge::empty(&writer.retained_drafts.quota);
    let retained = writer.retained_drafts.retain(
        path,
        cause,
        candidate.len() as u64,
        &mut charge,
        &mut read_chunk,
    )?;
    eprintln!(
        "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
        retained.id,
        retained.record.display()
    );
    Ok(retained.id)
}
