#![cfg(feature = "system-host-bindings")]

//! `sys.git.refs` and `sys.git.stashes` evaluated over a real repository the host
//! attached: one `sys.Ref` row per ref under `refs/` and one `sys.Stash` row per
//! stash entry, every reference pinned to the commit the host attached at, and
//! `sys.snapshot.incomplete` (never a fabricated row) when the repository or the
//! pin is unavailable (ORNA-SYS-050, ORNA-SYS-111, ORNA-SYS-112).

#[path = "support/git_members.rs"]
mod git_members;

use std::path::Path;
use std::process::Command;

use git_members::{
    DATABASE, attached, context, evaluate, git, git_with_input, int, is_absent, oid_bytes, pin,
    reference, repository, some, text, value,
};
use orna_value::{
    GitHash, GitObjectId, OvbRaw, commit_reference, git_object_reference, git_ref_reference,
    remote_reference, stash_reference,
};

fn encoded(reference_value: orna_value::SysRef) -> OvbRaw {
    reference(reference_value.to_canonical().expect("it encodes"))
}

fn object_id(hex: &str) -> GitObjectId {
    GitObjectId::new(GitHash::Sha1, oid_bytes(hex)).expect("an object id")
}

fn ref_ref(head: &str, name: &str) -> OvbRaw {
    encoded(git_ref_reference(DATABASE, pin(head), name).expect("a ref reference"))
}

fn object_ref(head: &str, object: &str) -> OvbRaw {
    encoded(
        git_object_reference(DATABASE, pin(head), object_id(object)).expect("an object reference"),
    )
}

fn commit_ref(head: &str, commit: &str) -> OvbRaw {
    encoded(commit_reference(DATABASE, pin(head), object_id(commit)).expect("a commit reference"))
}

fn remote_ref(head: &str, name: &str) -> OvbRaw {
    encoded(remote_reference(DATABASE, pin(head), name).expect("a remote reference"))
}

fn stash_ref(head: &str, index: u64) -> OvbRaw {
    encoded(stash_reference(DATABASE, pin(head), index).expect("a stash reference"))
}

/// The expression for field `name` of the row of the ref `named`.
fn ref_field(named: &str, name: &str) -> String {
    format!("sys.git.refs | filter(r => r.name == \"{named}\") | map(r => r.{name}) | first()")
}

/// The expression for field `name` of the row of the stash entry `index`.
fn stash_field(index: u64, name: &str) -> String {
    format!("sys.git.stashes | filter(s => s.index == {index}) | map(s => s.{name}) | first()")
}

fn instant(unix_seconds: i64) -> OvbRaw {
    OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![int(unix_seconds), int(0)])),
    )
}

/// A repository on `main` whose `HEAD` commit tracks `f.txt`, so there is
/// something to stash.
fn tracked_repository() -> tempfile::TempDir {
    let directory = repository();
    std::fs::write(directory.path().join("f.txt"), "base\n").expect("a tracked file");
    git(directory.path(), &["add", "f.txt"]);
    git(directory.path(), &["commit", "-q", "-m", "track f"]);
    directory
}

/// Stashes a change to `f.txt` as `message`, the committer date of the stash
/// commit being `date`.
fn stash_at(root: &Path, round: usize, message: &str, date: &str) {
    std::fs::write(root.join("f.txt"), format!("round {round}\n")).expect("a change to stash");
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_COMMITTER_DATE", date)
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "stash",
            "push",
            "-m",
            message,
        ])
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git stash push: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Specified: 34-system-reference.md `sys.Ref`; ORNA-SYS-112. One row per ref under
// `refs/`, named in full, in name order.
#[test]
fn refs_are_rows_by_full_name_in_name_order() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "topic"]);
    git(root, &["tag", "light"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git(root, &["update-ref", "refs/notes/commits", &commit]);
    let host = attached(root);

    assert_eq!(value("sys.git.refs | count()", &host), int(5));
    assert_eq!(
        some(value("sys.git.refs | map(r => r.name) | first()", &host)),
        text("refs/heads/main")
    );
    assert_eq!(
        some(value("sys.git.refs | map(r => r.name) | last()", &host)),
        text("refs/tags/light")
    );
    for name in [
        "refs/heads/main",
        "refs/heads/topic",
        "refs/notes/commits",
        "refs/remotes/origin/main",
        "refs/tags/light",
    ] {
        assert_eq!(
            some(value(&ref_field(name, "name"), &host)),
            text(name),
            "the full name"
        );
        assert_eq!(
            some(value(&ref_field(name, "reference"), &host)),
            ref_ref(&commit, name),
            "{name}"
        );
        assert_eq!(
            some(value(&ref_field(name, "target"), &host)),
            object_ref(&commit, &commit),
            "{name}"
        );
        assert!(
            is_absent(&some(value(&ref_field(name, "symbolic_target"), &host))),
            "{name} is not symbolic"
        );
    }
}

#[test]
fn an_annotated_tag_ref_targets_its_tag_object_and_not_the_commit() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    let tag_object = git(root, &["rev-parse", "refs/tags/v1"]);
    let host = attached(root);

    assert_eq!(
        some(value(&ref_field("refs/tags/v1", "target"), &host)),
        object_ref(&commit, &tag_object)
    );
}

