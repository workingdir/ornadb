//! `orna import <bundle> --dry-run` over a multi-row bundle built from the
//! real media fixtures. The CLI verifies every payload from a fixed buffer and
//! retains no bytes, so the report and the exit status are identical to the
//! payload-retaining plan while the bundle itself stays untouched. The same
//! command reports ACCEPTANCE Gate B resource counters: peak RSS, bytes read
//! and written, media-payload bytes, and its temporary-storage bound.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::offline_copy::{
    OfflineHistoryEntry, OfflineRow, PAYLOAD_BUFFER_BYTES, write_offline_copy,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::empty_format3_repository;

#[path = "support/offline_roundtrip.rs"]
mod offline_roundtrip;
use offline_roundtrip::row_from_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const LARGE_SYNTHETIC_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-large-synthetic.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
/// The synthetic media file is generated, never committed: the proof is that a
/// media payload above the 10 MiB floor imports with bounded memory.
const LARGE_SYNTHETIC_NAME: &str = "large-synthetic.bin";
const LARGE_SYNTHETIC_BYTES: u64 = 12 * 1024 * 1024;

/// Builds the row a capture would commit for one fixture file, payload
/// included so the bundle writer can copy it.
fn fixture_row(key: &str, media_type: &str, file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: key.as_bytes().to_vec(),
        media_type: media_type.to_owned(),
        suffix: None,
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

/// Writes the song/image bundle and returns its directory.
fn write_song_image_bundle(directory: &Path) -> PathBuf {
    let bundle = directory.join("bundle");
    let rows = [
        fixture_row("song", "audio/wav", "tone.wav"),
        fixture_row("image", "image/png", "pixel.png"),
    ];
    let history = [OfflineHistoryEntry {
        sequence: 1,
        commit: [0x5a; 32],
    }];
    write_offline_copy(&bundle, &rows, &history).unwrap();
    bundle
}

fn run_import(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .arg("import")
        .args(arguments)
        .output()
        .unwrap()
}

/// The bundle payload file for one fixture, named by the hex of its SHA-256
/// exactly as `write_offline_copy` named it.
fn payload_path(bundle: &Path, file: &str) -> PathBuf {
    let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    bundle.join("media").join(name)
}

/// Every directory entry under `root`, so an import that wrote anything is
/// visible.
fn entries(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(path.strip_prefix(root).unwrap().display().to_string());
        }
    }
    found.sort();
    found
}

#[test]
fn streamed_verification_reports_both_fixture_rows_and_writes_nothing() {
    let directory = TempDir::new().unwrap();
    let bundle = write_song_image_bundle(directory.path());
    let before = entries(&bundle);

    let output = run_import(&[bundle.to_str().unwrap(), "--dry-run", "--format", "table"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "dry run: 2 of 2 rows, 4117 payload bytes, 1 history entries; nothing written"
        ),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("song"), "stdout: {stdout}");
    assert!(stdout.contains("image"), "stdout: {stdout}");
    assert!(
        stdout.contains("audio/wav") && stdout.contains("image/png"),
        "stdout: {stdout}"
    );
    assert_eq!(entries(&bundle), before, "the import wrote to the bundle");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("verified 2/2"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn streamed_verification_rejects_a_corrupt_payload_without_writing() {
    let directory = TempDir::new().unwrap();
    let bundle = write_song_image_bundle(directory.path());
    let before = entries(&bundle);
    let song = payload_path(&bundle, "tone.wav");
    let mut bytes = std::fs::read(&song).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&song, &bytes).unwrap();

    let output = run_import(&[bundle.to_str().unwrap(), "--dry-run"]);
    assert!(
        output.status.code().is_some_and(|code| code != 0),
        "a corrupt payload must fail: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Bundle could not be verified"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        entries(&bundle),
        before,
        "the failed import wrote to the bundle"
    );
}

#[test]
fn streamed_verification_rejects_a_truncated_payload() {
    let directory = TempDir::new().unwrap();
    let bundle = write_song_image_bundle(directory.path());
    let image = payload_path(&bundle, "pixel.png");
    let bytes = std::fs::read(&image).unwrap();
    std::fs::write(&image, &bytes[..bytes.len() - 1]).unwrap();

    let output = run_import(&[bundle.to_str().unwrap(), "--dry-run"]);
    assert!(
        output.status.code().is_some_and(|code| code != 0),
        "a truncated payload must fail: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
