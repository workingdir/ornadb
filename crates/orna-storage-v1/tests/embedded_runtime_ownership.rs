use orna_repository_v1::Repository;
use orna_runtime_v1::{
    FaultInjector, FaultPoint, NoFault, RuntimeError, RuntimeIdentity, RuntimeState,
    TableMutation,
};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    git(temp.path(), &["config", "user.email", "test@example.invalid"]);
    git(temp.path(), &["config", "user.name", "Orna test"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).unwrap();
    (temp, repository)
}

#[derive(Debug)]
struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

/// ORNA-TURSO-001: opening a worktree creates a local SQLite-compatible state database without a server.
/// ORNA-TURSO-002: a failed row/checkpoint transaction rolls back both state changes atomically.
/// ORNA-TURSO-003: WAL mode is exercised by the runtime transaction; schema setup
/// documents FULL sync and the limits of its durability guarantees.
#[tokio::test]
async fn local_embedded_state_rolls_back_row_and_checkpoint_together() {
    let (_temp, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    };
    let owner_id = [3; 16];
    let runtime = RuntimeState::open_with_owner_id(&repository, identity, [4; 32], owner_id)
        .await
        .unwrap();
    let state_db = repository.runtime_paths().state_db();
    assert!(state_db.starts_with(repository.runtime_paths().root()));

    let lease = runtime.acquire_lease(owner_id).await.unwrap();
    let before = runtime.capture().await.unwrap();
    let context = runtime.begin_activation().await.unwrap();
    let mutation = TableMutation::new(
        [5; 16],
        "Contact",
        b"alice".to_vec(),
        Some(b"Alice".to_vec()),
    )
    .unwrap();

    assert_eq!(
        runtime
            .commit_table_activation(
                lease,
                &context,
                std::slice::from_ref(&mutation),
                [6; 32],
                &FailAt(FaultPoint::AfterCheckpoint),
            )
            .await,
        Err(RuntimeError::FaultInjected(FaultPoint::AfterCheckpoint))
    );
    assert_eq!(runtime.capture().await.unwrap(), before);
    assert_eq!(
        runtime.committed_table_row("Contact", b"alice").await.unwrap(),
        None
    );
    assert!(runtime.pending().await.unwrap().is_empty());

    let committed = runtime
        .commit_table_activation(
            lease,
            &context,
            &[mutation],
            [7; 32],
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.generation().to_string(), "1");
    assert_eq!(
        runtime.committed_table_row("Contact", b"alice").await.unwrap(),
        Some(b"Alice".to_vec())
    );
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert!(state_db.with_file_name("state.db-wal").is_file());
}

/// ORNA-TURSO-004: one worktree runtime owns the local owner lock; a second owner is a secondary participant.
/// ORNA-TURSO-005: a second local writer with a different owner identity fails to acquire the writer lease.
/// ORNA-TURSO-006: replacement ownership increments the lease fence and rejects writes from the stale owner.
#[tokio::test]
async fn replacement_runtime_fences_stale_writer_before_accepting_mutations() {
    let (_temp, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [11; 16],
        repository_id: [12; 16],
    };
    let old_owner_id = [13; 16];
    let new_owner_id = [14; 16];
    let old_owner = RuntimeState::open_with_owner_id(&repository, identity, [15; 32], old_owner_id)
        .await
        .unwrap();
    let secondary =
        RuntimeState::open_with_owner_id(&repository, identity, [15; 32], new_owner_id)
            .await
            .unwrap();
    assert!(old_owner.holds_runtime_owner_lock());
    assert!(!secondary.holds_runtime_owner_lock());

    let stale_lease = old_owner.acquire_lease(old_owner_id).await.unwrap();
    assert_eq!(
        secondary.acquire_lease(new_owner_id).await,
        Err(RuntimeError::LeaseHeld)
    );
    drop(old_owner);

    let replacement =
        RuntimeState::open_with_owner_id(&repository, identity, [15; 32], new_owner_id)
            .await
            .unwrap();
    assert!(replacement.holds_runtime_owner_lock());
    let replacement_lease = replacement
        .takeover_lease(stale_lease, new_owner_id)
        .await
        .unwrap();
    assert_eq!(replacement_lease.epoch, stale_lease.epoch + 1);

    let mutation = TableMutation::new(
        [16; 16],
        "Contact",
        b"alice".to_vec(),
        Some(b"Alice".to_vec()),
    )
    .unwrap();
    let stale_context = secondary.begin_activation().await.unwrap();
    assert_eq!(
        secondary
            .commit_table_activation(
                stale_lease,
                &stale_context,
                std::slice::from_ref(&mutation),
                [17; 32],
                &NoFault,
            )
            .await,
        Err(RuntimeError::OwnerLost)
    );
    assert_eq!(
        secondary.committed_table_row("Contact", b"alice").await.unwrap(),
        None
    );

    let context = replacement.begin_activation().await.unwrap();
    replacement
        .commit_table_activation(
            replacement_lease,
            &context,
            &[mutation],
            [18; 32],
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(
        replacement.committed_table_row("Contact", b"alice").await.unwrap(),
        Some(b"Alice".to_vec())
    );
}
