#![cfg(feature = "system-host-bindings")]

//! `sys.git.tags` and `sys.git.remotes` evaluated over a real repository the
//! host attached: one `sys.Tag` row per tag and one `sys.Remote` row per
//! configured remote, every reference pinned to the commit the host attached at,
//! no plaintext credential in any row, and `sys.snapshot.incomplete` (never a
//! fabricated row) when the repository or the pin is unavailable
//! (ORNA-SYS-111, ORNA-SYS-112).

#[path = "support/git_members.rs"]
mod git_members;

use std::path::Path;

use git_members::{
    DATABASE, attached, context, evaluate, field, git, git_with_input, int, is_absent, oid_bytes,
    pin, reference, repository, some, text, value,
};
use orna_value::{
    GitHash, GitObjectId, OvbRaw, git_object_reference, remote_reference, tag_reference,
};

fn tag_ref(head: &str, name: &str) -> OvbRaw {
    reference(
        tag_reference(DATABASE, pin(head), name)
            .expect("a tag reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

fn object_ref(head: &str, object: &str) -> OvbRaw {
    let oid = GitObjectId::new(GitHash::Sha1, oid_bytes(object)).expect("an object id");
    reference(
        git_object_reference(DATABASE, pin(head), oid)
            .expect("an object reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

fn remote_ref(head: &str, name: &str) -> OvbRaw {
    reference(
        remote_reference(DATABASE, pin(head), name)
            .expect("a remote reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

/// Writes a tag object from its text, which Git is not asked to validate, and
/// points `refs/tags/name` at it.
fn write_tag_object(root: &Path, name: &str, body: &str) {
    let oid = git_with_input(
        root,
        &["hash-object", "-t", "tag", "-w", "--stdin", "--literally"],
        body,
    );
    git(root, &["update-ref", &format!("refs/tags/{name}"), &oid]);
}

/// The expression for field `name` of the row of the tag `tag`.
fn tag_field(tag: &str, name: &str) -> String {
    format!("sys.git.tags | filter(t => t.name == \"{tag}\") | map(t => t.{name}) | first()")
}

/// The expression for field `name` of the row of the remote `remote`.
fn remote_field(remote: &str, name: &str) -> String {
    format!("sys.git.remotes | filter(r => r.name == \"{remote}\") | map(r => r.{name}) | first()")
}

fn urls(urls: &[&str]) -> OvbRaw {
    OvbRaw::Array(urls.iter().map(|url| text(url)).collect())
}

// Specified: 34-system-reference.md `sys.Tag`; ORNA-SYS-112. A repository with a
// lightweight and an annotated tag yields two rows with their own references.
#[test]
fn lightweight_and_annotated_tags_are_rows_in_name_order() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(root, &["tag", "light"]);
    git(root, &["tag", "-a", "-m", "Release 1\n\nbody", "v1"]);
    let host = attached(root);

    assert_eq!(value("sys.git.tags | count()", &host), int(2));
    assert_eq!(
        some(value("sys.git.tags | map(t => t.name) | first()", &host)),
        text("light")
    );
    assert_eq!(
        some(value("sys.git.tags | map(t => t.name) | last()", &host)),
        text("v1")
    );
    assert_eq!(
        some(value(
            "sys.git.tags | map(t => t.reference) | first()",
            &host
        )),
        tag_ref(&commit, "light")
    );
    assert_eq!(
        some(value(
            "sys.git.tags | map(t => t.reference) | last()",
            &host
        )),
        tag_ref(&commit, "v1")
    );
    for name in ["light", "v1"] {
        assert_eq!(
            some(value(&tag_field(name, "object"), &host)),
            object_ref(&commit, &commit),
            "{name} names the commit, not its tag object"
        );
    }
}

#[test]
fn an_annotated_tag_has_its_message_and_tagger_and_a_lightweight_one_has_neither() {
    let directory = repository();
    let root = directory.path();
    git(root, &["tag", "light"]);
    git(root, &["tag", "-a", "-m", "Release 1\n\nbody", "v1"]);
    let host = attached(root);

    assert_eq!(
        some(value(&tag_field("light", "annotated"), &host)),
        OvbRaw::Bool(false)
    );
    assert_eq!(
        some(value(&tag_field("v1", "annotated"), &host)),
        OvbRaw::Bool(true)
    );

    assert!(is_absent(&some(value(
        &tag_field("light", "message"),
        &host
    ))));
    assert!(is_absent(&some(value(
        &tag_field("light", "tagger"),
        &host
    ))));
    assert_eq!(
        some(some(value(&tag_field("v1", "message"), &host))),
        text("Release 1\n\nbody\n")
    );

    let tagger = some(some(value(&tag_field("v1", "tagger"), &host)));
    assert_eq!(field(&tagger, "name"), &text("orna-test"));
    assert_eq!(
        some(field(&tagger, "email").clone()),
        text("orna-test@example.invalid")
    );

    // The language reads the absent message as null.
    assert_eq!(
        some(value(
            "sys.git.tags | filter(t => t.name == \"light\") | map(t => t.message ?? \"none\") | first()",
            &host
        )),
        text("none")
    );
}

#[test]
fn a_tag_of_a_tag_names_the_inner_tag_object() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(root, &["tag", "-a", "-m", "inner", "inner"]);
    let inner_object = git(root, &["rev-parse", "refs/tags/inner"]);
    git(
        root,
        &[
            "-c",
            "advice.nestedTag=false",
            "tag",
            "-a",
            "-m",
            "outer",
            "outer",
            &inner_object,
        ],
    );
    let host = attached(root);

    assert_eq!(
        some(value(&tag_field("outer", "object"), &host)),
        object_ref(&commit, &inner_object)
    );
    assert_eq!(
        some(value(&tag_field("inner", "object"), &host)),
        object_ref(&commit, &commit)
    );
}

#[test]
fn a_tag_object_with_no_tagger_or_address_reads_as_written() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    let header = format!("object {commit}\ntype commit\n");
    write_tag_object(root, "old", &format!("{header}tag old\n\nold style tag\n"));
    write_tag_object(
        root,
        "noaddress",
        &format!("{header}tag noaddress\ntagger Nameless <> 1700000000 +0000\n\nm\n"),
    );
    let host = attached(root);

    assert!(
        is_absent(&some(value(&tag_field("old", "tagger"), &host))),
        "a tag object with no tagger header has no tagger"
    );
    assert_eq!(
        some(some(value(&tag_field("old", "message"), &host))),
        text("old style tag\n")
    );
    let tagger = some(some(value(&tag_field("noaddress", "tagger"), &host)));
    assert_eq!(field(&tagger, "name"), &text("Nameless"));
    assert!(
        is_absent(field(&tagger, "email")),
        "a tagger with no address"
    );
}

// Specified: ORNA-SYS-111; no row is served over a pin that is not there.
#[test]
fn a_host_with_no_commit_pin_serves_no_tag() {
    let directory = repository();
    git(directory.path(), &["tag", "v1"]);
    let host = context(directory.path(), None);
    let failure = evaluate("sys.git.tags | count()", &host)
        .expect_err("a tag cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_repository_with_no_tag_needs_no_pin() {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.tags | count()", &host), int(0));
}

#[test]
fn a_root_that_is_not_a_repository_fails_closed() {
    let directory = tempfile::tempdir().expect("plain directory");
    let host = context(directory.path(), Some(&"ab".repeat(20)));
    for source in ["sys.git.tags", "sys.git.remotes"] {
        let failure = evaluate(&format!("{source} | count()"), &host)
            .expect_err("no repository is attached at the root");
        assert_eq!(failure.code(), "sys.snapshot.incomplete", "{source}");
    }
}

#[test]
fn a_large_tag_set_is_read_across_pages_without_repeating_or_skipping() {
    let directory = repository();
    let root = directory.path();
    let tip = git(root, &["rev-parse", "HEAD"]);
    let updates = (0..600)
        .map(|index| format!("create refs/tags/bulk/{index:04} {tip}\n"))
        .collect::<String>();
    git_with_input(root, &["update-ref", "--stdin"], &updates);
    git(root, &["tag", "-a", "-m", "last", "zz-annotated"]);
    let host = attached(root);

    assert_eq!(value("sys.git.tags | count()", &host), int(601));
    assert_eq!(
        some(value("sys.git.tags | map(t => t.name) | last()", &host)),
        text("zz-annotated")
    );
    // The pages break after the 256th and 512th rows: the tags either side of
    // each break are each served exactly once.
    for index in [0, 255, 256, 257, 511, 512, 513, 599] {
        let name = format!("bulk/{index:04}");
        assert_eq!(
            value(
                &format!("sys.git.tags | filter(t => t.name == \"{name}\") | count()"),
                &host
            ),
            int(1),
            "{name}"
        );
    }
}

// Specified: 34-system-reference.md `sys.Remote`; ORNA-SYS-112.
#[test]
fn remotes_are_rows_in_name_order_with_their_urls() {
    let directory = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/org/repo.git",
        ],
    );
    git(
        root,
        &["remote", "add", "fork", "git@example.invalid:me/repo.git"],
    );
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://mirror.example.invalid/org/repo.git",
        ],
    );
    git(
        root,
        &[
            "config",
            "remote.fork.pushurl",
            "ssh://example.invalid/me/push.git",
        ],
    );
    let host = attached(root);

    assert_eq!(value("sys.git.remotes | count()", &host), int(2));
    assert_eq!(
        some(value("sys.git.remotes | map(r => r.name) | first()", &host)),
        text("fork")
    );
    assert_eq!(
        some(value("sys.git.remotes | map(r => r.name) | last()", &host)),
        text("origin")
    );
    assert_eq!(
        some(value(&remote_field("origin", "reference"), &host)),
        remote_ref(&commit, "origin")
    );
    assert_eq!(
        some(value(&remote_field("fork", "reference"), &host)),
        remote_ref(&commit, "fork")
    );

    assert_eq!(
        some(value(&remote_field("origin", "fetch_urls"), &host)),
        urls(&[
            "https://example.invalid/org/repo.git",
            "https://mirror.example.invalid/org/repo.git"
        ])
    );
    assert_eq!(
        some(value(&remote_field("origin", "push_urls"), &host)),
        urls(&[
            "https://example.invalid/org/repo.git",
            "https://mirror.example.invalid/org/repo.git"
        ]),
        "with no pushurl a push goes to the urls"
    );
    assert_eq!(
        some(value(&remote_field("fork", "fetch_urls"), &host)),
        urls(&["git@example.invalid:me/repo.git"])
    );
    assert_eq!(
        some(value(&remote_field("fork", "push_urls"), &host)),
        urls(&["ssh://example.invalid/me/push.git"])
    );
}

// Specified: 34-system-reference.md `sys.Remote` ("without plaintext
// credentials"); ORNA-SYS-099, ORNA-SYS-100.
#[test]
fn no_row_holds_a_plaintext_credential() {
    let directory = repository();
    let root = directory.path();
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    git(
        root,
        &[
            "config",
            "remote.origin.pushurl",
            "https://ghp_token123@example.invalid/push.git",
        ],
    );
    let host = attached(root);

    assert_eq!(
        some(value(&remote_field("origin", "fetch_urls"), &host)),
        urls(&["https://<redacted>@example.invalid/private.git"])
    );
    assert_eq!(
        some(value(&remote_field("origin", "push_urls"), &host)),
        urls(&["https://<redacted>@example.invalid/push.git"])
    );

    let whole_row = format!(
        "{:?}",
        value("sys.git.remotes | map(r => r) | first()", &host)
    );
    for secret in ["secret", "account", "ghp_token123"] {
        assert!(
            !whole_row.contains(secret),
            "{secret} reached a row: {whole_row}"
        );
    }
}

#[test]
fn a_remote_is_a_promisor_when_git_says_so_and_has_no_last_error() {
    let directory = repository();
    let root = directory.path();
    git(
        root,
        &["remote", "add", "plain", "https://a.example.invalid/x.git"],
    );
    git(
        root,
        &["remote", "add", "origin", "https://b.example.invalid/x.git"],
    );
    git(root, &["config", "remote.origin.promisor", "true"]);
    let host = attached(root);

    assert_eq!(
        some(value(&remote_field("origin", "promisor"), &host)),
        OvbRaw::Bool(true)
    );
    assert_eq!(
        some(value(&remote_field("plain", "promisor"), &host)),
        OvbRaw::Bool(false)
    );
    assert_eq!(
        value(
            "sys.git.remotes | filter(r => r.promisor) | map(r => r.name) | count()",
            &host
        ),
        int(1)
    );
    assert!(
        is_absent(&some(value(&remote_field("origin", "last_error"), &host))),
        "no remote failure is retained, so there is no last error"
    );
}

#[test]
fn a_host_with_no_commit_pin_serves_no_remote() {
    let directory = repository();
    git(
        directory.path(),
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    let host = context(directory.path(), None);
    let failure = evaluate("sys.git.remotes | count()", &host)
        .expect_err("a remote cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_repository_with_no_remote_needs_no_pin() {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.remotes | count()", &host), int(0));
}

#[test]
fn configuration_the_reader_does_not_understand_fails_closed() {
    let directory = repository();
    let root = directory.path();
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["config", "remote.origin.promisor", "maybe"]);
    let host = attached(root);
    let failure = evaluate("sys.git.remotes | count()", &host)
        .expect_err("a promisor that is not a boolean is not guessed at");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}
