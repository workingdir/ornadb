//! Cross-table module assertions (ORNA-ASSERT-021..030) through the trusted REPL.
//!
//! A module assertion depends on two tables, so it is not owned by either one.
//! A REPL write that changes a table in its dependency set must evaluate it
//! against the unpublished candidate database before publication; a false
//! result aborts the whole write boundary and leaves the store unchanged.

#[path = "support/repl_run.rs"]
mod repl_run;

use std::path::Path;
use std::process::Command;

use repl_run::{ReplRun, initialized_project, run_lines};

const LIBRARY_MODULE: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));\n";

fn combined(output: &ReplRun) -> String {
    format!("{}{}", output.stdout, output.stderr)
}

/// Commits the project so REPL writes start from a clean committed base.
fn commit_project(root: &Path) {
    let run = |arguments: &[&str]| {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git process");
        assert!(status.success(), "git {arguments:?}");
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "kieran@drewett.dev"]);
    run(&["config", "user.name", "kierandrewett"]);
    run(&["config", "commit.gpgsign", "false"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "module assertion fixture"]);
}

#[test]
fn satisfied_cross_table_assertion_admits_the_write() {
    let project = initialized_project(&[("main.orna", LIBRARY_MODULE)]);
    commit_project(project.path());

    let output = run_lines(
        project.path(),
        &[
            "main.Book.insert({ id: 1, title: \"Dune\" })",
            "main.Loan.insert({ id: 1, book_id: 1 })",
            "main.Loan | count()",
        ],
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(output.stdout.contains("1 : Int"), "{text}");
    assert!(!text.contains("ORNA-A091-009"), "{text}");
}

#[test]
fn violated_cross_table_assertion_rejects_the_write_and_keeps_the_store() {
    let project = initialized_project(&[("main.orna", LIBRARY_MODULE)]);
    commit_project(project.path());

    // Book 9 does not exist, so the module assertion is false for this Loan.
    let output = run_lines(
        project.path(),
        &[
            "main.Book.insert({ id: 1, title: \"Dune\" })",
            "main.Loan.insert({ id: 2, book_id: 9 })",
            "main.Loan | count()",
        ],
    );
    let text = combined(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(output.stdout.contains("0 : Int"), "{text}");
}