#[test]
fn a_symbolic_ref_names_its_target_ref_and_resolves_to_the_object() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let host = attached(root);

    assert_eq!(
        some(some(value(
            &ref_field("refs/remotes/origin/HEAD", "symbolic_target"),
            &host
        ))),
        text("refs/remotes/origin/main")
    );
    assert_eq!(
        some(value(
            &ref_field("refs/remotes/origin/HEAD", "target"),
            &host
        )),
        object_ref(&commit, &commit)
    );
}

// Specified: ORNA-SYS-050. Hidden Orna refs are visible through `sys.Ref.hidden`
// and stay ordinary refs.
#[test]
fn the_orna_namespace_is_hidden_and_every_other_ref_is_not() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    for name in [
        "refs/orna/ids/00112233445566778899aabbccddeeff",
        "refs/orna/pins/0123456789abcdef/scratch/row-1",
        "refs/orna-diff-fixture/anchor",
    ] {
        git(root, &["update-ref", name, &commit]);
    }
    let host = attached(root);

    assert_eq!(
        value("sys.git.refs | filter(r => r.hidden) | count()", &host),
        int(2)
    );
    assert_eq!(
        some(value(
            "sys.git.refs | filter(r => r.hidden) | map(r => r.name) | first()",
            &host
        )),
        text("refs/orna/ids/00112233445566778899aabbccddeeff")
    );
    for visible in ["refs/heads/main", "refs/orna-diff-fixture/anchor"] {
        assert_eq!(
            some(value(&ref_field(visible, "hidden"), &host)),
            OvbRaw::Bool(false),
            "{visible}"
        );
    }
    // A hidden ref is a valid ordinary Git ref with a row of its own.
    assert_eq!(
        some(value(
            &ref_field("refs/orna/pins/0123456789abcdef/scratch/row-1", "reference"),
            &host
        )),
        ref_ref(&commit, "refs/orna/pins/0123456789abcdef/scratch/row-1")
    );
}

#[test]
fn a_remote_tracking_ref_belongs_to_its_configured_remote_and_no_other_does() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git(root, &["update-ref", "refs/remotes/gone/main", &commit]);
    git(root, &["tag", "light"]);
    let host = attached(root);

    assert_eq!(
        some(some(value(
            &ref_field("refs/remotes/origin/main", "remote"),
            &host
        ))),
        remote_ref(&commit, "origin")
    );
    for none in [
        "refs/remotes/gone/main",
        "refs/heads/main",
        "refs/tags/light",
    ] {
        assert!(
            is_absent(&some(value(&ref_field(none, "remote"), &host))),
            "{none} belongs to no configured remote"
        );
    }
}

// The configuration is read only when some ref could belong to a remote.
#[test]
fn remote_configuration_is_not_read_without_a_remote_tracking_ref() {
    let directory = repository();
    let root = directory.path();
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["config", "remote.origin.promisor", "maybe"]);
    let host = attached(root);
    assert_eq!(value("sys.git.refs | count()", &host), int(1));
}

#[test]
fn remote_configuration_the_reader_does_not_understand_fails_closed_for_a_tracking_ref() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git(root, &["config", "remote.origin.promisor", "maybe"]);
    let host = attached(root);
    let failure = evaluate("sys.git.refs | count()", &host)
        .expect_err("a remote that cannot be read is not guessed at");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

