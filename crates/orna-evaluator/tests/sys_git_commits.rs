#![cfg(feature = "system-host-bindings")]

//! `sys.git.commits` evaluated over a real repository the host attached at: one
//! `sys.Commit` row per commit reachable from the pinned commit, newest first, each with
//! its `parents` relation served as the parents' own rows, and `sys.snapshot.incomplete`
//! (never a fabricated row) when the repository, the pin, or a shallow boundary's
//! parents are unavailable (ORNA-SYS-111, ORNA-SYS-112).

#[path = "support/git_members.rs"]
mod git_members;

use std::path::Path;

use orna_evaluator::{Limits, ReplSession};

use git_members::{
    DATABASE, attached, context, evaluate, git, git_with_environment, git_with_input, int, pin,
    reference, some, text, value,
};
use orna_value::{GitObjectId, OvbRaw, commit_reference, git_object_reference};

/// The e-mail address of every test author; the committer is the fixed test identity.
const ADDRESS: &str = "orna-test@example.invalid";

/// The author and committer dates of every commit a test makes.
const AUTHORED: &str = "2001-02-03T04:05:06Z";
const COMMITTED: &str = "2002-03-04T05:06:07Z";

/// A repository on `main` with no commit.
fn empty_repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    directory
}

/// A commit of the empty tree on `parents`, by `author` (a name and an e-mail address),
/// with `message` exactly as given. Returns its id.
fn commit(root: &Path, parents: &[&str], author: (&str, &str), message: &str) -> String {
    let tree = git_with_input(root, &["mktree"], "");
    let mut arguments = vec!["commit-tree", tree.as_str()];
    arguments.extend(parents.iter().flat_map(|parent| ["-p", *parent]));
    arguments.extend(["-F", "-"]);
    let (name, email) = author;
    git_with_environment(
        root,
        &[
            ("GIT_AUTHOR_NAME", name),
            ("GIT_AUTHOR_EMAIL", email),
            ("GIT_AUTHOR_DATE", AUTHORED),
            ("GIT_COMMITTER_DATE", COMMITTED),
        ],
        &arguments,
        message,
    )
}

fn object_id(hex: &str) -> GitObjectId {
    GitObjectId::from_hex(hex).expect("an object id")
}

fn encoded(reference_value: orna_value::SysRef) -> OvbRaw {
    reference(reference_value.to_canonical().expect("it encodes"))
}

/// The `sys.Commit` reference to `commit` as a row of the host pinned at `head`.
fn commit_ref(head: &str, commit: &str) -> OvbRaw {
    encoded(commit_reference(DATABASE, pin(head), object_id(commit)).expect("a commit reference"))
}

/// The reference to the tree `object` as a row of the host pinned at `head`.
fn object_ref(head: &str, object: &str) -> OvbRaw {
    encoded(
        git_object_reference(DATABASE, pin(head), object_id(object)).expect("an object reference"),
    )
}

/// The `sys.GitOid` value of the object id `hex`.
fn oid(hex: &str) -> OvbRaw {
    object_id(hex).to_opaque_raw()
}

/// The `sys.Instant` at `unix_seconds` whole seconds.
fn instant(unix_seconds: i64) -> OvbRaw {
    OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![int(unix_seconds), int(0)])),
    )
}

// Specified: 34-system-reference.md `sys.Commit`; ORNA-SYS-111. The rows of a linear
// history are its commits, the pinned commit first and the root last, and a commit's
// `oid`, `reference` and `parents` name its own row and its parents' rows.
#[test]
fn a_linear_history_lists_its_commits_newest_first() {
    let directory = empty_repository();
    let root = directory.path();
    let base = commit(root, &[], ("base", ADDRESS), "base\n");
    let second = commit(root, &[&base], ("second", ADDRESS), "second\n");
    let third = commit(root, &[&second], ("third", ADDRESS), "third\n");
    git(root, &["update-ref", "refs/heads/main", &third]);
    let host = attached(root);

    assert_eq!(value("sys.git.commits | count()", &host), int(3));
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.author.name) | first()",
            &host
        )),
        text("third")
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.author.name) | last()",
            &host
        )),
        text("base")
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.message) | first()",
            &host
        )),
        text("third\n")
    );
    assert_eq!(
        some(value("sys.git.commits | map(c => c.oid) | first()", &host)),
        oid(&third)
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.reference) | first()",
            &host
        )),
        commit_ref(&third, &third)
    );
    assert_eq!(
        some(value(
            "sys.git.commits | filter(c => c.author.name == \"second\") | flat_map(c => c.parents) | map(p => p.reference) | first()",
            &host
        )),
        commit_ref(&third, &base)
    );
}

