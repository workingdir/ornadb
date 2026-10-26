//! Shared helpers for Git-backed repository fixtures.

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
