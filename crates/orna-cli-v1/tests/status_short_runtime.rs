use std::process::{Command, Output};

use tempfile::tempdir;

const FIXTURE: &[u8] = include_bytes!("fixtures/status-short.orna");

fn git_status(directory: &std::path::Path) -> Output {
    Command::new("git")
        .args(["status", "--short"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Git short status process")
}

#[test]
fn short_status_preserves_git_rows_and_appends_runtime_summary() {
    let directory = tempdir().expect("temporary Git repository");
    std::fs::write(directory.path().join("status-short.orna"), FIXTURE)
        .expect("checked-in Orna fixture");
    let git_init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Git init process");
    assert!(
        git_init.status.success(),
        "git init stderr: {:?}",
        git_init.stderr
    );

    let expected_git = git_status(directory.path());
    assert!(expected_git.status.success());
    assert_eq!(expected_git.stdout, b"?? status-short.orna\n");

    let cli = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--color", "never", "status", "--short"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("CLI short status process");
    println!(
        "CLI proof: exit={:?} stdout={:?} stderr={:?}",
        cli.status.code(),
        String::from_utf8_lossy(&cli.stdout),
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(cli.status.code(), Some(0), "CLI exit: {:?}", cli.status);
    assert!(cli.stderr.is_empty(), "CLI stderr: {:?}", cli.stderr);
    assert!(
        cli.stdout.starts_with(&expected_git.stdout),
        "Git rows changed: expected {:?}, got {:?}",
        expected_git.stdout,
        cli.stdout
    );
    assert_eq!(
        &cli.stdout[expected_git.stdout.len()..],
        b"RM 0 runtime mutations\n"
    );
}
