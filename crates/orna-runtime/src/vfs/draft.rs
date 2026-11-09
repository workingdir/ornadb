//! EDIT-1 draft state machine and its validated fsync commit.
//!
//! One draft owns the private candidate bytes over an immutable accepted
//! image. Writes move only the candidate; `fsync` serializes against writes,
//! captures one revision, and hands it to the caller's validated activation
//! boundary. A rejected or stale revision keeps its bytes as a retained draft,
//! so no refusal silently discards a user's edit.

use std::{future::Future, sync::Arc};

use tokio::sync::Mutex;
use uuid::Uuid;

use super::spool::{CandidateSource, DraftSpool, QuotaCharge};
use super::{
    ActivationCandidate, ActivationDecision, CandidateSnapshot, DraftQuota, DraftStorage,
    EditDraftError, FsyncFailure, FsyncOutcome, SharedCacheEpoch, SnapshotPin, SnapshotReadHandle,
    VfsFileSnapshot,
};
use orna_stream::{DiagnosticClass, DiagnosticCode, SafeDiagnostic};

/// The latest rejected replacement remains separately readable even after a
/// later write starts a new candidate from its bytes.
pub struct RetainedInvalidDraft<S> {
    id: [u8; 16],
    baseline: SnapshotPin<S>,
    revision: u64,
    replacement: Arc<CandidateSnapshot>,
    diagnostic: SafeDiagnostic,
}

impl<S> Clone for RetainedInvalidDraft<S> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            baseline: self.baseline.clone(),
            revision: self.revision,
            replacement: Arc::clone(&self.replacement),
            diagnostic: self.diagnostic,
        }
    }
}

impl<S> RetainedInvalidDraft<S> {
    /// The unique local-state identity of this retained draft, issued when the
    /// rejection was recorded. A later rejection of the same revision keeps the
    /// identity it was first given (ORNA-VFS-009).
    pub const fn id(&self) -> [u8; 16] {
        self.id
    }

    pub fn baseline(&self) -> &SnapshotPin<S> {
        &self.baseline
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The retained candidate bytes, read from the retained view. The copy is
    /// made only for the caller, so the retained record holds no bytes in RAM.
    pub fn replacement_bytes(&self) -> Result<Vec<u8>, EditDraftError> {
        self.replacement
            .to_arc()
            .map(|bytes| bytes.to_vec())
            .map_err(|_| EditDraftError::BackingIo)
    }

    pub fn diagnostic(&self) -> SafeDiagnostic {
        self.diagnostic
    }
}

/// The latest terminal outcome recorded for one draft revision.
///
/// A draft has at most one current rejection or acceptance. Accepting a
/// revision clears the rejection it supersedes, and rejecting a later revision
/// makes the earlier acceptance unreachable: a draft with a candidate can
/// never serve its accepted image (ORNA-VFS-008).
pub(super) enum FsyncMemo<S> {
    /// No activation outcome has been recorded since the draft last changed.
    None,
    /// Activation refused this revision with a stable diagnostic. Repeating
    /// `fsync` returns it without calling the activation boundary again.
    Rejected {
        revision: u64,
        diagnostic: SafeDiagnostic,
    },
    /// Activation accepted this revision and published this image.
    Accepted {
        revision: u64,
        image: Arc<VfsFileSnapshot<S>>,
    },
}

pub(super) struct DraftState<S> {
    pub(super) quota: Arc<DraftQuota>,
    pub(super) baseline: Arc<VfsFileSnapshot<S>>,
    pub(super) candidate: Option<DraftSpool>,
    pub(super) revision: u64,
    pub(super) memo: FsyncMemo<S>,
    pub(super) retained_invalid: Option<RetainedRejection<S>>,
}

/// The retained rejected draft together with the identity of the rejection
/// that recorded it. One rejection has one identity, so a repeated validation
/// of the same revision is distinguishable from a fresh one (ORNA-VFS-009).
/// The retained copy is charged to the draft quota for as long as it is held
/// (profiles/vfs-1.md: rejected candidates are quota accounted).
pub(super) struct RetainedRejection<S> {
    backing: RetentionBacking,
    draft: RetainedInvalidDraft<S>,
}

/// How a retained rejected candidate holds its quota charge.
pub(super) enum RetentionBacking {
    /// The retained view aliases the live candidate spool's file, so the spool
    /// holds the charge until the candidate changes and the bytes move here.
    SharesCandidateSpool,
    /// The retained view owns its bytes; the charge is a drop guard that
    /// releases them when the retained copy goes away.
    Owned { _charge: QuotaCharge },
}

impl RetentionBacking {
    /// Moves a replaced candidate spool's charge into this backing.
    ///
    /// Only an aliasing backing takes it: an owned backing does not reference
    /// those bytes, so the spool's charge must be released with the spool at
    /// the moment it drops rather than delayed until the retained copy does.
    fn absorb_candidate_spool(&mut self, quota: &Arc<DraftQuota>, spool: &mut QuotaCharge) {
        match self {
            Self::SharesCandidateSpool => {
                let mut owned = QuotaCharge::empty(quota);
                owned.absorb(spool);
                *self = Self::Owned { _charge: owned };
            }
            Self::Owned { .. } => {}
        }
    }
}

impl<S> DraftState<S> {
    fn candidate_len(&self) -> u64 {
        self.candidate
            .as_ref()
            .map_or(self.baseline.bytes.len(), |spool| spool.len)
    }

