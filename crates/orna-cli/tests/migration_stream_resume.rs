//! Gate F: a legacy migration must carry a source stream's consumer checkpoints
//! into the destination, so delivery resumes exactly where the source committed.
//! Stream state that cannot be carried must fail closed and leave the source intact.

// The runtime's stream API returns large futures; each test awaits one at a
// time, so boxing them here would only move the state to the heap.
#![allow(clippy::large_futures)]

use std::{fs, path::Path, process::Command};

use orna_repository::{Repository, RepositoryFormat};
use orna_runtime::{
    CheckpointKey, CheckpointPrecondition, CheckpointResetAudit, CheckpointResetRequest, Component,
    ConsumerIdentity, ListStreamSource, Position, RequestIdentity, RunObservationRegistration,
    RuntimeError, RuntimeIdentity, RuntimeState, StreamCheckpoint, StreamHandler,
    StreamHandlerResult, StreamItem, StreamMutationBatch, StreamObservationRegistration,
    StreamStep, TerminalOutcome, WriterLease, import_stream_carry, migration_copy_runtime_identity,
    read_stream_carry, repository_runtime_identity, runtime_identity_for_database_id,
};
use tempfile::TempDir;

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// A format-1 legacy worktree with staged, unstaged and untracked changes and a gitlink.
fn dirty_legacy_source(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    git_output(root, &["config", "user.name", "kierandrewett"]);
    git_output(root, &["config", "user.email", "kieran@drewett.dev"]);
    git_output(root, &["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("main.orna"),
        include_str!("fixtures/project-core-main.orna"),
    )
    .unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(
        root.join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
    )
    .unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(root, &["add", "."]);
    git_output(root, &["commit", "-m", "legacy source"]);

    fs::write(root.join("tracked.txt"), b"one\n").unwrap();
    git_output(root, &["add", "tracked.txt"]);
    git_output(root, &["commit", "-m", "tracked"]);
    fs::write(root.join("tracked.txt"), b"two\n").unwrap();
    git_output(root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), b"three\n").unwrap();
    fs::write(root.join("untracked.txt"), b"untracked\n").unwrap();
    let head = String::from_utf8(git_output(root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    git_output(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},vendor/lib"),
        ],
    );
}

/// The legacy source has no migration continuity yet, so its runtime identity is
/// derived from the database id in its own `.orna/database.orna` sidecar, the
/// same derivation the destination uses from its recorded identity.
fn source_runtime_identity(root: &Path) -> (RuntimeIdentity, [u8; 32]) {
    let sidecar = fs::read_to_string(root.join(".orna/database.orna")).unwrap();
    let uuid = sidecar.split('"').nth(1).expect("quoted database id");
    let hex: String = uuid.chars().filter(|character| *character != '-').collect();
    assert_eq!(hex.len(), 32, "database id is a 128-bit uuid");
    let mut database_id = [0_u8; 16];
    for (index, byte) in database_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    runtime_identity_for_database_id(database_id)
}

fn checkpoint_key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("consume").unwrap(),
            binding: Component::new("binding").unwrap(),
        },
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("source").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: Some(Component::new("partition").unwrap()),
        position_format: Component::new("position-format").unwrap(),
    }
}

struct CommitEach {
    seen: Vec<Vec<u8>>,
    next_digest: [u8; 32],
}

impl StreamHandler for CommitEach {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        self.seen.push(item.payload.clone());
        StreamHandlerResult::Commit(StreamMutationBatch {
            mutations: Vec::new(),
            next_digest: self.next_digest,
        })
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Gives the legacy source a durable persistent consumer that has committed two of three items.
/// When `pause` is set, the source also holds a paused stream, whose control row is carried.
fn give_source_a_persistent_consumer(root: &Path, pause: bool) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let writer: WriterLease = state.acquire_lease([44; 16]).await.unwrap();
        let key = checkpoint_key();
        let mut source = ListStreamSource::new(
            key.clone(),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()],
        );
        for expected in [b"1".as_slice(), b"2".as_slice()] {
            let mut handler = CommitEach {
                seen: Vec::new(),
                next_digest: state.capture().await.unwrap().generation_digest(),
            };
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await
                .expect("consumer delivery commits");
            assert_eq!(handler.seen, vec![expected.to_vec()]);
        }
        if pause {
            state
                .pause_stream(writer, key.clone())
                .await
                .expect("pausing the stream writes stream_control state");
        }
        state.stream_checkpoint(&key).await.unwrap()
    })
}

