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

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{
    CwdCapture, DiagnosticClass, DiagnosticCode, RuntimeError, RuntimeState, RuntimeTableIdentity,
    SafeDiagnostic, TableActivationError, TableMutation, ValidatedTableActivationCommit,
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

    /// Renders the size for people: plain bytes below 1 KiB, then KiB and MiB
    /// with one decimal place. Sizes of 1 MiB and above are shown in MiB.
    pub fn human_size(&self) -> String {
        const KIB: u64 = 1024;
        const MIB: u64 = KIB * KIB;
        if self.size < KIB {
            format!("{} B", self.size)
        } else if self.size < MIB {
            format!("{:.1} KiB", self.size as f64 / KIB as f64)
        } else {
            format!("{:.1} MiB", self.size as f64 / MIB as f64)
        }
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

    /// Creates a private sibling backup from this destination's currently
    /// accepted bytes. Backup saves preserve the old row image as scratch only:
    /// fsyncing the backup persists the private file and never removes,
    /// replaces, or admits the managed `data.orna` row. A later editor temp
    /// rename must still pass through [`ManagedFile::rename_over`] and the
    /// validated activation boundary.
    pub async fn begin_temporary_backup(
        self: &Arc<Self>,
        max_file_bytes: usize,
    ) -> TemporarySave<S> {
        let baseline = Arc::clone(&self.state.lock().await.image);
        TemporarySave {
            target: Arc::clone(self),
            baseline: Arc::clone(&baseline),
            state: Mutex::new(TemporarySaveState {
                draft: EditDraft::open(baseline, max_file_bytes),
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

    /// Opens the `orna edit` read-to-save route: a retained strong read
    /// baseline plus a private draft bound to it, captured together.
    ///
    /// Unlike [`open_draft`], which baselines on whatever the destination holds
    /// at open time and accepts any save whose draft still matches it, the
    /// retained read image stays the baseline for the whole edit: the draft may
    /// only be saved while the destination still shows exactly the version the
    /// editor read. Capturing the image and the draft's baseline under one lock
    /// is what makes the retention exact — no change admitted between the two
    /// could be missed or double-counted.
    ///
    /// [`open_draft`]: ManagedFile::open_draft
    pub async fn open_read_baseline(
        self: &Arc<Self>,
        max_file_bytes: usize,
    ) -> EditReadBaseline<S> {
        let image = Arc::clone(&self.state.lock().await.image);
        EditReadBaseline {
            image: Arc::clone(&image),
            draft: EditDraft::open(image, max_file_bytes),
            epoch: self.cache_epoch.clone(),
        }
    }

    /// Saves a strong read-baseline edit through the validated activation
    /// boundary, refusing whenever the read version is no longer the accepted
    /// row version.
    ///
    /// The retained read image is the sole baseline: this checks that the
    /// destination still holds it *before* handing the candidate to activation.
    /// When a concurrent change has moved the destination on, the save returns
    /// [`TemporaryRenameOutcome::Stale`], the draft keeps its bytes and its
    /// retained-invalid copy for the caller to keep as a draft, and the
    /// activation boundary is never reached. This is exactly what a blind
    /// temp-create save cannot prove: a late temp-create has no witness for the
    /// version the editor originally read (VFS-011).
    pub async fn save_strong_with<F, Fut, E>(
        self: &Arc<Self>,
        baseline: &EditReadBaseline<S>,
        activate: F,
    ) -> Result<TemporaryRenameOutcome<S>, TemporaryRenameError<E>>
    where
        S: Send + Sync + 'static,
        F: FnOnce(ActivationCandidate<S>) -> Fut,
        Fut: Future<Output = Result<ActivationDecision<S>, E>>,
    {
        if !Arc::ptr_eq(&self.cache_epoch.generation, &baseline.epoch.generation) {
            return Err(TemporaryRenameError::WrongRepositoryScope);
        }
        let mut destination = self.state.lock().await;
        let reads_baseline =
            !destination.unlinked && Arc::ptr_eq(&destination.image, &baseline.image) && {
                let draft_state = baseline.draft.state.lock().await;
                Arc::ptr_eq(&draft_state.baseline, &baseline.image)
            };
        if !reads_baseline {
            let diagnostic = stale_baseline_diagnostic();
            retain_rejected_draft(&baseline.draft, &baseline.image, diagnostic).await;
            return Ok(TemporaryRenameOutcome::Stale { diagnostic });
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;
        match baseline.draft.fsync_with(activate).await {
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
                baseline.draft.rebase_accepted(Arc::clone(&image)).await;
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
    image: Arc<VfsFileSnapshot<S>>,
    draft: EditDraft<S>,
    epoch: SharedCacheEpoch,
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
fn stale_baseline_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Transient,
    }
}

/// Keeps a refused draft's bytes and its stable diagnostic, so a rejected
/// strong save is recoverable and is never silently dropped.
async fn retain_rejected_draft<S>(
    draft: &EditDraft<S>,
    baseline: &Arc<VfsFileSnapshot<S>>,
    diagnostic: SafeDiagnostic,
) {
    let mut draft_state = draft.state.lock().await;
    let revision = draft_state.revision;
    let replacement: Arc<[u8]> = Arc::from(
        draft_state
            .candidate
            .as_deref()
            .unwrap_or(draft_state.baseline.bytes.as_ref()),
    );
    draft_state.last_rejection = Some((revision, diagnostic));
    draft_state.retained_invalid = Some(RetainedInvalidDraft {
        baseline: baseline.pin.clone(),
        revision,
        replacement,
        diagnostic,
    });
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

// ---------------------------------------------------------------------------
// VFS-1 host namespace projection (ORNA-VFS-001, 002, 004, 016, 017, 018)
//
// A row directory addresses database/table/typed key. `data.orna` encodes the
// stored non-key fields and each stored Blob becomes an escaped sibling file
// named from its MIME-1 annotation. Nothing here materialises a competing
// authoritative copy: the projection records which stored field owns each host
// name, and it renders the row document from the stored field values the owner
// snapshot already holds.
// ---------------------------------------------------------------------------

/// Encoded component bound from the VFS-1 profile.
pub const VFS_MAX_COMPONENT_BYTES: usize = 200;
/// Repository-relative path bound from the VFS-1 profile.
pub const VFS_MAX_PATH_BYTES: usize = 1024;
/// The reserved projected name of the row document.
pub const VFS_ROW_DOCUMENT: &str = "data.orna";

/// `EAGAIN`: the captured baseline is no longer the accepted row (VFS-011).
pub const VFS_EAGAIN: i32 = 11;
/// `EINVAL`: malformed candidate, name, or document (VFS-001, VFS-016).
pub const VFS_EINVAL: i32 = 22;
/// `ENOENT`: no projected field or row at that address.
pub const VFS_ENOENT: i32 = 2;
/// `EPERM`: authority-refusing path or mutation (VFS-004, VFS-013).
pub const VFS_EPERM: i32 = 1;
/// `EOPNOTSUPP`: a VFS-1 unsupported operation (VFS-014).
pub const VFS_EOPNOTSUPP: i32 = 95;

/// Why a projected name, a resolved path, or a candidate document was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsPathError {
    /// Absolute paths, traversal spellings, separators inside one component,
    /// and any name outside the field's registered projection (VFS-004).
    OutsideProjection,
    /// The name does not belong to this row/field namespace (VFS-001).
    WrongNamespace,
    /// The encoded component or the whole path exceeds the VFS-1 bound (VFS-016).
    NameTooLong,
    /// Two distinct canonical names project to one host name (VFS-016).
    NameCollision,
    /// The candidate is not a complete stored non-key-field document (VFS-001).
    InvalidDocument,
    /// No stored field or row exists at that address (VFS-001).
    UnknownRow,
    /// The operation is unsupported in VFS-1 (VFS-013, VFS-014).
    Unsupported,
    /// The write baseline no longer matches the accepted row (VFS-011).
    StaleBaseline,
}

impl VfsPathError {
    /// The exact `errno` required by the VFS-1 profile error table.
    pub const fn errno(self) -> i32 {
        match self {
            Self::OutsideProjection => VFS_EPERM,
            Self::WrongNamespace | Self::NameTooLong | Self::NameCollision => VFS_EINVAL,
            Self::InvalidDocument => VFS_EINVAL,
            Self::UnknownRow => VFS_ENOENT,
            Self::Unsupported => VFS_EOPNOTSUPP,
            Self::StaleBaseline => VFS_EAGAIN,
        }
    }

    /// The stable Orna cause code kept alongside the host `errno`. Unknown
    /// rows report `ENOENT` with no Orna failure code of their own.
    pub const fn failure_code(self) -> Option<&'static str> {
        match self {
            Self::OutsideProjection | Self::Unsupported => Some("sys.vfs.unsupported"),
            Self::WrongNamespace | Self::NameTooLong | Self::InvalidDocument => {
                Some("sys.vfs.invalid_document")
            }
            Self::NameCollision => Some("sys.vfs.name_collision"),
            Self::StaleBaseline => Some("sys.vfs.stale_edit"),
            Self::UnknownRow => None,
        }
    }
}

/// One addressable projected component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VfsComponent {
    /// A component that spells its canonical typed key/field text directly.
    Canonical(String),
    /// A `~key-`/`~field-<digest>` long alias for a canonical name over the
    /// byte bound. Resolving it needs the snapshot-bound long-name index; the
    /// digest is verified there against the full retained name (VFS-016).
    LongAlias { digest: [u8; 32] },
}

/// The projection namespace a canonical component belongs to. Key and field
/// components share one host shape but keep distinct alias prefixes, so a key
/// alias can never resolve as a field name and vice versa (VFS-016).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsNameNamespace {
    /// One component of a typed table key.
    Key,
    /// One stored field path inside a row.
    Field,
}

impl VfsNameNamespace {
    const fn alias_prefix(self) -> &'static str {
        match self {
            Self::Key => "~key-",
            Self::Field => "~field-",
        }
    }
}

/// Projects one canonical typed key component to its host component, escaping
/// every byte outside `[A-Za-z0-9._-]` as uppercase `%HH` and escaping the
/// reserved names, trailing dots and DOS device basenames that would otherwise
/// alias. Names over the component bound become a deterministic `~key-` digest
/// alias instead of being dropped or merged (VFS-016).
pub fn project_component(text: &str) -> String {
    project_namespaced(text, VfsNameNamespace::Key)
}

/// Projects one stored field name to its host component. Field components use
/// the `~field-` alias prefix for names over the byte bound, so the same long
/// text in a key and in a field never lands on one host name (VFS-016).
pub fn project_field_name(text: &str) -> String {
    project_namespaced(text, VfsNameNamespace::Field)
}

fn project_namespaced(text: &str, namespace: VfsNameNamespace) -> String {
    if text.is_empty() {
        return "~empty".to_owned();
    }
    let out = escape_component_bytes(text.bytes().enumerate(), is_reserved_component(text));
    // A canonical name under the bound that already spells an alias prefix
    // (`~`, or any `%` spelling) is respelled, because the alias namespace is
    // reserved for names over the bound. The respelling is still an exact
    // `%HH` encoding of the same canonical text, so the name stays reversible
    // (VFS-016). Over the bound the digest alias is unreachable for a literal
    // `~key-` name only when the digest input is the canonical text itself.
    if out.len() <= VFS_MAX_COMPONENT_BYTES && !out.starts_with('~') {
        return out;
    }
    if out.len() > VFS_MAX_COMPONENT_BYTES {
        return long_alias(text, namespace);
    }
    // A canonical name under the bound whose own spelling starts with the
    // alias prefix is respelled by escaping its first byte, so it never claims
    // the alias namespace. The respelling is an exact `%HH` encoding of the
    // same canonical text and stays reversible (VFS-016).
    escape_component_bytes(text.bytes().enumerate(), true)
}

/// Escapes one canonical component's bytes: every byte outside
/// `[A-Za-z0-9._-]` becomes uppercase `%HH`, a reserved first byte is escaped,
/// and trailing dots are escaped so no host spelling ends in `.`.
fn escape_component_bytes<I>(bytes: I, force_first: bool) -> String
where
    I: Iterator<Item = (usize, u8)>,
{
    let mut out = String::new();
    for (index, byte) in bytes {
        let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        if allowed && !(force_first && index == 0) {
            out.push(byte as char);
        } else {
            out.push_str(&percent_byte(byte));
        }
    }
    let trailing = out.len() - out.trim_end_matches('.').len();
    if trailing > 0 {
        let stem = out.len() - trailing;
        out.truncate(stem);
        for _ in 0..trailing {
            out.push_str(&percent_byte(b'.'));
        }
    }
    out
}

fn percent_byte(byte: u8) -> String {
    format!("%{byte:02X}")
}

/// True for the names the profile reserves: `.`, `..`, `.git`, `.orna`, and
/// Windows device basenames in any case with any extension.
fn is_reserved_component(text: &str) -> bool {
    if matches!(text, "." | ".." | ".git" | ".orna") {
        return true;
    }
    let stem = text
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && matches!(stem.get(..3), Some("COM" | "LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// `~key-<64 hex SHA-256 of the canonical typed key component>` for keys and
/// `~field-<same digest>` for stored field names, per the VFS-1 long-name
/// rule. One derivation serves projection and verification, so a digest can
/// never name a component it did not come from (VFS-016).
fn long_alias(text: &str, namespace: VfsNameNamespace) -> String {
    format!(
        "{}{}",
        namespace.alias_prefix(),
        hex_digest(component_digest_input(text).as_bytes())
    )
}

/// The digest input for one long alias: the canonical typed component text
/// itself. Key and field namespaces stay distinct through the alias prefix, so
/// the same canonical text in a key and in a field never shares a host name.
fn component_digest_input(text: &str) -> &str {
    text
}

/// The digest bound into a `~field-` alias for one canonical field name. The
/// snapshot-bound long-name index re-derives it from the full retained name,
/// so a colliding digest is detected instead of merged (VFS-016).
pub fn field_name_digest(field: &str) -> [u8; 32] {
    Sha256::digest(component_digest_input(field).as_bytes()).into()
}

/// Resolves one `~field-` alias digest against a retained candidate field
/// name. The full name is re-derived and verified rather than trusted, so a
/// colliding digest is detected instead of merged (VFS-016).
pub fn resolve_field_alias(digest: &[u8; 32], candidates: &[&str]) -> Result<String, VfsPathError> {
    let mut resolved: Option<&str> = None;
    for candidate in candidates {
        if &field_name_digest(candidate) != digest {
            continue;
        }
        if resolved.is_some_and(|found| found != *candidate) {
            return Err(VfsPathError::NameCollision);
        }
        resolved = Some(candidate);
    }
    resolved.map(str::to_owned).ok_or(VfsPathError::UnknownRow)
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Projects one stored Blob field name to its sibling content-file name:
/// escaped field name plus the MIME-1 selected suffix. A field that would spell
/// the reserved row document keeps its own distinct name (VFS-016).
pub fn project_content_name(field: &str, media_type: &str, suffix: Option<&str>) -> String {
    let selected = match suffix {
        Some(hint) => hint.to_owned(),
        None => media_suffixes(media_type),
    };
    // The suffix is part of the projected component, so VFS-016's component
    // bound covers the whole `field + "." + suffix` spelling rather than the
    // escaped field base alone. A base that leaves no room for the suffix takes
    // the same `~field-` digest alias an over-bound field name takes: the alias
    // is derived from the stored field name, so the full name is still
    // re-derivable and verified by the long-name index (VFS-016).
    if project_field_name(field).len() + 1 + selected.len() > VFS_MAX_COMPONENT_BYTES {
        return format!(
            "{}.{}",
            long_alias(field, VfsNameNamespace::Field),
            selected
        );
    }
    let mut name = project_field_name(field);
    name.push('.');
    name.push_str(&selected);
    if name == VFS_ROW_DOCUMENT {
        name.replace_range(..1, &format!("%{:02X}", b'd'));
    }
    name
}

/// The MIME-1 suffix table: `(essence, preferred, compatible)`. One table
/// serves both projection and validation, so a hint can never be accepted that
/// the projection would not produce.
const MIME1_SUFFIXES: &[(&str, &str, &[&str])] = &[
    ("application/gzip", "gz", &["gz", "tar.gz", "tgz"]),
    ("application/json", "json", &["json"]),
    ("application/octet-stream", "bin", &["bin"]),
    ("application/pdf", "pdf", &["pdf"]),
    ("application/wasm", "wasm", &["wasm"]),
    ("application/zip", "zip", &["zip"]),
    ("audio/flac", "flac", &["flac"]),
    ("audio/mp4", "m4a", &["m4a", "m4b", "mp4", "mpg4"]),
    ("audio/mpeg", "mp3", &["mp1", "mp2", "mp3"]),
    ("audio/ogg", "ogg", &["oga", "ogg", "opus"]),
    ("audio/wav", "wav", &["wav"]),
    ("image/gif", "gif", &["gif"]),
    ("image/jpeg", "jpg", &["jpe", "jpeg", "jpg"]),
    ("image/png", "png", &["png"]),
    ("image/svg+xml", "svg", &["svg"]),
    ("image/webp", "webp", &["webp"]),
    ("text/css", "css", &["css"]),
    ("text/javascript", "js", &["js", "mjs"]),
    ("text/plain", "txt", &["text", "txt"]),
    ("video/mp4", "mp4", &["m4v", "mp4"]),
    ("video/webm", "webm", &["webm"]),
];

/// The MIME-1 essence of a media type, lowercased and stripped of parameters.
fn media_essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase()
}

/// The MIME-1 entry for an essence. An unknown essence keeps the profile's
/// `bin` hint instead of guessing from the media type text.
fn mime1_entry(media_type: &str) -> (&'static str, &'static [&'static str]) {
    let essence = media_essence(media_type);
    MIME1_SUFFIXES
        .iter()
        .find(|(name, _, _)| *name == essence)
        .map(|(_, preferred, compatible)| (*preferred, *compatible))
        .unwrap_or(("bin", &["bin"]))
}

/// The preferred MIME-1 suffix for an essence. Unknown essences keep the
/// profile's `bin` hint instead of guessing from the media type text.
fn media_suffixes(media_type: &str) -> String {
    mime1_entry(media_type).0.to_owned()
}

/// Whether `suffix` is a MIME-1 compatible hint for this media type. Only a
/// compatible spelling may be selected for a field (MIME-1, VFS-012).
pub fn content_suffix_is_compatible(media_type: &str, suffix: &str) -> bool {
    let suffix = suffix.trim().to_ascii_lowercase();
    mime1_entry(media_type).1.contains(&suffix.as_str())
}

/// The canonical stored hint for an editor-selected suffix (MIME-1, VFS-012).
///
/// `selected` is the suffix the sibling file now spells; `None` means the
/// projected preferred suffix. The result is the hint to record, or `None` when
/// the selection is the field's preferred suffix, which the profile stores as
/// an absent hint rather than a redundant one.
///
/// A rename changes only this hint: it never transcodes the stored bytes,
/// renames a column, or rekeys the row, and an incompatible suffix is refused
/// as a malformed candidate. The caller applies the result through the normal
/// CAS/activation boundary.
pub fn classify_content_suffix(
    media_type: &str,
    selected: Option<&str>,
) -> Result<Option<String>, VfsPathError> {
    let preferred = mime1_entry(media_type).0;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let selected = selected.trim().to_ascii_lowercase();
    if !content_suffix_is_compatible(media_type, &selected) {
        return Err(VfsPathError::InvalidDocument);
    }
    if selected == preferred {
        return Ok(None);
    }
    Ok(Some(selected))
}

/// Splits one mount-relative path and refuses absolute paths, empty
/// components, and traversal spellings before any name is resolved (VFS-004).
pub fn split_vfs_relative(path: &str) -> Result<Vec<&str>, VfsPathError> {
    if path.is_empty() || path.starts_with('/') || path.len() > VFS_MAX_PATH_BYTES {
        return Err(VfsPathError::OutsideProjection);
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(VfsPathError::OutsideProjection);
        }
        if component.contains('\\') {
            return Err(VfsPathError::OutsideProjection);
        }
        components.push(component);
    }
    Ok(components)
}

/// Decodes one projected component back to its canonical text, refusing any
/// spelling that is not exactly what the projection would produce. Aliases
/// therefore cannot target a different row or field.
pub fn unproject_component(component: &str) -> Result<VfsComponent, VfsPathError> {
    if let Some(hex) = component.strip_prefix("~key-") {
        if hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            let mut digest = [0u8; 32];
            for (index, slot) in digest.iter_mut().enumerate() {
                *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                    .map_err(|_| VfsPathError::NameCollision)?;
            }
            return Ok(VfsComponent::LongAlias { digest });
        }
        return Err(VfsPathError::WrongNamespace);
    }
    if component == "~empty" {
        return Ok(VfsComponent::Canonical(String::new()));
    }
    if component.contains('~') {
        return Err(VfsPathError::WrongNamespace);
    }
    if component.len() > VFS_MAX_COMPONENT_BYTES {
        return Err(VfsPathError::NameTooLong);
    }
    let mut text = Vec::with_capacity(component.len());
    let bytes = component.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = component
                .get(at + 1..at + 3)
                .ok_or(VfsPathError::WrongNamespace)?;
            if !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
            {
                return Err(VfsPathError::WrongNamespace);
            }
            text.push(u8::from_str_radix(hex, 16).map_err(|_| VfsPathError::WrongNamespace)?);
            at += 3;
            continue;
        }
        let byte = bytes[at];
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) {
            return Err(VfsPathError::WrongNamespace);
        }
        text.push(byte);
        at += 1;
    }
    let text = String::from_utf8(text).map_err(|_| VfsPathError::WrongNamespace)?;
    if project_component(&text) != component {
        return Err(VfsPathError::WrongNamespace);
    }
    Ok(VfsComponent::Canonical(text))
}

/// One stored field of a row as the shared store reports it. Content fields
/// carry only the MIME-1 annotation and descriptor size, never payload bytes,
/// so projecting a row reads no media (VFS-003, VFS-018).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsStoredField {
    name: String,
    kind: VfsStoredFieldKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VfsStoredFieldKind {
    /// A stored field rendered inside `data.orna` from its canonical text.
    Document { text: String },
    /// A stored optional field present as explicit `null`.
    Null,
    /// A stored Blob projected as a sibling content file. `required` is the
    /// schema's declaration: a required Blob can never be removed through the
    /// VFS, while an optional one may be unlinked to `null` (VFS-013).
    Content {
        media_type: String,
        suffix: Option<String>,
        descriptor_size: u64,
        required: bool,
    },
}

impl VfsStoredField {
    pub fn document(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: VfsStoredFieldKind::Document { text: text.into() },
        }
    }

    pub fn null(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: VfsStoredFieldKind::Null,
        }
    }

    /// A required stored Blob. Schema validity keeps its exact content
    /// descriptor, so unlinking its sibling file must fail (VFS-013).
    pub fn content(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor_size: u64,
    ) -> Self {
        Self::content_with_class(name, media_type, suffix, descriptor_size, true)
    }

    /// An optional stored Blob. Unlinking its sibling file may set the stored
    /// field to `null` after the row validates (VFS-013).
    pub fn content_optional(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor_size: u64,
    ) -> Self {
        Self::content_with_class(name, media_type, suffix, descriptor_size, false)
    }

    fn content_with_class(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor_size: u64,
        required: bool,
    ) -> Self {
        Self {
            name: name.into(),
            kind: VfsStoredFieldKind::Content {
                media_type: media_type.into(),
                suffix,
                descriptor_size,
                required,
            },
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> &VfsStoredFieldKind {
        &self.kind
    }
}

/// One sibling content file of a row directory. Its size is the exact content
/// descriptor size, so `stat` never touches payload bytes (VFS-018).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsProjectedFile {
    name: String,
    media_type: String,
    descriptor_size: u64,
    required: bool,
}

impl VfsProjectedFile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn descriptor_size(&self) -> u64 {
        self.descriptor_size
    }

    /// True when the schema declares this Blob required. A required Blob's
    /// sibling file can never be unlinked (VFS-013).
    pub const fn is_required(&self) -> bool {
        self.required
    }
}

