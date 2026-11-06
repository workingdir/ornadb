//! UPGRADE-3 row input: legacy rows mapped to the declared relations.
//!
//! The copy admits rows through `StagedRelation::with_rows`. This module builds
//! that input purely from a legacy snapshot and the committed schema: each
//! legacy table is mapped to the declared relation it answers, each relation
//! takes the one source the decoder chooses, and each row is lowered to the
//! canonical stored tuple the format-3 row writer admits. It writes nothing.
//! Every mismatch fails closed with a typed error.

use std::collections::{BTreeMap, BTreeSet};

use orna_project::StoredSchema;
use orna_repository::TypedKey;
use orna_repository::relation_admission::StagedRelation;
use orna_value::{Raw, encode_canonical_storage_value};

use crate::format3_store::StagedRelations;
use crate::legacy_store::{
    LegacyFieldName, LegacyReadError, LegacyTableAddress, LegacyTableRows, single_legacy_source,
};

/// One legacy table the copy reads, with the address it was read from.
#[derive(Clone, Copy, Debug)]
pub struct LegacyTable<'a> {
    address: &'a LegacyTableAddress,
    rows: &'a LegacyTableRows,
}

impl<'a> LegacyTable<'a> {
    /// Pairs a legacy table's committed address with its decoded rows.
    pub const fn new(address: &'a LegacyTableAddress, rows: &'a LegacyTableRows) -> Self {
        Self { address, rows }
    }
}

/// One declared relation with the canonical stored tuples the copy admits.
#[derive(Debug)]
pub struct MigrationRelation {
    relation: StagedRelation,
    rows: Vec<(TypedKey, Vec<u8>)>,
}

impl MigrationRelation {
    /// The declared relation's id.
    #[must_use]
    pub const fn relation_id(&self) -> &[u8; 16] {
        self.relation.relation_id()
    }

    /// The canonical stored tuples, in the key order the legacy reader returned.
    #[must_use]
    pub fn rows(&self) -> &[(TypedKey, Vec<u8>)] {
        &self.rows
    }
}

/// The admitted-row input of one migration: every declared relation with its
/// legacy rows, ready for `StagedRelation::with_rows`.
#[derive(Debug)]
pub struct MigrationRowInput {
    relations: Vec<MigrationRelation>,
}

impl MigrationRowInput {
    /// The relations with their rows, in declared module and table order.
    #[must_use]
    pub fn relations(&self) -> &[MigrationRelation] {
        &self.relations
    }

    /// Consumes the input into staged relations carrying their rows.
    #[must_use]
    pub fn into_staged_relations(self) -> Vec<StagedRelation> {
        self.relations
            .into_iter()
            .map(|migration| migration.relation.with_rows(migration.rows))
            .collect()
    }
}

/// Why a legacy snapshot does not map onto the declared schema.
#[derive(Debug)]
pub enum MigrationRowError {
    /// A legacy table names no declared relation (compact storage id or
    /// editable table path).
    UnmappableTable { table: String },
    /// A declared relation has no legacy table to supply its rows.
    RelationWithoutTable { relation: String },
    /// The declared relation takes its key from an automatic counter, which
    /// the copy cannot admit from legacy keys yet.
    AutomaticKeyUnsupported { relation: String },
    /// Two legacy tables of the same kind answer one relation.
    DuplicateSource { relation: String },
    /// The legacy reader refused the relation's sources (for example a
    /// cross-source conflict).
    Source(LegacyReadError),
    /// A legacy field names no stored field of the declared relation.
    UnknownField { relation: String, field: String },
    /// One legacy field name appears twice in one row.
    DuplicateField { relation: String, field: String },
    /// A declared relation's storage descriptor could not be read.
    InvalidDescriptor { relation: String },
    /// A row does not lower to its relation's stored fields.
    InvalidRow {
        relation: String,
        key: String,
        reason: String,
    },
    /// The canonical stored tuple could not be encoded.
    Encode { relation: String },
}

