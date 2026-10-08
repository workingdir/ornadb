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
