//! One export-import cycle over the committed `pixel.png` fixture: exporting,
//! importing the bundle, and re-exporting the verified payload produces bundles
//! with byte-identical manifests and the expected file list.

use std::path::Path;

use orna_repository_v1::offline_copy::{OfflineCopy, OfflineRow, write_offline_copy};
use orna_value_v1::Blob;
use tempfile::TempDir;

#[path = "support/bundle_manifest.rs"]
mod bundle_manifest;
use bundle_manifest::{bundle_files, bundle_manifest};

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

#[test]
fn one_export_import_cycle_leaves_the_bundle_manifest_identical() {
    let pixel = std::fs::read(Path::new(MEDIA_FIXTURES).join("pixel.png")).unwrap();
    let expected_files: Vec<String> =
        std::fs::read_to_string(Path::new(MEDIA_FIXTURES).join("export-bundle-files.txt"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
    let directory = TempDir::new().unwrap();

    // Export the committed image as one row.
    let blob = Blob::from_bytes_with_annotation(pixel.clone(), "image/png", None).unwrap();
    let row = OfflineRow::from_blob(b"image", &blob).unwrap();
    let first = directory.path().join("first");
    write_offline_copy(&first, std::slice::from_ref(&row), &[]).unwrap();
    assert_eq!(bundle_files(&first), expected_files);
    let first_manifest = bundle_manifest(&first);

    // Import the bundle, then re-export the verified payload.
    let plan = OfflineCopy::open(&first).unwrap().import_plan().unwrap();
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(plan.rows[0].payload, pixel);
    let blob =
        Blob::from_bytes_with_annotation(plan.rows[0].payload.clone(), "image/png", None).unwrap();
    let row = OfflineRow::from_blob(b"image", &blob).unwrap();
    let second = directory.path().join("second");
    write_offline_copy(&second, std::slice::from_ref(&row), &[]).unwrap();

    assert_eq!(bundle_files(&second), expected_files);
    assert_eq!(
        bundle_manifest(&second),
        first_manifest,
        "one export-import cycle leaves the bundle manifest identical"
    );
}