/// The `data.orna` document and sibling content files projected for one and the
/// same row, pinned to the snapshot that admitted them (VFS-001, VFS-002).
pub struct VfsRowProjection<S> {
    pin: SnapshotPin<S>,
    document: String,
    files: Vec<VfsProjectedFile>,
}

impl<S> VfsRowProjection<S> {
    /// Projects one row's stored non-key fields. Document and null fields are
    /// encoded in order inside `data.orna`; content fields become escaped
    /// sibling files. Two distinct fields that would share one host name are
    /// refused rather than merged (VFS-016).
    pub fn project(pin: SnapshotPin<S>, fields: &[VfsStoredField]) -> Result<Self, VfsPathError> {
        let mut document = String::from("{\n");
        let mut files = Vec::new();
        let mut names = vec![VFS_ROW_DOCUMENT.to_owned()];
        let mut fields_seen = std::collections::BTreeSet::new();
        for field in fields {
            if !fields_seen.insert(field.name().to_owned()) {
                return Err(VfsPathError::InvalidDocument);
            }
            match field.kind() {
                VfsStoredFieldKind::Document { text } => {
                    document.push_str("    ");
                    document.push_str(&render_field_name(&field.name)?);
                    document.push_str(": ");
                    document.push_str(text);
                    document.push_str(",\n");
                }
                VfsStoredFieldKind::Null => {
                    document.push_str("    ");
                    document.push_str(&render_field_name(&field.name)?);
                    document.push_str(": null,\n");
                }
                VfsStoredFieldKind::Content {
                    media_type,
                    suffix,
                    descriptor_size,
                    required,
                } => {
                    let name = project_content_name(&field.name, media_type, suffix.as_deref());
                    // Two stored values that claim one host name are refused
                    // rather than merged into one directory entry (VFS-016).
                    if names.contains(&name) {
                        return Err(VfsPathError::NameCollision);
                    }
                    document.push_str("    ");
                    document.push_str(&render_field_name(&field.name)?);
                    document.push_str(": { path: \"./");
                    document.push_str(&name);
                    document.push_str("\", media_type: \"");
                    document.push_str(&escape_text(media_type));
                    document.push_str("\" },\n");
                    names.push(name.clone());
                    files.push(VfsProjectedFile {
                        name,
                        media_type: media_type.clone(),
                        descriptor_size: *descriptor_size,
                        required: *required,
                    });
                }
            }
        }
        document.push_str("}\n");
        if document.len() > VFS_MAX_PATH_BYTES * 1024 {
            return Err(VfsPathError::NameTooLong);
        }
        Ok(Self {
            pin,
            document,
            files,
        })
    }

