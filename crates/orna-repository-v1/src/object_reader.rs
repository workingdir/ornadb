//! One long-lived Git object reader per repository handle.
//!
//! Admitting a snapshot needs a few fixed objects: the root tree, the `.orna`
//! tree, the tracked metadata blobs and the `.orna/store` and `main.orna`
//! entries (ORNA-META-001, ORNA-META-002). Reading them through ordinary Git
//! commands costs one or more processes per object, path or tree entry.
//! [`ObjectReader`] keeps a single `git cat-file --batch-command` process for
//! the life of a [`Repository`] handle and its clones, and resolves each object
//! when it is asked for. The work therefore follows the objects read, never
//! the size of the tree around them (ORNA-ROW-009).

use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Mutex, MutexGuard, OnceLock},
};

use crate::{
    CommittedTreeEntry, CommittedTreeEntryKind, GitCommitRef, ManagedPath, PrivateCommit,
    Repository, RepositoryError, init::format_context::STORE_PATH, native_graph::GitHashAlgorithm,
    scrub_git_routing_environment,
};

/// Most entries a committed tree walk may visit, counting every occurrence of
/// a shared subtree and every interior tree. The fixed walk limits are not
/// `ProjectLimits` fields; orna-project-v1 documents them and reports a breach
/// as `ProjectLoadError::SourceTreeLimit`.
const MAX_SOURCE_TREE_WALK_ENTRIES: u64 = 1 << 20;
/// Most path bytes a committed tree walk may list, counting every occurrence
/// of a shared subtree. The entry cap alone does not bound this: a chain of
/// 255-byte names expands to gigabytes of paths within the entry cap.
const MAX_SOURCE_TREE_PATH_BYTES: u64 = 1 << 27;
/// Deepest tree nesting a committed tree walk may expand.
const MAX_SOURCE_TREE_DEPTH: usize = 256;

/// Entry count, path bytes and nesting height of a measured tree.
#[derive(Clone, Copy)]
struct SourceTreeSummary {
    /// Entries the walk visits, counting every occurrence of shared subtrees.
    entries: u64,
    /// Bytes of every visited entry's full path, relative to the measured
    /// tree. A subtree's paths gain its parent's prefix once per occurrence.
    path_bytes: u64,
    /// Levels of tree nesting below the measured tree.
    height: usize,
}

/// Bounds one response header: an echoed object name or a full SHA-256 ID,
/// followed by the object type and size.
const MAX_HEADER_BYTES: u64 = 4 * 1024;
/// Bounds one tree entry name before its bytes are accumulated.
const MAX_TREE_ENTRY_NAME_BYTES: usize = 4 * 1024;
/// The longest mode Git writes, `100644`, plus its separating space, with one
/// byte to spare for a zero-padded mode.
const MAX_TREE_ENTRY_MODE_BYTES: u64 = 8;

/// A Git object type as `cat-file` reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl ObjectKind {
    fn parse(name: &[u8]) -> Option<Self> {
        match name {
            b"blob" => Some(Self::Blob),
            b"tree" => Some(Self::Tree),
            b"commit" => Some(Self::Commit),
            b"tag" => Some(Self::Tag),
            _ => None,
        }
    }

    /// Git's name for this type, as `git cat-file -t` prints it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }
}

/// The header `cat-file` reports for one existing object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectHeader {
    oid: String,
    kind: ObjectKind,
    size: u64,
}

impl ObjectHeader {
    /// The object's full native ID in lowercase hexadecimal.
    pub(crate) fn oid(&self) -> &str {
        &self.oid
    }

    pub(crate) const fn kind(&self) -> ObjectKind {
        self.kind
    }

    /// The object's size in bytes, as `info` reported it.
    pub(crate) const fn size(&self) -> u64 {
        self.size
    }
}

/// A tree entry mode after Git's own canonicalisation of recorded modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TreeEntryMode {
    File,
    Executable,
    Symlink,
    Tree,
    Gitlink,
}

impl TreeEntryMode {
    /// Mirrors Git's `canon_mode`: a regular file is `100644` or `100755`, and
    /// a mode that is not a file, symlink or directory is a gitlink.
    const fn from_recorded(mode: u32) -> Self {
        match mode & 0o170_000 {
            0o100_000 if mode & 0o100 != 0 => Self::Executable,
            0o100_000 => Self::File,
            0o120_000 => Self::Symlink,
            0o040_000 => Self::Tree,
            _ => Self::Gitlink,
        }
    }
}

/// One entry of a Git tree object. Names are raw bytes: Git does not require
/// UTF-8, and a lookup must not fail because of an unrelated sibling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntry {
    mode: TreeEntryMode,
    name: Vec<u8>,
    oid: String,
}

impl TreeEntry {
    pub(crate) const fn mode(&self) -> TreeEntryMode {
        self.mode
    }

    pub(crate) fn name(&self) -> &[u8] {
        &self.name
    }

    pub(crate) fn oid(&self) -> &str {
        &self.oid
    }
}

/// Reads Git objects through one `git cat-file --batch-command` process,
/// started on first use and shared by a repository handle and its clones.
///
/// Objects are read exactly: a promised partial-clone object is reported
/// missing instead of fetched, and a local `refs/replace` overlay is ignored.
/// A failed exchange discards the process, so a later read never resumes from
/// an unknown position in its output.
pub(crate) struct ObjectReader {
    worktree: PathBuf,
    algorithm: OnceLock<GitHashAlgorithm>,
    session: Mutex<Option<Session>>,
    /// Processes this reader has started, for tests that count spawns.
    #[cfg(test)]
    starts: std::sync::atomic::AtomicUsize,
}

impl fmt::Debug for ObjectReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectReader")
            .finish_non_exhaustive()
    }
}

