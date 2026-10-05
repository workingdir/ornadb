//! FUSE-free state for projected file handles and EDIT-1 replacement drafts.
//!
//! `S` is the repository owner's opaque row-map snapshot (in production,
//! `orna_repository_v1::row_store::RowMapSnapshot`). Keeping that value in a
//! [`SnapshotPin`] binds each open handle and readdir cursor to one immutable
//! row-map version without giving this module a second row authority.

use std::{future::Future, sync::Arc};

use tokio::sync::Mutex;

use super::{
    CwdCapture, DiagnosticClass, DiagnosticCode, RuntimeState, SafeDiagnostic,
    TableActivationError, ValidatedTableActivationCommit,
};

/// One repository-wide invalidation clock shared by directory, attribute,
/// and file-handle projections. A projection is current only while its
/// captured generation still matches this clock.
#[derive(Clone, Default)]
pub struct SharedCacheEpoch {
    generation: Arc<Mutex<u64>>,
}

/// A projection's immutable observation of a shared cache epoch.
#[derive(Clone)]
pub struct CacheProjection {
    epoch: SharedCacheEpoch,
    generation: u64,
}

impl SharedCacheEpoch {
    pub async fn capture(&self) -> CacheProjection {
        let generation = *self.generation.lock().await;
        CacheProjection {
            epoch: self.clone(),
            generation,
        }
    }

    pub async fn generation(&self) -> u64 {
        *self.generation.lock().await
    }
}

impl CacheProjection {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub async fn is_current(&self) -> bool {
        self.generation == *self.epoch.generation.lock().await
    }
}

/// A retained owner-issued immutable row-map snapshot.
pub struct SnapshotPin<S>(Arc<S>);

impl<S> Clone for SnapshotPin<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S> SnapshotPin<S> {
    /// Retains an opaque repository snapshot. The snapshot carries its own
    /// immutable version identity and remains the only row lookup authority.
    pub fn capture(snapshot: Arc<S>) -> Self {
        Self(snapshot)
    }

    /// Borrows the same owner-issued snapshot captured by this pin.
    pub fn snapshot(&self) -> &S {
        &self.0
    }

    /// Clones the pin for another handle that must remain on this version.
    pub fn clone_pin(&self) -> Self {
        self.clone()
    }
}

/// Immutable projected file bytes paired with the row-map snapshot that
/// admitted them.
pub struct VfsFileSnapshot<S> {
    pin: SnapshotPin<S>,
    bytes: Arc<[u8]>,
}

impl<S> VfsFileSnapshot<S> {
    pub fn new(pin: SnapshotPin<S>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            pin,
            bytes: bytes.into(),
        }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A read-only open file; its bytes and row-map snapshot never advance.
pub struct SnapshotReadHandle<S> {
    image: Arc<VfsFileSnapshot<S>>,
    projection: Option<CacheProjection>,
}

impl<S> Clone for SnapshotReadHandle<S> {
    fn clone(&self) -> Self {
        Self {
            image: Arc::clone(&self.image),
            projection: self.projection.clone(),
        }
    }
}

impl<S> SnapshotReadHandle<S> {
    pub fn open(image: Arc<VfsFileSnapshot<S>>) -> Self {
        Self {
            image,
            projection: None,
        }
    }

    pub fn open_projected(image: Arc<VfsFileSnapshot<S>>, projection: CacheProjection) -> Self {
        Self {
            image,
            projection: Some(projection),
        }
    }

    /// The cache observation attached to this handle, when it came from a
    /// managed repository view. Its pinned bytes remain readable if it goes
    /// stale; callers can refresh the projection against the shared epoch.
    pub fn projection(&self) -> Option<&CacheProjection> {
        self.projection.as_ref()
    }

    pub async fn projection_is_current(&self) -> Option<bool> {
        match &self.projection {
            Some(projection) => Some(projection.is_current().await),
            None => None,
        }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        self.image.pin()
    }

    pub fn len(&self) -> u64 {
        self.image.len()
    }

    pub fn is_empty(&self) -> bool {
        self.image.is_empty()
    }

