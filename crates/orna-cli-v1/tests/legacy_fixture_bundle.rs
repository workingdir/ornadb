//! Golden legacy format-1 compact repository, committed as a git bundle.
//!
//! The bundle is produced by `examples/build_legacy_fixture.rs` in
//! `orna-repository-v1` (feature `legacy-fixture-writer`). This test clones it,
//! reads the committed legacy authoritative tables, and admits it through
//! `orna migrate copy`, the path that consumes a format-1 snapshot.

use std::{path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";

#[test]
fn committed_legacy_format1_bundle_reads_and_admits_through_migrate_copy() {
    let parent = TempDir::new().unwrap();
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    let source = parent.path().join("source");
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(&bundle)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );

    let repository = Repository::discover(&source).unwrap();
    let head = repository.head().unwrap().expect("committed legacy HEAD");
    let tables = repository.read_legacy_authoritative_tables(&head).unwrap();

    // Int: two segments of two rows (4); Str: one segment of three rows (3);
    // Date: one segment of three rows (3).
    let mut row_counts = tables
        .iter()
        .map(|table| table.rows().len())
        .collect::<Vec<_>>();
    row_counts.sort_unstable();
    assert_eq!(row_counts, vec![3, 3, 4]);

    let destination = parent.path().join("migrated");
    let copy = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            "0",
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap();
    assert!(
        copy.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&copy.stderr)
    );

    let migrated = Repository::discover(&destination).unwrap();
    assert!(migrated.holds_prepared_migration());
}
