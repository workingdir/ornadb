//! Allowlisted native filesystem operations collected into the sys host ABI.
//!
//! The default provider caps UTF-8 file reads/writes at 8 MiB and directory
//! listings at 4096 entries; hosts can choose tighter limits with `with_limits`.
//! Captures stream without holding the source in memory, so a host can raise
//! their quota separately with `with_capture_limit`.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const DEFAULT_MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 4096;

use orna_sys_macros::sys_host_operation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemProviderError {
    Denied,
    InvalidRequest,
    NotFound,
    AlreadyExists,
    NotFile,
    NotDirectory,
    InvalidUtf8,
    Unavailable,
    OutputLimit,
}

/// Stable, redacted outcomes for `sys.blob.capture_file`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureFileError {
    UnauthorizedRoot,
    InvalidPath,
    SourceUnavailable,
    LimitExceeded,
    SourceChanged,
}

impl CaptureFileError {
    /// The portable failure code of the system reference's failure table
    /// (ORNA-SYS-096/097). A root, path or target that is not an authorised
    /// regular safe input is `sys.blob.source_denied`; exceeding the capture
    /// quota is `sys.blob.limit`; a detected change is
    /// `sys.blob.source_changed` (ORNA-INGEST-002, ORNA-INGEST-003).
    pub const fn failure_code(self) -> &'static str {
        match self {
            Self::UnauthorizedRoot | Self::InvalidPath | Self::SourceUnavailable => {
                "sys.blob.source_denied"
            }
            Self::LimitExceeded => "sys.blob.limit",
            Self::SourceChanged => "sys.blob.source_changed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFilesystemMetadata {
    pub kind: String,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
}

impl FilesystemProviderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Denied => "sys.host.filesystem.denied",
            Self::InvalidRequest => "sys.host.filesystem.invalid_request",
            Self::NotFound => "sys.host.filesystem.not_found",
            Self::AlreadyExists => "sys.host.filesystem.already_exists",
            Self::NotFile => "sys.host.filesystem.not_file",
            Self::NotDirectory => "sys.host.filesystem.not_directory",
            Self::InvalidUtf8 => "sys.host.filesystem.invalid_utf8",
            Self::Unavailable => "sys.host.filesystem.unavailable",
            Self::OutputLimit => "sys.host.filesystem.output_limit",
        }
    }
}

/// Filesystem access restricted to exact host-approved directory roots.
///
/// `allow_root` opens each root once and keeps that directory handle for the
/// provider's lifetime. Every operation resolves its path from the held handle
/// one component at a time and acts on the final entry through its open parent
/// directory, so renaming the root or swapping a component for a symbolic link
/// can make an operation fail but cannot redirect it outside the root.
/// Absolute paths, parent traversal, symbolic links and Git metadata (`.git`,
/// in any letter case and at any depth) are refused; only `symlink_metadata`
/// reports a final link, without following it. Platforms without a
/// handle-relative implementation refuse every operation as unavailable.
#[derive(Clone, Debug)]
pub struct FilesystemProvider {
    roots: BTreeMap<PathBuf, Arc<File>>,
    max_text_bytes: usize,
    max_directory_entries: usize,
    max_capture_bytes: u64,
}

impl PartialEq for FilesystemProvider {
    fn eq(&self, other: &Self) -> bool {
        self.roots.keys().eq(other.roots.keys())
            && self.max_text_bytes == other.max_text_bytes
            && self.max_directory_entries == other.max_directory_entries
            && self.max_capture_bytes == other.max_capture_bytes
    }
}

impl Eq for FilesystemProvider {}

impl Default for FilesystemProvider {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
            max_capture_bytes: DEFAULT_MAX_TEXT_BYTES as u64,
        }
    }
}

impl FilesystemProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider with host-selected file and listing bounds. The
    /// capture quota starts equal to the file bound.
    pub fn with_limits(
        max_text_bytes: usize,
        max_directory_entries: usize,
    ) -> Result<Self, FilesystemProviderError> {
        if max_text_bytes == 0 || max_directory_entries == 0 {
            return Err(FilesystemProviderError::InvalidRequest);
        }
        Ok(Self {
            roots: BTreeMap::new(),
            max_text_bytes,
            max_directory_entries,
            max_capture_bytes: max_text_bytes as u64,
        })
    }

    /// Sets the largest source one capture may stream. A capture never holds
    /// its source in memory, so this quota may exceed the file bound that
    /// text reads and writes load into memory.
    pub fn with_capture_limit(
        mut self,
        max_capture_bytes: u64,
    ) -> Result<Self, FilesystemProviderError> {
        if max_capture_bytes == 0 {
            return Err(FilesystemProviderError::InvalidRequest);
        }
        self.max_capture_bytes = max_capture_bytes;
        Ok(self)
    }

    /// Adds one existing directory as an explicit filesystem capability.
    pub fn allow_root(&mut self, root: impl AsRef<Path>) -> Result<(), FilesystemProviderError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| FilesystemProviderError::InvalidRequest)?;
        let handle = File::open(&root).map_err(|_| FilesystemProviderError::InvalidRequest)?;
        if !handle
            .metadata()
            .map_err(|_| FilesystemProviderError::InvalidRequest)?
            .is_dir()
            || self.roots.contains_key(&root)
        {
            return Err(FilesystemProviderError::InvalidRequest);
        }
        self.roots.insert(root, Arc::new(handle));
        Ok(())
    }

    /// Finds the handle held for a host-approved root, named by its canonical
    /// path or by the exact path under which it was approved. Operations use
    /// the held handle, never the name, so replacing the directory at that
    /// path later does not change what they reach.
    fn held_root(&self, root: &str) -> Option<&Arc<File>> {
        let requested = Path::new(root);
        requested
            .canonicalize()
            .ok()
            .and_then(|canonical| self.roots.get(&canonical))
            .or_else(|| self.roots.get(requested))
    }

    fn authorized_root(&self, root: &str) -> Result<&File, FilesystemProviderError> {
        self.held_root(root)
            .map(Arc::as_ref)
            .ok_or(FilesystemProviderError::Denied)
    }

    fn authorized_root_handle(&self, root: &str) -> Result<Arc<File>, CaptureFileError> {
        self.held_root(root)
            .map(Arc::clone)
            .ok_or(CaptureFileError::UnauthorizedRoot)
    }

    /// Opens one bounded regular file beneath a held, host-authorised root for
    /// streaming. Reading the returned stream never retains source bytes, and
    /// it can verify that both the open inode and the current path still match
    /// their initial identity, size and modification time.
    ///
    /// The source must fit both `max_bytes` and the provider limit; a
    /// `max_bytes` above the provider limit is not itself a failure, because
    /// the contract bounds the source and the captured Blob, not the request.
    #[sys_host_operation(
        r###"{"name":"sys.blob.capture_file","version":{"major":1,"minor":0},"signature":"fn sys.blob.capture_file(root: Str, path: Str, max_bytes: Int): Blob","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and contains no parent traversal or symbolic-link component","target is a regular file whose identity, size and modification time remain stable during capture","the source and captured Blob fit within max_bytes and the provider limit"],"failures":["sys.blob.source_denied","sys.blob.bounds","sys.blob.limit","sys.blob.source_changed"],"role":"host.sys.blob.capture@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"capture_file"}"###
    )]
    pub fn capture_file(
        &self,
        root: &str,
        path: &str,
        max_bytes: u64,
    ) -> Result<CaptureFileStream, CaptureFileError> {
        let root_handle = self.authorized_root_handle(root)?;
        let components = capture_components(path)?;
        let file = beneath::open_capture_source(&root_handle, &components)?;
        let metadata = file
            .metadata()
            .map_err(|_| CaptureFileError::SourceUnavailable)?;
        if !metadata.is_file() {
            return Err(CaptureFileError::SourceUnavailable);
        }
        let before = FileSnapshot::from_metadata(&metadata)?;
        let max_bytes = max_bytes.min(self.max_capture_bytes);
        if before.size > max_bytes {
            return Err(CaptureFileError::LimitExceeded);
        }
        Ok(CaptureFileStream {
            file,
            root: root_handle,
            components,
            before,
            max_bytes,
            bytes_read: 0,
            exceeded: false,
        })
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.read_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.read_text(root: Str, path: Str): Str","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","target is a regular file","file contents are valid UTF-8 and within the provider byte limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.invalid_utf8","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"read_text"}"###
    )]
    pub fn read_text(&self, root: &str, path: &str) -> Result<String, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::read_text(root, RootedPath::parse(path)?, self.max_text_bytes)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.write_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.write_text(root: Str, path: Str, contents: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","parent directory exists","contents are within the provider byte limit","overwrite is true only for an existing regular file"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"write_text"}"###
    )]
    pub fn write_text(
        &self,
        root: &str,
        path: &str,
        contents: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        if contents.len() > self.max_text_bytes {
            return Err(FilesystemProviderError::OutputLimit);
        }
        beneath::write_text(root, RootedPath::parse(path)?, contents, overwrite)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.append_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.append_text(root: Str, path: Str, contents: Str): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","parent directory exists","resulting file is within the provider byte limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"append_text"}"###
    )]
    pub fn append_text(
        &self,
        root: &str,
        path: &str,
        contents: &str,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        if contents.len() > self.max_text_bytes {
            return Err(FilesystemProviderError::OutputLimit);
        }
        beneath::append_text(
            root,
            RootedPath::parse(path)?,
            contents,
            self.max_text_bytes,
        )
    }

    /// Reports whether `path` names an entry. Nothing below a missing
    /// directory, or below an entry that is not a directory, exists.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.exists","version":{"major":1,"minor":0},"signature":"fn std.io.fs.exists(root: Str, path: Str): Bool","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"exists"}"###
    )]
    pub fn exists(&self, root: &str, path: &str) -> Result<bool, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::exists(root, RootedPath::parse(path)?)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.is_directory","version":{"major":1,"minor":0},"signature":"fn std.io.fs.is_directory(root: Str, path: Str): Bool","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"is_directory"}"###
    )]
    pub fn is_directory(&self, root: &str, path: &str) -> Result<bool, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::is_directory(root, RootedPath::parse(path)?)
    }

    /// Lists a directory's entry names in sorted order, omitting Git metadata.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.list","version":{"major":1,"minor":0},"signature":"fn std.io.fs.list(root: Str, path: Str): [Str]","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves to a directory beneath the held root without parent traversal, symbolic links or .git metadata","all child names other than .git metadata are Unicode and within the provider entry limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_directory","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"list"}"###
    )]
    pub fn list(&self, root: &str, path: &str) -> Result<Vec<String>, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::list(root, RootedPath::parse(path)?, self.max_directory_entries)
    }

    /// Reports the entry `path` names. A symbolic link is refused rather than
    /// followed; `symlink_metadata` describes the link itself.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.metadata","version":{"major":1,"minor":0},"signature":"fn std.io.fs.metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"metadata"}"###
    )]
    pub fn metadata(
        &self,
        root: &str,
        path: &str,
    ) -> Result<HostFilesystemMetadata, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let metadata = beneath::entry_metadata(root, RootedPath::parse(path)?)?;
        if metadata.file_type().is_symlink() {
            return Err(FilesystemProviderError::Denied);
        }
        Ok(metadata_value(&metadata, false))
    }

    /// Reports the entry `path` names, describing a final symbolic link itself
    /// without following it.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.symlink_metadata","version":{"major":1,"minor":0},"signature":"fn std.io.fs.symlink_metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative, names no .git metadata, and its parent components resolve beneath the held root without parent traversal or symbolic links","final symbolic link is reported without following it"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"symlink_metadata"}"###
    )]
    pub fn symlink_metadata(
        &self,
        root: &str,
        path: &str,
    ) -> Result<HostFilesystemMetadata, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let metadata = beneath::entry_metadata(root, RootedPath::parse(path)?)?;
        Ok(metadata_value(&metadata, metadata.file_type().is_symlink()))
    }

    /// Creates the directory `path` names. With `parents`, missing ancestors
    /// are created too; an existing directory at `path` itself is accepted only
    /// with `exist_ok`.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.create_dir","version":{"major":1,"minor":0},"signature":"fn std.io.fs.create_dir(root: Str, path: Str, parents: Bool, exist_ok: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","parent directory exists unless parents is true","existing directory is accepted only when exist_ok is true"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.already_exists","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"create_dir"}"###
    )]
    pub fn create_dir(
        &self,
        root: &str,
        path: &str,
        parents: bool,
        exist_ok: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::create_dir(root, RootedPath::parse(path)?, parents, exist_ok)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.remove_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.remove_file(root: Str, path: Str): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","target is not a directory"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"remove_file"}"###
    )]
    pub fn remove_file(&self, root: &str, path: &str) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        beneath::remove_file(root, RootedPath::parse(path)?)
    }

    /// Copies one regular file's bytes and permission bits. Copying a file
    /// onto another name for the same file leaves it unchanged.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.copy_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.copy_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","source and destination are relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","source is a regular file within the provider byte limit","overwrite is true only for an existing regular-file destination"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"copy_file"}"###
    )]
    pub fn copy_file(
        &self,
        root: &str,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let source = RootedPath::parse(source_path)?;
        let destination = RootedPath::parse(destination_path)?;
        beneath::copy_file(root, source, destination, overwrite, self.max_text_bytes)
    }

    /// Renames one regular file. Without `overwrite` the destination name is
    /// never replaced, even by a file created concurrently.
    #[sys_host_operation(
        r###"{"name":"std.io.fs.move_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.move_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","source and destination are relative and each component resolves beneath the held root without parent traversal, symbolic links or .git metadata","source is a regular file","overwrite is true only for an existing regular-file destination"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"move_file"}"###
    )]
    pub fn move_file(
        &self,
        root: &str,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let source = RootedPath::parse(source_path)?;
        let destination = RootedPath::parse(destination_path)?;
        beneath::move_file(root, source, destination, overwrite)
    }
}

