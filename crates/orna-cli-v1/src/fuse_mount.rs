use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, ReplyAttr,
    ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite,
    RenameFlags, Request,
};
use orna_project_v1::{
    ObjectIdentityManifest, ObjectKind, ProjectLoader, ResolvedTableSchema,
    admit_source_row_with_owner_fields,
};
use orna_repository_v1::{
    AdmittedBlobReference, CommittedTableBinding, GitCommitRef, GitHashAlgorithm,
    HistoricalFormatContext, KeyRange, NativeGraphContext, NativeOid, Repository, RowMapSnapshot,
    RuntimeGeneration, TypedKey,
};
use orna_runtime_v1::{
    NoFault, RuntimeState, RuntimeTableIdentity, TableObjectId,
    vfs::{
        EditDraft, ManagedFile, SnapshotPin, TemporarySave, VfsDurableSave, VfsFileSnapshot,
        VfsRepositoryCache, VfsRowProjection, VfsSaveCandidate, VfsSaveCoordinates,
        VfsStoredField, VfsTableRowReplacement, TemporaryRenameOutcome, project_component,
    },
};
use orna_value_v1::{Format3Context, Raw, SchemaDescriptor, decode_canonical_storage_value};
use orna_conformance_v1::transactional_loaded_project_candidate_validator;
use orna_evaluator_v1::Limits as EvaluatorLimits;
struct MountWriter {
    runtime: tokio::runtime::Runtime,
    state: RuntimeState,
    repository: Repository,
    project: Arc<orna_project_v1::LoadedProject>,
    cache: Arc<VfsRepositoryCache>,
    owner_id: [u8; 16],
}

impl MountWriter {
    fn open(repository: &Repository, commit: &GitCommitRef) -> Result<Arc<Self>, String> {
        let (requested_identity, initial_digest) = super::runtime_identity(repository)
            .map_err(|error| format!("repository runtime identity is unavailable: {error}"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("mounted editor runtime could not be created: {error}"))?;
        let state = runtime
            .block_on(RuntimeState::open(repository, requested_identity, initial_digest))
            .map_err(|error| format!("mounted editor runtime could not be opened: {error:?}"))?;
        let identity = runtime
            .block_on(state.identity())
            .map_err(|error| format!("mounted editor runtime identity is unavailable: {error:?}"))?;
        let owner_id = super::invocation_owner_id(identity);
        let project = ProjectLoader::default()
            .load_committed_snapshot(repository, commit)
            .map_err(|error| format!("committed source project could not be loaded: {error:?}"))?;
        Ok(Arc::new(Self {
            runtime,
            state,
            repository: repository.clone(),
            project: Arc::new(project),
            cache: Arc::new(VfsRepositoryCache::new()),
            owner_id,
        }))
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
            .map_err(|error| format!("mounted editor repository generation is unavailable: {error:?}"))
    }

    fn row_baseline(
        &self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, String> {
        let snapshot = self
            .runtime
            .block_on(self.state.begin_admitted_table_activation(std::slice::from_ref(table)))
            .map_err(|error| format!("mounted editor row admission failed: {error:?}"))?;
        snapshot
            .query_exact(table.table(), key)
            .map(|row| row.map(<[u8]>::to_vec))
            .map_err(|error| format!("mounted editor row baseline is unavailable: {error:?}"))
    }

    fn candidate(
        &self,
        row: &RowSaveContext,
        coordinates: &VfsSaveCoordinates,
        bytes: &[u8],
    ) -> Result<(VfsSaveCandidate, Vec<u8>), orna_runtime_v1::TableActivationError> {
        let source = std::str::from_utf8(bytes)
            .map_err(|_| orna_runtime_v1::TableActivationError::Runtime(orna_runtime_v1::RuntimeError::InvalidTableMutation))?;
        let mut mutation_id = [0_u8; 16];
        getrandom::fill(&mut mutation_id).map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(orna_runtime_v1::RuntimeError::InvalidTableMutation)
        })?;
        let admitted = admit_source_row_with_owner_fields(
            &self.project,
            &row.logical_path,
            source,
            &row.source.schema,
            &row.source.value_context,
            Vec::new(),
            Vec::new(),
            mutation_id,
        )
        .map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(orna_runtime_v1::RuntimeError::InvalidTableMutation)
        })?;
        let (mut mutation, bindings, resolved_schema) = admitted.into_mutation_and_bindings();
        if resolved_schema.table_object_id() != row.source.schema.table_object_id()
            || mutation.key() != coordinates.key()
            || mutation.table() != coordinates.table().table()
            || mutation.table_object_id() != Some(coordinates.table().object_id())
        {
            return Err(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ));
        }
        let mut pins = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let (_, _, pin) = binding.into_parts();
            mutation = mutation
                .with_protected_content_pin(&pin)
                .map_err(orna_runtime_v1::TableActivationError::Runtime)?;
            pins.push(pin);
        }
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
            coordinates.cwd_generation().clone(),
            EvaluatorLimits::default(),
        )
        .map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(orna_runtime_v1::RuntimeError::InvalidTableMutation)
        })?;
        Ok((VfsSaveCandidate::new(replacement, validator), replacement_bytes))
    }
}

