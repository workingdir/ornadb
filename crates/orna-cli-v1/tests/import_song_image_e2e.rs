//! One import of a real song file and a real image file: both captures commit
//! as annotated OVB-2 Blobs, the committed rows query without reading any
//! media bytes, and re-importing the identical bytes is idempotent - it adds
//! no new object to the repository.

use std::path::Path;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::KeyRange;
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

/// Counts the blob objects the repository holds, loose or packed. Identical
/// bytes resolve to their existing blob, so an idempotent re-import must leave
/// this unchanged. Commits and trees are excluded: publication writes a fresh
/// commit each run, and that is not a content object.
fn blob_count(root: &Path) -> usize {
    let listing = git_output(
        root,
        &["cat-file", "--batch-check", "--batch-all-objects"],
        None,
    );
    String::from_utf8_lossy(&listing)
        .lines()
        .filter(|line| line.split_whitespace().nth(1) == Some("blob"))
        .count()
}

#[tokio::test]
async fn one_import_commits_song_and_image_and_a_reimport_adds_no_object() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x71; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x72; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x73; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let capture = |name: &str| format!("sys.blob.capture_file({root}, {name:?}, 65536)");

    // Import the real song and the real image as two annotated Blobs.
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

    // The rows query from the committed relation alone: no media byte is read.
    let committed = state.committed_table_rows("media").await.unwrap();
    assert_eq!(committed.len(), 2, "the song and the image both commit");
    assert_eq!(committed[0].0, b"image".to_vec());
    assert_eq!(committed[1].0, b"song".to_vec());

    // Publication must leave both loose row files in the committed tree: the
    // explicit boundary is the only thing that advances the repository, and a
    // read of the tree is what a second process sees.
    let tracked = git_output(directory.path(), &["ls-files"], None);
    let tracked = String::from_utf8(tracked).unwrap();
    for row in ["media/song.orna", "media/image.orna"] {
        assert!(tracked.contains(row), "publication commits {row}");
    }

    // Both committed rows must also be reachable through the format-3 row map,
    // which is the store root the read path resolves (`open_format_context` ->
    // `load_row_map` -> `open_native_graph`). A row that lists through the
    // runtime that staged it but is absent from the row map was never projected
    // into the published store, so no other process can see it.
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let published = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .unwrap();
    let published_keys = published
        .iter()
        .map(|row| row.key().clone())
        .collect::<Vec<_>>();
    assert_eq!(
        published.len(),
        2,
        "both imported rows are projected into the published store, got {published_keys:?}"
    );

    let objects = blob_count(directory.path());
    assert!(objects > 0, "the captures committed real objects");

    // Re-import the identical bytes as replacements. The payload resolves to
    // the object already held, so the rows stand and nothing new is written.
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("tone.wav"),
        "song",
        0xa0,
        false,
    )
    .await;
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("pixel.png"),
        "image",
        0xb0,
        false,
    )
    .await;

    let recommitted = state.committed_table_rows("media").await.unwrap();
    assert_eq!(
        recommitted, committed,
        "re-importing identical bytes leaves both rows unchanged"
    );
    assert_eq!(
        blob_count(directory.path()),
        objects,
        "re-importing identical bytes must add no object"
    );

    drop(directory);
}
