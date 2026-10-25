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
pub fn expect_three(): Bool { let rows = music | count(); assert rows == 3; true }\n\
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

/// An `orna init` project with each `(file, source)` written and no commit: a Git
/// worktree whose HEAD is unborn.
fn uncommitted_modules(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    for (file, source) in files {
        std::fs::write(directory.path().join(file), source).expect("write module");
    }
    directory
}

/// An `orna init` project with `source` written as `main.orna` and no commit.
fn uncommitted_project(source: &str) -> tempfile::TempDir {
    uncommitted_modules(&[("main.orna", source)])
}

/// Seeds the committed format-3 table through the REPL, whose table mutations
/// carry their admitted `TableObjectId` (`repl_transaction.rs`). The runtime
/// tail is then published so the three rows are committed at HEAD. A one-shot
/// `run` write stages unstamped mutations that `publish` rejects (E2300).
fn committed_rows(source: &str) -> tempfile::TempDir {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(source, "music");
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

fn committed_three_rows() -> tempfile::TempDir {
    committed_rows(SOURCE)
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
        &[
            "repl",
            "main.music | map(row => row.id) | first() ?? \"none\"",
        ],
    );
    assert_success(&first, "first committed row");
    assert!(
        combined(&first).contains("\"london\" : Str"),
        "first row must be the smallest key: {}",
        combined(&first)
    );
    let second = orna(
        directory.path(),
        &[
            "repl",
            "main.music | map(row => row.id) | drop(1) | first() ?? \"none\"",
        ],
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
        &[
            "repl",
            "main.nums | map(row => row.n) | drop(1) | first() ?? 0",
        ],
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

const SYS_STORAGE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn storage_probe(): Bool { let rows = music | count(); let pending = sys.Storage | map(storage => storage.pending_rows) | first() ?? -1; assert rows == 3; assert pending >= 0; true }\n\
pub fn main() = music | count();\n";

/// B: a table-bearing entry that also scans `sys.Storage` gets the publication
/// snapshot and the sys host context through the table route. Without them the
/// `sys.Storage` read fails, so success shows the table route threads both.
#[test]
fn invoke_table_entry_reading_sys_storage_gets_publication_metadata() {
    let directory = committed_rows(SYS_STORAGE_SOURCE);
    let output = orna(directory.path(), &["invoke", "storage_probe"]);
    assert_success(&output, "table-bearing entry reading sys.Storage");
    assert!(
        combined(&output).contains("invocation completed"),
        "sys.Storage read did not complete: {}",
        combined(&output)
    );
}

/// C: a held runtime owner lock does not block a one-shot read. The busy lock
/// opens the runtime as a secondary participant, so the read completes and the
/// process exits within the deadline instead of waiting on the lock.
#[test]
fn invoke_reads_committed_rows_while_runtime_owner_lock_is_held() {
    let directory = committed_three_rows();
    let repository =
        orna_repository_v1::Repository::discover(directory.path()).expect("repository");
    let _owner = repository
        .acquire_runtime_owner_lock([9; 16])
        .expect("owner lock held by the test");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.path().to_str().expect("UTF-8 path")])
        .args(["invoke", "expect_three"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("orna CLI process");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if child.try_wait().expect("poll orna").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("invoke hung while the runtime owner lock was held");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("orna output");
    assert_success(&output, "invoke while the owner lock is held");
    assert!(
        combined(&output).contains("invocation completed"),
        "invoke did not complete under a held lock: {}",
        combined(&output)
    );
}

const STOCK_SOURCE: &str = "pub table Stock(location: Str, sku: Str) { quantity: Int, }\n\
pub fn main() = Stock | count();\n";

/// ORNA-ORDER-001 over a composite key: rows scan by ascending canonical key
/// tuple (location, then sku). Inserted b/x, a/y, a/x, so the scan order is
/// a/x, a/y, b/x.
#[test]
fn table_scan_orders_composite_keys_by_tuple() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(STOCK_SOURCE, "Stock");
    for row in [
        "main.Stock.insert({ location: \"b\", sku: \"x\", quantity: 1 })",
        "main.Stock.insert({ location: \"a\", sku: \"y\", quantity: 2 })",
        "main.Stock.insert({ location: \"a\", sku: \"x\", quantity: 3 })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    let cases = [
        (
            "main.Stock | map(row => row.sku) | first() ?? \"none\"",
            "\"x\" : Str",
        ),
        (
            "main.Stock | map(row => row.sku) | drop(1) | first() ?? \"none\"",
            "\"y\" : Str",
        ),
        (
            "main.Stock | map(row => row.location) | drop(2) | first() ?? \"none\"",
            "\"b\" : Str",
        ),
    ];
    for (expression, expected) in cases {
        let output = orna(directory.path(), &["repl", expression]);
        assert_success(&output, expression);
        assert!(
            combined(&output).contains(expected),
            "{expression} must yield {expected}: {}",
            combined(&output)
        );
    }
}

const MULTI_MAIN: &str = "use helper;\npub fn main() = helper.count_music();\n";
const MULTI_HELPER_TABLE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn count_music() = music | count();\n";

/// A table read reached across a module boundary cannot be proven by the one-shot
/// pure path: it must fail closed with the named E2200 reason instead of evaluating.
#[test]
fn multi_module_table_read_fails_closed_with_named_reason() {
    let directory = uncommitted_modules(&[
        ("main.orna", MULTI_MAIN),
        ("helper.orna", MULTI_HELPER_TABLE),
    ]);
    let output = orna(directory.path(), &["invoke", "main"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "a cross-module table read must fail closed: {}",
        combined(&output)
    );
    let text = combined(&output);
    assert!(
        text.contains("E2200"),
        "cross-module table read must report E2200: {text}"
    );
    assert!(
        text.contains(
            "table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run"
        ),
        "cross-module table read must name the multi-module limit: {text}"
    );
}

const FIELD_SOURCE: &str = "pub table sample(id: Int) { label: Str, active: Bool, ratio: Float, price: Decimal, }\n\
pub fn check(): Bool { let row = sample | filter(row => row.id == 2) | one(); assert row.label == \"beta\"; assert row.active == false; assert row.ratio == 2.5f; assert row.price == 3.2500; true }\n\
pub fn main() = sample | count();\n";

/// The committed-row run path converts every scalar stored field type into the
/// evaluator record (stored-to-evaluator conversion in `orna-project-v1`
/// `evaluator_rows_from_stored`). Each field of a committed row must read back
/// with its value, not only the row count.
#[test]
fn invoke_reads_every_scalar_field_type_of_a_committed_row() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(FIELD_SOURCE, "sample");
    for row in [
        "main.sample.insert({ id: 1, label: \"alpha\", active: true, ratio: 1.5f, price: 1.2500 })",
        "main.sample.insert({ id: 2, label: \"beta\", active: false, ratio: 2.5f, price: 3.2500 })",
    ] {
        assert_success(&orna(directory.path(), &["repl", row]), "REPL seed insert");
    }
    assert_success(
        &orna(directory.path(), &["publish", "--message", "seed"]),
        "publish seeded rows",
    );
    let output = orna(directory.path(), &["invoke", "check"]);
    assert_success(&output, "invoke over every committed field type");
    assert!(
        combined(&output).contains("invocation completed"),
        "invoke did not complete over every field type: {}",
        combined(&output)
    );
}