fn checkpoint_after_reopen(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("reopen source runtime");
        state.stream_checkpoint(&checkpoint_key()).await.unwrap()
    })
}

fn run_copy(source: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            "0",
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap()
}

/// Derives the runtime identity and digest a copy is opened by.
type DeriveIdentity = fn(&Repository) -> Result<(RuntimeIdentity, [u8; 32]), RuntimeError>;

/// Delivers the remaining items from a copy's carried checkpoint, opened by the
/// runtime identity that `derive_identity` returns for the copy's current HEAD.
fn deliver_resumed(root: &Path, derive_identity: DeriveIdentity) -> Vec<Vec<u8>> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = derive_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open destination runtime");
        let writer = state.acquire_lease([45; 16]).await.unwrap();
        let key = checkpoint_key();
        let mut source = ListStreamSource::new(
            key.clone(),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()],
        );
        let mut handler = CommitEach {
            seen: Vec::new(),
            next_digest: state.capture().await.unwrap().generation_digest(),
        };
        state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .expect("resumed delivery commits");
        handler.seen
    })
}

/// Delivers the remaining items from the destination's carried checkpoint before activation.
fn deliver_from_destination(root: &Path) -> Vec<Vec<u8>> {
    deliver_resumed(root, migration_copy_runtime_identity)
}

/// Delivers the remaining items from the activated format-3 HEAD, by its format-3 identity.
fn deliver_from_activated_head(root: &Path) -> Vec<Vec<u8>> {
    deliver_resumed(root, repository_runtime_identity)
}

/// Resumes the source's paused stream, leaving a running `stream_control` row behind.
fn resume_source_stream(root: &Path) {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let writer = state.acquire_lease([46; 16]).await.unwrap();
        state
            .resume_stream(writer, checkpoint_key())
            .await
            .expect("resuming the paused stream");
    });
}

/// Runs one delivery step from the destination's carried checkpoint and returns
/// the step, whether it committed, paused or was rejected.
fn step_from_destination(root: &Path) -> StreamStep {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = migration_copy_runtime_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open destination runtime");
        let writer = state.acquire_lease([47; 16]).await.unwrap();
        let key = checkpoint_key();
        let mut source = ListStreamSource::new(
            key.clone(),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()],
        );
        let mut handler = CommitEach {
            seen: Vec::new(),
            next_digest: state.capture().await.unwrap().generation_digest(),
        };
        state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .expect("delivery step runs")
    })
}

#[test]
fn migration_carries_persistent_stream_checkpoint_and_resumes_delivery() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, false);
    assert_eq!(before.version, 2);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        output.status.success(),
        "carriable stream state must migrate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("carried_checkpoints=1"));
    let source_after = checkpoint_after_reopen(&source);
    assert_eq!(source_after.version, before.version);
    assert_eq!(source_after.committed, before.committed);
    assert_eq!(deliver_from_destination(&destination), vec![b"3".to_vec()]);
}

/// A paused stream is carried as paused: the destination's first delivery step
/// reports `Paused` and advances nothing.
#[test]
fn migration_carries_paused_stream_control_and_keeps_delivery_paused() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, true);
    assert_eq!(before.version, 2);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        output.status.success(),
        "a paused stream is carriable stream state: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("carried_checkpoints=1"));
    match step_from_destination(&destination) {
        StreamStep::Paused { checkpoint } => {
            assert_eq!(checkpoint.version, before.version);
            assert_eq!(checkpoint.committed, before.committed);
        }
        other => panic!("a carried paused stream must stay paused, got {other:?}"),
    }
}

/// A pause followed by a resume leaves a running `stream_control` row. The row
/// is carried, so the copy migrates and delivery resumes.
#[test]
fn migration_carries_resumed_stream_and_resumes_delivery() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, true);
    resume_source_stream(&source);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        output.status.success(),
        "a resumed stream must migrate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(deliver_from_destination(&destination), vec![b"3".to_vec()]);
}

