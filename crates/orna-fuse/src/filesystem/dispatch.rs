use super::*;

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

    fn open(&self, request: &Request, inode: INodeNo, flags: fuser::OpenFlags, reply: ReplyOpen) {
        self.fuse_open(request, inode, flags, reply);
    }

    fn create(
        &self,
        request: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        self.fuse_create(request, parent, name, mode, umask, flags, reply);
    }

    fn rename(
        &self,
        request: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        self.fuse_rename(request, parent, name, newparent, newname, flags, reply);
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

    fn read(
        &self,
        request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        size: u32,
        flags: fuser::OpenFlags,
        lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        self.fuse_read(
            request, inode, handle, offset, size, flags, lock_owner, reply,
        );
    }

    fn write(
        &self,
        request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        data: &[u8],
        write_flags: fuser::WriteFlags,
        flags: fuser::OpenFlags,
        lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        self.fuse_write(
            request,
            inode,
            handle,
            offset,
            data,
            write_flags,
            flags,
            lock_owner,
            reply,
        );
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

    #[allow(clippy::similar_names)]
    fn setattr(
        &self,
        request: &Request,
        inode: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<std::time::SystemTime>,
        handle: Option<fuser::FileHandle>,
        crtime: Option<std::time::SystemTime>,
        chgtime: Option<std::time::SystemTime>,
        bkuptime: Option<std::time::SystemTime>,
        flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        self.fuse_setattr(
            request, inode, mode, uid, gid, size, atime, mtime, ctime, handle, crtime, chgtime,
            bkuptime, flags, reply,
        );
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
