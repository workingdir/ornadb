//! Private disk-backed EDIT-1 candidates and the aggregate draft quota.
//!
//! A candidate is an unlinked private file, so its bytes never appear under a
//! user-visible name and are released with the handle. Every logical byte is
//! charged to one quota shared by all drafts in a repository view, including a
//! retained rejected candidate, and readers stream through a bounded view so a
//! large candidate is never copied whole into memory.

use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt as _, FileExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use uuid::Uuid;

/// Aggregate byte budget shared by every open EDIT-1 candidate spool.
#[derive(Debug)]
pub struct DraftQuota {
    limit: u64,
    pub(super) used: AtomicU64,
}

impl DraftQuota {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: AtomicU64::new(0),
        }
    }

    pub(super) fn reserve(&self, bytes: u64) -> Result<(), EditDraftError> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current
                .checked_add(bytes)
                .filter(|next| *next <= self.limit)
                .ok_or(EditDraftError::QuotaExceeded)?;
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    pub(super) fn release(&self, bytes: u64) {
        // Saturating: a release that exceeds the charged total must not wrap the
        // aggregate budget, which would admit reservations past the limit.
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_sub(bytes))
            });
    }
}

/// Process-wide default budget for [`EditDraft::open`]. The spec names an
/// aggregate draft quota without a value; this bound is conservative and is
/// flagged for review.
const DEFAULT_DRAFT_QUOTA_BYTES: u64 = 1 << 30;

static DEFAULT_DRAFT_QUOTA: LazyLock<Arc<DraftQuota>> =
    LazyLock::new(|| Arc::new(DraftQuota::new(DEFAULT_DRAFT_QUOTA_BYTES)));

/// The process-wide default aggregate draft quota that
/// [`VfsRepositoryCache::in_runtime_root`] charges when no budget is supplied.
pub fn default_draft_quota() -> Arc<DraftQuota> {
    Arc::clone(&DEFAULT_DRAFT_QUOTA)
}

/// Where EDIT-1 candidate spools are created and which aggregate quota charges
/// them. Production routes through the runtime root's private draft directory.
#[derive(Clone)]
pub struct DraftStorage {
    directory: Arc<Path>,
    quota: Arc<DraftQuota>,
}

impl DraftStorage {
    /// Spools in the process temp directory under the process-wide default
    /// quota. Used by repository views that have no runtime root.
    pub fn with_default_quota() -> Self {
        Self::new(&std::env::temp_dir(), Arc::clone(&DEFAULT_DRAFT_QUOTA))
    }

    /// Spools in `directory`, charged to `quota`.
    pub fn new(directory: &Path, quota: Arc<DraftQuota>) -> Self {
        Self {
            directory: Arc::from(directory),
            quota,
        }
    }

    /// The aggregate quota that charges this storage's spools.
    pub fn quota(&self) -> Arc<DraftQuota> {
        Arc::clone(&self.quota)
    }

    /// Creates or reuses the private spool directory under the runtime root.
    /// A pre-existing path is accepted only if it is a real directory (not a
    /// symlink) owned by the same user this process creates files as; stale
    /// spool files left by an earlier crash are removed.
    pub(super) fn in_runtime_root(root: &Path, quota: Arc<DraftQuota>) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let directory = root.join(DRAFT_SPOOL_DIRECTORY);
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "draft spool path is not a real directory",
            ));
        }
        let probe = directory.join(format!("orna-probe-{}", Uuid::new_v4().simple()));
        let probe_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)?;
        let probe_owner = probe_file.metadata()?.uid();
        drop(probe_file);
        fs::remove_file(&probe)?;
        if metadata.uid() != probe_owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "draft spool directory is owned by another user",
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let stale = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("orna-draft-"));
            if stale && entry.file_type()?.is_file() {
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(Self::new(&directory, quota))
    }
}

/// Subdirectory of the runtime root that holds private EDIT-1 candidate spools.
pub(super) const DRAFT_SPOOL_DIRECTORY: &str = "vfs-draft-spool-v1";

/// Largest slice of a candidate that a live-mount read, copy or digest holds in
/// memory at once. Candidates stream through this bound, so memory does not
/// grow with candidate size.
pub const CANDIDATE_CHUNK_BYTES: usize = 1 << 20;

/// Logical bytes of one EDIT-1 candidate, held in an unlinked private file in
/// the storage directory. Holes are sparse and cost no RAM; every logical byte
/// is charged to the quota until the spool drops.
pub(super) struct DraftSpool {
    pub(super) file: fs::File,
    pub(super) len: u64,
    pub(super) charge: QuotaCharge,
}

