//! ORNA-ENUM-001..003 and ORNA-CASE-001..005 through product commands on a
//! database created by `orna init`: enum values are constructed in source,
//! matched with `case`, stored in a table, and invalid constructions fail
//! `orna check` with the published diagnostic codes.

use std::{
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

fn initialized(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    directory
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn assert_output(output: &Output, stdout: &str) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        stdout,
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn section_six_example_constructs_matches_and_stores_enum_values() {
    let project = initialized(include_str!("fixtures/enum-construction/main.orna"));
    let directory = project.path();

    assert_output(&orna(directory, &["check"]), "project valid\n");
    for (submission, expected) in [
        ("main.ok_count()", "3 : Int\n"),
        ("main.failed_reason()", "\"retry: offline\" : Str\n"),
        ("main.level_name()", "\"high\" : Str\n"),
        ("main.guarded(20)", "1 : Int\n"),
        ("main.guarded(5)", "2 : Int\n"),
    ] {
        assert_output(&orna(directory, &["repl", submission]), expected);
    }

    assert_output(
        &orna(directory, &["run", "main.seed"]),
        "invocation completed\n",
    );
    let status = orna(directory, &["status", "--short"]);
    assert!(status.status.success(), "{status:?}");
    assert!(
        String::from_utf8_lossy(&status.stdout).ends_with("RM 1 runtime mutation\n"),
        "{status:?}"
    );
}

#[test]
fn invalid_constructions_fail_check_with_published_codes() {
    for (fixture, detail) in [
        (
            include_str!("fixtures/enum-construction/unknown-variant.orna"),
            "ORNA-S012-UNRESOLVED: enum `Level` has no variant `medium`",
        ),
        (
            include_str!("fixtures/enum-construction/unknown-field.orna"),
            "E2003: unknown field `extra`",
        ),
        (
            include_str!("fixtures/enum-construction/missing-field.orna"),
            "E2002: missing required field `retryable`",
        ),
        (
            include_str!("fixtures/enum-construction/wrong-field-type.orna"),
            "E2001: expected Int, found Str",
        ),
    ] {
        let project = initialized(fixture);
        let output = orna(project.path(), &["check"]);
        assert_eq!(output.status.code(), Some(1), "{fixture}: {output:?}");
        assert!(output.stdout.is_empty(), "{fixture}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "error[E2101]: project semantic analysis failed\n  {detail}\nhelp: fix the first reported source contract error, then run check again\n"
            ),
            "{fixture}"
        );
    }
}
