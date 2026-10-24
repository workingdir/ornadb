//! Legacy format-1 compact repository, committed as a git bundle.
//!
//! The bundle is produced by `examples/build_legacy_fixture.rs` in
//! `orna-repository-v1` (feature `legacy-fixture-writer`). The builder writes the
//! 1.0.0 compact layout with plain files and `git commit`, not through the
//! production publication path, but it shares the canonical OVB encoder, base64
//! helper and Parquet page-checksum helper with production (see the builder's
//! module docs). The bundle bytes are pinned below by SHA-256 so the committed
//! artifact cannot drift silently.
//!
//! This test clones the bundle, checks the pinned digest, reads the committed
//! legacy authoritative tables, verifies the source repository is unchanged by
//! `orna migrate copy` (UPGRADE-006, `reference/Orna-1.1.0/source/40-upgrade-contract.md`
//! line 48), and admits the snapshot through that copy path.

use std::{path::Path, process::Command};

use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::Repository;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
/// SHA-256 of `BUNDLE`. Regenerate with the `build_legacy_fixture` example and
/// `python3 scripts/check_fixture_manifest.py --update` when the builder changes.
const BUNDLE_SHA256: &str = "315b989c25e1bdb46f446cc2531b488a08b0a8c076fa5ec9f3bac04639779c7f";

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn encoded_keys(keys: &[CanonicalValue]) -> Vec<Vec<u8>> {
    let mut encoded = keys
        .iter()
        .map(|key| key.encode().unwrap())
        .collect::<Vec<_>>();
    encoded.sort();
    encoded
}

#[test]
fn committed_legacy_format1_bundle_reads_and_admits_through_migrate_copy() {
    let parent = TempDir::new().unwrap();
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    let digest = Sha256::digest(std::fs::read(&bundle).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(digest, BUNDLE_SHA256, "committed legacy bundle digest changed");

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

    // Table 1 is Int (two segments, four rows); table 2 is Str (three rows);
    // table 3 is Date (three rows, including the negative day -1).
    let mut row_counts = tables
        .iter()
        .map(|table| table.rows().len())
        .collect::<Vec<_>>();
    row_counts.sort_unstable();
    assert_eq!(row_counts, vec![3, 3, 4]);

    let key_sets = tables
        .iter()
        .map(|table| {
            let keys = table
                .rows()
                .iter()
                .map(|row| row.key().clone())
                .collect::<Vec<_>>();
            encoded_keys(&keys)
        })
        .collect::<Vec<_>>();
    let int_keys = encoded_keys(
        &(1..=4)
            .map(|value: i64| CanonicalValue::new(OvbRaw::Int(value.into())).unwrap())
            .collect::<Vec<_>>(),
    );
    let str_keys = encoded_keys(
        &["alpha", "beta", "gamma"]
            .map(|value| CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()),
    );
    assert!(key_sets.contains(&int_keys), "Int keys 1..=4 not found");
    assert!(key_sets.contains(&str_keys), "Str keys alpha/beta/gamma not found");
    for keys in &key_sets {
        let mut distinct = keys.clone();
        distinct.dedup();
        assert_eq!(&distinct, keys, "each table key set is distinct");
    }

    let source_head_before = git(&source, &["rev-parse", "HEAD"]);
    let source_status_before = git(&source, &["status", "--porcelain"]);

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

    assert_eq!(
        git(&source, &["rev-parse", "HEAD"]),
        source_head_before,
        "migrate copy moved the source HEAD"
    );
    assert_eq!(
        git(&source, &["status", "--porcelain"]),
        source_status_before,
        "migrate copy changed the source worktree"
    );

    let migrated = Repository::discover(&destination).unwrap();
    assert!(migrated.holds_prepared_migration());
}
