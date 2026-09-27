//! ORNA-PUB-004 runtime publication metadata and completion accounting.

use std::{path::PathBuf, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{Mutation, NoFault, PublicationCommitId, RuntimeIdentity, RuntimeState};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

const PUBLICATION_METADATA_SOURCE: &str = include_str!("fixtures/publication_metadata.orna");

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-publication-metadata-")
        .tempdir_in(target)
        .expect("create repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize repository");
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn mutation(id: u8, payload: &[u8]) -> Mutation {
    Mutation {
        id: [id; 16],
        payload: payload.to_vec(),
        digest: Sha256::digest(payload).into(),
    }
}

#[tokio::test]
async fn publication_metadata_tracks_frozen_prefix_and_matches_both_projections() {
    assert!(PUBLICATION_METADATA_SOURCE.contains("storage_published_rows"));
    assert!(PUBLICATION_METADATA_SOURCE.contains("maintenance_published_rows"));
    assert!(PUBLICATION_METADATA_SOURCE.contains("publication_policy.compressed_target_bytes"));

    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    // Exercise the checked-in Orna source as the durable mutation itself, so
    // this proof covers a real source payload rather than a Rust-only marker.
    let published_mutation = mutation(5, PUBLICATION_METADATA_SOURCE.as_bytes());
    let first_capture = state.capture().await.expect("capture before mutation");
    state
        .commit(
            writer,
            &first_capture,
            &published_mutation,
            [6; 32],
            &NoFault,
        )
        .await
        .expect("append frozen mutation");
    let freeze = state
        .freeze(
            [7; 16],
            &state
                .latest_checkpoint()
                .await
                .expect("read frozen checkpoint")
                .expect("frozen checkpoint exists"),
        )
        .await
        .expect("freeze pending prefix");

    let trailing_mutation = mutation(8, b"trailing-payload");
    let capture = state.capture().await.expect("capture before tail");
    state
        .commit(writer, &capture, &trailing_mutation, [9; 32], &NoFault)
        .await
        .expect("append mutation after freeze");

    let pending_storage = state
        .sys_storage_publication_metadata()
        .await
        .expect("read sys.Storage metadata");
    let pending_maintenance = state
        .runtime_maintenance_publication_metadata()
        .await
        .expect("read maintenance metadata");
    assert_eq!(pending_storage, pending_maintenance);
    assert_eq!(
        pending_storage.publication_policy.compressed_target_bytes,
        16 * 1024 * 1024
    );
    assert_eq!(
        pending_storage.publication_policy.max_file_bytes,
        64 * 1024 * 1024
    );
    assert_eq!(
        pending_storage.publication_policy.max_pending_age_seconds,
        60
    );
    assert_eq!(pending_storage.pending_rows, 2);
    assert_eq!(
        pending_storage.pending_bytes,
        (published_mutation.payload.len() + trailing_mutation.payload.len()) as u64
    );
    assert_eq!(pending_storage.published_rows, 0);
    assert_eq!(pending_storage.published_bytes, 0);
    assert_eq!(pending_storage.last_publication_ms, None);

    let commit = PublicationCommitId::new(vec![b'a'; 40]).expect("valid commit id");
    state
        .complete_publication(&freeze, &commit)
        .await
        .expect("complete publication");
    // A replayed successful completion must not double-count the prefix.
    state
        .complete_publication(&freeze, &commit)
        .await
        .expect("retry publication completion");

    let published_storage = state
        .sys_storage_publication_metadata()
        .await
        .expect("read published sys.Storage metadata");
    let published_maintenance = state
        .runtime_maintenance_publication_metadata()
        .await
        .expect("read published maintenance metadata");
    assert_eq!(published_storage, published_maintenance);
    assert_eq!(published_storage.pending_rows, 1);
    assert_eq!(
        published_storage.pending_bytes,
        trailing_mutation.payload.len() as u64
    );
    assert_eq!(published_storage.published_rows, 1);
    assert_eq!(
        published_storage.published_bytes,
        published_mutation.payload.len() as u64
    );
    assert!(published_storage.last_publication_ms.is_some());

    drop(state);
    let reopened = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("reopen runtime");
    let reopened_storage = reopened
        .sys_storage_publication_metadata()
        .await
        .expect("read persisted sys.Storage metadata");
    let reopened_maintenance = reopened
        .runtime_maintenance_publication_metadata()
        .await
        .expect("read persisted maintenance metadata");
    assert_eq!(reopened_storage, reopened_maintenance);
    assert_eq!(reopened_storage, published_storage);
    assert_eq!(reopened.pending().await.expect("read pending tail"), vec![trailing_mutation]);
}
