//! Line-anchored 1.0 audit coverage:
//! - `source/17-repl.md:13,29,64,115` (`ORNA-REPL-001/002/003/005`).
//! - `source/18-cli.md:74,110,112` (`ORNA-DIAG-001`, `ORNA-UX-003/004`).
//! The REPL and diagnostic commands below load only checked-in `.orna` fixtures.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const VALID_SOURCE: &str = include_str!("fixtures/repl-cli-numeric-context.orna");
const REPL_STATUS: &str = include_str!("fixtures/repl-status-binding.orna");
const REPL_EFFECT: &str = include_str!("fixtures/repl-effect-preview.orna");
const REPL_AT_HEAD: &str = include_str!("fixtures/repl-at-head.orna");
const REPL_USE_LIBRARY: &str = include_str!("fixtures/repl-use-library.orna");
const REPL_USE_MAIN: &str = include_str!("fixtures/run/main.orna");
const REPL_USE_LIBRARY_MODULE: &str = include_str!("fixtures/run/library.orna");

fn project() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary Orna project");
    std::fs::write(directory.path().join("main.orna"), VALID_SOURCE)
        .expect("checked-in reference Orna source");
    let initialized = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna init process");
    assert_eq!(initialized.status.code(), Some(0));
    directory
}

fn run(directory: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna CLI process")
}

fn run_repl(directory: &std::path::Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}:quit\n").as_bytes())
        .expect("write checked-in REPL fixture");
    child.wait_with_output().expect("wait for Orna REPL")
}

fn git_head(directory: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("read Git HEAD");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("HEAD text").trim().to_owned()
}

fn commit_project(directory: &std::path::Path) {
    let added = Command::new("git")
        .args(["add", "--all"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("stage project fixture");
    assert!(added.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "fixture snapshot",
        ])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("commit project fixture");
    assert!(committed.success());
}

#[test]
fn help_advertises_the_explicit_debug_output_mode() {
    let output = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .arg("--help")
        .output()
        .expect("Orna help process");
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("--debug (show technical detail)"));
}

#[test]
fn checked_in_source_fixture_passes_check_with_and_without_debug() {
    let directory = project();
    let normal = run(directory.path(), &["check"]);
    let debug = run(directory.path(), &["--debug", "check"]);
    assert_eq!(normal.status.code(), Some(0), "{:?}", normal.stderr);
    assert_eq!(debug.status.code(), Some(0), "{:?}", debug.stderr);
    assert_eq!(normal.stdout, b"project valid\n");
    assert_eq!(debug.stdout, normal.stdout);
    assert!(normal.stderr.is_empty());
    assert!(debug.stderr.is_empty());
}

#[test]
fn normal_fetch_diagnostic_hides_raw_repository_detail_but_keeps_remedy() {
    let directory = project();
    let output = run(directory.path(), &["--color", "never", "fetch", "missing", "main"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("error[E2100]: Git fetch failed\n"), "{stderr}");
    assert!(stderr.contains("help: check the configured remote and retry `fetch`\n"), "{stderr}");
    assert!(!stderr.contains("\n  "), "raw technical detail leaked: {stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn debug_fetch_diagnostic_reveals_raw_repository_detail() {
    let directory = project();
    let output = run(
        directory.path(),
        &["--debug", "--color", "never", "fetch", "missing", "main"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("error[E2100]: Git fetch failed\n"), "{stderr}");
    assert!(stderr.contains("\n  "), "debug detail was not rendered: {stderr}");
    assert!(stderr.contains("help: check the configured remote and retry `fetch`\n"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn debug_flag_after_color_option_keeps_the_selected_nonterminal_style() {
    let directory = project();
    let output = run(
        directory.path(),
        &["--color", "never", "--debug", "fetch", "missing", "main"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("\x1b["), "debug mode changed color policy: {stderr:?}");
    assert!(stderr.contains("\n  "), "debug detail was not rendered: {stderr}");
}

#[test]
fn repl_uses_ordinary_module_imports_without_a_namespace_command() {
    let directory = tempfile::tempdir().expect("temporary Orna project");
    std::fs::write(directory.path().join("main.orna"), REPL_USE_MAIN)
        .expect("checked-in importing module fixture");
    std::fs::write(
        directory.path().join("library.orna"),
        REPL_USE_LIBRARY_MODULE,
    )
    .expect("checked-in imported module fixture");
    let initialized = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna init process");
    assert_eq!(initialized.status.code(), Some(0), "{:?}", initialized.stderr);

    let output = run_repl(directory.path(), REPL_USE_LIBRARY);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert!(stdout.contains("42 : Int"), "{stdout}");
    assert!(output.stderr.is_empty());
}

#[test]
fn repl_status_and_last_result_bindings_remain_session_local_and_redacted() {
    let directory = project();
    let output = run_repl(directory.path(), REPL_STATUS);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert!(stdout.contains("ORNA-S021-TYPE"), "{stdout}");
    assert!(stdout.contains("42 : Int"), "{stdout}");
    assert!(stdout.contains("<redacted>"), "{stdout}");
    assert!(!stdout.contains("private"), "sensitive fixture text leaked: {stdout}");
    assert!(output.stderr.is_empty());
}

#[test]
fn repl_repository_authority_absence_keeps_the_legacy_core_session() {
    let directory = tempfile::tempdir().expect("directory without a repository");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .arg("repl")
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(b"40 + 2\n:quit\n")
        .expect("write core-only expression");
    let output = child.wait_with_output().expect("wait for Orna REPL");

    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert_eq!(output.stdout.as_slice(), b"> 42 : Int\n> ");
    assert!(output.stderr.is_empty());
}

#[test]
fn repl_effect_preview_rejects_the_effect_and_preserves_last_value() {
    let directory = project();
    let output = run_repl(directory.path(), REPL_EFFECT);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert!(stdout.contains("error[ORNA-REPL-EFFECT]"), "{stdout}");
    assert!(stdout.ends_with("2 : Int\n> "), "{stdout}");
    assert!(output.stderr.is_empty());
}

#[test]
fn repl_at_head_changes_session_snapshot_without_moving_git_head() {
    let directory = project();
    commit_project(directory.path());
    let before = git_head(directory.path());
    let output = run_repl(directory.path(), REPL_AT_HEAD);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert!(stdout.contains("42 : Int"), "{stdout}");
    assert_eq!(git_head(directory.path()), before);
    assert!(output.stderr.is_empty());
}