struct RowSaveContext {
    writer: Arc<MountWriter>,
    source: Arc<TableSource>,
    key: TypedKey,
    target: Arc<ManagedFile<RowMapSnapshot>>,
    table: RuntimeTableIdentity,
    logical_path: String,
    expected_baseline_row: Mutex<Option<Vec<u8>>>,
}

fn row_save_context(
    writer: &Arc<MountWriter>,
    source: &Arc<TableSource>,
    key: &TypedKey,
    document_bytes: &[u8],
) -> Result<Arc<RowSaveContext>, String> {
    let declaration = source
        .binding
        .declaration_name()
        .ok_or_else(|| "committed table has no source declaration name".to_owned())?;
    let table = RuntimeTableIdentity::new(
        declaration,
        TableObjectId::new(*source.binding.relation_id()),
    )
    .map_err(|error| format!("committed table identity is invalid: {error:?}"))?;
    let key_bytes = key
        .canonical_bytes()
        .map_err(|error| format!("committed row key cannot be encoded: {error:?}"))?;
    let expected_baseline_row = writer.row_baseline(&table, &key_bytes)?;
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
        SnapshotPin::capture(Arc::clone(&source.rows)),
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
        expected_baseline_row: Mutex::new(expected_baseline_row),
    }))
}
impl RowSaveContext {
    fn durable_save(&self) -> Result<VfsDurableSave, String> {
        let cwd_generation = self.writer.cwd_generation()?;
        let key = self
            .key
            .canonical_bytes()
            .map_err(|error| format!("committed row key cannot be encoded: {error:?}"))?;
        let expected_baseline_row = self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(VfsDurableSave::new(
            self.writer.owner_id,
            cwd_generation,
            self.table.clone(),
            key,
        )
        .with_expected_baseline_row(expected_baseline_row))
    }

