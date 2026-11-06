use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_repository::{Repository, RepositoryFormat};
use tempfile::TempDir;

const GOLDEN_BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn clone_golden_bundle(parent: &Path, name: &str) -> PathBuf {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_BUNDLE);
    let source = parent.join(name);
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(&bundle)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    source
}

/// The reviewed sibling copy `orna upgrade --to-format 3` prepares beside the
/// worktree `source`.
fn sibling_candidate(source: &Path) -> PathBuf {
    let name = source.file_name().unwrap().to_string_lossy().into_owned();
    source
        .parent()
        .unwrap()
        .join(format!("{name}-format3-candidate"))
}

/// Runs the one-step `orna upgrade --to-format 3` against `source`.
fn run_in_place(source: &Path, confirm: Option<&str>) -> Output {
    let mut arguments = vec![
        "--db".to_owned(),
        source.to_str().unwrap().to_owned(),
        "--debug".to_owned(),
        "upgrade".to_owned(),
        "--to-format".to_owned(),
        "3".to_owned(),
        "--no-persistent-consumers".to_owned(),
    ];
    if let Some(id) = confirm {
        arguments.push("--confirm".to_owned());
        arguments.push(id.to_owned());
    }
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(&arguments)
        .output()
        .unwrap()
}

/// Reads `candidate_commit=` from the review printed before publication.
fn candidate_from_review(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .find_map(|word| word.strip_prefix("candidate_commit="))
        .unwrap_or_else(|| panic!("the review names the candidate: {stdout}"))
        .to_owned()
}

fn journal_is_present(copy: &Path) -> bool {
    Repository::discover(copy)
        .unwrap()
        .read_publication_journal()
        .unwrap()
        .is_some()
}

fn repository_format(path: &Path) -> RepositoryFormat {
    Repository::discover(path)
        .unwrap()
        .open_format_context()
        .unwrap()
        .repository_format()
}

/// Asserts that a refused upgrade exited with status 1 and the E2100 diagnostic
/// titled `title`.
fn assert_refused(output: &Output, title: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(stderr.contains(title), "{stderr}");
}

/// Without confirmation the one-step upgrade prepares the sibling candidate,
/// reviews it, and refuses to publish. The worktree is unchanged and the
/// prepared copy keeps its journal for `migrate abort`.
#[test]
fn upgrade_in_place_without_confirmation_refuses_and_keeps_the_prepared_copy() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");
    let source_head = git_output(&source, &["rev-parse", "HEAD"]);
    let source_status = git_output(&source, &["status", "--porcelain"]);

    let output = run_in_place(&source, None);
    assert_refused(&output, "publication was not confirmed");
    // The review must name the candidate; this panics otherwise.
    candidate_from_review(&output);

    let sibling = sibling_candidate(&source);
    assert!(journal_is_present(&sibling), "the prepared copy is kept");
    assert_eq!(
        git_output(&source, &["rev-parse", "HEAD"]),
        source_head,
        "the worktree HEAD is unchanged"
    );
    assert_eq!(
        git_output(&source, &["status", "--porcelain"]),
        source_status
    );
    assert_eq!(
        repository_format(&source),
        RepositoryFormat::Legacy1,
        "the worktree stays in its source format"
    );
}

/// A `--confirm` naming a different candidate refuses before publication.
#[test]
fn upgrade_in_place_with_a_mismatched_confirmation_refuses_and_keeps_the_worktree() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");
    let source_head = git_output(&source, &["rev-parse", "HEAD"]);

    let prepared = run_in_place(&source, None);
    assert_refused(&prepared, "publication was not confirmed");
    let candidate = candidate_from_review(&prepared);

    let wrong = "0000000000000000000000000000000000000000";
    assert_ne!(wrong, candidate);
    let output = run_in_place(&source, Some(wrong));
    assert_refused(&output, "confirmation names a different candidate");
    assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        repository_format(&source),
        RepositoryFormat::Legacy1,
        "the worktree stays in its source format"
    );
    assert!(journal_is_present(&sibling_candidate(&source)));
}

/// A confirmed one-step upgrade publishes the reviewed candidate in place: the
/// worktree path holds the activated format-3 copy at the candidate commit, and
/// the old snapshot is kept at the sibling path as the rollback copy.
#[test]
fn upgrade_in_place_with_confirmation_publishes_the_candidate_in_place() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");
    let legacy_head = git_output(&source, &["rev-parse", "HEAD"]);

    let review = run_in_place(&source, None);
    assert_refused(&review, "publication was not confirmed");
    let candidate = candidate_from_review(&review);

    let output = run_in_place(&source, Some(&candidate));
    assert!(
        output.status.success(),
        "confirmed upgrade: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("published format-3 repository in place")),
        "{stdout}"
    );

    assert_eq!(
        repository_format(&source),
        RepositoryFormat::Format3,
        "the worktree path now holds the activated format-3 copy"
    );
    assert_eq!(
        git_output(&source, &["rev-parse", "HEAD"]),
        format!("{candidate}\n").into_bytes()
    );
    assert!(
        !journal_is_present(&source),
        "activation completes the publication journal"
    );

    let rollback = sibling_candidate(&source);
    assert_eq!(
        git_output(&rollback, &["rev-parse", "HEAD"]),
        legacy_head,
        "the rollback copy keeps the old snapshot"
    );
    assert_ne!(
        repository_format(&rollback),
        RepositoryFormat::Format3,
        "the rollback copy stays in the source format"
    );
}

/// `--yes-activate` belongs to the DEST form only; the one-step form refuses it
/// before anything is changed.
#[test]
fn upgrade_in_place_rejects_yes_activate_without_a_destination() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");
    let head = git_output(&source, &["rev-parse", "HEAD"]);

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "upgrade",
            "--to-format",
            "3",
            "--no-persistent-consumers",
            "--yes-activate",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E1002"), "{stderr}");
    assert!(stderr.contains("needs a DEST"), "{stderr}");
    assert!(!sibling_candidate(&source).exists());
    assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), head);
}

/// The one-step form requires an explicit consumer-continuity choice.
#[test]
fn upgrade_in_place_requires_the_consumer_continuity_choice() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "upgrade",
            "--to-format",
            "3",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E1001"), "{stderr}");
    assert!(stderr.contains("consumer-continuity"), "{stderr}");
    assert!(!sibling_candidate(&source).exists());
}

/// A linked worktree's administrative directory would be orphaned by the swap,
/// so the one-step form refuses it and creates no copy.
#[test]
fn upgrade_in_place_refuses_a_linked_worktree() {
    let parent = TempDir::new().unwrap();
    let source = clone_golden_bundle(parent.path(), "source");
    let linked = parent.path().join("linked");
    git_output(
        &source,
        &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
    );
    let linked_head = git_output(&linked, &["rev-parse", "HEAD"]);

    let output = run_in_place(&linked, None);
    assert_refused(&output, "in-place upgrade needs a standalone worktree");
    assert!(!sibling_candidate(&linked).exists());
    assert_eq!(git_output(&linked, &["rev-parse", "HEAD"]), linked_head);
    assert!(fs::metadata(linked.join(".git")).unwrap().is_file());
}
