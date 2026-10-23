//! Parse-phase invalid fixtures must reach the CLI with their spec code as the
//! primary diagnostic and a source location, not the loader's generic E2100
//! wrapper. An invalid loose row unit must fail `check` with its parser code.

use std::process::{Command, Output};

use tempfile::TempDir;

/// `(fixture, manifest diagnostic)` for every `module_unit` parse-phase fixture.
const MODULE_FIXTURES: &[(&str, &str, &str)] = &[
    (
        "assert-empty.orna",
        include_str!("fixtures/parse-phase-invalid/assert-empty.orna"),
        "ORNA-A091-011",
    ),
    (
        "assert-missing-semicolon.orna",
        include_str!("fixtures/parse-phase-invalid/assert-missing-semicolon.orna"),
        "ORNA-A091-005",
    ),
    (
        "assignment-expression.orna",
        include_str!("fixtures/parse-phase-invalid/assignment-expression.orna"),
        "E1301",
    ),
    (
        "comparison-chain.orna",
        include_str!("fixtures/parse-phase-invalid/comparison-chain.orna"),
        "E1302",
    ),
    (
        "legacy-assert-else.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-assert-else.orna"),
        "ORNA-A091-006",
    ),
    (
        "legacy-assert-pipe-bang.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-assert-pipe-bang.orna"),
        "ORNA-A091-010",
    ),
    (
        "legacy-colon-bound.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-colon-bound.orna"),
        "ORNA091-E-BOUND-COLON",
    ),
    (
        "legacy-constraints-block.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-constraints-block.orna"),
        "ORNA-A091-010",
    ),
    (
        "legacy-currency-declaration.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-currency-declaration.orna"),
        "ORNA091-E-CURRENCY",
    ),
    (
        "legacy-empty-closure.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-empty-closure.orna"),
        "E1012",
    ),
    (
        "legacy-ensure.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-ensure.orna"),
        "ORNA-A091-010",
    ),
    (
        "legacy-fact.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-fact.orna"),
        "ORNA-A091-010",
    ),
    (
        "legacy-field-check.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-field-check.orna"),
        "ORNA091-E-FIELD-CONSTRAINT",
    ),
    (
        "legacy-field-unique.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-field-unique.orna"),
        "ORNA091-E-FIELD-CONSTRAINT",
    ),
    (
        "legacy-ingest.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-ingest.orna"),
        "E1004",
    ),
    (
        "legacy-log.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-log.orna"),
        "E1001",
    ),
    (
        "legacy-match.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-match.orna"),
        "ORNA091-E-MATCH",
    ),
    (
        "legacy-opaque.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-opaque.orna"),
        "ORNA091-E-OPAQUE",
    ),
    (
        "legacy-pipe-lambda.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-pipe-lambda.orna"),
        "E1011",
    ),
    (
        "legacy-postfix-question.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-postfix-question.orna"),
        "ORNA091-E-POSTFIX-QUESTION",
    ),
    (
        "legacy-refined-where.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-refined-where.orna"),
        "ORNA-A091-001",
    ),
    (
        "legacy-return-arrow.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-return-arrow.orna"),
        "ORNA091-E-RETURN-ARROW",
    ),
    (
        "legacy-store.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-store.orna"),
        "E1003",
    ),
    (
        "legacy-top-level-impl.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-top-level-impl.orna"),
        "ORNA091-E-IMPL-FOR",
    ),
    (
        "legacy-var.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-var.orna"),
        "ORNA091-E-VAR",
    ),
    (
        "legacy-view.orna",
        include_str!("fixtures/parse-phase-invalid/legacy-view.orna"),
        "E1002",
    ),
    (
        "question-coalesce-adjacent.orna",
        include_str!("fixtures/parse-phase-invalid/question-coalesce-adjacent.orna"),
        "ORNA091-E-POSTFIX-QUESTION",
    ),
    (
        "question-on-int.orna",
        include_str!("fixtures/parse-phase-invalid/question-on-int.orna"),
        "ORNA091-E-POSTFIX-QUESTION",
    ),
    (
        "record-punning.orna",
        include_str!("fixtures/parse-phase-invalid/record-punning.orna"),
        "E1013",
    ),
    (
        "static-protocol-function.orna",
        include_str!("fixtures/parse-phase-invalid/static-protocol-function.orna"),
        "ORNA091-E-STATIC-FN",
    ),
    (
        "top-level-expression.orna",
        include_str!("fixtures/parse-phase-invalid/top-level-expression.orna"),
        "E1006",
    ),
    (
        "top-level-on.orna",
        include_str!("fixtures/parse-phase-invalid/top-level-on.orna"),
        "E1005",
    ),
    (
        "transaction-block.orna",
        include_str!("fixtures/parse-phase-invalid/transaction-block.orna"),
        "E1007",
    ),
    (
        "unparenthesized-lambda-stage.orna",
        include_str!("fixtures/parse-phase-invalid/unparenthesized-lambda-stage.orna"),
        "E1204",
    ),
];

