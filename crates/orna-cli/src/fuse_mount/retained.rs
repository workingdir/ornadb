use super::*;

pub(crate) const VFS_RETAINED_DRAFT_DIRECTORY: &str = "vfs-retained-drafts-v1";
pub(crate) const VFS_RETAINED_DRAFT_HEADER: &str = "orna-vfs-retained-draft 1";
/// Recovery storage refuses new records past this count. Every record holds
/// one draft within the draft file bound, so the store stays bounded.
pub(crate) const VFS_RETAINED_DRAFT_LIMIT: usize = 256;
/// Editor drafts hold full candidate bytes, so they live in this private
/// subdirectory of the recovery store rather than the shared temp directory.
pub(crate) const VFS_EDITOR_DRAFT_DIRECTORY: &str = "editor-drafts";

/// Creates the explicit editor draft for `edit` (ORNA-VFS-024 recovery). The
/// file is created exclusively (`O_EXCL`) at mode 0600 under an unpredictable
/// name, inside a 0700 directory of the runtime recovery store. It is kept on
/// disk so a failed edit can be retained and reported by path.
pub(crate) fn create_editor_draft(runtime_root: &Path) -> Result<(File, PathBuf), String> {
    fs::create_dir_all(runtime_root)
        .map_err(|error| format!("runtime recovery root could not be created: {error}"))?;
    let retained = runtime_root.join(VFS_RETAINED_DRAFT_DIRECTORY);
    ensure_private_directory(&retained, "retained draft directory")?;
    let directory = retained.join(VFS_EDITOR_DRAFT_DIRECTORY);
    ensure_private_directory(&directory, "editor draft directory")?;
    let leftovers = count_editor_drafts(&directory)?;
    if leftovers >= VFS_RETAINED_DRAFT_LIMIT {
        return Err(format!(
            "editor draft store is full ({leftovers} leftover drafts, limit {VFS_RETAINED_DRAFT_LIMIT}); remove stale orna-edit-*.draft files"
        ));
    }
    let staged = tempfile::Builder::new()
        .prefix("orna-edit-")
        .suffix(".draft")
        .rand_bytes(16)
        .tempfile_in(&directory)
        .map_err(|error| error.to_string())?;
    staged.keep().map_err(|error| error.error.to_string())
}

/// Creates `directory` (mode 0700) when absent, then requires an existing
/// entry to be a real directory (symlinks refused), owned by the effective
/// uid. A group/other-accessible directory owned by us is tightened to 0700.
pub(crate) fn ensure_private_directory(directory: &Path, label: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("{label} could not be created: {error}")),
    }
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("{label} could not be inspected: {error}"))?;
    if !metadata.file_type().is_dir() {
        return Err(format!("{label} is not a real directory"));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!("{label} is not owned by the current user"));
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("{label} could not be restricted to 0700: {error}"))?;
    }
    Ok(())
}

/// Counts leftover `orna-edit-*.draft` files so the recovery store stays
/// bounded: creation refuses once the count reaches the retained-draft limit.
pub(crate) fn count_editor_drafts(directory: &Path) -> Result<usize, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("editor draft directory could not be read: {error}"))?;
    let mut count = 0;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("editor draft entry could not be read: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("orna-edit-") && name.ends_with(".draft") {
            count += 1;
        }
    }
    Ok(count)
}

/// Denies ownership, mode and flag changes. A mount carries no POSIX
/// permission model (access is owner-only by mount policy), so chmod/chown and
/// chflags are refused with EPERM. Read-only views never reach this check: the
/// kernel refuses their attribute writes with EROFS first.
pub(crate) fn denied_attribute_change(
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    flags: Option<fuser::BsdFileFlags>,
) -> Option<Errno> {
    (mode.is_some() || uid.is_some() || gid.is_some() || flags.is_some()).then_some(Errno::EPERM)
}

/// One candidate kept in recovery storage.
pub(crate) struct RetainedDraft {
    pub(crate) id: String,
    pub(crate) record: PathBuf,
}

/// A candidate recovery storage could not keep, carrying the errno the refused
/// write, rename or removal replies (ORNA-VFS-009, EDIT-1 step 4). A full store
/// is EDQUOT; any other storage failure is EIO.
#[derive(Debug)]
pub(crate) struct RetentionError {
    pub(crate) errno: Errno,
    pub(crate) message: String,
}

impl RetentionError {
    pub(crate) fn io(message: String, error: &std::io::Error) -> Self {
        let full = matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT));
        Self {
            errno: if full { Errno::EDQUOT } else { Errno::EIO },
            message,
        }
    }
}

