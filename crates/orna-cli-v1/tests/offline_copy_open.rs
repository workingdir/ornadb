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
    AdmittedRow, FormatContextError, KeyRange, NativeObjectKind, Repository, TypedKey,
    complete_copy::{
        CompleteCopyReadError, export_complete_copy, materialize_complete_copy,
        restore_complete_copy,
    },
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
    let capture_fixture = |name: &str| {
        std::fs::read_to_string(Path::new(MEDIA_FIXTURES).join(name))
            .unwrap()
            .replace("__MEDIA_ROOT__", &root)
    };
    let song_capture = capture_fixture("import-song.orna");
    let image_capture = capture_fixture("import-image.orna");
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &song_capture,
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
        &image_capture,
        "image",
        0x90,
        true,
    )
    .await;

    let source_rows = committed_rows(&repository, relation_id);
    assert_eq!(
        source_rows.len(),
        expected_payloads.len(),
        "the source producer must commit both media rows before export",
    );
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

    // Obtain an owner-issued pin for the archive member. It carries both the
    // exact commit identity and the repository-bound format context.
    let pin = manifest
        .pin_member(&copy, &manifest.snapshot)
        .expect("pin the selected copied member");
    assert_eq!(pin.commit().as_str(), manifest.snapshot.as_str());
    let row_map = pin
        .format_context()
        .load_row_map(relation_id)
        .expect("load the selected member's committed relation descriptor");

    let (_, earlier_format) = copy
        .open_pinned_format_context_with_commit("HEAD^")
        .expect("pin the earlier snapshot");
    let earlier_row_map = earlier_format
        .load_row_map(relation_id)
        .expect("load the earlier snapshot's relation descriptor");
    // A row map from another snapshot cannot be paired with this member pin.
    assert!(matches!(
        pin.format_context().open_native_graph(&earlier_row_map),
        Err(FormatContextError::RowMapMismatch)
    ));

    // Unlisted commits and repositories without the exact archived commit
    // cannot issue a member pin.
    let unrelated = TempDir::new().unwrap();
    assert!(matches!(
        manifest.pin_member(&copy, "unlisted"),
        Err(CompleteCopyReadError::MemberNotInCopy { .. })
    ));
    git(unrelated.path(), &["init", "--quiet"]);
    git(
        unrelated.path(),
        &["config", "user.email", "offline-copy@example.invalid"],
    );
    git(
        unrelated.path(),
        &["config", "user.name", "Offline Copy Test"],
    );
    std::fs::write(unrelated.path().join("unrelated"), b"not the copied member").unwrap();
    git(unrelated.path(), &["add", "unrelated"]);
    git(unrelated.path(), &["commit", "--quiet", "-m", "unrelated"]);
    let unrelated_repository = Repository::discover(unrelated.path()).unwrap();
    assert!(matches!(
        manifest.pin_member(&unrelated_repository, &manifest.snapshot),
        Err(CompleteCopyReadError::FormatContext(
            FormatContextError::SnapshotUnavailable
        ))
    ));

    let graph = pin
        .format_context()
        .open_native_graph(&row_map)
        .expect("open the selected member's authenticated graph");
    let scope = graph.open_read_scope().expect("open a payload read scope");
    for (key, expected_payload) in &expected_payloads {
        let row = graph
            .lookup_row(key, &scope)
            .expect("read the restored row")
            .expect("the restored row exists");
        let blob_descriptors = row
            .value()
            .dependencies()
            .into_iter()
            .filter(|dependency| dependency.kind() == NativeObjectKind::Tree)
            .collect::<Vec<_>>();
        assert_eq!(
            blob_descriptors.len(),
            1,
            "each fixture row has exactly one payload Blob",
        );
        let descriptor = blob_descriptors[0].oid().clone();
        let stored_blob = graph
            .read_stored_blob_value(&row, &descriptor, &scope)
            .expect("resolve the Blob value through the restored row");
        let identity = stored_blob.content_identity();
        let expected_hash: [u8; 32] = Sha256::digest(expected_payload).into();
        assert_eq!(identity.length(), expected_payload.len() as u64);
        assert_eq!(identity.sha256(), expected_hash);
        assert_eq!(scope.payload_bytes_read(), 0);

        let restored_payload = graph
            .read_blob_range(stored_blob.reference(), 0..identity.length(), &scope)
            .expect("read the archived payload without a remote");
        assert_eq!(restored_payload.bytes(), expected_payload.as_slice());
        let exported_hash: [u8; 32] = Sha256::digest(restored_payload.bytes()).into();
        assert_eq!(exported_hash, expected_hash);
    }
    let expected_bytes = expected_payloads
        .iter()
        .map(|(_, payload)| payload.len() as u64)
        .sum::<u64>();
    assert_eq!(scope.payload_bytes_read(), expected_bytes);
}
