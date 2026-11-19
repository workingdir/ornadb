//! The migration store of a legacy source (UPGRADE-3 steps 2 to 4; ADR 0121):
//! the format-3 relations, object identities and retired legacy files that a
//! migration candidate admits, built from the committed source snapshot.
//!
//! The repository admits exactly the store it is handed, so every fact in it
//! is derived here: the source project is loaded and analysed at its head, the
//! declared relations are built from the committed object identities, and each
//! declared table's legacy rows are read from the same snapshot.

use std::collections::{BTreeMap, BTreeSet};

use orna_check::Analysis;
use orna_evaluator::Limits;
use orna_evaluator::row_admission::{RowAdmissionIdentity, RowSource, admit_row_sources};
use orna_project::{ObjectIdentityManifest, ProjectLoader};
use orna_repository::migration_copy::{
    CandidateGate, MigrationStore, RetiredLegacyKind, RetiredLegacyPath,
};
use orna_repository::relation_admission::{OBJECT_IDENTITY_RELATION_ID, StoredObjectIdentity};
use orna_repository::{
    CommittedTreeEntryKind, DatabaseId, Repository, RepositoryError, RepositoryFormat,
};

use crate::format3_store::build_format3_store_relations;
use crate::legacy_store::{
    LegacyReadError, LegacyTableAddress, LegacyTableRows, LooseRowUnit, decode_loose_rows,
    loose_row_unit_paths, read_legacy_table_rows, read_loose_table_units,
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

/// The project id that names the migration's loose-row source units for
/// admission. Each row's source path is `{id}/{table path}/{key}.orna`.
const MIGRATION_ROW_SOURCE_PROJECT: &str = "migration-source";

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
        .load_committed_snapshot(repository, &commit)
        .map_err(|error| refused(format!("the source does not load: {error:?}")))?;
    let catalogue = project
        .production_catalogue()
        .map_err(|error| refused(format!("the source catalogue is unavailable: {error:?}")))?;
    let analysis = orna_check::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(refused("the source does not analyse"));
    }
    let previous = ObjectIdentityManifest::read_committed_if_present(repository, &commit)
        .map_err(|error| refused(format!("the object identities are unreadable: {error:?}")))?;
    let identities = ObjectIdentityManifest::reconcile_for_migration(
        project.modules(),
        previous.as_ref(),
        *database.as_bytes(),
    )
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

    let module_sources: Vec<&str> = project
        .modules()
        .iter()
        .map(|module| module.source.as_str())
        .collect();
    let mut rows = Vec::with_capacity(addresses.len());
    for address in &addresses {
        let table_rows = match address {
            LegacyTableAddress::Relation(_) => read_legacy_table_rows(repository, &commit, address),
            LegacyTableAddress::TablePath(path) => read_admitted_loose_rows(
                repository,
                &commit,
                path,
                &module_sources,
                &analysis,
                *database.as_bytes(),
            ),
        };
        rows.push(match table_rows {
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
            kind: RetiredLegacyKind::Metadata,
            committed,
        });
    }

    // The loose row units of every editable table the migration read are
    // retired with it. They come from the same enumeration the row reader
    // decodes, so a row file is retired exactly when its row was migrated.

    let mut loose_paths = BTreeSet::new();
    for address in &addresses {
        if let LegacyTableAddress::TablePath(table_path) = address {
            loose_paths.extend(
                loose_row_unit_paths(repository, &commit, table_path).map_err(|error| {
                    refused(format!("the loose rows are unreadable: {error:?}"))
                })?,
            );
        }
    }
    for path in loose_paths {
        let committed = repository.read_committed_file(&commit, &path, MAX_RETIRED_FILE_BYTES)?;
        retired.push(RetiredLegacyPath {
            path,
            kind: RetiredLegacyKind::LooseRowUnit,
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

/// Reads one editable table's loose row units and admits every unit against the
/// table's declared schema before any unit is decoded (UPGRADE-3 step 3; ADR
/// 0121). One refused row refuses the whole read.
fn read_admitted_loose_rows(
    repository: &Repository,
    commit: &orna_repository::GitCommitRef,
    table_path: &str,
    module_sources: &[&str],
    analysis: &Analysis,
    database_id: [u8; 16],
) -> Result<LegacyTableRows, LegacyReadError> {
    let units = read_loose_table_units(repository, commit, table_path)?;
    admit_loose_units(table_path, &units, module_sources, analysis, database_id)?;
    decode_loose_rows(table_path, units)
}

/// Admits every loose row unit of `table_path` against the project's declared
/// table schemas (ADR 0121). Each unit is one source of the admission project,
/// named by its repository path, keeping the `.orna` suffix that the key decoder
/// reads. The first unit that is not admitted refuses the table.
fn admit_loose_units(
    table_path: &str,
    units: &[(LooseRowUnit, Vec<u8>)],
    module_sources: &[&str],
    analysis: &Analysis,
    database_id: [u8; 16],
) -> Result<(), LegacyReadError> {
    let source_ids: Vec<String> = units
        .iter()
        .map(|(unit, _)| format!("{MIGRATION_ROW_SOURCE_PROJECT}/{}", unit.path))
        .collect();
    let texts = units
        .iter()
        .map(|(unit, bytes)| {
            std::str::from_utf8(bytes)
                .map_err(|_| LegacyReadError::UndecodableRowUnit(unit.path.clone()))
        })
        .collect::<Result<Vec<&str>, _>>()?;
    let rows: Vec<RowSource<'_>> = source_ids
        .iter()
        .zip(texts)
        .map(|(source_id, source)| RowSource {
            source_id: source_id.as_str(),
            source,
        })
        .collect();
    let identity = RowAdmissionIdentity {
        database_id,
        tables: BTreeMap::new(),
    };
    admit_row_sources(
        MIGRATION_ROW_SOURCE_PROJECT,
        module_sources,
        analysis,
        &rows,
        &identity,
        Limits::default(),
    )
    .map(|_admitted| ())
    .map_err(|diagnostic| LegacyReadError::RowNotAdmitted {
        table: table_path.to_owned(),
        code: diagnostic.code().to_owned(),
    })
}

fn refused(detail: impl Into<String>) -> RepositoryError {
    RepositoryError::MigrationStoreRefused(detail.into())
}

#[cfg(test)]
mod tests {
    use orna_check::{ModuleInput, analyze};

    use super::{LegacyReadError, LooseRowUnit, admit_loose_units};

    /// One root table `Note` keyed by an integer, with one stored text field.
    const NOTE_MODULE: &str = "pub table Note(id: Int) {\n    text: Str,\n}\n";

    /// The one loose row of `Note` at key `1`, carrying `source` as its body.
    fn note_row(source: &str) -> Vec<(LooseRowUnit, Vec<u8>)> {
        let unit = LooseRowUnit {
            path: "Note/1.orna".to_owned(),
            key_path: vec!["1.orna".to_owned()],
        };
        vec![(unit, source.as_bytes().to_vec())]
    }

    #[test]
    fn a_row_whose_fields_match_its_declared_table_is_admitted() {
        let analysis = analyze(&[ModuleInput::new("main.orna", NOTE_MODULE)]);
        assert!(analysis.is_ok(), "the Note module analyses");
        let units = note_row("{ text: \"Alpha\", }\n");
        admit_loose_units("Note", &units, &[NOTE_MODULE], &analysis, [0; 16])
            .expect("a row that matches its declared table is admitted");
    }

    #[test]
    fn a_row_with_a_field_its_table_does_not_declare_is_refused() {
        let analysis = analyze(&[ModuleInput::new("main.orna", NOTE_MODULE)]);
        let units = note_row("{ text: \"Alpha\", nickname: \"Al\", }\n");
        match admit_loose_units("Note", &units, &[NOTE_MODULE], &analysis, [0; 16]) {
            Err(LegacyReadError::RowNotAdmitted { table, code }) => {
                assert_eq!(table, "Note");
                assert_eq!(code, "ORNA-CONFORMANCE-ROW-UNKNOWN");
            }
            other => panic!("a row with an undeclared field is refused: {other:?}"),
        }
    }
}