    /// Materialises the whole candidate for activation. Ranged reads and writes
    /// stay on the spool, and a spool read failure is a typed backing error.
    fn candidate_arc(&self) -> Result<Arc<[u8]>, EditDraftError> {
        match &self.candidate {
            Some(spool) => spool.read_all().map_err(|_| EditDraftError::BackingIo),
            None => self
                .baseline
                .bytes
                .to_arc()
                .map_err(|_| EditDraftError::BackingIo),
        }
    }

    /// Opens a streaming view of the candidate for one fsync callback. A spool
    /// is shared through a cloned handle and the baseline through its `Arc`, so
    /// no candidate bytes are copied. Writers hold the operation lock, so the
    /// candidate cannot change while the view is in use.
    pub(super) fn candidate_snapshot(&self) -> Result<CandidateSnapshot, EditDraftError> {
        match &self.candidate {
            Some(spool) => Ok(CandidateSnapshot {
                len: spool.len,
                source: CandidateSource::Spool(
                    spool
                        .file
                        .try_clone()
                        .map_err(|_| EditDraftError::BackingIo)?,
                ),
            }),
            None => self
                .baseline
                .bytes
                .duplicate()
                .map_err(|_| EditDraftError::BackingIo),
        }
    }

    /// Reads `[offset, offset + length)` of the candidate, clamped to its
    /// length. Holds at most `length` bytes.
    fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>, EditDraftError> {
        match &self.candidate {
            Some(spool) => spool
                .read_range(offset, length)
                .map_err(|_| EditDraftError::BackingIo),
            None => self
                .baseline
                .bytes
                .read_range(offset, length)
                .map_err(|_| EditDraftError::BackingIo),
        }
    }

    /// Returns the private spool, creating it from the baseline on first use.
    fn ensure_spool(&mut self, storage: &DraftStorage) -> Result<&mut DraftSpool, EditDraftError> {
        if self.candidate.is_none() {
            self.candidate = Some(DraftSpool::from_baseline(&self.baseline.bytes, storage)?);
        } else if self.retained_is_live() {
            self.unalias_retained(storage)?;
        }
        self.candidate.as_mut().ok_or(EditDraftError::BackingIo)
    }

    /// Records one terminal rejection of `revision`, issuing a fresh identity
    /// unless the same revision was already retained. The rejection is cached
    /// only once its bytes are retained, so a failed retention stays retryable.
    pub(super) fn retain_rejection(
        &mut self,
        revision: u64,
        diagnostic: SafeDiagnostic,
    ) -> Result<(), EditDraftError> {
        self.retain_draft(revision, diagnostic)?;
        self.memo = FsyncMemo::Rejected {
            revision,
            diagnostic,
        };
        Ok(())
    }

    /// Retains a stale compare-and-swap candidate without caching the outcome.
    /// A caller may retry these same bytes against a newly captured baseline.
    pub(super) fn retain_retryable_conflict(
        &mut self,
        revision: u64,
        diagnostic: SafeDiagnostic,
    ) -> Result<(), EditDraftError> {
        self.retain_draft(revision, diagnostic)
    }

