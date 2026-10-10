//! Allowlisted native filesystem operations collected into the sys host ABI.
//!
//! The default provider caps UTF-8 file reads/writes at 8 MiB and directory
//! listings at 4096 entries; hosts can choose tighter limits with `with_limits`.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

#[cfg(target_os = "linux")]
use rustix::fs::StatExt;
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
    pub const fn failure_code(self) -> &'static str {
        match self {
            Self::UnauthorizedRoot => "sys.blob.capture.unauthorized_root",
            Self::InvalidPath => "sys.blob.capture.invalid_path",
            Self::SourceUnavailable => "sys.blob.capture.source_unavailable",
            Self::LimitExceeded => "sys.blob.capture.limit_exceeded",
            Self::SourceChanged => "sys.blob.capture.source_changed",
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::UnauthorizedRoot => "ORNA-INGEST-001",
            Self::InvalidPath => "ORNA-INGEST-002",
            Self::SourceUnavailable => "ORNA-INGEST-003",
            Self::LimitExceeded => "ORNA-INGEST-004",
            Self::SourceChanged => "ORNA-INGEST-005",
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
/// Relative paths are checked before and after resolution; absolute paths and
/// parent traversal are rejected. Hosts should provide roots that are not
/// concurrently mutated by untrusted processes.
#[derive(Clone, Debug)]
pub struct FilesystemProvider {
    roots: BTreeMap<PathBuf, Arc<File>>,
    max_text_bytes: usize,
    max_directory_entries: usize,
}

impl PartialEq for FilesystemProvider {
    fn eq(&self, other: &Self) -> bool {
        self.roots.keys().eq(other.roots.keys())
            && self.max_text_bytes == other.max_text_bytes
            && self.max_directory_entries == other.max_directory_entries
    }
}

impl Eq for FilesystemProvider {}

impl Default for FilesystemProvider {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
        }
    }
}

