//! A repository written by the pre-qualified-storage build, committed as a git
//! bundle with its runtime `state.db` (ornadb-5il1.199).
//!
//! The bundle was produced by the `origin/main` CLI before module tables were
//! stored under their qualified names: `a.Item` is stored as the bare `Item`
//! row. The committed state holds that row, so the current build must refuse
//! every invocation of the project with E2200 instead of reading past it or
//! forking the relation. Both artifacts are pinned by SHA-256 so they cannot
//! drift silently.

use std::{path::Path, process::Command};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-bare-module.bundle";
const STATE: &str = "tests/fixtures/legacy-bare-module-state.sqlite3";
/// SHA-256 of `BUNDLE`, produced by the pre-qualified-storage CLI.
const BUNDLE_SHA256: &str = "092852be93fa4cbea616c1168b707d6bc07c6bbf6dd3d492dd22cd0b643000b8";
/// SHA-256 of `STATE`, the runtime store that holds the bare `Item` row.
const STATE_SHA256: &str = "fca574faa6a1e60824a2099b023b7da5efa2bb1fa9c40a14a13681d93211e5ab";

fn fixture(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn legacy_bare_module_repository_is_refused_with_e2200() {
    assert_eq!(
        sha256_hex(&fixture(BUNDLE)),
        BUNDLE_SHA256,
        "committed legacy bundle digest changed"
    );
    assert_eq!(
        sha256_hex(&fixture(STATE)),
        STATE_SHA256,
        "committed legacy state digest changed"
    );

    let parent = TempDir::new().unwrap();
    let project = parent.path().join("project");
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(fixture(BUNDLE))
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    std::fs::create_dir_all(project.join(".git/orna")).unwrap();
    std::fs::copy(fixture(STATE), project.join(".git/orna/state.db")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("--db")
        .arg(&project)
        .args(["run", "main.ping"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "legacy store must be refused: {stderr}");
    assert!(stderr.contains("E2200"), "refusal must carry E2200: {stderr}");
}
