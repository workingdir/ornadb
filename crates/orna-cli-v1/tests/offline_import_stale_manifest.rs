//! Offline import against a stale manifest for one `.orna` fixture. The index
//! records a length that no longer matches the stored payload, and the import
//! must reject the whole bundle.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds the offline row for the named `.orna` fixture.
fn fixture_row(file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: b"current".to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn import_rejects_a_bundle_whose_index_records_a_stale_length() {
    use orna_repository_v1::offline_copy::{OfflineCopy, OfflineCopyError, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    write_offline_copy(&bundle, &[fixture_row("catalogue-stale-ogt1.orna")], &[]).unwrap();
    assert!(OfflineCopy::open(&bundle).unwrap().import_plan().is_ok());

    // Make the index claim one byte more than the stored payload holds.
    let index_path = bundle.join("index.tsv");
    let index = std::fs::read_to_string(&index_path).unwrap();
    let stale: String = index
        .lines()
        .map(|line| {
            if line.starts_with("row\t") {
                let mut fields: Vec<&str> = line.split('\t').collect();
                let stale_length = (fields[4].parse::<u64>().unwrap() + 1).to_string();
                fields[4] = &stale_length;
                fields.join("\t") + "\n"
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    std::fs::write(&index_path, stale).unwrap();

    let result = OfflineCopy::open(&bundle).unwrap().import_plan();
    assert!(
        matches!(result, Err(OfflineCopyError::PayloadMismatch { .. })),
        "a stale manifest must be rejected as a payload mismatch"
    );
}
