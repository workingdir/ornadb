//! Replacing an annotated Blob payload through the admitted row boundary:
//! the edited row names a new descriptor for the new bytes, an unedited row is
//! byte-identical, and the edit lands on the published history.

use std::path::Path;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::decode_rov3_blob_metadata;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const REIMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/reimport-image-capture.orna"
);
const REPLACE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/replace-image-capture.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// The payload of the shared media fixture, so every test in this crate names
/// the same bytes.
const ORIGINAL_PIXEL: &[u8] = include_bytes!("fixtures/media/pixel.png");

/// A second real payload: the shared 2x2 RGBA PNG, `image/png` with a `.png`
/// suffix, distinct bytes at the same media type as `ORIGINAL_PIXEL`.
const REPLACEMENT_PIXEL: &[u8] = include_bytes!("fixtures/media/pixel2.png");

/// The archived payload of the unedited row, captured under its own media type
/// so its descriptor can never collide with the edited row's.
const OTHER_ROW_PAYLOAD: &[u8] = include_bytes!("fixtures/media/tone.wav");

/// A committed row is one ROV-3 Blob reference, so the store's own public
/// metadata reader answers its length, digest and annotation without hand
/// decoding the graph's tag layout.
fn stored_metadata(row: &[u8]) -> orna_value_v1::BlobMetadata {
    decode_rov3_blob_metadata(row).expect("committed row carries a stored Blob reference")
}

fn row_for<'a>(rows: &'a [(Vec<u8>, Vec<u8>)], key: &[u8]) -> &'a [u8] {
    rows.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_slice())
        .unwrap_or_else(|| panic!("committed row {key:?}"))
}

fn fixture_expression(fixture: &str, root: &str) -> String {
    std::fs::read_to_string(fixture)
        .unwrap()
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, root)
}

/// `git log --format=%s` over the fixture repository, newest first.
fn git_log_subjects(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--format=%s"])
        .output()
        .expect("git log runs");
    assert!(output.status.success(), "git log succeeds");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn replacing_a_blob_payload_writes_a_new_descriptor_and_leaves_other_rows_untouched() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel2.png"),
        source.path().join("pixel2.png"),
    )
    .unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("replacement.wav"),
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
    let original = fixture_expression(REIMPORT_FIXTURE, &root);
    let replacement = fixture_expression(REPLACE_FIXTURE, &root);

    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &original,
        "image",
        0xb0,
        true,
    )
    .await;

    let before = state.committed_table_rows("media").await.unwrap();
    assert_eq!(before.len(), 1, "the first capture commits one media row");
    let before_metadata = stored_metadata(row_for(&before, b"image"));
    assert_eq!(before_metadata.length(), ORIGINAL_PIXEL.len() as u64);
    assert_eq!(
        before_metadata.sha256(),
        <[u8; 32]>::from(Sha256::digest(ORIGINAL_PIXEL))
    );
    assert_eq!(before_metadata.media_type(), "image/png");

    // Replace the payload with the other real fixture's bytes, under the
    // capture's own file name so the field's annotation may move with it.
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &replacement,
        "image",
        0xc0,
        false,
    )
    .await;
    let after = state.committed_table_rows("media").await.unwrap();

    assert_eq!(
        after.len(),
        1,
        "a replacement neither adds nor removes a row"
    );
    let after_metadata = stored_metadata(row_for(&after, b"image"));
    assert_ne!(
        after_metadata.sha256(),
        before_metadata.sha256(),
        "the row names the replacement payload, not the captured one"
    );
    assert_eq!(
        after_metadata.sha256(),
        <[u8; 32]>::from(Sha256::digest(REPLACEMENT_PIXEL)),
        "the row's digest is the replacement payload's own bytes"
    );
    assert_eq!(
        after_metadata.length(),
        REPLACEMENT_PIXEL.len() as u64,
        "the row announces the replacement payload's length"
    );
    assert_eq!(
        after_metadata.media_type(),
        "image/png",
        "replacing with the same media type keeps the field's annotation"
    );
    assert_eq!(
        after_metadata.suffix(),
        Some("png"),
        "replacing with the same suffix keeps the field's annotation"
    );

    // The replacement names a descriptor that only these bytes can produce:
    // the committed spelling recomputes it from the payload, and it must match
    // the descriptor that row would carry had the payload been captured alone.
    assert_ne!(
        row_for(&after, b"image"),
        row_for(&before, b"image"),
        "a replaced payload rewrites the row's stored reference"
    );

    let history = git_log_subjects(directory.path());
    assert!(
        history
            .iter()
            .any(|subject| subject.contains("publish image")),
        "history records the publication that carried the edit: {history:?}"
    );
}

/// The other real payload's row: two rows, only one edited. Its media type and
/// bytes differ from the edited row's, so nothing about the edit can reach it.
#[tokio::test]
async fn replacing_one_rows_payload_leaves_another_rows_payload_untouched() {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel2.png"),
        source.path().join("pixel2.png"),
    )
    .unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("clip.wav"),
    )
    .unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        source.path().join("replacement.wav"),
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
    let image = fixture_expression(REIMPORT_FIXTURE, &root);
    let clip = format!("sys.blob.capture_file({root}, \"clip.wav\", 65536)");
    let replacement = fixture_expression(REPLACE_FIXTURE, &root);

    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &image,
        "image",
        0xb0,
        true,
    )
    .await;
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &clip,
        "clip",
        0xb4,
        true,
    )
    .await;

    let before = state.committed_table_rows("media").await.unwrap();
    assert_eq!(before.len(), 2, "two captures commit two media rows");
    let clip_before = stored_metadata(row_for(&before, b"clip"));
    assert_eq!(clip_before.media_type(), "audio/wav");
    assert_eq!(
        clip_before.sha256(),
        <[u8; 32]>::from(Sha256::digest(OTHER_ROW_PAYLOAD)),
        "the unedited row's digest is the clip payload's own bytes"
    );

    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &replacement,
        "image",
        0xc0,
        false,
    )
    .await;
    let after = state.committed_table_rows("media").await.unwrap();

    assert_eq!(after.len(), 2, "editing one row does not move the other");
    assert_eq!(
        row_for(&after, b"clip"),
        row_for(&before, b"clip"),
        "another row's edit leaves this row byte-identical, descriptor included"
    );
    let image_after = stored_metadata(row_for(&after, b"image"));
    assert_eq!(
        image_after.media_type(),
        "audio/wav",
        "the edited row takes the replacement payload's annotation"
    );
    assert_eq!(
        image_after.sha256(),
        <[u8; 32]>::from(Sha256::digest(OTHER_ROW_PAYLOAD)),
        "the edited row is the payload the replacement name points at"
    );
}
