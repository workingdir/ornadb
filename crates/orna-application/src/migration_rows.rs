//! UPGRADE-3 row input: legacy rows mapped to the declared relations.
//!
//! The copy admits rows through `StagedRelation::with_rows`. This module builds
//! that input purely from a legacy snapshot and the committed schema: each
//! legacy table is mapped to the declared relation it answers, each relation
//! takes the one source the decoder chooses, and each row is lowered to the
//! canonical stored tuple the format-3 row writer admits. It writes nothing.
//! Every mismatch fails closed with a typed error.

use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigInt;
use orna_project::StoredSchema;
use orna_repository::relation_admission::{MigrationBlobDescriptors, StagedRelation};
use orna_repository::{ContentIdentity, NativeOid, TypedKey};
use orna_value::{ROV3_BLOB_TAG, Raw, encode_canonical_storage_value};
use sha2::{Digest, Sha256};

use crate::format3_store::StagedRelations;
use crate::legacy_store::{
    LegacyFieldName, LegacyReadError, LegacyTableAddress, LegacyTableRows, row_key_text,
    single_legacy_source,
};

/// The stored field tuple of every row of a relation, with the blob references
/// those tuples carry.
type StoredRows = (Vec<(TypedKey, Raw)>, Vec<ExpectedBlob>);

/// The staged relations of a migration, the stored tuples each must carry, and
/// the payload of every blob those tuples reference.
type StagedWithExpectation = (
    Vec<StagedRelation>,
    Vec<ExpectedRelation>,
    BTreeMap<ContentIdentity, Vec<u8>>,
);

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

/// One declared relation with the rows the copy admits. Blob values stay
/// pending until `encoded_rows` receives the blob descriptors.
#[derive(Debug)]
pub struct MigrationRelation {
    name: String,
    relation: StagedRelation,
    rows: Vec<MigrationRow>,
}

impl MigrationRelation {
    /// The declared relation's id.
    #[must_use]
    pub const fn relation_id(&self) -> &[u8; 16] {
        self.relation.relation_id()
    }

    /// The canonical stored tuples, in the key order the legacy reader
    /// returned. Each blob value references the descriptor that
    /// `write_migration_blobs` issued for it; a blob without one fails closed.
    pub fn encoded_rows(
        &self,
        descriptors: &MigrationBlobDescriptors,
    ) -> Result<Vec<(TypedKey, Vec<u8>)>, MigrationRowError> {
        let (stored, _) = self.stored_rows(descriptors)?;
        self.encode_stored(&stored)
    }

    /// The stored field tuple of every row, in the key order the legacy reader
    /// returned, with each blob reference completed by its descriptor. Also
    /// returns each blob reference, so the gate can read the candidate's bytes.
    fn stored_rows(
        &self,
        descriptors: &MigrationBlobDescriptors,
    ) -> Result<StoredRows, MigrationRowError> {
        let mut stored_rows = Vec::with_capacity(self.rows.len());
        let mut blobs = Vec::new();
        for row in &self.rows {
            let mut stored = Vec::with_capacity(row.fields.len());
            for field in &row.fields {
                stored.push(match field {
                    StoredField::Value(raw) => raw.clone(),
                    StoredField::Blob { identity, lowered } => {
                        let descriptor = descriptors.descriptor(identity).ok_or_else(|| {
                            MigrationRowError::BlobNotWritten {
                                relation: self.name.clone(),
                                key: row.key_text.clone(),
                            }
                        })?;
                        blobs.push(ExpectedBlob {
                            row: row.key.clone(),
                            identity: *identity,
                            descriptor: descriptor.clone(),
                        });
                        replace_subtree(
                            lowered,
                            &blob_tuple(identity, &[]),
                            &blob_tuple(identity, descriptor.as_bytes()),
                        )
                    }
                });
            }
            stored_rows.push((row.key.clone(), Raw::Array(stored)));
        }
        Ok((stored_rows, blobs))
    }

