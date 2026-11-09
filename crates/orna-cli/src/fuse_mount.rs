use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::cli_source_edit::{
    checked_project_source_path, restore_source_after_failed_write, write_source_atomically,
};
use crate::encoding::hex;
use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, ReplyXattr, Request,
};
use orna_conformance::transactional_loaded_project_candidate_validator;
use orna_evaluator::Limits as EvaluatorLimits;
use orna_project::{
    KeyComponentText, ObjectIdentityManifest, ObjectKind, ProjectLoader, ResolvedTableSchema,
    SourceRowRequest, StoredSchema, admit_source_row,
};
use orna_repository::schema_evolution::admit_row_under_descriptor;
use orna_repository::{
    AdmittedBlobReference, CommittedTableBinding, ContentIdentity, GitCommitRef, GitHashAlgorithm,
    GraphError, HistoricalFormatContext, KeyRange, MountRowBaselines, NativeGraphContext,
    NativeOid, OrpBlobBinding, Repository, RepositoryReadScope, RowMapSnapshot, TypedKey,
    VerifiedBlobRange,
};
use orna_runtime::{
    NoFault, RuntimeState, RuntimeTableActivationSnapshot, RuntimeTableIdentity, RuntimeTableRows,
    TableObjectId, TableSchemaIdentity,
    vfs::{
        CANDIDATE_CHUNK_BYTES, CandidateSnapshot, DraftQuota, EditDraft, EditDraftError,
        FsyncFailure, ManagedFile, QuotaCharge, SnapshotPin, TemporaryRenameOutcome, TemporarySave,
        VfsBlobReplacementError, VfsComponent, VfsDurableSave, VfsFileSnapshot, VfsRepositoryCache,
        VfsRowProjection, VfsSaveCandidate, VfsSaveCoordinates, VfsSaveError, VfsStoredField,
        VfsTableRowReplacement, project_colliding_table_component, project_component,
        project_key_component, unproject_component,
    },
};
use orna_storage::COMPACT_FILE_BOUND_BYTES;
use orna_syntax::{Expr, LiteralKind, parse_row};
use orna_value::{
    Format3Context, Raw, SchemaDescriptor, blob_metadata_from_raw, decode_canonical_storage_value,
};
use sha2::{Digest, Sha256};

mod source_write;

mod builder;
mod filesystem;
mod historical;
mod projection;
mod retained;
mod row_save;
mod scratch;
mod writer;

pub(crate) use crate::{invocation_owner_id, runtime_identity};
pub(crate) use builder::*;
pub(crate) use filesystem::*;
pub(crate) use historical::*;
pub(crate) use projection::*;
pub(crate) use retained::*;
pub(crate) use row_save::*;
pub(crate) use scratch::*;
pub(crate) use writer::*;

// The extended attributes through which a mount's owner is queried and
// controlled. The daemon serves them from its root directory and the CLI calls
// them against the mountpoint, so both ends name them here.

/// Set to `1` to stop accepting edits before an unmount, to `0` to resume.
pub(super) const XATTR_UNMOUNT_QUIESCE: &str = "user.orna.unmount-quiesce";
/// Set to `1` to keep every unresolved draft in recovery storage.
pub(super) const XATTR_RETAIN_DRAFTS: &str = "user.orna.retain-drafts";
/// `1` while an unresolved draft remains, `0` otherwise.
pub(super) const XATTR_UNRESOLVED_DRAFTS: &str = "user.orna.unresolved-drafts";
/// One `path<TAB>detail` line per unresolved draft.
pub(super) const XATTR_UNRESOLVED_DRAFT_LIST: &str = "user.orna.unresolved-draft-list";
/// One `path<TAB>code<TAB>draft<TAB>detail` line per unresolved draft.
pub(super) const XATTR_UNRESOLVED_DRAFT_FIELDS: &str = "user.orna.unresolved-draft-fields";
/// An explicit edit request: `mode NUL path NUL draft NUL digest`.
pub(super) const XATTR_EDIT_COMMIT: &str = "user.orna.edit-commit";

/// Implementation draft quota for every mounted draft and scratch file: 64 MiB,
/// the compact publication file bound (ORNA-PUB-001). It is not a VFS spec
/// bound. A write or truncate that grows a file past it is quota-rejected
/// (EDQUOT) and never admitted; shrinking an existing larger baseline is allowed.
#[allow(clippy::cast_possible_truncation)]
const DRAFT_FILE_BOUND_BYTES: usize = COMPACT_FILE_BOUND_BYTES as usize;

/// Bound on unbound editor scratch files one mount may create (41-vfs.md Temporary-save
/// replacement). Unbound scratch is recovery state, so an editor cannot grow it without limit.
/// Records restored from earlier mounts are not counted (`VfsScratchStore::reserve_unbound`).
const MAX_UNBOUND_SCRATCH: usize = 16;

/// Bound on the committed history a restarted mount scans for the schema generations of a
/// table whose CWD row predates the mounted schema (ORNA-EVOL-002). A longer history is
/// refused rather than scanned partially.
const HISTORY_LIMIT: usize = 1024;

/// How long a failed committed-schema scan is kept before the table is scanned again.
const HISTORICAL_SCAN_RETRY: Duration = Duration::from_secs(5);

/// How long one pass of a table's committed-schema scan may run before it pauses. A paused scan
/// keeps its progress and resumes on the next request for the table, so a long history never holds
/// a request or mount startup past this bound per pass.
const HISTORICAL_RESOLVE_BUDGET: Duration = Duration::from_secs(10);

/// Refusal text for a CWD row that needs a table's committed schema history while it is scanned.
const SCAN_PENDING: &str = "the committed schema history of this table is still being scanned";

/// Errno for a rejected draft operation (profiles/vfs-1 errno table): EDQUOT
/// for the per-file or aggregate draft quota, EIO for backing I/O failures.
#[allow(clippy::trivially_copy_pass_by_ref)]
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

/// Copies a row document candidate into an owned buffer for its node. Row documents are
/// bounded by the row schema, so one bounded read is the whole document.
fn candidate_document_bytes(bytes: &CandidateSnapshot) -> std::io::Result<Vec<u8>> {
    let length =
        usize::try_from(bytes.len()).map_err(|_| std::io::Error::other("candidate too large"))?;
    bytes.read_range(0, length)
}

/// Errno for a mutation refused on the live writable mount. Every such refusal
/// is a denied or unsupported operation, so it is EPERM (41-vfs.md:103-105;
/// profiles/vfs-1.md:139). EROFS is reserved for the kernel read-only snapshot
/// mount (`spawn_read_only`), which never reaches these handlers.
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
mod tests;

#[cfg(test)]
mod hardening_tests;

#[cfg(test)]
mod directory_removal_tests;

#[cfg(test)]
mod readdir_cursor_tests;

#[cfg(test)]
mod dirty_handle_ack_tests;

#[cfg(test)]
mod nested_projection_tests;

#[cfg(test)]
mod historical_scan_budget_tests;

/// Default build only: the draft-quota env seam must not exist. Setting the
/// variable needs `unsafe` on edition 2024 and this crate forbids it, so the
/// variable is set on a child run of this test binary.
#[cfg(all(test, not(feature = "test-seams")))]
mod draft_quota_seam_tests;
