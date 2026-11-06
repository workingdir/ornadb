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
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_application::build_migration_store;
use orna_repository::{
    DatabaseId, FormatContextError, ManagedFileChange, ManagedPath, MigrationAnnotationDefaults,
    MigrationContinuityRecord, PrivateCommit, Repository, RepositoryError, TypedKey,
    inspect_legacy_source_metadata,
    migration_copy::{self, CandidateGate, MigrationStore},
    relation_admission::StagedRelation,
};
use orna_value::{
    ROV3_BLOB_TAG, Raw, decode_canonical_storage_value, encode_canonical_storage_value,
};
use sha2::{Digest, Sha256};
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
        Some(MigrationAnnotationDefaults::new(1).expect("the default annotation")),
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

/// Installs `mutate` ahead of the gate `store` carries: the candidate is changed
/// on disk, then checked by the real gate.
fn gate_after<F>(mut store: MigrationStore, mutate: F) -> MigrationStore
where
    F: Fn(&Repository, &PrivateCommit) + 'static,
{
    let gate = store
        .candidate_gate
        .take()
        .expect("the migration store installs the candidate gate");
    store.candidate_gate = Some(CandidateGate::new(move |repository, candidate| {
        mutate(repository, candidate);
        gate.check(repository, candidate)
    }));
    store
}

/// Collects the content digest of every ROV-3 blob reference inside `raw`.
fn blob_digests(raw: &Raw, digests: &mut Vec<Vec<u8>>) {
    match raw {
        Raw::Tag(tag, inner) if *tag == ROV3_BLOB_TAG => {
            if let Raw::Array(fields) = &**inner
                && let Some(Raw::Bytes(sha)) = fields.get(1)
            {
                digests.push(sha.clone());
            }
        }
        Raw::Array(items) => items.iter().for_each(|item| blob_digests(item, digests)),
        Raw::Map(entries) => entries.iter().for_each(|(key, value)| {
            blob_digests(key, digests);
            blob_digests(value, digests);
        }),
        Raw::Tag(_, inner) => blob_digests(inner, digests),
        _ => {}
    }
}

/// A blob reference held by one row of the store.
struct BlobRow {
    relation: usize,
    key: TypedKey,
    digest: Vec<u8>,
}

/// A blob reference whose content digest no other reference in `store` names,
/// and which has content: the empty blob has no object to remove.
fn unique_blob(store: &MigrationStore) -> BlobRow {
    let empty: Vec<u8> = Sha256::digest(b"").to_vec();
    let mut refs = Vec::new();
    for (relation, staged) in store.relations.iter().enumerate() {
        for (key, encoded) in staged.rows() {
            let raw = decode_canonical_storage_value(encoded).expect("a golden row decodes");
            let mut digests = Vec::new();
            blob_digests(&raw, &mut digests);
            refs.extend(digests.into_iter().map(|digest| BlobRow {
                relation,
                key: key.clone(),
                digest,
            }));
        }
    }
    let position = refs
        .iter()
        .position(|blob| {
            blob.digest != empty
                && refs
                    .iter()
                    .filter(|other| other.digest == blob.digest)
                    .count()
                    == 1
        })
        .expect("the golden store holds a non-empty blob content no other reference names");
    refs.swap_remove(position)
}

/// Runs `git` in `root` with `arguments` and returns its standard output.
fn git_stdout(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Deletes every loose blob object of `repository` whose content hashes to
/// `digest`, and returns how many it removed.
fn remove_content_objects(repository: &Repository, digest: &[u8]) -> usize {
    let root = repository.worktree();
    let listing = git_stdout(
        root,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ],
    );
    let mut removed = 0;
    for line in String::from_utf8_lossy(&listing).lines() {
        let Some((oid, kind)) = line.split_once(' ') else {
            continue;
        };
        if kind != "blob" {
            continue;
        }
        let content = git_stdout(root, &["cat-file", "blob", oid]);
        if Sha256::digest(&content).as_slice() == digest {
            let object = root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
            fs::remove_file(&object).expect("the blob object is removed");
            removed += 1;
        }
    }
    removed
}

