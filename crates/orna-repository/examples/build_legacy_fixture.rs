//! Writes the legacy format-1 golden repository and packs it as a git bundle.
//!
//! Usage:
//! `cargo run -p orna-repository --example build_legacy_fixture \
//!     --features legacy-fixture-writer -- <empty-root> <bundle-path> <loose-source-dir>`
//!
//! `<loose-source-dir>` holds the editable table's declaring module and row
//! units. They live in the consuming crate's fixture tree (a test fixture may
//! not be included across crates), so the builder reads them from this path:
//! `crates/orna-cli/tests/fixtures`.

use std::{path::PathBuf, process::Command};

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let usage = "usage: build_legacy_fixture <empty-root> <bundle-path> <loose-source-dir>";
    let root = PathBuf::from(arguments.next().expect(usage));
    let bundle = PathBuf::from(arguments.next().expect(usage));
    let loose_sources = PathBuf::from(arguments.next().expect(usage));
    assert!(arguments.next().is_none(), "{usage}");

    orna_repository::write_legacy_format1_golden_fixture(&root, &loose_sources);

    let status = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(&root)
        .args(["bundle", "create"])
        .arg(&bundle)
        .arg("--all")
        .status()
        .expect("spawn git bundle create");
    assert!(status.success(), "git bundle create failed: {status}");
}
