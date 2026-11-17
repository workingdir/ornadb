//! `orna repl` `:copy EXPR [--format orna|json]` writes the codec text of a
//! value through the standard codec, and refuses `csv` and malformed input
//! with a located error. Each run feeds stdin from a writer thread so the
//! child never blocks on a full pipe.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_cli_testkit::format3;

const REPL_TABLE_SOURCE: &str = r"pub table RuntimeEvidence(id: Int) {
    language_version: Str,
}
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
fn copy_without_a_captured_standard_refuses_the_orna_codec_call() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":copy 1 + 2");
    let text = combined(&output);
    assert!(text.contains("ORNA-EVAL-UNSUPPORTED"), "{text}");
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "3"),
        "the refused codec call printed a value: {text}"
    );
}

#[test]
fn copy_without_a_captured_standard_refuses_the_json_codec_call() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":copy \"hi\" --format json");
    let text = combined(&output);
    assert!(text.contains("ORNA-EVAL-UNSUPPORTED"), "{text}");
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "\"hi\""),
        "the refused codec call printed a value: {text}"
    );
}

#[test]
fn copy_refuses_csv_by_name_because_no_codec_package_provides_it() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":copy 1 --format csv");
    let text = combined(&output);
    assert!(
        text.contains("`:copy --format csv` is not available"),
        "csv was not refused by name: {text}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "1"),
        "csv printed a value: {text}"
    );
}

#[test]
fn copy_without_an_expression_prints_usage() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(directory.path(), ":copy");
    let text = combined(&output);
    assert!(
        text.contains("usage: :copy EXPR [--format orna|json]"),
        "no copy usage line: {text}"
    );
}
