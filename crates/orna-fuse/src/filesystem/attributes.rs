use super::*;

impl WorkspaceFilesystem {
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    pub(crate) fn fuse_setattr(
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
}
