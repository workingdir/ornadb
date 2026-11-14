use super::*;
use orna_sys::SysFailureCode;

#[allow(clippy::wildcard_imports)]
mod attributes;
#[allow(clippy::wildcard_imports)]
mod commit;
#[allow(clippy::wildcard_imports)]
mod create_rename;
#[allow(clippy::wildcard_imports)]
mod dispatch;
#[allow(clippy::wildcard_imports)]
mod draft_state;
#[allow(clippy::wildcard_imports)]
mod file_handles;
#[allow(clippy::wildcard_imports)]
mod lifecycle;
#[allow(clippy::wildcard_imports)]
mod unresolved;

#[allow(clippy::wildcard_imports)]
pub use draft_state::*;

pub(crate) const ROOT_INODE: u64 = 1;
pub(crate) const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
pub(crate) const ENTRY_TTL: Duration = Duration::ZERO;
// Private editor drafts bypass the shared kernel page cache. Writeback cache is
// intentionally not requested: it can mix a later database generation into an
// already-open draft.
pub(crate) const EDITABLE_OPEN_FLAGS: fuser::FopenFlags = fuser::FopenFlags::FOPEN_DIRECT_IO;
pub(crate) const EDITOR_INIT_CAPABILITIES: fuser::InitFlags = fuser::InitFlags::FUSE_ATOMIC_O_TRUNC;
pub fn spawn_read_only(
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
pub fn spawn_editor(
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
pub enum EditError {
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
pub fn run_editor_on_draft(runtime_root: &Path, initial: &[u8]) -> Result<PathBuf, EditError> {
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
