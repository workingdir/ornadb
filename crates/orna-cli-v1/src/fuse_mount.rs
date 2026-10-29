use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, ReplyXattr, Request,
};
use orna_conformance_v1::transactional_loaded_project_candidate_validator;
use orna_evaluator_v1::Limits as EvaluatorLimits;
use orna_project_v1::{
    ObjectIdentityManifest, ObjectKind, ProjectLoader, ResolvedTableSchema,
    admit_source_row_with_owner_fields,
};
use orna_repository_v1::{
    AdmittedBlobReference, CommittedTableBinding, ContentIdentity, GitCommitRef, GitHashAlgorithm,
    GraphError, HistoricalFormatContext, KeyRange, NativeGraphContext, NativeOid, OrpBlobBinding,
    Repository, RepositoryReadScope, RowMapSnapshot, RuntimeGeneration, TypedKey,
    MountRowBaselines,
};
use orna_runtime_v1::{
    NoFault, RuntimeState, RuntimeTableActivationSnapshot, RuntimeTableIdentity, RuntimeTableRows,
    TableObjectId,
    vfs::{
        CANDIDATE_CHUNK_BYTES, EditDraft, EditDraftError, ManagedFile, SnapshotPin, TemporaryRenameOutcome, TemporarySave,
        VfsDurableSave, VfsComponent, VfsFileSnapshot, VfsRepositoryCache, VfsRowProjection,
        VfsSaveCandidate, VfsSaveCoordinates, VfsSaveError, VfsStoredField, VfsTableRowReplacement,
        project_component, project_key_component, unproject_component,
        FsyncFailure,
    },
};
use orna_storage_v1::COMPACT_FILE_BOUND_BYTES;
use orna_syntax_v1::{Expr, LiteralKind, parse_row};
use orna_value_v1::{
    Format3Context, Raw, SchemaDescriptor, blob_metadata_from_raw, decode_canonical_storage_value,
};
use sha2::{Digest, Sha256};

/// Implementation draft quota for every mounted draft and scratch file: 64 MiB,
/// the compact publication file bound (ORNA-PUB-001). It is not a VFS spec
/// bound. A write or truncate that grows a file past it is quota-rejected
/// (EDQUOT) and never admitted; shrinking an existing larger baseline is allowed.
const DRAFT_FILE_BOUND_BYTES: usize = COMPACT_FILE_BOUND_BYTES as usize;

/// Bound on unbound editor scratch files one mount may create (41-vfs.md Temporary-save
/// replacement). Unbound scratch is recovery state, so an editor cannot grow it without limit.
/// Records restored from earlier mounts are not counted (`VfsScratchStore::reserve_unbound`).
const MAX_UNBOUND_SCRATCH: usize = 16;

/// Errno for a rejected draft operation (profiles/vfs-1 errno table): EDQUOT
/// for the per-file or aggregate draft quota, EIO for backing I/O failures.
fn draft_errno(result: &Result<(), EditDraftError>) -> Errno {
    match result {
        Err(EditDraftError::FileTooLarge | EditDraftError::QuotaExceeded) => Errno::EDQUOT,
        _ => Errno::EIO,
    }
}

/// Diagnostic for a private candidate spool that could not be read. The
/// operation fails on this error; no bytes are guessed.
fn candidate_unreadable(error: EditDraftError) -> String {
    format!("private candidate could not be read: {error:?}")
}

/// Errno for a mutation refused on the live writable mount. Every such refusal
/// is a denied or unsupported operation, so it is EPERM (41-vfs.md:103-105;
/// profiles/vfs-1.md:139). EROFS is reserved for the kernel read-only snapshot
/// mount (spawn_read_only), which never reaches these handlers.
fn refused_mutation_errno() -> Errno {
    Errno::EPERM
}

/// Errno for an operation outside the VFS-1 contract (ORNA-VFS-014;
/// 34-system-reference.md `sys.vfs.unsupported`). The stable cause is logged to
/// the mount's stderr; the refusal is the table errno, never a fabricated success.
fn refuse_unsupported(operation: &str) -> Errno {
    eprintln!("{operation}: unsupported in VFS-1 (sys.vfs.unsupported)");
    refused_mutation_errno()
}

const VFS_SCRATCH_DIRECTORY: &str = "vfs-editor-scratch-v1";
const VFS_SCRATCH_TEMP_PREFIX: &str = ".scratch-save-";
const VFS_SCRATCH_RECORD_MAGIC: &[u8; 8] = b"ORNAVFS1";

#[derive(Clone, Debug)]
struct TemporaryScratchIdentity {
    logical_path: String,
    relation_id: [u8; 16],
    key: Vec<u8>,
    name: String,
    blob_field_name: Option<String>,
}

struct PersistedTemporaryScratch {
    identity: TemporaryScratchIdentity,
    baseline: Option<Vec<u8>>,
    bytes: Vec<u8>,
}

struct VfsScratchStore {
    directory: PathBuf,
    /// Unbound scratch record names this mount created; restored records are never counted.
    unbound_created: Mutex<HashSet<String>>,
}

impl VfsScratchStore {
    fn open(runtime_root: &Path) -> Result<Self, String> {
        let directory = runtime_root.join(VFS_SCRATCH_DIRECTORY);
        let created = {
            use std::os::unix::fs::DirBuilderExt as _;
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => {
                    return Err(format!(
                        "temporary scratch directory could not be created: {error}"
                    ));
                }
            }
        };
        if created {
            Self::sync_directory(runtime_root)?;
        }
        ensure_private_directory(&directory, "temporary scratch directory")?;
        Self::sync_directory(&directory)?;
        Ok(Self {
            directory,
            unbound_created: Mutex::new(HashSet::new()),
        })
    }

    fn persist(
        &self,
        identity: &TemporaryScratchIdentity,
        baseline: Option<&[u8]>,
        bytes: &[u8],
    ) -> Result<(), String> {
        Self::validate_identity(identity)?;
        let mut staged = tempfile::Builder::new()
            .prefix(VFS_SCRATCH_TEMP_PREFIX)
            .tempfile_in(&self.directory)
            .map_err(|error| format!("temporary scratch record could not be staged: {error}"))?;
        staged
            .write_all(VFS_SCRATCH_RECORD_MAGIC)
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_identity(staged.as_file_mut(), identity)?;
        Self::write_optional(staged.as_file_mut(), baseline)?;
        Self::write_bytes(staged.as_file_mut(), bytes)?;
        staged.as_file().sync_all().map_err(|error| {
            format!("temporary scratch record could not be synchronized: {error}")
        })?;
        staged
            .persist(self.record_path(identity))
            .map_err(|error| {
                format!(
                    "temporary scratch record could not be published: {}",
                    error.error
                )
            })?;
        Self::sync_directory(&self.directory)
    }

    fn load(&self) -> Result<Vec<PersistedTemporaryScratch>, String> {
        let mut records = Vec::new();
        let mut removed_staging = false;
        for entry in fs::read_dir(&self.directory)
            .map_err(|error| format!("temporary scratch records could not be listed: {error}"))?
        {
            let entry = entry.map_err(|error| {
                format!("temporary scratch record could not be inspected: {error}")
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!("temporary scratch record type could not be read: {error}")
            })?;
            if !file_type.is_file() {
                return Err("temporary scratch store contains a non-file entry".into());
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "temporary scratch record name is not UTF-8".to_owned())?;
            if name.starts_with(VFS_SCRATCH_TEMP_PREFIX) {
                fs::remove_file(entry.path()).map_err(|error| {
                    format!("stale temporary scratch staging file could not be removed: {error}")
                })?;
                removed_staging = true;
                continue;
            }
            if !name.ends_with(".scratch") {
                return Err("temporary scratch store contains an unknown file".into());
            }
            let record = Self::read_record(&entry.path())?;
            Self::validate_identity(&record.identity)?;
            if Self::record_name(&record.identity) != name {
                return Err("temporary scratch record identity does not match its filename".into());
            }
            records.push(record);
        }
        if removed_staging {
            Self::sync_directory(&self.directory)?;
        }
        records.sort_by(|left, right| {
            left.identity
                .logical_path
                .cmp(&right.identity.logical_path)
                .then_with(|| left.identity.name.cmp(&right.identity.name))
        });
        Ok(records)
    }

    fn remove(&self, identity: &TemporaryScratchIdentity) -> Result<(), String> {
        Self::validate_identity(identity)?;
        match fs::remove_file(self.record_path(identity)) {
            Ok(()) => Self::sync_directory(&self.directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "temporary scratch record could not be removed: {error}"
            )),
        }
    }

    /// Reserves one of this mount's unbound scratch slots for `identity`. A slot this mount
    /// already holds stays admitted; a new one past `MAX_UNBOUND_SCRATCH` is refused.
    fn reserve_unbound(&self, identity: &TemporaryScratchIdentity) -> bool {
        let name = Self::record_name(identity);
        let mut created = self
            .unbound_created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if created.contains(&name) {
            return true;
        }
        if created.len() >= MAX_UNBOUND_SCRATCH {
            return false;
        }
        created.insert(name);
        true
    }

    /// Releases the slot of a record this mount removed. Restored records were never reserved,
    /// so releasing them is a no-op.
    fn release_unbound(&self, identity: &TemporaryScratchIdentity) {
        let name = Self::record_name(identity);
        self.unbound_created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&name);
    }

    fn record_path(&self, identity: &TemporaryScratchIdentity) -> PathBuf {
        self.directory.join(Self::record_name(identity))
    }

    fn record_name(identity: &TemporaryScratchIdentity) -> String {
        let mut digest = Sha256::new();
        Self::update_digest(&mut digest, identity.logical_path.as_bytes());
        digest.update(identity.relation_id);
        Self::update_digest(&mut digest, &identity.key);
        Self::update_digest(&mut digest, identity.name.as_bytes());
        match &identity.blob_field_name {
            Some(name) => {
                digest.update([1]);
                Self::update_digest(&mut digest, name.as_bytes());
            }
            None => digest.update([0]),
        }
        let mut name = String::with_capacity(64 + ".scratch".len());
        for byte in digest.finalize() {
            let _ = write!(name, "{byte:02x}");
        }
        name.push_str(".scratch");
        name
    }

    fn update_digest(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    fn validate_identity(identity: &TemporaryScratchIdentity) -> Result<(), String> {
        if identity.logical_path.is_empty()
            || Path::new(&identity.logical_path).is_absolute()
            || identity.logical_path.contains('\0')
            || identity
                .logical_path
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err("temporary scratch row path is invalid".into());
        }
        if identity.name.is_empty()
            || identity.name == "."
            || identity.name == ".."
            || identity.name.contains('/')
            || identity.name.contains('\0')
        {
            return Err("temporary scratch name is invalid".into());
        }
        if identity
            .blob_field_name
            .as_deref()
            .is_some_and(|name| name.is_empty() || name.contains('\0'))
        {
            return Err("temporary scratch Blob field identity is invalid".into());
        }
        Ok(())
    }

    fn write_identity(file: &mut File, identity: &TemporaryScratchIdentity) -> Result<(), String> {
        Self::write_bytes(file, identity.logical_path.as_bytes())?;
        file.write_all(&identity.relation_id)
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_bytes(file, &identity.key)?;
        Self::write_bytes(file, identity.name.as_bytes())?;
        Self::write_optional(file, identity.blob_field_name.as_deref().map(str::as_bytes))
    }

    fn write_optional(file: &mut File, value: Option<&[u8]>) -> Result<(), String> {
        match value {
            Some(bytes) => {
                file.write_all(&[1]).map_err(|error| {
                    format!("temporary scratch record could not be written: {error}")
                })?;
                Self::write_bytes(file, bytes)
            }
            None => file
                .write_all(&[0])
                .map_err(|error| format!("temporary scratch record could not be written: {error}")),
        }
    }

    fn write_bytes(file: &mut File, bytes: &[u8]) -> Result<(), String> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| "temporary scratch record field is too large".to_owned())?;
        file.write_all(&length.to_le_bytes())
            .and_then(|()| file.write_all(bytes))
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))
    }

    fn read_record(path: &Path) -> Result<PersistedTemporaryScratch, String> {
        let mut file = File::open(path)
            .map_err(|error| format!("temporary scratch record could not be opened: {error}"))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)
            .map_err(|error| format!("temporary scratch record header is incomplete: {error}"))?;
        if &magic != VFS_SCRATCH_RECORD_MAGIC {
            return Err("temporary scratch record has an unsupported format".into());
        }
        let identity = Self::read_identity(&mut file)?;
        let baseline = Self::read_optional(&mut file)?;
        let bytes = Self::read_bytes(&mut file)?;
        let mut trailing = [0];
        if file
            .read(&mut trailing)
            .map_err(|error| format!("temporary scratch record could not be checked: {error}"))?
            != 0
        {
            return Err("temporary scratch record contains trailing data".into());
        }
        Ok(PersistedTemporaryScratch {
            identity,
            baseline,
            bytes,
        })
    }

    fn read_identity(file: &mut File) -> Result<TemporaryScratchIdentity, String> {
        let logical_path = Self::read_string(file)?;
        let mut relation_id = [0; 16];
        file.read_exact(&mut relation_id)
            .map_err(|error| format!("temporary scratch record identity is incomplete: {error}"))?;
        let key = Self::read_bytes(file)?;
        let name = Self::read_string(file)?;
        let blob_field_name = Self::read_optional(file)?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| {
                format!("temporary scratch Blob field identity is invalid: {error}")
            })?;
        Ok(TemporaryScratchIdentity {
            logical_path,
            relation_id,
            key,
            name,
            blob_field_name,
        })
    }

    fn read_string(file: &mut File) -> Result<String, String> {
        String::from_utf8(Self::read_bytes(file)?)
            .map_err(|error| format!("temporary scratch record string is invalid: {error}"))
    }

    fn read_optional(file: &mut File) -> Result<Option<Vec<u8>>, String> {
        let mut present = [0];
        file.read_exact(&mut present)
            .map_err(|error| format!("temporary scratch record field is incomplete: {error}"))?;
        match present[0] {
            0 => Ok(None),
            1 => Self::read_bytes(file).map(Some),
            _ => Err("temporary scratch record has an invalid field marker".into()),
        }
    }

    fn read_bytes(file: &mut File) -> Result<Vec<u8>, String> {
        let mut encoded_length = [0; 8];
        file.read_exact(&mut encoded_length).map_err(|error| {
            format!("temporary scratch record field length is incomplete: {error}")
        })?;
        let length = u64::from_le_bytes(encoded_length);
        if length
            > file
                .metadata()
                .map_err(|error| {
                    format!("temporary scratch record size could not be read: {error}")
                })?
                .len()
        {
            return Err("temporary scratch record field exceeds its file size".into());
        }
        let length = usize::try_from(length).map_err(|_| {
            "temporary scratch record field is too large for this platform".to_owned()
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            format!("temporary scratch record field could not be allocated: {error}")
        })?;
        bytes.resize(length, 0);
        file.read_exact(&mut bytes)
            .map_err(|error| format!("temporary scratch record field is incomplete: {error}"))?;
        Ok(bytes)
    }

    fn sync_directory(path: &Path) -> Result<(), String> {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!("temporary scratch directory could not be synchronized: {error}")
            })
    }
}

const VFS_RETAINED_DRAFT_DIRECTORY: &str = "vfs-retained-drafts-v1";
const VFS_RETAINED_DRAFT_HEADER: &str = "orna-vfs-retained-draft 1";
/// Recovery storage refuses new records past this count. Every record holds
/// one draft within the draft file bound, so the store stays bounded.
const VFS_RETAINED_DRAFT_LIMIT: usize = 256;
/// Editor drafts hold full candidate bytes, so they live in this private
/// subdirectory of the recovery store rather than the shared temp directory.
const VFS_EDITOR_DRAFT_DIRECTORY: &str = "editor-drafts";

