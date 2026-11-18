#![cfg(feature = "system-host-bindings")]

//! `sys.git.repositories` and `sys.git.worktree` evaluated over a real repository the
//! host attached: one `sys.GitRepository` row describing it, one `sys.WorktreeEntry`
//! row per path Git reports as changed or untracked, every reference pinned to the
//! commit the host attached at, and `sys.snapshot.incomplete` (never a fabricated row)
//! when the repository or the pin is unavailable (ORNA-SYS-111, ORNA-SYS-112).

#[path = "support/git_members.rs"]
mod git_members;

use std::path::Path;

use git_members::{
    DATABASE, attached, context, evaluate, git, int, is_absent, pin, reference, repository, some,
    text, value,
};
use orna_value::{
    PathFlavour, database_reference, git_repository_reference, path_value, worktree_entry_reference,
};

fn head_of(root: &Path) -> String {
    git(root, &["rev-parse", "HEAD"])
}

#[test]
fn the_attached_repository_is_one_row_describing_it() {
    let directory = repository();
    let root = directory.path();
    let head = head_of(root);
    let host = attached(root);
    assert_eq!(value("sys.git.repositories | count()", &host), int(1));
    assert_eq!(
        some(value(
            "sys.git.repositories | map(r => r.object_format) | first()",
            &host
        )),
        text("sha1")
    );
    for flag in ["bare", "partial_clone"] {
        assert_eq!(
            some(value(
                &format!("sys.git.repositories | map(r => r.{flag}) | first()"),
                &host
            )),
            value("false", &host),
            "{flag}"
        );
    }
    assert!(
        !is_absent(&some(value(
            "sys.git.repositories | map(r => r.worktree) | first()",
            &host
        ))),
        "a non-bare repository has a working tree"
    );
    let git_dir = git(root, &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(
        some(value(
            "sys.git.repositories | map(r => r.git_dir) | first()",
            &host
        )),
        path_value(PathFlavour::Posix, &git_dir)
            .expect("a host path")
            .raw()
            .clone()
    );
    let database = || database_reference(DATABASE, pin(&head)).expect("a database reference");
    assert_eq!(
        some(value(
            "sys.git.repositories | map(r => r.database) | first()",
            &host
        )),
        reference(database().to_canonical().expect("it encodes"))
    );
    assert_eq!(
        some(value(
            "sys.git.repositories | map(r => r.reference) | first()",
            &host
        )),
        reference(
            git_repository_reference(DATABASE, pin(&head), database().into_row_ref())
                .expect("a repository reference")
                .to_canonical()
                .expect("it encodes")
        )
    );
}

#[test]
fn a_clean_worktree_has_no_entries() {
    let directory = repository();
    let host = attached(directory.path());
    assert_eq!(value("sys.git.worktree | count()", &host), int(0));
    assert_eq!(value("sys.git.repositories | count()", &host), int(1));
}

#[test]
fn the_worktree_lists_each_changed_and_untracked_path_pinned_to_the_commit() {
    let directory = repository();
    let root = directory.path();
    std::fs::write(root.join("tracked.txt"), "one\n").expect("a tracked file");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-q", "-m", "tracked"]);
    std::fs::write(root.join("tracked.txt"), "two\n").expect("a modified file");
    std::fs::write(root.join("untracked.txt"), "new\n").expect("an untracked file");
    let head = head_of(root);
    let host = attached(root);
    assert_eq!(value("sys.git.worktree | count()", &host), int(2));
    assert_eq!(
        value(
            "sys.git.worktree | filter(e => e.untracked) | count()",
            &host
        ),
        int(1)
    );
    assert_eq!(
        value("sys.git.worktree | filter(e => e.ignored) | count()", &host),
        int(0)
    );
    assert!(
        !is_absent(&some(value(
            "sys.git.worktree | filter(e => e.untracked == false) | map(e => e.worktree_status) | first()",
            &host
        ))),
        "a modified path has a worktree status"
    );
    assert!(
        is_absent(&some(value(
            "sys.git.worktree | filter(e => e.untracked == false) | map(e => e.index_status) | first()",
            &host
        ))),
        "an unstaged path has no index status"
    );
    assert_eq!(
        some(value(
            "sys.git.worktree | filter(e => e.untracked) | map(e => e.reference) | first()",
            &host
        )),
        reference(
            worktree_entry_reference(DATABASE, pin(&head), "untracked.txt")
                .expect("a worktree entry reference")
                .to_canonical()
                .expect("it encodes")
        )
    );
}

#[test]
fn no_commit_pin_or_no_repository_is_incomplete() {
    let directory = repository();
    // A clean worktree serves no row, so the pin is read only once a row is there
    // to pin (git_rows::page_ordered); an untracked file makes one.
    std::fs::write(directory.path().join("untracked.txt"), "new\n").expect("an untracked file");
    let unpinned = context(directory.path(), None);
    let plain = tempfile::tempdir().expect("a directory");
    let unrepository = context(plain.path(), Some(&"ab".repeat(20)));
    for source in ["sys.git.repositories", "sys.git.worktree"] {
        for host in [&unpinned, &unrepository] {
            let failure = evaluate(&format!("{source} | count()"), host)
                .expect_err("no row is served without a pin and a repository");
            assert_eq!(failure.code(), "sys.snapshot.incomplete", "{source}");
        }
    }
}
