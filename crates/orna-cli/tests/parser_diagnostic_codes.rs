use std::process::{Command, Output};

use tempfile::TempDir;

// `var` is a removed 1.0 declaration form; the parser reports ORNA091-E-VAR.
const LEGACY_VAR_SOURCE: &str = "pub fn bad(): Int {\n    var value = 1;\n    value\n}\n";

fn project_with_main(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("parser diagnostic fixture project");
    std::fs::write(directory.path().join("main.orna"), source).expect("write main.orna");

    let initialized = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("initialize fixture project");
    assert!(
        initialized.status.success(),
        "init stderr: {:?}",
        initialized.stderr
    );
    directory
}

fn cli(project: &TempDir, command: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            project.path().to_str().expect("UTF-8 path"),
            "--color",
            "never",
            command,
        ])
        .output()
        .expect("run actual CLI command")
}

fn assert_primary_parser_code(output: &Output, command: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{command} exit; stderr: {:?}",
        output.stderr
    );
    let stderr = String::from_utf8(output.stderr.clone()).expect("diagnostic output is UTF-8");
    assert!(
        stderr.contains("error[ORNA091-E-VAR]"),
        "{command} must show the parser code as primary: {stderr:?}"
    );
    assert!(
        stderr.contains("main.orna:2:5"),
        "{command} must report the source location: {stderr:?}"
    );
    assert!(
        !stderr.contains("E2100") && !stderr.contains("E2101"),
        "{command} must not replace the parser code with a loader code: {stderr:?}"
    );
}

#[test]
fn check_reports_parser_spec_code_for_invalid_module() {
    let project = project_with_main(LEGACY_VAR_SOURCE);
    let output = cli(&project, "check");
    assert_primary_parser_code(&output, "check");
}

#[test]
fn run_reports_parser_spec_code_for_invalid_module() {
    let project = project_with_main(LEGACY_VAR_SOURCE);
    let output = cli(&project, "run");
    assert_primary_parser_code(&output, "run");
}
