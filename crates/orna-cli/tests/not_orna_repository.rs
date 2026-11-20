use std::process::{Command, Output};

use tempfile::TempDir;

const REJECTION_CODE: &str = "ORNA-CMD-REPOSITORY-UNAVAILABLE";
/// `Exit::Target`: the exit status of every target-level refusal.
const REFUSAL_EXIT_CODE: i32 = 1;

/// Every command form that reads the local worktree, in the form it is typed.
const WORKTREE_FORMS: &[(&str, &[&str])] = &[
    ("status", &["status"]),
    ("status --porcelain", &["status", "--porcelain"]),
    ("status --short", &["status", "--short"]),
    ("status --format json", &["status", "--format", "json"]),
    ("check", &["check"]),
];

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
    assert_eq!(
        output.status.code(),
        Some(REFUSAL_EXIT_CODE),
        "{command} exit status: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{command} wrote stdout: {:?}",
        output.stdout
    );
    assert!(!stderr.contains("panicked"), "{command} panicked: {stderr}");
    assert!(stderr.contains(REJECTION_CODE), "{command}: {stderr}");
    assert!(
        stderr.contains("not an Orna repository"),
        "{command}: {stderr}"
    );
    assert!(stderr.contains("orna init"), "{command}: {stderr}");
}

#[test]
fn plain_git_worktree_is_refused_by_every_worktree_form() {
    let directory = plain_git_repository();

    for (command, args) in WORKTREE_FORMS {
        assert_rejected(&orna(&directory, args), command);
    }
}

#[test]
fn plain_git_worktree_is_accepted_by_every_worktree_form_after_orna_init() {
    let directory = plain_git_repository();

    let target = directory.path().to_str().expect("UTF-8 path");
    let initialized = orna(&directory, &["init", target]);
    assert!(
        initialized.status.success(),
        "init stderr: {:?}",
        initialized.stderr
    );

    for (command, args) in WORKTREE_FORMS {
        let output = orna(&directory, args);
        assert!(
            output.status.success(),
            "{command} stderr: {:?}",
            output.stderr
        );
    }
}
