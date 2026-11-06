//! UPGRADE-3 semantic gate for a migration candidate (ornadb-5il1.315).
//!
//! Admission checks a candidate's shape. This gate checks what the candidate
//! carries, read back through its private format-3 store root and never
//! through the legacy inputs:
//!
//! - the store binds exactly the relations the migration staged, plus the
//!   system relations the store always carries;
//! - every relation holds exactly the expected rows, with equal stored values;
//! - every Blob value an expected row holds resolves to the content identity
//!   and bytes its legacy payload hashes to.
//!
//! A row lost or altered between the legacy decode and the committed store is
//! refused, and so is a relation the migration did not stage.

use std::collections::BTreeMap;

use orna_repository::relation_admission::is_system_relation;
use orna_repository::{
    ContentIdentity, KeyRange, NativeGraphContext, PrivateCommit, Repository,
    RepositoryFormatContext, RepositoryReadScope, TypedKey,
};
use orna_value::{Raw, decode_canonical_storage_value};

use crate::migration_rows::{ExpectedBlob, ExpectedRelation};

/// The number of rows read per page while a candidate relation is walked. It
/// is the largest page `KeyRange` admits.
const PAGE_ROWS: usize = 256;

/// What the migration expects the candidate to carry: every staged relation
/// with its expected rows and blob references, and the payload of every blob
/// those references name, keyed by content identity.
#[derive(Debug)]
pub(crate) struct GateExpectation {
    pub(crate) relations: Vec<ExpectedRelation>,
    pub(crate) blobs: BTreeMap<ContentIdentity, Vec<u8>>,
}

/// Checks `candidate` against `expectation`. The error names the failed
/// relation and the failed check.
pub(crate) fn verify_candidate(
    repository: &Repository,
    candidate: &PrivateCommit,
    expectation: &GateExpectation,
) -> Result<(), String> {
    let context = repository
        .open_private_candidate_format_context(candidate)
        .map_err(|error| format!("candidate format context is unavailable: {error}"))?;
    let store = context
        .store_relation_ids()
        .map_err(|error| format!("candidate relations are unreadable: {error}"))?;
    check_store_relations(&store, &expectation.relations)?;
    for relation in &expectation.relations {
        verify_relation(&context, relation, &expectation.blobs)?;
    }
    Ok(())
}

/// Requires the store to bind every staged relation, and no relation the
/// migration did not stage. System relations the store always carries are not
/// inspected.
fn check_store_relations(store: &[[u8; 16]], expected: &[ExpectedRelation]) -> Result<(), String> {
    for relation_id in store {
        let staged = expected
            .iter()
            .any(|relation| relation.relation_id == *relation_id);
        if !staged && !is_system_relation(relation_id) {
            return Err(format!(
                "candidate store binds relation {} the migration did not stage",
                relation_label(*relation_id)
            ));
        }
    }
    for relation in expected {
        if !store.contains(&relation.relation_id) {
            return Err(format!(
                "candidate store does not bind relation {}",
                relation_label(relation.relation_id)
            ));
        }
    }
    Ok(())
}

/// Checks one staged relation: its rows, then every blob its rows hold.
fn verify_relation(
    context: &RepositoryFormatContext,
    relation: &ExpectedRelation,
    payloads: &BTreeMap<ContentIdentity, Vec<u8>>,
) -> Result<(), String> {
    let label = relation_label(relation.relation_id);
    let rows = context
        .load_row_map(relation.relation_id)
        .map_err(|error| format!("relation {label} row map is unavailable: {error}"))?;
    let graph = context
        .open_native_graph(&rows)
        .map_err(|error| format!("relation {label} graph is unavailable: {error}"))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| format!("relation {label} read scope is unavailable: {error}"))?;
    let actual = candidate_rows(&graph, &scope, &label)?;
    check_relation(relation, &actual)?;
    for blob in &relation.blobs {
        verify_blob(&graph, &scope, &label, blob, payloads)?;
    }
    Ok(())
}

/// Reads every stored row of one candidate relation, keyed by its typed key.
/// Each page resumes strictly after the last key of the previous page, so a
/// repeated key means the store did not advance and is refused.
fn candidate_rows(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    label: &str,
) -> Result<BTreeMap<TypedKey, Raw>, String> {
    let mut actual = BTreeMap::new();
    let mut resume: Option<TypedKey> = None;
    loop {
        let range = KeyRange::new(None, None, PAGE_ROWS)
            .map_err(|error| format!("relation {label} key range is invalid: {error}"))?;
        let range = match resume.take() {
            Some(key) => range
                .with_resume_after(key)
                .map_err(|error| format!("relation {label} resume key is invalid: {error}"))?,
            None => range,
        };
        let page = graph
            .range_rows(&range, scope)
            .map_err(|error| format!("relation {label} rows could not be read: {error}"))?;
        let Some(last) = page.last() else {
            break;
        };
        let last_key = last.key().clone();
        for row in &page {
            let fields = graph
                .stored_row_fields(row, scope)
                .map_err(|error| format!("relation {label} row could not be read: {error}"))?;
            let raw = decode_canonical_storage_value(&fields)
                .map_err(|error| format!("relation {label} row value is not canonical: {error}"))?;
            if actual.insert(row.key().clone(), raw).is_some() {
                return Err(format!("relation {label} repeats a row key"));
            }
        }
        resume = Some(last_key);
    }
    Ok(actual)
}

