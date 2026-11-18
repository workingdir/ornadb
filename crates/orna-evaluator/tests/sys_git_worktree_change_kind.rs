#![cfg(feature = "system-host-bindings")]

//! The `index_status` and `worktree_status` of every `sys.git.worktree` row are the
//! `sys.ChangeKind` constants: the value `sys.ChangeKind.<variant>` evaluates to, which
//! is the canonical enum form (tag 60008 over `[type_id, variant_id, null]`, both ids
//! tag-37 ObjectIds derived from the qualified enum name, 1.1.0 value encoding `Enum`).
//! Each status decoded from a real worktree is compared with that constant, not with a
//! text spelling of its variant name.

#[path = "support/git_members.rs"]
mod git_members;

use git_members::{attached, git, int, repository, some, value};
use orna_evaluator::SysHostContext;
use orna_value::{ENUM_TAG, OvbRaw, UUID_TAG, enum_type_id, enum_variant_id};

/// The canonical enum form of the `sys.ChangeKind` constant `variant`.
fn change_kind(variant: &str) -> OvbRaw {
    OvbRaw::Tag(
        ENUM_TAG,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Tag(
                UUID_TAG,
                Box::new(OvbRaw::Bytes(enum_type_id("sys.ChangeKind").to_vec())),
            ),
            OvbRaw::Tag(
                UUID_TAG,
                Box::new(OvbRaw::Bytes(
                    enum_variant_id("sys.ChangeKind", variant).to_vec(),
                )),
            ),
            OvbRaw::Null,
        ])),
    )
}

/// The `sys.git.worktree` status `column` with an absent option projected to
/// `sys.ChangeKind.rewrite`, which no fixture here produces. `??` unwraps a present
/// option, so the projection compares as a plain `sys.ChangeKind` value.
fn status(column: &str) -> String {
    format!("(e.{column} ?? sys.ChangeKind.rewrite)")
}

/// The number of `sys.git.worktree` rows whose `column` is the constant `variant`.
fn rows_with(column: &str, variant: &str, host: &SysHostContext) -> OvbRaw {
    value(
        &format!(
            "sys.git.worktree | filter(e => {} == sys.ChangeKind.{variant}) | count()",
            status(column)
        ),
        host,
    )
}

/// The `column` status of the first row whose `column` is the constant `variant`.
fn first_status(column: &str, variant: &str, host: &SysHostContext) -> OvbRaw {
    some(value(
        &format!(
            "sys.git.worktree | filter(e => {} == sys.ChangeKind.{variant}) | map(e => {}) | first()",
            status(column),
            status(column)
        ),
        host,
    ))
}

#[test]
fn worktree_change_values_are_the_sys_change_kind_constants() {
    let directory = repository();
    let root = directory.path();
    for name in ["tracked.txt", "gone.txt", "old.txt"] {
        std::fs::write(root.join(name), "base\n").expect("a committed file");
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "files"]);
    // A modified path, a staged addition, a deleted path, a staged rename and an
    // untracked path: one row per changed path, covering the variants this fixture can
    // produce on either side.
    std::fs::write(root.join("tracked.txt"), "changed\n").expect("a modified file");
    std::fs::write(root.join("staged.txt"), "new\n").expect("a staged file");
    git(root, &["add", "staged.txt"]);
    std::fs::remove_file(root.join("gone.txt")).expect("a deleted file");
    git(root, &["mv", "old.txt", "moved.txt"]);
    std::fs::write(root.join("untracked.txt"), "new\n").expect("an untracked file");
    let host = attached(root);

    for variant in ["add", "modify", "delete", "rename"] {
        assert_eq!(
            value(&format!("sys.ChangeKind.{variant}"), &host),
            change_kind(variant),
            "sys.ChangeKind.{variant} is the canonical enum form"
        );
    }

    // Index side: `staged.txt` is added and `old.txt` renamed to `moved.txt`.
    assert_eq!(rows_with("index_status", "add", &host), int(1));
    assert_eq!(rows_with("index_status", "rename", &host), int(1));
    assert_eq!(rows_with("index_status", "modify", &host), int(0));
    assert_eq!(rows_with("index_status", "delete", &host), int(0));
    assert_eq!(
        first_status("index_status", "add", &host),
        change_kind("add"),
        "the decoded index add is the canonical sys.ChangeKind.add"
    );
    assert_eq!(
        first_status("index_status", "rename", &host),
        change_kind("rename"),
        "the decoded index rename is the canonical sys.ChangeKind.rename"
    );

    // Worktree side: `tracked.txt` is modified, `gone.txt` deleted, and `untracked.txt`
    // reported as added; `staged.txt` and `moved.txt` have no worktree change.
    assert_eq!(rows_with("worktree_status", "modify", &host), int(1));
    assert_eq!(rows_with("worktree_status", "delete", &host), int(1));
    assert_eq!(rows_with("worktree_status", "add", &host), int(1));
    assert_eq!(rows_with("worktree_status", "rename", &host), int(0));
    assert_eq!(
        first_status("worktree_status", "modify", &host),
        change_kind("modify"),
        "the decoded worktree modify is the canonical sys.ChangeKind.modify"
    );
    assert_eq!(
        first_status("worktree_status", "delete", &host),
        change_kind("delete"),
        "the decoded worktree delete is the canonical sys.ChangeKind.delete"
    );
    assert_eq!(
        first_status("worktree_status", "add", &host),
        change_kind("add"),
        "the decoded worktree add is the canonical sys.ChangeKind.add"
    );
}
