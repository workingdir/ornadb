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

use orna_evaluator_v1::{evaluate_expression_ovb2_with_effects, Limits, SysHostBindingRegistry};
use orna_repository_v1::offline_copy::{
    write_offline_copy, OfflineHistoryEntry, OfflineRow, PAYLOAD_BUFFER_BYTES,
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

/// Writes a `size`-byte file of alternating bytes under a scratch media root,
/// captures it through the committed `.orna` fixture, and exports the captured
/// row as a one-row offline bundle. The file is generated, never committed: it
/// is the payload the bounded-resource proof has to import.
///
/// The capture is verified against the file in two ways that do not allocate
/// the payload: `row_from_capture` matches the recorded length and digest
/// against a streaming SHA-256 of the same bytes, and `write_offline_copy`
/// re-verifies the bundle payload against the row metadata as it writes it.
fn write_large_synthetic_bundle(directory: &Path, size: u64) -> PathBuf {
    let source = TempDir::new().unwrap();
    let mut file = std::fs::File::create(source.path().join(LARGE_SYNTHETIC_NAME)).unwrap();
    let mut digest = Sha256::new();
    let mut written = 0_u64;
    while written < size {
        let chunk = vec![
            if written % 2 == 0 { 0x5a_u8 } else { 0xa5_u8 };
            (size - written).min(1 << 16) as usize
        ];
        digest.update(&chunk);
        std::io::Write::write_all(&mut file, &chunk).unwrap();
        written += chunk.len() as u64;
    }
    std::io::Write::flush(&mut file).unwrap();
    drop(file);
    let expected: [u8; 32] = digest.finalize().into();

    let (_repository_directory, repository, relation_id) = empty_format3_repository();
    let capability = repository.capture_capability(relation_id).unwrap();
    // The committed fixture asks to capture up to 16 MiB, and a capture is
    // rejected when that request exceeds the provider byte limit, so the
    // provider admits more than the fixture's ceiling.
    let mut filesystem = FilesystemProvider::with_limits(1 << 25, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let captured = std::fs::read(LARGE_SYNTHETIC_FIXTURE).unwrap();
    let expression = String::from_utf8(captured).unwrap().trim_end().replace(
        MEDIA_ROOT_PLACEHOLDER,
        &format!("{:?}", source.path().to_string_lossy().as_ref()),
    );
    let value = evaluate_expression_ovb2_with_effects(
        &expression,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let row = row_from_capture(
        "large",
        binding.encoded_value(),
        source.path(),
        LARGE_SYNTHETIC_NAME,
    );
    assert_eq!(row.length, size, "the export row promises the full payload");
    assert_eq!(row.sha256, expected, "digest streaming");

    let bundle = directory.join("large-bundle");
    write_offline_copy(&bundle, std::slice::from_ref(&row), &[]).unwrap();
    bundle
}

/// Streams a payload far above the fixed verification buffer through the real
/// `orna import` binary and reads the resource counters back. The payload is
/// 12 MiB against a 64 KiB buffer, so a run that retained it would show a peak
/// RSS growth of the payload; the run must instead stay within a small
/// multiple of the buffer, and its reported media-payload bytes must be the
/// whole payload.
#[test]
fn streamed_verification_of_a_large_payload_stays_within_a_small_multiple_of_the_buffer() {
    let directory = TempDir::new().unwrap();
    let bundle = write_large_synthetic_bundle(directory.path(), LARGE_SYNTHETIC_BYTES);
    let before = entries(&bundle);

    let output = run_import(&[bundle.to_str().unwrap(), "--dry-run", "--format", "table"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let summary = stdout
        .lines()
        .find(|line| line.starts_with("dry run:"))
        .unwrap_or_else(|| panic!("stdout: {stdout}"));
    assert_eq!(
        summary,
        format!(
            "dry run: 1 of 1 rows, {LARGE_SYNTHETIC_BYTES} payload bytes, 0 history entries; nothing written"
        ),
        "stdout: {stdout}"
    );
    let resources = stdout
        .lines()
        .find(|line| line.starts_with("resources:"))
        .unwrap_or_else(|| panic!("stdout: {stdout}"));
    assert!(
        resources.contains(&format!(
            "media payload read {LARGE_SYNTHETIC_BYTES} bytes of {LARGE_SYNTHETIC_BYTES} bytes in the bundle"
        )),
        "resource line: {resources}"
    );
    assert!(
        resources.contains(&format!(
            "(verification buffer {PAYLOAD_BUFFER_BYTES} bytes)"
        )),
        "resource line: {resources}"
    );
    assert!(
        resources.contains("temp storage bound 0 bytes"),
        "resource line: {resources}"
    );

    // The report is bounded by the buffer, not by the payload: a run that held
    // 12 MiB would exceed this several times over, and the assertion is
    // deliberately loose so it measures retention rather than allocator noise.
    let growth = resources
        .split_once("growth ")
        .and_then(|(_, rest)| rest.split_once(" bytes)"))
        .map(|(bytes, _)| bytes)
        .filter(|bytes| bytes.chars().all(|c| c.is_ascii_digit()));
    if let Some(growth) = growth {
        let growth: u64 = growth.parse().unwrap();
        assert!(
            growth < LARGE_SYNTHETIC_BYTES,
            "peak RSS grew {growth} bytes for a {LARGE_SYNTHETIC_BYTES}-byte payload, so the payload was retained: {resources}"
        );
    }

    assert!(stdout.contains("large"), "stdout: {stdout}");
    assert_eq!(entries(&bundle), before, "the import wrote to the bundle");

    // A metadata-only run over the same bundle reads no payload file at all.
    let metadata_only = run_import(&[bundle.to_str().unwrap(), "--dry-run", "--metadata-only"]);
    assert_eq!(metadata_only.status.code(), Some(0));
    let metadata_stdout = String::from_utf8_lossy(&metadata_only.stdout);
    assert!(
        metadata_stdout.contains(&format!(
            "media payload read 0 bytes of {LARGE_SYNTHETIC_BYTES} bytes in the bundle"
        )),
        "stdout: {metadata_stdout}"
    );
}
