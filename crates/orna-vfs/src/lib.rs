//! FUSE-free state for projected file handles and EDIT-1 replacement drafts.
//!
//! `S` is the repository owner's opaque row-map snapshot (in production,
//! `orna_repository::row_store::RowMapSnapshot`). Keeping that value in a
//! [`SnapshotPin`](orna_runtime::SnapshotPin) binds each open handle and readdir
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

use orna_runtime::{FaultInjector, RuntimeError, RuntimeState, TableActivationError};
use orna_stream::{DiagnosticCode, SafeDiagnostic};

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

use draft::{FsyncMemo, retain_rejected_draft, stale_baseline_diagnostic};
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
            state: Mutex::new(ManagedFileState::Live { image }),
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

/// Whether a managed destination still names a live row file.
pub(crate) enum ManagedFileState<S> {
    /// The destination is linked. New opens and replacements observe this
    /// image, and an accepted replacement advances it.
    Live { image: Arc<VfsFileSnapshot<S>> },
    /// An accepted unlink removed the destination. Existing handles keep the
    /// image they captured, and a destination-level read still serves it, but
    /// no new open, stat or replacement observes it.
    Unlinked { image: Arc<VfsFileSnapshot<S>> },
}

impl<S> ManagedFileState<S> {
    /// The last accepted image, readable even after an unlink.
    fn image(&self) -> &Arc<VfsFileSnapshot<S>> {
        match self {
            Self::Live { image } | Self::Unlinked { image } => image,
        }
    }

    /// The image a new open observes, or `None` once the destination is gone.
    fn live_image(&self) -> Option<&Arc<VfsFileSnapshot<S>>> {
        match self {
            Self::Live { image } => Some(image),
            Self::Unlinked { .. } => None,
        }
    }

    fn is_unlinked(&self) -> bool {
        matches!(self, Self::Unlinked { .. })
    }

