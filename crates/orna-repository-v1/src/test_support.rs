//! Shared helpers for Git-backed repository fixtures.

use std::{path::Path, process::Command};

/// Configures the repository-approved identity used by fixture commits.
/// Fixture commit authorship is not under test; proxy policy and assertions
/// remain unchanged.
pub(crate) fn configure_fixture_git_identity(directory: &Path) {
    const IDENTITY: [(&str, &str); 2] = [
        ("user.name", "kierandrewett"),
        ("user.email", "kieran@drewett.dev"),
    ];

    for (key, value) in IDENTITY {
        let output = Command::new("git")
            .current_dir(directory)
            .args(["config", key, value])
            .output()
            .expect("fixture Git identity configuration must start");
        assert!(
            output.status.success(),
            "git config {key} {value}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
