//! A run-staged delete or rekey of a published row (ornadb-9rzyn): `orna run`
//! accepts the change, the merged view (the REPL's CWD relation: committed HEAD
//! plus the runtime's pending changes) stops showing the row it removed or moved,
//! and the next `orna publish` publishes that change instead of failing with
//! "publication candidate could not be prepared".

#[path = "support/product.rs"]
mod product;

use std::path::Path;

use product::*;
use tempfile::TempDir;

const SOURCE: &str = "pub table media(id: Str) { title: Str, }\n\
pub fn seed() { media.insert({ id: \"london\", title: \"Original\" }); }\n\
pub fn remove() { media.delete(\"london\"); }\n\
pub fn relocate() { media.rekey(\"london\", \"paris\"); }\n";

/// Evaluates `expression` in one REPL session, which reads the merged view,
/// and requires it to count `expected` rows.
fn assert_merged_count(root: &Path, expression: &str, expected: u64) {
    let output = orna_repl(root, expression);
    assert_success(&output, "REPL read of the merged view");
    let text = transcript(&output);
    assert!(
        text.contains(&format!("{expected} : Int")),
        "`{expression}` must count {expected} rows in the merged view: {text}"
    );
}

/// The published `media` table with one row keyed `london` at HEAD.
fn published_london(root: &Path) -> String {
    let media = admitted_relations(&commit_source(root, "Declare media"))["media"].clone();
    assert_success(&orna(root, &["run", "main.seed"]), "orna run seed");
    publish(root, "Publish london");
    assert_eq!(
        query_rows(root, &["query", &media, "--format", "json"]),
        1,
        "the seed row is published"
    );
    media
}

/// ornadb-9rzyn: a run that deletes a published row is accepted, the merged view
/// no longer holds it, and the next publish removes it from the store.
#[test]
fn run_delete_of_a_published_row_leaves_the_merged_view_and_publishes() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, SOURCE);
    let media = published_london(&root);

    assert_success(&orna(&root, &["run", "main.remove"]), "orna run remove");
    assert_merged_count(&root, "main.media | count()", 0);

    publish(&root, "Remove london");
    assert_eq!(query_rows(&root, &["query", &media, "--format", "json"]), 0);
    assert_merged_count(&root, "main.media | count()", 0);
}

/// ornadb-9rzyn: a run that re-keys a published row is accepted, the merged view
/// moves the row to its new key, and the next publish stores the move.
#[test]
fn run_rekey_of_a_published_row_moves_it_in_the_merged_view_and_publishes() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, SOURCE);
    let media = published_london(&root);

    assert_success(&orna(&root, &["run", "main.relocate"]), "orna run relocate");
    assert_merged_count(
        &root,
        "main.media | filter(row => row.id == \"london\") | count()",
        0,
    );
    assert_merged_count(
        &root,
        "main.media | filter(row => row.id == \"paris\") | count()",
        1,
    );

    publish(&root, "Rekey london to paris");
    // The moved row is the table's only committed row, under its new key.
    assert_eq!(query_rows(&root, &["query", &media, "--format", "json"]), 1);
    assert_eq!(
        query_rows(
            &root,
            &["query", &media, "--key", "paris", "--format", "json"]
        ),
        1
    );
    // The old key is refused as not committed, not answered with an empty page.
    let absent = orna(&root, &["query", &media, "--key", "london", "--format", "json"]);
    assert!(
        !absent.status.success() && transcript(&absent).contains("Row is not committed"),
        "the moved key is not committed at HEAD: {}",
        transcript(&absent)
    );
    assert_merged_count(&root, "main.media | count()", 1);
}
