use std::process::{Command, Output};

use tempfile::TempDir;

const INVALID_SOURCE: &str = include_str!("../../../../reference/Orna-1.0.0/examples/invalid/wrong-field-type.orna");

fn invalid_fixture_project() -> TempDir {
    let directory = tempfile::tempdir().expect("diagnostic fixture project");
    std::fs::write(directory.path().join("main.orna"), INVALID_SOURCE)
        .expect("copy real Orna fixture source");

    let initialized = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
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

fn check(project: &TempDir, color: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            project.path().to_str().expect("UTF-8 path"),
            "--color",
            color,
            "check",
        ])
        .output()
        .expect("run actual CLI check command")
}

fn has_osc8(bytes: &[u8]) -> bool {
    bytes.windows(b"\x1b]8;".len()).any(|window| window == b"\x1b]8;")
}

fn trailing_line_feeds(bytes: &[u8]) -> usize {
    bytes.iter().rev().take_while(|byte| **byte == b'\n').count()
}

#[test]
fn actual_cli_diagnostic_preserves_detail_and_keeps_stderr_unlinked() {
    let project = invalid_fixture_project();

    for (color, expect_ansi) in [
        ("never", false),
        ("always", true),
        ("auto", false),
    ] {
        let output = check(&project, color);
        assert_eq!(output.status.code(), Some(1), "stderr: {:?}", output.stderr);
        assert!(output.stdout.is_empty(), "diagnostics belong on stderr");
        assert_eq!(trailing_line_feeds(&output.stderr), 1, "{:?}", output.stderr);
        assert!(!has_osc8(&output.stderr), "diagnostics must not emit OSC 8 links");
        assert_eq!(
            output.stderr.windows(2).any(|window| window == b"\x1b["),
            expect_ansi,
            "forced color policy for {color}"
        );

        let stderr = String::from_utf8(output.stderr).expect("diagnostic output is UTF-8");
        assert!(stderr.contains("E2101"), "stable CLI diagnostic code: {stderr:?}");
        assert!(
            stderr.contains("project semantic analysis failed"),
            "user-facing condition: {stderr:?}"
        );
        let detail_start = stderr.find("\n  ").expect("Diagnostic.detail line") + 3;
        let help_start = stderr.find("\nhelp: ").expect("actionable help line");
        assert!(detail_start < help_start, "detail precedes help: {stderr:?}");
        assert!(
            stderr[detail_start..help_start].contains(": "),
            "semantic diagnostic detail remains visible: {stderr:?}"
        );
    }
}