    /// The snapshot this projection was admitted from. A projection never
    /// synthesizes a row version of its own (VFS-002, VFS-005).
    pub fn pin(&self) -> &SnapshotPin<S> {
        &self.pin
    }

    /// The complete stored-non-key-field document, ready to serve as
    /// `data.orna`.
    pub fn document_text(&self) -> &str {
        &self.document
    }

    /// Binds the served document bytes to the snapshot that produced them, so
    /// a read handle observes exactly the admitted row.
    pub fn document_snapshot(&self) -> VfsFileSnapshot<S> {
        VfsFileSnapshot::new(self.pin.clone_pin(), self.document.as_bytes())
    }

    /// Directory entries of this row, document first, then content files.
    pub fn entry_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(VFS_ROW_DOCUMENT).chain(self.files.iter().map(VfsProjectedFile::name))
    }

    pub fn files(&self) -> &[VfsProjectedFile] {
        &self.files
    }

    /// Resolves one entry name inside this row directory. A name outside the
    /// projection is refused, so a lookup can never retarget another row's
    /// field (VFS-004, VFS-017).
    pub fn lookup(&self, name: &str) -> Result<VfsProjectedEntry<'_>, VfsPathError> {
        if name == VFS_ROW_DOCUMENT {
            return Ok(VfsProjectedEntry::Document);
        }
        let file = self
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or(VfsPathError::UnknownRow)?;
        Ok(VfsProjectedEntry::Content(file))
    }

    /// Resolves one directory entry to its authorized unlink target (VFS-013).
    /// The row document is a complete stored-non-key-field document, so
    /// removing it is refused: row deletion is an explicit database operation,
    /// never a VFS side effect. A sibling file resolves to its stored Blob
    /// field, carrying the schema's required/optional class. A name outside the
    /// projection is resolved to no target, exactly like its lookup (VFS-004).
    pub fn resolve_unlink(&self, name: &str) -> Result<VfsUnlinkTarget<'_>, VfsPathError> {
        match self.lookup(name)? {
            VfsProjectedEntry::Document => Ok(VfsUnlinkTarget::RowDocument),
            VfsProjectedEntry::Content(file) => Ok(if file.is_required() {
                VfsUnlinkTarget::RequiredContent(file)
            } else {
                VfsUnlinkTarget::OptionalContent(file)
            }),
        }
    }
}

