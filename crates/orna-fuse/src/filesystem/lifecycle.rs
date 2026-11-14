use super::*;

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
}