/// Creates the explicit editor draft for `edit` (ORNA-VFS-024 recovery). The
/// file is created exclusively (O_EXCL) at mode 0600 under an unpredictable
/// name, inside a 0700 directory of the runtime recovery store. It is kept on
/// disk so a failed edit can be retained and reported by path.
fn create_editor_draft(runtime_root: &Path) -> Result<(File, PathBuf), String> {
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
fn ensure_private_directory(directory: &Path, label: &str) -> Result<(), String> {
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
fn count_editor_drafts(directory: &Path) -> Result<usize, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("editor draft directory could not be read: {error}"))?;
    let mut count = 0;
    for entry in entries {
        let entry = entry.map_err(|error| format!("editor draft entry could not be read: {error}"))?;
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
fn denied_attribute_change(
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    flags: Option<fuser::BsdFileFlags>,
) -> Option<Errno> {
    (mode.is_some() || uid.is_some() || gid.is_some() || flags.is_some()).then_some(Errno::EPERM)
}

/// One candidate kept in recovery storage.
struct RetainedDraft {
    id: String,
    record: PathBuf,
}

/// A candidate recovery storage could not keep, carrying the errno the refused
/// write, rename or removal replies (ORNA-VFS-009, EDIT-1 step 4). A full store
/// is EDQUOT; any other storage failure is EIO.
#[derive(Debug)]
struct RetentionError {
    errno: Errno,
    message: String,
}

impl RetentionError {
    fn io(message: String, error: &std::io::Error) -> Self {
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
fn read_candidate_range<E>(
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
fn reply_read_size(size: u32) -> u32 {
    size.min(u32::try_from(CANDIDATE_CHUNK_BYTES).unwrap_or(u32::MAX))
}

/// Where a candidate the mount did not accept reads its bytes from. Retention
/// streams a live draft or a truncation by range; only a stash whose retention
/// was refused holds materialised bytes.
#[derive(Clone)]
enum CandidateSource {
    /// A live draft, read through its spool by range.
    Draft(Arc<EditDraft<RowMapSnapshot>>),
    /// The newest writer draft of the file when one is open or was released
    /// unretained, else its accepted baseline; either is cut or zero-extended to `size`.
    Truncated { size: u64, latest: Option<Arc<EditDraft<RowMapSnapshot>>> },
    /// Bytes already held in memory for a candidate whose retention was refused.
    Stashed(Arc<[u8]>),
}

/// Reads up to `length` bytes at `offset` of the candidate that results from
/// cutting or zero-extending a base of `base_len` bytes to `size`. `read_base`
/// reads the base's bytes; growth is zero-filled. Copies only the requested window.
fn truncated_window(
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

/// Reads up to `length` bytes at `offset` from in-memory candidate bytes.
fn stashed_range(bytes: &[u8], offset: u64, length: usize) -> Vec<u8> {
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
    let end = start.saturating_add(length).min(bytes.len());
    bytes[start..end].to_vec()
}

/// Keeps a refused scratch candidate in recovery storage before its rename or
/// removal replies, so the bytes outlive the scratch name (ORNA-VFS-009).
fn retain_scratch_candidate(
    save: &RowSaveContext,
    path: &str,
    cause: &str,
    len: u64,
    read_chunk: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, RetentionError>,
) -> Result<(), RetentionError> {
    let retained = save.writer.retained_drafts.retain(path, cause, len, read_chunk)?;
    eprintln!(
        "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
        retained.id,
        retained.record.display()
    );
    Ok(())
}

/// Bounded recovery storage for candidates the mount did not accept
/// (`sys.vfs.draft_retained`, ORNA-VFS-009, ORNA-VFS-024). A record keeps the
/// exact candidate bytes under a content-derived draft ID with the row path and
/// the cause, so a refused or abandoned edit stays recoverable from local state
/// after its handle, scratch name or mount is gone.
struct VfsRetainedDraftStore {
    directory: PathBuf,
}

impl VfsRetainedDraftStore {
    fn new(runtime_root: &Path) -> Self {
        Self {
            directory: runtime_root.join(VFS_RETAINED_DRAFT_DIRECTORY),
        }
    }

    /// Retains `bytes` as a candidate for the projected `path`. Retaining the
    /// same candidate for the same path again keeps its first record and ID.
    /// Holds one chunk at a time: `read_chunk(offset, length)` supplies the
    /// candidate, which is digested and written in two streaming passes.
    fn retain(
        &self,
        path: &str,
        cause: &str,
        len: u64,
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
        let id = hex_bytes(&digest[..16]);
        let record = self.directory.join(format!("{id}.draft"));
        if record.is_file() {
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
        let mut staged = tempfile::Builder::new()
            .prefix(".retain-")
            .tempfile_in(&self.directory)
            .map_err(|error| {
                RetentionError::io(format!("retained draft could not be staged: {error}"), &error)
            })?;
        let header = format!(
            "{VFS_RETAINED_DRAFT_HEADER}\ndraft {id}\npath {path}\ncause {cause}\nbytes {len}\n\n"
        );
        let written = |error: std::io::Error| {
            RetentionError::io(format!("retained draft could not be written: {error}"), &error)
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
        VfsScratchStore::sync_directory(&self.directory)?;
        Ok(RetainedDraft { id, record })
    }

    /// Feeds a candidate of `len` bytes to `sink` one [`CANDIDATE_CHUNK_BYTES`]
    /// slice at a time, so no whole copy is made. Fails if the source ends early.
    fn stream_candidate(
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

struct MountWriter {
    runtime: tokio::runtime::Runtime,
    state: RuntimeState,
    repository: Repository,
    identities: ObjectIdentityManifest,
    project: Arc<orna_project_v1::LoadedProject>,
    runtime_commit: GitCommitRef,
    mount_snapshot: OnceLock<RuntimeTableActivationSnapshot>,
    cache: Arc<VfsRepositoryCache>,
    scratch_store: VfsScratchStore,
    retained_drafts: VfsRetainedDraftStore,
    owner_id: [u8; 16],
    row_baselines: Option<MountRowBaselines>,
}

impl MountWriter {
    fn open(
        repository: &Repository,
        commit: &GitCommitRef,
        row_baselines: Option<MountRowBaselines>,
    ) -> Result<Arc<Self>, String> {
        let (identity, initial_digest) = super::runtime_identity(repository)
            .map_err(|error| format!("repository runtime identity is unavailable: {error}"))?;
        let owner_id = super::invocation_owner_id();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("mounted editor runtime could not be created: {error}"))?;
        let state = runtime
            .block_on(RuntimeState::open(repository, identity, initial_digest))
            .map_err(|error| format!("mounted editor runtime could not be opened: {error:?}"))?;
        let identities = ObjectIdentityManifest::read_committed(repository, commit)
            .map_err(|error| format!("committed source identities could not be loaded: {error:?}"))?;
        let runtime_commit = repository
            .head()
            .map_err(|error| format!("mounted editor runtime source commit is unavailable: {error:?}"))?
            .unwrap_or_else(|| commit.clone());
        let project = ProjectLoader::default()
            .load_committed_snapshot(repository, commit)
            .map_err(|error| format!("committed source project could not be loaded: {error:?}"))?;
        let scratch_store = VfsScratchStore::open(repository.runtime_paths().root())?;
        Ok(Arc::new(Self {
            runtime,
            state,
            repository: repository.clone(),
            identities,
            project: Arc::new(project),
            runtime_commit,
            mount_snapshot: OnceLock::new(),
            cache: Arc::new(
                VfsRepositoryCache::in_runtime_root(repository.runtime_paths().root())
                    .map_err(|error| format!("draft spool unavailable: {error}"))?,
            ),
            scratch_store,
            retained_drafts: VfsRetainedDraftStore::new(repository.runtime_paths().root()),
            owner_id,
            row_baselines,
        }))
    }

    fn capture_mount_activation(&self, tables: &[RuntimeTableIdentity]) -> Result<(), String> {
        let snapshot = self
            .runtime
            .block_on(self.state.begin_admitted_table_activation(tables))
            .map_err(|error| format!("mounted editor row admission failed: {error:?}"))?;
        self.mount_snapshot
            .set(snapshot)
            .map_err(|_| "mounted editor runtime snapshot was already captured".to_owned())
    }

    fn cwd_generation(&self) -> Result<orna_repository_v1::CwdGeneration, String> {
        let capture = self
            .runtime
            .block_on(self.state.capture())
            .map_err(|error| format!("mounted editor CWD could not be captured: {error:?}"))?;
        let generation = u64::try_from(capture.generation())
            .map_err(|_| "mounted editor runtime generation is invalid".to_owned())?;
        self.repository
            .cwd_generation(RuntimeGeneration::new(generation))
            .map_err(|error| {
                format!("mounted editor repository generation is unavailable: {error:?}")
            })
    }

    fn row_baseline<'a>(
        &'a self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<&'a [u8]>, String> {
        let snapshot = self
            .mount_snapshot
            .get()
            .ok_or_else(|| "mounted editor runtime snapshot was not captured".to_owned())?;
        runtime_row_baseline_from_rows(snapshot.table_rows(), table, key)
            .ok_or_else(|| format!("runtime table {} was not admitted", table.table()))
    }

    /// The CWD row the runtime holds for `key` now, read as one point lookup
    /// rather than from the snapshot captured at mount time. A stale save uses
    /// it to show and save against the row another writer accepted.
    fn live_row(&self, table: &RuntimeTableIdentity, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        self.runtime
            .block_on(self.state.admitted_table_row(table, key))
            .map_err(|error| format!("mounted editor CWD row could not be read: {error:?}"))
    }

    fn candidate(
        &self,
        row: &RowSaveContext,
        coordinates: &VfsSaveCoordinates,
        bytes: &[u8],
        replacement_binding: Option<(String, OrpBlobBinding)>,
    ) -> Result<(VfsSaveCandidate, Vec<u8>), orna_runtime_v1::TableActivationError> {
        let source = std::str::from_utf8(bytes).map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let blob_fields = row
            .blob_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        validate_blob_descriptors(source, &blob_fields).map_err(|error| {
            eprintln!("mounted row document was refused: {error}");
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let omitted_blob_fields = blob_fields
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        let source = source_without_fields(source, &omitted_blob_fields).map_err(|error| {
            eprintln!("mounted row source projection failed: {error}");
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let mut replacement_binding = replacement_binding;
        let mut blob_bindings = Vec::with_capacity(blob_fields.len());
        if !blob_fields.is_empty() {
            let scope = row.source.graph.open_read_scope().map_err(|_| {
                orna_runtime_v1::TableActivationError::Runtime(
                    orna_runtime_v1::RuntimeError::InvalidTableMutation,
                )
            })?;
            for field in &blob_fields {
                let binding = if replacement_binding
                    .as_ref()
                    .is_some_and(|(name, _)| name == &field.name)
                {
                    replacement_binding
                        .take()
                        .map(|(_, binding)| binding)
                        .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                            orna_runtime_v1::RuntimeError::InvalidTableMutation,
                        ))?
                } else if let Some(bytes) = &field.replacement_bytes {
                    let capture = self
                        .repository
                        .capture_capability(*row.source.binding.relation_id())
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    let candidate = capture
                        .capture_blob_candidate(bytes.as_ref(), bytes.len() as u64)
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    capture
                        .accept_captured_blob(candidate, &field.media_type, field.suffix.as_deref())
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?
                } else {
                    let pin = row
                        .source
                        .graph
                        .protect_admitted_blob_reference(&field.reference, &scope)
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    row.source
                        .graph
                        .accept_protected_blob_pin_with_annotation(
                            pin,
                            &field.media_type,
                            field.suffix.as_deref(),
                        )
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?
                };
                blob_bindings.push((field.name.clone(), binding));
            }
        }
        if replacement_binding.is_some() {
            return Err(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ));
        }
        let mut mutation_id = [0_u8; 16];
        getrandom::fill(&mut mutation_id).map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let admitted = admit_source_row_with_owner_fields(
            &self.project,
            &row.logical_path,
            &source,
            &row.source.schema,
            &row.source.value_context,
            blob_bindings,
            Vec::new(),
            mutation_id,
        )
        .map_err(|error| {
            eprintln!("mounted row source admission failed: {error:?}");
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let (source_mutation, bindings, resolved_schema) = admitted.into_mutation_and_bindings();
        // Text primary keys use their UTF-8 bytes in admitted mutations;
        // other key kinds keep the canonical typed-key encoding.
        let mutation_key_matches = match &row.key {
            TypedKey::Text(key) => source_mutation.key() == key.as_bytes(),
            _ => source_mutation.key() == coordinates.key(),
        };
        if resolved_schema.table_object_id() != row.source.schema.table_object_id()
            || !mutation_key_matches
            || source_mutation.table() != coordinates.table().table()
            || source_mutation.table_object_id() != Some(coordinates.table().object_id())
        {
            return Err(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ));
        }
        // Source-row admission creates insert-only mutations; this durable
        // save already admitted the existing baseline, so write a replacement.
        let replacement_value =
            source_mutation
                .value()
                .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                    orna_runtime_v1::RuntimeError::InvalidTableMutation,
                ))?;
        let mut mutation = orna_runtime_v1::TableMutation::new(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            coordinates.key().to_vec(),
            Some(replacement_value.to_vec()),
        )
        .map_err(orna_runtime_v1::TableActivationError::Runtime)?
        .with_table_object_id(coordinates.table().object_id());
        let mut pins = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let (_, _, pin) = binding.into_parts();
            mutation = mutation
                .with_protected_content_pin(&pin)
                .map_err(orna_runtime_v1::TableActivationError::Runtime)?;
            pins.push(pin);
        }
        let content_transfers = pins
            .iter()
            .map(|pin| pin.transfer_record())
            .collect::<Vec<_>>();
        let replacement_bytes = mutation
            .value()
            .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ))?
            .to_vec();
        let replacement = VfsTableRowReplacement::from_admitted_mutation(
            coordinates.table().clone(),
            mutation,
            pins,
        )
        .map_err(orna_runtime_v1::TableActivationError::Runtime)?;
        let validator = transactional_loaded_project_candidate_validator(
            &self.project,
            &self.identities,
            coordinates.cwd_generation().clone(),
            Arc::clone(&row.table_object_ids),
            EvaluatorLimits::default(),
        )
        .map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let candidate = VfsSaveCandidate::new(replacement, validator)
            .with_protected_content_transfers(replacement_bytes.clone(), content_transfers);
        Ok((candidate, replacement_bytes))
    }
}

#[derive(Clone)]
struct BlobField {
    name: String,
    file_name: String,
    media_type: String,
    suffix: Option<String>,
    reference: AdmittedBlobReference,
    replacement_bytes: Option<Arc<[u8]>>,
}
struct RowSaveContext {
    writer: Arc<MountWriter>,
    source: Arc<TableSource>,
    key: TypedKey,
    target: Arc<ManagedFile<RowMapSnapshot>>,
    table: RuntimeTableIdentity,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    logical_path: String,
    expected_baseline_row: Mutex<Option<Vec<u8>>>,
    blob_fields: Mutex<Vec<BlobField>>,
}

/// Whether two save contexts address the same stored row, whichever projection
/// of it each one was built from.
fn same_row(left: &RowSaveContext, right: &RowSaveContext) -> bool {
    left.source.binding.relation_id() == right.source.binding.relation_id() && left.key == right.key
}

// Source-row mutations store text primary keys as UTF-8; other keys use their
// canonical typed-key encoding.
fn runtime_table_key_bytes(key: &TypedKey) -> Result<Vec<u8>, String> {
    match key {
        TypedKey::Text(key) => Ok(key.as_bytes().to_vec()),
        _ => key
            .canonical_bytes()
            .map_err(|error| format!("committed row key cannot be encoded: {error:?}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredFieldShape {
    field_id: [u8; 16],
    field_type: Raw,
}

#[derive(Clone, Debug)]
struct StoredFieldLayout {
    fields: Vec<StoredFieldShape>,
    indexes: HashMap<[u8; 16], usize>,
}

impl StoredFieldLayout {
    fn field_type(&self, field_id: &[u8; 16]) -> Option<&Raw> {
        self.indexes
            .get(field_id)
            .and_then(|index| self.fields.get(*index))
            .map(|field| &field.field_type)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum RowProjectionSource {
    RuntimeSnapshot,
    PinnedSnapshot,
}

fn runtime_row_for_exact_key<'a>(
    rows: &'a [(Vec<u8>, Vec<u8>)],
    key: &[u8],
) -> Option<&'a [u8]> {
    rows.binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
        .ok()
        .map(|index| rows[index].1.as_slice())
}

fn runtime_row_baseline_from_rows<'a>(
    rows: &'a RuntimeTableRows,
    table: &RuntimeTableIdentity,
    key: &[u8],
) -> Option<Option<&'a [u8]>> {
    Some(runtime_row_for_exact_key(rows.get(table.table())?, key))
}

fn runtime_row_matches_layout(
    runtime_fields: &Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: &[StoredFieldShape],
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> bool {
    let Raw::Array(values) = runtime_fields else {
        return false;
    };
    runtime_layout == pinned_layout
        && values.len() == pinned_layout.len()
        && pinned_layout
            .iter()
            .map(|field| field.field_id)
            .eq(row_field_ids)
}

/// Chooses the values `data.orna` projects for one row. A row the CWD holds is
/// what the workspace mount shows (ORNA-VFS-005, W08-08): when that row cannot
/// be decoded or follows another stored layout, the projection fails (EIO)
/// instead of silently showing the pinned HEAD row in its place.
fn select_runtime_row_fields(
    runtime_row: Option<&[u8]>,
    pinned_fields: Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: Option<&[StoredFieldShape]>,
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> Result<(Raw, RowProjectionSource), String> {
    let Some(runtime_row) = runtime_row else {
        return Ok((pinned_fields, RowProjectionSource::PinnedSnapshot));
    };
    // The CWD row is validated against the mounted snapshot's schema (ORNA-VFS-001).
    // A HEAD that no longer declares the table must not make the row unreadable.
    let runtime_layout = runtime_layout.unwrap_or(pinned_layout);
    let runtime_fields = decode_canonical_storage_value(runtime_row)
        .map_err(|error| format!("the CWD row cannot be decoded: {error:?}"))?;
    if !runtime_row_matches_layout(&runtime_fields, pinned_layout, runtime_layout, row_field_ids)
    {
        return Err(
            "the CWD row follows another stored field layout than the mounted schema".into(),
        );
    }
    Ok((runtime_fields, RowProjectionSource::RuntimeSnapshot))
}

/// Captures a fresh owner-snapshot allocation for one projected file image.
///
/// The runtime stamps one projection epoch per snapshot allocation, and every
/// accepted save advances the epoch. An image built over the table's shared
/// row-map `Arc` after a save would find an earlier stamp and be refused
/// (`ImageAlreadyScoped`), so rows first opened after a save became unreadable
/// (W15-02). The clone shares the same immutable row entries, so every image
/// stays pinned to the mounted snapshot.
fn row_image_pin(rows: &Arc<RowMapSnapshot>) -> SnapshotPin<RowMapSnapshot> {
    SnapshotPin::capture(Arc::new(RowMapSnapshot::clone(rows)))
}

/// Builds the save context of one projected row document. `refreshed_baseline`
/// is the durable row a refresh after a stale save projects; it replaces the
/// view's recorded baseline because the document now shows that row.
fn row_save_context(
    writer: &Arc<MountWriter>,
    source: &Arc<TableSource>,
    key: &TypedKey,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    document_bytes: &[u8],
    blob_fields: Vec<BlobField>,
    refreshed_baseline: Option<Vec<u8>>,
) -> Result<Arc<RowSaveContext>, String> {
    let declaration = source
        .binding
        .declaration_name()
        .ok_or_else(|| "committed table has no source declaration name".to_owned())?;
    // Row mutations address the table by its qualified identity, the same
    // key the admitted validator and the identity manifest use.
    let qualified_name = match source.binding.module_name().unwrap_or_default() {
        "" => declaration.to_owned(),
        module => format!("{module}.{declaration}"),
    };
    let table = RuntimeTableIdentity::new(
        &qualified_name,
        TableObjectId::new(*source.binding.relation_id()),
    )
    .map_err(|error| format!("committed table identity is invalid: {error:?}"))?;
    let key_bytes = runtime_table_key_bytes(key)?;
    let live_baseline_row = source
        .runtime_table
        .as_ref()
        .map(|runtime_table| writer.row_baseline(runtime_table, &key_bytes))
        .transpose()?
        .flatten()
        .map(|row| row.to_vec());
    // A row this view already accepted keeps that baseline across the processes
    // that save through it; the live durable row is only the first baseline. A
    // temporary is the exception: its baseline is re-read when it is created.
    let expected_baseline_row = match (
        refreshed_baseline,
        &writer.row_baselines,
        &source.runtime_table,
    ) {
        (Some(row), _, _) => Some(row),
        (None, Some(baselines), Some(table)) => baselines
            .accepted_row(table.table(), &key_bytes)
            .map_err(|error| format!("mounted row baseline is unavailable: {error}"))?
            .or(live_baseline_row),
        _ => live_baseline_row,
    };
    let mut components = Vec::new();
    flatten_key_components(key, &mut components)?;
    let Some((leaf, prefixes)) = components.split_last() else {
        return Err("committed row key has no path components".into());
    };
    let mut logical_path = declaration.to_owned();
    for prefix in prefixes {
        logical_path.push('/');
        logical_path.push_str(&key_component(prefix)?);
    }
    logical_path.push('/');
    logical_path.push_str(&key_component(leaf)?);
    logical_path.push_str(".orna");
    let image = Arc::new(VfsFileSnapshot::new(
        row_image_pin(&source.rows),
        Arc::<[u8]>::from(document_bytes.to_vec()),
    ));
    let target = writer
        .runtime
        .block_on(writer.cache.managed_file(image))
        .map_err(|error| format!("mounted editor row image could not be admitted: {error:?}"))?;
    Ok(Arc::new(RowSaveContext {
        writer: Arc::clone(writer),
        source: Arc::clone(source),
        key: key.clone(),
        target,
        table,
        logical_path,
        table_object_ids,
        expected_baseline_row: Mutex::new(expected_baseline_row),
        blob_fields: Mutex::new(blob_fields),
    }))
}
/// Why a document save did not commit. `NoCandidate` means the handle never
/// submitted bytes, so there is nothing to accept or discard. `Stale` means the
/// mounted baseline no longer matches the durable row and nothing was written.
/// `Rejected` means the submitted draft is malformed or fails semantic
/// admission (profiles/vfs-1 EINVAL). `Stale`, `Rejected` and `Failed` all mean
/// a save was attempted and did not commit, so the draft stays unresolved.
#[derive(Debug)]
enum DocumentCommitError {
    NoCandidate,
    Stale,
    Rejected(String),
    Failed(String),
    /// The save did not commit and recovery storage could not keep its candidate;
    /// `errno` is the retention errno (ORNA-VFS-009, EDIT-1 step 4).
    Unretained { errno: Errno, cause: &'static str },
}

impl DocumentCommitError {
    /// Errno for a save that did not commit (profiles/vfs-1 errno table):
    /// EINVAL for malformed drafts, EAGAIN for stale CAS, EIO for backing I/O.
    fn errno(&self) -> Errno {
        match self {
            Self::Rejected(_) => Errno::EINVAL,
            Self::Unretained { errno, .. } => *errno,
            Self::Stale => Errno::EAGAIN,
            Self::NoCandidate | Self::Failed(_) => Errno::EIO,
        }
    }

    /// The cause recorded with a candidate that did not commit: the profile's
    /// `sys.vfs.*` code where one exists (ORNA-VFS-009).
    fn cause(&self) -> &'static str {
        match self {
            Self::Stale => "sys.vfs.stale_edit",
            Self::Rejected(_) => "sys.vfs.invalid_document",
            Self::Unretained { cause, .. } => *cause,
            Self::NoCandidate | Self::Failed(_) => "backing I/O failure",
        }
    }
}

/// Cause recorded for a dirty document released without an accepted save: the
/// refusal already recorded for it (ORNA-VFS-009), else `unsynced`.
fn release_retention_cause(refusal: Option<&DraftRefusal>) -> &'static str {
    refusal.map_or("unsynced", |refusal| refusal.cause)
}

/// Classifies a failed durable save. Semantic admission and malformed-draft
/// refusals are `Rejected`; CAS staleness is `Stale`; everything else is
/// backing I/O `Failed`.
fn save_failure(
    context: &str,
    error: VfsSaveError<orna_runtime_v1::TableActivationError>,
) -> DocumentCommitError {
    match error {
        VfsSaveError::Activation(orna_runtime_v1::TableActivationError::ValidationFailed(
            diagnostic,
        )) => DocumentCommitError::Rejected(format!("{context}: {diagnostic:?}")),
        VfsSaveError::Activation(orna_runtime_v1::TableActivationError::Runtime(
            orna_runtime_v1::RuntimeError::InvalidTableMutation,
        )) => DocumentCommitError::Rejected(format!("{context}: malformed draft")),
        VfsSaveError::Rejected { diagnostic, .. } => {
            DocumentCommitError::Rejected(format!("{context}: {diagnostic:?}"))
        }
        VfsSaveError::Stale { .. } => DocumentCommitError::Stale,
        VfsSaveError::NoCandidate => DocumentCommitError::NoCandidate,
        error => DocumentCommitError::Failed(format!("{context}: {error:?}")),
    }
}

impl From<String> for DocumentCommitError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for DocumentCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCandidate => formatter.write_str("mounted row save failed: NoCandidate"),
            Self::Stale => {
                formatter.write_str("mounted save is stale against its captured baseline")
            }
            Self::Unretained { cause, .. } => formatter.write_str(cause),
            Self::Rejected(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl RowSaveContext {
    fn durable_save(&self) -> Result<VfsDurableSave, String> {
        let expected_baseline_row = self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.durable_save_with_baseline(expected_baseline_row)
    }

    fn durable_save_with_baseline(
        &self,
        expected_baseline_row: Option<Vec<u8>>,
    ) -> Result<VfsDurableSave, String> {
        let cwd_generation = self.writer.cwd_generation()?;
        let key = runtime_table_key_bytes(&self.key)?;
        Ok(VfsDurableSave::new(
            self.writer.owner_id,
            cwd_generation,
            self.table.clone(),
            key,
        )
        .with_expected_baseline_row(expected_baseline_row))
    }

    fn record_accepted_row(&self, row: Option<Vec<u8>>) {
        let Some(row) = row else {
            return;
        };
        if let Some(baselines) = &self.writer.row_baselines {
            let recorded = runtime_table_key_bytes(&self.key).and_then(|key| {
                baselines
                    .record_accepted_row(self.table.table(), &key, &row)
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = recorded {
                eprintln!("mounted row baseline could not be recorded: {error}");
            }
        }
        *self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row);
    }

    /// The baseline a new temporary captures (41-vfs.md Temporary-save
    /// replacement, ORNA-VFS-011): the row's current durable image, so a temp
    /// created after another writer's commit renames over that row. A row
    /// another writer deleted keeps the recorded baseline, which refuses it.
    fn temporary_baseline(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let recorded = self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(table) = self.source.runtime_table.as_ref() else {
            return Ok(recorded);
        };
        Ok(self.writer.live_row(table, key)?.or(recorded))
    }

    fn temporary_target(
        &self,
        blob_field: Option<&BlobField>,
    ) -> Result<Arc<ManagedFile<RowMapSnapshot>>, String> {
        if blob_field.is_none() {
            return Ok(Arc::clone(&self.target));
        }
        let image = Arc::new(VfsFileSnapshot::new(
            row_image_pin(&self.source.rows),
            Arc::<[u8]>::from([]),
        ));
        self.writer
            .runtime
            .block_on(self.writer.cache.managed_file(image))
            .map_err(|error| {
                format!("mounted replacement scratch target could not be admitted: {error:?}")
            })
    }

    /// Re-admits an unbound scratch's candidate under the Blob `blob_field` target, the same
    /// target a Blob-bound create uses, so the rename commits through that target.
    fn retarget_unbound_scratch(
        &self,
        scratch: &TemporarySave<RowMapSnapshot>,
        blob_field: &BlobField,
    ) -> Result<(Arc<ManagedFile<RowMapSnapshot>>, Arc<TemporarySave<RowMapSnapshot>>), String>
    {
        let target = self.temporary_target(Some(blob_field))?;
        let retargeted = self
            .writer
            .runtime
            .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES));
        let len = self.writer.runtime.block_on(scratch.candidate_len());
        if len > 0 {
            let length = usize::try_from(len)
                .map_err(|_| "scratch candidate exceeds addressable memory".to_owned())?;
            let bytes = self
                .writer
                .runtime
                .block_on(scratch.read_range(0, length))
                .map_err(|error| format!("scratch candidate could not be read: {error:?}"))?;
            self.writer
                .runtime
                .block_on(retargeted.write_at(0, &bytes))
                .map_err(|error| format!("scratch candidate could not be moved: {error:?}"))?;
        }
        Ok((target, Arc::new(retargeted)))
    }

    fn fsync_temporary(
        &self,
        scratch: &TemporarySave<RowMapSnapshot>,
        identity: &TemporaryScratchIdentity,
        baseline: Option<&[u8]>,
    ) -> Result<(), String> {
        let store = &self.writer.scratch_store;
        self.writer
            .runtime
            .block_on(scratch.fsync_scratch_with(|_, bytes| async move {
                store.persist(identity, baseline, bytes.as_ref())
            }))
            .map(|_| ())
            .map_err(|failure| match failure {
                FsyncFailure::Backing(error) => candidate_unreadable(error),
                FsyncFailure::Activation(message) => message,
            })
    }

    fn commit_document(
        self: &Arc<Self>,
        draft: &EditDraft<RowMapSnapshot>,
        record_baseline: bool,
    ) -> Result<(), DocumentCommitError> {
        let save = self.durable_save().map_err(DocumentCommitError::Failed)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_document_save(
                draft,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let (candidate, row_bytes) =
                        row_context
                            .writer
                            .candidate(&row_context, &coordinates, &bytes, None)?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(accepted_row);
                }
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => Err(DocumentCommitError::Rejected(
                "mounted row document was rejected by semantic admission".into(),
            )),
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(save_failure("mounted row save failed", error)),
        }
    }

    fn commit_blob_temporary(
        self: &Arc<Self>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &TemporarySave<RowMapSnapshot>,
        field: &BlobField,
        expected_baseline_row: Option<Vec<u8>>,
        record_baseline: bool,
    ) -> Result<Option<Vec<u8>>, DocumentCommitError> {
        let replacement_bytes = self
            .writer
            .runtime
            .block_on(scratch.candidate_bytes())
            .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
        let length = u64::try_from(replacement_bytes.len())
            .map_err(|_| "replacement Blob length is invalid".to_owned())?;
        let digest: [u8; 32] = Sha256::digest(&replacement_bytes).into();
        let expected_identity = ContentIdentity::new(length, digest)
            .map_err(|error| format!("replacement Blob identity is invalid: {error:?}"))?;
        let capture = self
            .writer
            .repository
            .capture_capability(*self.source.binding.relation_id())
            .map_err(|error| format!("replacement Blob capture is unavailable: {error:?}"))?;
        let document = self.writer.runtime.block_on(self.target.open_read());
        let document_length = usize::try_from(document.len())
            .map_err(|_| "mounted row document length is invalid".to_owned())?;
        let document_bytes: Arc<[u8]> = Arc::from(document.read_at(0, document_length));
        let save = self.durable_save_with_baseline(expected_baseline_row)?;
        let row_context = Arc::clone(self);
        let field_name = field.name.clone();
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.writer.cache.commit_durable_blob_rename(
                target,
                scratch,
                &self.writer.state,
                save,
                &capture,
                expected_identity,
                &field.media_type,
                field.suffix.as_deref(),
                move |coordinates, _blob_bytes, binding| async move {
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &document_bytes,
                        Some((field_name, binding)),
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                if let Some(field) = self
                    .blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter_mut()
                    .find(|candidate| candidate.name == field.name)
                {
                    field.replacement_bytes = Some(replacement_bytes);
                } else {
                    return Err(DocumentCommitError::Failed(
                        "replaced Blob field disappeared from its row".into(),
                    ));
                }
                let row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(row.clone());
                }
                Ok(row)
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => Err(DocumentCommitError::Rejected(
                "mounted Blob replacement was rejected by semantic admission".into(),
            )),
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(save_failure("mounted Blob replacement failed", error)),
        }
    }

    fn commit_temporary(
        self: &Arc<Self>,
        scratch: &TemporarySave<RowMapSnapshot>,
        expected_baseline_row: Option<Vec<u8>>,
        record_baseline: bool,
    ) -> Result<(), DocumentCommitError> {
        let save = self.durable_save_with_baseline(expected_baseline_row)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_temporary_rename(
                scratch,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let (candidate, row_bytes) =
                        row_context
                            .writer
                            .candidate(&row_context, &coordinates, &bytes, None)?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(accepted_row);
                }
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => Err(DocumentCommitError::Rejected(
                "temporary mounted row document was rejected by semantic admission".into(),
            )),
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(save_failure("temporary mounted row save failed", error)),
        }
    }
}
const ROOT_INODE: u64 = 1;
const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
const ENTRY_TTL: Duration = Duration::ZERO;
// Private editor drafts bypass the shared kernel page cache. Writeback cache is
// intentionally not requested: it can mix a later database generation into an
// already-open draft.
const EDITABLE_OPEN_FLAGS: fuser::FopenFlags = fuser::FopenFlags::FOPEN_DIRECT_IO;
const EDITOR_INIT_CAPABILITIES: fuser::InitFlags = fuser::InitFlags::FUSE_ATOMIC_O_TRUNC;
pub(super) fn spawn_read_only(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
) -> Result<fuser::BackgroundSession, String> {
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context, None)?;
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(source),
        MountOption::RO,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
    ];
    fuser::spawn_mount(filesystem, mountpoint, &config)
        .map_err(|error| format!("FUSE mount could not be created: {error}"))
}
/// The editor mount omits `AllowOther`, so the kernel admits only the mounting
/// uid. The owner's control handler therefore does not check the caller uid
/// (`_request` is unused): same-user access is the access boundary.
pub(super) fn spawn_editor(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
    row_baselines: MountRowBaselines,
) -> Result<fuser::BackgroundSession, String> {
    let writer = MountWriter::open(repository, commit, Some(row_baselines))?;
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context, Some(writer))?;
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(source),
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
    ];
    fuser::spawn_mount(filesystem, mountpoint, &config)
        .map_err(|error| format!("FUSE editor mount could not be created: {error}"))
}

/// Why an explicit edit was not accepted. `Stale` means the mounted baseline no
/// longer matches the durable row: nothing was written and the draft is retained.
#[derive(Debug)]
pub(super) enum EditError {
    Stale(String),
    Failed(String),
}

impl From<String> for EditError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<&str> for EditError {
    fn from(error: &str) -> Self {
        Self::Failed(error.to_owned())
    }
}

/// An explicit edit prepared against a live row tree: `initial_bytes` seeds the
/// editor draft, and `commit` admits the editor's candidate exactly once.
pub(super) struct PreparedEdit {
    initial_bytes: Vec<u8>,
    commit: Box<dyn FnOnce(&[u8], &Path, bool) -> Result<(), EditError>>,
}

impl PreparedEdit {
    /// Admits `candidate` for the draft at `draft_path`; the draft is removed once accepted.
    /// `record_baseline` false leaves the mounted captured baseline unchanged (ORNA-VFS-015).
    fn commit(self, candidate: &[u8], draft_path: &Path, record_baseline: bool) -> Result<(), EditError> {
        (self.commit)(candidate, draft_path, record_baseline)
    }
}

/// Writes `initial` to a new explicit editor draft, runs `$EDITOR` on it and
/// returns the draft path. The draft is retained on every failure.
pub(super) fn run_editor_on_draft(runtime_root: &Path, initial: &[u8]) -> Result<PathBuf, EditError> {
    let (mut file, draft_path) = create_editor_draft(runtime_root)
        .map_err(|error| format!("explicit edit draft could not be created: {error}"))?;
    use std::io::Write as _;
    file.write_all(initial)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            format!(
                "explicit edit draft could not be initialized: {error}; retained at {}",
                draft_path.display()
            )
        })?;
    drop(file);
    let Some(editor) = std::env::var_os("EDITOR") else {
        return Err(format!(
            "EDITOR is not set; initial draft retained at {}",
            draft_path.display()
        )
        .into());
    };
    let status = std::process::Command::new(editor)
        .arg(&draft_path)
        .status()
        .map_err(|error| {
            format!(
                "editor could not be started: {error}; draft retained at {}",
                draft_path.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "editor exited unsuccessfully; draft retained at {}",
            draft_path.display()
        )
        .into());
    }
    Ok(draft_path)
}

fn retained_edit_error(detail: &str, draft_path: &Path) -> EditError {
    EditError::Failed(format!("{detail}; draft retained at {}", draft_path.display()))
}

/// Maps a durable admission result to the explicit edit outcome. An accepted
/// edit removes its draft; any refusal retains it.
fn admit_edit_result(
    admission: Result<(), DocumentCommitError>,
    draft_path: &Path,
) -> Result<(), EditError> {
    match admission {
        Ok(()) => {
            // The commit is durable, so the owner reports acceptance even when the
            // explicit draft cannot be removed (ORNA-ADMIN-003). The leftover draft
            // is named in the warning for the operator to remove.
            if let Err(error) = std::fs::remove_file(draft_path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "warning: edit was accepted but its explicit draft could not be removed: {error}; remove {} by hand",
                        draft_path.display()
                    );
                }
            }
            Ok(())
        }
        Err(DocumentCommitError::Stale) => Err(EditError::Stale(format!(
            "the mounted baseline is stale against the durable row; nothing was written; draft retained at {}",
            draft_path.display()
        ))),
        Err(error) => Err(EditError::Failed(format!(
            "edit candidate was rejected: {error}; draft retained at {}",
            draft_path.display()
        ))),
    }
}

