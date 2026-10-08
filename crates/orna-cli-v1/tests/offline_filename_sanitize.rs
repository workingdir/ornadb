//! Offline bundle filename safety for one `.orna` fixture. The row key is hostile
//! (it contains path traversal), so the bundle must still be confined to its own
//! directory. Keys are hex-encoded in the index and media files are named by
//! SHA-256, so the key never becomes a path.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row for the named `.orna` fixture under `key`.
fn fixture_row(key: &[u8], file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: key.to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn hostile_row_key_stays_inside_the_bundle_and_round_trips() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let parent = TempDir::new().unwrap();
    let bundle = parent.path().join("bundle");
    let hostile: &[u8] = b"../../escape/catalogue-filename-ogf1.orna";
    let row = fixture_row(hostile, "catalogue-filename-ogf1.orna");
    write_offline_copy(&bundle, std::slice::from_ref(&row), &[]).unwrap();

    // Nothing was written outside the bundle directory.
    let outside: Vec<_> = std::fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(outside, vec![std::ffi::OsString::from("bundle")]);

    // Every media file is named by its 64-character hex digest, never by the key.
    for entry in std::fs::read_dir(bundle.join("media")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert_eq!(name.len(), 64, "media name must be a digest: {name}");
        assert!(name.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    // The hostile key is preserved exactly and the payload imports unchanged.
    let copy = OfflineCopy::open(&bundle).unwrap();
    assert_eq!(copy.rows()[0].key, hostile);
    let plan = copy.import_plan().unwrap();
    assert_eq!(plan.rows[0].key, hostile);
    assert_eq!(plan.rows[0].payload, row.payload.clone().unwrap());
}
