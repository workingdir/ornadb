//! Multi-line REPL submissions through `orna repl`: an incomplete input keeps
//! reading lines under a `..` continuation prompt until it completes, a blank
//! line ends it as typed, and a continued submission is bounded by the input
//! limit (spec `17-repl.md`, "Syntax-aware editing": "multiline input until
//! syntax is complete").

#[path = "support/product.rs"]
mod product;

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use product::*;

const BOOK_LIBRARY: &str = "pub table Book(id: Str) {\n    title: Str,\n}\n";

/// A committed project whose REPL sessions can evaluate expressions.
fn committed_library(directory: &tempfile::TempDir) -> PathBuf {
    let root = initialized_database(directory, BOOK_LIBRARY);
    commit_source(&root, "book library");
    root
}

/// One `orna repl` session fed `payload` on stdin. The input is written from a
/// writer thread: a large payload echoes more output than a pipe buffer holds,
/// so writing it all before draining stdout deadlocks the parent and the child.
fn repl_session(root: &Path, payload: String) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the orna REPL starts");
    let mut stdin = child.stdin.take().expect("REPL stdin");
    let writer = std::thread::spawn(move || stdin.write_all(payload.as_bytes()));
    let output = child.wait_with_output().expect("the orna REPL exits");
    writer
        .join()
        .expect("REPL writer thread joins")
        .expect("write REPL input");
    output
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn an_expression_split_over_lines_runs_as_one_submission() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);

    let output = repl_session(&root, "(10 -\n    3)\n:quit\n".to_owned());
    assert_success(&output, "multi-line expression");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("7 : Int"), "{}", transcript(&output));
    assert!(stdout.contains(".. "), "{}", transcript(&output));
    assert!(!stdout.contains("error["), "{}", transcript(&output));
}

#[test]
fn a_let_binding_split_over_lines_is_bound_for_the_session() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);

    let output = repl_session(
        &root,
        "let total =\n    40 +\n    2;\ntotal\n:quit\n".to_owned(),
    );
    assert_success(&output, "multi-line let binding");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("42 : Int"), "{}", transcript(&output));
    assert!(!stdout.contains("error["), "{}", transcript(&output));
}

#[test]
fn a_blank_line_ends_an_incomplete_submission_and_the_session_continues() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);

    let output = repl_session(&root, "(1 +\n\n2 + 2\n:quit\n".to_owned());
    assert_success(&output, "blank line ends a continuation");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("error["), "{}", transcript(&output));
    assert!(stdout.contains("4 : Int"), "{}", transcript(&output));
}

#[test]
fn a_complete_line_is_not_continued() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);

    let output = repl_session(&root, "1 + 1\n:quit\n".to_owned());
    assert_success(&output, "complete line");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("2 : Int"), "{}", transcript(&output));
    assert!(!stdout.contains(".. "), "{}", transcript(&output));
}

#[test]
fn a_continued_submission_over_the_input_limit_is_refused() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);

    // Comment lines keep the open `let` incomplete without nesting, which the
    // parser caps (ORNA-PARSE-001), so the input limit is what ends the submission.
    let comment = format!("// {}\n", "x".repeat(60));
    let payload = format!("let answer =\n{}:quit\n", comment.repeat(2_000));
    let output = repl_session(&root, payload);
    assert_success(&output, "continued input over the limit");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("ORNA-REPL-INPUT-LIMIT"),
        "{}",
        transcript(&output)
    );
}
