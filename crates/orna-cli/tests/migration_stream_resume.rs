//! Gate F: a legacy migration must carry a source stream's consumer checkpoints
//! into the destination, so delivery resumes exactly where the source committed.
//! Stream state that cannot be carried must fail closed and leave the source intact.

// The runtime's stream API returns large futures; each test awaits one at a
// time, so boxing them here would only move the state to the heap.
#![allow(clippy::large_futures)]

use std::{fs, path::Path, process::Command};

use orna_repository::{Repository, RepositoryFormat};
use orna_runtime::{
    CheckpointKey, Component, ConsumerIdentity, ListStreamSource, RuntimeError, RuntimeIdentity,
    RuntimeState, StreamCheckpoint, StreamHandler, StreamHandlerResult, StreamItem,
    StreamMutationBatch, StreamStep, WriterLease, import_stream_carry,
    migration_copy_runtime_identity, read_stream_carry, repository_runtime_identity,
    runtime_identity_for_database_id,
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

/// A live consumer lease is per-attempt state. The carry refuses it by name
/// (`stream_lease`), copies nothing, and leaves the source checkpoint unchanged.
#[test]
fn migration_refuses_live_consumer_lease_and_names_it() {
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
                "INSERT INTO stream_lease
                 (key_id, delivery_position, successor_position, fence, purpose)
                 VALUES (?1, 'forged-delivery', 'forged-successor', 3, 1)",
                libsql::params![key_id],
            )
            .await
            .unwrap();
    });
    let destination = parent.path().join("migrated");

    // `--debug` is what prints the refusal's `tables=` detail on stderr.
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--debug",
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
        .unwrap();

    assert!(
        !output.status.success(),
        "a live consumer lease must not be dropped: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(stderr.contains("tables=stream_lease"), "{stderr}");
    assert!(
        !destination.exists(),
        "no migration copy may be prepared while a lease is live"
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

/// The checkpoint snapshot fixture the runtime's own bootstrap tests use. Its watermark
/// names the consumer returned by `snapshot_key`.
const CHECKPOINT_SNAPSHOT: &[u8] =
    include_bytes!("../../orna-runtime/tests/fixtures/checkpoint_snapshot_v1.orna");

fn snapshot_key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("urn:consumer:ledger").unwrap(),
            root: Component::new("app.root").unwrap(),
            function: Component::new("processor.consume").unwrap(),
            binding: Component::new("input.orders").unwrap(),
        },
        source_format: Component::new("kafka/v1").unwrap(),
        source: Component::new("orders/primary").unwrap(),
        partition_format: Component::new("kafka-partition/v1").unwrap(),
        partition: Some(Component::new("tenant:west/17").unwrap()),
        position_format: Component::new("offset/utf8").unwrap(),
    }
}

/// Commits the snapshot fixture as the source's HEAD, then bootstraps the snapshot
/// consumer from that commit. Returns the seeded checkpoint and the selected commit.
fn bootstrap_source_from_snapshot(root: &Path) -> (StreamCheckpoint, String) {
    let directory = root.join(".orna/checkpoints");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("selected.orna"), CHECKPOINT_SNAPSHOT).unwrap();
    git_output(root, &["add", "-f", "--", ".orna/checkpoints"]);
    git_output(
        root,
        &[
            "commit",
            "-m",
            "checkpoint snapshot",
            "--",
            ".orna/checkpoints",
        ],
    );
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let writer: WriterLease = state.acquire_lease([45; 16]).await.unwrap();
        let selected = repository.head().unwrap().expect("snapshot commit");
        let seeded = state
            .bootstrap_stream_from_selected_snapshot(
                writer,
                &repository,
                &selected,
                &snapshot_key(),
            )
            .await
            .expect("bootstrap from the selected snapshot");
        (seeded, selected.as_str().to_owned())
    })
}

/// The snapshot consumer's checkpoint in an activated copy, read through its format-3 HEAD.
fn activated_snapshot_checkpoint(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = repository_runtime_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open activated runtime");
        state.stream_checkpoint(&snapshot_key()).await.unwrap()
    })
}

/// The selected commits pinned by `stream_checkpoint_bootstrap` in an activated copy.
fn activated_bootstrap_pins(root: &Path) -> Vec<String> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let database = libsql::Builder::new_local(repository.runtime_paths().state_db())
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query(
                "SELECT selected_commit FROM stream_checkpoint_bootstrap ORDER BY key_id",
                (),
            )
            .await
            .unwrap();
        let mut pins = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            pins.push(row.get::<String>(0).unwrap());
        }
        pins
    })
}

/// A checkpoint bootstrap pin is carried verbatim. The activated copy keeps the snapshot
/// consumer at the selected version and the pin names the source HEAD the snapshot was
/// read from.
#[test]
fn migration_carries_checkpoint_bootstrap_pin_and_keeps_snapshot_checkpoint() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let (seeded, selected) = bootstrap_source_from_snapshot(&source);
    assert_eq!(seeded.version, 42);
    let source_head = String::from_utf8(git_output(&source, &["rev-parse", "HEAD"])).unwrap();
    assert_eq!(selected, source_head.trim());
    let destination = parent.path().join("migrated");
    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );

    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );

    assert_eq!(activated_bootstrap_pins(&destination), vec![selected]);
    let carried = activated_snapshot_checkpoint(&destination);
    assert_eq!(carried.version, seeded.version);
    assert_eq!(carried.committed, seeded.committed);
}