impl WorkspaceFilesystem {
    /// Resolves `relative` in the live row tree to its editable target and
    /// prepares the admission step. No draft is created on refusal.
    fn prepare_edit(&self, relative: &Path, grouped_row: bool) -> Result<PreparedEdit, EditError> {
        let writer = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .clone()
            .ok_or_else(|| "explicit edit requires an editor mount".to_owned())?;
        let mut inode = ROOT_INODE;
        {
            let mut builder = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for component in relative.components() {
                let std::path::Component::Normal(name) = component else {
                    return Err("edit path contains an unsupported component".into());
                };
                inode = builder
                    .lookup_child(inode, name)?
                    .ok_or_else(|| format!("edit path does not exist: {}", relative.display()))?;
            }
        }
        let (save, initial_bytes, blob_edit) = {
            let mut builder = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let node = builder
                .nodes
                .get(&inode)
                .ok_or_else(|| "edit target disappeared".to_owned())?;
            match &node.kind {
                NodeKind::Document { save, bytes } => (Arc::clone(save), bytes.clone(), None),
                NodeKind::Blob { graph, reference, .. } => {
                    let parent = node.parent;
                    let graph = Arc::clone(graph);
                    let reference = reference.clone();
                    let document = builder
                        .lookup_child(parent, OsStr::new("data.orna"))?
                        .ok_or_else(|| "Blob row has no data.orna document".to_owned())?;
                    let save = match &builder
                        .nodes
                        .get(&document)
                        .ok_or_else(|| "Blob row document disappeared".to_owned())?
                        .kind
                    {
                        NodeKind::Document { save, .. } => Arc::clone(save),
                        _ => return Err("Blob row document is not editable".into()),
                    };
                    let file_name = relative
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let field = save
                        .blob_fields
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .find(|field| field.file_name == file_name)
                        .cloned()
                        .ok_or_else(|| "Blob edit target is not an admitted row field".to_owned())?;
                    let initial = read_blob_bytes(&graph, &reference)?;
                    let expected = save
                        .expected_baseline_row
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let image = Arc::new(VfsFileSnapshot::new(
                        row_image_pin(&save.source.rows),
                        Arc::<[u8]>::from([]),
                    ));
                    let target = writer
                        .runtime
                        .block_on(writer.cache.managed_file(image))
                        .map_err(|error| format!("Blob edit target could not be opened: {error:?}"))?;
                    let scratch = Arc::new(
                        writer
                            .runtime
                            .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
                    );
                    (save, initial, Some((target, scratch, field, expected)))
                }
                _ => {
                    return Err(if grouped_row {
                        "edit --row requires a projected row document or Blob path".into()
                    } else {
                        "edit target is not a managed row document or Blob".into()
                    });
                }
            }
        };
        let commit: Box<dyn FnOnce(&[u8], &Path, bool) -> Result<(), EditError>> = match blob_edit {
            Some((target, scratch, field, expected)) => Box::new(move |candidate, draft_path, record_baseline| {
                writer
                    .runtime
                    .block_on(async {
                        scratch.truncate(0).await.map_err(|error| {
                            format!("Blob candidate exceeds draft limits: {error:?}")
                        })?;
                        scratch.write_at(0, candidate).await.map_err(|error| {
                            format!("Blob candidate exceeds draft limits: {error:?}")
                        })
                    })
                    .map_err(|error| retained_edit_error(&error, draft_path))?;
                admit_edit_result(
                    save.commit_blob_temporary(&target, &scratch, &field, expected, record_baseline)
                        .map(|_| ()),
                    draft_path,
                )
            }),
            None => {
                let draft = Arc::new(
                    writer
                        .runtime
                        .block_on(save.target.open_draft(DRAFT_FILE_BOUND_BYTES)),
                );
                Box::new(move |candidate, draft_path, record_baseline| {
                    writer
                        .runtime
                        .block_on(async {
                            draft.truncate(0).await.map_err(|error| {
                                format!("edited candidate exceeds draft limits: {error:?}")
                            })?;
                            draft.write_at(0, candidate).await.map_err(|error| {
                                format!("edited candidate exceeds draft limits: {error:?}")
                            })
                        })
                        .map_err(|error| retained_edit_error(&error, draft_path))?;
                    admit_edit_result(save.commit_document(&draft, record_baseline), draft_path)
                })
            }
        };
        Ok(PreparedEdit {
            initial_bytes,
            commit,
        })
    }

    /// Commits an explicit edit requested through the `user.orna.edit-commit`
    /// control (ORNA-VFS-015). `request` is `mode NUL path NUL draft`; the draft
    /// must be a regular editor draft in this repository's recovery store.
    fn commit_requested_edit(&self, request: &[u8]) -> Result<(), EditError> {
        use std::os::unix::ffi::OsStrExt as _;
        let fields: Vec<&[u8]> = request.split(|byte| *byte == 0).collect();
        let [mode, path, draft_bytes] = fields.as_slice() else {
            return Err("edit request is malformed".into());
        };
        let grouped_row = match *mode {
            b"0" => false,
            b"1" => true,
            _ => return Err("edit request names no edit mode".into()),
        };
        let writer = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .clone()
            .ok_or_else(|| "explicit edit requires an editor mount".to_owned())?;
        let drafts = writer
            .repository
            .runtime_paths()
            .root()
            .join(VFS_RETAINED_DRAFT_DIRECTORY)
            .join(VFS_EDITOR_DRAFT_DIRECTORY);
        let draft = Path::new(OsStr::from_bytes(draft_bytes));
        let in_store = draft.parent() == Some(drafts.as_path())
            && draft
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("orna-edit-") && name.ends_with(".draft"));
        if !in_store {
            return Err("edit draft is outside the editor draft store".into());
        }
        let candidate = read_regular_draft(draft)?;
        // ORNA-VFS-015: `edit --row` is this mount's own grouped row save, so the
        // mount records the row it accepted and its next save compares against it
        // (EDIT-1: explicit edits CAS their own read-to-save baseline). A plain
        // `edit PATH` owner-routed by another writer leaves the mounted baseline
        // unchanged, so the mount's next save is stale against that writer's row.
        self.prepare_edit(Path::new(OsStr::from_bytes(path)), grouped_row)?
            .commit(&candidate, draft, grouped_row)
    }
}

/// Reads an editor draft through one descriptor opened without following a
/// symbolic link, so the regular-file check and the read name the same inode.
/// `O_NONBLOCK` keeps a FIFO from stalling the open before the check rejects it.
fn read_regular_draft(draft: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let descriptor = rustix::fs::open(
        draft,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| format!("edit draft could not be opened: {error}"))?;
    let stat = rustix::fs::fstat(&descriptor)
        .map_err(|error| format!("edit draft could not be inspected: {error}"))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err("edit draft is not a regular file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::from(descriptor)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("edited candidate could not be read: {error}"))?;
    Ok(bytes)
}

pub(super) fn edit_path(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    path: &str,
    grouped_row: bool,
    row_baselines: Option<MountRowBaselines>,
) -> Result<(), EditError> {
    let writer = MountWriter::open(repository, commit, row_baselines)?;
    let filesystem =
        WorkspaceFilesystem::from_snapshot(repository, commit, context, Some(writer))?;
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err("edit path must be relative to the projected row tree".into());
    }
    let prepared = filesystem.prepare_edit(relative, grouped_row)?;
    let draft_path = run_editor_on_draft(repository.runtime_paths().root(), &prepared.initial_bytes)?;
    let candidate = read_regular_draft(&draft_path)
        .map_err(|error| retained_edit_error(&error, &draft_path))?;
    prepared.commit(&candidate, &draft_path, grouped_row)
}
#[derive(Debug)]
pub(super) enum UnmountPreparationError {
    UnresolvedDrafts,
    ReadOnlyMount,
    Disconnected,
    Operation(String),
}

pub(super) fn begin_unmount(mountpoint: &Path) -> Result<(), UnmountPreparationError> {
    rustix::fs::setxattr(
        mountpoint,
        "user.orna.unmount-quiesce",
        b"1",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::BUSY {
            UnmountPreparationError::UnresolvedDrafts
        } else if error == rustix::io::Errno::ROFS {
            UnmountPreparationError::ReadOnlyMount
        } else if error == rustix::io::Errno::NOTCONN {
            UnmountPreparationError::Disconnected
        } else {
            UnmountPreparationError::Operation(format!("could not quiesce Orna mount: {error}"))
        }
    })
}

pub(super) fn cancel_unmount(mountpoint: &Path) -> Result<(), String> {
    rustix::fs::setxattr(
        mountpoint,
        "user.orna.unmount-quiesce",
        b"0",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| format!("could not resume Orna mount after failed unmount: {error}"))
}
pub(super) fn has_unresolved_drafts(mountpoint: &Path) -> Result<bool, String> {
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(mountpoint, "user.orna.unresolved-drafts", &mut value)
    .map_err(|error| format!("could not read Orna mount draft state: {error}"))?;
    if length != 1 {
        return Err("Orna mount returned an invalid draft-state length".to_owned());
    }
    match value[0] {
        b'0' => Ok(false),
        b'1' => Ok(true),
        _ => Err("Orna mount returned invalid draft state".to_owned()),
    }
}

/// One unresolved draft of a mount, as the draft-list xattrs report it.
struct UnresolvedDraft {
    path: String,
    /// The `sys.*` Orna cause, or empty when the profile names none.
    code: &'static str,
    /// The retained draft id, or empty when no candidate was kept.
    draft: String,
    detail: String,
}

/// Reads one draft-list xattr of a live mount as UTF-8 text. The value can grow
/// between the size probe and the read, which fails with ERANGE; the read then
/// re-probes and retries, up to `READ_ATTEMPTS` times.
fn read_draft_xattr(mountpoint: &Path, name: &str) -> Result<String, rustix::io::Errno> {
    const READ_ATTEMPTS: usize = 5;
    for _ in 0..READ_ATTEMPTS {
        let length = rustix::fs::getxattr(mountpoint, name, &mut [])?;
        let mut value = vec![0_u8; length];
        match rustix::fs::getxattr(mountpoint, name, &mut value) {
            Ok(length) => {
                value.truncate(length);
                return String::from_utf8(value).map_err(|_| rustix::io::Errno::IO);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(error) => return Err(error),
        }
    }
    Err(rustix::io::Errno::RANGE)
}

/// The unresolved drafts of a live mount, one `path<TAB>detail` line each, for
/// an unmount refusal that names what blocks it (W15-03).
pub(super) fn unresolved_draft_list(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, "user.orna.unresolved-draft-list")
}

/// The unresolved drafts of a live mount, one `path<TAB>code<TAB>draft<TAB>detail`
/// line each, for `mount status`.
pub(super) fn unresolved_draft_fields(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, "user.orna.unresolved-draft-fields")
}

/// Why stop-time draft retention did not finish (ORNA-VFS-024).
pub(super) enum DraftRetentionStop {
    /// Recovery storage could not keep a draft; the mount stays up and the stop retries.
    Retention(String),
    /// The mount could not be quiesced for retention; the stop unmounts and reports it.
    Mount(String),
}

/// Stops a live mount from accepting edits before a stop-time unmount. A mount
/// that still holds unresolved drafts is asked to retain every one of them
/// (recovery storage, or the scratch store for scratch files) and to report
/// them, so stopping never silently discards a draft (ORNA-VFS-024). Returns
/// whether any draft was unresolved.
pub(super) fn retain_drafts_for_shutdown(mountpoint: &Path) -> Result<bool, DraftRetentionStop> {
    match begin_unmount(mountpoint) {
        Ok(()) | Err(UnmountPreparationError::ReadOnlyMount | UnmountPreparationError::Disconnected) => {
            Ok(false)
        }
        Err(UnmountPreparationError::UnresolvedDrafts) => rustix::fs::setxattr(
            mountpoint,
            "user.orna.retain-drafts",
            b"1",
            rustix::fs::XattrFlags::empty(),
        )
        .map(|()| true)
        .map_err(|error| {
            DraftRetentionStop::Retention(format!("could not retain Orna mount drafts: {error}"))
        }),
        Err(UnmountPreparationError::Operation(error)) => Err(DraftRetentionStop::Mount(error)),
    }
}

