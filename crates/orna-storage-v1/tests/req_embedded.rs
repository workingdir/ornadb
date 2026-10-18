use orna_repository_v1::{Repository, RepositoryError};
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
    assert!(fs::read(&state_db)
        .unwrap()
        .starts_with(b"SQLite format 3\0"));

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

/// ORNA-TURSO-004: losing the response after durable owner acquisition releases the lock on
/// process exit, while preserving the owner record for safe replacement recovery.
#[cfg(target_os = "linux")]
#[test]
fn owner_lock_recovers_after_lost_acquisition_response() {
    use std::os::unix::process::ExitStatusExt;

    const REPOSITORY_ENV: &str = "ORNA_OWNER_LOST_RESPONSE_REPOSITORY";
    const OWNER_ENV: &str = "ORNA_OWNER_LOST_RESPONSE_ID";
    const TEST_FILTER: &str = "owner_lock_recovers_after_lost_acquisition_response";
    if let (Ok(path), Ok(owner)) = (std::env::var(REPOSITORY_ENV), std::env::var(OWNER_ENV)) {
        let owner = owner.parse::<u8>().unwrap();
        let repository = Repository::discover(path).unwrap();
        let _lock = repository
            .acquire_runtime_owner_lock([owner; 16])
            .unwrap();
        std::process::abort();
    }

    let (root, repository) = repository();
    let binary = std::env::current_exe().unwrap();
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg("ulimit -c 0 2>/dev/null; exec \"$0\" \"$@\"")
        .arg(binary)
        .args([TEST_FILTER, "--nocapture", "--test-threads=1"])
        .env(REPOSITORY_ENV, root.path())
        .env(OWNER_ENV, "21")
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(6),
        "owner process must die before acknowledging acquisition"
    );

    let replacement = repository.acquire_runtime_owner_lock([22; 16]).unwrap();
    assert_eq!(replacement.previous_owner(), Some([21; 16]));
    assert_eq!(replacement.departed_owners(), &[[21; 16]]);
    assert_eq!(
        replacement
            .reclaim_departed_owner_pins(&Default::default())
            .unwrap(),
        Some(0)
    );
}

/// ORNA-TURSO-004: competing writers stay exclusive while owner state is atomically renamed.
#[cfg(target_os = "linux")]
#[test]
fn owner_lock_serializes_competing_state_renames() {
    use std::{
        os::unix::fs::MetadataExt,
        sync::{Arc, Barrier},
    };

    let (root, _) = repository();
    let first_root = root.path().to_path_buf();
    let second_root = root.path().to_path_buf();
    let start = Arc::new(Barrier::new(3));

    let first_start = Arc::clone(&start);
    let first = std::thread::spawn(move || {
        let repository = Repository::discover(first_root).unwrap();
        first_start.wait();
        repository.acquire_runtime_owner_lock([31; 16])
    });
    let second_start = Arc::clone(&start);
    let second = std::thread::spawn(move || {
        let repository = Repository::discover(second_root).unwrap();
        second_start.wait();
        repository.acquire_runtime_owner_lock([32; 16])
    });
    start.wait();

    let first = first.join().unwrap();
    let second = second.join().unwrap();
    let (winner, winner_id) = match (first, second) {
        (Ok(winner), Err(RepositoryError::RepositoryBusy)) => (winner, [31; 16]),
        (Err(RepositoryError::RepositoryBusy), Ok(winner)) => (winner, [32; 16]),
        _ => panic!("exactly one racing runtime owner must acquire the lock"),
    };

    let repository = Repository::discover(root.path()).unwrap();
    let locks = repository.runtime_paths().locks();
    let lock_path = locks.join("runtime-owner.lock");
    let owner_state_path = locks.join("runtime-owner.state");
    let lock_inode = fs::metadata(&lock_path).unwrap().ino();
    let state_inode = fs::metadata(&owner_state_path).unwrap().ino();
    drop(winner);

    let replacement_id = if winner_id == [31; 16] { [32; 16] } else { [31; 16] };
    let replacement = repository
        .acquire_runtime_owner_lock(replacement_id)
        .unwrap();
    assert_eq!(replacement.previous_owner(), Some(winner_id));
    assert_eq!(fs::metadata(lock_path).unwrap().ino(), lock_inode);
    assert_ne!(fs::metadata(owner_state_path).unwrap().ino(), state_inode);
}
