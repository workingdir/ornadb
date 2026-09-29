use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{Mutation, NoFault, RuntimeIdentity, RuntimeState, WriterLease};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const MAIN_SOURCE: &str = include_str!("fixtures/publication-repository-main.orna");

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("Git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("temporary Git repository");
    git(directory.path(), &["init", "--quiet", "--initial-branch=main"]);
    git(directory.path(), &["config", "user.name", "Runtime Test"]);
    git(
        directory.path(),
        &["config", "user.email", "runtime-test@example.invalid"],
    );
    fs::write(directory.path().join("main.orna"), MAIN_SOURCE)
        .expect("write checked-in Orna source fixture");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "fixture"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn identity(repository_id: u8) -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [repository_id; 16],
    }
}

async fn open_runtime(repository: &Repository, repository_id: u8) -> RuntimeState {
    RuntimeState::open(repository, identity(repository_id), [2; 32])
        .await
        .expect("open per-repository runtime state")
}

async fn append_fixture(state: &RuntimeState, lease: WriterLease, mutation_id: u8) {
    let payload = MAIN_SOURCE.as_bytes().to_vec();
    let mutation = Mutation {
        id: [mutation_id; 16],
        digest: Sha256::digest(&payload).into(),
        payload,
    };
    let capture = state.capture().await.expect("capture CWD");
    state
        .commit(lease, &capture, &mutation, [mutation_id; 32], &NoFault)
        .await
        .expect("append durable fixture mutation");
}

#[tokio::test]
async fn runtime_state_opens_under_git_resolved_worktree_administration() {
    let (root, repository) = repository();
    let state = open_runtime(&repository, 3).await;
    let git_path = git(
        root.path(),
        &["rev-parse", "--path-format=absolute", "--git-path", "orna"],
    );

    assert_eq!(repository.runtime_paths().root(), Path::new(&git_path));
    assert!(repository.runtime_paths().state_db().is_file());
    assert!(!repository
        .runtime_paths()
        .root()
        .starts_with(root.path().join(".orna")));
    drop(state);
}

#[tokio::test]
async fn linked_worktrees_keep_separate_runtime_tails() {
    let (root, main_repository) = repository();
    let linked_parent = tempfile::tempdir().expect("linked worktree parent");
    let linked_path = linked_parent.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked_path.to_str().expect("UTF-8 temp path"),
        ],
    );
    let linked_repository = Repository::discover(&linked_path).expect("discover linked worktree");
    let main_state = open_runtime(&main_repository, 3).await;
    let linked_state = open_runtime(&linked_repository, 3).await;

    assert_ne!(
        main_repository.runtime_paths().state_db(),
        linked_repository.runtime_paths().state_db()
    );
    let main_lease = main_state
        .acquire_lease([4; 16])
        .await
        .expect("acquire main worktree writer");
    append_fixture(&main_state, main_lease, 5).await;
    assert_eq!(main_state.pending_count().await.unwrap(), 1);
    assert_eq!(linked_state.pending_count().await.unwrap(), 0);

    let linked_lease = linked_state
        .acquire_lease([6; 16])
        .await
        .expect("acquire linked worktree writer");
    append_fixture(&linked_state, linked_lease, 7).await;
    assert_eq!(main_state.pending_count().await.unwrap(), 1);
    assert_eq!(linked_state.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn cloned_repositories_keep_independent_runtime_cwds() {
    let (root, main_repository) = repository();
    let clone_parent = tempfile::tempdir().expect("clone parent");
    let clone_path = clone_parent.path().join("clone");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            root.path().to_str().expect("UTF-8 source path"),
            clone_path.to_str().expect("UTF-8 clone path"),
        ],
    );
    let clone_repository = Repository::discover(&clone_path).expect("discover clone");
    let main_state = open_runtime(&main_repository, 8).await;
    let clone_state = open_runtime(&clone_repository, 9).await;
    let main_lease = main_state
        .acquire_lease([10; 16])
        .await
        .expect("acquire main clone writer");
    append_fixture(&main_state, main_lease, 11).await;

    assert_eq!(main_state.pending_count().await.unwrap(), 1);
    assert_eq!(clone_state.pending_count().await.unwrap(), 0);

    let clone_lease = clone_state
        .acquire_lease([12; 16])
        .await
        .expect("acquire receiving clone writer");
    append_fixture(&clone_state, clone_lease, 13).await;
    assert_eq!(main_state.pending_count().await.unwrap(), 1);
    assert_eq!(clone_state.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn unpublished_runtime_tail_is_absent_from_ordinary_git_state() {
    let (root, repository) = repository();
    let state = open_runtime(&repository, 3).await;
    let lease = state
        .acquire_lease([14; 16])
        .await
        .expect("acquire writer");
    append_fixture(&state, lease, 15).await;

    assert_eq!(git(root.path(), &["status", "--porcelain=v1"]), "");
    assert_eq!(git(root.path(), &["ls-files"]), "main.orna");
    assert_eq!(state.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn durable_unpublished_tail_survives_runtime_reopen() {
    let (_root, repository) = repository();
    let state = open_runtime(&repository, 3).await;
    let lease = state
        .acquire_lease([16; 16])
        .await
        .expect("acquire writer");
    append_fixture(&state, lease, 17).await;
    let pending_bytes = state
        .publication_metadata()
        .await
        .expect("read pending metadata")
        .pending_bytes;
    assert_eq!(state.pending_count().await.unwrap(), 1);
    drop(state);

    let reopened = open_runtime(&repository, 3).await;
    assert_eq!(reopened.pending_count().await.unwrap(), 1);
    assert_eq!(
        reopened
            .publication_metadata()
            .await
            .expect("read reopened pending metadata")
            .pending_bytes,
        pending_bytes
    );
}
