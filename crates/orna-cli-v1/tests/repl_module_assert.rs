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

#[test]
fn one_table_module_assertion_is_refused_with_its_spec_code() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(
        &directory,
        "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
         assert every(Book, book => book.id > 0);\n",
    );
    // ORNA-ASSERT-022: a module assertion over one table is invalid.
    let output = refused_commit(&root);
    assert!(output.contains("ORNA-A091-003"), "{output}");
}

#[test]
fn zero_table_module_assertion_is_refused_with_its_spec_code() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, "assert 1 == 1;\n");
    // ORNA-ASSERT-023: a module assertion with no table dependency is invalid.
    let output = refused_commit(&root);
    assert!(output.contains("ORNA-A091-012"), "{output}");
}

const KEYLESS_LIBRARY: &str = "pub table Book {\n    title: Str,\n}\n\n\
pub table Loan {\n    title: Str,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.title == loan.title));\n";

#[test]
fn keyless_tables_with_a_module_assertion_admit_and_reject_writes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, KEYLESS_LIBRARY);
    commit_source(&root, "keyless module assertion fixture");

    let satisfied = orna_repl(
        &root,
        "main.Book.insert({ title: \"Dune\" })\n\
         main.Loan.insert({ title: \"Dune\" })\n\
         main.Loan | count()",
    );
    assert_success(&satisfied, "satisfied keyless REPL session");
    let text = transcript(&satisfied);
    assert!(!text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&satisfied.stdout).contains("1 : Int"),
        "{text}"
    );

    let violated = orna_repl(
        &root,
        "main.Loan.insert({ title: \"Missing\" })\n\
         main.Loan | count()",
    );
    let text = transcript(&violated);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&violated.stdout).contains("1 : Int"),
        "{text}"
    );
}

#[test]
fn violated_update_is_rejected_and_leaves_the_store_unchanged() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, LIBRARY_MODULE);
    commit_source(&root, "module assertion fixture");
    assert_success(
        &orna_repl(
            &root,
            "main.Book.insert({ id: 1, title: \"Dune\" })\n\
             main.Loan.insert({ id: 1, book_id: 1 })",
        ),
        "admit fixture rows",
    );
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);

    // Book 9 does not exist, so the updated Loan violates the assertion.
    let output = orna_repl(&root, "main.Loan.update(1, { book_id: 9 })");
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["status", "--porcelain"]), status);

    // The failed update left Loan 1 pointing at Book 1, so deleting Book 1
    // must still be refused by the assertion.
    let guarded = orna_repl(&root, "main.Book.delete(1)");
    assert!(transcript(&guarded).contains("ORNA-A091-009"));
}

#[test]
fn violated_delete_is_rejected_and_keeps_the_referenced_row() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, LIBRARY_MODULE);
    commit_source(&root, "module assertion fixture");
    assert_success(
        &orna_repl(
            &root,
            "main.Book.insert({ id: 1, title: \"Dune\" })\n\
             main.Loan.insert({ id: 1, book_id: 1 })",
        ),
        "admit fixture rows",
    );

    // Deleting Book 1 leaves Loan 1 without a Book, so the assertion is false.
    let output = orna_repl(&root, "main.Book.delete(1)\nmain.Book | count()");
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
}

/// Stages the fixture and runs `orna commit`, which must refuse the module.
fn refused_commit(root: &std::path::Path) -> String {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", "refused module assertion"]);
    assert_ne!(
        committed.status.code(),
        Some(0),
        "{}",
        transcript(&committed)
    );
    transcript(&committed)
}

/// ORNA-ASSERT-026: the assertion depends on Book and Loan only, so a write to
/// the unrelated Note table must not evaluate it. The store is empty, so the
/// assertion's `exists(Book, id == 1)` conjunct is false on this candidate.
const NOTE_BESIDE_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
pub table Note(id: Int) {\n    body: Str,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id)) \
&& exists(Book, book => book.id == 1);\n";

#[test]
fn write_to_a_table_outside_the_dependency_set_skips_the_module_assertion() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, NOTE_BESIDE_LIBRARY);
    commit_source(&root, "module assertion gating fixture");

    let output = orna_repl(
        &root,
        "main.Note.insert({ id: 1, body: \"unrelated\" })\n\
         main.Note | count()",
    );
    let text = transcript(&output);
    assert_success(&output, "unrelated-table REPL write");
    assert!(!text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
}

#[test]
fn write_to_a_dependency_table_still_evaluates_the_module_assertion() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, NOTE_BESIDE_LIBRARY);
    commit_source(&root, "module assertion gating fixture");

    // Book 1 is the conjunct the assertion needs; a Book write must still be
    // evaluated, so inserting Book 2 (which leaves Book 1 absent) is refused.
    let output = orna_repl(&root, "main.Book.insert({ id: 2, title: \"Dune\" })");
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
}

const SEED_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));\n\n\
pub fn seed() {\n    Book.insert({ id: 1, title: \"Dune\" });\n    Loan.insert({ id: 2, book_id: 9 });\n}\n";

#[test]
fn violating_statement_in_a_multi_statement_call_rolls_back_every_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, SEED_LIBRARY);
    commit_source(&root, "multi-statement module assertion fixture");
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);
    let before = orna_repl(&root, "main.Book | count()");
    assert_success(&before, "count before rejected call");

    // The Book insert is valid on its own; the Loan insert violates the
    // assertion. ORNA-ASSERT-029 requires the whole boundary to roll back.
    let output = orna_repl(&root, "main.seed()\nmain.Book | count()\nmain.Loan | count()");
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("0 : Int"), "{text}");
    assert!(!stdout.contains("1 : Int"), "{text}");

    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["status", "--porcelain"]), status);
}

#[test]
fn valid_multi_statement_call_admits_every_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(
        &directory,
        &SEED_LIBRARY.replace("Loan.insert({ id: 2, book_id: 9 })", "Loan.insert({ id: 2, book_id: 1 })"),
    );
    commit_source(&root, "valid multi-statement fixture");

    let output = orna_repl(&root, "main.seed()\nmain.Book | count()\nmain.Loan | count()");
    let text = transcript(&output);
    assert_success(&output, "valid multi-statement call");
    assert!(!text.contains("ORNA-A091-009"), "{text}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(stdout.matches("1 : Int").count(), 2, "{text}");
}

const TWO_MODULE_ASSERTIONS: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n\n\
pub table Loan(id: Int) {\n    book_id: Int,\n}\n\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id)) \
&& exists(Book, book => book.id == 77);\n\
assert every(Loan, loan => exists(Book, book => book.id == loan.book_id)) \
&& exists(Loan, loan => loan.id == 88);\n";

#[test]
fn several_false_module_assertions_reject_the_write_and_keep_the_store() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, TWO_MODULE_ASSERTIONS);
    commit_source(&root, "several module assertions fixture");
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);

    // Both module assertions are false for this candidate. Whichever is
    // evaluated first, the boundary is refused as a whole (ORNA-ASSERT-027..029).
    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"Dune\" })\nmain.Book | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("0 : Int"),
        "{text}"
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["status", "--porcelain"]), status);
}
