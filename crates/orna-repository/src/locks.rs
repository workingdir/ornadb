//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Longest a publication waits for the repository coordination lock before it
/// reports `RepositoryBusy`. Bounded so a hung holder or a same-thread
/// re-entry cannot block publication forever; long enough to queue behind any
/// short coordination section that holds the lock.
pub(crate) const COORDINATION_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Sleep between try-lock attempts while the coordination lock is contended.
pub(crate) const COORDINATION_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Clone, Copy)]
pub(crate) enum ManagedIndexOperation<'contents> {
    Add,
    AddContents(&'contents [u8]),
    Unstage,
}

pub(crate) struct CoordinationLock {
    pub(crate) file: fs::File,
}

impl Drop for CoordinationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The held worktree publication lock (PUB-3 step 2 and step 7).
///
/// The lock is an advisory `flock` on the worktree's coordination file. The
/// kernel owns it: a killed publisher cannot leave it held, so an abandoned
/// publisher cannot keep writing publication state (ORNA-TURSO-006). The lease
/// is the coordination primitive cooperating Orna worktree mutators and
/// publishers share (ORNA-TURSO-005), and it names the runtime root it covers
/// so a lease taken in one worktree cannot authorise writes in another.
pub struct PublicationLease {
    pub(crate) file: fs::File,
    pub(crate) runtime_root: PathBuf,
}

impl PublicationLease {
    /// The runtime root whose publication lock this lease holds.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }
}

impl Drop for PublicationLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The maintenance-boundary mode requested from `maintenance.lock`.
#[derive(Clone, Copy)]
pub(crate) enum MaintenanceMode {
    Shared,
    Exclusive,
}