impl ObjectReader {
    pub(crate) fn new(worktree: PathBuf) -> Self {
        Self {
            worktree,
            algorithm: OnceLock::new(),
            session: Mutex::new(None),
            #[cfg(test)]
            starts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The repository's object format. It is fixed when the repository is
    /// created, so it is observed once per handle rather than once per caller.
    pub(crate) fn algorithm(&self) -> Result<GitHashAlgorithm, RepositoryError> {
        if let Some(algorithm) = self.algorithm.get() {
            return Ok(*algorithm);
        }
        let mut command = Command::new("git");
        command.current_dir(&self.worktree);
        scrub_git_routing_environment(&mut command);
        crate::bind_git_discovery_to_worktree(&mut command, &self.worktree);
        command.args(["rev-parse", "--show-object-format"]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let algorithm = match output.stdout.trim_ascii_end() {
            b"sha1" => GitHashAlgorithm::Sha1,
            b"sha256" => GitHashAlgorithm::Sha256,
            _ => return Err(RepositoryError::UnsupportedObjectFormat),
        };
        Ok(*self.algorithm.get_or_init(|| algorithm))
    }

    /// Reports the object that `name` resolves to, or `None` when Git has no
    /// such object. `name` is any object name `cat-file` accepts, such as
    /// `<commit>^{tree}` or `<commit>:<path>`.
    pub(crate) fn info(&self, name: &str) -> Result<Option<ObjectHeader>, RepositoryError> {
        let algorithm = self.algorithm()?;
        self.with_session(|session| session.request("info", name, algorithm))
    }

    /// Reads one blob of at most `max_bytes`. A missing object, another object
    /// type or a larger blob fails before any of its bytes are read.
    pub(crate) fn read_blob(
        &self,
        oid: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let algorithm = self.algorithm()?;
        self.with_session(|session| {
            let header = session
                .request("info", oid, algorithm)?
                .ok_or(RepositoryError::GitOperationFailed)?;
            let size =
                usize::try_from(header.size).map_err(|_| RepositoryError::GitOperationFailed)?;
            if header.kind != ObjectKind::Blob || size > max_bytes {
                return Err(RepositoryError::GitOperationFailed);
            }
            if session.request("contents", oid, algorithm)? != Some(header) {
                return Err(RepositoryError::GitOperationFailed);
            }
            let mut bytes = vec![0; size];
            session
                .output()?
                .read_exact(&mut bytes)
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            session.finish_contents()?;
            Ok(bytes)
        })
    }

    /// Reads the complete bytes of an object whose header `info` has just
    /// reported. The caller admits the header's kind and size first; the
    /// bytes are read through the same shared process, one exchange.
    pub(crate) fn read_exact(
        &self,
        oid: &str,
        header: &ObjectHeader,
        algorithm: GitHashAlgorithm,
    ) -> Result<Vec<u8>, RepositoryError> {
        let size = usize::try_from(header.size).map_err(|_| RepositoryError::GitOperationFailed)?;
        self.with_session(|session| {
            if session.request("contents", oid, algorithm)? != Some(header.clone()) {
                return Err(RepositoryError::GitOperationFailed);
            }
            let mut bytes = vec![0; size];
            session
                .output()?
                .read_exact(&mut bytes)
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            session.finish_contents()?;
            Ok(bytes)
        })
    }

    /// Lists every entry of one tree object, in Git's tree order.
    pub(crate) fn tree_entries(&self, oid: &str) -> Result<Vec<TreeEntry>, RepositoryError> {
        let mut entries = Vec::new();
        self.visit_tree(oid, |mode, name, object| {
            entries.push(TreeEntry {
                mode,
                name: name.to_vec(),
                oid: hex(object),
            });
        })?;
        Ok(entries)
    }

    /// Refuses a recursive listing of `treeish` under `path` before Git expands
    /// it. The treeish must resolve to a tree, so a missing one fails closed. A
    /// path that names no tree lists nothing, so it is accepted. Every
    /// occurrence of a shared subtree is measured, so the check is bounded even
    /// when the expansion it guards is not.
    pub(crate) fn check_expansion(&self, treeish: &str, path: &str) -> Result<(), RepositoryError> {
        let root = self
            .info(&format!("{treeish}^{{tree}}"))?
            .ok_or(RepositoryError::GitOperationFailed)?;
        if path.is_empty() {
            return self.check_tree_budget(root.oid(), None, 0);
        }
        match self.info(&format!("{treeish}^{{tree}}:{path}"))? {
            Some(target) if target.kind == ObjectKind::Tree => {
                let prefix = u64::try_from(path.len()).unwrap_or(u64::MAX).saturating_add(1);
                self.check_tree_budget(target.oid(), None, prefix)
            }
            _ => Ok(()),
        }
    }

    /// Checks that the tree at `oid`, listed under a prefix of `prefix` bytes
    /// that includes its separator, stays within the walk limits. `site` is as
    /// in [`Self::measure_tree`].
    fn check_tree_budget(
        &self,
        oid: &str,
        site: Option<usize>,
        prefix: u64,
    ) -> Result<(), RepositoryError> {
        let summary = self.measure_tree(oid, site, 0, &mut HashMap::new())?;
        let listed = summary
            .entries
            .saturating_mul(prefix)
            .saturating_add(summary.path_bytes);
        if listed > MAX_SOURCE_TREE_PATH_BYTES {
            return Err(RepositoryError::SourceTreeLimit);
        }
        Ok(())
    }

    /// Measures the tree at `oid` without listing it. `site` is the number of
    /// leading `STORE_PATH` components the tree names, or `None` once it leaves
    /// that prefix. Results are memoised per tree and site, so each shared
    /// subtree is read once while its expanded entries still count per
    /// occurrence.
    fn measure_tree(
        &self,
        oid: &str,
        site: Option<usize>,
        depth: usize,
        memo: &mut HashMap<(String, Option<usize>), SourceTreeSummary>,
    ) -> Result<SourceTreeSummary, RepositoryError> {
        let key = (oid.to_owned(), site);
        if let Some(summary) = memo.get(&key) {
            return Ok(*summary);
        }
        if depth > MAX_SOURCE_TREE_DEPTH {
            return Err(RepositoryError::SourceTreeLimit);
        }
        let store_depth = STORE_PATH.split('/').count();
        let mut summary = SourceTreeSummary {
            entries: 0,
            path_bytes: 0,
            height: 0,
        };
        for entry in self.tree_entries(oid)? {
            let name_bytes = u64::try_from(entry.name().len()).unwrap_or(u64::MAX);
            summary.entries += 1;
            summary.path_bytes = summary.path_bytes.saturating_add(name_bytes);
            Self::admit(&summary)?;
            if !matches!(entry.mode(), TreeEntryMode::Tree) {
                continue;
            }
            let child_site = Self::store_child_site(site, entry.name());
            if child_site == Some(store_depth) {
                continue;
            }
            let child = self.measure_tree(entry.oid(), child_site, depth + 1, memo)?;
            if depth + 1 + child.height > MAX_SOURCE_TREE_DEPTH {
                return Err(RepositoryError::SourceTreeLimit);
            }
            // Each of the child's entries gains this name and a separator.
            summary.entries += child.entries;
            summary.path_bytes = summary
                .path_bytes
                .saturating_add(name_bytes.saturating_add(1).saturating_mul(child.entries))
                .saturating_add(child.path_bytes);
            Self::admit(&summary)?;
            summary.height = summary.height.max(child.height + 1);
        }
        memo.insert(key, summary);
        Ok(summary)
    }

    /// Refuses a summary whose entries or path bytes exceed the walk limits.
    fn admit(summary: &SourceTreeSummary) -> Result<(), RepositoryError> {
        if summary.entries > MAX_SOURCE_TREE_WALK_ENTRIES
            || summary.path_bytes > MAX_SOURCE_TREE_PATH_BYTES
        {
            return Err(RepositoryError::SourceTreeLimit);
        }
        Ok(())
    }

    /// The store site of the child `name` of a tree at `site`, or `None` once
    /// the child leaves the `STORE_PATH` prefix.
    fn store_child_site(site: Option<usize>, name: &[u8]) -> Option<usize> {
        let index = site?;
        let component = STORE_PATH.split('/').nth(index)?;
        (component.as_bytes() == name).then_some(index + 1)
    }

    /// Finds the entry called `name` in one tree object without keeping its
    /// siblings.
    pub(crate) fn find_tree_entry(
        &self,
        oid: &str,
        name: &[u8],
    ) -> Result<Option<TreeEntry>, RepositoryError> {
        let mut found = None;
        self.visit_tree(oid, |mode, entry_name, object| {
            if entry_name == name {
                found = Some(TreeEntry {
                    mode,
                    name: entry_name.to_vec(),
                    oid: hex(object),
                });
            }
        })?;
        Ok(found)
    }

    /// Streams the entries of one tree object to `visit` as Git stores them.
    /// A missing object or another object type fails.
    fn visit_tree(
        &self,
        oid: &str,
        mut visit: impl FnMut(TreeEntryMode, &[u8], &[u8]),
    ) -> Result<(), RepositoryError> {
        let algorithm = self.algorithm()?;
        self.with_session(|session| {
            let header = session
                .request("contents", oid, algorithm)?
                .ok_or(RepositoryError::GitOperationFailed)?;
            if header.kind != ObjectKind::Tree {
                return Err(RepositoryError::GitOperationFailed);
            }
            let mut body = session.output()?.take(header.size);
            let mut mode = Vec::with_capacity(MAX_TREE_ENTRY_MODE_BYTES as usize);
            let mut name = Vec::new();
            let mut object = [0u8; GitHashAlgorithm::Sha256.width()];
            let object = &mut object[..algorithm.width()];
            loop {
                mode.clear();
                let read = (&mut body)
                    .take(MAX_TREE_ENTRY_MODE_BYTES)
                    .read_until(b' ', &mut mode)
                    .map_err(|_| RepositoryError::GitOperationFailed)?;
                if read == 0 {
                    break;
                }
                let recorded = parse_recorded_mode(&mode)?;
                name.clear();
                (&mut body)
                    .take(MAX_TREE_ENTRY_NAME_BYTES as u64 + 1)
                    .read_until(0, &mut name)
                    .map_err(|_| RepositoryError::GitOperationFailed)?;
                if name.pop() != Some(0) || name.is_empty() || name.contains(&b'/') {
                    return Err(RepositoryError::GitOperationFailed);
                }
                body.read_exact(object)
                    .map_err(|_| RepositoryError::GitOperationFailed)?;
                visit(TreeEntryMode::from_recorded(recorded), &name, object);
            }
            if body.limit() != 0 {
                return Err(RepositoryError::GitOperationFailed);
            }
            session.finish_contents()
        })
    }

    /// Runs one complete request/response exchange on the shared process,
    /// starting it first if needed.
    fn with_session<T>(
        &self,
        exchange: impl FnOnce(&mut Session) -> Result<T, RepositoryError>,
    ) -> Result<T, RepositoryError> {
        let mut slot = self.lock_session();
        if slot.is_none() {
            *slot = Some(Session::start(&self.worktree)?);
            #[cfg(test)]
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let Some(session) = slot.as_mut() else {
            return Err(RepositoryError::GitOperationFailed);
        };
        let result = exchange(session);
        if result.is_err() {
            // The position in the output stream is unknown after a failed
            // exchange; the next read starts a fresh process.
            *slot = None;
        }
        result
    }

    fn lock_session(&self) -> MutexGuard<'_, Option<Session>> {
        match self.session.lock() {
            Ok(slot) => slot,
            Err(poisoned) => {
                // A panic interrupted an exchange; its process cannot be trusted.
                let mut slot = poisoned.into_inner();
                *slot = None;
                self.session.clear_poison();
                slot
            }
        }
    }

    /// The process currently serving reads, for tests that prove reuse.
    #[cfg(test)]
    pub(crate) fn process_id(&self) -> Option<u32> {
        self.lock_session()
            .as_ref()
            .map(|session| session.child.id())
    }

    /// How many cat-file processes this reader has started.
    #[cfg(test)]
    pub(crate) fn process_starts(&self) -> usize {
        self.starts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One running `git cat-file --batch-command` process.
struct Session {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
}

impl Session {
    fn start(worktree: &Path) -> Result<Self, RepositoryError> {
        let mut command = Command::new("git");
        command.current_dir(worktree);
        scrub_git_routing_environment(&mut command);
        crate::bind_git_discovery_to_worktree(&mut command, worktree);
        // Set after scrubbing, which removes inherited replace-object settings.
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args(["cat-file", "--batch-command"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RepositoryError::GitOperationFailed);
        };
        Ok(Self {
            child,
            input: Some(input),
            output: Some(BufReader::new(output)),
        })
    }

    fn output(&mut self) -> Result<&mut BufReader<ChildStdout>, RepositoryError> {
        self.output
            .as_mut()
            .ok_or(RepositoryError::GitOperationFailed)
    }

    /// Sends one `info` or `contents` command and reads its header line.
    fn request(
        &mut self,
        command: &str,
        name: &str,
        algorithm: GitHashAlgorithm,
    ) -> Result<Option<ObjectHeader>, RepositoryError> {
        // One command per line: a line break in a name would desynchronise
        // every later response.
        if name.is_empty() || name.contains(['\n', '\r', '\0']) {
            return Err(RepositoryError::GitOperationFailed);
        }
        let input = self
            .input
            .as_mut()
            .ok_or(RepositoryError::GitOperationFailed)?;
        input
            .write_all(format!("{command} {name}\n").as_bytes())
            .and_then(|()| input.flush())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut line = Vec::new();
        self.output()?
            .take(MAX_HEADER_BYTES)
            .read_until(b'\n', &mut line)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if line.pop() != Some(b'\n') {
            return Err(RepositoryError::GitOperationFailed);
        }
        if line.strip_suffix(b" missing") == Some(name.as_bytes()) {
            return Ok(None);
        }
        let mut fields = line.split(|byte| *byte == b' ');
        let (Some(oid), Some(kind), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(RepositoryError::GitOperationFailed);
        };
        if oid.len() != algorithm.width() * 2
            || !oid
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        let kind = ObjectKind::parse(kind).ok_or(RepositoryError::GitOperationFailed)?;
        let size = std::str::from_utf8(size)
            .ok()
            .and_then(|size| size.parse::<u64>().ok())
            .ok_or(RepositoryError::GitOperationFailed)?;
        let oid =
            String::from_utf8(oid.to_vec()).map_err(|_| RepositoryError::GitOperationFailed)?;
        Ok(Some(ObjectHeader { oid, kind, size }))
    }

    /// Consumes the line feed that ends every `contents` response.
    fn finish_contents(&mut self) -> Result<(), RepositoryError> {
        let mut terminator = [0u8; 1];
        self.output()?
            .read_exact(&mut terminator)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if terminator == *b"\n" {
            Ok(())
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing both pipes ends `cat-file` at its next read or write; reap
        // it so no process outlives the handle.
        self.input.take();
        self.output.take();
        let _ = self.child.wait();
    }
}

fn parse_recorded_mode(field: &[u8]) -> Result<u32, RepositoryError> {
    let digits = field
        .strip_suffix(b" ")
        .filter(|digits| !digits.is_empty())
        .ok_or(RepositoryError::GitOperationFailed)?;
    digits.iter().try_fold(0u32, |mode, digit| match digit {
        b'0'..=b'7' => Ok((mode << 3) | u32::from(digit - b'0')),
        _ => Err(RepositoryError::GitOperationFailed),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

impl Repository {
    /// The object reader shared by this handle and its clones.
    pub(crate) fn objects(&self) -> &ObjectReader {
        &self.objects
    }

    /// Lists a committed tree like [`Self::list_committed_tree`], but never
    /// enters the format-3 native store at `.orna/store`.
    ///
    /// The store is database content that grows with the rows it holds; a
    /// repository keeps it "normally sparsely excluded" (19-repository), and no
    /// source module, row unit or metadata file lives below it. Trees are read
    /// through the repository's object reader, one exchange per directory, so
    /// the cost follows the source tree whatever the store holds.
    pub fn list_committed_source_tree(
        &self,
        commit: &GitCommitRef,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        let resolved = self
            .objects
            .info(&format!("{}^{{commit}}", commit.as_str()))?
            .ok_or(RepositoryError::GitOperationFailed)?;
        if resolved.kind() != ObjectKind::Commit || resolved.oid() != commit.as_str() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let root = self
            .objects
            .info(&format!("{}^{{tree}}", commit.as_str()))?
            .ok_or(RepositoryError::GitOperationFailed)?;
        if root.kind() != ObjectKind::Tree {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.source_tree_entries(root.oid())
    }

    /// Private-candidate variant of [`Self::list_committed_source_tree`] over
    /// the candidate's exact immutable tree.
    pub fn list_private_candidate_source_tree(
        &self,
        candidate: &PrivateCommit,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.source_tree_entries(candidate.tree().as_str())
    }

    /// Walks a tree depth first in Git's tree order, as `git ls-tree -r`
    /// lists it, and skips the subtree at the fixed store position. The walk
    /// is preceded by a measurement, because shared subtrees multiply their
    /// entries without any bound on the stored bytes.
    fn source_tree_entries(&self, root: &str) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        self.objects.check_tree_budget(root, Some(0), 0)?;
        let mut listed = Vec::new();
        let mut pending = vec![(String::new(), self.objects.tree_entries(root)?.into_iter())];
        loop {
            let Some((directory, entries)) = pending.last_mut() else {
                break;
            };
            let Some(entry) = entries.next() else {
                pending.pop();
                continue;
            };
            let name = std::str::from_utf8(entry.name())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            let path = if directory.is_empty() {
                name.to_owned()
            } else {
                format!("{directory}/{name}")
            };
            let kind = match entry.mode() {
                TreeEntryMode::Tree => {
                    if path != STORE_PATH {
                        let children = self.objects.tree_entries(entry.oid())?;
                        pending.push((path, children.into_iter()));
                    }
                    continue;
                }
                TreeEntryMode::File => CommittedTreeEntryKind::File { executable: false },
                TreeEntryMode::Executable => CommittedTreeEntryKind::File { executable: true },
                TreeEntryMode::Symlink => CommittedTreeEntryKind::Symlink,
                TreeEntryMode::Gitlink => CommittedTreeEntryKind::Submodule,
            };
            listed.push(CommittedTreeEntry {
                path: ManagedPath::new(&path)?,
                kind,
            });
        }
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write as _,
        path::Path,
        process::{Command, Stdio},
    };

    use super::{ObjectKind, TreeEntryMode};
    use crate::{CommittedTreeEntryKind, GitCommitRef, Repository, RepositoryError};

    fn git(directory: &Path, arguments: &[&str]) -> String {
        git_with_input(directory, arguments, "")
    }

    fn git_with_input(directory: &Path, arguments: &[&str], input: &str) -> String {
        let mut child = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .env("GIT_AUTHOR_NAME", "Orna")
            .env("GIT_AUTHOR_EMAIL", "orna@example.invalid")
            .env("GIT_COMMITTER_NAME", "Orna")
            .env("GIT_COMMITTER_EMAIL", "orna@example.invalid")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run git");
        child
            .stdin
            .take()
            .expect("git stdin")
            .write_all(input.as_bytes())
            .expect("write git stdin");
        let output = child.wait_with_output().expect("wait for git");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git stdout is UTF-8")
            .trim()
            .to_owned()
    }

    /// A committed tree with a nested module, an executable, a symlink and a
    /// gitlink beside the root module.
    fn repository() -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().expect("create repository directory");
        let root = directory.path();
        git(
            root,
            &["init", "--quiet", "--initial-branch=main", "--template="],
        );
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.join("lib")).expect("create module directory");
        fs::write(root.join("main.orna"), "pub fn main() {}\n").expect("write root module");
        fs::write(root.join("lib/main.orna"), "pub fn helper() {}\n").expect("write nested module");
        fs::write(root.join("run.sh"), "#!/bin/sh\n").expect("write script");
        git(root, &["add", "."]);
        git(root, &["update-index", "--chmod=+x", "run.sh"]);
        let target = git_with_input(root, &["hash-object", "-w", "--stdin"], "main.orna");
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{target},alias.orna"),
            ],
        );
        git(root, &["commit", "--quiet", "--no-verify", "-m", "fixture"]);
        let head = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},vendor"),
            ],
        );
        git(root, &["commit", "--quiet", "--no-verify", "-m", "gitlink"]);
        let repository = Repository::discover(root).expect("discover repository");
        (directory, repository)
    }

    #[test]
    fn reads_trees_and_blobs_through_one_long_lived_process() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let head = git(root, &["rev-parse", "HEAD"]);
        let tree = objects
            .info(&format!("{head}^{{tree}}"))
            .expect("resolve the root tree")
            .expect("the root tree exists");
        assert_eq!(tree.kind(), ObjectKind::Tree);
        assert_eq!(tree.oid(), git(root, &["rev-parse", "HEAD^{tree}"]));
        let process = objects
            .process_id()
            .expect("the first read started the reader");

        let entries = objects
            .tree_entries(tree.oid())
            .expect("list the root tree");
        let listed = entries
            .iter()
            .map(|entry| (entry.name(), entry.mode()))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            [
                (b"alias.orna".as_slice(), TreeEntryMode::Symlink),
                (b"lib".as_slice(), TreeEntryMode::Tree),
                (b"main.orna".as_slice(), TreeEntryMode::File),
                (b"run.sh".as_slice(), TreeEntryMode::Executable),
                (b"vendor".as_slice(), TreeEntryMode::Gitlink),
            ]
        );
        let module = objects
            .find_tree_entry(tree.oid(), b"main.orna")
            .expect("find the root module")
            .expect("the root module exists");
        assert_eq!(module.oid(), git(root, &["rev-parse", "HEAD:main.orna"]));
        assert_eq!(
            objects
                .read_blob(module.oid(), 64)
                .expect("read the root module"),
            b"pub fn main() {}\n"
        );
        assert!(
            objects
                .find_tree_entry(tree.oid(), b"absent.orna")
                .expect("look up an absent entry")
                .is_none()
        );
        assert!(
            objects
                .info(&format!("{head}:absent.orna"))
                .expect("resolve an absent path")
                .is_none()
        );
        assert_eq!(
            objects.process_id(),
            Some(process),
            "every read reused one process"
        );

        let clone = repository.clone();
        assert!(
            clone
                .objects()
                .info("HEAD")
                .expect("read through a clone")
                .is_some()
        );
        assert_eq!(
            clone.objects().process_id(),
            Some(process),
            "clones share the reader"
        );
    }

