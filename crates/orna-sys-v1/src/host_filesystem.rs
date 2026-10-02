//! Allowlisted native filesystem operations collected into the sys host ABI.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

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
        }
    }
}

/// Filesystem access restricted to exact host-approved directory roots.
/// Relative paths are checked before and after resolution; absolute paths and
/// parent traversal are rejected. Hosts should provide roots that are not
/// concurrently mutated by untrusted processes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemProvider {
    roots: BTreeSet<PathBuf>,
}

impl FilesystemProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one existing directory as an explicit filesystem capability.
    pub fn allow_root(&mut self, root: impl AsRef<Path>) -> Result<(), FilesystemProviderError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| FilesystemProviderError::InvalidRequest)?;
        if !root.is_dir() || !self.roots.insert(root) {
            return Err(FilesystemProviderError::InvalidRequest);
        }
        Ok(())
    }

    fn authorized_root(&self, root: &str) -> Result<PathBuf, FilesystemProviderError> {
        let root = Path::new(root)
            .canonicalize()
            .map_err(|_| FilesystemProviderError::Denied)?;
        self.roots
            .contains(&root)
            .then_some(root)
            .ok_or(FilesystemProviderError::Denied)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.read_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.read_text(root: Str, path: Str): Str","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","file contents are valid UTF-8"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_found","sys.host.filesystem.not_file","sys.host.filesystem.invalid_utf8","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"read_text"}"###
    )]
    pub fn read_text(&self, root: &str, path: &str) -> Result<String, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.resolve_existing(&root, path)?;
        if !path.is_file() {
            return Err(FilesystemProviderError::NotFile);
        }
        fs::read_to_string(path).map_err(map_io_error)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.fs.write_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.write_text(root: Str, path: Str, contents: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","parent directory exists","overwrite is true only for an existing regular file"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"write_text"}"###
    )]
    pub fn write_text(
        &self,
        root: &str,
        path: &str,
        contents: &str,
        overwrite: bool,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
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
        r###"{"name":"std.io.fs.append_text","version":{"major":1,"minor":0},"signature":"fn std.io.fs.append_text(root: Str, path: Str, contents: Str): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves beneath root","parent directory exists"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_file","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"append_text"}"###
    )]
    pub fn append_text(
        &self,
        root: &str,
        path: &str,
        contents: &str,
    ) -> Result<(), FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let destination = self.destination(&root, path, true)?;
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
        r###"{"name":"std.io.fs.list","version":{"major":1,"minor":0},"signature":"fn std.io.fs.list(root: Str, path: Str): [Str]","effects":["read"],"preconditions":["root is explicitly allowlisted by the host","path is relative and resolves to a directory beneath root","all child names are Unicode"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.not_directory","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.read@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"list"}"###
    )]
    pub fn list(&self, root: &str, path: &str) -> Result<Vec<String>, FilesystemProviderError> {
        let root = self.authorized_root(root)?;
        let path = self.resolve_existing(&root, path)?;
        if !path.is_dir() {
            return Err(FilesystemProviderError::NotDirectory);
        }
        let mut names = fs::read_dir(path)
            .map_err(map_io_error)?
            .map(|entry| {
                entry
                    .map_err(map_io_error)?
                    .file_name()
                    .into_string()
                    .map_err(|_| FilesystemProviderError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
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
        r###"{"name":"std.io.fs.copy_file","version":{"major":1,"minor":0},"signature":"fn std.io.fs.copy_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit","effects":["invoke"],"preconditions":["root is explicitly allowlisted by the host","source and destination are relative and remain beneath root","overwrite is true only for an existing regular-file destination"],"failures":["sys.host.filesystem.denied","sys.host.filesystem.invalid_request","sys.host.filesystem.already_exists","sys.host.filesystem.not_file","sys.host.filesystem.unavailable"],"role":"host.std.io.fs.write@1.0","provider":"orna.sys.host.filesystem.v1","implementation":"copy_file"}"###
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
