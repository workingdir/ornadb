//! A reader that closes stdout early (`orna --help | head -0`) ends `orna`
//! quietly with status 0, instead of panicking or printing a diagnostic.

use std::process::{Command, Stdio};

/// A stdout pipe whose read end is already closed, as `head -0` leaves it.
fn closed_stdout() -> Stdio {
    let (reader, writer) = std::io::pipe().expect("create stdout pipe");
    drop(reader);
    Stdio::from(writer)
}

fn assert_quiet_closed_stdout_exit(output: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.is_empty(), "orna wrote to stderr: {stderr}");
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
}

#[test]
fn help_into_closed_stdout_exits_quietly() {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--help")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(closed_stdout())
        .stderr(Stdio::piped())
        .output()
        .expect("run orna --help");
    assert_quiet_closed_stdout_exit(&output);
}

#[test]
fn status_into_closed_stdout_exits_quietly() {
    let directory = tempfile::tempdir().expect("project directory");
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("run git init");
    assert!(git.success(), "git init failed");
    orna_repository::initialize_repository(directory.path()).expect("initialize Orna repository");
    // An untracked file makes `status --porcelain` produce output to write.
    std::fs::write(directory.path().join("untracked.orna"), "").expect("write file");

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["status", "--porcelain"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(closed_stdout())
        .stderr(Stdio::piped())
        .output()
        .expect("run orna status");
    assert_quiet_closed_stdout_exit(&output);
}
