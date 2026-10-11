use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_value_v1::{ContextValue, ValueFormat};
use tempfile::TempDir;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(MEDIA_FIXTURES).join(name)
}

fn annotate(directory: &TempDir, source: &Path, media_type: &str) -> (Output, PathBuf) {
    let output_path = directory.path().join("annotated.ovb2");
    let result = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(directory.path())
        .args([
            "annotate",
            source.to_str().unwrap(),
            "--type",
            media_type,
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    (result, output_path)
}

fn assert_fixture_blob(source_name: &str, media_type: &str) {
    let directory = TempDir::new().unwrap();
    let source = fixture(source_name);
    let (result, output_path) = annotate(&directory, &source, media_type);

    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stdout.is_empty(),
        "binary output belongs in the requested file"
    );
    assert!(result.stderr.is_empty());
    let encoded = std::fs::read(output_path).unwrap();
    let blob = ContextValue::decode(&encoded, ValueFormat::Ovb2)
        .unwrap()
        .blob()
        .unwrap();
    assert_eq!(blob.media_type(), media_type);
    assert_eq!(blob.read_to_end().unwrap(), std::fs::read(source).unwrap());
    let entries: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["annotated.ovb2"]);
}

#[test]
fn annotates_real_song_fixture_without_opening_a_repository() {
    assert_fixture_blob("tone.wav", "audio/wav");
}

#[test]
fn annotates_real_image_fixture_without_opening_a_repository() {
    assert_fixture_blob("pixel.png", "image/png");
}

#[test]
fn preserves_existing_destination_and_removes_staging_file() {
    let directory = TempDir::new().unwrap();
    let output_path = directory.path().join("annotated.ovb2");
    std::fs::write(&output_path, b"existing destination").unwrap();

    let (result, output_path) = annotate(&directory, &fixture("tone.wav"), "audio/wav");

    assert!(String::from_utf8_lossy(&result.stderr).contains("could not be installed"));
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        b"existing destination"
    );
    let entries: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, ["annotated.ovb2"]);
}

#[test]
fn rejects_incompatible_suffix_without_creating_output() {
    let directory = TempDir::new().unwrap();
    let output_path = directory.path().join("annotated.ovb2");
    let result = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(directory.path())
        .args([
            "annotate",
            fixture("pixel.png").to_str().unwrap(),
            "--type",
            "image/png",
            "--suffix",
            "jpg",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_ne!(result.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&result.stderr).contains("media annotation is invalid"));
    assert!(!output_path.exists());
}
