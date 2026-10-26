//! Gate D owner fence (ORNA-TURSO-004/005/006, ORNA-TASK-003) under real
//! process death.
//!
//! Gate D clause "no abandoned owner retains write permission" is defined here
//! by the lease semantics: a writer is its lease token `(owner, epoch)`. Every
//! CLI process acquires the lease under its own per-process owner ID
//! (`process_owner_id`), and a lease is bound to the runtime owner lock of the
//! handle that established it. A child process acquires the lease while
//! holding that lock, publishes its `WriterLease` token, then blocks until it
//! is SIGKILLed. Only the death releases the lock, so only then can a
//! replacement process that takes the lock acquire the lease: it takes the
//! departed token over with a bumped epoch and, in the same transaction,
//! moves every Running request the dead owner left to a terminal uncertain
//! outcome. The dead owner's token must then be rejected with `OwnerLost` and
//! land nothing, the dead owner must not regain the lease, and the
//! replacement's next write must land.
//!
//! A live owner, even a hung one, keeps its lock and therefore its lease: a
//! second participant is refused with `LeaseHeld`, and so is a different
//! owner on the lock holder's own handle. This test does not claim that a live
//! writer is fenced from outside its process; that remains a gap (see the
//! filed bead for Gate D).

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
    NoFault, RecoveryDisposition, RequestIdentity, RequestState, RuntimeError, RuntimeIdentity,
    RuntimeState, TableMutation, WriterLease, process_owner_id,
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
/// A secondary participant's owner, opened while the child owns the worktree.
const OTHER_OWNER: [u8; 16] = [0xd2; 16];
/// A different owner tried through the replacement's own lock-holding handle.
const THIRD_OWNER: [u8; 16] = [0xd3; 16];
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

/// Publishes `contents` at `marker` atomically. The parent polls for the
/// marker's existence, so it must never observe a created but unwritten file.
fn publish_marker(marker: &Path, contents: &[u8]) {
    let staged = marker.with_extension("staged");
    fs::write(&staged, contents).expect("stage the parent signal");
    fs::rename(&staged, marker).expect("publish the parent signal");
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
        publish_marker(&marker, token.as_bytes());
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    });
}

