//! Gate D owner fence (ORNA-TURSO-004/005/006) under real process death.
//!
//! Gate D clause "no abandoned owner retains write permission" is defined here
//! by the existing lease semantics: a writer is its lease token `(owner, epoch)`.
//! Every CLI process acquires the lease under its own per-process owner ID
//! (`process_owner_id`). A child process acquires the lease while holding the
//! runtime owner lock, prints its `WriterLease` token, then blocks until it is
//! SIGKILLed. A replacement process that takes the runtime owner lock acquires
//! the lease, which takes over the departed token with a bumped epoch. The dead
//! owner's token must then be rejected with `OwnerLost` and land nothing, while
//! the replacement owner can commit.
//!
//! This test does not claim that a live writer is fenced from outside its
//! process; that remains a gap (see the filed bead for Gate D).

use std::{
    env, fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
    WriterLease, process_owner_id,
};
use tempfile::{Builder, TempDir};

const CHILD_REPOSITORY: &str = "ORNA_OWNER_FENCE_CHILD_REPOSITORY";
const CHILD_MARKER: &str = "ORNA_OWNER_FENCE_CHILD_MARKER";
const LEASE_CHILD_TEST: &str = "owner_fence_child_holds_writer_lease";
const RUNNING_CHILD_TEST: &str = "owner_fence_child_leaves_running_request";
const SIGKILL: i32 = 9;

/// The repository identity is shared by every writer of one worktree. Writer
/// ownership is per process (`process_owner_id`), not this value.
const REPOSITORY_ID: [u8; 16] = [2; 16];
/// A second, distinct writer that must never take a live owner's lease.
const OTHER_OWNER: [u8; 16] = [0xd2; 16];
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
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [1; 16],
        repository_id: REPOSITORY_ID,
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

fn spawn_child(test_name: &str, directory: &Path, marker: &Path) -> Child {
    let binary = env::current_exe().expect("resolve test binary");
    Command::new(binary)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD_REPOSITORY, directory)
        .env(CHILD_MARKER, marker)
        .spawn()
        .expect("spawn owner child")
}

fn wait_for_marker(marker: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never signalled the parent: {:?}",
        child.try_wait()
    );
}

fn kill_and_reap(child: &mut Child) {
    child.kill().expect("SIGKILL owner child");
    let status = child.wait().expect("reap killed owner");
    assert_eq!(status.signal(), Some(SIGKILL), "owner must die by SIGKILL");
}

fn encode_lease(lease: WriterLease) -> String {
    let owner: String = lease.owner_id.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("owner={owner} epoch={}", lease.epoch)
}

fn decode_lease(text: &str) -> WriterLease {
    let mut owner_id = [0; 16];
    let mut epoch = None;
    for field in text.split_whitespace() {
        if let Some(hex) = field.strip_prefix("owner=") {
            for (index, slot) in owner_id.iter_mut().enumerate() {
                *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                    .expect("hex owner byte");
            }
        } else if let Some(value) = field.strip_prefix("epoch=") {
            epoch = Some(value.parse().expect("decimal epoch"));
        }
    }
    WriterLease {
        owner_id,
        epoch: epoch.expect("epoch field"),
    }
}

/// The owner that gets killed. It only runs when the parent re-executes this
/// binary with `CHILD_REPOSITORY` set; it acquires the writer lease under its
/// process owner, commits one row, prints its `WriterLease` token, signals the
/// parent, then blocks until SIGKILL.
#[test]
fn owner_fence_child_holds_writer_lease() {
    let Some(repository_path) = env::var_os(CHILD_REPOSITORY) else {
        return;
    };
    let marker = PathBuf::from(env::var_os(CHILD_MARKER).expect("child marker path"));
    let repository =
        Repository::discover(PathBuf::from(repository_path)).expect("discover child repository");
    runtime().block_on(async {
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            process_owner_id(),
        )
        .await
        .expect("open child runtime");
        assert!(
            state.holds_runtime_owner_lock(),
            "child must own the worktree runtime lock"
        );
        let lease = state
            .acquire_lease(process_owner_id())
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
        let token = encode_lease(lease);
        println!("WriterLease {token}");
        fs::write(&marker, token.as_bytes()).expect("signal parent with the lease token");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    });
}

