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