// Specified: 34-system-reference.md `sys.Commit`, `parents`; ORNA-SYS-112. A merge commit
// serves both of its parents as rows, and a root commit serves none.
#[test]
fn a_merge_commit_serves_both_parents_and_a_root_serves_none() {
    let directory = empty_repository();
    let root = directory.path();
    let base = commit(root, &[], ("base", ADDRESS), "base\n");
    let left = commit(root, &[&base], ("left", ADDRESS), "left\n");
    let right = commit(root, &[&base], ("right", ADDRESS), "right\n");
    let merge = commit(root, &[&left, &right], ("merge", ADDRESS), "merge\n");
    git(root, &["update-ref", "refs/heads/main", &merge]);
    let host = attached(root);

    assert_eq!(value("sys.git.commits | count()", &host), int(4));
    assert_eq!(
        value(
            "sys.git.commits | filter(c => c.author.name == \"merge\") | flat_map(c => c.parents) | count()",
            &host
        ),
        int(2)
    );
    assert_eq!(
        value(
            "sys.git.commits | filter(c => c.author.name == \"base\") | flat_map(c => c.parents) | count()",
            &host
        ),
        int(0)
    );
    assert_eq!(
        value(
            "sys.git.commits | flat_map(c => c.parents) | count()",
            &host
        ),
        int(4)
    );
    for parent in ["left", "right"] {
        assert_eq!(
            value(
                &format!(
                    "sys.git.commits | flat_map(c => c.parents) | filter(p => p.author.name == \"{parent}\") | count()"
                ),
                &host
            ),
            int(1),
            "{parent}"
        );
    }
}

// Specified: 34-system-reference.md `sys.Commit`, `parents`; ORNA-SYS-112. A shallow
// boundary's parents are not in the repository: its own row is served, reading them fails.
#[test]
fn a_shallow_boundary_serves_its_row_but_not_its_parents() {
    let source = empty_repository();
    let base = commit(source.path(), &[], ("base", ADDRESS), "base\n");
    let tip = commit(source.path(), &[&base], ("tip", ADDRESS), "tip\n");
    git(source.path(), &["update-ref", "refs/heads/main", &tip]);
    let clones = tempfile::tempdir().expect("clone directory");
    let url = format!("file://{}", source.path().display());
    git(
        clones.path(),
        &["clone", "--quiet", "--depth", "1", &url, "clone"],
    );
    let host = attached(&clones.path().join("clone"));

    assert_eq!(value("sys.git.commits | count()", &host), int(1));
    let failure = evaluate(
        "sys.git.commits | flat_map(c => c.parents) | count()",
        &host,
    )
    .expect_err("the parents of a shallow boundary are not in the repository");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// Specified: ORNA-SYS-111; no commit is served over a pin that is not there.
#[test]
fn a_host_with_no_commit_pin_serves_no_commit() {
    let directory = empty_repository();
    let root = directory.path();
    let base = commit(root, &[], ("base", ADDRESS), "base\n");
    git(root, &["update-ref", "refs/heads/main", &base]);
    let host = context(root, None);
    let failure = evaluate("sys.git.commits | count()", &host)
        .expect_err("a commit cannot be served without a pin");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// An unborn `HEAD` names no commit: it serves none, and needs no pin to say so.
#[test]
fn an_unborn_head_serves_no_commit_without_a_pin() {
    let directory = empty_repository();
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.commits | count()", &host), int(0));
}

#[test]
fn a_root_that_is_not_a_repository_fails_closed() {
    let directory = tempfile::tempdir().expect("plain directory");
    let host = context(directory.path(), Some(&"ab".repeat(20)));
    let failure = evaluate("sys.git.commits | count()", &host)
        .expect_err("no repository is attached at the root");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// Specified: ORNA-SYS-111, ORNA-SYS-112. A history longer than a page is read across the
// page breaks: every commit is served exactly once, in walk order.
#[test]
fn a_long_history_is_read_across_pages_without_repeating_or_skipping() {
    let directory = empty_repository();
    let root = directory.path();
    let mut parent = commit(root, &[], ("base", ADDRESS), "base\n");
    for index in 0..600 {
        let name = format!("c{index:04}");
        parent = commit(
            root,
            &[&parent],
            (name.as_str(), ADDRESS),
            &format!("{name}\n"),
        );
    }
    git(root, &["update-ref", "refs/heads/main", &parent]);
    let host = attached(root);

    assert_eq!(value("sys.git.commits | count()", &host), int(601));
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.author.name) | first()",
            &host
        )),
        text("c0599")
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.author.name) | last()",
            &host
        )),
        text("base")
    );
    for index in [0, 255, 256, 257, 511, 512, 513, 599] {
        let name = format!("c{index:04}");
        assert_eq!(
            value(
                &format!("sys.git.commits | filter(c => c.author.name == \"{name}\") | count()"),
                &host
            ),
            int(1),
            "{name}"
        );
    }
}

