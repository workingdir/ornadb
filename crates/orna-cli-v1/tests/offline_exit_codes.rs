//! Exit-code contract for `orna import --dry-run` on a bundle exported from one
//! `.orna` fixture. Export itself has no CLI; the bundle is written by the
//! library and then read by the real binary.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_repository_v1::offline_copy::{OfflineRow, write_offline_copy};
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Builds an offline row from the named `.orna` fixture, keyed by its name.
fn fixture_row(file: &str) -> OfflineRow {
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
fn export_bundle(directory: &Path, file: &str) -> PathBuf {
    let bundle = directory.join("bundle");
    write_offline_copy(&bundle, &[fixture_row(file)], &[]).unwrap();
    bundle
}

/// Runs `orna-cli-v1 import <bundle> --dry-run`.
fn run_import(bundle: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .arg("import")
        .arg(bundle)
        .arg("--dry-run")
        .output()
        .unwrap()
}

#[test]
fn import_exits_zero_for_a_fixture_bundle_and_one_for_a_corrupt_payload() {
    use tempfile::TempDir;

    let directory = TempDir::new().unwrap();
    let bundle = export_bundle(directory.path(), "catalogue-exit-ogx1.orna");

    let good = run_import(&bundle);
    assert_eq!(
        good.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert!(String::from_utf8_lossy(&good.stdout).contains("dry run: 1 of 1 rows"));

    // Flip one byte of the stored payload so its digest no longer matches.
    let media = std::fs::read_dir(bundle.join("media"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut payload = std::fs::read(&media).unwrap();
    payload[0] ^= 0xff;
    std::fs::write(&media, payload).unwrap();

    let corrupt = run_import(&bundle);
    assert_eq!(
        corrupt.status.code(),
        Some(1),
        "stdout: {}",
        String::from_utf8_lossy(&corrupt.stdout)
    );
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("E2000"));
}
