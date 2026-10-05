use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(database: Option<&str>, legacy_format: Option<&str>, store: bool) -> TempDir {
    let directory = TempDir::new().expect("create format context repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(
        directory.path(),
        &["config", "user.name", "format-context-test"],
    );
    git(
        directory.path(),
        &["config", "user.email", "format-context@example.invalid"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(directory.path().join("main.orna"), "").expect("write source root");
    if let Some(database) = database {
        fs::create_dir_all(directory.path().join(".orna")).expect("create metadata directory");
        fs::write(directory.path().join(".orna/database.orna"), database)
            .expect("write database metadata");
    }
    if let Some(legacy_format) = legacy_format {
        fs::create_dir_all(directory.path().join(".orna")).expect("create legacy directory");
        fs::write(directory.path().join(".orna/format.orna"), legacy_format)
            .expect("write legacy metadata");
    }
    if store {
        fs::create_dir_all(directory.path().join(".orna/store")).expect("create store root");
        fs::write(directory.path().join(".orna/store/data"), "root")
            .expect("write store root marker");
    }
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "format context fixture"],
    );
    directory
}

#[test]
fn admits_final_format_three_metadata_and_keeps_root_seams_pinned() {
    let directory = repository(
        Some(&format!(
            "{{repository_format: 3, database_id: \"{DATABASE_ID}\"}}\n"
        )),
        None,
        true,
    );
    let repository = Repository::discover(directory.path()).expect("discover repository");
    let context = repository
        .open_format_context()
        .expect("admit final format metadata");

    assert_eq!(context.repository_format_number(), 3);
    assert_eq!(context.database_id().unwrap().to_string(), DATABASE_ID);
    assert!(!context.is_read_only());
    assert!(context.supports_writes());
    assert!(context.validate_schema_root().is_ok());
    assert!(context.validate_store_root().is_ok());
    assert_eq!(
        format!("{:?}", context.snapshot_pin()),
        "RepositorySnapshotPin { .. }"
    );
}

#[test]
fn dispatches_legacy_formats_as_explicit_read_only_inputs() {
    let format_one = repository(None, Some("format 1\n"), false);
    let context_one = Repository::discover(format_one.path())
        .unwrap()
        .open_format_context()
        .unwrap();
    assert_eq!(context_one.repository_format_number(), 1);
    assert!(context_one.is_read_only());
    assert!(!context_one.supports_writes());
    assert!(context_one.database_id().is_none());

    let format_two = repository(
        Some(&format!("{{database_id: \"{DATABASE_ID}\"}}\n")),
        Some("format 2\n"),
        false,
    );
    let context_two = Repository::discover(format_two.path())
        .unwrap()
        .open_format_context()
        .unwrap();
    assert_eq!(context_two.repository_format_number(), 2);
    assert!(context_two.is_read_only());
    assert_eq!(context_two.database_id().unwrap().to_string(), DATABASE_ID);
}

#[test]
fn fails_closed_on_unknown_mixed_malformed_and_unavailable_metadata() {
    let unknown = repository(
        Some(&format!(
            "{{repository_format: 99, database_id: \"{DATABASE_ID}\"}}\n"
        )),
        None,
        false,
    );
    let error = Repository::discover(unknown.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-003");

    let mixed = repository(
        Some(&format!(
            "{{repository_format: 3, database_id: \"{DATABASE_ID}\"}}\n"
        )),
        Some("format 1\n"),
        false,
    );
    let error = Repository::discover(mixed.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-004");

    let malformed = repository(
        Some("{repository_format: 3, database_id: \"not-a-uuid\"}\n"),
        None,
        false,
    );
    let error = Repository::discover(malformed.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");

    let absent = repository(None, None, false);
    let error = Repository::discover(absent.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-001");
}

#[test]
fn rejects_oversized_and_noncanonical_final_records() {
    let oversized_id = "x".repeat(70_000);
    let oversized = repository(
        Some(&format!(
            "{{repository_format: 3, database_id: \"{oversized_id}\"}}\n"
        )),
        None,
        false,
    );
    let error = Repository::discover(oversized.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");

    let noncanonical = repository(
        Some("{repository_format: 3, database_id: \"9F237B7E-6844-4498-BCD5-D24641C07449\"}\n"),
        None,
        false,
    );
    let error = Repository::discover(noncanonical.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");
}
