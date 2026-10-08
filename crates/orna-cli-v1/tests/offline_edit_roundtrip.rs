use std::path::Path;

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/offline_roundtrip.rs"]
mod offline_roundtrip;
use offline_roundtrip::row_from_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const EDIT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/offline-edit-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

#[test]
fn edited_catalogue_exports_offline_and_reimports_identically() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    // The edit replaces the song row with the image capture from the .orna fixture.
    let fixture = std::fs::read_to_string(EDIT_FIXTURE).unwrap();
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
    let value = evaluate_expression_ovb2_with_effects(
        &expression,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();

    // Export the edited row, then reimport the bundle from its own files.
    let row = row_from_capture("song", binding.encoded_value(), source.path(), "pixel.png");
    let bundle_dir = TempDir::new().unwrap();
    let bundle = bundle_dir.path().join("bundle");
    write_offline_copy(&bundle, std::slice::from_ref(&row), &[]).unwrap();

    let plan = OfflineCopy::open(&bundle).unwrap().import_plan().unwrap();
    assert_eq!(plan.rows.len(), 1, "the export holds exactly the edited row");
    let reimported = &plan.rows[0];
    assert_eq!(reimported.key, row.key);
    assert_eq!(reimported.media_type, row.media_type);
    assert_eq!(reimported.suffix, row.suffix);
    assert_eq!(reimported.length, row.length);
    assert_eq!(reimported.sha256, row.sha256);
    assert_eq!(Some(reimported.payload.clone()), row.payload);

    drop(directory);
}
