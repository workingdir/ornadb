//! UPGRADE-3 semantic gate for a migration candidate (ornadb-5il1.315).
//!
//! Admission checks a candidate's shape. This gate checks what the candidate
//! carries: for every relation the migration built from the legacy snapshot,
//! each expected row must be present in the candidate's own store with the same
//! stored value, and the relation must hold no row the snapshot did not yield.
//! The candidate is read back through its private format-3 store root, never
//! through the legacy inputs, so a row lost or altered between the legacy
//! decode and the committed store is refused.
//!
//! Relations the migration did not stage are not inspected by this gate.

use std::collections::BTreeMap;

use orna_repository::{KeyRange, PrivateCommit, Repository, RepositoryFormatContext, TypedKey};
use orna_value::{Raw, decode_canonical_storage_value};

use crate::migration_rows::ExpectedRelation;

/// The number of rows read per page while a candidate relation is walked. It
/// is the largest page `KeyRange` admits.
const PAGE_ROWS: usize = 256;

/// Checks `candidate` against the rows the migration expects, relation by
/// relation. The error names the failed relation and the failed check.
pub(crate) fn verify_candidate(
    repository: &Repository,
    candidate: &PrivateCommit,
    expected: &[ExpectedRelation],
) -> Result<(), String> {
    let context = repository
        .open_private_candidate_format_context(candidate)
        .map_err(|error| format!("candidate format context is unavailable: {error}"))?;
    for relation in expected {
        let actual = candidate_rows(&context, relation.relation_id)?;
        check_relation(relation, &actual)?;
    }
    Ok(())
}

/// Reads every stored row of one candidate relation, keyed by its typed key.
/// Each page resumes strictly after the last key of the previous page, so a
/// repeated key means the store did not advance and is refused.
fn candidate_rows(
    context: &RepositoryFormatContext,
    relation_id: [u8; 16],
) -> Result<BTreeMap<TypedKey, Raw>, String> {
    let label = relation_label(relation_id);
    let rows = context
        .load_row_map(relation_id)
        .map_err(|error| format!("relation {label} row map is unavailable: {error}"))?;
    let graph = context
        .open_native_graph(&rows)
        .map_err(|error| format!("relation {label} graph is unavailable: {error}"))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| format!("relation {label} read scope is unavailable: {error}"))?;

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
            .range_rows(&range, &scope)
            .map_err(|error| format!("relation {label} rows could not be read: {error}"))?;
        let Some(last) = page.last() else {
            break;
        };
        let last_key = last.key().clone();
        for row in &page {
            let fields = graph
                .stored_row_fields(row, &scope)
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

/// The relation identifier in lowercase hex, for error messages.
fn relation_label(relation_id: [u8; 16]) -> String {
    relation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
