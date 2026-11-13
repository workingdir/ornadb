use super::{
    Arc, BTreeMap, BlobCandidate, BlobField, CandidateSnapshot, ContentIdentity, Directory,
    DirectoryEntry, FileAttr, FileType, Format3Context, FsNode, GitHashAlgorithm, GraphError,
    HashMap, HashSet, INodeNo, KeyDirectory, KeyRange, MountWriter, NativeGraphContext, NativeOid,
    NestedDirectory, NestedEntries, NodeKind, OsStr, OsString, ROOT_INODE, ROW_PAGE_LIMIT, Raw,
    ResolvedTableSchema, RowDirectory, RowLoadError, RowLoadState, RowMapSnapshot, RowSaveContext,
    RuntimeTableIdentity, SnapshotPin, StoredFieldLayout, TableDirectory, TableObjectId,
    TableSource, TypedKey, UNIX_EPOCH, VfsComponent, VfsRowProjection, VfsStoredField,
    admit_accepted_replacement, blob_metadata_from_raw, decode_canonical_storage_value,
    flatten_key_components, has_nested_children, is_optional_field, key_component,
    large_text_payload, nested_entries, nested_entry, nested_leaf_text, nested_value_at,
    project_colliding_table_component, project_component, render_stored_source_value,
    row_save_context, runtime_table_key_bytes, select_runtime_row_fields, selected_blob_payload,
    selects_blob, stored_field_layout, stored_value_is_null, unproject_component,
};

pub(crate) struct FsBuilder {
    pub(crate) nodes: HashMap<u64, FsNode>,
    pub(crate) next_inode: u64,
    pub(crate) writer: Option<Arc<MountWriter>>,
    pub(crate) table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    /// Open handle counts for inodes opened with `opendir` or a stateless `open` (handle 0).
    pub(crate) open_handles: HashMap<u64, u64>,
    /// Retired subtrees whose root has an open handle at or below it. Each stays loaded
    /// until the last such handle is released.
    pub(crate) superseded_nodes: HashSet<u64>,
    /// The name each retired node was listed under. A diagnostic uses it to name the path of a
    /// retained node that a replacement took out of its listing; dropped with the node.
    pub(crate) retired_names: HashMap<u64, OsString>,
    /// Inodes of projected source modules and their directories. A table path
    /// that reaches one of these yields to the source path (profiles/vfs-1.md).
    pub(crate) source_nodes: HashSet<u64>,
}

