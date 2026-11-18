//! `orna repl` `:trace EXPR` needs the activation trace (`sys.rt.traces`), which
//! this implementation does not provide, so it refuses with the portable
//! `sys.version.incompatible` (ORNA-SYS-008) before it evaluates anything.
//! Each run feeds stdin from a writer thread so the child never blocks on a
//! full pipe.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_cli_testkit::format3;

const REPL_SOURCE: &str = r"pub table Reading(id: Int) {
    temperature: Int,
    label: Str,
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
fn trace_refuses_with_the_portable_code_and_names_the_missing_relation() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":trace 1 + 1");
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(
            "error[sys.version.incompatible]: `:trace` needs the activation trace, \
             `sys.rt.traces`, which has no provider linked in this implementation"
        ),
        "{text}"
    );
    assert!(text.contains("1 | :trace 1 + 1\n  | ^^^^^^"), "{text}");
    assert!(
        text.contains("help: `:plan EXPR` explains a query without running it"),
        "{text}"
    );
    assert!(
        !text.contains("not available in this console yet"),
        "the refusal is still the generic one: {text}"
    );
}

#[test]
fn trace_refusal_is_the_code_reading_the_trace_relation_gives() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":trace 1\nsys.rt.traces | take(1)");
    let text = combined(&output);
    assert_eq!(
        text.matches("error[sys.version.incompatible]").count(),
        2,
        "`:trace` and the relation read do not refuse alike: {text}"
    );
}

#[test]
fn trace_does_not_evaluate_its_expression() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":trace main.Reading | take(main.Reading.insert({ id: 1, temperature: 2, label: \"x\" }).id)\nmain.Reading | take(5)",
    );
    let text = combined(&output);
    assert!(text.contains("error[sys.version.incompatible]"), "{text}");
    assert!(
        text.contains("main.Reading | take(5) : Relation<Reading> · 0 rows · CWD"),
        "the refused expression ran and wrote a row: {text}"
    );
}

#[test]
fn trace_without_an_expression_prints_usage() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":trace");
    let text = combined(&output);
    assert!(text.contains("usage: :trace EXPR"), "{text}");
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
}
