//! Gate F: a legacy migration must not silently drop a source stream's
//! consumer checkpoint. Checkpoint carry-over is not implemented, so a source
//! holding durable stream state must fail closed and leave its checkpoint intact.

use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, ListStreamSource, RuntimeIdentity, RuntimeState,
    StreamCheckpoint, StreamHandler, StreamHandlerResult, StreamItem, StreamMutationBatch,
    WriterLease,
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
        include_str!("fixtures/format-context/format-1.orna"),
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

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [41; 16],
        repository_id: [42; 16],
    }
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
fn give_source_a_persistent_consumer(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let state = RuntimeState::open(&repository, identity(), [43; 32])
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
        state.stream_checkpoint(&key).await.unwrap()
    })
}

fn checkpoint_after_reopen(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let state = RuntimeState::open(&repository, identity(), [43; 32])
            .await
            .expect("reopen source runtime");
        state.stream_checkpoint(&checkpoint_key()).await.unwrap()
    })
}

fn run_copy(source: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
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

#[test]
fn migration_refuses_source_with_persistent_stream_state_and_keeps_checkpoint() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    dirty_legacy_source(&source);
    let before = give_source_a_persistent_consumer(&source);
    assert_eq!(before.version, 2);
    let destination = parent.path().join("migrated");

    let output = run_copy(&source, &destination);

    assert!(
        !output.status.success(),
        "migration must not drop persistent stream state: {}",
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