/// Unmounts a FUSE mount whose daemon is gone (its kernel mount answers
/// ENOTCONN). A plain unmount is tried first; a dead mount that a process
/// still references is detached lazily so the mountpoint can be reused.
pub(super) fn unmount_dead_mountpoint(mountpoint: &Path) -> Result<(), String> {
    let Err(first) = unmount_mountpoint(mountpoint) else {
        return Ok(());
    };
    let output = Command::new(fusermount_program())
        .args(["-u", "-z", "--"])
        .arg(mountpoint)
        .output()
        .map_err(|error| format!("{first}; lazy FUSE unmount could not be started: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{first}; lazy FUSE unmount failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(super) fn unmount_mountpoint(mountpoint: &Path) -> Result<(), String> {
    let output = Command::new(fusermount_program())
        .args(["-u", "--"])
        .arg(mountpoint)
        .output()
        .map_err(|error| format!("FUSE unmount helper could not be started: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(())
}

fn fusermount_program() -> OsString {
    if let Some(program) = std::env::var_os("FUSERMOUNT_PATH") {
        return program;
    }
    for program in [
        "fusermount3",
        "fusermount",
        "mount_fusefs",
        "/sbin/fusermount3",
        "/sbin/fusermount",
        "/sbin/mount_fusefs",
        "/bin/fusermount3",
        "/bin/fusermount",
    ] {
        if Command::new(program).arg("-h").output().is_ok() {
            return OsString::from(program);
        }
    }
    OsString::from("fusermount3")
}

#[derive(Clone)]
enum OpenFile {
    Document(Arc<RowSaveContext>, Arc<EditDraft<RowMapSnapshot>>),
    Temporary(
        Arc<RowSaveContext>,
        Arc<ManagedFile<RowMapSnapshot>>,
        Arc<TemporarySave<RowMapSnapshot>>,
        Option<BlobField>,
    ),
}
/// Why a submitted candidate was not accepted, for the unmount listing and
/// the stop-time report. `draft` is its recovery record once retained. While
/// recovery storage refuses the candidate, `unretained` holds its save context
/// and bytes so a later retention attempt can still write them.
struct DraftRefusal {
    cause: &'static str,
    draft: Option<String>,
    /// The handle whose candidate this refusal describes; a write by that handle clears it.
    handle: Option<u64>,
    /// The released or refused writer's draft: the base a path truncate that
    /// arrives before retention cuts (ORNA-VFS-009).
    candidate: Option<Arc<EditDraft<RowMapSnapshot>>>,
    unretained: Option<(Arc<RowSaveContext>, Arc<[u8]>)>,
}

/// A replacement candidate whose rename was refused. `kept` records that its
/// candidate reached recovery storage, so the draft list reports it as retained.
#[derive(Clone, Copy)]
struct ScratchRefusal {
    cause: &'static str,
    kept: bool,
}

/// Unsynced Document drafts are retained on release with cause `unsynced`; a
/// Document whose fsync was rejected stays unresolved until a later sync of the
/// same file is accepted.
/// Replacement scratch drafts remain unresolved until their rename is accepted
/// or the scratch is removed.
#[derive(Default)]
struct DraftState {
    pending_truncates: HashMap<u64, u64>,
    unresolved_handles: HashMap<u64, u64>,
    retained_scratch: HashSet<u64>,
    rejected_documents: HashMap<u64, DraftRefusal>,
    /// Scratch files whose rename was refused, with the cause and whether the candidate was kept.
    rejected_scratch: HashMap<u64, ScratchRefusal>,
    quiescing: bool,
    /// Handles whose latest write has not been accepted (per-handle dirty bit).
    dirty_handles: HashSet<u64>,
    /// Write-open Blob handles, keyed by handle number.
    blob_handles: HashMap<u64, BlobHandle>,
    /// Pid that opened each writable handle; a FLUSH from another process is not the writer's close.
    writer_owners: HashMap<u64, u32>,
}

impl DraftState {
    fn has_unresolved_drafts(&self) -> bool {
        !self.pending_truncates.is_empty()
            || !self.unresolved_handles.is_empty()
            || !self.retained_scratch.is_empty()
            || !self.rejected_documents.is_empty()
            || !self.rejected_scratch.is_empty()
    }

    /// A write or truncate by `handle` makes its candidate newer than the refused
    /// one, so the refusal no longer names that handle. An accepted save must not
    /// end the unresolved state of that newer candidate (ORNA-VFS-008).
    fn candidate_written(&mut self, handle: u64, inode: u64) {
        if let Some(refusal) = self.rejected_documents.get_mut(&inode) {
            if refusal.handle == Some(handle) {
                refusal.handle = None;
            }
        }
    }
}

/// A write-open Blob handle's candidate generation and the row baseline it captured at open
/// (ORNA-VFS-011). The baseline advances to the accepted row after each accept (41-vfs.md EDIT-1 step 3).
struct BlobHandle {
    generation: u64,
    baseline: Option<Vec<u8>>,
}

/// Decides the acknowledgement for a commit that found no candidate. A clean
/// handle submitted nothing, so fsync of it succeeds (POSIX). A handle with a
/// written but unaccepted candidate must not be acknowledged: that would report
/// durability the accepted row does not have, so it fails closed with EIO.
fn noncandidate_ack(
    draft_state: &mut DraftState,
    handle: u64,
    operation: &str,
) -> Result<(), Errno> {
    if draft_state.dirty_handles.contains(&handle) {
        eprintln!(
            "{operation}: handle {handle} wrote a candidate that no commit found; refusing to acknowledge it (EIO)"
        );
        return Err(Errno::EIO);
    }
    draft_state.unresolved_handles.remove(&handle);
    Ok(())
}

struct WorkspaceFilesystem {
    state: Mutex<FsBuilder>,
    open_files: Mutex<HashMap<u64, OpenFile>>,
    draft_state: Mutex<DraftState>,
    next_handle: AtomicU64,
}

/// The errno a retention refusal reports when several refusals meet: a full
/// store (EDQUOT) wins over other storage failures (EIO).
fn worst_retention_errno(current: Option<Errno>, next: Errno) -> Option<Errno> {
    Some(if current == Some(Errno::EDQUOT) || next == Errno::EDQUOT {
        Errno::EDQUOT
    } else {
        Errno::EIO
    })
}

impl WorkspaceFilesystem {
    fn from_snapshot(
        repository: &Repository,
        commit: &GitCommitRef,
        context: &HistoricalFormatContext,
        writer: Option<Arc<MountWriter>>,
    ) -> Result<Self, String> {
        // The caller passes the commit and its context from the same pinned view.
        let identities = ObjectIdentityManifest::read_committed(repository, commit)
            .map_err(|error| format!("committed table identities could not be read: {error:?}"))?;
        let tables = identities
            .records()
            .iter()
            .filter(|identity| identity.kind() == ObjectKind::Table && !identity.is_retired())
            .map(|identity| (*identity.id(), identity.qualified_name().to_owned()))
            .collect::<Vec<_>>();
        let loader = ProjectLoader::default();
        let mut builder = FsBuilder::new();
        builder.writer.clone_from(&writer);
        builder.table_object_ids = Arc::new(
            tables
                .iter()
                .map(|(relation_id, qualified_name)| {
                    (qualified_name.clone(), TableObjectId::new(*relation_id))
                })
                .collect(),
        );
        let mut runtime_tables = BTreeMap::new();
        if let Some(writer) = &builder.writer {
            // Every committed table is admitted under its pinned declaration name,
            // the name its row mutations address. The HEAD layout only gates the
            // runtime projection; an unresolved HEAD schema leaves that table pinned.
            let mut activation_tables = Vec::with_capacity(tables.len());
            for (relation_id, _) in &tables {
                let table_id = TableObjectId::new(*relation_id);
                let pinned_schema = loader
                    .resolve_committed_source_table_schema(repository, commit, table_id)
                    .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
                let runtime_table =
                    RuntimeTableIdentity::new(&pinned_schema.qualified_name(), table_id)
                        .map_err(|error| format!("runtime table identity is invalid: {error:?}"))?;
                let runtime_field_layout = loader
                    .resolve_committed_source_table_schema(
                        repository,
                        &writer.runtime_commit,
                        table_id,
                    )
                    .ok()
                    .map(|runtime_schema| stored_field_layout(runtime_schema.descriptor()))
                    .transpose()?;
                activation_tables.push(runtime_table.clone());
                runtime_tables.insert(*relation_id, (runtime_table, runtime_field_layout));
            }
            writer.capture_mount_activation(&activation_tables)?;
        }

        // A mount runs under the lease owner its writer holds, so accepted pins
        // the runtime captured resolve through this graph. A read-only mount takes
        // the owner the runtime would take: this process's owner.
        let graph_owner_id = match &writer {
            Some(writer) => Some(writer.owner_id),
            None => Some(super::invocation_owner_id()),
        };

        for (relation_id, qualified_name) in tables {
            let schema = loader
                .resolve_committed_source_table_schema(
                    repository,
                    commit,
                    TableObjectId::new(relation_id),
                )
                .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
            let table_directory = builder.table_directory(
                schema.binding().module_name().unwrap_or_default(),
                schema.declaration_name(),
            )?;
            let field_names = field_names_for_table(&identities, &qualified_name);
            let rows = context
                .load_row_map(relation_id)
                .map_err(|error| format!("committed row map could not be opened: {error}"))?;
            let graph = Arc::new(
                match graph_owner_id {
                    Some(owner_id) => context.open_native_graph_for_owner(&rows, owner_id),
                    None => context.open_native_graph(&rows),
                }
                .map_err(|error| format!("committed native graph could not be opened: {error}"))?,
            );
            let value_context = context.open_value_context(relation_id).map_err(|error| {
                format!("committed row value context could not be opened: {error:?}")
            })?;
            let (runtime_table, runtime_field_layout) = runtime_tables
                .remove(&relation_id)
                .map(|(table, layout)| (Some(table), layout))
                .unwrap_or((None, None));
            builder.add_table_rows(
                table_directory,
                graph,
                Arc::new(rows),
                &schema,
                value_context,
                &field_names,
                runtime_table,
                runtime_field_layout,
            )?;
        }
        let mut retained_scratch = HashSet::new();
        if let Some(writer) = &writer {
            for record in writer.scratch_store.load()? {
                let logical_path = record.identity.logical_path.clone();
                match Self::restore_temporary_scratch(&mut builder, writer, record) {
                    Ok(inode) => {
                        retained_scratch.insert(inode);
                    }
                    Err(error) => eprintln!(
                        "skipped unresolved persisted temporary scratch {logical_path}: {error}"
                    ),
                }
            }
        }

        Ok(Self {
            state: Mutex::new(builder),
            open_files: Mutex::new(HashMap::new()),
            draft_state: Mutex::new(DraftState {
                retained_scratch,
                ..DraftState::default()
            }),
            next_handle: AtomicU64::new(1),
        })
    }
    fn restore_temporary_scratch(
        builder: &mut FsBuilder,
        writer: &Arc<MountWriter>,
        record: PersistedTemporaryScratch,
    ) -> Result<u64, String> {
        let table_inode = builder
            .nodes
            .iter()
            .find_map(|(inode, node)| match &node.kind {
                NodeKind::Table(table)
                    if *table.source.binding.relation_id() == record.identity.relation_id =>
                {
                    Some(*inode)
                }
                _ => None,
            })
            .ok_or_else(|| "persisted temporary scratch target table is unavailable".to_owned())?;
        let mut path_components = record.identity.logical_path.split('/').skip(1).peekable();
        let mut parent_inode = table_inode;
        while let Some(component) = path_components.next() {
            let component = if path_components.peek().is_none() {
                component.strip_suffix(".orna").ok_or_else(|| {
                    "persisted temporary scratch target path is invalid".to_owned()
                })?
            } else {
                component
            };
            parent_inode = builder
                .lookup_child(parent_inode, OsStr::new(component))?
                .ok_or_else(|| {
                    "persisted temporary scratch target row is unavailable".to_owned()
                })?;
        }
        if !matches!(
            builder.nodes.get(&parent_inode).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return Err("persisted temporary scratch target row is unavailable".into());
        }
        let document_inode = builder
            .lookup_child(parent_inode, OsStr::new("data.orna"))?
            .ok_or_else(|| {
                "persisted temporary scratch target document is unavailable".to_owned()
            })?;
        let save = match builder.nodes.get(&document_inode).map(|node| &node.kind) {
            Some(NodeKind::Document { save, .. }) => Arc::clone(save),
            _ => return Err("persisted temporary scratch target document is unavailable".into()),
        };
        if *save.source.binding.relation_id() != record.identity.relation_id
            || runtime_table_key_bytes(&save.key)? != record.identity.key
        {
            return Err("persisted temporary scratch target identity has changed".into());
        }
        let parent = builder
            .nodes
            .get(&document_inode)
            .ok_or_else(|| "persisted temporary scratch row node is unavailable".to_owned())?
            .parent;
        let blob_field = match record.identity.blob_field_name.as_deref() {
            Some(name) => Some(
                save.blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .find(|field| field.name.as_str() == name)
                    .cloned()
                    .ok_or_else(|| {
                        "persisted temporary scratch Blob field is unavailable".to_owned()
                    })?,
            ),
            None => None,
        };
        let target = save.temporary_target(blob_field.as_ref())?;
        let scratch = Arc::new(
            writer
                .runtime
                .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
        );
        writer
            .runtime
            .block_on(scratch.write_at(0, &record.bytes))
            .map_err(|error| {
                format!("persisted temporary scratch bytes could not be restored: {error:?}")
            })?;
        let PersistedTemporaryScratch {
            identity,
            baseline,
            bytes,
        } = record;
        let name = identity.name.clone();
        builder.add_file(
            parent,
            &name,
            NodeKind::Temporary {
                len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity: Arc::new(identity),
            },
        )
    }

    fn update_writable_len(&self, inode: u64, len: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(node) = state.nodes.get_mut(&inode) {
            if let NodeKind::Temporary { len: current, .. } = &mut node.kind {
                *current = len;
            }
        }
    }
    fn has_unresolved_drafts(&self) -> bool {
        self.draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_unresolved_drafts()
    }

    /// Records a refused document save (ORNA-VFS-009): the candidate is kept in
    /// recovery storage under a stable draft ID before its handle can go away.
    /// A stale save also re-projects the row, so a new open shows and saves
    /// against the row another writer accepted (W15-03).
    fn refuse_document(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        handle: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
        error: &DocumentCommitError,
    ) -> Option<Errno> {
        self.retain_candidate(
            draft_state,
            inode,
            handle,
            save,
            draft,
            error.cause(),
            matches!(error, DocumentCommitError::Stale),
        )
    }

    /// Accepts a dirty document candidate (EDIT-1 steps 1-2). The caller holds
    /// the draft-state lock, so acceptance serializes with writes (ORNA-VFS-008).
    /// A refused candidate is retained before its error is returned.
    fn accept_document(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        inode: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
    ) -> Result<(), DocumentCommitError> {
        if let Err(error) = save.commit_document(draft, true) {
            // NoCandidate: the handle submitted no bytes, so there is no candidate to
            // retain and nothing blocks unmount (ORNA-VFS-009, EDIT-1 step 2).
            if matches!(error, DocumentCommitError::NoCandidate) {
                return Err(error);
            }
            // A candidate recovery storage cannot keep reports the retention errno
            // and stays unresolved (ORNA-VFS-009).
            return Err(match self.refuse_document(draft_state, inode, handle, save, draft, &error) {
                Some(errno) => DocumentCommitError::Unretained {
                    errno,
                    cause: error.cause(),
                },
                None => error,
            });
        }
        let bytes = save
            .writer
            .runtime
            .block_on(draft.candidate_bytes())
            .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
        if let Some(NodeKind::Document {
            bytes: current,
            save: current_save,
        }) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .nodes
            .get_mut(&inode)
            .map(|node| &mut node.kind)
        {
            *current = bytes.to_vec();
            // A handle opened before a refresh saves through its own
            // context; new opens now continue from the row it accepted.
            if !Arc::ptr_eq(current_save, save) {
                *current_save = Arc::clone(save);
            }
        }
        draft_state.unresolved_handles.remove(&handle);
        draft_state.dirty_handles.remove(&handle);
        // The accepted save supersedes the refusal it replaces. A refusing handle
        // whose candidate is unchanged holds only obsolete bytes, so its unresolved
        // state ends here and its later RELEASE retains nothing.
        if let Some(DraftRefusal {
            handle: Some(refused),
            ..
        }) = draft_state.rejected_documents.remove(&inode)
        {
            draft_state.unresolved_handles.remove(&refused);
        }
        Ok(())
    }

    /// Commits the pending state of an open handle: a document candidate is
    /// accepted, and a scratch file is persisted to the scratch store.
    fn commit_open_handle(
        &self,
        draft_state: &mut DraftState,
        inode: INodeNo,
        handle: u64,
    ) -> Result<(), DocumentCommitError> {
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle)
            .cloned();
        match open_file {
            Some(OpenFile::Document(save, draft)) => {
                self.accept_document(draft_state, handle, inode.0, &save, &draft)
            }
            Some(OpenFile::Temporary(save, target, scratch, Some(field)))
                if draft_state.blob_handles.contains_key(&handle) =>
            {
                self.commit_blob_candidate(draft_state, handle, &save, &target, &scratch, &field)
            }
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match state.nodes.get(&inode.0).map(|node| &node.kind) {
                    Some(NodeKind::Temporary {
                        identity, baseline, ..
                    }) => save
                        .fsync_temporary(&scratch, identity, baseline.as_deref())
                        .map_err(DocumentCommitError::Failed),
                    // Rename already accepted and removed this scratch node.
                    None => Ok(()),
                    _ => Err(DocumentCommitError::Failed(
                        "open scratch node is unavailable".to_owned(),
                    )),
                }
            }
            None => Err(DocumentCommitError::Failed(
                "file handle is not open".to_owned(),
            )),
        }
    }

    /// Accepts a Blob write-open's candidate as the row's replacement (41-vfs.md EDIT-1).
    /// The baseline is the one captured when the handle opened (ORNA-VFS-011), so an accept by a
    /// sibling handle since then fails the row CAS and the candidate is refused. After an accept,
    /// the handle's next candidate starts from the accepted bytes (EDIT-1 step 3).
    fn commit_blob_candidate(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        save: &Arc<RowSaveContext>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &Arc<TemporarySave<RowMapSnapshot>>,
        field: &BlobField,
    ) -> Result<(), DocumentCommitError> {
        let Some(blob) = draft_state.blob_handles.get(&handle) else {
            return Err(DocumentCommitError::Failed(
                "open Blob candidate has no captured baseline".to_owned(),
            ));
        };
        let inode = blob.generation;
        let baseline = blob.baseline.clone();
        let parent = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode) {
                Some(node)
                    if matches!(
                        node.kind,
                        NodeKind::Blob { candidate: Some(_), .. } | NodeKind::BlobBytes(_)
                    ) =>
                {
                    node.parent
                }
                // The node is gone, so there is no candidate left to accept.
                None => return Ok(()),
                Some(_) => {
                    return Err(DocumentCommitError::Failed(
                        "open Blob candidate node is unavailable".to_owned(),
                    ))
                }
            }
        };
        if let Err(error) = save.commit_blob_temporary(target, scratch, field, baseline, true) {
            draft_state.rejected_scratch.insert(
                inode,
                ScratchRefusal { cause: error.cause(), kept: false },
            );
            return Err(error);
        }
        let bytes = save
            .writer
            .runtime
            .block_on(scratch.candidate_bytes())
            .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
        let generation = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace_blob_generation(parent, OsStr::new(&field.file_name), inode, Arc::clone(&bytes))
            .map_err(DocumentCommitError::Failed)?;
        // The accepted candidate is no longer draft state (same as an accepted rename).
        draft_state.retained_scratch.remove(&inode);
        draft_state.rejected_scratch.remove(&inode);
        // The next candidate starts from the accepted bytes, and its baseline is the accepted row.
        let accepted_baseline = save
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let next_target = save
            .temporary_target(Some(field))
            .map_err(DocumentCommitError::Failed)?;
        let next_scratch = Arc::new(
            save.writer
                .runtime
                .block_on(next_target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
        );
        if !bytes.is_empty() {
            save.writer
                .runtime
                .block_on(next_scratch.write_at(0, &bytes))
                .map_err(|error| {
                    DocumentCommitError::Failed(format!(
                        "next Blob candidate could not be seeded: {error:?}"
                    ))
                })?;
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.track_blob_open(generation);
            state.track_blob_release(inode);
        }
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle,
                OpenFile::Temporary(
                    Arc::clone(save),
                    next_target,
                    next_scratch,
                    Some(field.clone()),
                ),
            );
        draft_state.blob_handles.insert(
            handle,
            BlobHandle {
                generation,
                baseline: accepted_baseline,
            },
        );
        Ok(())
    }

    /// Keeps the candidate of a document released while dirty (EDIT-1 step 5).
    /// Release only cleans up, so the candidate is retained and reported, never
    /// accepted.
    fn retain_unsynced_document(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        handle: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
    ) {
        // A candidate already refused by fsync or flush keeps its refusal cause
        // (ORNA-VFS-009); only a candidate never refused is recorded as unsynced.
        let cause = release_retention_cause(draft_state.rejected_documents.get(&inode));
        self.retain_candidate(draft_state, inode, handle, save, draft, cause, false);
    }

    /// Writes a candidate that was not accepted to recovery storage and records
    /// it against the file for unmount reporting (ORNA-VFS-009). A stale save
    /// also re-projects the row, so a new open shows and saves against the row
    /// another writer accepted (W15-03).
    fn retain_candidate(
        &self,
        draft_state: &mut DraftState,
        inode: u64,
        handle: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
        cause: &'static str,
        refresh: bool,
    ) -> Option<Errno> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = state.display_path(inode);
        let len = save.writer.runtime.block_on(draft.candidate_len());
        let retained = {
            let mut read_chunk = |offset: u64, length: usize| {
                save.writer
                    .runtime
                    .block_on(draft.read_range(offset, length))
                    .map_err(|error| RetentionError::from(candidate_unreadable(error)))
            };
            save.writer
                .retained_drafts
                .retain(&path, cause, len, &mut read_chunk)
        };
        let (retained_draft, unretained, failure) = match retained {
            Ok(retained) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
                    retained.id,
                    retained.record.display()
                );
                (Some(retained.id), None, None)
            }
            Err(retain_error) => {
                eprintln!("{path}: not accepted ({cause}); candidate could not be retained: {retain_error}");
                // Recovery storage refused the candidate, so the in-memory stash is the
                // only copy left. It is the sole whole-candidate copy on this path and
                // exists only while retention is refused (follow-up bead).
                match save.writer.runtime.block_on(draft.candidate_bytes()) {
                    Ok(bytes) => (None, Some((Arc::clone(save), bytes)), Some(retain_error.errno)),
                    Err(error) => {
                        // Nothing can be retained without the bytes; the refusal stays recorded.
                        eprintln!(
                            "{path}: not accepted ({cause}); candidate could not be read: {}",
                            candidate_unreadable(error)
                        );
                        (None, None, Some(Errno::EIO))
                    }
                }
            }
        };
        if let Some(superseded) = draft_state.rejected_documents.insert(
            inode,
            DraftRefusal {
                cause,
                handle: Some(handle),
                candidate: Some(Arc::clone(draft)),
                draft: retained_draft,
                unretained,
            },
        ) && superseded.unretained.is_some()
        {
            eprintln!("{path}: an earlier unretained candidate was superseded");
        }
        if refresh {
            state.report_refresh(inode, save, &path);
        }
        failure
    }

    /// The unresolved drafts of this mount, one per path. Both draft-list xattrs
    /// render these, so the unmount refusal and `mount status` name the same drafts.
    fn unresolved_drafts(&self) -> Vec<UnresolvedDraft> {
        // The `sys.vfs.*` code of an internal cause, when the cause has one.
        fn spec_code(cause: &'static str) -> Option<&'static str> {
            cause.starts_with("sys.").then_some(cause)
        }
        const DRAFT_RETAINED: &str = "sys.vfs.draft_retained";
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // inode -> (code, draft id, detail). The code is the spec code of the internal
        // cause; otherwise `draft_retained` when a candidate was kept; otherwise empty.
        let mut unresolved: BTreeMap<u64, (&'static str, String, String)> = BTreeMap::new();
        for (inode, refusal) in &draft_state.rejected_documents {
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.draft.as_ref().map(|_| DRAFT_RETAINED))
                .unwrap_or("");
            let (draft, detail) = match &refusal.draft {
                Some(draft) => (
                    draft.clone(),
                    format!(
                        "not accepted ({}); the file shows the current row; candidate retained as draft {draft}",
                        refusal.cause
                    ),
                ),
                None => (
                    String::new(),
                    format!("not accepted ({}); candidate not retained", refusal.cause),
                ),
            };
            unresolved.insert(*inode, (code, draft, detail));
        }
        for (inode, refusal) in &draft_state.rejected_scratch {
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.kept.then_some(DRAFT_RETAINED))
                .unwrap_or("");
            unresolved.entry(*inode).or_insert_with(|| {
                (code, String::new(), format!("scratch rename refused ({})", refusal.cause))
            });
        }
        for inode in &draft_state.retained_scratch {
            unresolved.entry(*inode).or_insert_with(|| {
                (DRAFT_RETAINED, String::new(), "scratch file not renamed".to_owned())
            });
        }
        for inode in draft_state.unresolved_handles.values() {
            unresolved.entry(*inode).or_insert_with(|| {
                ("", String::new(), "unsynced writes in an open file".to_owned())
            });
        }
        for (inode, size) in &draft_state.pending_truncates {
            unresolved.entry(*inode).or_insert_with(|| {
                ("", String::new(), format!("truncate to {size} bytes not synced"))
            });
        }
        let mut drafts = Vec::with_capacity(unresolved.len());
        for (inode, (code, draft, detail)) in unresolved {
            drafts.push(UnresolvedDraft {
                path: state.display_path(inode),
                code,
                draft,
                detail,
            });
        }
        drafts
    }

    /// One `path<TAB>detail` line per unresolved draft, for an unmount refusal.
    fn unresolved_draft_report(&self) -> String {
        self.render_unresolved_drafts(|draft| format!("{}\t{}", draft.path, draft.detail))
    }

    /// One `path<TAB>code<TAB>draft<TAB>detail` line per unresolved draft, for
    /// `mount status`. `code` is the `sys.*` Orna cause (or `sys.vfs.draft_retained`
    /// for a kept candidate with no other cause) and `draft` the retained draft id;
    /// each is empty when the draft has none.
    fn unresolved_draft_fields(&self) -> String {
        self.render_unresolved_drafts(|draft| {
            format!("{}\t{}\t{}\t{}", draft.path, draft.code, draft.draft, draft.detail)
        })
    }

    /// Renders the drafts as sorted lines, bounded by both a line count and a byte
    /// budget so the xattr value stays small enough for the kernel to return. A draft
    /// that does not fit is not listed; it is counted on one final `\tomitted\tN` line.
    /// That line has an empty first field, which no draft path can have, so it is
    /// never parsed as a draft.
    fn render_unresolved_drafts(&self, line: impl Fn(&UnresolvedDraft) -> String) -> String {
        const LISTED: usize = 64;
        const BYTE_BUDGET: usize = 32 * 1024;
        // Room for the final `\tomitted\tN` line (its count is at most 20 digits).
        const OMITTED_RESERVE: usize = 32;
        let mut lines: Vec<String> = self.unresolved_drafts().iter().map(line).collect();
        lines.sort();
        let total = lines.len();
        let mut kept = Vec::with_capacity(total.min(LISTED));
        let mut bytes = 0_usize;
        for text in lines {
            if kept.len() == LISTED || bytes + text.len() + 1 + OMITTED_RESERVE > BYTE_BUDGET {
                break;
            }
            bytes += text.len() + 1;
            kept.push(text);
        }
        let omitted = total - kept.len();
        if omitted > 0 {
            kept.push(format!("\tomitted\t{omitted}"));
        }
        kept.join("\n")
    }

    /// Stop-time retention (ORNA-VFS-024): no unresolved draft is silently
    /// discarded when the mount stops. Refused documents are already in
    /// recovery storage; unsynced document writes and truncates are added to
    /// it; scratch files are persisted to the scratch store, which the next
    /// mount restores. Every kept draft is reported with
    /// `sys.vfs.draft_retained`. A candidate that recovery storage refuses keeps
    /// its bytes as an unresolved refusal, so a later attempt retries it.
    /// Returns the errno of the refusal that kept a draft, or `None` when every draft was retained.
    fn retain_all_drafts(&self, draft_state: &mut DraftState) -> Option<Errno> {
        let open_files = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut reports = std::collections::BTreeSet::new();
        let mut candidates: Vec<(
            u64,
            Option<u64>,
            &'static str,
            Arc<RowSaveContext>,
            CandidateSource,
        )> = Vec::new();
        for (handle, inode) in &draft_state.unresolved_handles {
            if let Some(OpenFile::Document(save, draft)) = open_files.get(handle) {
                candidates.push((
                    *inode,
                    Some(*handle),
                    "unsynced",
                    Arc::clone(save),
                    CandidateSource::Draft(Arc::clone(draft)),
                ));
            }
        }
        for (inode, size) in &draft_state.pending_truncates {
            if let Some(NodeKind::Document { save, .. }) =
                state.nodes.get(inode).map(|node| &node.kind)
            {
                // A path truncate cuts the newest writer draft: the open writer's,
                // else the candidate a released writer's refusal still holds.
                let latest = draft_state
                    .unresolved_handles
                    .iter()
                    .filter(|(_, dirty_inode)| **dirty_inode == *inode)
                    .filter_map(|(handle, _)| match open_files.get(handle) {
                        Some(OpenFile::Document(_, draft)) => Some((*handle, Arc::clone(draft))),
                        _ => None,
                    })
                    .max_by_key(|(handle, _)| *handle)
                    .map(|(_, draft)| draft)
                    .or_else(|| {
                        draft_state
                            .rejected_documents
                            .get(inode)
                            .and_then(|refusal| refusal.candidate.clone())
                    });
                candidates.push((
                    *inode,
                    None,
                    "unsynced",
                    Arc::clone(save),
                    CandidateSource::Truncated { size: *size, latest },
                ));
            }
        }
        let mut unresolved = HashMap::new();
        let mut refused: Option<Errno> = None;
        for (inode, refusal) in std::mem::take(&mut draft_state.rejected_documents) {
            match (refusal.draft, refusal.unretained) {
                (_, Some((save, bytes))) => {
                    candidates.push((
                        inode,
                        refusal.handle,
                        refusal.cause,
                        save,
                        CandidateSource::Stashed(bytes),
                    ));
                }
                (Some(draft), None) => {
                    let path = state.display_path(inode);
                    reports.insert(format!(
                        "retained draft {draft} for {path} ({}; sys.vfs.draft_retained)",
                        refusal.cause
                    ));
                }
                (None, None) => {}
            }
        }
        for (inode, handle, cause, save, source) in candidates {
            let path = state.display_path(inode);
            let len = match &source {
                CandidateSource::Draft(draft) => save.writer.runtime.block_on(draft.candidate_len()),
                CandidateSource::Truncated { size, .. } => *size,
                CandidateSource::Stashed(bytes) => bytes.len() as u64,
            };
            let mut read_chunk = |offset: u64, length: usize| match &source {
                CandidateSource::Draft(draft) => save
                    .writer
                    .runtime
                    .block_on(draft.read_range(offset, length))
                    .map_err(|error| RetentionError::from(candidate_unreadable(error))),
                CandidateSource::Truncated { size, latest } => match latest {
                    Some(draft) => truncated_window(
                        save.writer.runtime.block_on(draft.candidate_len()),
                        *size,
                        offset,
                        length,
                        |start, count| {
                            save.writer
                                .runtime
                                .block_on(draft.read_range(start, count))
                                .map_err(|error| RetentionError::from(candidate_unreadable(error)))
                        },
                    ),
                    None => match state.nodes.get(&inode).map(|node| &node.kind) {
                        Some(NodeKind::Document { bytes, .. }) => {
                            truncated_window(bytes.len() as u64, *size, offset, length, |start, count| {
                                Ok(stashed_range(bytes, start, count))
                            })
                        }
                        _ => Err(RetentionError::from(format!(
                            "{path} has no accepted baseline to truncate"
                        ))),
                    },
                },
                CandidateSource::Stashed(bytes) => Ok(stashed_range(bytes, offset, length)),
            };
            match save
                .writer
                .retained_drafts
                .retain(&path, cause, len, &mut read_chunk)
            {
                Ok(retained) => {
                    reports.insert(format!(
                        "retained draft {} for {path} ({cause}; sys.vfs.draft_retained) in {}",
                        retained.id,
                        retained.record.display()
                    ));
                }
                Err(error) => {
                    refused = worst_retention_errno(refused, error.errno);
                    reports.insert(format!(
                        "draft for {path} ({cause}) could not be retained: {error}"
                    ));
                    // The stash is the only copy left while retention is refused; it is
                    // materialised here alone (follow-up bead bounds it).
                    let stash = match &source {
                        CandidateSource::Stashed(bytes) => Ok(Arc::clone(bytes)),
                        _ => {
                            let mut whole = Vec::new();
                            VfsRetainedDraftStore::stream_candidate(len, &mut read_chunk, |chunk| {
                                whole.extend_from_slice(chunk);
                                Ok(())
                            })
                            .map(|()| Arc::from(whole))
                        }
                    };
                    unresolved.insert(
                        inode,
                        DraftRefusal {
                            cause,
                            draft: None,
                            handle,
                            candidate: None,
                            unretained: stash.ok().map(|bytes| (save, bytes)),
                        },
                    );
                }
            }
        }
        let scratch = draft_state
            .retained_scratch
            .iter()
            .chain(draft_state.rejected_scratch.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        // Scratch whose bytes could not be kept stays unresolved and keeps the mount up.
        let mut unkept_scratch = std::collections::HashSet::new();
        for inode in scratch {
            let Some(NodeKind::Temporary {
                save,
                scratch,
                identity,
                baseline,
                ..
            }) = state.nodes.get(&inode).map(|node| &node.kind)
            else {
                continue;
            };
            let path = state.display_path(inode);
            match save.fsync_temporary(scratch, identity, baseline.as_deref()) {
                Ok(()) => {
                    reports.insert(format!(
                        "kept scratch {path} for the next mount (sys.vfs.draft_retained)"
                    ));
                }
                Err(error) => {
                    refused = worst_retention_errno(refused, Errno::EIO);
                    reports.insert(format!("scratch {path} could not be kept: {error}"));
                    unkept_scratch.insert(inode);
                }
            }
        }
        for report in reports {
            eprintln!("{report}");
        }
        draft_state.unresolved_handles.clear();
        draft_state.pending_truncates.clear();
        draft_state
            .retained_scratch
            .retain(|inode| unkept_scratch.contains(inode));
        draft_state
            .rejected_scratch
            .retain(|inode, _| unkept_scratch.contains(inode));
        draft_state.rejected_documents = unresolved;
        refused
    }
}

