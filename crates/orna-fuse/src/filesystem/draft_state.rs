use super::*;

/// The digest an explicit edit carries from its read to its save (41-vfs.md:75-77): the
/// lowercase hex SHA-256 of the mounted bytes the editor was seeded from.
#[must_use]
pub fn edit_view_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Reads an editor draft through one descriptor opened without following a
/// symbolic link, so the regular-file check and the read name the same inode.
/// `O_NONBLOCK` keeps a FIFO from stalling the open before the check rejects it.
pub(crate) fn read_regular_draft(draft: &Path) -> Result<Vec<u8>, String> {
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

pub fn edit_path(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    path: &str,
    grouped_row: bool,
    row_baselines: Option<MountRowBaselines>,
) -> Result<(), EditError> {
    let writer = MountWriter::open(repository, commit, row_baselines)?;
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context, Some(writer))?;
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err("edit path must be relative to the projected row tree".into());
    }
    let prepared = filesystem.prepare_edit(relative, grouped_row)?;
    let draft_path =
        run_editor_on_draft(repository.runtime_paths().root(), &prepared.initial_bytes)?;
    let candidate = read_regular_draft(&draft_path)
        .map_err(|error| retained_edit_error(&error, &draft_path))?;
    prepared.commit(&candidate, &draft_path, grouped_row)
}
#[derive(Debug)]
pub enum UnmountPreparationError {
    UnresolvedDrafts,
    ReadOnlyMount,
    Disconnected,
    Operation(String),
}

