//! UPGRADE-3 semantic gate (ornadb-5il1.315): a migration candidate is checked
//! for closure and value fidelity against the legacy rows it was built from,
//! not only for its shape.
//!
//! The golden bundle's rows are blob references, each stored as
//! `Tag(ROV3_BLOB_TAG, [Int len, Bytes sha, Text mime, Null, Bytes descriptor])`.
//! The positive case migrates the pinned golden bundle unmodified. Each negative
//! takes that same store, corrupts one field the candidate would carry into the
//! store, and expects the migration copy to refuse it.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_application::build_migration_store;
use orna_repository::{
    DatabaseId, FormatContextError, MigrationAnnotationDefaults, MigrationContinuityRecord,
    Repository, RepositoryError, TypedKey,
    migration_copy::{self, MigrationStore},
};
use orna_value::{
    ROV3_BLOB_TAG, Raw, decode_canonical_storage_value, encode_canonical_storage_value,
};
use tempfile::TempDir;

const GOLDEN_BUNDLE: &str = "../orna-cli/tests/fixtures/legacy-format1.bundle";

/// Clones the golden bundle into a fresh directory and opens it.
fn clone_golden() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("a temporary directory");
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_BUNDLE))
        .arg(directory.path())
        .output()
        .expect("the git clone process starts");
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let repository = Repository::discover(directory.path()).expect("the clone opens");
    (directory, repository)
}

/// Migrates `source` into a fresh destination with `store_for` supplying the
/// store, and reports whether the copy was accepted.
fn prepare(
    source: &Repository,
    store_for: migration_copy::MigrationStoreSource<'_>,
) -> Result<(), RepositoryError> {
    let parent = TempDir::new().expect("a temporary directory");
    let destination: PathBuf = parent.path().join("migrated");
    migration_copy::prepare_format3_migration(
        source,
        &destination,
        MigrationContinuityRecord::new(Vec::new()).expect("an empty continuity record"),
        MigrationAnnotationDefaults::new(1).expect("the default annotation"),
        store_for,
    )
    .map(|_| ())
}

/// Applies `edit` to the fields of the first blob reference in `raw`.
fn with_first_blob(raw: &mut Raw, edit: fn(&mut [Raw]) -> bool) -> bool {
    match raw {
        Raw::Tag(tag, inner) if *tag == ROV3_BLOB_TAG => match &mut **inner {
            Raw::Array(fields) => edit(fields),
            _ => false,
        },
        Raw::Array(items) => items.iter_mut().any(|item| with_first_blob(item, edit)),
        Raw::Map(entries) => entries
            .iter_mut()
            .any(|(_, value)| with_first_blob(value, edit)),
        Raw::Tag(_, inner) => with_first_blob(inner, edit),
        _ => false,
    }
}

