//! `orna check` backend selection (H3): the old checker is the default,
//! `ORNA_CHECK_HIR=1` checks on HIR, and `ORNA_CHECK_SHADOW=1` runs both
//! without changing the verdict.

use std::process::{Command, Output};

use tempfile::TempDir;

const VALID: &str = "pub fn one(): Int = 1;\n";

fn project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("check backend fixture project");
    std::fs::write(directory.path().join("main.orna"), source).expect("write fixture source");
    let initialized = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("initialize fixture project");
    assert!(
        initialized.status.success(),
        "init stderr: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    directory
}

fn check(project: &TempDir, backend: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env_remove("ORNA_CHECK_HIR");
    command.env_remove("ORNA_CHECK_SHADOW");
    match backend {
        Some("hir") => {
            command.env("ORNA_CHECK_HIR", "1");
        }
        Some("shadow") => {
            command.env("ORNA_CHECK_SHADOW", "1");
        }
        _ => {}
    }
    command
        .args([
            "--db",
            project.path().to_str().expect("UTF-8 path"),
            "--color",
            "never",
            "check",
        ])
        .output()
        .expect("run orna check")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn valid_project_passes_on_every_backend() {
    let project = project(VALID);
    for backend in [None, Some("hir"), Some("shadow")] {
        let output = check(&project, backend);
        assert!(
            output.status.success(),
            "backend {backend:?} stderr: {}",
            stderr(&output)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("project valid"),
            "backend {backend:?} stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn hir_reports_the_shared_unresolved_code_for_a_missing_name() {
    let project = project("pub fn uses(): Int = missing_name + 1;\n");
    let output = check(&project, Some("hir"));
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("ORNA-S012-UNRESOLVED"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn hir_rejects_a_type_mismatch_without_asserting_the_unproven_code_pairing() {
    let project = project("pub fn wrong(): Int = \"text\";\n");
    let output = check(&project, Some("hir"));
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("E2101"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn shadow_on_invalid_project_keeps_the_old_verdict_and_writes_differences() {
    let project = project("pub fn uses(): Int = missing_name + 1;\n");
    let output = check(&project, Some("shadow"));
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("orna check shadow: differ"), "stderr: {text}");
    assert!(
        text.contains("checker-only ORNA-S012-UNRESOLVED main.orna 0..38"),
        "stderr: {text}"
    );
    assert!(
        text.contains("hir-only ORNA-S012-UNRESOLVED main.orna 21..33"),
        "stderr: {text}"
    );
}

#[test]
fn shadow_on_valid_project_reports_agreement_on_stderr() {
    let project = project(VALID);
    let output = check(&project, Some("shadow"));
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("orna check shadow: agree on 0 diagnostic(s)"),
        "stderr: {}",
        stderr(&output)
    );
}
