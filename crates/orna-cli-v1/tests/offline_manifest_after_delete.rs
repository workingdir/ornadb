//! Offline export manifest after deleting the only row, for one `.orna` fixture.
//! There is no delete API: "delete" means the next export omits the row.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds the offline row for the named `.orna` fixture.
fn fixture_row(file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: b"retired".to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn manifest_after_deleting_the_only_row_has_no_row_and_no_orphan_media() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let row = fixture_row("catalogue-manifest-del-ogm2.orna");

    let before = directory.path().join("before");
    write_offline_copy(&before, std::slice::from_ref(&row), &[]).unwrap();
    assert_eq!(OfflineCopy::open(&before).unwrap().rows().len(), 1);

    // "Delete" the only row by exporting without it.
    let after = directory.path().join("after");
    write_offline_copy(&after, &[], &[]).unwrap();
    let index = std::fs::read_to_string(after.join("index.tsv")).unwrap();
    assert!(
        !index.lines().any(|line| line.starts_with("row\t")),
        "manifest still lists a row: {index}"
    );
    assert!(OfflineCopy::open(&after).unwrap().rows().is_empty());

    // No orphan payload is left behind for the deleted row.
    let media_dir = after.join("media");
    let leftover = std::fs::read_dir(&media_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftover, 0, "a deleted row left a payload behind");
}