/// `row_unit` parse-phase fixture: one record expression, no declarations.
const ROW_DECLARATION: &str = include_str!("fixtures/parse-phase-invalid/row-declaration.orna");

const TABLE_MAIN: &str = "pub table X { value: Int, }\n";

fn initialized_project(files: &[(&str, &str)]) -> TempDir {
    let directory = tempfile::tempdir().expect("parse diagnostic fixture project");
    for (path, source) in files {
        let target = directory.path().join(path);
        std::fs::create_dir_all(target.parent().expect("fixture file has a parent"))
            .expect("create fixture directory");
        std::fs::write(&target, source).expect("write fixture source");
    }
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

fn check(project: &TempDir) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            project.path().to_str().expect("UTF-8 path"),
            "--color",
            "never",
            "check",
        ])
        .output()
        .expect("run check")
}

/// The first `error[...]` line: the primary diagnostic `check` prints.
fn primary_error_line(stderr: &str) -> &str {
    stderr
        .lines()
        .find(|line| line.starts_with("error["))
        .unwrap_or_else(|| panic!("no primary error line in stderr: {stderr:?}"))
}

#[test]
fn parse_phase_module_fixtures_report_their_spec_code() {
    for (name, source, code) in MODULE_FIXTURES {
        let project = initialized_project(&[("main.orna", source)]);
        let output = check(&project);
        let stderr = String::from_utf8(output.stderr.clone()).expect("diagnostic output is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{name} exit; stderr: {stderr:?}"
        );
        let primary = primary_error_line(&stderr);
        assert!(
            primary.contains(&format!("error[{code}]")),
            "{name} must report {code} as primary: {stderr:?}"
        );
        assert!(
            primary.contains("main.orna:"),
            "{name} must report the source location: {stderr:?}"
        );
        assert!(
            !stderr.contains("E2100"),
            "{name} must not be replaced by the loader code: {stderr:?}"
        );
    }
}

#[test]
fn invalid_loose_row_unit_fails_check_with_parser_code() {
    let project = initialized_project(&[("main.orna", TABLE_MAIN), ("X/k.orna", ROW_DECLARATION)]);
    let output = check(&project);
    let stderr = String::from_utf8(output.stderr.clone()).expect("diagnostic output is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "row-declaration exit; stderr: {stderr:?}"
    );
    let primary = primary_error_line(&stderr);
    assert!(
        primary.contains("error[E8001]"),
        "row-declaration must report E8001 as primary: {stderr:?}"
    );
    assert!(
        primary.contains("X/k.orna:1:1"),
        "row-declaration must report its row location: {stderr:?}"
    );
    assert!(
        !stderr.contains("E2100"),
        "row-declaration must not be replaced by the loader code: {stderr:?}"
    );
}

#[test]
fn valid_loose_row_unit_passes_check() {
    let project =
        initialized_project(&[("main.orna", TABLE_MAIN), ("X/k.orna", "{ value: 1, }\n")]);
    let output = check(&project);
    let stderr = String::from_utf8(output.stderr.clone()).expect("diagnostic output is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(0),
        "valid row must pass check; stderr: {stderr:?}"
    );
}