    /// Reads at most `limit` bytes from this handle's captured image.
    pub fn read_at(&self, offset: u64, limit: usize) -> Vec<u8> {
        let Ok(start) = usize::try_from(offset) else {
            return Vec::new();
        };
        if start >= self.image.bytes.len() || limit == 0 {
            return Vec::new();
        }
        let end = start.saturating_add(limit).min(self.image.bytes.len());
        self.image.bytes[start..end].to_vec()
    }
}

/// A readdir continuation paired with the exact snapshot used for its scan.
/// The owner-provided continuation value can never be detached from that pin.
pub struct SnapshotReaddirCursor<S, C> {
    pin: SnapshotPin<S>,
    continuation: C,
    projection: Option<CacheProjection>,
}

impl<S, C> SnapshotReaddirCursor<S, C> {
    pub fn new(pin: SnapshotPin<S>, continuation: C) -> Self {
        Self {
            pin,
            continuation,
            projection: None,
        }
    }

    pub fn new_projected(
        pin: SnapshotPin<S>,
        continuation: C,
        projection: CacheProjection,
    ) -> Self {
        Self {
            pin,
            continuation,
            projection: Some(projection),
        }
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    pub fn continuation(&self) -> &C {
        &self.continuation
    }

    pub fn continuation_mut(&mut self) -> &mut C {
        &mut self.continuation
    }

    pub fn projection(&self) -> Option<&CacheProjection> {
        self.projection.as_ref()
    }

    pub async fn projection_is_current(&self) -> Option<bool> {
        match &self.projection {
            Some(projection) => Some(projection.is_current().await),
            None => None,
        }
    }
}

/// An owned copy-on-write candidate passed to the table activation adapter.
pub struct ActivationCandidate<S> {
    baseline: SnapshotPin<S>,
    revision: u64,
    replacement: Arc<[u8]>,
}

impl<S> ActivationCandidate<S> {
    pub fn baseline(&self) -> &SnapshotPin<S> {
        &self.baseline
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replacement_bytes(&self) -> &[u8] {
        &self.replacement
    }
}

/// The accepted replacement's new repository snapshot, or its stable safe
/// validation diagnostic. Rejection never discards the candidate.
pub enum ActivationDecision<S> {
    Accepted(SnapshotPin<S>),
    Rejected(SafeDiagnostic),
}

/// The result of fsync. Repeated syncs of a rejected revision return the same
/// diagnostic without calling the activation boundary again.
pub enum FsyncOutcome<S> {
    Accepted(SnapshotReadHandle<S>),
    Rejected(SafeDiagnostic),
    Unchanged(SnapshotReadHandle<S>),
}

/// The latest rejected replacement remains separately readable even after a
/// later write starts a new candidate from its bytes.
pub struct RetainedInvalidDraft<S> {
    baseline: SnapshotPin<S>,
    revision: u64,
    replacement: Arc<[u8]>,
    diagnostic: SafeDiagnostic,
}

impl<S> Clone for RetainedInvalidDraft<S> {
    fn clone(&self) -> Self {
        Self {
            baseline: self.baseline.clone(),
            revision: self.revision,
            replacement: Arc::clone(&self.replacement),
            diagnostic: self.diagnostic,
        }
    }
}

impl<S> RetainedInvalidDraft<S> {
    pub fn baseline(&self) -> &SnapshotPin<S> {
        &self.baseline
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn replacement_bytes(&self) -> &[u8] {
        &self.replacement
    }

    pub fn diagnostic(&self) -> SafeDiagnostic {
        self.diagnostic
    }
}

struct DraftState<S> {
    baseline: Arc<VfsFileSnapshot<S>>,
    candidate: Option<Vec<u8>>,
    revision: u64,
    last_rejection: Option<(u64, SafeDiagnostic)>,
    retained_invalid: Option<RetainedInvalidDraft<S>>,
}

/// A private EDIT-1 replacement draft. Writes use a copy-on-write buffer;
/// fsync serializes against writes and invokes the caller's validated table
/// activation path. Dropping or releasing the draft never accepts it.
pub struct EditDraft<S> {
    state: Mutex<DraftState<S>>,
    max_file_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditDraftError {
    FileTooLarge,
    OffsetOverflow,
    RevisionExhausted,
    AlreadyApplied,
}

struct ManagedFileState<S> {
    image: Arc<VfsFileSnapshot<S>>,
}

/// One already-managed destination. New opens observe its latest accepted
/// image; open handles keep the immutable image captured when they opened.
pub struct ManagedFile<S> {
    state: Mutex<ManagedFileState<S>>,
    cache_epoch: SharedCacheEpoch,
}

impl<S> ManagedFile<S> {
    pub fn new(image: Arc<VfsFileSnapshot<S>>) -> Arc<Self> {
        Self::with_cache_epoch(image, SharedCacheEpoch::default())
    }

