//! FUSE-free state for projected file handles and EDIT-1 replacement drafts.
//!
//! `S` is the repository owner's opaque row-map snapshot (in production,
//! `orna_repository::row_store::RowMapSnapshot`). Keeping that value in a
//! [`SnapshotPin`](crate::vfs::SnapshotPin) binds each open handle and readdir
//! cursor to one immutable row-map version without giving this module a second
//! row authority.

use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    io,
    path::Path,
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use orna_repository::{
    ContentIdentity, NativeObjectId, OrpBlobBinding, RepositoryCaptureCapability,
};
use tokio::sync::Mutex;

use super::{
    DiagnosticCode, FaultInjector, RuntimeError, RuntimeState, SafeDiagnostic, TableActivationError,
};

mod draft;
mod naming;
mod save;
mod spool;

pub use draft::{EditDraft, EditReadBaseline, RetainedInvalidDraft};
pub use naming::{
    VFS_EAGAIN, VFS_EINVAL, VFS_EIO, VFS_ENOENT, VFS_EOPNOTSUPP, VFS_EPERM,
    VFS_MAX_COMPONENT_BYTES, VFS_MAX_PATH_BYTES, VFS_ROW_DOCUMENT, VfsComponent, VfsNameNamespace,
    VfsPathError, classify_content_suffix, content_suffix_is_compatible, field_name_digest,
    project_colliding_table_component, project_component, project_content_name, project_field_name,
    project_key_component, project_text_name, unproject_component,
};
pub use save::{
    VfsBlobReplacementError, VfsDurableSave, VfsSaveAdmission, VfsSaveCandidate,
    VfsSaveCoordinates, VfsSaveError, VfsTableRowReplacement, commit_vfs_table_activation,
};
pub use spool::QuotaCharge;
pub use spool::{
    CANDIDATE_CHUNK_BYTES, CandidateReader, CandidateSnapshot, DraftQuota, DraftStorage,
    EditDraftError, default_draft_quota,
};

use draft::{retain_rejected_draft, stale_baseline_diagnostic};
use save::{VfsSaveAdmissionError, capture_vfs_blob_replacement};

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
pub struct VfsRepositoryCache {
    epoch: SharedCacheEpoch,
    draft_storage: DraftStorage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsRepositoryCacheError {
    ImageAlreadyScoped,
}

impl Default for VfsRepositoryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsRepositoryCache {
    /// A view whose EDIT-1 spools live in the process temp directory.
    pub fn new() -> Self {
        Self {
            epoch: SharedCacheEpoch::default(),
            draft_storage: DraftStorage::with_default_quota(),
        }
    }

    /// A view whose EDIT-1 spools live in the runtime root's private draft
    /// directory, created `0700` when absent, charged to the process-wide
    /// default aggregate quota.
    pub fn in_runtime_root(root: &Path) -> io::Result<Self> {
        Ok(Self {
            epoch: SharedCacheEpoch::default(),
            draft_storage: DraftStorage::in_runtime_root(root, default_draft_quota())?,
        })
    }

    /// As [`Self::in_runtime_root`], but every spool in this view is charged
    /// to `quota`. It is `pub` because the orna-cli mount (a separate crate)
    /// builds its view from an already-constructed budget. The mount passes the
    /// production budget in the default build; only the orna-cli `test-seams`
    /// feature substitutes a smaller one. No other caller passes a non-default
    /// budget.
    pub fn in_runtime_root_with_quota(root: &Path, quota: Arc<DraftQuota>) -> io::Result<Self> {
        Ok(Self {
            epoch: SharedCacheEpoch::default(),
            draft_storage: DraftStorage::in_runtime_root(root, quota)?,
        })
    }

    /// The aggregate draft quota this view's spools are charged to. Recovery
    /// storage charges its on-disk records to the same budget.
    pub fn draft_quota(&self) -> Arc<DraftQuota> {
        self.draft_storage.quota()
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

    /// Commits a temporary Blob replacement through the owner of this view's
    /// shared row transaction and projection epoch.
    /// The ten parameters are one durable save: the target file, the staged
    /// scratch, the runtime and capture it commits through, the canonical
    /// identity and media values, the admission callback and the fault seam.
    /// Each caller supplies all of them from a single request record.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_durable_blob_rename<S, A, AFut>(
        &self,
        target: &Arc<ManagedFile<S>>,
        scratch: &TemporarySave<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        capture: &RepositoryCaptureCapability,
        expected_identity: ContentIdentity,
        media_type: &str,
        suffix: Option<&str>,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        S: Send + Sync + 'static,
        A: FnOnce(VfsSaveCoordinates, CandidateSnapshot, OrpBlobBinding) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, TableActivationError>>,
    {
        if !Arc::ptr_eq(&target.cache_epoch.generation, &self.epoch.generation) {
            return Err(VfsSaveError::WrongRepositoryScope);
        }
        target
            .commit_durable_temporary_blob_rename(
                scratch,
                runtime,
                save,
                capture,
                expected_identity,
                media_type,
                suffix,
                admit,
                faults,
            )
            .await
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
            operation: Mutex::new(()),
            cache_epoch: self.epoch.clone(),
            draft_storage: self.draft_storage.clone(),
        })
    }
}

