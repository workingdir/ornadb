#![cfg(feature = "system-host-bindings")]

//! `sys.git.tree_entries` evaluated over a real repository the host attached: one
//! `sys.TreeEntry` row per entry under the root tree of the pinned commit, in path
//! order, every reference pinned to that commit, and `sys.snapshot.incomplete` (never
//! a fabricated row) when the repository or the pin is unavailable (ORNA-SYS-111,
//! ORNA-SYS-112).

#[path = "support/git_members.rs"]
mod git_members;

use std::path::Path;

use git_members::{
    DATABASE, attached, context, evaluate, git, int, oid_bytes, pin, reference, repository, some,
    value,
};
use orna_value::{
    ENUM_TAG, GitHash, GitObjectId, OvbRaw, UUID_TAG, enum_type_id, enum_variant_id,
    git_object_reference, tree_entry_reference,
};

/// The reference of the entry `path` of the tree `tree`, over the commit `head`.
fn tree_ref(head: &str, tree: &str, path: &str) -> OvbRaw {
    let tree = GitObjectId::new(GitHash::Sha1, oid_bytes(tree)).expect("a tree id");
    reference(
        tree_entry_reference(DATABASE, pin(head), tree, path)
            .expect("a tree entry reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

/// The reference of the object `object`, over the commit `head`.
fn object_ref(head: &str, object: &str) -> OvbRaw {
    let oid = GitObjectId::new(GitHash::Sha1, oid_bytes(object)).expect("an object id");
    reference(
        git_object_reference(DATABASE, pin(head), oid)
            .expect("an object reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

/// The canonical value of the `sys.GitObjectKind` variant `label`: the enum tag over the
/// derived identities of its type and of the variant, with no payload.
fn kind(label: &str) -> OvbRaw {
    let uuid = |id: [u8; 16]| OvbRaw::Tag(UUID_TAG, Box::new(OvbRaw::Bytes(id.to_vec())));
    OvbRaw::Tag(
        ENUM_TAG,
        Box::new(OvbRaw::Array(vec![
            uuid(enum_type_id("sys.GitObjectKind")),
            uuid(enum_variant_id("sys.GitObjectKind", label)),
            OvbRaw::Null,
        ])),
    )
}

/// Stages every change under `root` and commits it with `message`.
fn commit_all(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", message]);
}

/// The head commit and the root tree of the repository at `root`.
fn head_and_tree(root: &Path) -> (String, String) {
    (
        git(root, &["rev-parse", "HEAD"]),
        git(root, &["rev-parse", "HEAD^{tree}"]),
    )
}

// Specified: 34-system-reference.md `sys.TreeEntry`; ORNA-SYS-112. The entries of the
// root tree are rows in path order, a directory included, and each row is found.
#[test]
fn entries_of_the_root_tree_are_rows_in_path_order() {
    let directory = repository();
    let root = directory.path();
    std::fs::create_dir(root.join("dir")).expect("a directory");
    std::fs::write(root.join("a.txt"), "a\n").expect("a file");
    std::fs::write(root.join("dir/b.txt"), "b\n").expect("a file");
    std::fs::write(root.join("z.txt"), "z\n").expect("a file");
    commit_all(root, "tree");
    let (head, tree) = head_and_tree(root);
    let host = attached(root);

    assert_eq!(value("sys.git.tree_entries | count()", &host), int(4));
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.reference) | first()",
            &host
        )),
        tree_ref(&head, &tree, "a.txt")
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.reference) | last()",
            &host
        )),
        tree_ref(&head, &tree, "z.txt")
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | filter(e => e.mode == 16384) | map(e => e.reference) | first()",
            &host
        )),
        tree_ref(&head, &tree, "dir")
    );
    assert_eq!(
        value(
            "sys.git.tree_entries | filter(e => e.mode == 33188) | count()",
            &host
        ),
        int(3)
    );
}