    /// Encodes each stored field tuple to the canonical bytes the row writer
    /// admits.
    fn encode_stored(
        &self,
        stored: &[(TypedKey, Raw)],
    ) -> Result<Vec<(TypedKey, Vec<u8>)>, MigrationRowError> {
        stored
            .iter()
            .map(
                |(key, tuple)| -> Result<(TypedKey, Vec<u8>), MigrationRowError> {
                    let bytes = encode_canonical_storage_value(tuple).map_err(|cause| {
                        MigrationRowError::Encode {
                            relation: self.name.clone(),
                            cause,
                        }
                    })?;
                    Ok((key.clone(), bytes))
                },
            )
            .collect()
    }
}

/// One legacy row: its key and its stored fields in descriptor order.
#[derive(Debug)]
struct MigrationRow {
    key: TypedKey,
    key_text: String,
    fields: Vec<StoredField>,
}

/// One stored field of a row. A blob's reference is completed by its
/// descriptor when the row is encoded.
#[derive(Debug)]
enum StoredField {
    Value(Raw),
    Blob {
        identity: ContentIdentity,
        lowered: Raw,
    },
}

/// The media type recorded in a migrated blob's stored reference.
const BLOB_MEDIA_TYPE: &str = "application/octet-stream";

/// The admitted-row input of one migration: every declared relation with its
/// legacy rows, and the blob bytes those rows reference. Write the blobs with
/// `Repository::write_migration_blobs`, then consume the input with
/// `into_staged_relations`.
#[derive(Debug)]
pub struct MigrationRowInput {
    relations: Vec<MigrationRelation>,
    blobs: BTreeMap<ContentIdentity, Vec<u8>>,
}

impl MigrationRowInput {
    /// The relations with their rows, in declared module and table order.
    #[must_use]
    pub fn relations(&self) -> &[MigrationRelation] {
        &self.relations
    }

    /// The blob bytes the rows reference, keyed by content identity.
    #[must_use]
    pub const fn blobs(&self) -> &BTreeMap<ContentIdentity, Vec<u8>> {
        &self.blobs
    }

    /// The number of Blob values the decoded legacy rows carry. Each one takes
    /// the UPGRADE-004 default annotation (octet-stream, no suffix), so this is
    /// the annotation-default count the migration reports. It counts values,
    /// not rows, and reads no payload byte.
    #[must_use]
    pub fn annotation_default_count(&self) -> usize {
        self.relations
            .iter()
            .flat_map(|relation| relation.rows.iter())
            .flat_map(|row| row.fields.iter())
            .filter(|field| matches!(field, StoredField::Blob { .. }))
            .count()
    }

    /// Consumes the input into staged relations carrying their encoded rows,
    /// and returns with them the stored tuples each relation must carry, and the
    /// payload of every blob those tuples reference. The migration gate checks
    /// the candidate against that expectation.
    pub(crate) fn into_staged_relations_with_expectation(
        self,
        descriptors: &MigrationBlobDescriptors,
    ) -> Result<StagedWithExpectation, MigrationRowError> {
        let Self { relations, blobs } = self;
        let mut staged = Vec::with_capacity(relations.len());
        let mut expected = Vec::with_capacity(relations.len());
        for migration in relations {
            let relation_id = *migration.relation_id();
            let (stored, blob_refs) = migration.stored_rows(descriptors)?;
            let encoded = migration.encode_stored(&stored)?;
            expected.push(ExpectedRelation {
                relation_id,
                rows: stored,
                blobs: blob_refs,
            });
            staged.push(migration.relation.with_rows(encoded));
        }
        Ok((staged, expected, blobs))
    }
}

/// One declared relation's expected content: its id, the stored field tuple of
/// every legacy row in key order (each blob reference completed by its
/// descriptor), and the blob references those rows hold. The migration gate
/// checks the candidate against it.
#[derive(Debug)]
pub(crate) struct ExpectedRelation {
    pub(crate) relation_id: [u8; 16],
    pub(crate) rows: Vec<(TypedKey, Raw)>,
    pub(crate) blobs: Vec<ExpectedBlob>,
}

