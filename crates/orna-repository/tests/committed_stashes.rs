//! `Repository::committed_stashes` lists the stash entries of a real repository
//! as `git stash list` does: newest first, each with the stash commit, its
//! message and when it was made.

#[path = "../src/test_support.rs"]
mod test_support;

use std::path::Path;
use std::process::Command;

use orna_repository::{CommittedStash, Repository, RepositoryError};
use tempfile::TempDir;
use test_support::{commit_initial_module, git, git_ok};

/// A repository on `main` with one commit.
fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

/// Changes the tracked file so there is something to stash, then stashes it
/// under `message` (`None`: the message Git writes itself).
fn stash(root: &Path, round: usize, message: Option<&str>) {
    std::fs::write(root.join("main.orna"), format!("module main; // {round}\n"))
        .expect("a change to stash");
    match message {
        Some(message) => git_ok(root, &["stash", "push", "-m", message]),
        None => git_ok(root, &["stash", "push"]),
    }
}

/// As [`stash`], with the committer date of the stash commit fixed.
fn stash_dated(root: &Path, round: usize, message: &str, committer_date: &str) {
    std::fs::write(root.join("main.orna"), format!("module main; // {round}\n"))
        .expect("a change to stash");
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_COMMITTER_DATE", committer_date)
        .args(["stash", "push", "-m", message])
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git stash push: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn messages(stashes: &[CommittedStash]) -> Vec<&str> {
    stashes.iter().map(CommittedStash::message).collect()
}

#[test]
fn lists_the_entries_newest_first_with_their_commit_message_and_time() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("first stash"));
    stash(root, 2, None);
    stash(root, 3, Some("third stash"));

    let stashes = repository.committed_stashes(16).expect("a listing");
    assert_eq!(stashes.len(), 3);
    for (position, stash) in stashes.iter().enumerate() {
        assert_eq!(stash.index(), position as u64);
        let selector = format!("stash@{{{position}}}");
        assert_eq!(
            stash.commit().as_str(),
            git(root, &["rev-parse", &selector]),
            "{selector}"
        );
        assert_eq!(
            stash.created_unix_seconds().to_string(),
            git(root, &["log", "-1", "--format=%ct", &selector]),
            "{selector}"
        );
    }
    let default_message = format!(
        "WIP on main: {} initial",
        git(root, &["rev-parse", "--short", "HEAD"])
    );
    assert_eq!(
        messages(&stashes),
        [
            "On main: third stash",
            default_message.as_str(),
            "On main: first stash"
        ]
    );
}

#[test]
fn the_message_is_what_git_stash_list_prints() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("kept"));
    stash(root, 2, None);
    let listed = git(root, &["stash", "list"]);
    let expected = listed
        .lines()
        .map(|line| line.split_once(": ").expect("a selector").1.to_owned())
        .collect::<Vec<_>>();
    let stashes = repository.committed_stashes(16).expect("a listing");
    assert_eq!(messages(&stashes), expected);
}

#[test]
fn a_message_of_several_lines_reads_as_git_flattened_it() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("first line\nsecond line"));
    let stashes = repository.committed_stashes(16).expect("a listing");
    assert_eq!(messages(&stashes), ["On main: first line second line"]);
}

#[test]
fn the_time_is_the_committer_date_of_the_stash_commit() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash_dated(root, 1, "dated", "2001-02-03T04:05:06Z");
    stash_dated(root, 2, "later", "2003-04-05T06:07:08Z");
    let stashes = repository.committed_stashes(16).expect("a listing");
    assert_eq!(stashes[1].created_unix_seconds(), 981_173_106);
    assert_eq!(stashes[0].created_unix_seconds(), 1_049_522_828);
}

#[test]
fn dropping_an_entry_renumbers_the_entries_after_it() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("one"));
    stash(root, 2, Some("two"));
    stash(root, 3, Some("three"));
    git_ok(root, &["stash", "drop", "stash@{1}"]);

    let stashes = repository.committed_stashes(16).expect("a listing");
    let seen = stashes
        .iter()
        .map(|stash| (stash.index(), stash.message().to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        seen,
        [
            (0, "On main: three".to_owned()),
            (1, "On main: one".to_owned())
        ]
    );
}

#[test]
fn a_repository_with_no_stash_has_none() {
    let (_directory, repository) = repository();
    assert!(
        repository
            .committed_stashes(16)
            .expect("a listing")
            .is_empty()
    );

    let empty = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(empty.path());
    let repository = Repository::discover(empty.path()).expect("a Git worktree");
    assert!(
        repository
            .committed_stashes(16)
            .expect("a listing")
            .is_empty(),
        "a repository with no commit has no stash"
    );
}

#[test]
fn a_dropped_last_entry_leaves_no_stash() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("only"));
    git_ok(root, &["stash", "drop"]);
    assert!(
        repository
            .committed_stashes(16)
            .expect("a listing")
            .is_empty()
    );
}

#[test]
fn configured_log_options_do_not_change_what_is_listed() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("one"));
    stash(root, 2, Some("two"));
    git_ok(root, &["config", "log.date", "relative"]);
    git_ok(root, &["config", "log.showSignature", "true"]);
    git_ok(root, &["config", "format.pretty", "oneline"]);

    let stashes = repository.committed_stashes(16).expect("a listing");
    assert_eq!(messages(&stashes), ["On main: two", "On main: one"]);
    assert_eq!(
        stashes[0].commit().as_str(),
        git(root, &["rev-parse", "stash@{0}"])
    );
}

#[test]
fn more_entries_than_the_bound_are_refused_not_truncated() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("one"));
    stash(root, 2, Some("two"));
    assert_eq!(
        repository.committed_stashes(2).expect("at the bound").len(),
        2
    );
    assert!(matches!(
        repository.committed_stashes(1),
        Err(RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repository.committed_stashes(0),
        Err(RepositoryError::GitOperationFailed)
    ));
}

#[test]
fn an_entry_whose_commit_cannot_be_read_refuses_the_listing() {
    let (directory, repository) = repository();
    let root = directory.path();
    stash(root, 1, Some("one"));
    let commit = git(root, &["rev-parse", "stash@{0}"]);
    std::fs::remove_file(
        root.join(".git/objects")
            .join(&commit[..2])
            .join(&commit[2..]),
    )
    .expect("the stash commit is a loose object");
    assert!(
        repository.committed_stashes(16).is_err(),
        "no listing is served over a stash entry that cannot be read"
    );
}
