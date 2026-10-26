//! Writes the legacy format-1 golden repository and packs it as a git bundle.
//!
//! Usage:
//! `cargo run -p orna-repository-v1 --example build_legacy_fixture \
//!     --features legacy-fixture-writer -- <empty-root> <bundle-path>`

use std::{path::PathBuf, process::Command};

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let usage = "usage: build_legacy_fixture <empty-root> <bundle-path>";
    let root = PathBuf::from(arguments.next().expect(usage));
    let bundle = PathBuf::from(arguments.next().expect(usage));
    assert!(arguments.next().is_none(), "{usage}");

    orna_repository_v1::write_legacy_format1_fixture(&root);

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