/// Replaces the last character of a blob reference's mime text, keeping its
/// length, so the reference still resolves and only the carried value differs.
fn change_mime(fields: &mut [Raw]) -> bool {
    match fields.get_mut(2) {
        Some(Raw::Text(mime)) => match mime.pop() {
            Some(last) => {
                mime.push(if last == 'x' { 'y' } else { 'x' });
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Flips the first byte of a blob reference's content digest, so the reference
/// names content the legacy snapshot does not hold.
fn change_sha(fields: &mut [Raw]) -> bool {
    match fields.get_mut(1) {
        Some(Raw::Bytes(sha)) => match sha.first_mut() {
            Some(byte) => {
                *byte ^= 0xff;
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Flips the first byte of a blob reference's descriptor, so the reference names
/// an object the repository does not hold.
fn corrupt_descriptor(fields: &mut [Raw]) -> bool {
    match fields.get_mut(4) {
        Some(Raw::Bytes(descriptor)) => match descriptor.first_mut() {
            Some(byte) => {
                *byte ^= 0xff;
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// The first stored row `edit` changes, as its relation index, row index and
/// re-encoded stored tuple.
fn first_edited_row<F>(store: &MigrationStore, edit: F) -> (usize, usize, Vec<u8>)
where
    F: Fn(&mut Raw) -> bool,
{
    for (relation_index, relation) in store.relations.iter().enumerate() {
        for (row_index, (_, encoded)) in relation.rows().iter().enumerate() {
            let mut raw = decode_canonical_storage_value(encoded).expect("a golden row decodes");
            if edit(&mut raw) {
                let tuple = encode_canonical_storage_value(&raw).expect("the edited row encodes");
                return (relation_index, row_index, tuple);
            }
        }
    }
    panic!("the golden store has a row the edit can change");
}

/// Replaces one stored tuple of the store, keeping every other row.
fn replace_row(
    store: &mut MigrationStore,
    relation_index: usize,
    row_index: usize,
    tuple: Vec<u8>,
) {
    let mut rows: Vec<(TypedKey, Vec<u8>)> = store.relations[relation_index].rows().to_vec();
    rows[row_index].1 = tuple;
    let relation = store.relations[relation_index].clone().with_rows(rows);
    store.relations[relation_index] = relation;
}

#[test]
fn golden_candidate_passes_the_gate() {
    let (_source_directory, repository) = clone_golden();
    prepare(&repository, &build_migration_store)
        .expect("the unmodified golden candidate passes the semantic gate");
}

#[test]
fn candidate_missing_one_row_fails_the_gate() {
    let (_source_directory, repository) = clone_golden();
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let index = store
                .relations
                .iter()
                .position(|relation| !relation.rows().is_empty())
                .expect("the golden bundle has a populated relation");
            let mut rows: Vec<(TypedKey, Vec<u8>)> = store.relations[index].rows().to_vec();
            rows.pop();
            let relation = store.relations[index].clone().with_rows(rows);
            store.relations[index] = relation;
            Ok(store)
        };
    let error = prepare(&repository, &store_for)
        .expect_err("a candidate missing one row must not pass the gate");
    assert!(
        matches!(error, RepositoryError::MigrationStoreRefused(_)),
        "a missing row is refused by the gate: {error:?}"
    );
}

#[test]
fn candidate_with_a_changed_value_fails_the_gate() {
    let (_source_directory, repository) = clone_golden();
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let (relation, row, tuple) =
                first_edited_row(&store, |raw| with_first_blob(raw, change_mime));
            replace_row(&mut store, relation, row, tuple);
            Ok(store)
        };
    let error = prepare(&repository, &store_for)
        .expect_err("a candidate with a changed value must not pass the gate");
    assert!(
        matches!(error, RepositoryError::MigrationStoreRefused(_)),
        "a changed value is refused by the gate: {error:?}"
    );
}

#[test]
fn candidate_with_an_unlisted_blob_content_fails_the_gate() {
    let (_source_directory, repository) = clone_golden();
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let (relation, row, tuple) =
                first_edited_row(&store, |raw| with_first_blob(raw, change_sha));
            replace_row(&mut store, relation, row, tuple);
            Ok(store)
        };
    let error = prepare(&repository, &store_for)
        .expect_err("a candidate naming content the legacy snapshot lacks must not pass the gate");
    assert!(
        matches!(error, RepositoryError::MigrationStoreRefused(_)),
        "unlisted blob content is refused by the gate: {error:?}"
    );
}

#[test]
fn candidate_with_an_unknown_blob_descriptor_is_refused() {
    let (_source_directory, repository) = clone_golden();
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let (relation, row, tuple) =
                first_edited_row(&store, |raw| with_first_blob(raw, corrupt_descriptor));
            replace_row(&mut store, relation, row, tuple);
            Ok(store)
        };
    // The candidate builder resolves each descriptor in the repository before
    // the gate runs, so this refusal does not depend on the gate.
    prepare(&repository, &store_for)
        .expect_err("a candidate naming an object the repository lacks must not migrate");
}

#[test]
fn source_whose_store_root_is_a_text_file_is_refused() {
    let (source_directory, _repository) = clone_golden();
    fs::write(source_directory.path().join(".orna/store"), b"text root\n")
        .expect("the text store root is written");
    let add = Command::new("git")
        .current_dir(source_directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["add", "--", ".orna/store"])
        .output()
        .expect("git add starts");
    assert!(
        add.status.success(),
        "git add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .current_dir(source_directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Orna Test",
            "-c",
            "user.email=orna-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "legacy text store root",
        ])
        .output()
        .expect("git commit starts");
    assert!(
        commit.status.success(),
        "git commit: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let repository = Repository::discover(source_directory.path()).expect("the source reopens");
    let error = prepare(&repository, &build_migration_store)
        .expect_err("a source whose store root is a text file must not migrate");
    assert!(
        matches!(
            error,
            RepositoryError::FormatContext(FormatContextError::MixedFormats { .. })
        ),
        "a text-file store root is refused as a mixed format: {error:?}"
    );
}