impl Filesystem for WorkspaceFilesystem {
    fn getxattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let value = match name.to_str() {
            _ if inode.0 != ROOT_INODE => {
                reply.error(Errno::ENODATA);
                return;
            }
            Some("user.orna.unresolved-drafts") => {
                if self.has_unresolved_drafts() {
                    b"1".to_vec()
                } else {
                    b"0".to_vec()
                }
            }
            Some("user.orna.unresolved-draft-list") => self.unresolved_draft_report().into_bytes(),
            Some("user.orna.unresolved-draft-fields") => self.unresolved_draft_fields().into_bytes(),
            _ => {
                reply.error(Errno::ENODATA);
                return;
            }
        };
        if size == 0 {
            reply.size(u32::try_from(value.len()).unwrap_or(u32::MAX));
        } else if size as usize >= value.len() {
            reply.data(&value);
        } else {
            reply.error(Errno::ERANGE);
        }
    }
    fn setxattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if name.to_str() == Some("user.orna.edit-commit") {
            if inode.0 != ROOT_INODE {
                reply.error(Errno::ENODATA);
                return;
            }
            if flags != 0 || position != 0 {
                reply.error(Errno::EINVAL);
                return;
            }
            let quiescing = self
                .draft_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .quiescing;
            if quiescing {
                reply.error(Errno::EBUSY);
                return;
            }
            // Commit runs without the draft-state lock so it cannot deadlock
            // with the FUSE operations that take it (ORNA-VFS-015).
            match self.commit_requested_edit(value) {
                Ok(()) => reply.ok(),
                Err(EditError::Stale(message)) => {
                    eprintln!("explicit edit refused: {message}");
                    reply.error(Errno::ESTALE);
                }
                Err(EditError::Failed(message)) => {
                    eprintln!("explicit edit failed: {message}");
                    reply.error(Errno::EIO);
                }
            }
            return;
        }
        let retain = match name.to_str() {
            Some("user.orna.unmount-quiesce") if inode.0 == ROOT_INODE => false,
            Some("user.orna.retain-drafts") if inode.0 == ROOT_INODE => true,
            _ => {
                reply.error(refuse_unsupported("setxattr"));
                return;
            }
        };
        if flags != 0 || position != 0 {
            reply.error(Errno::EINVAL);
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if retain {
            if value != b"1" {
                reply.error(Errno::EINVAL);
                return;
            }
            draft_state.quiescing = true;
            match self.retain_all_drafts(&mut draft_state) {
                None => reply.ok(),
                Some(errno) => reply.error(errno),
            }
        } else if value == b"1" {
            if draft_state.has_unresolved_drafts() {
                reply.error(Errno::EBUSY);
                return;
            }
            draft_state.quiescing = true;
            reply.ok();
        } else if value == b"0" {
            draft_state.quiescing = false;
            reply.ok();
        } else {
            reply.error(Errno::EINVAL);
        }
    }
    fn init(
        &mut self,
        _request: &Request,
        config: &mut fuser::KernelConfig,
    ) -> std::io::Result<()> {
        let editor = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .is_some();
        if editor {
            config
                .add_capabilities(EDITOR_INIT_CAPABILITIES)
                .map_err(|unsupported| {
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        format!("editor mount requires atomic O_TRUNC support: {unsupported:?}"),
                    )
                })?;
        }
        Ok(())
    }

    fn removexattr(&self, _request: &Request, _inode: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        reply.error(refuse_unsupported(&format!("removexattr {name:?}")));
    }

    fn mknod(
        &self,
        _request: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        reply.error(refuse_unsupported("mknod"));
    }

    fn symlink(
        &self,
        _request: &Request,
        _parent: INodeNo,
        _link_name: &OsStr,
        _target: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(refuse_unsupported("symlink"));
    }

    fn link(
        &self,
        _request: &Request,
        _inode: INodeNo,
        _new_parent: INodeNo,
        _new_name: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(refuse_unsupported("link"));
    }

    fn mkdir(
        &self,
        _request: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        // Plain mkdir does not invent schema or key semantics (profiles/vfs-1.md:110-113).
        reply.error(refuse_unsupported("mkdir"));
    }

    fn rmdir(&self, _request: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(refuse_unsupported("rmdir"));
    }

    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.lookup_child(parent.0, name) {
            Ok(Some(inode)) => match state.attr(inode) {
                Some(attr) => reply.entry(&ENTRY_TTL, &attr, Generation(1)),
                None => reply.error(Errno::EIO),
            },
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn getattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        let open_len = |handle: u64| -> Option<u64> {
            let open_file = self
                .open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&handle)
                .cloned()?;
            Some(match &open_file {
                OpenFile::Document(save, draft) => {
                    save.writer.runtime.block_on(draft.candidate_len())
                }
                OpenFile::Temporary(save, _, scratch, _) => {
                    save.writer.runtime.block_on(scratch.candidate_len())
                }
            })
        };
        let candidate_size = match handle {
            Some(handle) => open_len(handle.0),
            // fstat without a handle reaches here; an open Document or Blob candidate is the file.
            None => {
                let handles: Vec<u64> = self
                    .draft_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .unresolved_handles
                    .iter()
                    .filter(|(_, dirty_inode)| **dirty_inode == inode.0)
                    .map(|(handle, _)| *handle)
                    .collect();
                handles
                    .into_iter()
                    .filter(|handle| {
                        self.open_files
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get(handle)
                            .is_some_and(|open| {
                                matches!(
                                    open,
                                    OpenFile::Document(..) | OpenFile::Temporary(_, _, _, Some(_))
                                )
                            })
                    })
                    .max()
                    .and_then(open_len)
            }
        };

        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.attr(inode.0) {
            Some(mut attr) => {
                if let Some(size) = candidate_size {
                    attr.size = size;
                    attr.blocks = size.saturating_add(511) / 512;
                }
                reply.attr(&ENTRY_TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, request: &Request, inode: INodeNo, flags: fuser::OpenFlags, reply: ReplyOpen) {
        // A Blob projection has no private candidate (41-vfs.md:42-44 places writes
        // in one), so a write-intent open, including O_TRUNC, is refused with EPERM
        // without truncating or replacing the blob. EPERM is the unsupported-mutation
        // errno (profiles/vfs-1.md:139; 41-vfs.md:103-105), and unsupported operations
        // return an explicit errno, never fake success (profiles/vfs-1.md:104-106).
        // Blob unlink, rename, and path truncate are refused with EPERM by
        // refused_mutation_errno, so every Blob mutation in this mount agrees.
        let write_mode =
            flags.0 & libc::O_ACCMODE != libc::O_RDONLY || flags.0 & libc::O_TRUNC != 0;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replaced_blob = matches!(
            state.nodes.get(&inode.0).map(|node| &node.kind),
            Some(NodeKind::BlobBytes(_))
        );
        if replaced_blob {
            if write_mode {
                drop(state);
                reply.error(Errno::EPERM);
                return;
            }
            state.track_blob_open(inode.0);
        }
        drop(state);
        if replaced_blob {
            reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (save, target, scratch, blob_field) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode.0).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => {
                    (Arc::clone(save), Arc::clone(&save.target), None, None)
                }
                Some(NodeKind::Temporary {
                    save,
                    target,
                    scratch,
                    blob_field,
                    ..
                }) => (
                    Arc::clone(save),
                    Arc::clone(target),
                    Some(Arc::clone(scratch)),
                    blob_field.clone(),
                ),
                Some(NodeKind::Blob {
                    graph,
                    reference,
                    candidate: Some(candidate),
                }) if write_mode => {
                    let graph = Arc::clone(graph);
                    let reference = reference.clone();
                    let save = Arc::clone(&candidate.save);
                    let field = candidate.field.clone();
                    let initial = if flags.0 & libc::O_TRUNC != 0 {
                        Ok(Vec::new())
                    } else {
                        read_blob_bytes(&graph, &reference)
                    };
                    if matches!(&initial, Ok(bytes) if bytes.len() > DRAFT_FILE_BOUND_BYTES) {
                        reply.error(Errno::EDQUOT);
                        return;
                    }
                    let seeded = initial.and_then(|initial| {
                        let target = save.temporary_target(Some(&field))?;
                        let scratch = Arc::new(
                            save.writer
                                .runtime
                                .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
                        );
                        if !initial.is_empty() {
                            save.writer
                                .runtime
                                .block_on(scratch.write_at(0, &initial))
                                .map_err(|error| {
                                    format!("Blob candidate could not be seeded: {error:?}")
                                })?;
                        }
                        Ok((target, scratch))
                    });
                    match seeded {
                        Ok((target, scratch)) => {
                            (save, target, Some(scratch), Some(field))
                        }
                        Err(error) => {
                            eprintln!("mounted Blob candidate could not be opened: {error}");
                            reply.error(Errno::EIO);
                            return;
                        }
                    }
                }
                Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_)) => {
                    if write_mode {
                        reply.error(Errno::EPERM);
                        return;
                    }
                    state.track_blob_open(inode.0);
                    reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
                    return;
                }
                Some(NodeKind::Bytes(_)) => {
                    reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
                    return;
                }
                Some(_) => {
                    reply.error(Errno::EISDIR);
                    return;
                }
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let pending_truncate = draft_state.pending_truncates.get(&inode.0).copied();
        let truncate_size = if flags.0 & libc::O_TRUNC != 0 {
            Some(0)
        } else {
            pending_truncate
        };
        let is_scratch = scratch.is_some();
        let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
        if truncate_size.is_some() {
            draft_state.unresolved_handles.insert(handle, inode.0);
            draft_state.candidate_written(handle, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        if let Some(scratch) = scratch {
            if let Some(size) = truncate_size {
                let result = save.writer.runtime.block_on(scratch.truncate(size));
                if result.is_err() {
                    draft_state.unresolved_handles.remove(&handle);
                    if is_scratch && !had_retained_scratch {
                        draft_state.retained_scratch.remove(&inode.0);
                    }
                    reply.error(draft_errno(&result));
                    return;
                }
            }
            if blob_field.is_some() {
                // The handle holds its generation and the row baseline it captured at open (ORNA-VFS-011).
                let baseline = save
                    .expected_baseline_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .track_blob_open(inode.0);
                draft_state.blob_handles.insert(
                    handle,
                    BlobHandle {
                        generation: inode.0,
                        baseline,
                    },
                );
            }
            self.open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    handle,
                    OpenFile::Temporary(save, target, scratch, blob_field),
                );
            if truncate_size.is_some() {
                draft_state.pending_truncates.remove(&inode.0);
            }
            if write_mode {
                draft_state.writer_owners.insert(handle, request.pid());
            }
            reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
            return;
        }
        let draft = Arc::new(
            save.writer
                .runtime
                .block_on(save.target.open_draft(DRAFT_FILE_BOUND_BYTES)),
        );
        if let Some(size) = truncate_size {
            let result = save.writer.runtime.block_on(draft.truncate(size));
            if result.is_err() {
                draft_state.unresolved_handles.remove(&handle);
                reply.error(draft_errno(&result));
                return;
            }
        }
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Document(save, draft));
        if truncate_size.is_some() {
            draft_state.pending_truncates.remove(&inode.0);
        }
        if write_mode {
            draft_state.writer_owners.insert(handle, request.pid());
        }
        reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
    }
    fn create(
        &self,
        request: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (attr, save, target, scratch, blob_field) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(
                state.nodes.get(&parent.0).map(|node| &node.kind),
                Some(NodeKind::Row(_))
            ) {
                reply.error(Errno::EPERM);
                return;
            }
            let destination = match state.lookup_child(parent.0, OsStr::new("data.orna")) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::EIO);
                    return;
                }
            };
            let save = match state.nodes.get(&destination).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => Arc::clone(save),
                _ => {
                    reply.error(Errno::EPERM);
                    return;
                }
            };
            // 41-vfs.md Temporary-save replacement: an unknown file inside a row directory is
            // private editor scratch, not a new column. A replacement name of a Blob field binds
            // to that field; any other unknown name stays unbound until renamed.
            let blob_field = save
                .blob_fields
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .find(|field| is_replacement_scratch_for(name, &field.file_name))
                .cloned();
            let Ok(key) = runtime_table_key_bytes(&save.key) else {
                reply.error(Errno::EIO);
                return;
            };
            let Ok(baseline) = save.temporary_baseline(&key) else {
                reply.error(Errno::EIO);
                return;
            };
            let identity = Arc::new(TemporaryScratchIdentity {
                logical_path: save.logical_path.clone(),
                relation_id: *save.source.binding.relation_id(),
                key,
                name: name.to_owned(),
                blob_field_name: blob_field.as_ref().map(|field| field.name.clone()),
            });
            let Ok(target) = save.temporary_target(blob_field.as_ref()) else {
                reply.error(Errno::EIO);
                return;
            };
            let scratch = Arc::new(
                save.writer
                    .runtime
                    .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
            );
            if blob_field.is_none() {
                if matches!(state.lookup_child(parent.0, std::ffi::OsStr::new(name)), Ok(Some(_))) {
                    reply.error(Errno::EEXIST);
                    return;
                }
                if !save.writer.scratch_store.reserve_unbound(&identity) {
                    reply.error(Errno::EDQUOT);
                    return;
                }
            }
            let inode = match state.add_file(
                parent.0,
                name,
                NodeKind::Temporary {
                    len: 0,
                    save: Arc::clone(&save),
                    target: Arc::clone(&target),
                    scratch: Arc::clone(&scratch),
                    blob_field: blob_field.clone(),
                    baseline,
                    identity: Arc::clone(&identity),
                },
            ) {
                Ok(inode) => inode,
                Err(_) => {
                    save.writer.scratch_store.release_unbound(&identity);
                    reply.error(Errno::EEXIST);
                    return;
                }
            };
            let Some(attr) = state.attr(inode) else {
                reply.error(Errno::EIO);
                return;
            };
            (attr, save, target, scratch, blob_field)
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        draft_state.writer_owners.insert(handle, request.pid());
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle,
                OpenFile::Temporary(save, target, scratch, blob_field),
            );
        reply.created(
            &ENTRY_TTL,
            &attr,
            Generation(1),
            fuser::FileHandle(handle),
            EDITABLE_OPEN_FLAGS,
        );
    }
    fn rename(
        &self,
        _request: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if flags != RenameFlags::empty() {
            reply.error(refuse_unsupported("rename flags"));
            return;
        }
        if parent != newparent {
            reply.error(refuse_unsupported("cross-directory rename"));
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (
            source_inode,
            destination_inode,
            save,
            target,
            scratch,
            blob_field,
            baseline,
            identity,
        ) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_inode = match state.lookup_child(parent.0, name) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            let Some(NodeKind::Temporary {
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity,
                ..
            }) = state.nodes.get(&source_inode).map(|node| &node.kind)
            else {
                reply.error(refused_mutation_errno());
                return;
            };
            let (save, target, scratch, blob_field, baseline, identity) = (
                Arc::clone(save),
                Arc::clone(target),
                Arc::clone(scratch),
                blob_field.clone(),
                baseline.clone(),
                Arc::clone(identity),
            );
            // An unbound scratch commits to the destination its rename names: the row document
            // or the Blob field of that name. A Blob-bound scratch must rename to its own field.
            let blob_field = match blob_field.clone() {
                Some(field) if newname.to_str() == Some(field.file_name.as_str()) => Some(field),
                Some(_) => {
                    reply.error(Errno::EPERM);
                    return;
                }
                None if newname.to_str() == Some("data.orna") => None,
                None => {
                    let field = save
                        .blob_fields
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .find(|field| newname.to_str() == Some(field.file_name.as_str()))
                        .cloned();
                    let Some(field) = field else {
                        reply.error(Errno::EPERM);
                        return;
                    };
                    Some(field)
                }
            };
            let destination_inode = match state.lookup_child(newparent.0, newname) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            match (
                blob_field.as_ref(),
                state.nodes.get(&destination_inode).map(|node| &node.kind),
            ) {
                (
                    None,
                    Some(NodeKind::Document {
                        save: destination_save,
                        ..
                    }),
                ) => {
                    // A refresh can replace the document's context after this
                    // scratch captured its baseline; the scratch still targets
                    // the same row and keeps its own baseline (ORNA-VFS-011).
                    if !same_row(&save, destination_save) {
                        reply.error(Errno::EPERM);
                        return;
                    }
                }
                (Some(_), Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_))) => {}
                _ => {
                    reply.error(Errno::EPERM);
                    return;
                }
            }
            (
                source_inode,
                destination_inode,
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity,
            )
        };
        // An unbound scratch renamed onto a Blob moves its candidate into a scratch of that Blob's
        // own target, the target a Blob-bound create uses, so it commits through that target.
        let (target, scratch) = match &blob_field {
            Some(field) if identity.blob_field_name.is_none() => {
                match save.retarget_unbound_scratch(&scratch, field) {
                    Ok(retargeted) => retargeted,
                    Err(error) => {
                        eprintln!("mounted replacement scratch retarget failed: {error}");
                        reply.error(Errno::EIO);
                        return;
                    }
                }
            }
            _ => (target, scratch),
        };
        let committed = match &blob_field {
            Some(field) => save.commit_blob_temporary(&target, &scratch, field, baseline, true),
            None => save.commit_temporary(&scratch, baseline, true).map(|()| None),
        };
        let accepted_row = committed.as_ref().ok().and_then(Option::clone);
        if let Err(error) = committed {
            eprintln!("mounted replacement failed: {error}");
            draft_state.rejected_scratch.insert(
                source_inode,
                ScratchRefusal { cause: error.cause(), kept: false },
            );
            let stale = matches!(error, DocumentCommitError::Stale);
            // A refused candidate is retained before its rename replies (ORNA-VFS-009).
            // When recovery storage cannot keep it, the rename replies that retention
            // errno (EDQUOT when full) and the scratch stays in place.
            let path = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .display_path(source_inode);
            let len = save.writer.runtime.block_on(scratch.candidate_len());
            let error = match retain_scratch_candidate(
                &save,
                &path,
                error.cause(),
                len,
                &mut |offset, length| {
                    save.writer
                        .runtime
                        .block_on(scratch.read_range(offset, length))
                        .map_err(|read| RetentionError::from(candidate_unreadable(read)))
                },
            ) {
                Ok(()) => {
                    if let Some(refusal) = draft_state.rejected_scratch.get_mut(&source_inode) {
                        refusal.kept = true;
                    }
                    error
                }
                Err(retention) => DocumentCommitError::Unretained {
                    errno: retention.errno,
                    cause: error.cause(),
                },
            };
            if stale {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let document_inode = if blob_field.is_some() {
                    state
                        .lookup_child(newparent.0, OsStr::new("data.orna"))
                        .ok()
                        .flatten()
                } else {
                    Some(destination_inode)
                };
                if let Some(document_inode) = document_inode {
                    let path = state.display_path(document_inode);
                    state.report_refresh(document_inode, &save, &path);
                }
            }
            reply.error(error.errno());
            return;
        }
        let accepted_bytes = match save.writer.runtime.block_on(scratch.candidate_bytes()) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "mounted replacement accepted but its candidate could not be read; cache not refreshed: {}",
                    candidate_unreadable(error)
                );
                reply.error(Errno::EIO);
                return;
            }
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match blob_field {
            Some(_) => {
                if let Err(error) = state.replace_blob_generation(
                    newparent.0,
                    newname,
                    destination_inode,
                    accepted_bytes,
                ) {
                    eprintln!("mounted replacement install failed: {error}");
                    reply.error(Errno::EIO);
                    return;
                }
                if let Some(row) = accepted_row {
                    if let Ok(Some(document)) = state.lookup_child(newparent.0, OsStr::new("data.orna")) {
                        if let Some(NodeKind::Document { bytes, .. }) =
                            state.nodes.get_mut(&document).map(|node| &mut node.kind)
                        {
                            *bytes = row;
                        }
                    }
                }
            }
            None => {
                let Some(NodeKind::Document {
                    bytes,
                    save: destination_save,
                }) = state
                    .nodes
                    .get_mut(&destination_inode)
                    .map(|node| &mut node.kind)
                else {
                    reply.error(Errno::EIO);
                    return;
                };
                *bytes = accepted_bytes.to_vec();
                // New opens continue from the row this scratch's context accepted.
                if !Arc::ptr_eq(destination_save, &save) {
                    *destination_save = Arc::clone(&save);
                }
            }
        }
        if let Err(error) = save.writer.scratch_store.remove(&identity) {
            eprintln!("mounted replacement scratch cleanup failed: {error}");
            reply.error(Errno::EIO);
            return;
        }
        save.writer.scratch_store.release_unbound(&identity);
        let removed = state
            .nodes
            .get_mut(&parent.0)
            .and_then(|node| node.kind.directory_mut())
            .and_then(|directory| directory.remove_entry(name));
        if removed.is_none() {
            reply.error(Errno::EIO);
            return;
        }
        draft_state
            .unresolved_handles
            .retain(|_, dirty_inode| *dirty_inode != source_inode);
        draft_state.retained_scratch.remove(&source_inode);
        draft_state.rejected_scratch.remove(&source_inode);
        // The accepted replacement supersedes the refused candidate it replaces, so
        // the refusing handle no longer holds the destination unresolved (ORNA-VFS-024).
        if let Some(DraftRefusal {
            handle: Some(refused),
            ..
        }) = draft_state.rejected_documents.remove(&destination_inode)
        {
            draft_state.unresolved_handles.remove(&refused);
        }
        draft_state.pending_truncates.remove(&source_inode);
        state.nodes.remove(&source_inode);
        reply.ok();
    }
    fn unlink(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(
            state.nodes.get(&parent.0).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            reply.error(Errno::EPERM);
            return;
        }
        let inode = match state.lookup_child(parent.0, OsStr::new(name)) {
            Ok(Some(inode)) => inode,
            Ok(None) => {
                reply.error(Errno::ENOENT);
                return;
            }
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };
        if !matches!(
            state.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Temporary { .. })
        ) {
            reply.error(refused_mutation_errno());
            return;
        }
        if draft_state
            .unresolved_handles
            .values()
            .any(|open| *open == inode)
        {
            reply.error(Errno::EBUSY);
            return;
        }
        let (save, identity, scratch) = match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Temporary {
                save,
                identity,
                scratch,
                ..
            }) => (Arc::clone(save), Arc::clone(identity), Arc::clone(scratch)),
            _ => {
                reply.error(refused_mutation_errno());
                return;
            }
        };
        if let Some(ScratchRefusal { cause, .. }) =
            draft_state.rejected_scratch.get(&inode).copied()
        {
            // A refused replacement stays recoverable (ORNA-VFS-009): its
            // candidate moves to recovery storage before its name goes away. A
            // candidate recovery storage cannot keep fails with its retention
            // errno (EDQUOT when full) and the scratch stays in place.
            let path = state.display_path(inode);
            let len = save.writer.runtime.block_on(scratch.candidate_len());
            if let Err(error) = retain_scratch_candidate(&save, &path, cause, len, &mut |offset, length| {
                save.writer
                    .runtime
                    .block_on(scratch.read_range(offset, length))
                    .map_err(|read| RetentionError::from(candidate_unreadable(read)))
            }) {
                reply.error(error.errno);
                return;
            }
        }
        if let Err(error) = save.writer.scratch_store.remove(&identity) {
            eprintln!("mounted temporary scratch removal failed: {error}");
            reply.error(Errno::EIO);
            return;
        }
        save.writer.scratch_store.release_unbound(&identity);
        if state
            .directory_mut(parent.0)
            .and_then(|directory| directory.remove_entry(OsStr::new(name)))
            .is_none()
        {
            reply.error(Errno::EIO);
            return;
        }
        state.nodes.remove(&inode);
        draft_state.retained_scratch.remove(&inode);
        draft_state.rejected_scratch.remove(&inode);
        reply.ok();
    }
    fn read(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let private_read = match open_file {
            Some(OpenFile::Document(save, draft)) => Some(read_candidate_range(
                |offset, length| save.writer.runtime.block_on(draft.read_range(offset, length)),
                offset,
                reply_read_size(size),
            )),
            Some(OpenFile::Temporary(save, _, scratch, _)) => Some(read_candidate_range(
                |offset, length| save.writer.runtime.block_on(scratch.read_range(offset, length)),
                offset,
                reply_read_size(size),
            )),
            None => None,
        };
        if let Some(bytes) = private_read {
            match bytes {
                Ok(bytes) => reply.data(&bytes),
                Err(error) => {
                    eprintln!("mounted private candidate read failed: {}", candidate_unreadable(error));
                    reply.error(Errno::EIO);
                }
            }
            return;
        }
        // A Temporary node's candidate lives in its scratch, so a read without an open
        // handle is served by range from there. The state lock is released first.
        let temporary = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode.0).map(|node| &node.kind) {
                Some(NodeKind::Temporary { save, scratch, .. }) => {
                    Some((Arc::clone(save), Arc::clone(scratch)))
                }
                _ => None,
            }
        };
        if let Some((save, scratch)) = temporary {
            match read_candidate_range(
                |offset, length| save.writer.runtime.block_on(scratch.read_range(offset, length)),
                offset,
                reply_read_size(size),
            ) {
                Ok(bytes) => reply.data(&bytes),
                Err(error) => {
                    eprintln!("mounted temporary candidate read failed: {}", candidate_unreadable(error));
                    reply.error(Errno::EIO);
                }
            }
            return;
        }
        let blob = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(node) = state.nodes.get(&inode.0) else {
                reply.error(Errno::ENOENT);
                return;
            };
            match &node.kind {
                NodeKind::Bytes(bytes) | NodeKind::Document { bytes, .. } => {
                    let start = match usize::try_from(offset) {
                        Ok(start) if start < bytes.len() => start,
                        _ => {
                            reply.data(&[]);
                            return;
                        }
                    };
                    let end = start.saturating_add(size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                    return;
                }
                NodeKind::BlobBytes(bytes) => {
                    let start = match usize::try_from(offset) {
                        Ok(start) if start < bytes.len() => start,
                        _ => {
                            reply.data(&[]);
                            return;
                        }
                    };
                    let end = start.saturating_add(size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                    return;
                }
                NodeKind::Blob { graph, reference, .. } => Some((
                    Arc::clone(graph),
                    reference.clone(),
                    reference.content_identity().length(),
                )),
                // Temporary nodes are served from their scratch before this lookup.
                NodeKind::Temporary { .. } => None,
                NodeKind::Directory(_)
                | NodeKind::Table(_)
                | NodeKind::KeyDirectory(_)
                | NodeKind::Row(_) => {
                    reply.error(Errno::EISDIR);
                    return;
                }
            }
        };
        let Some((graph, reference, length)) = blob else {
            reply.error(Errno::EIO);
            return;
        };
        if offset >= length {
            reply.data(&[]);
            return;
        }
        let end = offset.saturating_add(u64::from(size)).min(length);
        let scope = match graph.open_read_scope() {
            Ok(scope) => scope,
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };
        match graph.read_blob_range(&reference, offset..end, &scope) {
            Ok(range) => reply.data(range.bytes()),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn write(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
        let had_unresolved_handle = draft_state.unresolved_handles.contains_key(&handle.0);
        let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
        // A scratch unlinked or renamed away while this handle stayed open is no
        // longer in the namespace; writing through it is not draft state.
        let is_tracked = !is_scratch
            || self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nodes
                .contains_key(&inode.0);
        let writes_candidate = !data.is_empty() && is_tracked;
        if writes_candidate {
            draft_state.unresolved_handles.insert(handle.0, inode.0);
            draft_state.candidate_written(handle.0, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        let (result, candidate_len) = match open_file {
            Some(OpenFile::Document(save, draft)) => (
                save.writer.runtime.block_on(draft.write_at(offset, data)),
                None,
            ),
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                let result = save.writer.runtime.block_on(scratch.write_at(offset, data));
                let len = result
                    .is_ok()
                    .then(|| save.writer.runtime.block_on(scratch.candidate_len()));
                (result, len)
            }
            None => {
                if !had_unresolved_handle {
                    draft_state.unresolved_handles.remove(&handle.0);
                }
                reply.error(Errno::EBADF);
                return;
            }
        };
        if result.is_err() {
            if !had_unresolved_handle {
                draft_state.unresolved_handles.remove(&handle.0);
            }
            if is_scratch && !had_retained_scratch {
                draft_state.retained_scratch.remove(&inode.0);
            }
            reply.error(draft_errno(&result));
        } else {
            if writes_candidate {
                // Dirty means an accepted-or-refused candidate is pending. A failed
                // write leaves no candidate, so it must not mark the handle dirty.
                draft_state.dirty_handles.insert(handle.0);
            }
            if let Some(len) = candidate_len {
                self.update_writable_len(inode.0, len);
            }
            reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX));
        }
    }

    fn fsync(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        match self.commit_open_handle(&mut draft_state, inode, handle.0) {
            Ok(()) => {
                // A successful fsync makes this handle's bytes durable; nothing is
                // unresolved for it, so unlink must not wait on its RELEASE.
                draft_state.unresolved_handles.remove(&handle.0);
                reply.ok();
            }
            Err(DocumentCommitError::NoCandidate) => {
                // The handle submitted nothing, so nothing is pending for it. A clean
                // handle acknowledges (POSIX fsync of an unmodified handle succeeds);
                // a dirty one fails closed (EDIT-1 step 2: a successful sync acknowledges
                // only accepted state).
                match noncandidate_ack(&mut draft_state, handle.0, "fsync") {
                    Ok(()) => reply.ok(),
                    Err(errno) => reply.error(errno),
                }
            }
            // A refused document candidate was already retained by accept_document
            // (ORNA-VFS-009); the error goes to the caller.
            Err(error) => reply.error(error.errno()),
        }
    }

    fn flush(
        &self,
        request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        // Close-time flush (EDIT-1 step 5) attempts the same commit as fsync, but
        // only for a dirty handle. An accepted handle is clean, so a repeated flush
        // cannot accept the same candidate twice (ORNA-VFS-008).
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !draft_state.unresolved_handles.contains_key(&handle.0) {
            reply.ok();
            return;
        }
        // The kernel issues FLUSH when an inherited copy of this descriptor is
        // closed by another process (exec(2) closes CLOEXEC fds through filp_close).
        // While quiescing, a FLUSH on a dirty handle fails with EBUSY from any
        // process, so the close reports that the draft was not accepted.
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        // PR policy (not EDIT-1 step 5 text): a FLUSH from a process other than the
        // one that opened the handle acknowledges without accepting the draft; the
        // opener's own close is the close-time commit. The opener pid is recorded at
        // open, so a later writer cannot move the gate to itself.
        if draft_state
            .writer_owners
            .get(&handle.0)
            .is_some_and(|writer| *writer != request.pid())
        {
            reply.ok();
            return;
        }
        match self.commit_open_handle(&mut draft_state, inode, handle.0) {
            Ok(()) => {
                draft_state.unresolved_handles.remove(&handle.0);
                reply.ok();
            }
            // Same rule as fsync: a clean handle acknowledges; a dirty one fails closed.
            Err(DocumentCommitError::NoCandidate) => {
                match noncandidate_ack(&mut draft_state, handle.0, "flush") {
                    Ok(()) => reply.ok(),
                    Err(errno) => reply.error(errno),
                }
            }
            // The error goes to close(2), which applications may ignore; a refused
            // candidate stays retained (ORNA-VFS-009).
            Err(error) => reply.error(error.errno()),
        }
    }

    fn release(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if handle.0 == 0 {
            // Stateless blob opens are tracked so superseded generations stay readable.
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .track_blob_release(inode.0);
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let released = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&handle.0);
        if matches!(released, Some(OpenFile::Temporary(_, _, _, Some(_)))) {
            let generation = draft_state
                .blob_handles
                .remove(&handle.0)
                .map_or(inode.0, |blob| blob.generation);
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .track_blob_release(generation);
        }
        draft_state.dirty_handles.remove(&handle.0);
        draft_state.writer_owners.remove(&handle.0);
        match (draft_state.unresolved_handles.remove(&handle.0), released) {
            (Some(inode), Some(OpenFile::Temporary(_, _, _, _))) => {
                draft_state.retained_scratch.insert(inode);
            }
            // Release is cleanup only (EDIT-1 step 4): an unsynced document is
            // retained as a draft, never accepted here.
            (Some(inode), Some(OpenFile::Document(save, draft))) => {
                self.retain_unsynced_document(&mut draft_state, inode, handle.0, &save, &draft);
            }
            _ => {}
        }
        reply.ok();
    }

    fn setattr(
        &self,
        request: &Request,
        inode: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        handle: Option<fuser::FileHandle>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        if let Some(errno) = denied_attribute_change(mode, uid, gid, flags) {
            reply.error(errno);
            return;
        }
        if let Some(size) = size {
            let mut draft_state = self
                .draft_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if draft_state.quiescing {
                reply.error(Errno::EBUSY);
                return;
            }
            if let Some(handle) = handle {
                let open_file = self
                    .open_files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&handle.0)
                    .cloned();
                let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
                let had_unresolved_handle = draft_state.unresolved_handles.contains_key(&handle.0);
                let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
                let Some(open_file) = open_file else {
                    reply.error(Errno::EBADF);
                    return;
                };
                // A scratch unlinked or renamed away while this handle stayed open is
                // no longer in the namespace; truncating it is not draft state.
                let is_tracked = !is_scratch
                    || self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .nodes
                        .contains_key(&inode.0);
                // Keep the pid recorded at open(); or_insert covers handles without one.
                draft_state.writer_owners.entry(handle.0).or_insert(request.pid());
                if is_tracked {
                    draft_state.unresolved_handles.insert(handle.0, inode.0);
                    if is_scratch {
                        draft_state.retained_scratch.insert(inode.0);
                    }
                }
                let (result, candidate_len) = match open_file {
                    OpenFile::Document(save, draft) => {
                        (save.writer.runtime.block_on(draft.truncate(size)), None)
                    }
                    OpenFile::Temporary(save, _, scratch, _) => {
                        let result = save.writer.runtime.block_on(scratch.truncate(size));
                        let len = result
                            .is_ok()
                            .then(|| save.writer.runtime.block_on(scratch.candidate_len()));
                        (result, len)
                    }
                };
                if result.is_err() {
                    if !had_unresolved_handle {
                        draft_state.unresolved_handles.remove(&handle.0);
                    }
                    if is_scratch && !had_retained_scratch {
                        draft_state.retained_scratch.remove(&inode.0);
                    }
                    reply.error(draft_errno(&result));
                    return;
                }
                if let Some(len) = candidate_len {
                    self.update_writable_len(inode.0, len);
                }
            } else {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !matches!(
                    state.nodes.get(&inode.0).map(|node| &node.kind),
                    Some(NodeKind::Document { .. })
                ) {
                    reply.error(refused_mutation_errno());
                    return;
                }
                if size > DRAFT_FILE_BOUND_BYTES as u64 {
                    reply.error(Errno::EDQUOT);
                    return;
                }
                draft_state.pending_truncates.insert(inode.0, size);
            }
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.attr(inode.0) {
            Some(mut attr) => {
                if let Some(size) = size {
                    attr.size = size;
                    attr.blocks = size.saturating_add(511) / 512;
                }
                reply.attr(&ENTRY_TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _handle: fuser::FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let parent = match state.nodes.get(&inode.0) {
            Some(node) => node.parent,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        if state.directory(inode.0).is_none() {
            reply.error(Errno::ENOTDIR);
            return;
        }
        if start == 0 && reply.add(INodeNo(inode.0), 1, FileType::Directory, OsStr::new(".")) {
            reply.ok();
            return;
        }
        if start <= 1
            && reply.add(INodeNo(parent), 2, FileType::Directory, OsStr::new(".."))
        {
            reply.ok();
            return;
        }
        if state
            .readdir_page(inode.0, start, |index, entry| {
                reply.add(
                    INodeNo(entry.inode),
                    u64::try_from(index.saturating_add(3)).unwrap_or(u64::MAX),
                    entry.kind,
                    entry.name.as_os_str(),
                )
            })
            .is_err()
        {
            reply.error(Errno::EIO);
            return;
        }
        reply.ok();
    }
}

struct FsBuilder {
    nodes: HashMap<u64, FsNode>,
    next_inode: u64,
    writer: Option<Arc<MountWriter>>,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    /// Open handle counts for blob inodes opened without draft state (handle 0).
    open_blobs: HashMap<u64, u64>,
    /// Replaced blob inodes kept readable until their last open handle is released.
    superseded_blobs: HashSet<u64>,
}

impl FsBuilder {
    fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(
            ROOT_INODE,
            FsNode {
                parent: ROOT_INODE,
                kind: NodeKind::Directory(Directory::new()),
            },
        );
        Self {
            nodes,
            next_inode: ROOT_INODE + 1,
            writer: None,
            table_object_ids: Arc::new(BTreeMap::new()),
            open_blobs: HashMap::new(),
            superseded_blobs: HashSet::new(),
        }
    }

    /// Attributes report uid/gid 0 and advisory permission bits. chmod and
    /// chown are refused with EPERM by `denied_attribute_change`, so the bits
    /// here never grant or revoke access.
    fn attr(&self, inode: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&inode)?;
        let (kind, size, permissions, links) = match &node.kind {
            NodeKind::Directory(_)
            | NodeKind::Table(_)
            | NodeKind::KeyDirectory(_)
            | NodeKind::Row(_) => {
                let directory = node.kind.directory()?;
                (
                    FileType::Directory,
                    0,
                    0o555,
                    2 + u32::try_from(
                        directory
                            .entries
                            .iter()
                            .filter(|entry| entry.kind == FileType::Directory)
                            .count(),
                    )
                    .unwrap_or(u32::MAX - 2),
                )
            }
            NodeKind::Bytes(bytes) => (FileType::RegularFile, bytes.len() as u64, 0o444, 1),
            NodeKind::BlobBytes(bytes) => (FileType::RegularFile, bytes.len() as u64, 0o444, 1),
            NodeKind::Document { bytes, .. } => {
                (FileType::RegularFile, bytes.len() as u64, 0o644, 1)
            }
            NodeKind::Temporary { len, .. } => (FileType::RegularFile, *len, 0o644, 1),
            NodeKind::Blob { reference, .. } => (
                FileType::RegularFile,
                reference.content_identity().length(),
                0o444,
                1,
            ),
        };
        Some(FileAttr {
            ino: INodeNo(inode),
            size,
            blocks: size.saturating_add(511) / 512,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm: permissions,
            nlink: links,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }

    fn directory(&self, inode: u64) -> Option<&Directory> {
        self.nodes.get(&inode)?.kind.directory()
    }

    fn directory_mut(&mut self, inode: u64) -> Option<&mut Directory> {
        self.nodes.get_mut(&inode)?.kind.directory_mut()
    }

    fn table_directory(&mut self, module_name: &str, table_name: &str) -> Result<u64, String> {
        let mut parent = ROOT_INODE;
        if !module_name.is_empty() {
            for component in module_name.split('.') {
                parent = self.ensure_directory(parent, &project_component(component))?;
            }
        }
        self.ensure_directory(parent, &project_component(table_name))
    }

    fn add_table_rows(
        &mut self,
        table_directory: u64,
        graph: Arc<NativeGraphContext>,
        rows: Arc<RowMapSnapshot>,
        schema: &ResolvedTableSchema,
        value_context: Format3Context,
        field_names: &HashMap<[u8; 16], String>,
        runtime_table: Option<RuntimeTableIdentity>,
        runtime_field_layout: Option<StoredFieldLayout>,
    ) -> Result<(), String> {
        let pinned_field_layout = stored_field_layout(schema.descriptor())?;
        let source = Arc::new(TableSource {
            graph,
            rows,
            binding: schema.binding().clone(),
            schema: schema.clone(),
            value_context,
            field_names: field_names.clone(),
            pinned_field_layout,
            runtime_table,
            runtime_field_layout,
        });
        let node = self
            .nodes
            .get_mut(&table_directory)
            .ok_or_else(|| "mount table directory is missing".to_owned())?;
        let NodeKind::Directory(directory) =
            std::mem::replace(&mut node.kind, NodeKind::Directory(Directory::new()))
        else {
            return Err("mount table path conflicts with another table".into());
        };
        node.kind = NodeKind::Table(TableDirectory {
            directory,
            source,
            lower: None,
            exhausted: false,
            error: None,
            indexed_rows: HashMap::new(),
        });
        Ok(())
    }

    fn ensure_directory(&mut self, parent: u64, name: &str) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            if self
                .nodes
                .get(&existing)
                .is_some_and(|node| node.kind.is_directory())
            {
                return Ok(existing);
            }
            return Err(format!(
                "mount path component conflicts with a file: {name}"
            ));
        }
        self.insert_node(
            parent,
            name,
            NodeKind::Directory(Directory::new()),
            FileType::Directory,
        )
    }

    fn lookup_child(&mut self, parent: u64, name: &OsStr) -> Result<Option<u64>, String> {
        let parent_kind = self
            .nodes
            .get(&parent)
            .map(|node| &node.kind)
            .ok_or_else(|| "mount lookup parent is missing".to_owned())?;
        if matches!(parent_kind, NodeKind::Row(_)) {
            self.ensure_row_loaded(parent)?;
        }
        if let Some(inode) = self
            .directory(parent)
            .and_then(|directory| directory.entry(name))
            .map(|entry| entry.inode)
        {
            return Ok(Some(inode));
        }
        if matches!(self.nodes.get(&parent).map(|node| &node.kind), Some(NodeKind::Table(_)))
            && let Some(inode) = self.lookup_indexed_text_row(parent, name)?
        {
            return Ok(Some(inode));
        }
        let table_inode = match self.nodes.get(&parent).map(|node| &node.kind) {
            Some(NodeKind::Table(_)) => Some(parent),
            Some(NodeKind::KeyDirectory(directory)) => Some(directory.table_inode),
            _ => None,
        };
        let Some(table_inode) = table_inode else {
            return Ok(None);
        };
        loop {
            if let Some(inode) = self
                .directory(parent)
                .and_then(|directory| directory.entry(name))
                .map(|entry| entry.inode)
            {
                return Ok(Some(inode));
            }
            let (exhausted, error) = match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (table.exhausted, table.error.clone()),
                _ => return Err("mount table directory is missing".into()),
            };
            if let Some(error) = error {
                return Err(error);
            }
            if exhausted {
                return Ok(None);
            }
            self.load_next_key_page(table_inode)?;
        }
    }

    fn lookup_indexed_text_row(&mut self, table_inode: u64, name: &OsStr) -> Result<Option<u64>, String> {
        let (source, cached) = match self.nodes.get(&table_inode).map(|node| &node.kind) {
            Some(NodeKind::Table(table)) => (
                Arc::clone(&table.source),
                table.indexed_rows.get(name).copied(),
            ),
            _ => return Ok(None),
        };
        if let Some(inode) = cached {
            return Ok(Some(inode));
        }
        let Some(projected) = name.to_str() else {
            return Ok(None);
        };
        let Ok(VfsComponent::Canonical(key_text)) = unproject_component(projected) else {
            return Ok(None);
        };
        let key = TypedKey::Text(key_text);
        let scope = source
            .graph
            .open_read_scope()
            .map_err(|error| format!("indexed row lookup scope could not be opened: {error:?}"))?;
        let row = source
            .graph
            .lookup_row(&key, &scope)
            .map_err(|error| format!("indexed row lookup failed: {error:?}"))?;
        if row.is_none() {
            return Ok(None);
        }
        let inode = self.next_inode;
        let next_inode = inode
            .checked_add(1)
            .ok_or_else(|| "mount tree has too many entries".to_owned())?;
        self.nodes.insert(
            inode,
            FsNode {
                parent: table_inode,
                kind: NodeKind::Row(RowDirectory {
                    directory: Directory::new(),
                    source,
                    key,
                    state: RowLoadState::Unloaded,
                }),
            },
        );
        self.next_inode = next_inode;
        let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
        else {
            return Err("mount table directory is missing".into());
        };
        table.indexed_rows.insert(name.to_os_string(), inode);
        Ok(Some(inode))
    }

    fn ensure_readdir(&mut self, inode: u64, start: usize) -> Result<(), String> {
        if matches!(
            self.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return self.ensure_row_loaded(inode);
        }
        let table_inode = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Table(_)) => Some(inode),
            Some(NodeKind::KeyDirectory(directory)) => Some(directory.table_inode),
            _ => None,
        };
        let Some(table_inode) = table_inode else {
            return Ok(());
        };
        let child_start = start.saturating_sub(2);
        let loaded = self
            .directory(inode)
            .ok_or_else(|| "mount directory is missing".to_owned())?
            .entries
            .len();
        if child_start > loaded {
            return Ok(());
        }
        let target = child_start.saturating_add(ROW_PAGE_LIMIT);
        loop {
            let loaded = self
                .directory(inode)
                .ok_or_else(|| "mount directory is missing".to_owned())?
                .entries
                .len();
            let (exhausted, error) = match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (table.exhausted, table.error.clone()),
                _ => return Err("mount table directory is missing".into()),
            };
            if loaded >= target || exhausted {
                return Ok(());
            }
            if let Some(error) = error {
                return Err(error);
            }
            self.load_next_key_page(table_inode)?;
        }
    }

    fn readdir_page(
        &mut self,
        inode: u64,
        start: usize,
        mut visit: impl FnMut(usize, &DirectoryEntry) -> bool,
    ) -> Result<(), String> {
        self.ensure_readdir(inode, start)?;
        let directory = self
            .directory(inode)
            .ok_or_else(|| "mount directory is missing".to_owned())?;
        for (index, entry) in directory
            .entries
            .iter()
            .enumerate()
            .skip(start.saturating_sub(2))
        {
            if !visit(index, entry) {
                break;
            }
        }
        Ok(())
    }

    fn load_next_key_page(&mut self, table_inode: u64) -> Result<(), String> {
        let (source, lower, exhausted, error) =
            match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (
                    Arc::clone(&table.source),
                    table.lower.clone(),
                    table.exhausted,
                    table.error.clone(),
                ),
                _ => return Err("mount table directory is missing".into()),
            };
        if let Some(error) = error {
            return Err(error);
        }
        if exhausted {
            return Ok(());
        }
        let range = KeyRange::new(lower.clone(), None, ROW_PAGE_LIMIT)
            .map_err(|error| format!("row key range could not be created: {error:?}"))?;
        let scope = source
            .graph
            .open_read_scope()
            .map_err(|error| format!("row key read scope could not be opened: {error:?}"))?;
        let keys = match source.graph.range_row_keys(&range, &scope) {
            Ok(keys) => keys,
            Err(error) => {
                let error = format!("committed row keys could not be read: {error:?}");
                self.record_table_error(table_inode, &error);
                return Err(error);
            }
        };
        if keys.is_empty() {
            if let Some(NodeKind::Table(table)) =
                self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
            {
                table.exhausted = true;
            }
            return Ok(());
        }
        let page_length = keys.len();
        let next_lower = keys.last().cloned();
        if page_length >= ROW_PAGE_LIMIT && next_lower.as_ref() == lower.as_ref() {
            let error = "row-key pagination did not advance".to_owned();
            self.record_table_error(table_inode, &error);
            return Err(error);
        }
        let skip_duplicate = usize::from(lower.is_some());
        for key in keys.into_iter().skip(skip_duplicate) {
            if let Err(error) = self.add_key_path(table_inode, &source, key) {
                self.record_table_error(table_inode, &error);
                return Err(error);
            }
        }
        if let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
        {
            table.lower = next_lower;
            table.exhausted = page_length < ROW_PAGE_LIMIT;
        }
        Ok(())
    }

    fn record_table_error(&mut self, table_inode: u64, error: &str) {
        if let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
        {
            if table.error.is_none() {
                table.error = Some(error.to_owned());
                eprintln!("FUSE table row listing failed: {error}");
            }
        }
    }

    fn add_key_path(
        &mut self,
        table_inode: u64,
        source: &Arc<TableSource>,
        key: TypedKey,
    ) -> Result<(), String> {
        let mut components = Vec::new();
        flatten_key_components(&key, &mut components)?;
        let Some((leaf, prefix_components)) = components.split_last() else {
            return Err("committed row key has no path components".into());
        };
        let mut parent = table_inode;
        let mut prefix = Vec::with_capacity(prefix_components.len());
        for component in prefix_components {
            prefix.push(component.clone());
            parent = self.ensure_key_directory(
                parent,
                table_inode,
                &prefix,
                &key_component(component)?,
            )?;
        }
        self.ensure_row_directory(parent, source, key, &key_component(leaf)?)?;
        Ok(())
    }

    fn ensure_key_directory(
        &mut self,
        parent: u64,
        table_inode: u64,
        prefix: &[TypedKey],
        name: &str,
    ) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            let matches = self.nodes.get(&existing).is_some_and(|node| {
                matches!(
                    &node.kind,
                    NodeKind::KeyDirectory(directory)
                        if directory.table_inode == table_inode && directory.prefix == prefix
                )
            });
            return if matches {
                Ok(existing)
            } else {
                Err("committed row key path collides with another projected entry".into())
            };
        }
        self.insert_node(
            parent,
            name,
            NodeKind::KeyDirectory(KeyDirectory {
                directory: Directory::new(),
                table_inode,
                prefix: prefix.to_vec(),
            }),
            FileType::Directory,
        )
    }

    fn ensure_row_directory(
        &mut self,
        parent: u64,
        source: &Arc<TableSource>,
        key: TypedKey,
        name: &str,
    ) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            let matches = self
                .nodes
                .get(&existing)
                .is_some_and(|node| matches!(&node.kind, NodeKind::Row(row) if row.key == key));
            return if matches {
                Ok(existing)
            } else {
                Err("committed row key path collides with another projected entry".into())
            };
        }
        let indexed = match self.nodes.get_mut(&parent).map(|node| &mut node.kind) {
            Some(NodeKind::Table(table)) => table.indexed_rows.remove(OsStr::new(name)),
            _ => None,
        };
        if let Some(inode) = indexed {
            let matches = self
                .nodes
                .get(&inode)
                .is_some_and(|node| matches!(&node.kind, NodeKind::Row(row) if row.key == key));
            if !matches {
                return Err("indexed row key path collides with another projected entry".into());
            }
            let directory = self
                .directory_mut(parent)
                .ok_or_else(|| "mount row parent is not a directory".to_owned())?;
            let entry_name = OsString::from(name);
            let index = directory.entries.len();
            directory.by_name.insert(entry_name.clone(), index);
            directory.entries.push(DirectoryEntry {
                inode,
                name: entry_name,
                kind: FileType::Directory,
            });
            return Ok(inode);
        }
        self.insert_node(
            parent,
            name,
            NodeKind::Row(RowDirectory {
                directory: Directory::new(),
                source: Arc::clone(source),
                key,
                state: RowLoadState::Unloaded,
            }),
            FileType::Directory,
        )
    }

    fn ensure_row_loaded(&mut self, inode: u64) -> Result<(), String> {
        let (source, key, state) = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Row(row)) => (
                Arc::clone(&row.source),
                row.key.clone(),
                match &row.state {
                    RowLoadState::Unloaded => None,
                    RowLoadState::Ready => Some(Ok(())),
                    RowLoadState::Failed(error) => Some(Err(error.clone())),
                },
            ),
            _ => return Err("mount row directory is missing".into()),
        };
        if let Some(state) = state {
            return state;
        }
        let result = self.row_file_specs(&source, &key).and_then(|files| {
            let directory = self
                .directory(inode)
                .ok_or_else(|| "mount row directory is missing".to_owned())?;
            let mut names = HashMap::with_capacity(files.len());
            for (name, _) in &files {
                let name = OsString::from(name);
                if directory.entry(name.as_os_str()).is_some() || names.insert(name, ()).is_some() {
                    return Err("projected row file names collide".into());
                }
            }
            for (name, kind) in files {
                self.add_file(inode, &name, kind)?;
            }
            Ok(())
        });
        if let Some(NodeKind::Row(row)) = self.nodes.get_mut(&inode).map(|node| &mut node.kind) {
            row.state = match &result {
                Ok(()) => RowLoadState::Ready,
                Err(error) => RowLoadState::Failed(error.clone()),
            };
        }
        if let Err(error) = &result {
            eprintln!("FUSE row projection failed: {error}");
        }
        result
    }

    fn row_file_specs(
        &self,
        source: &Arc<TableSource>,
        key: &TypedKey,
    ) -> Result<Vec<(String, NodeKind)>, String> {
        // Runtime rows are joined by current table identity and the exact pinned
        // key. Rekeys have no row-lineage metadata, so they remain pinned here.
        let runtime_row = match (&self.writer, &source.runtime_table) {
            (Some(writer), Some(table)) => {
                writer.row_baseline(table, &runtime_table_key_bytes(key)?)?
            }
            _ => None,
        };
        self.project_row_files(source, key, runtime_row, None)
    }

    /// Projects one row's document and content files from the pinned row and
    /// the CWD row the runtime holds for it. A CWD row that fails validation is
    /// reported (EIO) rather than replaced by the pinned HEAD row (W08-08).
    /// `refreshed_baseline` reaches the document's save context.
    fn project_row_files(
        &self,
        source: &Arc<TableSource>,
        key: &TypedKey,
        runtime_row: Option<&[u8]>,
        refreshed_baseline: Option<Vec<u8>>,
    ) -> Result<Vec<(String, NodeKind)>, String> {
        let row_scope = source
            .graph
            .open_read_scope()
            .map_err(|error| format!("row read scope could not be opened: {error:?}"))?;
        let row = source
            .graph
            .lookup_row(key, &row_scope)
            .map_err(|error| format!("committed row could not be looked up: {error:?}"))?
            .ok_or_else(|| {
                "indexed committed row disappeared from the pinned snapshot".to_owned()
            })?;
        let decoded = source
            .graph
            .decode_admitted_row_fields(&row, &source.binding, &row_scope)
            .map_err(|error| format!("committed row fields could not be decoded: {error:?}"))?;
        let pinned_fields = decode_canonical_storage_value(decoded.canonical_fields())
            .map_err(|error| format!("committed row values could not be decoded: {error:?}"))?;
        if let Some(runtime_row) = runtime_row {
            let canonical_key = key
                .canonical_bytes()
                .map_err(|error| format!("committed row key cannot be encoded: {error:?}"))?;
            source
                .schema
                .descriptor()
                .validate_stored_row(*source.binding.relation_id(), &canonical_key, runtime_row)
                .map_err(|error| {
                    format!("the CWD row failed validation against the mounted schema: {error:?}")
                })?;
        }
        let (canonical_fields, _projection_source) = select_runtime_row_fields(
            runtime_row,
            pinned_fields,
            &source.pinned_field_layout.fields,
            source
                .runtime_field_layout
                .as_ref()
                .map(|layout| layout.fields.as_slice()),
            decoded.fields().iter().map(|field| *field.field_id()),
        )?;
        let Raw::Array(values) = canonical_fields else {
            return Err("committed row values are not a canonical field array".into());
        };
        if values.len() != decoded.fields().len() {
            return Err("committed row value and field counts differ".into());
        }

        let mut projected_fields = Vec::with_capacity(values.len());
        let mut blob_files = Vec::new();
        let mut blob_fields = Vec::new();
        for (field, value) in decoded.fields().iter().zip(values) {
            let field_name = source
                .field_names
                .get(field.field_id())
                .ok_or_else(|| "committed field has no source identity".to_owned())?;
            let field_type = source
                .pinned_field_layout
                .field_type(field.field_id())
                .ok_or_else(|| "committed field has no schema type".to_owned())?;
            // A selected Blob takes this branch even when the pinned field is a null
            // optional, so a CWD replacement renders. A committed Blob keeps the branch for
            // every non-null selection, so a selection that is not a Blob is still refused.
            if selects_blob(&value)
                || (field.blob_metadata().is_some() && !stored_value_is_null(&value))
            {
                // The selected value is the Blob the mount shows: the CWD value when
                // the runtime holds the row, otherwise the pinned value. A null
                // selection shows no Blob and takes the null branch below.
                let payload = selected_blob_payload(&value);
                let selected = blob_metadata_from_raw(payload).map_err(|error| {
                    format!("committed Blob field {field_name} holds no Blob value: {error:?}")
                })?;
                let value_oid = selected
                    .descriptor_oid()
                    .ok_or_else(|| "committed Blob field has no descriptor identity".to_owned())?;
                let algorithm = match value_oid.len() {
                    20 => GitHashAlgorithm::Sha1,
                    32 => GitHashAlgorithm::Sha256,
                    _ => return Err("committed Blob descriptor has an invalid OID width".into()),
                };
                let descriptor = NativeOid::new(algorithm, value_oid.as_bytes())
                    .map_err(|error| format!("committed Blob descriptor is invalid: {error:?}"))?;
                let read = source
                    .graph
                    .read_stored_blob_value(&row, &descriptor, &row_scope);
                let (media_type, suffix, reference) = match read {
                    Ok(stored) => {
                        // The pinned row names this descriptor, so its reference is the
                        // committed Blob. The media type and suffix are the selected value's:
                        // the descriptor carries no annotation, so a CWD row may annotate the
                        // same content differently from the pinned row. The descriptor fixes
                        // the content identity, so the selected length and digest must agree.
                        let identity = ContentIdentity::new(selected.length(), selected.sha256())
                            .map_err(|error| {
                                format!("committed Blob identity is invalid: {error:?}")
                            })?;
                        if stored.reference().content_identity() != identity {
                            return Err(format!(
                                "committed Blob field {field_name} does not match its selected identity"
                            ));
                        }
                        (
                            selected.media_type().to_owned(),
                            selected.suffix().map(str::to_owned),
                            stored.reference().clone(),
                        )
                    }
                    // A CWD replacement stores a descriptor the pinned row does not
                    // name. Only an accepted pin the runtime holds may supply it.
                    Err(GraphError::DescriptorNotInRow) => {
                        let reference = admit_accepted_replacement(
                            self.writer.as_deref(),
                            &source.graph,
                            &descriptor,
                            selected.length(),
                            selected.sha256(),
                            &row_scope,
                        )?;
                        (
                            selected.media_type().to_owned(),
                            selected.suffix().map(str::to_owned),
                            reference,
                        )
                    }
                    Err(error) => {
                        return Err(format!(
                            "committed Blob reference could not be admitted: {error:?}"
                        ));
                    }
                };
                let file_name = orna_runtime_v1::vfs::project_content_name(
                    field_name,
                    media_type.as_str(),
                    suffix.as_deref(),
                )
                .map_err(|error| {
                    format!(
                        "stored annotation of Blob field {field_name} is refused by MIME-1: {error:?}"
                    )
                })?;
                let projected = if is_optional_field(field_type) {
                    VfsStoredField::content_optional(
                        field_name.clone(),
                        media_type.as_str(),
                        suffix.clone(),
                        selected.length(),
                    )
                } else {
                    VfsStoredField::content(
                        field_name.clone(),
                        media_type.as_str(),
                        suffix.clone(),
                        selected.length(),
                    )
                };
                projected_fields.push(projected);
                blob_fields.push(BlobField {
                    name: field_name.clone(),
                    file_name: file_name.clone(),
                    media_type,
                    suffix,
                    reference: reference.clone(),
                    replacement_bytes: None,
                });
                blob_files.push((file_name, reference));
            } else if stored_value_is_null(&value) {
                projected_fields.push(VfsStoredField::null(field_name.clone()));
            } else {
                projected_fields.push(VfsStoredField::document(
                    field_name.clone(),
                    render_stored_source_value(&value)?,
                ));
            }
        }
        let projection = VfsRowProjection::project(
            SnapshotPin::capture(Arc::clone(&source.graph)),
            &projected_fields,
        )
        .map_err(|error| format!("committed row projection is invalid: {error:?}"))?;
        let mut files = Vec::with_capacity(projection.files().len() + 1);
        let document_bytes = projection.document_text().as_bytes().to_vec();
        let candidate_fields = blob_fields.clone();
        let document_save = match &self.writer {
            Some(writer) => Some(row_save_context(
                writer,
                source,
                key,
                Arc::clone(&self.table_object_ids),
                &document_bytes,
                blob_fields,
                refreshed_baseline,
            )?),
            None => None,
        };
        let document = match &document_save {
            Some(save) => NodeKind::Document {
                save: Arc::clone(save),
                bytes: document_bytes,
            },
            None => NodeKind::Bytes(document_bytes),
        };
        files.push(("data.orna".to_owned(), document));
        for file in projection.files() {
            let reference = blob_files
                .iter()
                .find_map(|(name, reference)| (name == file.name()).then_some(reference))
                .ok_or_else(|| "projected Blob has no admitted file reference".to_owned())?;
            let candidate = match &document_save {
                Some(save) => Some(BlobCandidate {
                    save: Arc::clone(save),
                    field: candidate_fields
                        .iter()
                        .find(|field| field.file_name == file.name())
                        .cloned()
                        .ok_or_else(|| "projected Blob has no admitted field".to_owned())?,
                }),
                None => None,
            };
            files.push((
                file.name().to_owned(),
                NodeKind::Blob {
                    graph: Arc::clone(&source.graph),
                    reference: reference.clone(),
                    candidate,
                },
            ));
        }
        Ok(files)
    }

    /// The mount-relative path of `inode`, as the user sees it.
    fn path_of(&self, inode: u64) -> Option<String> {
        let mut components = Vec::new();
        let mut current = inode;
        while current != ROOT_INODE {
            let parent = self.nodes.get(&current)?.parent;
            let parent_kind = &self.nodes.get(&parent)?.kind;
            let listed = parent_kind
                .directory()?
                .entries
                .iter()
                .find(|entry| entry.inode == current)
                .map(|entry| entry.name.clone());
            // A row reached through the key index is not in its table's page yet.
            let indexed = || match parent_kind {
                NodeKind::Table(table) => table
                    .indexed_rows
                    .iter()
                    .find(|(_, row)| **row == current)
                    .map(|(name, _)| name.clone()),
                _ => None,
            };
            components.push(listed.or_else(indexed)?.to_string_lossy().into_owned());
            current = parent;
        }
        components.reverse();
        Some(components.join("/"))
    }

    fn display_path(&self, inode: u64) -> String {
        self.path_of(inode)
            .unwrap_or_else(|| format!("inode {inode} (no longer in the mount)"))
    }

    /// Re-projects a document after a stale save (ORNA-VFS-011, W15-03). New
    /// opens then show the row another writer accepted and save against it;
    /// handles and scratch files opened earlier keep their own baseline and
    /// stay stale. Returns whether the document now shows a newer row.
    fn refresh_stale_document(
        &mut self,
        document_inode: u64,
        stale: &Arc<RowSaveContext>,
    ) -> Result<bool, String> {
        let save = match self.nodes.get(&document_inode).map(|node| &node.kind) {
            Some(NodeKind::Document { save, .. }) if Arc::ptr_eq(save, stale) => Arc::clone(save),
            // A later refresh or save already replaced this document's context.
            _ => return Ok(false),
        };
        let Some(table) = save.source.runtime_table.as_ref() else {
            return Ok(false);
        };
        let key = runtime_table_key_bytes(&save.key)?;
        let live_row = save.writer.live_row(table, &key)?;
        let expected = save
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if live_row == expected {
            return Ok(false);
        }
        let row = live_row
            .ok_or_else(|| "another writer deleted the row; remount to stop showing it".to_owned())?;
        let document = self
            .project_row_files(&save.source, &save.key, Some(&row), Some(row.clone()))?
            .into_iter()
            .find_map(|(name, kind)| (name == "data.orna").then_some(kind))
            .ok_or_else(|| "the refreshed row has no data.orna document".to_owned())?;
        if let Some(baselines) = &save.writer.row_baselines {
            baselines
                .record_accepted_row(table.table(), &key, &row)
                .map_err(|error| format!("the refreshed row baseline could not be recorded: {error}"))?;
        }
        let node = self
            .nodes
            .get_mut(&document_inode)
            .ok_or_else(|| "the refreshed document disappeared".to_owned())?;
        node.kind = document;
        Ok(true)
    }

    /// Refreshes a stale document and reports the outcome on stderr.
    fn report_refresh(&mut self, document_inode: u64, stale: &Arc<RowSaveContext>, path: &str) {
        match self.refresh_stale_document(document_inode, stale) {
            Ok(true) => eprintln!(
                "{path} was changed outside this mount; it now shows the current row"
            ),
            Ok(false) => {}
            Err(error) => eprintln!("{path}: the current row could not be shown: {error}"),
        }
    }

    fn add_file(&mut self, parent: u64, name: &str, kind: NodeKind) -> Result<u64, String> {
        if self
            .directory(parent)
            .is_none_or(|directory| directory.entry(OsStr::new(name)).is_some())
        {
            return Err(format!("mount path contains a duplicate file name: {name}"));
        }
        let file_type = match &kind {
            NodeKind::Directory(_)
            | NodeKind::Table(_)
            | NodeKind::KeyDirectory(_)
            | NodeKind::Row(_) => FileType::Directory,
            NodeKind::Bytes(_)
            | NodeKind::BlobBytes(_)
            | NodeKind::Document { .. }
            | NodeKind::Temporary { .. }
            | NodeKind::Blob { .. } => FileType::RegularFile,
        };
        self.insert_node(parent, name, kind, file_type)
    }

    fn insert_node(
        &mut self,
        parent: u64,
        name: &str,
        kind: NodeKind,
        file_type: FileType,
    ) -> Result<u64, String> {
        let inode = self.next_inode;
        let next_inode = self
            .next_inode
            .checked_add(1)
            .ok_or_else(|| "mount tree has too many entries".to_owned())?;
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| "mount parent directory is missing".to_owned())?;
        let directory = parent_node
            .kind
            .directory_mut()
            .ok_or_else(|| "mount parent is not a directory".to_owned())?;
        let name = OsString::from(name);
        if directory.entry(name.as_os_str()).is_some() {
            return Err("mount directory contains a duplicate entry".into());
        }
        let index = directory.entries.len();
        directory.by_name.insert(name.clone(), index);
        directory.entries.push(DirectoryEntry {
            inode,
            name,
            kind: file_type,
        });
        self.next_inode = next_inode;
        self.nodes.insert(inode, FsNode { parent, kind });
        Ok(inode)
    }

    /// Records a stateless open of a blob generation so a replacement keeps the captured
    /// node readable for as long as a handle still refers to it.
    fn track_blob_open(&mut self, inode: u64) {
        *self.open_blobs.entry(inode).or_insert(0) += 1;
    }

    /// Releases one stateless blob open. The last release of a superseded generation
    /// removes its node; untracked inodes are ignored.
    fn track_blob_release(&mut self, inode: u64) {
        let Some(count) = self.open_blobs.get_mut(&inode) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.open_blobs.remove(&inode);
            if self.superseded_blobs.remove(&inode) {
                self.nodes.remove(&inode);
            }
        }
    }

    /// Drops a generation that is no longer reachable by name. Open handles keep it
    /// readable (profiles/vfs-1.md: old handles remain on captured immutable content).
    ///
    /// Accepted FUSE nodeid-lifetime deviation: retention follows tracked open handles, not
    /// kernel lookup counts. `forget` and `batch_forget` keep fuser's no-op defaults, so a
    /// retired generation is removed once no handle holds it, even if the kernel still
    /// caches its nodeid. `ENTRY_TTL` is zero, so the kernel revalidates names and attributes
    /// on use; a stale nodeid then fails with ENOENT instead of serving retired bytes.
    fn retire_node(&mut self, inode: u64) {
        if self.open_blobs.contains_key(&inode) {
            self.superseded_blobs.insert(inode);
        } else {
            self.nodes.remove(&inode);
        }
    }

    /// Replaces the blob named `name` under `parent` with a new generation inode.
    /// The superseded inode keeps its captured content for existing handles.
    fn replace_blob_generation(
        &mut self,
        parent: u64,
        name: &OsStr,
        superseded: u64,
        bytes: Arc<[u8]>,
    ) -> Result<u64, String> {
        if !matches!(
            self.nodes.get(&superseded).map(|node| &node.kind),
            Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_))
        ) {
            return Err("replaced mount entry is not a blob".into());
        }
        let generation = self.next_inode;
        let next_inode = generation
            .checked_add(1)
            .ok_or_else(|| "mount tree has too many entries".to_owned())?;
        let directory = self
            .nodes
            .get_mut(&parent)
            .and_then(|node| node.kind.directory_mut())
            .ok_or_else(|| "mount parent is not a directory".to_owned())?;
        let index = *directory
            .by_name
            .get(name)
            .ok_or_else(|| "replaced blob is missing from its directory".to_owned())?;
        let entry = directory
            .entries
            .get_mut(index)
            .ok_or_else(|| "mount directory index is stale".to_owned())?;
        if entry.inode != superseded {
            return Err("replaced blob directory entry changed".into());
        }
        entry.inode = generation;
        self.next_inode = next_inode;
        self.nodes.insert(
            generation,
            FsNode {
                parent,
                kind: NodeKind::BlobBytes(bytes),
            },
        );
        self.retire_node(superseded);
        Ok(generation)
    }
}

