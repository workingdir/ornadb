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

// INTERIM (ornadb-gq4g): the candidate seam is project-wide, so an enum key on
// any table refuses every write in a project that declares an assertion.
const MIXED_KEY_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Book(id: Int) {\n    title: Str,\n    \
assert every(row => row.title != \"\");\n}\n\
pub table Journal(level: Level) {\n    note: Str,\n}\n";

const ENUM_KEYED_ASSERTED_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Task(level: Level) {\n    title: Str,\n    \
assert every(row => row.title != \"\");\n}\n";

/// Commits `asserting` over the committed source with plain git. The commit gate
/// refuses an asserting project with a non-scalar key (ORNA-ASSERT-050, interim
/// ornadb-gq4g), so the asserting source reaches HEAD the way a repository
/// committed before that gate would hold it. The store already admitted the
/// tables through `orna commit`; only the source changes here.
fn commit_asserting_source_outside_the_gate(root: &std::path::Path, asserting: &str) {
    std::fs::write(root.join("main.orna"), asserting).expect("write asserting source");
    git(root, &["add", "main.orna"]);
    git(root, &["commit", "-m", "asserting source outside the gate"]);
}

const MIXED_KEY_PLAIN_LIBRARY: &str = "pub enum Level {\n    low,\n    high,\n}\n\
pub table Book(id: Int) {\n    title: Str,\n}\n\
pub table Journal(level: Level) {\n    note: Str,\n}\n";

#[test]
fn enum_keyed_assertion_write_is_refused_and_leaves_the_store_unchanged() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, ENUM_KEYED_LIBRARY);
    commit_source(&root, "enum keyed table fixture");
    commit_asserting_source_outside_the_gate(&root, ENUM_KEYED_ASSERTED_LIBRARY);
    let head = git(&root, &["rev-parse", "HEAD"]);

    let output = orna_repl(
        &root,
        "main.Task.insert({ level: main.Level.low, title: \"Ship\" })\n\
         main.Task | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-REPL-EFFECT"), "{text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("0 : Int"),
        "{text}"
    );
}

#[test]
fn unrelated_enum_keyed_table_refuses_a_valid_write_in_an_asserting_project() {
    // INTERIM: Book has a scalar key and a true assertion, but Journal's enum
    // key is checked project-wide, so the valid write is refused. The read-only
    // count still answers. Expect admission here once the seam accepts enums.
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, MIXED_KEY_PLAIN_LIBRARY);
    commit_source(&root, "mixed key fixture");
    commit_asserting_source_outside_the_gate(&root, MIXED_KEY_LIBRARY);
    let head = git(&root, &["rev-parse", "HEAD"]);

    let output = orna_repl(
        &root,
        "main.Book.insert({ id: 1, title: \"Dune\" })\n\
         main.Book | count()",
    );
    let text = transcript(&output);
    assert!(text.contains("ORNA-REPL-EFFECT"), "{text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("0 : Int"),
        "{text}"
    );
}

const DATE_KEYED_LIBRARY: &str = "pub table Day(date: Date) {\n    title: Str,\n}\n";
const INSTANT_KEYED_LIBRARY: &str = "pub table Tick(at: Instant) {\n    value: Str,\n}\n";
const COMPOSITE_KEYED_LIBRARY: &str =
    "pub table Reading(sensor: Int, time: Instant) {\n    label: Str,\n}\n";

// Assert-free projects never reach the candidate seam, so these key types take
// the unvalidated REPL path (ORNA-KEY-003 forbids only Float keys).
fn assert_free_write_is_admitted(library: &str, write: &str, count: &str) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = initialized_database(&directory, library);
    commit_source(&root, "assert-free key fixture");
    let output = orna_repl(&root, &format!("{write}\n{count}"));
    let text = transcript(&output);
    assert_success(&output, "REPL session");
    assert!(!text.contains("ORNA-REPL-EFFECT"), "{text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 : Int"),
        "{text}"
    );
}

#[test]
fn date_keyed_table_without_assertion_admits_the_write() {
    assert_free_write_is_admitted(
        DATE_KEYED_LIBRARY,
        "main.Day.insert({ date: 2026-09-01, title: \"Ship\" })",
        "main.Day | count()",
    );
}

#[test]
fn instant_keyed_table_without_assertion_admits_the_write() {
    assert_free_write_is_admitted(
        INSTANT_KEYED_LIBRARY,
        "main.Tick.insert({ at: 2026-09-01T14:30:00Z, value: \"Ship\" })",
        "main.Tick | count()",
    );
}

// No Uuid case: the REPL does not resolve uuid7() yet (ORNA-EVAL-NAME).
#[test]
fn composite_keyed_table_without_assertion_admits_the_write() {
    assert_free_write_is_admitted(
        COMPOSITE_KEYED_LIBRARY,
        "main.Reading.insert({ sensor: 1, time: 2026-09-01T14:30:00Z, label: \"Ship\" })",
        "main.Reading | count()",
    );
}
