//! FUSE-free state for projected file handles and EDIT-1 replacement drafts.
//!
//! `S` is the repository owner's opaque row-map snapshot (in production,
//! `orna_repository_v1::row_store::RowMapSnapshot`). Keeping that value in a
//! [`SnapshotPin`] binds each open handle and readdir cursor to one immutable
//! row-map version without giving this module a second row authority.

use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use tokio::sync::Mutex;

use super::{
    CwdCapture, DiagnosticClass, DiagnosticCode, RuntimeState, SafeDiagnostic,
    TableActivationError, ValidatedTableActivationCommit,
};

/// One repository-wide invalidation clock shared by projections in a VFS
/// repository scope.
#[derive(Clone, Default)]
struct SharedCacheEpoch {
    generation: Arc<Mutex<u64>>,
}

/// A projection's immutable observation of a shared cache epoch.
#[derive(Clone)]
pub struct CacheProjection {
    epoch: SharedCacheEpoch,
    generation: u64,
}

impl SharedCacheEpoch {
    async fn capture(&self) -> CacheProjection {
        let generation = *self.generation.lock().await;
        CacheProjection {
            epoch: self.clone(),
            generation,
        }
    }

    async fn generation(&self) -> u64 {
        *self.generation.lock().await
    }
}

impl CacheProjection {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn belongs_to(&self, epoch: &SharedCacheEpoch) -> bool {
        Arc::ptr_eq(&self.epoch.generation, &epoch.generation)
    }

    pub async fn is_current(&self) -> bool {
        self.generation == *self.epoch.generation.lock().await
    }
}

struct SnapshotProjectionBinding {
    snapshot: Weak<dyn Any + Send + Sync>,
    projection: Arc<OnceLock<CacheProjection>>,
}

#[derive(Default)]
struct SnapshotProjectionRegistry {
    bindings: HashMap<usize, Vec<SnapshotProjectionBinding>>,
    lookups_since_prune: usize,
}

static SNAPSHOT_PROJECTION_REGISTRY: OnceLock<StdMutex<SnapshotProjectionRegistry>> =
    OnceLock::new();