impl std::fmt::Display for MigrationRowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnmappableTable { table } => {
                write!(formatter, "legacy table {table} names no declared relation")
            }
            Self::RelationWithoutTable { relation } => {
                write!(
                    formatter,
                    "declared relation {relation} has no legacy table"
                )
            }
            Self::AutomaticKeyUnsupported { relation } => write!(
                formatter,
                "declared relation {relation} has an automatic key, which the copy does not admit"
            ),
            Self::DuplicateSource { relation } => {
                write!(
                    formatter,
                    "two legacy tables of one kind answer relation {relation}"
                )
            }
            Self::Source(error) => write!(formatter, "{error}"),
            Self::UnknownField { relation, field } => {
                write!(formatter, "relation {relation} has no stored field {field}")
            }
            Self::DuplicateField { relation, field } => {
                write!(
                    formatter,
                    "relation {relation} row names field {field} twice"
                )
            }
            Self::InvalidDescriptor { relation } => {
                write!(
                    formatter,
                    "relation {relation} has an unreadable storage descriptor"
                )
            }
            Self::InvalidRow {
                relation,
                key,
                reason,
            } => write!(
                formatter,
                "row {key} of relation {relation} does not lower: {reason}"
            ),
            Self::Encode { relation } => {
                write!(formatter, "relation {relation} row cannot be encoded")
            }
        }
    }
}

impl std::error::Error for MigrationRowError {}

/// The legacy sources one declared relation may draw on: its compact table and
/// its editable table.
#[derive(Default)]
struct Sources<'a> {
    compact: Option<&'a LegacyTableRows>,
    loose: Option<&'a LegacyTableRows>,
}