/// The owner that gets killed while a request is Running. It leaves a durable
/// `Running` request-ledger row under its process owner, as an `orna run`
/// killed mid-invocation would, and publishes its `WriterLease` token.
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
        publish_marker(&marker, encode_lease(lease).as_bytes());
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
    assert_ne!(
        dead.owner_id,
        process_owner_id(),
        "a different process never reuses this process's owner ID"
    );

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
        assert_eq!(secondary.current_lease().await, Err(RuntimeError::LeaseHeld));
        drop(secondary);
    });

    kill_and_reap(&mut child);

    runtime().block_on(async {
        let replacement = process_owner_id();
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            replacement,
        )
        .await
        .expect("reopen after owner death");
        assert!(
            state.holds_runtime_owner_lock(),
            "the dead owner must release the runtime lock"
        );

        // The dead owner's lock was released, so the lease it bound to that
        // lock has no live holder: the replacement takes it over with a
        // bumped epoch.
        let fresh = state
            .acquire_lease(replacement)
            .await
            .expect("replacement owner takes over the dead lease");
        assert_eq!(fresh.owner_id, replacement);
        assert_eq!(fresh.epoch, dead.epoch + 1);
        assert_eq!(state.current_lease().await.unwrap(), Some(fresh));

        // The replacement's lease is bound to this live lock. The abandoned
        // owner cannot regain it, and a different owner cannot fence it, even
        // through the lock holder's own handle.
        assert_eq!(
            state.acquire_lease(dead.owner_id).await,
            Err(RuntimeError::LeaseHeld),
            "the abandoned owner must not regain the writer lease"
        );
        assert_eq!(
            state.acquire_lease(THIRD_OWNER).await,
            Err(RuntimeError::LeaseHeld),
            "holding the runtime owner lock must not fence a live owner"
        );
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
/// owner. The next process takes the lease over and, in the same
/// transaction, recovers that request to a terminal uncertain outcome
/// (ORNA-TASK-003, REQUEST-1 step 5). Its own write then lands instead of
/// waiting behind a recovery barrier, and the dead token lands nothing.
#[test]
fn running_request_from_killed_run_is_recovered_before_the_next_write() {
    let (directory, repository) = repository();
    let marker = directory.path().join("owner-fence-running-marker");
    let mut child = spawn_child(RUNNING_CHILD_TEST, directory.path(), &marker);
    wait_for_marker(&marker, &mut child);
    let dead = decode_lease(&fs::read_to_string(&marker).expect("child lease token"));
    kill_and_reap(&mut child);

    runtime().block_on(async {
        // A later CLI process reopens the worktree and acquires the lease.
        let owner = process_owner_id();
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            owner,
        )
        .await
        .expect("reopen after the killed run");
        assert!(state.holds_runtime_owner_lock());
        assert_eq!(
            state
                .request_status(request(), [4; 32])
                .await
                .unwrap()
                .map(|status| status.state),
            Some(RequestState::Running),
            "the killed run leaves its request Running until an owner takes over"
        );
        let lease = state
            .acquire_lease(owner)
            .await
            .expect("the next process takes over the killed run's lease");
        assert_eq!(lease.owner_id, owner);
        assert_eq!(
            lease.epoch,
            dead.epoch + 1,
            "takeover from a departed owner bumps the epoch"
        );

        // The takeover recovered the abandoned request: it is terminal and
        // keeps uncertain external effects, so it is never re-executed.
        let recovered = state
            .request_status(request(), [4; 32])
            .await
            .unwrap()
            .expect("the recovered request stays retained");
        assert_eq!(recovered.state, RequestState::Orphaned);
        assert!(recovered.terminal_outcome.is_some());
        assert_eq!(
            state
                .request_recovery_disposition(request(), [4; 32])
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
        assert!(state.running_requests().await.unwrap().is_empty());

        let context = state.begin_activation().await.expect("capture after recovery");
        state
            .commit_table_activation(
                lease,
                &context,
                &[insert([13; 16], NEW_OWNER_ROW, 4)],
                [12; 32],
                &NoFault,
            )
            .await
            .expect("the next write is admitted after recovery");
        assert_eq!(
            state.committed_table_row("records", NEW_OWNER_ROW).await.unwrap(),
            Some(vec![4])
        );
        let context = state.begin_activation().await.expect("capture for the dead token");
        let rejected = state
            .commit_table_activation(
                dead,
                &context,
                &[insert([14; 16], DEAD_OWNER_ROW, 5)],
                [13; 32],
                &NoFault,
            )
            .await;
        assert!(
            matches!(rejected, Err(RuntimeError::OwnerLost)),
            "the killed run's token must be rejected with OwnerLost, got {rejected:?}"
        );
        assert_eq!(
            state.committed_table_row("records", DEAD_OWNER_ROW).await.unwrap(),
            None
        );
    });
}

/// ORNA-TURSO-005: a participant without the worktree's runtime owner lock can
/// neither create the writer lease nor take it from the live lock holder.
#[test]
fn a_secondary_participant_cannot_create_or_take_the_writer_lease() {
    let (_directory, repository) = repository();
    runtime().block_on(async {
        let holder = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            process_owner_id(),
        )
        .await
        .expect("open the lock holder");
        assert!(holder.holds_runtime_owner_lock());
        let secondary = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            OTHER_OWNER,
        )
        .await
        .expect("open a secondary participant");
        assert!(!secondary.holds_runtime_owner_lock());
        assert_eq!(
            secondary.acquire_lease(OTHER_OWNER).await,
            Err(RuntimeError::LeaseHeld),
            "a secondary must not create the lease on a fresh worktree"
        );
        assert_eq!(secondary.current_lease().await, Err(RuntimeError::LeaseHeld));

        let lease = holder
            .acquire_lease(process_owner_id())
            .await
            .expect("the lock holder creates the lease");
        assert_eq!(lease.epoch, 1);
        assert_eq!(
            secondary.acquire_lease(OTHER_OWNER).await,
            Err(RuntimeError::LeaseHeld),
            "a secondary must not take the lease from the live lock holder"
        );
        assert_eq!(secondary.current_lease().await, Err(RuntimeError::LeaseHeld));
        assert_eq!(holder.current_lease().await.unwrap(), Some(lease));
    });
}

/// Holding the runtime owner lock does not prove that a different recorded
/// owner departed. A lease bound to the holder's own live lock is refused to
/// a different owner, and its holder keeps writing.
#[test]
fn the_lock_holder_does_not_fence_a_live_owner_bound_to_its_own_lock() {
    let (_directory, repository) = repository();
    runtime().block_on(async {
        let state = RuntimeState::open_with_owner_id(
            &repository,
            identity(),
            INITIAL_DIGEST,
            process_owner_id(),
        )
        .await
        .expect("open the lock holder");
        let owner = state
            .acquire_lease(OTHER_OWNER)
            .await
            .expect("the first owner acquires the lease");
        assert_eq!(
            state.acquire_lease(THIRD_OWNER).await,
            Err(RuntimeError::LeaseHeld)
        );
        assert_eq!(state.current_lease().await.unwrap(), Some(owner));
        let context = state.begin_activation().await.expect("capture");
        state
            .commit_table_activation(
                owner,
                &context,
                &[insert([15; 16], NEW_OWNER_ROW, 6)],
                [14; 32],
                &NoFault,
            )
            .await
            .expect("the live owner keeps writing");
    });
}