/// A projected Blob's writable identity: the row save that accepts its candidate and the
/// admitted field it replaces (41-vfs.md EDIT-1).
struct BlobCandidate {
    save: Arc<RowSaveContext>,
    field: BlobField,
}

/// Reads a whole admitted Blob's bytes through a fresh read scope.
fn read_blob_bytes(
    graph: &NativeGraphContext,
    reference: &AdmittedBlobReference,
) -> Result<Vec<u8>, String> {
    let scope = graph
        .open_read_scope()
        .map_err(|error| format!("Blob read scope could not be opened: {error:?}"))?;
    let length = usize::try_from(reference.content_identity().length())
        .map_err(|_| "Blob length is not addressable".to_owned())?;
    Ok(graph
        .read_blob_range(reference, 0..length as u64, &scope)
        .map_err(|error| format!("Blob content could not be read: {error:?}"))?
        .bytes()
        .to_vec())
}

struct FsNode {
    parent: u64,
    kind: NodeKind,
}

enum NodeKind {
    Directory(Directory),
    Table(TableDirectory),
    KeyDirectory(KeyDirectory),
    Row(RowDirectory),
    Bytes(Vec<u8>),
    Document {
        bytes: Vec<u8>,
        save: Arc<RowSaveContext>,
    },
    Temporary {
        /// Cached candidate length; the bytes stay in the scratch store and are read by range.
        len: u64,
        save: Arc<RowSaveContext>,
        target: Arc<ManagedFile<RowMapSnapshot>>,
        scratch: Arc<TemporarySave<RowMapSnapshot>>,
        blob_field: Option<BlobField>,
        baseline: Option<Vec<u8>>,
        identity: Arc<TemporaryScratchIdentity>,
    },
    Blob {
        graph: Arc<NativeGraphContext>,
        reference: AdmittedBlobReference,
        /// The row's save context and this file's field; `None` on read-only mounts, where
        /// write-intent opens are refused (41-vfs.md EDIT-1 candidates require a writer).
        candidate: Option<BlobCandidate>,
    },
    BlobBytes(Arc<[u8]>),
}

