//! The migration store of a legacy source (UPGRADE-3 steps 2 to 4; ADR 0121):
//! the format-3 relations, object identities and retired legacy files that a
//! migration candidate admits, built from the committed source snapshot.
//!
//! The repository admits exactly the store it is handed, so every fact in it
//! is derived here: the source project is loaded and analysed at its head, the
//! declared relations are built from the committed object identities, and each
//! declared table's legacy rows are read from the same snapshot.

use orna_project::{ObjectIdentityManifest, ProjectLoadMode, ProjectLoader};
use orna_repository::migration_copy::{CandidateGate, MigrationStore, RetiredLegacyPath};
use orna_repository::relation_admission::{OBJECT_IDENTITY_RELATION_ID, StoredObjectIdentity};
use orna_repository::{
    CommittedTreeEntryKind, DatabaseId, Repository, RepositoryError, RepositoryFormat,
};

use crate::format3_store::build_format3_store_relations;
use crate::legacy_store::{
    LegacyReadError, LegacyTableAddress, LegacyTableRows, read_legacy_table_rows,
};
use crate::migration_rows::{
    ExpectedRelation, Format2BlobReader, LegacyTable, migration_row_input_with_format2_blobs,
};
use crate::upgrade_gate::{GateExpectation, verify_candidate};

/// The largest committed `.orna` file a migration retires. It matches the
/// bound on a compact storage file, the largest legacy file a source holds.
const MAX_RETIRED_FILE_BYTES: usize = 64 * 1024 * 1024;

/// The most committed tree entries the retired-file walk lists.
const MAX_RETIRED_TREE_ENTRIES: usize = 1 << 20;

/// The largest Blob a format-2 slot may materialise (ORNA-BLOB-011: 8 MiB).
const FORMAT2_BLOB_LIMIT: u64 = 8 * 1024 * 1024;

/// The format-3 database record a migration writes; never retired.
const DATABASE_RECORD: &str = ".orna/database.orna";

/// The legacy format marker a migration drops; never retired as a file.
const LEGACY_FORMAT_RECORD: &str = ".orna/format.orna";

/// Builds the [`MigrationStore`] of the source repository's committed head for
/// the destination `database`.
///
/// Refuses, with [`RepositoryError::MigrationStoreRefused`], a source that has
/// no head, does not load or analyse, declares a table whose schema cannot be
/// built, or holds legacy rows that no declared relation admits.
pub fn build_migration_store(
    repository: &Repository,
    database: DatabaseId,
) -> Result<MigrationStore, RepositoryError> {
    let (mut store, expectation) = build_store_with_expectation(repository, database)?;
    store.candidate_gate = Some(CandidateGate::new(move |repository, candidate| {
        verify_candidate(repository, candidate, &expectation)
    }));
    Ok(store)
}