/// A file opened beneath a held filesystem root for bounded streaming.
/// Reading it never retains source bytes in memory.
pub struct CaptureFileStream {
    file: File,
    root: Arc<File>,
    components: Vec<OsString>,
    before: FileSnapshot,
    max_bytes: u64,
    bytes_read: u64,
    exceeded: bool,
}

impl CaptureFileStream {
    pub const fn length(&self) -> u64 {
        self.before.size
    }

    pub const fn exceeded(&self) -> bool {
        self.exceeded
    }

    /// Verifies the opened inode and current path entry still match their
    /// pre-read identity, length and modification time.
    pub fn verify_unchanged(&self) -> Result<(), CaptureFileError> {
        let opened = self
            .file
            .metadata()
            .map_err(|_| CaptureFileError::SourceChanged)?;
        let opened =
            FileSnapshot::from_metadata(&opened).map_err(|_| CaptureFileError::SourceChanged)?;
        let current = beneath::open_capture_source(&self.root, &self.components)
            .map_err(|_| CaptureFileError::SourceChanged)?;
        let current = current
            .metadata()
            .map_err(|_| CaptureFileError::SourceChanged)?;
        let current =
            FileSnapshot::from_metadata(&current).map_err(|_| CaptureFileError::SourceChanged)?;
        if opened != self.before || current != self.before {
            return Err(CaptureFileError::SourceChanged);
        }
        Ok(())
    }
}

impl Read for CaptureFileStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.exceeded {
            return Ok(0);
        }
        let remaining = self.max_bytes.saturating_sub(self.bytes_read);
        let allowed = remaining.saturating_add(1);
        let read_len = output
            .len()
            .min(usize::try_from(allowed).unwrap_or(usize::MAX));
        let count = self.file.read(&mut output[..read_len])?;
        if count == 0 {
            return Ok(0);
        }
        self.bytes_read = self.bytes_read.saturating_add(count as u64);
        self.exceeded = self.bytes_read > self.max_bytes;
        Ok(count)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshot {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    size: u64,
    modified: SystemTime,
}

impl FileSnapshot {
    fn from_metadata(metadata: &fs::Metadata) -> Result<Self, CaptureFileError> {
        Ok(Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|_| CaptureFileError::SourceUnavailable)?,
        })
    }
}

fn capture_components(path: &str) -> Result<Vec<OsString>, CaptureFileError> {
    if path.is_empty() || path.contains('\0') {
        return Err(CaptureFileError::InvalidPath);
    }
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => components.push(part.to_os_string()),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(CaptureFileError::InvalidPath);
            }
        }
    }
    if components.is_empty() {
        return Err(CaptureFileError::InvalidPath);
    }
    Ok(components)
}

/// A validated `std.io.fs` path beneath one held root. Parsing refuses
/// absolute paths, parent traversal and Git metadata before any I/O, so only
/// ordinary names remain; `.` components are skipped, and a path with no names
/// left denotes the root directory itself.
#[derive(Clone, Copy)]
struct RootedPath<'a>(&'a Path);

impl<'a> RootedPath<'a> {
    fn parse(path: &'a str) -> Result<Self, FilesystemProviderError> {
        if path.contains('\0') {
            return Err(FilesystemProviderError::InvalidRequest);
        }
        let path = Path::new(path);
        for component in path.components() {
            match component {
                Component::Normal(name) if !is_git_metadata(name.as_encoded_bytes()) => {}
                Component::CurDir => {}
                Component::Normal(_)
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => return Err(FilesystemProviderError::Denied),
            }
        }
        Ok(Self(path))
    }

    /// The path's names in order from the root.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn components(self) -> impl DoubleEndedIterator<Item = &'a OsStr> {
        self.0.components().filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
    }
}

/// Git's metadata directory, or a linked worktree's `.git` file, is never part
/// of a granted root. Every letter case is refused, as Git itself refuses
/// them, because case-insensitive filesystems treat them as one name.
fn is_git_metadata(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b".git")
}