/// One Blob value an expected row holds: the row it sits in, the content
/// identity its legacy payload hashes to, and the descriptor written for it.
#[derive(Debug)]
pub(crate) struct ExpectedBlob {
    pub(crate) row: TypedKey,
    pub(crate) identity: ContentIdentity,
    pub(crate) descriptor: NativeOid,
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
    /// A row references a blob whose descriptor was not written.
    BlobNotWritten { relation: String, key: String },
    /// The canonical stored tuple could not be encoded.
    Encode {
        relation: String,
        cause: orna_value::Error,
    },
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
            Self::BlobNotWritten { relation, key } => write!(
                formatter,
                "row {key} of relation {relation} references a blob that was not written"
            ),
            Self::Encode { relation, cause } => {
                write!(
                    formatter,
                    "relation {relation} row cannot be encoded: {cause}"
                )
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
    let mut blobs = BTreeMap::<ContentIdentity, Vec<u8>>::new();
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
            Some(rows) => lower_rows(relation, &name, rows, &mut blobs)?,
            None => Vec::new(),
        };
        relations.push(MigrationRelation {
            name,
            relation: relation.clone(),
            rows,
        });
    }
    Ok(MigrationRowInput { relations, blobs })
}

/// Lowers each legacy row to its relation's stored fields in descriptor order.
/// Blob values are recorded in `blobs` and held as a provisional reference
/// until the blob descriptors are written.
fn lower_rows(
    relation: &StagedRelation,
    name: &str,
    rows: &LegacyTableRows,
    blobs: &mut BTreeMap<ContentIdentity, Vec<u8>>,
) -> Result<Vec<MigrationRow>, MigrationRowError> {
    let descriptor = relation.descriptor();
    let schema =
        StoredSchema::new(descriptor).map_err(|_| MigrationRowError::InvalidDescriptor {
            relation: name.to_owned(),
        })?;
    let mut lowered = Vec::with_capacity(rows.rows().len());
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
        let key_text = row_key_text(row.key());
        let fields = lower_row_fields(&schema, name, &key_text, &values, blobs)?;
        lowered.push(MigrationRow {
            key: row.key().clone(),
            key_text,
            fields,
        });
    }
    Ok(lowered)
}

/// Lowers one row's stored fields. A `Blob` value is recorded in `blobs` and
/// lowered against a provisional blob reference; every other value is lowered
/// to its final stored form here.
fn lower_row_fields(
    schema: &StoredSchema<'_>,
    relation: &str,
    key: &str,
    values: &BTreeMap<&str, &Raw>,
    blobs: &mut BTreeMap<ContentIdentity, Vec<u8>>,
) -> Result<Vec<StoredField>, MigrationRowError> {
    let invalid = |reason: String| MigrationRowError::InvalidRow {
        relation: relation.to_owned(),
        key: key.to_owned(),
        reason,
    };
    let mut fields = Vec::new();
    for field in schema.fields().iter().filter(|field| field.role() == 1) {
        let Some(value) = values.get(field.name()) else {
            return Err(invalid(format!(
                "stored field `{}` has no value",
                field.name()
            )));
        };
        let stored = match value {
            Raw::Bytes(bytes) if field.is_blob() => {
                let identity = blob_identity(bytes).map_err(&invalid)?;
                blobs.entry(identity).or_insert_with(|| bytes.clone());
                let provisional = blob_tuple(&identity, &[]);
                let lowered = schema
                    .lower_field(field, &provisional)
                    .map_err(|error| invalid(error.to_string()))?;
                StoredField::Blob { identity, lowered }
            }
            _ => StoredField::Value(
                schema
                    .lower_field(field, value)
                    .map_err(|error| invalid(error.to_string()))?,
            ),
        };
        fields.push(stored);
    }
    Ok(fields)
}

/// The content identity of blob bytes, computed the way the repository
/// computes it for `write_migration_blobs`.
fn blob_identity(bytes: &[u8]) -> Result<ContentIdentity, String> {
    let length = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
    let sha256: [u8; 32] = Sha256::digest(bytes).into();
    ContentIdentity::new(length, sha256).map_err(|error| error.to_string())
}