/// Inserts a failure for the source's checkpoint consumer, in `status`.
const INSERT_FAILURE_FROM_CHECKPOINT: &str = "INSERT INTO stream_failure (identity_id, key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, delivery_position, successor_position, version, attempts, status, diagnostic_code, diagnostic_class) SELECT ?1, key_id, consumer_principal, consumer_root, consumer_function, consumer_binding, source_format, source, partition_format, partition, position_format, 'delivery-position', 'successor-position', version, 1, ?2, 1, 1 FROM stream_checkpoint";

async fn state_database(root: &Path) -> libsql::Database {
    let repository = Repository::discover(root).unwrap();
    libsql::Builder::new_local(repository.runtime_paths().state_db())
        .build()
        .await
        .unwrap()
}

/// Seeds one failure in `status` for the source's checkpoint consumer.
fn seed_failure(root: &Path, identity: &str, status: i64) {
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        connection
            .execute(
                INSERT_FAILURE_FROM_CHECKPOINT,
                libsql::params![identity, status],
            )
            .await
            .unwrap();
    });
}

/// Seeds a failure in each non-in-flight state with its companion rows: an inline
/// payload, a referenced payload with assertion detail, a legacy marker, and a
/// provider failure on the checkpoint.
fn seed_failure_records(root: &Path) {
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        for (identity, status) in [("failure-payload", 1_i64), ("failure-detail", 6), ("failure-legacy", 7)] {
            connection
                .execute(
                    INSERT_FAILURE_FROM_CHECKPOINT,
                    libsql::params![identity, status],
                )
                .await
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO stream_failure_payload (identity_id, payload, payload_reference, payload_digest, retention) VALUES ('failure-payload', X'0102', NULL, NULL, 1)",
                (),
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO stream_failure_payload (identity_id, payload, payload_reference, payload_digest, retention) VALUES ('failure-detail', NULL, 'payload-ref', zeroblob(32), 2)",
                (),
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO stream_failure_payload_legacy (identity_id) VALUES ('failure-legacy')",
                (),
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO stream_failure_assertion_detail (identity_id, code, assertion_ref, owner_ref, owner_kind, predicate_ref, source_file_ref, start_byte, end_byte, start_line, start_column, end_line, end_column, safe_witness) VALUES ('failure-detail', 'ORNA-A091-009', X'01', X'02', 1, X'03', X'04', X'05', X'06', X'07', X'08', X'09', X'0A', 'witness text')",
                (),
            )
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO stream_provider_failure (key_id, checkpoint_version, committed_position, attempts, diagnostic_code, diagnostic_class) SELECT key_id, version, committed_position, 1, 1, 1 FROM stream_checkpoint",
                (),
            )
            .await
            .unwrap();
    });
}

/// Every row of the failure tables in a repository's runtime state, table by table.
fn failure_table_rows(root: &Path) -> Vec<(&'static str, Vec<Vec<libsql::Value>>)> {
    const TABLES: [(&str, &str); 5] = [
        ("stream_failure", "identity_id"),
        ("stream_failure_payload", "identity_id"),
        ("stream_failure_payload_legacy", "identity_id"),
        ("stream_failure_assertion_detail", "identity_id"),
        ("stream_provider_failure", "key_id"),
    ];
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        let mut tables = Vec::new();
        for (table, key) in TABLES {
            let mut rows = connection
                .query(&format!("SELECT * FROM {table} ORDER BY {key}"), ())
                .await
                .unwrap();
            let mut values = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                let width = row.column_count();
                values.push(
                    (0..width)
                        .map(|index| row.get_value(index).unwrap())
                        .collect(),
                );
            }
            tables.push((table, values));
        }
        tables
    })
}

/// Every row of the stream checkpoint table, read without opening the runtime.
fn stream_checkpoint_rows(root: &Path) -> Vec<Vec<libsql::Value>> {
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query("SELECT * FROM stream_checkpoint ORDER BY key_id", ())
            .await
            .unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let width = row.column_count();
            values.push(
                (0..width)
                    .map(|index| row.get_value(index).unwrap())
                    .collect(),
            );
        }
        values
    })
}

/// Runs `migrate copy` with `--debug`, which prints the refusal's `tables=` detail.
fn run_copy_with_debug(source: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--debug",
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

/// Failure records that are not in flight are carried verbatim with their payload,
/// assertion-detail, legacy and provider companion rows. The activated copy holds
/// the same rows, and the checkpoint is carried unchanged.
#[test]
fn migration_carries_failure_records_verbatim() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, false);
    seed_failure_records(&source);
    let expected = failure_table_rows(&source);
    for (table, rows) in &expected {
        assert!(!rows.is_empty(), "{table} must be seeded");
    }
    let destination = parent.path().join("migrated");

    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );

    assert_eq!(failure_table_rows(&destination), expected);
    let carried = activated_checkpoint(&destination);
    assert_eq!(carried.version, before.version);
    assert_eq!(carried.committed, before.committed);
}