    /// Retires the destination while keeping its last image readable for
    /// handles that already hold it. A second unlink changes nothing.
    fn mark_unlinked(&mut self) {
        if let Self::Live { image } = self {
            *self = Self::Unlinked {
                image: Arc::clone(image),
            };
        }
    }
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
        SnapshotReadHandle::open(Arc::clone(self.state.lock().await.image()))
    }

    /// Opens the destination as it currently exists. Returns `None` after an
    /// accepted unlink, so a new open can never see the removed file's bytes.
    pub async fn open_current(&self) -> Option<SnapshotReadHandle<S>> {
        let state = self.state.lock().await;
        state
            .live_image()
            .map(|image| SnapshotReadHandle::open(Arc::clone(image)))
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
            let image = state.live_image()?;
            (image.len(), image.pin().clone_pin())
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
        let baseline = Arc::clone(self.state.lock().await.image());
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
                sync: ScratchSync::Unsaved,
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
        let baseline = Arc::clone(self.state.lock().await.image());
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
                sync: ScratchSync::Unsaved,
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
        if let ScratchSync::Applied { generation, handle } = &scratch_state.sync {
            return Ok(TemporaryRenameOutcome::Applied {
                generation: *generation,
                handle: handle.clone(),
            });
        }
        if destination.is_unlinked() || !Arc::ptr_eq(destination.image(), &scratch.baseline) {
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
                *destination = ManagedFileState::Live {
                    image: Arc::clone(&image),
                };
                *cache_generation = next_generation;
                let handle = SnapshotReadHandle::open(image);
                scratch_state.sync = ScratchSync::Applied {
                    generation: next_generation,
                    handle: handle.clone(),
                };
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
        let baseline = Arc::clone(self.state.lock().await.image());
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
        let image = Arc::clone(self.state.lock().await.image());
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
            !destination.is_unlinked() && Arc::ptr_eq(destination.image(), &baseline.image) && {
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
                *destination = ManagedFileState::Live {
                    image: Arc::clone(&image),
                };
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
                !destination.is_unlinked()
                    && Arc::ptr_eq(destination.image(), &draft_state.baseline),
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
                *destination = ManagedFileState::Live {
                    image: Arc::clone(&image),
                };
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
        if destination.is_unlinked() {
            return Ok(UnlinkOutcome::AlreadyUnlinked);
        }
        let mut cache_generation = self.cache_epoch.generation.lock().await;
        let next_generation = cache_generation
            .checked_add(1)
            .ok_or(TemporaryRenameError::GenerationExhausted)?;
        let candidate = ActivationCandidate {
            baseline: destination.image().pin.clone_pin(),
            revision: 0,
            replacement: Arc::from(&[][..]),
            removal: true,
        };
        match activate(candidate)
            .await
            .map_err(TemporaryRenameError::Activation)?
        {
            ActivationDecision::Accepted(_) => {
                destination.mark_unlinked();
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
    sync: ScratchSync<S>,
}

/// How far a temporary file's scratch sequence has advanced.
pub(crate) enum ScratchSync<S> {
    /// No scratch sequence has been persisted yet.
    Unsaved,
    /// `fsync` persisted one draft revision as scratch.
    Synced { revision: u64 },
    /// A rename admitted the scratch file. Later writes are refused, and a
    /// repeated save returns this recorded outcome.
    Applied {
        generation: u64,
        handle: SnapshotReadHandle<S>,
    },
}

impl<S> TemporarySaveState<S> {
    fn is_applied(&self) -> bool {
        matches!(&self.sync, ScratchSync::Applied { .. })
    }
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
        if state.is_applied() {
            return Err(EditDraftError::AlreadyApplied);
        }
        state.draft.write_at(offset, bytes).await
    }

    pub async fn truncate(&self, length: u64) -> Result<(), EditDraftError> {
        let _operation = self.operation.lock().await;
        let state = self.state.lock().await;
        if state.is_applied() {
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
            let synced = match &state.sync {
                ScratchSync::Applied { generation, .. } => return Ok(*generation),
                ScratchSync::Synced { revision } => Some(*revision),
                ScratchSync::Unsaved => None,
            };
            let draft = state.draft.state.lock().await;
            let revision = draft.revision;
            if synced == Some(revision) {
                return Ok(revision);
            }
            let snapshot = draft.candidate_snapshot().map_err(FsyncFailure::Backing)?;
            (revision, snapshot)
        };
        persist(revision, snapshot)
            .await
            .map_err(FsyncFailure::Activation)?;
        self.state.lock().await.sync = ScratchSync::Synced { revision };
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
        if orna_runtime::runtime_activation_active(runtime) {
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
        if destination.is_unlinked() || !Arc::ptr_eq(destination.image(), &draft_state.baseline) {
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
        if let Some(diagnostic) = draft_state.rejection_for(revision) {
            let generation = *self.cache_epoch.generation.lock().await;
            return Err(VfsSaveError::Rejected {
                generation,
                diagnostic,
            });
        }
        if let Some(image) = draft_state.accepted_for(revision) {
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
        // A repeated accepted rename is served from `scratch_state.sync`
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
        let candidate = orna_runtime::with_activation_scope(runtime, writer, move || async move {
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
            draft_state.retained_invalid = None;
            draft_state.baseline = Arc::clone(&image);
            draft_state.memo = FsyncMemo::Accepted {
                revision,
                image: Arc::clone(&image),
            };
            *destination = ManagedFileState::Live {
                image: Arc::clone(&image),
            };
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
        if orna_runtime::runtime_activation_active(runtime) {
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
            if let ScratchSync::Applied { generation, handle } = &scratch_state.sync {
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
            scratch.state.lock().await.sync = ScratchSync::Applied {
                generation: *generation,
                handle: handle.clone(),
            };
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::spool::{CandidateSource, DRAFT_SPOOL_DIRECTORY};
    use super::*;
    use orna_repository::{ProtectedContentTransfer, Repository};
    use orna_runtime::{
        ProtectedContentTransferEvidence, RuntimeIdentity, RuntimeQueryError, RuntimeState,
        RuntimeTableIdentity, RuntimeTableRows, TableActivationCandidateValidator, TableMutation,
        TableObjectId, TableSchemaIdentity,
    };
    use orna_stream::DiagnosticClass;
    use orna_value::{Raw, SchemaDescriptor};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::sync::{
        Mutex as TestMutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::{
        collections::BTreeMap,
        io::Write,
        path::Path,
        process::{Command, Stdio},
    };
    use uuid::Uuid;

    fn fixture_image() -> Arc<VfsFileSnapshot<u64>> {
        let contents = include_str!("../tests/fixtures/publication-repository-main.orna");
        Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::new(7)),
            Arc::<[u8]>::from(contents.as_bytes()),
        ))
    }

    fn test_git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .unwrap()
                .success(),
            "git {args:?} succeeds"
        );
    }

    fn test_git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(path)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn test_git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
        String::from_utf8(test_git_output(
            path,
            &["hash-object", "-w", "-t", kind, "--stdin"],
            Some(bytes),
        ))
        .unwrap()
        .trim()
        .to_owned()
    }

    fn test_git_tree(path: &Path, entries: &str) -> String {
        String::from_utf8(test_git_output(path, &["mktree"], Some(entries.as_bytes())))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn test_native_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
        let data_oid = test_git_object(path, "blob", data);
        let mut dependencies = dependencies.to_vec();
        dependencies.sort_by_key(|(oid, _)| *oid);
        let refs_oid = if dependencies.is_empty() {
            None
        } else {
            let refs = dependencies
                .iter()
                .map(|(oid, kind)| {
                    let mode = if *kind == "tree" { "040000" } else { "100644" };
                    format!("{mode} {kind} {oid}\t{oid}\n")
                })
                .collect::<String>();
            Some(test_git_tree(path, &refs))
        };
        let mut envelope = format!("100644 blob {data_oid}\tdata\n");
        if let Some(refs_oid) = refs_oid {
            envelope.push_str(&format!("040000 tree {refs_oid}\trefs\n"));
        }
        test_git_tree(path, &envelope)
    }

    fn append_fixture_cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
        let prefix = major << 5;
        match value {
            0..=23 => output.push(prefix | value as u8),
            24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
            _ => panic!("fixture CBOR value is unexpectedly large"),
        }
    }

    fn append_fixture_cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
        append_fixture_cbor_head(output, 2, bytes.len());
        output.extend_from_slice(bytes);
    }

    fn fixture_oid_bytes(oid: &str) -> Vec<u8> {
        oid.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn format3_blob_repository() -> (tempfile::TempDir, Repository, [u8; 16]) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        test_git(root, &["init", "--quiet"]);
        test_git(root, &["config", "user.email", "kieran@drewett.dev"]);
        test_git(root, &["config", "user.name", "kierandrewett"]);
        test_git(root, &["config", "commit.gpgsign", "false"]);

        let database_id = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
        let relation_id = [0x43; 16];
        let database_uuid = "00000000-0000-4000-8000-000000000001";
        let database =
            format!("{{\n    repository_format: 3,\n    database_id: \"{database_uuid}\",\n}}\n");
        let source = b"capture transaction test schema";
        let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
        let field = |id: [u8; 16], name: &str, ty: Raw, role: u8| {
            Raw::Array(vec![
                uuid(id),
                Raw::Text(name.to_owned()),
                ty,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        let schema_bytes = SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid(relation_id)),
            (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
            (
                Raw::Int(3.into()),
                Raw::Array(vec![
                    field(
                        [0x11; 16],
                        "id",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]),
                        0,
                    ),
                    field(
                        [0x22; 16],
                        "payload",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".into())]),
                        1,
                    ),
                ]),
            ),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .unwrap()
        .encode()
        .unwrap();
        std::fs::create_dir_all(root.join(".orna/store")).unwrap();
        std::fs::write(root.join(".orna/database.orna"), &database).unwrap();
        std::fs::write(root.join(".orna/store/data"), b"store marker").unwrap();
        std::fs::write(root.join("main.orna"), source).unwrap();
        test_git(root, &["add", "--all"]);
        test_git(root, &["commit", "--quiet", "-m", "format3 Blob fixture"]);

        // The chunk entry carries the chunk's SHA-256; the kind-6 node and the
        // rows domain carry the format-3 schema identity.
        let chunk_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
        let schema_digest: [u8; 32] = Sha256::new()
            .chain_update(b"orna.schema.v3\0")
            .chain_update(&schema_bytes)
            .finalize()
            .into();
        let chunk_oid = test_git_object(root, "blob", &schema_bytes);
        let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
        append_fixture_cbor_head(&mut byte_index, 0, schema_bytes.len());
        byte_index.extend_from_slice(&[0x81, 0x83]);
        append_fixture_cbor_head(&mut byte_index, 0, schema_bytes.len());
        append_fixture_cbor_bytes(&mut byte_index, &fixture_oid_bytes(&chunk_oid));
        append_fixture_cbor_bytes(&mut byte_index, &chunk_digest);
        let byte_index_oid = test_native_node(root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

        let mut schema = vec![0x85, 0x01, 0x06];
        append_fixture_cbor_head(&mut schema, 0, schema_bytes.len());
        append_fixture_cbor_bytes(&mut schema, &schema_digest);
        append_fixture_cbor_bytes(&mut schema, &fixture_oid_bytes(&byte_index_oid));
        let schema_oid = test_native_node(root, &schema, &[(byte_index_oid.as_str(), "tree")]);

        let mut row_domain = vec![0x83, 0x64];
        row_domain.extend_from_slice(b"rows");
        append_fixture_cbor_bytes(&mut row_domain, &relation_id);
        append_fixture_cbor_bytes(&mut row_domain, &schema_digest);
        let mut empty_rows = vec![0x84, 0x01, 0x01];
        empty_rows.extend_from_slice(&row_domain);
        empty_rows.push(0x80);
        let row_root_oid = test_native_node(root, &empty_rows, &[]);

        let mut relation_domain = vec![0x82, 0x69];
        relation_domain.extend_from_slice(b"relations");
        append_fixture_cbor_bytes(&mut relation_domain, &database_id);
        let mut relation_map = vec![0x84, 0x01, 0x01];
        relation_map.extend_from_slice(&relation_domain);
        relation_map.extend_from_slice(&[0x81, 0x82]);
        append_fixture_cbor_bytes(&mut relation_map, &relation_id);
        relation_map.push(0x84);
        append_fixture_cbor_bytes(&mut relation_map, &fixture_oid_bytes(&schema_oid));
        append_fixture_cbor_bytes(&mut relation_map, &fixture_oid_bytes(&row_root_oid));
        relation_map.extend_from_slice(&[0xf6, 0x00]);
        let relation_map_oid = test_native_node(
            root,
            &relation_map,
            &[
                (schema_oid.as_str(), "tree"),
                (row_root_oid.as_str(), "tree"),
            ],
        );

        let mut store_root = vec![0x83, 0x01, 0x00];
        append_fixture_cbor_bytes(&mut store_root, &fixture_oid_bytes(&relation_map_oid));
        let store_root_oid =
            test_native_node(root, &store_root, &[(relation_map_oid.as_str(), "tree")]);
        let database_oid = test_git_object(root, "blob", database.as_bytes());
        let source_oid = test_git_object(root, "blob", source);
        let orna_tree = test_git_tree(
            root,
            &format!(
                "100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_root_oid}\tstore\n"
            ),
        );
        let root_tree = test_git_tree(
            root,
            &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
        );
        let parent = String::from_utf8(test_git_output(root, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_owned();
        let commit = String::from_utf8(test_git_output(
            root,
            &[
                "commit-tree",
                &root_tree,
                "-p",
                &parent,
                "-m",
                "format3 Blob graph fixture",
            ],
            None,
        ))
        .unwrap()
        .trim()
        .to_owned();
        let head_ref = String::from_utf8(test_git_output(root, &["symbolic-ref", "HEAD"], None))
            .unwrap()
            .trim()
            .to_owned();
        test_git(root, &["update-ref", &head_ref, &commit]);
        let repository = Repository::discover(root).unwrap();
        (temp, repository, relation_id)
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

        let content = project_content_name("content", "audio/mpeg", None).unwrap();
        let cover = project_content_name("cover", "image/jpeg", None).unwrap();
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

    /// ORNA-VFS-012: suffix selection updates only compatible hints and never implies byte transcoding or row rekeying.
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
        // Octet-stream and unregistered essences admit any safe hint and store
        // `bin` as no hint (MIME-1 `unknown_policy` and the normative model's
        // `annotation()`), but never an unsafe spelling.
        assert_eq!(
            classify_content_suffix("application/x-unknown", Some("gz")),
            Ok(Some("gz".to_owned()))
        );
        assert_eq!(
            classify_content_suffix("application/x-unknown", Some("bin")),
            Ok(None)
        );
        assert_eq!(
            classify_content_suffix("application/octet-stream", Some("dat")),
            Ok(Some("dat".to_owned()))
        );
        for unsafe_suffix in ["", ".dat", "a..b", "a/b", "dat "] {
            assert_eq!(
                classify_content_suffix("application/octet-stream", Some(unsafe_suffix)),
                Err(VfsPathError::InvalidDocument),
                "{unsafe_suffix:?}"
            );
            assert!(!content_suffix_is_compatible(
                "application/x-unknown",
                unsafe_suffix
            ));
        }
        assert!(content_suffix_is_compatible("application/x-unknown", "xyz"));
        assert!(!content_suffix_is_compatible("image/jpeg", "png"));
        assert_eq!(
            project_content_name("content", "application/x-unknown", None),
            Ok("content.bin".to_owned())
        );
        assert_eq!(
            project_content_name("content", "application/x-unknown", Some("xyz")),
            Ok("content.xyz".to_owned())
        );
        // Projection reads the registry's selected suffix, so an alias spelling
        // projects as its canonical essence. A stored hint the registry refuses
        // is corrupt metadata: it is reported (EIO), never spelled into the host
        // name and never renamed to the octet-stream `bin` fallback (W08-09).
        assert_eq!(
            project_content_name("content", "Audio/X-WAV", None),
            Ok("content.wav".to_owned())
        );
        assert_eq!(
            project_content_name("content", "audio/mpeg", Some("../mp3")),
            Err(VfsPathError::CorruptAnnotation)
        );
        assert_eq!(VfsPathError::CorruptAnnotation.errno(), VFS_EIO);
        assert_eq!(VfsPathError::CorruptAnnotation.failure_code(), None);
        assert_eq!(
            VfsRowProjection::project(
                SnapshotPin::capture(Arc::new(7_u64)),
                &[VfsStoredField::content(
                    "content",
                    "audio/mpeg",
                    Some("../mp3".to_owned()),
                    4
                )],
            )
            .err(),
            Some(VfsPathError::CorruptAnnotation),
            "a row whose stored annotation is refused is not projected"
        );

        // The suffix rename never changes the stored content identity: the same
        // descriptor size projects under the selected hint, and the row's bytes
        // are untouched.
        let renamed = project_content_name("content", "audio/mpeg", Some("mp1")).unwrap();
        assert_eq!(renamed, "content.mp1");
        assert_eq!(
            project_content_name("content", "audio/mpeg", None),
            Ok("content.mp3".to_owned())
        );
    }

    #[test]
    fn vfs_table_row_replacement_preserves_admitted_table_identity() {
        let table = RuntimeTableIdentity::new("Song", orna_runtime::TableObjectId::new([0x71; 16]))
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
        let projected = project_content_name(&long_field, "audio/mpeg", None).unwrap();
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
    fn nested_directory_takes_escaped_field_name_and_shares_the_file_collision_check() {
        let row = VfsRowProjection::project_with_nested_directories(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::document("con", "{ x: 1 }")],
            &["con"],
        )
        .expect("nested directory projection");
        assert_eq!(row.directories().len(), 1);
        assert_eq!(row.directories()[0].field(), "con");
        assert_eq!(row.directories()[0].name(), "%63on");
        assert_eq!(row.directories()[0].name(), project_field_name("con"));

        // Two nested directories that name one host directory take the same
        // collision refusal two sibling files take (VFS-016).
        assert!(matches!(
            VfsRowProjection::project_with_nested_directories(
                SnapshotPin::capture(Arc::new(7_u64)),
                &[VfsStoredField::document("x", "{ y: 2 }")],
                &["x", "x"],
            ),
            Err(VfsPathError::NameCollision)
        ));
        // A directory name carries no dot, so it never meets a sibling file's
        // `.suffix` host name: a Text sibling `x` and a nested directory `y` coexist.
        let row = VfsRowProjection::project_with_nested_directories(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[
                VfsStoredField::text("x", 3, true),
                VfsStoredField::document("y", "{ z: 3 }"),
            ],
            &["y"],
        )
        .expect("text sibling and nested directory coexist");
        assert_eq!(row.directories()[0].name(), "y");
    }

    #[test]
    fn a_saved_row_reuses_the_captured_blob_descriptor_instead_of_re_deriving_it() {
        // A content sibling that a save admitted from the captured field keeps
        // the descriptor the store reported for the row version it came from:
        // the byte length, the raw digest, and the OGS-1 descriptor OID are the
        // captured ones, so a save that changes only the selected suffix (or
        // any other field) cannot mint a new descriptor or re-hash the payload
        // (VFS-008: an unchanged descriptor uses the captured field, not a new
        // path lookup into current state).
        let descriptor_oid =
            NativeObjectId::new("a".repeat(40)).expect("well-formed SHA-1 object id");
        let captured = CapturedContentDescriptor::new(4096, [0x5a; 32], descriptor_oid.clone());
        let row = VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::captured_content(
                "content",
                "audio/mpeg",
                Some("mp1".to_owned()),
                captured.clone(),
                true,
            )],
        )
        .expect("captured content projection");

        let VfsProjectedEntry::Content(file) = row
            .lookup("content.mp1")
            .expect("the compatible suffix names the content file")
        else {
            panic!("content lookup should return the projected file");
        };

        let projected = file
            .captured_descriptor()
            .expect("the captured descriptor reaches the projected file");
        assert_eq!(projected.length(), 4096);
        assert_eq!(projected.sha256(), &[0x5a; 32]);
        assert_eq!(projected.descriptor_oid(), &descriptor_oid);
        assert_eq!(projected, &captured);
        assert_eq!(file.descriptor_size(), captured.length());
        assert!(file.is_required());

        // A projection that was given no captured descriptor reports none, so a
        // file can never claim an identity the store did not hand it.
        let plain = VfsRowProjection::project(
            SnapshotPin::capture(Arc::new(7_u64)),
            &[VfsStoredField::content("content", "audio/mpeg", None, 4096)],
        )
        .expect("plain content projection");
        let VfsProjectedEntry::Content(file) = plain.lookup("content.mp3").expect("content file")
        else {
            panic!("content lookup should return the projected file");
        };
        assert!(file.captured_descriptor().is_none());

        // The descriptor is admitted only for the payload the store captured:
        // a save that keeps those exact bytes rebuilds the field from it, while
        // a save that replaced the payload reports no admission, so the new
        // bytes mint their own descriptor instead of inheriting one that no
        // longer names them (VFS-008, VFS-020).
        let VfsProjectedEntry::Content(content) = row
            .lookup("content.mp1")
            .expect("the compatible suffix names the content file")
        else {
            panic!("content lookup should return the projected file");
        };
        assert_eq!(content.admitted_capture(4096, &[0x5a; 32]), Some(&captured));
        assert!(content.admitted_capture(4096, &[0x5b; 32]).is_none());
        assert!(content.admitted_capture(4095, &[0x5a; 32]).is_none());
    }

    #[test]
    fn content_file_name_stays_within_the_component_bound_with_its_suffix() {
        // The projected content file is one host component, so VFS-016's bound
        // covers the whole `field.suffix` spelling. A field base that leaves no
        // room for its suffix takes the `~field-` digest alias instead of
        // overflowing the bound (VFS-016).
        let field = "f".repeat(200);
        let name = project_content_name(&field, "audio/mpeg", None).unwrap();
        assert!(name.len() <= VFS_MAX_COMPONENT_BYTES);
        assert!(name.starts_with("~field-"));
        assert!(name.ends_with(".mp3"));
        // The alias digests the stored name's canonical OVB-1 Str encoding (a
        // 200-byte Str is the CBOR text head `78 c8` then its bytes), so the
        // snapshot-bound long-name index re-derives and verifies the full name
        // rather than trusting the truncated spelling (VFS-1, VFS-016).
        let encoding = [&[0x78, 0xc8][..], field.as_bytes()].concat();
        let digest = Sha256::digest(&encoding)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(name, format!("~field-{digest}.mp3"));
        assert_eq!(field_name_digest(&field)[..], Sha256::digest(&encoding)[..]);

        // A base with room for the suffix still spells the escaped name
        // directly; only the over-bound spelling changes.
        let field = "f".repeat(196);
        let name = project_content_name(&field, "audio/mpeg", None).unwrap();
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
            target
                .open_read()
                .await
                .read_at(0, accepted_bytes.len())
                .unwrap(),
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
            target
                .open_read()
                .await
                .read_at(0, accepted_bytes.len())
                .unwrap(),
            accepted_bytes.as_bytes()
        );
        assert_eq!(
            draft
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes()
                .unwrap(),
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
            live.read_at(0, accepted_bytes.len()).unwrap(),
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
        let original = handle.read_at(0, handle.len() as usize).unwrap();

        let draft = EditDraft::open(image, 1 << 20, DraftStorage::with_default_quota());
        draft.write_at(0, &[0x58]).await.unwrap();

        assert_eq!(handle.read_at(0, original.len()).unwrap(), original);
        assert_eq!(*cursor.pin().snapshot(), 7);
        assert_ne!(
            draft.candidate_bytes().await.unwrap().as_ref(),
            original.as_slice()
        );
    }

    /// A 256 MiB sparse write allocates only the blocks it touches. This reads
    /// the spool file's own block count, not process RSS, so it cannot flake
    /// on other tests in the binary.
    #[tokio::test]
    async fn sparse_write_near_draft_bound_stays_sparse_on_disk() {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};
        const BOUND: usize = 256 << 20;
        let quota = Arc::new(DraftQuota::new(1 << 30));
        let storage = DraftStorage::new(&std::env::temp_dir(), quota);
        let draft = EditDraft::open(fixture_image(), BOUND, storage);
        draft.write_at(BOUND as u64 - 1, &[0x58]).await.unwrap();
        let state = draft.state.lock().await;
        let spool = state.candidate.as_ref().expect("spool");
        assert_eq!(spool.len, BOUND as u64);
        let allocated = spool.file.metadata().unwrap().blocks() * 512;
        assert!(
            allocated < 1 << 20,
            "a sparse 256 MiB candidate allocated {allocated} bytes on disk"
        );
        let mut last = [0u8; 1];
        spool
            .file
            .read_exact_at(&mut last, BOUND as u64 - 1)
            .unwrap();
        assert_eq!(last, [0x58]);
    }

    #[tokio::test]
    async fn aggregate_draft_quota_returns_edquot() {
        const LIMIT: usize = 1 << 20;
        let quota = Arc::new(DraftQuota::new(LIMIT as u64));
        let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
        let first = EditDraft::open(fixture_image(), LIMIT, storage.clone());
        let second = EditDraft::open(fixture_image(), LIMIT, storage);
        first.write_at(LIMIT as u64 - 1, &[0x58]).await.unwrap();
        assert_eq!(
            second.write_at(0, &[0x59]).await,
            Err(EditDraftError::QuotaExceeded)
        );
        first.truncate(0).await.unwrap();
        second.write_at(0, &[0x59]).await.unwrap();
        assert_eq!(second.candidate_bytes().await.unwrap()[0], 0x59);
    }

    /// Growth is what the quota refuses. The baseline is charged on the first
    /// write; a growth larger than the remaining room returns EDQUOT and
    /// changes nothing: no content, no revision, no charge.
    #[tokio::test]
    async fn growth_refused_by_quota_changes_nothing() {
        let baseline_len = fixture_image().bytes.len();
        let quota = Arc::new(DraftQuota::new(baseline_len + 16));
        let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
        let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
        draft.write_at(0, &[0x58]).await.unwrap();
        assert_eq!(quota.used.load(Ordering::Acquire), baseline_len);

        let revision = draft.state.lock().await.revision;
        let before = draft.candidate_bytes().await.unwrap();
        assert_eq!(
            draft.write_at(baseline_len, &[0x59; 32]).await,
            Err(EditDraftError::QuotaExceeded)
        );
        assert_eq!(draft.state.lock().await.revision, revision);
        assert_eq!(draft.candidate_bytes().await.unwrap(), before);
        assert_eq!(quota.used.load(Ordering::Acquire), baseline_len);

        // The room that remains is usable: a growth that fits is accepted.
        draft.write_at(baseline_len, &[0x59; 16]).await.unwrap();
        assert_eq!(quota.used.load(Ordering::Acquire), baseline_len + 16);
    }

    /// A refused candidate is charged once, by the live spool that already holds
    /// it. Its retained copy adds no charge, so the disk record of the same
    /// candidate (its header plus its bytes, one reservation) still fits beside
    /// the spool at a quota of exactly `2 * candidate + header`.
    #[tokio::test]
    async fn refused_candidate_is_charged_once_and_leaves_room_for_its_disk_record() {
        const HEADER: u64 = 64;
        let len = fixture_image().bytes.len().max(32 << 10);
        let quota = Arc::new(DraftQuota::new(2 * len + HEADER));
        let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
        let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
        draft.write_at(0, &[0x58; 32 << 10]).await.unwrap();
        assert_eq!(draft.candidate_len().await, len);
        assert_eq!(quota.used.load(Ordering::Acquire), len);

        let mut state = draft.state.lock().await;
        let revision = state.revision;
        state
            .retain_rejection(revision, stale_baseline_diagnostic())
            .unwrap();
        drop(state);
        assert_eq!(quota.used.load(Ordering::Acquire), len);
        let disk = QuotaCharge::reserve(&quota, HEADER + len);
        assert!(
            disk.is_ok(),
            "the disk record of the refused candidate must fit"
        );
    }

    /// Several drafts edited at once stay within the 1 GiB default limit when
    /// each grows to a 64 MiB compact file: 8 * 64 MiB = 512 MiB.
    #[tokio::test]
    async fn several_drafts_edit_within_the_default_limit() {
        const BOUND: usize = 64 << 20;
        let quota = Arc::new(DraftQuota::new(1 << 30));
        let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
        let mut drafts = Vec::new();
        for index in 0..8u8 {
            let draft = EditDraft::open(fixture_image(), BOUND, storage.clone());
            draft
                .write_at(BOUND as u64 - 1, &[index + 1])
                .await
                .unwrap();
            drafts.push(draft);
        }
        assert_eq!(quota.used.load(Ordering::Acquire), 8 * BOUND as u64);
        drop(drafts);
        assert_eq!(quota.used.load(Ordering::Acquire), 0);
    }

    /// A retained rejected draft is quota accounted (profiles/vfs-1.md): its
    /// copy aliases the live spool, so the spool's single charge is carried
    /// over while held and released when the draft drops.
    #[tokio::test]
    async fn retained_rejected_draft_is_charged_and_released() {
        let quota = Arc::new(DraftQuota::new(1 << 20));
        let storage = DraftStorage::new(&std::env::temp_dir(), Arc::clone(&quota));
        let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
        draft.write_at(0, &[0x58]).await.unwrap();
        let revision = draft.state.lock().await.revision;
        let used_before = quota.used.load(Ordering::Acquire);
        draft
            .state
            .lock()
            .await
            .retain_rejection(revision, rejected())
            .unwrap();
        assert_eq!(quota.used.load(Ordering::Acquire), used_before);
        drop(draft);
        assert_eq!(quota.used.load(Ordering::Acquire), 0);
    }

    /// An over-release cannot wrap the aggregate budget: the charge floors at
    /// zero, so the budget still refuses bytes beyond its limit.
    #[test]
    fn draft_quota_release_saturates_at_zero() {
        let quota = DraftQuota::new(100);
        quota.reserve(40).unwrap();
        quota.release(60);
        assert_eq!(quota.used.load(Ordering::Acquire), 0);
        quota.release(1);
        assert_eq!(quota.used.load(Ordering::Acquire), 0);
        assert!(quota.reserve(101).is_err());
        quota.reserve(100).unwrap();
        assert!(quota.reserve(1).is_err());
    }

    #[tokio::test]
    async fn spool_creation_fails_closed_when_the_spool_directory_is_missing() {
        let directory =
            std::env::temp_dir().join(format!("orna-missing-spool-{}", Uuid::new_v4().simple()));
        let storage = DraftStorage::new(&directory, Arc::new(DraftQuota::new(1 << 20)));
        let draft = EditDraft::open(fixture_image(), 1 << 20, storage);
        assert_eq!(
            draft.write_at(0, &[0x58]).await,
            Err(EditDraftError::BackingIo)
        );
    }

    #[test]
    fn runtime_root_draft_spool_directory_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let root =
            std::env::temp_dir().join(format!("orna-spool-root-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let _cache = VfsRepositoryCache::in_runtime_root(&root).unwrap();
        let mode = fs::metadata(root.join(DRAFT_SPOOL_DIRECTORY))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn stat_reports_accepted_size_pin_and_generation_and_hides_unlinked() {
        let image = fixture_image();
        let expected_len = image.len();
        let managed = ManagedFile {
            state: Mutex::new(ManagedFileState::Live { image }),
            operation: Mutex::new(()),
            cache_epoch: SharedCacheEpoch::default(),
            draft_storage: DraftStorage::with_default_quota(),
        };

        let stat = managed.stat().await.unwrap();
        assert_eq!(stat.size(), expected_len);
        assert_eq!(*stat.pin().snapshot(), 7);
        assert_eq!(stat.generation(), 0);

        managed.state.lock().await.mark_unlinked();
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

    /// ORNA-VFS-008 and ORNA-VFS-009: rejected draft sequences are serialized, retained with their diagnostic, and repeated sync does not replay activation.
    #[tokio::test]
    async fn rejected_draft_is_retained_and_repeated_fsync_is_idempotent() {
        let draft = EditDraft::open(fixture_image(), 1 << 20, DraftStorage::with_default_quota());
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
        assert_ne!(
            retained.replacement_bytes().unwrap(),
            &fixture_image().bytes.to_arc().unwrap()[..]
        );

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

    /// ORNA-VFS-007 and ORNA-VFS-008: accepted fsync publishes before release, preserves old open handles, and repeat sync does not accept the same sequence twice.
    #[tokio::test]
    async fn accepted_fsync_advances_only_future_opens_and_release_never_commits() {
        let image = fixture_image();
        let old_handle = SnapshotReadHandle::open(Arc::clone(&image));
        let draft = EditDraft::open(image, 1 << 20, DraftStorage::with_default_quota());
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
        assert_ne!(
            old_handle.read_at(0, 1).unwrap(),
            new_handle.read_at(0, 1).unwrap()
        );
        let repeated_calls = AtomicUsize::new(0);
        let repeated = draft
            .fsync_with(|_| {
                repeated_calls.fetch_add(1, Ordering::SeqCst);
                async {
                    Ok::<_, ()>(ActivationDecision::Accepted(SnapshotPin::capture(
                        Arc::new(9),
                    )))
                }
            })
            .await
            .unwrap();
        let FsyncOutcome::Accepted(repeated_handle) = repeated else {
            panic!("repeated sync must return its accepted outcome");
        };
        assert_eq!(repeated_calls.load(Ordering::SeqCst), 0);
        assert_eq!(*repeated_handle.pin().snapshot(), 8);
        assert_eq!(repeated_handle.read_at(0, 1).unwrap(), [0x58]);

        let calls = AtomicUsize::new(0);
        let draft = EditDraft::open(fixture_image(), 1 << 20, DraftStorage::with_default_quota());
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
        assert_eq!(backup.candidate_bytes().await.unwrap().as_ref(), row_text);
        let backup_syncs = Arc::new(AtomicUsize::new(0));
        let backup_syncs_by_route = Arc::clone(&backup_syncs);
        let backup_revision = backup
            .fsync_scratch_with(move |revision, bytes| {
                backup_syncs_by_route.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(revision, 0);
                    assert_eq!(bytes.read_range(0, row_text.len()).unwrap(), row_text);
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
                    assert_eq!(bytes.read_range(0, row_text.len()).unwrap(), row_text);
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
            target.open_read().await.read_at(0, row_text.len()).unwrap(),
            row_text
        );
        let retained = save.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes().unwrap(), invalid);
        assert_eq!(retained.diagnostic(), rejected());
        assert_ne!(retained.id(), [0u8; 16]);

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
        // Repeated syncs of one unchanged revision are one rejection, so the
        // retained draft keeps the identity it was first given (ORNA-VFS-009).
        assert_eq!(
            save.retained_invalid_draft().await.unwrap().id(),
            retained.id()
        );

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
            target.open_read().await.read_at(0, row_text.len()).unwrap(),
            row_text
        );
        let retained = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes().unwrap(), invalid);
        assert_eq!(retained.diagnostic(), rejected());
        assert_eq!(repository.generation().await, 0);

        // A stale candidate is a rejected candidate: its bytes and diagnostic
        // stay readable through the same path as a validation rejection, and
        // the destination keeps the accepted row (ORNA-VFS-009).
        let stale = target.open_draft(1 << 20).await;
        stale.truncate(0).await.unwrap();
        stale.write_at(0, row_text).await.unwrap();
        let row_store_for_stale = Arc::clone(&row_store);
        let seeded = target
            .commit_draft_with(&stale, move |candidate| {
                Arc::clone(&row_store_for_stale).activate(candidate, 21)
            })
            .await
            .unwrap();
        assert!(matches!(seeded, TemporaryRenameOutcome::Applied { .. }));
        assert!(stale.retained_invalid_draft().await.is_none());

        // `draft` still holds the rejected candidate and its recorded baseline,
        // so its next commit is stale before any activation is consulted.
        let before_stale_commits = row_store.transaction_count();
        let row_store_for_stale_commit = Arc::clone(&row_store);
        let stale_outcome = target
            .commit_draft_with(&draft, move |candidate| {
                Arc::clone(&row_store_for_stale_commit).activate(candidate, 22)
            })
            .await
            .unwrap();
        let TemporaryRenameOutcome::Stale { diagnostic } = stale_outcome else {
            panic!("a draft over a replaced image must be stale");
        };
        let stale_diagnostic = stale_baseline_diagnostic();
        assert_eq!(diagnostic, stale_diagnostic);
        assert_eq!(row_store.transaction_count(), before_stale_commits);
        assert_eq!(row_store.accepted(), row_text);
        assert_eq!(
            target.open_read().await.read_at(0, row_text.len()).unwrap(),
            row_text
        );
        let parked = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(parked.replacement_bytes().unwrap(), invalid);
        assert_eq!(parked.diagnostic(), stale_diagnostic);
        assert_ne!(parked.id(), [0u8; 16]);
        // A stale rejection of the same unchanged revision keeps the identity
        // the draft already had; only its diagnostic is superseded.
        assert_eq!(parked.id(), retained.id());

        // A changed candidate is a new revision, so its rejection is a new
        // retained draft with its own identity (ORNA-VFS-009).
        draft.write_at(0, b"x").await.unwrap();
        let row_store_for_stale_again = Arc::clone(&row_store);
        let stale_again = target
            .commit_draft_with(&draft, move |candidate| {
                Arc::clone(&row_store_for_stale_again).activate(candidate, 23)
            })
            .await
            .unwrap();
        assert!(matches!(stale_again, TemporaryRenameOutcome::Stale { .. }));
        let reparked = draft.retained_invalid_draft().await.unwrap();
        assert_eq!(reparked.revision(), parked.revision() + 1);
        assert_ne!(reparked.id(), parked.id());
        assert_eq!(row_store.transaction_count(), before_stale_commits);
        assert_eq!(row_store.accepted(), row_text);
    }

    #[tokio::test]
    async fn temp_fsync_is_scratch_only_and_rename_preserves_rejected_and_open_state() {
        let repository = VfsRepositoryCache::new();
        let target = repository.managed_file(fixture_image()).await.unwrap();
        let old_handle = target.open_read().await;
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(0, &[0x58]).await.unwrap();

        let scratch_syncs = AtomicUsize::new(0);
        let sequence = scratch
            .fsync_scratch_with(|revision, bytes| {
                scratch_syncs.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(revision, 2);
                    assert_eq!(bytes.read_range(0, 1).unwrap(), &[0x58]);
                    Ok::<_, ()>(())
                }
            })
            .await
            .unwrap();
        assert_eq!(sequence, 2);
        assert_eq!(scratch_syncs.load(Ordering::SeqCst), 1);
        assert_eq!(target.cache_generation().await, 0);
        assert_eq!(
            target
                .open_read()
                .await
                .read_at(0, old_bytes.len())
                .unwrap(),
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
        assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
        let retained = scratch.retained_invalid_draft().await.unwrap();
        assert_eq!(retained.replacement_bytes().unwrap(), &[0x58]);
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
        assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
        assert_eq!(*target.open_read().await.pin().snapshot(), 8);
        assert_eq!(handle.read_at(0, 2).unwrap(), [0x58, 0x59]);
        assert_eq!(
            scratch
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes()
                .unwrap(),
            &[0x58]
        );
    }

    /// ORNA-VFS-019: accepted API replacement invalidates shared directory, attribute, and sibling-handle projections without retargeting open handles.
    #[tokio::test]
    async fn accepted_rename_invalidates_shared_directory_attribute_and_handle_projections() {
        let repository = VfsRepositoryCache::new();
        let raw_target_image = fixture_image();
        let stale_snapshot = Arc::clone(&raw_target_image.pin.snapshot);
        let target = repository
            .managed_file(Arc::clone(&raw_target_image))
            .await
            .unwrap();
        let sibling_image = Arc::new(VfsFileSnapshot::from_candidate(
            SnapshotPin::capture(Arc::clone(&stale_snapshot)),
            raw_target_image.bytes.duplicate().unwrap(),
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
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
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
        assert_eq!(old_handle.read_at(0, old_bytes.len()).unwrap(), old_bytes);
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
        let rewrapped_stale_image = Arc::new(VfsFileSnapshot::from_candidate(
            SnapshotPin::capture(Arc::clone(&stale_snapshot)),
            raw_target_image.bytes.duplicate().unwrap(),
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
                .replacement_bytes()
                .unwrap(),
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
            target.open_read().await.read_at(0, baseline.len()).unwrap(),
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
            next.baseline()
                .await
                .read_at(0, accepted_bytes.len())
                .unwrap(),
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
        assert_eq!(target.open_read().await.read_at(0, 1).unwrap(), [0x5A]);
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
            read_handle.read_at(0, read_bytes.len()).unwrap(),
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
                .replacement_bytes()
                .unwrap(),
            draft_bytes.as_bytes()
        );
        // The REPL change is still what the row holds, and the read handle
        // still reads the version it captured.
        assert_eq!(
            target
                .open_read()
                .await
                .read_at(0, repl_bytes.len())
                .unwrap(),
            repl_bytes.as_bytes()
        );
        assert_eq!(
            read_handle.read_at(0, read_bytes.len()).unwrap(),
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
            target
                .open_read()
                .await
                .read_at(0, draft_bytes.len())
                .unwrap(),
            draft_bytes.as_bytes()
        );
    }

    struct DurableRenameValidator {
        table: String,
        object_id: TableObjectId,
        accepted: Vec<u8>,
        expected_content_transfer: Option<ProtectedContentTransferEvidence>,
        schema: TableSchemaIdentity,
    }

    impl TableActivationCandidateValidator for DurableRenameValidator {
        fn tables(&self) -> &[String] {
            std::slice::from_ref(&self.table)
        }

        fn table_object_ids(&self) -> BTreeMap<String, TableObjectId> {
            BTreeMap::from([(self.table.clone(), self.object_id)])
        }

        fn schema_identity(&mut self) -> Option<TableSchemaIdentity> {
            Some(self.schema.clone())
        }

        fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic> {
            let accepted = rows
                .get(&self.table)
                .and_then(|rows| rows.first())
                .map(|(_, value)| value.as_slice());
            if accepted == Some(self.accepted.as_slice()) {
                Ok(())
            } else {
                Err(rejected())
            }
        }

        fn validate_content_pins(
            &mut self,
            _: &RuntimeTableRows,
            transfers: &[ProtectedContentTransfer],
        ) -> bool {
            match &self.expected_content_transfer {
                Some(expected) => {
                    transfers.len() == 1
                        && ProtectedContentTransferEvidence::from_transfer(&transfers[0])
                            == *expected
                }
                None => transfers.is_empty(),
            }
        }
    }

    /// ORNA-VFS-021: durable temporary-save admission commits a replayable acknowledgement and rejects stale row baselines.
    #[tokio::test]
    async fn durable_temporary_rename_commits_and_refuses_changed_row_baseline() {
        let temp = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(temp.path())
                .status()
                .unwrap()
                .success()
        );
        let repo = Repository::discover(temp.path()).unwrap();
        let schema = TableSchemaIdentity::new([7; 32]);
        let state = RuntimeState::open(
            &repo,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            [3; 32],
        )
        .await
        .unwrap();
        let identity = RuntimeTableIdentity::new("books", TableObjectId::new([11; 16])).unwrap();
        let key = vec![1];
        let target = VfsRepositoryCache::new()
            .managed_file(Arc::new(VfsFileSnapshot::new(
                SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(b"original".as_slice()),
            )))
            .await
            .unwrap();
        let accepted_for_admit =
            include_bytes!("../tests/fixtures/vfs-row-music-london.orna").to_vec();
        let stale_fixture =
            include_bytes!("../tests/fixtures/publication-repository-main.orna").to_vec();
        let schema_for_admit = schema.clone();
        let accepted_baseline = accepted_for_admit.clone();
        let stale_for_validation = stale_fixture.clone();
        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(0, &accepted_for_admit).await.unwrap();
        let save = VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
            .with_expected_baseline_row(None);
        let identity_for_admit = identity.clone();
        let accepted_for_validation = accepted_for_admit.clone();
        let admitted_from_spool = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let admitted_from_spool_output = Arc::clone(&admitted_from_spool);
        let applied = target
            .commit_durable_temporary_rename(
                &scratch,
                &state,
                save,
                move |coordinates, bytes| async move {
                    admitted_from_spool_output.store(
                        matches!(bytes.source, CandidateSource::Spool(_)),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    let replacement = VfsTableRowReplacement::new(
                        [7; 16],
                        identity_for_admit.clone(),
                        coordinates.key().to_vec(),
                        Some(bytes.to_arc().unwrap().to_vec()),
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: identity_for_admit.table().to_owned(),
                            object_id: identity_for_admit.object_id(),
                            accepted: accepted_for_validation.clone(),
                            expected_content_transfer: None,
                            schema: schema_for_admit.clone(),
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            applied,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert!(
            admitted_from_spool.load(std::sync::atomic::Ordering::SeqCst),
            "the admission callback must receive the spool-backed candidate, not a RAM copy"
        );
        assert_eq!(state.pending().await.unwrap().len(), 1);
        let replay = target
            .commit_durable_temporary_rename(
                &scratch,
                &state,
                VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                    .with_expected_baseline_row(None),
                |_, _| async { panic!("an accepted temp rename must replay its outcome") },
                &orna_runtime::NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            replay,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(state.pending().await.unwrap().len(), 1);

        // A different writer may commit byte-identical row content. That is
        // not evidence this temp save was already accepted: its captured
        // baseline is still stale, and the scratch remains retryable.
        let identical_scratch = target.begin_temporary_replacement(1 << 20).await;
        identical_scratch
            .write_at(0, &accepted_for_admit)
            .await
            .unwrap();
        let identical_identity = identity.clone();
        let identical_bytes = accepted_for_admit.clone();
        let schema_for_identical = schema.clone();
        let identical = target
            .commit_durable_temporary_rename(
                &identical_scratch,
                &state,
                VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                    .with_expected_baseline_row(None),
                move |coordinates, bytes| async move {
                    let replacement = VfsTableRowReplacement::new(
                        [12; 16],
                        identical_identity.clone(),
                        coordinates.key().to_vec(),
                        Some(bytes.to_arc().unwrap().to_vec()),
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: identical_identity.table().to_owned(),
                            object_id: identical_identity.object_id(),
                            accepted: identical_bytes,
                            expected_content_transfer: None,
                            schema: schema_for_identical,
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await;
        let Err(VfsSaveError::Stale { diagnostic, .. }) = identical else {
            panic!("byte-identical content cannot acknowledge a stale temp save");
        };
        assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
        assert_eq!(
            identical_scratch.candidate_bytes().await.unwrap().as_ref(),
            accepted_for_admit
        );
        assert_eq!(state.pending().await.unwrap().len(), 1);

        let stale_scratch = target.begin_temporary_replacement(1 << 20).await;
        stale_scratch.write_at(0, &stale_fixture).await.unwrap();
        let stale_identity = identity.clone();
        let schema_for_stale = schema.clone();
        let stale = target
            .commit_durable_temporary_rename(
                &stale_scratch,
                &state,
                VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                    .with_expected_baseline_row(Some(b"not-current".to_vec())),
                move |coordinates, bytes| async move {
                    let replacement = VfsTableRowReplacement::new(
                        [8; 16],
                        stale_identity.clone(),
                        coordinates.key().to_vec(),
                        Some(bytes.to_arc().unwrap().to_vec()),
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: stale_identity.table().to_owned(),
                            object_id: stale_identity.object_id(),
                            accepted: stale_for_validation,
                            expected_content_transfer: None,
                            schema: schema_for_stale.clone(),
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await;
        let Err(VfsSaveError::Stale { diagnostic, .. }) = stale else {
            panic!("a temp rename over a changed durable row must be stale");
        };
        assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
        assert_eq!(
            stale_scratch.candidate_bytes().await.unwrap().as_ref(),
            stale_fixture
        );
        assert_eq!(state.pending().await.unwrap().len(), 1);
        let retry_identity = identity.clone();
        let retry_fixture = stale_fixture.clone();
        let schema_for_retry = schema.clone();
        let retried = target
            .commit_durable_temporary_rename(
                &stale_scratch,
                &state,
                VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone())
                    .with_expected_baseline_row(Some(accepted_baseline.clone())),
                move |coordinates, bytes| async move {
                    let replacement = VfsTableRowReplacement::new(
                        [9; 16],
                        retry_identity.clone(),
                        coordinates.key().to_vec(),
                        Some(bytes.to_arc().unwrap().to_vec()),
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: retry_identity.table().to_owned(),
                            object_id: retry_identity.object_id(),
                            accepted: retry_fixture,
                            expected_content_transfer: None,
                            schema: schema_for_retry.clone(),
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            retried,
            TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(state.pending().await.unwrap().len(), 2);

        let mut failed_lookup = VfsSaveAdmission::<u64>::begin(
            &state,
            VfsDurableSave::new([4; 16], schema.clone(), identity.clone(), key.clone()),
            &orna_runtime::NoFault,
        )
        .await
        .unwrap();
        failed_lookup.snapshot.clear_table_rows_for_test();
        assert_eq!(
            failed_lookup.admitted_row(),
            Err(RuntimeQueryError::TableNotAdmitted),
            "an unverifiable admitted row must remain a lookup error, not an absent row"
        );
        drop(failed_lookup);

        drop(state);
        let reopened = RuntimeState::open(
            &repo,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            [3; 32],
        )
        .await
        .unwrap();
        let snapshot = reopened
            .begin_admitted_table_activation(std::slice::from_ref(&identity))
            .await
            .unwrap();
        assert_eq!(
            snapshot.query_exact(identity.table(), &key).unwrap(),
            Some(stale_fixture.as_slice())
        );
    }

    fn source_blob_mib_row(encoded_blob: &[u8]) -> Vec<u8> {
        // Preserve the source row's Blob plus Float<MiB> tuple as one value.
        let mut row = vec![0x84, 0x64, b'n', b'a', b'm', b'e'];
        row.extend_from_slice(encoded_blob);
        row.extend_from_slice(&[0x64, b's', b'i', b'z', b'e', 0xf9, 0x3e, 0x00]);
        row
    }

    #[tokio::test]
    async fn durable_blob_temp_rename_cas_commits_canonical_media_and_pin() {
        const PIXEL: &[u8] = include_bytes!("../tests/fixtures/media/pixel.png");
        let (_temp, repo, relation_id) = format3_blob_repository();
        let capture = repo.capture_capability(relation_id).unwrap();
        let runtime_identity = RuntimeIdentity {
            database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
            repository_id: [0x61; 16],
        };
        let state = RuntimeState::open(&repo, runtime_identity, [0x62; 32])
            .await
            .unwrap();
        let cache = Arc::new(VfsRepositoryCache::new());
        let retention = Arc::new(());
        let retained = Arc::downgrade(&retention);
        let target = cache
            .managed_file(Arc::new(VfsFileSnapshot::new(
                SnapshotPin::capture_with_retention(Arc::new(0_u64), retention),
                Arc::<[u8]>::from(PIXEL),
            )))
            .await
            .unwrap();
        let old_handle = target.open_read().await;
        let old_bytes = old_handle.read_at(0, old_handle.len() as usize).unwrap();
        let scratch = target.begin_temporary_replacement(1 << 20).await;
        scratch.write_at(8, &[0xa5, 0x5a]).await.unwrap();
        let mut sparse_hole = vec![0; 10];
        sparse_hole[8..10].copy_from_slice(&[0xa5, 0x5a]);
        assert_eq!(
            scratch.candidate_bytes().await.unwrap().as_ref(),
            sparse_hole.as_slice()
        );
        scratch.truncate(16).await.unwrap();
        sparse_hole.resize(16, 0);
        assert_eq!(
            scratch.candidate_bytes().await.unwrap().as_ref(),
            sparse_hole.as_slice()
        );
        scratch.truncate(5).await.unwrap();
        assert_eq!(scratch.candidate_bytes().await.unwrap().as_ref(), &[0; 5]);
        scratch.truncate(0).await.unwrap();
        let mut replacement_bytes = PIXEL.to_vec();
        replacement_bytes.resize(PIXEL.len() + 4, 0);
        scratch.write_at(0, PIXEL).await.unwrap();
        scratch
            .truncate(replacement_bytes.len() as u64)
            .await
            .unwrap();
        assert_eq!(
            scratch.candidate_bytes().await.unwrap().as_ref(),
            replacement_bytes.as_slice()
        );
        let scratch_for_persist = &scratch;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            scratch.fsync_scratch_with(|_, bytes| {
                let expected_bytes = replacement_bytes.as_slice();
                async move {
                    assert_eq!(
                        bytes.read_range(0, expected_bytes.len()).unwrap(),
                        expected_bytes
                    );
                    assert_eq!(
                        scratch_for_persist
                            .candidate_bytes()
                            .await
                            .unwrap()
                            .as_ref(),
                        expected_bytes
                    );
                    Ok::<_, ()>(())
                }
            }),
        )
        .await
        .expect("scratch persistence must release its state mutex")
        .unwrap();
        let owner_id = [0x63; 16];
        let table = RuntimeTableIdentity::new("media", TableObjectId::new(relation_id)).unwrap();
        let table_for_checks = table.clone();
        let key = b"image".to_vec();
        let schema = TableSchemaIdentity::new([7; 32]);
        let pins_before = test_git_output(
            repo.worktree(),
            &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
            None,
        );
        assert_eq!(
            target
                .open_read()
                .await
                .read_at(0, old_bytes.len())
                .unwrap(),
            old_bytes,
            "scratch flush leaves new opens on the accepted image"
        );
        let initial_rows = state
            .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
            .await
            .unwrap();
        assert_eq!(
            initial_rows
                .query_exact(table_for_checks.table(), &key)
                .unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());

        let mut wrong_bytes = replacement_bytes.clone();
        wrong_bytes[0] ^= 1;
        let mismatch = cache
            .commit_durable_blob_rename(
                &target,
                &scratch,
                &state,
                VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
                    .with_expected_baseline_row(None),
                &capture,
                ContentIdentity::new(
                    replacement_bytes.len() as u64,
                    Sha256::digest(&wrong_bytes).into(),
                )
                .unwrap(),
                "image/png",
                Some("png"),
                |_, _, _| async { panic!("identity mismatch must stop before row admission") },
                &orna_runtime::NoFault,
            )
            .await;
        assert!(matches!(
            mismatch,
            Err(VfsSaveError::BlobCapture(
                VfsBlobReplacementError::IdentityMismatch { .. }
            ))
        ));
        assert_eq!(
            scratch.candidate_bytes().await.unwrap().as_ref(),
            replacement_bytes.as_slice()
        );
        let rejected_rows = state
            .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
            .await
            .unwrap();
        assert_eq!(
            rejected_rows
                .query_exact(table_for_checks.table(), &key)
                .unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            test_git_output(
                repo.worktree(),
                &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
                None,
            ),
            pins_before,
            "identity rejection removes its provisional graph pin"
        );
        // A changed candidate sequence makes corrected bytes retryable after
        // the identity rejection was recorded for the previous sequence.
        scratch.write_at(0, &wrong_bytes).await.unwrap();
        scratch.write_at(0, &replacement_bytes).await.unwrap();
        let replacement_for_flush = replacement_bytes.clone();
        scratch
            .fsync_scratch_with(move |_, bytes| async move {
                assert_eq!(
                    bytes.read_range(0, replacement_for_flush.len()).unwrap(),
                    replacement_for_flush.as_slice()
                );
                Ok::<_, ()>(())
            })
            .await
            .unwrap();

        let expected_identity = ContentIdentity::new(
            replacement_bytes.len() as u64,
            Sha256::digest(&replacement_bytes).into(),
        )
        .unwrap();
        let stale_table = table.clone();
        let stale_table_name = stale_table.table().to_owned();
        let stale_object_id = stale_table.object_id();
        let stale_schema = schema.clone();
        let stale = cache
            .commit_durable_blob_rename(
                &target,
                &scratch,
                &state,
                VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
                    .with_expected_baseline_row(Some(b"stale baseline".to_vec())),
                &capture,
                expected_identity,
                "image/png",
                None,
                move |coordinates, _, encoded_blob| async move {
                    let expected_content_transfer = ProtectedContentTransferEvidence::from_transfer(
                        &encoded_blob.transfer_record(),
                    );
                    let row_value = source_blob_mib_row(encoded_blob.encoded_value());
                    let pin = encoded_blob.into_pin();
                    let mutation = TableMutation::new(
                        [0x64; 16],
                        stale_table.table(),
                        coordinates.key().to_vec(),
                        Some(row_value.clone()),
                    )
                    .map_err(TableActivationError::Runtime)?
                    .with_table_object_id(stale_table.object_id())
                    .with_protected_content_pin(&pin)
                    .map_err(TableActivationError::Runtime)?;
                    let replacement = VfsTableRowReplacement::from_admitted_mutation(
                        stale_table,
                        mutation,
                        vec![pin],
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: stale_table_name,
                            object_id: stale_object_id,
                            accepted: row_value,
                            expected_content_transfer: Some(expected_content_transfer),
                            schema: stale_schema,
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await;
        let diagnostic = match stale {
            Err(VfsSaveError::Stale { diagnostic, .. }) => diagnostic,
            Err(error) => panic!("stale Blob temp rename returned another error: {error:?}"),
            Ok(_) => panic!("stale Blob temp rename must retain its retryable candidate"),
        };
        assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
        let stale_rows = state
            .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
            .await
            .unwrap();
        assert_eq!(
            stale_rows
                .query_exact(table_for_checks.table(), &key)
                .unwrap(),
            None
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            test_git_output(
                repo.worktree(),
                &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
                None,
            ),
            pins_before,
            "stale rejection removes its provisional graph pin"
        );

        let cache_for_admit = Arc::clone(&cache);
        let encoded_row = Arc::new(TestMutex::new(None));
        let encoded_row_for_admit = Arc::clone(&encoded_row);
        let save_table = table.clone();
        let save_table_name = save_table.table().to_owned();
        let save_object_id = save_table.object_id();
        let save_schema = schema.clone();
        let target_for_admit = Arc::clone(&target);
        let scratch_for_admit = &scratch;
        let state_for_admit = &state;
        let nested_save = VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone());
        let nested_document_save =
            VfsDurableSave::new(owner_id, schema.clone(), table.clone(), key.clone())
                .with_expected_baseline_row(None);
        let save_key = key.clone();
        let replacement_for_admit = replacement_bytes.clone();
        let saved = cache
            .commit_durable_blob_rename(
                &target,
                &scratch,
                &state,
                VfsDurableSave::new(owner_id, schema, table, key.clone())
                    .with_expected_baseline_row(None),
                &capture,
                expected_identity,
                "image/png",
                None,
                move |coordinates, _, encoded_blob| async move {
                    assert_eq!(cache_for_admit.generation().await, 0);
                    assert_eq!(
                        target_for_admit
                            .open_read()
                            .await
                            .read_at(0, PIXEL.len())
                            .unwrap(),
                        PIXEL
                    );
                    assert_eq!(
                        scratch_for_admit.candidate_bytes().await.unwrap().as_ref(),
                        replacement_for_admit.as_slice()
                    );
                    assert_eq!(
                        state_for_admit
                            .committed_table_row("media", &save_key)
                            .await
                            .unwrap(),
                        None
                    );
                    assert!(matches!(
                        VfsSaveAdmission::<u64>::begin(
                            state_for_admit,
                            nested_save,
                            &orna_runtime::NoFault,
                        )
                        .await,
                        Err(TableActivationError::Runtime(RuntimeError::AdminBusy))
                    ));
                    let nested_draft = target_for_admit.open_draft(1 << 20).await;
                    let nested_result = target_for_admit
                        .commit_durable_document_save(
                            &nested_draft,
                            state_for_admit,
                            nested_document_save,
                            |_, _| async {
                                Err::<VfsSaveCandidate, TableActivationError>(
                                    TableActivationError::Runtime(RuntimeError::AdminBusy),
                                )
                            },
                            &orna_runtime::NoFault,
                        )
                        .await;
                    assert!(matches!(
                        nested_result,
                        Err(VfsSaveError::Activation(TableActivationError::Runtime(
                            RuntimeError::AdminBusy
                        )))
                    ));
                    let expected_content_transfer = ProtectedContentTransferEvidence::from_transfer(
                        &encoded_blob.transfer_record(),
                    );
                    let row_value = source_blob_mib_row(encoded_blob.encoded_value());
                    *encoded_row_for_admit.lock().unwrap() = Some(row_value.clone());
                    let pin = encoded_blob.into_pin();
                    let mutation = TableMutation::new(
                        [0x65; 16],
                        save_table.table(),
                        coordinates.key().to_vec(),
                        Some(row_value.clone()),
                    )
                    .map_err(TableActivationError::Runtime)?
                    .with_table_object_id(save_table.object_id())
                    .with_protected_content_pin(&pin)
                    .map_err(TableActivationError::Runtime)?;
                    let replacement = VfsTableRowReplacement::from_admitted_mutation(
                        save_table,
                        mutation,
                        vec![pin],
                    )
                    .map_err(TableActivationError::Runtime)?;
                    Ok(VfsSaveCandidate::new(
                        replacement,
                        Box::new(DurableRenameValidator {
                            table: save_table_name,
                            object_id: save_object_id,
                            accepted: row_value,
                            expected_content_transfer: Some(expected_content_transfer),
                            schema: save_schema,
                        }),
                    ))
                },
                &orna_runtime::NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            saved,
            TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(
            target
                .open_read()
                .await
                .read_at(0, replacement_bytes.len())
                .unwrap(),
            replacement_bytes
        );
        assert_eq!(
            old_handle.read_at(0, old_bytes.len()).unwrap(),
            old_bytes,
            "the already-open read handle remains pinned to its original image"
        );
        drop(scratch);
        assert!(retained.upgrade().is_some());
        drop(old_handle);
        assert!(retained.upgrade().is_none());
        let encoded_row = encoded_row.lock().unwrap().clone().unwrap();
        let committed_rows = state
            .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
            .await
            .unwrap();
        assert_eq!(
            committed_rows
                .query_exact(table_for_checks.table(), &key)
                .unwrap(),
            Some(encoded_row.as_slice())
        );
        let pending = state.pending().await.unwrap();
        let transfers = TableMutation::decode(pending.last().unwrap())
            .unwrap()
            .protected_content_transfers()
            .to_vec();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].content_identity(), expected_identity);
        let protected_refs = test_git_output(
            repo.worktree(),
            &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
            None,
        );
        let protected_refs_text = String::from_utf8(protected_refs.clone()).unwrap();
        assert_eq!(protected_refs_text.lines().count(), 1);
        assert!(protected_refs_text.contains("/pending/"));
        assert!(!protected_refs_text.contains("/scratch/"));

        drop(target);
        drop(state);
        let reopened = RuntimeState::open(&repo, runtime_identity, [0x62; 32])
            .await
            .unwrap();
        let reopened_rows = reopened
            .begin_admitted_table_activation(std::slice::from_ref(&table_for_checks))
            .await
            .unwrap();
        assert_eq!(
            reopened_rows
                .query_exact(table_for_checks.table(), &key)
                .unwrap(),
            Some(encoded_row.as_slice())
        );
        let reopened_pending = reopened.pending().await.unwrap();
        assert_eq!(
            TableMutation::decode(reopened_pending.last().unwrap())
                .unwrap()
                .protected_content_transfers(),
            transfers
        );
        assert_eq!(
            test_git_output(
                repo.worktree(),
                &["for-each-ref", "--format=%(refname)", "refs/orna/pins"],
                None,
            ),
            protected_refs
        );
    }

    #[cfg(test)]
    mod git_metadata_reserved_tests {
        use super::naming::is_reserved_component;

        #[test]
        fn git_metadata_spellings_are_reserved_components() {
            for name in [".git", ".GIT", "git~1", ".git. "] {
                assert!(is_reserved_component(name), "{name:?} must be reserved");
            }
            assert!(!is_reserved_component("git-notes"));
            assert!(!is_reserved_component("gitfile"));
        }

        #[test]
        fn row_document_spelling_is_a_reserved_component() {
            assert!(is_reserved_component("data.orna"));
            assert!(!is_reserved_component("data"));
            assert!(!is_reserved_component("data.orna.bak"));
        }
    }
}
