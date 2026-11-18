//! `orna repl` `:open EXPR` needs the file and line of the source it opens,
//! which a REPL session does not retain, so it refuses with the portable
//! `sys.source.unavailable` and starts no editor. Each run feeds stdin from a
//! writer thread so the child never blocks on a full pipe.

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_cli_testkit::format3;

const REPL_SOURCE: &str = r"pub table Reading(id: Int) {
    temperature: Int,
}

/// Doubles an integer.
pub fn double(value: Int): Int = value * 2;
";

/// Runs the REPL with `$EDITOR` and `$VISUAL` set to a script that records its
/// arguments in `marker`, so a test can tell whether an editor was started.
fn repl(directory: &Path, marker: &Path, input: &str) -> Output {
    let editor = marker.with_extension("sh");
    std::fs::write(
        &editor,
        format!("#!/bin/sh\necho \"$@\" > '{}'\n", marker.display()),
    )
    .expect("write fake editor");
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755))
        .expect("make fake editor executable");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("EDITOR", &editor)
        .env("VISUAL", &editor)
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
fn open_refuses_with_the_portable_code_and_starts_no_editor() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let output = repl(directory.path(), &marker, ":open double");
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(
            "error[sys.source.unavailable]: `:open` needs the file and line a declaration was \
             read from, which this session does not retain"
        ),
        "{text}"
    );
    assert!(text.contains("1 | :open double\n  | ^^^^^"), "{text}");
    assert!(
        text.contains("help: `:source NAME` prints a declaration's source text"),
        "{text}"
    );
    assert!(
        !text.contains("not available in this console yet"),
        "the refusal is still the generic one: {text}"
    );
    assert!(!marker.exists(), "a refused `:open` started the editor");
}

#[test]
fn open_refuses_a_name_the_session_does_not_declare_the_same_way() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let output = repl(directory.path(), &marker, ":open nosuch");
    let text = combined(&output);
    assert!(text.contains("error[sys.source.unavailable]"), "{text}");
    assert!(!marker.exists(), "a refused `:open` started the editor");
}

#[test]
fn open_without_an_expression_prints_usage() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let output = repl(directory.path(), &marker, ":open");
    let text = combined(&output);
    assert!(text.contains("usage: :open EXPR"), "{text}");
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
    assert!(!marker.exists(), "`:open` started the editor");
}