/// Handle-relative implementations of every filesystem operation.
///
/// A path is resolved one component at a time from the root handle held since
/// `allow_root`: each parent is opened with `O_DIRECTORY | O_NOFOLLOW`, and the
/// final entry is inspected, opened, created, renamed or removed through its
/// open parent with the `*at` system calls, never following a link. A
/// concurrent rename or a planted symbolic link can therefore make an
/// operation fail, but cannot redirect it outside the root.
#[cfg(target_os = "linux")]
mod beneath {
    use std::{
        ffi::{OsStr, OsString},
        fs::{self, File},
        io::{self, Read, Write},
        os::fd::{AsFd, BorrowedFd, OwnedFd},
    };

    use rustix::{
        fs::{AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat, StatExt},
        io::Errno,
    };

    use super::{CaptureFileError, FilesystemProviderError, RootedPath, is_git_metadata};

    /// Opens one directory component, never through a link.
    const DIRECTORY: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    /// Opens a final regular file for reading. Its type is checked before it
    /// is opened; `NONBLOCK` and `NOCTTY` stop an entry swapped for a FIFO or a
    /// terminal in between from blocking or becoming the controlling terminal.
    const READ: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOCTTY)
        .union(OFlags::CLOEXEC);
    /// Opens or creates a final regular file for writing, with the same guards.
    const WRITE: OFlags = OFlags::WRONLY
        .union(OFlags::CREATE)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOCTTY)
        .union(OFlags::CLOEXEC);
    /// Names an entry, even a link, without opening it.
    const INSPECT: OFlags = OFlags::PATH.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
    /// Permission bits for new files before the umask, as `std::fs` uses.
    pub(super) const FILE_MODE: Mode = Mode::from_raw_mode(0o666);
    /// Permission bits for new directories before the umask, as `std::fs` uses.
    const DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o777);

    /// A directory reached beneath a held root: the root handle itself, or a
    /// descendant opened from it without following links.
    pub(super) enum Directory<'root> {
        Root(&'root File),
        Child(OwnedFd),
    }

    impl AsFd for Directory<'_> {
        fn as_fd(&self) -> BorrowedFd<'_> {
            match self {
                Self::Root(root) => root.as_fd(),
                Self::Child(directory) => directory.as_fd(),
            }
        }
    }

    /// Why a walk stopped before reaching a directory.
    pub(super) enum WalkError {
        /// A component is a symbolic link, which a walk never follows.
        Symlink,
        /// A component exists but is not a directory.
        NotDirectory,
        /// The operating system refused or failed the step.
        Os(Errno),
    }

    /// Opens every component in turn as a directory beneath `root`.
    pub(super) fn walk<'root, 'path>(
        root: &'root File,
        components: impl IntoIterator<Item = &'path OsStr>,
    ) -> Result<Directory<'root>, WalkError> {
        let mut directory = Directory::Root(root);
        for component in components {
            directory = Directory::Child(open_directory(&directory, component)?);
        }
        Ok(directory)
    }

    /// Opens `name` in `parent` as a directory without following a link.
    fn open_directory(parent: impl AsFd, name: &OsStr) -> Result<OwnedFd, WalkError> {
        rustix::fs::openat(&parent, name, DIRECTORY, Mode::empty()).map_err(|error| {
            // With `O_DIRECTORY | O_NOFOLLOW` a link fails like any other
            // non-directory, so look at the entry itself to tell them apart.
            if error == Errno::NOTDIR || error == Errno::LOOP {
                match rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(status) if FileType::from_raw_mode(status.st_mode) == FileType::Symlink => {
                        WalkError::Symlink
                    }
                    Ok(_) => WalkError::NotDirectory,
                    Err(error) => WalkError::Os(error),
                }
            } else {
                WalkError::Os(error)
            }
        })
    }

    /// Walks to the parent of the entry `path` names. The root itself is a
    /// directory, so an operation that needs an entry below it fails as
    /// `NotFile` when given the root.
    fn parent_and_name<'root, 'path>(
        root: &'root File,
        path: RootedPath<'path>,
    ) -> Result<(Directory<'root>, &'path OsStr), FilesystemProviderError> {
        let mut components = path.components();
        let name = components
            .next_back()
            .ok_or(FilesystemProviderError::NotFile)?;
        Ok((walk(root, components).map_err(walk_failure)?, name))
    }

    /// Why a final entry could not be opened as a regular file.
    enum RegularError {
        Symlink,
        NotRegular,
        Os(Errno),
    }

    /// Opens the regular file `name` in `parent` for reading. The entry is
    /// inspected first so a device, FIFO or socket is never opened; the status
    /// seen before and after opening is returned so callers can detect a swap
    /// in between.
    fn open_regular(parent: impl AsFd, name: &OsStr) -> Result<(File, Stat, Stat), RegularError> {
        let before = rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(RegularError::Os)?;
        match FileType::from_raw_mode(before.st_mode) {
            FileType::RegularFile => {}
            FileType::Symlink => return Err(RegularError::Symlink),
            _ => return Err(RegularError::NotRegular),
        }
        let file = File::from(
            rustix::fs::openat(&parent, name, READ, Mode::empty()).map_err(RegularError::Os)?,
        );
        let opened = rustix::fs::fstat(&file).map_err(RegularError::Os)?;
        Ok((file, before, opened))
    }

    /// Opens the existing regular file `path` names for reading.
    fn open_file(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<(File, Stat), FilesystemProviderError> {
        let (parent, name) = parent_and_name(root, path)?;
        let (file, _, opened) = open_regular(&parent, name).map_err(|error| match error {
            RegularError::Symlink => FilesystemProviderError::Denied,
            RegularError::NotRegular => FilesystemProviderError::NotFile,
            RegularError::Os(error) => filesystem_failure(error),
        })?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
            return Err(FilesystemProviderError::NotFile);
        }
        Ok((file, opened))
    }

    /// What a write does when a regular file already has its name.
    #[derive(Clone, Copy)]
    pub(super) enum Existing {
        /// Fail with `AlreadyExists`: creation is exclusive.
        Refuse,
        /// Open it for writing; the caller truncates it once that is safe.
        Open,
        /// Open it for appending.
        Append,
    }

    /// Opens `name` in `parent` for writing, creating it when absent. An
    /// existing link is refused without being followed, and any other
    /// non-regular entry is refused before it is opened.
    pub(super) fn open_writable(
        parent: impl AsFd,
        name: &OsStr,
        existing: Existing,
        mode: Mode,
    ) -> Result<(File, Stat), FilesystemProviderError> {
        match rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(status) => match FileType::from_raw_mode(status.st_mode) {
                FileType::Symlink => return Err(FilesystemProviderError::Denied),
                FileType::RegularFile if matches!(existing, Existing::Refuse) => {
                    return Err(FilesystemProviderError::AlreadyExists);
                }
                FileType::RegularFile => {}
                _ => return Err(FilesystemProviderError::NotFile),
            },
            Err(Errno::NOENT) => {}
            Err(error) => return Err(filesystem_failure(error)),
        }
        let flags = match existing {
            Existing::Refuse => WRITE.union(OFlags::EXCL),
            Existing::Open => WRITE,
            Existing::Append => WRITE.union(OFlags::APPEND),
        };
        let file =
            File::from(rustix::fs::openat(&parent, name, flags, mode).map_err(filesystem_failure)?);
        let opened = rustix::fs::fstat(&file).map_err(filesystem_failure)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
            return Err(FilesystemProviderError::NotFile);
        }
        Ok((file, opened))
    }

    /// Empties a regular file opened for writing; a new file needs nothing.
    fn truncate(file: &File, status: &Stat) -> Result<(), FilesystemProviderError> {
        if status.st_size == 0 {
            return Ok(());
        }
        rustix::fs::ftruncate(file, 0).map_err(filesystem_failure)
    }

    pub(super) fn read_text(
        root: &File,
        path: RootedPath<'_>,
        max_bytes: usize,
    ) -> Result<String, FilesystemProviderError> {
        let (file, status) = open_file(root, path)?;
        let limit = max_bytes.saturating_add(1);
        let mut bytes = Vec::with_capacity(usize::try_from(status.st_size).unwrap_or(0).min(limit));
        file.take(limit as u64)
            .read_to_end(&mut bytes)
            .map_err(io_failure)?;
        if bytes.len() > max_bytes {
            return Err(FilesystemProviderError::OutputLimit);
        }
        String::from_utf8(bytes).map_err(|_| FilesystemProviderError::InvalidUtf8)
    }

    pub(super) fn write_text(
        root: &File,
        path: RootedPath<'_>,
        contents: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let (parent, name) = parent_and_name(root, path)?;
        let existing = if overwrite {
            Existing::Open
        } else {
            Existing::Refuse
        };
        let (mut file, status) = open_writable(&parent, name, existing, FILE_MODE)?;
        truncate(&file, &status)?;
        file.write_all(contents.as_bytes()).map_err(io_failure)
    }

    pub(super) fn append_text(
        root: &File,
        path: RootedPath<'_>,
        contents: &str,
        max_bytes: usize,
    ) -> Result<(), FilesystemProviderError> {
        let (parent, name) = parent_and_name(root, path)?;
        let (mut file, status) = open_writable(&parent, name, Existing::Append, FILE_MODE)?;
        if size(&status).saturating_add(contents.len() as u64) > max_bytes as u64 {
            return Err(FilesystemProviderError::OutputLimit);
        }
        file.write_all(contents.as_bytes()).map_err(io_failure)
    }

    /// The type of the entry `path` names, read without following a link.
    /// `None` means nothing is there, including below a missing directory or
    /// below an entry that is not a directory.
    fn entry_type(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<Option<FileType>, FilesystemProviderError> {
        let mut components = path.components();
        let Some(name) = components.next_back() else {
            return Ok(Some(FileType::Directory));
        };
        let parent = match walk(root, components) {
            Ok(parent) => parent,
            Err(WalkError::NotDirectory | WalkError::Os(Errno::NOENT)) => return Ok(None),
            Err(error) => return Err(walk_failure(error)),
        };
        match rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(status) => match FileType::from_raw_mode(status.st_mode) {
                FileType::Symlink => Err(FilesystemProviderError::Denied),
                kind => Ok(Some(kind)),
            },
            Err(Errno::NOENT) => Ok(None),
            Err(error) => Err(filesystem_failure(error)),
        }
    }

    pub(super) fn exists(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<bool, FilesystemProviderError> {
        entry_type(root, path).map(|kind| kind.is_some())
    }

    pub(super) fn is_directory(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<bool, FilesystemProviderError> {
        entry_type(root, path).map(|kind| kind == Some(FileType::Directory))
    }

    pub(super) fn list(
        root: &File,
        path: RootedPath<'_>,
        max_entries: usize,
    ) -> Result<Vec<String>, FilesystemProviderError> {
        let directory = match walk(root, path.components()).map_err(walk_failure)? {
            // Reading entries advances a descriptor's offset, so the shared
            // root handle is reopened rather than read directly.
            Directory::Root(root) => rustix::fs::openat(root, c".", DIRECTORY, Mode::empty())
                .map_err(filesystem_failure)?,
            Directory::Child(directory) => directory,
        };
        let mut entries = Dir::new(directory).map_err(filesystem_failure)?;
        let mut names = Vec::new();
        while let Some(entry) = entries.read() {
            let entry = entry.map_err(filesystem_failure)?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") || is_git_metadata(name) {
                continue;
            }
            if names.len() == max_entries {
                return Err(FilesystemProviderError::OutputLimit);
            }
            let name =
                std::str::from_utf8(name).map_err(|_| FilesystemProviderError::InvalidRequest)?;
            names.push(name.to_owned());
        }
        names.sort();
        Ok(names)
    }

    /// Reads the status of the entry `path` names without following a final
    /// link. An `O_PATH` descriptor names the entry without opening it, so a
    /// device or FIFO is never opened just to be inspected.
    pub(super) fn entry_metadata(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<fs::Metadata, FilesystemProviderError> {
        let mut components = path.components();
        let Some(name) = components.next_back() else {
            return root.metadata().map_err(io_failure);
        };
        let parent = walk(root, components).map_err(walk_failure)?;
        let entry = rustix::fs::openat(&parent, name, INSPECT, Mode::empty())
            .map_err(filesystem_failure)?;
        File::from(entry).metadata().map_err(io_failure)
    }

    pub(super) fn create_dir(
        root: &File,
        path: RootedPath<'_>,
        parents: bool,
        exist_ok: bool,
    ) -> Result<(), FilesystemProviderError> {
        let mut components = path.components();
        let Some(name) = components.next_back() else {
            // The root is always an existing directory.
            return if exist_ok {
                Ok(())
            } else {
                Err(FilesystemProviderError::AlreadyExists)
            };
        };
        let mut directory = Directory::Root(root);
        for component in components {
            let child = match open_directory(&directory, component) {
                Err(WalkError::Os(Errno::NOENT)) if parents => {
                    match rustix::fs::mkdirat(&directory, component, DIRECTORY_MODE) {
                        // A concurrent creator is fine; the reopen below still
                        // refuses anything that is not a directory.
                        Ok(()) | Err(Errno::EXIST) => open_directory(&directory, component),
                        Err(error) => Err(WalkError::Os(error)),
                    }
                }
                opened => opened,
            };
            directory = Directory::Child(child.map_err(walk_failure)?);
        }
        match rustix::fs::mkdirat(&directory, name, DIRECTORY_MODE) {
            Ok(()) => Ok(()),
            Err(Errno::EXIST) => {
                let existing = rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(filesystem_failure)?;
                match FileType::from_raw_mode(existing.st_mode) {
                    FileType::Symlink => Err(FilesystemProviderError::Denied),
                    FileType::Directory if exist_ok => Ok(()),
                    _ => Err(FilesystemProviderError::AlreadyExists),
                }
            }
            Err(error) => Err(filesystem_failure(error)),
        }
    }

    pub(super) fn remove_file(
        root: &File,
        path: RootedPath<'_>,
    ) -> Result<(), FilesystemProviderError> {
        let (parent, name) = parent_and_name(root, path)?;
        let status = rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(filesystem_failure)?;
        match FileType::from_raw_mode(status.st_mode) {
            FileType::Symlink => Err(FilesystemProviderError::Denied),
            FileType::Directory => Err(FilesystemProviderError::NotFile),
            _ => rustix::fs::unlinkat(&parent, name, AtFlags::empty()).map_err(filesystem_failure),
        }
    }

    pub(super) fn copy_file(
        root: &File,
        source: RootedPath<'_>,
        destination: RootedPath<'_>,
        overwrite: bool,
        max_bytes: usize,
    ) -> Result<(), FilesystemProviderError> {
        let (source, status) = open_file(root, source)?;
        if size(&status) > max_bytes as u64 {
            return Err(FilesystemProviderError::OutputLimit);
        }
        let (parent, name) = parent_and_name(root, destination)?;
        // Like `std::fs::copy`, the copy takes the source's permission bits.
        let permissions = Mode::from_raw_mode(status.st_mode);
        let existing = if overwrite {
            Existing::Open
        } else {
            Existing::Refuse
        };
        let (mut target, opened) = open_writable(&parent, name, existing, permissions)?;
        if (opened.st_dev, opened.st_ino) == (status.st_dev, status.st_ino) {
            // Both names reach one file, which already holds the source bytes;
            // truncating it first would destroy them.
            return Ok(());
        }
        // Permissions change first so a destination owned by another user is
        // refused before its bytes are discarded.
        rustix::fs::fchmod(&target, permissions).map_err(filesystem_failure)?;
        truncate(&target, &opened)?;
        let limit = (max_bytes as u64).saturating_add(1);
        let copied = io::copy(&mut (&source).take(limit), &mut target).map_err(io_failure)?;
        if copied > max_bytes as u64 {
            return Err(FilesystemProviderError::OutputLimit);
        }
        Ok(())
    }

    pub(super) fn move_file(
        root: &File,
        source: RootedPath<'_>,
        destination: RootedPath<'_>,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let (source_parent, source_name) = parent_and_name(root, source)?;
        let status = rustix::fs::statat(&source_parent, source_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(filesystem_failure)?;
        match FileType::from_raw_mode(status.st_mode) {
            FileType::RegularFile => {}
            FileType::Symlink => return Err(FilesystemProviderError::Denied),
            _ => return Err(FilesystemProviderError::NotFile),
        }
        let (target_parent, target_name) = parent_and_name(root, destination)?;
        let replace =
            match rustix::fs::statat(&target_parent, target_name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(existing) => match FileType::from_raw_mode(existing.st_mode) {
                    FileType::Symlink => return Err(FilesystemProviderError::Denied),
                    FileType::RegularFile if overwrite => true,
                    FileType::RegularFile => return Err(FilesystemProviderError::AlreadyExists),
                    _ => return Err(FilesystemProviderError::NotFile),
                },
                Err(Errno::NOENT) => overwrite,
                Err(error) => return Err(filesystem_failure(error)),
            };
        if replace {
            rustix::fs::renameat(&source_parent, source_name, &target_parent, target_name)
                .map_err(filesystem_failure)
        } else {
            rename_without_replacing(&source_parent, source_name, &target_parent, target_name)
        }
    }

    /// Renames without ever replacing an existing destination. A filesystem
    /// that cannot refuse replacement atomically (`RENAME_NOREPLACE`) gets an
    /// exclusive hard link followed by removal of the old name instead.
    fn rename_without_replacing(
        source_parent: impl AsFd,
        source_name: &OsStr,
        target_parent: impl AsFd,
        target_name: &OsStr,
    ) -> Result<(), FilesystemProviderError> {
        match rustix::fs::renameat_with(
            &source_parent,
            source_name,
            &target_parent,
            target_name,
            RenameFlags::NOREPLACE,
        ) {
            Err(Errno::INVAL | Errno::NOSYS) => {
                link_then_unlink(source_parent, source_name, target_parent, target_name)
            }
            result => result.map_err(filesystem_failure),
        }
    }

    /// Gives a file a new name that must not exist yet, then drops the old one.
    pub(super) fn link_then_unlink(
        source_parent: impl AsFd,
        source_name: &OsStr,
        target_parent: impl AsFd,
        target_name: &OsStr,
    ) -> Result<(), FilesystemProviderError> {
        rustix::fs::linkat(
            &source_parent,
            source_name,
            &target_parent,
            target_name,
            AtFlags::empty(),
        )
        .map_err(filesystem_failure)?;
        rustix::fs::unlinkat(&source_parent, source_name, AtFlags::empty())
            .map_err(filesystem_failure)
    }

    /// Opens a capture source: a regular file whose identity, size and
    /// modification time did not change between inspection and opening.
    pub(super) fn open_capture_source(
        root: &File,
        components: &[OsString],
    ) -> Result<File, CaptureFileError> {
        let (name, parents) = components
            .split_last()
            .ok_or(CaptureFileError::InvalidPath)?;
        let parent =
            walk(root, parents.iter().map(OsString::as_os_str)).map_err(|error| match error {
                WalkError::Symlink | WalkError::NotDirectory => CaptureFileError::InvalidPath,
                WalkError::Os(error) => capture_failure(error),
            })?;
        let (file, before, opened) = open_regular(&parent, name).map_err(|error| match error {
            RegularError::Symlink => CaptureFileError::InvalidPath,
            RegularError::NotRegular => CaptureFileError::SourceUnavailable,
            RegularError::Os(error) => capture_failure(error),
        })?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
            || before.st_dev != opened.st_dev
            || before.st_ino != opened.st_ino
            || before.st_size != opened.st_size
            || before.mtime() != opened.mtime()
            || before.st_mtime_nsec != opened.st_mtime_nsec
        {
            return Err(CaptureFileError::SourceChanged);
        }
        Ok(file)
    }

    fn walk_failure(error: WalkError) -> FilesystemProviderError {
        match error {
            WalkError::Symlink => FilesystemProviderError::Denied,
            WalkError::NotDirectory => FilesystemProviderError::NotDirectory,
            WalkError::Os(error) => filesystem_failure(error),
        }
    }

    fn filesystem_failure(error: Errno) -> FilesystemProviderError {
        match error {
            Errno::NOENT => FilesystemProviderError::NotFound,
            Errno::EXIST => FilesystemProviderError::AlreadyExists,
            // `O_NOFOLLOW` reports a final symbolic link as `ELOOP`.
            Errno::ACCESS | Errno::PERM | Errno::LOOP => FilesystemProviderError::Denied,
            // A directory, or a FIFO or socket without a peer, is not a file.
            Errno::ISDIR | Errno::NXIO => FilesystemProviderError::NotFile,
            Errno::NOTDIR => FilesystemProviderError::NotDirectory,
            Errno::NAMETOOLONG => FilesystemProviderError::InvalidRequest,
            _ => FilesystemProviderError::Unavailable,
        }
    }

    fn io_failure(error: io::Error) -> FilesystemProviderError {
        error
            .raw_os_error()
            .map_or(FilesystemProviderError::Unavailable, |code| {
                filesystem_failure(Errno::from_raw_os_error(code))
            })
    }

    fn capture_failure(error: Errno) -> CaptureFileError {
        if error == Errno::LOOP || error == Errno::NOTDIR {
            CaptureFileError::InvalidPath
        } else {
            CaptureFileError::SourceUnavailable
        }
    }

    fn size(status: &Stat) -> u64 {
        u64::try_from(status.st_size).unwrap_or(u64::MAX)
    }
}

/// No handle-relative implementation exists for this platform. Resolving host
/// paths by name could be redirected by a concurrent rename or a planted link,
/// so every operation refuses with a typed outcome instead.
#[cfg(not(target_os = "linux"))]
mod beneath {
    use std::{
        ffi::OsString,
        fs::{self, File},
    };

    use super::{CaptureFileError, FilesystemProviderError, RootedPath};

    const REFUSED: FilesystemProviderError = FilesystemProviderError::Unavailable;

    pub(super) fn read_text(
        _: &File,
        _: RootedPath<'_>,
        _: usize,
    ) -> Result<String, FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn write_text(
        _: &File,
        _: RootedPath<'_>,
        _: &str,
        _: bool,
    ) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn append_text(
        _: &File,
        _: RootedPath<'_>,
        _: &str,
        _: usize,
    ) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn exists(_: &File, _: RootedPath<'_>) -> Result<bool, FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn is_directory(
        _: &File,
        _: RootedPath<'_>,
    ) -> Result<bool, FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn list(
        _: &File,
        _: RootedPath<'_>,
        _: usize,
    ) -> Result<Vec<String>, FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn entry_metadata(
        _: &File,
        _: RootedPath<'_>,
    ) -> Result<fs::Metadata, FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn create_dir(
        _: &File,
        _: RootedPath<'_>,
        _: bool,
        _: bool,
    ) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn remove_file(_: &File, _: RootedPath<'_>) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn copy_file(
        _: &File,
        _: RootedPath<'_>,
        _: RootedPath<'_>,
        _: bool,
        _: usize,
    ) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn move_file(
        _: &File,
        _: RootedPath<'_>,
        _: RootedPath<'_>,
        _: bool,
    ) -> Result<(), FilesystemProviderError> {
        Err(REFUSED)
    }

    pub(super) fn open_capture_source(_: &File, _: &[OsString]) -> Result<File, CaptureFileError> {
        Err(CaptureFileError::SourceUnavailable)
    }
}

fn metadata_value(metadata: &fs::Metadata, is_symlink: bool) -> HostFilesystemMetadata {
    let kind = if is_symlink {
        "symlink"
    } else if metadata.is_file() {
        "file"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "other"
    };
    HostFilesystemMetadata {
        kind: kind.to_owned(),
        size: (kind == "file" || kind == "symlink").then_some(metadata.len()),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write};

    fn provider(root: &Path) -> FilesystemProvider {
        let mut provider = FilesystemProvider::with_limits(1024, 16).unwrap();
        provider.allow_root(root).unwrap();
        provider
    }

    fn failure(result: Result<CaptureFileStream, CaptureFileError>) -> CaptureFileError {
        result.err().expect("capture should fail")
    }

    /// ORNA-INGEST-002: capture requires an authorised root and a safe relative
    /// path, rejects directory inputs and enforces the byte bound, and missing
    /// permission and quota exhaustion have distinct typed outcomes carrying
    /// the portable `sys.blob.source_denied` and `sys.blob.limit` codes
    /// (ORNA-SYS-097).
    #[test]
    fn capture_file_reports_each_ingestion_outcome_with_its_portable_code() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("small.bin"), [0, 255, 1]).unwrap();
        fs::create_dir(directory.path().join("directory")).unwrap();
        let provider = provider(directory.path());

        assert_eq!(
            failure(provider.capture_file("/not/authorized", "small.bin", 10)),
            CaptureFileError::UnauthorizedRoot
        );
        assert_eq!(
            CaptureFileError::UnauthorizedRoot.failure_code(),
            "sys.blob.source_denied"
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "../small.bin", 10)),
            CaptureFileError::InvalidPath
        );
        assert_eq!(
            CaptureFileError::InvalidPath.failure_code(),
            "sys.blob.source_denied"
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "missing", 10)),
            CaptureFileError::SourceUnavailable
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "directory", 10)),
            CaptureFileError::SourceUnavailable
        );
        assert_eq!(
            CaptureFileError::SourceUnavailable.failure_code(),
            "sys.blob.source_denied"
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "small.bin", 2)),
            CaptureFileError::LimitExceeded
        );
        assert_eq!(
            CaptureFileError::LimitExceeded.failure_code(),
            "sys.blob.limit"
        );
    }

    /// The host contract bounds "the source and captured Blob" by `max_bytes`
    /// and the provider limit, so a generous `max_bytes` alone never fails.
    #[test]
    fn capture_bounds_the_source_rather_than_a_generous_max_bytes() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("small.bin"), [7; 16]).unwrap();
        fs::write(directory.path().join("large.bin"), vec![9; 1025]).unwrap();
        let provider = provider(directory.path());
        let root = directory.path().to_str().unwrap();

        let mut source = provider.capture_file(root, "small.bin", u64::MAX).unwrap();
        let mut streamed = Vec::new();
        source.read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed, [7; 16]);
        assert!(!source.exceeded());
        assert_eq!(source.verify_unchanged(), Ok(()));
        assert_eq!(
            failure(provider.capture_file(root, "large.bin", u64::MAX)),
            CaptureFileError::LimitExceeded,
            "the provider limit still bounds the source"
        );
        assert_eq!(
            failure(provider.capture_file(root, "small.bin", 15)),
            CaptureFileError::LimitExceeded,
            "max_bytes still bounds the source"
        );
    }

    /// Capture streams, so its quota is set apart from the file bound that
    /// text reads load into memory.
    #[test]
    fn capture_quota_is_independent_of_the_text_read_limit() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("large.bin"), vec![b'x'; 2048]).unwrap();
        let mut provider = FilesystemProvider::with_limits(1024, 16)
            .unwrap()
            .with_capture_limit(4096)
            .unwrap();
        provider.allow_root(directory.path()).unwrap();
        let root = directory.path().to_str().unwrap();

        let mut source = provider.capture_file(root, "large.bin", u64::MAX).unwrap();
        let mut streamed = Vec::new();
        source.read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed.len(), 2048);
        assert_eq!(
            provider.read_text(root, "large.bin"),
            Err(FilesystemProviderError::OutputLimit),
            "text reads keep their own bound"
        );
        assert_eq!(
            FilesystemProvider::new().with_capture_limit(0).err(),
            Some(FilesystemProviderError::InvalidRequest)
        );
    }

    /// ORNA-INGEST-003: a detected change after the streamed read is rejected
    /// as `sys.blob.source_changed`.
    #[test]
    fn capture_reader_detects_path_replacement_after_streaming() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.dat");
        fs::write(&path, b"stable bytes").unwrap();
        let provider = provider(directory.path());
        let mut source = provider
            .capture_file(directory.path().to_str().unwrap(), "source.dat", 64)
            .unwrap();
        let mut streamed = Vec::new();
        source.read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed, b"stable bytes");
        fs::write(&path, b"replacement bytes").unwrap();

        assert_eq!(
            source.verify_unchanged(),
            Err(CaptureFileError::SourceChanged)
        );
        assert_eq!(
            CaptureFileError::SourceChanged.failure_code(),
            "sys.blob.source_changed"
        );
    }

    #[test]
    fn capture_reader_streams_at_most_the_limit_and_detects_growth() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.dat");
        fs::write(&path, b"1234").unwrap();
        let provider = provider(directory.path());
        let mut source = provider
            .capture_file(directory.path().to_str().unwrap(), "source.dat", 4)
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"5678")
            .unwrap();

        let mut streamed = Vec::new();
        source.read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed, b"12345", "the reader consumes one overflow byte");
        assert!(source.exceeded());
        assert_eq!(
            source.verify_unchanged(),
            Err(CaptureFileError::SourceChanged)
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_reader_rejects_symbolic_links_without_following_them() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), directory.path().join("link"))
            .unwrap();
        let provider = provider(directory.path());

        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "link", 64)),
            CaptureFileError::InvalidPath
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod confinement_tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        },
        time::{Duration, Instant},
    };

    use super::FilesystemProviderError::{
        AlreadyExists, Denied, InvalidRequest, InvalidUtf8, NotDirectory, NotFile, NotFound,
        OutputLimit,
    };

    /// A granted `root` directory beside an ungranted `outside` directory and
    /// `sibling.txt`, all inside one temporary directory.
    struct Fixture {
        _temporary: tempfile::TempDir,
        base: PathBuf,
        provider: FilesystemProvider,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let base = temporary.path().canonicalize().unwrap();
            fs::create_dir(base.join("root")).unwrap();
            fs::create_dir(base.join("outside")).unwrap();
            fs::write(base.join("outside/secret.txt"), "outside-secret").unwrap();
            fs::write(base.join("sibling.txt"), "sibling").unwrap();
            fs::write(base.join("root/input.txt"), "inside").unwrap();
            let mut provider = FilesystemProvider::with_limits(64, 8).unwrap();
            provider.allow_root(base.join("root")).unwrap();
            Self {
                _temporary: temporary,
                base,
                provider,
            }
        }

        fn root(&self) -> PathBuf {
            self.base.join("root")
        }

        fn outside(&self) -> PathBuf {
            self.base.join("outside")
        }

        /// The granted root as Orna callers name it.
        fn root_name(&self) -> String {
            self.root().to_str().unwrap().to_owned()
        }

        /// Every entry beside the granted root.
        fn beyond_root(&self) -> BTreeMap<PathBuf, String> {
            snapshot(&self.base)
                .into_iter()
                .filter(|(path, _)| !path.starts_with("root"))
                .collect()
        }
    }

    /// Describes every entry below `directory` without following links: file
    /// bytes, link targets and directories.
    fn snapshot(directory: &Path) -> BTreeMap<PathBuf, String> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(current) = pending.pop() {
            for entry in fs::read_dir(&current).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let description = if metadata.file_type().is_symlink() {
                    format!("link to {}", fs::read_link(&path).unwrap().display())
                } else if metadata.is_dir() {
                    pending.push(path.clone());
                    "directory".to_owned()
                } else if metadata.is_file() {
                    format!("file {:?}", fs::read(&path).unwrap())
                } else {
                    "special".to_owned()
                };
                entries.insert(
                    path.strip_prefix(directory).unwrap().to_path_buf(),
                    description,
                );
            }
        }
        entries
    }

    /// Runs every operation against `path`, using it as both the source and
    /// the destination of copies and moves, and keeps only each outcome.
    fn every_operation(
        provider: &FilesystemProvider,
        root: &str,
        path: &str,
    ) -> Vec<(&'static str, Result<(), FilesystemProviderError>)> {
        vec![
            ("read_text", provider.read_text(root, path).map(drop)),
            ("write_text", provider.write_text(root, path, "x", false)),
            (
                "write_text overwrite",
                provider.write_text(root, path, "x", true),
            ),
            ("append_text", provider.append_text(root, path, "x")),
            ("exists", provider.exists(root, path).map(drop)),
            ("is_directory", provider.is_directory(root, path).map(drop)),
            ("list", provider.list(root, path).map(drop)),
            ("metadata", provider.metadata(root, path).map(drop)),
            ("create_dir", provider.create_dir(root, path, false, false)),
            (
                "create_dir parents",
                provider.create_dir(root, path, true, true),
            ),
            ("remove_file", provider.remove_file(root, path)),
            (
                "copy_file from",
                provider.copy_file(root, path, "copied.txt", false),
            ),
            (
                "copy_file onto",
                provider.copy_file(root, "input.txt", path, true),
            ),
            (
                "move_file from",
                provider.move_file(root, path, "moved.txt", false),
            ),
            (
                "move_file onto",
                provider.move_file(root, "input.txt", path, true),
            ),
        ]
    }

    #[test]
    fn symbolic_links_inside_the_root_are_refused_by_every_operation() {
        let fixture = Fixture::new();
        let root_path = fixture.root();
        symlink(
            fixture.outside().join("secret.txt"),
            root_path.join("file-link"),
        )
        .unwrap();
        symlink(fixture.outside(), root_path.join("directory-link")).unwrap();
        symlink(
            fixture.outside().join("absent.txt"),
            root_path.join("dangling-link"),
        )
        .unwrap();
        let beyond = fixture.beyond_root();
        let within = snapshot(&root_path);
        let root = fixture.root_name();

        for path in [
            "file-link",
            "dangling-link",
            "directory-link",
            "directory-link/secret.txt",
            "directory-link/absent/deeper.txt",
        ] {
            for (operation, outcome) in every_operation(&fixture.provider, &root, path) {
                assert_eq!(outcome, Err(Denied), "{operation} on {path}");
            }
        }
        assert_eq!(
            fixture
                .provider
                .symlink_metadata(&root, "directory-link/secret.txt"),
            Err(Denied)
        );
        // The one non-following report describes the link itself, not its target.
        assert_eq!(
            fixture
                .provider
                .symlink_metadata(&root, "file-link")
                .map(|metadata| metadata.kind),
            Ok("symlink".to_owned())
        );
        assert_eq!(
            fixture.beyond_root(),
            beyond,
            "nothing outside the root changed"
        );
        assert_eq!(
            snapshot(&root_path),
            within,
            "refused operations changed nothing"
        );
    }

    #[test]
    fn parent_traversal_and_absolute_paths_are_refused_even_when_the_target_exists() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root().join("inner")).unwrap();
        let beyond = fixture.beyond_root();
        let within = snapshot(&fixture.root());
        let root = fixture.root_name();
        let absolute = fixture.base.join("sibling.txt");

        for path in [
            "../sibling.txt",
            "inner/../../sibling.txt",
            "inner/../input.txt",
            "..",
            "../outside/secret.txt",
            absolute.to_str().unwrap(),
        ] {
            for (operation, outcome) in every_operation(&fixture.provider, &root, path) {
                assert_eq!(outcome, Err(Denied), "{operation} on {path}");
            }
            assert_eq!(
                fixture.provider.symlink_metadata(&root, path),
                Err(Denied),
                "{path}"
            );
        }
        assert_eq!(fixture.beyond_root(), beyond);
        assert_eq!(snapshot(&fixture.root()), within);
    }

    #[test]
    fn missing_targets_report_not_found_distinct_from_out_of_root_refusals() {
        let fixture = Fixture::new();
        // The same names exist beside the root, so a provider that resolved
        // the traversal would find them rather than refuse.
        fs::write(fixture.base.join("missing.txt"), "beside the root").unwrap();
        fs::create_dir(fixture.base.join("missing-directory")).unwrap();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();

        assert_eq!(provider.read_text(root, "missing.txt"), Err(NotFound));
        assert_eq!(provider.read_text(root, "../missing.txt"), Err(Denied));
        assert_eq!(provider.metadata(root, "missing.txt"), Err(NotFound));
        assert_eq!(provider.metadata(root, "../missing.txt"), Err(Denied));
        assert_eq!(
            provider.symlink_metadata(root, "missing.txt"),
            Err(NotFound)
        );
        assert_eq!(provider.list(root, "missing-directory"), Err(NotFound));
        assert_eq!(provider.list(root, "../missing-directory"), Err(Denied));
        assert_eq!(provider.remove_file(root, "missing.txt"), Err(NotFound));
        assert_eq!(provider.remove_file(root, "../missing.txt"), Err(Denied));
        assert_eq!(
            provider.copy_file(root, "missing.txt", "copy.txt", false),
            Err(NotFound)
        );
        assert_eq!(
            provider.move_file(root, "missing.txt", "moved.txt", false),
            Err(NotFound)
        );
        assert_eq!(provider.exists(root, "missing.txt"), Ok(false));
        assert_eq!(provider.exists(root, "../missing.txt"), Err(Denied));
        assert_eq!(provider.is_directory(root, "missing-directory"), Ok(false));
        for below_missing in [
            "missing-directory/new.txt",
            "missing-directory/deeper/new.txt",
        ] {
            assert_eq!(provider.read_text(root, below_missing), Err(NotFound));
            assert_eq!(
                provider.write_text(root, below_missing, "x", false),
                Err(NotFound)
            );
            assert_eq!(
                provider.append_text(root, below_missing, "x"),
                Err(NotFound)
            );
            assert_eq!(
                provider.create_dir(root, below_missing, false, false),
                Err(NotFound)
            );
            assert_eq!(
                provider.copy_file(root, "input.txt", below_missing, false),
                Err(NotFound)
            );
            assert_eq!(
                provider.move_file(root, "input.txt", below_missing, false),
                Err(NotFound)
            );
            assert_eq!(provider.exists(root, below_missing), Ok(false));
        }
        // A root the host never approved is refused like any other escape.
        assert_eq!(
            provider.read_text(fixture.outside().to_str().unwrap(), "secret.txt"),
            Err(Denied)
        );
        // A component that exists but is not a directory is reported as such.
        assert_eq!(
            provider.read_text(root, "input.txt/child"),
            Err(NotDirectory)
        );
        assert_eq!(provider.list(root, "input.txt"), Err(NotDirectory));
        assert_eq!(provider.exists(root, "input.txt/child"), Ok(false));
        assert!(!fixture.root().join("missing-directory").exists());
        assert_eq!(
            fs::read_to_string(fixture.root().join("input.txt")).unwrap(),
            "inside"
        );
    }

    #[test]
    fn git_metadata_is_excluded_from_every_operation() {
        let fixture = Fixture::new();
        let root_path = fixture.root();
        fs::create_dir_all(root_path.join(".git/orna")).unwrap();
        fs::write(root_path.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root_path.join(".git/index"), [0x44, 0x49, 0x52, 0x43]).unwrap();
        fs::write(root_path.join(".git/orna/state.db"), "runtime state").unwrap();
        fs::create_dir(root_path.join("nested")).unwrap();
        fs::write(
            root_path.join("nested/.git"),
            "gitdir: ../.git/worktrees/nested\n",
        )
        .unwrap();
        let before = snapshot(&root_path);
        let root = fixture.root_name();

        for path in [
            ".git",
            ".git/HEAD",
            "./.git/index",
            ".git/orna/state.db",
            ".git/new.txt",
            ".GIT/HEAD",
            ".Git",
            "nested/.git",
            "nested/.git/config",
        ] {
            for (operation, outcome) in every_operation(&fixture.provider, &root, path) {
                assert_eq!(outcome, Err(Denied), "{operation} on {path}");
            }
            assert_eq!(
                fixture.provider.symlink_metadata(&root, path),
                Err(Denied),
                "{path}"
            );
        }
        assert_eq!(
            fixture.provider.list(&root, "."),
            Ok(vec!["input.txt".to_owned(), "nested".to_owned()])
        );
        assert_eq!(fixture.provider.list(&root, "nested"), Ok(Vec::new()));
        assert_eq!(
            snapshot(&root_path),
            before,
            "Git metadata and the rest of the root are unchanged"
        );
    }

    #[test]
    fn writes_create_exclusively_unless_overwriting_and_stay_within_the_byte_limit() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();
        let notes = fixture.root().join("notes.txt");

        assert_eq!(
            provider.write_text(root, "notes.txt", "first", false),
            Ok(())
        );
        assert_eq!(
            provider.write_text(root, "notes.txt", "second", false),
            Err(AlreadyExists)
        );
        assert_eq!(fs::read_to_string(&notes).unwrap(), "first");
        assert_eq!(provider.write_text(root, "notes.txt", "2nd", true), Ok(()));
        assert_eq!(
            fs::read_to_string(&notes).unwrap(),
            "2nd",
            "overwriting truncates the old bytes"
        );
        assert_eq!(provider.append_text(root, "notes.txt", "+more"), Ok(()));
        assert_eq!(
            provider.read_text(root, "notes.txt"),
            Ok("2nd+more".to_owned())
        );
        assert_eq!(provider.append_text(root, "log.txt", "created"), Ok(()));
        assert_eq!(
            provider.read_text(root, "log.txt"),
            Ok("created".to_owned())
        );
        assert_eq!(
            provider.append_text(root, "notes.txt", &"x".repeat(64)),
            Err(OutputLimit)
        );
        assert_eq!(
            provider.write_text(root, "big.txt", &"x".repeat(65), true),
            Err(OutputLimit)
        );
        assert_eq!(
            provider.read_text(root, "notes.txt"),
            Ok("2nd+more".to_owned())
        );
        assert!(!fixture.root().join("big.txt").exists());
        fs::write(fixture.root().join("large.txt"), "y".repeat(65)).unwrap();
        assert_eq!(provider.read_text(root, "large.txt"), Err(OutputLimit));
        fs::write(fixture.root().join("binary.txt"), [0xff, 0xfe]).unwrap();
        assert_eq!(provider.read_text(root, "binary.txt"), Err(InvalidUtf8));
        fs::create_dir(fixture.root().join("folder")).unwrap();
        assert_eq!(
            provider.write_text(root, "folder", "x", false),
            Err(NotFile)
        );
        assert_eq!(provider.write_text(root, "folder", "x", true), Err(NotFile));
        assert_eq!(provider.append_text(root, "folder", "x"), Err(NotFile));
        assert_eq!(provider.read_text(root, "folder"), Err(NotFile));
        assert_eq!(provider.remove_file(root, "folder"), Err(NotFile));
        assert_eq!(provider.remove_file(root, "log.txt"), Ok(()));
        assert!(!fixture.root().join("log.txt").exists());
    }

    #[test]
    fn create_dir_honours_parents_and_exist_ok() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();

        assert_eq!(
            provider.create_dir(root, "a/b/c", false, false),
            Err(NotFound)
        );
        assert_eq!(provider.create_dir(root, "a/b/c", true, false), Ok(()));
        assert!(fixture.root().join("a/b/c").is_dir());
        assert_eq!(
            provider.create_dir(root, "a/b/c", true, false),
            Err(AlreadyExists)
        );
        assert_eq!(provider.create_dir(root, "a/b/c", false, true), Ok(()));
        assert_eq!(provider.create_dir(root, "a/b/d", false, false), Ok(()));
        assert_eq!(
            provider.create_dir(root, "input.txt", false, true),
            Err(AlreadyExists)
        );
        assert_eq!(
            provider.create_dir(root, "input.txt/child", true, true),
            Err(NotDirectory)
        );
        assert_eq!(provider.create_dir(root, ".", false, true), Ok(()));
        assert_eq!(
            provider.create_dir(root, "", true, false),
            Err(AlreadyExists)
        );
        assert_eq!(provider.is_directory(root, "a/b/c"), Ok(true));
        assert_eq!(provider.is_directory(root, "input.txt"), Ok(false));
        assert_eq!(provider.exists(root, "a/b/d"), Ok(true));
    }

    #[test]
    fn copies_and_moves_never_replace_a_destination_without_overwrite() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();
        let root_path = fixture.root();
        fs::set_permissions(
            root_path.join("input.txt"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        fs::write(root_path.join("taken.txt"), "keep").unwrap();

        assert_eq!(
            provider.copy_file(root, "input.txt", "copy.txt", false),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(root_path.join("copy.txt")).unwrap(),
            "inside"
        );
        assert_eq!(
            fs::metadata(root_path.join("copy.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o640,
            "a copy takes the source's permission bits"
        );
        assert_eq!(
            provider.copy_file(root, "input.txt", "taken.txt", false),
            Err(AlreadyExists)
        );
        assert_eq!(
            fs::read_to_string(root_path.join("taken.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            provider.copy_file(root, "taken.txt", "copy.txt", true),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(root_path.join("copy.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            provider.copy_file(root, "input.txt", "./input.txt", true),
            Ok(()),
            "copying a file onto itself keeps its bytes"
        );
        assert_eq!(
            fs::read_to_string(root_path.join("input.txt")).unwrap(),
            "inside"
        );

        assert_eq!(
            provider.move_file(root, "copy.txt", "taken.txt", false),
            Err(AlreadyExists)
        );
        assert_eq!(
            fs::read_to_string(root_path.join("taken.txt")).unwrap(),
            "keep"
        );
        assert!(root_path.join("copy.txt").exists());
        assert_eq!(
            provider.move_file(root, "copy.txt", "moved.txt", false),
            Ok(())
        );
        assert!(!root_path.join("copy.txt").exists());
        assert_eq!(
            fs::read_to_string(root_path.join("moved.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            provider.move_file(root, "input.txt", "moved.txt", true),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(root_path.join("moved.txt")).unwrap(),
            "inside"
        );
        fs::create_dir(root_path.join("archive")).unwrap();
        assert_eq!(
            provider.move_file(root, "moved.txt", "archive/moved.txt", false),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(root_path.join("archive/moved.txt")).unwrap(),
            "inside"
        );
        assert_eq!(
            provider.move_file(root, "archive", "elsewhere", false),
            Err(NotFile)
        );
        assert_eq!(
            provider.move_file(root, "taken.txt", "archive", true),
            Err(NotFile)
        );
        assert_eq!(
            provider.copy_file(root, "archive", "copied", false),
            Err(NotFile)
        );
        fs::write(root_path.join("large.txt"), "z".repeat(65)).unwrap();
        assert_eq!(
            provider.copy_file(root, "large.txt", "large-copy.txt", false),
            Err(OutputLimit)
        );
        assert!(!root_path.join("large-copy.txt").exists());
    }

    #[test]
    fn the_root_itself_is_listed_and_inspected_but_never_treated_as_a_file() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();

        assert_eq!(provider.list(root, ""), Ok(vec!["input.txt".to_owned()]));
        assert_eq!(provider.exists(root, ""), Ok(true));
        assert_eq!(provider.is_directory(root, "."), Ok(true));
        assert_eq!(
            provider.metadata(root, "").map(|metadata| metadata.kind),
            Ok("directory".to_owned())
        );
        assert_eq!(
            provider
                .symlink_metadata(root, ".")
                .map(|metadata| metadata.kind),
            Ok("directory".to_owned())
        );
        assert_eq!(provider.read_text(root, ""), Err(NotFile));
        assert_eq!(provider.write_text(root, ".", "x", true), Err(NotFile));
        assert_eq!(provider.append_text(root, "", "x"), Err(NotFile));
        assert_eq!(provider.remove_file(root, "."), Err(NotFile));
        assert_eq!(
            provider.copy_file(root, "", "copy.txt", false),
            Err(NotFile)
        );
        assert_eq!(
            provider.copy_file(root, "input.txt", ".", true),
            Err(NotFile)
        );
        assert_eq!(provider.move_file(root, ".", "moved", false), Err(NotFile));
        // Listing again sees new entries: the held root handle is never read
        // directly, so its offset is never consumed.
        fs::write(fixture.root().join("second.txt"), "2").unwrap();
        assert_eq!(
            provider.list(root, "."),
            Ok(vec!["input.txt".to_owned(), "second.txt".to_owned()])
        );
        let metadata = provider.metadata(root, "input.txt").unwrap();
        assert_eq!((metadata.kind.as_str(), metadata.size), ("file", Some(6)));
    }

    #[test]
    fn listings_are_sorted_and_bounded_and_names_must_be_unicode_without_nul() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();
        for name in ["c", "a", "b"] {
            fs::write(fixture.root().join(name), name).unwrap();
        }

        assert_eq!(
            provider.list(root, "."),
            Ok(["a", "b", "c", "input.txt"].map(str::to_owned).to_vec())
        );
        for index in 0..4 {
            fs::write(fixture.root().join(format!("extra-{index}")), "").unwrap();
        }
        assert_eq!(provider.list(root, ".").map(|names| names.len()), Ok(8));
        fs::write(fixture.root().join("extra-4"), "").unwrap();
        assert_eq!(provider.list(root, "."), Err(OutputLimit));
        fs::create_dir(fixture.root().join("raw")).unwrap();
        fs::write(
            fixture.root().join("raw").join(OsStr::from_bytes(b"\xff")),
            "",
        )
        .unwrap();
        assert_eq!(provider.list(root, "raw"), Err(InvalidRequest));
        assert_eq!(provider.read_text(root, "input\0.txt"), Err(InvalidRequest));
    }

    #[test]
    fn special_files_are_refused_without_being_opened() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let root = root.as_str();
        let fifo = fixture.root().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            fifo.as_path(),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let started = Instant::now();

        assert_eq!(provider.read_text(root, "pipe"), Err(NotFile));
        assert_eq!(provider.write_text(root, "pipe", "x", true), Err(NotFile));
        assert_eq!(provider.append_text(root, "pipe", "x"), Err(NotFile));
        assert_eq!(
            provider.copy_file(root, "pipe", "copy.txt", false),
            Err(NotFile)
        );
        assert_eq!(
            provider.copy_file(root, "input.txt", "pipe", true),
            Err(NotFile)
        );
        assert_eq!(
            provider.move_file(root, "pipe", "moved", false),
            Err(NotFile)
        );
        assert_eq!(
            provider
                .metadata(root, "pipe")
                .map(|metadata| metadata.kind),
            Ok("other".to_owned())
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no operation waited for a FIFO peer"
        );
        assert_eq!(provider.remove_file(root, "pipe"), Ok(()));
        assert!(!fifo.exists());
    }

    #[test]
    fn operations_keep_using_the_root_handle_held_since_it_was_allowed() {
        let fixture = Fixture::new();
        let provider = &fixture.provider;
        let root = fixture.root_name();
        let held = fixture.base.join("held");
        fs::rename(fixture.root(), &held).unwrap();
        // The approved path now leads outside the root instead.
        symlink(fixture.outside(), fixture.root()).unwrap();

        assert_eq!(
            provider.read_text(&root, "input.txt"),
            Ok("inside".to_owned())
        );
        assert_eq!(provider.read_text(&root, "secret.txt"), Err(NotFound));
        assert_eq!(
            provider.write_text(&root, "written.txt", "held", false),
            Ok(())
        );
        assert_eq!(
            fs::read_to_string(held.join("written.txt")).unwrap(),
            "held"
        );
        assert!(!fixture.outside().join("written.txt").exists());
        assert_eq!(
            provider.list(&root, "."),
            Ok(vec!["input.txt".to_owned(), "written.txt".to_owned()])
        );
    }

    #[test]
    fn an_opened_parent_stays_put_when_its_name_is_swapped_for_a_link() {
        let fixture = Fixture::new();
        let root_path = fixture.root();
        fs::create_dir(root_path.join("reports")).unwrap();
        let handle = File::open(&root_path).unwrap();
        let Ok(parent) = beneath::walk(&handle, [OsStr::new("reports")]) else {
            panic!("the reports directory opens beneath the root");
        };
        // Between the walk and the final call the parent is renamed away and
        // a link to the outside directory takes its name.
        fs::rename(root_path.join("reports"), root_path.join("reports-old")).unwrap();
        symlink(fixture.outside(), root_path.join("reports")).unwrap();

        let (mut file, _) = beneath::open_writable(
            &parent,
            OsStr::new("summary.txt"),
            beneath::Existing::Refuse,
            beneath::FILE_MODE,
        )
        .unwrap();
        file.write_all(b"held parent").unwrap();
        assert_eq!(
            fs::read_to_string(root_path.join("reports-old/summary.txt")).unwrap(),
            "held parent"
        );
        assert!(!fixture.outside().join("summary.txt").exists());
        assert_eq!(
            fixture
                .provider
                .write_text(&fixture.root_name(), "reports/summary.txt", "x", false),
            Err(Denied)
        );
    }

    #[test]
    fn moves_without_rename_noreplace_fall_back_to_an_exclusive_link() {
        let fixture = Fixture::new();
        let root_path = fixture.root();
        fs::write(root_path.join("taken.txt"), "keep").unwrap();
        let handle = File::open(&root_path).unwrap();

        assert_eq!(
            beneath::link_then_unlink(
                &handle,
                OsStr::new("input.txt"),
                &handle,
                OsStr::new("taken.txt")
            ),
            Err(AlreadyExists)
        );
        assert_eq!(
            fs::read_to_string(root_path.join("taken.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            fs::read_to_string(root_path.join("input.txt")).unwrap(),
            "inside"
        );
        assert_eq!(
            beneath::link_then_unlink(
                &handle,
                OsStr::new("input.txt"),
                &handle,
                OsStr::new("moved.txt")
            ),
            Ok(())
        );
        assert!(!root_path.join("input.txt").exists());
        assert_eq!(
            fs::read_to_string(root_path.join("moved.txt")).unwrap(),
            "inside"
        );
    }
}
