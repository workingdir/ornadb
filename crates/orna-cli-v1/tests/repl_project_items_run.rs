//! A project enum constructed through the REPL as `main.Availability.ready`
//! is the same value that a `run` entry point matches with the bare
//! `Availability.ready` arm; bare root spellings stay unresolved in the REPL.

use std::{
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

const PROJECT_ITEMS: &str = include_str!("fixtures/repl-project-items.orna");
const READY_ENTRY: &str = "\npub fn ready_label(): Str = describe(Availability.ready);\n";

fn initialized() -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    let source = format!("{PROJECT_ITEMS}{READY_ENTRY}");
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

fn assert_stdout(output: &Output, stdout: &str) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        stdout,
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn repl_qualified_root_enum_matches_the_run_entry_arm() {
    let project = initialized();
    let directory = project.path();

    assert_stdout(&orna(directory, &["check"]), "project valid\n");
    assert_stdout(
        &orna(directory, &["repl", "main.describe(main.Availability.ready)"]),
        "\"ready\" : Str\n",
    );
    assert_stdout(
        &orna(directory, &["repl", "main.ready_label()"]),
        "\"ready\" : Str\n",
    );
    assert_stdout(
        &orna(directory, &["run", "main.ready_label"]),
        "invocation completed\n",
    );
}

#[test]
fn repl_bare_root_enum_spelling_is_unresolved() {
    let project = initialized();
    let output = orna(project.path(), &["repl", "Availability.ready"]);
    assert!(!output.status.success(), "{output:?}");
}