    /// Keeps the candidate of `revision` as its retained copy without copying
    /// its bytes. While the candidate is a spool, the copy aliases that spool's
    /// file and the candidate keeps the charge. A baseline-only candidate shares
    /// the baseline bytes and reserves their length. A revision keeps its identity
    /// and its existing copy. A new revision reserves before anything is
    /// replaced, so a refused reservation (EDQUOT) leaves the previous copy in place.
    fn retain_draft(
        &mut self,
        revision: u64,
        diagnostic: SafeDiagnostic,
    ) -> Result<(), EditDraftError> {
        if let Some(previous) = self.retained_invalid.as_mut()
            && previous.draft.revision == revision
        {
            previous.draft.baseline = self.baseline.pin.clone();
            previous.draft.diagnostic = diagnostic;
            return Ok(());
        }
        let replacement = Arc::new(self.candidate_snapshot()?);
        let backing = if self.candidate.is_some() {
            RetentionBacking::SharesCandidateSpool
        } else {
            RetentionBacking::Owned {
                _charge: QuotaCharge::reserve(&self.quota, replacement.len())?,
            }
        };
        let id = *Uuid::new_v4().as_bytes();
        self.retained_invalid = Some(RetainedRejection {
            backing,
            draft: RetainedInvalidDraft {
                id,
                baseline: self.baseline.pin.clone(),
                revision,
                replacement,
                diagnostic,
            },
        });
        Ok(())
    }

    /// Whether the retained copy aliases the live candidate's spool file.
    fn retained_is_live(&self) -> bool {
        self.retained_invalid.as_ref().is_some_and(|retained| {
            matches!(&retained.backing, RetentionBacking::SharesCandidateSpool)
        })
    }

    /// Moves the retained copy off the candidate spool before the spool changes.
    /// The old spool's file and its charge move to the retained copy, and the
    /// candidate continues on a copy of those bytes. A refused copy (EDQUOT)
    /// changes nothing.
    fn unalias_retained(&mut self, storage: &DraftStorage) -> Result<(), EditDraftError> {
        let copy = DraftSpool::from_baseline(&self.candidate_snapshot()?, storage)?;
        let mut previous = self
            .candidate
            .replace(copy)
            .ok_or(EditDraftError::BackingIo)?;
        if let Some(retained) = self.retained_invalid.as_mut() {
            retained
                .backing
                .absorb_candidate_spool(&self.quota, &mut previous.charge);
        }
        Ok(())
    }

    /// Drops the candidate spool. A retained copy that aliases it takes over the
    /// spool's charge, so the bytes stay charged while they are retained.
    pub(super) fn drop_candidate(&mut self) {
        if let Some(mut spool) = self.candidate.take()
            && let Some(retained) = self.retained_invalid.as_mut()
        {
            retained
                .backing
                .absorb_candidate_spool(&self.quota, &mut spool.charge);
        }
    }

    /// The diagnostic activation recorded for this exact revision, if it
    /// refused it. A later revision never reads an earlier refusal.
    pub(super) fn rejection_for(&self, revision: u64) -> Option<SafeDiagnostic> {
        match &self.memo {
            FsyncMemo::Rejected {
                revision: rejected,
                diagnostic,
            } if *rejected == revision => Some(*diagnostic),
            _ => None,
        }
    }

    /// The image activation recorded for this exact revision, if it accepted it.
    pub(super) fn accepted_for(&self, revision: u64) -> Option<&Arc<VfsFileSnapshot<S>>> {
        match &self.memo {
            FsyncMemo::Accepted {
                revision: accepted,
                image,
            } if *accepted == revision => Some(image),
            _ => None,
        }
    }
}

/// A private EDIT-1 replacement draft. Writes land in a disk-backed spool
/// charged to a shared quota; fsync serializes against writes and invokes the
/// caller's validated table activation path. Dropping or releasing the draft
/// never accepts it.
pub struct EditDraft<S> {
    pub(super) state: Mutex<DraftState<S>>,
    pub(super) operation: Mutex<()>,
    max_file_bytes: usize,
    storage: DraftStorage,
}

/// A retained `orna edit` read-to-save baseline.
///
/// The image is the exact destination version the editor read at capture time,
/// kept readable for the whole edit, and the draft holds this edit's private
/// bytes over it. [`ManagedFile::save_strong_with`] is its only accepting route:
/// a save proves the retained image is still the accepted version before the
/// candidate reaches activation. A superseded read still reads its own version,
/// because this type owns the retained image — a later accepted replacement
/// only ends what new opens observe, never what this handle already holds.
pub struct EditReadBaseline<S> {
    pub(super) image: Arc<VfsFileSnapshot<S>>,
    pub(super) draft: EditDraft<S>,
    pub(super) epoch: SharedCacheEpoch,
}

impl<S> EditReadBaseline<S> {
    /// Opens the retained read version. This reads the exact image captured at
    /// edit start, never the destination's live image.
    pub fn baseline(&self) -> SnapshotReadHandle<S> {
        SnapshotReadHandle::open(Arc::clone(&self.image))
    }