impl NodeKind {
    fn directory(&self) -> Option<&Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&table.directory),
            Self::KeyDirectory(directory) => Some(&directory.directory),
            Self::Row(row) => Some(&row.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    fn directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&mut table.directory),
            Self::KeyDirectory(directory) => Some(&mut directory.directory),
            Self::Row(row) => Some(&mut row.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    fn is_directory(&self) -> bool {
        self.directory().is_some()
    }
}

struct Directory {
    entries: Vec<DirectoryEntry>,
    by_name: HashMap<OsString, usize>,
}

impl Directory {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    fn entry(&self, name: &OsStr) -> Option<&DirectoryEntry> {
        self.by_name
            .get(name)
            .and_then(|index| self.entries.get(*index))
    }
    fn remove_entry(&mut self, name: &OsStr) -> Option<DirectoryEntry> {
        let index = self.by_name.remove(name)?;
        let removed = self.entries.swap_remove(index);
        if let Some(moved) = self.entries.get(index) {
            self.by_name.insert(moved.name.clone(), index);
        }
        Some(removed)
    }
}

struct TableDirectory {
    directory: Directory,
    source: Arc<TableSource>,
    lower: Option<TypedKey>,
    exhausted: bool,
    error: Option<String>,
    indexed_rows: HashMap<OsString, u64>,
}

struct KeyDirectory {
    directory: Directory,
    table_inode: u64,
    prefix: Vec<TypedKey>,
}

struct RowDirectory {
    directory: Directory,
    source: Arc<TableSource>,
    key: TypedKey,
    state: RowLoadState,
}

enum RowLoadState {
    Unloaded,
    Ready,
    Failed(String),
}

struct TableSource {
    graph: Arc<NativeGraphContext>,
    rows: Arc<RowMapSnapshot>,
    binding: CommittedTableBinding,
    schema: ResolvedTableSchema,
    value_context: Format3Context,
    field_names: HashMap<[u8; 16], String>,
    pinned_field_layout: StoredFieldLayout,
    runtime_table: Option<RuntimeTableIdentity>,
    runtime_field_layout: Option<StoredFieldLayout>,
}
struct DirectoryEntry {
    inode: u64,
    name: OsString,
    kind: FileType,
}

fn flatten_key_components(key: &TypedKey, components: &mut Vec<TypedKey>) -> Result<(), String> {
    match key {
        TypedKey::Tuple(values) if values.is_empty() => {
            Err("composite row key has no components".into())
        }
        TypedKey::Tuple(values) => {
            for value in values {
                flatten_key_components(value, components)?;
            }
            Ok(())
        }
        _ => {
            components.push(key.clone());
            Ok(())
        }
    }
}

fn field_names_for_table(
    identities: &ObjectIdentityManifest,
    qualified_table: &str,
) -> HashMap<[u8; 16], String> {
    let prefix = format!("{qualified_table}.");
    identities
        .records()
        .iter()
        .filter(|identity| identity.kind() == ObjectKind::Field && !identity.is_retired())
        .filter_map(|identity| {
            identity
                .qualified_name()
                .strip_prefix(&prefix)
                .map(|name| (*identity.id(), name.to_owned()))
        })
        .collect()
}

fn stored_field_layout(descriptor: &SchemaDescriptor) -> Result<StoredFieldLayout, String> {
    let Raw::Map(schema) = descriptor.raw() else {
        return Err("committed table schema is not a canonical map".into());
    };
    let fields_key = Raw::Int(3.into());
    let fields = schema
        .iter()
        .find_map(|(key, value)| (key == &fields_key).then_some(value))
        .ok_or_else(|| "committed table schema has no fields".to_owned())?;
    let Raw::Array(fields) = fields else {
        return Err("committed table schema fields are not an array".into());
    };
    let stored_role = Raw::Int(1.into());
    let mut stored_fields = Vec::with_capacity(fields.len());
    let mut indexes = HashMap::with_capacity(fields.len());
    for field in fields {
        let Raw::Array(parts) = field else {
            return Err("committed table schema field is not an array".into());
        };
        let [Raw::Tag(37, identity), _, field_type, role, _] = parts.as_slice() else {
            return Err("committed table schema field is malformed".into());
        };
        if role != &stored_role {
            continue;
        }
        let Raw::Bytes(identity) = identity.as_ref() else {
            return Err("committed field identity is not a UUID".into());
        };
        let identity: [u8; 16] = identity
            .as_slice()
            .try_into()
            .map_err(|_| "committed field identity has the wrong width")?;
        let index = stored_fields.len();
        if indexes.insert(identity, index).is_some() {
            return Err("committed table schema repeats a stored field".into());
        }
        stored_fields.push(StoredFieldShape {
            field_id: identity,
            field_type: field_type.clone(),
        });
    }
    Ok(StoredFieldLayout {
        fields: stored_fields,
        indexes,
    })
}

fn is_optional_field(field_type: &Raw) -> bool {
    matches!(field_type, Raw::Array(parts) if parts.len() == 2 && parts[0] == Raw::Int(2.into()))
}

fn stored_value_is_null(value: &Raw) -> bool {
    matches!(value, Raw::Null)
        || matches!(value, Raw::Tag(60013, option) if matches!(option.as_ref(), Raw::Array(parts) if matches!(parts.as_slice(), [Raw::Int(index)] if index.to_string() == "0")))
}

/// The Blob payload a row field value selects: a present optional's payload, or the bare Blob.
fn selected_blob_payload(value: &Raw) -> &Raw {
    match value {
        Raw::Tag(60013, option) => match option.as_ref() {
            Raw::Array(parts) => match parts.as_slice() {
                [Raw::Int(index), payload] if index.to_string() == "1" => payload,
                _ => value,
            },
            _ => value,
        },
        _ => value,
    }
}

/// Whether a selected row value shows a Blob: a bare Blob or a present optional's Blob.
fn selects_blob(value: &Raw) -> bool {
    matches!(selected_blob_payload(value), Raw::Tag(orna_value_v1::ROV3_BLOB_TAG, _))
}

/// Admits the accepted pin that holds the selected replacement content for a descriptor the
/// pinned row does not name. Every pin matching the selected length and SHA-256 is tried in
/// turn; when none is admitted the Blob is refused rather than served from the committed row.
fn admit_accepted_replacement(
    writer: Option<&MountWriter>,
    graph: &NativeGraphContext,
    descriptor: &NativeOid,
    length: u64,
    sha256: [u8; 32],
    scope: &RepositoryReadScope,
) -> Result<AdmittedBlobReference, String> {
    let writer = writer.ok_or_else(|| {
        "committed Blob content is absent from the row and no CWD runtime holds it".to_owned()
    })?;
    let pins = writer
        .runtime
        .block_on(writer.state.accepted_content_pins_for_descriptor(descriptor))
        .map_err(|error| format!("accepted content pins could not be listed: {error:?}"))?;
    let identity = ContentIdentity::new(length, sha256)
        .map_err(|error| format!("committed Blob identity is invalid: {error:?}"))?;
    let mut refusal = "no accepted pin holds the selected Blob content".to_owned();
    for pin in pins
        .iter()
        .filter(|pin| pin.content_length() == length && *pin.content_sha256() == sha256)
    {
        match graph.admit_accepted_blob_pin(pin.pin_id(), descriptor, identity, scope) {
            Ok(reference) => return Ok(reference),
            Err(error) => refusal = format!("accepted Blob pin could not be admitted: {error:?}"),
        }
    }
    Err(refusal)
}