impl DraftSpool {
    fn create(storage: &DraftStorage) -> Result<Self, EditDraftError> {
        let directory = &storage.directory;
        for _ in 0..8 {
            let path = directory.join(format!("orna-draft-{}", Uuid::new_v4().simple()));
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create_new(true).mode(0o600);
            match options.open(&path) {
                Ok(file) => {
                    // Unlink at once so the spool lives only through this handle.
                    fs::remove_file(&path).map_err(|_| EditDraftError::BackingIo)?;
                    return Ok(Self {
                        file,
                        len: 0,
                        charge: QuotaCharge::empty(&storage.quota),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(EditDraftError::BackingIo),
            }
        }
        Err(EditDraftError::BackingIo)
    }

    pub(super) fn from_baseline(
        bytes: &CandidateSnapshot,
        storage: &DraftStorage,
    ) -> Result<Self, EditDraftError> {
        let mut spool = Self::create(storage)?;
        spool.set_len(bytes.len())?;
        let mut reader = bytes.reader();
        let chunk = usize::try_from(bytes.len())
            .map_or(CANDIDATE_CHUNK_BYTES, |len| len.min(CANDIDATE_CHUNK_BYTES));
        let mut buffer = vec![0; chunk];
        let mut offset = 0_u64;
        loop {
            let count =
                io::Read::read(&mut reader, &mut buffer).map_err(|_| EditDraftError::BackingIo)?;
            if count == 0 {
                return Ok(spool);
            }
            spool
                .file
                .write_all_at(&buffer[..count], offset)
                .map_err(|_| EditDraftError::BackingIo)?;
            offset += count as u64;
        }
    }

    /// Grows (sparse, zero-filled) or shrinks the spool to `length`, moving the
    /// quota charge with it. A growth that the quota refuses changes nothing.
    pub(super) fn set_len(&mut self, length: u64) -> Result<(), EditDraftError> {
        if length > self.len {
            let grow = length - self.len;
            self.charge.grow(grow)?;
            if self.file.set_len(length).is_err() {
                self.charge.shrink(grow);
                return Err(EditDraftError::BackingIo);
            }
        } else if length < self.len {
            self.file
                .set_len(length)
                .map_err(|_| EditDraftError::BackingIo)?;
            self.charge.shrink(self.len - length);
        }
        self.len = length;
        Ok(())
    }

    pub(super) fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), EditDraftError> {
        let length = u64::try_from(bytes.len()).map_err(|_| EditDraftError::OffsetOverflow)?;
        let end = offset
            .checked_add(length)
            .ok_or(EditDraftError::OffsetOverflow)?;
        self.set_len(self.len.max(end))?;
        self.file
            .write_all_at(bytes, offset)
            .map_err(|_| EditDraftError::BackingIo)
    }

    pub(super) fn read_all(&self) -> io::Result<Arc<[u8]>> {
        let length = usize::try_from(self.len).map_err(|_| io::Error::other("draft too large"))?;
        // One allocation: the zeroed buffer is read in place, so no intermediate `Vec` is copied
        // into a second `Arc` buffer.
        let zeroed = Arc::<[u8]>::new_zeroed_slice(length);
        // SAFETY: every byte of the buffer is zero, which is a valid `u8`, so it is initialised.
        let mut bytes = unsafe { zeroed.assume_init() };
        let buffer = Arc::get_mut(&mut bytes).expect("a fresh Arc is unique");
        self.file.read_exact_at(buffer, 0)?;
        Ok(bytes)
    }

    /// Reads `[offset, offset + length)` clamped to the spool length. Holds at
    /// most `length` bytes.
    pub(super) fn read_range(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        let end = offset.saturating_add(length as u64).min(self.len);
        if offset >= end {
            return Ok(Vec::new());
        }
        let size =
            usize::try_from(end - offset).map_err(|_| io::Error::other("range too large"))?;
        let mut bytes = vec![0; size];
        self.file.read_exact_at(&mut bytes, offset)?;
        Ok(bytes)
    }
}

/// A read-only, ranged view of one candidate revision, valid for the fsync
/// callback that receives it. It holds a private handle on the spool or a
/// share of the baseline bytes, never a copy of the candidate, so a consumer
/// streams it through [`CANDIDATE_CHUNK_BYTES`]-sized reads.
pub struct CandidateSnapshot {
    pub(super) len: u64,
    pub(super) source: CandidateSource,
}

pub(super) enum CandidateSource {
    Spool(fs::File),
    Baseline(Arc<[u8]>),
}

impl From<Arc<[u8]>> for CandidateSnapshot {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self {
            len: bytes.len() as u64,
            source: CandidateSource::Baseline(bytes),
        }
    }
}

impl CandidateSnapshot {
    /// The candidate's length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the candidate has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Another view of the same candidate. A spool gets a duplicate handle on
    /// its private file and a baseline shares its bytes; no candidate bytes
    /// are copied.
    pub fn duplicate(&self) -> io::Result<Self> {
        let source = match &self.source {
            CandidateSource::Spool(file) => CandidateSource::Spool(file.try_clone()?),
            CandidateSource::Baseline(bytes) => CandidateSource::Baseline(Arc::clone(bytes)),
        };
        Ok(Self {
            len: self.len,
            source,
        })
    }

