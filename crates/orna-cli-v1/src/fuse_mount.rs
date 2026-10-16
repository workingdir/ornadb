use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use orna_project_v1::{ObjectIdentityManifest, ObjectKind, ProjectLoader};
use orna_repository_v1::{
    AdmittedBlobReference, CommittedTableBinding, GitCommitRef, GitHashAlgorithm,
    HistoricalFormatContext, KeyRange, NativeGraphContext, NativeOid, Repository, TypedKey,
};
use orna_runtime_v1::TableObjectId;

const ROOT_INODE: u64 = 1;
const ROW_PAGE_LIMIT: usize = 256;
const ENTRY_TTL: Duration = Duration::from_secs(1);

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

struct WorkspaceFilesystem {
    nodes: HashMap<u64, FsNode>,
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
            let graph = Arc::new(
                context
                    .open_native_graph(&rows)
                    .map_err(|error| format!("committed native graph could not be opened: {error}"))?,
            );
            builder.add_table_rows(table_directory, graph, schema.binding(), &field_names)?;
        }

        Ok(WorkspaceFilesystem {
            nodes: builder.nodes,
        })
    }

    fn attr(&self, inode: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&inode)?;
        let (kind, size, permissions, links) = match &node.kind {
            NodeKind::Directory(entries) => (
                FileType::Directory,
                0,
                0o555,
                2 + u32::try_from(entries.iter().filter(|entry| entry.kind == FileType::Directory).count())
                    .unwrap_or(u32::MAX - 2),
            ),
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
}

impl Filesystem for WorkspaceFilesystem {
    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(directory) = self.nodes.get(&parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let NodeKind::Directory(entries) = &directory.kind else {
            reply.error(Errno::ENOTDIR);
            return;
        };
        let Some(entry) = entries.iter().find(|entry| entry.name.as_os_str() == name) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(attr) = self.attr(entry.inode) else {
            reply.error(Errno::EIO);
            return;
        };
        reply.entry(&ENTRY_TTL, &attr, Generation(1));
    }

