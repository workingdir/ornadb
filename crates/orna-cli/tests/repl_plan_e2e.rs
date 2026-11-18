//! `orna repl` `:plan EXPR` prints the structured `sys.Plan` of a query without
//! running it (ORNA-SYS-053, ORNA-PLAN-002), leaves `$_` alone, and refuses an
//! expression that would write while its argument is evaluated (ORNA-REPL-003).
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

/// The identity of each plan printed, in print order: the text before
/// ` : sys.Plan` on a plan's first line, after the prompt.
fn plan_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once(" : sys.Plan"))
        .map(|(head, _)| head.trim_start_matches("> ").to_owned())
        .collect()
}

#[test]
fn plan_prints_the_structured_plan_of_a_table_query() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":plan main.Reading | take(1)");
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    let ids = plan_ids(&text);
    assert_eq!(ids.len(), 1, "one plan was not printed: {text}");
    assert!(ids[0].starts_with("plan:"), "{text}");
    assert!(text.contains("  snapshot        snapshot:"), "{text}");
    assert!(
        text.contains(&format!("  root            {}:0", ids[0])),
        "{text}"
    );
    assert!(text.contains("  estimated cost  "), "{text}");
    // ORNA-SYS-054: a plan that was not executed carries no actual counters.
    assert!(text.contains("  actual counters absent"), "{text}");
    assert!(text.contains("  warnings        none"), "{text}");
    assert!(!text.contains("error["), "{text}");
}

#[test]
fn plan_is_stable_for_one_query_and_differs_for_another() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":plan main.Reading | take(1)\n:plan main.Reading | take(1)\n:plan main.Reading | take(2)",
    );
    let text = combined(&output);
    let ids = plan_ids(&text);
    assert_eq!(ids.len(), 3, "three plans were not printed: {text}");
    assert_eq!(ids[0], ids[1], "the same query changed plan: {text}");
    assert_ne!(ids[0], ids[2], "a different limit kept the plan: {text}");
}

#[test]
fn plan_leaves_the_last_result_and_the_bindings_alone() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        "1 + 1\n:plan main.Reading | take(1)\n$_\nplanned",
    );
    let text = combined(&output);
    assert!(
        text.contains("> 2 : Int\n> plan:"),
        "the first result was not printed: {text}"
    );
    assert!(
        text.contains("  warnings        none\n> 2 : Int\n"),
        "`:plan` replaced `$_`: {text}"
    );
    assert!(
        text.contains("error[ORNA-S012-UNRESOLVED]") && text.contains("`planned`"),
        "{text}"
    );
}

#[test]
fn plan_refuses_an_expression_that_would_write_and_writes_nothing() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":plan main.Reading | take(main.Reading.insert({ id: 1, temperature: 2, label: \"x\" }).id)\nmain.Reading | take(5)",
    );
    let text = combined(&output);
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
    assert!(
        text.contains("`:plan` explains a query without running it, so its expression cannot perform table_write"),
        "{text}"
    );
    assert!(plan_ids(&text).is_empty(), "a plan was printed: {text}");
    assert!(
        text.contains("main.Reading | take(5) : Relation<Reading> · 0 rows · CWD"),
        "the refused expression wrote a row: {text}"
    );
}

#[test]
fn plan_names_the_portable_code_when_the_planner_cannot_describe_the_query() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":plan main.Reading | filter(row => row.temperature > 1)",
    );
    let text = combined(&output);
    // A filter needs a snapshot-pinned expression reference the explain cannot
    // supply; the failure is the portable unavailable-coordinate code, located
    // on the expression.
    assert!(
        text.contains(
            "error[sys.version.incompatible]: the planner has no exact plan for this query"
        ),
        "{text}"
    );
    assert!(
        text.contains("main.Reading | filter(row => row.temperature > 1)"),
        "{text}"
    );
    assert!(plan_ids(&text).is_empty(), "a plan was printed: {text}");
}

#[test]
fn plan_refuses_a_value_that_is_not_a_query_by_located_error() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":plan 1 + 1");
    let text = combined(&output);
    assert!(text.contains("error[ORNA-S021-TYPE]"), "{text}");
    assert!(
        text.contains("1 | 1 + 1\n  | ^^^^^"),
        "the error is not located on the expression: {text}"
    );
    assert!(plan_ids(&text).is_empty(), "a plan was printed: {text}");
}

#[test]
fn plan_without_an_expression_prints_usage() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":plan");
    let text = combined(&output);
    assert!(text.contains("usage: :plan EXPR"), "{text}");
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
}