/// Requires every expected row to be present with an equal stored value, and
/// the candidate relation to hold no other row.
fn check_relation(
    relation: &ExpectedRelation,
    actual: &BTreeMap<TypedKey, Raw>,
) -> Result<(), String> {
    let label = relation_label(relation.relation_id);
    for (key, expected) in &relation.rows {
        match actual.get(key) {
            Some(value) if value == expected => {}
            Some(_) => return Err(format!("relation {label} carries a changed row value")),
            None => return Err(format!("relation {label} is missing a legacy row")),
        }
    }
    if actual.len() != relation.rows.len() {
        return Err(format!(
            "relation {label} carries a row the legacy snapshot does not hold"
        ));
    }
    Ok(())
}

/// Requires the Blob value `blob` names to resolve, through the candidate's
/// own row, to the content identity and bytes its legacy payload hashes to.
fn verify_blob(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    label: &str,
    blob: &ExpectedBlob,
    payloads: &BTreeMap<ContentIdentity, Vec<u8>>,
) -> Result<(), String> {
    let payload = payloads
        .get(&blob.identity)
        .ok_or_else(|| format!("relation {label} expects a blob the migration did not stage"))?;
    let row = graph
        .lookup_row(&blob.row, scope)
        .map_err(|error| format!("relation {label} blob row could not be read: {error}"))?
        .ok_or_else(|| format!("relation {label} is missing a row holding a blob"))?;
    let stored = graph
        .read_stored_blob_value(&row, &blob.descriptor, scope)
        .map_err(|error| format!("relation {label} blob could not be read: {error}"))?;
    let reference = stored.reference();
    if reference.content_identity() != blob.identity {
        return Err(format!(
            "relation {label} holds a blob with a changed content identity"
        ));
    }
    let length = blob.identity.length();
    if length > 0 {
        let range = graph
            .read_blob_range(reference, 0..length, scope)
            .map_err(|error| format!("relation {label} blob bytes could not be read: {error}"))?;
        if range.bytes() != payload.as_slice() {
            return Err(format!(
                "relation {label} holds blob bytes that differ from the legacy payload"
            ));
        }
    }
    Ok(())
}

/// The relation identifier in lowercase hex, for error messages.
fn relation_label(relation_id: [u8; 16]) -> String {
    relation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        process::Command,
    };

    use orna_repository::migration_copy::{self, CandidateGate, MigrationStore};
    use orna_repository::relation_admission::OBJECT_IDENTITY_RELATION_ID;
    use orna_repository::{
        DatabaseId, ManagedFileChange, ManagedPath, MigrationContinuityRecord, RepositoryError,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::migration_store::build_store_with_expectation;

    const GOLDEN_BUNDLE: &str = "../orna-cli/tests/fixtures/legacy-format1.bundle";

    /// Clones the golden bundle into a fresh directory and opens it.
    fn golden_repository() -> (TempDir, Repository) {
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
            None,
            store_for,
        )
        .map(|_| ())
    }

    #[test]
    fn store_binds_exactly_the_staged_and_system_relations() {
        let staged = ExpectedRelation {
            relation_id: [1; 16],
            rows: Vec::new(),
            blobs: Vec::new(),
        };
        let staged = std::slice::from_ref(&staged);
        assert!(check_store_relations(&[[1; 16]], staged).is_ok());
        assert!(check_store_relations(&[[1; 16], OBJECT_IDENTITY_RELATION_ID], staged).is_ok());
        assert!(check_store_relations(&[OBJECT_IDENTITY_RELATION_ID], staged).is_err());
        assert!(check_store_relations(&[], staged).is_err());
        assert!(check_store_relations(&[[1; 16], [2; 16]], staged).is_err());
    }

    #[test]
    fn a_blob_payload_the_candidate_does_not_hold_is_refused() {
        let (_source, repository) = golden_repository();
        let store_for = |source: &Repository,
                         database: DatabaseId|
         -> Result<MigrationStore, RepositoryError> {
            let (mut store, mut expectation) = build_store_with_expectation(source, database)?;
            let identity = expectation
                .relations
                .iter()
                .flat_map(|relation| relation.blobs.iter())
                .map(|blob| blob.identity)
                .find(|identity| identity.length() > 0)
                .expect("the golden bundle holds a non-empty blob");
            let payload = expectation
                .blobs
                .get_mut(&identity)
                .expect("the blob payload is staged");
            payload[0] ^= 0xff;
            store.candidate_gate = Some(CandidateGate::new(move |repository, candidate| {
                verify_candidate(repository, candidate, &expectation)
            }));
            Ok(store)
        };
        let error = prepare(&repository, &store_for)
            .expect_err("a candidate holding other blob bytes must not pass the gate");
        match error {
            RepositoryError::MigrationStoreRefused(detail) => assert!(
                detail.starts_with("relation ") && detail.contains("holds blob bytes that differ"),
                "the refusal names the blob's relation: {detail}"
            ),
            other => panic!("a blob payload mismatch is refused by the gate: {other:?}"),
        }
    }

    #[test]
    fn candidate_with_a_text_store_root_is_refused() {
        let (_source, repository) = golden_repository();
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
        let expectation = GateExpectation {
            relations: Vec::new(),
            blobs: BTreeMap::new(),
        };
        let detail = verify_candidate(&repository, &candidate, &expectation)
            .expect_err("a candidate whose store root is a text file must not pass the gate");
        assert!(
            detail.starts_with("candidate format context is unavailable"),
            "the text store root is refused at the format context: {detail}"
        );
    }
}
