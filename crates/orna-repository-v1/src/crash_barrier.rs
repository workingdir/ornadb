//! Process-kill barriers at publication and capture durability boundaries.
//!
//! The barrier is inert unless a test harness sets both
//! `ORNA_TEST_PUBLICATION_CRASH_AT` (the exact boundary name) and
//! `ORNA_TEST_PUBLICATION_CRASH_MARKER` (a file path). When the selected
//! boundary is reached, the marker receives the boundary name and the process
//! parks forever so the harness can SIGKILL it at that exact point.

use std::{path::PathBuf, sync::LazyLock, thread};

const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
const CRASH_MARKER: &str = "ORNA_TEST_PUBLICATION_CRASH_MARKER";

struct SelectedBarrier {
    boundary: String,
    marker: PathBuf,
}

/// Environment is read once per process so production paths pay only a cached
/// load when no barrier is configured.
static SELECTED: LazyLock<Option<SelectedBarrier>> = LazyLock::new(|| {
    let boundary = std::env::var(CRASH_AT).ok()?;
    let marker = std::env::var_os(CRASH_MARKER)?;
    Some(SelectedBarrier {
        boundary,
        marker: PathBuf::from(marker),
    })
});

/// Parks the calling thread when `boundary` is the harness-selected crash
/// point.
pub fn publication_crash_barrier(boundary: &str) {
    let Some(selected) = &*SELECTED else {
        return;
    };
    if selected.boundary != boundary {
        return;
    }
    std::fs::write(&selected.marker, boundary).expect("write publication crash marker");
    loop {
        thread::park();
    }
}
