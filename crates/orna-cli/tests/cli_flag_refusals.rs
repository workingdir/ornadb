//! Git options that `fetch`, `push` and `commit` do not implement are refused
//! by name at the binary boundary (ORNA-CLI-001), not read as a remote,
//! branch or missing message. The refusals happen before any repository is
//! opened, so an empty directory suffices.

use std::process::{Command, Output};

/// The usage exit status (ORNA-CMD-ARGUMENT-INVALID, exit 2).
const USAGE_EXIT_STATUS: i32 = 2;

fn orna(arguments: &[&str]) -> Output {
    let directory = tempfile::tempdir().expect("empty directory");
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna")
}

fn assert_refused_by_name(arguments: &[&str], flag: &str) {
    let output = orna(arguments);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(USAGE_EXIT_STATUS),
        "orna {arguments:?}: {stderr}"
    );
    assert!(
        stderr.contains(&format!("does not accept `{flag}`")),
        "orna {arguments:?} names {flag}: {stderr}"
    );
}

#[test]
fn fetch_refuses_all_by_name_instead_of_reading_it_as_a_remote() {
    assert_refused_by_name(&["fetch", "--all"], "--all");
}

#[test]
fn push_refuses_upstream_flag_by_name_instead_of_reading_it_as_a_remote() {
    assert_refused_by_name(&["push", "-u", "origin", "master"], "-u");
}

#[test]
fn commit_refuses_all_flag_by_name_even_with_a_message() {
    assert_refused_by_name(&["commit", "-a", "-m", "x"], "-a");
}

#[test]
fn commit_accepts_the_git_message_spelling_before_looking_for_a_repository() {
    // The message is accepted, so the refusal comes from the missing
    // repository, not from argument parsing.
    let output = orna(&["commit", "--message=x"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("requires a message"),
        "--message=x is a message: {stderr}"
    );
}
