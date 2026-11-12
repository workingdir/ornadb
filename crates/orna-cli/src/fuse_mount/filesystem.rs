use super::*;
use orna_sys::SysFailureCode;

pub(crate) const ROOT_INODE: u64 = 1;
pub(crate) const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
pub(crate) const ENTRY_TTL: Duration = Duration::ZERO;
// Private editor drafts bypass the shared kernel page cache. Writeback cache is
// intentionally not requested: it can mix a later database generation into an
// already-open draft.
pub(crate) const EDITABLE_OPEN_FLAGS: fuser::FopenFlags = fuser::FopenFlags::FOPEN_DIRECT_IO;
pub(crate) const EDITOR_INIT_CAPABILITIES: fuser::InitFlags = fuser::InitFlags::FUSE_ATOMIC_O_TRUNC;
pub(crate) fn spawn_read_only(
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
pub(crate) fn spawn_editor(
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
/// `draft` names the retained candidate so the CLI can report its location by
/// default (ORNA-UX-004; profiles/vfs-1.md:101-102).
#[derive(Debug)]
pub(crate) enum EditError {
    Stale {
        message: String,
        draft: PathBuf,
    },
    Failed {
        message: String,
        draft: Option<PathBuf>,
    },
}

impl From<String> for EditError {
    fn from(error: String) -> Self {
        Self::Failed {
            message: error,
            draft: None,
        }
    }
}

impl From<&str> for EditError {
    fn from(error: &str) -> Self {
        Self::from(error.to_owned())
    }
}

#[allow(clippy::type_complexity)]
/// An explicit edit prepared against a live row tree: `initial_bytes` seeds the
/// editor draft, and `commit` admits the editor's candidate exactly once.
pub(crate) struct PreparedEdit {
    initial_bytes: Vec<u8>,
    commit: Box<dyn FnOnce(&[u8], &Path, bool) -> Result<(), EditError>>,
    /// The document an accepted `edit --row` re-projects: its inode and save context.
    reproject: Option<(u64, Arc<RowSaveContext>)>,
}

impl PreparedEdit {
    #[allow(clippy::type_complexity)]
    /// Admits `candidate` for the draft at `draft_path`; the draft is removed once accepted.
    /// `record_baseline` false leaves the mounted captured baseline unchanged (ORNA-VFS-015).
    pub(crate) fn commit(
        self,
        candidate: &[u8],
        draft_path: &Path,
        record_baseline: bool,
    ) -> Result<(), EditError> {
        (self.commit)(candidate, draft_path, record_baseline)
    }
}

#[allow(clippy::items_after_statements)]
/// Writes `initial` to a new explicit editor draft, runs `$EDITOR` on it and
/// returns the draft path. The draft is retained on every failure.
pub(crate) fn run_editor_on_draft(
    runtime_root: &Path,
    initial: &[u8],
) -> Result<PathBuf, EditError> {
    let (mut file, draft_path) = create_editor_draft(runtime_root)
        .map_err(|error| format!("explicit edit draft could not be created: {error}"))?;
    use std::io::Write as _;
    file.write_all(initial)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            retained_draft_failure(
                format!(
                    "explicit edit draft could not be initialized: {error}; retained at {}",
                    draft_path.display()
                ),
                &draft_path,
            )
        })?;
    drop(file);
    let Some(editor) = std::env::var_os("EDITOR") else {
        return Err(retained_draft_failure(
            format!(
                "EDITOR is not set; initial draft retained at {}",
                draft_path.display()
            ),
            &draft_path,
        ));
    };
    let status = std::process::Command::new(editor)
        .arg(&draft_path)
        .status()
        .map_err(|error| {
            retained_draft_failure(
                format!(
                    "editor could not be started: {error}; draft retained at {}",
                    draft_path.display()
                ),
                &draft_path,
            )
        })?;
    if !status.success() {
        return Err(retained_draft_failure(
            format!(
                "editor exited unsuccessfully; draft retained at {}",
                draft_path.display()
            ),
            &draft_path,
        ));
    }
    Ok(draft_path)
}

#[allow(clippy::items_after_statements)]
pub(crate) fn retained_edit_error(detail: &str, draft_path: &Path) -> EditError {
    retained_draft_failure(
        format!("{detail}; draft retained at {}", draft_path.display()),
        draft_path,
    )
}

/// Refuses an edit whose candidate stays at `draft_path` for recovery.
pub(crate) fn retained_draft_failure(message: String, draft_path: &Path) -> EditError {
    EditError::Failed {
        message,
        draft: Some(draft_path.to_path_buf()),
    }
}

/// Maps a durable admission result to the explicit edit outcome. An accepted
/// edit removes its draft; any refusal retains it.
pub(crate) fn admit_edit_result(
    admission: Result<(), DocumentCommitError>,
    draft_path: &Path,
) -> Result<(), EditError> {
    match admission {
        Ok(()) => {
            // The commit is durable, so the owner reports acceptance even when the
            // explicit draft cannot be removed (ORNA-ADMIN-003). The leftover draft
            // is named in the warning for the operator to remove.
            if let Err(error) = std::fs::remove_file(draft_path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "warning: edit was accepted but its explicit draft could not be removed: {error}; remove {} by hand",
                    draft_path.display()
                );
            }
            Ok(())
        }
        Err(DocumentCommitError::Stale) => Err(EditError::Stale {
            message: format!(
                "the mounted baseline is stale against the durable row; nothing was written; draft retained at {}",
                draft_path.display()
            ),
            draft: draft_path.to_path_buf(),
        }),
        Err(error) => Err(retained_draft_failure(
            format!(
                "edit candidate was rejected: {error}; draft retained at {}",
                draft_path.display()
            ),
            draft_path,
        )),
    }
}

impl WorkspaceFilesystem {
    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_lines)]
    /// Resolves `relative` in the live row tree to its editable target and
    /// prepares the admission step. No draft is created on refusal.
    pub(crate) fn prepare_edit(
        &self,
        relative: &Path,
        grouped_row: bool,
    ) -> Result<PreparedEdit, EditError> {
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
                NodeKind::Blob {
                    graph, reference, ..
                } => {
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
                        .ok_or_else(|| {
                            "Blob edit target is not an admitted row field".to_owned()
                        })?;
                    let initial = read_blob_bytes(&graph, &reference)?;
                    let schema_identity =
                        orna_cli_support::table_schema::committed_table_schema_identity(
                            &writer.repository,
                            save.table.object_id(),
                        )
                        .map_err(|error| {
                            format!("edit could not capture the table schema: {error}")
                        })?;
                    let image = Arc::new(VfsFileSnapshot::new(
                        row_image_pin(&save.source.rows),
                        Arc::<[u8]>::from([]),
                    ));
                    let target = writer
                        .runtime
                        .block_on(writer.cache.managed_file(image))
                        .map_err(|error| {
                            format!("Blob edit target could not be opened: {error:?}")
                        })?;
                    let scratch = Arc::new(
                        writer
                            .runtime
                            .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES)),
                    );
                    (
                        save,
                        initial,
                        Some((target, scratch, field, schema_identity)),
                    )
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
        let reproject = blob_edit.is_none().then(|| (inode, Arc::clone(&save)));
        let commit: Box<dyn FnOnce(&[u8], &Path, bool) -> Result<(), EditError>> =
            if let Some((target, scratch, field, schema_identity)) = blob_edit {
                Box::new(move |candidate, draft_path, record_baseline| {
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
                    let expected = save
                        .explicit_edit_baseline(grouped_row)
                        .map_err(|error| retained_edit_error(&error, draft_path))?;
                    admit_edit_result(
                        save.commit_blob_temporary(
                            &target,
                            &scratch,
                            &field,
                            expected,
                            schema_identity,
                            record_baseline,
                        )
                        .map(|_| ()),
                        draft_path,
                    )
                })
            } else {
                let opened = orna_cli_support::table_schema::committed_table_schema_identity(
                    &writer.repository,
                    save.table.object_id(),
                )
                .map_err(|error| format!("edit could not capture the table schema: {error}"))?;
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
                    let expected = save
                        .explicit_edit_baseline(grouped_row)
                        .map_err(|error| retained_edit_error(&error, draft_path))?;
                    admit_edit_result(
                        save.commit_document_against(&draft, record_baseline, expected, &opened)
                            .map(|_| ()),
                        draft_path,
                    )
                })
            };
        Ok(PreparedEdit {
            initial_bytes,
            commit,
            reproject,
        })
    }

    #[allow(clippy::type_complexity)]
    /// Attributes of `inode` as a path stat reports them. A document's size is
    /// the accepted image its new opens read (`ManagedFile::stat`), so an owner
    /// write is visible to stat at once (profiles/vfs-1.md:133, ORNA-VFS-019).
    /// Open handles keep their own draft, captured baseline and generation, so
    /// reads and stale-save refusal are unaffected. A document an owner write
    /// superseded reports the size of the generation it captured, so fstat on a
    /// handle opened before that write keeps its generation's size
    /// (41-vfs.md:38-40, profiles/vfs-1.md:126-133). FUSE passes no handle to
    /// fstat, so the nodeid alone selects the generation.
    pub(crate) fn attr_of(&self, inode: u64) -> Option<FileAttr> {
        let (mut attr, document) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let attr = state.attr(inode)?;
            let document = match state.nodes.get(&inode).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) if !state.is_superseded(inode) => {
                    Some(Arc::clone(save))
                }
                _ => None,
            };
            (attr, document)
        };
        if let Some(save) = document
            && let Some(stat) = save.writer.runtime.block_on(save.target.stat())
        {
            attr.size = stat.size();
            attr.blocks = attr.size.saturating_add(511) / 512;
        }
        Some(attr)
    }

    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    /// Commits an explicit edit requested through the `user.orna.edit-commit`
    /// control (ORNA-VFS-015). `request` is `mode NUL path NUL draft NUL digest`; the draft
    /// must be a regular editor draft in this repository's recovery store.
    pub(crate) fn commit_requested_edit(&self, request: &[u8]) -> Result<(), EditError> {
        use std::os::unix::ffi::OsStrExt as _;
        let fields: Vec<&[u8]> = request.split(|byte| *byte == 0).collect();
        let [mode, path, draft_bytes, digest] = fields.as_slice() else {
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
        // ORNA-VFS-015: `edit --row` records the row it accepted as this mount's own grouped save.
        // An explicit edit CASes against the durable row read at commit, not the mount's recorded
        // baseline, which a plain owner-routed edit leaves at the row before it (41-vfs.md:75-77).
        let prepared = self.prepare_edit(Path::new(OsStr::from_bytes(path)), grouped_row)?;
        // 41-vfs.md:75-77: the editor was seeded from the bytes the CLI read. If the mounted row
        // moved since, that read is stale: nothing is written and the draft stays retained.
        if edit_view_digest(&prepared.initial_bytes).as_bytes() != *digest {
            return Err(EditError::Stale {
                message: format!(
                    "the mounted row changed after orna edit read it; nothing was written; draft retained at {}",
                    draft.display()
                ),
                draft: draft.to_path_buf(),
            });
        }
        let reproject = prepared.reproject.clone();
        prepared.commit(&candidate, draft, grouped_row)?;
        if let Some((inode, save)) = reproject {
            self.adopt_accepted_document(inode, &save, &candidate);
        }
        Ok(())
    }

    /// Shows the row an owner-routed `edit --row` accepted: the document takes the accepted
    /// bytes, and a Str over 64 KiB is re-projected to its `.txt` sibling (profiles/vfs-1.md:76-80).
    /// A document an open handle pins keeps the generation it captured: the accepted bytes go
    /// to a new node that shows the accepted row's projection and replaces it (41-vfs.md:38-40).
    pub(crate) fn adopt_accepted_document(
        &self,
        inode: u64,
        save: &Arc<RowSaveContext>,
        accepted: &[u8],
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let document = if state.subtree_pinned(inode) {
            match state.replace_document_generation(inode) {
                Ok(document) => document,
                Err(error) => {
                    eprintln!("the accepted row document could not be shown: {error}");
                    return;
                }
            }
        } else {
            if let Some(NodeKind::Document { bytes, .. }) =
                state.nodes.get_mut(&inode).map(|node| &mut node.kind)
            {
                *bytes = accepted.to_vec();
            }
            inode
        };
        if let Err(error) = state.project_accepted_document(document, save) {
            eprintln!("the accepted row document could not be re-projected: {error}");
        }
    }
}