/// Builds the migration row input for `declared` from `legacy` tables.
///
/// The relation of a compact table is its storage table id; the relation of an
/// editable table is the declared table whose source name is its key-path
/// directory. A table that names no declared relation fails closed, and so does
/// a declared relation that no legacy table answers.
pub fn migration_row_input(
    declared: &StagedRelations,
    legacy: &[LegacyTable<'_>],
) -> Result<MigrationRowInput, MigrationRowError> {
    let mut sources = BTreeMap::<[u8; 16], Sources<'_>>::new();
    for table in legacy {
        let (relation_id, is_compact) = match table.address {
            LegacyTableAddress::Relation(id) => {
                if !declared.relations().iter().any(|r| r.relation_id() == id) {
                    return Err(MigrationRowError::UnmappableTable {
                        table: uuid_text(id),
                    });
                }
                (*id, true)
            }
            LegacyTableAddress::TablePath(path) => {
                let relation = declared
                    .relations()
                    .iter()
                    .map(|relation| *relation.relation_id())
                    .find(|id| declared.table_name(id) == Some(path.as_str()))
                    .ok_or_else(|| MigrationRowError::UnmappableTable {
                        table: path.clone(),
                    })?;
                (relation, false)
            }
        };
        let slot = sources.entry(relation_id).or_default();
        let target = if is_compact {
            &mut slot.compact
        } else {
            &mut slot.loose
        };
        if target.is_some() {
            return Err(MigrationRowError::DuplicateSource {
                relation: relation_name(declared, &relation_id),
            });
        }
        *target = Some(table.rows);
    }

    let mut relations = Vec::with_capacity(declared.relations().len());
    for relation in declared.relations() {
        let id = *relation.relation_id();
        let name = relation_name(declared, &id);
        if relation.automatic_key() {
            return Err(MigrationRowError::AutomaticKeyUnsupported { relation: name });
        }
        let Some(source) = sources.get(&id) else {
            return Err(MigrationRowError::RelationWithoutTable { relation: name });
        };
        let chosen = single_legacy_source(&name, source.compact, source.loose)
            .map_err(MigrationRowError::Source)?;
        let rows = match chosen {
            Some(rows) => encode_rows(relation, &name, rows)?,
            None => Vec::new(),
        };
        relations.push(MigrationRelation {
            relation: relation.clone(),
            rows,
        });
    }
    Ok(MigrationRowInput { relations })
}

/// Lowers each legacy row to its relation's canonical stored tuple.
fn encode_rows(
    relation: &StagedRelation,
    name: &str,
    rows: &LegacyTableRows,
) -> Result<Vec<(TypedKey, Vec<u8>)>, MigrationRowError> {
    let descriptor = relation.descriptor();
    let schema =
        StoredSchema::new(descriptor).map_err(|_| MigrationRowError::InvalidDescriptor {
            relation: name.to_owned(),
        })?;
    let mut encoded = Vec::with_capacity(rows.rows().len());
    for row in rows.rows() {
        let mut values = BTreeMap::<&str, &Raw>::new();
        let mut named = BTreeSet::<String>::new();
        for field in row.fields() {
            let field_name = match field.name() {
                LegacyFieldName::Declared(declared) => declared.clone(),
                LegacyFieldName::ObjectId(id) => schema
                    .fields()
                    .iter()
                    .find(|candidate| candidate.id() == id)
                    .map(|candidate| candidate.name().to_owned())
                    .ok_or_else(|| MigrationRowError::UnknownField {
                        relation: name.to_owned(),
                        field: uuid_text(id),
                    })?,
            };
            let Some(stored) = schema.field(&field_name) else {
                return Err(MigrationRowError::UnknownField {
                    relation: name.to_owned(),
                    field: field_name,
                });
            };
            if !named.insert(stored.name().to_owned()) {
                return Err(MigrationRowError::DuplicateField {
                    relation: name.to_owned(),
                    field: field_name,
                });
            }
            values.insert(stored.name(), field.value());
        }
        let stored_fields =
            schema
                .lower_stored_fields(&values)
                .map_err(|error| MigrationRowError::InvalidRow {
                    relation: name.to_owned(),
                    key: format!("{:?}", row.key()),
                    reason: error.to_string(),
                })?;
        let tuple = encode_canonical_storage_value(&Raw::Array(stored_fields)).map_err(|_| {
            MigrationRowError::Encode {
                relation: name.to_owned(),
            }
        })?;
        encoded.push((row.key().clone(), tuple));
    }
    Ok(encoded)
}

fn relation_name(declared: &StagedRelations, id: &[u8; 16]) -> String {
    declared
        .table_name(id)
        .map_or_else(|| uuid_text(id), str::to_owned)
}

fn uuid_text(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use orna_repository::TypedKey;
    use orna_repository::relation_admission::StagedRelation;
    use orna_value::{
        Raw, SchemaDescriptor, decode_canonical_storage_value, encode_canonical_storage_value,
    };

    use super::{LegacyTable, MigrationRowError, migration_row_input};
    use crate::format3_store::StagedRelations;
    use crate::legacy_store::{
        LegacyField, LegacyFieldName, LegacyReadError, LegacyRow, LegacyTableAddress,
        LegacyTableRows,
    };
    use std::{path::Path, process::Command};

    use orna_repository::{GitCommitRef, Repository, Uuid, golden_compact_schemas};
    use tempfile::TempDir;

    use crate::legacy_store::read_legacy_table_rows;

    const TABLE: [u8; 16] = [0x71; 16];
    const KEY_FIELD: [u8; 16] = [0x72; 16];
    const NAME_FIELD: [u8; 16] = [0x73; 16];
    const EMAILS_FIELD: [u8; 16] = [0x74; 16];
    const PATH: &str = "contacts/Contact";

    fn tagged(id: [u8; 16]) -> Raw {
        Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
    }

    /// The descriptor of `contacts/Contact`: a `Str` key `id` and stored
    /// `name: Str`, `emails: List<Str>`, in the shape the schema builder writes.
    fn descriptor() -> SchemaDescriptor {
        descriptor_with(Vec::new())
    }

    /// The contacts descriptor with `extra` stored fields appended.
    fn descriptor_with(extra: Vec<Raw>) -> SchemaDescriptor {
        let field = |id: [u8; 16], name: &str, node: Raw, role: i64| {
            Raw::Array(vec![
                tagged(id),
                Raw::Text(name.to_owned()),
                node,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        let str_node = || Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".to_owned())]);
        let list_node = Raw::Array(vec![Raw::Int(1.into()), str_node()]);
        SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), tagged(TABLE)),
            (Raw::Int(2.into()), Raw::Array(vec![tagged(KEY_FIELD)])),
            (
                Raw::Int(3.into()),
                Raw::Array(
                    [
                        field(KEY_FIELD, "id", str_node(), 0),
                        field(NAME_FIELD, "name", str_node(), 1),
                        field(EMAILS_FIELD, "emails", list_node, 1),
                    ]
                    .into_iter()
                    .chain(extra)
                    .collect(),
                ),
            ),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("the contacts descriptor is valid")
    }

    /// The contacts descriptor extended with an unannotated Blob `avatar`.
    fn blob_declared() -> StagedRelations {
        const AVATAR: [u8; 16] = [0x75; 16];
        let blob_node = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".to_owned())]);
        let avatar = Raw::Array(vec![
            tagged(AVATAR),
            Raw::Text("avatar".to_owned()),
            blob_node,
            Raw::Int(1.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ]);
        StagedRelations::for_tests(
            vec![StagedRelation::new(TABLE, descriptor_with(vec![avatar]))],
            vec![(TABLE, PATH)],
        )
    }

    fn declared() -> StagedRelations {
        StagedRelations::for_tests(
            vec![StagedRelation::new(TABLE, descriptor())],
            vec![(TABLE, PATH)],
        )
    }

    fn text(value: &str) -> Raw {
        Raw::Text(value.to_owned())
    }

    fn declared_field(name: &str, value: Raw) -> LegacyField {
        LegacyField::for_tests(LegacyFieldName::Declared(name.to_owned()), value)
    }

    /// Two loose contacts, `alice` and `bob`, as the legacy reader returns them.
    fn contacts() -> LegacyTableRows {
        LegacyTableRows::loose_for_tests(vec![
            LegacyRow::for_tests(
                TypedKey::Text("alice".to_owned()),
                vec![
                    declared_field("name", text("Alice Smith")),
                    declared_field("emails", Raw::Array(vec![text("alice@example.com")])),
                ],
            ),
            LegacyRow::for_tests(
                TypedKey::Text("bob".to_owned()),
                vec![
                    declared_field("name", text("Bob Jones")),
                    declared_field("emails", Raw::Array(Vec::new())),
                ],
            ),
        ])
    }

    fn map_contacts(
        declared: &StagedRelations,
        rows: &LegacyTableRows,
    ) -> Result<super::MigrationRowInput, MigrationRowError> {
        let address = LegacyTableAddress::TablePath(PATH.to_owned());
        migration_row_input(declared, &[LegacyTable::new(&address, rows)])
    }

    /// Each admitted tuple decodes to the legacy reader's field values in
    /// descriptor order, and the tuple is the canonical encoding of them.
    #[test]
    fn decoded_rows_equal_the_legacy_reader_values() {
        let rows = contacts();
        let input = map_contacts(&declared(), &rows).expect("the declared contacts relation maps");
        let admitted = input.relations()[0].rows();
        assert_eq!(admitted.len(), 2);

        let expected = [
            (
                TypedKey::Text("alice".to_owned()),
                Raw::Array(vec![
                    text("Alice Smith"),
                    Raw::Array(vec![text("alice@example.com")]),
                ]),
            ),
            (
                TypedKey::Text("bob".to_owned()),
                Raw::Array(vec![text("Bob Jones"), Raw::Array(Vec::new())]),
            ),
        ];
        for ((key, tuple), (expected_key, expected_fields)) in admitted.iter().zip(expected) {
            assert_eq!(key, &expected_key);
            assert_eq!(
                decode_canonical_storage_value(tuple).expect("the tuple decodes"),
                expected_fields
            );
            assert_eq!(
                tuple,
                &encode_canonical_storage_value(&expected_fields).expect("the tuple encodes")
            );
        }
    }

    /// Two runs over the same legacy input produce identical rows.
    #[test]
    fn mapping_is_deterministic() {
        let rows = contacts();
        let first = map_contacts(&declared(), &rows).expect("first run maps");
        let second = map_contacts(&declared(), &rows).expect("second run maps");
        assert_eq!(first.relations()[0].rows(), second.relations()[0].rows());
    }

    /// A legacy table that names no declared relation fails closed.
    #[test]
    fn unmappable_table_fails_closed() {
        let rows = contacts();
        let stranger = LegacyTableAddress::TablePath("contacts/Stranger".to_owned());
        let error = migration_row_input(&declared(), &[LegacyTable::new(&stranger, &rows)])
            .expect_err("an undeclared table has no relation");
        assert!(matches!(
            error,
            MigrationRowError::UnmappableTable { ref table } if table == "contacts/Stranger"
        ));

        let compact = LegacyTableAddress::Relation([9; 16]);
        let error = migration_row_input(&declared(), &[LegacyTable::new(&compact, &rows)])
            .expect_err("an undeclared compact table has no relation");
        assert!(matches!(error, MigrationRowError::UnmappableTable { .. }));
    }

    /// A declared relation that no legacy table answers fails closed.
    #[test]
    fn relation_without_table_fails_closed() {
        let error = migration_row_input(&declared(), &[])
            .expect_err("the declared relation has no legacy table");
        assert!(matches!(
            error,
            MigrationRowError::RelationWithoutTable { ref relation } if relation == PATH
        ));
    }

    /// A legacy field the declared relation does not store fails closed.
    #[test]
    fn unknown_field_fails_closed() {
        let rows = LegacyTableRows::loose_for_tests(vec![LegacyRow::for_tests(
            TypedKey::Text("carol".to_owned()),
            vec![declared_field("nickname", text("Caz"))],
        )]);
        let error = map_contacts(&declared(), &rows).expect_err("nickname is not a stored field");
        assert!(matches!(
            error,
            MigrationRowError::UnknownField { ref field, .. } if field == "nickname"
        ));
    }

    /// A compact and an editable source for one relation are a cross-source
    /// conflict, refused through the whole mapping.
    #[test]
    fn cross_source_conflict_fails_closed() {
        let rows = contacts();
        let loose = LegacyTableAddress::TablePath(PATH.to_owned());
        let compact = LegacyTableAddress::Relation(TABLE);
        let compact_rows = contacts();
        let error = migration_row_input(
            &declared(),
            &[
                LegacyTable::new(&loose, &rows),
                LegacyTable::new(&compact, &compact_rows),
            ],
        )
        .expect_err("two sources for one relation conflict");
        assert!(matches!(
            error,
            MigrationRowError::Source(LegacyReadError::CrossSourceConflict { .. })
        ));
    }

    /// A relation with an automatic key fails closed.
    #[test]
    fn automatic_key_relation_fails_closed() {
        let declared = StagedRelations::for_tests(
            vec![StagedRelation::new(TABLE, descriptor()).with_automatic_key()],
            vec![(TABLE, PATH)],
        );
        let error = map_contacts(&declared, &contacts())
            .expect_err("automatic keys are not admitted from legacy rows");
        assert!(matches!(
            error,
            MigrationRowError::AutomaticKeyUnsupported { ref relation } if relation == PATH
        ));
    }

    /// A stored Blob field admits only a ROV3 blob reference. Legacy raw bytes
    /// are not a reference and cannot be made one without the blob store, so
    /// they fail closed with a typed row error instead of being admitted.
    #[test]
    fn legacy_blob_bytes_fail_closed() {
        let rows = LegacyTableRows::loose_for_tests(vec![LegacyRow::for_tests(
            TypedKey::Text("alice".to_owned()),
            vec![
                declared_field("name", text("Alice Smith")),
                declared_field("emails", Raw::Array(Vec::new())),
                declared_field("avatar", Raw::Bytes(vec![1, 2, 3])),
            ],
        )]);
        let error = map_contacts(&blob_declared(), &rows)
            .expect_err("raw Blob bytes are not a stored blob reference");
        assert!(matches!(
            error,
            MigrationRowError::InvalidRow { ref reason, .. } if reason.contains("avatar")
        ));
    }

    /// The golden bundle the CLI legacy tests read.
    const GOLDEN_BUNDLE: &str = "../orna-cli/tests/fixtures/legacy-format1.bundle";

    /// Clones the golden bundle and returns its repository and HEAD commit.
    fn golden() -> (TempDir, Repository, GitCommitRef) {
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
        let commit = repository
            .head()
            .expect("the clone reads HEAD")
            .expect("the bundle has a commit");
        (directory, repository, commit)
    }

    /// The golden compact table `number`'s storage id, as the builder names it.
    fn golden_table(number: u64) -> [u8; 16] {
        *Uuid::from_u64_pair(0x018f_0000_0000_7100, number).as_bytes()
    }

    /// A declared set holding the golden compact tables `numbers`, with the
    /// schemas the builder publishes.
    fn golden_compact_declared(numbers: &[u64]) -> StagedRelations {
        let relations = golden_compact_schemas()
            .into_iter()
            .filter(|(number, _, _)| numbers.contains(number))
            .map(|(_, id, schema)| StagedRelation::new(id, schema))
            .collect();
        StagedRelations::for_tests(relations, Vec::new())
    }

    #[test]
    fn golden_compact_tables_keep_the_legacy_reader_keys() {
        let (_directory, repository, commit) = golden();
        for number in [1_u64, 2, 3] {
            let id = golden_table(number);
            let address = LegacyTableAddress::Relation(id);
            let legacy = read_legacy_table_rows(&repository, &commit, &address)
                .expect("the legacy compact table decodes");
            let declared = golden_compact_declared(&[number]);
            let input = migration_row_input(&declared, &[LegacyTable::new(&address, &legacy)])
                .expect("the golden compact table maps onto its declared schema");
            let relation = input
                .relations()
                .iter()
                .find(|relation| relation.relation_id() == &id)
                .expect("the declared table is in the input");
            let mapped: Vec<TypedKey> =
                relation.rows().iter().map(|(key, _)| key.clone()).collect();
            let expected: Vec<TypedKey> =
                legacy.rows().iter().map(|row| row.key().clone()).collect();
            assert_eq!(mapped, expected, "table {number} keeps the legacy keys");
        }
    }

    #[test]
    fn golden_loose_contacts_keep_the_legacy_reader_keys() {
        let (_directory, repository, commit) = golden();
        let address = LegacyTableAddress::TablePath(PATH.to_owned());
        let legacy = read_legacy_table_rows(&repository, &commit, &address)
            .expect("the legacy loose table decodes");
        let input = migration_row_input(&declared(), &[LegacyTable::new(&address, &legacy)])
            .expect("the golden contacts map onto the declared Contact table");
        let relation = input
            .relations()
            .iter()
            .find(|relation| relation.relation_id() == &TABLE)
            .expect("the declared Contact table is in the input");
        let mapped: Vec<TypedKey> = relation.rows().iter().map(|(key, _)| key.clone()).collect();
        let expected: Vec<TypedKey> = legacy.rows().iter().map(|row| row.key().clone()).collect();
        assert_eq!(legacy.rows().len(), 2, "the golden holds two contacts");
        assert_eq!(mapped, expected, "contacts keep the legacy keys");
    }

    #[test]
    fn golden_blob_table_fails_closed_as_invalid_row() {
        let (_directory, repository, commit) = golden();
        let address = LegacyTableAddress::Relation(golden_table(5));
        let legacy = read_legacy_table_rows(&repository, &commit, &address)
            .expect("the legacy blob table decodes");
        let error = migration_row_input(
            &golden_compact_declared(&[5]),
            &[LegacyTable::new(&address, &legacy)],
        )
        .expect_err("raw Blob bytes are not a stored blob reference");
        assert!(matches!(error, MigrationRowError::InvalidRow { .. }));
    }
}