    /// Creates a managed file in a shared repository cache epoch. All files
    /// and projected caches in one repository view should use the same epoch.
    pub fn with_cache_epoch(
        image: Arc<VfsFileSnapshot<S>>,
        cache_epoch: SharedCacheEpoch,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ManagedFileState { image }),
            cache_epoch,
        })
    }

    pub async fn open_read(&self) -> SnapshotReadHandle<S> {
        let state = self.state.lock().await;
        let image = Arc::clone(&state.image);
        let projection = self.cache_epoch.capture().await;
        SnapshotReadHandle::open_projected(image, projection)
    }

    /// Returns the repository-wide invalidation generation.
    pub async fn cache_generation(&self) -> u64 {
        self.cache_epoch.generation().await
    }

    pub fn shared_cache_epoch(&self) -> SharedCacheEpoch {
        self.cache_epoch.clone()
    }

    /// Creates a private sibling-save candidate and captures this destination's
    /// current immutable baseline for rename-time CAS.
    pub async fn begin_temporary_replacement(
        self: &Arc<Self>,
        max_file_bytes: usize,
    ) -> TemporarySave<S> {
        let baseline = Arc::clone(&self.state.lock().await.image);
        let draft = EditDraft::open(Arc::clone(&baseline), max_file_bytes);
        // A typical editor temp starts empty; its commit baseline remains the
        // managed destination image captured above.
        draft
            .truncate(0)
            .await
            .expect("zero-length scratch is within every file quota");
        TemporarySave {
            target: Arc::clone(self),
            baseline,
            state: Mutex::new(TemporarySaveState {
                draft,
                synced_revision: None,
                applied: None,
            }),
        }
    }

    /// Applies a sibling temporary file over this managed destination. The
    /// temp must have been created from this exact entry; stale and rejected
    /// candidates keep their scratch bytes and leave this image untouched.
    pub async fn rename_over<F, Fut, E>(
        self: &Arc<Self>,
        scratch: &TemporarySave<S>,
        activate: F,
    ) -> Result<TemporaryRenameOutcome<S>, TemporaryRenameError<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        if !Arc::ptr_eq(self, &scratch.target) {
            return Err(TemporaryRenameError::WrongDestination);
        }

        let mut destination = self.state.lock().await;
        let mut scratch_state = scratch.state.lock().await;
        if let Some((generation, handle)) = &scratch_state.applied {
            return Ok(TemporaryRenameOutcome::Applied {
                generation: *generation,
                handle: handle.clone(),
            });
        }
        if !Arc::ptr_eq(&destination.image, &scratch.baseline) {
            let diagnostic = SafeDiagnostic {
                code: DiagnosticCode::ExecutionRejected,
                class: DiagnosticClass::Transient,
            };
            let mut draft = scratch_state.draft.state.lock().await;
            let revision = draft.revision;
            let replacement: Arc<[u8]> = Arc::from(
                draft
                    .candidate
                    .as_deref()
                    .unwrap_or(draft.baseline.bytes.as_ref()),
            );
            draft.last_rejection = Some((revision, diagnostic));
            draft.retained_invalid = Some(RetainedInvalidDraft {
                baseline: scratch.baseline.pin.clone(),
                revision,
                replacement,
                diagnostic,
            });
            return Ok(TemporaryRenameOutcome::Stale { diagnostic });
        }
        // Serialize commits in this repository epoch and retain the lock over
        // activation, so no projection can observe a partially advanced
        // generation. Failed/rejected candidates leave the epoch unchanged.
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;