/// Live stream state that a carry cannot resume refuses the migration and
/// leaves the source checkpoint untouched.
#[test]
fn migration_refuses_non_carriable_stream_state_and_keeps_checkpoint() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, false);
    assert_eq!(before.version, 2);
    runtime().block_on(async {
        let repository = Repository::discover(&source).unwrap();
        let database = libsql::Builder::new_local(repository.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query("SELECT key_id FROM stream_checkpoint", ())
            .await
            .unwrap();
        let key_id: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        connection
            .execute(
                "INSERT INTO stream_pause_pending (key_id) VALUES (?1)",
                libsql::params![key_id],
            )
            .await
            .unwrap();
    });
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        !output.status.success(),
        "migration must not drop non-carriable stream state: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E2100"));
    assert!(
        !destination.exists(),
        "no migration copy may be prepared when stream state is not carried"
    );
    let after = checkpoint_after_reopen(&source);
    assert_eq!(after.version, before.version);
    assert_eq!(after.committed, before.committed);
}

#[test]
fn stream_carry_import_without_recorded_continuity_writes_nothing() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    let clean_source = parent.path().join("clean");
    fs::create_dir(&clean_source).unwrap();
    dirty_legacy_source(&clean_source);
    let destination = parent.path().join("migrated");
    let output = run_copy(&clean_source, &destination);
    assert!(
        output.status.success(),
        "clean source must prepare a destination: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    runtime().block_on(async {
        let source_repository = Repository::discover(&source).unwrap();
        let carry = read_stream_carry(&source_repository)
            .await
            .expect("source stream state is carriable");
        assert_eq!(carry.checkpoint_count(), 1);

        let destination_repository = Repository::discover(&destination).unwrap();
        let refused = import_stream_carry(&destination_repository, &carry, None).await;
        assert_eq!(refused, Err(RuntimeError::StreamIdentityMismatch));

        let (identity, digest) = migration_copy_runtime_identity(&destination_repository).unwrap();
        let state = RuntimeState::open(&destination_repository, identity, digest)
            .await
            .expect("open destination runtime");
        let absent = match state.stream_checkpoint(&checkpoint_key()).await {
            Ok(checkpoint) => checkpoint.version == 0,
            Err(_) => true,
        };
        assert!(absent, "a refused import must not write a checkpoint");
    });
}

#[test]
fn migration_of_dirty_source_without_stream_state_still_prepares_copy() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        output.status.success(),
        "clean stream state must still migrate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("prepared or recovered format-3 migration copy")
    );
    assert!(destination.join("main.orna").exists());
}

/// A carried checkpoint's history chain is only provable from its migration bridge
/// origin. Removing the bridge and leaving a chain that begins above version zero
/// must refuse the destination open rather than resume from an unproven state.
#[test]
fn migration_carry_without_bridge_origin_refuses_open() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        output.status.success(),
        "carriable stream state must migrate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    runtime().block_on(async {
        let repository = Repository::discover(&destination).unwrap();
        let (identity, digest) = migration_copy_runtime_identity(&repository).unwrap();
        RuntimeState::open(&repository, identity, digest)
            .await
            .expect("intact destination opens before forging");

        let database = libsql::Builder::new_local(repository.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute("DELETE FROM stream_checkpoint_migration_bridge", ())
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO stream_checkpoint_history
                 SELECT key_id, version, committed_position, snapshot, generation_digest, transition
                 FROM stream_checkpoint_migration_archive
                 WHERE version = 2",
                (),
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);

        match RuntimeState::open(&repository, identity, digest).await {
            Ok(_) => panic!("a bridge-less chain beginning at version 2 must not open"),
            Err(error) => assert_eq!(error, RuntimeError::RecoveryInvalid),
        }
    });
}

