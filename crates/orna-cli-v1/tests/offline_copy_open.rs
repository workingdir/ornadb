//! A complete copy opens offline as an Orna repository.
//!
//! Exporting one pinned snapshot freezes its whole object closure — the
//! format-3 store, the published song and image rows, and their media blobs —
//! into one archive. This test reconstructs that archive into a fresh
//! directory with no remote, then reads metadata and raw bytes through the
//! selected committed member's authenticated relation descriptor.

use std::path::Path;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::{
    AdmittedRow, KeyRange, NativeObjectKind, Repository, TypedKey,
    complete_copy::{export_complete_copy, materialize_complete_copy, restore_complete_copy},
};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// Reads every committed row through the ordinary repository reader, exactly
/// as a query does: open the format context, load the relation's row map, open
/// a bounded read scope, and walk the key range.
fn committed_rows(repository: &Repository, relation_id: [u8; 16]) -> Vec<AdmittedRow> {
    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    graph
        .range_rows(&KeyRange::new(None, None, 16).expect("key range"), &scope)
        .expect("read the committed rows")
}

#[tokio::test]
async fn a_restored_complete_copy_opens_offline_and_reads_the_same_rows() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }
    let expected_payloads = [
        (
            TypedKey::Bytes(b"song".to_vec()),
            std::fs::read(source.path().join("tone.wav")).unwrap(),
        ),
        (
            TypedKey::Bytes(b"image".to_vec()),
            std::fs::read(source.path().join("pixel.png")).unwrap(),
        ),
    ];

    // Capture and publish the song and image rows into one pinned snapshot.
    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x61; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x62; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x63; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let capture = |name: &str| format!("sys.blob.capture_file({root}, {name:?}, 65536)");
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("tone.wav"),
        "song",
        0x80,
        true,
    )
    .await;
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("pixel.png"),
        "image",
        0x90,
        true,
    )
    .await;

    let source_rows = committed_rows(&repository, relation_id);
    assert_eq!(
        source_rows.len(),
        2,
        "the source commits song and image rows"
    );
    for (key, _) in &expected_payloads {
        assert!(source_rows.iter().any(|row| row.key() == key));
    }
    drop(bindings);
    drop(state);
    // The source advertises an unavailable remote; restore must carry no remote
    // and all row/blob reads must succeed after the source repository is gone.
    let unavailable_remote = source.path().join("unavailable-origin");
    let unavailable_remote_url = format!("file://{}", unavailable_remote.display());
    git_output(
        directory.path(),
        &["remote", "add", "origin", &unavailable_remote_url],
        None,
    );

    // Freeze the snapshot, then reconstruct it somewhere else entirely.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let manifest = export_complete_copy(&repository, "HEAD", &archive, &[])
        .expect("export the pinned snapshot");

    // The runtime is closed and the source worktree is dropped before the copy
    // is read, so a read that needed the source could not succeed.
    drop(repository);
    drop(directory);

    let restored = outside.path().join("restored");
    let reconstructed = restore_complete_copy(&archive, &restored).expect("restore the copy");
    assert_eq!(
        reconstructed, manifest,
        "the reconstruction matches the recorded manifest"
    );
    materialize_complete_copy(&restored, &reconstructed).expect("materialise the copy");

    // The copy is a repository in its own right with nothing to fetch from.
    assert_eq!(
        String::from_utf8(git_output(&restored, &["remote"], None))
            .unwrap()
            .trim(),
        "",
        "the copy configures no remote"
    );

    // Opening the copy offline reads exactly the rows the source read.
    let copy = Repository::discover(&restored).expect("the copy is a discoverable repository");
    let restored_rows = committed_rows(&copy, relation_id);
    assert_eq!(
        restored_rows, source_rows,
        "the restored copy reads the same committed rows offline"
    );

    // Query only through an authenticated descriptor issued from the selected
    // committed member. The metadata projection reads no media payload bytes.
    let range = KeyRange::new(None, None, 16).unwrap();
    let (_, selected_format) = copy
        .open_pinned_format_context_with_commit(&manifest.snapshot)
        .expect("pin the selected copied member");
    let row_map = selected_format
        .load_row_map(relation_id)
        .expect("load the selected member's committed relation descriptor");
    let metadata = manifest
        .project_member_blob_metadata(&copy, &manifest.snapshot, &row_map, &range, 0)
        .expect("project metadata through the authenticated relation descriptor");
    assert_eq!(metadata.payload_bytes_read, 0);
    assert_eq!(metadata.rows.len(), expected_payloads.len());
    for (key, expected_payload) in &expected_payloads {
        let (_, stored) = metadata
            .rows
            .iter()
            .find(|(row_key, _)| row_key == key)
            .expect("metadata includes each selected media row");
        let expected_hash: [u8; 32] = Sha256::digest(expected_payload).into();
        assert_eq!(stored.length(), expected_payload.len() as u64);
        assert_eq!(stored.sha256(), expected_hash);
        assert!(!stored.is_hydrated());
    }

    // The separate raw read traverses the same restored snapshot. Each payload
    // is compared with its source bytes and the hash returned by metadata.
    let graph = selected_format
        .open_native_graph(&row_map)
        .expect("open the selected member's authenticated graph");
    let scope = graph.open_read_scope().expect("open a payload read scope");
    for (key, expected_payload) in &expected_payloads {
        let row = graph
            .lookup_row(key, &scope)
            .expect("read the restored row")
            .expect("the restored row exists");
        let descriptor = row
            .value()
            .dependencies()
            .into_iter()
            .find(|dependency| dependency.kind() == NativeObjectKind::Tree)
            .expect("the row carries its Blob descriptor")
            .oid()
            .clone();
        let reference = graph
            .admit_blob_reference(&row, &descriptor, &scope)
            .expect("admit the descriptor through the restored row");
        let restored_payload = graph
            .read_blob_range(&reference, 0..expected_payload.len() as u64, &scope)
            .expect("read the archived payload without a remote");
        let (_, stored) = metadata
            .rows
            .iter()
            .find(|(row_key, _)| row_key == key)
            .expect("metadata includes the selected row");
        assert_eq!(restored_payload.bytes(), expected_payload.as_slice());
        let exported_hash: [u8; 32] = Sha256::digest(restored_payload.bytes()).into();
        assert_eq!(exported_hash, stored.sha256());
    }
    let expected_bytes = expected_payloads
        .iter()
        .map(|(_, payload)| payload.len() as u64)
        .sum::<u64>();
    assert_eq!(scope.payload_bytes_read(), expected_bytes);
}