        match scratch_state.draft.fsync_with(activate).await {
            Err(error) => Err(TemporaryRenameError::Activation(error)),
            Ok(FsyncOutcome::Rejected(diagnostic)) => {
                Ok(TemporaryRenameOutcome::Rejected { diagnostic })
            }
            Ok(FsyncOutcome::Accepted(handle)) => {
                destination.image = Arc::clone(&handle.image);
                *cache_generation = next_generation;
                let handle = SnapshotReadHandle::open_projected(
                    Arc::clone(&handle.image),
                    CacheProjection {
                        epoch: self.cache_epoch.clone(),
                        generation: next_generation,
                    },
                );
                scratch_state.applied = Some((next_generation, handle.clone()));
                Ok(TemporaryRenameOutcome::Applied {
                    generation: next_generation,
                    handle,
                })
            }
            Ok(FsyncOutcome::Unchanged(_)) => Err(TemporaryRenameError::NoCandidate),
        }
    }
}

struct TemporarySaveState<S> {
    draft: EditDraft<S>,
    synced_revision: Option<u64>,
    applied: Option<(u64, SnapshotReadHandle<S>)>,
}

/// A temp-file save tied to the managed destination that admitted its
/// creation. Scratch fsync is a persistence callback only; row acceptance is
/// performed by renaming it over the destination.
pub struct TemporarySave<S> {
    target: Arc<ManagedFile<S>>,
    baseline: Arc<VfsFileSnapshot<S>>,
    state: Mutex<TemporarySaveState<S>>,
}

impl<S> TemporarySave<S> {
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        let state = self.state.lock().await;
        if state.applied.is_some() {
            return Err(EditDraftError::AlreadyApplied);
        }
        state.draft.write_at(offset, bytes).await
    }

    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let state = self.state.lock().await;
        if state.applied.is_some() {
            return Err(EditDraftError::AlreadyApplied);
        }
        state.draft.truncate(length).await
    }

    pub async fn candidate_bytes(&self) -> Arc<[u8]> {
        self.state.lock().await.draft.candidate_bytes().await
    }

    pub async fn retained_invalid_draft(&self) -> Option<RetainedInvalidDraft<S>> {
        self.state.lock().await.draft.retained_invalid_draft().await
    }

    /// Persists only the private scratch sequence. This callback does not call
    /// the row activation boundary and is skipped for an already-synced seq.
    pub async fn fsync_scratch_with<F, Fut, E>(&self, persist: F) -> Result<u64, E>
    where
        F: FnOnce(u64, Arc<[u8]>) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let mut state = self.state.lock().await;
        if let Some((revision, _)) = &state.applied {
            return Ok(*revision);
        }
        let (revision, bytes) = {
            let draft = state.draft.state.lock().await;
            let revision = draft.revision;
            let bytes: Arc<[u8]> = Arc::from(
                draft
                    .candidate
                    .as_deref()
                    .unwrap_or(draft.baseline.bytes.as_ref()),
            );
            (revision, bytes)
        };
        if state.synced_revision == Some(revision) {
            return Ok(revision);
        }
        persist(revision, bytes).await?;
        state.synced_revision = Some(revision);
        Ok(revision)
    }
}

