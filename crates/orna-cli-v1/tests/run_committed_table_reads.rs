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

/// Seeds the same three keys as `committed_three_rows`, inserted rome, london,
/// paris (not ascending), so storage insertion order cannot satisfy ORNA-ORDER-001.
fn committed_rows_inserted_out_of_order() -> tempfile::TempDir {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(SOURCE, "music");
    for row in [
        "main.music.insert({ id: \"rome\", title: \"Third\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" })",
        "main.music.insert({ id: \"paris\", title: \"Second\", artist: \"Kieran\" })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    directory
}

/// ORNA-ORDER-001: a base table scan is ordered by ascending canonical primary
/// key. The scan pages one row at a time (`RELATION_SCAN_PAGE_ROWS`), so the
/// second and third rows come from later pages.
#[test]
fn table_scan_orders_committed_rows_by_ascending_primary_key() {
    let directory = committed_rows_inserted_out_of_order();
    let first = orna(
        directory.path(),
        &["repl", "main.music | map(row => row.id) | first() ?? \"none\""],
    );
    assert_success(&first, "first committed row");
    assert!(
        combined(&first).contains("\"london\" : Str"),
        "first row must be the smallest key: {}",
        combined(&first)
    );
    let second = orna(
        directory.path(),
        &["repl", "main.music | map(row => row.id) | drop(1) | first() ?? \"none\""],
    );
    assert_success(&second, "second committed row");
    assert!(
        combined(&second).contains("\"paris\" : Str"),
        "second row must be the next key: {}",
        combined(&second)
    );
}

const INT_KEY_SOURCE: &str = "pub table nums(n: Int) { label: Str, }\n\
pub fn main() = nums | count();\n";

/// Seeds an Int-keyed table with 2, -1, 300. Their canonical CBOR key bytes sort as
/// 2, 300, -1, which is not numeric order.
fn committed_int_keys() -> tempfile::TempDir {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(INT_KEY_SOURCE, "nums");
    for row in [
        "main.nums.insert({ n: 2, label: \"two\" })",
        "main.nums.insert({ n: -1, label: \"minus\" })",
        "main.nums.insert({ n: 300, label: \"big\" })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    directory
}

/// ORNA-ORDER-001 over a non-text key: Int keys scan in numeric order.
#[test]
fn table_scan_orders_int_keys_numerically() {
    let directory = committed_int_keys();
    let first = orna(
        directory.path(),
        &["repl", "main.nums | map(row => row.n) | first() ?? 0"],
    );
    assert_success(&first, "first Int key");
    assert!(
        combined(&first).contains("-1 : Int"),
        "first Int key must be the smallest: {}",
        combined(&first)
    );
    let second = orna(
        directory.path(),
        &["repl", "main.nums | map(row => row.n) | drop(1) | first() ?? 0"],
    );
    assert_success(&second, "second Int key");
    assert!(
        combined(&second).contains("2 : Int"),
        "second Int key must be the next numeric key: {}",
        combined(&second)
    );
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