/// A shared or exclusive hold on the declared maintenance boundary
/// (ORNA-CAP-006). The lease is released when it is dropped.
pub struct MaintenanceLease {
    pub(crate) file: fs::File,
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Opens an advisory lock file, creating it when absent and never truncating
/// it. The kernel owns the lock, so the bytes are never read or written back;
/// `O_NOFOLLOW` keeps a planted symlink from redirecting the lock.
pub(crate) fn open_lock_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Polls `attempt` until it takes the lock or `deadline` passes. A contended
/// or interrupted attempt is retried; the wait is bounded, so a held or hung
/// lock is reported as `RepositoryBusy` rather than blocking forever.
pub(crate) fn lock_before_deadline(
    file: fs::File,
    deadline: std::time::Instant,
    mut attempt: impl FnMut(&fs::File) -> std::io::Result<()>,
) -> Result<fs::File, RepositoryError> {
    loop {
        match attempt(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                if std::time::Instant::now() >= deadline {
                    return Err(RepositoryError::RepositoryBusy);
                }
                std::thread::sleep(COORDINATION_LOCK_POLL);
            }
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
    }
}

/// Writes `bytes` to `path` through `temporary`: create it exclusively, write,
/// sync it, rename it over `path`, then sync the directory so the rename is
/// durable. A failure removes the temporary and leaves `path` untouched.
pub(crate) fn write_atomically(
    path: &Path,
    temporary: &Path,
    bytes: &[u8],
) -> Result<(), RepositoryError> {
    let parent = path
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.write_all(bytes)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.sync_all()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::rename(temporary, path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

const RUNTIME_OWNER_STATE_MAGIC: &[u8; 8] = b"ORNAOWN2";
const RUNTIME_OWNER_CHAIN_MAX_ENTRIES: usize = 65_536;

pub(crate) fn read_runtime_owner_state(
    path: &Path,
    legacy_lock: &mut fs::File,
) -> Result<([u8; 16], Vec<[u8; 16]>), RepositoryError> {
    let bytes = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::read(path).map_err(|_| RepositoryError::LocalStateUnavailable)?
        }
        Ok(_) => return Err(RepositoryError::LocalStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            legacy_lock
                .seek(SeekFrom::Start(0))
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let mut legacy = Vec::new();
            (&*legacy_lock)
                .take(17)
                .read_to_end(&mut legacy)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            return match legacy.as_slice() {
                bytes if bytes.len() == 16 => Ok((
                    bytes
                        .try_into()
                        .map_err(|_| RepositoryError::LocalStateUnavailable)?,
                    Vec::new(),
                )),
                [] => Ok(([0; 16], Vec::new())),
                _ => Err(RepositoryError::LocalStateUnavailable),
            };
        }
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    };
    if bytes.len() < 28 || &bytes[..8] != RUNTIME_OWNER_STATE_MAGIC {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let current: [u8; 16] = bytes[8..24]
        .try_into()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let count = u32::from_le_bytes(
        bytes[24..28]
            .try_into()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?,
    ) as usize;
    let expected_length = 28usize
        .checked_add(
            count
                .checked_mul(16)
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    if current == [0; 16]
        || count > RUNTIME_OWNER_CHAIN_MAX_ENTRIES
        || bytes.len() != expected_length
    {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let mut seen = BTreeSet::new();
    let mut departed = Vec::with_capacity(count);
    for chunk in bytes[28..].chunks_exact(16) {
        let owner: [u8; 16] = chunk
            .try_into()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if owner == [0; 16] || !seen.insert(owner) {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        departed.push(owner);
    }
    Ok((current, departed))
}

pub(crate) fn persist_runtime_owner_state(
    path: &Path,
    current: [u8; 16],
    departed: &[[u8; 16]],
) -> Result<(), RepositoryError> {
    if current == [0; 16] || departed.len() > RUNTIME_OWNER_CHAIN_MAX_ENTRIES {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let count =
        u32::try_from(departed.len()).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let mut bytes = Vec::with_capacity(28 + departed.len() * 16);
    bytes.extend_from_slice(RUNTIME_OWNER_STATE_MAGIC);
    bytes.extend_from_slice(&current);
    bytes.extend_from_slice(&count.to_le_bytes());
    let mut seen = BTreeSet::new();
    for owner in departed {
        if *owner == [0; 16] || !seen.insert(*owner) {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        bytes.extend_from_slice(owner);
    }
    let parent = path
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let temporary = parent.join("runtime-owner-state.next");
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(&temporary).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
        Ok(_) => return Err(RepositoryError::LocalStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    }
    write_atomically(path, &temporary, &bytes)
}

/// A file staged next to the one it will replace: an index candidate, or the
/// private index a candidate commit is built from. Dropping it removes the
/// file, so every early return leaves nothing behind; a successful rename has
/// already moved the file away, which makes the removal a no-op.
pub(crate) struct StagedFile {
    path: PathBuf,
}

impl StagedFile {
    /// Claims `path` for a file another process creates, such as the index Git
    /// builds. Nothing is written here.
    pub(crate) fn reserve(path: PathBuf) -> Self {
        Self { path }
    }

    /// Writes `bytes` to a brand-new file. `create_new` refuses any existing
    /// file or symlink at the path, so a candidate is never written through a
    /// link or over an attacker-prepared name, and the guard exists only once
    /// the file is ours to remove.
    pub(crate) fn create(path: PathBuf, bytes: &[u8]) -> Result<Self, RepositoryError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let staged = Self { path };
        file.write_all(bytes)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(staged)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        // Best effort by design: the file is already gone after a successful
        // rename, and a failed removal must not mask the error that ended the
        // operation.
        let _ = fs::remove_file(&self.path);
    }
}

/// A process-lifetime local runtime owner lock.
pub struct RuntimeOwnerLock {
    pub(crate) file: fs::File,
    pub(crate) repository: Repository,
    pub(crate) previous_owner: Option<[u8; 16]>,
    pub(crate) current_owner: [u8; 16],
    pub(crate) departed_owners: Vec<[u8; 16]>,
    pub(crate) owner_state_path: PathBuf,
}

impl RuntimeOwnerLock {
    /// Returns the owner identity recorded by the previous holder, if any.
    pub fn previous_owner(&self) -> Option<[u8; 16]> {
        self.previous_owner
    }

    /// Returns the persisted departed owners awaiting manifest-backed cleanup.
    pub fn departed_owners(&self) -> &[[u8; 16]] {
        &self.departed_owners
    }

    /// Reconciles every departed owner's refs against a fully validated pin
    /// manifest grouped by owner. Returns `None` while a publication journal
    /// remains authoritative; the durable chain is retained for retry.
    pub fn reclaim_departed_owner_pins(
        &self,
        accepted_by_owner: &BTreeMap<[u8; 16], BTreeMap<[u8; 16], native_graph::NativeOid>>,
    ) -> Result<Option<usize>, RepositoryError> {
        let empty = BTreeMap::new();
        let departed = self
            .departed_owners
            .iter()
            .map(|owner| (*owner, accepted_by_owner.get(owner).unwrap_or(&empty)))
            .collect::<Vec<_>>();
        let reclaimed =
            match native_graph::reclaim_departed_owner_pin_refs(&self.repository, &departed) {
                Ok(count) => count,
                Err(native_graph::GraphError::PublicationNotTerminal) => return Ok(None),
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            };
        if !self.departed_owners.is_empty() {
            persist_runtime_owner_state(&self.owner_state_path, self.current_owner, &[])?;
        }
        Ok(Some(reclaimed))
    }

    /// Reconciles the previous owner's pins against an already-validated
    /// accepted-content manifest. Retained entries map durable pin IDs to
    /// their expected Blob descriptor OIDs. Call only after runtime recovery
    /// has verified that manifest; an in-flight publication journal defers
    /// cleanup because it remains authoritative for candidate roots.
    pub fn reclaim_previous_owner_pins(
        &self,
        accepted_pending: &std::collections::BTreeMap<[u8; 16], native_graph::NativeOid>,
    ) -> Result<usize, RepositoryError> {
        let Some(dead_owner) = self.previous_owner else {
            return Ok(0);
        };
        if self.repository.read_publication_journal()?.is_some() {
            return Err(RepositoryError::PublicationPending);
        }
        native_graph::reclaim_dead_owner_pin_refs(&self.repository, dead_owner, accepted_pending)
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }
}

impl Drop for RuntimeOwnerLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