impl FilesystemProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider with host-selected file and listing bounds.
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
        })
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

    fn authorized_root(&self, root: &str) -> Result<PathBuf, FilesystemProviderError> {
        let root = Path::new(root)
            .canonicalize()
            .map_err(|_| FilesystemProviderError::Denied)?;
        self.roots
            .contains_key(&root)
            .then_some(root)
            .ok_or(FilesystemProviderError::Denied)
    }

    fn authorized_root_handle(&self, root: &str) -> Result<(PathBuf, Arc<File>), CaptureFileError> {
        let requested = Path::new(root);
        let canonical = requested.canonicalize().ok();
        let Some((path, handle)) = canonical
            .as_ref()
            .and_then(|path| self.roots.get_key_value(path))
            .or_else(|| self.roots.get_key_value(requested))
        else {
            return Err(CaptureFileError::UnauthorizedRoot);
        };
        Ok((path.clone(), Arc::clone(handle)))
    }

    /// Opens one bounded regular file beneath a held, host-authorized root.
    /// The returned reader retains the open file, captures no more than the
    /// requested bytes, and can verify the same path identity after streaming.
    #[sys_host_operation(
        r###"{"name":"sys.blob.capture_file","version":{"major":1,"minor":0},"signature":"fn sys.blob.capture_file(root: Str, path: Str, max_bytes: Int): Blob","effects":["read"],"preconditions":["root is explicitly allowlisted by the host and held for the provider lifetime","path is relative and contains no parent traversal or symbolic-link component","target is a regular file whose identity, size and modification time remain stable during capture","the source and captured Blob fit within max_bytes and the provider limit"],"failures":["sys.blob.capture.unauthorized_root","sys.blob.capture.invalid_path","sys.blob.capture.source_unavailable","sys.blob.capture.limit_exceeded","sys.blob.capture.source_changed"],"role":"host.sys.blob.capture@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"capture_file"}"###
    )]
    pub fn capture_file(
        &self,
        root: &str,
        path: &str,
        max_bytes: u64,
    ) -> Result<CaptureFileReader, CaptureFileError> {
        let (authorized_path, root_handle) = self.authorized_root_handle(root)?;
        let components = capture_components(path)?;
        if max_bytes > self.max_text_bytes as u64 || max_bytes > i64::MAX as u64 {
            return Err(CaptureFileError::LimitExceeded);
        }
        let file = open_relative_regular(&authorized_path, &root_handle, &components)?;
        let metadata = file
            .metadata()
            .map_err(|_| CaptureFileError::SourceUnavailable)?;
        if !metadata.is_file() {
            return Err(CaptureFileError::SourceUnavailable);
        }
        let before = FileSnapshot::from_metadata(&metadata)?;
        if before.size > max_bytes {
            return Err(CaptureFileError::LimitExceeded);
        }
        Ok(CaptureFileReader {
            file,
            root_path: authorized_path,
            root: root_handle,
            components,
            before,
            max_bytes,
            bytes_read: 0,
            captured: Vec::new(),
            exceeded: false,
        })
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.read_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.read_text(root: Str, path: Str): Str","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","file contents are valid UTF-8 and within the provider byte limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_file","sys.host.filesystem.invalid_utf8","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"read_text"}"###
    )]
    pub fn read_text(&self, root: &str, path: &str) -> Result<String, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.resolve_existing(&root, path)?;
        if !path.is_file() {
            return Err(FilesystemProviderError::NotFile);
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(map_io_error)?
            .take(self.max_text_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(map_io_error)?;
        if bytes.len() > self.max_text_bytes {
            return Err(FilesystemProviderError::OutputLimit);
        }
        String::from_utf8(bytes).map_err(|_| FilesystemProviderError::InvalidUtf8)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.write_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.write_text(root: Str, path: Str, contents: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","parent directory exists","contents are within the provider byte limit","overwrite is true only for an existing regular file"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"write_text"}"###
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
        let destination = self.destination(&root, path, overwrite)?;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(map_io_error)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.append_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.append_text(root: Str, path: Str, contents: Str): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","parent directory exists","resulting file is within the provider byte limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_file","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"append_text"}"###
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
        let destination = self.destination(&root, path, true)?;
        match fs::metadata(&destination) {
            Ok(metadata)
                if metadata.len().saturating_add(contents.len() as u64)
                    > self.max_text_bytes as u64 =>
            {
                return Err(FilesystemProviderError::OutputLimit);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(map_io_error(error)),
        }
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(destination)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(map_io_error)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.exists","version":{"major":1,"minor":0},"signature":"fn std.io.fs.exists(root: Str, path: Str): Bool","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and any existing symlink target remains beneath root"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"exists"}"###
    )]
    pub fn exists(&self, root: &str, path: &str) -> Result<bool, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let candidate = self.lexical_path(&root, path)?;
        self.check_parent(&root, &candidate)?;
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    let resolved = candidate
                        .canonicalize()
                        .map_err(|_| FilesystemProviderError::Denied)?;
                    ensure_within(&root, &resolved)?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(map_io_error(error)),
        }
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.is_directory","version":{"major":1,"minor":0},"signature":"fn std.io.fs.is_directory(root: Str, path: Str): Bool","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and any existing symlink target remains beneath root"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"is_directory"}"###
    )]
    pub fn is_directory(&self, root: &str, path: &str) -> Result<bool, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let candidate = self.lexical_path(&root, path)?;
        self.check_parent(&root, &candidate)?;
        match candidate.canonicalize() {
            Ok(resolved) => {
                ensure_within(&root, &resolved)?;
                Ok(resolved.is_dir())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(map_io_error(error)),
        }
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.list","version":{"major":1,"minor":0},"signature":"fn std.io.fs.list(root: Str, path: Str): [Str]","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves to a directory beneath root","all child names are Unicode and within the provider entry limit"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_directory","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"list"}"###
    )]
    pub fn list(&self, root: &str, path: &str) -> Result<Vec<String>, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.resolve_existing(&root, path)?;
        if !path.is_dir() {
            return Err(FilesystemProviderError::NotDirectory);
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(path).map_err(map_io_error)? {
            if names.len() == self.max_directory_entries {
                return Err(FilesystemProviderError::OutputLimit);
            }
            names.push(
                entry
                    .map_err(map_io_error)?
                    .file_name()
                    .into_string()
                    .map_err(|_| FilesystemProviderError::InvalidRequest)?,
            );
        }
        names.sort();
        Ok(names)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.metadata","version":{"major":1,"minor":0},"signature":"fn std.io.fs.metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","resolved symbolic links remain beneath root"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"metadata"}"###
    )]
    pub fn metadata(
        &self,
        root: &str,
        path: &str,
    ) -> Result<HostFilesystemMetadata, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let resolved = self.resolve_existing(&root, path)?;
        let metadata = fs::metadata(resolved).map_err(map_io_error)?;
        Ok(metadata_value(&metadata, false))
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.symlink_metadata","version":{"major":1,"minor":0},"signature":"fn std.io.fs.symlink_metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and its parent resolves beneath root","final symbolic link is reported without following it"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"symlink_metadata"}"###
    )]
    pub fn symlink_metadata(
        &self,
        root: &str,
        path: &str,
    ) -> Result<HostFilesystemMetadata, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.lexical_path(&root, path)?;
        self.check_parent(&root, &path)?;
        let metadata = fs::symlink_metadata(path).map_err(map_io_error)?;
        Ok(metadata_value(&metadata, metadata.file_type().is_symlink()))
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.create_dir","version":{"major":1,"minor":0},"signature":"fn std.io.fs.create_dir(root: Str, path: Str, parents: Bool, exist_ok: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","existing directory is accepted only when exist_ok is true"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"create_dir"}"###
    )]
    pub fn create_dir(
        &self,
        root: &str,
        path: &str,
        parents: bool,
        exist_ok: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.lexical_path(&root, path)?;
        self.check_existing_ancestors(&root, &path)?;
        match if parents {
            fs::create_dir_all(&path)
        } else {
            fs::create_dir(&path)
        } {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && exist_ok => {
                let resolved = path
                    .canonicalize()
                    .map_err(|_| FilesystemProviderError::Denied)?;
                ensure_within(&root, &resolved)?;
                if resolved.is_dir() {
                    Ok(())
                } else {
                    Err(FilesystemProviderError::AlreadyExists)
                }
            }
            Err(error) => Err(map_io_error(error)),
        }
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.remove_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.remove_file(root: Str, path: Str): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and its parent resolves beneath root","target is not a directory"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_file","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"remove_file"}"###
    )]
    pub fn remove_file(&self, root: &str, path: &str) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.lexical_path(&root, path)?;
        self.check_parent(&root, &path)?;
        let metadata = fs::symlink_metadata(&path).map_err(map_io_error)?;
        if metadata.is_dir() {
            return Err(FilesystemProviderError::NotFile);
        }
        fs::remove_file(path).map_err(map_io_error)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.copy_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.copy_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","source and destination are relative and remain beneath root","source size is within the provider byte limit","overwrite is true only for an existing regular-file destination"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.output_limit","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"copy_file"}"###
    )]
    pub fn copy_file(
        &self,
        root: &str,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let source = self.resolve_existing(&root, source_path)?;
        if !source.is_file() {
            return Err(FilesystemProviderError::NotFile);
        }
        if fs::metadata(&source).map_err(map_io_error)?.len() > self.max_text_bytes as u64 {
            return Err(FilesystemProviderError::OutputLimit);
        }
        let destination = self.destination(&root, destination_path, overwrite)?;
        fs::copy(source, destination).map_err(map_io_error)?;
        Ok(())
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.move_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.move_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","source and destination are relative and remain beneath root","overwrite is true only for an existing regular-file destination"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"move_file"}"###
    )]
    pub fn move_file(
        &self,
        root: &str,
        source_path: &str,
        destination_path: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let source = self.resolve_existing(&root, source_path)?;
        if !source.is_file() {
            return Err(FilesystemProviderError::NotFile);
        }
        let destination = self.destination(&root, destination_path, overwrite)?;
        fs::rename(source, destination).map_err(map_io_error)
    }

    fn lexical_path(&self, root: &Path, path: &str) -> Result<PathBuf, FilesystemProviderError> {
        let relative = Path::new(path);
        if relative.is_absolute() {
            return Err(FilesystemProviderError::Denied);
        }
        let mut clean = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(FilesystemProviderError::Denied);
                }
            }
        }
        Ok(root.join(clean))
    }

    fn resolve_existing(
        &self,
        root: &Path,
        path: &str,
    ) -> Result<PathBuf, FilesystemProviderError> {
        let path = self.lexical_path(root, path)?;
        self.check_parent(root, &path)?;
        let resolved = path.canonicalize().map_err(map_io_error)?;
        ensure_within(root, &resolved)?;
        Ok(resolved)
    }

    fn destination(
        &self,
        root: &Path,
        path: &str,
        overwrite: bool,
    ) -> Result<PathBuf, FilesystemProviderError> {
        let path = self.lexical_path(root, path)?;
        self.check_parent(root, &path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let resolved = path
                    .canonicalize()
                    .map_err(|_| FilesystemProviderError::Denied)?;
                ensure_within(root, &resolved)?;
                if !metadata.is_file() && !metadata.file_type().is_symlink() {
                    return Err(FilesystemProviderError::NotFile);
                }
                if !overwrite {
                    return Err(FilesystemProviderError::AlreadyExists);
                }
                if !resolved.is_file() {
                    return Err(FilesystemProviderError::NotFile);
                }
                Ok(resolved)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
            Err(error) => Err(map_io_error(error)),
        }
    }

    fn check_parent(&self, root: &Path, path: &Path) -> Result<(), FilesystemProviderError> {
        if path == root {
            return Ok(());
        }
        let parent = path.parent().ok_or(FilesystemProviderError::Denied)?;
        let resolved = parent.canonicalize().map_err(map_io_error)?;
        ensure_within(root, &resolved)
    }

    fn check_existing_ancestors(
        &self,
        root: &Path,
        path: &Path,
    ) -> Result<(), FilesystemProviderError> {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| FilesystemProviderError::Denied)?;
        let mut ancestor = root.to_path_buf();
        for component in relative.components() {
            ancestor.push(component);
            if ancestor.exists() {
                let resolved = ancestor.canonicalize().map_err(map_io_error)?;
                ensure_within(root, &resolved)?;
            }
        }
        Ok(())
    }
}

