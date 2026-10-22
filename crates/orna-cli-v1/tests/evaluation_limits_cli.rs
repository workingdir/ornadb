//! Product-path proof for evaluator call depth and `case` arm slots.
//!
//! ORNA-CLOSURE-004: the default depth limit bounds active calls, and
//! exhausting it is reported as the explicit `ORNA-EVAL-LIMIT` code rather than
//! a partial result. ORNA-CFLOW-005: an assignment inside a `case` arm replaces
//! the enclosing `let` slot.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

const LIMITS_MODULE: &str = "
pub fn depth(n: Int): Int = if n == 0 { 0 } else { 1 + depth(n - 1) };

pub fn tally(): Int {
    let total = 0;
    for item in [Some(1), null, Some(3)] {
        case item {
            Some(value): {
                total += value;
            },
            null: {},
        }
    }
    total
}
";

const MAIN_MODULE: &str = "
use limits;

pub fn main(): Int = limits.depth(3);
";

fn initialized_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary Orna project");
    std::fs::write(directory.path().join("main.orna"), MAIN_MODULE).expect("main module");
    std::fs::write(directory.path().join("limits.orna"), LIMITS_MODULE).expect("limits module");
    let initialized = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna init process");
    assert_eq!(
        initialized.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    directory
}

fn run_repl(directory: &std::path::Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("wait for Orna REPL")
}

#[test]
fn repl_recursion_is_bounded_by_active_calls_not_expression_nesting() {
    let project = initialized_project();
    let output = run_repl(project.path(), "use limits;\nlimits.depth(60)\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("60 : Int"), "{stdout}");
    assert!(!stdout.contains("ORNA-EVAL-LIMIT"), "{stdout}");
}

#[test]
fn repl_reports_call_budget_exhaustion_as_an_explicit_limit() {
    let project = initialized_project();
    let output = run_repl(project.path(), "use limits;\nlimits.depth(70)\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ORNA-EVAL-LIMIT"), "{stdout}");
    assert!(!stdout.contains(": Int"), "{stdout}");
}

#[test]
fn repl_case_arm_assignment_updates_the_enclosing_slot() {
    let project = initialized_project();
    let output = run_repl(project.path(), "use limits;\nlimits.tally()\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("4 : Int"), "{stdout}");
}
