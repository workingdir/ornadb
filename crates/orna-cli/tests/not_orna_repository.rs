use std::process::{Command, Output};

use tempfile::TempDir;

const REJECTION_CODE: &str = "ORNA-CMD-REPOSITORY-UNAVAILABLE";

fn plain_git_repository() -> TempDir {
    let directory = tempfile::tempdir().expect("plain Git repository");
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Git init process");
    assert!(
        initialized.status.success(),
        "git init stderr: {:?}",
        initialized.stderr
    );
    std::fs::write(directory.path().join("main.orna"), "pub fn run(): Int = 1;")
        .expect("main source");
    directory
}

fn orna(directory: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--color", "never"])
        .args(args)
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("CLI process starts")
}

fn assert_rejected(output: &Output, command: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{command} accepted a plain Git repository: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{command} wrote stdout: {:?}",
        output.stdout
    );
    assert!(stderr.contains(REJECTION_CODE), "{command}: {stderr}");
    assert!(
        stderr.contains("not an Orna repository"),
        "{command}: {stderr}"
    );
}

#[test]
fn status_and_check_reject_plain_git_until_orna_init() {
    let directory = plain_git_repository();

    assert_rejected(&orna(&directory, &["status"]), "status");
    assert_rejected(&orna(&directory, &["status", "--short"]), "status --short");
    assert_rejected(&orna(&directory, &["check"]), "check");

    let target = directory.path().to_str().expect("UTF-8 path");
    let initialized = orna(&directory, &["init", target]);
    assert!(
        initialized.status.success(),
        "init stderr: {:?}",
        initialized.stderr
    );

    let status = orna(&directory, &["status", "--short"]);
    assert!(
        status.status.success(),
        "status stderr: {:?}",
        status.stderr
    );
    let checked = orna(&directory, &["check"]);
    assert!(
        checked.status.success(),
        "check stderr: {:?}",
        checked.stderr
    );
}