fn render_stored_source_value(value: &Raw) -> Result<String, String> {
    match value {
        Raw::Null => Ok("null".into()),
        Raw::Bool(value) => Ok(value.to_string()),
        Raw::Int(value) => Ok(value.to_string()),
        Raw::Float(bits) => {
            let value = f64::from_bits(*bits);
            if !value.is_finite() {
                return Err("committed row contains a non-finite source number".into());
            }
            Ok(format!("{value:?}f"))
        }
        Raw::Text(value) => serde_json::to_string(value)
            .map_err(|error| format!("stored string could not be rendered: {error}")),
        Raw::Bytes(_) => {
            Err("committed row contains a byte value without a source spelling".into())
        }
        Raw::Array(values) => {
            let values = values
                .iter()
                .map(render_stored_source_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", values.join(", ")))
        }
        Raw::Map(entries) => {
            let fields = entries
                .iter()
                .map(|(key, value)| {
                    let Raw::Text(name) = key else {
                        return Err("stored record has a non-text field name".to_owned());
                    };
                    Ok(format!("{name}: {}", render_stored_source_value(value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{{ {} }}", fields.join(", ")))
        }
        Raw::Tag(60000, decimal) => {
            let Raw::Array(parts) = decimal.as_ref() else {
                return Err("stored decimal has an invalid payload".into());
            };
            let [Raw::Int(coefficient), Raw::Int(exponent)] = parts.as_slice() else {
                return Err("stored decimal has an invalid payload".into());
            };
            Ok(format!("{coefficient}e{exponent}.decimal"))
        }
        Raw::Tag(60001, date) => match date.as_ref() {
            Raw::Text(date) => Ok(date.clone()),
            _ => Err("stored date has an invalid payload".into()),
        },
        Raw::Tag(60002, instant) => render_stored_instant(instant),
        Raw::Tag(60013, option) => {
            let Raw::Array(parts) = option.as_ref() else {
                return Err("stored optional field has an invalid payload".into());
            };
            match parts.as_slice() {
                [Raw::Int(index)] if index.to_string() == "0" => Ok("null".into()),
                [Raw::Int(index), value] if index.to_string() == "1" => {
                    render_stored_source_value(value)
                }
                _ => Err("stored optional field has an invalid value".into()),
            }
        }
        Raw::Tag(60014, _) => Ok("()".into()),
        Raw::Tag(60015, tuple) => {
            let Raw::Array(values) = tuple.as_ref() else {
                return Err("stored tuple has an invalid payload".into());
            };
            let values = values
                .iter()
                .map(render_stored_source_value)
                .collect::<Result<Vec<_>, _>>()?;
            let mut body = values.join(", ");
            if values.len() == 1 {
                body.push(',');
            }
            Ok(format!("({body})"))
        }
        Raw::Tag(60009, record) => {
            let Raw::Array(parts) = record.as_ref() else {
                return Err("stored record has an invalid payload".into());
            };
            let [_, Raw::Array(entries)] = parts.as_slice() else {
                return Err("stored record has an invalid payload".into());
            };
            let fields = entries
                .iter()
                .map(|entry| {
                    let Raw::Array(parts) = entry else {
                        return Err("stored record field is malformed".to_owned());
                    };
                    let [Raw::Text(name), value] = parts.as_slice() else {
                        return Err("stored record field has no source name".to_owned());
                    };
                    Ok(format!("{name}: {}", render_stored_source_value(value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{{ {} }}", fields.join(", ")))
        }
        Raw::Tag(tag, _) => Err(format!(
            "committed row value tag {tag} has no source spelling"
        )),
    }
}

fn render_stored_instant(instant: &Raw) -> Result<String, String> {
    let Raw::Array(parts) = instant else {
        return Err("stored instant has an invalid payload".into());
    };
    let [Raw::Int(seconds), Raw::Int(nanoseconds)] = parts.as_slice() else {
        return Err("stored instant has an invalid payload".into());
    };
    let seconds = seconds
        .to_string()
        .parse::<i64>()
        .map_err(|_| "stored instant seconds are out of range")?;
    let nanoseconds = nanoseconds
        .to_string()
        .parse::<u32>()
        .map_err(|_| "stored instant nanoseconds are out of range")?;
    if nanoseconds >= 1_000_000_000 {
        return Err("stored instant nanoseconds are out of range".into());
    }
    let day = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, date) = civil_date_from_days(day);
    if !(0..=9999).contains(&year) {
        return Err("stored instant is outside the source calendar range".into());
    }
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    let mut rendered = format!("{year:04}-{month:02}-{date:02}T{hour:02}:{minute:02}:{second:02}");
    if nanoseconds != 0 {
        let mut fraction = format!("{nanoseconds:09}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        rendered.push('.');
        rendered.push_str(&fraction);
    }
    rendered.push('Z');
    Ok(rendered)
}

fn civil_date_from_days(days: i64) -> (i64, i64, i64) {
    let adjusted = days + 719_468;
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted - 146_096
    } / 146_097;
    let day_of_era = adjusted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, date)
}

fn key_component(key: &TypedKey) -> Result<String, String> {
    let canonical_text = match key {
        TypedKey::Null => "null".to_owned(),
        TypedKey::Bool(value) => value.to_string(),
        TypedKey::UInt(value) => value.to_string(),
        TypedKey::Int(value) => value.to_string(),
        TypedKey::Bytes(bytes) => format!("0x{}", hex_bytes(bytes)),
        TypedKey::Text(value) => value.clone(),
        TypedKey::Tuple(_) => {
            return Err("tuple key must be projected component by component".into());
        }
        TypedKey::Value(value) => value
            .canonical_text()
            .ok_or_else(|| "key type needs its schema to project".to_owned())?,
    };
    // A long key's `~key-` alias digests the typed component's canonical
    // OVB-1 encoding, not its text (VFS-1, "Row/key directory mapping").
    let canonical_encoding = key
        .canonical_bytes()
        .map_err(|_| "key component has no canonical encoding".to_owned())?;
    Ok(project_key_component(&canonical_text, &canonical_encoding))
}

/// Checks every captured Blob field of a candidate `data.orna` before its
/// descriptors are replaced by owner bindings (A15-04, ORNA-VFS-004,
/// ORNA-VFS-012). `data.orna` is a complete document, so a missing Blob field
/// fails like any missing stored field. The descriptor must stay exactly as
/// projected: its path names the field's own sibling file and its media type
/// describes the captured content. Content changes by renaming a file over the
/// sibling, so any other descriptor is refused instead of acknowledged and then
/// dropped.
fn validate_blob_descriptors(source: &str, fields: &[BlobField]) -> Result<(), String> {
    if fields.is_empty() {
        return Ok(());
    }
    let parsed = parse_row(source);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record { fields: entries, .. } = &parsed.value else {
        return Err("mounted row document is not a record".into());
    };
    for field in fields {
        let name = &field.name;
        let Some(entry) = entries.iter().find(|entry| &entry.name == name) else {
            return Err(format!(
                "Blob field {name} is missing; data.orna must keep its descriptor"
            ));
        };
        let Expr::Record {
            fields: descriptor, ..
        } = &entry.value
        else {
            return Err(format!(
                "Blob field {name} must keep its {{ path, media_type }} descriptor"
            ));
        };
        let mut path = None;
        let mut media_type = None;
        for part in descriptor {
            let value = plain_string_literal(&part.value).ok_or_else(|| {
                format!("Blob field {name} descriptor entries must be plain strings")
            })?;
            match part.name.as_str() {
                "path" if path.is_none() => path = Some(value),
                "media_type" if media_type.is_none() => media_type = Some(value),
                other => {
                    return Err(format!(
                        "Blob field {name} descriptor has an unsupported entry {other}"
                    ));
                }
            }
        }
        let expected_path = format!("./{}", field.file_name);
        if path != Some(expected_path.as_str()) {
            return Err(format!(
                "Blob field {name} must keep path \"{expected_path}\"; replace its content by renaming a file over {}",
                field.file_name
            ));
        }
        if media_type != Some(field.media_type.as_str()) {
            return Err(format!(
                "Blob field {name} must keep media_type \"{}\" of its captured content",
                field.media_type
            ));
        }
    }
    Ok(())
}

/// The value of a string literal written without escapes, the only spelling a
/// projected descriptor uses.
fn plain_string_literal(value: &Expr) -> Option<&str> {
    match value {
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => text
            .strip_prefix('"')?
            .strip_suffix('"')
            .filter(|body| !body.contains('\\')),
        _ => None,
    }
}

/// Removes projected Blob summary fields from a source row before row admission.
/// The selected values are supplied separately by owner-issued Blob bindings.
fn source_without_fields(source: &str, omitted: &[String]) -> Result<String, String> {
    let parsed = parse_row(source);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return Err("mounted row document is not a record".into());
    };
    let mut ranges = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if !omitted.iter().any(|name| name == &field.name) {
            continue;
        }
        let start = field.span.start;
        let end = field.span.end;
        let next_start = fields
            .get(index + 1)
            .map_or(source.len(), |next| next.span.start);
        if let Some(relative_comma) = source[end..next_start].find(',') {
            ranges.push(start..end + relative_comma + 1);
        } else if index > 0 {
            let previous_end = fields[index - 1].span.end;
            let relative_comma = source[previous_end..start]
                .rfind(',')
                .ok_or_else(|| "mounted row field separator is missing".to_owned())?;
            ranges.push(previous_end + relative_comma..end);
        } else {
            ranges.push(start..end);
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut output = source.to_owned();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    Ok(output)
}

fn is_replacement_scratch_for(name: &str, file_name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_prefix(file_name))
        .and_then(|name| name.strip_prefix('.'))
    else {
        return false;
    };
    !suffix.is_empty() && suffix != "." && suffix != ".." && !suffix.contains('/') && suffix != "/"
}
fn hex_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
fn key_directory(builder: &mut FsBuilder, parent: u64, key: &TypedKey) -> Result<u64, String> {
    let mut components = Vec::new();
    flatten_key_components(key, &mut components)?;
    let mut directory = parent;
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.clone());
        directory = builder.ensure_key_directory(
            directory,
            parent,
            &prefix,
            &key_component(&component)?,
        )?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    #[test]
    fn candidate_read_is_chunked_and_stops_at_candidate_end() {
        use super::{CANDIDATE_CHUNK_BYTES, read_candidate_range};
        let total = CANDIDATE_CHUNK_BYTES * 2 + 5;
        let bytes: Vec<u8> = (0..total).map(|index| (index % 251) as u8).collect();
        let mut requests = Vec::new();
        let read = read_candidate_range(
            |offset, length| {
                requests.push(length);
                let start = (offset as usize).min(bytes.len());
                let end = (start + length).min(bytes.len());
                Ok::<_, ()>(bytes[start..end].to_vec())
            },
            CANDIDATE_CHUNK_BYTES as u64 / 2,
            (total + 10) as u32,
        )
        .unwrap();
        assert_eq!(read, bytes[CANDIDATE_CHUNK_BYTES / 2..].to_vec());
        assert!(requests.iter().all(|length| *length <= CANDIDATE_CHUNK_BYTES));
        assert!(requests.len() >= 3);
    }

    #[test]
    fn candidate_read_propagates_backing_errors() {
        use super::read_candidate_range;
        let failed = read_candidate_range(|_, _| Err::<Vec<u8>, _>("backing"), 0, 8);
        assert_eq!(failed, Err("backing"));
    }

    #[test]
    fn truncated_and_stashed_ranges_clamp_to_candidate_size() {
        use super::{RetentionError, stashed_range, truncated_window};
        let baseline: &[u8] = b"abcdef";
        let window = |size: u64, offset: u64, length: usize| {
            truncated_window(baseline.len() as u64, size, offset, length, |start, count| {
                Ok::<_, RetentionError>(stashed_range(baseline, start, count))
            })
            .unwrap()
        };
        assert_eq!(window(4, 0, 10), b"abcd".to_vec());
        assert_eq!(window(8, 4, 10), vec![b'e', b'f', 0, 0]);
        assert_eq!(window(4, 6, 10), Vec::<u8>::new());
        assert!(truncated_window(6, 4, 0, 4, |_, _| Err(RetentionError::from(String::from("boom")))).is_err());
        assert_eq!(stashed_range(baseline, 2, 3), b"cde".to_vec());
        assert_eq!(stashed_range(baseline, 9, 3), Vec::<u8>::new());
    }

    #[test]
    fn reply_read_size_caps_each_reply_at_one_chunk() {
        let chunk = u32::try_from(super::CANDIDATE_CHUNK_BYTES).unwrap_or(u32::MAX);
        assert_eq!(super::reply_read_size(u32::MAX), chunk);
        assert_eq!(super::reply_read_size(5), 5);
    }

    #[test]
    fn quota_exceeded_maps_to_edquot_and_backing_io_to_eio() {
        use orna_runtime_v1::vfs::EditDraftError;
        assert_eq!(
            super::draft_errno(&Err(EditDraftError::QuotaExceeded)).code(),
            122
        );
        assert_eq!(
            super::draft_errno(&Err(EditDraftError::FileTooLarge)).code(),
            122
        );
        assert_eq!(
            super::draft_errno(&Err(EditDraftError::BackingIo)).code(),
            5
        );
    }

    use std::ffi::OsStr;

    use super::{
        Expr, FsBuilder, NodeKind, ROOT_INODE, RowProjectionSource, StoredFieldShape,
        is_replacement_scratch_for, key_directory, parse_row, runtime_row_baseline_from_rows,
        runtime_table_key_bytes,
        select_runtime_row_fields, source_without_fields,
    };
    use super::{EDITABLE_OPEN_FLAGS, EDITOR_INIT_CAPABILITIES, ROW_PAGE_LIMIT, WorkspaceFilesystem};
    use orna_repository_v1::TypedKey;
    use orna_runtime_v1::{
        RuntimeTableIdentity, RuntimeTableRows, TableObjectId, vfs::project_component,
    };
    use orna_value_v1::{Raw, Value};
    fn encode_fields(fields: Vec<Raw>) -> Vec<u8> {
        Value::new(Raw::Array(fields)).unwrap().encode().unwrap()
    }

    #[test]
    fn renamed_tables_use_current_identity_while_rekeys_keep_the_pinned_boundary() {
        let id = [1; 16];
        let current_table =
            RuntimeTableIdentity::new("Renamed", TableObjectId::new(id)).unwrap();
        let pinned_table =
            RuntimeTableIdentity::new("OldName", TableObjectId::new(id)).unwrap();
        let layout = [StoredFieldShape {
            field_id: id,
            field_type: Raw::Text("Str".into()),
        }];
        let pinned = Raw::Array(vec![Raw::Text("pinned value".into())]);
        let current_bytes = encode_fields(vec![Raw::Text("current value".into())]);
        let renamed_rows: RuntimeTableRows = [(
            "Renamed".to_owned(),
            vec![(b"same-key".to_vec(), current_bytes.clone())],
        )]
        .into();

        let current_row =
            runtime_row_baseline_from_rows(&renamed_rows, &current_table, b"same-key").unwrap();
        assert_eq!(current_row, Some(current_bytes.as_slice()));
        let (projected, source) = select_runtime_row_fields(
            current_row,
            pinned.clone(),
            &layout,
            Some(&layout),
            [id].into_iter(),
        )
        .unwrap();
        assert_eq!(source, RowProjectionSource::RuntimeSnapshot);
        assert_eq!(
            projected,
            Raw::Array(vec![Raw::Text("current value".into())])
        );
        assert_eq!(
            runtime_row_baseline_from_rows(&renamed_rows, &pinned_table, b"same-key"),
            None
        );

        let rekeyed_rows: RuntimeTableRows = [(
            "Renamed".to_owned(),
            vec![(b"new-key".to_vec(), current_bytes)],
        )]
        .into();
        let current =
            runtime_row_baseline_from_rows(&rekeyed_rows, &current_table, b"old-key").unwrap();
        assert_eq!(current, None);
        let (projected, source) = select_runtime_row_fields(
            current,
            pinned.clone(),
            &layout,
            Some(&layout),
            [id].into_iter(),
        )
        .unwrap();
        assert_eq!(source, RowProjectionSource::PinnedSnapshot);
        assert_eq!(projected, pinned);
    }

    /// W08-08: a CWD row whose stored layout no longer matches the mounted
    /// schema is reported, never replaced by the pinned HEAD row.
    #[test]
    fn equal_length_field_order_and_schema_changes_report_runtime_rows() {
        let pinned_layout = [
            StoredFieldShape {
                field_id: [1; 16],
                field_type: Raw::Text("Str".into()),
            },
            StoredFieldShape {
                field_id: [2; 16],
                field_type: Raw::Text("Int".into()),
            },
        ];
        let runtime_bytes = encode_fields(vec![Raw::Text("title".into()), Raw::Int(7.into())]);
        let pinned_fields = Raw::Array(vec![Raw::Text("old title".into()), Raw::Int(6.into())]);
        let reordered_layout = [pinned_layout[1].clone(), pinned_layout[0].clone()];
        let changed_schema_layout = [
            pinned_layout[0].clone(),
            StoredFieldShape {
                field_id: [2; 16],
                field_type: Raw::Text("Bool".into()),
            },
        ];

        for runtime_layout in [&reordered_layout[..], &changed_schema_layout[..]] {
            let projected = select_runtime_row_fields(
                Some(&runtime_bytes),
                pinned_fields.clone(),
                &pinned_layout,
                Some(runtime_layout),
                [[1; 16], [2; 16]].into_iter(),
            );
            assert!(
                projected.is_err(),
                "a CWD row in another layout must not project as HEAD: {projected:?}"
            );
        }
        // ORNA-VFS-001: a row is validated under the mounted snapshot's schema, so
        // with no runtime layout a valid CWD row still projects from the CWD.
        let unresolved_schema = select_runtime_row_fields(
            Some(&runtime_bytes),
            pinned_fields,
            &pinned_layout,
            None,
            [[1; 16], [2; 16]].into_iter(),
        );
        let (projected, source) =
            unresolved_schema.expect("a CWD row valid under the mounted schema projects");
        assert_eq!(source, RowProjectionSource::RuntimeSnapshot);
        assert_eq!(
            projected,
            Raw::Array(vec![Raw::Text("title".into()), Raw::Int(7.into())])
        );
    }

    /// W08-08: an undecodable CWD row is reported while the pinned HEAD row
    /// exists; the projection never falls back to that HEAD row, whether or not
    /// a runtime layout is resolved.
    #[test]
    fn undecodable_cwd_row_is_reported_when_a_head_row_exists() {
        let pinned_layout = [
            StoredFieldShape {
                field_id: [1; 16],
                field_type: Raw::Text("Str".into()),
            },
            StoredFieldShape {
                field_id: [2; 16],
                field_type: Raw::Text("Int".into()),
            },
        ];
        let pinned_fields = Raw::Array(vec![Raw::Text("old title".into()), Raw::Int(6.into())]);
        for runtime_layout in [None, Some(&pinned_layout[..])] {
            let projected = select_runtime_row_fields(
                Some(&[0xff]),
                pinned_fields.clone(),
                &pinned_layout,
                runtime_layout,
                [[1; 16], [2; 16]].into_iter(),
            );
            let error = projected.expect_err("an undecodable CWD row is reported");
            assert!(
                error.contains("cannot be decoded"),
                "an undecodable CWD row must not project HEAD: {error}"
            );
        }
    }

    #[test]
    fn lazy_row_lookups_share_the_captured_runtime_generation() {
        let generation_one: RuntimeTableRows = [(
            "Music".to_owned(),
            vec![
                (b"one".to_vec(), encode_fields(vec![Raw::Text("one-v1".into())])),
                (b"two".to_vec(), encode_fields(vec![Raw::Text("two-v1".into())])),
            ],
        )]
        .into();
        let generation_two: RuntimeTableRows = [(
            "Music".to_owned(),
            vec![
                (b"one".to_vec(), encode_fields(vec![Raw::Text("one-v2".into())])),
                (b"two".to_vec(), encode_fields(vec![Raw::Text("two-v2".into())])),
            ],
        )]
        .into();
        let table = RuntimeTableIdentity::new("Music", TableObjectId::new([3; 16])).unwrap();
        let mount_snapshot = std::sync::OnceLock::new();
        assert!(mount_snapshot.set(generation_one).is_ok());

        let first = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"one")
            .unwrap()
            .unwrap();
        let second = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"two")
            .unwrap()
            .unwrap();
        let later_second = runtime_row_baseline_from_rows(&generation_two, &table, b"two")
            .unwrap()
            .unwrap();
        assert_eq!(
            Value::decode(first).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("one-v1".into())]),
        );
        assert_eq!(
            Value::decode(second).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("two-v1".into())]),
        );
        assert_eq!(
            Value::decode(later_second).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("two-v2".into())]),
        );
    }

    #[test]
    fn runtime_table_keys_use_persisted_text_bytes_and_typed_encoding_otherwise() {
        assert_eq!(
            runtime_table_key_bytes(&TypedKey::Text("london".into())).unwrap(),
            b"london"
        );

        let numeric = TypedKey::Int(7);
        assert_eq!(
            runtime_table_key_bytes(&numeric).unwrap(),
            numeric.canonical_bytes().unwrap()
        );
    }
    #[test]
    fn source_row_blob_fields_are_omitted_for_owner_bindings() {
        let source = r#"{ title: "song", content: { path: "./content.mp3", media_type: "audio/mpeg" }, artist: "A", }"#;
        let without_content = source_without_fields(source, &["content".to_owned()]).unwrap();
        let parsed = parse_row(&without_content);
        assert!(parsed.diagnostics.is_empty());
        let Expr::Record { fields, .. } = parsed.value else {
            panic!("row remains a record");
        };
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["title", "artist"]
        );
    }

    #[test]
    fn blob_replacement_scratch_names_stay_bound_to_their_target() {
        assert!(is_replacement_scratch_for(
            ".content.mp3.tmp",
            "content.mp3"
        ));
        assert!(is_replacement_scratch_for(
            ".content.mp3.12345",
            "content.mp3"
        ));
        assert!(!is_replacement_scratch_for("unrelated", "content.mp3"));
        assert!(!is_replacement_scratch_for(".content.mp3.", "content.mp3"));
    }

    #[test]
    fn table_and_text_key_paths_use_runtime_components() {
        let mut builder = FsBuilder::new();
        let table = builder.table_directory("Music", "data.orna").unwrap();
        let row = key_directory(&mut builder, table, &TypedKey::Text("".into())).unwrap();

        let NodeKind::Directory(root) = &builder.nodes.get(&ROOT_INODE).unwrap().kind else {
            panic!("root is a directory");
        };
        let module = root.entry(OsStr::new("Music")).unwrap();
        let NodeKind::Directory(tables) = &builder.nodes.get(&module.inode).unwrap().kind else {
            panic!("module path is a directory");
        };
        let table_entry = tables
            .entry(OsStr::new(&project_component("data.orna")))
            .unwrap();
        assert_eq!(table_entry.inode, table);
        let NodeKind::Directory(rows) = &builder.nodes.get(&table).unwrap().kind else {
            panic!("table path is a directory");
        };
        let row_entry = rows.entry(OsStr::new(&project_component(""))).unwrap();
        assert_eq!(row_entry.inode, row);
        assert!(matches!(
            builder.nodes.get(&row).unwrap().kind,
            NodeKind::KeyDirectory(_)
        ));
    }

    #[test]
    fn composite_keys_use_one_projected_path_component_per_part() {
        let mut builder = FsBuilder::new();
        let table = builder.table_directory("Music", "Music").unwrap();
        let row = key_directory(
            &mut builder,
            table,
            &TypedKey::Tuple(vec![TypedKey::Text("live/in".into()), TypedKey::Int(7)]),
        )
        .unwrap();

        let NodeKind::Directory(table_entries) = &builder.nodes.get(&table).unwrap().kind else {
            panic!("table path is a directory");
        };
        let first = table_entries
            .entry(OsStr::new(&project_component("live/in")))
            .unwrap();
        let NodeKind::KeyDirectory(first_entries) = &builder.nodes.get(&first.inode).unwrap().kind
        else {
            panic!("first key component is a directory");
        };
        let second = first_entries
            .directory
            .entry(OsStr::new(&project_component("7")))
            .unwrap();
        assert_eq!(second.inode, row);
    }
    #[test]
    fn editor_open_flags_bypass_kernel_cache_without_writeback_capability() {
        assert!(EDITOR_INIT_CAPABILITIES.contains(fuser::InitFlags::FUSE_ATOMIC_O_TRUNC));
        assert!(!EDITOR_INIT_CAPABILITIES.contains(fuser::InitFlags::FUSE_WRITEBACK_CACHE));
        assert_eq!(EDITABLE_OPEN_FLAGS, fuser::FopenFlags::FOPEN_DIRECT_IO);
    }

    #[tokio::test]
    async fn workspace_readdir_uses_lazy_indexed_pages() {
        const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
        let fixture_rows = (0..ROW_PAGE_LIMIT)
            .map(|index| {
                let key = format!("row-{index:03}");
                let source = format!("{{ title: \"{key}\", }}\n");
                (key, source)
            })
            .collect::<Vec<_>>();
        let (_repository_directory, repository, relation_id, format_context) =
            crate::format3::format3_repository_with_context(SOURCE, "Music");
        let (identity, digest) = crate::format3::cli_runtime_identity(&repository);
        let state = orna_runtime_v1::RuntimeState::open(&repository, identity, digest)
            .await
            .unwrap();
        let writer = state.acquire_lease(identity.repository_id).await.unwrap();
        let mut bindings = orna_evaluator_v1::SysHostBindingRegistry::new(
            orna_sys_v1::EnvironmentProvider::default(),
        );
        let captures = fixture_rows
            .iter()
            .enumerate()
            .map(|(index, (key, source))| crate::media_commit::SourceCaptureRow {
                key,
                blob_fields: &[],
                source_row: source,
                ordinal: u16::try_from(index).unwrap(),
                insert_only: true,
            })
            .collect::<Vec<_>>();
        crate::media_commit::commit_source_captures_for_table(
            &repository,
            &state,
            writer,
            &mut bindings,
            relation_id,
            &format_context,
            "Music",
            &captures,
        )
        .await;
        drop(state);

        let context = repository.open_format_context_at_selector("HEAD").unwrap();
        let filesystem = WorkspaceFilesystem::from_snapshot(
            &repository,
            context.commit(),
            &context,
            None,
        )
        .unwrap();
        let mut builder = filesystem.state.lock().unwrap();
        let table_inode = builder
            .directory(ROOT_INODE)
            .unwrap()
            .entry(OsStr::new("Music"))
            .unwrap()
            .inode;

        let indexed_name = project_component("row-255");
        let indexed_inode = builder
            .lookup_child(table_inode, OsStr::new(&indexed_name))
            .unwrap()
            .expect("indexed row lookup finds a stored text key");
        let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
            panic!("projected Music path is a table directory");
        };
        assert!(table.directory.entries.is_empty(), "lookup does not materialize a readdir page");
        assert!(table.lower.is_none(), "indexed lookup leaves the page cursor untouched");
        assert_eq!(table.indexed_rows.len(), 1, "the direct lookup is retained outside the ordered page");

        let mut first_page = Vec::with_capacity(ROW_PAGE_LIMIT);
        builder
            .readdir_page(table_inode, 0, |_, entry| {
                first_page.push(entry.name.to_string_lossy().into_owned());
                true
            })
            .unwrap();
        assert_eq!(first_page.len(), ROW_PAGE_LIMIT);
        assert_eq!(first_page.first().map(String::as_str), Some("row-000"));
        assert_eq!(first_page.last().map(String::as_str), Some("row-255"));
        assert!(builder.nodes.get(&indexed_inode).is_some());
        let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
            panic!("projected Music path is a table directory");
        };
        assert_eq!(table.directory.entries.len(), ROW_PAGE_LIMIT);
        assert!(table.indexed_rows.is_empty(), "enumeration absorbs the cached indexed row in key order");
        assert!(!table.exhausted, "a full page does not prove that another page is absent");

        let mut next_page = Vec::new();
        builder
            .readdir_page(table_inode, ROW_PAGE_LIMIT + 2, |_, entry| {
                next_page.push(entry.name.to_string_lossy().into_owned());
                true
            })
            .unwrap();
        assert!(next_page.is_empty(), "the exact page boundary does not duplicate the inclusive cursor");
        let NodeKind::Table(table) = &builder.nodes.get(&table_inode).unwrap().kind else {
            panic!("projected Music path is a table directory");
        };
        assert!(table.exhausted, "the empty final page marks the directory exhausted");
        assert_eq!(
            builder
                .lookup_child(table_inode, OsStr::new(&indexed_name))
                .unwrap(),
            Some(indexed_inode),
        );
    }
}

#[cfg(test)]
mod hardening_tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::{
        Errno, VFS_EDITOR_DRAFT_DIRECTORY, VFS_RETAINED_DRAFT_DIRECTORY, create_editor_draft,
        denied_attribute_change, refused_mutation_errno,
    };

    #[test]
    fn editor_draft_is_private_exclusive_and_unpredictable() {
        let runtime = tempfile::tempdir().unwrap();
        let (first, first_path) = create_editor_draft(runtime.path()).unwrap();
        let (_second, second_path) = create_editor_draft(runtime.path()).unwrap();
        let directory = runtime
            .path()
            .join(VFS_RETAINED_DRAFT_DIRECTORY)
            .join(VFS_EDITOR_DRAFT_DIRECTORY);
        assert_eq!(first_path.parent(), Some(directory.as_path()));
        assert_eq!(second_path.parent(), Some(directory.as_path()));
        assert_ne!(first_path, second_path);
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(first);
    }

    #[test]
    fn ownership_mode_and_flag_changes_are_denied_with_eperm() {
        assert_eq!(denied_attribute_change(None, None, None, None), None);
        assert_eq!(
            denied_attribute_change(Some(0o600), None, None, None),
            Some(Errno::EPERM)
        );
        assert_eq!(
            denied_attribute_change(None, Some(0), None, None),
            Some(Errno::EPERM)
        );
        assert_eq!(
            denied_attribute_change(None, None, Some(0), None),
            Some(Errno::EPERM)
        );
    }

    #[test]
    fn editor_draft_directory_pre_created_symlink_is_refused() {
        let runtime = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let retained = runtime.path().join(VFS_RETAINED_DRAFT_DIRECTORY);
        std::fs::create_dir(&retained).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), retained.join(VFS_EDITOR_DRAFT_DIRECTORY))
            .unwrap();
        let error = create_editor_draft(runtime.path()).unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn editor_draft_directory_pre_created_world_writable_is_tightened() {
        let runtime = tempfile::tempdir().unwrap();
        let directory = runtime
            .path()
            .join(VFS_RETAINED_DRAFT_DIRECTORY)
            .join(VFS_EDITOR_DRAFT_DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        let (_draft, _path) = create_editor_draft(runtime.path()).unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn editor_draft_creation_refuses_when_leftover_store_is_full() {
        let runtime = tempfile::tempdir().unwrap();
        let directory = runtime
            .path()
            .join(VFS_RETAINED_DRAFT_DIRECTORY)
            .join(VFS_EDITOR_DRAFT_DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        for index in 0..super::VFS_RETAINED_DRAFT_LIMIT {
            std::fs::write(directory.join(format!("orna-edit-{index}.draft")), b"").unwrap();
        }
        let error = create_editor_draft(runtime.path()).unwrap_err();
        assert!(error.contains("editor draft store is full"), "{error}");
    }

    #[test]
    fn owner_draft_read_refuses_a_symlink_and_reads_a_regular_draft() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("elsewhere");
        std::fs::write(&target, b"outside the draft store").unwrap();
        let draft = directory.path().join("orna-edit-1.draft");
        std::os::unix::fs::symlink(&target, &draft).unwrap();
        assert!(super::read_regular_draft(&draft).is_err());
        std::fs::remove_file(&draft).unwrap();
        std::fs::write(&draft, b"candidate").unwrap();
        assert_eq!(super::read_regular_draft(&draft).unwrap(), b"candidate");
    }

    #[test]
    fn accepted_edit_reports_success_even_when_its_draft_cannot_be_removed() {
        let directory = tempfile::tempdir().unwrap();
        // A directory at the draft path makes remove_file fail after a durable commit.
        let undeletable = directory.path().join("orna-edit-1.draft");
        std::fs::create_dir(&undeletable).unwrap();
        assert!(super::admit_edit_result(Ok(()), &undeletable).is_ok());
        assert!(undeletable.is_dir(), "the undeletable draft is left in place");
        // A draft that is already gone is still an accepted edit.
        let gone = directory.path().join("orna-edit-2.draft");
        assert!(super::admit_edit_result(Ok(()), &gone).is_ok());
    }

    #[test]
    fn accepted_edit_removes_its_draft_and_refusals_retain_it() {
        let directory = tempfile::tempdir().unwrap();
        let draft = directory.path().join("orna-edit-3.draft");
        std::fs::write(&draft, b"candidate").unwrap();
        assert!(super::admit_edit_result(Err(super::DocumentCommitError::Stale), &draft).is_err());
        assert!(draft.exists(), "a refused edit retains its draft");
        assert!(super::admit_edit_result(Ok(()), &draft).is_ok());
        assert!(!draft.exists(), "an accepted edit removes its draft");
    }

    #[test]
    fn release_names_an_unrefused_candidate_unsynced_and_keeps_a_refusal_cause() {
        assert_eq!(super::release_retention_cause(None), "unsynced");
        let refusal = super::DraftRefusal {
            cause: "sys.vfs.stale_edit",
            handle: None,
            candidate: None,
            draft: None,
            unretained: None,
        };
        assert_eq!(super::release_retention_cause(Some(&refusal)), "sys.vfs.stale_edit");
    }

    /// A 64 MiB candidate is retained without a whole copy: every candidate read
    /// is at most one chunk, and the record holds all of its bytes.
    #[test]
    fn retained_candidate_streams_in_bounded_chunks() {
        const LEN: u64 = 64 * 1024 * 1024;
        let runtime = tempfile::tempdir().unwrap();
        let store = super::VfsRetainedDraftStore::new(runtime.path());
        let largest_read = std::cell::Cell::new(0_usize);
        let total_read = std::cell::Cell::new(0_u64);
        let mut read_chunk = |offset: u64, length: usize| -> Result<Vec<u8>, super::RetentionError> {
            largest_read.set(largest_read.get().max(length));
            total_read.set(total_read.get() + length as u64);
            Ok((offset..offset + length as u64).map(|index| (index % 251) as u8).collect())
        };
        let retained = store
            .retain("/music/big.orna", "refused", LEN, &mut read_chunk)
            .unwrap();
        assert!(largest_read.get() > 0);
        assert!(largest_read.get() <= super::CANDIDATE_CHUNK_BYTES);
        // The digest pass and the write pass each read the whole candidate once.
        assert_eq!(total_read.get(), 2 * LEN);
        let record_bytes = std::fs::read(&retained.record).unwrap();
        let marker = format!("bytes {LEN}\n\n");
        let start = record_bytes
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
            .expect("record header carries the candidate length")
            + marker.len();
        let body = &record_bytes[start..];
        assert_eq!(body.len() as u64, LEN);
        assert!(body.iter().enumerate().all(|(index, byte)| *byte == (index as u64 % 251) as u8));
    }

    #[test]
    fn live_mount_refusal_is_eperm() {
        assert_eq!(refused_mutation_errno(), Errno::EPERM);
    }
}

#[cfg(test)]
mod dirty_handle_ack_tests {
    use fuser::Errno;

    use super::{noncandidate_ack, DraftState};

    #[test]
    fn clean_handle_without_candidate_acknowledges() {
        let mut draft_state = DraftState::default();
        assert_eq!(noncandidate_ack(&mut draft_state, 7, "fsync"), Ok(()));
    }

    #[test]
    fn dirty_handle_without_candidate_fails_closed_with_eio() {
        let mut draft_state = DraftState::default();
        draft_state.dirty_handles.insert(7);
        assert_eq!(noncandidate_ack(&mut draft_state, 7, "fsync"), Err(Errno::EIO));
    }
}
