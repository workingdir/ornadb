//! Offline export row counts after a row is removed from the source. There is
//! no delete API, so "delete" means the next export omits the row. This is
//! payload-level: both rows share one `.orna` fixture's bytes.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row under `key` from the named `.orna` fixture.
fn fixture_row(key: &str, file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: key.as_bytes().to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn offline_export_count_drops_after_a_row_is_removed_and_keeps_shared_media() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let kept = fixture_row("kept", "catalogue-delete-ogd1.orna");
    let removed = fixture_row("removed", "catalogue-delete-ogd1.orna");

    let before = directory.path().join("before");
    write_offline_copy(&before, &[kept.clone(), removed], &[]).unwrap();
    let before_copy = OfflineCopy::open(&before).unwrap();
    assert_eq!(before_copy.rows().len(), 2);
    assert!(before_copy.metadata(b"removed").is_some());

    // "Delete" the row by exporting without it.
    let after = directory.path().join("after");
    write_offline_copy(&after, std::slice::from_ref(&kept), &[]).unwrap();
    let after_copy = OfflineCopy::open(&after).unwrap();
    assert_eq!(after_copy.rows().len(), 1);
    assert!(after_copy.metadata(b"removed").is_none());
    assert_eq!(after_copy.metadata(b"kept").unwrap().sha256, kept.sha256);

    // Both rows shared one payload file, which the remaining row still needs.
    let media: Vec<_> = std::fs::read_dir(after.join("media")).unwrap().collect();
    assert_eq!(media.len(), 1);
    assert_eq!(after_copy.import_plan().unwrap().rows.len(), 1);
}
