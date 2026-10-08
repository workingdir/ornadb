//! Offline export stream ordering for one `.orna` fixture. Rows are written in
//! the order given, and the bundle, its listing and its import plan all keep
//! that order.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row for the named `.orna` fixture under `key`.
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
fn exported_rows_keep_the_order_they_were_written_in() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let rows = [
        fixture_row("c", "catalogue-order-ogo1.orna"),
        fixture_row("a", "catalogue-order-ogo1.orna"),
        fixture_row("b", "catalogue-order-ogo1.orna"),
    ];
    let bundle = directory.path().join("bundle");
    write_offline_copy(&bundle, &rows, &[]).unwrap();

    // The index lists rows in the order they were written.
    let index = std::fs::read_to_string(bundle.join("index.tsv")).unwrap();
    let listed: Vec<&str> = index
        .lines()
        .filter(|line| line.starts_with("row\t"))
        .map(|line| line.split('\t').nth(1).unwrap())
        .collect();
    let expected: Vec<String> = ["c", "a", "b"]
        .iter()
        .map(|key| hex(key.as_bytes()))
        .collect();
    assert_eq!(listed, expected);

    // The listing and the import plan keep that same order.
    let copy = OfflineCopy::open(&bundle).unwrap();
    let keys: Vec<&[u8]> = copy.rows().iter().map(|row| row.key.as_slice()).collect();
    assert_eq!(keys, vec![b"c".as_slice(), b"a", b"b"]);
    let plan_keys: Vec<Vec<u8>> = copy
        .import_plan()
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.key)
        .collect();
    assert_eq!(plan_keys, vec![b"c".to_vec(), b"a".to_vec(), b"b".to_vec()]);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