    /// This edit's private draft over the retained read version.
    pub fn draft(&self) -> &EditDraft<S> {
        &self.draft
    }
}

/// The safe diagnostic a superseded read-to-save baseline reports. It carries
/// no repository detail: the retained draft and its bytes are what the caller
/// keeps, and the code is the profile's `sys.vfs.stale_edit` refusal.
pub(super) fn stale_baseline_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::StaleBaseline,
        class: DiagnosticClass::Transient,
    }
}

impl<S> EditDraft<S> {
    /// Opens a draft whose candidate spool is created under `storage`.
    pub fn open(
        image: Arc<VfsFileSnapshot<S>>,
        max_file_bytes: usize,
        storage: DraftStorage,
    ) -> Self {
        Self {
            state: Mutex::new(DraftState {
                quota: storage.quota(),
                baseline: image,
                candidate: None,
                revision: 0,
                memo: FsyncMemo::None,
                retained_invalid: None,
            }),
            operation: Mutex::new(()),
            max_file_bytes,
            storage,
        }
    }

    pub async fn baseline(&self) -> SnapshotReadHandle<S> {
        SnapshotReadHandle::open(Arc::clone(&self.state.lock().await.baseline))
    }

    /// Re-anchors an accepted draft on the projected image its destination now
    /// holds, so the next commit's CAS compares against the live image.
    pub(super) async fn rebase_accepted(&self, image: Arc<VfsFileSnapshot<S>>) {
        self.state.lock().await.baseline = image;
    }

    /// Applies a positional write, zero-filling any gap in the private copy.
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let length = u64::try_from(bytes.len()).map_err(|_| EditDraftError::OffsetOverflow)?;
        let end = offset
            .checked_add(length)
            .ok_or(EditDraftError::OffsetOverflow)?;

