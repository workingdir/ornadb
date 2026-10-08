//! Payload-level reimport identity, round two: one `.orna` fixture's bytes are
//! written into an offline bundle and read back unchanged. Not a repository or
//! catalogue round-trip.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds the offline row for one `.orna` fixture, keyed by its file name.
fn fixture_row(file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: file.as_bytes().to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn orna_fixture_payload_reimports_with_identical_bytes_and_index_round_two() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let row = fixture_row("catalogue-reimport2-ogr2.orna");
    let original = row.payload.clone().unwrap();

    let first = directory.path().join("first");
    write_offline_copy(&first, std::slice::from_ref(&row), &[]).unwrap();
    let plan = OfflineCopy::open(&first).unwrap().import_plan().unwrap();
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(plan.rows[0].payload, original);
    assert_eq!(plan.rows[0].sha256, row.sha256);
    assert_eq!(plan.rows[0].length, row.length);

    let imported = OfflineRow {
        key: plan.rows[0].key.clone(),
        media_type: plan.rows[0].media_type.clone(),
        suffix: plan.rows[0].suffix.clone(),
        length: plan.rows[0].length,
        sha256: plan.rows[0].sha256,
        payload: Some(plan.rows[0].payload.clone()),
    };
    let second = directory.path().join("second");
    write_offline_copy(&second, std::slice::from_ref(&imported), &[]).unwrap();
    assert_eq!(
        std::fs::read(first.join("index.tsv")).unwrap(),
        std::fs::read(second.join("index.tsv")).unwrap()
    );
}