/// The digest an explicit edit carries from its read to its save (41-vfs.md:75-77): the
/// lowercase hex SHA-256 of the mounted bytes the editor was seeded from.
pub(crate) fn edit_view_digest(bytes: &[u8]) -> String {
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

pub(crate) fn edit_path(
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
pub(crate) enum UnmountPreparationError {
    UnresolvedDrafts,
    ReadOnlyMount,
    Disconnected,
    Operation(String),
}

pub(crate) fn begin_unmount(mountpoint: &Path) -> Result<(), UnmountPreparationError> {
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

pub(crate) fn cancel_unmount(mountpoint: &Path) -> Result<(), String> {
    rustix::fs::setxattr(
        mountpoint,
        XATTR_UNMOUNT_QUIESCE,
        b"0",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| format!("could not resume Orna mount after failed unmount: {error}"))
}
pub(crate) fn has_unresolved_drafts(mountpoint: &Path) -> Result<bool, String> {
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
pub(crate) fn unresolved_draft_list(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, XATTR_UNRESOLVED_DRAFT_LIST)
}

/// The unresolved drafts of a live mount, one `path<TAB>code<TAB>draft<TAB>detail`
/// line each, for `mount status`.
pub(crate) fn unresolved_draft_fields(mountpoint: &Path) -> Result<String, rustix::io::Errno> {
    read_draft_xattr(mountpoint, XATTR_UNRESOLVED_DRAFT_FIELDS)
}

/// Why stop-time draft retention did not finish (ORNA-VFS-024).
pub(crate) enum DraftRetentionStop {
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
pub(crate) fn retain_drafts_for_shutdown(mountpoint: &Path) -> Result<bool, DraftRetentionStop> {
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
pub(crate) fn unmount_dead_mountpoint(mountpoint: &Path) -> Result<(), String> {
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

pub(crate) fn unmount_mountpoint(mountpoint: &Path) -> Result<(), String> {
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
    pub(super) source_drafts: BTreeMap<u64, source_write::SourceDraft>,
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

impl WorkspaceFilesystem {
    /// Whether `name` under `parent` is a Blob or large-Str sibling this mount can write.
    pub(crate) fn is_candidate_blob(&self, parent: u64, name: &OsStr) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(
            state.lookup_child(parent, name),
            Ok(Some(inode))
                if matches!(
                    state.nodes.get(&inode).map(|node| &node.kind),
                    Some(
                        NodeKind::Blob { candidate: Some(_), .. }
                            | NodeKind::Text { candidate: Some(_), .. },
                    )
                )
        )
    }

    /// Renames a Blob to a compatible suffix (41-vfs.md VFS-012): the bytes are not transcoded
    /// and the field is not renamed. Only the suffix hint moves, committed through the row's
    /// CAS boundary as a rewritten `data.orna` descriptor.
    pub(crate) fn rename_blob_suffix(
        &self,
        parent: u64,
        name: &OsStr,
        newname: &OsStr,
    ) -> Result<(), Errno> {
        if newname == name {
            return Ok(());
        }
        // The guard spans the EEXIST check, the baseline capture, the commit and the directory
        // update, so no other namespace operation on this mount can claim `newname` in between.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let inode = state
            .lookup_child(parent, name)
            .map_err(|_| Errno::EIO)?
            .ok_or(Errno::ENOENT)?;
        let old = match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Blob {
                candidate: Some(candidate),
                ..
            }) => candidate.field.clone(),
            _ => return Err(refused_mutation_errno()),
        };
        let newname_text = newname.to_str().ok_or(Errno::EPERM)?;
        let suffix =
            blob_rename_hint(&old.media_type, &old.name, newname_text).ok_or(Errno::EPERM)?;
        if state
            .lookup_child(parent, newname)
            .map_err(|_| Errno::EIO)?
            .is_some()
        {
            return Err(Errno::EEXIST);
        }
        let new = BlobField {
            file_name: newname_text.to_owned(),
            suffix,
            ..old.clone()
        };
        let (document_inode, document) = row_document(&mut state, parent)?;
        let save = document_save(&state, document_inode)?;
        // The baseline is read with the bytes it describes, so the CAS compares the same image.
        let baseline = save.recorded_baseline();
        let text = std::str::from_utf8(&document).map_err(|_| Errno::EIO)?;
        let rewritten = replace_descriptor_path(text, &old.name, &old.file_name, &new.file_name)
            .map_err(|error| {
                eprintln!("mounted Blob rename refused: {error}");
                Errno::EIO
            })?;
        let schema_identity = save.live_schema_identity().map_err(|_| Errno::EIO)?;
        if let Err(error) = commit_blob_descriptor_change(
            &save,
            &old.name,
            Some(new.clone()),
            rewritten.as_bytes(),
            baseline,
            schema_identity,
        ) {
            eprintln!("mounted Blob rename failed: {error}");
            drop(state);
            self.refresh_stale_blob_document(document_inode, &save, &error);
            return Err(error.errno());
        }
        if let Some(NodeKind::Document { bytes, .. }) = state
            .nodes
            .get_mut(&document_inode)
            .map(|node| &mut node.kind)
        {
            *bytes = rewritten.into_bytes();
        }
        // The commit is already durable, so a refused directory rename is reported, not failed.
        let renamed = state
            .directory_mut(parent)
            .is_some_and(|directory| directory.rename_entry(name, newname));
        if !renamed {
            eprintln!(
                "mounted Blob rename: \"{name}\" could not take the name \"{newname}\" in its directory",
                name = name.to_string_lossy(),
                newname = newname.to_string_lossy(),
            );
        }
        if let Some(NodeKind::Blob {
            candidate: Some(candidate),
            ..
        }) = state.nodes.get_mut(&inode).map(|node| &mut node.kind)
        {
            candidate.field = new;
        }
        Ok(())
    }

    /// Unlinks an optional Blob or large-Str field (ORNA-VFS-013, profiles/vfs-1.md): its descriptor becomes
    /// `null` through the row's CAS boundary, then its sibling file goes away. A required Blob
    /// field is refused with EPERM.
    pub(crate) fn unlink_blob(&self, parent: u64, name: &OsStr) -> Result<(), Errno> {
        // The guard spans the field lookup, the baseline capture, the commit and the directory
        // update, as in rename_blob_suffix.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(
            state.nodes.get(&parent).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return Err(Errno::EPERM);
        }
        let inode = state
            .lookup_child(parent, name)
            .map_err(|_| Errno::EIO)?
            .ok_or(Errno::ENOENT)?;
        let (is_text, field) = match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Blob {
                candidate: Some(candidate),
                ..
            }) => (false, candidate.field.clone()),
            Some(NodeKind::Text {
                candidate: Some(candidate),
                ..
            }) => (true, candidate.field.clone()),
            _ => return Err(refused_mutation_errno()),
        };
        if field.required {
            return Err(Errno::EPERM);
        }
        let (document_inode, document) = row_document(&mut state, parent)?;
        let save = document_save(&state, document_inode)?;
        // The baseline is read with the bytes it describes, so the CAS compares the same image.
        let baseline = save.recorded_baseline();
        let text = std::str::from_utf8(&document).map_err(|_| Errno::EIO)?;
        let nulled = if is_text {
            null_text_descriptor(text, &field)
        } else {
            null_descriptor(text, &field.name, &field.file_name)
        };
        let rewritten = nulled.map_err(|error| {
            eprintln!("mounted Blob unlink refused: {error}");
            Errno::EIO
        })?;
        let schema_identity = save.live_schema_identity().map_err(|_| Errno::EIO)?;
        if let Err(error) = commit_blob_descriptor_change(
            &save,
            &field.name,
            None,
            rewritten.as_bytes(),
            baseline,
            schema_identity,
        ) {
            eprintln!("mounted Blob unlink failed: {error}");
            drop(state);
            self.refresh_stale_blob_document(document_inode, &save, &error);
            return Err(error.errno());
        }
        if let Some(NodeKind::Document { bytes, .. }) = state
            .nodes
            .get_mut(&document_inode)
            .map(|node| &mut node.kind)
        {
            *bytes = rewritten.into_bytes();
        }
        // The commit is durable, so a missing entry is reported, not failed (LOW-4).
        if state
            .directory_mut(parent)
            .and_then(|directory| directory.remove_entry(name))
            .is_none()
        {
            eprintln!(
                "mounted Blob unlink: \"{name}\" had already left its directory",
                name = name.to_string_lossy(),
            );
        }
        // Open handles keep reading the captured bytes until their last release (profiles/vfs-1.md).
        state.retire_node(inode);
        Ok(())
    }

    /// After a stale refusal the row's current image becomes the recorded baseline, so the next
    /// rename or unlink compares against the durable row instead of the refused one.
    pub(crate) fn refresh_stale_blob_document(
        &self,
        document_inode: u64,
        save: &Arc<RowSaveContext>,
        error: &DocumentCommitError,
    ) {
        if !matches!(error, DocumentCommitError::Stale) {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = state.display_path(document_inode);
        state.report_refresh(document_inode, save, &path);
    }

    #[allow(clippy::needless_pass_by_value)]
    #[allow(clippy::too_many_lines)]
    pub(crate) fn from_snapshot(
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
                    RuntimeTableIdentity::new(pinned_schema.qualified_name(), table_id)
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
            None => Some(orna_runtime::process_owner_id()),
        };

        let project = match &writer {
            Some(writer) => Arc::clone(&writer.project),
            None => Arc::new(loader.load_committed_snapshot(repository, commit).map_err(
                |error| format!("committed source project could not be loaded: {error:?}"),
            )?),
        };
        // Reachable source modules are projected before any table directory, so a
        // generated component that collides with a source path escapes its first
        // byte instead of dropping the table (profiles/vfs-1.md:31-33).
        builder.add_source_modules(project.modules())?;

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
                .map_or((None, None), |(table, layout)| (Some(table), layout));
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
            let ScratchLoad {
                records,
                unreadable,
            } = writer.scratch_store.load()?;
            for record in &unreadable {
                eprintln!(
                    "unreadable persisted temporary scratch {} left on disk: {}",
                    record.path.display(),
                    record.reason
                );
            }
            // Restoring a row may need its table's committed schema history (ORNA-VFS-019). The
            // mount serves nothing yet, so each such scan is advanced here within one budget that
            // all of them share. A table whose scan runs out of it keeps its progress and resumes
            // on the next request for that table; the scratch of such a table is left on disk, as
            // any unresolved record is.
            let deadline = Instant::now() + HISTORICAL_RESOLVE_BUDGET;
            let relations: HashSet<[u8; 16]> = records
                .iter()
                .filter(|record| record.schema_identity.is_some())
                .map(|record| record.identity.relation_id)
                .collect();
            for relation_id in relations {
                writer.resolve_historical_blocking(relation_id, deadline);
            }
            for record in records {
                let logical_path = record.identity.logical_path.clone();
                if record.schema_identity.is_none() {
                    if let Err(error) = Self::retain_legacy_scratch(writer, &record) {
                        eprintln!(
                            "legacy persisted temporary scratch {logical_path} left on disk: {error}"
                        );
                    }
                    continue;
                }
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
            source_gate: Mutex::new(()),
            next_handle: AtomicU64::new(1),
        })
    }
    #[allow(clippy::needless_pass_by_value)]
    /// Keeps a legacy ORNAVFS1 candidate as a retained draft instead of binding it to the live
    /// schema (ORNA-VFS-019, ORNA-VFS-009). A legacy record has no creation schema identity, so its
    /// staleness cannot be decided: the candidate is listed as a retained draft (cause
    /// `sys.vfs.stale_edit`), its commit stays refused until the user re-applies it, and the scratch
    /// record is removed only after retention has succeeded. Returns the retained draft ID.
    pub(crate) fn retain_legacy_scratch(
        writer: &MountWriter,
        record: &PersistedTemporaryScratch,
    ) -> Result<String, String> {
        let candidate = &record.candidate;
        let mut file = File::open(&candidate.record).map_err(|error| {
            format!("persisted temporary scratch record could not be reopened: {error}")
        })?;
        let retained = writer
            .retained_drafts
            .retain(
                &record.identity.logical_path,
                SysFailureCode::VfsStaleEdit.as_str(),
                candidate.len,
                &mut QuotaCharge::empty(&writer.retained_drafts.quota),
                &mut |offset, length| {
                    file.seek(SeekFrom::Start(candidate.offset + offset))
                        .map_err(|error| {
                            RetentionError::io(
                                format!(
                                    "persisted temporary scratch bytes could not be read: {error}"
                                ),
                                &error,
                            )
                        })?;
                    let mut buffer = vec![0_u8; length];
                    file.read_exact(&mut buffer).map_err(|error| {
                        RetentionError::io(
                            format!("persisted temporary scratch bytes could not be read: {error}"),
                            &error,
                        )
                    })?;
                    Ok(buffer)
                },
            )
            .map_err(|error| error.to_string())?;
        writer.scratch_store.remove(&record.identity)?;
        eprintln!(
            "{}: legacy scratch record (ORNAVFS1) kept as retained draft {} in {} (sys.vfs.draft_retained); commit refused until re-applied",
            record.identity.logical_path,
            retained.id,
            retained.record.display()
        );
        Ok(retained.id)
    }

    pub(crate) fn restore_temporary_scratch(
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
        Self::restore_candidate(writer, &scratch, &record.candidate)?;
        let PersistedTemporaryScratch {
            identity,
            baseline,
            candidate,
            schema_identity,
        } = record;
        // `from_snapshot` routes a legacy ORNAVFS1 record (no creation identity) to
        // `retain_legacy_scratch` before any scratch is begun, so only identified records restore.
        let schema_identity = schema_identity.ok_or_else(|| {
            "legacy persisted temporary scratch cannot restore as a Temporary node".to_owned()
        })?;
        let name = identity.name.clone();
        builder.add_file(
            parent,
            &name,
            NodeKind::Temporary {
                len: candidate.len,
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity: Arc::new(identity),
                schema_identity,
            },
        )
    }

    /// Copies a recorded candidate into `scratch` in [`CANDIDATE_CHUNK_BYTES`]
    /// slices read from the record file, so restore never holds the whole
    /// candidate in memory.
    pub(crate) fn restore_candidate(
        writer: &Arc<MountWriter>,
        scratch: &TemporarySave<RowMapSnapshot>,
        candidate: &RecordedCandidate,
    ) -> Result<(), String> {
        let mut file = File::open(&candidate.record).map_err(|error| {
            format!("persisted temporary scratch record could not be reopened: {error}")
        })?;
        file.seek(SeekFrom::Start(candidate.offset))
            .map_err(|error| {
                format!("persisted temporary scratch bytes could not be read: {error}")
            })?;
        let buffer_len = candidate.len.min(CANDIDATE_CHUNK_BYTES as u64);
        let mut buffer = vec![0_u8; usize::try_from(buffer_len).unwrap_or(CANDIDATE_CHUNK_BYTES)];
        let mut offset = 0_u64;
        while offset < candidate.len {
            let wanted = usize::try_from(candidate.len - offset)
                .unwrap_or(usize::MAX)
                .min(CANDIDATE_CHUNK_BYTES);
            let slice = &mut buffer[..wanted];
            file.read_exact(slice).map_err(|error| {
                format!("persisted temporary scratch bytes could not be read: {error}")
            })?;
            writer
                .runtime
                .block_on(scratch.write_at(offset, slice))
                .map_err(|error| {
                    format!("persisted temporary scratch bytes could not be restored: {error:?}")
                })?;
            offset += wanted as u64;
        }
        Ok(())
    }

    pub(crate) fn update_writable_len(&self, inode: u64, len: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(node) = state.nodes.get_mut(&inode)
            && let NodeKind::Temporary { len: current, .. } = &mut node.kind
        {
            *current = len;
        }
    }
    pub(crate) fn has_unresolved_drafts(&self) -> bool {
        !self.unresolved_drafts().is_empty()
    }

    /// Records a refused document save (ORNA-VFS-009): the candidate is kept in
    /// recovery storage under a stable draft ID before its handle can go away.
    /// A stale save also re-projects the row, so a new open shows and saves
    /// against the row another writer accepted (W15-03).
    pub(crate) fn refuse_document(
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
    pub(crate) fn accept_document(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        inode: u64,
        save: &Arc<RowSaveContext>,
        draft: &Arc<EditDraft<RowMapSnapshot>>,
        opened: &Arc<Mutex<DocumentBaseline>>,
    ) -> Result<(), DocumentCommitError> {
        // A path truncate issued while this writer was open is recorded, not applied to
        // the draft. It is applied before the candidate is judged, so the accepted or
        // refused candidate reflects the cut (ORNA-VFS-009; POSIX arrival order).
        if let Some(size) = draft_state.pending_truncates.get(&inode).copied() {
            save.writer
                .runtime
                .block_on(draft.truncate(size))
                .map_err(|error| {
                    DocumentCommitError::Failed(format!(
                        "candidate could not be cut to {size} bytes: {error:?}"
                    ))
                })?;
            draft_state.pending_truncates.remove(&inode);
        }
        // The handle commits against the row it captured at open and advances only with its
        // own accepted candidate (ORNA-VFS-011), so a row another writer accepted since then
        // fails the compare. A handle pinned to a superseded generation whose draft holds a
        // candidate is refused before the compare: its generation is retired (see
        // `replace_document_generation`). A clean superseded handle has no candidate to refuse,
        // so it reaches the commit, which reports NoCandidate; fsync acknowledges that.
        let (based_on, schema_identity) = {
            let baseline = opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (baseline.row.clone(), baseline.schema_identity.clone())
        };
        let superseded = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_superseded(inode);
        let refuse_superseded = superseded && save.writer.runtime.block_on(draft.has_candidate());
        let outcome = if refuse_superseded {
            Err(DocumentCommitError::Stale)
        } else {
            save.commit_document_against(draft, true, based_on, &schema_identity)
        };
        let accepted_row = match outcome {
            Ok(accepted_row) => accepted_row,
            Err(error) => {
                // NoCandidate: the handle submitted no bytes, so there is no candidate to
                // retain and nothing blocks unmount (ORNA-VFS-009, EDIT-1 step 2).
                if matches!(error, DocumentCommitError::NoCandidate) {
                    return Err(error);
                }
                // A candidate recovery storage cannot keep reports the retention errno
                // and stays unresolved (ORNA-VFS-009).
                let retained =
                    self.refuse_document(draft_state, inode, handle, save, draft, &error);
                return Err(match retained {
                    Some(errno) => DocumentCommitError::Unretained {
                        errno,
                        cause: error.cause(),
                    },
                    None => error,
                });
            }
        };
        if let Some(row) = accepted_row {
            let mut baseline = opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            baseline.row = Some(row);
        }
        let bytes = save
            .writer
            .runtime
            .block_on(draft.candidate_bytes())
            .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A generation superseded while this commit ran keeps the bytes it captured
        // (profiles/vfs-1.md:126-133), so only a live generation takes the accepted row.
        let retired = state.is_superseded(inode);
        if let Some(NodeKind::Document {
            bytes: current,
            save: current_save,
        }) = state
            .nodes
            .get_mut(&inode)
            .map(|node| &mut node.kind)
            .filter(|_| !retired)
        {
            *current = bytes.to_vec();
            // A handle opened before a refresh saves through its own
            // context; new opens now continue from the row it accepted.
            if !Arc::ptr_eq(current_save, save) {
                *current_save = Arc::clone(save);
            }
        }
        // The document shows the accepted row, so a Str over 64 KiB is named by its `.txt`.
        if !retired && let Err(error) = state.project_accepted_document(inode, save) {
            eprintln!("the accepted row document could not be re-projected: {error}");
        }
        let accepted_path = state.display_path(inode);
        drop(state);
        draft_state.take_unresolved(handle);
        draft_state.clear_unaccepted_write(handle);
        // The accepted save supersedes the refusals it replaces (ORNA-VFS-009, ORNA-VFS-024).
        draft_state.supersede_refusals(inode, &accepted_path);
        Ok(())
    }

    /// Commits the pending state of an open handle: a document candidate is
    /// accepted, and a scratch file is persisted to the scratch store.
    pub(crate) fn commit_open_handle(
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
            Some(OpenFile::Document(save, draft, opened)) => {
                self.accept_document(draft_state, handle, inode.0, &save, &draft, &opened)
            }
            Some(OpenFile::Temporary(save, target, scratch, Some(field)))
                if draft_state.has_blob(handle) =>
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
                        identity,
                        baseline,
                        schema_identity,
                        ..
                    }) => save
                        .fsync_temporary(&scratch, identity, schema_identity, baseline.as_deref())
                        .map_err(DocumentCommitError::Failed),
                    // Rename already accepted and removed this scratch node.
                    None => Ok(()),
                    _ => Err(DocumentCommitError::Failed(
                        "open scratch node is unavailable".to_owned(),
                    )),
                }
            }
            Some(OpenFile::Source(_)) => Err(DocumentCommitError::Failed(
                "source module commits go through source_sync".to_owned(),
            )),
            None => Err(DocumentCommitError::Failed(
                "file handle is not open".to_owned(),
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    /// Accepts a Blob write-open's candidate as the row's replacement (41-vfs.md EDIT-1).
    /// The baseline is the one captured when the handle opened (ORNA-VFS-011), so an accept by a
    /// sibling handle since then fails the row CAS and the candidate is refused. After an accept,
    /// the handle's next candidate starts from the accepted bytes (EDIT-1 step 3).
    pub(crate) fn commit_blob_candidate(
        &self,
        draft_state: &mut DraftState,
        handle: u64,
        save: &Arc<RowSaveContext>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &Arc<TemporarySave<RowMapSnapshot>>,
        field: &BlobField,
    ) -> Result<(), DocumentCommitError> {
        let Some(blob) = draft_state.blob(handle) else {
            return Err(DocumentCommitError::Failed(
                "open Blob candidate has no captured baseline".to_owned(),
            ));
        };
        let inode = blob.generation;
        let baseline = blob.baseline.clone();
        let schema_identity = blob.schema_identity.clone();
        let path = blob.path.clone();
        let (parent, text_field) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode) {
                Some(node)
                    if matches!(
                        node.kind,
                        NodeKind::Blob {
                            candidate: Some(_),
                            ..
                        } | NodeKind::BlobBytes(_)
                            | NodeKind::Text {
                                candidate: Some(_),
                                ..
                            }
                    ) =>
                {
                    (node.parent, matches!(node.kind, NodeKind::Text { .. }))
                }
                // The node is gone, so there is no candidate left to accept.
                None => return Ok(()),
                Some(_) => {
                    return Err(DocumentCommitError::Failed(
                        "open Blob candidate node is unavailable".to_owned(),
                    ));
                }
            }
        };
        let accepted = match save.commit_blob_temporary(
            target,
            scratch,
            field,
            baseline,
            schema_identity.clone(),
            true,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                return Err(retain_refused_scratch(
                    draft_state,
                    save,
                    scratch,
                    inode,
                    &path,
                    error,
                ));
            }
        };
        // The accepted image is the one the vfs published; it is not read back from scratch.
        let bytes = accepted.bytes;
        let generation = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace_blob_generation(
                parent,
                OsStr::new(&field.file_name),
                inode,
                Arc::clone(&bytes),
            )
            .map_err(DocumentCommitError::Failed)?;
        if text_field {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Ok(Some(document)) = state.lookup_child(parent, OsStr::new("data.orna"))
                && let Err(error) = state.project_accepted_document(document, save)
            {
                eprintln!("the accepted row document could not be re-projected: {error}");
            }
        }
        // The accepted candidate is no longer draft state (same as an accepted rename).
        draft_state.retained_scratch.remove(&inode);
        draft_state.rejected_scratch.remove(&inode);
        draft_state.supersede_refusals(inode, &path);
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
        // The next candidate is seeded one bounded chunk at a time; the accepted bytes are never
        // copied whole.
        let accepted_length = bytes.len();
        let mut seeded = 0_u64;
        while seeded < accepted_length {
            let chunk = bytes
                .read_range(seeded, CANDIDATE_CHUNK_BYTES)
                .map_err(|error| {
                    DocumentCommitError::Failed(format!(
                        "next Blob candidate could not be read: {error}"
                    ))
                })?;
            if chunk.is_empty() {
                return Err(DocumentCommitError::Failed(
                    "next Blob candidate ended before its recorded length".into(),
                ));
            }
            save.writer
                .runtime
                .block_on(next_scratch.write_at(seeded, &chunk))
                .map_err(|error| {
                    DocumentCommitError::Failed(format!(
                        "next Blob candidate could not be seeded: {error:?}"
                    ))
                })?;
            seeded += chunk.len() as u64;
        }
        let next_path = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.track_open(generation);
            state.track_release(inode);
            state.display_path(generation)
        };
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
        draft_state.set_blob(
            handle,
            BlobHandle {
                generation,
                baseline: accepted_baseline,
                path: next_path,
                schema_identity,
            },
        );
        Ok(())
    }

    /// Keeps the candidate of a document released while dirty (EDIT-1 step 5).
    /// Release only cleans up, so the candidate is retained and reported, never
    /// accepted.
    pub(crate) fn retain_unsynced_document(
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

    #[allow(clippy::too_many_arguments)]
    /// Writes a candidate that was not accepted to recovery storage and records
    /// it against the file for unmount reporting (ORNA-VFS-009). A stale save
    /// also re-projects the row, so a new open shows and saves against the row
    /// another writer accepted (W15-03).
    pub(crate) fn retain_candidate(
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
        let mut candidate = save.writer.runtime.block_on(draft.take_candidate_charge());
        let retained = {
            let mut read_chunk = |offset: u64, length: usize| {
                save.writer
                    .runtime
                    .block_on(draft.read_range(offset, length))
                    .map_err(|error| RetentionError::from(candidate_unreadable(error)))
            };
            save.writer
                .retained_drafts
                .retain(&path, cause, len, &mut candidate, &mut read_chunk)
        };
        save.writer
            .runtime
            .block_on(draft.restore_candidate_charge(candidate));
        let (retained_draft, unretained, failure) = match retained {
            Ok(retained) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate retained as draft {} in {} (sys.vfs.draft_retained)",
                    retained.id,
                    retained.record.display()
                );
                (Some(retained.id), Vec::new(), None)
            }
            Err(retain_error) => {
                eprintln!(
                    "{path}: not accepted ({cause}); candidate could not be retained: {retain_error}"
                );
                // Recovery storage refused the candidate: the live draft is the only copy
                // left, so it stays unresolved and is retried at the next retention.
                (
                    None,
                    vec![(Arc::clone(save), CandidateSource::Draft(Arc::clone(draft)))],
                    Some(retain_error.errno),
                )
            }
        };
        let refusal = DraftRefusal {
            cause,
            path: path.clone(),
            handle: Some(handle),
            candidate: Some(Arc::clone(draft)),
            draft: retained_draft,
            unretained,
        };
        match draft_state.rejected_documents.entry(inode) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let kept = entry.get_mut();
                if kept.unretained.is_empty() {
                    *kept = refusal;
                } else {
                    // An earlier refusal of this inode still holds an unretained source: keep it
                    // alongside this one rather than replacing it, so neither live draft is lost.
                    kept.unretained.extend(refusal.unretained);
                    kept.handle = refusal.handle;
                    kept.candidate = refusal.candidate;
                    if kept.draft.is_none() {
                        kept.draft = refusal.draft;
                    }
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(refusal);
            }
        }
        if refresh {
            state.report_refresh(inode, save, &path);
        }
        failure
    }

    /// The unresolved drafts of this mount, one per path. Both draft-list xattrs
    /// render these, so the unmount refusal and `mount status` name the same drafts.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn unresolved_drafts_of(
        draft_state: &DraftState,
        state: &FsBuilder,
        for_unmount: bool,
    ) -> Vec<UnresolvedDraft> {
        // The generated `sys` code of an internal cause, when the cause names one.
        fn spec_code(cause: &'static str) -> Option<&'static str> {
            SysFailureCode::parse(cause).map(SysFailureCode::as_str)
        }
        let draft_retained = SysFailureCode::VfsDraftRetained.as_str();
        // inode -> (code, draft id, detail). The code is the spec code of the internal
        // cause; otherwise `draft_retained` when a candidate was kept; otherwise empty.
        let mut unresolved: BTreeMap<u64, (&'static str, String, String)> = BTreeMap::new();
        // The path each refusal captured when recorded; the refused node may leave the tree first.
        let mut captured: HashMap<u64, String> = HashMap::new();
        for (inode, refusal) in &draft_state.rejected_documents {
            // Unmount refuses only drafts the mount can still resolve (profiles/vfs-1.md:144). A
            // candidate recovery storage kept whose document can no longer be opened is already
            // explicitly retained there, and the mount cannot serve its row to resolve it.
            if for_unmount && refusal_is_unservable_retained(state, *inode, refusal) {
                continue;
            }
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.draft.as_ref().map(|_| draft_retained))
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
            captured.insert(*inode, refusal.path.clone());
            unresolved.insert(*inode, (code, draft, detail));
        }
        for (inode, source) in &draft_state.source_drafts {
            let Some(refusal) = source.refusal() else {
                if source.is_unsynced() {
                    unresolved.entry(*inode).or_insert_with(|| {
                        (
                            "",
                            String::new(),
                            "unsynced writes in an open file".to_owned(),
                        )
                    });
                }
                continue;
            };
            // A candidate kept as a draft whose node left the mount has nothing left to resolve
            // (ORNA-VFS-009).
            if refusal.draft.is_some() && state.path_of(*inode).is_none() {
                continue;
            }
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.draft.as_ref().map(|_| draft_retained))
                .unwrap_or("");
            let (draft, detail) = match &refusal.draft {
                Some(draft) => (
                    draft.clone(),
                    format!(
                        "not accepted ({}); candidate retained as draft {draft}",
                        refusal.cause
                    ),
                ),
                None => (
                    String::new(),
                    format!("not accepted ({}); candidate not retained", refusal.cause),
                ),
            };
            unresolved.entry(*inode).or_insert((code, draft, detail));
        }
        for (inode, refusal) in &draft_state.rejected_scratch {
            captured.insert(*inode, refusal.path.clone());
            let code = spec_code(refusal.cause)
                .or_else(|| refusal.kept.then_some(draft_retained))
                .unwrap_or("");
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    code,
                    String::new(),
                    format!("scratch rename refused ({})", refusal.cause),
                )
            });
        }
        for inode in &draft_state.retained_scratch {
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    draft_retained,
                    String::new(),
                    "scratch file not renamed".to_owned(),
                )
            });
        }
        for inode in draft_state.unresolved_inodes() {
            unresolved.entry(inode).or_insert_with(|| {
                (
                    "",
                    String::new(),
                    "unsynced writes in an open file".to_owned(),
                )
            });
        }
        for (inode, size) in &draft_state.pending_truncates {
            unresolved.entry(*inode).or_insert_with(|| {
                (
                    "",
                    String::new(),
                    format!("truncate to {size} bytes not synced"),
                )
            });
        }
        let mut drafts = Vec::with_capacity(unresolved.len());
        for (inode, (code, draft, detail)) in unresolved {
            drafts.push(UnresolvedDraft {
                path: state
                    .path_through(inode, true)
                    .or_else(|| captured.get(&inode).cloned())
                    .unwrap_or_else(|| state.display_path(inode)),
                code,
                draft,
                detail,
            });
        }
        drafts
    }

    pub(crate) fn unresolved_drafts(&self) -> Vec<UnresolvedDraft> {
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::unresolved_drafts_of(&draft_state, &state, false)
    }

    /// One `path<TAB>detail` line per unresolved draft, for an unmount refusal.
    pub(crate) fn unresolved_draft_report(&self) -> String {
        self.render_unresolved_drafts(|draft| format!("{}\t{}", draft.path, draft.detail))
    }

    /// One `path<TAB>code<TAB>draft<TAB>detail` line per unresolved draft, for
    /// `mount status`. `code` is the `sys.*` Orna cause (or `sys.vfs.draft_retained`
    /// for a kept candidate with no other cause) and `draft` the retained draft id;
    /// each is empty when the draft has none.
    pub(crate) fn unresolved_draft_fields(&self) -> String {
        self.render_unresolved_drafts(|draft| {
            format!(
                "{}\t{}\t{}\t{}",
                draft.path, draft.code, draft.draft, draft.detail
            )
        })
    }

    /// Renders the drafts as sorted lines, bounded by both a line count and a byte
    /// budget so the xattr value stays small enough for the kernel to return. A draft
    /// that does not fit is not listed; it is counted on one final `\tomitted\tN` line.
    /// That line has an empty first field, which no draft path can have, so it is
    /// never parsed as a draft.
    pub(crate) fn render_unresolved_drafts(
        &self,
        line: impl Fn(&UnresolvedDraft) -> String,
    ) -> String {
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

    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_lines)]
    /// Stop-time retention (ORNA-VFS-024): no unresolved draft is silently
    /// discarded when the mount stops. Refused documents are already in
    /// recovery storage; unsynced document writes and truncates are added to
    /// it; scratch files are persisted to the scratch store, which the next
    /// mount restores. Every kept draft is reported with
    /// `sys.vfs.draft_retained`. A candidate that recovery storage refuses keeps
    /// its bytes as an unresolved refusal, so a later attempt retries it.
    /// Returns the errno of the refusal that kept a draft, or `None` when every draft was retained.
    pub(crate) fn retain_all_drafts(&self, draft_state: &mut DraftState) -> Option<Errno> {
        let source_refused = self.retain_unsynced_sources(draft_state);
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
        // A path truncate cuts the newest writer draft of its inode. That cut is
        // the record the draft is retained as, so the same draft's untruncated
        // copy is superseded and is not retained a second time.
        let mut truncations = Vec::new();
        for (inode, size) in &draft_state.pending_truncates {
            if let Some(NodeKind::Document { save, .. }) =
                state.nodes.get(inode).map(|node| &node.kind)
            {
                // A path truncate cuts the newest writer draft: the open writer's,
                // else the candidate a released writer's refusal still holds.
                let latest = draft_state
                    .unresolved_handles()
                    .filter(|(_, dirty_inode)| *dirty_inode == *inode)
                    .filter_map(|(handle, _)| match open_files.get(&handle) {
                        Some(OpenFile::Document(_, draft, _)) => Some((handle, Arc::clone(draft))),
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
                truncations.push((*inode, *size, Arc::clone(save), latest));
            }
        }
        for (handle, inode) in draft_state.unresolved_handles() {
            if let Some(OpenFile::Document(save, draft, _)) = open_files.get(&handle) {
                let cut_by_truncate = truncations.iter().any(|(truncated, _, _, latest)| {
                    *truncated == inode
                        && latest
                            .as_ref()
                            .is_some_and(|latest| Arc::ptr_eq(latest, draft))
                });
                if cut_by_truncate {
                    continue;
                }
                candidates.push((
                    inode,
                    Some(handle),
                    "unsynced",
                    Arc::clone(save),
                    CandidateSource::Draft(Arc::clone(draft)),
                ));
            }
        }
        for (inode, size, save, latest) in &truncations {
            candidates.push((
                *inode,
                None,
                "unsynced",
                Arc::clone(save),
                CandidateSource::Truncated {
                    size: *size,
                    latest: latest.clone(),
                },
            ));
        }
        let mut unresolved: HashMap<u64, DraftRefusal> = HashMap::new();
        let mut refused: Option<Errno> = None;
        let mut superseded: HashMap<u64, (String, String)> = HashMap::new();
        for (inode, refusal) in std::mem::take(&mut draft_state.rejected_documents) {
            // A refusal whose candidate a path truncate cuts is superseded by the
            // truncated record once that record is retained.
            let cut_by_truncate = truncations.iter().any(|(truncated, _, _, latest)| {
                *truncated == inode
                    && match (latest, refusal.candidate.as_ref()) {
                        (Some(latest), Some(candidate)) => Arc::ptr_eq(latest, candidate),
                        // Neither a writer nor a refusal candidate: the cut is of
                        // the refusal itself, so its untruncated copy is superseded.
                        (None, None) => true,
                        _ => false,
                    }
            });
            for (save, source) in refusal.unretained {
                // A cut candidate is retained once, as its truncated record above; its
                // untruncated copy is not a second record (ORNA-VFS-009, EDIT-1 step 4).
                if !cut_by_truncate {
                    candidates.push((inode, refusal.handle, refusal.cause, save, source));
                }
            }
            if let Some(draft) = refusal.draft {
                let path = state.display_path(inode);
                let report = format!(
                    "retained draft {draft} for {path} ({}; sys.vfs.draft_retained)",
                    refusal.cause
                );
                if cut_by_truncate {
                    superseded.insert(inode, (draft, report));
                } else {
                    reports.insert(report);
                }
            }
        }
        for (inode, handle, cause, save, source) in candidates {
            let path = state.display_path(inode);
            let len = match &source {
                CandidateSource::Draft(draft) => {
                    save.writer.runtime.block_on(draft.candidate_len())
                }
                CandidateSource::Truncated { size, .. } => *size,
            };
            let truncated = matches!(source, CandidateSource::Truncated { .. });
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
                        Some(NodeKind::Document { bytes, .. }) => truncated_window(
                            bytes.len() as u64,
                            *size,
                            offset,
                            length,
                            |start, count| Ok(stashed_range(bytes, start, count)),
                        ),
                        _ => Err(RetentionError::from(format!(
                            "{path} has no accepted baseline to truncate"
                        ))),
                    },
                },
            };
            let mut candidate = match &source {
                CandidateSource::Draft(draft) => {
                    save.writer.runtime.block_on(draft.take_candidate_charge())
                }
                CandidateSource::Truncated { .. } => {
                    QuotaCharge::empty(&save.writer.retained_drafts.quota)
                }
            };
            let retained = save.writer.retained_drafts.retain(
                &path,
                cause,
                len,
                &mut candidate,
                &mut read_chunk,
            );
            if let CandidateSource::Draft(draft) = &source {
                save.writer
                    .runtime
                    .block_on(draft.restore_candidate_charge(candidate));
            }
            match retained {
                Ok(retained) => {
                    reports.insert(format!(
                        "retained draft {} for {path} ({cause}; sys.vfs.draft_retained) in {}",
                        retained.id,
                        retained.record.display()
                    ));
                    if truncated
                        && let Some((old, _)) = superseded.remove(&inode)
                        && old != retained.id
                        && let Err(error) = save.writer.retained_drafts.remove(&old)
                    {
                        reports.insert(format!(
                            "superseded draft {old} for {path} could not be removed: {error}"
                        ));
                    }
                }
                Err(error) => {
                    refused = worst_retention_errno(refused, error.errno);
                    reports.insert(format!(
                        "draft for {path} ({cause}) could not be retained: {error}"
                    ));
                    if truncated && let Some((_, report)) = superseded.remove(&inode) {
                        reports.insert(report);
                    }
                    // Recovery storage refused the candidate: it stays unresolved, and its
                    // live source is retried at the next retention rather than copied here.
                    let live = source.live_draft();
                    match unresolved.entry(inode) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            // A second refused candidate of this inode: both stay unresolved, so
                            // neither the earlier unretained source nor its live draft is dropped.
                            let kept = entry.get_mut();
                            kept.unretained.push((save, source));
                            if live.is_some() {
                                kept.candidate = live;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(DraftRefusal {
                                cause,
                                path,
                                draft: None,
                                handle,
                                candidate: live,
                                unretained: vec![(save, source)],
                            });
                        }
                    }
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
                schema_identity,
                baseline,
                ..
            }) = state.nodes.get(&inode).map(|node| &node.kind)
            else {
                continue;
            };
            let path = state.display_path(inode);
            match save.fsync_temporary(scratch, identity, schema_identity, baseline.as_deref()) {
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
        draft_state.clear_unresolved();
        draft_state.pending_truncates.clear();
        draft_state
            .retained_scratch
            .retain(|inode| unkept_scratch.contains(inode));
        draft_state
            .rejected_scratch
            .retain(|inode, _| unkept_scratch.contains(inode));
        draft_state.rejected_documents = unresolved;
        if let Some(errno) = source_refused {
            refused = worst_retention_errno(refused, errno);
        }
        refused
    }
}