        let _operation = self.operation.lock().await;
        let mut guard = self.state.lock().await;
        let state = &mut *guard;
        let max_file_bytes = u64::try_from(self.max_file_bytes).unwrap_or(u64::MAX);
        if end > state.candidate_len() && end > max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        // A quota or offset refusal happens before any byte moves, so it
        // changes nothing and keeps the revision. A backing failure can leave
        // a partial pwrite, so it advances the revision rather than vouch for
        // the old contents.
        let result = state
            .ensure_spool(&self.storage)
            .and_then(|spool| spool.write_at(offset, bytes));
        if !matches!(
            result,
            Err(EditDraftError::QuotaExceeded | EditDraftError::OffsetOverflow)
        ) {
            state.revision = next_revision;
        }
        result
    }

    /// Changes the candidate length. Extending creates zero-filled holes.
    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let _operation = self.operation.lock().await;
        let mut guard = self.state.lock().await;
        let state = &mut *guard;
        let max_file_bytes = u64::try_from(self.max_file_bytes).unwrap_or(u64::MAX);
        let current_len = state.candidate_len();
        if length > current_len && length > max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        if current_len == length {
            return Ok(());
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        let result = state
            .ensure_spool(&self.storage)
            .and_then(|spool| spool.set_len(length));
        if result.is_ok() {
            state.revision = next_revision;
        }
        result
    }
    /// Whole-value copy for commit and activation only, which own the value.
    /// Live-mount reads, writes and retention use [`Self::read_range`].
    pub async fn candidate_bytes(&self) -> Result<Arc<[u8]>, EditDraftError> {
        let state = self.state.lock().await;
        state.candidate_arc()
    }

    /// A streaming view of the candidate that may be retained past the commit. A
    /// spool is shared through a duplicate handle, so no candidate bytes are copied.
    pub async fn candidate_snapshot(&self) -> Result<CandidateSnapshot, EditDraftError> {
        let state = self.state.lock().await;
        state.candidate_snapshot()
    }

    /// The candidate's length in bytes.
    pub async fn candidate_len(&self) -> u64 {
        self.state.lock().await.candidate_len()
    }

    /// Moves the live candidate's quota charge to the caller, which returns
    /// what is left through [`Self::restore_candidate_charge`]. A baseline-only
    /// candidate holds no charge, so this returns an empty one.
    pub async fn take_candidate_charge(&self) -> QuotaCharge {
        let mut guard = self.state.lock().await;
        let state = &mut *guard;
        match state.candidate.as_mut() {
            Some(spool) => spool.charge.take(),
            None => QuotaCharge::empty(&state.quota),
        }
    }

    /// Returns a charge taken by [`Self::take_candidate_charge`] to the live
    /// candidate. Bytes still held when no candidate is live are released.
    pub async fn restore_candidate_charge(&self, mut charge: QuotaCharge) {
        let mut guard = self.state.lock().await;
        if let Some(spool) = guard.candidate.as_mut() {
            spool.charge.absorb(&mut charge);
        }
    }

    /// Whether the draft holds submitted candidate bytes. A draft that never
    /// submitted bytes has nothing for a commit to accept or refuse, the same
    /// condition `commit_durable_save` tests before it judges staleness.
    pub async fn has_candidate(&self) -> bool {
        self.state.lock().await.candidate.is_some()
    }

    /// Reads `[offset, offset + length)` of the candidate, clamped to its
    /// length. Live-mount paths pass a chunk-sized `length`, at most
    /// [`CANDIDATE_CHUNK_BYTES`], so no whole copy is made.
    pub async fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>, EditDraftError> {
        self.state.lock().await.read_range(offset, length)
    }

    pub async fn retained_invalid_draft(&self) -> Option<RetainedInvalidDraft<S>> {
        self.state
            .lock()
            .await
            .retained_invalid
            .as_ref()
            .map(|retained| retained.draft.clone())
    }

    /// Calls the validated activation adapter exactly once for a changed
    /// revision. A failed callback leaves the draft retryable; a validation
    /// rejection is cached and retained until the draft changes or is dropped.
    pub async fn fsync_with<F, Fut, E>(
        &self,
        activate: F,
    ) -> Result<FsyncOutcome<S>, FsyncFailure<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        let _operation = self.operation.lock().await;
        let mut state = self.state.lock().await;
        if state.candidate.is_none() {
            if let FsyncMemo::Accepted { revision, image } = &state.memo
                && *revision == state.revision
            {
                return Ok(FsyncOutcome::Accepted(SnapshotReadHandle::open(
                    Arc::clone(image),
                )));
            }
            return Ok(FsyncOutcome::Unchanged(SnapshotReadHandle::open(
                Arc::clone(&state.baseline),
            )));
        }
        if let FsyncMemo::Rejected {
            revision,
            diagnostic,
        } = &state.memo
            && *revision == state.revision
        {
            return Ok(FsyncOutcome::Rejected(*diagnostic));
        }

        let revision = state.revision;
        let bytes = state.candidate_arc().map_err(FsyncFailure::Backing)?;
        let candidate = ActivationCandidate {
            baseline: state.baseline.pin.clone(),
            revision,
            replacement: Arc::clone(&bytes),
            removal: false,
        };
        let decision = activate(candidate)
            .await
            .map_err(FsyncFailure::Activation)?;
        match decision {
            ActivationDecision::Accepted(pin) => {
                let image = Arc::new(VfsFileSnapshot::new(pin, bytes));
                state.drop_candidate();
                state.baseline = Arc::clone(&image);
                state.memo = FsyncMemo::Accepted {
                    revision,
                    image: Arc::clone(&image),
                };
                Ok(FsyncOutcome::Accepted(SnapshotReadHandle::open(image)))
            }
            ActivationDecision::Rejected(diagnostic) => {
                state
                    .retain_rejection(revision, diagnostic)
                    .map_err(FsyncFailure::Backing)?;
                Ok(FsyncOutcome::Rejected(diagnostic))
            }
        }
    }

    /// Explicit close path. It drops private state and never invokes activation.
    pub fn release(self) {}
}

/// Keeps a refused draft's bytes and its stable diagnostic, so a rejected
/// strong save is recoverable and is never silently dropped.
///
/// This records the rejection through the draft's own retention, so a strong
/// save that is refused because its read baseline went stale keeps the same
/// retained-draft identity rules as every other rejection (ORNA-VFS-009): the
/// identity is issued once per rejected revision and reused while that
/// revision stays unchanged.
///
/// [`DraftState::retain_rejection`] is the one authority for a retained
/// rejection, so this route and the direct draft routes cannot disagree about
/// a draft's retained identity.
pub(super) async fn retain_rejected_draft<S>(
    draft: &EditDraft<S>,
    diagnostic: SafeDiagnostic,
) -> Result<(), EditDraftError> {
    let mut draft_state = draft.state.lock().await;
    let revision = draft_state.revision;
    draft_state.retain_rejection(revision, diagnostic)
}
