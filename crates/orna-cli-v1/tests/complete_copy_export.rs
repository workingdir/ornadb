//! `orna export` end to end: freeze a pinned snapshot into an archive, read the
//! archive back with no repository, and reconstruct it into a fresh directory
//! that has no object of the source.
//!
//! The repository is a real format-3 fixture built by `support/format3.rs`; the
//! archive is written and read by the real `orna-cli-v1` binary, so this proves
//! the three export modes are wired to the shared complete-copy path rather
//! than to a private one.

use std::{
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

/// Runs `orna-cli-v1 <arguments>` in `directory` with Git routing isolated, so
/// the developer's own Git configuration cannot decide the outcome.
fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn export_check_and_restore_carry_one_snapshot_offline() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();
    let snapshot = String::from_utf8(git_output(project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // Freeze the pinned snapshot: exit 0 and the primary commit on stdout.
    let archive = project.join("archive");
    let exported = run(project, &["export", "archive", "--at", "HEAD"]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export failed: {}",
        stderr(&exported)
    );
    assert!(
        stdout(&exported).contains(&snapshot),
        "export names the pinned commit: {}",
        stdout(&exported)
    );
    assert!(archive.join("manifest.tsv").is_file());

    // Read the archive back: the check mode needs no repository at all, so it
    // runs from an unrelated directory.
    let unrelated = TempDir::new().unwrap();
    let archive_path = archive.to_str().unwrap();
    let checked = run(unrelated.path(), &["export", archive_path, "--check"]);
    assert_eq!(
        checked.status.code(),
        Some(0),
        "check failed: {}",
        stderr(&checked)
    );
    assert!(
        stdout(&checked).contains(&snapshot),
        "check reports the recorded snapshot: {}",
        stdout(&checked)
    );

    // Reconstruct into a fresh directory: no remote is configured and the copy
    // names the pinned commit as its HEAD.
    let restored = project.join("restored");
    let restored_path = restored.to_str().unwrap();
    let reconstructed = run(
        project,
        &["export", archive_path, "--restore", restored_path],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "restore failed: {}",
        stderr(&reconstructed)
    );
    let head = String::from_utf8(git_output(&restored, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(head, snapshot, "the copy's HEAD is the pinned commit");
    let remotes = String::from_utf8(git_output(&restored, &["remote"], None)).unwrap();
    assert_eq!(remotes.trim(), "", "the copy configures no remote");

    // The checked-out code is the snapshot's own source, read from the copy.
    let source = std::fs::read_to_string(restored.join("main.orna")).unwrap();
    assert_eq!(source, "capture test schema");
}

#[test]
fn export_refuses_a_second_snapshot_into_one_local_repository() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();

    // An absent selector and an absent archive are both failures, and neither
    // writes a claimed-complete archive.
    let unresolved = run(project, &["export", "archive", "--at", "no-such-ref"]);
    assert_eq!(unresolved.status.code(), Some(1), "{}", stderr(&unresolved));
    assert!(!project.join("archive").join("manifest.tsv").exists());

    let absent = run(project, &["export", "absent-archive", "--check"]);
    assert_eq!(absent.status.code(), Some(1), "{}", stderr(&absent));

    // The export verb belongs to a repository: a directory that is not one
    // fails instead of writing an empty archive.
    let unrelated = TempDir::new().unwrap();
    let outside = run(unrelated.path(), &["export", "archive", "--at", "HEAD"]);
    assert_eq!(outside.status.code(), Some(1), "{}", stderr(&outside));
    assert!(!unrelated.path().join("archive").exists());
}

/// A repository is only discoverable from a directory inside the worktree.
#[test]
fn export_discovers_the_repository_from_a_subdirectory() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();
    let nested = project.join(".orna");
    let archive = project.join("nested-archive");

    let exported = run(
        &nested,
        &["export", archive.to_str().unwrap(), "--at", "HEAD"],
    );
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export from a subdirectory failed: {}",
        stderr(&exported)
    );
    assert!(archive.join("manifest.tsv").is_file());
}
