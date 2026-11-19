//! Dependency gitlinks of a loaded project. Each gitlink is bound to the commit its
//! parent records and to the origin its `.gitmodules` declares for that path.

use std::{fs, io::Read, path::Path};

use orna_repository::dependency_origins::{
    GitmodulesError, MAX_GITMODULES_BYTES, parse_dependency_origins,
};
use orna_repository::{GitCommitRef, RepositoryError};

use super::{OpaqueGitlinkPin, ProjectLoadError};

/// Binds each dependency gitlink to the commit its parent records and to the URL
/// its `.gitmodules` declares for that path. `read_gitmodules` runs only when a
/// gitlink exists. A gitlink without exactly one declared origin is refused, since
/// the dependency could not be identified.
pub(super) fn bind_opaque_pins(
    gitlinks: Vec<(String, GitCommitRef)>,
    read_gitmodules: impl FnOnce() -> Result<Option<Vec<u8>>, ProjectLoadError>,
) -> Result<Vec<OpaqueGitlinkPin>, ProjectLoadError> {
    if gitlinks.is_empty() {
        return Ok(Vec::new());
    }
    let origins = match read_gitmodules()? {
        Some(bytes) => parse_dependency_origins(&bytes).map_err(|error| match error {
            GitmodulesError::GitUnavailable => {
                ProjectLoadError::Repository(RepositoryError::GitUnavailable)
            }
            GitmodulesError::TooLarge
            | GitmodulesError::NotUtf8
            | GitmodulesError::InvalidValue => ProjectLoadError::InvalidGitmodules,
        })?,
        None => Vec::new(),
    };
    gitlinks
        .into_iter()
        .map(|(path, commit)| {
            let declared = origins
                .iter()
                .filter(|origin| origin.path() == path)
                .collect::<Vec<_>>();
            let origin = match declared.as_slice() {
                [origin] => origin.url().to_owned(),
                [] => return Err(ProjectLoadError::UndeclaredGitlink { path }),
                _ => return Err(ProjectLoadError::InvalidGitmodules),
            };
            Ok(OpaqueGitlinkPin {
                path,
                commit,
                origin,
            })
        })
        .collect()
}

/// Reads the worktree's `.gitmodules`, bounded to one byte past
/// [`MAX_GITMODULES_BYTES`] so an oversized file is detected. A missing file is
/// `None`; a symbolic link or a non-file is refused.
pub(super) fn read_worktree_gitmodules(root: &Path) -> Result<Option<Vec<u8>>, ProjectLoadError> {
    let path = root.join(".gitmodules");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ProjectLoadError::Symlink),
        Ok(metadata) if !metadata.is_file() => Err(ProjectLoadError::InvalidGitmodules),
        Ok(_) => {
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .map_err(|_| ProjectLoadError::InvalidGitmodules)?
                .take(MAX_GITMODULES_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ProjectLoadError::InvalidGitmodules)?;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ProjectLoadError::InvalidGitmodules),
    }
}
