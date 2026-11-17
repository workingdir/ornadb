//! `orna repl` serves `sys.Storage` in a committed project with an admitted
//! table. The REPL read path and the write path both attach the publication
//! snapshot, so the storage metadata reads resolve instead of failing as
//! unsupported relation reads.

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
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the REPL exits")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The REPL evaluates a `sys.Storage` read against the committed publication
/// snapshot: the pending-row counter resolves to an integer rather than an
/// unsupported-relation diagnostic.
#[test]
fn repl_reads_sys_storage_in_a_committed_project() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(
        directory.path(),
        "sys.Storage | map(storage => storage.pending_rows) | first() ?? -1",
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        !text.contains("error["),
        "the REPL refused sys.Storage: {text}"
    );
    assert!(text.contains(": Int"), "no Int result printed: {text}");
}

/// The write path attaches the same publication snapshot: an insert whose value
/// reads `sys.Storage` is not table-read-only, so it reaches the effect
/// evaluator, which must see the storage metadata rows rather than refuse.
#[test]
fn repl_write_reads_sys_storage_in_a_committed_project() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let output = repl(
        directory.path(),
        "main.RuntimeEvidence.insert({ id: sys.Storage | map(storage => storage.pending_rows) | first() ?? -1, language_version: \"stored\" })",
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        !text.contains("error["),
        "the REPL refused the write: {text}"
    );
}
