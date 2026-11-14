use super::*;

impl WorkspaceFilesystem {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn fuse_open(
        &self,
        request: &Request,
        inode: INodeNo,
        flags: fuser::OpenFlags,
        reply: ReplyOpen,
    ) {
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

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    pub(crate) fn fuse_read(
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fuse_write(
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
}