    fn getattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        _handle: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        match self.attr(inode.0) {
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
        let Some(node) = self.nodes.get(&inode.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match &node.kind {
            NodeKind::Directory(_) => reply.error(Errno::EISDIR),
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
            }
            NodeKind::Blob { graph, reference } => {
                let length = reference.content_identity().length();
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
                match graph.read_blob_range(reference, offset..end, &scope) {
                    Ok(range) => reply.data(range.bytes()),
                    Err(_) => reply.error(Errno::EIO),
                }
            }
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
        let Some(directory) = self.nodes.get(&inode.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let NodeKind::Directory(children) = &directory.kind else {
            reply.error(Errno::ENOTDIR);
            return;
        };
        let mut entries = Vec::with_capacity(children.len() + 2);
        entries.push(DirectoryEntry {
            inode: inode.0,
            name: OsString::from("."),
            kind: FileType::Directory,
        });
        entries.push(DirectoryEntry {
            inode: directory.parent,
            name: OsString::from(".."),
            kind: FileType::Directory,
        });
        entries.extend(children.iter().cloned());
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        for (index, entry) in entries.iter().enumerate().skip(start) {
            if reply.add(
                INodeNo(entry.inode),
                u64::try_from(index + 1).unwrap_or(u64::MAX),
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
                kind: NodeKind::Directory(Vec::new()),
            },
        );
        Self {
            nodes,
            next_inode: ROOT_INODE + 1,
        }
    }

    fn table_directory(&mut self, module_name: &str, table_name: &str) -> Result<u64, String> {
        let mut parent = ROOT_INODE;
        if !module_name.is_empty() {
            for component in module_name.split('.') {
                parent = self.ensure_directory(parent, &safe_component(component))?;
            }
        }
        self.ensure_directory(parent, &safe_component(table_name))
    }

    fn add_table_rows(
        &mut self,
        table_directory: u64,
        graph: Arc<NativeGraphContext>,
        binding: &CommittedTableBinding,
        field_names: &HashMap<[u8; 16], String>,
    ) -> Result<(), String> {
        let mut lower = None;
        loop {
            let range = KeyRange::new(lower.clone(), None, ROW_PAGE_LIMIT)
                .map_err(|error| format!("row range could not be created: {error:?}"))?;
            let range_scope = graph
                .open_read_scope()
                .map_err(|error| format!("row read scope could not be opened: {error:?}"))?;
            let rows = graph
                .range_rows(&range, &range_scope)
                .map_err(|error| format!("committed rows could not be read: {error:?}"))?;
            if rows.is_empty() {
                break;
            }
            let page_length = rows.len();
            let next_lower = rows.last().map(|row| row.key().clone());
            let skip_duplicate = usize::from(lower.is_some());
            for row in rows.into_iter().skip(skip_duplicate) {
                let row_scope = graph
                    .open_read_scope()
                    .map_err(|error| format!("row read scope could not be opened: {error:?}"))?;
                let decoded = graph
                    .decode_admitted_row_fields(&row, binding, &row_scope)
                    .map_err(|error| format!("committed row fields could not be decoded: {error:?}"))?;
                let row_directory = self.ensure_directory(
                    table_directory,
                    &key_component(row.key())?,
                )?;
                let mut document = String::new();
                let mut blob_files = Vec::new();
                for field in decoded.fields() {
                    let Some(metadata) = field.blob_metadata() else {
                        continue;
                    };
                    let field_name = field_names.get(field.field_id()).ok_or_else(|| {
                        "committed Blob field has no source identity".to_owned()
                    })?;
                    let value_oid = metadata.descriptor_oid().ok_or_else(|| {
                        "committed Blob field has no descriptor identity".to_owned()
                    })?;
                    let algorithm = match value_oid.len() {
                        20 => GitHashAlgorithm::Sha1,
                        32 => GitHashAlgorithm::Sha256,
                        _ => return Err("committed Blob descriptor has an invalid OID width".into()),
                    };
                    let descriptor = NativeOid::new(algorithm, value_oid.as_bytes())
                        .map_err(|error| format!("committed Blob descriptor is invalid: {error:?}"))?;
                    let stored = graph
                        .read_stored_blob_value(&row, &descriptor, &row_scope)
                        .map_err(|error| format!("committed Blob reference could not be admitted: {error:?}"))?;
                    let suffix = stored
                        .annotation()
                        .suffix()
                        .or(metadata.suffix())
                        .unwrap_or("blob");
                    let file_name = safe_component(&format!("{field_name}.{suffix}"));
                    let relative_path = format!("./{file_name}");
                    writeln!(
                        &mut document,
                        "{field_name}: {{ path: {relative_path:?}, media_type: {:?} }}",
                        stored.annotation().media_type(),
                    )
                    .expect("formatting into a String succeeds");
                    blob_files.push((file_name, stored.reference().clone()));
                }
                self.add_file(
                    row_directory,
                    "data.orna",
                    NodeKind::Bytes(document.into_bytes()),
                )?;
                for (file_name, reference) in blob_files {
                    self.add_file(
                        row_directory,
                        &file_name,
                        NodeKind::Blob {
                            graph: Arc::clone(&graph),
                            reference,
                        },
                    )?;
                }
            }
            if page_length < ROW_PAGE_LIMIT {
                break;
            }
            if next_lower == lower {
                return Err("row pagination did not advance".into());
            }
            lower = next_lower;
        }
        Ok(())
    }

    fn ensure_directory(&mut self, parent: u64, name: &str) -> Result<u64, String> {
        if let Some(existing) = self.nodes.get(&parent).and_then(|node| match &node.kind {
            NodeKind::Directory(entries) => entries
                .iter()
                .find(|entry| entry.name == OsStr::new(name))
                .map(|entry| entry.inode),
            _ => None,
        }) {
            if self.nodes.get(&existing).is_some_and(|node| matches!(&node.kind, NodeKind::Directory(_))) {
                return Ok(existing);
            }
            return Err(format!("mount path component conflicts with a file: {name}"));
        }
        self.insert_node(
            parent,
            name,
            NodeKind::Directory(Vec::new()),
            FileType::Directory,
        )
    }

    fn add_file(&mut self, parent: u64, name: &str, kind: NodeKind) -> Result<u64, String> {
        if self.nodes.get(&parent).is_some_and(|node| match &node.kind {
            NodeKind::Directory(entries) => entries.iter().any(|entry| entry.name == OsStr::new(name)),
            _ => true,
        }) {
            return Err(format!("mount path contains a duplicate file name: {name}"));
        }
        let file_type = match &kind {
            NodeKind::Directory(_) => FileType::Directory,
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
        self.next_inode = self
            .next_inode
            .checked_add(1)
            .ok_or_else(|| "mount tree has too many entries".to_owned())?;
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| "mount parent directory is missing".to_owned())?;
        let NodeKind::Directory(entries) = &mut parent_node.kind else {
            return Err("mount parent is not a directory".into());
        };
        entries.push(DirectoryEntry {
            inode,
            name: OsString::from(name),
            kind: file_type,
        });
        self.nodes.insert(inode, FsNode { parent, kind });
        Ok(inode)
    }
}

struct FsNode {
    parent: u64,
    kind: NodeKind,
}

enum NodeKind {
    Directory(Vec<DirectoryEntry>),
    Bytes(Vec<u8>),
    Blob {
        graph: Arc<NativeGraphContext>,
        reference: AdmittedBlobReference,
    },
}

#[derive(Clone)]
struct DirectoryEntry {
    inode: u64,
    name: OsString,
    kind: FileType,
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

fn key_component(key: &TypedKey) -> Result<String, String> {
    match key {
        TypedKey::Text(value) => Ok(safe_component(value)),
        _ => key
            .canonical_bytes()
            .map(|bytes| format!("@{}", hex_bytes(&bytes)))
            .map_err(|error| format!("row key could not be encoded: {error:?}")),
    }
}

fn safe_component(value: &str) -> String {
    if value.is_empty() {
        return "%00".into();
    }
    let escape_dots = value == "." || value == "..";
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if !escape_dots
            && (byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "%{byte:02X}");
        }
    }
    escaped
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