    /// Materialises the whole candidate in one allocation. Only retention and
    /// whole-value activation need it; streaming consumers use
    /// [`Self::reader`] or [`Self::read_range`].
    pub fn to_arc(&self) -> io::Result<Arc<[u8]>> {
        let length =
            usize::try_from(self.len).map_err(|_| io::Error::other("candidate too large"))?;
        // The zeroed buffer is read in place, so no intermediate copy is made.
        let zeroed = Arc::<[u8]>::new_zeroed_slice(length);
        // SAFETY: every byte of the buffer is zero, which is a valid `u8`, so it is initialised.
        let mut bytes = unsafe { zeroed.assume_init() };
        let buffer = Arc::get_mut(&mut bytes).expect("a fresh Arc is unique");
        self.read_into(0, buffer)?;
        Ok(bytes)
    }

    /// Reads `[offset, offset + length)` clamped to the candidate length. Holds
    /// at most `length` bytes.
    pub fn read_range(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        let end = offset.saturating_add(length as u64).min(self.len);
        if offset >= end {
            return Ok(Vec::new());
        }
        let size =
            usize::try_from(end - offset).map_err(|_| io::Error::other("range too large"))?;
        let mut bytes = vec![0; size];
        self.read_into(offset, &mut bytes)?;
        Ok(bytes)
    }

    /// A sequential reader over the whole candidate. Each call holds at most
    /// [`CANDIDATE_CHUNK_BYTES`] of it, so streaming never copies the candidate.
    pub fn reader(&self) -> CandidateReader<'_> {
        CandidateReader {
            snapshot: self,
            offset: 0,
        }
    }

    /// Fills `buffer` from `offset`; the range must lie within the candidate.
    fn read_into(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        match &self.source {
            CandidateSource::Spool(file) => file.read_exact_at(buffer, offset),
            CandidateSource::Baseline(source) => {
                let start =
                    usize::try_from(offset).map_err(|_| io::Error::other("range too large"))?;
                buffer.copy_from_slice(&source[start..start + buffer.len()]);
                Ok(())
            }
        }
    }
}

/// Sequential [`io::Read`] over a [`CandidateSnapshot`], one bounded chunk per call.
pub struct CandidateReader<'a> {
    snapshot: &'a CandidateSnapshot,
    offset: u64,
}

impl io::Read for CandidateReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining =
            usize::try_from(self.snapshot.len.saturating_sub(self.offset)).unwrap_or(usize::MAX);
        let size = buffer.len().min(CANDIDATE_CHUNK_BYTES).min(remaining);
        self.snapshot.read_into(self.offset, &mut buffer[..size])?;
        self.offset += size as u64;
        Ok(size)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditDraftError {
    FileTooLarge,
    OffsetOverflow,
    RevisionExhausted,
    AlreadyApplied,
    /// The aggregate draft quota cannot hold the requested bytes.
    QuotaExceeded,
    /// The private candidate spool could not be created, read, or written.
    BackingIo,
}
/// Quota charge for bytes held beyond a live spool: one retained rejected
/// candidate, or one on-disk retained draft record. Dropping the charge
/// releases its bytes.
pub struct QuotaCharge {
    quota: Arc<DraftQuota>,
    bytes: u64,
}

impl QuotaCharge {
    /// Charges `bytes` to `quota`, refusing with [`EditDraftError::QuotaExceeded`]
    /// when the quota cannot hold them.
    pub fn reserve(quota: &Arc<DraftQuota>, bytes: u64) -> Result<Self, EditDraftError> {
        quota.reserve(bytes)?;
        Ok(Self {
            quota: Arc::clone(quota),
            bytes,
        })
    }

    /// Charges `bytes` that already exist on disk, even past the limit. Only new
    /// reservations are refused until enough charges are released.
    pub fn restore(quota: &Arc<DraftQuota>, bytes: u64) -> Self {
        quota.used.fetch_add(bytes, Ordering::AcqRel);
        Self {
            quota: Arc::clone(quota),
            bytes,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// A charge that holds no bytes yet, to be grown or absorbed into.
    pub fn empty(quota: &Arc<DraftQuota>) -> Self {
        Self {
            quota: Arc::clone(quota),
            bytes: 0,
        }
    }

    /// Charges `bytes` more, refusing (and changing nothing) when the quota cannot hold them.
    fn grow(&mut self, bytes: u64) -> Result<(), EditDraftError> {
        self.quota.reserve(bytes)?;
        self.bytes += bytes;
        Ok(())
    }

    /// Releases up to `bytes` of this charge.
    fn shrink(&mut self, bytes: u64) {
        let released = bytes.min(self.bytes);
        self.quota.release(released);
        self.bytes -= released;
    }

    /// Moves every byte of `other` into this charge, which must share its quota.
    /// `other` is left holding none.
    pub fn absorb(&mut self, other: &mut QuotaCharge) {
        self.bytes += other.bytes;
        other.bytes = 0;
    }

    /// Moves every byte of this charge into a new charge, leaving this one holding none.
    pub fn take(&mut self) -> QuotaCharge {
        let mut moved = QuotaCharge::empty(&self.quota);
        moved.absorb(self);
        moved
    }
}

impl Drop for QuotaCharge {
    fn drop(&mut self) {
        self.quota.release(self.bytes);
    }
}
