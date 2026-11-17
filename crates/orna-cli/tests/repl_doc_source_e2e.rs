//! `orna repl` `:doc NAME` prints the `///` doc comment lines of a declaration
//! the project declares, and `:source NAME` prints its source text. A name no
//! declaration carries is refused with a located error. Each run feeds stdin
//! from a writer thread so the child never blocks on a full pipe.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_cli_testkit::format3;

const REPL_SOURCE: &str = r"/// Runtime evidence rows.
/// One row per run.
pub table RuntimeEvidence(id: Int) {
    language_version: Str,
}

/// Doubles an integer.
pub fn double(value: Int): Int = value * 2;

pub fn plain(value: Int): Int = value;
";

fn repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the REPL starts");
    let stdin = child.stdin.take().expect("REPL stdin");
    let payload = format!("{input}\n:quit\n");
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        stdin.write_all(payload.as_bytes())
    });
    let output = child.wait_with_output().expect("the REPL exits");
    writer
        .join()
        .expect("REPL writer thread joins")
        .expect("write REPL input");
    output
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn doc_prints_every_doc_comment_line_of_a_table() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":doc RuntimeEvidence");
    let text = combined(&output);
    assert!(
        text.contains("Runtime evidence rows.\nOne row per run.\n"),
        "doc lines were not printed: {text}"
    );
}

#[test]
fn doc_prints_the_doc_comment_of_a_function() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":doc double");
    let text = combined(&output);
    assert!(text.contains("Doubles an integer.\n"), "{text}");
}

#[test]
fn doc_says_when_a_declaration_has_no_doc_comment() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":doc plain");
    let text = combined(&output);
    assert!(text.contains("(no doc comment)\n"), "{text}");
}

#[test]
fn source_prints_the_declaration_text() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":source double");
    let text = combined(&output);
    assert!(
        text.contains("pub fn double(value: Int): Int = value * 2"),
        "source text was not printed: {text}"
    );
    assert!(
        !text.contains("Doubles an integer."),
        "source printed the doc comment: {text}"
    );
}

#[test]
fn doc_and_source_refuse_an_unknown_name_by_located_error() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":doc nosuch");
    let text = combined(&output);
    assert!(
        text.contains("no declaration named `nosuch` in this session"),
        "unknown name was not refused: {text}"
    );
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
}

#[test]
fn source_without_a_name_prints_usage() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":source");
    let text = combined(&output);
    assert!(text.contains("usage: :source NAME"), "{text}");
}