/// A failure in `Retrying` or `Replaying` names a claim that is not carried. The carry
/// refuses it by name (`stream_failure`), copies nothing, and leaves the checkpoint
/// unchanged.
#[test]
fn migration_refuses_in_flight_failure_and_names_it() {
    for status in [2_i64, 5] {
        let parent = TempDir::new().unwrap();
        let source = parent.path().join("source");
        fs::create_dir(&source).unwrap();
        dirty_legacy_source(&source);
        give_source_a_persistent_consumer(&source, false);
        let checkpoint_before = stream_checkpoint_rows(&source);
        seed_failure(&source, "failure-in-flight", status);
        let failure_before = failure_table_rows(&source);
        let destination = parent.path().join("migrated");

        let output = run_copy_with_debug(&source, &destination);

        assert!(
            !output.status.success(),
            "status {status} failure must not be dropped: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E2100"), "{stderr}");
        assert!(stderr.contains("tables=stream_failure"), "{stderr}");
        assert!(
            !destination.exists(),
            "no migration copy may be prepared while a failure is in flight"
        );
        assert_eq!(stream_checkpoint_rows(&source), checkpoint_before);
        assert_eq!(failure_table_rows(&source), failure_before);
    }
}

/// Inserts one provider effect receipt into the source's runtime state. Status 2 carries a
/// receipt payload; every other status carries none, matching the table's CHECK.
fn seed_receipt(root: &Path, identity: &[u8], status: i64, receipt: Option<&[u8]>) {
    const INSERT_RECEIPT_FIXTURE: &str = "INSERT INTO stream_provider_effect_receipt (effect_identity, request_digest, idempotency_key, lease_fence, attempts_in_fence, total_attempts, status, receipt) VALUES (?1, ?2, ?3, 1, 1, 1, ?4, ?5)";
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        let receipt = receipt.map_or(libsql::Value::Null, |bytes| {
            libsql::Value::Blob(bytes.to_vec())
        });
        connection
            .execute(
                INSERT_RECEIPT_FIXTURE,
                libsql::params![
                    identity.to_vec(),
                    vec![7_u8; 32],
                    vec![8_u8; 32],
                    status,
                    receipt
                ],
            )
            .await
            .unwrap();
    });
}

/// Every row of the provider effect receipt table, read without opening the runtime.
fn stream_receipt_rows(root: &Path) -> Vec<Vec<libsql::Value>> {
    runtime().block_on(async {
        let database = state_database(root).await;
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query(
                "SELECT * FROM stream_provider_effect_receipt ORDER BY effect_identity",
                (),
            )
            .await
            .unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let width = row.column_count();
            values.push(
                (0..width)
                    .map(|index| row.get_value(index).unwrap())
                    .collect(),
            );
        }
        values
    })
}

/// Settled delivery receipts (statuses 2, 3 and 4) are carried verbatim: the activated copy
/// holds the same rows, and the checkpoint is carried unchanged.
#[test]
fn migration_carries_settled_receipts_verbatim() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, false);
    seed_receipt(&source, b"receipt-delivered", 2, Some(b"delivered-receipt"));
    seed_receipt(&source, b"receipt-abandoned", 3, None);
    seed_receipt(&source, b"receipt-terminal", 4, None);
    let expected = stream_receipt_rows(&source);
    assert_eq!(
        expected.len(),
        3,
        "all three settled receipts must be seeded"
    );
    let destination = parent.path().join("migrated");

    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let activated = run_upgrade(&source, &destination);
    assert!(
        activated.status.success(),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );

    assert_eq!(stream_receipt_rows(&destination), expected);
    let carried = activated_checkpoint(&destination);
    assert_eq!(carried.version, before.version);
    assert_eq!(carried.committed, before.committed);
}

/// A receipt in `Pending` (status 1) names an effect claim that is not carried. The carry
/// refuses it by name (`stream_provider_effect_receipt`), prepares no copy, and leaves the
/// checkpoint and receipt rows unchanged.
#[test]
fn migration_refuses_in_flight_receipt_and_names_it() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    give_source_a_persistent_consumer(&source, false);
    let checkpoint_before = stream_checkpoint_rows(&source);
    seed_receipt(&source, b"receipt-in-flight", 1, None);
    let receipts_before = stream_receipt_rows(&source);
    let destination = parent.path().join("migrated");

    let output = run_copy_with_debug(&source, &destination);

    assert!(
        !output.status.success(),
        "in-flight receipt must not be dropped: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(
        stderr.contains("tables=stream_provider_effect_receipt"),
        "{stderr}"
    );
    assert!(
        !destination.exists(),
        "no migration copy may be prepared while a receipt is in flight"
    );
    assert_eq!(stream_checkpoint_rows(&source), checkpoint_before);
    assert_eq!(stream_receipt_rows(&source), receipts_before);
}
