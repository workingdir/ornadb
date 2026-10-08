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
