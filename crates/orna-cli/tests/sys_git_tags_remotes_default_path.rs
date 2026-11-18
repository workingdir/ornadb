//! The default path serves the linked `sys.git.tags` and `sys.git.remotes`
//! members (`sys.Tag`, `sys.Remote`) from the repository of a project, through
//! both `orna run` and `orna repl`: one row per tag and one per configured
//! remote, annotated tags told from lightweight ones, and no plaintext credential
//! in any row (ORNA-SYS-111, ORNA-SYS-112, ORNA-SYS-099, ORNA-SYS-100). Rows are
//! never served without the commit pin their references need.

#[path = "support/git_project.rs"]
mod git_project;

use git_project::{combined, committed_project, git, orna, repl, run, uncommitted_project};
use tempfile::TempDir;

const SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn tag_count(): Int {
    sys.git.tags | count()
}

pub fn annotated_tag(): Str {
    sys.git.tags | filter(t => t.annotated) | map(t => t.name) | first() ?? "none"
}

pub fn release_message(): Str {
    sys.git.tags | filter(t => t.name == "v1") | map(t => t.message ?? "no message") | first() ?? "no tag"
}

pub fn light_message(): Str {
    sys.git.tags | filter(t => t.name == "light") | map(t => t.message ?? "no message") | first() ?? "no tag"
}

pub fn pairs_sharing_an_object(): Int {
    sys.git.tags | flat_map(a => sys.git.tags | filter(b => a.name != b.name && a.object == b.object) | map(b => 1)) | count()
}

pub fn pairs_sharing_a_reference(): Int {
    sys.git.tags | flat_map(a => sys.git.tags | filter(b => a.name != b.name && a.reference == b.reference) | map(b => 1)) | count()
}

pub fn remote_count(): Int {
    sys.git.remotes | count()
}

pub fn promisor_remote(): Str {
    sys.git.remotes | filter(r => r.promisor) | map(r => r.name) | first() ?? "none"
}

pub fn origin_fetch_url(): Str {
    sys.git.remotes | filter(r => r.name == "origin") | flat_map(r => r.fetch_urls) | first() ?? "none"
}

pub fn origin_push_url(): Str {
    sys.git.remotes | filter(r => r.name == "origin") | flat_map(r => r.push_urls) | first() ?? "none"
}
"#;

/// A committed project with a lightweight tag `light` and an annotated tag `v1`,
/// both at the committed commit.
fn tagged_project() -> TempDir {
    let directory = committed_project(SOURCE);
    git(directory.path(), &["tag", "light"]);
    git(directory.path(), &["tag", "-a", "-m", "Release 1", "v1"]);
    directory
}

/// A committed project with the remotes `fork` and `origin`, the latter a
/// promisor whose URL carries a credential.
fn remote_project() -> TempDir {
    let directory = committed_project(SOURCE);
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
        &["remote", "add", "fork", "git@example.invalid:me/repo.git"],
    );
    git(root, &["config", "remote.origin.promisor", "true"]);
    directory
}

#[test]
// Specified: 34-system-reference.md `sys.Tag`; one row per tag.
fn orna_run_counts_the_tags() {
    let project = committed_project(SOURCE);
    assert_eq!(run(project.path(), "tag_count"), "0 : Int\n");
    git(project.path(), &["tag", "light"]);
    assert_eq!(run(project.path(), "tag_count"), "1 : Int\n");
    git(project.path(), &["tag", "-a", "-m", "Release 1", "v1"]);
    assert_eq!(run(project.path(), "tag_count"), "2 : Int\n");
}

#[test]
// Specified: `message` and `tagger` are the tag object's, so a lightweight tag
// has none.
fn orna_run_tells_annotated_tags_from_lightweight_ones() {
    let project = tagged_project();
    assert_eq!(run(project.path(), "annotated_tag"), "\"v1\" : Str\n");
    assert_eq!(
        run(project.path(), "release_message"),
        "\"Release 1\\n\" : Str\n"
    );
    assert_eq!(
        run(project.path(), "light_message"),
        "\"no message\" : Str\n",
        "a lightweight tag has no message"
    );
}