/// The stored blob reference: the ROV3 blob tuple carrying the content
/// identity and the descriptor's object id (empty while provisional).
fn blob_tuple(identity: &ContentIdentity, descriptor: &[u8]) -> Raw {
    Raw::Tag(
        ROV3_BLOB_TAG,
        Box::new(Raw::Array(vec![
            Raw::Int(BigInt::from(identity.length())),
            Raw::Bytes(identity.sha256().to_vec()),
            Raw::Text(BLOB_MEDIA_TYPE.to_owned()),
            Raw::Null,
            Raw::Bytes(descriptor.to_vec()),
        ])),
    )
}

/// Replaces every occurrence of `from` in `raw` with `to`.
fn replace_subtree(raw: &Raw, from: &Raw, to: &Raw) -> Raw {
    if raw == from {
        return to.clone();
    }
    match raw {
        Raw::Array(items) => Raw::Array(
            items
                .iter()
                .map(|item| replace_subtree(item, from, to))
                .collect(),
        ),
        Raw::Map(entries) => Raw::Map(
            entries
                .iter()
                .map(|(key, value)| {
                    (
                        replace_subtree(key, from, to),
                        replace_subtree(value, from, to),
                    )
                })
                .collect(),
        ),
        Raw::Tag(tag, inner) => Raw::Tag(*tag, Box::new(replace_subtree(inner, from, to))),
        other => other.clone(),
    }
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
        ROV3_BLOB_TAG, Raw, SchemaDescriptor, decode_canonical_storage_value,
        encode_canonical_storage_value,
    };

    use super::{
        LegacyTable, MigrationBlobDescriptors, MigrationRowError, MigrationRowInput, blob_identity,
        migration_row_input,
    };
    use crate::format3_store::StagedRelations;
    use crate::legacy_store::{
        LegacyField, LegacyFieldName, LegacyReadError, LegacyRow, LegacyTableAddress,
        LegacyTableRows,
    };
    use std::{path::Path, process::Command};

    use orna_project::StoredSchema;
    use orna_repository::relation_admission::{
        AllocatorWatermarks, RelationCandidateContent, StoredObjectIdentity,
    };
    use orna_repository::{
        GitCommitRef, KeyRange, ManagedFileChange, ManagedPath, PrivateCommit, Repository, Uuid,
        golden_compact_schemas,
    };
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
        let descriptors = MigrationBlobDescriptors::default();
        let admitted = input.relations()[0]
            .encoded_rows(&descriptors)
            .expect("the contacts encode");
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
        let descriptors = MigrationBlobDescriptors::default();
        assert_eq!(
            first.relations()[0]
                .encoded_rows(&descriptors)
                .expect("the first run encodes"),
            second.relations()[0]
                .encoded_rows(&descriptors)
                .expect("the second run encodes"),
        );
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

    /// A stored Blob field lowers legacy raw bytes to a ROV3 blob reference:
    /// the bytes enter the migration blob table, and the encoded row carries
    /// the reference naming the descriptor the repository wrote for them.
    #[test]
    fn legacy_blob_bytes_lower_to_blob_references() {
        let rows = LegacyTableRows::loose_for_tests(vec![LegacyRow::for_tests(
            TypedKey::Text("alice".to_owned()),
            vec![
                declared_field("name", text("Alice Smith")),
                declared_field("emails", Raw::Array(Vec::new())),
                declared_field("avatar", Raw::Bytes(vec![1, 2, 3])),
            ],
        )]);
        let input = map_contacts(&blob_declared(), &rows).expect("the Blob bytes map");
        let identity = blob_identity(&[1, 2, 3]).expect("the blob identity");
        assert_eq!(
            input.blobs().get(&identity).map(Vec::as_slice),
            Some(&[1, 2, 3][..])
        );

        let missing = MigrationBlobDescriptors::default();
        let error = input.relations()[0]
            .encoded_rows(&missing)
            .expect_err("a blob without a written descriptor fails closed");
        assert!(matches!(error, MigrationRowError::BlobNotWritten { .. }));

        let (_directory, repository, _commit) = golden();
        let descriptors = write_blobs(&repository, &input);
        let encoded = input.relations()[0]
            .encoded_rows(&descriptors)
            .expect("the blob row encodes");
        let (_, tuple) = &encoded[0];
        let Raw::Array(fields) = decode_canonical_storage_value(tuple).expect("the tuple decodes")
        else {
            panic!("the stored row is a tuple");
        };
        let Raw::Tag(ROV3_BLOB_TAG, payload) = &fields[2] else {
            panic!("the Blob field is an ROV3 reference");
        };
        let Raw::Array(reference) = payload.as_ref() else {
            panic!("the ROV3 reference is a tuple");
        };
        let oid = descriptors
            .descriptor(&identity)
            .expect("the descriptor was written");
        assert_eq!(reference[4], Raw::Bytes(oid.as_bytes().to_vec()));
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

    /// The stored tuple a legacy row lowers to: its role-1 field values in
    /// descriptor order, read from the legacy row by stored field.
    fn stored_values(relation: &StagedRelation, row: &LegacyRow) -> Raw {
        let schema = StoredSchema::new(relation.descriptor()).expect("the descriptor reads");
        Raw::Array(
            schema
                .fields()
                .iter()
                .filter(|stored| stored.role() == 1)
                .map(|stored| {
                    row.fields()
                        .iter()
                        .find(|field| match field.name() {
                            LegacyFieldName::Declared(name) => name == stored.name(),
                            LegacyFieldName::ObjectId(id) => id == stored.id(),
                        })
                        .unwrap_or_else(|| panic!("the row has field {}", stored.name()))
                        .value()
                        .clone()
                })
                .collect(),
        )
    }

    /// Each admitted tuple decodes to the legacy row's stored values, is their
    /// canonical encoding, and carries the legacy reader's key.
    fn assert_rows_match_legacy(
        relation: &StagedRelation,
        admitted: &[(TypedKey, Vec<u8>)],
        legacy: &LegacyTableRows,
    ) {
        assert_eq!(admitted.len(), legacy.rows().len());
        for ((key, tuple), row) in admitted.iter().zip(legacy.rows()) {
            assert_eq!(key, row.key(), "the admitted key is the legacy key");
            let values = stored_values(relation, row);
            assert_eq!(
                decode_canonical_storage_value(tuple).expect("the tuple decodes"),
                values
            );
            assert_eq!(
                tuple,
                &encode_canonical_storage_value(&values).expect("the values encode")
            );
        }
    }

    #[test]
    fn golden_compact_tables_keep_the_legacy_keys_and_values() {
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
            let descriptors = write_blobs(&repository, &input);
            let rows = relation
                .encoded_rows(&descriptors)
                .expect("the golden compact rows encode");
            assert_rows_match_legacy(&declared.relations()[0], &rows, &legacy);
        }
    }

    #[test]
    fn golden_loose_contacts_keep_the_legacy_keys_and_values() {
        let (_directory, repository, commit) = golden();
        let address = LegacyTableAddress::TablePath(PATH.to_owned());
        let legacy = read_legacy_table_rows(&repository, &commit, &address)
            .expect("the legacy loose table decodes");
        assert_eq!(legacy.rows().len(), 2, "the golden holds two contacts");
        let declared = declared();
        let input = migration_row_input(&declared, &[LegacyTable::new(&address, &legacy)])
            .expect("the golden contacts map onto the declared Contact table");
        let relation = input
            .relations()
            .iter()
            .find(|relation| relation.relation_id() == &TABLE)
            .expect("the declared Contact table is in the input");
        let descriptors = write_blobs(&repository, &input);
        let rows = relation
            .encoded_rows(&descriptors)
            .expect("the golden contacts encode");
        assert_rows_match_legacy(&declared.relations()[0], &rows, &legacy);
    }

    /// Sets the commit identity the candidate's commit objects need in the
    /// cloned bundle.
    fn configure_identity(root: &Path) {
        for (key, value) in [
            ("user.name", "Migration Test"),
            ("user.email", "migration@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            let status = Command::new("git")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(root)
                .args(["config", key, value])
                .status()
                .expect("the git config process starts");
            assert!(status.success(), "git config {key}");
        }
    }
    /// Deletes every legacy `.orna` file other than the format-3 database
    /// record: the 1.0.0 storage files the migrated candidate does not carry.
    fn legacy_orna_removals(root: &Path) -> Vec<ManagedFileChange> {
        fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("the golden .orna directory reads") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    collect(root, &path, out);
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("the entry is under the root")
                    .to_str()
                    .expect("the golden paths are UTF-8")
                    .to_owned();
                if relative != ".orna/database.orna" && relative != ".orna/format.orna" {
                    out.push(relative);
                }
            }
        }
        let mut paths = Vec::new();
        collect(root, &root.join(".orna"), &mut paths);
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                ManagedFileChange::new(ManagedPath::new(&path).expect("managed path"), None)
            })
            .collect()
    }

    /// Writes the input's legacy blob values as Git blobs and returns the
    /// descriptors the rows reference.
    fn write_blobs(repository: &Repository, input: &MigrationRowInput) -> MigrationBlobDescriptors {
        repository
            .write_migration_blobs(input.blobs())
            .expect("the migration blobs write")
    }

    /// Admits `relations` as the candidate of `commit`, with the format-3
    /// record the legacy golden does not carry, and returns the private commit.
    fn admit(
        repository: &Repository,
        root: &Path,
        commit: &GitCommitRef,
        relations: &[StagedRelation],
        identities: &[StoredObjectIdentity],
    ) -> PrivateCommit {
        let watermarks = AllocatorWatermarks::new();
        // The legacy golden records no database identity in format 3, so the
        // candidate names a fixed test identity and writes the format-3 record
        // that carries it.
        let database_id = repository
            .open_pinned_format_context(commit.as_str())
            .expect("the golden commit opens")
            .require_database_id()
            .unwrap_or_else(|_| {
                "0f4c1f2e-8a3b-4c5d-9e6f-7a8b9c0d1e2f"
                    .parse()
                    .expect("the test identity parses")
            });
        let database =
            format!("{{\n    repository_format: 3,\n    database_id: \"{database_id}\",\n}}\n")
                .into_bytes();
        let mut changes = vec![
            ManagedFileChange::new(
                ManagedPath::new(".orna/database.orna").expect("managed path"),
                Some(database),
            ),
            ManagedFileChange::new(
                ManagedPath::new(".orna/format.orna").expect("managed path"),
                None,
            ),
        ];
        changes.extend(legacy_orna_removals(root));
        repository
            .build_private_commit_with_relations(
                commit,
                &changes,
                RelationCandidateContent {
                    relations,
                    identities,
                    watermarks: &watermarks,
                },
                database_id,
                "migration candidate",
            )
            .expect("the candidate admits the migrated rows")
    }

    /// A loose Contact admitted through the repository candidate path reads
    /// back through the ordinary committed reader with the legacy key and the
    /// stored tuple the copy derived.
    #[test]
    fn loose_contacts_round_trip_through_repository_admission() {
        let (directory, repository, commit) = golden();
        configure_identity(directory.path());
        let address = LegacyTableAddress::TablePath(PATH.to_owned());
        let legacy = read_legacy_table_rows(&repository, &commit, &address)
            .expect("the legacy loose table decodes");
        let declared_set = declared();
        let input = migration_row_input(&declared_set, &[LegacyTable::new(&address, &legacy)])
            .expect("the golden contacts map onto the declared Contact table");
        let descriptors = write_blobs(&repository, &input);
        let expected: Vec<(TypedKey, Vec<u8>)> = input.relations()[0]
            .encoded_rows(&descriptors)
            .expect("the golden contacts encode");
        let (relations, _, _) = input
            .into_staged_relations_with_expectation(&descriptors)
            .expect("the golden contacts stage");

        let identities = [StoredObjectIdentity::new(TABLE, 1, "Contact", false)];
        let candidate = admit(
            &repository,
            directory.path(),
            &commit,
            &relations,
            &identities,
        );

        let format = repository
            .open_private_candidate_format_context(&candidate)
            .expect("the candidate opens");
        let row_map = format
            .load_row_map(TABLE)
            .expect("the Contact row map loads");
        let graph = format
            .open_native_graph(&row_map)
            .expect("the native graph opens");
        let scope = graph.open_read_scope().expect("a read scope opens");
        let rows = graph
            .range_rows(&KeyRange::new(None, None, 16).expect("key range"), &scope)
            .expect("the committed rows read");
        assert_eq!(rows.len(), expected.len());
        for (row, (key, tuple)) in rows.iter().zip(&expected) {
            assert_eq!(row.key(), key);
            assert_eq!(row.value().encoded_fields(), Some(tuple.as_slice()));
        }
    }

    /// The golden blob table admits through the repository candidate path:
    /// each row reads back under its legacy key, and each legacy blob's bytes
    /// read back through the committed blob descriptor.
    #[test]
    fn golden_blob_table_round_trips_through_repository_admission() {
        let (directory, repository, commit) = golden();
        configure_identity(directory.path());
        let id = golden_table(5);
        let address = LegacyTableAddress::Relation(id);
        let legacy = read_legacy_table_rows(&repository, &commit, &address)
            .expect("the legacy blob table decodes");
        let declared = golden_compact_declared(&[5]);
        let input = migration_row_input(&declared, &[LegacyTable::new(&address, &legacy)])
            .expect("the golden blob table maps onto its declared schema");
        let descriptors = write_blobs(&repository, &input);
        let encoded_keys: Vec<TypedKey> = input.relations()[0]
            .encoded_rows(&descriptors)
            .expect("the golden blob rows encode")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        let legacy_keys: Vec<TypedKey> =
            legacy.rows().iter().map(|row| row.key().clone()).collect();
        assert_eq!(
            encoded_keys, legacy_keys,
            "the admitted keys are the legacy keys"
        );
        let (relations, _, _) = input
            .into_staged_relations_with_expectation(&descriptors)
            .expect("the golden blob rows stage");
        let identities = [StoredObjectIdentity::new(id, 1, "BlobTable", false)];
        let candidate = admit(
            &repository,
            directory.path(),
            &commit,
            &relations,
            &identities,
        );

        let format = repository
            .open_private_candidate_format_context(&candidate)
            .expect("the candidate opens");
        let row_map = format
            .load_row_map(id)
            .expect("the blob table row map loads");
        let graph = format
            .open_native_graph(&row_map)
            .expect("the native graph opens");
        let scope = graph.open_read_scope().expect("a read scope opens");
        let mut blobs_read = 0_usize;
        for row in legacy.rows() {
            let admitted = graph
                .lookup_row(row.key(), &scope)
                .expect("the admitted row reads")
                .expect("the legacy row is admitted");
            for field in row.fields() {
                let Raw::Bytes(bytes) = field.value() else {
                    continue;
                };
                let identity = blob_identity(bytes).expect("the blob identity");
                let descriptor = descriptors
                    .descriptor(&identity)
                    .expect("the blob descriptor was written");
                let stored = graph
                    .read_stored_blob_value(&admitted, descriptor, &scope)
                    .expect("the stored blob reads");
                let read = graph
                    .read_blob_range(stored.reference(), 0..identity.length(), &scope)
                    .expect("the blob bytes read");
                assert_eq!(read.bytes(), bytes.as_slice());
                blobs_read += 1;
            }
        }
        assert!(blobs_read > 0, "the golden blob table holds blob content");
    }

    /// Two independent repositories, each written twice, yield the same blob
    /// content identities, descriptors and encoded rows: a descriptor is
    /// content-addressed, so it depends on the blob bytes alone.
    #[test]
    fn migration_blob_descriptors_are_identical_across_runs() {
        let run = || {
            let (_directory, repository, commit) = golden();
            let address = LegacyTableAddress::Relation(golden_table(5));
            let legacy = read_legacy_table_rows(&repository, &commit, &address)
                .expect("the legacy blob table decodes");
            let declared = golden_compact_declared(&[5]);
            let input = migration_row_input(&declared, &[LegacyTable::new(&address, &legacy)])
                .expect("the golden blob table maps onto its declared schema");
            let descriptors = write_blobs(&repository, &input);
            let rewritten = write_blobs(&repository, &input);
            assert_eq!(
                descriptors, rewritten,
                "rewriting the same blobs yields the same descriptors"
            );
            let identities: Vec<_> = input.blobs().keys().copied().collect();
            let encoded = input.relations()[0]
                .encoded_rows(&descriptors)
                .expect("the golden blob rows encode");
            (identities, descriptors, encoded)
        };
        let first = run();
        let second = run();
        assert!(
            !first.0.is_empty(),
            "the golden blob table holds blob content"
        );
        assert_eq!(
            first, second,
            "two runs yield identical identities, descriptors and rows"
        );
    }

    /// An empty legacy Blob admits through repository admission: the private
    /// candidate reads back zero bytes through the descriptor written for the
    /// empty content, which names the null byte root.
    #[test]
    fn empty_blob_round_trips_through_repository_admission() {
        let (directory, repository, commit) = golden();
        configure_identity(directory.path());
        let rows = LegacyTableRows::loose_for_tests(vec![LegacyRow::for_tests(
            TypedKey::Text("alice".to_owned()),
            vec![
                declared_field("name", text("Alice Smith")),
                declared_field("emails", Raw::Array(Vec::new())),
                declared_field("avatar", Raw::Bytes(Vec::new())),
            ],
        )]);
        let address = LegacyTableAddress::TablePath(PATH.to_owned());
        let input = migration_row_input(&blob_declared(), &[LegacyTable::new(&address, &rows)])
            .expect("the empty Blob maps");
        let empty = blob_identity(&[]).expect("the empty blob identity");
        assert_eq!(input.blobs().get(&empty).map(Vec::as_slice), Some(&[][..]));
        let descriptors = write_blobs(&repository, &input);
        let (relations, _, _) = input
            .into_staged_relations_with_expectation(&descriptors)
            .expect("the empty blob row stages");
        let identities = [StoredObjectIdentity::new(TABLE, 1, "Contact", false)];
        let candidate = admit(
            &repository,
            directory.path(),
            &commit,
            &relations,
            &identities,
        );

        let format = repository
            .open_private_candidate_format_context(&candidate)
            .expect("the candidate opens");
        let row_map = format
            .load_row_map(TABLE)
            .expect("the Contact row map loads");
        let graph = format
            .open_native_graph(&row_map)
            .expect("the native graph opens");
        let scope = graph.open_read_scope().expect("a read scope opens");
        let admitted = graph
            .lookup_row(&TypedKey::Text("alice".to_owned()), &scope)
            .expect("the admitted row reads")
            .expect("the legacy row is admitted");
        let empty_descriptor = descriptors
            .descriptor(&empty)
            .expect("the empty descriptor was written");
        let stored = graph
            .read_stored_blob_value(&admitted, empty_descriptor, &scope)
            .expect("the stored empty blob reads");
        let read = graph
            .read_blob_range(stored.reference(), 0..0, &scope)
            .expect("the empty blob bytes read");
        assert!(read.bytes().is_empty());
    }

    /// A Blob nested inside a List fails closed with a typed row error: the
    /// nested bytes are not lowered to a blob reference, so nothing is written
    /// for them.
    #[test]
    fn nested_blob_fails_closed() {
        const PHOTOS: [u8; 16] = [0x76; 16];
        let blob_node = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".to_owned())]);
        let photos = Raw::Array(vec![
            tagged(PHOTOS),
            Raw::Text("photos".to_owned()),
            Raw::Array(vec![Raw::Int(1.into()), blob_node]),
            Raw::Int(1.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ]);
        let nested = StagedRelations::for_tests(
            vec![StagedRelation::new(TABLE, descriptor_with(vec![photos]))],
            vec![(TABLE, PATH)],
        );
        let rows = LegacyTableRows::loose_for_tests(vec![LegacyRow::for_tests(
            TypedKey::Text("alice".to_owned()),
            vec![
                declared_field("name", text("Alice Smith")),
                declared_field("emails", Raw::Array(Vec::new())),
                declared_field("photos", Raw::Array(vec![Raw::Bytes(vec![1, 2, 3])])),
            ],
        )]);
        let Err(error) = map_contacts(&nested, &rows) else {
            panic!("a Blob nested in a List is refused");
        };
        assert!(matches!(
            error,
            MigrationRowError::InvalidRow { ref relation, .. } if relation == PATH
        ));
    }
}
