//! A complete copy opens offline as an Orna repository.
//!
//! Exporting one pinned snapshot freezes its whole object closure — the
//! format-3 store, the published row, and the captured media blob — into one
//! archive. This test reconstructs that archive into a fresh directory, checks
//! that the copy configures no remote, and then opens the copy *through the
//! ordinary repository reader* and reads the committed rows from it.
//!
//! The point is the last assertion: the rows read out of the reconstructed copy
//! are `==` the rows read out of the source repository. The copy therefore
//! serves identical row reads with the source gone and nothing to fetch; a blob
//! that had been left behind would fail the read rather than silently return a
//! partial row.

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
const IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

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
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let raw_payload = std::fs::read(source.path().join("pixel.png")).unwrap();

    // Capture and publish one committed media row, so the snapshot carries a
    // row and the blob its value refers to.
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

    let fixture = std::fs::read_to_string(IMPORT_FIXTURE).unwrap();
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xb0,
        true,
    )
    .await;

    // The row the snapshot must reproduce, read from the source repository
    // while its published snapshot is still the runtime's own.
    let source_rows = committed_rows(&repository, relation_id);
    assert_eq!(source_rows.len(), 1, "the source commits one image row");
    assert!(
        source_rows.iter().any(|row| {
            row.key() == &TypedKey::Bytes(b"image".to_vec())
                || row.key() == &TypedKey::Text("image".to_owned())
        }),
        "the committed row is the image row"
    );
    drop(bindings);
    drop(state);

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

    // The public complete-copy reader selects the recorded snapshot, loads its
    // admitted native row map, and returns stored metadata without hydrating media.
    let range = KeyRange::new(None, None, 16).unwrap();
    let metadata = manifest
        .project_member_blob_metadata(&copy, &manifest.snapshot, relation_id, &range, 0)
        .expect("project metadata through the restored snapshot's row map");
    assert_eq!(metadata.len(), 1);
    assert_eq!(&metadata[0].0, source_rows[0].key());
    assert_eq!(metadata[0].1.length(), raw_payload.len() as u64);
    let expected_hash: [u8; 32] = Sha256::digest(&raw_payload).into();
    assert_eq!(metadata[0].1.sha256(), expected_hash);
    assert!(!metadata[0].1.is_hydrated());
    let format = copy
        .open_format_context()
        .expect("open the restored snapshot");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the restored relation row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the restored native graph");
    let scope = graph.open_read_scope().expect("open a payload read scope");
    let row = graph
        .lookup_row(&metadata[0].0, &scope)
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
        .read_blob_range(&reference, 0..metadata[0].1.length(), &scope)
        .expect("read the archived payload without a remote");
    assert_eq!(restored_payload.bytes(), raw_payload.as_slice());
    assert_eq!(scope.payload_bytes_read(), metadata[0].1.length());
}
