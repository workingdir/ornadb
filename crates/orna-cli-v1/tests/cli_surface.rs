//! CLI surface contract (W38): documented Git-native commands run against the
//! selected repository, unavailable workflow commands are refused at parse time
//! with a usage error, and malformed publish flags name the offending word.

use std::process::{Command, Output};

use tempfile::tempdir;

fn orna(directory: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("CLI starts")
}

fn git(repository: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .expect("git starts")
}

fn git_succeeds(repository: &std::path::Path, arguments: &[&str]) {
    let output = git(repository, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unavailable_workflow_commands_are_usage_refusals_before_any_repository_opens() {
    let directory = tempdir().expect("temporary directory");
    for command in [
        "checkout", "switch", "merge", "reset", "pull", "stash", "fmt", "verify", "prune",
        "upgrade", "restore",
    ] {
        let output = orna(directory.path(), &[command]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{command}: {stderr}");
        assert!(stderr.contains("E1002"), "{command}: {stderr}");
        assert!(
            stderr.contains(&format!("`{command}` is not available in this build")),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn restore_without_the_staged_row_form_is_refused() {
    let directory = tempdir().expect("temporary directory");
    let output = orna(directory.path(), &["restore", "--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E1002"), "{stderr}");
}

#[test]
fn publish_unknown_flag_is_a_usage_error_that_names_the_flag() {
    let directory = tempdir().expect("temporary directory");
    let output = orna(directory.path(), &["publish", "--bogus"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E1002"), "{stderr}");
    assert!(stderr.contains("--bogus"), "{stderr}");
}

#[test]
fn branch_runs_git_against_the_db_repository_not_the_current_directory() {
    let repository = tempdir().expect("repository");
    let elsewhere = tempdir().expect("working directory outside the repository");
    git_succeeds(repository.path(), &["init", "--quiet", "--initial-branch=cli-surface"]);
    git_succeeds(repository.path(), &["config", "user.email", "kieran@drewett.dev"]);
    git_succeeds(repository.path(), &["config", "user.name", "kierandrewett"]);
    git_succeeds(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "baseline"],
    );

    let native = git(repository.path(), &["branch", "--show-current"]);
    let db = repository.path().to_str().expect("utf-8 repository path");
    let cli = orna(elsewhere.path(), &["--db", db, "branch", "--show-current"]);

    assert_eq!(cli.status.code(), Some(0), "{:?}", String::from_utf8_lossy(&cli.stderr));
    assert_eq!(cli.stdout, native.stdout);
    assert_eq!(String::from_utf8_lossy(&cli.stdout).trim(), "cli-surface");
}

#[test]
fn branch_name_creates_a_branch_at_head_and_makes_no_commit() {
    let repository = tempdir().expect("repository");
    git_succeeds(
        repository.path(),
        &["init", "--quiet", "--initial-branch=cli-surface"],
    );
    git_succeeds(
        repository.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git_succeeds(repository.path(), &["config", "user.name", "kierandrewett"]);
    git_succeeds(
        repository.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "baseline"],
    );
    let db = repository.path().to_str().expect("utf-8 repository path");
    let head_before = git(repository.path(), &["rev-parse", "HEAD"]);

    let cli = orna(repository.path(), &["--db", db, "branch", "feature-x"]);
    assert_eq!(
        cli.status.code(),
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(cli.stdout.is_empty(), "branch creation prints nothing");

    let created = git(repository.path(), &["rev-parse", "refs/heads/feature-x"]);
    assert_eq!(created.stdout, head_before.stdout);
    let head_after = git(repository.path(), &["rev-parse", "HEAD"]);
    assert_eq!(head_after.stdout, head_before.stdout);
    let current = git(repository.path(), &["branch", "--show-current"]);
    assert_eq!(
        String::from_utf8_lossy(&current.stdout).trim(),
        "cli-surface"
    );

    let again = orna(repository.path(), &["--db", db, "branch", "feature-x"]);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert_eq!(again.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(stderr.contains("already exists"), "{stderr}");

    let reserved = orna(repository.path(), &["--db", db, "branch", "HEAD"]);
    let stderr = String::from_utf8_lossy(&reserved.stderr);
    assert_eq!(reserved.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E1002"), "{stderr}");
}
