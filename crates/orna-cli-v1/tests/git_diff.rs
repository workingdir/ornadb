use std::process::{Command, Output};

use tempfile::tempdir;

const FIXTURE: &[u8] = include_bytes!("fixtures/git-diff/changed path.orna");

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
        "git {arguments:?} failed: stdout={:?} stderr={:?}",
        output.stdout,
        output.stderr
    );
}

#[test]
fn diff_streams_native_output_and_preserves_exit_code_for_paths_with_spaces() {
    let repository = tempdir().expect("temporary repository");
    let file = repository.path().join("changed path.orna");
    std::fs::write(&file, FIXTURE).expect("write checked-in Orna fixture");
    git_succeeds(repository.path(), &["init", "--quiet"]);
    git_succeeds(repository.path(), &["config", "user.email", "kieran@drewett.dev"]);
    git_succeeds(repository.path(), &["config", "user.name", "kierandrewett"]);
    git_succeeds(repository.path(), &["add", "--", "changed path.orna"]);
    git_succeeds(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

    let changed = String::from_utf8_lossy(FIXTURE).replace("= 1", "= 2");
    std::fs::write(&file, changed).expect("modify fixture after baseline commit");

    let native = git(
        repository.path(),
        &["diff", "--no-color", "--", "changed path.orna"],
    );
    let cli = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["diff", "--no-color", "--", "changed path.orna"])
        .current_dir(repository.path())
        .output()
        .expect("CLI diff starts");
    println!(
        "normal: native exit={:?} stdout={:?} stderr={:?}; CLI exit={:?} stdout={:?} stderr={:?}",
        native.status.code(), native.stdout, native.stderr, cli.status.code(), cli.stdout, cli.stderr
    );
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(cli.status.code(), native.status.code());
    assert_eq!(cli.stdout, native.stdout);
    assert_eq!(cli.stderr, native.stderr);

    let native_exit_code = git(
        repository.path(),
        &["diff", "--exit-code", "--", "changed path.orna"],
    );
    let cli_exit_code = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["diff", "--exit-code", "--", "changed path.orna"])
        .current_dir(repository.path())
        .output()
        .expect("CLI diff --exit-code starts");
    println!(
        "exit-code: native exit={:?} stdout={:?} stderr={:?}; CLI exit={:?} stdout={:?} stderr={:?}",
        native_exit_code.status.code(), native_exit_code.stdout, native_exit_code.stderr,
        cli_exit_code.status.code(), cli_exit_code.stdout, cli_exit_code.stderr
    );
    assert_eq!(native_exit_code.status.code(), Some(1));
    assert_eq!(cli_exit_code.status.code(), native_exit_code.status.code());
    assert_eq!(cli_exit_code.stdout, native_exit_code.stdout);
    assert_eq!(cli_exit_code.stderr, native_exit_code.stderr);
}
