//! The commit's table-assertion gate (ORNA-ASSERT-042, ORNA-ASSERT-046,
//! ORNA-ASSERT-048) through `orna commit`: the staged candidate's own rows must
//! satisfy every admitted table assertion before any ref advances.
//!
//! Rows reach the candidate only once they are published, so each test admits
//! its rows under a project that declares no assertion, publishes them, and
//! then stages the asserting source.
//!
//! Interim gap (bead ornadb-gq4g): the transactional seam reads only scalar
//! primary keys. An asserting project with a table of any other key is refused
//! whole at commit (E2300); no subset of its assertions is validated
//! (ORNA-ASSERT-050).

#[path = "support/product.rs"]
mod product;

use product::*;
use std::fs;
use std::path::Path;
use std::process::Output;

const BOOK_TABLE: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n";

/// The published row `Dune` violates this assertion.
const BOOK_REJECTING_DUNE: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    assert every(book => book.title != \"Dune\");\n}\n";

/// The published row `Dune` satisfies this assertion.
const BOOK_ACCEPTING_DUNE: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    assert every(book => book.title != \"\");\n}\n";

const ENUM_KEYED_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n}\n";

/// The enum-keyed `Task` table with a table assertion.
const ENUM_KEYED_ASSERTED_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n    assert every(row => row.title != \"\");\n}\n";

/// The enum-keyed `Task` table beside a scalar `Book` table whose assertion
/// the published row `Dune` violates.
const ENUM_KEYED_BESIDE_BOOK_REJECTING_DUNE: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n}\n\n\
pub table Book(id: Int) {\n    title: Str,\n    assert every(book => book.title != \"Dune\");\n}\n";

/// The enum-keyed `Task` table beside an assert-free scalar `Book` table.
const ENUM_KEYED_BESIDE_BOOK_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n}\n\n\
pub table Book(id: Int) {\n    title: Str,\n}\n";

/// Writes `main.orna` with `source`, stages it with the metadata, and runs
/// `orna commit`, returning its output without asserting success.
fn stage_and_commit(root: &Path, source: &str, message: &str) -> Output {
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    orna(root, &["commit", "-m", message])
}

#[test]
fn violating_published_row_refuses_the_commit_and_keeps_head() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_TABLE);
    commit_source(&root, "book table");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit the row before the assertion exists",
    );
    publish(&root, "Dune");
    let before = head(&root);

    let refused = stage_and_commit(&root, BOOK_REJECTING_DUNE, "assert away the stored row");
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(head(&root), before, "{text}");
}

#[test]
fn holding_published_row_commits_under_its_assertion() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_TABLE);
    commit_source(&root, "book table");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit the row",
    );
    publish(&root, "Dune");
    let before = head(&root);

    let committed = stage_and_commit(&root, BOOK_ACCEPTING_DUNE, "assert a holding rule");
    assert_success(&committed, "commit with a holding assertion");
    assert_ne!(head(&root), before);
}

#[test]
fn assert_free_project_commits_its_published_rows() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_TABLE);
    commit_source(&root, "book table");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit the row",
    );
    publish(&root, "Dune");
    let before = head(&root);

    let committed = stage_and_commit(
        &root,
        &format!("// rows without assertions\n{BOOK_TABLE}"),
        "rows without assertions",
    );
    assert_success(&committed, "commit of an assert-free project");
    assert_ne!(head(&root), before);
}

/// Interim gap (bead ornadb-gq4g): an asserting project whose table has an
/// enum key is refused whole at commit, even with no rows to validate. An
/// enum-keyed row cannot be published (`E2300 source row key is not
/// canonical`), so this test stages the table with no rows.
#[test]
fn interim_non_scalar_key_asserting_project_is_refused_whole() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_LIBRARY);
    commit_source(&root, "enum keyed table fixture");
    let before = head(&root);

    let refused =
        stage_and_commit(&root, ENUM_KEYED_ASSERTED_LIBRARY, "assert on enum-keyed table");
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("E2300"), "{text}");
    assert_eq!(head(&root), before, "{text}");
}

/// Interim gap (bead ornadb-gq4g): a violating scalar table beside an
/// enum-keyed table is refused whole with E2300. The published row `Dune`
/// violates the scalar `Book` assertion, but that assertion is not evaluated
/// on its own, so the refusal does not name `ORNA-A091-009`.
#[test]
fn interim_non_scalar_key_refuses_a_violating_scalar_table_whole() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_BESIDE_BOOK_LIBRARY);
    commit_source(&root, "enum-keyed table beside a scalar table");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit the row before the assertion exists",
    );
    publish(&root, "Dune");
    let before = head(&root);

    let refused = stage_and_commit(
        &root,
        ENUM_KEYED_BESIDE_BOOK_REJECTING_DUNE,
        "assert beside an enum-keyed table",
    );
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("E2300"), "{text}");
    assert!(!text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(head(&root), before, "{text}");
}

/// Assert-free projects are unaffected: the commit gate refuses only a project
/// that declares table assertions, so an enum-keyed table with no assertion
/// still commits.
#[test]
fn assert_free_project_with_non_scalar_key_still_commits() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_LIBRARY);
    commit_source(&root, "enum keyed table fixture");
    let before = head(&root);

    let committed = stage_and_commit(
        &root,
        &format!("// assert-free change\n{ENUM_KEYED_LIBRARY}"),
        "assert-free change to an enum-keyed table",
    );
    assert_success(&committed, "commit of an assert-free project with an enum key");
    assert_ne!(head(&root), before);
}