impl From<String> for RetentionError {
    fn from(message: String) -> Self {
        Self {
            errno: Errno::EIO,
            message,
        }
    }
}

impl std::fmt::Display for RetentionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Reads up to `size` bytes at `offset` from a candidate through `read_chunk`,
/// one [`CANDIDATE_CHUNK_BYTES`] slice at a time. Only the reply buffer holds
/// more than one chunk, so memory is bounded by the reply, never by the
/// candidate's length. Stops at the candidate end.
pub(crate) fn read_candidate_range<E>(
    mut read_chunk: impl FnMut(u64, usize) -> Result<Vec<u8>, E>,
    offset: u64,
    size: u32,
) -> Result<Vec<u8>, E> {
    let wanted = size as usize;
    let mut out = Vec::with_capacity(wanted.min(CANDIDATE_CHUNK_BYTES));
    while out.len() < wanted {
        let position = offset.saturating_add(out.len() as u64);
        let chunk = read_chunk(position, (wanted - out.len()).min(CANDIDATE_CHUNK_BYTES))?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Caps a FUSE read reply at one candidate chunk, so each reply takes the
/// candidate lock once. The kernel re-requests the rest of a larger read.
pub(crate) fn reply_read_size(size: u32) -> u32 {
    size.min(u32::try_from(CANDIDATE_CHUNK_BYTES).unwrap_or(u32::MAX))
}

/// Where a candidate the mount did not accept reads its bytes from.
/// Retention streams a live draft or a truncation by range; nothing is copied whole.
#[derive(Clone)]
pub(crate) enum CandidateSource {
    /// A live draft, read through its spool by range.
    Draft(Arc<EditDraft<RowMapSnapshot>>),
    /// The newest writer draft of the file when one is open or was released
    /// unretained, else its accepted baseline; either is cut or zero-extended to `size`.
    Truncated {
        size: u64,
        latest: Option<Arc<EditDraft<RowMapSnapshot>>>,
    },
}

impl CandidateSource {
    /// The live draft this candidate was cut from: the draft itself, or the draft a truncate cut.
    pub(crate) fn live_draft(&self) -> Option<Arc<EditDraft<RowMapSnapshot>>> {
        match self {
            Self::Draft(draft) => Some(Arc::clone(draft)),
            Self::Truncated { latest, .. } => latest.clone(),
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
/// Reads up to `length` bytes at `offset` of the candidate that results from
/// cutting or zero-extending a base of `base_len` bytes to `size`. `read_base`
/// reads the base's bytes; growth is zero-filled. Copies only the requested window.
pub(crate) fn truncated_window(
    base_len: u64,
    size: u64,
    offset: u64,
    length: usize,
    mut read_base: impl FnMut(u64, usize) -> Result<Vec<u8>, RetentionError>,
) -> Result<Vec<u8>, RetentionError> {
    let end = offset.saturating_add(length as u64).min(size);
    let Some(window) = end.checked_sub(offset).filter(|window| *window > 0) else {
        return Ok(Vec::new());
    };
    let mut out = vec![0_u8; window as usize];
    let copy_end = end.min(base_len);
    if offset < copy_end {
        let copied = (copy_end - offset) as usize;
        let bytes = read_base(offset, copied)?;
        let kept = bytes.len().min(copied);
        out[..kept].copy_from_slice(&bytes[..kept]);
    }
    Ok(out)
}

#[allow(clippy::cast_possible_truncation)]
/// Reads up to `length` bytes at `offset` from in-memory candidate bytes.
pub(crate) fn stashed_range(bytes: &[u8], offset: u64, length: usize) -> Vec<u8> {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start.saturating_add(length).min(bytes.len());
    bytes[start..end].to_vec()
}

/// Keeps a refused scratch candidate in recovery storage before its rename or
/// removal replies, so the bytes outlive the scratch name (ORNA-VFS-009).
pub(crate) fn retain_scratch_candidate(
    save: &RowSaveContext,
    path: &str,
    cause: &str,
    len: u64,
    read_chunk: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, RetentionError>,
) -> Result<(), RetentionError> {
    let mut candidate = QuotaCharge::empty(&save.writer.retained_drafts.quota);
    let retained =
        save.writer
            .retained_drafts
            .retain(path, cause, len, &mut candidate, read_chunk)?;
    eprintln!(
        "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
        retained.id,
        retained.record.display()
    );
    Ok(())
}

/// Records a refused scratch candidate and keeps it in recovery storage before the
/// call that refused it replies (ORNA-VFS-009). Returns the error to reply with: the
/// retention errno when recovery storage cannot keep the candidate.
pub(crate) fn retain_refused_scratch(
    draft_state: &mut DraftState,
    save: &RowSaveContext,
    scratch: &TemporarySave<RowMapSnapshot>,
    inode: u64,
    path: &str,
    error: DocumentCommitError,
) -> DocumentCommitError {
    let cause = error.cause();
    draft_state.rejected_scratch.insert(
        inode,
        ScratchRefusal {
            cause,
            kept: false,
            path: path.to_owned(),
        },
    );
    let len = save.writer.runtime.block_on(scratch.candidate_len());
    let retained = retain_scratch_candidate(save, path, cause, len, &mut |offset, length| {
        save.writer
            .runtime
            .block_on(scratch.read_range(offset, length))
            .map_err(|read| RetentionError::from(candidate_unreadable(read)))
    });
    match retained {
        Ok(()) => {
            if let Some(refusal) = draft_state.rejected_scratch.get_mut(&inode) {
                refusal.kept = true;
            }
            error
        }
        Err(retention) => DocumentCommitError::Unretained {
            errno: retention.errno,
            cause,
        },
    }
}

/// Bounded recovery storage for candidates the mount did not accept
/// (`sys.vfs.draft_retained`, ORNA-VFS-009, ORNA-VFS-024). A record keeps the
/// exact candidate bytes under a content-derived draft ID with the row path and
/// the cause, so a refused or abandoned edit stays recoverable from local state
/// after its handle, scratch name or mount is gone.
pub(crate) struct VfsRetainedDraftStore {
    pub(crate) directory: PathBuf,
    /// The aggregate draft budget every retained record is charged to.
    pub(crate) quota: Arc<DraftQuota>,
    /// One charge per record on disk, keyed by draft ID. A record's charge is
    /// released when the record is removed.
    pub(crate) records: Mutex<BTreeMap<String, QuotaCharge>>,
}

impl VfsRetainedDraftStore {
    /// Opens recovery storage under `runtime_root`. Every record already on disk
    /// is charged to `quota` as it is found, even past the limit, so a restart
    /// cannot grow past the budget; only new retention is refused.
    pub(crate) fn open(runtime_root: &Path, quota: Arc<DraftQuota>) -> Result<Self, String> {
        let directory = runtime_root.join(VFS_RETAINED_DRAFT_DIRECTORY);
        let mut records = BTreeMap::new();
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| {
                        format!("VFS recovery storage could not be listed: {error}")
                    })?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Some(id) = name.strip_suffix(".draft") else {
                        continue;
                    };
                    let metadata = entry.metadata().map_err(|error| {
                        format!("VFS recovery record {name} could not be inspected: {error}")
                    })?;
                    if metadata.is_file() {
                        records.insert(id.to_owned(), QuotaCharge::restore(&quota, metadata.len()));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("VFS recovery storage could not be listed: {error}"));
            }
        }
        Ok(Self {
            directory,
            quota,
            records: Mutex::new(records),
        })
    }

    /// Removes the record `id` and releases its quota charge. A record that is
    /// already gone is not an error.
    pub(crate) fn remove(&self, id: &str) -> Result<(), RetentionError> {
        let record = self.directory.join(format!("{id}.draft"));
        match fs::remove_file(&record) {
            Ok(()) => VfsScratchStore::sync_directory(&self.directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RetentionError::io(
                    format!("retained draft {id} could not be removed: {error}"),
                    &error,
                ));
            }
        }
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    /// Retains `bytes` as a candidate for the projected `path`. Retaining the
    /// same candidate for the same path again keeps its first record and ID.
    /// Holds one chunk at a time: `read_chunk(offset, length)` supplies the
    /// candidate, which is digested and written in two streaming passes.
    /// A live candidate passes its charge as `candidate`: a new record absorbs it, so
    /// its bytes are charged once. A record already on disk leaves `candidate` alone.
    pub(crate) fn retain(
        &self,
        path: &str,
        cause: &str,
        len: u64,
        candidate: &mut QuotaCharge,
        read_chunk: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, RetentionError>,
    ) -> Result<RetainedDraft, RetentionError> {
        let mut prefix = Sha256::new();
        prefix.update(b"orna.vfs.retained-draft.v1\0");
        VfsScratchStore::update_digest(&mut prefix, path.as_bytes());
        prefix.update(len.to_le_bytes());
        let mut digest = prefix.clone();
        Self::stream_candidate(len, read_chunk, |chunk| {
            digest.update(chunk);
            Ok(())
        })?;
        let digest = digest.finalize();
        let id = hex(&digest[..16]);
        let record = self.directory.join(format!("{id}.draft"));
        if record.is_file() {
            // A record already on disk, written by another process, counts too.
            let on_disk = fs::metadata(&record).map_or(0, |metadata| metadata.len());
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(id.clone())
                .or_insert_with(|| QuotaCharge::restore(&self.quota, on_disk));
            return Ok(RetainedDraft { id, record });
        }
        match fs::create_dir(&self.directory) {
            Ok(()) => {
                if let Some(parent) = self.directory.parent() {
                    VfsScratchStore::sync_directory(parent)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(RetentionError::io(
                    format!("VFS recovery storage could not be created: {error}"),
                    &error,
                ));
            }
        }
        let retained = fs::read_dir(&self.directory)
            .map_err(|error| {
                RetentionError::io(
                    format!("VFS recovery storage could not be listed: {error}"),
                    &error,
                )
            })?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".draft"))
            .count();
        if retained >= VFS_RETAINED_DRAFT_LIMIT {
            return Err(RetentionError {
                errno: Errno::EDQUOT,
                message: format!(
                    "VFS recovery storage {} already holds {VFS_RETAINED_DRAFT_LIMIT} drafts; remove recovered records first",
                    self.directory.display()
                ),
            });
        }
        let header = format!(
            "{VFS_RETAINED_DRAFT_HEADER}\ndraft {id}\npath {path}\ncause {cause}\nbytes {len}\n\n"
        );
        // The whole record is charged before any byte is staged, so a refused
        // reservation (EDQUOT) leaves nothing behind. Bytes the candidate already
        // holds are not charged again.
        let mut charge = QuotaCharge::reserve(
            &self.quota,
            (header.len() as u64 + len).saturating_sub(candidate.bytes()),
        )
        .map_err(|_| {
            RetentionError {
                errno: Errno::EDQUOT,
                message: format!(
                    "VFS recovery storage quota is exhausted; a {len}-byte candidate cannot be retained; remove recovered records first"
                ),
            }
        })?;
        let mut staged = tempfile::Builder::new()
            .prefix(".retain-")
            .tempfile_in(&self.directory)
            .map_err(|error| {
                RetentionError::io(
                    format!("retained draft could not be staged: {error}"),
                    &error,
                )
            })?;
        let written = |error: std::io::Error| {
            RetentionError::io(
                format!("retained draft could not be written: {error}"),
                &error,
            )
        };
        staged.write_all(header.as_bytes()).map_err(written)?;
        let mut check = prefix.clone();
        Self::stream_candidate(len, read_chunk, |chunk| {
            check.update(chunk);
            staged.write_all(chunk).map_err(written)
        })?;
        // The candidate is streamed twice; a change between the passes would
        // retain bytes that do not match the record's ID.
        if check.finalize() != digest {
            return Err(RetentionError::from(
                "retained candidate changed while it was being written".to_owned(),
            ));
        }
        staged.as_file().sync_all().map_err(written)?;
        staged.persist(&record).map_err(|error| {
            RetentionError::io(
                format!("retained draft could not be published: {}", error.error),
                &error.error,
            )
        })?;
        charge.absorb(candidate);
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), charge);
        VfsScratchStore::sync_directory(&self.directory)?;
        Ok(RetainedDraft { id, record })
    }

    /// Feeds a candidate of `len` bytes to `sink` one [`CANDIDATE_CHUNK_BYTES`]
    /// slice at a time, so no whole copy is made. Fails if the source ends early.
    pub(crate) fn stream_candidate(
        len: u64,
        read_chunk: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, RetentionError>,
        mut sink: impl FnMut(&[u8]) -> Result<(), RetentionError>,
    ) -> Result<(), RetentionError> {
        let mut offset = 0_u64;
        while offset < len {
            let wanted = usize::try_from(len - offset)
                .unwrap_or(usize::MAX)
                .min(CANDIDATE_CHUNK_BYTES);
            let chunk = read_chunk(offset, wanted)?;
            if chunk.is_empty() {
                return Err(RetentionError::from(
                    "retained candidate ended before its recorded length".to_owned(),
                ));
            }
            sink(&chunk)?;
            offset += chunk.len() as u64;
        }
        Ok(())
    }
}
