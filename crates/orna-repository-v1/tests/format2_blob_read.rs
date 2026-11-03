//! Repository-format-2 Blob reads (ORNA-UPGRADE-002, ORNA-ROW-013).
//!
//! `Repository::read_format2_orr_blob` and `::read_format2_rov_blob` are the
//! compatibility readers for format-2 Blob values. Nothing in the CLI reaches
//! them yet, so before this file they had no caller at all and no test.
//!
//! Covered here: the inline ORR-2 spelling, the read limit that is checked
//! before any content object is read, and the refusal to read a format-2 Blob
//! field out of a commit that is not format 2.
//!
//! Not covered here, and still without a caller: the storage-record spelling,
//! which resolves through an OGB-1 content root under `.orna/blobs/<2>/<62>/`.
//! Building one needs a chunked OGB-1 tree, so that case wants its own slice.

#[path = "../src/test_support.rs"]
mod test_support;

use std::{fs, path::Path, process::Command};

use orna_repository_v1::{Repository, format2_read::Format2BlobError};

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
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

fn init_repository(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(root);
    git_output(root, &["config", "commit.gpgsign", "false"]);
}

/// A committed repository whose recorded format is 2.
fn format2_repository(root: &Path) {
    init_repository(root);
    fs::write(root.join("tracked.txt"), b"format-2 committed bytes\n").unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(
        root.join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    )
    .unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(root, &["add", "."]);
    git_output(root, &["commit", "-m", "format-2 source"]);
}

fn format3_repository(root: &Path) {
    init_repository(root);
    fs::write(root.join("tracked.txt"), b"format-3 committed bytes\n").unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-final.orna"),
    )
    .unwrap();
    git_output(root, &["add", "."]);
    git_output(root, &["commit", "-m", "format-3 source"]);
}

/// `aGVsbG8=` is padded Base64 for `hello`, which is the inline ORR-2 spelling
/// (draft ORNA-OGB-007). An inline value needs no content root, so the reader
/// returns its decoded bytes at the pinned commit.
#[test]
fn an_inline_orr2_field_returns_its_decoded_bytes() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    format2_repository(root);
    let repository = Repository::discover(root).unwrap();

    let bytes = repository
        .read_format2_orr_blob("HEAD", "aGVsbG8=", 1024)
        .expect("an inline ORR-2 field reads at a format-2 commit");

    assert_eq!(bytes, b"hello");
}

/// The limit is checked against the decoded length before any object is read,
/// so a caller that asks for fewer bytes learns the total and the limit rather
/// than receiving a truncated value.
#[test]
fn an_inline_orr2_field_reports_the_limit_it_was_given() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    format2_repository(root);
    let repository = Repository::discover(root).unwrap();

    let error = repository
        .read_format2_orr_blob("HEAD", "aGVsbG8=", 4)
        .expect_err("five bytes do not fit a four-byte limit");

    assert!(
        matches!(error, Format2BlobError::Limit { total: 5, limit: 4 }),
        "unexpected error: {error:?}"
    );
}

/// Format-2 Blob fields are never read from another format (ORNA-UPGRADE-001:
/// readers dispatch by the recorded format), so a format-3 commit is refused by
/// name rather than read as if it were legacy.
#[test]
fn a_format3_commit_is_refused_as_a_format2_blob_source() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    format3_repository(root);
    let repository = Repository::discover(root).unwrap();

    let error = repository
        .read_format2_orr_blob("HEAD", "aGVsbG8=", 1024)
        .expect_err("a format-3 commit is not a format-2 source");

    assert!(
        matches!(error, Format2BlobError::NotFormat2(_)),
        "unexpected error: {error:?}"
    );
}

/// The same refusal applies to the ROV-2 entry point, so a caller cannot reach
/// legacy resolution by choosing the other reader.
#[test]
fn a_format3_commit_is_refused_as_a_rov2_reference_source() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    format3_repository(root);
    let repository = Repository::discover(root).unwrap();

    let error = repository
        .read_format2_rov_blob("HEAD", &[0xa0], 1024)
        .expect_err("a format-3 commit is not a format-2 source");

    assert!(
        matches!(error, Format2BlobError::NotFormat2(_)),
        "unexpected error: {error:?}"
    );
}
