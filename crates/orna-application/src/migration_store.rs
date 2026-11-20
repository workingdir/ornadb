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
use orna_evaluator::row_admission::{
    AdmittedLooseRow, RowAdmissionIdentity, RowSource, admit_row_sources, admit_stored_row,
    declared_table, resolve_stored_references, table_owner_for_path,
};
use orna_project::{ObjectIdentityManifest, ProjectLoader, StoredSchema};
use orna_repository::migration_copy::{
    CandidateGate, MigrationStore, RetiredLegacyKind, RetiredLegacyPath,
};
use orna_repository::relation_admission::{
    OBJECT_IDENTITY_RELATION_ID, StagedRelation, StoredObjectIdentity,
};
use orna_repository::{
    CommittedTreeEntryKind, DatabaseId, Repository, RepositoryError, RepositoryFormat,
};
use orna_value::{Raw, Value};

use crate::format3_store::{StagedRelations, build_format3_store_relations};
use crate::legacy_store::{
    LegacyFieldName, LegacyReadError, LegacyTableAddress, LegacyTableRows, LooseRowUnit,
    decode_loose_rows, loose_row_unit_paths, read_legacy_table_rows, read_loose_table_units,
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

/// The tag of a multi-column composite key: a tuple of its key values.
const TUPLE_TAG: u64 = 60015;

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
            .map_err(RepositoryError::MigrationStoreRefused)
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
    let mut admitted = Vec::new();
    let mut rows = Vec::with_capacity(addresses.len());
    for address in &addresses {
        let table_rows = match address {
            LegacyTableAddress::Relation(relation_id) => {
                read_admitted_compact_rows(repository, &commit, &declared, &analysis, relation_id)
            }
            LegacyTableAddress::TablePath(path) => {
                read_admitted_loose_rows(repository, &commit, path, &module_sources, &analysis)
            }
        };
        let table_rows = match table_rows {
            Ok((table_rows, units)) => {
                admitted.extend(units);
                table_rows
            }
            // An editable table with no directory in the snapshot has no rows.
            Err(LegacyReadError::MissingTablePath(_)) => LegacyTableRows::absent_loose(),
            Err(error) => return Err(refused(format!("{error}"))),
        };
        rows.push(table_rows);
    }
    resolve_admitted_references(&declared, &analysis, &admitted, *database.as_bytes())?;
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

/// Resolves every stored table reference of the admitted rows against the
/// snapshot's admitted keys (ADR 0121; ORNA-CONFORMANCE-ROW-REFERENCE).
fn resolve_admitted_references(
    declared: &StagedRelations,
    analysis: &Analysis,
    admitted: &[AdmittedLooseRow],
    database_id: [u8; 16],
) -> Result<(), RepositoryError> {
    let table_paths = declared
        .relations()
        .iter()
        .map(|relation| {
            let id = *relation.relation_id();
            declared
                .table_name(&id)
                .map(|path| (id, path))
                .ok_or_else(|| refused("a declared relation has no source table name"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let identity = stored_reference_identity(&table_paths, analysis, admitted, database_id)?;
    resolve_stored_references(admitted, &identity).map_err(|diagnostic| {
        refused(format!(
            "a stored table reference is not admitted: {}",
            diagnostic.code()
        ))
    })
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
) -> Result<(LegacyTableRows, Vec<AdmittedLooseRow>), LegacyReadError> {
    let units = read_loose_table_units(repository, commit, table_path)?;
    let admitted = admit_loose_units(table_path, &units, module_sources, analysis)?;
    Ok((decode_loose_rows(table_path, units)?, admitted))
}

/// Reads one compact table and admits every stored row against its declared
/// table before the rows are decoded (UPGRADE-3 step 3; ADR 0121). The declared
/// table is found by its storage id; a compact table with no declared relation
/// is an unknown relation.
fn read_admitted_compact_rows(
    repository: &Repository,
    commit: &orna_repository::GitCommitRef,
    declared: &StagedRelations,
    analysis: &Analysis,
    relation_id: &[u8; 16],
) -> Result<(LegacyTableRows, Vec<AdmittedLooseRow>), LegacyReadError> {
    let relation = declared
        .relations()
        .iter()
        .find(|relation| relation.relation_id() == relation_id)
        .ok_or(LegacyReadError::UnknownRelation(*relation_id))?;
    let table_path = declared
        .table_name(relation_id)
        .ok_or(LegacyReadError::UnknownRelation(*relation_id))?;
    let table_rows = read_legacy_table_rows(
        repository,
        commit,
        &LegacyTableAddress::Relation(*relation_id),
    )?;
    let admitted = admit_compact_rows(&table_rows, relation, table_path, analysis)?;
    Ok((table_rows, admitted))
}

/// Admits every loose row unit of `table_path` against the project's declared
/// table schemas (ADR 0121). Each unit is one source of the admission project,
/// named by its repository path, keeping the `.orna` suffix that the key decoder
/// reads. Any unit that is not admitted refuses the table. The table-wide
/// diagnostic does not name a unit, so the first unit refused when admitted
/// alone is named in the refusal.
fn admit_loose_units(
    table_path: &str,
    units: &[(LooseRowUnit, Vec<u8>)],
    module_sources: &[&str],
    analysis: &Analysis,
) -> Result<Vec<AdmittedLooseRow>, LegacyReadError> {
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
    match admit_row_sources(
        MIGRATION_ROW_SOURCE_PROJECT,
        module_sources,
        analysis,
        &rows,
        Limits::default(),
    ) {
        Ok(admitted) => Ok(admitted),
        Err(diagnostic) => {
            let unit = rows.iter().zip(units).find_map(|(row, (unit, _))| {
                admit_row_sources(
                    MIGRATION_ROW_SOURCE_PROJECT,
                    module_sources,
                    analysis,
                    std::slice::from_ref(row),
                    Limits::default(),
                )
                .err()
                .map(|_| unit.path.clone())
            });
            Err(LegacyReadError::RowNotAdmitted {
                table: table_path.to_owned(),
                unit,
                code: diagnostic.code().to_owned(),
            })
        }
    }
}

/// Admits every stored row of one compact table against its declared table
/// (UPGRADE-3 step 3; ADR 0121). A multi-column key is stored as one tuple, and
/// each field is named by its declared object id. The record holds the live
/// projection, so it may repeat key columns; those are checked against the
/// stored key and are not part of the body.
fn admit_compact_rows(
    table_rows: &LegacyTableRows,
    relation: &StagedRelation,
    table_path: &str,
    analysis: &Analysis,
) -> Result<Vec<AdmittedLooseRow>, LegacyReadError> {
    let refuse = |code: &str| LegacyReadError::RowNotAdmitted {
        table: table_path.to_owned(),
        unit: None,
        code: code.to_owned(),
    };
    let owner = table_owner_for_path(analysis, table_path)
        .map_err(|diagnostic| refuse(diagnostic.code()))?;
    let declared =
        declared_table(analysis, &owner).map_err(|diagnostic| refuse(diagnostic.code()))?;
    let admission = declared
        .admission
        .as_ref()
        .ok_or_else(|| refuse("ORNA-CONFORMANCE-ROW-SCHEMA"))?;
    let key_columns = admission
        .keys
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    let schema = StoredSchema::new(relation.descriptor())
        .map_err(|_| LegacyReadError::UndecodableCompactRow)?;
    table_rows
        .rows()
        .iter()
        .map(|row| -> Result<AdmittedLooseRow, LegacyReadError> {
            let stored = row
                .stored_key()
                .ok_or(LegacyReadError::UndecodableCompactRow)?;
            let key = compact_key_components(stored, key_columns.len())?;
            let fields = row
                .fields()
                .iter()
                .map(|field| -> Result<(String, Raw), LegacyReadError> {
                    let name = match field.name() {
                        LegacyFieldName::Declared(name) => name.clone(),
                        LegacyFieldName::ObjectId(id) => schema
                            .fields()
                            .iter()
                            .find(|candidate| candidate.id() == id)
                            .map(|candidate| candidate.name().to_owned())
                            .ok_or_else(|| refuse("ORNA-CONFORMANCE-ROW-UNKNOWN"))?,
                    };
                    Ok((name, field.value().clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let body = non_key_fields(&key_columns, &key, fields).map_err(refuse)?;
            admit_stored_row(analysis, &owner, key, &body)
                .map_err(|diagnostic| refuse(diagnostic.code()))
        })
        .collect()
}

/// The body fields of one compact record: every field that is not a key column.
/// A key column the record repeats must appear once and must equal its
/// component of the stored key; otherwise the row is refused with the code
/// returned.
fn non_key_fields(
    key_columns: &[&str],
    key: &[Value],
    fields: Vec<(String, Raw)>,
) -> Result<Vec<(String, Raw)>, &'static str> {
    let mut repeated = BTreeSet::new();
    let mut body = Vec::with_capacity(fields.len());
    for (name, raw) in fields {
        let Some(position) = key_columns.iter().position(|column| *column == name) else {
            body.push((name, raw));
            continue;
        };
        if !repeated.insert(position) {
            return Err("ORNA-CONFORMANCE-ROW-DUPLICATE");
        }
        if key.get(position).map(Value::raw) != Some(&raw) {
            return Err("ORNA-CONFORMANCE-ROW-PATH");
        }
    }
    Ok(body)
}

/// The key values of one compact row in key-column order. A multi-column key
/// (`arity` of two or more) is stored as one tuple of its columns; a
/// single-column key is the stored value itself.
fn compact_key_components(stored: &Value, arity: usize) -> Result<Vec<Value>, LegacyReadError> {
    if arity < 2 {
        return Ok(vec![stored.clone()]);
    }
    let Raw::Tag(TUPLE_TAG, parts) = stored.raw() else {
        return Ok(vec![stored.clone()]);
    };
    let Raw::Array(parts) = parts.as_ref() else {
        return Ok(vec![stored.clone()]);
    };
    parts
        .iter()
        .map(|part| Value::new(part.clone()).map_err(|_| LegacyReadError::UndecodableCompactRow))
        .collect()
}

/// The snapshot identity that stored table references resolve against: each
/// declared table's owner and the keys its admitted rows carry (ADR 0121).
fn stored_reference_identity(
    table_paths: &[([u8; 16], &str)],
    analysis: &Analysis,
    admitted: &[AdmittedLooseRow],
    database_id: [u8; 16],
) -> Result<RowAdmissionIdentity, RepositoryError> {
    let mut tables = BTreeMap::new();
    for (id, path) in table_paths {
        let owner = table_owner_for_path(analysis, path).map_err(|diagnostic| {
            refused(format!(
                "a declared table has no admission owner: {}",
                diagnostic.code()
            ))
        })?;
        let keys: Vec<Vec<Value>> = admitted
            .iter()
            .filter(|row| row.owner == owner)
            .map(|row| row.key.clone())
            .collect();
        tables.insert(*id, (owner, keys));
    }
    Ok(RowAdmissionIdentity {
        database_id,
        tables,
    })
}

fn refused(detail: impl Into<String>) -> RepositoryError {
    RepositoryError::MigrationStoreRefused(detail.into())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use orna_check::{ModuleInput, analyze};
    use orna_evaluator::row_admission::{AdmittedLooseRow, resolve_stored_references};
    use orna_value::{Raw, Value};

    use super::{
        LegacyReadError, LooseRowUnit, TUPLE_TAG, admit_loose_units, compact_key_components,
        non_key_fields, stored_reference_identity,
    };

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
        admit_loose_units("Note", &units, &[NOTE_MODULE], &analysis)
            .expect("a row that matches its declared table is admitted");
    }

    #[test]
    fn a_row_with_a_field_its_table_does_not_declare_is_refused() {
        let analysis = analyze(&[ModuleInput::new("main.orna", NOTE_MODULE)]);
        let units = note_row("{ text: \"Alpha\", nickname: \"Al\", }\n");
        match admit_loose_units("Note", &units, &[NOTE_MODULE], &analysis) {
            Err(LegacyReadError::RowNotAdmitted { table, unit, code }) => {
                assert_eq!(table, "Note");
                assert_eq!(unit.as_deref(), Some("Note/1.orna"));
                assert_eq!(code, "ORNA-CONFORMANCE-ROW-UNKNOWN");
            }
            other => panic!("a row with an undeclared field is refused: {other:?}"),
        }
    }

    #[test]
    fn a_compact_tuple_key_splits_into_its_columns_and_a_single_key_stays_whole() {
        let columns = vec![
            Value::new(Raw::Int(1.into())).expect("a canonical key"),
            Value::new(Raw::Text("two".into())).expect("a canonical key"),
        ];
        let tuple = Value::new(Raw::Tag(
            TUPLE_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Text("two".into()),
            ])),
        ))
        .expect("a canonical tuple key");
        assert_eq!(
            compact_key_components(&tuple, 2).expect("a tuple splits"),
            columns
        );
        let whole = compact_key_components(&tuple, 1).expect("a one-column key stays whole");
        assert_eq!(whole, vec![tuple]);
        let single = Value::new(Raw::Int(7.into())).expect("a canonical key");
        let passed = compact_key_components(&single, 1).expect("a single key passes through");
        assert_eq!(passed, vec![single]);
    }

    #[test]
    fn a_compact_record_drops_repeated_key_columns_that_agree_with_the_key() {
        let key = vec![
            Value::new(Raw::Int(1.into())).expect("a canonical key"),
            Value::new(Raw::Text("two".into())).expect("a canonical key"),
        ];
        let columns = ["a", "b"];
        let record = vec![
            ("b".to_owned(), Raw::Text("two".into())),
            ("c".to_owned(), Raw::Int(3.into())),
            ("a".to_owned(), Raw::Int(1.into())),
        ];
        assert_eq!(
            non_key_fields(&columns, &key, record),
            Ok(vec![("c".to_owned(), Raw::Int(3.into()))])
        );
    }

    #[test]
    fn a_compact_record_refuses_a_repeated_key_column_that_disagrees_or_repeats() {
        let key = vec![Value::new(Raw::Int(1.into())).expect("a canonical key")];
        let columns = ["id"];
        let disagreeing = vec![("id".to_owned(), Raw::Int(2.into()))];
        assert_eq!(
            non_key_fields(&columns, &key, disagreeing),
            Err("ORNA-CONFORMANCE-ROW-PATH")
        );
        let repeated = vec![
            ("id".to_owned(), Raw::Int(1.into())),
            ("id".to_owned(), Raw::Int(1.into())),
        ];
        assert_eq!(
            non_key_fields(&columns, &key, repeated),
            Err("ORNA-CONFORMANCE-ROW-DUPLICATE")
        );
    }

    #[test]
    fn a_stored_reference_resolves_only_to_an_admitted_key_of_its_database() {
        let analysis = analyze(&[ModuleInput::new("main.orna", NOTE_MODULE)]);
        let units = note_row("{ text: \"Alpha\", }\n");
        let target = admit_loose_units("Note", &units, &[NOTE_MODULE], &analysis)
            .expect("the Note row is admitted");
        let database = [1; 16];
        let note_table = [7; 16];
        let identity =
            stored_reference_identity(&[(note_table, "Note")], &analysis, &target, database)
                .expect("the identity of the declared table builds");
        let owner = target[0].owner.clone();
        let resolve = |db: [u8; 16], key: Raw| {
            let link = Value::table_reference(db, note_table, key).expect("a stored reference");
            let referrer = AdmittedLooseRow {
                owner: owner.clone(),
                key: vec![Value::new(Raw::Int(2.into())).expect("a canonical key")],
                body: BTreeMap::from([("link".to_owned(), link)]),
            };
            resolve_stored_references(&[referrer], &identity)
        };
        resolve(database, Raw::Int(1.into())).expect("an admitted key resolves");
        let missing = resolve(database, Raw::Int(9.into())).expect_err("a missing key is refused");
        assert_eq!(missing.code(), "ORNA-CONFORMANCE-ROW-REFERENCE");
        let foreign = resolve([2; 16], Raw::Int(1.into())).expect_err("a foreign database");
        assert_eq!(foreign.code(), "ORNA-CONFORMANCE-ROW-REFERENCE");
    }
}