pub enum TemporaryRenameOutcome<S> {
    Applied {
        generation: u64,
        handle: SnapshotReadHandle<S>,
    },
    Rejected {
        diagnostic: SafeDiagnostic,
    },
    Stale {
        diagnostic: SafeDiagnostic,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub enum TemporaryRenameError<E> {
    WrongDestination,
    GenerationExhausted,
    Activation(E),
    NoCandidate,
}

impl<S> EditDraft<S> {
    pub fn open(image: Arc<VfsFileSnapshot<S>>, max_file_bytes: usize) -> Self {
        Self {
            state: Mutex::new(DraftState {
                baseline: image,
                candidate: None,
                revision: 0,
                last_rejection: None,
                retained_invalid: None,
            }),
            max_file_bytes,
        }
    }

    pub async fn baseline(&self) -> SnapshotReadHandle<S> {
        SnapshotReadHandle::open(Arc::clone(&self.state.lock().await.baseline))
    }

    /// Applies a positional write, zero-filling any gap in the private copy.
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let start = usize::try_from(offset).map_err(|_| EditDraftError::OffsetOverflow)?;
        let end = start
            .checked_add(bytes.len())
            .ok_or(EditDraftError::OffsetOverflow)?;
        if end > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }

        let mut state = self.state.lock().await;
        let current_len = state
            .candidate
            .as_ref()
            .map_or(state.baseline.bytes.len(), Vec::len);
        if current_len.max(end) > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        let baseline_bytes = Arc::clone(&state.baseline.bytes);
        let candidate = state
            .candidate
            .get_or_insert_with(|| baseline_bytes.to_vec());
        candidate.resize(candidate.len().max(end), 0);
        candidate[start..end].copy_from_slice(bytes);
        state.revision = next_revision;
        Ok(())
    }

    /// Changes the candidate length. Extending creates zero-filled holes.
    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let length = usize::try_from(length).map_err(|_| EditDraftError::OffsetOverflow)?;
        if length > self.max_file_bytes {
            return Err(EditDraftError::FileTooLarge);
        }
        let mut state = self.state.lock().await;
        let current_len = state
            .candidate
            .as_ref()
            .map_or(state.baseline.bytes.len(), Vec::len);
        if current_len == length {
            return Ok(());
        }
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(EditDraftError::RevisionExhausted)?;
        let baseline_bytes = Arc::clone(&state.baseline.bytes);
        let candidate = state
            .candidate
            .get_or_insert_with(|| baseline_bytes.to_vec());
        candidate.resize(length, 0);
        state.revision = next_revision;
        Ok(())
    }

    pub async fn candidate_bytes(&self) -> Arc<[u8]> {
        let state = self.state.lock().await;
        Arc::from(
            state
                .candidate
                .as_deref()
                .unwrap_or(state.baseline.bytes.as_ref()),
        )
    }

    pub async fn retained_invalid_draft(&self) -> Option<RetainedInvalidDraft<S>> {
        self.state.lock().await.retained_invalid.clone()
    }

    /// Calls the validated activation adapter exactly once for a changed
    /// revision. A failed callback leaves the draft retryable; a validation
    /// rejection is cached and retained until the draft changes or is dropped.
    pub async fn fsync_with<F, Fut, E>(&self, activate: F) -> Result<FsyncOutcome<S>, E>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        let mut state = self.state.lock().await;
        let Some(bytes) = state.candidate.as_ref() else {
            return Ok(FsyncOutcome::Unchanged(SnapshotReadHandle::open(
                Arc::clone(&state.baseline),
            )));
        };
        if let Some((revision, diagnostic)) = state.last_rejection
            && revision == state.revision
        {
            return Ok(FsyncOutcome::Rejected(diagnostic));
        }

        let revision = state.revision;
        let candidate = ActivationCandidate {
            baseline: state.baseline.pin.clone(),
            revision,
            replacement: Arc::from(bytes.as_slice()),
        };
        match activate(candidate).await? {
            ActivationDecision::Accepted(pin) => {
                let bytes: Arc<[u8]> =
                    Arc::from(state.candidate.take().expect("candidate held by lock"));
                let image = Arc::new(VfsFileSnapshot::new(pin, bytes));
                state.baseline = Arc::clone(&image);
                state.last_rejection = None;
                Ok(FsyncOutcome::Accepted(SnapshotReadHandle::open(image)))
            }
            ActivationDecision::Rejected(diagnostic) => {
                let replacement: Arc<[u8]> =
                    Arc::from(state.candidate.as_deref().expect("candidate held by lock"));
                state.last_rejection = Some((revision, diagnostic));
                state.retained_invalid = Some(RetainedInvalidDraft {
                    baseline: state.baseline.pin.clone(),
                    revision,
                    replacement,
                    diagnostic,
                });
                Ok(FsyncOutcome::Rejected(diagnostic))
            }
        }
    }

    /// Explicit close path. It drops private state and never invokes activation.
    pub fn release(self) {}
}