#[test]
// Specified: ORNA-SYS-112; a reference is the coordinates of its row, so every
// tag has a reference of its own, and two tags that name one object share the
// reference to it, whether or not one is annotated.
fn orna_run_gives_each_tag_its_own_reference_and_tags_at_one_object_one_object_reference() {
    let project = tagged_project();
    assert_eq!(
        run(project.path(), "pairs_sharing_an_object"),
        "2 : Int\n",
        "light and v1 name the one commit"
    );
    assert_eq!(
        run(project.path(), "pairs_sharing_a_reference"),
        "0 : Int\n",
        "every tag has a reference of its own"
    );
}

#[test]
// Specified: 34-system-reference.md `sys.Remote`; one row per configured remote.
fn orna_run_counts_the_remotes_and_finds_the_promisor() {
    let project = committed_project(SOURCE);
    assert_eq!(run(project.path(), "remote_count"), "0 : Int\n");
    let root = project.path();
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(
        root,
        &["remote", "add", "fork", "git@example.invalid:me/x.git"],
    );
    assert_eq!(run(root, "remote_count"), "2 : Int\n");
    assert_eq!(run(root, "promisor_remote"), "\"none\" : Str\n");
    git(root, &["config", "remote.fork.promisor", "true"]);
    assert_eq!(run(root, "promisor_remote"), "\"fork\" : Str\n");
}

#[test]
// Specified: `sys.Remote` is a "Git remote without plaintext credentials";
// ORNA-SYS-099, ORNA-SYS-100.
fn orna_run_never_prints_a_credential() {
    let project = remote_project();
    git(
        project.path(),
        &[
            "config",
            "remote.origin.pushurl",
            "https://ghp_token123@example.invalid/push.git",
        ],
    );
    let fetch = run(project.path(), "origin_fetch_url");
    let push = run(project.path(), "origin_push_url");
    assert_eq!(
        fetch,
        "\"https://<redacted>@example.invalid/private.git\" : Str\n"
    );
    assert_eq!(
        push,
        "\"https://<redacted>@example.invalid/push.git\" : Str\n"
    );
    for secret in ["secret", "account", "ghp_token123"] {
        assert!(
            !fetch.contains(secret) && !push.contains(secret),
            "{secret}"
        );
    }
}

#[test]
// Specified: ORNA-SYS-111. A repository with no commit has no tag and no remote
// to pin, and an empty relation needs no pin.
fn orna_run_of_a_project_with_no_commit_has_no_tags_or_remotes() {
    let project = uncommitted_project(SOURCE);
    assert_eq!(run(project.path(), "tag_count"), "0 : Int\n");
    assert_eq!(run(project.path(), "remote_count"), "0 : Int\n");
}

#[test]
// Specified: ORNA-SYS-111. With tags or remotes but no `HEAD` commit there is no
// pin for their references, so the read fails closed with
// `sys.snapshot.incomplete` instead of serving rows with null or invented
// references.
fn orna_run_refuses_tags_and_remotes_it_cannot_pin() {
    let project = remote_project();
    git(project.path(), &["tag", "light"]);
    git(project.path(), &["checkout", "-q", "--orphan", "fresh"]);
    for (function, handle) in [
        ("tag_count", "sys.git.tags"),
        ("remote_count", "sys.git.remotes"),
    ] {
        let output = orna(project.path(), &["run", &format!("main.{function}")]);
        let text = combined(&output);
        assert!(
            !output.status.success(),
            "{handle} is not served without a commit pin: {text}"
        );
        assert!(
            text.contains(handle),
            "the refusal points at the {handle} read: {text}"
        );
        assert!(!text.contains(": Int"), "no row was served: {text}");
    }
}

#[test]
fn repl_reads_the_tags_of_the_project() {
    let project = tagged_project();
    let output = repl(project.path(), "sys.git.tags | map(t => t.name)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"light\"") && text.contains("\"v1\""),
        "one row per tag: {text}"
    );
}

#[test]
fn repl_reads_the_remotes_of_the_project_without_a_credential() {
    let project = remote_project();
    let output = repl(project.path(), "sys.git.remotes | map(r => r.fetch_urls)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("https://<redacted>@example.invalid/private.git")
            && text.contains("git@example.invalid:me/repo.git"),
        "one row per remote, its credential redacted: {text}"
    );
    for secret in ["secret", "account"] {
        assert!(!text.contains(secret), "{secret} reached the REPL: {text}");
    }
}
