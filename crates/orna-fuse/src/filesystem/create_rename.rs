use super::*;

impl WorkspaceFilesystem {
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    pub(crate) fn fuse_create(
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fuse_rename(
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
}
