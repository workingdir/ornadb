//! A pending insert whose key the published HEAD already holds (ORNA-PUB-013).
//! `orna publish` over a HEAD that another worktree moved onto refuses the
//! candidate as a row-key conflict: nothing is published, HEAD stays on the
//! other worktree's commit, and the pending insert is kept, so a retry refuses
//! it again. The refusal names the conflict rather than malformed repository
//! data.

#[path = "support/product.rs"]
mod product;

use std::path::{Path, PathBuf};

use product::*;
use tempfile::TempDir;

const TASKS: &str = "pub table Task(id: Str) {\n    title: Str,\n}\n";
const PENDING: &str = "main.Task.insert({ id: \"write\", title: \"Write the report\" })";
const PUBLISHED: &str = "main.Task.insert({ id: \"write\", title: \"Write the draft\" })";
const MESSAGE: &str = "Publish the pending insert";

/// A second clone of the library stands in for another worktree. Its runtime
/// state lives in its own `.git`, so it never sees the pending insert of this
/// worktree.
fn clone_owner(directory: &TempDir, root: &Path) -> PathBuf {
    let owner = directory.path().join("owner");
    git(
        directory.path(),
        &[
            "clone",
            "--quiet",
            root.to_str().expect("UTF-8 test path"),
            owner.to_str().expect("UTF-8 test path"),
        ],
    );
    git(&owner, &["config", "user.name", "Orna Test"]);
    git(
        &owner,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&owner, &["config", "commit.gpgsign", "false"]);
    owner
}

#[test]
fn pending_insert_on_a_key_the_head_holds_is_refused_as_a_row_key_conflict() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, TASKS);
    let task = admitted_relations(&commit_source(&root, "Declare Task"))["Task"].clone();
    repl_writes(&root, PENDING);

    // The other worktree publishes the same key, and this worktree pulls that
    // publication: HEAD now holds the row the pending insert would duplicate.
    let owner = clone_owner(&directory, &root);
    repl_writes(&owner, PUBLISHED);
    publish(&owner, "Publish the draft");
    git(
        &root,
        &[
            "pull",
            "--ff-only",
            "--quiet",
            owner.to_str().expect("UTF-8 test path"),
            "HEAD",
        ],
    );
    let published = head(&root);

    // Publication builds its candidate on HEAD, where the key already exists.
    // Nothing is published, HEAD stays put and the pending insert is kept, so
    // the retry is refused the same way. The key-level detail is attached to
    // the diagnostic and printed only with --debug (ORNA-UX-004).
    for (attempt, debug) in [
        ("first", false),
        ("retried", false),
        ("first debug", true),
        ("retried debug", true),
    ] {
        let refused = if debug {
            orna(&root, &["--debug", "publish", "--message", MESSAGE])
        } else {
            orna(&root, &["publish", "--message", MESSAGE])
        };
        let message = transcript(&refused);
        assert_ne!(
            refused.status.code(),
            Some(0),
            "{attempt} publish: {message}"
        );
        assert!(
            message.contains(
                "error[ORNA-CMD-WORKFLOW-REFUSED]: publication candidate could not be prepared"
            ),
            "{attempt} publish: {message}"
        );
        assert!(
            message.contains("the pending change is kept")
                && message.contains(
                    "reconciles a pending insert with a row the published HEAD already holds"
                ),
            "{attempt} publish: {message}"
        );
        assert_eq!(
            message.contains("the pending row's key already exists"),
            debug,
            "{attempt} publish: {message}"
        );
        assert!(
            !message.contains("malformed") && !message.contains("ORNA-REPO-CONTEXT-017"),
            "{attempt} publish: {message}"
        );
        assert_eq!(head(&root), published, "{attempt} publish moved HEAD");
    }

    // HEAD holds the other worktree's row alone; the pending insert is not in it.
    let rows = stored_rows(&root, &head(&root), relation_bytes(&task));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].fields.get("title"), Some(&text("Write the draft")));
}