    fn record_accepted_row(&self, row: Option<Vec<u8>>) {
        if let Some(row) = row {
            *self
                .expected_baseline_row
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row);
        }
    }

    fn commit_document(self: &Arc<Self>, draft: &EditDraft<RowMapSnapshot>) -> Result<(), String> {
        let save = self.durable_save()?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self.writer.runtime.block_on(self.target.commit_durable_document_save(
            draft,
            &self.writer.state,
            save,
            move |coordinates, bytes| async move {
                let (candidate, row_bytes) = row_context
                    .writer
                    .candidate(&row_context, &coordinates, &bytes)?;
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
                self.record_accepted_row(accepted_row);
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => {
                Err("mounted row document was rejected by semantic admission".into())
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => {
                Err("mounted row document is stale against its captured baseline".into())
            }
            Err(error) => Err(format!("mounted row save failed: {error:?}")),
        }
    }

    fn fsync_temporary(&self, scratch: &TemporarySave<RowMapSnapshot>) -> Result<(), String> {
        self.writer
            .runtime
            .block_on(scratch.fsync_scratch_with(|_, _| async { Ok::<(), String>(()) }))
            .map(|_| ())
    }

    fn commit_temporary(
        self: &Arc<Self>,
        scratch: &TemporarySave<RowMapSnapshot>,
    ) -> Result<(), String> {
        let save = self.durable_save()?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self.writer.runtime.block_on(self.target.commit_durable_temporary_rename(
            scratch,
            &self.writer.state,
            save,
            move |coordinates, bytes| async move {
                let (candidate, row_bytes) = row_context
                    .writer
                    .candidate(&row_context, &coordinates, &bytes)?;
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
                self.record_accepted_row(accepted_row);
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => {
                Err("temporary mounted row document was rejected by semantic admission".into())
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => {
                Err("temporary mounted row document is stale against its captured baseline".into())
            }
            Err(error) => Err(format!("temporary mounted row save failed: {error:?}")),
        }
    }
}
const ROOT_INODE: u64 = 1;
const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
const ENTRY_TTL: Duration = Duration::ZERO;
// Private editor drafts must bypass the shared kernel page cache.
const EDITABLE_OPEN_FLAGS: fuser::FopenFlags = fuser::FopenFlags::FOPEN_DIRECT_IO;
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
pub(super) fn spawn_editor(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
) -> Result<fuser::BackgroundSession, String> {
    let writer = MountWriter::open(repository, commit)?;
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
    Temporary(Arc<RowSaveContext>, Arc<TemporarySave<RowMapSnapshot>>),
}

struct WorkspaceFilesystem {
    state: Mutex<FsBuilder>,
    open_files: Mutex<HashMap<u64, OpenFile>>,
    pending_truncates: Mutex<HashMap<u64, u64>>,
    next_handle: AtomicU64,
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
        builder.writer = writer;

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
            let graph =
                Arc::new(context.open_native_graph(&rows).map_err(|error| {
                    format!("committed native graph could not be opened: {error}")
                })?);
            let value_context = context
                .open_value_context(relation_id)
                .map_err(|error| format!("committed row value context could not be opened: {error:?}"))?;
            builder.add_table_rows(table_directory, graph, Arc::new(rows), &schema, value_context, &field_names)?;
        }

        Ok(Self {
            state: Mutex::new(builder),
            open_files: Mutex::new(HashMap::new()),
            pending_truncates: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        })
    }
    fn update_writable_bytes(&self, inode: u64, bytes: &[u8]) -> bool {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(node) = state.nodes.get_mut(&inode) else {
            return false;
        };
        let current = match &mut node.kind {
            NodeKind::Document { bytes, .. } | NodeKind::Temporary { bytes, .. } => bytes,
            _ => return false,
        };
        current.clear();
        current.extend_from_slice(bytes);
        true
    }
}

impl Filesystem for WorkspaceFilesystem {
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
        let candidate_size = handle.and_then(|handle| {
            let open_file = self
                .open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&handle.0)
                .cloned()?;
            let bytes = match open_file {
                OpenFile::Document(save, draft) => {
                    save.writer.runtime.block_on(draft.candidate_bytes())
                }
                OpenFile::Temporary(save, scratch) => {
                    save.writer.runtime.block_on(scratch.candidate_bytes())
                }
            };
            Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        });
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

    fn open(&self, _request: &Request, inode: INodeNo, _flags: fuser::OpenFlags, reply: ReplyOpen) {
        let (save, scratch) = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode.0).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => (Arc::clone(save), None),
                Some(NodeKind::Temporary { save, scratch, .. }) => {
                    (Arc::clone(save), Some(Arc::clone(scratch)))
                }
                Some(NodeKind::Bytes(_) | NodeKind::Blob { .. }) => {
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
        if let Some(scratch) = scratch {
            self.open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(handle, OpenFile::Temporary(save, scratch));
            reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
            return;
        }
        let draft = Arc::new(save.writer.runtime.block_on(save.target.open_draft(usize::MAX)));
        if let Some(size) = self
            .pending_truncates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&inode.0)
        {
            if save.writer.runtime.block_on(draft.truncate(size)).is_err() {
                reply.error(Errno::EIO);
                return;
            }
        }
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Document(save, draft));
        reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
    }
    fn create(
        &self,
        _request: &Request,
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
        let (attr, save, scratch) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(state.nodes.get(&parent.0).map(|node| &node.kind), Some(NodeKind::Row(_))) {
                reply.error(Errno::EROFS);
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
                    reply.error(Errno::EROFS);
                    return;
                }
            };
            let scratch = Arc::new(save.writer.runtime.block_on(
                save.target.begin_temporary_replacement(usize::MAX),
            ));
            let inode = match state.add_file(
                parent.0,
                name,
                NodeKind::Temporary { bytes: Vec::new(), save: Arc::clone(&save), scratch: Arc::clone(&scratch) },
            ) {
                Ok(inode) => inode,
                Err(_) => {
                    reply.error(Errno::EEXIST);
                    return;
                }
            };
            let Some(attr) = state.attr(inode) else {
                reply.error(Errno::EIO);
                return;
            };
            (attr, save, scratch)
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Temporary(save, scratch));
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
        if parent != newparent || newname != OsStr::new("data.orna") || flags != RenameFlags::empty() {
            reply.error(Errno::EROFS);
            return;
        }
        let (source_inode, destination_inode, save, scratch) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_inode = match state.lookup_child(parent.0, name) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            let destination_inode = match state.lookup_child(newparent.0, newname) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            let (save, scratch) = match state.nodes.get(&source_inode).map(|node| &node.kind) {
                Some(NodeKind::Temporary { save, scratch, .. }) => {
                    (Arc::clone(save), Arc::clone(scratch))
                }
                _ => {
                    reply.error(Errno::EROFS);
                    return;
                }
            };
            let destination_save = match state.nodes.get(&destination_inode).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => save,
                _ => {
                    reply.error(Errno::EROFS);
                    return;
                }
            };
            if !Arc::ptr_eq(&save, destination_save) {
                reply.error(Errno::EXDEV);
                return;
            }
            (source_inode, destination_inode, save, scratch)
        };
        if save.commit_temporary(&scratch).is_err() {
            reply.error(Errno::EIO);
            return;
        }
        let accepted_bytes = save.writer.runtime.block_on(scratch.candidate_bytes());
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(NodeKind::Document { bytes, .. }) = state
            .nodes
            .get_mut(&destination_inode)
            .map(|node| &mut node.kind)
        {
            *bytes = accepted_bytes.to_vec();
        } else {
            reply.error(Errno::EIO);
            return;
        }
        let removed = state
            .nodes
            .get_mut(&parent.0)
            .and_then(|node| node.kind.directory_mut())
            .and_then(|directory| directory.remove_entry(name));
        if removed.is_none() {
            reply.error(Errno::EIO);
            return;
        }
        state.nodes.remove(&source_inode);
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
        let private_bytes = match open_file {
            Some(OpenFile::Document(save, draft)) => {
                Some(save.writer.runtime.block_on(draft.candidate_bytes()))
            }
            Some(OpenFile::Temporary(save, scratch)) => {
                Some(save.writer.runtime.block_on(scratch.candidate_bytes()))
            }
            None => None,
        };
        if let Some(bytes) = private_bytes {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(size as usize).min(bytes.len());
            reply.data(bytes.get(start..end).unwrap_or(&[]));
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
                NodeKind::Bytes(bytes) | NodeKind::Document { bytes, .. } | NodeKind::Temporary { bytes, .. } => {
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
                NodeKind::Blob { graph, reference } => Some((
                    Arc::clone(graph),
                    reference.clone(),
                    reference.content_identity().length(),
                )),
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
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let (result, candidate_bytes) = match open_file {
            Some(OpenFile::Document(save, draft)) => {
                (save.writer.runtime.block_on(draft.write_at(offset, data)), None)
            }
            Some(OpenFile::Temporary(save, scratch)) => {
                let result = save.writer.runtime.block_on(scratch.write_at(offset, data));
                let bytes = result
                    .is_ok()
                    .then(|| save.writer.runtime.block_on(scratch.candidate_bytes()));
                (result, bytes)
            }
            None => {
                reply.error(Errno::EBADF);
                return;
            }
        };
        if result.is_err() {
            reply.error(Errno::EIO);
        } else {
            if let Some(bytes) = candidate_bytes {
                self.update_writable_bytes(inode.0, &bytes);
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
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let result = match open_file {
            Some(OpenFile::Document(save, draft)) => {
                let result = save.commit_document(&draft);
                if result.is_ok() {
                    let bytes = save.writer.runtime.block_on(draft.candidate_bytes());
                    if let Some(NodeKind::Document { bytes: current, .. }) = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .nodes
                        .get_mut(&inode.0)
                        .map(|node| &mut node.kind)
                    {
                        *current = bytes.to_vec();
                    }
                }
                result
            }
            Some(OpenFile::Temporary(save, scratch)) => save.fsync_temporary(&scratch),
            None => Err("file handle is not open".to_owned()),
        };
        if result.is_ok() {
            reply.ok();
        } else {
            reply.error(Errno::EIO);
        }
    }

    fn flush(
        &self,
        _request: &Request,
        _inode: INodeNo,
        _handle: fuser::FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn release(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: fuser::FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&handle.0);
        reply.ok();
    }

    fn setattr(
        &self,
        _request: &Request,
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
        if mode.is_some() || uid.is_some() || gid.is_some() || flags.is_some() {
            reply.error(Errno::EIO);
            return;
        }
        if let Some(size) = size {
            if let Some(handle) = handle {
                let open_file = self
                    .open_files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&handle.0)
                    .cloned();
                let (result, candidate_bytes) = match open_file {
                    Some(OpenFile::Document(save, draft)) => {
                        (save.writer.runtime.block_on(draft.truncate(size)), None)
                    }
                    Some(OpenFile::Temporary(save, scratch)) => {
                        let result = save.writer.runtime.block_on(scratch.truncate(size));
                        let bytes = result
                            .is_ok()
                            .then(|| save.writer.runtime.block_on(scratch.candidate_bytes()));
                        (result, bytes)
                    }
                    None => {
                        reply.error(Errno::EBADF);
                        return;
                    }
                };
                if result.is_err() {
                    reply.error(Errno::EIO);
                    return;
                }
                if let Some(bytes) = candidate_bytes {
                    self.update_writable_bytes(inode.0, &bytes);
                }
            } else {
                let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !matches!(state.nodes.get(&inode.0).map(|node| &node.kind), Some(NodeKind::Document { .. })) {
                    reply.error(Errno::EROFS);
                    return;
                }
                self.pending_truncates
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(inode.0, size);
            }
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        if state.ensure_readdir(inode.0, start).is_err() {
            reply.error(Errno::EIO);
            return;
        }
        let Some(directory) = state.directory(inode.0) else {
            if state.nodes.contains_key(&inode.0) {
                reply.error(Errno::ENOTDIR);
            } else {
                reply.error(Errno::ENOENT);
            }
            return;
        };
        let Some(node) = state.nodes.get(&inode.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if start == 0 && reply.add(INodeNo(inode.0), 1, FileType::Directory, OsStr::new(".")) {
            reply.ok();
            return;
        }
        if start <= 1
            && reply.add(
                INodeNo(node.parent),
                2,
                FileType::Directory,
                OsStr::new(".."),
            )
        {
            reply.ok();
            return;
        }
        for (index, entry) in directory
            .entries
            .iter()
            .enumerate()
            .skip(start.saturating_sub(2))
        {
            if reply.add(
                INodeNo(entry.inode),
                u64::try_from(index.saturating_add(3)).unwrap_or(u64::MAX),
                entry.kind,
                entry.name.as_os_str(),
            ) {
                break;
            }
        }
        reply.ok();
    }
}

struct FsBuilder {
    nodes: HashMap<u64, FsNode>,
    next_inode: u64,
    writer: Option<Arc<MountWriter>>,
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
        }
    }

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
            NodeKind::Document { bytes, .. } => (FileType::RegularFile, bytes.len() as u64, 0o644, 1),
            NodeKind::Temporary { bytes, .. } => (FileType::RegularFile, bytes.len() as u64, 0o644, 1),
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
    ) -> Result<(), String> {
        let source = Arc::new(TableSource {
            graph,
            rows,
            binding: schema.binding().clone(),
            schema: schema.clone(),
            value_context,
            field_names: field_names.clone(),
            field_types: stored_field_types(schema.descriptor())?,
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
        let canonical_fields = decode_canonical_storage_value(decoded.canonical_fields())
            .map_err(|error| format!("committed row values could not be decoded: {error:?}"))?;
        let Raw::Array(values) = canonical_fields else {
            return Err("committed row values are not a canonical field array".into());
        };
        if values.len() != decoded.fields().len() {
            return Err("committed row value and field counts differ".into());
        }

        let mut projected_fields = Vec::with_capacity(values.len());
        let mut blob_files = Vec::new();
        for (field, value) in decoded.fields().iter().zip(values) {
            let field_name = source
                .field_names
                .get(field.field_id())
                .ok_or_else(|| "committed field has no source identity".to_owned())?;
            let field_type = source
                .field_types
                .get(field.field_id())
                .ok_or_else(|| "committed field has no schema type".to_owned())?;
            if let Some(metadata) = field.blob_metadata() {
                let value_oid = metadata
                    .descriptor_oid()
                    .ok_or_else(|| "committed Blob field has no descriptor identity".to_owned())?;
                let algorithm = match value_oid.len() {
                    20 => GitHashAlgorithm::Sha1,
                    32 => GitHashAlgorithm::Sha256,
                    _ => return Err("committed Blob descriptor has an invalid OID width".into()),
                };
                let descriptor = NativeOid::new(algorithm, value_oid.as_bytes())
                    .map_err(|error| format!("committed Blob descriptor is invalid: {error:?}"))?;
                let stored = source
                    .graph
                    .read_stored_blob_value(&row, &descriptor, &row_scope)
                    .map_err(|error| {
                        format!("committed Blob reference could not be admitted: {error:?}")
                    })?;
                let annotation = stored.annotation();
                let suffix = annotation.suffix().or(metadata.suffix()).map(str::to_owned);
                let file_name = orna_runtime_v1::vfs::project_content_name(
                    field_name,
                    annotation.media_type(),
                    suffix.as_deref(),
                );
                let projected = if is_optional_field(field_type) {
                    VfsStoredField::content_optional(
                        field_name.clone(),
                        annotation.media_type(),
                        suffix,
                        metadata.length(),
                    )
                } else {
                    VfsStoredField::content(
                        field_name.clone(),
                        annotation.media_type(),
                        suffix,
                        metadata.length(),
                    )
                };
                projected_fields.push(projected);
                blob_files.push((file_name, stored.reference().clone()));
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
        let document = match &self.writer {
            Some(writer) => NodeKind::Document {
                save: row_save_context(writer, source, key, &document_bytes)?,
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
            files.push((
                file.name().to_owned(),
                NodeKind::Blob {
                    graph: Arc::clone(&source.graph),
                    reference: reference.clone(),
                },
            ));
        }
        Ok(files)
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
            NodeKind::Bytes(_) | NodeKind::Document { .. } | NodeKind::Temporary { .. } | NodeKind::Blob { .. } => FileType::RegularFile,
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
        bytes: Vec<u8>,
        save: Arc<RowSaveContext>,
        scratch: Arc<TemporarySave<RowMapSnapshot>>,
    },
    Blob {
        graph: Arc<NativeGraphContext>,
        reference: AdmittedBlobReference,
    },
}

impl NodeKind {
    fn directory(&self) -> Option<&Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&table.directory),
            Self::KeyDirectory(directory) => Some(&directory.directory),
            Self::Row(row) => Some(&row.directory),
            Self::Bytes(_) | Self::Document { .. } | Self::Temporary { .. } | Self::Blob { .. } => None,
        }
    }

    fn directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&mut table.directory),
            Self::KeyDirectory(directory) => Some(&mut directory.directory),
            Self::Row(row) => Some(&mut row.directory),
            Self::Bytes(_) | Self::Document { .. } | Self::Temporary { .. } | Self::Blob { .. } => None,
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
    field_types: HashMap<[u8; 16], Raw>,
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

fn stored_field_types(descriptor: &SchemaDescriptor) -> Result<HashMap<[u8; 16], Raw>, String> {
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
    let mut types = HashMap::with_capacity(fields.len());
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
        if types.insert(identity, field_type.clone()).is_some() {
            return Err("committed table schema repeats a stored field".into());
        }
    }
    Ok(types)
}

fn is_optional_field(field_type: &Raw) -> bool {
    matches!(field_type, Raw::Array(parts) if parts.len() == 2 && parts[0] == Raw::Int(2.into()))
}

fn stored_value_is_null(value: &Raw) -> bool {
    matches!(value, Raw::Null)
        || matches!(value, Raw::Tag(60013, option) if matches!(option.as_ref(), Raw::Array(parts) if matches!(parts.as_slice(), [Raw::Int(index)] if index.to_string() == "0")))
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
    };
    Ok(project_component(&canonical_text))
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
    use std::ffi::OsStr;

    use super::{FsBuilder, NodeKind, ROOT_INODE, key_directory};
    use orna_repository_v1::TypedKey;
    use orna_runtime_v1::vfs::project_component;

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
}
