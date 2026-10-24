//! Gate F: a legacy migration must carry a source stream's consumer checkpoints
//! into the destination, so delivery resumes exactly where the source committed.
//! Stream state that cannot be carried must fail closed and leave the source intact.

use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, ListStreamSource, RuntimeError, RuntimeIdentity,
    RuntimeState, StreamCheckpoint, StreamHandler, StreamHandlerResult, StreamItem,
    StreamMutationBatch, WriterLease, import_stream_carry, read_stream_carry,
    repository_runtime_identity, runtime_identity_for_database_id,
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
/// When `pause` is set, the source also holds a paused stream, which is non-carriable state.
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
                .expect("pausing the stream writes non-carriable stream_control state");
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

/// Delivers the remaining items from the destination's carried checkpoint.
fn deliver_from_destination(root: &Path) -> Vec<Vec<u8>> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = repository_runtime_identity(&repository).unwrap();
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

#[test]
fn migration_refuses_non_carriable_stream_state_and_keeps_checkpoint() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source, true);
    assert_eq!(before.version, 2);
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

        let (identity, digest) = repository_runtime_identity(&destination_repository).unwrap();
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
        let (identity, digest) = repository_runtime_identity(&repository).unwrap();
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
