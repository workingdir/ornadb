use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const LEGACY_FORMATS: [(&str, u8); 2] = [
    (include_str!("fixtures/format-context/format-1.orna"), 1),
    (include_str!("fixtures/format-context/format-2.orna"), 2),
];
const FORMAT_THREE_DATABASE: &str =
    include_str!("fixtures/format-context/database-final.orna");

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository_with_legacy_format(format: &str) -> (TempDir, String) {
    const HISTORICAL_VALUE: &str = "historical value";
    let directory = TempDir::new().expect("create repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(directory.path(), &["config", "user.name", "Orna tests"]);
    git(directory.path(), &["config", "user.email", "tests@example.invalid"]);
    fs::create_dir_all(directory.path().join(".orna/store")).unwrap();
    fs::write(directory.path().join(".orna/format.orna"), format).unwrap();
    fs::write(directory.path().join(".orna/store/root"), HISTORICAL_VALUE).unwrap();
    fs::write(directory.path().join("main.orna"), "// source at historical commit
").unwrap();
    git(directory.path(), &["add", "."]);
    git(directory.path(), &["commit", "--quiet", "-m", "historical format"]);
    let old_commit = String::from_utf8(
        Command::new("git")
            .current_dir(directory.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    fs::remove_file(directory.path().join(".orna/format.orna")).unwrap();
    fs::write(
        directory.path().join(".orna/database.orna"),
        FORMAT_THREE_DATABASE,
    )
    .unwrap();
    fs::write(directory.path().join(".orna/store/root"), "current value").unwrap();
    git(directory.path(), &["add", "--all"]);
    git(directory.path(), &["commit", "--quiet", "-m", "format 3 current snapshot"]);
    (directory, old_commit)
}

#[test]
fn req_evol_003_historical_snapshot_uses_its_recorded_decoder_and_value() {
    // ORNA-EVOL-003: historical rows are interpreted with the schema/value
    // profile recorded by that snapshot, never the current source checkout.
    for (legacy_format, format_number) in LEGACY_FORMATS {
        let (directory, old_commit) = repository_with_legacy_format(legacy_format);
        let repository = Repository::discover(directory.path()).expect("discover repository");
        let historical = repository
            .open_format_context_at_selector(&old_commit)
            .expect("open the historical snapshot");
        assert_eq!(historical.repository_format_number(), format_number);
        assert_eq!(
            historical.read_committed_file(".orna/store/root", 64).unwrap(),
            b"historical value"
        );
        let current = repository.open_format_context().expect("open current snapshot");
        assert_eq!(current.repository_format_number(), 3);
        assert_eq!(
            current.read_committed_file(".orna/store/root", 64).unwrap(),
            b"current value"
        );
    }
}
