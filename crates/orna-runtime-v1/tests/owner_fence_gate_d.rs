//! Gate D owner fence (ORNA-TURSO-004/005/006) under real process death.
//!
//! A child process takes the production fenced writer lease while holding the
//! worktree runtime owner lock, commits a row, and then blocks. The parent
//! proves that a live owner excludes a second writer, SIGKILLs the child, and
//! then fences it: the dead owner's token must be rejected with `OwnerLost` and
//! must land no write, while the replacement owner can commit.

use std::{
    env, fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{NoFault, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation};
use tempfile::{Builder, TempDir};

const CHILD_REPOSITORY: &str = "ORNA_OWNER_FENCE_CHILD_REPOSITORY";
const CHILD_MARKER: &str = "ORNA_OWNER_FENCE_CHILD_MARKER";
const CHILD_TEST: &str = "owner_fence_child_holds_writer_lease";
const SIGKILL: i32 = 9;

const CHILD_OWNER: [u8; 16] = [0xc1; 16];
const PARENT_OWNER: [u8; 16] = [0xd2; 16];
const INITIAL_DIGEST: [u8; 32] = [3; 32];

/// Row key written by the child before it is killed.
const CHILD_ROW: &[u8] = &[1];
/// Row key the dead owner attempts after being fenced.
const DEAD_OWNER_ROW: &[u8] = &[2];
/// Row key written by the replacement owner.
const NEW_OWNER_ROW: &[u8] = &[3];

fn repository() -> (TempDir, Repository) {
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-owner-fence-")
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
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/durable_crash_recovery_main.orna"),
    )
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
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
}

fn insert(id: [u8; 16], row_key: &[u8], value: u8) -> TableMutation {
    TableMutation::insert(id, "records", row_key.to_vec(), vec![value])
        .expect("valid typed table mutation")
}

/// The owner that gets killed. It only runs when the parent re-executes this
/// binary with `CHILD_REPOSITORY` set; it acquires the production fenced writer
/// lease, commits one row, signals the parent, then blocks until SIGKILL.
#[test]
fn owner_fence_child_holds_writer_lease() {
    let Some(repository_path) = env::var_os(CHILD_REPOSITORY) else {
        return;
    };
    let marker = PathBuf::from(env::var_os(CHILD_MARKER).expect("child marker path"));
    let repository =
        Repository::discover(PathBuf::from(repository_path)).expect("discover child repository");
    runtime().block_on(async {
        let state =
            RuntimeState::open_with_owner_id(&repository, identity(), INITIAL_DIGEST, CHILD_OWNER)
                .await
                .expect("open child runtime");
        assert!(
            state.holds_runtime_owner_lock(),
            "child must own the worktree runtime lock"
        );
        let lease = state
            .acquire_fenced_writer_lease(CHILD_OWNER)
            .await
            .expect("child acquires the writer lease");
        let context = state.begin_activation().await.expect("child capture");
        state
            .commit_table_activation(
                lease,
                &context,
                &[insert([7; 16], CHILD_ROW, 9)],
                [8; 32],
                &NoFault,
            )
            .await
            .expect("child commits its row under the lease");
        fs::write(&marker, b"committed").expect("signal parent");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    });
}

#[test]
fn killed_writer_is_fenced_and_cannot_land_a_write() {
    let (directory, repository) = repository();
    let marker = directory.path().join("owner-fence-child-marker");
    let binary = env::current_exe().expect("resolve test binary");
    let mut child: Child = Command::new(binary)
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_REPOSITORY, directory.path())
        .env(CHILD_MARKER, &marker)
        .spawn()
        .expect("spawn owner child");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never committed under its lease: {:?}",
        child.try_wait()
    );

    runtime().block_on(async {
        // While the child is alive it owns the runtime lock and the lease. A
        // second participant must not become a writer.
        let secondary = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            PARENT_OWNER,
        )
        .await
        .expect("secondary runtime opens while owner is alive");
        assert!(
            !secondary.holds_runtime_owner_lock(),
            "a live owner keeps the runtime owner lock"
        );
        assert!(
            matches!(
                secondary.acquire_fenced_writer_lease(PARENT_OWNER).await,
                Err(RuntimeError::LeaseHeld)
            ),
            "a secondary participant must not take the writer lease from a live owner"
        );
        assert_eq!(
            secondary.current_lease().await.unwrap().map(|lease| lease.owner_id),
            Some(CHILD_OWNER),
            "the live owner's lease must be untouched by the secondary"
        );
        drop(secondary);
    });

    child.kill().expect("SIGKILL owner child");
    let status = child.wait().expect("reap killed owner");
    assert_eq!(status.signal(), Some(SIGKILL), "owner must die by SIGKILL");

    runtime().block_on(async {
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            PARENT_OWNER,
        )
        .await
        .expect("reopen after owner death");
        assert!(
            state.holds_runtime_owner_lock(),
            "the dead owner must release the runtime lock"
        );
        let dead = state
            .current_lease()
            .await
            .unwrap()
            .expect("dead owner lease");
        assert_eq!(dead.owner_id, CHILD_OWNER);
        assert_eq!(dead.epoch, 1);

        let fresh = state
            .acquire_fenced_writer_lease(PARENT_OWNER)
            .await
            .expect("replacement owner fences the dead lease");
        assert_eq!(fresh.owner_id, PARENT_OWNER);
        assert_eq!(fresh.epoch, dead.epoch + 1);
        assert_eq!(state.current_lease().await.unwrap(), Some(fresh));

        // The dead owner's token is stale: its write is rejected and lands nothing.
        let context = state.begin_activation().await.expect("capture after fence");
        let rejected = state
            .commit_table_activation(
                dead,
                &context,
                &[insert([11; 16], DEAD_OWNER_ROW, 1)],
                [10; 32],
                &NoFault,
            )
            .await;
        assert!(
            matches!(rejected, Err(RuntimeError::OwnerLost)),
            "dead owner token must be rejected with OwnerLost, got {rejected:?}"
        );
        assert_eq!(
            state.committed_table_row("records", DEAD_OWNER_ROW).await.unwrap(),
            None,
            "a fenced owner's write must not land"
        );
        assert_eq!(
            state.committed_table_row("records", CHILD_ROW).await.unwrap(),
            Some(vec![9]),
            "the row acknowledged before the kill must survive fencing"
        );

        // The replacement owner writes under its fenced lease.
        let context = state.begin_activation().await.expect("capture for new owner");
        state
            .commit_table_activation(
                fresh,
                &context,
                &[insert([12; 16], NEW_OWNER_ROW, 2)],
                [9; 32],
                &NoFault,
            )
            .await
            .expect("replacement owner commits under the fenced lease");
        assert_eq!(
            state.committed_table_row("records", NEW_OWNER_ROW).await.unwrap(),
            Some(vec![2])
        );
    });
}
