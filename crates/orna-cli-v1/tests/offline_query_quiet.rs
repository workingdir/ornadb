//! `--quiet` proof for `orna import --dry-run`. Without `--quiet` the command
//! prints one progress line per verified row on stderr and the dry-run report
//! on stdout. With `--quiet` both are suppressed, the exit status stays 0 for a
//! good bundle, and a corrupt bundle still fails with exit 1 and `E2000`.

use std::path::Path;

use tempfile::TempDir;

#[path = "support/offline_bundle_fixture.rs"]
mod offline_bundle_fixture;
use offline_bundle_fixture::{export_fixture_bundle, run_import_dry_run};

const FIXTURE: &str = "catalogue-query-quiet-ogx1.orna";

/// Flips one byte of the stored payload so its digest no longer matches.
fn corrupt_payload(bundle: &Path) {
    let media = std::fs::read_dir(bundle.join("media"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut payload = std::fs::read(&media).unwrap();
    payload[0] ^= 0xff;
    std::fs::write(&media, payload).unwrap();
}

#[test]
fn quiet_suppresses_progress_and_report_on_a_good_bundle() {
    let directory = TempDir::new().unwrap();
    let bundle = export_fixture_bundle(directory.path(), FIXTURE);

    let loud = run_import_dry_run(&bundle, &[]);
    assert_eq!(loud.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&loud.stdout).contains("dry run: 1 of 1 rows"));
    assert!(String::from_utf8_lossy(&loud.stderr).contains("verified"));

    let quiet = run_import_dry_run(&bundle, &["--quiet"]);
    assert_eq!(quiet.status.code(), Some(0));
    assert!(
        quiet.stdout.is_empty(),
        "--quiet must print no report: {}",
        String::from_utf8_lossy(&quiet.stdout)
    );
    assert!(
        quiet.stderr.is_empty(),
        "--quiet must write no progress: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
}

#[test]
fn quiet_still_fails_a_corrupt_bundle_with_exit_one() {
    let directory = TempDir::new().unwrap();
    let bundle = export_fixture_bundle(directory.path(), FIXTURE);
    corrupt_payload(&bundle);

    let quiet = run_import_dry_run(&bundle, &["--quiet"]);
    assert_eq!(
        quiet.status.code(),
        Some(1),
        "stdout: {}",
        String::from_utf8_lossy(&quiet.stdout)
    );
    assert!(String::from_utf8_lossy(&quiet.stderr).contains("E2000"));
}
