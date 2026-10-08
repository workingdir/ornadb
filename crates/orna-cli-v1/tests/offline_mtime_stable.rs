//! Offline export stability under source mtime changes for one `.orna`
//! fixture. The fixture is copied before its mtime is changed, so the committed
//! file is never modified. This is payload-level, not a catalogue export.

use std::{
    fs::{self, File},
    path::Path,
    time::{Duration, SystemTime},
};

use orna_repository_v1::offline_copy::OfflineRow;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Copies the named fixture into `directory` and returns the copy's path.
fn copy_fixture(directory: &Path, file: &str) -> std::path::PathBuf {
    let copy = directory.join(file);
    fs::copy(Path::new(FIXTURES).join(file), &copy).unwrap();
    copy
}

/// Sets the file's modification time to `seconds` after the Unix epoch.
fn set_mtime(path: &Path, seconds: u64) {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// Builds the offline row for a copied `.orna` file, keyed by its name.
fn source_row(path: &Path) -> OfflineRow {
    let bytes = fs::read(path).unwrap();
    OfflineRow {
        key: path.file_name().unwrap().as_encoded_bytes().to_vec(),
        media_type: "text/x-orna".to_owned(),
        suffix: Some("orna".to_owned()),
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}
