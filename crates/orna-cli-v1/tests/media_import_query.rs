use std::{path::Path, sync::Arc};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::{KeyRange, Repository, RepositoryCaptureCapability, TypedKey};
use orna_runtime_v1::{
    RequestIdentity, RequestState, RuntimeIdentity, RuntimeState, TableMutation, TerminalOutcome,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::decode_rov3_blob_metadata;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// One committed media row, as its listing query sees it.
#[derive(Debug, PartialEq, Eq)]
struct MediaListing {
    key: Vec<u8>,
    media_type: String,
    length: u64,
    sha256: [u8; 32],
    hydrated: bool,
}

#[tokio::test]
async fn song_and_image_import_commit_through_capture_and_list_without_payloads() {
    let (directory, repository, relation_id) = empty_format3_repository();
    // Import from a scratch copy so the source files can be removed afterwards.
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(name), source.path().join(name)).unwrap();
    }

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x61; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x62; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x63; 16]).await.unwrap();
    let capability = capture_capability(&repository, relation_id);
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let imports = [("song", "tone.wav", 0x70_u8), ("image", "pixel.png", 0x80_u8)];
    for (key, file, ordinal) in imports {
        import_media(
            &state,
            writer,
            &mut bindings,
            source.path(),
            key,
            file,
            ordinal,
        )
        .await;
    }

    // Source files are gone; listing must still answer from committed rows.
    std::fs::remove_file(source.path().join("tone.wav")).unwrap();
    std::fs::remove_file(source.path().join("pixel.png")).unwrap();

    let listing = list_media(&state).await;
    // Rows come back ordered by key: image before song.
    let expected = [
        ("image", "image/png", 73_u64, "pixel.png"),
        ("song", "audio/wav", 4044_u64, "tone.wav"),
    ];
    assert_eq!(listing.len(), expected.len());
    for (row, (key, media_type, length, file)) in listing.iter().zip(expected) {
        assert_eq!(row.key, key.as_bytes());
        assert_eq!(row.media_type, media_type);
        assert_eq!(row.length, length);
        assert!(!row.hydrated, "listing must not hydrate {key}");
        let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
        assert_eq!(row.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    }

    assert_song_revision_history(&repository, &directory, relation_id);

    drop(directory);
}

/// Lists the committed song row's revisions through the OGS-1 walk. Only
/// commit headers and tree listings are read; no blob is opened or pinned.
fn assert_song_revision_history(
    repository: &Repository,
    directory: &TempDir,
    relation_id: [u8; 16],
) {
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let song = rows
        .iter()
        .find(|row| {
            row.key() == &TypedKey::Bytes(b"song".to_vec())
                || row.key() == &TypedKey::Text("song".to_owned())
        })
        .expect("the song row is committed");

    let revisions = graph.list_row_revisions(song, 64, &scope).unwrap();
    let head = git_output(directory.path(), &["rev-parse", "HEAD"], None);
    let head = String::from_utf8(head).unwrap().trim().to_owned();
    // Store install plus one commit per imported row, newest first.
    assert!(revisions.len() >= 3, "history lists every reachable commit");
    assert_eq!(revisions[0].commit().to_hex(), head);
    assert!(revisions[0].present(), "the head revision carries the song");
    assert!(
        !revisions.last().unwrap().present(),
        "the oldest revision predates the import"
    );
    let newest = graph.list_row_revisions(song, 1, &scope).unwrap();
    assert_eq!(newest, revisions[..1]);
}

/// Captures one file through `sys.blob.capture_file` and commits its row in an
/// admitted request. The row stores the annotated Blob reference.
async fn import_media(
    state: &RuntimeState,
    writer: orna_runtime_v1::WriterLease,
    bindings: &mut SysHostBindingRegistry,
    root: &Path,
    key: &str,
    file: &str,
    ordinal: u8,
) {
    let request_identity = RequestIdentity {
        session_id: [ordinal; 16],
        request_id: [ordinal + 1; 16],
    };
    let fingerprint = [ordinal + 2; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();

    let source = format!(
        "sys.blob.capture_file({root:?}, {file:?}, 65536)",
        root = root.to_string_lossy().as_ref()
    );
    let value = evaluate_expression_ovb2_with_effects(
        &source,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let mutation = TableMutation::insert(
        [ordinal + 3; 16],
        "media",
        key.as_bytes().to_vec(),
        Vec::new(),
    )
    .unwrap()
    .with_orp_blob_binding(binding)
    .unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [ordinal + 4; 32],
            TerminalOutcome::new(vec![ordinal + 5]).unwrap(),
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
}

/// Lists every committed media row as payload-free metadata. Rows are decoded
/// as Blob references only; no read or hydration call is made.
async fn list_media(state: &RuntimeState) -> Vec<MediaListing> {
    state
        .committed_table_rows("media")
        .await
        .unwrap()
        .into_iter()
        .map(|(key, row)| {
            let metadata = decode_rov3_blob_metadata(&row).unwrap();
            MediaListing {
                key,
                media_type: metadata.media_type().to_owned(),
                length: metadata.length(),
                sha256: metadata.sha256(),
                hydrated: metadata.is_hydrated(),
            }
        })
        .collect()
}

fn capture_capability(
    repository: &Repository,
    relation_id: [u8; 16],
) -> RepositoryCaptureCapability {
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = Arc::new(format.open_native_graph(&row_map).unwrap());
    let scope = graph.open_read_scope().unwrap();
    RepositoryCaptureCapability::new(graph, scope).unwrap()
}
