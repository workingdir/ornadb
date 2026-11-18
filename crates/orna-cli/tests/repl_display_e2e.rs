//! `orna repl` `:display Type formatter|default` records a session presentation
//! override in `sys.repl.presentation_overrides`, which has no provider linked
//! in this implementation, so it refuses with the portable
//! `sys.version.incompatible` (ORNA-SYS-008) and changes no rendering. Each run
//! feeds stdin from a writer thread so the child never blocks on a full pipe.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_cli_testkit::format3;

const REPL_SOURCE: &str = r"pub table Reading(id: Int) {
    temperature: Int,
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
fn display_refuses_with_the_portable_code_and_leaves_the_rendering_alone() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":display Duration std.time.duration.clock\n1.hour",
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(
            "error[sys.version.incompatible]: `:display` records the override in \
             `sys.repl.presentation_overrides`, which has no provider linked in this \
             implementation"
        ),
        "{text}"
    );
    assert!(
        text.contains("1 | :display Duration std.time.duration.clock\n  | ^^^^^^^^"),
        "{text}"
    );
    assert!(
        !text.contains("not available in this console yet"),
        "the refusal is still the generic one: {text}"
    );
    // The refused override changed nothing: the result renders as it did.
    assert!(
        text.contains("Duration([3600, 0]) : std.DURATION"),
        "the rendering changed: {text}"
    );
}

#[test]
fn display_default_is_refused_with_the_same_code() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":display Duration default");
    let text = combined(&output);
    assert!(text.contains("error[sys.version.incompatible]"), "{text}");
}

#[test]
fn display_refusal_is_the_code_reading_the_repl_view_gives() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":display Duration default\nsys.repl.presentation_overrides | take(1)",
    );
    let text = combined(&output);
    assert_eq!(
        text.matches("error[sys.version.incompatible]").count(),
        2,
        "`:display` and the view read do not refuse alike: {text}"
    );
}

#[test]
fn display_without_a_type_and_a_formatter_prints_usage() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    for input in [":display", ":display Duration"] {
        let output = repl(directory.path(), input);
        let text = combined(&output);
        assert!(
            text.contains("usage: :display Type formatter|default"),
            "{input}: {text}"
        );
        assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{input}: {text}");
    }
}
