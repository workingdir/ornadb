//! REPL relation windows over committed and unpublished rows, through product
//! commands only: `orna init`, `orna commit`, REPL inserts, `orna publish`,
//! then fresh `orna repl` sessions that read the table back. A bare table read
//! prints its window as a header and a row table, bounded at 20 rows, and a
//! read writes no commit (spec `17-repl.md`, "Default presentation").

#[path = "support/product.rs"]
mod product;

use std::{
    ops::Range,
    path::{Path, PathBuf},
    process::Output,
};

use product::*;

const BOOK_LIBRARY: &str = "pub table Book(id: Str) {\n    title: Str,\n}\n";

/// One insert per index in `range`, keyed `b00`, `b01`, ... with a matching
/// title.
fn inserts(range: Range<usize>) -> String {
    range
        .map(|index| {
            format!("main.Book.insert({{ id: \"b{index:02}\", title: \"Title b{index:02}\" }})")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A library whose `range` rows are inserted through the REPL and published;
/// an empty range publishes nothing.
fn published_books(directory: &tempfile::TempDir, range: Range<usize>) -> PathBuf {
    let root = initialized_database(directory, BOOK_LIBRARY);
    commit_source(&root, "book library");
    if !range.is_empty() {
        repl_writes(&root, &inserts(range));
        publish(&root, "publish books");
    }
    root
}

/// One fresh REPL session that reads the whole `main.Book` table.
fn read_books(root: &Path) -> Output {
    let output = orna_repl(root, "main.Book");
    assert_success(&output, "REPL relation read");
    output
}

#[test]
fn committed_rows_are_read_as_a_relation_window() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..3);

    let output = read_books(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 rows · CWD"), "{}", transcript(&output));
    for id in ["b00", "b01", "b02"] {
        assert!(stdout.contains(id), "{}", transcript(&output));
    }
}

#[test]
fn empty_committed_table_reads_as_zero_rows() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..0);

    let output = read_books(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0 rows · CWD"), "{}", transcript(&output));
}

#[test]
fn window_holds_twenty_rows_and_says_when_more_remain() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..25);

    let output = read_books(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("first 20 rows · CWD"),
        "{}",
        transcript(&output)
    );
    assert!(stdout.contains("b19"), "{}", transcript(&output));
    assert!(!stdout.contains("b20"), "{}", transcript(&output));
}

#[test]
fn take_bounds_the_relation_pipeline_to_its_window() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..3);

    let output = orna_repl(&root, "main.Book | take(2)");
    assert_success(&output, "REPL bounded relation read");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 rows · CWD"), "{}", transcript(&output));
    assert!(stdout.contains("b01"), "{}", transcript(&output));
    assert!(!stdout.contains("b02"), "{}", transcript(&output));
}

#[test]
fn unpublished_rows_are_read_in_a_fresh_session_and_reads_write_nothing() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..3);
    let head_before = head(&root);

    repl_writes(
        &root,
        "main.Book.insert({ id: \"local\", title: \"Only local\" })",
    );
    assert_eq!(
        head(&root),
        head_before,
        "an unpublished insert commits nothing"
    );

    let output = read_books(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4 rows · CWD"), "{}", transcript(&output));
    assert!(stdout.contains("Only local"), "{}", transcript(&output));
    assert_eq!(head(&root), head_before, "a read commits nothing");
}

#[test]
fn refused_writes_leave_the_rows_and_head_unchanged() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = published_books(&directory, 0..3);
    let head_before = head(&root);

    // A duplicate key and an ill-typed title are both refused.
    let refused = orna_repl(
        &root,
        "main.Book.insert({ id: \"b00\", title: \"Shadow\" })\n\
         main.Book.insert({ id: \"b91\", title: 5 })",
    );
    let refusals = transcript(&refused);
    assert!(refusals.contains("error["), "{refusals}");

    let output = read_books(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 rows · CWD"), "{}", transcript(&output));
    assert!(
        !stdout.contains("Shadow") && !stdout.contains("b91"),
        "{}",
        transcript(&output)
    );
    assert_eq!(head(&root), head_before, "a refused write commits nothing");
}