/// The authorized outcome of resolving one unlink inside a row directory. The
/// projection decides this from schema-issued field classes alone; a host name
/// never widens what an unlink may remove (VFS-013).
#[derive(Debug, Eq, PartialEq)]
pub enum VfsUnlinkTarget<'a> {
    /// `data.orna` itself: always refused (VFS-013).
    RowDocument,
    /// A required stored Blob: refused, because schema validity depends on the
    /// exact content descriptor (VFS-013).
    RequiredContent(&'a VfsProjectedFile),
    /// An optional stored Blob: the unlink may assign that field `null`
    /// through the validated activation boundary (VFS-013).
    OptionalContent(&'a VfsProjectedFile),
}

impl VfsUnlinkTarget<'_> {
    /// The exact `errno` this target requires, or `None` when the unlink may
    /// proceed to validation.
    pub fn refusal_errno(&self) -> Option<i32> {
        match self {
            Self::RowDocument | Self::RequiredContent(_) => Some(VFS_EPERM),
            Self::OptionalContent(_) => None,
        }
    }

    /// The stable Orna cause code kept alongside the host `errno` for a
    /// refused unlink.
    pub fn refusal_code(&self) -> Option<&'static str> {
        match self {
            Self::RowDocument | Self::RequiredContent(_) => Some("sys.vfs.unsupported"),
            Self::OptionalContent(_) => None,
        }
    }
}