    #[test]
    fn a_multi_object_walk_starts_exactly_one_process() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
        assert_eq!(objects.process_starts(), 0, "no read has started a process");
        for _ in 0..8 {
            objects
                .info(&tree)
                .expect("read the tree")
                .expect("the tree exists");
            objects
                .tree_entries(&tree)
                .expect("list the tree");
        }
        assert_eq!(
            objects.process_starts(),
            1,
            "a multi-object walk reuses one cat-file process"
        );
    }

    /// Blob reads, which the walk uses for every chunk, must also share the one
    /// process; a walk that only lists trees would not prove that.
    #[test]
    fn a_multi_object_walk_reads_blobs_through_the_same_process() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let blob = git(root, &["rev-parse", "HEAD:main.orna"]);
        let algorithm = objects.algorithm().expect("the repository algorithm");
        for _ in 0..8 {
            assert_eq!(
                objects.read_blob(&blob, 1024).expect("read the blob"),
                b"pub fn main() {}\n"
            );
            let header = objects
                .info(&blob)
                .expect("info the blob")
                .expect("the blob exists");
            assert_eq!(
                objects
                    .read_exact(&blob, &header, algorithm)
                    .expect("read the blob exactly"),
                b"pub fn main() {}\n"
            );
        }
        assert_eq!(objects.process_starts(), 1, "blob reads reuse one process");
    }

    /// A reader process that died is discarded rather than left in use: the
    /// read reports an error instead of hanging on a closed pipe, and the next
    /// read starts a fresh process.
    #[test]
    fn a_dead_reader_process_is_discarded_and_the_next_read_restarts_it() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let blob = git(root, &["rev-parse", "HEAD:main.orna"]);
        assert_eq!(
            objects.read_blob(&blob, 1024).expect("first read"),
            b"pub fn main() {}\n"
        );
        let pid = objects.process_id().expect("a live reader process");
        let killed = Command::new("sh")
            .args(["-c", &format!("kill -9 {pid}")])
            .status()
            .expect("send SIGKILL");
        assert!(killed.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| sender.send(objects.read_blob(&blob, 1024).is_err()));
            let failed = receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a read on a dead reader returns instead of hanging");
            assert!(failed, "a dead reader cannot serve a read");
        });
        assert_eq!(
            objects.read_blob(&blob, 1024).expect("restarted read"),
            b"pub fn main() {}\n"
        );
        assert_eq!(objects.process_starts(), 2, "the reader was restarted once");
    }

    /// A caller header that disagrees with git's header is refused before any
    /// bytes are read, and the reader is restarted so it stays in step.
    #[test]
    fn a_header_that_disagrees_with_git_is_refused_and_the_reader_restarts() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let blob = git(root, &["rev-parse", "HEAD:main.orna"]);
        let algorithm = objects.algorithm().expect("the repository algorithm");
        let header = objects
            .info(&blob)
            .expect("info the blob")
            .expect("the blob exists");
        let mut wrong: super::ObjectHeader = header.clone();
        wrong.size += 1;
        assert!(objects.read_exact(&blob, &wrong, algorithm).is_err());
        assert_eq!(
            objects.read_blob(&blob, 1024).expect("restarted read"),
            b"pub fn main() {}\n"
        );
        assert_eq!(objects.process_starts(), 2, "the reader was restarted once");
    }

    /// A git process that dies after writing part of an object leaves the
    /// reader short of the declared size. The read reports the failure without
    /// hanging on the closed pipe, the broken process is discarded, and the
    /// next read starts a fresh one.
    #[test]
    fn a_truncated_object_stream_discards_the_session_and_the_next_read_restarts() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        let blob = git(root, &["rev-parse", "HEAD:main.orna"]);
        let algorithm = objects.algorithm().expect("the repository algorithm");
        let header = super::ObjectHeader {
            oid: blob.clone(),
            kind: ObjectKind::Blob,
            size: 20,
        };
        // The stub answers the `contents` request with a 20-byte header, writes
        // 5 bytes, and exits: the stream ends mid-object.
        let script = format!(r#"read line; printf '{blob} blob 20\n'; printf short; exit 0"#);
        let mut child = Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the stub reader");
        let input = child.stdin.take().expect("stub stdin");
        let output = std::io::BufReader::new(child.stdout.take().expect("stub stdout"));
        *objects.lock_session() = Some(super::Session {
            child,
            input: Some(input),
            output: Some(output),
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| sender.send(objects.read_exact(&blob, &header, algorithm).is_err()));
            let failed = receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a truncated read returns instead of hanging");
            assert!(failed, "a truncated object cannot be read exactly");
        });
        assert!(
            objects.process_id().is_none(),
            "the truncated process was discarded"
        );
        assert_eq!(
            objects.read_blob(&blob, 1024).expect("restarted read"),
            b"pub fn main() {}\n"
        );
        assert_eq!(
            objects.process_starts(),
            1,
            "the next read started a fresh process"
        );
    }

    #[test]
    fn a_failed_read_restarts_the_reader_without_desynchronising_it() {
        let (directory, repository) = repository();
        let objects = repository.objects();
        let tree = git(directory.path(), &["rev-parse", "HEAD^{tree}"]);
        let module = git(directory.path(), &["rev-parse", "HEAD:main.orna"]);

        assert!(
            objects.read_blob(&tree, 1024).is_err(),
            "a tree is not a blob"
        );
        assert!(
            objects.read_blob(&module, 4).is_err(),
            "the blob exceeds the bound"
        );
        assert!(
            objects.tree_entries(&module).is_err(),
            "a blob is not a tree"
        );
        assert!(
            objects.info("HEAD\nHEAD").is_err(),
            "one name per command line"
        );
        assert_eq!(
            objects.read_blob(&module, 64).expect("read after failures"),
            b"pub fn main() {}\n"
        );
        assert_eq!(
            objects
                .tree_entries(&tree)
                .expect("list after failures")
                .len(),
            5
        );
    }

    #[test]
    fn reads_objects_written_after_the_reader_started() {
        let (directory, repository) = repository();
        let root = directory.path();
        let objects = repository.objects();
        assert!(objects.info("HEAD").expect("start the reader").is_some());
        let process = objects.process_id();

        fs::write(root.join("added.orna"), "pub fn added() {}\n").expect("write module");
        git(root, &["add", "added.orna"]);
        git(
            root,
            &["commit", "--quiet", "--no-verify", "-m", "add module"],
        );
        let head = git(root, &["rev-parse", "HEAD"]);
        let added = objects
            .info(&format!("{head}:added.orna"))
            .expect("resolve the new module")
            .expect("a loose object written later is visible");
        assert_eq!(
            objects
                .read_blob(added.oid(), 64)
                .expect("read the new module"),
            b"pub fn added() {}\n"
        );

        git(root, &["gc", "--quiet", "--prune=now"]);
        assert_eq!(
            objects
                .read_blob(added.oid(), 64)
                .expect("read the module from its pack"),
            b"pub fn added() {}\n"
        );
        assert_eq!(
            objects.process_id(),
            process,
            "no read restarted the reader"
        );
    }

    /// The source listing is the recursive listing minus the store subtree,
    /// in the same order, and never reads a store node.
    #[test]
    fn source_listing_skips_only_the_store_and_reads_none_of_it() {
        let (directory, repository) = repository();
        let root = directory.path();
        let head = repository.head().expect("read HEAD").expect("HEAD is born");
        let complete = repository
            .list_committed_tree(&head, 64)
            .expect("list recursively");
        assert_eq!(
            repository
                .list_committed_source_tree(&head)
                .expect("list the source tree"),
            complete
        );
        assert!(complete.iter().any(|entry| {
            entry.path().as_path() == Path::new("lib/main.orna")
                && entry.kind() == CommittedTreeEntryKind::File { executable: false }
        }));

        let absent_node = "1".repeat(head.as_str().len());
        let store = git_with_input(
            root,
            &["mktree", "--missing"],
            &format!("040000 tree {absent_node}\tnode\n"),
        );
        let metadata = git_with_input(root, &["mktree"], &format!("040000 tree {store}\tstore\n"));
        let mut listing = git(root, &["ls-tree", "HEAD"]);
        listing.push_str(&format!("\n040000 tree {metadata}\t.orna\n"));
        let tree = git_with_input(root, &["mktree"], &listing);
        let commit = git(root, &["commit-tree", &tree, "-p", "HEAD", "-m", "store"]);
        git(root, &["update-ref", "HEAD", &commit]);
        let commit = repository
            .resolve_snapshot(&commit)
            .expect("resolve the new commit");

        assert!(
            repository.list_committed_tree(&commit, 64).is_err(),
            "the recursive listing reads the absent store node"
        );
        assert_eq!(
            repository
                .list_committed_source_tree(&commit)
                .expect("the source listing never enters the store"),
            complete
        );
    }

    /// Each level holds two entries naming the same next tree, so the listing
    /// of the root expands to `2^levels` leaves from `levels + 1` objects. The
    /// walk must refuse it before it materialises any of that expansion.
    #[test]
    fn source_listing_rejects_a_shared_subtree_bomb_before_expanding_it() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let mut tree = git_with_input(root, &["mktree"], &format!("100644 blob {leaf}\tf\n"));
        for _ in 0..30 {
            tree = git_with_input(
                root,
                &["mktree"],
                &format!("040000 tree {tree}\ta\n040000 tree {tree}\tb\n"),
            );
        }
        let commit = git(root, &["commit-tree", &tree, "-m", "tree bomb"]);
        git(root, &["update-ref", "HEAD", &commit]);
        let commit = repository
            .resolve_snapshot(&commit)
            .expect("resolve the bomb commit");
        assert!(matches!(
            repository.list_committed_source_tree(&commit),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// A chain nested beyond the depth limit is refused as well, even though
    /// it holds only one entry per level.
    #[test]
    fn source_listing_rejects_trees_nested_beyond_the_depth_limit() {
        let (directory, repository) = repository();
        let root = directory.path();
        let mut tree = git_with_input(root, &["mktree"], "");
        for _ in 0..300 {
            tree = git_with_input(root, &["mktree"], &format!("040000 tree {tree}\td\n"));
        }
        let commit = git(root, &["commit-tree", &tree, "-m", "deep chain"]);
        git(root, &["update-ref", "HEAD", &commit]);
        let commit = repository
            .resolve_snapshot(&commit)
            .expect("resolve the deep commit");
        assert!(matches!(
            repository.list_committed_source_tree(&commit),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// Commits `tree` as the new HEAD of the fixture repository and resolves
    /// it, so a test can list it by commit.
    fn commit_tree_as_head(
        directory: &Path,
        repository: &Repository,
        tree: &str,
        message: &str,
    ) -> GitCommitRef {
        let commit = git(directory, &["commit-tree", tree, "-m", message]);
        git(directory, &["update-ref", "HEAD", &commit]);
        repository
            .resolve_snapshot(&commit)
            .expect("resolve the fixture commit")
    }

    /// Two entries per level name the same next tree and the leaf is an empty
    /// tree, so `ls-tree -r` emits no leaf while still visiting every interior
    /// tree: 2^depth occurrences. A cap on listed leaves never trips on it, so
    /// the bound must count interior trees before git expands them.
    #[test]
    fn committed_tree_listing_refuses_an_empty_leaf_bomb_before_git_expands_it() {
        let (directory, repository) = repository();
        let root = directory.path();
        let mut tree = git_with_input(root, &["mktree"], "");
        for _ in 0..30 {
            tree = git_with_input(
                root,
                &["mktree"],
                &format!("040000 tree {tree}\ta\n040000 tree {tree}\tb\n"),
            );
        }
        let commit = commit_tree_as_head(root, &repository, &tree, "empty leaf bomb");
        assert!(matches!(
            repository.list_committed_tree(&commit, 64),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// A shared DAG whose expansion stays under the limit lists every
    /// occurrence: ten levels of two entries name 2^10 leaves.
    #[test]
    fn committed_tree_listing_lists_a_shared_dag_under_the_limit_in_full() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let mut tree = git_with_input(root, &["mktree"], &format!("100644 blob {leaf}\tf\n"));
        for _ in 0..10 {
            tree = git_with_input(
                root,
                &["mktree"],
                &format!("040000 tree {tree}\ta\n040000 tree {tree}\tb\n"),
            );
        }
        let commit = commit_tree_as_head(root, &repository, &tree, "shared dag");
        let listed = repository
            .list_committed_tree(&commit, 4096)
            .expect("list the shared DAG under the limit");
        assert_eq!(listed.len(), 1024);
    }

    /// Eighteen levels of two 255-byte names hold about 3 * 2^18 entries,
    /// under the entry cap, yet list gigabytes of paths. The path-byte budget
    /// must refuse them.
    #[test]
    fn committed_tree_listing_refuses_long_names_past_the_path_byte_budget() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let mut tree = git_with_input(root, &["mktree"], &format!("100644 blob {leaf}\tf\n"));
        let (long_a, long_b) = ("a".repeat(255), "b".repeat(255));
        for _ in 0..18 {
            tree = git_with_input(
                root,
                &["mktree"],
                &format!("040000 tree {tree}\t{long_a}\n040000 tree {tree}\t{long_b}\n"),
            );
        }
        let commit = commit_tree_as_head(root, &repository, &tree, "long names");
        assert!(matches!(
            repository.list_committed_tree(&commit, 1 << 20),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// A flat tree of 10,000 entries is far below every walk limit and lists
    /// in full.
    #[test]
    fn committed_tree_listing_accepts_a_flat_tree_of_ten_thousand_entries() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let listing = (0..10_000)
            .map(|index| format!("100644 blob {leaf}\tf{index:05}\n"))
            .collect::<String>();
        let tree = git_with_input(root, &["mktree"], &listing);
        let commit = commit_tree_as_head(root, &repository, &tree, "flat");
        let listed = repository
            .list_committed_tree(&commit, 10_000)
            .expect("list the flat tree");
        assert_eq!(listed.len(), 10_000);
    }

    /// A narrow chain nested 60 levels deep is well under the depth limit and
    /// lists its single file.
    #[test]
    fn committed_tree_listing_accepts_a_narrow_tree_sixty_levels_deep() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let mut tree = git_with_input(root, &["mktree"], &format!("100644 blob {leaf}\tf\n"));
        for _ in 0..60 {
            tree = git_with_input(root, &["mktree"], &format!("040000 tree {tree}\td\n"));
        }
        let commit = commit_tree_as_head(root, &repository, &tree, "deep narrow");
        let listed = repository
            .list_committed_tree(&commit, 64)
            .expect("list the narrow deep tree");
        assert_eq!(listed.len(), 1);
    }

    /// The materialised store is walked only by its own reader, which lists
    /// its ten thousand entries, so the store is accepted at that size and the
    /// source listing never enters it.
    #[test]
    fn large_orna_store_is_accepted_by_the_store_and_skipped_by_the_source_listing() {
        let (directory, repository) = repository();
        let root = directory.path();
        let leaf = git_with_input(root, &["hash-object", "-w", "--stdin"], "leaf\n");
        let listing = (0..10_000)
            .map(|index| format!("100644 blob {leaf}\tn{index:05}\n"))
            .collect::<String>();
        let store = git_with_input(root, &["mktree"], &listing);
        let metadata = git_with_input(root, &["mktree"], &format!("040000 tree {store}\tstore\n"));
        let mut listing = git(root, &["ls-tree", "HEAD"]);
        listing.push_str(&format!("\n040000 tree {metadata}\t.orna\n"));
        let tree = git_with_input(root, &["mktree"], &listing);
        let commit = commit_tree_as_head(root, &repository, &tree, "large store");
        repository
            .objects()
            .check_expansion(commit.as_str(), ".orna/store")
            .expect("a 10k-entry store is under every limit");
        repository
            .list_committed_source_tree(&commit)
            .expect("the source listing skips the store");
    }

    /// An absent path lists nothing and is accepted; a treeish that names no
    /// object fails closed.
    #[test]
    fn expansion_check_accepts_an_absent_path_and_fails_closed_on_a_missing_treeish() {
        let (directory, repository) = repository();
        let root = directory.path();
        let head = git(root, &["rev-parse", "HEAD"]);
        let objects = repository.objects();
        objects
            .check_expansion(&head, "no/such/path")
            .expect("an absent path lists nothing");
        assert!(matches!(
            objects.check_expansion("0123456789abcdef0123456789abcdef01234567", ""),
            Err(RepositoryError::GitOperationFailed)
        ));
    }
}