#[test]
fn stream_carry_import_is_idempotent_and_refuses_rows_that_differ() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    let destination = parent.path().join("migrated");
    let output = run_copy(&source, &destination);
    assert!(
        output.status.success(),
        "first carry must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = runtime().block_on(async {
        let source_repository = Repository::discover(&source).unwrap();
        let carry = read_stream_carry(&source_repository)
            .await
            .expect("source stream state is carriable");
        let recorded = carry
            .continuity_record()
            .expect("carry continuity is recorded");
        let destination_repository = Repository::discover(&destination).unwrap();
        import_stream_carry(&destination_repository, &carry, Some(&recorded))
            .await
            .expect("re-importing an identical carry is a no-op");
        recorded
    });
    assert_eq!(deliver_from_destination(&destination), vec![b"3".to_vec()]);

    runtime().block_on(async {
        let source_repository = Repository::discover(&source).unwrap();
        let carry = read_stream_carry(&source_repository)
            .await
            .expect("source stream state is carriable");
        let destination_repository = Repository::discover(&destination).unwrap();
        let differs = import_stream_carry(&destination_repository, &carry, Some(&recorded)).await;
        assert_eq!(differs, Err(RuntimeError::MigrationCarryTargetDiffers));
    });
}

#[test]
fn migration_copy_retry_after_carry_recovers() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    let destination = parent.path().join("migrated");
    let first = run_copy(&source, &destination);
    assert!(
        first.status.success(),
        "first copy must carry: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let retry = run_copy(&source, &destination);
    assert!(
        retry.status.success(),
        "rerunning a completed carry must recover: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(deliver_from_destination(&destination), vec![b"3".to_vec()]);
}

/// `orna upgrade --to-format 3 DEST --yes-activate` publishes a prepared copy.
fn run_upgrade(source: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "upgrade",
            "--to-format",
            "3",
            destination.to_str().unwrap(),
            "--yes-activate",
        ])
        .output()
        .unwrap()
}

/// The checkpoint stored in an activated copy, read through its format-3 HEAD identity.
fn activated_checkpoint(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = repository_runtime_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open activated runtime");
        state.stream_checkpoint(&checkpoint_key()).await.unwrap()
    })
}

/// The number of predecessor bridges recorded in an activated copy's runtime state.
fn activated_bridge_count(root: &Path) -> i64 {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let database = libsql::Builder::new_local(repository.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM stream_checkpoint_migration_bridge",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("a count row");
        row.get::<i64>(0).unwrap()
    })
}

/// Activation publishes the prepared copy. The carried checkpoint and its predecessor
/// bridge stay in the activated format-3 runtime, delivery resumes after version 2
/// without a reset, and the source is left unchanged.
#[test]
fn activated_migration_copy_resumes_carried_checkpoint_without_reset() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, false);
    assert_eq!(before.version, 2);
    let destination = parent.path().join("migrated");
    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let source_head = git_output(&source, &["rev-parse", "HEAD"]);
    let source_status = git_output(&source, &["status", "--porcelain"]);

    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );
    assert!(
        String::from_utf8_lossy(&activated.stdout).starts_with("activated format-3 migration copy")
    );
    assert_eq!(
        Repository::discover(&destination)
            .unwrap()
            .open_format_context()
            .unwrap()
            .repository_format(),
        RepositoryFormat::Format3
    );

    let carried = activated_checkpoint(&destination);
    assert_eq!(carried.version, before.version);
    assert_eq!(carried.committed, before.committed);
    assert_eq!(
        activated_bridge_count(&destination),
        1,
        "the predecessor bridge survives activation"
    );

    assert_eq!(
        deliver_from_activated_head(&destination),
        vec![b"3".to_vec()]
    );
    let source_after = checkpoint_after_reopen(&source);
    assert_eq!(source_after.version, before.version);
    assert_eq!(source_after.committed, before.committed);
    assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        git_output(&source, &["status", "--porcelain"]),
        source_status
    );
}

/// Resets the paused source consumer one version forward, to a new position.
fn reset_source_checkpoint(root: &Path, before: &StreamCheckpoint) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let writer = state.acquire_lease([48; 16]).await.unwrap();
        state
            .reset_checkpoint(
                writer,
                CheckpointResetRequest {
                    key: checkpoint_key(),
                    expected: CheckpointPrecondition {
                        version: before.version,
                        committed: before.committed.clone(),
                    },
                    to: Position {
                        token: Component::new("opaque-41").unwrap(),
                    },
                    reason: "operator reset after upstream repair".into(),
                },
            )
            .await
            .expect("resetting the paused consumer")
    })
}

/// The checkpoint reset audit rows in an activated copy, in admission order.
fn activated_reset_audits(root: &Path) -> Vec<CheckpointResetAudit> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = repository_runtime_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open activated runtime");
        state
            .checkpoint_reset_audits(&checkpoint_key())
            .await
            .unwrap()
    })
}