/// The owner that gets killed while a request is Running. It leaves a durable
/// `Running` request-ledger row under its process owner, as an `orna run`
/// killed mid-invocation would.
#[test]
fn owner_fence_child_leaves_running_request() {
    let Some(repository_path) = env::var_os(CHILD_REPOSITORY) else {
        return;
    };
    let marker = PathBuf::from(env::var_os(CHILD_MARKER).expect("child marker path"));
    let repository =
        Repository::discover(PathBuf::from(repository_path)).expect("discover child repository");
    runtime().block_on(async {
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            process_owner_id(),
        )
        .await
        .expect("open child runtime");
        let lease = state
            .acquire_lease(process_owner_id())
            .await
            .expect("child acquires the writer lease");
        let (_, capability) = state
            .reserve_request_with_admission(request(), [4; 32])
            .await
            .expect("child reserves the request");
        let capability = capability.expect("fresh owner-bound capability");
        state
            .start_request_with_owner_and_admission(request(), [4; 32], lease, capability)
            .await
            .expect("child starts the admitted request, leaving it Running");
        fs::write(&marker, b"running").expect("signal parent");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn request() -> RequestIdentity {
    RequestIdentity {
        session_id: [5; 16],
        request_id: [6; 16],
    }
}

#[test]
fn killed_writer_is_fenced_and_cannot_land_a_write() {
    let (directory, repository) = repository();
    let marker = directory.path().join("owner-fence-child-marker");
    let mut child = spawn_child(LEASE_CHILD_TEST, directory.path(), &marker);
    wait_for_marker(&marker, &mut child);
    // The dead writer's token, exactly as the child printed and published it.
    let dead = decode_lease(&fs::read_to_string(&marker).expect("child lease token"));
    assert_eq!(dead.epoch, 1, "the child's first acquisition is epoch 1");
    assert_ne!(dead.owner_id, OTHER_OWNER);

    runtime().block_on(async {
        // While the child is alive it owns the runtime lock and the lease. A
        // second participant must not become a writer.
        let secondary = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            OTHER_OWNER,
        )
        .await
        .expect("secondary runtime opens while owner is alive");
        assert!(
            !secondary.holds_runtime_owner_lock(),
            "a live owner keeps the runtime owner lock"
        );
        assert!(
            matches!(
                secondary.acquire_lease(OTHER_OWNER).await,
                Err(RuntimeError::LeaseHeld)
            ),
            "a secondary participant must not take the writer lease from a live owner"
        );
        assert_eq!(secondary.current_lease().await.unwrap(), Some(dead));
        drop(secondary);
    });

    kill_and_reap(&mut child);

    runtime().block_on(async {
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            OTHER_OWNER,
        )
        .await
        .expect("reopen after owner death");
        assert!(
            state.holds_runtime_owner_lock(),
            "the dead owner must release the runtime lock"
        );

        // The replacement process holds the runtime lock, so acquiring the
        // lease takes the departed token over with a bumped epoch.
        let fresh = state
            .acquire_lease(OTHER_OWNER)
            .await
            .expect("replacement owner takes over the dead lease");
        assert_eq!(fresh.owner_id, OTHER_OWNER);
        assert_eq!(fresh.epoch, dead.epoch + 1);
        assert_eq!(state.current_lease().await.unwrap(), Some(fresh));
        assert!(
            matches!(
                state.takeover_lease(dead, [0xe3; 16]).await,
                Err(RuntimeError::OwnerLost)
            ),
            "a second takeover of the already-fenced token must fail with OwnerLost"
        );

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

/// A SIGKILLed `orna run` leaves a Running request under its dead process
/// owner. A later process that takes the lease over bumps the epoch, and the
/// takeover records a recovery barrier for that Running request, so the new
/// writer's commit is refused with `RecoveryPending` until that request is
/// recovered. This pins the fence; the owner-side recovery path remains open.
#[test]
fn running_request_from_killed_run_is_fenced_by_takeover() {
    let (directory, repository) = repository();
    let marker = directory.path().join("owner-fence-running-marker");
    let mut child = spawn_child(RUNNING_CHILD_TEST, directory.path(), &marker);
    wait_for_marker(&marker, &mut child);
    kill_and_reap(&mut child);

    runtime().block_on(async {
        // A later CLI process reopens the worktree and acquires the lease.
        let repl = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            OTHER_OWNER,
        )
        .await
        .expect("REPL reopens after the killed run");
        assert!(repl.holds_runtime_owner_lock());
        let lease = repl
            .acquire_lease(OTHER_OWNER)
            .await
            .expect("REPL takes over the killed run's lease");
        assert_eq!(lease.owner_id, OTHER_OWNER);
        assert_eq!(lease.epoch, 2, "takeover from a departed owner bumps the epoch");

        let context = repl.begin_activation().await.expect("REPL capture");
        let refused = repl
            .commit_table_activation(
                lease,
                &context,
                &[insert([13; 16], NEW_OWNER_ROW, 4)],
                [12; 32],
                &NoFault,
            )
            .await;
        assert!(
            matches!(refused, Err(RuntimeError::RecoveryPending)),
            "a Running request from the departed owner must hold a recovery barrier, got {refused:?}"
        );
        assert_eq!(
            repl.committed_table_row("records", NEW_OWNER_ROW).await.unwrap(),
            None
        );
    });
}
