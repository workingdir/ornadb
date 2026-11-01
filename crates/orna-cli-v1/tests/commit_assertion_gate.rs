//! The commit's assertion gate (ORNA-ASSERT-026, ORNA-ASSERT-042,
//! ORNA-ASSERT-048, ORNA-ASSERT-051) through `orna commit`: the staged
//! candidate's own rows must satisfy every admitted table assertion, and every
//! module assertion whose dependency table the commit changes, before any ref
//! advances. A refused module assertion is reported at its module and source span.
//!
//! Rows reach the candidate only once they are published, so each test admits
//! its rows under a project that declares no assertion, publishes them, and
//! then stages the asserting source.
//!
//! Interim gap (bead ornadb-gq4g): the transactional seam reads only scalar
//! primary keys. A check that reads a table of any other key is refused as
//! unvalidated at commit (E2300), and no subset of the candidate's assertions is
//! published (ORNA-ASSERT-049, ORNA-ASSERT-050). A project whose assertions do
//! not read such a table is not refused for it.

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

/// `Book` and `Loan` for a module assertion over both tables.
const BOOK_LOAN_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n    copies: Int,\n}\n";

/// `Book` gains a constant `edition` default, so the published `Dune` row reads
/// as edition 7. The module assertion needs a Book whose edition equals each
/// Loan's copies; the Loan with one copy finds none, so the assertion is false.
const BOOK_EDITION_LOAN_MODULE_LIBRARY: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    edition: Int = 7,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n    copies: Int,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.edition == loan.copies));\n";

/// `Book`, `Loan` and an empty `Shelf` for a module assertion over `Loan` and `Shelf`.
const BOOK_LOAN_SHELF_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
pub table Shelf(id: Int) {\n    label: Str,\n}\n";

/// `Book` gains the constant `edition` default; `Loan` and `Shelf` keep their
/// definitions. The module assertion over `Loan` and `Shelf` is false because
/// `Shelf` has no rows, so it refuses the commit only if the commit evaluates it.
const BOOK_EDITION_LOAN_SHELF_MODULE_LIBRARY: &str =
    "pub table Book(id: Int) {\n    title: Str,\n    edition: Int = 7,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
pub table Shelf(id: Int) {\n    label: Str,\n}\n\n\
assert every(Loan, loan => exists(Shelf, shelf => shelf.id == loan.book_id));\n";

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

/// Interim gap (bead ornadb-gq4g): the enum-keyed `Task` table's own assertion
/// reads a table the seam cannot lower, so the commit is refused as unvalidated
/// with its cause, even with no rows to validate. An enum-keyed row cannot be
/// published (`E2300 source row key is not canonical`), so this test stages the
/// table with no rows.
#[test]
fn enum_keyed_table_assertion_refuses_the_commit_with_its_cause() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_LIBRARY);
    commit_source(&root, "enum keyed table fixture");
    let before = head(&root);

    let refused =
        stage_and_commit(&root, ENUM_KEYED_ASSERTED_LIBRARY, "assert on enum-keyed table");
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("E2300"), "{text}");
    assert!(
        text.contains("could not be validated against its assertions"),
        "{text}"
    );
    assert!(
        text.contains("an assertion could not be evaluated over the staged rows"),
        "{text}"
    );
    assert_eq!(head(&root), before, "{text}");
}

/// Interim gap (bead ornadb-gq4g), checked per assertion: the violating scalar
/// `Book` assertion is still evaluated beside the enum-keyed `Task` table, which
/// declares no assertion. The published row `Dune` violates the scalar `Book`
/// assertion, so the refusal names `ORNA-A091-009` and no unvalidated cause.
#[test]
fn scalar_table_assertion_beside_enum_keyed_table_is_still_evaluated() {
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
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(text.contains("table assertion is false"), "{text}");
    assert!(!text.contains("could not be validated"), "{text}");
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

/// ORNA-ASSERT-026 and ORNA-ASSERT-051: a module assertion whose dependency table
/// the commit changes is evaluated over the candidate rows, and its refusal is
/// reported at its module and source span rather than as a table assertion.
#[test]
fn module_assertion_over_changed_table_refuses_the_commit_with_its_span() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_LOAN_LIBRARY);
    commit_source(&root, "book and loan tables");
    assert_success(
        &orna_repl(
            &root,
            "main.Book.insert({ id: 1, title: \"Dune\" })\n\
             main.Loan.insert({ id: 1, book_id: 1, copies: 1 })",
        ),
        "admit the rows before the module assertion exists",
    );
    publish(&root, "Dune");
    let before = head(&root);

    // The `edition` default reads the published Dune row as edition 7, so Book is
    // in the changed set and the module assertion applies. No Book has edition 1.
    let refused = stage_and_commit(
        &root,
        BOOK_EDITION_LOAN_MODULE_LIBRARY,
        "assert across the changed table",
    );
    let text = transcript(&refused);
    assert_ne!(refused.status.code(), Some(0), "{text}");
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(text.contains("assertion at"), "{text}");
    assert!(!text.contains("table assertion is false"), "{text}");
    assert_eq!(head(&root), before, "{text}");
}

/// ORNA-ASSERT-026: a module assertion whose dependency tables the commit does not
/// change is not evaluated, even though it is false over the candidate rows.
#[test]
fn module_assertion_over_unchanged_tables_is_skipped_by_the_commit() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, BOOK_LOAN_SHELF_LIBRARY);
    commit_source(&root, "book, loan and shelf tables");
    assert_success(
        &orna_repl(
            &root,
            "main.Book.insert({ id: 1, title: \"Dune\" })\n\
             main.Loan.insert({ id: 1, book_id: 1 })",
        ),
        "admit the rows before the module assertion exists",
    );
    publish(&root, "Dune");
    let before = head(&root);

    // Only Book changes. Without the changed-table scoping the assertion would be
    // evaluated and refuse, because Shelf is empty and no shelf matches the Loan.
    let committed = stage_and_commit(
        &root,
        BOOK_EDITION_LOAN_SHELF_MODULE_LIBRARY,
        "change only the book table",
    );
    assert_success(&committed, "commit that changes only Book");
    assert_ne!(head(&root), before);
}
