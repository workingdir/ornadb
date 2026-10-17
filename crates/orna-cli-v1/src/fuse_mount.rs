use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, ReplyAttr,
    ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use orna_project_v1::{ObjectIdentityManifest, ObjectKind, ProjectLoader};
use orna_repository_v1::{
    AdmittedBlobReference, CommittedTableBinding, GitCommitRef, GitHashAlgorithm,
    HistoricalFormatContext, KeyRange, NativeGraphContext, NativeOid, Repository, TypedKey,
};
use orna_runtime_v1::{
    TableObjectId,
    vfs::{SnapshotPin, VfsRowProjection, VfsStoredField, project_component},
};
use orna_value_v1::{Raw, SchemaDescriptor, decode_canonical_storage_value};

const ROOT_INODE: u64 = 1;
const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
const ENTRY_TTL: Duration = Duration::ZERO;

pub(super) fn spawn_read_only(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
) -> Result<fuser::BackgroundSession, String> {
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context)?;
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

struct WorkspaceFilesystem {
    state: Mutex<FsBuilder>,
}

impl WorkspaceFilesystem {
    fn from_snapshot(
        repository: &Repository,
        commit: &GitCommitRef,
        context: &HistoricalFormatContext,
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
            builder.add_table_rows(table_directory, graph, schema.binding(), &field_names)?;
        }

        Ok(Self {
            state: Mutex::new(builder),
        })
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
        _handle: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.attr(inode.0) {
            Some(attr) => reply.attr(&ENTRY_TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        _request: &Request,
        inode: INodeNo,
        _handle: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
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
                NodeKind::Bytes(bytes) => {
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
        binding: &CommittedTableBinding,
        field_names: &HashMap<[u8; 16], String>,
    ) -> Result<(), String> {
        let source = Arc::new(TableSource {
            field_types: stored_field_types(binding.descriptor())?,
            graph,
            binding: binding.clone(),
            field_names: field_names.clone(),
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
        source: &TableSource,
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
        files.push((
            "data.orna".to_owned(),
            NodeKind::Bytes(projection.document_text().as_bytes().to_vec()),
        ));
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
            NodeKind::Bytes(_) | NodeKind::Blob { .. } => FileType::RegularFile,
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
            Self::Bytes(_) | Self::Blob { .. } => None,
        }
    }

    fn directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&mut table.directory),
            Self::KeyDirectory(directory) => Some(&mut directory.directory),
            Self::Row(row) => Some(&mut row.directory),
            Self::Bytes(_) | Self::Blob { .. } => None,
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
    binding: CommittedTableBinding,
    field_names: HashMap<[u8; 16], String>,
    field_types: HashMap<[u8; 16], Raw>,
}

#[derive(Clone)]
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
