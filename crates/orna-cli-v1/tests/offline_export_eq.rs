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
