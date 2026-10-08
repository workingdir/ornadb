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
