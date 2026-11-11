//! Staging: a private copy of the verified bytes, which the loaded library owns
//! (docs/design/native-libraries.md, section 8). Linux stages in an anonymous
//! `memfd`; other platforms stage in a private temporary file, which narrows the
//! race between check and load but does not remove it.

use std::io;
use std::path::{Path, PathBuf};

/// A staged copy. Its path is what the loader opens; the copy stays alive while
/// this value does.
pub(crate) struct Staged {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    _memfd: std::fs::File,
    #[cfg(not(target_os = "linux"))]
    _temp: tempfile::NamedTempFile,
}

impl Staged {
    /// The path the loader opens: `/proc/self/fd/<n>` on Linux, a temporary file
    /// elsewhere.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "linux")]
impl Staged {
    pub(crate) fn new(bytes: &[u8]) -> io::Result<Self> {
        let (file, path) = crate::ffi::memfd(bytes)?;
        Ok(Self { path, _memfd: file })
    }
}

#[cfg(not(target_os = "linux"))]
impl Staged {
    pub(crate) fn new(bytes: &[u8]) -> io::Result<Self> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(bytes)?;
        file.flush()?;
        let path = file.path().to_owned();
        Ok(Self { path, _temp: file })
    }
}