/// A checkpoint reset is durable history. The migrated copy keeps its audit row,
/// with the old and new typed versions and positions and the reason, and the
/// carried checkpoint is the reset target.
#[test]
fn activated_migration_copy_carries_checkpoint_reset_audit() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, true);
    assert_eq!(before.version, 2);
    let reset = reset_source_checkpoint(&source, &before);
    assert_eq!(reset.version, 3);
    let destination = parent.path().join("migrated");

    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy must carry the reset audit: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );

    let audits = activated_reset_audits(&destination);
    assert_eq!(audits.len(), 1, "the reset audit survives activation");
    let audit = &audits[0];
    assert_eq!(audit.key, checkpoint_key());
    assert_eq!(audit.old_version, before.version);
    assert_eq!(audit.old_position, before.committed);
    assert_eq!(audit.new_version, reset.version);
    assert_eq!(
        audit.new_position,
        Position {
            token: Component::new("opaque-41").unwrap(),
        }
    );
    assert_eq!(audit.reason, "operator reset after upstream repair");
    assert!(!audit.redacted);
    assert_eq!(activated_checkpoint(&destination).version, reset.version);
    assert_eq!(activated_bridge_count(&destination), 1);
}

/// Gives the legacy source a run observation and its stream observation. A
/// completed run is terminal; an uncompleted run is live.
fn give_source_an_observed_run(root: &Path, complete: bool) {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let request = RequestIdentity {
            session_id: [51; 16],
            request_id: [52; 16],
        };
        state
            .reserve_request(request, [53; 32])
            .await
            .expect("reserve request");
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: checkpoint_key().consumer,
                function: "pkg.consume".into(),
                source_identity: None,
                invocation_id: [54; 16],
            })
            .await
            .expect("register run");
        state
            .register_stream_observation(StreamObservationRegistration {
                run: run.id,
                producer: "source-object".into(),
                consumer: Some("pkg.consume".into()),
                checkpoint: checkpoint_key(),
            })
            .await
            .expect("register stream");
        if complete {
            state
                .complete_request(request, [53; 32], TerminalOutcome::new(vec![55]).unwrap())
                .await
                .expect("complete request");
        }
    });
}

/// Every row of an observation table, in primary-key order, as stored values.
fn observation_rows(root: &Path, table: &str) -> Vec<Vec<libsql::Value>> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let database = libsql::Builder::new_local(repository.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query(&format!("SELECT * FROM {table} ORDER BY 1"), ())
            .await
            .unwrap();
        let mut collected = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            collected.push(
                (0..row.column_count())
                    .map(|index| row.get_value(index).unwrap())
                    .collect(),
            );
        }
        collected
    })
}

/// A terminal run and its stream observation are durable history. The migrated
/// copy carries both rows verbatim, so the activated copy holds the same rows.
#[test]
fn activated_migration_copy_carries_terminal_run_and_stream_observations_verbatim() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_an_observed_run(&source, true);
    let source_runs = observation_rows(&source, "sys_run_observation");
    let source_streams = observation_rows(&source, "sys_stream_observation");
    assert_eq!(source_runs.len(), 1, "the source holds the terminal run");
    assert_eq!(
        source_streams.len(),
        1,
        "the source holds its stream observation"
    );
    let destination = parent.path().join("migrated");

    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy must carry run observations: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );

    assert_eq!(
        observation_rows(&destination, "sys_run_observation"),
        source_runs
    );
    assert_eq!(
        observation_rows(&destination, "sys_stream_observation"),
        source_streams
    );
}

/// A live run has no terminal state to carry. The copy refuses with E2100,
/// prepares no destination and leaves the source run unchanged.
#[test]
fn migration_refuses_live_run_observation_and_keeps_source() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    give_source_an_observed_run(&source, false);
    let before = observation_rows(&source, "sys_run_observation");
    assert_eq!(before.len(), 1, "the source holds the live run");
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        !output.status.success(),
        "migration must not drop a live run: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("E2100"));
    assert!(
        !destination.exists(),
        "no migration copy may be prepared for a live run"
    );
    assert_eq!(observation_rows(&source, "sys_run_observation"), before);
}
