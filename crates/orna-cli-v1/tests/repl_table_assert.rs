//! Table-member assertions (ORNA-ASSERT-042) through the trusted REPL.
//!
//! A REPL write checks the post-mutation candidate against the assertions the
//! project declares before any row, checkpoint or terminal is published. A
//! false assertion refuses the write with ORNA-A091-009 and leaves the store
//! unchanged; a true assertion admits it.

#[path = "support/product.rs"]
mod product;

use product::*;

const TITLED_LIBRARY: &str = "pub table Book(id: Int) {\n    title: Str,\n    \
assert every(row => row.title != \"\");\n}\n";

#[test]
fn valid_table_assertion_admits_the_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, TITLED_LIBRARY);
    commit_source(&root, "table assertion fixture");

    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"Dune\" })\n\
         main.Book | count()",
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
fn false_table_assertion_rejects_the_insert() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, TITLED_LIBRARY);
    commit_source(&root, "table assertion fixture");

    // The empty title makes the table assertion false for this candidate.
    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"\" })\n\
         main.Book | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("0 : Int"),
        "{text}"
    );
}

#[test]
fn false_table_assertion_keeps_the_committed_store_unchanged() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, TITLED_LIBRARY);
    commit_source(&root, "table assertion fixture");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit valid row",
    );
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);

    let output = orna_repl(&root, "main.Book.insert({ id: 2, title: \"\" })");
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["status", "--porcelain"]), status);

    // The rejected row was never published; the admitted row is still counted.
    let counted = orna_repl(&root, "main.Book | count()");
    assert_success(&counted, "count rows");
    assert!(
        String::from_utf8_lossy(&counted.stdout).contains("1 : Int"),
        "{}",
        transcript(&counted)
    );
}

#[test]
fn false_table_assertion_rejects_the_update() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, TITLED_LIBRARY);
    commit_source(&root, "table assertion fixture");
    assert_success(
        &orna_repl(&root, "main.Book.insert({ id: 1, title: \"Dune\" })"),
        "admit valid row",
    );
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);

    let output = orna_repl(
        &root,
        "main.Book.update(1, { title: \"\" })\n\
         main.Book | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-A091-009"), "{text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["status", "--porcelain"]), status);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
    let title = orna_repl(
        &root,
        "main.Book | map(row => row.title) | first() ?? \"none\"",
    );
    assert_success(&title, "read the admitted title");
    assert!(
        String::from_utf8_lossy(&title.stdout).contains("\"Dune\" : Str"),
        "{}",
        transcript(&title)
    );
}

const ENUM_KEYED_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n}\n";

#[test]
fn enum_keyed_table_without_assertion_admits_the_write() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_LIBRARY);
    commit_source(&root, "enum keyed table fixture");

    // No assertion is declared, so the write keeps the unvalidated commit path
    // and the scalar-only candidate seam is never reached.
    let output = orna_repl(
        &root,
        "main.Task.insert({ level: main.Level.low, title: \"Ship\" })\n\
         main.Task | count()",
    );
    let text = transcript(&output);
    assert_success(&output, "REPL session");
    assert!(!text.contains("ORNA-REPL-EFFECT"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
}
