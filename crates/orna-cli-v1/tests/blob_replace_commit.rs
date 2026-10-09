//! Replacing an annotated Blob payload through the admitted row boundary:
//! the edited row names a new descriptor for the new bytes, an unedited row is
//! byte-identical, and the edit lands on the published history.

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

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

/// A second, distinct PNG payload (1x1 RGBA, 73 bytes). Same `image/png` media
/// type and same `.png` suffix as `ORIGINAL_PIXEL`, different bytes.
const REPLACEMENT_PIXEL: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\x0aIDATx\x9cc\0\x01\0\0\x05\0\x01\r\n-\xb4\0\0\0\0IEND\xaeB`\x82";

/// The `image` field of one committed row, in its stored raw spelling.
fn image_field(row: &[u8]) -> OvbRaw {
    let value = Value::decode(row).expect("canonical stored row");
    let OvbRaw::Map(fields) = value.raw() else {
        panic!("stored media row is not a record");
    };
    fields
        .iter()
        .find_map(|(key, value)| match key {
            OvbRaw::Text(key) if key == "image" => Some(value.clone()),
            _ => None,
        })
        .expect("the committed row carries its image field")
}

/// The `[descriptor, ...]` tail of a stored Blob field. The native spelling the
/// store writes is the kind-3 tag naming the shared OGS-1 descriptor first.
fn blob_tail(row: &[u8]) -> Box<[OvbRaw]> {
    let OvbRaw::Tag(_, fields) = image_field(row) else {
        panic!("the stored image field is a tagged Blob");
    };
    let OvbRaw::Array(fields) = fields.as_ref() else {
        panic!("the stored Blob tag carries an array");
    };
    fields.clone()
}

/// The OGS-1 descriptor OID the stored Blob names.
fn descriptor_oid(row: &[u8]) -> Vec<u8> {
    match &blob_tail(row)[0] {
        OvbRaw::Bytes(descriptor) => descriptor.clone(),
        other => panic!("the stored Blob names its descriptor first, found {other:?}"),
    }
}

/// The announced byte length of the stored Blob field.
fn announced_length(row: &[u8]) -> u64 {
    match &blob_tail(row)[1] {
        OvbRaw::Int(value) => value.try_into().expect("bounded length"),
        other => panic!("the stored Blob announces its length second, found {other:?}"),
    }
}

fn row_for(rows: &[(Vec<u8>, Vec<u8>)], key: &[u8]) -> &[u8] {
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
fn git_log_subjects(repository: &Repository) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.worktree())
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
    let (_directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::write(source.path().join("pixel.png"), ORIGINAL_PIXEL).unwrap();
    std::fs::write(source.path().join("replacement.png"), REPLACEMENT_PIXEL).unwrap();

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

    // Two annotated rows; only the edited one may move.
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
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &original,
        "other",
        0xb4,
        true,
    )
    .await;
    let before = state.committed_table_rows("media").await.unwrap();
    assert_eq!(before.len(), 2, "the fixture leaves two media rows");
    let original_oid = descriptor_oid(row_for(&before, b"image"));
    let untouched_oid = descriptor_oid(row_for(&before, b"other"));
    assert_eq!(
        announced_length(row_for(&before, b"image")),
        ORIGINAL_PIXEL.len() as u64
    );
    assert_ne!(
        original_oid, untouched_oid,
        "each captured payload is its own descriptor"
    );

    // Replace one row's payload with different bytes of the same media type.
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

    // The edited row names the replacement payload's own descriptor.
    assert_eq!(
        after.len(),
        2,
        "a replacement neither adds nor removes a row"
    );
    let edited_oid = descriptor_oid(row_for(&after, b"image"));
    assert_ne!(
        edited_oid, original_oid,
        "a replaced payload mints its own descriptor"
    );
    assert_eq!(
        announced_length(row_for(&after, b"image")),
        REPLACEMENT_PIXEL.len() as u64,
        "the row announces the replacement payload's length"
    );

    // The row that was not edited is byte-identical, descriptor included.
    assert_eq!(
        row_for(&after, b"other"),
        row_for(&before, b"other"),
        "another row's edit leaves this row untouched"
    );
    assert_eq!(descriptor_oid(row_for(&after, b"other")), untouched_oid);

    // The edit is carried on the published history as its own commit.
    let history = git_log_subjects(&repository);
    assert!(
        history
            .iter()
            .any(|subject| subject.contains("publish image")),
        "history records the publication that carried the edit: {history:?}"
    );
    assert!(
        history
            .iter()
            .any(|subject| subject.contains("publish other")),
        "history records the first row's own publication: {history:?}"
    );
}