/// Whether `detail` begins with `relation ` followed by a 32-character hex id.
fn names_a_relation(detail: &str) -> bool {
    detail
        .strip_prefix("relation ")
        .and_then(|rest| rest.get(..32))
        .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
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

#[test]
fn candidate_with_a_relation_the_legacy_snapshot_does_not_stage_is_refused() {
    let (_source_directory, repository) = clone_golden();
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let descriptor = store
                .relations
                .first()
                .expect("the golden store has a relation")
                .descriptor()
                .clone();
            store
                .relations
                .push(StagedRelation::new([0xEE; 16], descriptor));
            Ok(store)
        };
    let error = prepare(&repository, &store_for).expect_err(
        "a candidate storing a relation the legacy snapshot does not stage must not pass the gate",
    );
    match error {
        RepositoryError::MigrationStoreRefused(detail) => assert!(
            detail.contains("the migration did not stage"),
            "the refusal is the gate's unstaged-relation check: {detail}"
        ),
        other => panic!("an unstaged relation is refused by the gate: {other:?}"),
    }
}

#[test]
fn candidate_whose_blob_content_is_removed_fails_the_gate() {
    let (_source_directory, repository) = clone_golden();
    let named = RefCell::new(None);
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let store = build_migration_store(source, database)?;
            let blob = unique_blob(&store);
            *named.borrow_mut() = Some(format!("{:?}", blob.key));
            let digest = blob.digest;
            Ok(gate_after(store, move |repository, _candidate| {
                assert!(
                    remove_content_objects(repository, &digest) > 0,
                    "the candidate holds the blob content the test removes"
                );
            }))
        };
    let error = prepare(&repository, &store_for)
        .expect_err("a candidate whose blob content is gone must not pass the gate");
    let key = named
        .into_inner()
        .unwrap_or_else(|| panic!("the store was not built before the refusal: {error:?}"));
    match error {
        RepositoryError::MigrationStoreRefused(detail) => assert!(
            names_a_relation(&detail) && detail.contains(&format!("row {key} ")),
            "the refusal names the relation and the row key: {detail}"
        ),
        other => panic!("missing blob content is refused by the gate: {other:?}"),
    }
}

#[test]
fn candidate_missing_a_blob_row_names_the_row_key() {
    let (_source_directory, repository) = clone_golden();
    let named = RefCell::new(None);
    let store_for =
        |source: &Repository, database: DatabaseId| -> Result<MigrationStore, RepositoryError> {
            let mut store = build_migration_store(source, database)?;
            let blob = unique_blob(&store);
            *named.borrow_mut() = Some(format!("{:?}", blob.key));
            let mut rows: Vec<(TypedKey, Vec<u8>)> = store.relations[blob.relation].rows().to_vec();
            rows.retain(|(key, _)| *key != blob.key);
            let relation = store.relations[blob.relation].clone().with_rows(rows);
            store.relations[blob.relation] = relation;
            Ok(store)
        };
    let error = prepare(&repository, &store_for)
        .expect_err("a candidate missing a blob-holding row must not pass the gate");
    let key = named
        .into_inner()
        .expect("the store holds a blob reference");
    match error {
        RepositoryError::MigrationStoreRefused(detail) => assert!(
            names_a_relation(&detail) && detail.contains(&format!("is missing legacy row {key}")),
            "the refusal names the relation and the missing row key: {detail}"
        ),
        other => panic!("a missing blob-holding row is refused by the gate: {other:?}"),
    }
}

#[test]
fn candidate_with_a_text_store_root_fails_the_installed_gate() {
    let (_directory, repository) = clone_golden();
    let database = inspect_legacy_source_metadata(&repository)
        .expect("the golden metadata reads")
        .database_id();
    let store = build_migration_store(&repository, database).expect("the golden store builds");
    let head = repository
        .head()
        .expect("the golden head reads")
        .expect("the golden clone has a head");
    let root = ManagedFileChange::new(
        ManagedPath::new(".orna/store").expect("a managed path"),
        Some(b"text root\n".to_vec()),
    );
    let candidate = repository
        .build_private_commit(&head, &[root], "legacy text store root")
        .expect("a candidate commit builds");
    let gate = store
        .candidate_gate
        .as_ref()
        .expect("the migration store installs the candidate gate");
    let detail = gate
        .check(&repository, &candidate)
        .expect_err("a candidate whose store root is a text file must not pass the gate");
    assert!(
        detail.starts_with("candidate format context is unavailable"),
        "the text store root is refused at the format context: {detail}"
    );
}
