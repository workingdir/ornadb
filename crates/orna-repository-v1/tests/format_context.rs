use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";

fn canonical_database(database_id: &str) -> String {
    format!("{{\n    repository_format: 3,\n    database_id: \"{database_id}\",\n}}\n")
}

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

fn commit_database(directory: &Path, database: &str) {
    fs::write(directory.join(".orna/database.orna"), database).expect("update database metadata");
    git(directory, &["add", ".orna/database.orna"]);
    git(
        directory,
        &["commit", "--quiet", "-m", "advance database identity"],
    );
}

fn replace_database_with_directory(directory: &Path) {
    let database = directory.join(".orna/database.orna");
    fs::remove_file(&database).expect("remove database file");
    fs::create_dir(&database).expect("replace database with directory");
    fs::write(database.join("nested"), "not metadata").expect("write invalid database entry");
    git(directory, &["add", "--all"]);
    git(
        directory,
        &["commit", "--quiet", "-m", "replace database with directory"],
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
    let database = canonical_database(DATABASE_ID);
    let directory = repository(Some(&database), None, true);
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
fn context_binds_database_identity_to_its_committed_snapshot() {
    const FIRST_DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";
    const SECOND_DATABASE_ID: &str = "a4a0a7d1-4f5c-4dc4-a5bf-f3f7f6a8d7e1";
    let first_database = canonical_database(FIRST_DATABASE_ID);
    let directory = repository(Some(&first_database), None, true);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    let first = repository
        .open_format_context()
        .expect("admit first committed identity");

    commit_database(directory.path(), &canonical_database(SECOND_DATABASE_ID));
    let second = repository
        .open_format_context()
        .expect("admit second committed identity");

    assert_eq!(first.database_id().unwrap().to_string(), FIRST_DATABASE_ID);
    assert_eq!(
        second.database_id().unwrap().to_string(),
        SECOND_DATABASE_ID
    );
    assert!(first.validate_schema_root().is_ok());
    assert!(first.validate_store_root().is_ok());
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
            "{{\n    repository_format: 99,\n    database_id: \"{DATABASE_ID}\",\n}}\n"
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
        Some(&canonical_database(DATABASE_ID)),
        Some("format 1\n"),
        false,
    );
    let error = Repository::discover(mixed.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-004");

    let malformed = repository(
        Some("{\n    repository_format: 3,\n    database_id: \"not-a-uuid\",\n}\n"),
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
            "{{\n    repository_format: 3,\n    database_id: \"{oversized_id}\",\n}}\n"
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

#[test]
fn rejects_an_unborn_snapshot_as_unavailable() {
    let directory = TempDir::new().expect("create unborn repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    let error = Repository::discover(directory.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-005");
}

#[test]
fn rejects_noncanonical_final_database_record_bytes() {
    for database in [
        format!("{{\n    database_id: \"{DATABASE_ID}\",\n    repository_format: 3,\n}}\n"),
        format!("{{repository_format: 3, database_id: \"{DATABASE_ID}\"}}\n"),
        format!("{{\n  repository_format: 3,\n  database_id: \"{DATABASE_ID}\",\n}}\n"),
    ] {
        let directory = repository(Some(&database), None, false);
        let error = Repository::discover(directory.path())
            .unwrap()
            .open_format_context()
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");
    }
}

#[test]
fn rejects_unenumerated_legacy_storage_profiles() {
    for database in [
        "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n",
        "{repository_format: 2, storage_profile: \"compact-storage-v2\"}\n",
        "{repository_format: 1, storage_profile: \"future-profile\"}\n",
    ] {
        let directory = repository(None, Some(database), false);
        let error = Repository::discover(directory.path())
            .unwrap()
            .open_format_context()
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");
    }
}

#[test]
fn does_not_downgrade_an_invalid_database_entry_to_legacy() {
    let directory = repository(Some("not-a-record\n"), Some("format 1\n"), false);
    replace_database_with_directory(directory.path());
    let error = Repository::discover(directory.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");
}
