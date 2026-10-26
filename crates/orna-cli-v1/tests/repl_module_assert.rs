//! Cross-table module assertions (ORNA-ASSERT-021..030) through the trusted REPL.
//!
//! A module assertion depends on two tables, so it is not owned by either one.
//! A REPL write that changes a table in its dependency set must evaluate it
//! against the unpublished candidate database before publication; a false
//! result aborts the whole write boundary and leaves the store unchanged.

#[path = "support/product.rs"]
mod product;

use product::*;

const LIBRARY_MODULE: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));\n";

#[test]
fn satisfied_cross_table_assertion_admits_the_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, LIBRARY_MODULE);
    commit_source(&root, "module assertion fixture");

    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"Dune\" })\n\
         main.Loan.insert({ id: 1, book_id: 1 })\n\
         main.Loan | count()",
    );
    let text = transcript(&output);
    assert_success(&output, "REPL session");
    assert!(!text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
}

#[test]
fn violated_cross_table_assertion_rejects_the_write_and_keeps_the_store() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, LIBRARY_MODULE);
    commit_source(&root, "module assertion fixture");

    // Book 9 does not exist, so the module assertion is false for this Loan.
    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"Dune\" })\n\
         main.Loan.insert({ id: 2, book_id: 9 })\n\
         main.Loan | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("0 : Int"),
        "{text}"
    );
}
