//! The default path serves the linked `sys.git.objects` member (`sys.GitObject`)
//! from the object store of the repository of a project, through both `orna run`
//! and `orna repl`: one row per object the store holds, whether or not a reference
//! reaches it, with its id, the `sys.GitObjectKind` Git reads it as and its size in
//! bytes. Every stored object is available locally with no promisor remote
//! (ORNA-SYS-052, ORNA-SYS-111). A partial clone, whose store does not enumerate the
//! objects it has promised, and a project with no commit for the rows to pin to are
//! both refused with `sys.snapshot.incomplete` rather than served truncated.

#[path = "support/git_project.rs"]
mod git_project;

use std::{collections::BTreeMap, path::Path};

use git_project::{combined, committed_project, git, orna, repl, run};
use tempfile::TempDir;

const SOURCE: &str = r"pub table Item(id: Str) {
    name: Str,
}

pub fn object_count(): Int {
    sys.git.objects | count()
}

pub fn local_object_count(): Int {
    sys.git.objects | filter(o => o.available_locally) | count()
}

pub fn empty_object_count(): Int {
    sys.git.objects | filter(o => (o.size ?? -1) == 0) | count()
}

pub fn blob_count(): Int {
    sys.git.objects | filter(o => o.kind == sys.GitObjectKind.blob) | count()
}

pub fn tree_count(): Int {
    sys.git.objects | filter(o => o.kind == sys.GitObjectKind.tree) | count()
}

pub fn commit_count(): Int {
    sys.git.objects | filter(o => o.kind == sys.GitObjectKind.commit) | count()
}

pub fn tag_count(): Int {
    sys.git.objects | filter(o => o.kind == sys.GitObjectKind.tag) | count()
}
";

/// Every object of the repository at `root` as `git cat-file` lists it: its kind
/// and its size in bytes, keyed by id. Git lists an object once for each pack or
/// loose copy it has, so the listing is deduplicated by id.
fn stored_objects(root: &Path) -> BTreeMap<String, (String, u64)> {
    let listing = git(
        root,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
    );
    listing
        .lines()
        .map(|line| {
            let mut fields = line.split(' ');
            let oid = fields.next().expect("an object id").to_owned();
            let kind = fields.next().expect("an object kind").to_owned();
            let size = fields
                .next()
                .expect("an object size")
                .parse::<u64>()
                .expect("a size");
            (oid, (kind, size))
        })
        .collect()
}

fn count_of_kind(objects: &BTreeMap<String, (String, u64)>, kind: &str) -> usize {
    objects
        .values()
        .filter(|(object_kind, _)| object_kind == kind)
        .count()
}

/// Runs `main.FUNCTION`, which counts rows, and checks it printed `expected`.
fn assert_count(root: &Path, function: &str, expected: usize) {
    let text = run(root, function);
    assert_eq!(text, format!("{expected} : Int\n"), "main.{function}");
}

/// Feeds `input` to the REPL and checks that it printed `expected` as an `Int`.
fn assert_repl_count(root: &Path, input: &str, expected: usize) {
    let output = repl(root, input);
    let text = combined(&output);
    assert!(!text.contains("error["), "{text}");
    assert!(text.contains(&format!("{expected} : Int")), "{text}");
}

/// Runs `main.FUNCTION` and checks the read is refused: the run fails, the refusal
/// names `sys.git.objects`, and no row was printed.
fn assert_refused(root: &Path, function: &str) {
    let output = orna(root, &["run", &format!("main.{function}")]);
    let text = combined(&output);
    assert!(!output.status.success(), "{function} was served: {text}");
    assert!(text.contains("sys.git.objects"), "{text}");
    assert!(!text.contains(": Int"), "no row was served: {text}");
}

/// A committed project holding every kind of object: the commits of its history,
/// the trees and blobs they reach, the blob of an empty file, an annotated tag, and
/// a commit no reference reaches.
fn project_with_every_kind() -> TempDir {
    let directory = committed_project(SOURCE);
    let root = directory.path();
    std::fs::write(root.join("empty.txt"), "").expect("empty file");
    git(root, &["add", "empty.txt"]);
    git(root, &["commit", "-q", "-m", "empty file"]);
    git(root, &["tag", "-a", "light", "-m", "light"]);
    let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
    git(
        root,
        &["commit-tree", &tree, "-p", "HEAD", "-m", "unreferenced"],
    );
    directory
}

