//! The default path serves the linked `sys.git.refs` and `sys.git.stashes`
//! members (`sys.Ref`, `sys.Stash`) from the repository of a project, through
//! both `orna run` and `orna repl`: one row per ref under `refs/` named in full,
//! Orna's own `refs/orna/` refs marked hidden, a remote-tracking ref tied to its
//! configured remote, and one row per entry of the stash list, newest first
//! (ORNA-SYS-050, ORNA-SYS-111, ORNA-SYS-112). Rows are never served without the
//! commit pin their references need. The expectations are read from Git.

#[path = "support/git_project.rs"]
mod git_project;

use std::path::Path;

use git_project::{combined, committed_project, git, orna, repl, run, uncommitted_project};
use tempfile::TempDir;

const SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn ref_count(): Int {
    sys.git.refs | count()
}

pub fn hidden_count(): Int {
    sys.git.refs | filter(r => r.hidden) | count()
}

pub fn first_ref(): Str {
    sys.git.refs | map(r => r.name) | first() ?? "none"
}

pub fn last_ref(): Str {
    sys.git.refs | map(r => r.name) | last() ?? "none"
}

pub fn first_hidden_ref(): Str {
    sys.git.refs | filter(r => r.hidden) | map(r => r.name) | first() ?? "none"
}

pub fn origin_head_target(): Str {
    sys.git.refs | filter(r => r.name == "refs/remotes/origin/HEAD") | map(r => r.symbolic_target ?? "direct") | first() ?? "no ref"
}

pub fn light_tag_object_is_the_ref_target(): Int {
    sys.git.tags | filter(t => t.name == "light") | flat_map(t => sys.git.refs | filter(r => r.name == "refs/tags/light" && r.target == t.object) | map(r => 1)) | count()
}

pub fn remotes_matching_origin_main(): Int {
    sys.git.remotes | flat_map(m => sys.git.refs | filter(r => r.name == "refs/remotes/origin/main" && (r.remote ?? m.reference) == m.reference) | map(r => 1)) | count()
}

pub fn stash_count(): Int {
    sys.git.stashes | count()
}

pub fn newest_stash(): Str {
    sys.git.stashes | filter(s => s.index == 0) | map(s => s.message) | first() ?? "none"
}

pub fn oldest_stash(): Str {
    sys.git.stashes | filter(s => s.index == 1) | map(s => s.message) | first() ?? "none"
}

pub fn pairs_sharing_a_stash_reference(): Int {
    sys.git.stashes | flat_map(a => sys.git.stashes | filter(b => a.index != b.index && a.reference == b.reference) | map(b => 1)) | count()
}

pub fn record_and_count_refs(): Int {
    Item.insert({ id: "a", name: "A" });
    sys.git.refs | count()
}

pub fn record_and_newest_stash(): Str {
    Item.insert({ id: "b", name: "B" });
    sys.git.stashes | filter(s => s.index == 0) | map(s => s.message) | first() ?? "none"
}
"#;

/// How many refs Git lists in `root`, under `prefix` when one is given.
fn git_ref_count(root: &Path, prefix: &str) -> usize {
    git(root, &["for-each-ref", "--format=%(refname)", prefix])
        .lines()
        .count()
}

/// Appends a line to the tracked `main.orna` and stashes the change as
/// `message`.
fn stash(root: &Path, round: usize, message: &str) {
    let source = std::fs::read_to_string(root.join("main.orna")).expect("the project source");
    std::fs::write(
        root.join("main.orna"),
        format!("{source}// stash {round}\n"),
    )
    .expect("a change to stash");
    git(root, &["stash", "push", "-m", message]);
}

/// A committed project with two stash entries, `first stash` then `second stash`.
fn stashed_project() -> TempDir {
    let directory = committed_project(SOURCE);
    stash(directory.path(), 1, "first stash");
    stash(directory.path(), 2, "second stash");
    directory
}

