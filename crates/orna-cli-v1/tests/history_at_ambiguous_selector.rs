//! `orna history --at SELECTOR` refuses a selector that names more than one
//! branch, tag or remote-tracking branch.
//!
//! Git resolves such a name by ref precedence and reports success, emitting
//! only a warning, so a read pinned with `--at amb` would silently return
//! whichever of the colliding refs it preferred and nothing in the listing
//! could report that choice. The refusal is typed and names the ambiguity, so
//! the user learns which spelling to use instead of reading the wrong snapshot.
//!
//! The repository here is a plain Git worktree. The ambiguity gate runs before
//! any format metadata or row is read, which is what these cases pin: the
//! selector is judged on the ref names alone.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[path = "support/orna_cli_run.rs"]
mod orna_cli_run;
use orna_cli_run::run_in;

/// A relation id that exists in no repository; these cases never reach a row.
const RELATION: &str = "00000000000000000000000000000001";

/// Initializes a worktree with one commit and returns it.
fn repository_with_one_commit() -> TempDir {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    run_git(root, &["init", "--quiet"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("main.orna"), b"pub table Music(id: Str) {}\n").unwrap();
    run_git(root, &["add", "--all"]);
    run_git(
        root,
        &[
            "-c",
            "user.email=kieran@drewett.dev",
            "-c",
            "user.name=kierandrewett",
            "commit",
            "--quiet",
            "--message",
            "one",
        ],
    );
    directory
}

fn run_git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("Git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_history(directory: &Path, selector: &str) -> std::process::Output {
    run_in(directory, &["history", RELATION, "song", "--at", selector])
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_name_carried_by_a_branch_and_a_tag_is_refused_as_ambiguous() {
    let directory = repository_with_one_commit();
    run_git(directory.path(), &["branch", "collide"]);
    run_git(directory.path(), &["tag", "collide"]);

    let output = run_history(directory.path(), "collide");

    assert_eq!(
        output.status.code(),
        Some(1),
        "an ambiguous selector must fail closed: {}",
        stderr_of(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be listed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("ambiguous"),
        "the refusal names the ambiguity rather than reporting a missing row: {stderr}"
    );
}

#[test]
fn a_slashed_name_carried_by_a_branch_and_a_tag_is_refused() {
    let directory = repository_with_one_commit();
    run_git(directory.path(), &["branch", "feature/x"]);
    run_git(directory.path(), &["tag", "feature/x"]);

    let output = run_history(directory.path(), "feature/x");

    assert_eq!(output.status.code(), Some(1), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("ambiguous"),
        "a slashed name collides just as a bare one does: {}",
        stderr_of(&output)
    );
}

#[test]
fn a_name_carried_by_one_ref_only_is_not_refused() {
    let directory = repository_with_one_commit();
    run_git(directory.path(), &["branch", "solo"]);

    let output = run_history(directory.path(), "solo");

    // The pinned read still fails (this worktree holds no format-3 row), but
    // it must not be refused for ambiguity: one ref carries the name.
    assert!(
        !stderr_of(&output).contains("ambiguous"),
        "a unique name is not ambiguous: {}",
        stderr_of(&output)
    );
}

#[test]
fn a_full_ref_name_is_not_refused() {
    let directory = repository_with_one_commit();
    run_git(directory.path(), &["branch", "collide"]);
    run_git(directory.path(), &["tag", "collide"]);

    let output = run_history(directory.path(), "refs/heads/collide");

    // Naming the namespace picks exactly one ref, so the collision no longer
    // applies and the failure that follows is a different one.
    assert!(
        !stderr_of(&output).contains("ambiguous"),
        "a full ref name is unambiguous: {}",
        stderr_of(&output)
    );
}

#[test]
fn head_is_not_refused() {
    let directory = repository_with_one_commit();

    let output = run_history(directory.path(), "HEAD");

    assert!(
        !stderr_of(&output).contains("ambiguous"),
        "HEAD names one object by construction: {}",
        stderr_of(&output)
    );
}