// Specified: 34-system-reference.md `sys.Commit`; ORNA-SYS-112. A row carries its author
// and committer identities, its message with the trailing newline, its tree, and its times.
#[test]
fn a_commit_row_carries_its_identities_message_tree_and_times() {
    let directory = empty_repository();
    let root = directory.path();
    let id = commit(
        root,
        &[],
        ("Ada Lovelace", "ada@example.invalid"),
        "identity and time\n",
    );
    git(root, &["update-ref", "refs/heads/main", &id]);
    let host = attached(root);
    let tree = git(root, &["log", "-1", "--format=%T"]);
    let authored: i64 = git(root, &["log", "-1", "--format=%at"])
        .parse()
        .expect("an author time");
    let committed: i64 = git(root, &["log", "-1", "--format=%ct"])
        .parse()
        .expect("a committer time");

    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.author.name) | first()",
            &host
        )),
        text("Ada Lovelace")
    );
    assert_eq!(
        some(some(value(
            "sys.git.commits | map(c => c.author.email) | first()",
            &host
        ))),
        text("ada@example.invalid")
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.committer.name) | first()",
            &host
        )),
        text("orna-test")
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.authored_at) | first()",
            &host
        )),
        instant(authored)
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.committed_at) | first()",
            &host
        )),
        instant(committed)
    );
    assert_eq!(
        some(value(
            "sys.git.commits | map(c => c.message) | first()",
            &host
        )),
        text("identity and time\n")
    );
    assert_eq!(
        some(value("sys.git.commits | map(c => c.tree) | first()", &host)),
        object_ref(&id, &tree)
    );
}

// Specified: 34-system-reference.md `sys.Commit`, `parents`; the REPL result boundary. A
// `parents` relation nested in a result is summarised as `{ type: "Relation", rows: N }` when
// its count is known: a merge commit's two parents are, so its summary carries `rows: 2`.
#[test]
fn a_nested_parents_relation_is_summarised_with_its_known_row_count() {
    let directory = empty_repository();
    let root = directory.path();
    let base = commit(root, &[], ("base", ADDRESS), "base\n");
    let left = commit(root, &[&base], ("left", ADDRESS), "left\n");
    let right = commit(root, &[&base], ("right", ADDRESS), "right\n");
    let merge = commit(root, &[&left, &right], ("merge", ADDRESS), "merge\n");
    git(root, &["update-ref", "refs/heads/main", &merge]);
    let host = attached(root);

    let mut session = ReplSession::new(Limits::default()).with_sys_host_context(host);
    let result = session
        .submit(
            "sys.git.commits | filter(c => c.author.name == \"merge\") | map(c => c.parents) | first()",
        )
        .expect("a result holding the merge's parents relation")
        .expect("a value");

    let summary = some(result.raw().clone());
    let OvbRaw::Map(entries) = &summary else {
        panic!("a nested relation is summarised as a record: {summary:?}");
    };
    assert!(
        entries.contains(&(
            OvbRaw::Text("type".to_owned()),
            OvbRaw::Text("Relation".to_owned())
        )),
        "the summary names its kind: {entries:?}"
    );
    assert!(
        entries.contains(&(OvbRaw::Text("rows".to_owned()), int(2))),
        "a merge's two parents are a known count: {entries:?}"
    );
}