/// A retained owner-issued immutable row-map snapshot and any additional
/// owner-issued resources needed to keep its generation readable.
pub struct SnapshotPin<S> {
    snapshot: Arc<S>,
    projection: Arc<OnceLock<CacheProjection>>,
    retention: Option<Arc<dyn Any + Send + Sync>>,
}

impl<S> Clone for SnapshotPin<S> {
    fn clone(&self) -> Self {
        Self {
            snapshot: Arc::clone(&self.snapshot),
            projection: Arc::clone(&self.projection),
            retention: self.retention.as_ref().map(Arc::clone),
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
            retention: None,
        }
    }

    /// Captures a row-map generation together with an owner-issued lease for
    /// resources that must remain readable through that generation, such as
    /// protected media references. Cloned pins and every VFS reader/cursor
    /// derived from them share the lease; it is released after the last clone.
    pub fn capture_with_retention<R>(snapshot: Arc<S>, retention: Arc<R>) -> Self
    where
        S: Any + Send + Sync + 'static,
        R: Any + Send + Sync + 'static,
    {
        Self {
            projection: snapshot_projection_cell(&snapshot),
            snapshot,
            retention: Some(retention),
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
    /// Makes a provisional image pin over this row-map snapshot without
    /// inheriting resource leases that belong only to the retired generation.
    fn clone_without_retention(&self) -> Self {
        Self {
            snapshot: Arc::clone(&self.snapshot),
            projection: Arc::clone(&self.projection),
            retention: None,
        }
    }

    pub fn projection(&self) -> Option<&CacheProjection> {
        self.projection.get()
    }
}

/// Immutable projected file bytes paired with the row-map snapshot that
/// admitted them.
pub struct VfsFileSnapshot<S> {
    pin: SnapshotPin<S>,
    bytes: Arc<CandidateSnapshot>,
}

impl<S> VfsFileSnapshot<S> {
    pub fn new(pin: SnapshotPin<S>, bytes: impl Into<Arc<[u8]>>) -> Self {
        let bytes: Arc<[u8]> = bytes.into();
        Self::from_candidate(pin, CandidateSnapshot::from(bytes))
    }

    /// Wraps a candidate view, which may stream from a private spool rather
    /// than hold the bytes in memory.
    pub fn from_candidate(pin: SnapshotPin<S>, bytes: CandidateSnapshot) -> Self {
        Self {
            pin,
            bytes: Arc::new(bytes),
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
        self.bytes.len()
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

    /// Reads at most `limit` bytes from this handle's captured image. A
    /// spool-backed image is read from its private file, never copied whole.
    pub fn read_at(&self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
        self.image.bytes.read_range(offset, limit)
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
    operation: Mutex<()>,
    cache_epoch: SharedCacheEpoch,
    draft_storage: DraftStorage,
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
        let draft = Arc::new(EditDraft::open(
            Arc::clone(&baseline),
            max_file_bytes,
            self.draft_storage.clone(),
        ));
        // A typical editor temp starts empty; its commit baseline remains the
        // managed destination image captured above.
        draft
            .truncate(0)
            .await
            .expect("zero-length scratch is within every file quota");
        TemporarySave {
            target: Arc::clone(self),
            baseline,
            operation: Mutex::new(()),
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
            operation: Mutex::new(()),
            state: Mutex::new(TemporarySaveState {
                draft: Arc::new(EditDraft::open(
                    baseline,
                    max_file_bytes,
                    self.draft_storage.clone(),
                )),
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

        let _operation = self.operation.lock().await;
        let mut destination = self.state.lock().await;
        let _scratch_operation = scratch.operation.lock().await;
        let mut scratch_state = scratch.state.lock().await;
        if let Some((generation, handle)) = &scratch_state.applied {
            return Ok(TemporaryRenameOutcome::Applied {
                generation: *generation,
                handle: handle.clone(),
            });
        }
        if destination.unlinked || !Arc::ptr_eq(&destination.image, &scratch.baseline) {
            let diagnostic = stale_baseline_diagnostic();
            let mut draft = scratch_state.draft.state.lock().await;
            let revision = draft.revision;
            draft
                .retain_rejection(revision, diagnostic)
                .map_err(TemporaryRenameError::Backing)?;
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
            Err(error) => Err(rename_failure(error)),
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
        EditDraft::open(baseline, max_file_bytes, self.draft_storage.clone())
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
            draft: EditDraft::open(image, max_file_bytes, self.draft_storage.clone()),
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
        let _operation = self.operation.lock().await;
        let mut destination = self.state.lock().await;
        let reads_baseline =
            !destination.unlinked && Arc::ptr_eq(&destination.image, &baseline.image) && {
                let draft_state = baseline.draft.state.lock().await;
                Arc::ptr_eq(&draft_state.baseline, &baseline.image)
            };
        if !reads_baseline {
            let diagnostic = stale_baseline_diagnostic();
            retain_rejected_draft(&baseline.draft, diagnostic)
                .await
                .map_err(TemporaryRenameError::Backing)?;
            return Ok(TemporaryRenameOutcome::Stale { diagnostic });
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;
        match baseline.draft.fsync_with(activate).await {
            Err(error) => Err(rename_failure(error)),
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
        let _operation = self.operation.lock().await;
        let mut destination = self.state.lock().await;
        let (based_on_current, revision) = {
            let draft_state = draft.state.lock().await;
            (
                !destination.unlinked && Arc::ptr_eq(&destination.image, &draft_state.baseline),
                draft_state.revision,
            )
        };
        if !based_on_current {
            let diagnostic = stale_baseline_diagnostic();
            // A stale candidate is still a rejected candidate. Without this the
            // draft's bytes and diagnostic would be dropped with the handle,
            // while the temp-rename route keeps them readable (ORNA-VFS-009).
            draft
                .state
                .lock()
                .await
                .retain_rejection(revision, diagnostic)
                .map_err(TemporaryRenameError::Backing)?;
            return Ok(TemporaryRenameOutcome::Stale { diagnostic });
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;

        match draft.fsync_with(activate).await {
            Err(error) => Err(rename_failure(error)),
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
        let _operation = self.operation.lock().await;
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
    draft: Arc<EditDraft<S>>,
    synced_revision: Option<u64>,
    applied: Option<(u64, SnapshotReadHandle<S>)>,
}

/// A temp-file save tied to the managed destination that admitted its
/// creation. Scratch fsync is a persistence callback only; row acceptance is
/// performed by renaming it over the destination.
pub struct TemporarySave<S> {
    target: Arc<ManagedFile<S>>,
    baseline: Arc<VfsFileSnapshot<S>>,
    operation: Mutex<()>,
    state: Mutex<TemporarySaveState<S>>,
}

impl<S> TemporarySave<S> {
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        let _operation = self.operation.lock().await;
        let state = self.state.lock().await;
        if state.applied.is_some() {
            return Err(EditDraftError::AlreadyApplied);
        }
        state.draft.write_at(offset, bytes).await
    }

    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let _operation = self.operation.lock().await;
        let state = self.state.lock().await;
        if state.applied.is_some() {
            return Err(EditDraftError::AlreadyApplied);
        }
        state.draft.truncate(length).await
    }
    /// Whole-value copy for commit only. Live-mount reads, writes and retention
    /// use [`Self::read_range`].
    pub async fn candidate_bytes(&self) -> Result<Arc<[u8]>, EditDraftError> {
        self.state.lock().await.draft.candidate_bytes().await
    }

    /// A streaming view of the candidate for the replay fallback that retains it.
    /// No candidate bytes are copied; see [`EditDraft::candidate_snapshot`].
    pub async fn candidate_snapshot(&self) -> Result<CandidateSnapshot, EditDraftError> {
        self.state.lock().await.draft.candidate_snapshot().await
    }

    /// The candidate's length in bytes.
    pub async fn candidate_len(&self) -> u64 {
        self.state.lock().await.draft.candidate_len().await
    }

    /// Reads `[offset, offset + length)` of the candidate; see
    /// [`EditDraft::read_range`].
    pub async fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>, EditDraftError> {
        self.state
            .lock()
            .await
            .draft
            .read_range(offset, length)
            .await
    }

    pub async fn retained_invalid_draft(&self) -> Option<RetainedInvalidDraft<S>> {
        self.state.lock().await.draft.retained_invalid_draft().await
    }

    /// Persists only the private scratch sequence. The callback receives a
    /// streaming [`CandidateSnapshot`] of the fsynced revision, so no whole
    /// copy of the candidate is made. This callback does not call the row
    /// activation boundary and is skipped for an already-synced seq.
    pub async fn fsync_scratch_with<F, Fut, E>(&self, persist: F) -> Result<u64, FsyncFailure<E>>
    where
        F: FnOnce(u64, CandidateSnapshot) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let _operation = self.operation.lock().await;
        let (revision, snapshot) = {
            let state = self.state.lock().await;
            if let Some((revision, _)) = &state.applied {
                return Ok(*revision);
            }
            let draft = state.draft.state.lock().await;
            let revision = draft.revision;
            if state.synced_revision == Some(revision) {
                return Ok(revision);
            }
            let snapshot = draft.candidate_snapshot().map_err(FsyncFailure::Backing)?;
            (revision, snapshot)
        };
        persist(revision, snapshot)
            .await
            .map_err(FsyncFailure::Activation)?;
        self.state.lock().await.synced_revision = Some(revision);
        Ok(revision)
    }
}

/// The stable Orna cause code for a VFS save refusal, or `None` when the code
/// has no VFS profile spelling of its own. `sys.vfs.stale_edit` is the cause a
/// superseded baseline reports, alongside `EAGAIN` (ORNA-VFS-011).
pub const fn vfs_stale_failure_code(code: DiagnosticCode) -> Option<&'static str> {
    match code {
        DiagnosticCode::StaleBaseline => Some("sys.vfs.stale_edit"),
        _ => None,
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
    /// The private candidate spool could not be read or written.
    Backing(EditDraftError),
    NoCandidate,
}

/// Why a validated activation through a draft did not produce a decision.
#[derive(Debug)]
pub enum FsyncFailure<E> {
    /// The private candidate spool could not be read.
    Backing(EditDraftError),
    /// The activation callback itself failed.
    Activation(E),
}

fn rename_failure<E>(failure: FsyncFailure<E>) -> TemporaryRenameError<E> {
    match failure {
        FsyncFailure::Backing(error) => TemporaryRenameError::Backing(error),
        FsyncFailure::Activation(error) => TemporaryRenameError::Activation(error),
    }
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
    ///
    /// A sibling file can be renamed to select a compatible suffix, and the
    /// resulting save rewrites the row. The stored value is then rebuilt from
    /// the descriptor this variant carries: the captured Blob identity and its
    /// exact matching values, taken from the field the save was admitted from
    /// and never re-derived from current state (VFS-008, VFS-020). A file that
    /// reassembles a row without them has no captured descriptor to preserve.
    Content {
        media_type: String,
        suffix: Option<String>,
        descriptor_size: u64,
        required: bool,
        captured: Option<CapturedContentDescriptor>,
    },
    /// A stored Str longer than 64 KiB projected as a sibling `<field>.txt`
    /// file. `data.orna` names the file with `{ text: "./<file>", encoding:
    /// "utf-8" }`; the file's bytes are the stored string. `required` mirrors
    /// the schema's declaration, as for a Blob (VFS-013).
    Text { length: u64, required: bool },
}

/// The captured Blob descriptor of one stored content field, as the shared
/// store reported it for the row version the projection was admitted from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedContentDescriptor {
    length: u64,
    sha256: [u8; 32],
    descriptor_oid: NativeObjectId,
}

impl CapturedContentDescriptor {
    /// A descriptor for the Blob the store captured: its byte length, the
    /// SHA-256 of its raw bytes, and the OID of the OGS-1 descriptor node that
    /// holds its byte range. The OID is what an accepted row keeps, so a save
    /// that does not replace the payload can name it again instead of minting
    /// a new one.
    pub fn new(length: u64, sha256: [u8; 32], descriptor_oid: NativeObjectId) -> Self {
        Self {
            length,
            sha256,
            descriptor_oid,
        }
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// The OGS-1 descriptor node holding the Blob's byte range.
    pub fn descriptor_oid(&self) -> &NativeObjectId {
        &self.descriptor_oid
    }
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
        Self::content_with_class(name, media_type, suffix, descriptor_size, true, None)
    }

    /// A required stored Blob that a save admitted from the captured field can
    /// rebuild without re-deriving the descriptor (VFS-008).
    pub fn captured_content(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor: CapturedContentDescriptor,
        required: bool,
    ) -> Self {
        Self::content_with_class(
            name,
            media_type,
            suffix,
            descriptor.length(),
            required,
            Some(descriptor),
        )
    }

    /// An optional stored Blob. Unlinking its sibling file may set the stored
    /// field to `null` after the row validates (VFS-013).
    pub fn content_optional(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor_size: u64,
    ) -> Self {
        Self::content_with_class(name, media_type, suffix, descriptor_size, false, None)
    }

    fn content_with_class(
        name: impl Into<String>,
        media_type: impl Into<String>,
        suffix: Option<String>,
        descriptor_size: u64,
        required: bool,
        captured: Option<CapturedContentDescriptor>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: VfsStoredFieldKind::Content {
                media_type: media_type.into(),
                suffix,
                descriptor_size,
                required,
                captured,
            },
        }
    }

    /// A stored Str longer than 64 KiB, projected as a sibling text file. A
    /// required Str can never be unlinked; an optional one may be set `null`.
    pub fn text(name: impl Into<String>, length: u64, required: bool) -> Self {
        Self {
            name: name.into(),
            kind: VfsStoredFieldKind::Text { length, required },
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
    captured: Option<CapturedContentDescriptor>,
    text: bool,
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

    /// The captured Blob descriptor this sibling file was projected from, when
    /// the store supplied one. A save admitted from this file reuses it instead
    /// of re-deriving the descriptor (VFS-008).
    pub fn captured_descriptor(&self) -> Option<&CapturedContentDescriptor> {
        self.captured.as_ref()
    }

    /// The captured descriptor one save may rebuild this content field from.
    ///
    /// `digest` is the SHA-256 of the bytes the save holds for this file. The
    /// descriptor belongs to the payload the store captured, so it is returned
    /// only while the saved bytes are exactly that payload: same length and
    /// same digest. A save that replaced the payload drops it, and the row's
    /// value is minted from the new bytes rather than keeping a descriptor that
    /// no longer names them (VFS-008, VFS-020).
    pub fn admitted_capture(
        &self,
        length: u64,
        digest: &[u8; 32],
    ) -> Option<&CapturedContentDescriptor> {
        self.captured
            .as_ref()
            .filter(|descriptor| descriptor.length() == length && descriptor.sha256() == digest)
    }

    /// True when the schema declares this Blob required. A required Blob's
    /// sibling file can never be unlinked (VFS-013).
    pub const fn is_required(&self) -> bool {
        self.required
    }

    /// True when this sibling is a stored Str projected as UTF-8 text
    /// (Str>64KiB) rather than a Blob.
    pub const fn is_text(&self) -> bool {
        self.text
    }
}

/// A nested record, tuple, list or map field projected as its own directory
/// (profiles/vfs-1.md:75). Its host name is the escaped field name, the base a
/// sibling Text or Blob takes, so the two never share one host name (VFS-016).
pub struct VfsProjectedDirectory {
    field: String,
    name: String,
}

impl VfsProjectedDirectory {
    /// The stored field this directory projects.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// The host directory name inside the row.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The `data.orna` document and sibling content files projected for one and the
/// same row, pinned to the snapshot that admitted them (VFS-001, VFS-002).
pub struct VfsRowProjection<S> {
    pin: SnapshotPin<S>,
    document: String,
    files: Vec<VfsProjectedFile>,
    directories: Vec<VfsProjectedDirectory>,
}

impl<S> VfsRowProjection<S> {
    /// Projects one row's stored non-key fields. Document and null fields are
    /// encoded in order inside `data.orna`; content fields become escaped
    /// sibling files. Two distinct fields that would share one host name are
    /// refused rather than merged (VFS-016).
    pub fn project(pin: SnapshotPin<S>, fields: &[VfsStoredField]) -> Result<Self, VfsPathError> {
        Self::project_with_nested_directories(pin, fields, &[])
    }

    /// Projects one row like `project`, and also projects each named Document field as a
    /// nested directory (profiles/vfs-1.md:75). The directory's host name is the escaped
    /// field name, the base a sibling Text or Blob takes, so it takes the same collision
    /// refusal (VFS-016).
    pub fn project_with_nested_directories(
        pin: SnapshotPin<S>,
        fields: &[VfsStoredField],
        nested_directory_fields: &[&str],
    ) -> Result<Self, VfsPathError> {
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
                    captured,
                } => {
                    let name = project_content_name(&field.name, media_type, suffix.as_deref())?;
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
                        captured: captured.clone(),
                        text: false,
                    });
                }
                VfsStoredFieldKind::Text { length, required } => {
                    let name = project_text_name(&field.name)?;
                    if names.contains(&name) {
                        return Err(VfsPathError::NameCollision);
                    }
                    document.push_str("    ");
                    document.push_str(&render_field_name(&field.name)?);
                    document.push_str(": { text: \"./");
                    document.push_str(&name);
                    document.push_str("\", encoding: \"utf-8\" },\n");
                    names.push(name.clone());
                    files.push(VfsProjectedFile {
                        name,
                        media_type: "text/plain".to_owned(),
                        descriptor_size: *length,
                        required: *required,
                        captured: None,
                        text: true,
                    });
                }
            }
        }
        let mut directories = Vec::with_capacity(nested_directory_fields.len());
        for field in nested_directory_fields {
            let name = project_field_name(field);
            // A nested directory is one more host name in the row, so it takes the same
            // collision refusal as a sibling file (VFS-016).
            if names.contains(&name) {
                return Err(VfsPathError::NameCollision);
            }
            names.push(name.clone());
            directories.push(VfsProjectedDirectory {
                field: (*field).to_owned(),
                name,
            });
        }
        document.push_str("}\n");
        if document.len() > VFS_MAX_PATH_BYTES * 1024 {
            return Err(VfsPathError::NameTooLong);
        }
        Ok(Self {
            pin,
            document,
            files,
            directories,
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

    pub fn files(&self) -> &[VfsProjectedFile] {
        &self.files
    }

    /// Nested directories of this row, one per nested record, tuple, list or map field.
    pub fn directories(&self) -> &[VfsProjectedDirectory] {
        &self.directories
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

impl<S: Send + Sync + 'static> ManagedFile<S> {
    /// Saves an editor document through the durable shared-row transaction.
    pub async fn commit_durable_document_save<A, AFut>(
        self: &Arc<Self>,
        draft: &EditDraft<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        A: FnOnce(VfsSaveCoordinates, Arc<[u8]>) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, TableActivationError>>,
    {
        if super::activation::runtime_activation_active(runtime) {
            return Err(VfsSaveError::Activation(TableActivationError::Runtime(
                RuntimeError::AdminBusy,
            )));
        }
        let _operation = self.operation.lock().await;
        let _draft_operation = draft.operation.lock().await;
        self.commit_durable_save(
            draft,
            runtime,
            save,
            move |coordinates, bytes| async move {
                let bytes = bytes
                    .to_arc()
                    .map_err(|_| VfsSaveAdmissionError::Backing(EditDraftError::BackingIo))?;
                admit(coordinates, bytes)
                    .await
                    .map_err(VfsSaveAdmissionError::Activation)
            },
            faults,
        )
        .await
    }

    /// Captures replacement bytes as a protected ORP Blob and admits the full
    /// typed row through the same revision-checked transaction as document
    /// saves. The caller receives the graph-issued binding so source admission
    /// can bind its canonical Blob field and pin into one row mutation.
    /// `media_type` and `suffix` must already be canonical MIME-1 annotation
    /// values, as issued by the source schema.
    /// Same durable save as `commit_durable_blob_rename`, addressed through the
    /// managed file's owner rather than the managed file itself.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_durable_temporary_blob_rename<A, AFut>(
        self: &Arc<Self>,
        scratch: &TemporarySave<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        capture: &RepositoryCaptureCapability,
        expected_identity: ContentIdentity,
        media_type: &str,
        suffix: Option<&str>,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        A: FnOnce(VfsSaveCoordinates, CandidateSnapshot, OrpBlobBinding) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, TableActivationError>>,
    {
        self.commit_durable_temporary_save(
            scratch,
            runtime,
            save,
            move |coordinates, bytes| async move {
                let binding = capture_vfs_blob_replacement(
                    capture,
                    &bytes,
                    expected_identity,
                    media_type,
                    suffix,
                )
                .map_err(VfsSaveAdmissionError::BlobCapture)?;
                let candidate = admit(coordinates, bytes, binding)
                    .await
                    .map_err(VfsSaveAdmissionError::Activation)?;
                Ok(candidate)
            },
            faults,
        )
        .await
    }

    /// Adopts one complete VFS row-document save through the same durable
    /// validation and transaction boundary the CLI uses, then publishes it to
    /// the projection cache (ORNA-VFS-006, VFS-009, VFS-021).
    ///
    /// The row is committed to the runtime's own state database, so an accepted
    /// save is visible to any reopen of that database and a rejected one is not
    /// durably accepted at all. `admit` turns the candidate bytes into the
    /// complete typed row replacement; this module never invents a second row
    /// authority (VFS-002). Stale and rejected candidates retain their bytes and
    /// diagnostic through the draft (VFS-009). `faults` is the commit-boundary
    /// seam: crash recovery is exercised through the same runtime transaction a
    /// normal save uses.
    async fn commit_durable_save<A, AFut>(
        self: &Arc<Self>,
        draft: &EditDraft<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        A: FnOnce(VfsSaveCoordinates, CandidateSnapshot) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, VfsSaveAdmissionError>>,
    {
        let destination = self.state.lock().await;
        let mut draft_state = draft.state.lock().await;
        // A draft that never submitted bytes has nothing to accept or reject, so a
        // moved baseline does not make it stale; no rejection is retained for it.
        if draft_state.candidate.is_none() {
            return Err(VfsSaveError::NoCandidate);
        }
        if destination.unlinked || !Arc::ptr_eq(&destination.image, &draft_state.baseline) {
            let diagnostic = stale_baseline_diagnostic();
            let generation = *self.cache_epoch.generation.lock().await;
            let revision = draft_state.revision;
            draft_state
                .retain_rejection(revision, diagnostic)
                .map_err(VfsSaveError::Backing)?;
            return Err(VfsSaveError::Stale {
                generation,
                diagnostic,
            });
        }
        let revision = draft_state.revision;
        // Repeated callbacks for one sequence return the recorded outcome; a
        // later write begins a new candidate based on accepted state, so one
        // revision is never admitted twice (VFS-008).
        if let Some((rejected, diagnostic)) = draft_state.last_rejection
            && rejected == revision
        {
            let generation = *self.cache_epoch.generation.lock().await;
            return Err(VfsSaveError::Rejected {
                generation,
                diagnostic,
            });
        }
        if let Some((accepted_revision, image)) = &draft_state.last_accepted
            && *accepted_revision == revision
        {
            let generation = *self.cache_epoch.generation.lock().await;
            return Ok(TemporaryRenameOutcome::Applied {
                generation,
                handle: SnapshotReadHandle::open(Arc::clone(image)),
            });
        }
        if draft_state.candidate.is_none() {
            return Err(VfsSaveError::NoCandidate);
        }
        // The candidate is a view over the draft's own bytes. The admitted row
        // and the accepted image share it, so no candidate copy is made here.
        let snapshot = draft_state
            .candidate_snapshot()
            .map_err(VfsSaveError::Backing)?;
        drop(draft_state);
        drop(destination);

        let admission = VfsSaveAdmission::<S>::begin(runtime, save, faults)
            .await
            .map_err(VfsSaveError::Activation)?;
        // A repeated accepted rename is served from `scratch_state.applied`
        // above. Matching replacement bytes alone cannot prove that another
        // writer committed this save; baseline mismatches therefore fail closed.
        let admitted_row = admission
            .admitted_row()
            .map_err(VfsSaveError::AdmissionLookup)?;
        if admission
            .expected_baseline_matches(admitted_row)
            .is_some_and(|matches| !matches)
        {
            let diagnostic = stale_baseline_diagnostic();
            draft
                .state
                .lock()
                .await
                .retain_retryable_conflict(revision, diagnostic)
                .map_err(VfsSaveError::Backing)?;
            return Err(VfsSaveError::Stale {
                generation: *self.cache_epoch.generation.lock().await,
                diagnostic,
            });
        }
        let writer = admission.writer;
        let coordinates = admission.coordinates();
        let callback_snapshot = snapshot
            .duplicate()
            .map_err(|_| VfsSaveError::Backing(EditDraftError::BackingIo))?;
        let candidate =
            super::activation::with_activation_scope(runtime, writer, move || async move {
                admit(coordinates, callback_snapshot).await
            })
            .await
            .map_err(|error| VfsSaveError::Activation(TableActivationError::Runtime(error)))?
            .map_err(|error| match error {
                VfsSaveAdmissionError::BlobCapture(error) => VfsSaveError::BlobCapture(error),
                VfsSaveAdmissionError::Activation(error) => VfsSaveError::Activation(error),
                VfsSaveAdmissionError::Backing(error) => VfsSaveError::Backing(error),
            })?;
        admission
            .validate_replacement(candidate.replacement())
            .map_err(VfsSaveError::Activation)?;
        // Admission callbacks may read or mutate the shared projection epoch.
        // Acquire its lock only after that callback and baseline check finish.
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = match cache_generation.checked_add(1) {
            Some(next) => next,
            None => {
                return Err(VfsSaveError::Activation(TableActivationError::Runtime(
                    RuntimeError::RecoveryInvalid,
                )));
            }
        };
        match admission.commit(candidate).await {
            Err(TableActivationError::ValidationFailed(diagnostic)) => {
                draft
                    .state
                    .lock()
                    .await
                    .retain_rejection(revision, diagnostic)
                    .map_err(VfsSaveError::Backing)?;
                return Err(VfsSaveError::Rejected {
                    generation: *cache_generation,
                    diagnostic,
                });
            }
            Err(TableActivationError::Runtime(RuntimeError::StaleTableSchema)) => {
                let diagnostic = stale_baseline_diagnostic();
                draft
                    .state
                    .lock()
                    .await
                    .retain_retryable_conflict(revision, diagnostic)
                    .map_err(VfsSaveError::Backing)?;
                return Err(VfsSaveError::Stale {
                    generation: *cache_generation,
                    diagnostic,
                });
            }
            Err(error) => return Err(VfsSaveError::Activation(error)),
            Ok(_capture) => {}
        }
        {
            // The runtime transaction has committed; now publish its accepted
            // image under short-lived VFS state locks. Operation gates kept the
            // destination and draft stable while those locks were released.
            let mut destination = self.state.lock().await;
            let mut draft_state = draft.state.lock().await;
            let accepted = Arc::new(VfsFileSnapshot::from_candidate(
                draft_state.baseline.pin.clone_without_retention(),
                snapshot,
            ));
            let image = Arc::new(VfsFileSnapshot::with_projection(
                &accepted,
                CacheProjection {
                    epoch: self.cache_epoch.clone(),
                    generation: next_generation,
                },
            ));
            draft_state.drop_candidate();
            draft_state.last_rejection = None;
            draft_state.retained_invalid = None;
            draft_state.baseline = Arc::clone(&image);
            draft_state.last_accepted = Some((revision, Arc::clone(&image)));
            destination.image = Arc::clone(&image);
            *cache_generation = next_generation;
            Ok(TemporaryRenameOutcome::Applied {
                generation: next_generation,
                handle: SnapshotReadHandle::open(image),
            })
        }
    }

    /// Commits a temporary editor replacement through the same durable row
    /// transaction as a document save. Unlike the cache-only rename route, this
    /// route requires the exact row value observed when the temporary was made;
    /// a changed durable row is rejected before activation (VFS-011).
    pub async fn commit_durable_temporary_rename<A, AFut>(
        self: &Arc<Self>,
        scratch: &TemporarySave<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        A: FnOnce(VfsSaveCoordinates, CandidateSnapshot) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, TableActivationError>>,
    {
        self.commit_durable_temporary_save(
            scratch,
            runtime,
            save,
            move |coordinates, bytes| async move {
                admit(coordinates, bytes)
                    .await
                    .map_err(VfsSaveAdmissionError::Activation)
            },
            faults,
        )
        .await
    }

    async fn commit_durable_temporary_save<A, AFut>(
        self: &Arc<Self>,
        scratch: &TemporarySave<S>,
        runtime: &RuntimeState,
        save: VfsDurableSave,
        admit: A,
        faults: &dyn FaultInjector,
    ) -> Result<TemporaryRenameOutcome<S>, VfsSaveError<TableActivationError>>
    where
        A: FnOnce(VfsSaveCoordinates, CandidateSnapshot) -> AFut,
        AFut: Future<Output = Result<VfsSaveCandidate, VfsSaveAdmissionError>>,
    {
        if !Arc::ptr_eq(self, &scratch.target) {
            return Err(VfsSaveError::WrongDestination);
        }
        if save.expected_baseline_row.is_none() {
            return Err(VfsSaveError::MissingBaseline);
        }
        if super::activation::runtime_activation_active(runtime) {
            return Err(VfsSaveError::Activation(TableActivationError::Runtime(
                RuntimeError::AdminBusy,
            )));
        }
        // Lock order (ORNA-VFS-019, ornadb-eufo.2.2): the VFS operation gates
        // below are taken first, then the VFS projection epoch lock
        // (`cache_epoch.generation`, taken in `commit_durable_save` after the
        // admission callback and held across `admission.commit`), then the
        // SQLite writer transaction inside `admission.commit`. The committed
        // table schema identity is read inside that transaction, before
        // validation and again immediately before COMMIT
        // (`require_live_table_schema` in lib.rs). No lock covers git refs, so
        // a schema commit landing after the last read is not serialized with
        // this save; that residual is recorded in CHANGELOG.md.
        let _operation = self.operation.lock().await;
        let _scratch_operation = scratch.operation.lock().await;
        let draft = {
            let scratch_state = scratch.state.lock().await;
            if let Some((generation, handle)) = &scratch_state.applied {
                return Ok(TemporaryRenameOutcome::Applied {
                    generation: *generation,
                    handle: handle.clone(),
                });
            }
            Arc::clone(&scratch_state.draft)
        };
        let _draft_operation = draft.operation.lock().await;
        let outcome = self
            .commit_durable_save(&draft, runtime, save, admit, faults)
            .await?;
        if let TemporaryRenameOutcome::Applied { generation, handle } = &outcome {
            scratch.state.lock().await.applied = Some((*generation, handle.clone()));
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests;
