//! A complete copy opens offline as an Orna repository.
//!
//! Exporting one pinned snapshot freezes its whole object closure — the
//! format-3 store, the published song and image rows, and their captured blobs
//! — into one archive. This test reconstructs that archive into a fresh
//! directory, checks that the copy configures no remote, and then opens the
//! copy through the ordinary repository reader.
//!
//! The point is the last assertion: the rows read out of the reconstructed copy
//! are `==` the rows read out of the source repository. The copy therefore
//! serves identical row reads with the source gone and nothing to fetch; a blob
//! that had been left behind would fail the read rather than silently return a
//! partial row.

use std::{path::Path, process::Command};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::{
    AdmittedRow, FormatContextError, GitHashAlgorithm, KeyRange, NativeOid,
    RangeVerification, Repository, TypedKey,
    complete_copy::{CompleteCopyReadError, export_complete_copy, read_complete_copy_manifest},
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
const SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song.orna"
);
const IMAGE_IMPORT_FIXTURE: &str = concat!(
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
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }
    let expected_payloads = [
        (
            "song",
            std::fs::read(source.path().join("tone.wav")).unwrap(),
        ),
        (
            "image",
            std::fs::read(source.path().join("pixel.png")).unwrap(),
        ),
    ];

    // Capture and publish both checked-in media fixtures in one snapshot.
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
    let song_fixture = std::fs::read_to_string(SONG_IMPORT_FIXTURE).unwrap();
    let song_expression = song_fixture
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, &root);
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &song_expression,
        "song",
        0x80,
        true,
        relation_id,
    )
    .await;
    let image_fixture = std::fs::read_to_string(IMAGE_IMPORT_FIXTURE).unwrap();
    let image_expression = image_fixture
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, &root);
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &image_expression,
        "image",
        0x90,
        true,
        relation_id,
    )
    .await;

    let source_rows = committed_rows(&repository, relation_id);
    assert_eq!(source_rows.len(), expected_payloads.len());
    for (key, _) in &expected_payloads {
        assert!(source_rows.iter().any(|row| {
            row.key() == &TypedKey::Bytes(key.as_bytes().to_vec())
                || row.key() == &TypedKey::Text((*key).to_owned())
        }));
    }
    drop(bindings);
    drop(state);
    // Remove the original media directory and advertise a remote beneath it;
    // export and restore must not need either source.
    let original_media_root = source.path().to_path_buf();
    let unavailable_remote = source.path().join("unavailable-origin");
    let unavailable_remote_url = format!("file://{}", unavailable_remote.display());
    drop(source);
    assert!(!original_media_root.exists(), "the original media directory is unavailable");
    git_output(
        directory.path(),
        &["remote", "add", "origin", &unavailable_remote_url],
        None,
    );

    // Freeze the runtime snapshot, then advance the primary ref so the archive
    // must preserve and later select the earlier media-bearing commit.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let source_snapshot =
        String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_owned();
    git_output(
        directory.path(),
        &[
            "commit",
            "--allow-empty",
            "-m",
            "advance offline copy primary snapshot",
        ],
        None,
    );
    let latest_snapshot =
        String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_owned();
    let export = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(directory.path())
        .args([
            "export",
            archive.to_str().unwrap(),
            "--at",
            &latest_snapshot,
            "--at",
            &source_snapshot,
        ])
        .env("GIT_ALLOW_PROTOCOL", "file")
        .output()
        .unwrap();
    assert_eq!(
        export.status.code(),
        Some(0),
        "offline export failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let manifest = read_complete_copy_manifest(&archive).expect("read the exported manifest");
    assert_eq!(manifest.snapshot, latest_snapshot);
    assert_eq!(
        manifest
            .members
            .iter()
            .map(|member| member.commit.as_str())
            .collect::<Vec<_>>(),
        vec![latest_snapshot.as_str(), source_snapshot.as_str()],
        "the archive records both exact selected source commits"
    );

    // The runtime is closed and the source worktree is dropped before restore,
    // so a read that needed the source could not succeed.
    drop(repository);
    drop(directory);

    let restored = outside.path().join("restored");
    let restore = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(outside.path())
        .args([
            "export",
            archive.to_str().unwrap(),
            "--restore",
            restored.to_str().unwrap(),
            "--at",
            &source_snapshot,
            "--worktree",
        ])
        .env("GIT_ALLOW_PROTOCOL", "file")
        .output()
        .unwrap();
    assert_eq!(
        restore.status.code(),
        Some(0),
        "offline restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    assert!(
        String::from_utf8_lossy(&restore.stdout)
            .contains(&format!("restored {source_snapshot} (2 snapshots)")),
        "selected restore output did not name the archived commit: {}",
        String::from_utf8_lossy(&restore.stdout)
    );
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

    // Obtain an owner-issued pin for the selected archived member. It carries
    // the exact commit identity and repository-bound format context.
    let pin = manifest
        .pin_member(&copy, &source_snapshot)
        .expect("pin the selected copied member");
    assert_eq!(pin.commit().as_str(), source_snapshot);
    let pin_row_map = pin
        .format_context()
        .load_row_map(relation_id)
        .expect("load the selected member's committed relation descriptor");
    let pin_graph = pin
        .format_context()
        .open_native_graph(&pin_row_map)
        .expect("open the selected member's graph");
    let pin_scope = pin_graph.open_read_scope().expect("open a pin read scope");
    for (key, _) in &expected_payloads {
        assert!(
            pin_graph
                .lookup_row(&TypedKey::Text((*key).to_owned()), &pin_scope)
                .expect("read the selected member row")
                .is_some(),
            "the selected member contains {key}"
        );
    }

    let (_, earlier_format) = copy
        .open_pinned_format_context_with_commit("HEAD^")
        .expect("pin the earlier snapshot");
    let earlier_row_map = earlier_format
        .load_row_map(relation_id)
        .expect("load the earlier snapshot's relation descriptor");
    assert!(matches!(
        pin.format_context().open_native_graph(&earlier_row_map),
        Err(FormatContextError::RowMapMismatch)
    ));
    assert!(matches!(
        manifest.pin_member(&copy, "unlisted"),
        Err(CompleteCopyReadError::MemberNotInCopy { .. })
    ));

    // A repository without the exact archived commit cannot issue a pin.
    let unrelated = TempDir::new().unwrap();
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
        manifest.pin_member(&unrelated_repository, &source_snapshot),
        Err(CompleteCopyReadError::FormatContext(
            FormatContextError::SnapshotUnavailable
        ))
    ));
    // The selected snapshot's query reports row metadata without reading its
    // payload; the later explicit Blob read exports the exact captured bytes.
    let selected_snapshot = String::from_utf8(git_output(&restored, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(selected_snapshot, source_snapshot);
    let query = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(&restored)
        .args([
            "query",
            &hex(&relation_id),
            "--at",
            &selected_snapshot,
            "--format",
            "json",
        ])
        .env("GIT_ALLOW_PROTOCOL", "file")
        .output()
        .unwrap();
    assert_eq!(
        query.status.code(),
        Some(0),
        "offline query failed: {}",
        String::from_utf8_lossy(&query.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    let listings = report["listings"].as_array().unwrap();
    assert_eq!(listings.len(), expected_payloads.len());
    for (key, expected_payload) in &expected_payloads {
        let listing = listings
            .iter()
            .find(|listing| listing["key"] == *key)
            .expect("the query reports each captured row");
        let expected_digest: [u8; 32] = Sha256::digest(expected_payload).into();
        assert_eq!(
            listing["length"].as_u64(),
            Some(expected_payload.len() as u64)
        );
        assert_eq!(listing["sha256"], hex(&expected_digest));
    }
    assert_eq!(report["media_payload_bytes_read"].as_u64(), Some(0));

    let format = copy
        .open_pinned_format_context(&selected_snapshot)
        .expect("pin the restored snapshot for explicit Blob export");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load pinned row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open pinned native graph");
    let algorithm = match String::from_utf8(git_output(
        &restored,
        &["rev-parse", "--show-object-format"],
        None,
    ))
    .unwrap()
    .trim()
    {
        "sha1" => GitHashAlgorithm::Sha1,
        "sha256" => GitHashAlgorithm::Sha256,
        other => panic!("unsupported Git object format {other:?}"),
    };
    for (key, expected_payload) in &expected_payloads {
        let scope = graph.open_read_scope().expect("open pinned read scope");
        let row = graph
            .lookup_row(&TypedKey::Text((*key).to_owned()), &scope)
            .expect("look up captured row")
            .expect("captured media row exists");
        let dependency = row
            .value()
            .dependencies()
            .into_iter()
            .next()
            .expect("row retains its captured Blob descriptor");
        let descriptor = NativeOid::new(algorithm, dependency.oid().as_bytes())
            .expect("parse the row's descriptor in the pinned hash mode");
        let stored = graph
            .read_stored_blob_value(&row, &descriptor, &scope)
            .expect("admit the Blob referenced by the row");
        let expected_digest: [u8; 32] = Sha256::digest(expected_payload).into();
        assert_eq!(
            stored.content_identity().length(),
            expected_payload.len() as u64
        );
        assert_eq!(stored.content_identity().sha256(), expected_digest);
        let exported = graph
            .read_blob_range(
                stored.reference(),
                0..stored.content_identity().length(),
                &scope,
            )
            .expect("export captured Blob bytes from the copy");
        assert_eq!(exported.verification(), RangeVerification::FullBlob);
        assert_eq!(exported.bytes(), expected_payload.as_slice());
        assert_eq!(scope.payload_bytes_read(), expected_payload.len() as u64);
    }
}

#[test]
fn complete_copy_rejects_a_valid_unlisted_commit_at_admission() {
    let (directory, repository, _) = empty_format3_repository();
    let root = directory.path();
    let archive = root.join("archive");
    let manifest = export_complete_copy(&repository, "HEAD", &archive, &[])
        .expect("export the listed snapshot");

    git(
        root,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "unlisted complete-copy member",
        ],
    );
    let unlisted = String::from_utf8(git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(unlisted, manifest.snapshot);
    assert!(matches!(
        manifest.pin_member(&repository, &unlisted),
        Err(CompleteCopyReadError::MemberNotInCopy { .. })
    ));
}
