//! `Repository::local_branches` lists the local branches of a real repository:
//! the commit each names, the branch `HEAD` is attached to, and the upstream
//! each tracks with how far the two have diverged.

#[path = "../src/test_support.rs"]
mod test_support;

use std::path::Path;

use orna_repository::{CommittedBranch, Repository, RepositoryError};
use tempfile::TempDir;
use test_support::{commit_initial_module, configure_fixture_git_identity, git, git_ok};

/// A repository on `main` with one commit.
fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

fn empty_commit(root: &Path, message: &str) {
    git_ok(root, &["commit", "--allow-empty", "-m", message]);
}

fn names(branches: &[CommittedBranch]) -> Vec<&str> {
    branches.iter().map(CommittedBranch::name).collect()
}

fn branch<'a>(branches: &'a [CommittedBranch], name: &str) -> &'a CommittedBranch {
    branches
        .iter()
        .find(|branch| branch.name() == name)
        .unwrap_or_else(|| panic!("no branch {name}"))
}

#[test]
fn lists_every_local_branch_in_name_order_with_its_tip() {
    let (directory, repository) = repository();
    let root = directory.path();
    let main_tip = git(root, &["rev-parse", "main"]);
    git_ok(root, &["branch", "zeta"]);
    git_ok(root, &["branch", "alpha"]);
    git_ok(root, &["branch", "feature/x"]);
    git_ok(root, &["checkout", "-q", "alpha"]);
    empty_commit(root, "alpha moves");
    let alpha_tip = git(root, &["rev-parse", "alpha"]);
    assert_ne!(alpha_tip, main_tip);

    let branches = repository.local_branches(16).expect("a listing");
    assert_eq!(names(&branches), ["alpha", "feature/x", "main", "zeta"]);
    for (name, tip) in [
        ("alpha", &alpha_tip),
        ("feature/x", &main_tip),
        ("main", &main_tip),
        ("zeta", &main_tip),
    ] {
        assert_eq!(branch(&branches, name).commit().as_str(), tip, "{name}");
        assert_eq!(branch(&branches, name).upstream(), None, "{name}");
    }
    assert_eq!(
        branches
            .iter()
            .filter(|branch| branch.is_current())
            .map(CommittedBranch::name)
            .collect::<Vec<_>>(),
        ["alpha"],
        "only the branch HEAD is attached to is current"
    );
}

#[test]
fn a_detached_head_is_attached_to_no_branch() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["checkout", "-q", "--detach"]);
    let branches = repository.local_branches(16).expect("a listing");
    assert_eq!(names(&branches), ["main"]);
    assert!(!branches[0].is_current());
}

#[test]
fn a_repository_with_no_commit_has_no_branch() {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    assert!(repository.local_branches(16).expect("a listing").is_empty());
}

#[test]
fn a_branch_tracking_a_local_branch_counts_its_divergence() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(root, &["branch", "topic"]);
    git_ok(root, &["branch", "--set-upstream-to=main", "topic"]);
    let level = repository.local_branches(16).expect("a listing");
    let upstream = branch(&level, "topic").upstream().expect("an upstream");
    assert_eq!(upstream.reference(), "refs/heads/main");
    assert_eq!((upstream.ahead(), upstream.behind()), (0, 0));

    git_ok(root, &["checkout", "-q", "topic"]);
    empty_commit(root, "topic one");
    git_ok(root, &["checkout", "-q", "main"]);
    empty_commit(root, "main one");
    empty_commit(root, "main two");

    let branches = repository.local_branches(16).expect("a listing");
    let upstream = branch(&branches, "topic").upstream().expect("an upstream");
    assert_eq!(upstream.reference(), "refs/heads/main");
    assert_eq!(
        (upstream.ahead(), upstream.behind()),
        (1, 2),
        "topic has one commit main lacks and lacks two main has"
    );
    assert_eq!(branch(&branches, "main").upstream(), None);
}

/// A clone of a one-commit `origin`, whose `main` tracks `origin/main`.
fn clone_of_origin() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let origin = directory.path().join("origin");
    std::fs::create_dir(&origin).expect("origin directory");
    test_support::initialize_fixture_repository(&origin);
    commit_initial_module(&origin);
    let work = directory.path().join("work");
    git_ok(
        directory.path(),
        &["clone", "-q", "origin", work.to_str().expect("UTF-8 path")],
    );
    configure_fixture_git_identity(&work);
    (directory, origin, work)
}

#[test]
fn a_branch_tracking_a_remote_branch_counts_its_divergence() {
    let (_directory, origin, work) = clone_of_origin();
    empty_commit(&work, "work one");
    empty_commit(&origin, "origin one");
    empty_commit(&origin, "origin two");
    empty_commit(&origin, "origin three");
    git_ok(&work, &["fetch", "-q"]);

    let repository = Repository::discover(&work).expect("a Git worktree");
    let branches = repository.local_branches(16).expect("a listing");
    let upstream = branch(&branches, "main").upstream().expect("an upstream");
    assert_eq!(upstream.reference(), "refs/remotes/origin/main");
    assert_eq!((upstream.ahead(), upstream.behind()), (1, 3));
    assert!(branch(&branches, "main").is_current());
}

#[test]
fn an_upstream_whose_ref_is_gone_is_no_upstream() {
    let (_directory, _origin, work) = clone_of_origin();
    let repository = Repository::discover(&work).expect("a Git worktree");
    assert!(
        branch(&repository.local_branches(16).expect("a listing"), "main")
            .upstream()
            .is_some()
    );

    git_ok(&work, &["update-ref", "-d", "refs/remotes/origin/main"]);
    let branches = repository.local_branches(16).expect("a listing");
    assert_eq!(
        branch(&branches, "main").upstream(),
        None,
        "the tracking ref is gone: there is no ref to name and nothing to count against"
    );
}

#[test]
fn more_branches_than_the_bound_are_refused_not_truncated() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["branch", "one"]);
    git_ok(directory.path(), &["branch", "two"]);
    assert_eq!(repository.local_branches(3).expect("at the bound").len(), 3);
    assert!(matches!(
        repository.local_branches(2),
        Err(RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repository.local_branches(0),
        Err(RepositoryError::GitOperationFailed)
    ));
}
