//! Gate D2: publication-state writes below the CLI require the worktree
//! publication lease (ORNA-TURSO-005/006, PUB-3 steps 2 and 7).
//!
//! `RuntimeState::freeze`, `freeze_compact`, `bind_compact_publication`,
//! `complete_compact_publication`, `complete_publication` and
//! `set_publication_compressed_target_bytes` are private. The only way to
//! reach them is [`RuntimeState::publication_writer`], which takes the
//! worktree's publication lease and refuses one taken in another worktree.
//! These tests drive that boundary directly, not through `orna publish`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use orna_repository::Repository;
use orna_runtime::{
    NoFault, PublicationCommitId, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
    process_owner_id,
};
use tempfile::{Builder, TempDir};

const INITIAL_DIGEST: [u8; 32] = [3; 32];
const OTHER_OWNER: [u8; 16] = [0xd4; 16];
const ROW: &[u8] = &[1];

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-publication-writer-")
        .tempdir_in(target)
        .expect("create repository directory");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(directory.path(), &["config", "user.name", "kierandrewett"]);
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    std::fs::write(directory.path().join("main.orna"), "module main;\n")
        .expect("write fixture root module");
    git(directory.path(), &["add", "main.orna"]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "initial snapshot"],
    );
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    }
}

fn insert(id: [u8; 16], value: u8) -> TableMutation {
    TableMutation::insert(id, "records", ROW.to_vec(), vec![value])
        .expect("valid typed table mutation")
}

/// A publication lease is exclusive per process and bound to one worktree: a
/// second request for the same worktree fails while the first is held, and a
/// lease taken in another worktree never authorises this runtime.
#[tokio::test]
async fn publication_writer_lease_is_exclusive_and_bound_to_one_worktree() {
    let (_first_directory, first_repository) = repository();
    let (_second_directory, second_repository) = repository();
    let state = RuntimeState::open(&first_repository, identity(), INITIAL_DIGEST)
        .await
        .expect("open owner runtime");
    assert!(state.holds_runtime_owner_lock());

    let held = state
        .publication_writer(&first_repository)
        .expect("take the worktree publication lease");
    assert_eq!(
        held.lease().runtime_root(),
        first_repository.runtime_paths().root()
    );
    assert_eq!(
        state.publication_writer(&first_repository).err(),
        Some(RuntimeError::LeaseHeld),
        "a second publication writer must not take a held lease"
    );
    assert_eq!(
        state.publication_writer(&second_repository).err(),
        Some(RuntimeError::OwnerLost),
        "another worktree's publication lease must not authorise this runtime"
    );
    drop(held);
    let reacquired = state
        .publication_writer(&first_repository)
        .expect("the lease is released on drop");
    drop(reacquired);
}

/// A secondary participant cannot take the runtime writer lease, but it can
/// freeze and complete a publication while it holds the worktree publication
/// lease. That is the documented REPL-plus-publish workflow (PUB-3 step 7):
/// publication state is guarded by the publication lease, and the live runtime
/// owner keeps its lease throughout.
#[tokio::test]
async fn secondary_handle_freezes_and_completes_under_the_publication_lease() {
    let (_directory, repository) = repository();
    let owner = process_owner_id();
    let owner_state =
        RuntimeState::open_with_owner_id(&repository, identity(), INITIAL_DIGEST, owner)
            .await
            .expect("open the lock holder");
    assert!(owner_state.holds_runtime_owner_lock());
    let writer = owner_state
        .acquire_lease(owner)
        .await
        .expect("the owner acquires the writer lease");
    let context = owner_state
        .begin_activation()
        .await
        .expect("capture before the durable row");
    owner_state
        .commit_table_activation(
            writer,
            &context,
            &[insert([0x11; 16], 7)],
            [0x22; 32],
            &NoFault,
        )
        .await
        .expect("commit the durable row");
    let checkpoint = owner_state
        .latest_checkpoint()
        .await
        .expect("read the committed checkpoint")
        .expect("the commit leaves a checkpoint");

    let secondary =
        RuntimeState::open_with_owner_id(&repository, identity(), INITIAL_DIGEST, OTHER_OWNER)
            .await
            .expect("open a secondary participant");
    assert!(
        !secondary.holds_runtime_owner_lock(),
        "a live owner keeps the runtime owner lock"
    );
    assert_eq!(
        secondary.acquire_lease(OTHER_OWNER).await,
        Err(RuntimeError::LeaseHeld),
        "the secondary must not take the runtime writer lease"
    );

    let freeze = secondary
        .publication_writer(&repository)
        .expect("take the worktree publication lease")
        .freeze([0x33; 16], &checkpoint)
        .await
        .expect("freeze the committed prefix under the publication lease");
    assert_eq!(freeze.checkpoint, checkpoint);

    let commit = PublicationCommitId::new(vec![b'a'; 40]).expect("valid commit id");
    secondary
        .publication_writer(&repository)
        .expect("take the worktree publication lease")
        .complete_publication(&freeze, &commit)
        .await
        .expect("complete the publication under the publication lease");
    assert_eq!(
        secondary.pending().await.expect("read the pending tail"),
        Vec::new(),
        "completion consumes the frozen prefix"
    );

    assert_eq!(
        secondary.acquire_lease(OTHER_OWNER).await,
        Err(RuntimeError::LeaseHeld),
        "publishing must not grant the runtime writer lease"
    );
    assert!(
        owner_state.holds_runtime_owner_lock(),
        "the live runtime owner keeps its lock"
    );
}