#[test]
// Specified: ORNA-SYS-052. One `sys.GitObject` row per object the store holds,
// whether or not a reference reaches it, for each of the four kinds Git stores.
fn orna_run_counts_the_objects_the_store_holds_by_kind() {
    let project = project_with_every_kind();
    let root = project.path();
    let objects = stored_objects(root);
    assert_count(root, "object_count", objects.len());
    for (function, kind) in [
        ("blob_count", "blob"),
        ("tree_count", "tree"),
        ("commit_count", "commit"),
        ("tag_count", "tag"),
    ] {
        let expected = count_of_kind(&objects, kind);
        assert!(expected > 0, "the fixture stores a {kind} object");
        assert_count(root, function, expected);
    }
}

#[test]
// Specified: ORNA-SYS-052. `size` is the object's size in bytes: the empty file's
// blob is zero bytes and `main.orna`'s blob is as long as its source. Every object
// the store holds is available locally.
fn orna_run_serves_each_objects_size_and_availability() {
    let project = project_with_every_kind();
    let root = project.path();
    let objects = stored_objects(root);
    let zero_sized = objects.values().filter(|(_, size)| *size == 0).count();
    assert!(zero_sized > 0, "the empty file's blob has no bytes");
    assert_count(root, "empty_object_count", zero_sized);
    let source_size = u64::try_from(SOURCE.len()).expect("the source's size");
    let sized_like_source = objects
        .values()
        .filter(|(kind, size)| kind == "blob" && *size == source_size)
        .count();
    assert!(
        sized_like_source > 0,
        "main.orna's blob is as long as its source"
    );
    assert_repl_count(
        root,
        &format!("sys.git.objects | filter(o => (o.size ?? -1) == {source_size}) | count()"),
        sized_like_source,
    );
    assert_count(root, "local_object_count", objects.len());
}

#[test]
// Specified: ORNA-SYS-052. Each commit the history reaches is served as a `commit`
// object with the commit's id. The store also holds a commit no reference reaches,
// and that one is served as an object too.
fn repl_serves_each_reachable_commit_as_a_commit_object() {
    let project = project_with_every_kind();
    let root = project.path();
    let reachable = git(root, &["rev-list", "--count", "HEAD"])
        .parse::<usize>()
        .expect("a commit count");
    assert_repl_count(
        root,
        "sys.git.commits | flat_map(c => sys.git.objects | filter(o => o.kind == sys.GitObjectKind.commit && o.oid == c.oid) | map(o => 1)) | count()",
        reachable,
    );
    let stored = count_of_kind(&stored_objects(root), "commit");
    assert!(
        stored > reachable,
        "the unreferenced commit is stored: {stored} against {reachable}"
    );
    assert_repl_count(root, "main.object_count()", stored_objects(root).len());
}

#[test]
// Specified: ORNA-SYS-111. A partial clone's store does not enumerate the objects it
// has promised, so the read fails closed with `sys.snapshot.incomplete` rather than
// serving the objects it holds as though they were all of them.
fn orna_run_refuses_the_objects_of_a_partial_clone() {
    let project = committed_project(SOURCE);
    let root = project.path();
    assert_count(root, "object_count", stored_objects(root).len());
    git(root, &["remote", "add", "origin", "."]);
    git(root, &["config", "remote.origin.promisor", "true"]);
    git(
        root,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
    git(root, &["config", "extensions.partialClone", "origin"]);
    assert_refused(root, "object_count");
}

#[test]
// Specified: ORNA-SYS-111. Object rows are pinned to the commit `HEAD` names. A
// project with objects but no `HEAD` commit has no pin, so the read fails closed
// with `sys.snapshot.incomplete` instead of serving rows that name no commit.
fn orna_run_refuses_objects_it_cannot_pin() {
    let project = committed_project(SOURCE);
    git(project.path(), &["checkout", "-q", "--orphan", "fresh"]);
    assert_refused(project.path(), "object_count");
}
