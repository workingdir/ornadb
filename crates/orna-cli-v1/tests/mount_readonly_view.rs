//! Gate F: a format-1/2 repository opens read-only through the migration
//! reader alongside a format-3 mount in the same process, without sharing
//! state (profiles/vfs-1.md, ACCEPTANCE.md Gate F).
//!
//! The fixture repositories are real Git worktrees: the legacy one commits the
//! `format 1` metadata file, the format-3 one is created by `orna init` through
//! the same CLI. Both views are then attached and read in this one process, and
//! the CLI is driven end to end for the reported status.

#[path = "support/orna_cli_run.rs"]
mod orna_cli_run;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_cli_run::run_in;
use orna_repository_v1::{MountView, Repository};
use tempfile::TempDir;

const LEGACY_FORMAT_METADATA: &str = ".orna/format.orna";
const LEGACY_SOURCE: &str = include_str!("fixtures/mount/legacy-main.orna");

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(directory: &Path, message: &str) -> String {
    git(directory, &["add", "--all"]);
    git(directory, &["commit", "--quiet", "-m", message]);
    git(directory, &["rev-parse", "HEAD"])
}

/// A real format-1 repository: the legacy metadata file plus an ordinary
/// source fixture, committed through Git.
fn legacy_repository() -> (TempDir, String) {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet", "-b", "main"]);
    git(root, &["config", "user.email", "kieran@drewett.dev"]);
    git(root, &["config", "user.name", "kierandrewett"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), LEGACY_SOURCE).unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(root.join(LEGACY_FORMAT_METADATA), "format 1\n").unwrap();
    let head = commit(root, "legacy format 1 fixture");
    (directory, head)
}

