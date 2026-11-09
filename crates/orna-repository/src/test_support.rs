//! Shared helpers for Git-backed repository fixtures.

// Included by path from several test targets, each of which uses the part of
// the support surface it needs.
#![allow(dead_code)]

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

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

/// Runs one fixture Git command and returns its trimmed UTF-8 stdout.
pub(crate) fn git(directory: &Path, arguments: &[&str]) -> String {
    run_fixture_git(directory, arguments, false)
}

/// Runs one fixture Git command that is expected to produce no output under
/// test, asserting success.
pub(crate) fn git_ok(directory: &Path, arguments: &[&str]) {
    let _ = run_fixture_git(directory, arguments, false);
}

/// Runs one fixture Git command with lazy fetching disabled, returning its
/// trimmed UTF-8 stdout. Fixtures that build unreachable objects must not let
/// Git ask a configured remote for them.
pub(crate) fn git_no_lazy(directory: &Path, arguments: &[&str]) -> String {
    run_fixture_git(directory, arguments, true)
}

fn run_fixture_git(directory: &Path, arguments: &[&str], no_lazy_fetch: bool) -> String {
    let mut command = Command::new("git");
    command.current_dir(directory).args(arguments);
    if no_lazy_fetch {
        command.env("GIT_NO_LAZY_FETCH", "1");
    }
    let output = command.output().expect("fixture git must start");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture git output is UTF-8")
        .trim()
        .to_owned()
}

/// Runs one Git command with `input` on stdin and returns its trimmed stdout.
pub(crate) fn git_with_input(directory: &Path, arguments: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start fixture git");
    child
        .stdin
        .take()
        .expect("fixture git stdin")
        .write_all(input.as_bytes())
        .expect("write fixture git stdin");
    let output = child.wait_with_output().expect("wait for fixture git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture git output is UTF-8")
        .trim()
        .to_owned()
}

/// Creates a commit (not referenced by any ref) whose tree nests one file
/// `depth` directories deep. Each level holds a single entry, so Git reads the
/// tree cheaply while a committed tree walk refuses it by depth alone.
pub(crate) fn deep_nested_commit(directory: &Path, depth: usize) -> String {
    let blob = git_with_input(directory, &["hash-object", "-w", "--stdin"], "deep\n");
    let mut tree = git_with_input(
        directory,
        &["mktree"],
        &format!("100644 blob {blob}\tfile\n"),
    );
    for _ in 0..depth {
        tree = git_with_input(
            directory,
            &["mktree"],
            &format!("040000 tree {tree}\tdeep\n"),
        );
    }
    git_with_input(directory, &["commit-tree", &tree, "-m", "deep"], "")
}