/// One resolved entry of a projected row directory.
#[derive(Debug)]
pub enum VfsProjectedEntry<'a> {
    Document,
    Content(&'a VfsProjectedFile),
}

/// Escapes one Orna string-literal body. Stored field names are plain
/// identifiers, but a MIME-1 annotation can carry RFC 9110 quoting that must be
/// re-escaped to stay inside the row document.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// Renders one stored field name as the Orna record key used inside
/// `data.orna`. Only plain identifiers are valid, so a name that would need
/// quoting is refused instead of being written ambiguously.
fn render_field_name(name: &str) -> Result<String, VfsPathError> {
    let mut bytes = name.bytes();
    let valid = match bytes.next() {
        Some(first) if first.is_ascii_alphabetic() || first == b'_' => {
            bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }
        _ => false,
    };
    if !valid {
        return Err(VfsPathError::InvalidDocument);
    }
    Ok(name.to_owned())
}

/// One complete VFS row replacement routed into the runtime's validated table
/// activation boundary. The table identity is repository-issued admission
/// metadata; host paths never supply or override it.
pub struct VfsTableRowReplacement {
    mutation_id: [u8; 16],
    table: RuntimeTableIdentity,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
}

impl VfsTableRowReplacement {
    pub fn new(
        mutation_id: [u8; 16],
        table: RuntimeTableIdentity,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    ) -> Result<Self, RuntimeError> {
        let mutation = TableMutation::new(mutation_id, table.table(), key.clone(), value.clone())?
            .with_table_object_id(table.object_id());
        Ok(Self {
            mutation_id: mutation.id(),
            table,
            key: mutation.key().to_vec(),
            value,
        })
    }

    pub fn from_candidate<S>(
        mutation_id: [u8; 16],
        table: RuntimeTableIdentity,
        key: Vec<u8>,
        candidate: &ActivationCandidate<S>,
    ) -> Result<Self, RuntimeError> {
        let value = (!candidate.is_removal()).then(|| candidate.replacement_bytes().to_vec());
        Self::new(mutation_id, table, key, value)
    }

    pub fn table(&self) -> &RuntimeTableIdentity {
        &self.table
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    pub fn mutation(&self) -> Result<TableMutation, RuntimeError> {
        TableMutation::new(
            self.mutation_id,
            self.table.table(),
            self.key.clone(),
            self.value.clone(),
        )
        .map(|mutation| mutation.with_table_object_id(self.table.object_id()))
    }
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
    use std::sync::{
        Mutex as TestMutex,
        atomic::{AtomicUsize, Ordering},
    };

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

    #[test]
    fn unlink_refuses_row_document_and_required_blob_while_allowing_optional() {
        let row = VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(17_u64)),
            &[
                VfsStoredField::document("title", "\"Live in London\""),
                VfsStoredField::content("content", "audio/mpeg", None, 99),
                VfsStoredField::content_optional("cover", "image/jpeg", None, 4096),
                VfsStoredField::null("booklet"),
            ],
        )
        .expect("row projection");

        let content = project_content_name("content", "audio/mpeg", None);
        let cover = project_content_name("cover", "image/jpeg", None);
        assert_eq!(content, "content.mp3");
        assert_eq!(cover, "cover.jpg");

        // Deleting `data.orna` is refused: row deletion is an explicit database
        // operation, never a VFS unlink side effect (VFS-013).
        let document = row.resolve_unlink(VFS_ROW_DOCUMENT).expect("document");
        assert_eq!(document, VfsUnlinkTarget::RowDocument);
        assert_eq!(document.refusal_errno(), Some(VFS_EPERM));
        assert_eq!(document.refusal_code(), Some("sys.vfs.unsupported"));

        // A required Blob cannot be removed, because schema validity depends on
        // its exact content descriptor (VFS-013).
        let required = row.resolve_unlink(&content).expect("required blob");
        let VfsUnlinkTarget::RequiredContent(file) = &required else {
            panic!("required blob must resolve to a required target");
        };
        assert!(file.is_required());
        assert_eq!(required.refusal_errno(), Some(VFS_EPERM));
        assert_eq!(required.refusal_code(), Some("sys.vfs.unsupported"));

        // An optional Blob may be unlinked to `null`, so the target authorizes
        // validation instead of refusing in the projection (VFS-013).
        let optional = row.resolve_unlink(&cover).expect("optional blob");
        let VfsUnlinkTarget::OptionalContent(file) = &optional else {
            panic!("optional blob must resolve to an optional target");
        };
        assert!(!file.is_required());
        assert_eq!(optional.refusal_errno(), None);
        assert_eq!(optional.refusal_code(), None);

