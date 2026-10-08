//! Offline export path normalization for one `.orna` fixture. A bundle directory
//! written with a trailing slash is the same bundle as one written without it.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row for the named `.orna` fixture.
fn fixture_row(file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: b"route".to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn trailing_slash_export_path_is_the_same_bundle_as_the_plain_path() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let row = fixture_row("catalogue-slash-ogs1.orna");

    // Write through a path with a trailing slash, then open the plain path.
    let with_slash = format!("{}/", directory.path().join("bundle").display());
    write_offline_copy(Path::new(&with_slash), std::slice::from_ref(&row), &[]).unwrap();
    let plain = directory.path().join("bundle");
    let copy = OfflineCopy::open(&plain).unwrap();
    assert_eq!(copy.rows().len(), 1);
    assert_eq!(copy.rows()[0].key, b"route");
    assert_eq!(
        copy.import_plan().unwrap().rows[0].payload,
        row.payload.clone().unwrap()
    );

    // Writing the plain path again must match the trailing-slash bundle's index.
    let second = directory.path().join("second");
    write_offline_copy(&second, std::slice::from_ref(&row), &[]).unwrap();
    assert_eq!(
        std::fs::read(plain.join("index.tsv")).unwrap(),
        std::fs::read(second.join("index.tsv")).unwrap()
    );
}
