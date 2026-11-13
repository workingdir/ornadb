//! Source-file edits shared by `orna mv` and the FUSE source writer: resolve a
//! project source path without following symlinks, replace the file atomically
//! only when its bytes are still the expected ones, and restore it when a later
//! step of the same operation fails.

use crate::request_nonce;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
};

/// A logical source path that is absent, unsafe, or not a regular file under the
/// project root. The refusal carries no detail: each caller reports it in its own
/// terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePathError;

pub fn checked_project_source_path(
    root: &Path,
    logical_path: &str,
) -> Result<PathBuf, SourcePathError> {
    let relative = Path::new(logical_path);
    let component_count = relative.components().count();
    if relative.is_absolute()
        || component_count == 0
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(SourcePathError);
    }
    let root_metadata = fs::symlink_metadata(root).map_err(|_| SourcePathError)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(SourcePathError);
    }
    let mut path = root.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|_| SourcePathError)?;
        if metadata.file_type().is_symlink()
            || (index + 1 == component_count && !metadata.is_file())
            || (index + 1 != component_count && !metadata.is_dir())
        {
            return Err(SourcePathError);
        }
    }
    Ok(path)
}

pub fn write_source_atomically(path: &Path, expected: &[u8], replacement: &[u8]) -> io::Result<()> {
    write_source_atomically_before_replace(path, expected, replacement, || Ok(()))
}

fn write_source_atomically_before_replace(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
    before_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::other("source path is not a regular file"));
    }
    if fs::read(path)? != expected {
        return Err(io::Error::other("source changed during rename"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("source has no parent directory"))?;
    let temp = parent.join(format!(".orna-mv-{:032x}.tmp", request_nonce()));
    let mut temp_created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        temp_created = true;
        // Copy only the permission bits: never carry setuid/setgid/sticky onto
        // the replacement source (W62-08).
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt as _;
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o777)
        };
        #[cfg(not(unix))]
        let permissions = metadata.permissions();
        file.set_permissions(permissions)?;
        file.write_all(replacement)?;
        file.sync_all()?;
        drop(file);
        before_replace()?;
        let current_metadata = fs::symlink_metadata(path)?;
        if current_metadata.file_type().is_symlink()
            || !current_metadata.is_file()
            || fs::read(path)? != expected
        {
            return Err(io::Error::other("source changed during rename"));
        }
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() && temp_created {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn restore_source_after_failed_write(path: &Path, original: &[u8], updated: &[u8]) -> bool {
    let Ok(current) = fs::symlink_metadata(path) else {
        return false;
    };
    if current.file_type().is_symlink() || !current.is_file() {
        return false;
    }
    match fs::read(path) {
        Ok(bytes) if bytes == original => true,
        Ok(bytes) if bytes == updated => write_source_atomically(path, updated, original).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod source_edit_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn source_writer_rejects_stale_expected_bytes_without_creating_a_temp_file() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"current").unwrap();

        let error = write_source_atomically(&source, b"stale", b"replacement").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&source).unwrap(), b"current");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".orna-mv-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn source_writer_refuses_symlinks_without_changing_the_target() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let target = directory.path().join("target.orna");
        let link = directory.path().join("main.orna");
        fs::write(&target, b"target").unwrap();
        symlink(&target, &link).unwrap();

        assert!(write_source_atomically(&link, b"target", b"replacement").is_err());

        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// W62-08: the replacement source must not inherit setuid/setgid/sticky.
    #[cfg(unix)]
    #[test]
    fn source_writer_drops_setuid_setgid_and_sticky_from_the_replaced_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"before").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o7755)).unwrap();

        write_source_atomically(&source, b"before", b"after").unwrap();

        assert_eq!(fs::read(&source).unwrap(), b"after");
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }

    #[test]
    fn source_writer_cleans_its_temp_file_when_the_expected_source_changes() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"before").unwrap();

        let error =
            write_source_atomically_before_replace(&source, b"before", b"replacement", || {
                fs::write(&source, b"external change")
            })
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&source).unwrap(), b"external change");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".orna-mv-")
        }));
    }

    #[test]
    fn source_rollback_restores_only_known_states() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("main.orna");
        fs::write(&source, b"original").unwrap();

        assert!(restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        fs::write(&source, b"updated").unwrap();
        assert!(restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        assert_eq!(fs::read(&source).unwrap(), b"original");
        fs::write(&source, b"concurrent").unwrap();
        assert!(!restore_source_after_failed_write(
            &source,
            b"original",
            b"updated"
        ));
        assert_eq!(fs::read(&source).unwrap(), b"concurrent");
    }
}
