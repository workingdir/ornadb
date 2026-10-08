//! Offline export path normalization for one `.orna` fixture. A bundle directory
//! written with a trailing slash is the same bundle as one written without it.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row for the named `.orna` fixture.
fn fixture_row(file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: b"route".to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}
