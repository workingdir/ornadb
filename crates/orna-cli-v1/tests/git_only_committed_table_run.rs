//! ORNA-RUN-001/002: a project committed with plain git, without `orna commit`,
//! carries no object identity inventory. A run that touches its tables must fail
//! closed with the identity diagnostic and its `orna commit` remedy, not report
//! the committed rows as unreadable. The same project, once published with
//! `orna commit`, runs; a project with no commit at all passes through.

use std::{path::Path, process::Command};

use tempfile::TempDir;

const SOURCE: &str = r#"pub table Widget(id: Str) {
    label: Str,
}

pub fn seed() {
    Widget.insert({ id: "w-1", label: "first" });
}
"#;

fn orna(directory: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .status()
        .expect("git process");
    assert!(status.success(), "git {arguments:?}: {status:?}");
}

fn initialized_project() -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), SOURCE).expect("project source");
    git(directory.path(), &["init", "-q"]);
    directory
}

fn git_only_committed_project() -> TempDir {
    let directory = initialized_project();
    git(directory.path(), &["add", "-A"]);
    git(directory.path(), &["commit", "-q", "-m", "seed project"]);
    directory
}

#[test]
fn git_only_committed_table_run_reports_missing_identity_inventory() {
    let project = git_only_committed_project();

    let output = orna(project.path(), &["run", "main.seed"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        stderr.contains("E2200") && stderr.contains("committed table identities are unavailable"),
        "expected the identity diagnostic: {stderr}"
    );
    assert!(
        stderr.contains("run `orna commit -m MESSAGE`"),
        "expected the orna commit remedy: {stderr}"
    );
    assert!(
        !stderr.contains("committed table rows could not be read"),
        "the rows diagnostic must not mask a missing inventory: {stderr}"
    );
}

#[test]
fn git_only_committed_project_runs_after_orna_commit() {
    let project = initialized_project();
    git(project.path(), &["add", "-A"]);
    let committed = orna(project.path(), &["commit", "-m", "publish widgets"]);
    assert!(committed.status.success(), "commit: {committed:?}");

    let output = orna(project.path(), &["run", "main.seed"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(
        !stderr.contains("committed table identities are unavailable"),
        "a published project must not be refused: {stderr}"
    );
}

#[test]
fn unborn_head_project_passes_through_the_identity_refusal() {
    let project = initialized_project();

    let output = orna(project.path(), &["run", "main.seed"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("committed table identities are unavailable"),
        "an unborn HEAD carries no committed tables to refuse: {stderr}"
    );
    assert!(
        !stderr.contains("committed HEAD is unreadable"),
        "an unborn HEAD is not an unreadable HEAD: {stderr}"
    );
}