        // Field classes come from the schema, not the host name: a name outside
        // the projection resolves to no target at all (VFS-004).
        assert_eq!(
            row.resolve_unlink("missing.mp3"),
            Err(VfsPathError::UnknownRow)
        );
        assert_eq!(
            row.resolve_unlink("~key-not-a-field.mp3"),
            Err(VfsPathError::UnknownRow)
        );
    }

    #[test]
    fn suffix_selection_is_mime1_compatible_and_preferred_hint_is_absent() {
        // A compatible non-preferred spelling is stored as that explicit hint,
        // and the file name changes only in its suffix (MIME-1, VFS-012).
        assert_eq!(
            classify_content_suffix("audio/mpeg", Some("mp1")),
            Ok(Some("mp1".to_owned()))
        );
        assert_eq!(
            classify_content_suffix("audio/mpeg", Some("MP1")),
            Ok(Some("mp1".to_owned()))
        );
        assert_eq!(
            classify_content_suffix("image/jpeg", Some("jpeg")),
            Ok(Some("jpeg".to_owned()))
        );
        assert_eq!(
            classify_content_suffix("application/gzip", Some("tgz")),
            Ok(Some("tgz".to_owned()))
        );

        // The catalogued preferred suffix is recorded as an absent hint, never
        // as a redundant one (MIME-1 default_hint_canonicalization).
        assert_eq!(classify_content_suffix("audio/mpeg", Some("mp3")), Ok(None));
        assert_eq!(classify_content_suffix("audio/mpeg", None), Ok(None));

        // A suffix outside the MIME-1 entry is a malformed candidate rather
        // than a silent transcode or a rekeyed row (VFS-012).
        assert_eq!(
            classify_content_suffix("audio/mpeg", Some("m4a")),
            Err(VfsPathError::InvalidDocument)
        );
        assert_eq!(
            classify_content_suffix("image/jpeg", Some("png")),
            Err(VfsPathError::InvalidDocument)
        );
        // An unknown essence keeps only the profile's `bin` hint.
        assert_eq!(
            classify_content_suffix("application/x-unknown", Some("gz")),
            Err(VfsPathError::InvalidDocument)
        );
        assert_eq!(
            classify_content_suffix("application/x-unknown", Some("bin")),
            Ok(None)
        );

        // The suffix rename never changes the stored content identity: the same
        // descriptor size projects under the selected hint, and the row's bytes
        // are untouched.
        let renamed = project_content_name("content", "audio/mpeg", Some("mp1"));
        assert_eq!(renamed, "content.mp1");
        assert_eq!(
            project_content_name("content", "audio/mpeg", None),
            "content.mp3"
        );
    }

    #[test]
    fn vfs_table_row_replacement_preserves_admitted_table_identity() {
        let table = RuntimeTableIdentity::new("Song", crate::TableObjectId::new([0x71; 16]))
            .expect("valid table identity");
        let replacement = VfsTableRowReplacement::new(
            [0x55; 16],
            table.clone(),
            vec![0x18, 0x2a],
            Some(vec![0xa1]),
        )
        .expect("valid replacement");

        assert_eq!(replacement.table(), &table);
        assert_eq!(replacement.key(), &[0x18, 0x2a]);
        assert_eq!(replacement.value(), Some(&[0xa1][..]));
        let mutation = replacement.mutation().expect("valid mutation");
        assert_eq!(mutation.table(), "Song");
        assert_eq!(mutation.table_object_id(), Some(table.object_id()));
        assert_eq!(mutation.key(), &[0x18, 0x2a]);
        assert_eq!(mutation.value(), Some(&[0xa1][..]));
    }

    #[test]
    fn content_file_projection_uses_field_namespace_and_exact_entry_identity() {
        let long_field = "content_".repeat(40);
        let projected = project_content_name(&long_field, "audio/mpeg", None);
        assert!(projected.starts_with("~field-"));
        assert!(projected.ends_with(".mp3"));

        let row = VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::content(
                long_field.clone(),
                "audio/mpeg",
                None,
                99,
            )],
        )
        .expect("content field projection");
        assert_eq!(row.files()[0].name(), projected);
        let VfsProjectedEntry::Content(file) = row.lookup(&projected).expect("projected file")
        else {
            panic!("content lookup should return the projected file");
        };
        assert_eq!(file.descriptor_size(), 99);
        assert!(matches!(
            row.lookup("~key-not-a-field.mp3"),
            Err(VfsPathError::UnknownRow)
        ));
    }

    #[test]
    fn content_file_name_stays_within_the_component_bound_with_its_suffix() {
        // The projected content file is one host component, so VFS-016's bound
        // covers the whole `field.suffix` spelling. A field base that leaves no
        // room for its suffix takes the `~field-` digest alias instead of
        // overflowing the bound (VFS-016).
        let field = "f".repeat(200);
        let name = project_content_name(&field, "audio/mpeg", None);
        assert!(name.len() <= VFS_MAX_COMPONENT_BYTES);
        assert!(name.starts_with("~field-"));
        assert!(name.ends_with(".mp3"));
        // The alias is the field namespace digest of the stored name, so the
        // snapshot-bound long-name index re-derives and verifies the full name
        // rather than trusting the truncated spelling (VFS-016).
        assert_eq!(name, format!("~field-{}.mp3", hex_digest(field.as_bytes())));

        // A base with room for the suffix still spells the escaped name
        // directly; only the over-bound spelling changes.
        let field = "f".repeat(196);
        let name = project_content_name(&field, "audio/mpeg", None);
        assert!(name.len() <= VFS_MAX_COMPONENT_BYTES);
        assert!(!name.starts_with("~field-"));
        assert!(name.ends_with(".mp3"));

        // A long field base and the row document stay distinct names in one row
        // directory (VFS-016, VFS-017).
        let row = VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(23_u64)),
            &[
                VfsStoredField::document("title", "\"Live in London\""),
                VfsStoredField::content("f".repeat(200), "audio/mpeg", None, 99),
            ],
        )
        .expect("row projection");
        let long = row.files()[0].name().to_owned();
        assert!(long.len() <= VFS_MAX_COMPONENT_BYTES);
        assert_ne!(long, VFS_ROW_DOCUMENT);
        assert!(matches!(
            row.lookup(&long),
            Ok(VfsProjectedEntry::Content(_))
        ));
        assert!(matches!(
            row.lookup(VFS_ROW_DOCUMENT),
            Ok(VfsProjectedEntry::Document)
        ));
    }

    fn vfs_row_image() -> Arc<VfsFileSnapshot<u64>> {
        let contents = include_str!("../tests/fixtures/vfs-row-music-london.orna");
        Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(17)),
            Arc::<[u8]>::from(contents.as_bytes()),
        ))
    }

    struct ValidatedRowStore {
        accepted: TestMutex<Vec<u8>>,
        expected: Vec<u8>,
        transactions: AtomicUsize,
    }

    impl ValidatedRowStore {
        fn new(expected: &[u8]) -> Self {
            Self {
                accepted: TestMutex::new(expected.to_vec()),
                expected: expected.to_vec(),
                transactions: AtomicUsize::new(0),
            }
        }

        fn accepted(&self) -> Vec<u8> {
            self.accepted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn transaction_count(&self) -> usize {
            self.transactions.load(Ordering::SeqCst)
        }

        async fn activate(
            self: Arc<Self>,
            candidate: ActivationCandidate<u64>,
            accepted_snapshot: u64,
        ) -> Result<ActivationDecision<u64>, ()> {
            self.transactions.fetch_add(1, Ordering::SeqCst);
            if candidate.replacement_bytes() == self.expected.as_slice() {
                *self
                    .accepted
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    candidate.replacement_bytes().to_vec();
                Ok(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(accepted_snapshot),
                )))
            } else {
                Ok(ActivationDecision::Rejected(rejected()))
            }
        }
    }

    #[tokio::test]
    async fn direct_draft_commit_publishes_through_destination_and_rejects_stale_drafts() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");
        let rejected_bytes = include_str!("../tests/fixtures/case_closure_edge_tail_sixty.orna");
        let old_handle = target.open_read().await;
        assert!(old_handle.projection_is_current().await.unwrap());

        let draft = target.open_draft(1 << 20).await;
        let stale_draft = target.open_draft(1 << 20).await;

        draft.truncate(0).await.unwrap();
        draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let accepted = target
            .commit_draft_with(&draft, |candidate| async move {
                assert_eq!(candidate.replacement_bytes(), accepted_bytes.as_bytes());
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Applied { generation, handle } = accepted else {
            panic!("validated direct draft commit should be accepted");
        };
        assert_eq!(generation, 1);
        assert!(!old_handle.projection_is_current().await.unwrap());
        assert!(handle.projection_is_current().await.unwrap());
        assert_eq!(
            target.open_read().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );

        // A draft opened before the accepted commit cannot publish over it, so
        // the activation boundary is never reached.
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

        // A rejection retains the candidate and leaves the destination, the
        // generation, and the earlier handle's bytes alone.
        draft.truncate(0).await.unwrap();
        draft.write_at(0, rejected_bytes.as_bytes()).await.unwrap();
        let rejected_commit = target
            .commit_draft_with(&draft, |_| async {
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await
            .unwrap();
        assert!(matches!(
            rejected_commit,
            TemporaryRenameOutcome::Rejected { .. }
        ));
        assert_eq!(
            target.open_read().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        assert_eq!(
            draft
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes(),
            rejected_bytes.as_bytes()
        );
        assert_ne!(baseline.as_bytes(), accepted_bytes.as_bytes());
    }

    #[tokio::test]
    async fn unlink_hides_new_opens_and_keeps_earlier_handle_bytes() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");

        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let seeded = target
            .commit_draft_with(&draft, |_| async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        assert!(matches!(seeded, TemporaryRenameOutcome::Applied { .. }));
        let live = target.open_current().await.expect("linked before unlink");

        let refused = target
            .unlink_with(|_| async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) })
            .await
            .unwrap();
        assert!(matches!(refused, UnlinkOutcome::Rejected { .. }));
        assert_eq!(repository.generation().await, 1);
        assert!(target.open_current().await.is_some());

        let unlinked = target
            .unlink_with(|candidate| async move {
                assert!(candidate.is_removal());
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(9),
                )))
            })
            .await
            .unwrap();
        assert_eq!(unlinked, UnlinkOutcome::Applied { generation: 2 });
        assert_eq!(repository.generation().await, 2);
        assert!(target.open_current().await.is_none());
        assert!(!live.projection_is_current().await.unwrap());
        assert_eq!(
            live.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );

        // Replacements against the removed destination are refused before any
        // activation, and a second unlink is a no-op rather than another one.
        let late_calls = Arc::new(AtomicUsize::new(0));
        let late_calls_by_route = Arc::clone(&late_calls);
        let late = target.open_draft(1 << 20).await;
        late.truncate(0).await.unwrap();
        late.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let stale = target
            .commit_draft_with(&late, move |_| async move {
                late_calls_by_route.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await
            .unwrap();
        assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
        assert_eq!(late_calls.load(Ordering::SeqCst), 0);
        let again = target
            .unlink_with(|_| async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) })
            .await
            .unwrap();
        assert_eq!(again, UnlinkOutcome::AlreadyUnlinked);
        assert_eq!(repository.generation().await, 2);
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

    #[test]
    fn stat_human_size_switches_units_at_kib_and_mib_boundaries() {
        let sized = |size: u64| ManagedFileStat {
            size,
            pin: SnapshotPin::capture(Arc::new(7_u64)),
            generation: 0,
        };
        assert_eq!(sized(0).human_size(), "0 B");
        assert_eq!(sized(1023).human_size(), "1023 B");
        assert_eq!(sized(1024).human_size(), "1.0 KiB");
        assert_eq!(sized(1536).human_size(), "1.5 KiB");
        assert_eq!(sized(1024 * 1024 - 1).human_size(), "1024.0 KiB");
        assert_eq!(sized(1024 * 1024).human_size(), "1.0 MiB");
        assert_eq!(sized(3 * 1024 * 1024 + 512 * 1024).human_size(), "3.5 MiB");
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
    async fn vfs_editor_backup_temp_and_truncate_patterns_share_one_validated_transaction() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(vfs_row_image()).await.unwrap();
        let row_text = include_str!("../tests/fixtures/vfs-row-music-london.orna").as_bytes();
        let row_store = Arc::new(ValidatedRowStore::new(row_text));

        // A backup file is private scratch seeded with the accepted row bytes.
        // Fsyncing it persists only that scratch and cannot admit a row.
        let backup = target.begin_temporary_backup(1 << 20).await;
        assert_eq!(backup.candidate_bytes().await.as_ref(), row_text);
        let backup_syncs = Arc::new(AtomicUsize::new(0));
        let backup_syncs_by_route = Arc::clone(&backup_syncs);
        let backup_revision = backup
            .fsync_scratch_with(move |revision, bytes| {
                backup_syncs_by_route.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(revision, 0);
                    assert_eq!(bytes.as_ref(), row_text);
                    Ok::<_, ()>(())
                }
            })
            .await
            .unwrap();
        assert_eq!(backup_revision, 0);
        let backup_syncs_by_route = Arc::clone(&backup_syncs);
        let repeated_backup_revision = backup
            .fsync_scratch_with(move |_, _| {
                backup_syncs_by_route.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, ()>(()) }
            })
            .await
            .unwrap();
        assert_eq!(repeated_backup_revision, 0);
        assert_eq!(backup_syncs.load(Ordering::SeqCst), 1);
        assert_eq!(row_store.transaction_count(), 0);
        assert_eq!(repository.generation().await, 0);

        // A temp-file editor may fsync scratch first; only rename over the
        // managed target crosses the validated row-store transaction boundary.
        let save = target.begin_temporary_replacement(1 << 20).await;
        save.write_at(0, row_text).await.unwrap();
        let temp_syncs = Arc::new(AtomicUsize::new(0));
        let temp_syncs_by_route = Arc::clone(&temp_syncs);
        let temp_revision = save
            .fsync_scratch_with(move |revision, bytes| {
                temp_syncs_by_route.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(revision, 2);
                    assert_eq!(bytes.as_ref(), row_text);
                    Ok::<_, ()>(())
                }
            })
            .await
            .unwrap();
        assert_eq!(temp_revision, 2);
        let temp_syncs_by_route = Arc::clone(&temp_syncs);
        let repeated_temp_revision = save
            .fsync_scratch_with(move |_, _| {
                temp_syncs_by_route.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, ()>(()) }
            })
            .await
            .unwrap();
        assert_eq!(repeated_temp_revision, 2);
        assert_eq!(temp_syncs.load(Ordering::SeqCst), 1);
        assert_eq!(row_store.transaction_count(), 0);

        let row_store_for_rename = Arc::clone(&row_store);
        let applied = target
            .rename_over(&save, move |candidate| {
                Arc::clone(&row_store_for_rename).activate(candidate, 18)
            })
            .await
            .unwrap();
        assert!(matches!(
            applied,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(row_store.transaction_count(), 1);
        assert_eq!(row_store.accepted(), row_text);
        assert_eq!(repository.generation().await, 1);

        let row_store_for_repeat = Arc::clone(&row_store);
        let repeated = target
            .rename_over(&save, move |candidate| {
                Arc::clone(&row_store_for_repeat).activate(candidate, 19)
            })
            .await
            .unwrap();
        assert!(matches!(
            repeated,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(row_store.transaction_count(), 1);

        // In-place truncate+write is a distinct editor pattern, but it uses
        // the same single validated activation boundary as temp rename.
        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, row_text).await.unwrap();
        let row_store_for_direct = Arc::clone(&row_store);
        let direct = target
            .commit_draft_with(&draft, move |candidate| {
                Arc::clone(&row_store_for_direct).activate(candidate, 19)
            })
            .await
            .unwrap();
        assert!(matches!(
            direct,
            TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(row_store.transaction_count(), 2);
        assert_eq!(row_store.accepted(), row_text);
        assert_eq!(repository.generation().await, 2);
    }

    #[tokio::test]
    async fn vfs_rejected_editor_saves_keep_row_store_and_expose_typed_diagnostic() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(vfs_row_image()).await.unwrap();
        let row_text = include_str!("../tests/fixtures/vfs-row-music-london.orna").as_bytes();
        let invalid = &row_text[..row_text.len() / 2];
        let row_store = Arc::new(ValidatedRowStore::new(row_text));

        let save = target.begin_temporary_replacement(1 << 20).await;
        save.write_at(0, invalid).await.unwrap();
        let row_store_for_rename = Arc::clone(&row_store);
        let rejected_rename = target
            .rename_over(&save, move |candidate| {
                Arc::clone(&row_store_for_rename).activate(candidate, 18)
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Rejected { diagnostic } = rejected_rename else {
            panic!("invalid temp rename must be rejected");
        };
        assert_eq!(diagnostic, rejected());
        assert_eq!(row_store.transaction_count(), 1);
        assert_eq!(row_store.accepted(), row_text);
        assert_eq!(
            target.open_read().await.read_at(0, row_text.len()),
            row_text
        );
        let retained = save.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes(), invalid);
        assert_eq!(retained.diagnostic(), rejected());

        let row_store_for_repeat = Arc::clone(&row_store);
        let repeated = target
            .rename_over(&save, move |candidate| {
                Arc::clone(&row_store_for_repeat).activate(candidate, 19)
            })
            .await
            .unwrap();
        assert!(matches!(
            repeated,
            TemporaryRenameOutcome::Rejected {
                diagnostic
            } if diagnostic == rejected()
        ));
        assert_eq!(row_store.transaction_count(), 1);
        assert_eq!(repository.generation().await, 0);

        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, invalid).await.unwrap();
        let row_store_for_direct = Arc::clone(&row_store);
        let rejected_direct = target
            .commit_draft_with(&draft, move |candidate| {
                Arc::clone(&row_store_for_direct).activate(candidate, 20)
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Rejected { diagnostic } = rejected_direct else {
            panic!("invalid direct write must be rejected");
        };
        assert_eq!(diagnostic, rejected());
        assert_eq!(row_store.transaction_count(), 2);
        assert_eq!(row_store.accepted(), row_text);
        assert_eq!(
            target.open_read().await.read_at(0, row_text.len()),
            row_text
        );
        let retained = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes(), invalid);
        assert_eq!(retained.diagnostic(), rejected());
        assert_eq!(repository.generation().await, 0);
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
    async fn repository_scoped_rename_refuses_a_foreign_cache_scope() {
        let repository = VfsRepositoryCache::new();
        let unrelated = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let foreign_image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(7_u64)),
            Arc::<[u8]>::from(
                include_str!("../tests/fixtures/publication-repository-main.orna").as_bytes(),
            ),
        ));
        let foreign = unrelated.managed_file(foreign_image).await.unwrap();
        let foreign_save = foreign.begin_temporary_replacement(1 << 20).await;
        foreign_save.write_at(0, &[0x58]).await.unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_route = Arc::clone(&calls);
        let outcome = repository
            .rename_over(&target, &foreign_save, move |_| async move {
                calls_by_route.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await;
        assert!(matches!(
            outcome,
            Err(TemporaryRenameError::WrongDestination)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(repository.generation().await, 0);
        assert_eq!(unrelated.generation().await, 0);

        // A scratch tied to a destination in a different repository scope is
        // refused the same way, and the target keeps its accepted bytes.
        let save = target.begin_temporary_replacement(1 << 20).await;
        save.write_at(0, &[0x59]).await.unwrap();
        let foreign_route = unrelated
            .rename_over(&target, &save, |_| async {
                Ok::<_, ()>(ActivationDecision::Rejected(rejected()))
            })
            .await;
        assert!(matches!(
            foreign_route,
            Err(TemporaryRenameError::WrongRepositoryScope)
        ));
        assert_eq!(repository.generation().await, 0);
        let baseline = include_str!("../tests/fixtures/publication-repository-main.orna");
        assert_eq!(
            target.open_read().await.read_at(0, baseline.len()),
            baseline.as_bytes()
        );
    }

    #[tokio::test]
    async fn accepted_rename_rebases_the_next_draft_on_the_live_image() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let accepted_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");

        let save = target.begin_temporary_replacement(1 << 20).await;
        save.write_at(0, accepted_bytes.as_bytes()).await.unwrap();
        let applied = target
            .rename_over(&save, |_| async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        assert!(matches!(
            applied,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));

        // A draft opened after the accepted rename compares against the live
        // image, so it is not stale and it commits instead of being refused.
        let next = target.open_draft(1 << 20).await;
        assert_eq!(
            next.baseline().await.read_at(0, accepted_bytes.len()),
            accepted_bytes.as_bytes()
        );
        next.write_at(0, &[0x5A]).await.unwrap();
        let committed = target
            .commit_draft_with(&next, |candidate| async move {
                assert_eq!(*candidate.baseline().snapshot(), 8);
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(11),
                )))
            })
            .await
            .unwrap();
        assert!(matches!(
            committed,
            TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(target.open_read().await.read_at(0, 1), [0x5A]);
    }

    /// A strong read baseline refuses the save once a concurrent REPL change
    /// has moved the row on, keeps the rejected draft, and never reaches
    /// activation; a save whose read is still current applies. An open read
    /// keeps the version it captured in both cases.
    #[tokio::test]
    async fn strong_read_baseline_refuses_a_save_after_a_concurrent_repl_change() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let read_bytes = include_str!("../tests/fixtures/publication-repository-main.orna");
        let repl_bytes = include_str!("../tests/fixtures/checkpoint_snapshot_v1.orna");
        let draft_bytes = include_str!("../tests/fixtures/case_closure_edge_tail_sixty.orna");

        // The editor reads the row and retains that exact version, with its own
        // draft over the version it read.
        let baseline = target.open_read_baseline(1 << 20).await;
        let read_handle = baseline.baseline();
        assert_eq!(
            read_handle.read_at(0, read_bytes.len()),
            read_bytes.as_bytes()
        );
        baseline.draft().truncate(0).await.unwrap();
        baseline
            .draft()
            .write_at(0, draft_bytes.as_bytes())
            .await
            .unwrap();

        // A concurrent REPL change is accepted first, so the row no longer
        // holds the version the editor read.
        let repl = target.open_draft(1 << 20).await;
        repl.truncate(0).await.unwrap();
        repl.write_at(0, repl_bytes.as_bytes()).await.unwrap();
        let repl_commit = target
            .commit_draft_with(&repl, |_| async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(8),
                )))
            })
            .await
            .unwrap();
        assert!(matches!(
            repl_commit,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));

        // The editor's save is refused on its read baseline: activation is
        // never reached and the draft is retained rather than discarded.
        let activation_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&activation_calls);
        let stale = target
            .save_strong_with(&baseline, move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, ()>(ActivationDecision::Rejected(rejected())) }
            })
            .await
            .unwrap();
        assert!(matches!(stale, TemporaryRenameOutcome::Stale { .. }));
        assert_eq!(activation_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            baseline
                .draft()
                .retained_invalid_draft()
                .await
                .expect("a refused read-baseline save keeps its draft")
                .replacement_bytes(),
            draft_bytes.as_bytes()
        );
        // The REPL change is still what the row holds, and the read handle
        // still reads the version it captured.
        assert_eq!(
            target.open_read().await.read_at(0, repl_bytes.len()),
            repl_bytes.as_bytes()
        );
        assert_eq!(
            read_handle.read_at(0, read_bytes.len()),
            read_bytes.as_bytes()
        );

        // A read whose version is still the accepted one saves normally, so
        // the refusal above is the baseline check and not a broken route.
        let current = target.open_read_baseline(1 << 20).await;
        current.draft().truncate(0).await.unwrap();
        current
            .draft()
            .write_at(0, draft_bytes.as_bytes())
            .await
            .unwrap();
        let applied = target
            .save_strong_with(&current, |_| async {
                Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                    Arc::new(11),
                )))
            })
            .await
            .unwrap();
        assert!(matches!(
            applied,
            TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(
            target.open_read().await.read_at(0, draft_bytes.len()),
            draft_bytes.as_bytes()
        );
    }
}
