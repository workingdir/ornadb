//! One-shot table reads over committed identities (ornadb-03ol, Orna-1.1.0
//! 10-execution.md). `orna invoke <fn>` evaluates a root-module entry. An entry
//! that names no table evaluates without committed table state, even in a
//! project with no commit. An entry that names an admitted table binds that
//! table to its committed identity at `HEAD` and reads the committed rows; with
//! no commit it fails closed. A mutating entry fails closed because one-shot
//! invocation never commits table mutations.

use std::{
    path::Path,
    process::{Command, Output},
};

#[path = "support/format3.rs"]
mod format3;

/// `seed` inserts three rows. A durable `run main.seed` followed by `publish`
/// commits them to HEAD. `expect_three` divides by the row count, so it succeeds
/// only when the committed table holds rows; an empty committed table fails at
/// evaluation with a divide-by-zero.
const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); music.insert({ id: \"paris\", title: \"Second\", artist: \"Kieran\" }); music.insert({ id: \"rome\", title: \"Third\", artist: \"Kieran\" }); }\n\
pub fn expect_three() = 6 / (music | count());\n\
pub fn main() = music | count();\n";

const WRITE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); }\n\
pub fn main() = music | count();\n";

const SYS_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn sys_probe(): Str { let rows = music | count(); sys.rt.info().language_version }\n\
pub fn main() = music | count();\n";

const NON_TABLE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn answer() = 42;\n\
pub fn main() = 42;\n";

const TABLE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn count_music() = music | count();\n\
pub fn main() = music | count();\n";

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} failed: {}",
        combined(output)
    );
}

/// An `orna init` project with `source` written and no commit: a Git worktree
/// whose HEAD is unborn.
fn uncommitted_project(source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("write main.orna");
    directory
}

/// Seeds the committed format-3 table through the REPL, whose table mutations
/// carry their admitted `TableObjectId` (`repl_transaction.rs`). The runtime
/// tail is then published so the three rows are committed at HEAD. A one-shot
/// `run` write stages unstamped mutations that `publish` rejects (E2300).
fn committed_three_rows() -> tempfile::TempDir {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(SOURCE, "music");
    for row in [
        "main.music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"paris\", title: \"Second\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"rome\", title: \"Third\", artist: \"Kieran\" })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    directory
}

#[test]
fn invoke_reads_three_committed_rows_as_a_value() {
    let directory = committed_three_rows();
    let output = orna(directory.path(), &["invoke", "expect_three"]);
    assert_success(&output, "invoke over three committed rows");
    assert!(
        combined(&output).contains("invocation completed"),
        "invoke did not complete: {}",
        combined(&output)
    );
}

#[test]
fn invoke_over_empty_committed_table_fails_at_evaluation() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(SOURCE, "music");
    let output = orna(directory.path(), &["invoke", "expect_three"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "empty committed table must count zero rows: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("pure project invocation failed"),
        "empty read must fail at evaluation: {}",
        combined(&output)
    );
}

#[test]
fn repl_reads_three_committed_rows_as_a_value() {
    let directory = committed_three_rows();
    let output = orna(directory.path(), &["repl", "main.music | count()"]);
    assert_success(&output, "REPL bare-table read");
    assert!(
        combined(&output).contains("3 : Int"),
        "REPL bare-table read must return the committed row count: {}",
        combined(&output)
    );
}

#[test]
fn invoke_mutating_table_entry_fails_closed() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(WRITE_SOURCE, "music");
    let output = orna(directory.path(), &["invoke", "seed"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "one-shot invoke must not commit table mutations: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("table mutations"),
        "mutating invoke must name the rejected mutation: {}",
        combined(&output)
    );
}

#[test]
fn invoke_table_entry_with_sys_host_context_evaluates() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(SYS_SOURCE, "music");
    let output = orna(directory.path(), &["invoke", "sys_probe"]);
    assert_success(&output, "table-bearing entry reading sys.rt.info");
    assert!(
        combined(&output).contains("invocation completed"),
        "sys host read did not complete: {}",
        combined(&output)
    );
}

#[test]
fn uncommitted_project_with_non_table_entry_still_succeeds() {
    let directory = uncommitted_project(NON_TABLE_SOURCE);
    let output = orna(directory.path(), &["invoke", "answer"]);
    assert_success(
        &output,
        "non-table entry of an uncommitted table-bearing project",
    );
    assert!(
        combined(&output).contains("invocation completed"),
        "non-table entry did not complete: {}",
        combined(&output)
    );
}

#[test]
fn uncommitted_project_table_read_fails_with_named_reason() {
    let directory = uncommitted_project(TABLE_SOURCE);
    let output = orna(directory.path(), &["invoke", "count_music"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "a table read without a HEAD commit must fail: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("table reads need a committed project"),
        "missing HEAD must name the table-read requirement: {}",
        combined(&output)
    );
}
