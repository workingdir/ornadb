//! The publish gate (ORNA-ASSERT-042, ORNA-ASSERT-045, ORNA-ASSERT-046,
//! ORNA-ASSERT-048) through `orna publish`: the candidate built from the frozen
//! runtime range must satisfy every admitted table assertion before any ref,
//! pin or journal names it. A refused publication leaves HEAD unchanged and the
//! frozen range pending, so a retry after the fix publishes the same rows.
//!
//! The commit gate sees only committed rows, so each test leaves its row
//! pending while the asserting source commits. The publish gate is then the
//! check that refuses the row.

#[path = "support/product.rs"]
mod product;

use product::*;
use std::fs;
use std::path::Path;
use std::process::Output;

const BOOK_TABLE: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n";

/// The pending row `Dune` violates this assertion.
const BOOK_REJECTING_DUNE: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    assert every(book => book.title != \"Dune\");\n}\n";

/// The pending row `Dune` satisfies this assertion.
const BOOK_ACCEPTING_DUNE: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    assert every(book => book.title != \"\");\n}\n";

/// Writes `main.orna` with `source`, stages it with the metadata, and runs
/// `orna commit`, returning its output without asserting success.
fn stage_and_commit(root: &Path, source: &str, message: &str) -> Output {
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    orna(root, &["commit", "-m", message])
}

/// Runs `orna publish` and returns its output without asserting success.
fn publish_output(root: &Path, message: &str) -> Output {
    orna(root, &["publish", "--message", message])
}

/// A database whose `Book` table commits the source `asserting` while the row
/// `Dune` stays pending, so the publish gate is the first check that sees it.
fn database_with_pending_dune(directory: &tempfile::TempDir, asserting: &str) -> std::path::PathBuf {
    let root = initialized_database(directory, BOOK_TABLE);
    commit_source(&root, "book table");
    repl_writes(&root, "main.Book.insert({ id: 1, title: \"Dune\" })");
    assert_success(
        &stage_and_commit(&root, asserting, "assert over the pending row"),
        "commit the asserting source",
    );
    root
}

#[test]
fn assert_free_project_publishes_its_pending_rows() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_TABLE);
    commit_source(&root, "book table");
    repl_writes(&root, "main.Book.insert({ id: 1, title: \"Dune\" })");
    let before = head(&root);

    let report = publish(&root, "Dune");
    assert!(report.starts_with("published "), "{report}");
    assert_ne!(head(&root), before);
}

#[test]
fn violating_pending_row_refuses_the_publish_and_keeps_head() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = database_with_pending_dune(&directory, BOOK_REJECTING_DUNE);
    let before = head(&root);

    let refused = publish_output(&root, "Dune");
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(head(&root), before, "{text}");
}

#[test]
fn refused_publish_keeps_the_range_pending_for_a_retry_after_the_fix() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = database_with_pending_dune(&directory, BOOK_REJECTING_DUNE);
    let before = head(&root);
    let refused = publish_output(&root, "Dune");
    assert_ne!(refused.status.code(), Some(0), "{}", transcript(&refused));
    assert_eq!(head(&root), before);

    // The fix: the asserting source changes to one that `Dune` satisfies.
    assert_success(
        &stage_and_commit(&root, BOOK_ACCEPTING_DUNE, "assert a holding rule"),
        "commit the holding assertion",
    );
    let fixed = head(&root);
    assert_ne!(fixed, before);

    // A retry publishes the same pending row; a consumed range would report
    // "nothing to publish" instead.
    let report = publish(&root, "Dune");
    assert!(report.starts_with("published "), "{report}");
    assert_ne!(head(&root), fixed);
}

#[test]
fn holding_pending_row_publishes_under_its_assertion() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = database_with_pending_dune(&directory, BOOK_ACCEPTING_DUNE);
    let before = head(&root);

    let report = publish(&root, "Dune");
    assert!(report.starts_with("published "), "{report}");
    assert_ne!(head(&root), before);
}
