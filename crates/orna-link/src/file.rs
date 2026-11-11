//! A library's file under the project root, and its content digest
//! (docs/design/native-libraries.md, sections 3 and 10).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Resolves `relative` under `root` to a regular file that is not a symbolic link
/// and whose real location is inside the real project root. Returns the path to
/// read, or the reason it is unusable.
pub(crate) fn resolve_library_file(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let path = root.join(relative);
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| format!("cannot be read ({error})"))?;
    if metadata.file_type().is_symlink() {
        return Err("is a symbolic link".to_owned());
    }
    if !metadata.is_file() {
        return Err("is not a regular file".to_owned());
    }
    let real_root = fs::canonicalize(root)
        .map_err(|error| format!("project root cannot be resolved ({error})"))?;
    let real_path =
        fs::canonicalize(&path).map_err(|error| format!("cannot be resolved ({error})"))?;
    if !real_path.starts_with(&real_root) {
        return Err("resolves outside the project root".to_owned());
    }
    Ok(path)
}

/// The SHA-256 of the file's bytes exactly as stored.
pub(crate) fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let bytes = fs::read(path)?;
    Ok(Sha256::digest(&bytes).into())
}