// Specified: ORNA-SYS-111; no row is served over a pin that is not there.
#[test]
fn a_host_with_no_commit_pin_serves_no_ref() {
    let directory = repository();
    let host = context(directory.path(), None);
    let failure = evaluate("sys.git.refs | count()", &host)
        .expect_err("a ref cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_repository_with_no_ref_needs_no_pin() {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.refs | count()", &host), int(0));
}

#[test]
fn a_ref_whose_object_is_missing_fails_closed() {
    let directory = repository();
    let root = directory.path();
    std::fs::write(
        root.join(".git/refs/heads/ghost"),
        "1111111111111111111111111111111111111111\n",
    )
    .expect("a ref to an object the repository does not have");
    let host = attached(root);
    let failure = evaluate("sys.git.refs | count()", &host)
        .expect_err("a ref that cannot be read is not served");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_root_that_is_not_a_repository_fails_closed() {
    let directory = tempfile::tempdir().expect("plain directory");
    let host = context(directory.path(), Some(&"ab".repeat(20)));
    for source in ["sys.git.refs", "sys.git.stashes"] {
        let failure = evaluate(&format!("{source} | count()"), &host)
            .expect_err("no repository is attached at the root");
        assert_eq!(failure.code(), "sys.snapshot.incomplete", "{source}");
    }
}

#[test]
fn a_large_ref_set_is_read_across_pages_without_repeating_or_skipping() {
    let directory = repository();
    let root = directory.path();
    let tip = git(root, &["rev-parse", "HEAD"]);
    let updates = (0..600)
        .map(|index| format!("create refs/bulk/{index:04} {tip}\n"))
        .collect::<String>();
    git_with_input(root, &["update-ref", "--stdin"], &updates);
    let host = attached(root);

    assert_eq!(value("sys.git.refs | count()", &host), int(601));
    assert_eq!(
        some(value("sys.git.refs | map(r => r.name) | last()", &host)),
        text("refs/heads/main")
    );
    // The pages break after the 256th and 512th rows: the refs either side of
    // each break are each served exactly once.
    for index in [0, 255, 256, 257, 511, 512, 513, 599] {
        let name = format!("refs/bulk/{index:04}");
        assert_eq!(
            value(
                &format!("sys.git.refs | filter(r => r.name == \"{name}\") | count()"),
                &host
            ),
            int(1),
            "{name}"
        );
    }
}

// Specified: 34-system-reference.md `sys.Stash`; ORNA-SYS-112. One row per entry
// of the stash list, newest first.
#[test]
fn stashes_are_rows_newest_first_with_their_commit_message_and_time() {
    let directory = tracked_repository();
    let root = directory.path();
    stash_at(root, 1, "first stash", "2001-02-03T04:05:06Z");
    stash_at(root, 2, "second stash", "2002-03-04T05:06:07Z");
    stash_at(root, 3, "third stash", "2003-04-05T06:07:08Z");
    let head = git(root, &["rev-parse", "HEAD"]);
    let host = attached(root);

    assert_eq!(value("sys.git.stashes | count()", &host), int(3));
    assert_eq!(
        some(value(
            "sys.git.stashes | map(s => s.index) | first()",
            &host
        )),
        int(0)
    );
    assert_eq!(
        some(value("sys.git.stashes | map(s => s.index) | last()", &host)),
        int(2)
    );
    let expected = [
        (0, "On main: third stash", 1_049_522_828),
        (1, "On main: second stash", 1_015_218_367),
        (2, "On main: first stash", 981_173_106),
    ];
    for (index, message, created) in expected {
        let selector = format!("stash@{{{index}}}");
        let stash_commit = git(root, &["rev-parse", &selector]);
        assert_eq!(
            some(value(&stash_field(index, "reference"), &host)),
            stash_ref(&head, index),
            "{selector}"
        );
        assert_eq!(
            some(value(&stash_field(index, "commit"), &host)),
            commit_ref(&head, &stash_commit),
            "{selector}"
        );
        assert_eq!(
            some(value(&stash_field(index, "message"), &host)),
            text(message),
            "{selector}"
        );
        assert_eq!(
            some(some(value(&stash_field(index, "created"), &host))),
            instant(created),
            "{selector}"
        );
    }
}

#[test]
fn every_stash_entry_has_a_reference_of_its_own() {
    let directory = tracked_repository();
    let root = directory.path();
    stash_at(root, 1, "one", "2001-02-03T04:05:06Z");
    stash_at(root, 2, "two", "2001-02-03T04:05:06Z");
    let host = attached(root);
    assert_eq!(
        value(
            "sys.git.stashes | flat_map(a => sys.git.stashes | filter(b => a.index != b.index && a.reference == b.reference) | map(b => 1)) | count()",
            &host
        ),
        int(0)
    );
}

#[test]
fn a_repository_with_no_stash_serves_an_empty_relation_without_a_pin() {
    let directory = tracked_repository();
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.stashes | count()", &host), int(0));

    let empty = tempfile::tempdir().expect("repository directory");
    git(empty.path(), &["init", "-q", "-b", "main"]);
    let host = context(empty.path(), None);
    assert_eq!(value("sys.git.stashes | count()", &host), int(0));
}

#[test]
fn a_host_with_no_commit_pin_serves_no_stash() {
    let directory = tracked_repository();
    let root = directory.path();
    stash_at(root, 1, "one", "2001-02-03T04:05:06Z");
    let host = context(root, None);
    let failure = evaluate("sys.git.stashes | count()", &host)
        .expect_err("a stash cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_dropped_entry_renumbers_the_entries_after_it() {
    let directory = tracked_repository();
    let root = directory.path();
    stash_at(root, 1, "one", "2001-02-03T04:05:06Z");
    stash_at(root, 2, "two", "2001-02-03T04:05:06Z");
    stash_at(root, 3, "three", "2001-02-03T04:05:06Z");
    git(root, &["stash", "drop", "stash@{1}"]);
    let host = attached(root);

    assert_eq!(value("sys.git.stashes | count()", &host), int(2));
    assert_eq!(
        some(value(&stash_field(1, "message"), &host)),
        text("On main: one")
    );
}

#[test]
fn a_stash_entry_whose_commit_cannot_be_read_fails_closed() {
    let directory = tracked_repository();
    let root = directory.path();
    stash_at(root, 1, "one", "2001-02-03T04:05:06Z");
    let commit = git(root, &["rev-parse", "stash@{0}"]);
    let host = attached(root);
    std::fs::remove_file(
        root.join(".git/objects")
            .join(&commit[..2])
            .join(&commit[2..]),
    )
    .expect("the stash commit is a loose object");
    let failure = evaluate("sys.git.stashes | count()", &host)
        .expect_err("a stash that cannot be read is not served");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_large_stash_list_is_read_across_pages_in_index_order() {
    let directory = repository();
    let root = directory.path();
    let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
    let parent = git(root, &["rev-parse", "HEAD"]);
    // A stash-like commit is a merge of `HEAD` and a commit of the index; each
    // entry needs a commit of its own, since Git does not store the commit the
    // stash already holds twice in a row.
    let index_commit = git(root, &["commit-tree", &tree, "-p", &parent, "-m", "index"]);
    for index in 0..300 {
        let work = git(
            root,
            &[
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-p",
                &index_commit,
                "-m",
                &format!("wip {index}"),
            ],
        );
        git(
            root,
            &["stash", "store", "-m", &format!("entry {index}"), &work],
        );
    }
    let host = attached(root);

    assert_eq!(value("sys.git.stashes | count()", &host), int(300));
    assert_eq!(
        some(value(
            "sys.git.stashes | map(s => s.index) | first()",
            &host
        )),
        int(0)
    );
    assert_eq!(
        some(value("sys.git.stashes | map(s => s.index) | last()", &host)),
        int(299),
        "index order, not the order of the numbers spelt out"
    );
    // The newest entry is index 0 and holds the last message stored. The pages
    // break after the 256th row: the entries either side are each served once.
    for index in [0, 9, 10, 99, 100, 255, 256, 257, 299] {
        assert_eq!(
            value(
                &format!("sys.git.stashes | filter(s => s.index == {index}) | count()"),
                &host
            ),
            int(1),
            "{index}"
        );
    }
    assert_eq!(
        some(value(&stash_field(0, "message"), &host)),
        text("entry 299")
    );
    assert_eq!(
        some(value(&stash_field(299, "message"), &host)),
        text("entry 0")
    );
}
