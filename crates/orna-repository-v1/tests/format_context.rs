use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";
const CANONICAL_DATABASE: &str = include_str!("fixtures/format-context/database-final.orna");
const DATABASE_TEMPLATE: &str = include_str!("fixtures/format-context/database-template.orna");
const UNKNOWN_FORMAT_DATABASE: &str =
    include_str!("fixtures/format-context/database-unknown-format.orna");
const BAD_UUID_DATABASE: &str = include_str!("fixtures/format-context/database-bad-uuid.orna");
const UPPERCASE_UUID_DATABASE: &str =
    include_str!("fixtures/format-context/database-uppercase-uuid.orna");
const SIDECAR_DATABASE: &str = include_str!("fixtures/format-context/database-sidecar.orna");
const INVALID_DATABASE: &str = include_str!("fixtures/format-context/database-invalid.orna");
const REORDERED_DATABASE: &str = include_str!("fixtures/format-context/database-reordered.orna");
const COMPACT_DATABASE: &str = include_str!("fixtures/format-context/database-compact.orna");
const INDENTED_DATABASE: &str = include_str!("fixtures/format-context/database-indented-two.orna");
const LEGACY_FORMAT_ONE: &str = include_str!("fixtures/format-context/format-1.orna");
const LEGACY_FORMAT_TWO: &str = include_str!("fixtures/format-context/format-2.orna");
const LEGACY_PROFILE_ONE: &str = include_str!("fixtures/format-context/profile-1-unknown.orna");
const LEGACY_PROFILE_TWO: &str = include_str!("fixtures/format-context/profile-2-unknown.orna");

fn canonical_database(database_id: &str) -> String {
    DATABASE_TEMPLATE.replace("00000000-0000-4000-8000-000000000000", database_id)
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

/// Runs one read-only Git query and returns its trimmed stdout line.
fn git_line(directory: &Path, arguments: &[&str]) -> String {
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
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
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
    git(directory.path(), &["config", "user.name", "kierandrewett"]);
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .expect("write source root");
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
    let directory = repository(Some(CANONICAL_DATABASE), None, true);
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
    let format_one = repository(None, Some(LEGACY_FORMAT_ONE), false);
    let context_one = Repository::discover(format_one.path())
        .unwrap()
        .open_format_context()
        .unwrap();
    assert_eq!(context_one.repository_format_number(), 1);
    assert!(context_one.is_read_only());
    assert!(!context_one.supports_writes());
    assert!(context_one.database_id().is_none());

    let format_two = repository(Some(SIDECAR_DATABASE), Some(LEGACY_FORMAT_TWO), false);
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
    let unknown = repository(Some(UNKNOWN_FORMAT_DATABASE), None, false);
    let error = Repository::discover(unknown.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-003");

    let mixed = repository(Some(CANONICAL_DATABASE), Some(LEGACY_FORMAT_ONE), false);
    let error = Repository::discover(mixed.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-004");

    let malformed = repository(Some(BAD_UUID_DATABASE), None, false);
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
    let oversized_database = CANONICAL_DATABASE.repeat(800);
    let oversized = repository(Some(&oversized_database), None, false);
    let error = Repository::discover(oversized.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");

    let noncanonical = repository(Some(UPPERCASE_UUID_DATABASE), None, false);
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
        REORDERED_DATABASE.to_owned(),
        COMPACT_DATABASE.to_owned(),
        INDENTED_DATABASE.to_owned(),
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
    for database in [LEGACY_PROFILE_ONE, LEGACY_PROFILE_TWO] {
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
    let directory = repository(Some(INVALID_DATABASE), Some(LEGACY_FORMAT_ONE), false);
    replace_database_with_directory(directory.path());
    let error = Repository::discover(directory.path())
        .unwrap()
        .open_format_context()
        .unwrap_err();
    assert_eq!(error.code(), "ORNA-REPO-CONTEXT-002");
}

#[test]
fn reopens_legacy_and_format_three_mounts_side_by_side_without_sharing_state() {
    const LEGACY_BLOB: &str = "legacy bytes";
    const FORMAT_THREE_BLOB: &str = "format three bytes";
    let directory = repository(None, Some(LEGACY_FORMAT_ONE), false);
    // The legacy commit's own content, before the format-3 commit exists.
    fs::create_dir_all(directory.path().join(".orna/store")).unwrap();
    fs::write(directory.path().join(".orna/store/root"), LEGACY_BLOB).unwrap();
    git(directory.path(), &["add", "."]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "legacy store root"],
    );
    let legacy_commit = git_line(directory.path(), &["rev-parse", "HEAD"]);

    // A later commit converts the workspace to format 3. The legacy commit is
    // still reachable, so its committed bytes must stay readable and unchanged.
    fs::remove_file(directory.path().join(".orna/format.orna")).unwrap();
    fs::write(
        directory.path().join(".orna/database.orna"),
        CANONICAL_DATABASE,
    )
    .unwrap();
    fs::write(directory.path().join(".orna/store/root"), FORMAT_THREE_BLOB).unwrap();
    git(directory.path(), &["add", "--all"]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "publish format 3"],
    );

    let repository = Repository::discover(directory.path()).expect("discover repository");
    let legacy = repository
        .open_format_context_at_selector(&legacy_commit)
        .expect("reopen the legacy commit read-only");
    let current = repository
        .open_format_context_at_selector("HEAD")
        .expect("reopen the format-3 commit");

    // Both views are open at once and neither rewrites the other's coordinate.
    assert_eq!(legacy.repository_format_number(), 1);
    assert!(legacy.is_legacy_format());
    assert_eq!(current.repository_format_number(), 3);
    assert!(!current.is_legacy_format());
    assert!(
        repository.head().unwrap().unwrap().as_str() != legacy_commit,
        "the legacy view did not move the workspace HEAD"
    );

    // The legacy view is genuinely readable: its committed bytes come from its
    // own snapshot, so the later format-3 write never leaks into it.
    assert_eq!(
        legacy
            .read_committed_file(".orna/store/root", 64)
            .expect("read the legacy snapshot's own bytes"),
        LEGACY_BLOB.as_bytes()
    );
    assert_eq!(
        current
            .read_committed_file(".orna/store/root", 64)
            .expect("read the current snapshot's bytes"),
        FORMAT_THREE_BLOB.as_bytes()
    );

    // The legacy pin admits no native graph, so it can never share the
    // current store's row authority.
    assert_eq!(
        legacy.load_row_map([0; 16]).unwrap_err().code(),
        "ORNA-REPO-CONTEXT-003"
    );

    // A read past the bound is refused rather than silently truncated, while
    // the same path reads exactly at the bound.
    assert!(legacy.read_committed_file(".orna/store/root", 4).is_err());
    assert_eq!(
        legacy
            .read_committed_file(".orna/store/root", LEGACY_BLOB.len())
            .expect("read exactly at the bound"),
        LEGACY_BLOB.as_bytes()
    );
}
