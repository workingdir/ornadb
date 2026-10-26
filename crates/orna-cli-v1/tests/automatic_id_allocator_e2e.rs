//! Automatic table IDs of a database the product itself created are never
//! reused (ORNA-AUTOID-001, ORNA-AUTOID-002, ORNA-AUTOID-004, ORNA-META-004,
//! ORNA-BRANCH-002; scenarios KEY-001 and KEY-002): the allocator lives in the
//! hidden ref `refs/orna/ids/<table ObjectId>`, outside every branch tree, so
//! deleting rows, resetting the branch, checking out an older commit and
//! switching branches all leave it where it is. Every commit records the
//! watermark it knew in its store for history (ORNA-AUTOID-005).

#[path = "support/product.rs"]
mod product;

use product::*;
use tempfile::TempDir;

const NOTES: &str = "pub table Note {\n    text: Str,\n}\n";

/// The automatic IDs one REPL session allocates, read from the rows the
/// REPL prints for its inserts.
fn insert_notes(root: &std::path::Path, texts: &[&str]) -> Vec<u64> {
    let input = texts
        .iter()
        .map(|text| format!("main.Note.insert({{ text: \"{text}\" }})"))
        .collect::<Vec<_>>()
        .join("\n");
    let output = repl_writes(root, &input);
    let ids = output
        .match_indices("\"id\": ")
        .map(|(at, prefix)| {
            output[at + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u64>()
                .expect("an automatic id is an integer")
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), texts.len(), "{output}");
    ids
}

#[test]
fn automatic_ids_survive_delete_reset_checkout_and_branch_switch() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, NOTES);
    let note = admitted_relations(&commit_source(&root, "Declare Note"))["Note"].clone();
    let declared = head(&root);
    let allocator = format!("refs/orna/ids/{note}");
    let recorded = |commit: &str| {
        committed_watermarks(&root, commit).expect("the commit records an allocator summary")[&note]
            .clone()
    };
    // ORNA-AUTOID-005: the declaring commit knew no reservation yet.
    assert_eq!(recorded(&declared), "0");

    // ORNA-AUTOID-001: a key-less table gets increasing Int keys.
    assert_eq!(insert_notes(&root, &["a", "b", "c"]), vec![1, 2, 3]);
    // The watermark is durable before the rows are published.
    assert_eq!(git(&root, &["cat-file", "-p", &allocator]), "3");
    publish(&root, "Three notes");
    let three = head(&root);
    assert_eq!(recorded(&three), "3");

    // KEY-001: deleting the highest row does not free its ID.
    repl_writes(&root, "main.Note.delete(3)");
    publish(&root, "Delete note 3");
    assert_eq!(query_rows(&root, &["query", &note, "--format", "json"]), 2);
    assert_eq!(insert_notes(&root, &["d"]), vec![4]);
    publish(&root, "Note 4");

    // KEY-002: a branch reset to a commit whose rows end at 3 continues
    // above the allocator, not above the rows.
    git(&root, &["reset", "--quiet", "--hard", &three]);
    assert_eq!(insert_notes(&root, &["e"]), vec![5]);
    publish(&root, "Note 5 after reset");
    assert_eq!(recorded(&head(&root)), "5");

    // Checking out the commit with no rows on a new branch does not rewind
    // it either (ORNA-META-004, ORNA-BRANCH-002).
    git(&root, &["switch", "--quiet", "-c", "elsewhere", &declared]);
    assert_eq!(insert_notes(&root, &["f"]), vec![6]);
    publish(&root, "Note 6 on another branch");
    assert_eq!(query_rows(&root, &["query", &note, "--format", "json"]), 1);
    assert_eq!(recorded(&head(&root)), "6");

    // The allocator ref is not part of any branch tree, and the watermarks
    // older commits recorded stay what they were.
    assert_eq!(git(&root, &["cat-file", "-p", &allocator]), "6");
    for branch in ["elsewhere", "HEAD"] {
        let tree = git(&root, &["ls-tree", "-r", "--name-only", branch]);
        assert!(!tree.contains("refs/orna"), "{tree}");
    }
    assert_eq!(recorded(&three), "3");
    assert_eq!(recorded(&declared), "0");
}

#[test]
fn ids_allocated_by_a_failed_activation_are_never_reused() {
    // ORNA-AUTOID-002 forbids reuse; ORNA-AUTOID-003 makes the gap a failed
    // allocation leaves expected. The `if` block inserts (allocating ID 3)
    // and then fails, so the REPL reports an evaluation error and commits
    // nothing.
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, NOTES);
    let note = admitted_relations(&commit_source(&root, "Declare Note"))["Note"].clone();
    assert_eq!(insert_notes(&root, &["a", "b"]), vec![1, 2]);

    let failed = orna_repl(
        &root,
        "if true { main.Note.insert({ text: \"lost\" }); fail(error(code: \"test.stop\", message: \"stop\")); }",
    );
    assert_success(&failed, "REPL session with a failing activation");
    assert!(
        transcript(&failed).contains("ORNA-EVAL-ERROR"),
        "{}",
        transcript(&failed)
    );

    // The next allocation continues above the failed activation's ID 3.
    assert_eq!(insert_notes(&root, &["c"]), vec![4]);
    publish(&root, "Notes after a failed activation");
    assert_eq!(query_rows(&root, &["query", &note, "--format", "json"]), 3);
}

#[test]
fn renaming_an_automatic_key_table_commits_its_allocator_summary() {
    // ORNA-AUTOID-004 and ORNA-EVOL-001: the allocator summary is a system
    // relation, not a table, so a rename that restages the store inventory
    // must keep it. `orna mv` of a key-less table with allocated IDs must
    // still commit, and the table keeps its ObjectId and watermark.
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, NOTES);
    let note = admitted_relations(&commit_source(&root, "Declare Note"))["Note"].clone();
    assert_eq!(insert_notes(&root, &["a", "b"]), vec![1, 2]);
    publish(&root, "Two notes");

    let renamed = orna(&root, &["mv", "Note", "Memo"]);
    assert_success(&renamed, "orna mv");
    commit_source(&root, "Rename Note to Memo");

    assert_eq!(
        committed_watermarks(&root, &head(&root)).expect("the commit records an allocator summary")[&note],
        "2"
    );
}
