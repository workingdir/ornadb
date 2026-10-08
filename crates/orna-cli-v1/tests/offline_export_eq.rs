//! Offline export manifest equality for one `.orna` fixture. Exporting the same
//! rows again ("republish") must give an identical bundle, and changing a row's
//! metadata must change the manifest.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row for the named `.orna` fixture with the given media type.
fn fixture_row(file: &str, media_type: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: b"catalogue".to_vec(),
        media_type: media_type.to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn republished_fixture_export_has_an_equal_manifest_and_a_changed_row_does_not() {
    use orna_repository_v1::offline_copy::write_offline_copy;
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let row = fixture_row("catalogue-eq-ogq1.orna", "text/x-orna");

    let first = directory.path().join("first");
    write_offline_copy(&first, std::slice::from_ref(&row), &[]).unwrap();
    let second = directory.path().join("second");
    write_offline_copy(&second, std::slice::from_ref(&row), &[]).unwrap();

    // Republishing the same rows gives a byte-identical manifest and payload.
    let first_index = std::fs::read(first.join("index.tsv")).unwrap();
    assert_eq!(
        first_index,
        std::fs::read(second.join("index.tsv")).unwrap()
    );
    let media = first.join("media").join(hex(&row.sha256));
    assert_eq!(
        std::fs::read(&media).unwrap(),
        std::fs::read(second.join("media").join(hex(&row.sha256))).unwrap()
    );

    // A changed media type is a different manifest.
    let changed_row = fixture_row("catalogue-eq-ogq1.orna", "text/plain");
    let changed = directory.path().join("changed");
    write_offline_copy(&changed, std::slice::from_ref(&changed_row), &[]).unwrap();
    assert_ne!(
        first_index,
        std::fs::read(changed.join("index.tsv")).unwrap()
    );
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