impl FsBuilder {
    pub(crate) fn new() -> Self {
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
            open_handles: HashMap::new(),
            superseded_nodes: HashSet::new(),
            retired_names: HashMap::new(),
            source_nodes: HashSet::new(),
        }
    }

    /// Attributes report uid/gid 0 and advisory permission bits. chmod and
    /// chown are refused with EPERM by `denied_attribute_change`, so the bits
    /// here never grant or revoke access.
    pub(crate) fn attr(&self, inode: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&inode)?;
        let (kind, size, permissions, links) = match &node.kind {
            NodeKind::Directory(_)
            | NodeKind::Table(_)
            | NodeKind::KeyDirectory(_)
            | NodeKind::Row(_)
            | NodeKind::NestedDirectory(_) => {
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
            NodeKind::BlobBytes(candidate) => (FileType::RegularFile, candidate.len(), 0o444, 1),
            NodeKind::Text { bytes, .. } => (FileType::RegularFile, bytes.len(), 0o444, 1),
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

    pub(crate) fn directory(&self, inode: u64) -> Option<&Directory> {
        self.nodes.get(&inode)?.kind.directory()
    }

    pub(crate) fn directory_mut(&mut self, inode: u64) -> Option<&mut Directory> {
        self.nodes.get_mut(&inode)?.kind.directory_mut()
    }

    /// Projects each reachable source module at its logical path, creating its
    /// directories. Reserved root `.git` and `.orna` are never projected.
    pub(crate) fn add_source_modules(
        &mut self,
        modules: &[orna_check::ModuleInput],
    ) -> Result<(), String> {
        for module in modules {
            let components = module.logical_path.split('/').collect::<Vec<_>>();
            let Some((leaf, directories)) = components.split_last() else {
                continue;
            };
            if matches!(components.first(), Some(&(".git" | ".orna"))) {
                continue;
            }
            let mut parent = ROOT_INODE;
            for directory in directories {
                parent = self.ensure_directory(parent, directory)?;
                self.source_nodes.insert(parent);
            }
            let inode = self.insert_node(
                parent,
                leaf,
                NodeKind::Bytes(module.source.clone().into_bytes()),
                FileType::RegularFile,
            )?;
            self.source_nodes.insert(inode);
        }
        Ok(())
    }

    /// Whether the source tree occupies `component` directly under `parent`.
    pub(crate) fn source_occupies(&self, parent: u64, component: &str) -> bool {
        self.directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(component)))
            .is_some_and(|entry| self.source_nodes.contains(&entry.inode))
    }

    /// Projects a generated namespace or table component under `parent`. A source
    /// path keeps its plain name on collision, and the generated component escapes
    /// its first byte instead, so the table stays projected (profiles/vfs-1.md:31-33).
    pub(crate) fn generated_component(&self, parent: u64, name: &str) -> Result<String, String> {
        let plain = project_component(name);
        if !self.source_occupies(parent, &plain) {
            return Ok(plain);
        }
        let escaped = project_colliding_table_component(name);
        if self.source_occupies(parent, &escaped) {
            return Err(format!(
                "generated name {escaped:?} collides with a source path"
            ));
        }
        Ok(escaped)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn table_directory(
        &mut self,
        module_name: &str,
        table_name: &str,
    ) -> Result<u64, String> {
        let mut parent = ROOT_INODE;
        if !module_name.is_empty() {
            for component in module_name.split('.') {
                let name = self.generated_component(parent, component)?;
                parent = self.ensure_directory(parent, &name)?;
            }
        }
        let name = self.generated_component(parent, table_name)?;
        self.ensure_directory(parent, &name)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_table_rows(
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

    pub(crate) fn ensure_directory(&mut self, parent: u64, name: &str) -> Result<u64, String> {
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

    pub(crate) fn lookup_child(
        &mut self,
        parent: u64,
        name: &OsStr,
    ) -> Result<Option<u64>, String> {
        let parent_kind = self
            .nodes
            .get(&parent)
            .map(|node| &node.kind)
            .ok_or_else(|| "mount lookup parent is missing".to_owned())?;
        if matches!(parent_kind, NodeKind::Row(_)) {
            self.ensure_row_loaded(parent)?;
        }
        self.load_nested_for_name(parent, name)?;
        if let Some(inode) = self
            .directory(parent)
            .and_then(|directory| directory.entry(name))
            .map(|entry| entry.inode)
        {
            return Ok(Some(inode));
        }
        if matches!(
            self.nodes.get(&parent).map(|node| &node.kind),
            Some(NodeKind::Table(_))
        ) && let Some(inode) = self.lookup_indexed_text_row(parent, name)?
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

    pub(crate) fn lookup_indexed_text_row(
        &mut self,
        table_inode: u64,
        name: &OsStr,
    ) -> Result<Option<u64>, String> {
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

    pub(crate) fn ensure_readdir(&mut self, inode: u64, start: u64) -> Result<(), String> {
        while self.nested_pending(inode) {
            let directory = self
                .directory(inode)
                .ok_or_else(|| "mount directory is missing".to_owned())?;
            // Pages only append, so the cursor's index stays valid across loads.
            let target = directory.resume_index(start).saturating_add(ROW_PAGE_LIMIT);
            if directory.entries.len() >= target {
                break;
            }
            self.load_nested_page(inode)?;
        }
        if matches!(
            self.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return self.ensure_row_loaded(inode).map_err(String::from);
        }
        let table_inode = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Table(_)) => Some(inode),
            Some(NodeKind::KeyDirectory(directory)) => Some(directory.table_inode),
            _ => None,
        };
        let Some(table_inode) = table_inode else {
            return Ok(());
        };
        loop {
            let directory = self
                .directory(inode)
                .ok_or_else(|| "mount directory is missing".to_owned())?;
            // Loading pages moves the cursor's index, so the target is recomputed each round.
            let target = directory.resume_index(start).saturating_add(ROW_PAGE_LIMIT);
            let loaded = directory.entries.len();
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

    /// Visits the entries after the kernel cursor `start`, loading table pages as needed.
    pub(crate) fn readdir_page(
        &mut self,
        inode: u64,
        start: u64,
        mut visit: impl FnMut(u64, &DirectoryEntry) -> bool,
    ) -> Result<(), String> {
        self.ensure_readdir(inode, start)?;
        let directory = self
            .directory(inode)
            .ok_or_else(|| "mount directory is missing".to_owned())?;
        for entry in &directory.entries[directory.resume_index(start)..] {
            if !visit(entry.offset(), entry) {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn load_next_key_page(&mut self, table_inode: u64) -> Result<(), String> {
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

    pub(crate) fn record_table_error(&mut self, table_inode: u64, error: &str) {
        if let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
            && table.error.is_none()
        {
            table.error = Some(error.to_owned());
            eprintln!("FUSE table row listing failed: {error}");
        }
    }

    pub(crate) fn add_key_path(
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

    pub(crate) fn ensure_key_directory(
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

    pub(crate) fn ensure_row_directory(
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
            directory.push(OsString::from(name), inode, FileType::Directory);
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

    #[allow(clippy::zero_sized_map_values)]
    pub(crate) fn ensure_row_loaded(&mut self, inode: u64) -> Result<(), RowLoadError> {
        let (source, key, state) = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Row(row)) => (
                Arc::clone(&row.source),
                row.key.clone(),
                match &row.state {
                    RowLoadState::Unloaded => None,
                    RowLoadState::Ready => Some(Ok(())),
                    RowLoadState::Failed(error) => Some(Err(RowLoadError::Fatal(error.clone()))),
                },
            ),
            _ => return Err("mount row directory is missing".into()),
        };
        if let Some(state) = state {
            return state;
        }
        let result =
            self.row_file_specs(&source, &key)
                .and_then(|files| -> Result<(), RowLoadError> {
                    let directory = self
                        .directory(inode)
                        .ok_or_else(|| "mount row directory is missing".to_owned())?;
                    let mut names = HashMap::with_capacity(files.len());
                    for (name, _) in &files {
                        let name = OsString::from(name);
                        if directory.entry(name.as_os_str()).is_some()
                            || names.insert(name, ()).is_some()
                        {
                            return Err("projected row file names collide".into());
                        }
                    }
                    for (name, kind) in files {
                        self.add_file(inode, &name, kind)?;
                    }
                    Ok(())
                });
        match &result {
            Ok(()) => {
                if let Some(NodeKind::Row(row)) =
                    self.nodes.get_mut(&inode).map(|node| &mut node.kind)
                {
                    row.state = RowLoadState::Ready;
                }
            }
            // A transient failure leaves the row Unloaded, so the next access retries it.
            Err(RowLoadError::Transient(error)) => {
                eprintln!("FUSE row projection deferred: {error}");
            }
            Err(RowLoadError::Fatal(error)) => {
                if let Some(NodeKind::Row(row)) =
                    self.nodes.get_mut(&inode).map(|node| &mut node.kind)
                {
                    row.state = RowLoadState::Failed(error.clone());
                }
                eprintln!("FUSE row projection failed: {error}");
            }
        }
        result
    }

    pub(crate) fn row_file_specs(
        &self,
        source: &Arc<TableSource>,
        key: &TypedKey,
    ) -> Result<Vec<(String, NodeKind)>, RowLoadError> {
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

    #[allow(clippy::too_many_lines)]
    /// Projects one row's document and content files from the pinned row and
    /// the CWD row the runtime holds for it. A CWD row that fails validation is
    /// reported (EIO) rather than replaced by the pinned HEAD row (W08-08).
    /// `refreshed_baseline` reaches the document's save context.
    pub(crate) fn project_row_files(
        &self,
        source: &Arc<TableSource>,
        key: &TypedKey,
        runtime_row: Option<&[u8]>,
        refreshed_baseline: Option<Vec<u8>>,
    ) -> Result<Vec<(String, NodeKind)>, RowLoadError> {
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
        let admitted = match runtime_row {
            Some(runtime_row) => {
                let canonical_key = key
                    .canonical_bytes()
                    .map_err(|error| format!("committed row key cannot be encoded: {error:?}"))?;
                let writer = self.writer.as_ref().ok_or_else(|| {
                    "the CWD row cannot be admitted without a mounted writer".to_owned()
                })?;
                Some(writer.admit_runtime_row(source, &canonical_key, runtime_row)?)
            }
            None => None,
        };
        let (canonical_fields, _projection_source) = select_runtime_row_fields(
            admitted.as_deref(),
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
        let mut nested_trees: Vec<(String, NodeKind)> = Vec::new();
        let mut nested_fields: Vec<&str> = Vec::new();
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
                let algorithm = match value_oid.width() {
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
                            )
                            .into());
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
                        )
                        .into());
                    }
                };
                let file_name = orna_vfs::project_content_name(
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
                    reference: Some(reference.clone()),
                    replacement_bytes: None,
                    required: !is_optional_field(field_type),
                    text: false,
                });
                blob_files.push((file_name, reference));
            } else if let Some(text) = large_text_payload(&value) {
                let required = !is_optional_field(field_type);
                let text_file =
                    orna_vfs::project_text_name(field_name).map_err(|error| {
                        format!(
                            "stored Str field {field_name} cannot be projected as text: {error:?}"
                        )
                    })?;
                projected_fields.push(VfsStoredField::text(
                    field_name.clone(),
                    u64::try_from(text.len())
                        .map_err(|_| "stored Str length is invalid".to_owned())?,
                    required,
                ));
                blob_fields.push(BlobField {
                    name: field_name.clone(),
                    file_name: text_file,
                    media_type: "text/plain".to_owned(),
                    suffix: None,
                    reference: None,
                    replacement_bytes: Some(Arc::new(CandidateSnapshot::from(Arc::<[u8]>::from(
                        text.as_bytes(),
                    )))),
                    required,
                    text: true,
                });
            } else if stored_value_is_null(&value) {
                projected_fields.push(VfsStoredField::null(field_name.clone()));
            } else {
                projected_fields.push(VfsStoredField::document(
                    field_name.clone(),
                    render_stored_source_value(&value)?,
                ));
                if has_nested_children(&value) {
                    nested_fields.push(field_name.as_str());
                    nested_trees.push((
                        field_name.clone(),
                        NodeKind::NestedDirectory(NestedDirectory::new(
                            Arc::new(value),
                            Vec::new(),
                        )),
                    ));
                }
            }
        }
        let projection = VfsRowProjection::project_with_nested_directories(
            SnapshotPin::capture(Arc::clone(&source.graph)),
            &projected_fields,
            &nested_fields,
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
        for (field, node) in nested_trees {
            let directory = projection
                .directories()
                .iter()
                .find(|directory| directory.field() == field.as_str())
                .ok_or_else(|| "projected nested directory has no admitted field".to_owned())?;
            files.push((directory.name().to_owned(), node));
        }
        for file in projection.files() {
            let candidate_field = candidate_fields
                .iter()
                .find(|field| field.file_name == file.name())
                .cloned()
                .ok_or_else(|| "projected file has no admitted field".to_owned())?;
            let candidate = document_save.as_ref().map(|save| BlobCandidate {
                save: Arc::clone(save),
                field: candidate_field.clone(),
            });
            if file.is_text() {
                let bytes = Arc::clone(
                    candidate_field
                        .replacement_bytes
                        .as_ref()
                        .ok_or_else(|| "projected text file has no captured text".to_owned())?,
                );
                files.push((file.name().to_owned(), NodeKind::Text { bytes, candidate }));
                continue;
            }
            let reference = blob_files
                .iter()
                .find_map(|(name, reference)| (name == file.name()).then_some(reference))
                .ok_or_else(|| "projected Blob has no admitted file reference".to_owned())?;
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
    pub(crate) fn path_of(&self, inode: u64) -> Option<String> {
        self.path_through(inode, false)
    }

    /// Walks up from `inode`. With `retired` set, a node no longer listed under its parent is
    /// named by the name it was listed under, so a diagnostic still names the path of a handle
    /// that a replacement left behind.
    pub(crate) fn path_through(&self, inode: u64, retired: bool) -> Option<String> {
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
            let retired_name = || {
                self.retired_names
                    .get(&current)
                    .cloned()
                    .filter(|_| retired)
            };
            let name = listed.or_else(indexed).or_else(retired_name)?;
            components.push(name.to_string_lossy().into_owned());
            current = parent;
        }
        components.reverse();
        Some(components.join("/"))
    }

    pub(crate) fn display_path(&self, inode: u64) -> String {
        self.path_through(inode, true)
            .unwrap_or_else(|| format!("inode {inode} (no longer in the mount)"))
    }

    /// Re-projects a document after a stale save (ORNA-VFS-011, W15-03). New
    /// opens then show the row another writer accepted and save against it;
    /// handles and scratch files opened earlier keep their own baseline and
    /// stay stale. Returns whether the document now shows a newer row.
    pub(crate) fn refresh_stale_document(
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
        let row = live_row.ok_or_else(|| {
            "another writer deleted the row; remount to stop showing it".to_owned()
        })?;
        let files =
            self.project_row_files(&save.source, &save.key, Some(&row), Some(row.clone()))?;
        let mut document = None;
        let mut others = Vec::with_capacity(files.len());
        for (name, kind) in files {
            if name == "data.orna" {
                document = Some(kind);
            } else {
                others.push((name, kind));
            }
        }
        let document =
            document.ok_or_else(|| "the refreshed row has no data.orna document".to_owned())?;
        if let Some(baselines) = &save.writer.row_baselines {
            baselines
                .record_accepted_row(table.table(), &key, &row)
                .map_err(|error| {
                    format!("the refreshed row baseline could not be recorded: {error}")
                })?;
        }
        self.reconcile_row_files(document_inode, Some(document), others)?;
        Ok(true)
    }

    /// The save context and document bytes a replacement generation of a row document takes once
    /// the mount has accepted a new row: the context compares against that accepted row, and the
    /// bytes are the projection that context serves, so the generation's own edit reads the image
    /// a fresh open reads. The superseded generation keeps the context it captured (ORNA-VFS-011,
    /// 41-vfs.md:38-40).
    pub(crate) fn accepted_generation_document(
        &self,
        superseded: &Arc<RowSaveContext>,
    ) -> Result<(Arc<RowSaveContext>, Vec<u8>), String> {
        let table = superseded
            .source
            .runtime_table
            .as_ref()
            .ok_or_else(|| "the accepted row has no runtime table".to_owned())?;
        let key = runtime_table_key_bytes(&superseded.key)?;
        let row = superseded
            .writer
            .live_row(table, &key)?
            .ok_or_else(|| "the accepted row is missing after its save".to_owned())?;
        let files = self.project_row_files(
            &superseded.source,
            &superseded.key,
            Some(&row),
            Some(row.clone()),
        )?;
        match files.into_iter().find(|(name, _)| name == "data.orna") {
            Some((_, NodeKind::Document { save, bytes })) => Ok((save, bytes)),
            _ => Err("the accepted row has no data.orna document".to_owned()),
        }
    }

    /// Re-projects a row from the row the save has just accepted (profiles/vfs-1.md:76-80).
    /// A Str over 64 KiB shows its `.txt` sibling beside `data.orna`; a Str at or under the
    /// bound is inline again, so its stale `.txt` sibling leaves the directory. Open handles
    /// keep the `.txt` generation they captured (`retire_node`).
    pub(crate) fn project_accepted_document(
        &mut self,
        document_inode: u64,
        save: &Arc<RowSaveContext>,
    ) -> Result<(), String> {
        let table = save
            .source
            .runtime_table
            .as_ref()
            .ok_or_else(|| "the accepted row has no runtime table".to_owned())?;
        let key = runtime_table_key_bytes(&save.key)?;
        let row = save
            .writer
            .live_row(table, &key)?
            .ok_or_else(|| "the accepted row is missing after its save".to_owned())?;
        let parent = self
            .nodes
            .get(&document_inode)
            .map(|node| node.parent)
            .ok_or_else(|| "the accepted document disappeared".to_owned())?;
        let files =
            self.project_row_files(&save.source, &save.key, Some(&row), Some(row.clone()))?;
        let mut document = None;
        let mut others = Vec::with_capacity(files.len());
        for (name, kind) in files {
            if name == "data.orna" {
                document = Some(kind);
            } else {
                others.push((name, kind));
            }
        }
        // Only a row that carries or carried a projected child changes its layout; other rows
        // keep the document node the handles were opened against.
        let listed_children = self
            .directory(parent)
            .ok_or_else(|| "the accepted row directory disappeared".to_owned())?
            .entries
            .iter()
            .any(|entry| {
                entry.name.as_os_str() != OsStr::new("data.orna")
                    && !matches!(
                        self.nodes.get(&entry.inode).map(|node| &node.kind),
                        Some(NodeKind::Temporary { .. })
                    )
            });
        if others.is_empty() && !listed_children {
            return Ok(());
        }
        // A row with a `.txt` sibling, projected or listed, takes the freshly projected document
        // node as base does; any other row keeps the node its handles were opened against
        // (profiles/vfs-1.md:71-80).
        let text_layout = others
            .iter()
            .any(|(_, kind)| matches!(kind, NodeKind::Text { .. }))
            || self.directory(parent).is_some_and(|directory| {
                directory
                    .entries
                    .iter()
                    .any(|entry| self.is_text_node(entry.inode))
            });
        let document =
            document.ok_or_else(|| "the accepted row has no data.orna document".to_owned())?;
        // Refuse a name collision before any mutation, so a refused projection changes nothing.
        for (name, kind) in &others {
            if !matches!(kind, NodeKind::Text { .. }) {
                continue;
            }
            let name = OsStr::new(name);
            let collides = self
                .directory(parent)
                .and_then(|directory| directory.entry(name))
                .is_some_and(|entry| !self.is_text_node(entry.inode));
            if collides {
                return Err(format!(
                    "the projected text file \"{name}\" collides with a sibling",
                    name = name.to_string_lossy(),
                ));
            }
        }
        let document = text_layout.then_some(document);
        self.reconcile_row_files(document_inode, document, others)
    }

    /// Reconciles the row directory of `document_inode` with its freshly projected `document`
    /// and the row's other projected files `others`. A stale refresh and an accepted save share
    /// this, so a new open shows the row that was accepted (profiles/vfs-1.md:71-80).
    pub(crate) fn reconcile_row_files(
        &mut self,
        document_inode: u64,
        document: Option<NodeKind>,
        others: Vec<(String, NodeKind)>,
    ) -> Result<(), String> {
        let row_inode = self
            .nodes
            .get(&document_inode)
            .map(|node| node.parent)
            .ok_or_else(|| "the refreshed document disappeared".to_owned())?;
        // data.orna keeps its node unless a new one is given, so handles opened earlier keep their
        // captured baseline. Pending scratch files keep their entries: their saves belong to the
        // user, not the row. The row's other projected files are reconciled by name, in place, so
        // every view agrees on the new row and held listings keep their positions. Files the row no
        // longer projects are retired.
        let (kept_inodes, kept_names): (HashSet<u64>, HashSet<OsString>) = {
            let directory = self
                .directory(row_inode)
                .ok_or_else(|| "the refreshed row is not a directory".to_owned())?;
            directory
                .entries
                .iter()
                .filter(|entry| {
                    entry.name.as_os_str() == OsStr::new("data.orna")
                        || matches!(
                            self.nodes.get(&entry.inode).map(|node| &node.kind),
                            Some(NodeKind::Temporary { .. })
                        )
                })
                .map(|entry| (entry.inode, entry.name.clone()))
                .unzip()
        };
        if let Some((name, _)) = others
            .iter()
            .find(|(name, _)| kept_names.contains(OsStr::new(name.as_str())))
        {
            return Err(format!(
                "the refreshed row entry {name} collides with a pending scratch file"
            ));
        }
        // Reconcile by name so a surviving row file keeps its readdir position and cookie: a file
        // the refreshed row still projects is swapped in place, a new one is appended, and a file
        // the row no longer projects leaves. Pending scratch files were kept above.
        let vanished: Vec<(OsString, u64)> = self
            .directory(row_inode)
            .ok_or_else(|| "the refreshed row is not a directory".to_owned())?
            .entries
            .iter()
            .filter(|entry| {
                !kept_inodes.contains(&entry.inode)
                    && !others
                        .iter()
                        .any(|(name, _)| name.as_str() == entry.name.as_os_str())
            })
            .map(|entry| (entry.name.clone(), entry.inode))
            .collect();
        for (name, inode) in vanished {
            self.directory_mut(row_inode)
                .and_then(|directory| directory.remove_entry(&name))
                .ok_or_else(|| "a vanished row entry had already left its directory".to_owned())?;
            self.retire_node(inode);
        }
        for (name, kind) in others {
            let listed = self
                .directory(row_inode)
                .and_then(|directory| directory.entry(OsStr::new(name.as_str())))
                .is_some();
            if listed {
                self.replace_child(row_inode, OsStr::new(name.as_str()), kind)?;
            } else {
                self.add_file(row_inode, &name, kind)?;
            }
        }
        if let Some(document) = document {
            self.nodes
                .get_mut(&document_inode)
                .ok_or_else(|| "the refreshed document disappeared".to_owned())?
                .kind = document;
        }
        Ok(())
    }

    /// Whether `inode` is a projected `.txt` sibling of a large Str.
    pub(crate) fn is_text_node(&self, inode: u64) -> bool {
        matches!(
            self.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Text { .. })
        )
    }

    /// Swaps the child `name` of `parent` for a new node at the same readdir position, so
    /// sibling offsets hold. The superseded node keeps its captured bytes for open handles,
    /// and a superseded directory stays loaded for a held listing.
    pub(crate) fn replace_child(
        &mut self,
        parent: u64,
        name: &OsStr,
        kind: NodeKind,
    ) -> Result<u64, String> {
        let inode = self.next_inode;
        self.next_inode = inode
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
            .ok_or_else(|| "the mount child to replace is missing".to_owned())?;
        let file_type = kind.file_type();
        let entry = &mut directory.entries[index];
        let superseded = std::mem::replace(&mut entry.inode, inode);
        entry.kind = file_type;
        self.nodes.insert(inode, FsNode { parent, kind });
        self.retired_names.insert(superseded, name.to_owned());
        self.retire_node(superseded);
        Ok(inode)
    }

    /// Replaces the accepted document `document` with a generation that shows the accepted row.
    /// The superseded node keeps the bytes it captured, so a handle pinned to it reports the
    /// size it opened with (profiles/vfs-1.md:126-133). A handle pinned to a superseded
    /// generation never commits a candidate: `accept_document` refuses its dirty close flush as
    /// stale and retained before the CAS compare, so a row that returns to the bytes that handle
    /// captured is not accepted over the owner's row (ORNA-VFS-011; a conservative choice,
    /// spec-silent on that return). A clean handle has no candidate and acknowledges its fsync.
    /// The new generation saves against the row the mount
    /// accepted and shows that row's projected document, the image a fresh open reads
    /// (ORNA-VFS-011). Returns the new node's inode.
    pub(crate) fn replace_document_generation(&mut self, document: u64) -> Result<u64, String> {
        let (parent, superseded) = match self.nodes.get(&document) {
            Some(FsNode {
                parent,
                kind: NodeKind::Document { save, .. },
            }) => (*parent, Arc::clone(save)),
            _ => return Err("replaced document is not a row document".into()),
        };
        let (save, bytes) = self.accepted_generation_document(&superseded)?;
        let name = self
            .directory(parent)
            .and_then(|directory| {
                directory
                    .entries
                    .iter()
                    .find(|entry| entry.inode == document)
            })
            .map(|entry| entry.name.clone())
            .ok_or_else(|| "replaced document is missing from its row".to_owned())?;
        self.replace_child(parent, name.as_os_str(), NodeKind::Document { bytes, save })
    }

    /// Refreshes a stale document and reports the outcome on stderr.
    pub(crate) fn report_refresh(
        &mut self,
        document_inode: u64,
        stale: &Arc<RowSaveContext>,
        path: &str,
    ) {
        match self.refresh_stale_document(document_inode, stale) {
            Ok(true) => {
                eprintln!("{path} was changed outside this mount; it now shows the current row");
            }
            Ok(false) => {}
            Err(error) => eprintln!("{path}: the current row could not be shown: {error}"),
        }
    }

    pub(crate) fn add_file(
        &mut self,
        parent: u64,
        name: &str,
        kind: NodeKind,
    ) -> Result<u64, String> {
        if self
            .directory(parent)
            .is_none_or(|directory| directory.entry(OsStr::new(name)).is_some())
        {
            return Err(format!("mount path contains a duplicate file name: {name}"));
        }
        let file_type = kind.file_type();
        self.insert_node(parent, name, kind, file_type)
    }

    /// Whether `inode` is a nested directory with children that have not been listed yet.
    pub(crate) fn nested_pending(&self, inode: u64) -> bool {
        matches!(
            self.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::NestedDirectory(nested)) if !nested.exhausted
        )
    }

    /// Lists the next page of at most `ROW_PAGE_LIMIT` children of a nested directory from its
    /// pinned value (profiles/vfs-1.md:75). Container children become unloaded directories; other
    /// children become read-only leaves. A page is recorded as listed only after all its children
    /// are inserted. A failed insert removes the children that page added, so a later lookup or
    /// readdir retries the same page.
    pub(crate) fn load_nested_page(&mut self, inode: u64) -> Result<(), String> {
        let (root, path, start) = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::NestedDirectory(nested)) if !nested.exhausted => (
                Arc::clone(&nested.root),
                nested.path.clone(),
                nested.next_index,
            ),
            _ => return Ok(()),
        };
        let value = nested_value_at(&root, &path)
            .ok_or_else(|| "a nested value is no longer present in the pinned row".to_owned())?;
        let count = nested_entries(value)
            .map(NestedEntries::child_count)
            .ok_or_else(|| "a nested directory no longer holds a container value".to_owned())?;
        let end = count.min(start.saturating_add(ROW_PAGE_LIMIT));
        let mut files = Vec::with_capacity(end - start);
        for index in start..end {
            let (name, child) = nested_entry(value, index)
                .ok_or_else(|| "a nested directory entry is malformed".to_owned())?;
            let kind = if has_nested_children(child) {
                let mut child_path = path.clone();
                child_path.push(index);
                NodeKind::NestedDirectory(NestedDirectory::new(Arc::clone(&root), child_path))
            } else {
                NodeKind::Bytes(nested_leaf_text(child)?.into_bytes())
            };
            files.push((name.project(index), kind));
        }
        {
            let mut seen = HashSet::with_capacity(files.len());
            if let Some((name, _)) = files.iter().find(|(name, _)| !seen.insert(name.as_str())) {
                return Err(format!("nested directory projects two children as {name}"));
            }
        }
        let first_cookie = self
            .directory(inode)
            .map_or(0, |directory| directory.next_cookie);
        for (name, kind) in files {
            if let Err(error) = self.add_file(inode, &name, kind) {
                if let Some(directory) = self
                    .nodes
                    .get_mut(&inode)
                    .and_then(|node| node.kind.directory_mut())
                {
                    directory.retain_entries(|entry| entry.cookie < first_cookie);
                }
                return Err(error);
            }
        }
        if let Some(NodeKind::NestedDirectory(nested)) =
            self.nodes.get_mut(&inode).map(|node| &mut node.kind)
        {
            nested.next_index = end;
            nested.exhausted = end == count;
        }
        Ok(())
    }

    /// Lists pages until `name` is listed or the level is exhausted. An absent name lists the
    /// whole level, since no page can be ruled out without reading it.
    pub(crate) fn load_nested_for_name(&mut self, inode: u64, name: &OsStr) -> Result<(), String> {
        while self.nested_pending(inode) {
            if self
                .directory(inode)
                .is_some_and(|directory| directory.entry(name).is_some())
            {
                break;
            }
            self.load_nested_page(inode)?;
        }
        Ok(())
    }

    pub(crate) fn insert_node(
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
        directory.push(name, inode, file_type);
        self.next_inode = next_inode;
        self.nodes.insert(inode, FsNode { parent, kind });
        Ok(inode)
    }

    /// Records an open of `inode` through `opendir` or a stateless `open` (handle 0), so a
    /// replacement keeps the captured subtree loaded for as long as a handle refers to it.
    pub(crate) fn track_open(&mut self, inode: u64) {
        *self.open_handles.entry(inode).or_insert(0) += 1;
    }

    /// Releases one open of `inode`. When the last handle on `inode` is released, every
    /// retired subtree that no open handle still pins is removed. Untracked inodes are ignored.
    pub(crate) fn track_release(&mut self, inode: u64) {
        let Some(count) = self.open_handles.get_mut(&inode) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            self.open_handles.remove(&inode);
            self.reclaim_superseded();
        }
    }

    /// Whether `root` or a node below it has an open handle.
    pub(crate) fn subtree_pinned(&self, root: u64) -> bool {
        if self.open_handles.is_empty() {
            return false;
        }
        let mut pending = vec![root];
        while let Some(inode) = pending.pop() {
            if self.open_handles.contains_key(&inode) {
                return true;
            }
            if let Some(directory) = self.directory(inode) {
                pending.extend(directory.entries.iter().map(|entry| entry.inode));
            }
        }
        false
    }

    /// Whether `inode` or an ancestor was superseded by a replacement. Such a node is retired
    /// and shows only the generation it captured.
    pub(crate) fn is_superseded(&self, inode: u64) -> bool {
        let mut current = inode;
        loop {
            if self.superseded_nodes.contains(&current) {
                return true;
            }
            match self.nodes.get(&current) {
                Some(node) if node.parent != current => current = node.parent,
                _ => return false,
            }
        }
    }

    /// Removes `root` and every node below it.
    pub(crate) fn remove_subtree(&mut self, root: u64) {
        let mut pending = vec![root];
        while let Some(inode) = pending.pop() {
            if let Some(directory) = self.directory(inode) {
                pending.extend(directory.entries.iter().map(|entry| entry.inode));
            }
            self.nodes.remove(&inode);
            self.retired_names.remove(&inode);
        }
    }

    /// Removes each retired subtree that no open handle still pins.
    pub(crate) fn reclaim_superseded(&mut self) {
        let unpinned: Vec<u64> = self
            .superseded_nodes
            .iter()
            .copied()
            .filter(|&root| !self.subtree_pinned(root))
            .collect();
        for root in unpinned {
            self.superseded_nodes.remove(&root);
            self.remove_subtree(root);
        }
    }

    /// Drops a retired subtree that no longer has a name. Open handles keep it readable
    /// (profiles/vfs-1.md: old handles remain on captured immutable content).
    ///
    /// A subtree with an open handle at or below its root is kept in `superseded_nodes` and
    /// stays loaded, so a held directory handle keeps enumerating its listing and a refresh
    /// never makes that listing fail with ENOENT (41-vfs.md:39-40). The last release reclaims
    /// it. Any other subtree is removed at once, with its descendants.
    ///
    /// Accepted FUSE nodeid-lifetime deviation: retention follows tracked open handles, not
    /// kernel lookup counts. `forget` and `batch_forget` keep fuser's no-op defaults, so a
    /// retired subtree is removed once no handle holds it, even if the kernel still caches its
    /// nodeid. `ENTRY_TTL` is zero, so the kernel revalidates names and attributes on use; a
    /// stale nodeid then fails with ENOENT instead of serving retired bytes.
    pub(crate) fn retire_node(&mut self, inode: u64) {
        if self.subtree_pinned(inode) {
            self.superseded_nodes.insert(inode);
        } else {
            self.remove_subtree(inode);
        }
    }

    /// Replaces the blob named `name` under `parent` with a new generation inode.
    /// The superseded inode keeps its captured content for existing handles.
    pub(crate) fn replace_blob_generation(
        &mut self,
        parent: u64,
        name: &OsStr,
        superseded: u64,
        bytes: Arc<CandidateSnapshot>,
    ) -> Result<u64, String> {
        let (text, candidate) = match self.nodes.get(&superseded).map(|node| &node.kind) {
            Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_)) => (false, None),
            Some(NodeKind::Text { candidate, .. }) => (
                true,
                candidate.as_ref().map(|candidate| BlobCandidate {
                    save: Arc::clone(&candidate.save),
                    field: candidate.field.clone(),
                }),
            ),
            _ => return Err("replaced mount entry is not a blob".into()),
        };
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
                kind: if text {
                    NodeKind::Text { bytes, candidate }
                } else {
                    NodeKind::BlobBytes(bytes)
                },
            },
        );
        self.retire_node(superseded);
        Ok(generation)
    }
}