/// The VFS call boundary into the runtime's single validated table transaction.
/// Callers prepare complete typed row mutations and graph-issued pin transfers
/// in the request; this wrapper adds no journal or publication authority.
pub async fn commit_vfs_table_activation(
    runtime: &RuntimeState,
    request: ValidatedTableActivationCommit<'_>,
) -> Result<CwdCapture, TableActivationError> {
    runtime.commit_validated_table_activation(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture_image() -> Arc<VfsFileSnapshot<u64>> {
        let contents = include_str!("../tests/fixtures/publication-repository-main.orna");
        Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(7)),
            Arc::<[u8]>::from(contents.as_bytes()),
        ))
    }

    fn rejected() -> SafeDiagnostic {
        SafeDiagnostic {
            code: DiagnosticCode::TableAssertionFalse,
            class: DiagnosticClass::Permanent,
        }
    }

    #[tokio::test]
    async fn open_read_keeps_its_captured_image_and_directory_pin() {
        let image = fixture_image();
        let handle = SnapshotReadHandle::open(Arc::clone(&image));
        let cursor = SnapshotReaddirCursor::new(image.pin().clone_pin(), 23_u64);
        let original = handle.read_at(0, handle.len() as usize);

        let draft = EditDraft::open(image, 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();

        assert_eq!(handle.read_at(0, original.len()), original);
        assert_eq!(*cursor.pin().snapshot(), 7);
        assert_ne!(draft.candidate_bytes().await.as_ref(), original.as_slice());
    }

    #[tokio::test]
    async fn rejected_draft_is_retained_and_repeated_fsync_is_idempotent() {
        let draft = EditDraft::open(fixture_image(), 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();
        let calls = AtomicUsize::new(0);
        let first = draft
            .fsync_with(|candidate| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert!(!candidate.replacement_bytes().is_empty());
                    Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
                }
            })
            .await
            .unwrap();
        assert!(matches!(first, FsyncOutcome::Rejected(_)));

        let retained = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.diagnostic(), rejected());
        assert_ne!(retained.replacement_bytes(), fixture_image().bytes.as_ref());

        let second = draft
            .fsync_with(|_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                        Arc::new(8),
                    )))
                }
            })
            .await
            .unwrap();
        assert!(matches!(second, FsyncOutcome::Rejected(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn accepted_fsync_advances_only_future_opens_and_release_never_commits() {
        let image = fixture_image();
        let old_handle = SnapshotReadHandle::open(Arc::clone(&image));
        let draft = EditDraft::open(image, 1 << 20);
        draft.write_at(0, &[0x58]).await.unwrap();
        let accepted = draft
            .fsync_with(|candidate| async move {
                assert_eq!(*candidate.baseline().snapshot(), 7);
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        let FsyncOutcome::Accepted(new_handle) = accepted else {
            panic!("activation should accept the candidate");
        };

        assert_eq!(*new_handle.pin().snapshot(), 8);
        assert_eq!(*old_handle.pin().snapshot(), 7);
        assert_ne!(old_handle.read_at(0, 1), new_handle.read_at(0, 1));

        let calls = AtomicUsize::new(0);
        let draft = EditDraft::open(fixture_image(), 1 << 20);
        draft.write_at(1, &[0x59]).await.unwrap();
        draft.release();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn temp_fsync_is_scratch_only_and_rename_preserves_rejected_and_open_state() {
        let target = ManagedFile::new(fixture_image());
        let old_handle = target.open_read().await;
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize);
        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(0, &[0x58]).await.unwrap();

        let scratch_syncs = AtomicUsize::new(0);
        let sequence = scratch
            .fsync_scratch_with(|revision, bytes| {
                scratch_syncs.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(revision, 2);
                    assert_eq!(bytes.as_ref(), &[0x58]);
                    Ok::<_, ()>(())
                }
            })
            .await
            .unwrap();
        assert_eq!(sequence, 2);
        assert_eq!(scratch_syncs.load(Ordering::SeqCst), 1);
        assert_eq!(target.cache_generation().await, 0);
        assert_eq!(
            target.open_read().await.read_at(0, old_bytes.len()),
            old_bytes
        );

        let activations = AtomicUsize::new(0);
        let rejected_outcome = target
            .rename_over(&scratch, |candidate| {
                activations.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(candidate.replacement_bytes(), &[0x58]);
                    Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
                }
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected_outcome,
            TemporaryRenameOutcome::Rejected { .. }
        ));
        assert_eq!(target.cache_generation().await, 0);
        assert_eq!(old_handle.read_at(0, old_bytes.len()), old_bytes);
        let retained = scratch.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes(), &[0x58]);
        assert_eq!(*retained.baseline().snapshot(), 7);

        let repeated = target
            .rename_over(&scratch, |_| {
                activations.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                        Arc::new(9),
                    )))
                }
            })
            .await
            .unwrap();
        assert!(matches!(repeated, TemporaryRenameOutcome::Rejected { .. }));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        scratch.write_at(1, &[0x59]).await.unwrap();
        let accepted = target
            .rename_over(&scratch, |candidate| async move {
                assert_eq!(candidate.replacement_bytes(), &[0x58, 0x59]);
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
            panic!("validated rename should replace the managed target");
        };
        assert_eq!(generation, 1);
        assert_eq!(target.cache_generation().await, 1);
        assert_eq!(*old_handle.pin().snapshot(), 7);
        assert_eq!(old_handle.read_at(0, old_bytes.len()), old_bytes);
        assert_eq!(*target.open_read().await.pin().snapshot(), 8);
        assert_eq!(handle.read_at(0, 2), [0x58, 0x59]);
        assert_eq!(
            scratch
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes(),
            &[0x58]
        );
    }

    #[tokio::test]
    async fn accepted_rename_invalidates_shared_directory_attribute_and_handle_projections() {
        let epoch = SharedCacheEpoch::default();
        let target = ManagedFile::with_cache_epoch(fixture_image(), epoch.clone());
        let old_handle = target.open_read().await;
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize);
        let directory = SnapshotReaddirCursor::new_projected(
            old_handle.pin().clone_pin(),
            0_u64,
            epoch.capture().await,
        );
        // Attribute caches use the same token type and repository epoch.
        let attributes = epoch.capture().await;
        assert_eq!(old_handle.projection().unwrap().generation(), 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(attributes.is_current().await);

        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(0, &[0x58]).await.unwrap();
        assert_eq!(epoch.generation().await, 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(attributes.is_current().await);

        let rejected_outcome = target
            .rename_over(&scratch, |_| async {
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected_outcome,
            TemporaryRenameOutcome::Rejected { .. }
        ));
        assert_eq!(epoch.generation().await, 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(attributes.is_current().await);

        // A new editor revision can be accepted while the prior rejected
        // bytes remain available through the retained-draft interface.
        scratch.write_at(1, &[0x59]).await.unwrap();
        let applied = target
            .rename_over(&scratch, |candidate| async move {
                assert_eq!(candidate.replacement_bytes(), &[0x58, 0x59]);
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied { generation, handle } = applied else {
            panic!("validated rename should advance the shared projection epoch");
        };
        assert_eq!(generation, 1);
        assert_eq!(epoch.generation().await, 1);
        assert!(!old_handle.projection_is_current().await.unwrap());
        assert!(!directory.projection_is_current().await.unwrap());
        assert!(!attributes.is_current().await);
        assert_eq!(old_handle.read_at(0, old_bytes.len()), old_bytes);
        assert!(handle.projection_is_current().await.unwrap());
        let fresh_handle = target.open_read().await;
        assert_eq!(fresh_handle.projection().unwrap().generation(), 1);
        assert!(fresh_handle.projection_is_current().await.unwrap());
        assert_eq!(
            scratch
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes(),
            &[0x58]
        );
    }
}
