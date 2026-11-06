//! `orna migrate copy` refuses a legacy identity sidecar that does not name the
//! compact tables exactly once (ornadb-5il1.481).
//!
//! The migration store is built by `orna_application::build_migration_store`.
//! A legacy sidecar that does not decode, or whose identities collide, is
//! refused as unreadable. A compact table whose storage id has no unretired
//! Table identity is refused by `legacy_table_names` as a correspondence error.
//! These tests start from the pinned format-1 bundle, commit a corrupted sidecar
//! on top of it in a clone, and run the real `migrate copy` command. The pinned
//! bundle itself is not touched.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_foundation::{CanonicalValue, OvbRaw};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
const SIDECAR: &str = ".orna/object-identities.cbor";
/// The refusal `build_migration_store` gives when a compact table's storage id
/// does not name exactly one unretired Table identity.
const CORRESPONDENCE_REFUSAL: &str =
    "the compact tables do not correspond to the legacy identities";
/// The refusal `build_migration_store` gives when the sidecar does not decode.
/// `validate_records` rejects any repeated storage id here, before the
/// correspondence check runs.
const UNREADABLE_REFUSAL: &str = "the object identities are unreadable";

fn git(directory: &Path, arguments: &[&str]) -> Vec<u8> {
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
    output.stdout
}

fn git_text(directory: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git(directory, arguments))
        .unwrap()
        .trim()
        .to_owned()
}

fn run_copy(source: &Path, destination: &Path, git_trace: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE", git_trace)
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            "2",
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap()
}

/// The kind code that marks Table identities in a sidecar of this version: 1 in
/// the legacy version-1 layout, and 2 in version 2.
fn table_code(version: &OvbRaw) -> i64 {
    match version {
        OvbRaw::Int(value) if value.to_string() == "1" => 1,
        OvbRaw::Int(value) if value.to_string() == "2" => 2,
        other => panic!("sidecar has an unknown version: {other:?}"),
    }
}

/// Whether a sidecar entry is a Table identity: its first field is the Table
/// kind code.
fn is_table_entry(entry: &OvbRaw, table: i64) -> bool {
    matches!(
        entry,
        OvbRaw::Array(fields)
            if matches!(fields.first(), Some(OvbRaw::Int(code)) if code.to_string() == table.to_string())
    )
}

/// Commits, on top of the bundle's history, the committed sidecar with `mutate`
/// applied to its identity entries. `mutate` receives the entries and the code
/// that marks Table identities in this sidecar's version.
fn commit_corrupted_sidecar(source: &Path, mutate: impl FnOnce(&mut Vec<OvbRaw>, i64)) {
    let committed = git(source, &["show", &format!("HEAD:{SIDECAR}")]);
    let value = CanonicalValue::decode(&committed).unwrap();
    let mut top = match value.raw().clone() {
        OvbRaw::Array(items) => items,
        other => panic!("sidecar is not an array: {other:?}"),
    };
    let table = table_code(&top[0]);
    let entries = match top.get_mut(1) {
        Some(OvbRaw::Array(entries)) => entries,
        other => panic!("sidecar has no identity array: {other:?}"),
    };
    mutate(entries, table);
    let bytes = CanonicalValue::new(OvbRaw::Array(top))
        .unwrap()
        .encode()
        .unwrap();
    fs::write(source.join(SIDECAR), bytes).unwrap();
    git(source, &["add", "--", SIDECAR]);
    git(
        source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "corrupt the legacy identity sidecar",
        ],
    );
}

/// Runs `orna migrate copy` on a source whose sidecar was corrupted by `mutate`,
/// and asserts the refusal carries `expected` and leaves the source and
/// destination untouched.
fn assert_copy_refuses_corrupted_sidecar(
    label: &str,
    expected: &str,
    mutate: impl FnOnce(&mut Vec<OvbRaw>, i64),
) {
    let parent = TempDir::new().unwrap();
    let source: PathBuf = parent.path().join("source");
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    git(
        parent.path(),
        &[
            "clone",
            "--quiet",
            bundle.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    );
    commit_corrupted_sidecar(&source, mutate);

    let head = git_text(&source, &["rev-parse", "HEAD"]);
    let status = git(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let destination = parent.path().join("migrated");
    let git_trace = parent.path().join("git-trace.log");

    let output = run_copy(&source, &destination, &git_trace);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{label}: copy succeeded");
    assert!(stderr.contains("E2100"), "{label}: {stderr}");
    assert!(
        stderr.contains(expected),
        "{label}: refusal is not `{expected}`: {stderr}"
    );
    assert!(
        !destination.exists(),
        "{label}: a refused copy created {}",
        destination.display()
    );
    assert_eq!(git_text(&source, &["rev-parse", "HEAD"]), head, "{label}");
    assert_eq!(
        git(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        status,
        "{label}"
    );
}

/// The sidecar drops one compact table's Table identity, so its storage id has
/// no unretired Table identity to resolve to.
#[test]
fn migrate_copy_refuses_a_sidecar_missing_a_compact_table_identity() {
    assert_copy_refuses_corrupted_sidecar("missing", CORRESPONDENCE_REFUSAL, |entries, table| {
        let index = entries
            .iter()
            .position(|entry| is_table_entry(entry, table))
            .expect("the bundle sidecar has a Table identity");
        entries.remove(index);
    });
}

/// The sidecar repeats one Table identity's storage id. The sidecar decoder
/// rejects the repeated id as an identity collision, so the copy is refused as
/// unreadable and never reaches the correspondence check.
#[test]
fn migrate_copy_refuses_a_sidecar_repeating_a_table_storage_id() {
    assert_copy_refuses_corrupted_sidecar("duplicate", UNREADABLE_REFUSAL, |entries, table| {
        let duplicate = entries
            .iter()
            .find(|entry| is_table_entry(entry, table))
            .expect("the bundle sidecar has a Table identity")
            .clone();
        entries.push(duplicate);
    });
}
