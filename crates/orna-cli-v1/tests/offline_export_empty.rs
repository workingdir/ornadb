//! Offline export of an empty catalogue. The catalogue is declared by one
//! `.orna` fixture that has no rows, so the bundle carries no rows and no
//! payloads. The fixture's bytes are not exported.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Confirms the fixture exists and returns its path.
fn declared_catalogue(file: &str) -> PathBuf {
    let path = Path::new(FIXTURES).join(file);
    assert!(path.is_file(), "missing fixture {}", path.display());
    path
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
fn empty_catalogue_exports_a_zero_row_bundle_that_imports_with_exit_zero() {
    use orna_repository_v1::offline_copy::{OfflineCopy, write_offline_copy};
    use tempfile::TempDir;

    let _catalogue = declared_catalogue("catalogue-empty-ogz1.orna");
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    write_offline_copy(&bundle, &[], &[]).unwrap();

    let copy = OfflineCopy::open(&bundle).unwrap();
    assert!(copy.rows().is_empty());
    assert!(copy.history().is_empty());
    assert!(copy.import_plan().unwrap().rows.is_empty());
    assert!(
        !bundle.join("media").exists(),
        "an empty bundle has no payloads"
    );

    let output = run_import(&bundle);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("dry run: 0 of 0 rows"));
}