impl Filesystem for WorkspaceFilesystem {
    #[allow(clippy::type_complexity)]
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
            Some(XATTR_UNRESOLVED_DRAFTS) => {
                if self.has_unresolved_drafts() {
                    b"1".to_vec()
                } else {
                    b"0".to_vec()
                }
            }
            Some(XATTR_UNRESOLVED_DRAFT_LIST) => self.unresolved_draft_report().into_bytes(),
            Some(XATTR_UNRESOLVED_DRAFT_FIELDS) => self.unresolved_draft_fields().into_bytes(),
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
        if name.to_str() == Some(XATTR_EDIT_COMMIT) {
            if inode.0 != ROOT_INODE {
                reply.error(Errno::ENODATA);
                return;
            }
            if flags != 0 || position != 0 {
                reply.error(Errno::EINVAL);
                return;
            }
            let phase = self
                .draft_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .phase;
            if phase != MountPhase::Serving {
                reply.error(Errno::EBUSY);
                return;
            }
            // Commit runs without the draft-state lock so it cannot deadlock
            // with the FUSE operations that take it (ORNA-VFS-015).
            match self.commit_requested_edit(value) {
                Ok(()) => reply.ok(),
                Err(EditError::Stale { message, .. }) => {
                    eprintln!("explicit edit refused: {message}");
                    // ESTALE is the stale signal on this xattr path. profiles/vfs-1.md:138 names
                    // EAGAIN, but the kernel rewrites an EAGAIN setxattr reply to EOPNOTSUPP
                    // (fs/xattr.c:245-246 on v6.19; tests/vfs_setxattr_errno_kernel_limit.rs), so
                    // `orna edit` would report E2010 instead of E2000 (ornadb-eufo.1.1; spec
                    // amendment ornadb-eufo.1.2). The kernel retries an ESTALE setxattr once with
                    // LOOKUP_REVAL: the same proof observes 2 daemon calls on 6.19.10. Each call
                    // reaches this arm before any row write (the digest check returns before
                    // `prepared.commit`), so the retry refuses again and writes nothing.
                    reply.error(Errno::ESTALE);
                }
                Err(EditError::Failed { message, .. }) => {
                    eprintln!("explicit edit failed: {message}");
                    reply.error(Errno::EIO);
                }
            }
            return;
        }
        let retain = match name.to_str() {
            Some(XATTR_UNMOUNT_QUIESCE) if inode.0 == ROOT_INODE => false,
            Some(XATTR_RETAIN_DRAFTS) if inode.0 == ROOT_INODE => true,
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
            draft_state.phase = MountPhase::Quiescing;
            match self.retain_all_drafts(&mut draft_state) {
                None => reply.ok(),
                Some(errno) => reply.error(errno),
            }
        } else if value == b"1" {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !Self::unresolved_drafts_of(&draft_state, &state, true).is_empty() {
                reply.error(Errno::EBUSY);
                return;
            }
            draft_state.phase = MountPhase::Quiescing;
            reply.ok();
        } else if value == b"0" {
            draft_state.phase = MountPhase::Serving;
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
        reply.error(refuse_unsupported(&format!(
            "removexattr \"{name}\"",
            name = name.to_string_lossy()
        )));
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
        let looked_up = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lookup_child(parent.0, name);
        match looked_up {
            Ok(Some(inode)) => match self.attr_of(inode) {
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
                OpenFile::Document(save, draft, _) => {
                    save.writer.runtime.block_on(draft.candidate_len())
                }
                OpenFile::Temporary(save, _, scratch, _) => {
                    save.writer.runtime.block_on(scratch.candidate_len())
                }
                OpenFile::Source(source_inode) => self.source_candidate_len(*source_inode),
            })
        };
        // fstat without a handle reaches here; an open Document, Temporary or
        // Source candidate is the file.
        let candidate_size = if let Some(handle) = handle {
            open_len(handle.0)
        } else {
            {
                let handles: Vec<u64> = self
                    .draft_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .unresolved_handles()
                    .filter(|(_, dirty_inode)| *dirty_inode == inode.0)
                    .map(|(handle, _)| handle)
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
                                    OpenFile::Document(..)
                                        | OpenFile::Temporary(_, _, _, Some(_))
                                        | OpenFile::Source(_)
                                )
                            })
                    })
                    .max()
                    .and_then(open_len)
            }
        };

        match self.attr_of(inode.0) {
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

    #[allow(clippy::too_many_lines)]
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
            state.track_open(inode.0);
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
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        if write_mode && self.is_source_module(inode.0) {
            let truncate = flags.0 & libc::O_TRUNC != 0;
            match self.source_open_write(&mut draft_state, inode.0, truncate, request.pid()) {
                Ok(handle) => reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS),
                Err(errno) => reply.error(errno),
            }
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
                    let length = reference.content_identity().length();
                    let seed_len = if flags.0 & libc::O_TRUNC != 0 {
                        0
                    } else {
                        length
                    };
                    if seed_len > DRAFT_FILE_BOUND_BYTES as u64 {
                        reply.error(Errno::EDQUOT);
                        return;
                    }
                    // The admitted Blob is copied into the scratch one bounded range at a time,
                    // so its whole content is never read into memory.
                    let seeded = (|| {
                        let target = save.temporary_target(Some(&field))?;
                        let scratch =
                            Arc::new(save.writer.runtime.block_on(
                                target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES),
                            ));
                        if seed_len > 0 {
                            let scope = graph.open_read_scope().map_err(|error| {
                                format!("Blob read scope could not be opened: {error:?}")
                            })?;
                            let mut offset = 0_u64;
                            while offset < seed_len {
                                let end = offset
                                    .saturating_add(CANDIDATE_CHUNK_BYTES as u64)
                                    .min(seed_len);
                                let range = graph
                                    .read_blob_range(&reference, offset..end, &scope)
                                    .map_err(|error| {
                                        format!("Blob content could not be read: {error:?}")
                                    })?;
                                let chunk = range.bytes();
                                if chunk.len() as u64 != end - offset {
                                    return Err(
                                        "Blob content ended before its recorded length".to_owned()
                                    );
                                }
                                save.writer
                                    .runtime
                                    .block_on(scratch.write_at(offset, chunk))
                                    .map_err(|error| {
                                        format!("Blob candidate could not be seeded: {error:?}")
                                    })?;
                                offset = end;
                            }
                        }
                        Ok::<_, String>((target, scratch))
                    })();
                    match seeded {
                        Ok((target, scratch)) => (save, target, Some(scratch), Some(field)),
                        Err(error) => {
                            eprintln!("mounted Blob candidate could not be opened: {error}");
                            reply.error(Errno::EIO);
                            return;
                        }
                    }
                }
                Some(NodeKind::Text {
                    bytes,
                    candidate: Some(candidate),
                }) if write_mode => {
                    let save = Arc::clone(&candidate.save);
                    let field = candidate.field.clone();
                    let seed_len = if flags.0 & libc::O_TRUNC != 0 {
                        0
                    } else {
                        bytes.len()
                    };
                    if seed_len > DRAFT_FILE_BOUND_BYTES as u64 {
                        reply.error(Errno::EDQUOT);
                        return;
                    }
                    // The accepted text is copied into the scratch one bounded chunk at a time,
                    // so the whole candidate is never materialised in memory.
                    let seeded =
                        (|| {
                            let target = save.temporary_target(Some(&field))?;
                            let scratch = Arc::new(save.writer.runtime.block_on(
                                target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES),
                            ));
                            let mut offset = 0_u64;
                            while offset < seed_len {
                                let wanted = usize::try_from(seed_len - offset)
                                    .unwrap_or(usize::MAX)
                                    .min(CANDIDATE_CHUNK_BYTES);
                                let chunk = bytes.read_range(offset, wanted).map_err(|error| {
                                    format!("Text candidate could not be read: {error}")
                                })?;
                                if chunk.is_empty() {
                                    return Err("Text candidate ended before its recorded length"
                                        .to_owned());
                                }
                                save.writer
                                    .runtime
                                    .block_on(scratch.write_at(offset, &chunk))
                                    .map_err(|error| {
                                        format!("Text candidate could not be seeded: {error:?}")
                                    })?;
                                offset += chunk.len() as u64;
                            }
                            Ok::<_, String>((target, scratch))
                        })();
                    match seeded {
                        Ok((target, scratch)) => (save, target, Some(scratch), Some(field)),
                        Err(error) => {
                            eprintln!("mounted Text candidate could not be opened: {error}");
                            reply.error(Errno::EIO);
                            return;
                        }
                    }
                }
                Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_) | NodeKind::Text { .. }) => {
                    if write_mode {
                        reply.error(Errno::EPERM);
                        return;
                    }
                    state.track_open(inode.0);
                    reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
                    return;
                }
                Some(NodeKind::Bytes(_)) => {
                    if write_mode {
                        reply.error(refuse_unsupported("open for write"));
                        return;
                    }
                    state.track_open(inode.0);
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
        // A read-only open views a pending path truncate but is not a writer: it holds no
        // candidate, so its release retains nothing and the truncate is retained once at stop.
        if write_mode && truncate_size.is_some() {
            draft_state.set_unresolved(handle, inode.0);
            draft_state.candidate_written(handle, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        if let Some(scratch) = scratch {
            if let Some(size) = truncate_size {
                let result = save.writer.runtime.block_on(scratch.truncate(size));
                if result.is_err() {
                    draft_state.take_unresolved(handle);
                    if is_scratch && !had_retained_scratch {
                        draft_state.retained_scratch.remove(&inode.0);
                    }
                    reply.error(draft_errno(&result));
                    return;
                }
            }
            if blob_field.is_some() {
                // The handle holds its generation, the row baseline and the table schema it
                // captured at open (ORNA-VFS-011, ORNA-VFS-019).
                let schema_identity = match save.live_schema_identity() {
                    Ok(identity) => identity,
                    Err(error) => {
                        eprintln!("mounted blob open could not capture the table schema: {error}");
                        draft_state.take_unresolved(handle);
                        if is_scratch && !had_retained_scratch {
                            draft_state.retained_scratch.remove(&inode.0);
                        }
                        reply.error(Errno::EIO);
                        return;
                    }
                };
                let baseline = save
                    .expected_baseline_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let path = {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.track_open(inode.0);
                    state.display_path(inode.0)
                };
                draft_state.set_blob(
                    handle,
                    BlobHandle {
                        generation: inode.0,
                        baseline,
                        path,
                        schema_identity,
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
            if write_mode && truncate_size.is_some() {
                draft_state.pending_truncates.remove(&inode.0);
            }
            if write_mode {
                draft_state.set_writer_owner(handle, request.pid());
            }
            reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
            return;
        }
        let schema_identity = match orna_cli_support::table_schema::committed_table_schema_identity(
            &save.writer.repository,
            save.table.object_id(),
        ) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("mounted document open could not capture the table schema: {error}");
                draft_state.take_unresolved(handle);
                reply.error(Errno::EIO);
                return;
            }
        };
        let draft = Arc::new(
            save.writer
                .runtime
                .block_on(save.target.open_draft(DRAFT_FILE_BOUND_BYTES)),
        );
        if let Some(size) = truncate_size {
            let result = save.writer.runtime.block_on(draft.truncate(size));
            if result.is_err() {
                draft_state.take_unresolved(handle);
                reply.error(draft_errno(&result));
                return;
            }
        }
        let baseline = Arc::new(Mutex::new(DocumentBaseline {
            schema_identity,
            row: save.recorded_baseline(),
        }));
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Document(save, draft, baseline));
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .track_open(inode.0);
        if write_mode && truncate_size.is_some() {
            draft_state.pending_truncates.remove(&inode.0);
        }
        if write_mode {
            draft_state.set_writer_owner(handle, request.pid());
        }
        reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
    }
    #[allow(clippy::too_many_lines)]
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
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
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
            let Ok(Some(destination)) = state.lookup_child(parent.0, OsStr::new("data.orna"))
            else {
                reply.error(Errno::EIO);
                return;
            };
            let save = if let Some(NodeKind::Document { save, .. }) =
                state.nodes.get(&destination).map(|node| &node.kind)
            {
                Arc::clone(save)
            } else {
                reply.error(Errno::EPERM);
                return;
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
            let Ok(schema_identity) = save.live_schema_identity() else {
                reply.error(Errno::EIO);
                return;
            };
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
                if matches!(
                    state.lookup_child(parent.0, std::ffi::OsStr::new(name)),
                    Ok(Some(_))
                ) {
                    reply.error(Errno::EEXIST);
                    return;
                }
                if !save.writer.scratch_store.reserve_unbound(&identity) {
                    reply.error(Errno::EDQUOT);
                    return;
                }
            }
            let Ok(inode) = state.add_file(
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
                    schema_identity,
                },
            ) else {
                save.writer.scratch_store.release_unbound(&identity);
                reply.error(Errno::EEXIST);
                return;
            };
            let Some(attr) = state.attr(inode) else {
                reply.error(Errno::EIO);
                return;
            };
            (attr, save, target, scratch, blob_field)
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        draft_state.set_writer_owner(handle, request.pid());
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
    #[allow(clippy::too_many_lines)]
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
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        if self.is_candidate_blob(parent.0, name) {
            match self.rename_blob_suffix(parent.0, name, newname) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(errno),
            }
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
            schema_identity,
        ) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Ok(Some(source_inode)) = state.lookup_child(parent.0, name) else {
                reply.error(Errno::ENOENT);
                return;
            };
            let Some(NodeKind::Temporary {
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity,
                schema_identity,
                ..
            }) = state.nodes.get(&source_inode).map(|node| &node.kind)
            else {
                reply.error(refused_mutation_errno());
                return;
            };
            let (save, target, scratch, blob_field, baseline, identity, schema_identity) = (
                Arc::clone(save),
                Arc::clone(target),
                Arc::clone(scratch),
                blob_field.clone(),
                baseline.clone(),
                Arc::clone(identity),
                schema_identity.clone(),
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
            let Ok(Some(destination_inode)) = state.lookup_child(newparent.0, newname) else {
                reply.error(Errno::ENOENT);
                return;
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
                (
                    Some(_),
                    Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_) | NodeKind::Text { .. }),
                ) => {}
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
                schema_identity,
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
            Some(field) => save
                .commit_blob_temporary(&target, &scratch, field, baseline, schema_identity, true)
                .map(|accepted| (accepted.row, accepted.bytes)),
            None => save
                .commit_temporary(&scratch, baseline, schema_identity, true)
                .map(|bytes| (None, bytes)),
        };
        let (accepted_row, accepted_bytes) = match committed {
            Ok(accepted) => accepted,
            Err(error) => {
                eprintln!("mounted replacement failed: {error}");
                let commit_was_stale = matches!(error, DocumentCommitError::Stale);
                let path = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .display_path(source_inode);
                // A refused candidate is retained before its rename replies (ORNA-VFS-009).
                // When recovery storage cannot keep it, the rename replies that retention
                // errno (EDQUOT when full) and the scratch stays in place.
                let error = retain_refused_scratch(
                    &mut draft_state,
                    &save,
                    &scratch,
                    source_inode,
                    &path,
                    error,
                );
                if commit_was_stale {
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
        };
        // The accepted replacement supersedes the bytes a path truncate cut from the
        // destination, so that truncate is not retained against the replaced file (ORNA-VFS-024).
        // Cleared before the post-commit install steps, which can still refuse the reply with EIO:
        // the accepted row is durable, so its truncate must not outlive it.
        draft_state.pending_truncates.remove(&destination_inode);
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
                if let Some(row) = accepted_row
                    && let Ok(Some(document)) =
                        state.lookup_child(newparent.0, OsStr::new("data.orna"))
                    && let Some(NodeKind::Document { bytes, .. }) =
                        state.nodes.get_mut(&document).map(|node| &mut node.kind)
                {
                    *bytes = row;
                }
            }
            None if state.subtree_pinned(destination_inode) => {
                // 41-vfs.md:38-40: a handle pinned to the replaced row keeps the generation it
                // captured; the accepted bytes go to a new node that new opens show
                // (profiles/vfs-1.md:126-133), the same policy adopt_accepted_document applies.
                let document_bytes = match candidate_document_bytes(&accepted_bytes) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!("mounted replacement could not be read: {error}");
                        reply.error(Errno::EIO);
                        return;
                    }
                };
                let new_inode = match state.replace_child(
                    newparent.0,
                    newname,
                    NodeKind::Document {
                        bytes: document_bytes,
                        save: Arc::clone(&save),
                    },
                ) {
                    Ok(inode) => inode,
                    Err(error) => {
                        eprintln!("mounted replacement install failed: {error}");
                        reply.error(Errno::EIO);
                        return;
                    }
                };
                // The `.txt` sibling of a Str over 64 KiB follows the accepted row, as
                // accept_document and adopt_accepted_document project it (profiles/vfs-1.md:76-80).
                if !state.is_superseded(new_inode)
                    && let Err(error) = state.project_accepted_document(new_inode, &save)
                {
                    eprintln!("the accepted row document could not be re-projected: {error}");
                }
            }
            None => {
                let document_bytes = match candidate_document_bytes(&accepted_bytes) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!("mounted replacement could not be read: {error}");
                        reply.error(Errno::EIO);
                        return;
                    }
                };
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
                *bytes = document_bytes;
                // New opens continue from the row this scratch's context accepted.
                if !Arc::ptr_eq(destination_save, &save) {
                    *destination_save = Arc::clone(&save);
                }
                // The `.txt` sibling of a Str over 64 KiB follows the accepted row.
                if !state.is_superseded(destination_inode)
                    && let Err(error) = state.project_accepted_document(destination_inode, &save)
                {
                    eprintln!("the accepted row document could not be re-projected: {error}");
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
        draft_state.clear_unresolved_inode(source_inode);
        draft_state.retained_scratch.remove(&source_inode);
        draft_state.rejected_scratch.remove(&source_inode);
        // The accepted replacement supersedes the refused candidate it replaces, so
        // the refusing handle no longer holds the destination unresolved (ORNA-VFS-024).
        if let Some(DraftRefusal {
            handle: Some(refused),
            ..
        }) = draft_state.rejected_documents.remove(&destination_inode)
        {
            draft_state.take_unresolved(refused);
        }
        draft_state.pending_truncates.remove(&source_inode);
        state.nodes.remove(&source_inode);
        reply.ok();
    }
    #[allow(clippy::too_many_lines)]
    fn unlink(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        if self.is_candidate_blob(parent.0, OsStr::new(name)) {
            match self.unlink_blob(parent.0, OsStr::new(name)) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(errno),
            }
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
        if draft_state.unresolved_inodes().any(|open| open == inode) {
            reply.error(Errno::EBUSY);
            return;
        }
        let (save, identity, scratch) = if let Some(NodeKind::Temporary {
            save,
            identity,
            scratch,
            ..
        }) = state.nodes.get(&inode).map(|node| &node.kind)
        {
            (Arc::clone(save), Arc::clone(identity), Arc::clone(scratch))
        } else {
            reply.error(refused_mutation_errno());
            return;
        };
        if let Some(cause) = draft_state
            .rejected_scratch
            .get(&inode)
            .map(|refusal| refusal.cause)
        {
            // A refused replacement stays recoverable (ORNA-VFS-009): its
            // candidate moves to recovery storage before its name goes away. A
            // candidate recovery storage cannot keep fails with its retention
            // errno (EDQUOT when full) and the scratch stays in place.
            let path = state.display_path(inode);
            let len = save.writer.runtime.block_on(scratch.candidate_len());
            if let Err(error) =
                retain_scratch_candidate(&save, &path, cause, len, &mut |offset, length| {
                    save.writer
                        .runtime
                        .block_on(scratch.read_range(offset, length))
                        .map_err(|read| RetentionError::from(candidate_unreadable(read)))
                })
            {
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
    #[allow(clippy::too_many_lines)]
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
        if let Some(OpenFile::Source(source_inode)) = &open_file {
            reply.data(&self.source_read(*source_inode, offset, size));
            return;
        }
        let private_read = match open_file {
            Some(OpenFile::Document(save, draft, _)) => Some(read_candidate_range(
                |offset, length| {
                    save.writer
                        .runtime
                        .block_on(draft.read_range(offset, length))
                },
                offset,
                reply_read_size(size),
            )),
            Some(OpenFile::Temporary(save, _, scratch, _)) => Some(read_candidate_range(
                |offset, length| {
                    save.writer
                        .runtime
                        .block_on(scratch.read_range(offset, length))
                },
                offset,
                reply_read_size(size),
            )),
            Some(OpenFile::Source(_)) | None => None,
        };
        if let Some(bytes) = private_read {
            match bytes {
                Ok(bytes) => reply.data(&bytes),
                Err(error) => {
                    eprintln!(
                        "mounted private candidate read failed: {}",
                        candidate_unreadable(error)
                    );
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
                |offset, length| {
                    save.writer
                        .runtime
                        .block_on(scratch.read_range(offset, length))
                },
                offset,
                reply_read_size(size),
            ) {
                Ok(bytes) => reply.data(&bytes),
                Err(error) => {
                    eprintln!(
                        "mounted temporary candidate read failed: {}",
                        candidate_unreadable(error)
                    );
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
                NodeKind::BlobBytes(candidate) => {
                    match candidate.read_range(offset, size as usize) {
                        Ok(data) => reply.data(&data),
                        Err(_) => reply.error(Errno::EIO),
                    }
                    return;
                }
                NodeKind::Text { bytes, .. } => {
                    match bytes.read_range(offset, size as usize) {
                        Ok(data) => reply.data(&data),
                        Err(_) => reply.error(Errno::EIO),
                    }
                    return;
                }
                NodeKind::Blob {
                    graph, reference, ..
                } => Some((
                    Arc::clone(graph),
                    reference.clone(),
                    reference.content_identity().length(),
                )),
                // Temporary nodes are served from their scratch before this lookup.
                NodeKind::Temporary { .. } => None,
                NodeKind::Directory(_)
                | NodeKind::Table(_)
                | NodeKind::KeyDirectory(_)
                | NodeKind::Row(_)
                | NodeKind::NestedDirectory(_) => {
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
        let Ok(scope) = graph.open_read_scope() else {
            reply.error(Errno::EIO);
            return;
        };
        match read_blob_window(&graph, &reference, offset, size, &scope) {
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
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        if let Some(source_inode) = self.source_handle_inode(handle.0) {
            match self.source_write(&mut draft_state, handle.0, source_inode, offset, data) {
                Ok(()) => reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX)),
                Err(errno) => reply.error(errno),
            }
            return;
        }
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
        let had_unresolved_handle = draft_state.has_unresolved(handle.0);
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
            draft_state.set_unresolved(handle.0, inode.0);
            draft_state.candidate_written(handle.0, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        let (result, candidate_len) = match open_file {
            Some(OpenFile::Document(save, draft, _)) => (
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
            Some(OpenFile::Source(_)) | None => {
                if !had_unresolved_handle {
                    draft_state.take_unresolved(handle.0);
                }
                reply.error(Errno::EBADF);
                return;
            }
        };
        if result.is_err() {
            if !had_unresolved_handle {
                draft_state.take_unresolved(handle.0);
            }
            if is_scratch && !had_retained_scratch {
                draft_state.retained_scratch.remove(&inode.0);
            }
            reply.error(draft_errno(&result));
        } else {
            if writes_candidate {
                // Dirty means an accepted-or-refused candidate is pending. A failed
                // write leaves no candidate, so it must not mark the handle dirty.
                draft_state.set_unaccepted_write(handle.0);
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
        if let Some(trigger) = self.source_handle_inode(handle.0) {
            match self.source_sync(trigger) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(errno),
            }
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        match self.commit_open_handle(&mut draft_state, inode, handle.0) {
            Ok(()) => {
                // A successful fsync makes this handle's bytes durable; nothing is
                // unresolved for it, so unlink must not wait on its RELEASE.
                draft_state.take_unresolved(handle.0);
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
        if !draft_state.has_unresolved(handle.0) {
            reply.ok();
            return;
        }
        // The kernel issues FLUSH when an inherited copy of this descriptor is
        // closed by another process (exec(2) closes CLOEXEC fds through filp_close).
        // While quiescing, a FLUSH on a dirty handle fails with EBUSY from any
        // process, so the close reports that the draft was not accepted.
        if let Err(errno) = draft_state.ensure_serving() {
            reply.error(errno);
            return;
        }
        // PR policy (not EDIT-1 step 5 text): a FLUSH from a process other than the
        // one that opened the handle acknowledges without accepting the draft; the
        // opener's own close is the close-time commit. The opener pid is recorded at
        // open, so a later writer cannot move the gate to itself.
        if draft_state
            .writer_owner(handle.0)
            .is_some_and(|writer| writer != request.pid())
        {
            reply.ok();
            return;
        }
        if let Some(trigger) = self.source_handle_inode(handle.0) {
            drop(draft_state);
            match self.source_sync(trigger) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(errno),
            }
            return;
        }
        match self.commit_open_handle(&mut draft_state, inode, handle.0) {
            Ok(()) => {
                draft_state.take_unresolved(handle.0);
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
                .track_release(inode.0);
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
                .take_blob(handle.0)
                .map_or(inode.0, |blob| blob.generation);
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .track_release(generation);
        }
        draft_state.clear_unaccepted_write(handle.0);
        draft_state.clear_writer_owner(handle.0);
        if let Some(OpenFile::Source(source_inode)) = &released {
            self.source_release(&mut draft_state, *source_inode);
        }
        let released_document = matches!(released, Some(OpenFile::Document(..)));
        match (draft_state.take_unresolved(handle.0), released) {
            (Some(inode), Some(OpenFile::Temporary(_, _, _, _))) => {
                draft_state.retained_scratch.insert(inode);
            }
            // Release is cleanup only (EDIT-1 step 4): an unsynced document is
            // retained as a draft, never accepted here.
            (Some(inode), Some(OpenFile::Document(save, draft, _))) => {
                self.retain_unsynced_document(&mut draft_state, inode, handle.0, &save, &draft);
            }
            _ => {}
        }
        if released_document {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .track_release(inode.0);
        }
        reply.ok();
    }

    #[allow(clippy::too_many_lines)]
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
            if let Err(errno) = draft_state.ensure_serving() {
                reply.error(errno);
                return;
            }
            let source = handle.and_then(|handle| {
                self.source_handle_inode(handle.0)
                    .map(|source_inode| (handle.0, source_inode))
            });
            if let Some((handle, source_inode)) = source {
                if let Err(errno) =
                    self.source_truncate(&mut draft_state, handle, source_inode, size)
                {
                    reply.error(errno);
                    return;
                }
            } else if let Some(handle) = handle {
                let open_file = self
                    .open_files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&handle.0)
                    .cloned();
                let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
                let had_unresolved_handle = draft_state.has_unresolved(handle.0);
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
                draft_state.set_writer_owner_if_absent(handle.0, request.pid());
                if is_tracked {
                    draft_state.set_unresolved(handle.0, inode.0);
                    if is_scratch {
                        draft_state.retained_scratch.insert(inode.0);
                    }
                }
                let (result, candidate_len) = match open_file {
                    OpenFile::Document(save, draft, _) => {
                        (save.writer.runtime.block_on(draft.truncate(size)), None)
                    }
                    OpenFile::Temporary(save, _, scratch, _) => {
                        let result = save.writer.runtime.block_on(scratch.truncate(size));
                        let len = result
                            .is_ok()
                            .then(|| save.writer.runtime.block_on(scratch.candidate_len()));
                        (result, len)
                    }
                    OpenFile::Source(_) => {
                        reply.error(Errno::EBADF);
                        return;
                    }
                };
                if result.is_err() {
                    if !had_unresolved_handle {
                        draft_state.take_unresolved(handle.0);
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
        match self.attr_of(inode.0) {
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
        let parent = if let Some(node) = state.nodes.get(&inode.0) {
            node.parent
        } else {
            reply.error(Errno::ENOENT);
            return;
        };
        if state.directory(inode.0).is_none() {
            reply.error(Errno::ENOTDIR);
            return;
        }
        if offset == 0 && reply.add(INodeNo(inode.0), 1, FileType::Directory, OsStr::new(".")) {
            reply.ok();
            return;
        }
        if offset <= 1 && reply.add(INodeNo(parent), 2, FileType::Directory, OsStr::new("..")) {
            reply.ok();
            return;
        }
        if state
            .readdir_page(inode.0, offset, |entry_offset, entry| {
                !reply.add(
                    INodeNo(entry.inode),
                    entry_offset,
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

    fn opendir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _flags: fuser::OpenFlags,
        reply: ReplyOpen,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.nodes.contains_key(&inode.0) {
            reply.error(Errno::ENOENT);
            return;
        }
        if state.directory(inode.0).is_none() {
            reply.error(Errno::ENOTDIR);
            return;
        }
        // A held listing keeps a retired directory loaded until it is released (41-vfs.md:39-40).
        state.track_open(inode.0);
        reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
    }

    fn releasedir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _handle: fuser::FileHandle,
        _flags: fuser::OpenFlags,
        reply: ReplyEmpty,
    ) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .track_release(inode.0);
        reply.ok();
    }
}
