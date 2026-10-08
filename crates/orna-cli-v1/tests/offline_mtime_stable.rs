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

#[test]
fn offline_export_of_an_orna_fixture_is_stable_under_mtime_changes() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};

    let directory = TempDir::new().unwrap();
    let source = copy_fixture(directory.path(), "catalogue-mtime-ogm1.orna");

    set_mtime(&source, 1_700_000_000);
    let first_row = source_row(&source);
    let first = directory.path().join("first");
    write_offline_copy(&first, std::slice::from_ref(&first_row), &[]).unwrap();

    set_mtime(&source, 1_800_000_000);
    let mtime_after = fs::metadata(&source).unwrap().modified().unwrap();
    assert_eq!(
        mtime_after,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        "the copy's mtime must actually change for this proof to mean anything"
    );
    let second_row = source_row(&source);
    let second = directory.path().join("second");
    write_offline_copy(&second, std::slice::from_ref(&second_row), &[]).unwrap();

    assert_eq!(
        fs::read(first.join("index.tsv")).unwrap(),
        fs::read(second.join("index.tsv")).unwrap()
    );
    let digest = hex_digest(&first_row.sha256);
    assert_eq!(
        fs::read(first.join("media").join(&digest)).unwrap(),
        fs::read(second.join("media").join(&digest)).unwrap()
    );
    let opened = OfflineCopy::open(&second).unwrap();
    assert_eq!(opened.rows()[0].sha256, first_row.sha256);
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