/// A file opened beneath a held filesystem root for one bounded Blob capture.
/// Its reader retains at most `max_bytes` and reads one extra byte only to
/// distinguish an exact fit from a growing or oversized source.
pub struct CaptureFileReader {
    file: File,
    root_path: PathBuf,
    root: Arc<File>,
    components: Vec<std::ffi::OsString>,
    before: FileSnapshot,
    max_bytes: u64,
    bytes_read: u64,
    captured: Vec<u8>,
    exceeded: bool,
}

impl CaptureFileReader {
    pub fn captured_bytes(&self) -> &[u8] {
        &self.captured
    }
    /// Consumes the reader and transfers its retained bytes without cloning them.
    pub fn into_captured_bytes(self) -> Vec<u8> {
        self.captured
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
        let current = open_relative_regular(&self.root_path, &self.root, &self.components)
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

impl Read for CaptureFileReader {
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
        let retained = count.min(usize::try_from(remaining).unwrap_or(usize::MAX));
        self.captured
            .try_reserve(retained)
            .map_err(|_| io::Error::other("capture allocation failed"))?;
        self.captured.extend_from_slice(&output[..retained]);
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

fn capture_components(path: &str) -> Result<Vec<std::ffi::OsString>, CaptureFileError> {
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

#[cfg(target_os = "linux")]
fn open_relative_regular(
    _root_path: &Path,
    root: &File,
    components: &[std::ffi::OsString],
) -> Result<File, CaptureFileError> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};

    let mut directory = root
        .try_clone()
        .map_err(|_| CaptureFileError::SourceUnavailable)?;
    for (index, component) in components.iter().enumerate() {
        let is_final = index + 1 == components.len();
        if is_final {
            let before = rustix::fs::statat(&directory, component, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(map_rustix_capture_error)?;
            match FileType::from_raw_mode(before.st_mode) {
                FileType::RegularFile => {}
                FileType::Symlink => return Err(CaptureFileError::InvalidPath),
                _ => return Err(CaptureFileError::SourceUnavailable),
            }
            let descriptor = rustix::fs::openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(map_rustix_capture_error)?;
            let file = File::from(descriptor);
            let opened = rustix::fs::fstat(&file).map_err(map_rustix_capture_error)?;
            if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
                || before.st_dev != opened.st_dev
                || before.st_ino != opened.st_ino
                || before.st_size != opened.st_size
                || before.mtime() != opened.mtime()
                || before.st_mtime_nsec != opened.st_mtime_nsec
            {
                return Err(CaptureFileError::SourceChanged);
            }
            return Ok(file);
        }
        let descriptor = rustix::fs::openat(
            &directory,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(map_rustix_capture_error)?;
        directory = File::from(descriptor);
    }
    Err(CaptureFileError::InvalidPath)
}

#[cfg(target_os = "linux")]
fn map_rustix_capture_error(error: rustix::io::Errno) -> CaptureFileError {
    if error == rustix::io::Errno::NOENT {
        CaptureFileError::SourceUnavailable
    } else if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
        CaptureFileError::InvalidPath
    } else {
        CaptureFileError::SourceUnavailable
    }
}

#[cfg(not(target_os = "linux"))]
fn open_relative_regular(
    _root_path: &Path,
    _root: &File,
    _components: &[std::ffi::OsString],
) -> Result<File, CaptureFileError> {
    Err(CaptureFileError::SourceUnavailable)
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

fn ensure_within(root: &Path, path: &Path) -> Result<(), FilesystemProviderError> {
    path.starts_with(root)
        .then_some(())
        .ok_or(FilesystemProviderError::Denied)
}

fn map_io_error(error: std::io::Error) -> FilesystemProviderError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FilesystemProviderError::NotFound,
        std::io::ErrorKind::AlreadyExists => FilesystemProviderError::AlreadyExists,
        std::io::ErrorKind::PermissionDenied => FilesystemProviderError::Denied,
        std::io::ErrorKind::InvalidData => FilesystemProviderError::InvalidUtf8,
        _ => FilesystemProviderError::Unavailable,
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    fn provider(root: &Path) -> FilesystemProvider {
        let mut provider = FilesystemProvider::with_limits(1024, 16).unwrap();
        provider.allow_root(root).unwrap();
        provider
    }

    fn failure(result: Result<CaptureFileReader, CaptureFileError>) -> CaptureFileError {
        result.err().expect("capture should fail")
    }

    #[test]
    fn capture_file_reports_each_ingestion_outcome_with_a_distinct_code() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("small.bin"), [0, 255, 1]).unwrap();
        fs::create_dir(directory.path().join("directory")).unwrap();
        let provider = provider(directory.path());

        assert_eq!(
            failure(provider.capture_file("/not/authorized", "small.bin", 10)),
            CaptureFileError::UnauthorizedRoot
        );
        assert_eq!(CaptureFileError::UnauthorizedRoot.code(), "ORNA-INGEST-001");
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "../small.bin", 10)),
            CaptureFileError::InvalidPath
        );
        assert_eq!(CaptureFileError::InvalidPath.code(), "ORNA-INGEST-002");
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "missing", 10)),
            CaptureFileError::SourceUnavailable
        );
        assert_eq!(
            CaptureFileError::SourceUnavailable.code(),
            "ORNA-INGEST-003"
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "directory", 10)),
            CaptureFileError::SourceUnavailable
        );
        assert_eq!(
            failure(provider.capture_file(directory.path().to_str().unwrap(), "small.bin", 2)),
            CaptureFileError::LimitExceeded
        );
        assert_eq!(CaptureFileError::LimitExceeded.code(), "ORNA-INGEST-004");
    }

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
        assert_eq!(CaptureFileError::SourceChanged.code(), "ORNA-INGEST-005");
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
        assert_eq!(source.captured_bytes(), b"1234");
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
