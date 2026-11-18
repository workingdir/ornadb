//! `Repository::committed_refs` lists the refs of a real repository: the full
//! name of each, the object it names and, for a symbolic ref, the ref it points
//! at.

#[path = "../src/test_support.rs"]
mod test_support;

use orna_repository::{CommittedRef, Repository, RepositoryError};
use tempfile::TempDir;
use test_support::{commit_initial_module, git, git_ok, git_with_input};

/// A repository on `main` with one commit.
fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

fn names(refs: &[CommittedRef]) -> Vec<&str> {
    refs.iter().map(CommittedRef::name).collect()
}

fn named<'a>(refs: &'a [CommittedRef], name: &str) -> &'a CommittedRef {
    refs.iter()
        .find(|candidate| candidate.name() == name)
        .unwrap_or_else(|| panic!("no ref {name}"))
}

#[test]
fn lists_every_ref_under_refs_by_full_name_in_name_order() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["branch", "topic"]);
    git_ok(root, &["tag", "light"]);
    git_ok(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    git_ok(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git_ok(root, &["update-ref", "refs/notes/commits", &commit]);

    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(
        names(&refs),
        [
            "refs/heads/main",
            "refs/heads/topic",
            "refs/notes/commits",
            "refs/remotes/origin/main",
            "refs/tags/light",
            "refs/tags/v1",
        ]
    );
    for name in [
        "refs/heads/main",
        "refs/heads/topic",
        "refs/notes/commits",
        "refs/remotes/origin/main",
        "refs/tags/light",
    ] {
        let listed = named(&refs, name);
        assert_eq!(listed.target().as_str(), commit, "{name}");
        assert_eq!(listed.symbolic_target(), None, "{name}");
    }
}

#[test]
fn an_annotated_tag_ref_names_its_tag_object_and_not_the_commit() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    let tag_object = git(root, &["rev-parse", "refs/tags/v1"]);
    assert_ne!(tag_object, commit, "the ref names a tag object");

    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(named(&refs, "refs/tags/v1").target().as_str(), tag_object);
}

#[test]
fn a_tag_of_a_tag_names_the_outer_tag_object() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["tag", "-a", "-m", "inner", "inner"]);
    let inner = git(root, &["rev-parse", "refs/tags/inner"]);
    git_ok(root, &["tag", "-a", "-m", "outer", "outer", &inner]);
    let outer = git(root, &["rev-parse", "refs/tags/outer"]);

    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(named(&refs, "refs/tags/outer").target().as_str(), outer);
    assert_eq!(named(&refs, "refs/tags/inner").target().as_str(), inner);
}

#[test]
fn a_symbolic_ref_names_the_ref_it_points_at_and_the_object_that_resolves_to() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git_ok(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let refs = repository.committed_refs(32).expect("a listing");
    let symbolic = named(&refs, "refs/remotes/origin/HEAD");
    assert_eq!(symbolic.symbolic_target(), Some("refs/remotes/origin/main"));
    assert_eq!(symbolic.target().as_str(), commit);
    assert_eq!(
        named(&refs, "refs/remotes/origin/main").symbolic_target(),
        None
    );
}

#[test]
fn a_symbolic_ref_to_a_ref_that_does_not_exist_names_no_object_and_is_not_listed() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/dangling/HEAD",
            "refs/remotes/dangling/main",
        ],
    );
    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(names(&refs), ["refs/heads/main"]);
}

#[test]
fn head_and_the_pseudo_refs_are_not_listed() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join(".git/ORIG_HEAD"), format!("{commit}\n")).expect("a pseudo-ref");
    std::fs::write(
        root.join(".git/FETCH_HEAD"),
        format!("{commit}\t\tfetched\n"),
    )
    .expect("a pseudo-ref");
    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(names(&refs), ["refs/heads/main"]);
}

#[test]
fn the_orna_namespace_is_listed_and_marked_hidden() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    for name in [
        "refs/orna/ids/00112233445566778899aabbccddeeff",
        "refs/orna/pins/0123456789abcdef/scratch/row-1",
        "refs/orna-diff-fixture/anchor",
    ] {
        git_ok(root, &["update-ref", name, &commit]);
    }
    let refs = repository.committed_refs(32).expect("a listing");
    let hidden = refs
        .iter()
        .filter(|listed| listed.is_hidden())
        .map(CommittedRef::name)
        .collect::<Vec<_>>();
    assert_eq!(
        hidden,
        [
            "refs/orna/ids/00112233445566778899aabbccddeeff",
            "refs/orna/pins/0123456789abcdef/scratch/row-1",
        ]
    );
    for visible in ["refs/heads/main", "refs/orna-diff-fixture/anchor"] {
        assert!(!named(&refs, visible).is_hidden(), "{visible}");
    }
}

#[test]
fn a_remote_tracking_ref_resolves_to_a_configured_remote_by_name() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git_ok(root, &["update-ref", "refs/remotes/origin/main", &commit]);
    git_ok(root, &["update-ref", "refs/remotes/gone/main", &commit]);

    let remotes = repository.configured_remotes(8).expect("the remotes");
    let configured = remotes.iter().map(|remote| remote.name());
    let refs = repository.committed_refs(32).expect("a listing");
    let remote_of = |name: &str| named(&refs, name).remote(configured.clone());
    assert_eq!(remote_of("refs/remotes/origin/main"), Some("origin"));
    assert_eq!(remote_of("refs/remotes/gone/main"), None);
    assert_eq!(remote_of("refs/heads/main"), None);
}

#[test]
fn a_ref_component_may_start_with_a_dash_though_git_tag_will_not_create_one() {
    let (directory, repository) = repository();
    let root = directory.path();
    let commit = git(root, &["rev-parse", "HEAD"]);
    git_ok(root, &["update-ref", "refs/tags/-odd", &commit]);
    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(named(&refs, "refs/tags/-odd").target().as_str(), commit);
}

#[test]
fn a_repository_with_no_commit_has_no_ref() {
    let empty = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(empty.path());
    let repository = Repository::discover(empty.path()).expect("a Git worktree");
    assert!(
        repository.committed_refs(16).expect("a listing").is_empty(),
        "an unborn HEAD names no ref and HEAD is not listed"
    );
}

#[test]
fn more_refs_than_the_bound_are_refused_not_truncated() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["branch", "one"]);
    git_ok(directory.path(), &["tag", "two"]);
    assert_eq!(repository.committed_refs(3).expect("at the bound").len(), 3);
    assert!(matches!(
        repository.committed_refs(2),
        Err(RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repository.committed_refs(0),
        Err(RepositoryError::GitOperationFailed)
    ));
}

#[test]
fn a_ref_whose_object_is_missing_refuses_the_listing() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["tag", "fine"]);
    std::fs::write(
        root.join(".git/refs/heads/ghost"),
        "1111111111111111111111111111111111111111\n",
    )
    .expect("a ref to an object the repository does not have");
    assert!(
        repository.committed_refs(16).is_err(),
        "no listing is served over a ref that cannot be read"
    );
}

#[test]
fn a_ref_may_name_a_tree_or_a_blob() {
    let (directory, repository) = repository();
    let root = directory.path();
    let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
    let blob = git_with_input(root, &["hash-object", "-w", "--stdin"], "blob\n");
    git_ok(root, &["update-ref", "refs/tags/tree", &tree]);
    git_ok(root, &["update-ref", "refs/tags/blob", &blob]);
    let refs = repository.committed_refs(32).expect("a listing");
    assert_eq!(named(&refs, "refs/tags/tree").target().as_str(), tree);
    assert_eq!(named(&refs, "refs/tags/blob").target().as_str(), blob);
}
