//! Writes one-row offline bundles whose payload is a `.orna` fixture.

use std::path::{Path, PathBuf};

use orna_repository_v1::offline_copy::{OfflineRow, write_offline_copy};
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row from the named `.orna` fixture, keyed by its name.
pub fn fixture_row(file: &str) -> OfflineRow {
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

/// Writes a one-row bundle for `file` under `directory` and returns its path.
pub fn export_fixture_bundle(directory: &Path, file: &str) -> PathBuf {
    let bundle = directory.join("bundle");
    write_offline_copy(&bundle, &[fixture_row(file)], &[]).unwrap();
    bundle
}