pub fn begin_unmount(mountpoint: &Path) -> Result<(), UnmountPreparationError> {
    rustix::fs::setxattr(
        mountpoint,
        XATTR_UNMOUNT_QUIESCE,
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

pub fn cancel_unmount(mountpoint: &Path) -> Result<(), String> {
    rustix::fs::setxattr(
        mountpoint,
        XATTR_UNMOUNT_QUIESCE,
        b"0",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| format!("could not resume Orna mount after failed unmount: {error}"))
}
pub fn has_unresolved_drafts(mountpoint: &Path) -> Result<bool, String> {
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(mountpoint, XATTR_UNRESOLVED_DRAFTS, &mut value)
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
pub(crate) struct UnresolvedDraft {
    pub(crate) path: String,
    /// The `sys.*` Orna cause, or empty when the profile names none.
    pub(crate) code: &'static str,
    /// The retained draft id, or empty when no candidate was kept.
    pub(crate) draft: String,
    pub(crate) detail: String,
}

/// Reads one draft-list xattr of a live mount as UTF-8 text. The value can grow
/// between the size probe and the read, which fails with ERANGE; the read then
/// re-probes and retries, up to `READ_ATTEMPTS` times.
pub(crate) fn read_draft_xattr(mountpoint: &Path, name: &str) -> Result<String, rustix::io::Errno> {
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
pub fn unresolved_draft_list(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, XATTR_UNRESOLVED_DRAFT_LIST)
}

/// The unresolved drafts of a live mount, one `path<TAB>code<TAB>draft<TAB>detail`
/// line each, for `mount status`.
pub fn unresolved_draft_fields(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, XATTR_UNRESOLVED_DRAFT_FIELDS)
}

/// Why stop-time draft retention did not finish (ORNA-VFS-024).
pub enum DraftRetentionStop {
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
pub fn retain_drafts_for_shutdown(mountpoint: &Path) -> Result<bool, DraftRetentionStop> {
    match begin_unmount(mountpoint) {
        Ok(())
        | Err(UnmountPreparationError::ReadOnlyMount | UnmountPreparationError::Disconnected) => {
            Ok(false)
        }
        Err(UnmountPreparationError::UnresolvedDrafts) => rustix::fs::setxattr(
            mountpoint,
            XATTR_RETAIN_DRAFTS,
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
pub fn unmount_dead_mountpoint(mountpoint: &Path) -> Result<(), String> {
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

pub fn unmount_mountpoint(mountpoint: &Path) -> Result<(), String> {
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

pub(crate) fn fusermount_program() -> OsString {
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

/// The CAS baseline an open document handle captured at open (ORNA-VFS-011): the durable row its
/// close flush compares against and the table schema it opened under. The row advances only when
/// this handle's own candidate is accepted, so a row another writer accepted makes the next flush
/// of this handle stale.
pub(crate) struct DocumentBaseline {
    pub(crate) schema_identity: TableSchemaIdentity,
    pub(crate) row: Option<Vec<u8>>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum OpenFile {
    Document(
        Arc<RowSaveContext>,
        Arc<EditDraft<RowMapSnapshot>>,
        Arc<Mutex<DocumentBaseline>>,
    ),
    Temporary(
        Arc<RowSaveContext>,
        Arc<ManagedFile<RowMapSnapshot>>,
        Arc<TemporarySave<RowMapSnapshot>>,
        Option<BlobField>,
    ),
    Source(u64),
}
/// Why a submitted candidate was not accepted, for the unmount listing and
/// the stop-time report. `draft` is its recovery record once retained. While
/// recovery storage refuses the candidate, `unretained` holds its save context
/// and its live source so a later retention attempt can still write it.
pub(crate) struct DraftRefusal {
    pub(crate) cause: &'static str,
    /// The row path the candidate was refused for; an accepted save of that path supersedes it.
    pub(crate) path: String,
    pub(crate) draft: Option<String>,
    /// The handle whose candidate this refusal describes; a write by that handle clears it.
    pub(crate) handle: Option<u64>,
    /// The released or refused writer's draft: the base a path truncate that
    /// arrives before retention cuts (ORNA-VFS-009).
    pub(crate) candidate: Option<Arc<EditDraft<RowMapSnapshot>>>,
    pub(crate) unretained: Vec<(Arc<RowSaveContext>, CandidateSource)>,
}

/// A replacement candidate whose rename was refused. `kept` records that its
/// candidate reached recovery storage, so the draft list reports it as retained.
#[derive(Clone)]
pub(crate) struct ScratchRefusal {
    pub(crate) cause: &'static str,
    pub(crate) kept: bool,
    /// The row path the candidate was refused for; an accepted save of that path supersedes it.
    pub(crate) path: String,
}

/// Whether the mount still serves draft requests or is stopping for unmount.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MountPhase {
    #[default]
    Serving,
    Quiescing,
}

/// One open writable handle's draft state: the candidate it wrote, whether a
/// commit accepted it, the Blob metadata it captured and the pid that opened it.
#[derive(Default)]
pub(crate) struct HandleDraft {
    /// Inode whose latest candidate no commit has accepted or retained.
    pub(crate) unresolved: Option<u64>,
    /// The latest write or truncate has not been accepted, so a sync of a
    /// handle with no candidate must fail closed.
    pub(crate) unaccepted_write: bool,
    /// The generation, row baseline and table schema a write-open Blob handle
    /// captured at open.
    pub(crate) blob: Option<BlobHandle>,
    /// Pid that opened the writable handle; a FLUSH from another process is not
    /// the writer's close.
    pub(crate) writer_owner: Option<u32>,
}

impl HandleDraft {
    /// Whether nothing about this handle needs to be kept any more.
    pub(crate) fn is_idle(&self) -> bool {
        self.unresolved.is_none()
            && !self.unaccepted_write
            && self.blob.is_none()
            && self.writer_owner.is_none()
    }
}

/// Unsynced Document drafts are retained on release with cause `unsynced`; a
/// Document whose fsync was rejected stays unresolved until a later sync of the
/// same file is accepted.
/// Replacement scratch drafts remain unresolved until their rename is accepted
/// or the scratch is removed.
#[derive(Default)]
pub(crate) struct DraftState {
    pub(crate) pending_truncates: HashMap<u64, u64>,
    /// Per-handle draft state, keyed by handle number.
    pub(crate) handles: HashMap<u64, HandleDraft>,
    pub(crate) retained_scratch: HashSet<u64>,
    pub(crate) rejected_documents: HashMap<u64, DraftRefusal>,
    /// Scratch files whose rename was refused, with the cause and whether the candidate was kept.
    pub(crate) rejected_scratch: HashMap<u64, ScratchRefusal>,
    /// Whether draft requests are served or the mount is stopping.
    pub(crate) phase: MountPhase,
    /// Source modules with a private write-intent candidate (ORNA-VFS-020), keyed by inode.
    pub(crate) source_drafts: BTreeMap<u64, source_write::SourceDraft>,
}

impl DraftState {
    /// The state of `handle`, created when the handle is first recorded.
    pub(crate) fn handle_mut(&mut self, handle: u64) -> &mut HandleDraft {
        self.handles.entry(handle).or_default()
    }

    pub(crate) fn handle(&self, handle: u64) -> Option<&HandleDraft> {
        self.handles.get(&handle)
    }

    /// Records that `handle` holds an unresolved candidate at `inode`.
    pub(crate) fn set_unresolved(&mut self, handle: u64, inode: u64) {
        self.handle_mut(handle).unresolved = Some(inode);
    }

    pub(crate) fn has_unresolved(&self, handle: u64) -> bool {
        self.handle(handle)
            .is_some_and(|draft| draft.unresolved.is_some())
    }

    /// Ends the unresolved state of `handle` and returns the inode it named.
    pub(crate) fn take_unresolved(&mut self, handle: u64) -> Option<u64> {
        let inode = self.handles.get_mut(&handle)?.unresolved.take();
        self.prune(handle);
        inode
    }

    /// Ends every unresolved handle: retention has taken over their candidates.
    pub(crate) fn clear_unresolved(&mut self) {
        for draft in self.handles.values_mut() {
            draft.unresolved = None;
        }
        self.handles.retain(|_, draft| !draft.is_idle());
    }

    /// Every unresolved handle and the inode its candidate belongs to.
    pub(crate) fn unresolved_handles(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.handles
            .iter()
            .filter_map(|(handle, draft)| draft.unresolved.map(|inode| (*handle, inode)))
    }

    pub(crate) fn unresolved_inodes(&self) -> impl Iterator<Item = u64> + '_ {
        self.handles.values().filter_map(|draft| draft.unresolved)
    }

    /// Ends the unresolved state of every handle that wrote `inode`.
    pub(crate) fn clear_unresolved_inode(&mut self, inode: u64) {
        for draft in self.handles.values_mut() {
            if draft.unresolved == Some(inode) {
                draft.unresolved = None;
            }
        }
        self.handles.retain(|_, draft| !draft.is_idle());
    }

    /// Ends the unresolved state of handles whose candidate an accepted save
    /// superseded.
    pub(crate) fn retract_superseded(&mut self, superseded: &HashSet<u64>) {
        for draft in self.handles.values_mut() {
            if draft
                .unresolved
                .is_some_and(|inode| superseded.contains(&inode))
            {
                draft.unresolved = None;
            }
        }
        self.handles.retain(|_, draft| !draft.is_idle());
    }

    /// Records that the latest write or truncate of `handle` was not accepted.
    pub(crate) fn set_unaccepted_write(&mut self, handle: u64) {
        self.handle_mut(handle).unaccepted_write = true;
    }

    pub(crate) fn has_unaccepted_write(&self, handle: u64) -> bool {
        self.handle(handle)
            .is_some_and(|draft| draft.unaccepted_write)
    }

    pub(crate) fn clear_unaccepted_write(&mut self, handle: u64) {
        if let Some(draft) = self.handles.get_mut(&handle) {
            draft.unaccepted_write = false;
        }
        self.prune(handle);
    }

    pub(crate) fn set_blob(&mut self, handle: u64, blob: BlobHandle) {
        self.handle_mut(handle).blob = Some(blob);
    }

    pub(crate) fn blob(&self, handle: u64) -> Option<&BlobHandle> {
        self.handle(handle)?.blob.as_ref()
    }

    pub(crate) fn has_blob(&self, handle: u64) -> bool {
        self.blob(handle).is_some()
    }

    pub(crate) fn take_blob(&mut self, handle: u64) -> Option<BlobHandle> {
        let blob = self.handles.get_mut(&handle)?.blob.take();
        self.prune(handle);
        blob
    }

    pub(crate) fn set_writer_owner(&mut self, handle: u64, pid: u32) {
        self.handle_mut(handle).writer_owner = Some(pid);
    }

    /// Records `pid` as the opener unless a pid is already recorded.
    pub(crate) fn set_writer_owner_if_absent(&mut self, handle: u64, pid: u32) {
        self.handle_mut(handle).writer_owner.get_or_insert(pid);
    }

    pub(crate) fn writer_owner(&self, handle: u64) -> Option<u32> {
        self.handle(handle)?.writer_owner
    }

    pub(crate) fn clear_writer_owner(&mut self, handle: u64) {
        if let Some(draft) = self.handles.get_mut(&handle) {
            draft.writer_owner = None;
        }
        self.prune(handle);
    }

    /// Forgets `handle` once nothing about it needs to be kept.
    pub(crate) fn prune(&mut self, handle: u64) {
        if self.handles.get(&handle).is_some_and(HandleDraft::is_idle) {
            self.handles.remove(&handle);
        }
    }

    /// Whether a draft request may still be served.
    pub(crate) fn ensure_serving(&self) -> Result<(), Errno> {
        match self.phase {
            MountPhase::Serving => Ok(()),
            MountPhase::Quiescing => Err(Errno::EBUSY),
        }
    }

    /// A write or truncate by `handle` makes its candidate newer than the refused
    /// one, so the refusal no longer names that handle. An accepted save must not
    /// end the unresolved state of that newer candidate (ORNA-VFS-008).
    pub(crate) fn candidate_written(&mut self, handle: u64, inode: u64) {
        if let Some(refusal) = self.rejected_documents.get_mut(&inode)
            && refusal.handle == Some(handle)
        {
            refusal.handle = None;
        }
    }

    /// An accepted save of `inode` at row `path` supersedes the refusals it replaces: the one
    /// recorded under its own node, and those of the same row path recorded under a node the
    /// re-projection has since replaced. A refusing handle whose candidate is unchanged holds
    /// only obsolete bytes, so its unresolved state ends here.
    pub(crate) fn supersede_refusals(&mut self, inode: u64, path: &str) {
        let mut superseded = HashSet::new();
        let documents = self
            .rejected_documents
            .iter()
            .filter(|(candidate, refusal)| **candidate == inode || refusal.path == path)
            .map(|(candidate, _)| *candidate)
            .collect::<Vec<_>>();
        for candidate in documents {
            if let Some(DraftRefusal {
                handle: Some(refused),
                ..
            }) = self.rejected_documents.remove(&candidate)
            {
                self.take_unresolved(refused);
            }
            if candidate != inode {
                superseded.insert(candidate);
            }
        }
        self.rejected_scratch.retain(|candidate, refusal| {
            let keep = *candidate != inode && refusal.path != path;
            if !keep && *candidate != inode {
                superseded.insert(*candidate);
            }
            keep
        });
        self.retract_superseded(&superseded);
        self.retained_scratch
            .retain(|candidate| !superseded.contains(candidate));
    }
}

/// A write-open Blob handle's candidate generation, the row baseline and the table schema it
/// captured at open (ORNA-VFS-011, ORNA-VFS-019). The baseline advances to the accepted row after
/// each accept (41-vfs.md EDIT-1 step 3).
pub(crate) struct BlobHandle {
    pub(crate) generation: u64,
    pub(crate) baseline: Option<Vec<u8>>,
    pub(crate) schema_identity: TableSchemaIdentity,
    /// The mount-relative path captured at open; a refusal keeps it after the row leaves the mount.
    pub(crate) path: String,
}

/// Decides the acknowledgement for a commit that found no candidate. A clean
/// handle submitted nothing, so fsync of it succeeds (POSIX). A handle with a
/// written but unaccepted candidate must not be acknowledged: that would report
/// durability the accepted row does not have, so it fails closed with EIO.
pub(crate) fn noncandidate_ack(
    draft_state: &mut DraftState,
    handle: u64,
    operation: &str,
) -> Result<(), Errno> {
    if draft_state.has_unaccepted_write(handle) {
        eprintln!(
            "{operation}: handle {handle} wrote a candidate that no commit found; refusing to acknowledge it (EIO)"
        );
        return Err(Errno::EIO);
    }
    draft_state.take_unresolved(handle);
    Ok(())
}

/// Whether a refused candidate is kept in recovery storage (`draft` is set) while its document can
/// no longer be opened: the committed table schema is unavailable, the same condition on which a
/// document open returns EIO (profiles/vfs-1.md:134). Such a draft cannot be resolved through the
/// mount, and its retained copy is the explicit retention profiles/vfs-1.md:144 asks unmount to
/// accept.
///
/// A refusal still holding an unretained source is never unservable: that live copy exists only in
/// the mount, so unmount must refuse it. A `HEAD` that cannot be read is not an unavailable schema,
/// so the draft stays listed and blocks unmount.
pub(crate) fn refusal_is_unservable_retained(
    state: &FsBuilder,
    inode: u64,
    refusal: &DraftRefusal,
) -> bool {
    if refusal.draft.is_none() || !refusal.unretained.is_empty() {
        return false;
    }
    let Some(NodeKind::Document { save, .. }) = state.nodes.get(&inode).map(|node| &node.kind)
    else {
        return false;
    };
    orna_cli_support::table_schema::committed_table_schema_unresolvable(
        &save.writer.repository,
        save.table.object_id(),
    )
    .unwrap_or(false)
}

pub(crate) struct WorkspaceFilesystem {
    pub(crate) state: Mutex<FsBuilder>,
    pub(crate) open_files: Mutex<HashMap<u64, OpenFile>>,
    pub(crate) draft_state: Mutex<DraftState>,
    /// Serializes source-module group commits; held outside the draft lock.
    pub(crate) source_gate: Mutex<()>,
    pub(crate) next_handle: AtomicU64,
}

#[allow(clippy::unnecessary_wraps)]
/// The errno a retention refusal reports when several refusals meet: a full
/// store (EDQUOT) wins over other storage failures (EIO).
pub(crate) fn worst_retention_errno(current: Option<Errno>, next: Errno) -> Option<Errno> {
    Some(if current == Some(Errno::EDQUOT) || next == Errno::EDQUOT {
        Errno::EDQUOT
    } else {
        Errno::EIO
    })
}