/// Returns one stable projection cell for an owner snapshot allocation. A
/// freshly captured pin for the same Arc therefore retains the original
/// admission stamp instead of creating a path around it.
fn snapshot_projection_cell<S>(snapshot: &Arc<S>) -> Arc<OnceLock<CacheProjection>>
where
    S: Any + Send + Sync + 'static,
{
    let address = Arc::as_ptr(snapshot) as usize;
    let erased_snapshot: Arc<dyn Any + Send + Sync> = snapshot.clone();
    let identity = Arc::downgrade(&erased_snapshot);
    let registry = SNAPSHOT_PROJECTION_REGISTRY
        .get_or_init(|| StdMutex::new(SnapshotProjectionRegistry::default()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    registry.lookups_since_prune += 1;
    if registry.lookups_since_prune >= 256 {
        for bindings in registry.bindings.values_mut() {
            bindings.retain(|binding| binding.snapshot.strong_count() != 0);
        }
        registry.bindings.retain(|_, bindings| !bindings.is_empty());
        registry.lookups_since_prune = 0;
    }

    let bindings = registry.bindings.entry(address).or_default();
    bindings.retain(|binding| binding.snapshot.strong_count() != 0);
    if let Some(binding) = bindings
        .iter()
        .find(|binding| binding.snapshot.ptr_eq(&identity))
    {
        return Arc::clone(&binding.projection);
    }

    let projection = Arc::new(OnceLock::new());
    bindings.push(SnapshotProjectionBinding {
        snapshot: identity,
        projection: Arc::clone(&projection),
    });
    projection
}

/// Repository-view owner for managed files and their shared projection epoch.
/// Create one scope for a repository view, then construct every managed file
/// through it so a committed replacement invalidates all of that view's
/// projections together.
#[derive(Default)]
pub struct VfsRepositoryCache {
    epoch: SharedCacheEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsRepositoryCacheError {
    ImageAlreadyScoped,
}

impl VfsRepositoryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn generation(&self) -> u64 {
        self.epoch.generation().await
    }

    /// Admits an immutable image to this repository view. Images sharing a
    /// current projection from this scope reuse that projection; stale or
    /// foreign bindings cannot be relabeled with the current generation.
    pub async fn managed_file<S>(
        &self,
        image: Arc<VfsFileSnapshot<S>>,
    ) -> Result<Arc<ManagedFile<S>>, VfsRepositoryCacheError> {
        if image.pin.projection().is_some() {
            if self.has_current_projection(&image).await {
                return Ok(self.managed_file_for_image(image));
            }
            return Err(VfsRepositoryCacheError::ImageAlreadyScoped);
        }
        let projection = self.epoch.capture().await;
        if image.pin.projection.set(projection).is_err()
            && !self.has_current_projection(&image).await
        {
            return Err(VfsRepositoryCacheError::ImageAlreadyScoped);
        }
        Ok(self.managed_file_for_image(image))
    }

    /// Routes a temporary rename through this repository view. A file created
    /// by another cache scope cannot advance this scope's projection epoch.
    pub async fn rename_over<S, F, Fut, E>(
        &self,
        target: &Arc<ManagedFile<S>>,
        scratch: &TemporarySave<S>,
        activate: F,
    ) -> Result<TemporaryRenameOutcome<S>, TemporaryRenameError<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        if !Arc::ptr_eq(&target.cache_epoch.generation, &self.epoch.generation) {
            return Err(TemporaryRenameError::WrongRepositoryScope);
        }
        target.rename_over(scratch, activate).await
    }

    async fn has_current_projection<S>(&self, image: &VfsFileSnapshot<S>) -> bool {
        match image.pin.projection() {
            Some(projection) => projection.belongs_to(&self.epoch) && projection.is_current().await,
            None => false,
        }
    }

    fn managed_file_for_image<S>(&self, image: Arc<VfsFileSnapshot<S>>) -> Arc<ManagedFile<S>> {
        Arc::new(ManagedFile {
            state: Mutex::new(ManagedFileState {
                image,
                unlinked: false,
            }),
            cache_epoch: self.epoch.clone(),
        })
    }
}

/// A retained owner-issued immutable row-map snapshot.
pub struct SnapshotPin<S> {
    snapshot: Arc<S>,
    projection: Arc<OnceLock<CacheProjection>>,
}

impl<S> Clone for SnapshotPin<S> {
    fn clone(&self) -> Self {
        Self {
            snapshot: Arc::clone(&self.snapshot),
            projection: Arc::clone(&self.projection),
        }
    }
}

impl<S> SnapshotPin<S> {
    /// Retains an opaque repository snapshot. The snapshot carries its own
    /// immutable version identity and remains the only row lookup authority.
    pub fn capture(snapshot: Arc<S>) -> Self
    where
        S: Any + Send + Sync + 'static,
    {
        Self {
            projection: snapshot_projection_cell(&snapshot),
            snapshot,
        }
    }

    /// Borrows the same owner-issued snapshot captured by this pin.
    pub fn snapshot(&self) -> &S {
        &self.snapshot
    }

    /// Clones the pin for another handle that must remain on this version.
    pub fn clone_pin(&self) -> Self {
        self.clone()
    }

    pub fn projection(&self) -> Option<&CacheProjection> {
        self.projection.get()
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

    fn with_projection(image: &Arc<Self>, projection: CacheProjection) -> Self {
        // The pin's cell is shared by every capture of this owner snapshot.
        // Once stamped, later wrappers keep that first epoch instead of
        // relabeling the same snapshot as current.
        let pin = image.pin.clone_pin();
        let _ = pin.projection.set(projection);
        Self {
            pin,
            bytes: Arc::clone(&image.bytes),
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

    pub fn projection(&self) -> Option<&CacheProjection> {
        self.pin.projection()
    }
}

/// A read-only open file; its bytes and row-map snapshot never advance.
pub struct SnapshotReadHandle<S> {
    image: Arc<VfsFileSnapshot<S>>,
}

impl<S> Clone for SnapshotReadHandle<S> {
    fn clone(&self) -> Self {
        Self {
            image: Arc::clone(&self.image),
        }
    }
}

impl<S> SnapshotReadHandle<S> {
    pub fn open(image: Arc<VfsFileSnapshot<S>>) -> Self {
        Self { image }
    }

    /// The cache observation attached to this handle, when it came from a
    /// managed repository view. Its pinned bytes remain readable if it goes
    /// stale; callers can refresh the projection against the shared epoch.
    pub fn projection(&self) -> Option<&CacheProjection> {
        self.image.projection()
    }

    pub async fn projection_is_current(&self) -> Option<bool> {
        match self.projection() {
            Some(projection) => Some(projection.is_current().await),
            None => None,
        }
    }

    /// Associates a cached attribute or other derived value with this exact
    /// immutable image. The returned projection cannot be retagged with a
    /// token captured from another image or epoch.
    pub fn project<T>(&self, value: T) -> CacheProjectedValue<T, S> {
        CacheProjectedValue {
            image: Arc::clone(&self.image),
            value,
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
}

impl<S, C> SnapshotReaddirCursor<S, C> {
    pub fn new(pin: SnapshotPin<S>, continuation: C) -> Self {
        Self { pin, continuation }
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
        self.pin.projection()
    }

    pub async fn projection_is_current(&self) -> Option<bool> {
        match self.projection() {
            Some(projection) => Some(projection.is_current().await),
            None => None,
        }
    }
}

/// A derived cache value tied to the exact immutable image from which its
/// projection was produced.
pub struct CacheProjectedValue<T, S> {
    image: Arc<VfsFileSnapshot<S>>,
    value: T,
}

impl<T, S> CacheProjectedValue<T, S> {
    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn pin(&self) -> &SnapshotPin<S> {
        self.image.pin()
    }

    pub fn projection(&self) -> Option<&CacheProjection> {
        self.image.projection()
    }

    pub async fn projection_is_current(&self) -> Option<bool> {
        match self.projection() {
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
    removal: bool,
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

    /// True for an unlink candidate: the activation must delete the file's
    /// row, and `replacement_bytes` is empty and meaningless.
    pub fn is_removal(&self) -> bool {
        self.removal
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
    /// Set by an accepted unlink. Once set, no new open or replacement can
    /// observe or change this destination; existing handles keep their pins.
    unlinked: bool,
}

/// Stat of one managed destination's accepted image, from [`ManagedFile::stat`].
pub struct ManagedFileStat<S> {
    size: u64,
    pin: SnapshotPin<S>,
    generation: u64,
}

impl<S> ManagedFileStat<S> {
    /// Byte length of the accepted image.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Snapshot pin of the accepted image.
    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    /// Repository-wide invalidation generation at the time of the stat.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Renders size and generation as a JSON object. The pin is not emitted:
    /// `S` is the repository's opaque snapshot type, so its encoding belongs
    /// to the caller. Both fields are integers, so no escaping is needed.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"size\":{},\"generation\":{}}}",
            self.size, self.generation
        )
    }
}

/// One already-managed destination. New opens observe its latest accepted
/// image; open handles keep the immutable image captured when they opened.
pub struct ManagedFile<S> {
    state: Mutex<ManagedFileState<S>>,
    cache_epoch: SharedCacheEpoch,
}

impl<S> ManagedFile<S> {
    pub async fn open_read(&self) -> SnapshotReadHandle<S> {
        SnapshotReadHandle::open(Arc::clone(&self.state.lock().await.image))
    }

    /// Opens the destination as it currently exists. Returns `None` after an
    /// accepted unlink, so a new open can never see the removed file's bytes.
    pub async fn open_current(&self) -> Option<SnapshotReadHandle<S>> {
        let state = self.state.lock().await;
        (!state.unlinked).then(|| SnapshotReadHandle::open(Arc::clone(&state.image)))
    }

    /// Returns the repository-wide invalidation generation.
    pub async fn cache_generation(&self) -> u64 {
        self.cache_epoch.generation().await
    }

    /// Reports size, snapshot pin and cache generation of the accepted image.
    /// Returns `None` after an accepted unlink, like [`ManagedFile::open_current`].
    /// Draft state is not part of this view: a stat never observes or
    /// accepts a private EDIT-1 draft.
    pub async fn stat(&self) -> Option<ManagedFileStat<S>> {
        let (size, pin) = {
            let state = self.state.lock().await;
            if state.unlinked {
                return None;
            }
            (state.image.len(), state.image.pin().clone_pin())
        };
        Some(ManagedFileStat {
            size,
            pin,
            generation: self.cache_epoch.generation().await,
        })
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
        if destination.unlinked || !Arc::ptr_eq(&destination.image, &scratch.baseline) {
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
                let image = Arc::new(VfsFileSnapshot::with_projection(
                    &handle.image,
                    CacheProjection {
                        epoch: self.cache_epoch.clone(),
                        generation: next_generation,
                    },
                ));
                destination.image = Arc::clone(&image);
                *cache_generation = next_generation;
                let handle = SnapshotReadHandle::open(image);
                scratch_state.applied = Some((next_generation, handle.clone()));
                Ok(TemporaryRenameOutcome::Applied {
                    generation: next_generation,
                    handle,
                })
            }
            Ok(FsyncOutcome::Unchanged(_)) => Err(TemporaryRenameError::NoCandidate),
        }
    }

    /// Opens a private EDIT-1 draft over this destination's current image. The
    /// draft commits only through [`ManagedFile::commit_draft_with`].
    pub async fn open_draft(self: &Arc<Self>, max_file_bytes: usize) -> EditDraft<S> {
        let baseline = Arc::clone(&self.state.lock().await.image);
        EditDraft::open(baseline, max_file_bytes)
    }

    /// Commits a direct draft over this destination through the same validated
    /// activation boundary as a temp rename. A draft whose baseline is no longer
    /// this destination's image returns `Stale` without calling activation.
    /// An accepted revision replaces the image seen by new opens and invalidates
    /// every older projection in this repository scope.
    pub async fn commit_draft_with<F, Fut, E>(
        self: &Arc<Self>,
        draft: &EditDraft<S>,
        activate: F,
    ) -> Result<TemporaryRenameOutcome<S>, TemporaryRenameError<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        let mut destination = self.state.lock().await;
        let based_on_current = !destination.unlinked && {
            let draft_state = draft.state.lock().await;
            Arc::ptr_eq(&destination.image, &draft_state.baseline)
        };
        if !based_on_current {
            return Ok(TemporaryRenameOutcome::Stale {
                diagnostic: SafeDiagnostic {
                    code: DiagnosticCode::ExecutionRejected,
                    class: DiagnosticClass::Transient,
                },
            });
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;

        match draft.fsync_with(activate).await {
            Err(error) => Err(TemporaryRenameError::Activation(error)),
            Ok(FsyncOutcome::Rejected(diagnostic)) => {
                Ok(TemporaryRenameOutcome::Rejected { diagnostic })
            }
            Ok(FsyncOutcome::Accepted(handle)) => {
                let image = Arc::new(VfsFileSnapshot::with_projection(
                    &handle.image,
                    CacheProjection {
                        epoch: self.cache_epoch.clone(),
                        generation: next_generation,
                    },
                ));
                draft.rebase_accepted(Arc::clone(&image)).await;
                destination.image = Arc::clone(&image);
                *cache_generation = next_generation;
                Ok(TemporaryRenameOutcome::Applied {
                    generation: next_generation,
                    handle: SnapshotReadHandle::open(image),
                })
            }
            Ok(FsyncOutcome::Unchanged(_)) => Err(TemporaryRenameError::NoCandidate),
        }
    }

    /// Removes this destination through the validated activation boundary. The
    /// candidate is a removal (`is_removal`), so the caller must delete the
    /// file's row. An accepted unlink bumps the shared generation, invalidating
    /// every projection of the removed image, and makes new opens return `None`.
    pub async fn unlink_with<F, Fut, E>(
        self: &Arc<Self>,
        activate: F,
    ) -> Result<UnlinkOutcome, TemporaryRenameError<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        let mut destination = self.state.lock().await;
        if destination.unlinked {
            return Ok(UnlinkOutcome::AlreadyUnlinked);
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;
        let candidate = ActivationCandidate {
            baseline: destination.image.pin.clone_pin(),
            revision: 0,
            replacement: Arc::from(&[][..]),
            removal: true,
        };
        match activate(candidate)
            .await
            .map_err(TemporaryRenameError::Activation)?
        {
            ActivationDecision::Accepted(_) => {
                destination.unlinked = true;
                *cache_generation = next_generation;
                Ok(UnlinkOutcome::Applied {
                    generation: next_generation,
                })
            }
            ActivationDecision::Rejected(diagnostic) => Ok(UnlinkOutcome::Rejected { diagnostic }),
        }
    }
}

/// The result of [`ManagedFile::unlink_with`].
#[derive(Debug, Eq, PartialEq)]
pub enum UnlinkOutcome {
    Applied { generation: u64 },
    Rejected { diagnostic: SafeDiagnostic },
    AlreadyUnlinked,
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
    WrongRepositoryScope,
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

    /// Re-anchors an accepted draft on the projected image its destination now
    /// holds, so the next commit's CAS compares against the live image.
    async fn rebase_accepted(&self, image: Arc<VfsFileSnapshot<S>>) {
        self.state.lock().await.baseline = image;
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
            removal: false,
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
    use crate::{
        NoFault, RuntimeIdentity, RuntimeTableRows, TableActivationCandidateValidator,
        TableMutation, WriterLease,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ExactRuntimeCandidateValidator {
        tables: Vec<String>,
        key: Vec<u8>,
        value: Vec<u8>,
        accept: bool,
    }

    impl TableActivationCandidateValidator for ExactRuntimeCandidateValidator {
        fn tables(&self) -> &[String] {
            &self.tables
        }

        fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic> {
            let expected = vec![(self.key.clone(), self.value.clone())];
            if self.accept && rows.get("books") == Some(&expected) {
                Ok(())
            } else {
                Err(rejected())
            }
        }
    }

    async fn activate_runtime_candidate(
        runtime: Arc<RuntimeState>,
        writer: WriterLease,
        candidate: ActivationCandidate<CwdCapture>,
        key: Vec<u8>,
        mutation_id: [u8; 16],
        accept: bool,
    ) -> Result<ActivationDecision<CwdCapture>, TableActivationError> {
        let context = runtime
            .begin_activation()
            .await
            .map_err(TableActivationError::Runtime)?;
        assert_eq!(context.capture(), candidate.baseline().snapshot());

        let value = candidate.replacement_bytes().to_vec();
        let mutation = TableMutation::new(mutation_id, "books", key.clone(), Some(value.clone()))
            .map_err(TableActivationError::Runtime)?;
        let mutations = [mutation];
        let mut validator = ExactRuntimeCandidateValidator {
            tables: vec!["books".into()],
            key,
            value,
            accept,
        };
        let faults = NoFault;

        match commit_vfs_table_activation(
            &runtime,
            ValidatedTableActivationCommit {
                writer,
                context: &context,
                mutations: &mutations,
                next_digest: [mutation_id[0]; 32],
                validator: &mut validator,
                faults: &faults,
            },
        )
        .await
        {
            Ok(next_capture) => Ok(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(next_capture),
            ))),
            Err(TableActivationError::ValidationFailed(diagnostic)) => {
                Ok(ActivationDecision::Rejected(diagnostic))
            }
            Err(error) => Err(error),
        }
    }

    /// Routes an unlink candidate through the same validated runtime
    /// transaction as a write. The row is deleted only when `accept` is set.
    async fn activate_runtime_removal(
        runtime: Arc<RuntimeState>,
        writer: WriterLease,
        candidate: ActivationCandidate<CwdCapture>,
        key: Vec<u8>,
        mutation_id: [u8; 16],
        accept: bool,
    ) -> Result<ActivationDecision<CwdCapture>, TableActivationError> {
        assert!(candidate.is_removal());
        let context = runtime
            .begin_activation()
            .await
            .map_err(TableActivationError::Runtime)?;
        let mutation = TableMutation::new(mutation_id, "books", key.clone(), None)
            .map_err(TableActivationError::Runtime)?;
        let mutations = [mutation];
        let mut validator = ExactRuntimeRemovalValidator {
            tables: vec!["books".into()],
            key,
            accept,
        };
        let faults = NoFault;
        match commit_vfs_table_activation(
            &runtime,
            ValidatedTableActivationCommit {
                writer,
                context: &context,
                mutations: &mutations,
                next_digest: [mutation_id[0]; 32],
                validator: &mut validator,
                faults: &faults,
            },
        )
        .await
        {
            Ok(next_capture) => Ok(ActivationDecision::Accepted(SnapshotPin::capture(
                Arc::new(next_capture),
            ))),
            Err(TableActivationError::ValidationFailed(diagnostic)) => {
                Ok(ActivationDecision::Rejected(diagnostic))
            }
            Err(error) => Err(error),
        }
    }

    struct ExactRuntimeRemovalValidator {
        tables: Vec<String>,
        key: Vec<u8>,
        accept: bool,
    }

    impl TableActivationCandidateValidator for ExactRuntimeRemovalValidator {
        fn tables(&self) -> &[String] {
            &self.tables
        }

        fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic> {
            let still_present = rows
                .get("books")
                .is_some_and(|rows| rows.iter().any(|(key, _)| key == &self.key));
            if self.accept && !still_present {
                Ok(())
            } else {
                Err(rejected())
            }
        }
    }

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
    async fn direct_draft_commit_publishes_through_destination_and_rejects_stale_drafts() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            RuntimeState::open_path(
                &temp.path().join("state.db"),
                RuntimeIdentity {
                    database_id: [0x11; 16],
                    repository_id: [0x22; 16],
                },
                [0x33; 32],
                None,
            )
            .await
            .unwrap(),
        );
        let writer = runtime.acquire_lease([0x44; 16]).await.unwrap();

        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");
        let rejected_bytes = include_str!("../tests/fixtures/case_closure_edge_tail_sixty.orna");

        let initial_capture = Arc::new(runtime.capture().await.unwrap());
        let image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&initial_capture)),
            Arc::<[u8]>::from(baseline.as_bytes()),
        ));
        let target = runtime.manage_vfs_file(image).await.unwrap();
        let old_handle = target.open_read().await;
        assert!(old_handle.projection_is_current().await.unwrap());

        let draft = target.open_draft(1 << 20).await;
        let stale_draft = target.open_draft(1 << 20).await;

        draft.truncate(0).await.unwrap();
        draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let accepted_runtime = Arc::clone(&runtime);
        let accepted = target
            .commit_draft_with(&draft, move |candidate| {
                activate_runtime_candidate(
                    accepted_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x61; 16],
                    true,
                )
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
            panic!("validated direct draft commit should be accepted");
        };
        assert_eq!(generation, 1);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);
        assert!(!old_handle.projection_is_current().await.unwrap());
        assert!(handle.projection_is_current().await.unwrap());
        assert_eq!(
            target.open_read().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        assert_eq!(
            runtime.committed_table_row("books", &[0x51]).await.unwrap(),
            Some(accepted_bytes.as_bytes().to_vec())
        );

        // A draft opened before the accepted commit cannot publish over it.
        let stale_calls = Arc::new(AtomicUsize::new(0));
        let stale_calls_by_route = Arc::clone(&stale_calls);
        stale_draft.truncate(0).await.unwrap();
        stale_draft
            .write_at(0, rejected_bytes.as_bytes())
            .await
            .unwrap();
        let stale = target
            .commit_draft_with(&stale_draft, move |_| async move {
                stale_calls_by_route.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await
            .unwrap();
        assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
        assert_eq!(stale_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);

        // A validation rejection leaves the destination image and generation alone.
        draft.truncate(0).await.unwrap();
        draft.write_at(0, rejected_bytes.as_bytes()).await.unwrap();
        let rejected_runtime = Arc::clone(&runtime);
        let rejected_commit = target
            .commit_draft_with(&draft, move |candidate| {
                activate_runtime_candidate(
                    rejected_runtime,
                    writer,
                    candidate,
                    vec![0x52],
                    [0x62; 16],
                    false,
                )
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected_commit,
            TemporaryRenameOutcome::Rejected { .. }
        ));
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);
        assert_eq!(
            target.open_read().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        assert!(draft.retained_invalid_draft().await.is_some());
        assert_eq!(
            runtime.committed_table_row("books", &[0x52]).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn unlink_deletes_row_through_validated_commit_and_hides_new_opens() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            RuntimeState::open_path(
                &temp.path().join("state.db"),
                RuntimeIdentity {
                    database_id: [0x11; 16],
                    repository_id: [0x22; 16],
                },
                [0x33; 32],
                None,
            )
            .await
            .unwrap(),
        );
        let writer = runtime.acquire_lease([0x44; 16]).await.unwrap();

        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");

        let initial_capture = Arc::new(runtime.capture().await.unwrap());
        let image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&initial_capture)),
            Arc::<[u8]>::from(baseline.as_bytes()),
        ));
        let target = runtime.manage_vfs_file(image).await.unwrap();

        // Seed the file's row through the validated write path so the unlink
        // has a committed row to delete.
        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let seed_runtime = Arc::clone(&runtime);
        let seeded = target
            .commit_draft_with(&draft, move |candidate| {
                activate_runtime_candidate(
                    seed_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x61; 16],
                    true,
                )
            })
            .await
            .unwrap();
        assert!(matches!(seeded, TemporaryRenameOutcome::Applied { .. }));
        let live = target.open_current().await.expect("linked before unlink");

        let rejected_runtime = Arc::clone(&runtime);
        let rejected = target
            .unlink_with(move |candidate| {
                activate_runtime_removal(
                    rejected_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x71; 16],
                    false,
                )
            })
            .await
            .unwrap();
        assert!(matches!(rejected, UnlinkOutcome::Rejected { .. }));
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);
        assert!(target.open_current().await.is_some());
        assert_eq!(
            runtime.committed_table_row("books", &[0x51]).await.unwrap(),
            Some(accepted_bytes.as_bytes().to_vec())
        );

        let unlink_runtime = Arc::clone(&runtime);
        let unlinked = target
            .unlink_with(move |candidate| {
                activate_runtime_removal(
                    unlink_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x72; 16],
                    true,
                )
            })
            .await
            .unwrap();
        assert_eq!(unlinked, UnlinkOutcome::Applied { generation: 2 });
        assert_eq!(runtime.vfs_repository_cache().generation().await, 2);
        assert!(target.open_current().await.is_none());
        assert!(!live.projection_is_current().await.unwrap());
        assert_eq!(
            runtime.committed_table_row("books", &[0x51]).await.unwrap(),
            None
        );

        // Replacements against the removed file are refused, and a second
        // unlink is a no-op rather than another activation.
        let late = target.open_draft(1 << 20).await;
        late.truncate(0).await.unwrap();
        late.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let stale = target
            .commit_draft_with(&late, |_| async {
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await
            .unwrap();
        assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
        let again = target
            .unlink_with(|_| async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) })
            .await
            .unwrap();
        assert_eq!(again, UnlinkOutcome::AlreadyUnlinked);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 2);
    }

    #[tokio::test]
    async fn fresh_projection_observes_committed_write_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            RuntimeState::open_path(
                &temp.path().join("state.db"),
                RuntimeIdentity {
                    database_id: [0x11; 16],
                    repository_id: [0x22; 16],
                },
                [0x33; 32],
                None,
            )
            .await
            .unwrap(),
        );
        let writer = runtime.acquire_lease([0x44; 16]).await.unwrap();

        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");
        let rejected_bytes = include_str!("../tests/fixtures/case_closure_edge_tail_sixty.orna");

        let initial_capture = Arc::new(runtime.capture().await.unwrap());
        let image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&initial_capture)),
            Arc::<[u8]>::from(baseline.as_bytes()),
        ));
        let target = runtime.manage_vfs_file(image).await.unwrap();
        let before_write = target.open_read().await;
        let activations = Arc::new(AtomicUsize::new(0));

        // One draft commit: one activation, one generation, one committed row.
        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let draft_calls = Arc::clone(&activations);
        let draft_runtime = Arc::clone(&runtime);
        let committed = target
            .commit_draft_with(&draft, move |candidate| {
                draft_calls.fetch_add(1, Ordering::SeqCst);
                activate_runtime_candidate(
                    draft_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x61; 16],
                    true,
                )
            })
            .await
            .unwrap();
        assert!(matches!(
            committed,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        // Every fresh projection observes the write, current and with the bytes.
        for _ in 0..2 {
            let fresh = target.open_read().await;
            assert_eq!(fresh.projection_is_current().await, Some(true));
            assert_eq!(fresh.len() as usize, accepted_bytes.len());
            assert_eq!(
                fresh.read_at(0, accepted_bytes.len()),
                accepted_bytes.as_bytes()
            );
        }
        assert!(!before_write.projection_is_current().await.unwrap());

        // Re-syncing the committed draft is a no-op: no second activation and
        // no generation bump.
        let retry_calls = Arc::clone(&activations);
        let retry = target
            .commit_draft_with(&draft, move |_| async move {
                retry_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await;
        assert!(matches!(retry, Err(TemporaryRenameError::NoCandidate)));
        assert_eq!(activations.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);

        // A temp-file rename applied twice activates once and returns the same
        // generation and handle the first time did.
        let save = target.begin_temporary_replacement(1 << 20).await;
        save.write_at(0, rejected_bytes.as_bytes()).await.unwrap();
        let rename_calls = Arc::clone(&activations);
        let rename_runtime = Arc::clone(&runtime);
        let first = runtime
            .rename_vfs_temporary_over(&target, &save, move |candidate| {
                rename_calls.fetch_add(1, Ordering::SeqCst);
                activate_runtime_candidate(
                    rename_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x62; 16],
                    true,
                )
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied {
            generation: first_generation,
            handle: first_handle,
        } = first
        else {
            panic!("validated temp rename should be accepted");
        };
        assert_eq!(first_generation, 2);
        assert_eq!(activations.load(Ordering::SeqCst), 2);

        let second_calls = Arc::clone(&activations);
        let second = runtime
            .rename_vfs_temporary_over(&target, &save, move |candidate| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                activate_runtime_candidate(
                    Arc::clone(&runtime),
                    writer,
                    candidate,
                    vec![0x51],
                    [0x63; 16],
                    true,
                )
            })
            .await
            .unwrap();
        assert!(matches!(
            second,
            TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(activations.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 2);

        assert!(first_handle.projection_is_current().await.unwrap());
        let latest = target.open_read().await;
        assert_eq!(latest.projection_is_current().await, Some(true));
        assert_eq!(
            latest.read_at(0, rejected_bytes.len()),
            rejected_bytes.as_bytes()
        );
        assert_eq!(
            runtime.committed_table_row("books", &[0x51]).await.unwrap(),
            Some(rejected_bytes.as_bytes().to_vec())
        );
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
    async fn stat_reports_accepted_size_pin_and_generation_and_hides_unlinked() {
        let image = fixture_image();
        let expected_len = image.len();
        let managed = ManagedFile {
            state: Mutex::new(ManagedFileState {
                image,
                unlinked: false,
            }),
            cache_epoch: SharedCacheEpoch::default(),
        };

        let stat = managed.stat().await.unwrap();
        assert_eq!(stat.size(), expected_len);
        assert_eq!(*stat.pin().snapshot(), 7);
        assert_eq!(stat.generation(), 0);

        managed.state.lock().await.unlinked = true;
        assert!(managed.stat().await.is_none());
    }

    #[test]
    fn stat_json_renders_size_and_generation_without_pin() {
        let stat = ManagedFileStat {
            size: 5,
            pin: SnapshotPin::capture(Arc::new(7_u64)),
            generation: 3,
        };
        assert_eq!(stat.to_json(), r#"{"size":5,"generation":3}"#);
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
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
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
        let repository = VfsRepositoryCache::new();
        let raw_target_image = fixture_image();
        let stale_snapshot = Arc::clone(&raw_target_image.pin.snapshot);
        let target = repository
            .managed_file(Arc::clone(&raw_target_image))
            .await
            .unwrap();
        let sibling_image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&stale_snapshot)),
            Arc::clone(&raw_target_image.bytes),
        ));
        assert!(!Arc::ptr_eq(&raw_target_image, &sibling_image));
        let sibling = repository
            .managed_file(Arc::clone(&sibling_image))
            .await
            .unwrap();
        assert!(std::ptr::eq(
            raw_target_image.projection().unwrap(),
            sibling_image.projection().unwrap()
        ));
        let old_handle = target.open_read().await;
        let sibling_handle = sibling.open_read().await;
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize);
        let directory = SnapshotReaddirCursor::new(old_handle.pin().clone_pin(), 0_u64);
        let attributes = old_handle.project(("size", old_handle.len()));
        assert_eq!(old_handle.projection().unwrap().generation(), 0);
        assert_eq!(sibling_handle.projection().unwrap().generation(), 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(sibling_handle.projection_is_current().await.unwrap());
        assert!(attributes.projection_is_current().await.unwrap());

        let unrelated_scope = VfsRepositoryCache::new();
        assert!(matches!(
            unrelated_scope
                .managed_file(Arc::clone(&old_handle.image))
                .await,
            Err(VfsRepositoryCacheError::ImageAlreadyScoped)
        ));

        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(0, &[0x58]).await.unwrap();
        assert_eq!(repository.generation().await, 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(sibling_handle.projection_is_current().await.unwrap());
        assert!(attributes.projection_is_current().await.unwrap());

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
        assert_eq!(repository.generation().await, 0);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());
        assert!(sibling_handle.projection_is_current().await.unwrap());
        assert!(attributes.projection_is_current().await.unwrap());

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
        assert_eq!(repository.generation().await, 1);
        assert!(!old_handle.projection_is_current().await.unwrap());
        assert!(!directory.projection_is_current().await.unwrap());
        assert!(!sibling_handle.projection_is_current().await.unwrap());
        assert!(!attributes.projection_is_current().await.unwrap());
        assert_eq!(old_handle.read_at(0, old_bytes.len()), old_bytes);
        assert!(handle.projection_is_current().await.unwrap());
        // Admission binds every alias of the original raw image to generation
        // zero. After the accepted replacement advances the repository epoch,
        // replaying that alias cannot stamp its stale bytes as current.
        let raw_projection = raw_target_image.projection().unwrap();
        assert_eq!(raw_projection.generation(), 0);
        assert!(!raw_projection.is_current().await);
        assert!(matches!(
            repository.managed_file(Arc::clone(&raw_target_image)).await,
            Err(VfsRepositoryCacheError::ImageAlreadyScoped)
        ));
        // Re-capturing the same owner snapshot Arc must recover the original
        // binding too; making a new VfsFileSnapshot wrapper cannot bypass it.
        let rewrapped_stale_image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&stale_snapshot)),
            Arc::clone(&raw_target_image.bytes),
        ));
        assert_eq!(rewrapped_stale_image.projection().unwrap().generation(), 0);
        assert!(
            !rewrapped_stale_image
                .projection()
                .unwrap()
                .is_current()
                .await
        );
        assert!(matches!(
            repository.managed_file(rewrapped_stale_image).await,
            Err(VfsRepositoryCacheError::ImageAlreadyScoped)
        ));
        let fresh_handle = target.open_read().await;
        assert_eq!(fresh_handle.projection().unwrap().generation(), 1);
        assert!(fresh_handle.projection_is_current().await.unwrap());
        // A managed file that was not itself replaced keeps its old image
        // stamp; opening it cannot relabel that stale image as current.
        assert_eq!(
            sibling.open_read().await.projection().unwrap().generation(),
            0
        );
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
    async fn exported_temp_rename_uses_validated_runtime_commit_and_shared_cache_epoch() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            RuntimeState::open_path(
                &temp.path().join("state.db"),
                RuntimeIdentity {
                    database_id: [0x11; 16],
                    repository_id: [0x22; 16],
                },
                [0x33; 32],
                None,
            )
            .await
            .unwrap(),
        );
        let writer = runtime.acquire_lease([0x44; 16]).await.unwrap();

        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");
        let rejected_bytes = include_str!("../tests/fixtures/case_closure_edge_tail_sixty.orna");
        assert_ne!(baseline.as_bytes(), accepted_bytes.as_bytes());
        assert_ne!(accepted_bytes.as_bytes(), rejected_bytes.as_bytes());

        let initial_capture = Arc::new(runtime.capture().await.unwrap());
        let image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&initial_capture)),
            Arc::<[u8]>::from(baseline.as_bytes()),
        ));
        let target = runtime.manage_vfs_file(image).await.unwrap();
        let old_handle = target.open_read().await;
        let directory =
            SnapshotReaddirCursor::new(SnapshotPin::capture(Arc::clone(&initial_capture)), 0_u64);
        assert!(old_handle.projection_is_current().await.unwrap());
        assert!(directory.projection_is_current().await.unwrap());

        let unrelated_cache = VfsRepositoryCache::new();
        let unrelated_image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(initial_capture.as_ref().clone())),
            Arc::<[u8]>::from(baseline.as_bytes()),
        ));
        let unrelated_file = unrelated_cache.managed_file(unrelated_image).await.unwrap();
        let unrelated_save = unrelated_file.begin_temporary_replacement(1 << 20).await;
        unrelated_save
            .write_at(0, accepted_bytes.as_bytes())
            .await
            .unwrap();
        let callback_calls = Arc::new(AtomicUsize::new(0));
        let callback_calls_by_route = Arc::clone(&callback_calls);
        let foreign_route = runtime
            .rename_vfs_temporary_over(&unrelated_file, &unrelated_save, move |_| async move {
                callback_calls_by_route.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await;
        assert!(matches!(
            foreign_route,
            Err(TemporaryRenameError::WrongRepositoryScope)
        ));
        assert_eq!(callback_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 0);

        let accepted_save = target.begin_temporary_replacement(1 << 20).await;
        accepted_save
            .write_at(0, accepted_bytes.as_bytes())
            .await
            .unwrap();
        let accepted_runtime = Arc::clone(&runtime);
        let accepted = runtime
            .rename_vfs_temporary_over(&target, &accepted_save, move |candidate| {
                activate_runtime_candidate(
                    accepted_runtime,
                    writer,
                    candidate,
                    vec![0x51],
                    [0x61; 16],
                    true,
                )
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
            panic!("validated runtime activation should accept the editor replacement");
        };
        assert_eq!(generation, 1);
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);
        assert!(!old_handle.projection_is_current().await.unwrap());
        assert!(!directory.projection_is_current().await.unwrap());
        assert!(handle.projection_is_current().await.unwrap());
        assert_eq!(
            handle.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        assert_eq!(
            runtime.committed_table_row("books", &[0x51]).await.unwrap(),
            Some(accepted_bytes.as_bytes().to_vec())
        );

        let rejected_save = target.begin_temporary_replacement(1 << 20).await;
        rejected_save
            .write_at(0, rejected_bytes.as_bytes())
            .await
            .unwrap();
        let rejected_runtime = Arc::clone(&runtime);
        let rejected = runtime
            .rename_vfs_temporary_over(&target, &rejected_save, move |candidate| {
                activate_runtime_candidate(
                    rejected_runtime,
                    writer,
                    candidate,
                    vec![0x52],
                    [0x62; 16],
                    false,
                )
            })
            .await
            .unwrap();
        assert!(matches!(rejected, TemporaryRenameOutcome::Rejected { .. }));
        assert_eq!(runtime.vfs_repository_cache().generation().await, 1);
        assert_eq!(
            target.open_read().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        assert_eq!(
            rejected_save
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes(),
            rejected_bytes.as_bytes()
        );
        assert_eq!(
            runtime.committed_table_row("books", &[0x52]).await.unwrap(),
            None
        );
    }
}
