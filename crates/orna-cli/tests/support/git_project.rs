//! What the CLI tests of the `sys.git.*` members share: a committed project, the
//! `orna` binary run over it, and its REPL driven through standard input.

// Included by path from several test targets, each of which uses the part of
// the support surface it needs.
#![allow(dead_code)]

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

/// Runs `orna --db <directory> ARGUMENTS`.
pub fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

/// Runs `git` in `directory` under a fixed identity and returns its trimmed
/// standard output.
pub fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
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
        .output()
        .expect("git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

/// An initialised project with `source` staged as `main.orna` and no commit.
pub fn uncommitted_project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Orna Test"]);
    git(
        directory.path(),
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(directory.path(), &["add", "-A"]);
    directory
}

/// A project of `source` published with `orna commit` on a branch named `main`.
pub fn committed_project(source: &str) -> TempDir {
    let directory = uncommitted_project(source);
    let committed = orna(directory.path(), &["commit", "-m", "publish project"]);
    assert!(committed.status.success(), "commit: {committed:?}");
    if git(directory.path(), &["symbolic-ref", "--short", "HEAD"]) != "main" {
        git(directory.path(), &["branch", "-m", "main"]);
    }
    directory
}

/// Runs `orna --db <directory> repl`, feeds it `input` and `:quit`, and waits
/// for it to exit.
pub fn repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the REPL exits")
}

/// Everything `output` printed, standard output first.
pub fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Runs `main.FUNCTION` and returns what it printed.
pub fn run(directory: &Path, function: &str) -> String {
    let output = orna(directory, &["run", &format!("main.{function}")]);
    let text = combined(&output);
    assert!(output.status.success(), "{function}: {text}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}
