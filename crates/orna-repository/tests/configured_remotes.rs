//! `Repository::configured_remotes` lists the remotes a real repository
//! configures: their URLs with credentials redacted and their promisor flag.

#[path = "../src/test_support.rs"]
mod test_support;

use std::io::Write as _;

use orna_repository::{ConfiguredRemote, REDACTED_CREDENTIALS, Repository, RepositoryError};
use tempfile::TempDir;
use test_support::{commit_initial_module, git_ok};

fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("repository directory");
    test_support::initialize_fixture_repository(directory.path());
    commit_initial_module(directory.path());
    let repository = Repository::discover(directory.path()).expect("a Git worktree");
    (directory, repository)
}

fn names(remotes: &[ConfiguredRemote]) -> Vec<&str> {
    remotes.iter().map(ConfiguredRemote::name).collect()
}

fn remote<'a>(remotes: &'a [ConfiguredRemote], name: &str) -> &'a ConfiguredRemote {
    remotes
        .iter()
        .find(|remote| remote.name() == name)
        .unwrap_or_else(|| panic!("no remote {name}"))
}

#[test]
fn lists_remotes_in_name_order_with_their_urls() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/org/repo.git",
        ],
    );
    git_ok(
        root,
        &["remote", "add", "fork", "git@example.invalid:me/repo.git"],
    );
    git_ok(
        root,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://mirror.example.invalid/org/repo.git",
        ],
    );
    git_ok(
        root,
        &[
            "config",
            "remote.fork.pushurl",
            "ssh://example.invalid/me/push.git",
        ],
    );

    let remotes = repository.configured_remotes(16).expect("a listing");
    assert_eq!(names(&remotes), ["fork", "origin"]);

    let origin = remote(&remotes, "origin");
    assert_eq!(
        origin.fetch_urls(),
        [
            "https://example.invalid/org/repo.git",
            "https://mirror.example.invalid/org/repo.git"
        ]
    );
    assert_eq!(
        origin.push_urls(),
        origin.fetch_urls(),
        "no pushurl: a push goes to the urls"
    );
    assert!(!origin.is_promisor());

    let fork = remote(&remotes, "fork");
    assert_eq!(fork.fetch_urls(), ["git@example.invalid:me/repo.git"]);
    assert_eq!(fork.push_urls(), ["ssh://example.invalid/me/push.git"]);
}

#[test]
fn credentials_never_reach_the_listing() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    git_ok(
        root,
        &[
            "config",
            "remote.origin.pushurl",
            "https://ghp_token123@example.invalid/push.git",
        ],
    );

    let remotes = repository.configured_remotes(16).expect("a listing");
    let origin = remote(&remotes, "origin");
    assert_eq!(
        origin.fetch_urls(),
        [format!(
            "https://{REDACTED_CREDENTIALS}@example.invalid/private.git"
        )]
    );
    assert_eq!(
        origin.push_urls(),
        [format!(
            "https://{REDACTED_CREDENTIALS}@example.invalid/push.git"
        )]
    );
    let shown = format!("{remotes:?}");
    for secret in ["secret", "account", "ghp_token123"] {
        assert!(
            !shown.contains(secret),
            "{secret} reached the listing: {shown}"
        );
    }
}

#[test]
fn a_remote_is_a_promisor_when_it_says_so_or_the_partial_clone_names_it() {
    let (directory, repository) = repository();
    let root = directory.path();
    git_ok(
        root,
        &["remote", "add", "plain", "https://a.example.invalid/x.git"],
    );
    git_ok(
        root,
        &[
            "remote",
            "add",
            "flagged",
            "https://b.example.invalid/x.git",
        ],
    );
    git_ok(
        root,
        &["remote", "add", "named", "https://c.example.invalid/x.git"],
    );
    git_ok(root, &["config", "remote.flagged.promisor", "true"]);
    git_ok(root, &["config", "extensions.partialClone", "named"]);

    let remotes = repository.configured_remotes(16).expect("a listing");
    assert!(!remote(&remotes, "plain").is_promisor());
    assert!(remote(&remotes, "flagged").is_promisor());
    assert!(remote(&remotes, "named").is_promisor());

    git_ok(root, &["config", "remote.flagged.promisor", "false"]);
    let remotes = repository.configured_remotes(16).expect("a listing");
    assert!(!remote(&remotes, "flagged").is_promisor());
}

#[test]
fn a_repository_with_no_remote_has_none() {
    let (_directory, repository) = repository();
    assert!(
        repository
            .configured_remotes(16)
            .expect("a listing")
            .is_empty()
    );
}

#[test]
fn a_remote_with_no_url_is_still_a_remote() {
    let (directory, repository) = repository();
    git_ok(
        directory.path(),
        &[
            "config",
            "remote.stub.fetch",
            "+refs/heads/*:refs/remotes/stub/*",
        ],
    );
    let remotes = repository.configured_remotes(16).expect("a listing");
    assert_eq!(names(&remotes), ["stub"]);
    assert!(remotes[0].fetch_urls().is_empty());
    assert!(remotes[0].push_urls().is_empty());
}

#[test]
fn a_remote_name_may_hold_dots() {
    let (directory, repository) = repository();
    git_ok(
        directory.path(),
        &[
            "config",
            "remote.team.mirror.url",
            "https://example.invalid/m.git",
        ],
    );
    let remotes = repository.configured_remotes(16).expect("a listing");
    assert_eq!(names(&remotes), ["team.mirror"]);
    assert_eq!(remotes[0].fetch_urls(), ["https://example.invalid/m.git"]);
}

#[test]
fn configuration_the_reader_does_not_understand_is_refused() {
    let (directory, repository) = repository();
    git_ok(
        directory.path(),
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(directory.path().join(".git/config"))
        .expect("the repository configuration");
    config
        .write_all(b"[remote \"bare\"]\n\turl\n")
        .expect("a url with no value");
    drop(config);
    assert!(matches!(
        repository.configured_remotes(16),
        Err(RepositoryError::GitOperationFailed)
    ));
}

#[test]
fn more_remotes_than_the_bound_are_refused_not_truncated() {
    let (directory, repository) = repository();
    git_ok(
        directory.path(),
        &["remote", "add", "one", "https://a.invalid/"],
    );
    git_ok(
        directory.path(),
        &["remote", "add", "two", "https://b.invalid/"],
    );
    assert_eq!(
        repository
            .configured_remotes(2)
            .expect("at the bound")
            .len(),
        2
    );
    assert!(matches!(
        repository.configured_remotes(1),
        Err(RepositoryError::GitOperationFailed)
    ));
}