/// A real format-3 repository created by the CLI's own `init`, so the writer
/// coordinate is whatever production writes rather than a test fabrication.
fn format3_repository() -> (TempDir, String) {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet", "-b", "main"]);
    git(root, &["config", "user.email", "kieran@drewett.dev"]);
    git(root, &["config", "user.name", "kierandrewett"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    let output = run_in(root, &["init"]);
    assert!(
        output.status.success(),
        "orna init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let head = commit(root, "format 3 fixture");
    (directory, head)
}

fn mountpoint(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir_all(&path).unwrap();
    path
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

#[test]
fn legacy_and_format3_views_attach_in_one_process_without_sharing_state() {
    let (legacy, legacy_head) = legacy_repository();
    let (workspace, workspace_head) = format3_repository();
    let mounts = TempDir::new().unwrap();
    let legacy_mountpoint = mountpoint(mounts.path(), "old");
    let workspace_mountpoint = mountpoint(mounts.path(), "library");

    let legacy_repository = Repository::discover(legacy.path()).unwrap();
    let workspace_repository = Repository::discover(workspace.path()).unwrap();

    // Both views live at the same time in this process. Each resolves its own
    // selector once and keeps its own format coordinate and identity.
    let legacy_view = MountView::attach(&legacy_repository, &legacy_mountpoint, &legacy_head)
        .expect("the format-1 migration reader attaches read-only");
    let workspace_view = MountView::attach(
        &workspace_repository,
        &workspace_mountpoint,
        &workspace_head,
    )
    .expect("the format-3 workspace view attaches read-only");

    assert_eq!(legacy_view.repository_format_number(), 1);
    assert!(legacy_view.is_legacy_format());
    assert!(legacy_view.is_read_only());
    assert_eq!(workspace_view.repository_format_number(), 3);
    assert!(!workspace_view.is_legacy_format());
    assert!(workspace_view.is_read_only());

    // No shared state: distinct snapshots, distinct view identities, and
    // neither repository's registry reports the other's view.
    assert_ne!(legacy_view.snapshot_id(), workspace_view.snapshot_id());
    assert_ne!(legacy_view.view_id(), workspace_view.view_id());

    let legacy_status = legacy_repository.mount_status().unwrap();
    assert_eq!(
        legacy_status.len(),
        1,
        "the legacy registry reports one view"
    );
    assert_eq!(legacy_status[0].mountpoint(), legacy_mountpoint.as_path());
    assert_eq!(legacy_status[0].repository_format_number(), 1);
    assert!(legacy_status[0].is_legacy_format());

    let workspace_status = workspace_repository.mount_status().unwrap();
    assert_eq!(
        workspace_status.len(),
        1,
        "the workspace registry reports one view"
    );
    assert_eq!(
        workspace_status[0].mountpoint(),
        workspace_mountpoint.as_path()
    );
    assert_eq!(workspace_status[0].repository_format_number(), 3);
    assert!(!workspace_status[0].is_legacy_format());

    // The attached view still resolves its own immutable snapshot after the
    // workspace moves: a later commit on the format-3 repository does not
    // retarget the attached legacy view.
    fs::write(workspace.path().join("later.txt"), "later\n").unwrap();
    commit(workspace.path(), "move the workspace forward");
    assert_eq!(workspace_repository.mount_status().unwrap().len(), 1);
    assert_eq!(legacy_view.repository_format_number(), 1);
    assert_eq!(workspace_view.repository_format_number(), 3);
}

#[test]
fn cli_reports_and_releases_a_readonly_view_of_each_format() {
    let (legacy, legacy_head) = legacy_repository();
    let (workspace, _) = format3_repository();
    let mounts = TempDir::new().unwrap();
    let legacy_mountpoint = mountpoint(mounts.path(), "old");
    let workspace_mountpoint = mountpoint(mounts.path(), "library");

    // `mount DIR --at SELECTOR` on the format-1 repository.
    let output = run_in(
        legacy.path(),
        &[
            "mount",
            legacy_mountpoint.to_str().unwrap(),
            "--at",
            &legacy_head,
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let mounted = stdout(&output);
    assert!(mounted.contains("format 1"), "legacy format 1: {mounted}");
    assert!(
        mounted.contains("legacy true"),
        "migration reader: {mounted}"
    );
    assert!(mounted.contains("read-only"), "read-only view: {mounted}");

    // `mount DIR --at SELECTOR` on the format-3 repository.
    let output = run_in(
        workspace.path(),
        &[
            "mount",
            workspace_mountpoint.to_str().unwrap(),
            "--at",
            "HEAD",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let mounted = stdout(&output);
    assert!(mounted.contains("format 3"), "format 3 view: {mounted}");
    assert!(
        mounted.contains("legacy false"),
        "workspace view: {mounted}"
    );

    // Each repository reports only its own attached view.
    let output = run_in(legacy.path(), &["mount", "status", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let reported = stdout(&output);
    assert!(
        reported.contains(legacy_mountpoint.to_str().unwrap()),
        "legacy status: {reported}"
    );
    assert!(
        !reported.contains(workspace_mountpoint.to_str().unwrap()),
        "the legacy registry must not report the workspace view: {reported}"
    );
    assert!(
        reported.contains("\"format\":1"),
        "legacy status: {reported}"
    );
    assert!(
        reported.contains("\"legacy\":true"),
        "legacy status: {reported}"
    );
    assert!(
        reported.contains("\"read_only\":true"),
        "legacy status: {reported}"
    );

    let output = run_in(workspace.path(), &["mount", "status", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let reported = stdout(&output);
    assert!(
        reported.contains(workspace_mountpoint.to_str().unwrap()),
        "workspace status: {reported}"
    );
    assert!(
        !reported.contains(legacy_mountpoint.to_str().unwrap()),
        "the workspace registry must not report the legacy view: {reported}"
    );
    assert!(
        reported.contains("\"format\":3"),
        "workspace status: {reported}"
    );

    // `unmount DIR` releases only the record; the snapshot and workspace stay.
    let output = run_in(
        legacy.path(),
        &["unmount", legacy_mountpoint.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let output = run_in(legacy.path(), &["mount", "status"]);
    assert_eq!(stdout(&output), "no mounts", "released view: {output:?}");
    assert!(
        legacy.path().join(LEGACY_FORMAT_METADATA).is_file(),
        "unmount must not touch the backing worktree"
    );
    assert_eq!(
        fs::read_to_string(legacy.path().join(LEGACY_FORMAT_METADATA)).unwrap(),
        "format 1\n",
        "the legacy metadata file is preserved byte for byte"
    );
}

#[test]
fn mount_rejects_an_unresolvable_selector_and_a_missing_selector() {
    let (legacy, _) = legacy_repository();
    let mounts = TempDir::new().unwrap();
    let target = mountpoint(mounts.path(), "old");

    // A selector that does not resolve exits 1 and attaches nothing.
    let output = run_in(
        legacy.path(),
        &[
            "mount",
            target.to_str().unwrap(),
            "--at",
            "0000000000000000000000000000000000000000",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let repository = Repository::discover(legacy.path()).unwrap();
    assert!(
        repository.mount_status().unwrap().is_empty(),
        "a refused mount must not be recorded"
    );

    // Exactly one selector is required.
    let output = run_in(legacy.path(), &["mount", target.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
}
