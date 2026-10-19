//! ORNA-APP-022; `store-format-3.md` §§2 and 5 (row maps and ROV-3 Blob descriptors).
//! The browser profile's media row and Blob survive source admission, format-3
//! publication, and retrieval through the committed store graph.

use std::path::Path;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::TypedKey;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const BROWSER_PROFILE_BLOB: &[u8] = include_bytes!("fixtures/media/pixel.png");

#[tokio::test]
async fn browser_profile_stores_and_retrieves_row_and_blob_from_format3_store() {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("profile.png"),
    )
    .unwrap();

    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
            repository_id: [0x61; 16],
        },
        [0x62; 32],
    )
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
    let capture = format!("sys.blob.capture_file({root}, {:?}, 65536)", "profile.png");

    // The admitted capture commits the row; the helper publishes its staged
    // mutation through the real format-3 graph path.
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture,
        "browser-profile",
        0xb0,
        true,
        relation_id,
    )
    .await;

    // Read the row independently from the published relation graph.
    let rows = committed_store_rows(&repository, relation_id);
    assert_eq!(rows.len(), 1);
    let row = rows
        .iter()
        .find(|row| row.key() == &TypedKey::Text("browser-profile".to_owned()))
        .expect("published browser-profile row is present");
    let blobs = row.blob_fields().expect("the row stores an annotated Blob");
    assert_eq!(blobs.len(), 1);

    // Hydrate the payload only after resolving its stored descriptor from the
    // committed row, using the public native graph range reader.
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let stored_row = graph.lookup_row(row.key(), &scope).unwrap().unwrap();
    let stored_blob = stored_row
        .value()
        .dependencies()
        .iter()
        .find_map(|dependency| {
            if dependency.kind() != orna_repository_v1::NativeObjectKind::Tree {
                return None;
            }
            graph
                .read_stored_blob_value(&stored_row, dependency.oid(), &scope)
                .ok()
        })
        .expect("the published row resolves its Blob descriptor");
    assert_eq!(stored_blob.annotation().media_type(), "image/png");
    assert_eq!(stored_blob.reference().content_identity().length(), BROWSER_PROFILE_BLOB.len() as u64);
    let payload = graph
        .read_blob_range(
            stored_blob.reference(),
            0..BROWSER_PROFILE_BLOB.len() as u64,
            &scope,
        )
        .expect("the committed Blob is retrievable")
        .bytes()
        .to_vec();
    assert_eq!(payload, BROWSER_PROFILE_BLOB);
}
