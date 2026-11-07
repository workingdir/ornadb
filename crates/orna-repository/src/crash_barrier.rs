//! Process-kill barriers at publication and capture durability boundaries.
//!
//! Compiled to a no-op unless the non-default `crash-barriers` feature is
//! enabled. Only feature-enabled test builds can read the harness environment,
//! so release binaries contain neither the arming code nor its variable names.
//!
//! With the feature enabled, a harness sets `ORNA_TEST_PUBLICATION_CRASH_AT` to
//! the exact boundary name and `ORNA_TEST_PUBLICATION_CRASH_DIR` to a directory
//! it owns. When the selected boundary is reached, the process publishes
//! `publication-crash-marker` in that directory exclusively (failing rather than
//! overwriting) and parks forever so the harness can SIGKILL it at that point.
//! The marker becomes visible under its final name only after it holds the
//! complete boundary name, so a harness that sees the marker can read it.
//!
//! `publication_crash_pause_until_released` fires once per process at its
//! boundary. When the harness also sets `ORNA_TEST_PUBLICATION_CRASH_RELEASE`
//! to a file path, the process resumes once that file exists, so the harness
//! can run work such as garbage collection while the process is paused. Without
//! that variable it parks forever, exactly like `publication_crash_barrier`.

#[cfg(feature = "crash-barriers")]
mod armed {
    use std::{
        io::Write,
        path::{Path, PathBuf},
        sync::{
            LazyLock,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
    const CRASH_DIR: &str = "ORNA_TEST_PUBLICATION_CRASH_DIR";
    const CRASH_RELEASE: &str = "ORNA_TEST_PUBLICATION_CRASH_RELEASE";
    /// Fixed marker name inside the harness-named directory.
    pub const MARKER_FILE_NAME: &str = "publication-crash-marker";

    /// Environment is read once per process.
    static SELECTED: LazyLock<Option<(String, PathBuf)>> = LazyLock::new(|| {
        let boundary = std::env::var(CRASH_AT).ok()?;
        let directory = PathBuf::from(std::env::var_os(CRASH_DIR)?);
        Some((boundary, directory.join(MARKER_FILE_NAME)))
    });

    /// Release file that resumes a paused barrier. Unset means park forever.
    static RELEASE: LazyLock<Option<PathBuf>> =
        LazyLock::new(|| std::env::var_os(CRASH_RELEASE).map(PathBuf::from));
    /// A pause fires once per process, so resumed work is never paused again.
    static PAUSE_FIRED: AtomicBool = AtomicBool::new(false);

    pub fn publication_crash_barrier(boundary: &str) {
        let Some((selected, marker)) = &*SELECTED else {
            return;
        };
        if selected != boundary {
            return;
        }
        publish_marker_exclusively(marker, boundary);
        loop {
            thread::park();
        }
    }

    /// Pauses at `boundary` the first time it is reached in this process,
    /// publishing the marker first. Resumes when the release file exists, or
    /// parks forever when no release file was configured.
    pub fn publication_crash_pause_until_released(boundary: &str) {
        let Some((selected, marker)) = &*SELECTED else {
            return;
        };
        if selected != boundary || PAUSE_FIRED.swap(true, Ordering::SeqCst) {
            return;
        }
        publish_marker_exclusively(marker, boundary);
        let Some(release) = &*RELEASE else {
            loop {
                thread::park();
            }
        };
        while !release.exists() {
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Writes and syncs the boundary name in a private staging file, then
    /// hard-links it to `marker`. `hard_link` fails when `marker` already
    /// exists, preserving exclusivity, and the final name never refers to a
    /// partially written file. The staging name is removed after publication.
    fn publish_marker_exclusively(marker: &Path, boundary: &str) {
        let directory = marker.parent().expect("marker has a parent directory");
        let staging = directory.join(format!(".{MARKER_FILE_NAME}.{}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .expect("create staged publication crash marker");
        file.write_all(boundary.as_bytes())
            .expect("write staged publication crash marker");
        file.sync_all()
            .expect("sync staged publication crash marker");
        drop(file);
        std::fs::hard_link(&staging, marker).expect("publish publication crash marker");
        std::fs::remove_file(&staging).expect("remove staged publication crash marker");
    }
}

/// Parks the calling process at `boundary` when the `crash-barriers` test
/// feature is enabled and the harness selected that boundary.
#[cfg(feature = "crash-barriers")]
pub fn publication_crash_barrier(boundary: &str) {
    armed::publication_crash_barrier(boundary);
}

/// Pauses the calling process at `boundary` the first time it is reached when
/// the `crash-barriers` test feature is enabled and the harness selected that
/// boundary. Resumes once the harness's release file exists, or parks forever
/// when no release file was configured.
#[cfg(feature = "crash-barriers")]
pub fn publication_crash_pause_until_released(boundary: &str) {
    armed::publication_crash_pause_until_released(boundary);
}

/// Release-build form: no environment access and no code path.
#[cfg(not(feature = "crash-barriers"))]
#[inline(always)]
pub fn publication_crash_barrier(_boundary: &str) {}

/// Release-build form of the pause: no environment access and no code path.
#[cfg(not(feature = "crash-barriers"))]
#[inline(always)]
pub fn publication_crash_pause_until_released(_boundary: &str) {}