// Specified: 34-system-reference.md `sys.TreeEntry`; ORNA-SYS-112. A row names the root
// tree it is an entry of, the object it names, that object's kind and the entry's mode.
#[test]
fn each_row_names_its_tree_its_object_its_kind_and_its_mode() {
    let directory = repository();
    let root = directory.path();
    std::fs::create_dir(root.join("dir")).expect("a directory");
    std::fs::write(root.join("a.txt"), "a\n").expect("a file");
    std::fs::write(root.join("dir/b.txt"), "b\n").expect("a file");
    commit_all(root, "tree");
    let (head, tree) = head_and_tree(root);
    let blob = git(root, &["rev-parse", "HEAD:a.txt"]);
    let directory_tree = git(root, &["rev-parse", "HEAD:dir"]);
    let host = attached(root);

    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.tree) | first()",
            &host
        )),
        object_ref(&head, &tree)
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.object) | first()",
            &host
        )),
        object_ref(&head, &blob)
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.mode) | first()",
            &host
        )),
        int(33188)
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.kind) | first()",
            &host
        )),
        kind("blob")
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | filter(e => e.mode == 16384) | map(e => e.object) | first()",
            &host
        )),
        object_ref(&head, &directory_tree)
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | filter(e => e.mode == 16384) | map(e => e.kind) | first()",
            &host
        )),
        kind("tree")
    );
}

// Specified: 34-system-reference.md `sys.TreeEntry`; the mode is Git's, so an executable
// regular file is `100755` (33261), not `100644`.
#[test]
fn an_executable_file_has_mode_33261() {
    let directory = repository();
    let root = directory.path();
    std::fs::write(root.join("run.sh"), "#!/bin/sh\n").expect("a file");
    git(root, &["add", "run.sh"]);
    git(root, &["update-index", "--chmod=+x", "run.sh"]);
    git(root, &["commit", "-q", "-m", "script"]);
    let (head, tree) = head_and_tree(root);
    let host = attached(root);

    assert_eq!(
        some(value(
            "sys.git.tree_entries | filter(e => e.mode == 33261) | map(e => e.reference) | first()",
            &host
        )),
        tree_ref(&head, &tree, "run.sh")
    );
    assert_eq!(
        value(
            "sys.git.tree_entries | filter(e => e.mode == 33188) | count()",
            &host
        ),
        int(0)
    );
}

// Specified: 34-system-reference.md `sys.TreeEntry`; the empty tree has no entries.
#[test]
fn a_commit_with_no_files_lists_no_entries() {
    let directory = repository();
    let host = attached(directory.path());
    assert_eq!(value("sys.git.tree_entries | count()", &host), int(0));
}

// Specified: ORNA-SYS-111; no row is served over a pin that is not there.
#[test]
fn a_host_with_no_commit_pin_serves_no_tree_entry() {
    let directory = repository();
    let host = context(directory.path(), None);
    let failure = evaluate("sys.git.tree_entries | count()", &host)
        .expect_err("a tree entry cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// Specified: ORNA-SYS-111; an unborn `HEAD` names no commit, so it has no tree to list
// and needs no pin.
#[test]
fn a_repository_with_no_commit_needs_no_pin() {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.tree_entries | count()", &host), int(0));
}

// Specified: ORNA-SYS-111; no repository is attached at the root, so nothing is served.
#[test]
fn a_root_that_is_not_a_repository_fails_closed() {
    let directory = tempfile::tempdir().expect("plain directory");
    let host = context(directory.path(), Some(&"ab".repeat(20)));
    let failure = evaluate("sys.git.tree_entries | count()", &host)
        .expect_err("no repository is attached at the root");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// A path Git lists as bytes that are not UTF-8 is not a `sys.TreeEntry` path: the read
// fails closed rather than serving a row under a guessed name.
#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_fails_closed() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let directory = repository();
    let root = directory.path();
    std::fs::write(root.join(OsStr::from_bytes(b"bad\xff.txt")), "x\n").expect("a file");
    commit_all(root, "non-UTF-8 name");
    let host = attached(root);
    let failure = evaluate("sys.git.tree_entries | count()", &host)
        .expect_err("a path that is not UTF-8 has no row");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// The pages hold 256 rows: a tree of 600 files is read as 600 rows, and the first and
// last of them are each served once.
#[test]
fn a_large_tree_is_read_across_pages_without_repeating_or_skipping() {
    let directory = repository();
    let root = directory.path();
    for index in 0..600 {
        std::fs::write(root.join(format!("f{index:04}.txt")), format!("{index}\n"))
            .expect("a file");
    }
    commit_all(root, "many files");
    let (head, tree) = head_and_tree(root);
    let host = attached(root);

    assert_eq!(value("sys.git.tree_entries | count()", &host), int(600));
    assert_eq!(
        value(
            "sys.git.tree_entries | filter(e => e.mode == 33188) | count()",
            &host
        ),
        int(600)
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.reference) | first()",
            &host
        )),
        tree_ref(&head, &tree, "f0000.txt")
    );
    assert_eq!(
        some(value(
            "sys.git.tree_entries | map(e => e.reference) | last()",
            &host
        )),
        tree_ref(&head, &tree, "f0599.txt")
    );
}
