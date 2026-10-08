//! Exit-code proof for `orna import --dry-run --type` when the media type cannot
//! be admitted. The value is rejected while parsing arguments, before any bundle
//! row is read, so the binary must exit 1 with E2000 and write nothing to stdout.

use std::{
    path::Path,
    process::{Command, Output},
};

use tempfile::TempDir;

#[path = "support/offline_bundle_fixture.rs"]
mod offline_bundle_fixture;
use offline_bundle_fixture::export_fixture_bundle;

const FIXTURE: &str = "catalogue-unsupported-media-ogx1.orna";

/// Runs `orna-cli-v1 import <bundle> --dry-run --type <media_type>`.
fn run_import(bundle: &Path, media_type: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .arg("import")
        .arg(bundle)
        .arg("--dry-run")
        .arg("--type")
        .arg(media_type)
        .output()
        .unwrap()
}

#[test]
fn unsupported_media_type_exits_one_with_e2000_and_no_report() {
    let directory = TempDir::new().unwrap();
    let bundle = export_fixture_bundle(directory.path(), FIXTURE);

    let rejected = run_import(&bundle, "bad type");
    assert_eq!(
        rejected.status.code(),
        Some(1),
        "stdout: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );
    assert!(
        rejected.stdout.is_empty(),
        "no report may be printed: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("E2000"), "stderr: {stderr}");
    assert!(
        stderr.contains("--type is not a valid media type"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_valid_media_type_on_the_same_bundle_exits_zero() {
    let directory = TempDir::new().unwrap();
    let bundle = export_fixture_bundle(directory.path(), FIXTURE);

    let accepted = run_import(&bundle, "text/plain");
    assert_eq!(
        accepted.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert!(String::from_utf8_lossy(&accepted.stdout).contains("dry run: 1 of 1 rows"));
}
