use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn initialize_legacy_source(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    git_output(root, &["config", "user.name", "kierandrewett"]);
    git_output(root, &["config", "user.email", "kieran@drewett.dev"]);
    git_output(root, &["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("main.orna"),
        include_str!("fixtures/project-core-main.orna"),
    )
    .unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(
        root.join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1.orna"),
    )
    .unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(root, &["add", "."]);
    git_output(root, &["commit", "-m", "legacy source"]);
}

fn run_copy(
    source: &Path,
    destination: &Path,
    count: &str,
    git_trace: &Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE", git_trace)
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            count,
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap()
}

#[test]
fn cli_migration_copy_retry_recovers_the_same_candidate() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let destination = parent.path().join("migrated");
    let git_trace = parent.path().join("git-trace.log");
    let source_head = String::from_utf8(git_output(&source, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let source_status = git_output(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );

    let first = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        first.status.success(),
        "first migration copy: {}\nGit trace:\n{}",
        String::from_utf8_lossy(&first.stderr),
        fs::read_to_string(&git_trace).unwrap_or_else(|error| format!("<unavailable: {error}>")),
    );
    assert!(first.stderr.is_empty());
    let first_output = String::from_utf8(first.stdout).unwrap();
    assert!(first_output.starts_with(
        "prepared or recovered format-3 migration copy: source_format=1 source_commit="
    ));

    let retry = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        retry.status.success(),
        "migration retry: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(retry.stdout, first_output.as_bytes());
    assert!(retry.stderr.is_empty());

    let conflict = run_copy(&source, &destination, "1", &git_trace);
    assert_eq!(conflict.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("E2100"));
    let foreign = parent.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("marker"), b"foreign destination\n").unwrap();
    let foreign_copy = run_copy(&source, &foreign, "0", &git_trace);
    assert_eq!(foreign_copy.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&foreign_copy.stderr).contains("E2100"));
    assert_eq!(
        fs::read(foreign.join("marker")).unwrap(),
        b"foreign destination\n",
        "a foreign destination is never overwritten"
    );
    assert_eq!(
        String::from_utf8(git_output(&source, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim(),
        source_head
    );
    assert_eq!(
        git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
}
