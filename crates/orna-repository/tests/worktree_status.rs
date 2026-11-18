//! `Repository::worktree_status_entries` reports the paths `git status` reports,
//! and `Repository::git_repository_facts` observes where the repository's Git
//! directory and worktree are, its object format and its partial-clone state:
//! both against real Git worktrees.

#[path = "../src/test_support.rs"]
mod test_support;

use std::fs;
use std::path::Path;

use orna_repository::{GitHashAlgorithm, Repository, StatusChange, WorktreeStatusEntry};
use tempfile::TempDir;
use test_support::{commit_initial_module, git_ok};

/// A repository on `main` with one commit, which tracks `main.orna`.
fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

fn listed(repository: &Repository) -> Vec<WorktreeStatusEntry> {
    repository
        .worktree_status_entries(64)
        .expect("a status listing")
}

fn entry<'a>(entries: &'a [WorktreeStatusEntry], path: &str) -> &'a WorktreeStatusEntry {
    entries
        .iter()
        .find(|entry| entry.path() == path)
        .unwrap_or_else(|| panic!("no status entry for {path}"))
}

fn write_file(root: &Path, path: &str, text: &str) {
    fs::write(root.join(path), text).expect("write fixture file");
}

#[test]
fn a_clean_worktree_lists_no_paths() {
    let (_directory, repository) = repository();
    assert!(listed(&repository).is_empty());
}

#[test]
fn an_unstaged_edit_is_a_worktree_modification() {
    let (directory, repository) = repository();
    write_file(directory.path(), "main.orna", "module changed;\n");
    let entries = listed(&repository);
    let edited = entry(&entries, "main.orna");
    assert_eq!(edited.index_status(), None);
    assert_eq!(edited.worktree_status(), Some(StatusChange::Modify));
    assert!(!edited.untracked());
    assert!(!edited.ignored());
}

#[test]
fn an_untracked_file_reads_as_added_in_the_worktree() {
    let (directory, repository) = repository();
    write_file(directory.path(), "notes.txt", "scratch\n");
    let entries = listed(&repository);
    let untracked = entry(&entries, "notes.txt");
    assert_eq!(untracked.index_status(), None);
    assert_eq!(untracked.worktree_status(), Some(StatusChange::Add));
    assert!(untracked.untracked());
    assert!(!untracked.ignored());
}

#[test]
fn a_staged_addition_is_an_index_change() {
    let (directory, repository) = repository();
    write_file(directory.path(), "added.orna", "module added;\n");
    git_ok(directory.path(), &["add", "added.orna"]);
    let entries = listed(&repository);
    let added = entry(&entries, "added.orna");
    assert_eq!(added.index_status(), Some(StatusChange::Add));
    assert_eq!(added.worktree_status(), None);
    assert!(!added.untracked());
}

#[test]
fn a_path_staged_then_edited_reports_both_sides() {
    let (directory, repository) = repository();
    write_file(directory.path(), "main.orna", "module staged;\n");
    git_ok(directory.path(), &["add", "main.orna"]);
    write_file(directory.path(), "main.orna", "module edited again;\n");
    let entries = listed(&repository);
    let both = entry(&entries, "main.orna");
    assert_eq!(both.index_status(), Some(StatusChange::Modify));
    assert_eq!(both.worktree_status(), Some(StatusChange::Modify));
}

#[test]
fn a_staged_rename_is_one_entry_at_its_new_path() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["mv", "main.orna", "renamed.orna"]);
    let entries = listed(&repository);
    assert_eq!(entries.len(), 1, "the origin of a rename is not a second entry");
    let renamed = entry(&entries, "renamed.orna");
    assert_eq!(renamed.index_status(), Some(StatusChange::Rename));
    assert_eq!(renamed.worktree_status(), None);
}

#[test]
fn an_unstaged_deletion_is_a_worktree_delete() {
    let (directory, repository) = repository();
    fs::remove_file(directory.path().join("main.orna")).expect("delete a tracked file");
    let entries = listed(&repository);
    let deleted = entry(&entries, "main.orna");
    assert_eq!(deleted.index_status(), None);
    assert_eq!(deleted.worktree_status(), Some(StatusChange::Delete));
}

#[test]
fn an_ignored_path_has_no_change_on_either_side() {
    let (directory, repository) = repository();
    write_file(directory.path(), ".gitignore", "*.log\n");
    git_ok(directory.path(), &["add", ".gitignore"]);
    git_ok(directory.path(), &["commit", "-m", "ignore logs"]);
    write_file(directory.path(), "build.log", "output\n");
    let entries = listed(&repository);
    let ignored = entry(&entries, "build.log");
    assert!(ignored.ignored());
    assert!(!ignored.untracked());
    assert_eq!(ignored.index_status(), None);
    assert_eq!(ignored.worktree_status(), None);
}

#[test]
fn entries_are_listed_in_path_order() {
    let (directory, repository) = repository();
    write_file(directory.path(), "zulu.txt", "z\n");
    write_file(directory.path(), "alpha.txt", "a\n");
    let paths: Vec<String> = listed(&repository)
        .iter()
        .map(|entry| entry.path().to_owned())
        .collect();
    assert_eq!(paths, ["alpha.txt", "zulu.txt"]);
}

#[test]
fn a_listing_past_its_bound_is_refused_not_truncated() {
    let (directory, repository) = repository();
    write_file(directory.path(), "a.txt", "a\n");
    write_file(directory.path(), "b.txt", "b\n");
    assert!(repository.worktree_status_entries(1).is_err());
    assert_eq!(
        repository
            .worktree_status_entries(2)
            .expect("a listing")
            .len(),
        2
    );
}

#[test]
fn a_detached_head_is_observed_like_a_branch() {
    let (directory, repository) = repository();
    git_ok(directory.path(), &["checkout", "--detach"]);
    write_file(directory.path(), "notes.txt", "scratch\n");
    let entries = listed(&repository);
    assert!(entry(&entries, "notes.txt").untracked());
}

#[test]
fn facts_name_the_git_directory_worktree_and_object_format() {
    let (directory, repository) = repository();
    let root = fs::canonicalize(directory.path()).expect("canonical root");
    let facts = repository.git_repository_facts().expect("repository facts");
    assert_eq!(facts.git_dir(), root.join(".git").as_path());
    assert_eq!(facts.worktree(), Some(root.as_path()));
    assert!(!facts.bare());
    assert_eq!(facts.object_format(), GitHashAlgorithm::Sha1);
    assert!(!facts.partial_clone());
}

#[test]
fn a_linked_worktree_reports_its_own_administrative_directory() {
    let (directory, _repository) = repository();
    let parent = tempfile::tempdir().expect("linked worktree parent");
    let linked = parent.path().join("linked");
    let linked_text = linked.to_str().expect("a UTF-8 path");
    git_ok(
        directory.path(),
        &["worktree", "add", "--detach", linked_text],
    );
    let linked_repository = Repository::discover(&linked).expect("a linked Git worktree");
    let root = fs::canonicalize(directory.path()).expect("canonical root");
    let linked_root = fs::canonicalize(&linked).expect("canonical linked root");
    let facts = linked_repository
        .git_repository_facts()
        .expect("repository facts");
    assert_eq!(facts.git_dir(), root.join(".git/worktrees/linked").as_path());
    assert_eq!(facts.worktree(), Some(linked_root.as_path()));
}
