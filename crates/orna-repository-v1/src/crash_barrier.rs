//! Process-kill barriers at publication and capture durability boundaries.
//!
//! Compiled to a no-op unless the non-default `crash-barriers` feature is
//! enabled. Only feature-enabled test builds can read the harness environment,
//! so release binaries contain neither the arming code nor its variable names.
//!
//! With the feature enabled, a harness sets `ORNA_TEST_PUBLICATION_CRASH_AT` to
//! the exact boundary name and `ORNA_TEST_PUBLICATION_CRASH_DIR` to a directory
//! it owns. When the selected boundary is reached, the process creates
//! `publication-crash-marker` in that directory exclusively (failing rather than
//! overwriting) and parks forever so the harness can SIGKILL it at that point.

#[cfg(feature = "crash-barriers")]
mod armed {
    use std::{io::Write, path::PathBuf, sync::LazyLock, thread};

    const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
    const CRASH_DIR: &str = "ORNA_TEST_PUBLICATION_CRASH_DIR";
    /// Fixed marker name inside the harness-named directory.
    pub const MARKER_FILE_NAME: &str = "publication-crash-marker";

    /// Environment is read once per process.
    static SELECTED: LazyLock<Option<(String, PathBuf)>> = LazyLock::new(|| {
        let boundary = std::env::var(CRASH_AT).ok()?;
        let directory = PathBuf::from(std::env::var_os(CRASH_DIR)?);
        Some((boundary, directory.join(MARKER_FILE_NAME)))
    });

    pub fn publication_crash_barrier(boundary: &str) {
        let Some((selected, marker)) = &*SELECTED else {
            return;
        };
        if selected != boundary {
            return;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .expect("create new publication crash marker");
        file.write_all(boundary.as_bytes())
            .expect("write publication crash marker");
        file.sync_all().expect("sync publication crash marker");
        loop {
            thread::park();
        }
    }
}

/// Parks the calling process at `boundary` when the `crash-barriers` test
/// feature is enabled and the harness selected that boundary.
#[cfg(feature = "crash-barriers")]
pub fn publication_crash_barrier(boundary: &str) {
    armed::publication_crash_barrier(boundary);
}

/// Release-build form: no environment access and no code path.
#[cfg(not(feature = "crash-barriers"))]
#[inline(always)]
pub fn publication_crash_barrier(_boundary: &str) {}