#[test]
// Specified: 34-system-reference.md `sys.Ref`; one row per ref under `refs/`.
fn orna_run_counts_the_refs() {
    let project = committed_project(SOURCE);
    let root = project.path();
    let expected = |root: &Path| format!("{} : Int\n", git_ref_count(root, "refs/"));
    assert_eq!(run(root, "ref_count"), expected(root));
    git(root, &["tag", "light"]);
    assert_eq!(run(root, "ref_count"), expected(root));
    git(root, &["branch", "topic"]);
    assert_eq!(run(root, "ref_count"), expected(root));
    git(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    assert_eq!(run(root, "ref_count"), expected(root));
}

#[test]
// Specified: `sys.Ref.name` is the full ref name, the key `sys.Branch.upstream`
// names a tracked ref by; rows are in name order.
fn orna_run_names_every_ref_in_full_in_name_order() {
    let project = committed_project(SOURCE);
    let root = project.path();
    assert_eq!(run(root, "first_ref"), "\"refs/heads/main\" : Str\n");
    git(root, &["tag", "light"]);
    git(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    assert_eq!(run(root, "last_ref"), "\"refs/tags/v1\" : Str\n");
}

#[test]
// Specified: ORNA-SYS-050. Hidden Orna refs are visible through `sys.Ref.hidden`
// and stay ordinary refs.
fn orna_run_marks_the_orna_namespace_hidden() {
    let project = committed_project(SOURCE);
    let root = project.path();
    let head = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &[
            "update-ref",
            "refs/orna/pins/0123456789abcdef/scratch/row-1",
            &head,
        ],
    );
    git(
        root,
        &["update-ref", "refs/orna-diff-fixture/anchor", &head],
    );

    assert_eq!(
        run(root, "hidden_count"),
        format!("{} : Int\n", git_ref_count(root, "refs/orna/"))
    );
    let first_hidden = git(root, &["for-each-ref", "--format=%(refname)", "refs/orna/"])
        .lines()
        .next()
        .expect("a hidden ref")
        .to_owned();
    assert_eq!(
        run(root, "first_hidden_ref"),
        format!("{first_hidden:?} : Str\n")
    );
    assert_eq!(
        git(
            root,
            &["rev-parse", "refs/orna/pins/0123456789abcdef/scratch/row-1"]
        ),
        head,
        "a hidden ref is still an ordinary, valid Git ref"
    );
}

#[test]
// Specified: ORNA-SYS-112; a tag row and the ref row of that tag name one object
// by one reference.
fn orna_run_gives_a_tag_and_its_ref_the_same_object_reference() {
    let project = committed_project(SOURCE);
    let root = project.path();
    git(root, &["tag", "light"]);
    assert_eq!(run(root, "light_tag_object_is_the_ref_target"), "1 : Int\n");
}

#[test]
// Specified: `sys.Ref.symbolic_target`, `sys.Ref.remote`.
fn orna_run_names_the_target_and_the_remote_of_a_remote_tracking_ref() {
    let project = committed_project(SOURCE);
    let root = project.path();
    let head = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(
        root,
        &["remote", "add", "fork", "https://example.invalid/y.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    assert_eq!(
        run(root, "origin_head_target"),
        "\"refs/remotes/origin/main\" : Str\n"
    );
    assert_eq!(
        run(root, "remotes_matching_origin_main"),
        "1 : Int\n",
        "of two configured remotes only `origin` is the ref's remote"
    );
    // A remote that is not configured is not one the ref can name: with `origin`
    // gone and two other remotes configured, the ref names none of them, so
    // every remote matches the null it falls back from.
    git(root, &["remote", "remove", "origin"]);
    git(
        root,
        &["remote", "add", "mirror", "https://example.invalid/z.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    assert_eq!(
        run(root, "remotes_matching_origin_main"),
        "2 : Int\n",
        "the ref's remote is null, so both configured remotes match its fallback"
    );
}

#[test]
// Specified: 34-system-reference.md `sys.Stash`; one row per entry, newest first.
fn orna_run_serves_the_stash_entries_newest_first() {
    let project = stashed_project();
    let root = project.path();
    assert_eq!(run(root, "stash_count"), "2 : Int\n");
    assert_eq!(
        run(root, "newest_stash"),
        "\"On main: second stash\" : Str\n"
    );
    assert_eq!(
        run(root, "oldest_stash"),
        "\"On main: first stash\" : Str\n"
    );
    assert_eq!(
        run(root, "pairs_sharing_a_stash_reference"),
        "0 : Int\n",
        "every entry has a reference of its own"
    );
    git(root, &["stash", "drop"]);
    assert_eq!(run(root, "stash_count"), "1 : Int\n");
    assert_eq!(
        run(root, "newest_stash"),
        "\"On main: first stash\" : Str\n",
        "dropping the newest entry renumbers the one before it"
    );
}

#[test]
// Specified: a project with no stash has an empty relation.
fn orna_run_of_a_project_with_no_stash_has_none() {
    let project = committed_project(SOURCE);
    assert_eq!(run(project.path(), "stash_count"), "0 : Int\n");
    assert_eq!(run(project.path(), "newest_stash"), "\"none\" : Str\n");
}

#[test]
// Specified: ORNA-SYS-111. A repository with no commit has no ref and no stash,
// and an empty relation needs no pin.
fn orna_run_of_a_project_with_no_commit_has_no_refs_or_stashes() {
    let project = uncommitted_project(SOURCE);
    assert_eq!(run(project.path(), "ref_count"), "0 : Int\n");
    assert_eq!(run(project.path(), "stash_count"), "0 : Int\n");
}

#[test]
// Specified: ORNA-SYS-111. With refs or stash entries but no `HEAD` commit there
// is no pin for their references, so the read fails closed with
// `sys.snapshot.incomplete` instead of serving rows with null or invented
// references.
fn orna_run_refuses_refs_and_stashes_it_cannot_pin() {
    let project = stashed_project();
    git(project.path(), &["tag", "light"]);
    git(project.path(), &["checkout", "-q", "--orphan", "fresh"]);
    for (function, handle) in [
        ("ref_count", "sys.git.refs"),
        ("stash_count", "sys.git.stashes"),
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
fn repl_reads_the_refs_of_the_project() {
    let project = committed_project(SOURCE);
    git(project.path(), &["tag", "light"]);
    let output = repl(project.path(), "sys.git.refs | map(r => r.name)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"refs/heads/main\"") && text.contains("\"refs/tags/light\""),
        "one row per ref, each named in full: {text}"
    );
}

#[test]
fn repl_reads_the_stash_entries_of_the_project() {
    let project = stashed_project();
    let output = repl(project.path(), "sys.git.stashes | map(s => s.message)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"On main: second stash\"") && text.contains("\"On main: first stash\""),
        "one row per entry: {text}"
    );
}

#[test]
// The REPL input that also writes runs on the runtime writer thread, over the
// same repository and pin as a read-only one.
fn repl_input_that_writes_reads_the_same_refs_and_stashes_on_the_writer() {
    let project = stashed_project();
    let root = project.path();
    let expected_refs = git_ref_count(root, "refs/");
    let output = repl(
        root,
        "main.record_and_count_refs()\nmain.record_and_newest_stash()",
    );
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the input: {text}"
    );
    assert!(
        text.contains(&format!("{expected_refs} : Int")),
        "the writer read every ref: {text}"
    );
    assert!(
        text.contains("\"On main: second stash\""),
        "the writer read the newest stash entry: {text}"
    );
}