/// Builds the store of [`build_migration_store`] together with the content the
/// candidate must carry, without installing the gate that checks it.
pub(crate) fn build_store_with_expectation(
    repository: &Repository,
    database: DatabaseId,
) -> Result<(MigrationStore, GateExpectation), RepositoryError> {
    let commit = repository
        .head()?
        .ok_or_else(|| refused("the source has no committed head"))?;
    let format = repository
        .open_format_context_at_selector(commit.as_str())
        .map_err(RepositoryError::FormatContext)?;
    let format2 = format.repository_format() == RepositoryFormat::Legacy2;
    let project = ProjectLoader::default()
        .with_mode(ProjectLoadMode::Migration)
        .load_committed_snapshot(repository, &commit)
        .map_err(|error| refused(format!("the source does not load: {error:?}")))?;
    let catalogue = project
        .production_catalogue()
        .map_err(|error| refused(format!("the source catalogue is unavailable: {error:?}")))?;
    let analysis = orna_semantic::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(refused("the source does not analyse"));
    }
    let previous = ObjectIdentityManifest::read_committed_if_present(repository, &commit)
        .map_err(|error| refused(format!("the object identities are unreadable: {error:?}")))?;
    let identities = ObjectIdentityManifest::reconcile(project.modules(), previous.as_ref())
        .map_err(|error| refused(format!("the object identities do not reconcile: {error:?}")))?;
    let declared =
        build_format3_store_relations(&project, &analysis, &identities, *database.as_bytes())
            .map_err(|error| {
                refused(format!(
                    "a declared table has no format-3 schema: {error:?}"
                ))
            })?;

    // Each compact table is addressed by its storage id. A declared relation
    // with no compact storage is an editable table, addressed by its key path.
    // Format 2 has no compact-storage-v2 decoder: a snapshot with compact
    // manifests is refused by name, and a format-2 snapshot without them has
    // only editable tables.
    let compact = if format2 {
        if !repository
            .read_compact_manifest_inventory(&commit)?
            .is_empty()
        {
            return Err(RepositoryError::Format2CompactUnimplemented);
        }
        Vec::new()
    } else {
        repository.read_legacy_authoritative_tables(&commit)?
    };
    let mut addresses: Vec<LegacyTableAddress> = compact
        .iter()
        .map(|table| LegacyTableAddress::Relation(*table.table().as_bytes()))
        .collect();
    // Each compact table's storage id must name exactly one unretired Table
    // identity in the legacy sidecar, and that table must be declared. The
    // reconciled identities reuse these ids, so a missing or duplicate id is a
    // typed refusal rather than a fresh identity.
    let compact_ids: Vec<[u8; 16]> = compact
        .iter()
        .map(|table| *table.table().as_bytes())
        .collect();
    if !compact_ids.is_empty() {
        let sidecar = previous
            .as_ref()
            .ok_or_else(|| refused("a compact table has no legacy object identity sidecar"))?;
        let names = sidecar.legacy_table_names(&compact_ids).map_err(|error| {
            refused(format!(
                "the compact tables do not correspond to the legacy identities: {error:?}"
            ))
        })?;
        for id in names.keys() {
            if declared.table_name(id).is_none() {
                return Err(refused("a compact table has no declared relation"));
            }
        }
    }
    for relation in declared.relations() {
        let id = relation.relation_id();
        if addresses
            .iter()
            .any(|address| matches!(address, LegacyTableAddress::Relation(r) if r == id))
        {
            continue;
        }
        let path = declared
            .table_name(id)
            .ok_or_else(|| refused("a declared relation has no source table name"))?;
        addresses.push(LegacyTableAddress::TablePath(path.to_owned()));
    }

    let mut rows = Vec::with_capacity(addresses.len());
    for address in &addresses {
        rows.push(match read_legacy_table_rows(repository, &commit, address) {
            Ok(table_rows) => table_rows,
            // An editable table with no directory in the snapshot has no rows.
            Err(LegacyReadError::MissingTablePath(_)) => LegacyTableRows::absent_loose(),
            Err(error) => return Err(refused(format!("{error}"))),
        });
    }
    let tables: Vec<LegacyTable<'_>> = addresses
        .iter()
        .zip(rows.iter())
        .map(|(address, table_rows)| LegacyTable::new(address, table_rows))
        .collect();
    let format2_blob_reader = |text: &str| {
        repository
            .read_format2_orr_blob(commit.as_str(), text, FORMAT2_BLOB_LIMIT)
            .map_err(|error| error.to_string())
    };
    let format2_blobs: Option<Format2BlobReader<'_>> = format2.then_some(&format2_blob_reader);
    let input = migration_row_input_with_format2_blobs(&declared, &tables, format2_blobs)
        .map_err(|error| refused(format!("{error}")))?;
    let annotation_default_entries = u32::try_from(input.annotation_default_count())
        .map_err(|_| refused("the legacy Blob count exceeds the migration coordinate"))?;
    let descriptors = repository
        .write_migration_blobs(input.blobs())
        .map_err(|error| refused(format!("{error}")))?;
    let (relations, mut expected, blobs) = input
        .into_staged_relations_with_expectation(&descriptors)
        .map_err(|error| refused(format!("{error}")))?;
    let stored_identities = identities.to_stored();
    let identity_rows = stored_identities
        .iter()
        .map(StoredObjectIdentity::inventory_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| refused(format!("the object identities are not canonical: {error}")))?;
    expected.push(ExpectedRelation {
        relation_id: OBJECT_IDENTITY_RELATION_ID,
        rows: identity_rows,
        blobs: Vec::new(),
    });

    let mut retired = Vec::new();
    for entry in repository.list_committed_tree(&commit, MAX_RETIRED_TREE_ENTRIES)? {
        let path = entry
            .path()
            .as_path()
            .to_str()
            .ok_or_else(|| refused("a legacy path is not UTF-8"))?;
        if !path.starts_with(".orna/") || path == DATABASE_RECORD || path == LEGACY_FORMAT_RECORD {
            continue;
        }
        if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
            return Err(refused(format!(
                "the legacy path {path} is not a regular file"
            )));
        }
        let committed = repository.read_committed_file(&commit, path, MAX_RETIRED_FILE_BYTES)?;
        retired.push(RetiredLegacyPath {
            path: path.to_owned(),
            committed,
        });
    }

    let store = MigrationStore {
        relations,
        identities: stored_identities,
        retired,
        annotation_default_entries,
        ..MigrationStore::default()
    };
    Ok((
        store,
        GateExpectation {
            relations: expected,
            blobs,
        },
    ))
}

fn refused(detail: impl Into<String>) -> RepositoryError {
    RepositoryError::MigrationStoreRefused(detail.into())
}
