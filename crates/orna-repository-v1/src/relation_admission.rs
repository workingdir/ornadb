//! Format-3 store bootstrap, relation admission and the committed object
//! identity inventory (store-format-3 §2 and §6; ORNA-ROW-001,
//! ORNA-META-001, ORNA-OBJECT-001/002, ORNA-STAGE-001/002).
//!
//! `orna init` writes no store. Initialization neither stages nor commits
//! (ORNA-META-003), and `.orna/store` is a native Git tree entry that only an
//! index or a commit can hold. The first staged candidate that needs the
//! store creates it here: a store root over an ORP-1 relation map with domain
//! `["relations", database_uuid]`. Each admitted source table gets its
//! relation in the same staging transaction: a kind-6 schema node over its
//! closed descriptor, one empty leaf as its primary map ("Empty table: one
//! empty leaf"), and a canonical insert into the relation map under its
//! stable relation UUID.
//!
//! Stable object IDs are committed metadata (ORNA-OBJECT-002), and format-3
//! tracked `.orna/` may hold only `database.orna` and `store`
//! (ORNA-META-001). The identity inventory is therefore a sealed system
//! relation in the same store ("System historical relations use identical
//! maps but their schema/admission authority remains sealed", §2), not a
//! sidecar file. A commit that still tracks the legacy sidecar predates this
//! relation; staging migrates it and removes the sidecar from the index.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    process::Stdio,
};

use orna_value_v1::{Raw, SchemaDescriptor, Value, decode_canonical_storage_value};
use sha2::{Digest, Sha256};

use crate::{
    DatabaseId, FormatContextError, GitCommitRef, GitIndexLock, IndexGeneration, IndexTreeRef,
    HistoricalFormatContext,
    PrivateCommit, Repository, RepositoryError, RepositoryFormat, RepositoryFormatContext, Uuid,
    bind_tree_entry_in_index,
    blob_store::GearChunker,
    native_graph::{
        ByteIndexEntry, GitHashAlgorithm, GraphError, MAX_GRAPH_HEIGHT, MAX_REFS,
        NativeGraphContext, NativeNodeEnvelope, NativeObjectKind, NativeOid, NodeData,
        StoreRelationEntry, hex_encode, sync_object_files,
    },
    row_store::{KeyRange, MAX_ROW_RANGE_LIMIT, RowDependency, RowEntry, RowValue, TypedKey},
};

/// Stable relation UUID of the sealed object-identity inventory: the
/// committed metadata that holds stable object IDs (ORNA-OBJECT-002). It is
/// implementation storage, not the `sys.Object` catalogue view.
///
/// It is the UUIDv8 derived from SHA-256 of `orna.system-relation.v1`, NUL and
/// `orna.object-identity`, so it can never equal an allocated UUIDv4 table
/// identity.
pub const OBJECT_IDENTITY_RELATION_ID: [u8; 16] = [
    0x2a, 0x3c, 0xc8, 0x5f, 0x98, 0x9e, 0x8a, 0x79, 0xa2, 0x42, 0xee, 0xf6, 0x9e, 0x41, 0x17, 0x4b,
];
/// Field UUIDs of the sealed descriptor, derived like the relation UUID from
/// `orna.object-identity.<field>`.
const OBJECT_ID_FIELD: [u8; 16] = [
    0x29, 0xa2, 0x2c, 0x32, 0x7d, 0x5e, 0x8d, 0x40, 0x9f, 0x4a, 0xd4, 0xe9, 0xd7, 0x65, 0xa0, 0x4d,
];
const OBJECT_KIND_FIELD: [u8; 16] = [
    0xab, 0x30, 0x41, 0x93, 0x50, 0xd6, 0x87, 0x7a, 0x98, 0x73, 0x3a, 0x16, 0x28, 0x56, 0x68, 0xdb,
];
const OBJECT_QUALIFIED_NAME_FIELD: [u8; 16] = [
    0x7d, 0x16, 0x18, 0xf5, 0x59, 0xad, 0x8e, 0xb2, 0xad, 0x6b, 0x68, 0xf4, 0x64, 0xaa, 0x46, 0xef,
];
const OBJECT_RETIRED_FIELD: [u8; 16] = [
    0xc1, 0xba, 0x14, 0xb3, 0xf7, 0x90, 0x8d, 0xcc, 0x95, 0x9a, 0x61, 0xe2, 0xe5, 0xe6, 0x7d, 0xbb,
];

/// Location of the identity inventory before it moved into the store. Only
/// legacy commits track it; staging removes it from the candidate index.
pub const LEGACY_OBJECT_IDENTITY_SIDECAR_PATH: &str = ".orna/object-identities.cbor";

const STORE_PATH: &str = ".orna/store";
const STAGED_CANDIDATE_MESSAGE: &str = "orna: staged candidate";

/// Stable relation UUID of the sealed explicit-rekey log (ORNA-EVOL-007,
/// ORNA-MUT-010): one row per logical rekey a publication committed. It is
/// the UUIDv8 derived from SHA-256 of `orna.system-relation.v1`, NUL and
/// `orna.object-rekey`.
pub const OBJECT_REKEY_RELATION_ID: [u8; 16] = [
    0x17, 0xb5, 0xc3, 0x0d, 0xc4, 0x72, 0x8b, 0xba, 0xb6, 0xf2, 0x09, 0xdb, 0x6f, 0xad, 0x56, 0x1f,
];
/// Field UUIDs of the rekey descriptor, derived from `orna.object-rekey.<field>`.
const REKEY_ORDINAL_FIELD: [u8; 16] = [
    0x59, 0x70, 0xed, 0xf6, 0xb0, 0xb2, 0x8e, 0x6e, 0xb5, 0x0d, 0xf4, 0x29, 0x1e, 0xc7, 0xe0, 0x82,
];
const REKEY_OLD_KEY_FIELD: [u8; 16] = [
    0x73, 0xdf, 0xcb, 0x47, 0x45, 0x69, 0x88, 0xe1, 0x98, 0x83, 0xbf, 0xa9, 0x4d, 0xbd, 0xef, 0xa0,
];
const REKEY_RELATION_FIELD: [u8; 16] = [
    0x88, 0x1d, 0x15, 0x48, 0xbc, 0x48, 0x8e, 0xde, 0x85, 0xc6, 0x8b, 0xd3, 0xf5, 0x6d, 0x94, 0x54,
];
const REKEY_NEW_KEY_FIELD: [u8; 16] = [
    0xf3, 0x83, 0x86, 0x4d, 0x46, 0xd6, 0x85, 0x1e, 0x95, 0x9f, 0xb5, 0x59, 0x7b, 0xbd, 0xeb, 0xd5,
];

/// Failures of store bootstrap, relation admission and identity reads.
#[derive(Debug)]
pub enum RelationAdmissionError {
    /// Format 3 is required to stage a native store.
    NotFormat3,
    /// The index has no tree to stage, so there is nothing to commit.
    EmptyIndex,
    /// The ordinary index or `HEAD` changed after the candidate was captured.
    StaleIndex,
    /// The staged `.orna/store` entry is not a native tree.
    InvalidStoreEntry,
    /// The staged source changes the descriptor of a relation `HEAD`
    /// commits. Schema evolution needs an explicit admission this writer does
    /// not perform, so the staged store would not decode under the staged
    /// schema (ORNA-STAGE-002).
    SchemaChanged {
        relation_id: [u8; 16],
    },
    /// The staged store keeps a relation `HEAD` commits that no admitted
    /// staged table owns, so its rows would not belong to a reachable table
    /// (ORNA-REPO-004).
    OrphanRelation {
        relation_id: [u8; 16],
    },
    /// The staged store lacks a relation `HEAD` commits for a table the
    /// staged source still declares, so committing it would drop the
    /// relation's rows.
    CommittedRelationUnstaged {
        relation_id: [u8; 16],
    },
    /// A relation no commit has received holds staged rows, so it cannot be
    /// rebuilt or dropped with the staged source without losing them.
    UncommittedRelationRows {
        relation_id: [u8; 16],
    },
    /// One relation was supplied twice, or a table used the system relation's
    /// identity.
    DuplicateRelation {
        relation_id: [u8; 16],
    },
    /// An identity record is not a valid inventory entry.
    InvalidIdentity,
    /// A rekey log record is not a valid system relation entry.
    InvalidRekeyLog,
    Format(FormatContextError),
    Repository(RepositoryError),
    Graph(GraphError),
}

impl std::fmt::Display for RelationAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFormat3 => {
                formatter.write_str("the staged candidate is not repository format 3")
            }
            Self::EmptyIndex => formatter.write_str("the Git index has no staged tree"),
            Self::StaleIndex => {
                formatter.write_str("the Git index or HEAD changed while the candidate was staged")
            }
            Self::InvalidStoreEntry => {
                formatter.write_str("the staged .orna/store is not a native tree")
            }
            Self::SchemaChanged { relation_id } => write!(
                formatter,
                "the staged source changes the committed schema of relation {}; schema evolution is not admitted",
                Uuid::from_bytes(*relation_id)
            ),
            Self::OrphanRelation { relation_id } => write!(
                formatter,
                "relation {} has no admitted table in the staged source; removing a table is not admitted",
                Uuid::from_bytes(*relation_id)
            ),
            Self::CommittedRelationUnstaged { relation_id } => write!(
                formatter,
                "the staged store lacks committed relation {}; restore it with `git restore --staged .orna/store`",
                Uuid::from_bytes(*relation_id)
            ),
            Self::UncommittedRelationRows { relation_id } => write!(
                formatter,
                "relation {} has staged rows but was never committed; commit its table unchanged first or unstage the rows",
                Uuid::from_bytes(*relation_id)
            ),
            Self::DuplicateRelation { relation_id } => write!(
                formatter,
                "relation {} is admitted more than once",
                Uuid::from_bytes(*relation_id)
            ),
            Self::InvalidIdentity => formatter.write_str("an object identity record is invalid"),
            Self::InvalidRekeyLog => formatter.write_str("an object rekey log record is invalid"),
            Self::Format(error) => write!(formatter, "format context: {error:?}"),
            Self::Repository(error) => write!(formatter, "{error}"),
            Self::Graph(error) => write!(formatter, "native graph operation failed: {error}"),
        }
    }
}

impl std::error::Error for RelationAdmissionError {}

impl From<RepositoryError> for RelationAdmissionError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

impl From<GraphError> for RelationAdmissionError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

impl From<FormatContextError> for RelationAdmissionError {
    fn from(error: FormatContextError) -> Self {
        Self::Format(error)
    }
}

/// One committed stable object identity (ORNA-OBJECT-001): its UUID, its
/// object kind (its `sys.ObjectKind` ordinal), its qualified source name and
/// whether it is retired.
/// Retired identities stay in the inventory so they are never reused
/// (ORNA-OBJECT-005).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredObjectIdentity {
    id: [u8; 16],
    kind: u64,
    qualified_name: String,
    retired: bool,
}

impl StoredObjectIdentity {
    pub fn new(id: [u8; 16], kind: u64, qualified_name: impl Into<String>, retired: bool) -> Self {
        Self {
            id,
            kind,
            qualified_name: qualified_name.into(),
            retired,
        }
    }

    pub const fn id(&self) -> &[u8; 16] {
        &self.id
    }

    pub const fn kind(&self) -> u64 {
        self.kind
    }

    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    pub const fn retired(&self) -> bool {
        self.retired
    }

    /// The row key: the identity as a canonical tag-37 Uuid value.
    fn row_key(&self) -> Result<TypedKey, RelationAdmissionError> {
        Value::new(uuid_value(self.id))
            .map_err(|_| RelationAdmissionError::InvalidIdentity)
            .and_then(|value| {
                TypedKey::from_value(&value).map_err(|_| RelationAdmissionError::InvalidIdentity)
            })
    }

    /// The stored fields in descriptor (field-UUID) order: `qualified_name`,
    /// `kind`, `retired`.
    fn row_fields(&self) -> Result<Vec<u8>, RelationAdmissionError> {
        Value::new(Raw::Array(vec![
            Raw::Text(self.qualified_name.clone()),
            Raw::Int(self.kind.into()),
            Raw::Bool(self.retired),
        ]))
        .and_then(|value| value.encode())
        .map_err(|_| RelationAdmissionError::InvalidIdentity)
    }

    fn from_row(key: &TypedKey, fields: &[u8]) -> Result<Self, RelationAdmissionError> {
        let TypedKey::Value(key) = key else {
            return Err(RelationAdmissionError::InvalidIdentity);
        };
        let Raw::Tag(37, payload) = key.value().raw() else {
            return Err(RelationAdmissionError::InvalidIdentity);
        };
        let Raw::Bytes(bytes) = payload.as_ref() else {
            return Err(RelationAdmissionError::InvalidIdentity);
        };
        let id = <[u8; 16]>::try_from(bytes.as_slice())
            .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
        let Raw::Array(values) = decode_canonical_storage_value(fields)
            .map_err(|_| RelationAdmissionError::InvalidIdentity)?
        else {
            return Err(RelationAdmissionError::InvalidIdentity);
        };
        let [
            Raw::Text(qualified_name),
            Raw::Int(kind),
            Raw::Bool(retired),
        ] = values.as_slice()
        else {
            return Err(RelationAdmissionError::InvalidIdentity);
        };
        let kind = u64::try_from(kind).map_err(|_| RelationAdmissionError::InvalidIdentity)?;
        Ok(Self::new(id, kind, qualified_name.clone(), *retired))
    }
}

/// A tag-37 Uuid value.
fn uuid_value(id: [u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
}

/// The sealed closed descriptor of the object-identity inventory relation.
/// Its key is the identity UUID; its stored fields are the qualified name,
/// the `sys.ObjectKind` ordinal of the object kind and the retirement flag.
pub fn object_identity_descriptor() -> Result<SchemaDescriptor, RelationAdmissionError> {
    let field = |id: [u8; 16], name: &str, primitive: &str, role: u64| {
        Raw::Array(vec![
            uuid_value(id),
            Raw::Text(name.to_owned()),
            Raw::Array(vec![Raw::Int(0.into()), Raw::Text(primitive.to_owned())]),
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    };
    // A closed descriptor lists its fields in ascending field-UUID order.
    SchemaDescriptor::new(Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid_value(OBJECT_IDENTITY_RELATION_ID)),
        (
            Raw::Int(2.into()),
            Raw::Array(vec![uuid_value(OBJECT_ID_FIELD)]),
        ),
        (
            Raw::Int(3.into()),
            Raw::Array(vec![
                field(OBJECT_ID_FIELD, "id", "Uuid", 0),
                field(OBJECT_QUALIFIED_NAME_FIELD, "qualified_name", "Str", 1),
                field(OBJECT_KIND_FIELD, "kind", "Int", 1),
                field(OBJECT_RETIRED_FIELD, "retired", "Bool", 1),
            ]),
        ),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .map_err(|_| RelationAdmissionError::InvalidIdentity)
}

/// The sealed closed descriptor of the rekey log relation. Its key is the
/// dense publication ordinal; its stored fields are the old canonical key,
/// the relation UUID and the new canonical key, in ascending field-UUID order.
/// A stored `Blob` field must hold a ROV-3 content reference, so the canonical
/// ORP key bytes are stored as their lowercase hex `Str` spelling instead.
pub fn object_rekey_descriptor() -> Result<SchemaDescriptor, RelationAdmissionError> {
    let field = |id: [u8; 16], name: &str, primitive: &str, role: u64| {
        Raw::Array(vec![
            uuid_value(id),
            Raw::Text(name.to_owned()),
            Raw::Array(vec![Raw::Int(0.into()), Raw::Text(primitive.to_owned())]),
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    };
    SchemaDescriptor::new(Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid_value(OBJECT_REKEY_RELATION_ID)),
        (
            Raw::Int(2.into()),
            Raw::Array(vec![uuid_value(REKEY_ORDINAL_FIELD)]),
        ),
        (
            Raw::Int(3.into()),
            Raw::Array(vec![
                field(REKEY_ORDINAL_FIELD, "ordinal", "Int", 0),
                field(REKEY_OLD_KEY_FIELD, "old_key", "Str", 1),
                field(REKEY_RELATION_FIELD, "relation", "Uuid", 1),
                field(REKEY_NEW_KEY_FIELD, "new_key", "Str", 1),
            ]),
        ),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .map_err(|_| RelationAdmissionError::InvalidRekeyLog)
}

/// One committed logical rekey (ORNA-EVOL-007, ORNA-MUT-010): its dense
/// publication ordinal, the stable relation it moved a row in, and the exact
/// canonical old and new row addresses. Ordinals run from zero in publication
/// order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRekey {
    ordinal: u64,
    relation_id: [u8; 16],
    old_key: Vec<u8>,
    new_key: Vec<u8>,
}

impl CommittedRekey {
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn old_key(&self) -> &[u8] {
        &self.old_key
    }

    pub fn new_key(&self) -> &[u8] {
        &self.new_key
    }

    /// The row key: the ordinal as a canonical Int.
    fn row_key(&self) -> TypedKey {
        TypedKey::UInt(self.ordinal)
    }

    /// The stored fields in descriptor (field-UUID) order: `old_key`,
    /// `relation`, `new_key`, each key as its lowercase hex spelling.
    fn row_fields(&self) -> Result<Vec<u8>, RelationAdmissionError> {
        Value::new(Raw::Array(vec![
            Raw::Text(hex_encode(&self.old_key)),
            uuid_value(self.relation_id),
            Raw::Text(hex_encode(&self.new_key)),
        ]))
        .and_then(|value| value.encode())
        .map_err(|_| RelationAdmissionError::InvalidRekeyLog)
    }

    fn from_row(key: &TypedKey, fields: &[u8]) -> Result<Self, RelationAdmissionError> {
        // A non-negative canonical Int key decodes as the `UInt` spelling.
        let ordinal = match key {
            TypedKey::UInt(ordinal) => *ordinal,
            TypedKey::Int(ordinal) => {
                u64::try_from(*ordinal).map_err(|_| RelationAdmissionError::InvalidRekeyLog)?
            }
            _ => return Err(RelationAdmissionError::InvalidRekeyLog),
        };
        let Raw::Array(values) = decode_canonical_storage_value(fields)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?
        else {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        };
        let [
            Raw::Text(old_key),
            Raw::Tag(37, relation),
            Raw::Text(new_key),
        ] = values.as_slice()
        else {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        };
        let Raw::Bytes(relation) = relation.as_ref() else {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        };
        let relation_id = <[u8; 16]>::try_from(relation.as_slice())
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        let old_key = canonical_key_from_hex(old_key)?;
        let new_key = canonical_key_from_hex(new_key)?;
        if old_key == new_key {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        }
        Ok(Self {
            ordinal,
            relation_id,
            old_key,
            new_key,
        })
    }
}

/// Decodes one rekey log address: the canonical lowercase hex spelling
/// `hex_encode` writes, naming a canonical ORP key.
fn canonical_key_from_hex(text: &str) -> Result<Vec<u8>, RelationAdmissionError> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(RelationAdmissionError::InvalidRekeyLog),
    };
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return Err(RelationAdmissionError::InvalidRekeyLog);
    };
    let key = pairs
        .iter()
        .map(|[high, low]| Ok((digit(*high)? << 4) | digit(*low)?))
        .collect::<Result<Vec<_>, RelationAdmissionError>>()?;
    TypedKey::decode_canonical(&key).map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
    Ok(key)
}

/// Reads the whole rekey log of one format-3 snapshot, validating each row
/// against the sealed descriptor and checking the ordinals are dense.
fn read_rekey_rows(
    format: &HistoricalFormatContext,
) -> Result<Vec<CommittedRekey>, RelationAdmissionError> {
    let rows = format.load_row_map(OBJECT_REKEY_RELATION_ID)?;
    let graph = format.open_native_graph(&rows)?;
    let descriptor = object_rekey_descriptor()?;
    if graph.read_schema_descriptor(&graph.open_read_scope()?)? != descriptor {
        return Err(RelationAdmissionError::InvalidRekeyLog);
    }
    let mut log = Vec::new();
    let mut lower = None;
    loop {
        // A fresh scope per page keeps every bounded range read inside its
        // own object budget.
        let scope = graph.open_read_scope()?;
        let range = KeyRange::new(lower.clone(), None, MAX_ROW_RANGE_LIMIT)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        let page = graph.range_rows(&range, &scope)?;
        let full = page.len() == MAX_ROW_RANGE_LIMIT;
        for row in &page {
            let fields = row
                .value()
                .encoded_fields()
                .ok_or(RelationAdmissionError::InvalidRekeyLog)?;
            let key = row
                .key()
                .canonical_bytes()
                .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
            descriptor
                .validate_stored_row(OBJECT_REKEY_RELATION_ID, &key, fields)
                .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
            let rekey = CommittedRekey::from_row(row.key(), fields)?;
            if rekey.ordinal != log.len() as u64 {
                return Err(RelationAdmissionError::InvalidRekeyLog);
            }
            log.push(rekey);
        }
        if !full {
            break;
        }
        // Ordinals are dense, so the next page starts at the next ordinal.
        lower = Some(TypedKey::UInt(log.len() as u64));
    }
    if log.len() as u64 != rows.version().row_count().unwrap_or(u64::MAX) {
        return Err(RelationAdmissionError::InvalidRekeyLog);
    }
    Ok(log)
}

/// Writes the rekey log's schema node and its complete row map.
fn write_rekey_relation(
    writer: &mut NodeWriter<'_>,
    log: &[CommittedRekey],
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let descriptor = object_rekey_descriptor()?;
    let (schema_oid, schema_digest) = write_schema_node(writer, &descriptor)?;
    let mut rows = Vec::with_capacity(log.len());
    for rekey in log {
        let key = rekey.row_key();
        let key_bytes = key
            .canonical_bytes()
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        let fields = rekey.row_fields()?;
        descriptor
            .validate_stored_row(OBJECT_REKEY_RELATION_ID, &key_bytes, &fields)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        let value = RowValue::decode_canonical_fields(fields)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        rows.push(RowEntry::new(key, value));
    }
    rows.sort_by(|left, right| left.key().cmp(right.key()));
    let primary_root = NativeGraphContext::build_canonical_ordered_map(
        &rows_domain(&OBJECT_REKEY_RELATION_ID, &schema_digest),
        &rows,
        &mut |node| writer.node(node),
    )?;
    Ok(StoreRelationEntry {
        relation_id: OBJECT_REKEY_RELATION_ID,
        schema_oid,
        primary_root,
        secondary_root: None,
        row_count: rows.len() as u64,
    })
}

impl Repository {
    /// Appends a publication's logical rekeys to the sealed rekey log of
    /// `store_root` and returns the new store root. The log is rebuilt whole
    /// from the prior rows the base snapshot `format` carries plus the new
    /// ones, and every other relation entry of `store_root` is kept. An empty
    /// publication leaves the store untouched.
    pub fn append_rekey_log(
        &self,
        format: &HistoricalFormatContext,
        store_root: &NativeOid,
        rekeys: &[crate::PublicationLogicalRowRekey],
    ) -> Result<NativeOid, RelationAdmissionError> {
        if rekeys.is_empty() {
            return Ok(store_root.clone());
        }
        let database_id = *format
            .database_id()
            .ok_or(RelationAdmissionError::InvalidRekeyLog)?
            .as_bytes();
        let entries = NativeGraphContext::read_store_relations(
            self,
            store_root.algorithm(),
            database_id,
            store_root,
        )?;
        let mut log = if entries
            .iter()
            .any(|entry| entry.relation_id == OBJECT_REKEY_RELATION_ID)
        {
            read_rekey_rows(format)?
        } else {
            Vec::new()
        };
        for rekey in rekeys {
            log.push(CommittedRekey {
                ordinal: log.len() as u64,
                relation_id: rekey.relation_id(),
                old_key: rekey.old_address().to_vec(),
                new_key: rekey.new_address().to_vec(),
            });
        }
        let mut writer = NodeWriter::new(self)?;
        let rekey_entry = write_rekey_relation(&mut writer, &log)?;
        let mut merged = entries
            .into_iter()
            .map(|entry| (entry.relation_id, entry))
            .collect::<BTreeMap<_, _>>();
        merged.insert(OBJECT_REKEY_RELATION_ID, rekey_entry);
        let merged = merged.into_values().collect::<Vec<_>>();
        let root = write_store_root(&mut writer, database_id, &merged)?;
        writer.sync()?;
        Ok(root)
    }

    /// The committed rekey log of a format-3 snapshot, in publication order.
    /// Empty when the snapshot has no rekey log.
    pub fn read_committed_rekeys(
        &self,
        commit: &GitCommitRef,
    ) -> Result<Vec<CommittedRekey>, RelationAdmissionError> {
        let format = self.open_format_context_at_selector(commit.as_str())?;
        let Some(database_id) = format.database_id() else {
            return Ok(Vec::new());
        };
        if format.repository_format() != RepositoryFormat::Format3
            || !self
                .store_relations_in(commit.as_str(), *database_id.as_bytes())?
                .contains(&OBJECT_REKEY_RELATION_ID)
        {
            return Ok(Vec::new());
        }
        read_rekey_rows(&format)
    }
}

/// One source table the staged candidate admits: its stable relation UUID
/// and its closed storage descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRelation {
    relation_id: [u8; 16],
    descriptor: SchemaDescriptor,
}

impl StagedRelation {
    pub fn new(relation_id: [u8; 16], descriptor: SchemaDescriptor) -> Self {
        Self {
            relation_id,
            descriptor,
        }
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }
}

/// The ordinary index captured as an immutable private candidate, so the
/// staged source can be loaded and admitted before the index is changed.
#[derive(Clone, Debug)]
pub struct StagedCandidate {
    candidate: PrivateCommit,
    index: IndexGeneration,
    index_digest: [u8; 32],
    database_id: DatabaseId,
    store_root: Option<NativeOid>,
}

impl StagedCandidate {
    /// The private commit of the captured index tree. Its parent is the
    /// captured `HEAD`, or none on an unborn branch.
    pub fn candidate(&self) -> &PrivateCommit {
        &self.candidate
    }

    /// The staged `.orna/database.orna` identity.
    pub const fn database_id(&self) -> DatabaseId {
        self.database_id
    }

    /// The staged `.orna/store` root, when the index already holds one.
    pub fn store_root(&self) -> Option<&NativeOid> {
        self.store_root.as_ref()
    }
}

/// The result of staging relation admission into the ordinary index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAdmission {
    store_root: NativeOid,
    admitted: Vec<[u8; 16]>,
    changed: bool,
}

impl StoreAdmission {
    /// The staged `.orna/store` root.
    pub fn store_root(&self) -> &NativeOid {
        &self.store_root
    }

    /// Relations created by this staging, in relation-UUID order.
    pub fn admitted(&self) -> &[[u8; 16]] {
        &self.admitted
    }

    /// Whether the index changed. An already complete store is left alone.
    pub const fn changed(&self) -> bool {
        self.changed
    }
}

impl Repository {
    /// Captures the ordinary index as an immutable private candidate without
    /// changing the index, worktree or refs. The commit object stays
    /// unreachable; it only names the exact staged tree for loading.
    pub fn capture_staged_candidate(&self) -> Result<StagedCandidate, RelationAdmissionError> {
        self.ensure_no_git_operation_in_progress()?;
        let index = self.index_generation()?;
        let tree = index
            .tree()
            .cloned()
            .ok_or(RelationAdmissionError::EmptyIndex)?;
        let index_digest = self.index_file_digest()?;
        let candidate = self.private_commit_for_tree(&tree, index.head())?;
        let format = self.open_private_candidate_format_context(&candidate)?;
        if format.repository_format() != RepositoryFormat::Format3 {
            return Err(RelationAdmissionError::NotFormat3);
        }
        let database_id = format.require_database_id()?;
        let store_root = self.tree_store_root(tree.as_str())?;
        Ok(StagedCandidate {
            candidate,
            index,
            index_digest,
            database_id,
            store_root,
        })
    }

    /// Refuses the staged index, before any commit object is written, when it
    /// has no `.orna/store` while `HEAD` commits a relation in one: that
    /// candidate drops rows the commit holds, so 16-administration treats it
    /// as invalid (`sys.admin.assertion_failed`). The capture itself stays a
    /// pure observation; the commit path calls this first.
    pub fn refuse_unretained_committed_store(&self) -> Result<(), RelationAdmissionError> {
        self.ensure_no_git_operation_in_progress()?;
        let index = self.index_generation()?;
        let (Some(head), Some(tree)) = (index.head(), index.tree()) else {
            return Ok(());
        };
        if self.tree_store_root(tree.as_str())?.is_some() {
            return Ok(());
        }
        let format = self.open_pinned_format_context(head.as_str())?;
        if format.repository_format() != RepositoryFormat::Format3 {
            return Ok(());
        }
        match self.committed_store_relations(head)?.into_iter().next() {
            Some(relation_id) => {
                Err(RelationAdmissionError::CommittedRelationUnstaged { relation_id })
            }
            None => Ok(()),
        }
    }

    /// Stages the format-3 store for the captured candidate's admitted tables
    /// and its complete identity inventory.
    ///
    /// A missing store is created. A table without a relation gets its schema
    /// node, one empty leaf and a canonical relation-map insert. Decisions
    /// follow `HEAD`'s committed store, not the index: a relation `HEAD`
    /// commits must keep its descriptor exactly and cannot be dropped, while
    /// a relation only an earlier staging created (its commit failed, so it
    /// was never committed) follows the staged source as long as it holds no
    /// rows (ORNA-ADMIN-003). The identity inventory relation is rewritten
    /// from `identities`, and the legacy identity sidecar leaves the index.
    /// The new store is bound at `.orna/store` in the ordinary index under
    /// Git's index lock, its node metadata is materialized in the worktree
    /// like a publication's, and every other staged or unstaged path is
    /// preserved.
    pub fn stage_store_admission(
        &self,
        staged: &StagedCandidate,
        relations: &[StagedRelation],
        identities: &[StoredObjectIdentity],
    ) -> Result<StoreAdmission, RelationAdmissionError> {
        let database_id = *staged.database_id.as_bytes();
        let mut writer = NodeWriter::new(self)?;
        // An unborn branch commits no relation.
        let committed: BTreeMap<[u8; 16], NativeOid> = match staged.index.head() {
            Some(head) => self
                .store_entries_in(head.as_str(), database_id)?
                .into_iter()
                .map(|entry| (entry.relation_id, entry.schema_oid))
                .collect(),
            None => BTreeMap::new(),
        };
        let base = match staged.store_root.as_ref() {
            // `git rm --cached` leaves skip-worktree payload entries in the
            // index, so the store tree survives while some node data does
            // not. A committed relation whose node is absent is unstaged,
            // not a corrupt store (ORNA-STAGE-004 refuses the candidate).
            Some(root) => {
                NativeGraphContext::read_store_relations(self, writer.algorithm, database_id, root)
                    .map_err(|error| match (error, committed.keys().next()) {
                        (GraphError::MissingNodeData, Some(relation_id)) => {
                            RelationAdmissionError::CommittedRelationUnstaged {
                                relation_id: *relation_id,
                            }
                        }
                        (error, _) => error.into(),
                    })?
            }
            None => Vec::new(),
        };
        let (entries, admitted) =
            admit_relations(&mut writer, base, &committed, relations, Some(identities))?;
        let store_root = write_store_root(&mut writer, database_id, &entries)?;
        let sidecar_staged = self.index_tracks(None, LEGACY_OBJECT_IDENTITY_SIDECAR_PATH)?;
        if staged.store_root.as_ref() == Some(&store_root) && !sidecar_staged {
            return Ok(StoreAdmission {
                store_root,
                admitted,
                changed: false,
            });
        }
        writer.sync()?;
        self.install_staged_store(staged, &store_root)?;
        Ok(StoreAdmission {
            store_root,
            admitted,
            changed: true,
        })
    }

    /// The relation UUIDs a format-3 commit's `.orna/store` binds, empty when
    /// the commit has no store. Every page of the relation map is validated
    /// before the set is returned.
    pub fn committed_store_relations(
        &self,
        commit: &GitCommitRef,
    ) -> Result<BTreeSet<[u8; 16]>, RelationAdmissionError> {
        let format = self.open_pinned_format_context(commit.as_str())?;
        if format.repository_format() != RepositoryFormat::Format3 {
            return Err(RelationAdmissionError::NotFormat3);
        }
        self.store_relations_in(commit.as_str(), *format.require_database_id()?.as_bytes())
    }

    /// Reads the committed identity inventory from a format-3 snapshot's
    /// store. `Ok(None)` means the snapshot has no inventory relation: it is a
    /// legacy snapshot (an older format, no store, or a store written before
    /// the inventory moved into it).
    pub fn read_committed_object_identities(
        &self,
        commit: &GitCommitRef,
    ) -> Result<Option<Vec<StoredObjectIdentity>>, RelationAdmissionError> {
        let format = self.open_pinned_format_context(commit.as_str())?;
        self.read_object_identities_in(&format, commit.as_str())
    }

    /// Reads the identity inventory the staged candidate's store carries:
    /// the committed one, or one an earlier staging wrote, so a retried
    /// commit keeps its IDs. `Ok(None)` when the staged store has none.
    pub fn read_staged_object_identities(
        &self,
        staged: &StagedCandidate,
    ) -> Result<Option<Vec<StoredObjectIdentity>>, RelationAdmissionError> {
        let format = self.open_private_candidate_format_context(&staged.candidate)?;
        self.read_object_identities_in(&format, staged.candidate.tree.as_str())
    }

    /// The bytes of a legacy commit's tracked identity sidecar, or `None` when
    /// the commit does not track one. Only commits written before the
    /// inventory moved into the store carry it.
    pub fn read_legacy_identity_sidecar(
        &self,
        commit: &GitCommitRef,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RelationAdmissionError> {
        if !self.tracks_legacy_identity_sidecar(commit.as_str())? {
            return Ok(None);
        }
        Ok(Some(self.read_committed_file(
            commit,
            LEGACY_OBJECT_IDENTITY_SIDECAR_PATH,
            max_bytes,
        )?))
    }

    /// The legacy identity sidecar the staged candidate's tree tracks, as a
    /// legacy repository's index still does before its first store staging.
    pub fn read_staged_legacy_identity_sidecar(
        &self,
        staged: &StagedCandidate,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RelationAdmissionError> {
        let tree = staged.candidate.tree.as_str();
        if !self.tracks_legacy_identity_sidecar(tree)? {
            return Ok(None);
        }
        Ok(Some(self.read_tree_file(
            tree,
            LEGACY_OBJECT_IDENTITY_SIDECAR_PATH,
            max_bytes,
        )?))
    }

    /// The validated relation entries of the store a tree-ish names; empty
    /// when it has no store.
    fn store_entries_in(
        &self,
        treeish: &str,
        database_id: [u8; 16],
    ) -> Result<Vec<StoreRelationEntry>, RelationAdmissionError> {
        let Some(store_root) = self.tree_store_root(treeish)? else {
            return Ok(Vec::new());
        };
        Ok(NativeGraphContext::read_store_relations(
            self,
            store_root.algorithm(),
            database_id,
            &store_root,
        )?)
    }

    /// The relation UUIDs bound by the store a tree-ish names, validated.
    fn store_relations_in(
        &self,
        treeish: &str,
        database_id: [u8; 16],
    ) -> Result<BTreeSet<[u8; 16]>, RelationAdmissionError> {
        Ok(self
            .store_entries_in(treeish, database_id)?
            .into_iter()
            .map(|entry| entry.relation_id)
            .collect())
    }

    /// Reads the identity relation of the store `treeish` names through the
    /// format context pinned to that same tree.
    fn read_object_identities_in(
        &self,
        format: &RepositoryFormatContext,
        treeish: &str,
    ) -> Result<Option<Vec<StoredObjectIdentity>>, RelationAdmissionError> {
        if format.repository_format() != RepositoryFormat::Format3
            || !self
                .store_relations_in(treeish, *format.require_database_id()?.as_bytes())?
                .contains(&OBJECT_IDENTITY_RELATION_ID)
        {
            return Ok(None);
        }
        let rows = format.load_row_map(OBJECT_IDENTITY_RELATION_ID)?;
        let graph = format.open_native_graph(&rows)?;
        let descriptor = object_identity_descriptor()?;
        if graph.read_schema_descriptor(&graph.open_read_scope()?)? != descriptor {
            return Err(RelationAdmissionError::InvalidIdentity);
        }
        let mut identities = Vec::new();
        let mut lower = None;
        loop {
            // A fresh scope per page keeps every bounded range read inside
            // its own object budget.
            let scope = graph.open_read_scope()?;
            let range = KeyRange::new(lower.clone(), None, MAX_ROW_RANGE_LIMIT)
                .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
            let page = graph.range_rows(&range, &scope)?;
            let full = page.len() == MAX_ROW_RANGE_LIMIT;
            for row in &page {
                let fields = row
                    .value()
                    .encoded_fields()
                    .ok_or(RelationAdmissionError::InvalidIdentity)?;
                let key = row
                    .key()
                    .canonical_bytes()
                    .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
                descriptor
                    .validate_stored_row(OBJECT_IDENTITY_RELATION_ID, &key, fields)
                    .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
                identities.push(StoredObjectIdentity::from_row(row.key(), fields)?);
            }
            let Some(last) = page.last() else {
                break;
            };
            if !full {
                break;
            }
            // The next page starts strictly after the last key read: NUL is
            // the least character, so `key + NUL` is the key's successor.
            let TypedKey::Text(text) = last.key() else {
                return Err(RelationAdmissionError::InvalidIdentity);
            };
            lower = Some(TypedKey::Text(format!("{text}\0")));
        }
        if identities.len() as u64 != rows.version().row_count().unwrap_or(u64::MAX) {
            return Err(RelationAdmissionError::InvalidIdentity);
        }
        Ok(Some(identities))
    }

    /// Whether a tree-ish tracks the legacy identity sidecar as a file.
    fn tracks_legacy_identity_sidecar(
        &self,
        treeish: &str,
    ) -> Result<bool, RelationAdmissionError> {
        let mut command = self.command();
        command.args([
            "ls-tree",
            "-z",
            "--full-tree",
            treeish,
            "--",
            LEGACY_OBJECT_IDENTITY_SIDECAR_PATH,
        ]);
        let listed = self.run(command)?.stdout;
        Ok(listed.split(|byte| *byte == 0).any(|entry| {
            entry
                .rsplit(|byte| *byte == b'\t')
                .next()
                .is_some_and(|path| path == LEGACY_OBJECT_IDENTITY_SIDECAR_PATH.as_bytes())
        }))
    }

    /// The repository format the ordinary index's tree declares. An unborn
    /// branch has no committed format, so its first reviewed commit takes
    /// the staged one.
    pub(crate) fn staged_repository_format(&self) -> Result<RepositoryFormat, RepositoryError> {
        let index = self.index_generation()?;
        let tree = index
            .tree()
            .cloned()
            .ok_or(RepositoryError::SysGitNothingToCommit)?;
        let candidate = self
            .private_commit_for_tree(&tree, index.head())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        Ok(self
            .open_private_candidate_format_context(&candidate)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?
            .repository_format())
    }

    /// SHA-256 of the ordinary index file, or of nothing when it is absent.
    fn index_file_digest(&self) -> Result<[u8; 32], RelationAdmissionError> {
        let index = self.git_path("index")?;
        match fs::read(&index) {
            Ok(bytes) => Ok(Sha256::digest(&bytes).into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Sha256::digest([]).into())
            }
            Err(_) => Err(RepositoryError::LocalStateUnavailable.into()),
        }
    }

    /// Builds an unreachable commit naming `tree`, parented on `head` when the
    /// branch is born.
    fn private_commit_for_tree(
        &self,
        tree: &IndexTreeRef,
        head: Option<&GitCommitRef>,
    ) -> Result<PrivateCommit, RelationAdmissionError> {
        let mut command = self.command();
        command.args(["commit-tree", tree.as_str()]);
        if let Some(head) = head {
            command.args(["-p", head.as_str()]);
        }
        command.args(["-m", STAGED_CANDIDATE_MESSAGE]);
        let commit =
            self.snapshot_from_native_oid(crate::trim_output(&self.run(command)?.stdout))?;
        Ok(PrivateCommit {
            tree: tree.clone(),
            commit,
        })
    }

    /// The `.orna/store` tree named by a tree-ish, when it has one.
    fn tree_store_root(&self, treeish: &str) -> Result<Option<NativeOid>, RelationAdmissionError> {
        let mut command = self.command();
        command.args(["ls-tree", "-z", "--full-tree", treeish, "--", STORE_PATH]);
        let output = self.run(command)?.stdout;
        let Some(entry) = output
            .split(|byte| *byte == 0)
            .find(|entry| !entry.is_empty())
        else {
            return Ok(None);
        };
        let entry =
            std::str::from_utf8(entry).map_err(|_| RelationAdmissionError::InvalidStoreEntry)?;
        let (header, path) = entry
            .split_once('\t')
            .ok_or(RelationAdmissionError::InvalidStoreEntry)?;
        let mut fields = header.split(' ');
        let (Some("040000"), Some("tree"), Some(object), None, STORE_PATH) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            path,
        ) else {
            return Err(RelationAdmissionError::InvalidStoreEntry);
        };
        let algorithm = self.native_hash_algorithm()?;
        Ok(Some(NativeOid::from_hex(algorithm, object)?))
    }

    fn native_hash_algorithm(&self) -> Result<GitHashAlgorithm, RelationAdmissionError> {
        match self.native_object_id_length()? {
            40 => Ok(GitHashAlgorithm::Sha1),
            64 => Ok(GitHashAlgorithm::Sha256),
            _ => Err(RepositoryError::UnsupportedObjectFormat.into()),
        }
    }

    /// Whether `path` is an entry of the ordinary index, or of `index` when
    /// one is supplied.
    fn index_tracks(
        &self,
        index: Option<&Path>,
        path: &str,
    ) -> Result<bool, RelationAdmissionError> {
        let mut command = self.command();
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command.args(["ls-files", "-z", "--", path]);
        Ok(self
            .run(command)?
            .stdout
            .split(|byte| *byte == 0)
            .any(|entry| entry == path.as_bytes()))
    }

    /// Replaces the ordinary index's `.orna/store` entry with `store_root`
    /// and drops the legacy identity sidecar, while holding the coordination
    /// lock and Git's own `index.lock`. A retained publication or checkout
    /// recovery record owns the index and store worktree until its recovery
    /// completes, so staging refuses before touching either (22-publication
    /// recovery table, ORNA-PUB-007; CHECKOUT-3 step 7). The captured index
    /// and `HEAD` must be unchanged, and the new index is installed by an
    /// atomic rename. If the index cannot be installed after the store
    /// worktree moved, the worktree is moved back to the captured store.
    fn install_staged_store(
        &self,
        staged: &StagedCandidate,
        store_root: &NativeOid,
    ) -> Result<(), RelationAdmissionError> {
        let _coordination = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        self.require_no_pending_checkout_recovery_locked()?;
        self.ensure_no_git_operation_in_progress()?;
        let index = self.git_path("index")?;
        let _git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let unchanged = || -> Result<bool, RelationAdmissionError> {
            let current = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            Ok(
                <[u8; 32]>::from(Sha256::digest(current)) == staged.index_digest
                    && self.head()?.as_ref() == staged.index.head(),
            )
        };
        if !unchanged()? {
            return Err(RelationAdmissionError::StaleIndex);
        }
        let candidate_index = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .join(format!(
                "index-store-admission-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
        fs::write(&candidate_index, &base_index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let result = (|| {
            bind_tree_entry_in_index(
                || self.command(),
                &candidate_index,
                STORE_PATH,
                "040000",
                &store_root.to_hex(),
            )?;
            if self.index_tracks(Some(&candidate_index), LEGACY_OBJECT_IDENTITY_SIDECAR_PATH)? {
                let mut remove = self.command();
                remove
                    .env("GIT_INDEX_FILE", &candidate_index)
                    .args(["update-index", "--force-remove", "--"])
                    .arg(LEGACY_OBJECT_IDENTITY_SIDECAR_PATH);
                self.run(remove)?;
            }
            let mut write_tree = self.command();
            write_tree
                .env("GIT_INDEX_FILE", &candidate_index)
                .args(["write-tree"]);
            let tree =
                self.index_tree_from_native_oid(crate::trim_output(&self.run(write_tree)?.stdout))?;
            let next = self.private_commit_for_tree(&tree, staged.index.head())?;
            // A writer that bypassed Git's index lock is detected before the
            // store worktree changes.
            if !unchanged()? {
                return Err(RelationAdmissionError::StaleIndex);
            }
            // Envelope `data` leaves are materialized and payload chunks stay
            // skip-worktree, exactly as a publication reconciles the store.
            self.reconcile_store_worktree_transition(&staged.index, &next, true)?;
            let installed = (|| -> Result<(), RelationAdmissionError> {
                self.configure_store_tree_skip_worktree(&candidate_index)?;
                fs::File::open(&candidate_index)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                if !unchanged()? {
                    return Err(RelationAdmissionError::StaleIndex);
                }
                fs::rename(&candidate_index, &index)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                Ok(())
            })();
            if let Err(error) = installed {
                // The index keeps the captured store, so its worktree files
                // go back to that store's.
                let captured_tree = staged
                    .index
                    .tree()
                    .ok_or(RelationAdmissionError::EmptyIndex)?;
                let captured = self.private_commit_for_tree(captured_tree, staged.index.head())?;
                let moved = IndexGeneration {
                    head: staged.index.head().cloned(),
                    tree: Some(tree),
                };
                self.reconcile_store_worktree_transition(&moved, &captured, true)?;
                return Err(error);
            }
            fs::File::open(
                index
                    .parent()
                    .ok_or(RepositoryError::LocalStateUnavailable)?,
            )
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            Ok(())
        })();
        let _ = fs::remove_file(&candidate_index);
        result
    }
}

/// Merges `relations` into the validated `base` relation entries of the
/// staged store. `committed` maps each relation `HEAD` commits to its schema
/// node. A committed relation keeps its schema node exactly, stays in the
/// staged store and cannot be dropped. Any other relation is admitted by
/// this commit: a new one, or one only an earlier staging created, gets its
/// schema node and one empty leaf, and an uncommitted relation the staged
/// source no longer declares is dropped; either is refused when the staged
/// relation holds rows. When `identities` is supplied the inventory relation
/// is rewritten from it, and every other base relation must be owned by a
/// staged table.
fn admit_relations(
    writer: &mut NodeWriter<'_>,
    base: Vec<StoreRelationEntry>,
    committed: &BTreeMap<[u8; 16], NativeOid>,
    relations: &[StagedRelation],
    identities: Option<&[StoredObjectIdentity]>,
) -> Result<(Vec<StoreRelationEntry>, Vec<[u8; 16]>), RelationAdmissionError> {
    let mut entries = base
        .into_iter()
        .map(|entry| (entry.relation_id, entry))
        .collect::<BTreeMap<_, _>>();
    let mut staged = BTreeSet::new();
    let mut admitted = Vec::new();
    for relation in relations {
        let relation_id = relation.relation_id;
        if relation_id == OBJECT_IDENTITY_RELATION_ID
            || relation_id == OBJECT_REKEY_RELATION_ID
            || !staged.insert(relation_id)
        {
            return Err(RelationAdmissionError::DuplicateRelation { relation_id });
        }
        let (schema_oid, schema_digest) = write_schema_node(writer, &relation.descriptor)?;
        let existing = entries.get(&relation_id);
        if let Some(committed_schema) = committed.get(&relation_id) {
            if *committed_schema != schema_oid
                || existing.is_some_and(|entry| entry.schema_oid != schema_oid)
            {
                return Err(RelationAdmissionError::SchemaChanged { relation_id });
            }
            if existing.is_none() {
                return Err(RelationAdmissionError::CommittedRelationUnstaged { relation_id });
            }
            continue;
        }
        admitted.push(relation_id);
        match existing {
            Some(entry) if entry.schema_oid == schema_oid => {}
            Some(entry) if entry.row_count != 0 => {
                return Err(RelationAdmissionError::UncommittedRelationRows { relation_id });
            }
            _ => {
                let primary_root = NativeGraphContext::build_canonical_ordered_map(
                    &rows_domain(&relation_id, &schema_digest),
                    &[],
                    &mut |node| writer.node(node),
                )?;
                entries.insert(
                    relation_id,
                    StoreRelationEntry {
                        relation_id,
                        schema_oid,
                        primary_root,
                        secondary_root: None,
                        row_count: 0,
                    },
                );
            }
        }
    }
    if let Some(identities) = identities {
        let mut dropped = Vec::new();
        for (relation_id, entry) in &entries {
            let relation_id = *relation_id;
            if relation_id == OBJECT_IDENTITY_RELATION_ID
                || relation_id == OBJECT_REKEY_RELATION_ID
                || staged.contains(&relation_id)
            {
                continue;
            }
            if committed.contains_key(&relation_id) {
                return Err(RelationAdmissionError::OrphanRelation { relation_id });
            }
            if entry.row_count != 0 {
                return Err(RelationAdmissionError::UncommittedRelationRows { relation_id });
            }
            dropped.push(relation_id);
        }
        for relation_id in dropped {
            entries.remove(&relation_id);
        }
        if !committed.contains_key(&OBJECT_IDENTITY_RELATION_ID) {
            admitted.push(OBJECT_IDENTITY_RELATION_ID);
        }
        entries.insert(
            OBJECT_IDENTITY_RELATION_ID,
            write_identity_relation(writer, identities)?,
        );
    }
    admitted.sort_unstable();
    Ok((entries.into_values().collect(), admitted))
}

/// Writes the inventory relation's schema node and its complete row map.
fn write_identity_relation(
    writer: &mut NodeWriter<'_>,
    identities: &[StoredObjectIdentity],
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let descriptor = object_identity_descriptor()?;
    let (schema_oid, schema_digest) = write_schema_node(writer, &descriptor)?;
    let mut ids = BTreeSet::new();
    let mut rows = Vec::with_capacity(identities.len());
    for identity in identities {
        if identity.qualified_name.is_empty()
            || identity.qualified_name.contains('\0')
            || !ids.insert(identity.id)
        {
            return Err(RelationAdmissionError::InvalidIdentity);
        }
        let key = identity.row_key()?;
        let key_bytes = key
            .canonical_bytes()
            .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
        let fields = identity.row_fields()?;
        descriptor
            .validate_stored_row(OBJECT_IDENTITY_RELATION_ID, &key_bytes, &fields)
            .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
        let value = RowValue::decode_canonical_fields(fields)
            .map_err(|_| RelationAdmissionError::InvalidIdentity)?;
        rows.push(RowEntry::new(key, value));
    }
    rows.sort_by(|left, right| left.key().cmp(right.key()));
    let primary_root = NativeGraphContext::build_canonical_ordered_map(
        &rows_domain(&OBJECT_IDENTITY_RELATION_ID, &schema_digest),
        &rows,
        &mut |node| writer.node(node),
    )?;
    Ok(StoreRelationEntry {
        relation_id: OBJECT_IDENTITY_RELATION_ID,
        schema_oid,
        primary_root,
        secondary_root: None,
        row_count: rows.len() as u64,
    })
}

/// Writes the relation map for `entries` (ascending relation UUIDs) and the
/// store root naming it.
fn write_store_root(
    writer: &mut NodeWriter<'_>,
    database_id: [u8; 16],
    entries: &[StoreRelationEntry],
) -> Result<NativeOid, RelationAdmissionError> {
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut value = Vec::with_capacity(96);
        cbor_head(&mut value, 4, 4);
        cbor_bytes(&mut value, entry.schema_oid.as_bytes());
        cbor_bytes(&mut value, entry.primary_root.as_bytes());
        let mut dependencies = vec![
            RowDependency::new(entry.schema_oid.clone(), NativeObjectKind::Tree),
            RowDependency::new(entry.primary_root.clone(), NativeObjectKind::Tree),
        ];
        match &entry.secondary_root {
            Some(secondary) => {
                cbor_bytes(&mut value, secondary.as_bytes());
                dependencies.push(RowDependency::new(
                    secondary.clone(),
                    NativeObjectKind::Tree,
                ));
            }
            None => value.push(0xf6),
        }
        cbor_head(&mut value, 0, entry.row_count);
        dependencies.sort_by(|left, right| left.oid().cmp(right.oid()));
        dependencies.dedup();
        let value =
            RowValue::inline(value, dependencies).map_err(|_| GraphError::NonCanonicalData)?;
        rows.push(RowEntry::new(
            TypedKey::Bytes(entry.relation_id.to_vec()),
            value,
        ));
    }
    let mut domain = Vec::with_capacity(32);
    cbor_head(&mut domain, 4, 2);
    cbor_text(&mut domain, "relations");
    cbor_bytes(&mut domain, &database_id);
    let relation_map =
        NativeGraphContext::build_canonical_ordered_map(&domain, &rows, &mut |node| {
            writer.node(node)
        })?;
    Ok(writer.node(&NodeData::StoreRoot { relation_map })?)
}

/// Writes a closed descriptor as a kind-6 schema node over its OGB-2 byte
/// index. The digest is the SHA-256 of the encoded descriptor.
fn write_schema_node(
    writer: &mut NodeWriter<'_>,
    descriptor: &SchemaDescriptor,
) -> Result<(NativeOid, [u8; 32]), RelationAdmissionError> {
    let bytes = descriptor
        .encode()
        .map_err(|_| GraphError::NonCanonicalData)?;
    let schema_digest: [u8; 32] = Sha256::digest(&bytes).into();
    let byte_root = write_byte_index(writer, &bytes)?.ok_or(GraphError::InvalidByteIndex)?;
    let schema_oid = writer.node(&NodeData::Schema {
        encoded_length: bytes.len() as u64,
        schema_digest,
        byte_root,
    })?;
    Ok((schema_oid, schema_digest))
}

/// Writes `bytes` as the canonical OGB-2 byte index (store-format-3 §4): Gear
/// chunks at height 0, grouped by the ORP-1 limits and the
/// `orna.ogb.index.v2` anchor, level by level until one root remains. This is
/// the grouping the capture writer streams; empty input has no index.
fn write_byte_index(
    writer: &mut NodeWriter<'_>,
    bytes: &[u8],
) -> Result<Option<NativeOid>, GraphError> {
    let spans = GearChunker::new()
        .spans(bytes)
        .map_err(|_| GraphError::InvalidByteIndex)?;
    let mut levels: Vec<Vec<ByteIndexEntry>> = vec![Vec::new()];
    for span in spans {
        let start = usize::try_from(span.offset()).map_err(|_| GraphError::InvalidByteIndex)?;
        let end = start
            .checked_add(usize::try_from(span.length()).map_err(|_| GraphError::InvalidByteIndex)?)
            .ok_or(GraphError::InvalidByteIndex)?;
        let target = writer.blob(bytes.get(start..end).ok_or(GraphError::InvalidByteIndex)?)?;
        push_byte_index_entry(
            writer,
            &mut levels,
            0,
            ByteIndexEntry {
                span_length: span.length(),
                target,
                chunk_sha256: Some(span.sha256()),
            },
        )?;
    }
    loop {
        let Some(level) = levels.iter().position(|entries| !entries.is_empty()) else {
            return Ok(None);
        };
        let only_top_entry = level > 0
            && levels[level].len() == 1
            && levels.iter().skip(level + 1).all(Vec::is_empty);
        if only_top_entry {
            return Ok(levels[level].first().map(|entry| entry.target.clone()));
        }
        flush_byte_index_level(writer, &mut levels, level)?;
    }
}

fn push_byte_index_entry(
    writer: &mut NodeWriter<'_>,
    levels: &mut Vec<Vec<ByteIndexEntry>>,
    level: usize,
    entry: ByteIndexEntry,
) -> Result<(), GraphError> {
    if level > usize::from(MAX_GRAPH_HEIGHT) {
        return Err(GraphError::HeightExceeded(
            u8::try_from(level).unwrap_or(u8::MAX),
        ));
    }
    if levels.len() <= level {
        levels.resize_with(level + 1, Vec::new);
    }
    let anchor = byte_index_anchor(level, &entry)?;
    levels[level].push(entry);
    let held = levels[level].len();
    if held == MAX_REFS || (held >= 16 && anchor) {
        flush_byte_index_level(writer, levels, level)?;
    }
    Ok(())
}

fn flush_byte_index_level(
    writer: &mut NodeWriter<'_>,
    levels: &mut Vec<Vec<ByteIndexEntry>>,
    level: usize,
) -> Result<(), GraphError> {
    let entries = std::mem::take(levels.get_mut(level).ok_or(GraphError::InvalidByteIndex)?);
    if entries.is_empty() {
        return Ok(());
    }
    let total_length = entries.iter().try_fold(0u64, |total, entry| {
        total
            .checked_add(entry.span_length)
            .ok_or(GraphError::InvalidByteIndex)
    })?;
    let target = writer.node(&NodeData::ByteIndex {
        height: u8::try_from(level).map_err(|_| GraphError::InvalidByteIndex)?,
        total_length,
        entries,
    })?;
    push_byte_index_entry(
        writer,
        levels,
        level + 1,
        ByteIndexEntry {
            span_length: total_length,
            target,
            chunk_sha256: None,
        },
    )
}

/// `SHA256(ASCII("orna.ogb.index.v2") || 00 || uint32be(level) || token)` has
/// its low six bits clear. The token is CBOR `[span_length, chunk_sha256]` at
/// height 0 and the child index OID above.
fn byte_index_anchor(level: usize, entry: &ByteIndexEntry) -> Result<bool, GraphError> {
    let mut token = Vec::with_capacity(48);
    if level == 0 {
        let digest = entry.chunk_sha256.ok_or(GraphError::InvalidByteIndex)?;
        cbor_head(&mut token, 4, 2);
        cbor_head(&mut token, 0, entry.span_length);
        cbor_bytes(&mut token, &digest);
    } else {
        if entry.chunk_sha256.is_some() {
            return Err(GraphError::InvalidByteIndex);
        }
        token.extend_from_slice(entry.target.as_bytes());
    }
    let mut hasher = Sha256::new();
    hasher.update(b"orna.ogb.index.v2\0");
    hasher.update(
        u32::try_from(level)
            .map_err(|_| GraphError::InvalidByteIndex)?
            .to_be_bytes(),
    );
    hasher.update(token);
    Ok(hasher.finalize()[31] & 0x3f == 0)
}

/// The primary-map domain `["rows", table_uuid, schema_digest]`.
fn rows_domain(relation_id: &[u8; 16], schema_digest: &[u8; 32]) -> Vec<u8> {
    let mut domain = Vec::with_capacity(56);
    cbor_head(&mut domain, 4, 3);
    cbor_text(&mut domain, "rows");
    cbor_bytes(&mut domain, relation_id);
    cbor_bytes(&mut domain, schema_digest);
    domain
}

/// Appends a shortest-form definite CBOR head.
fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    if value < 24 {
        output.push(prefix | value as u8);
    } else if value <= u64::from(u8::MAX) {
        output.extend_from_slice(&[prefix | 24, value as u8]);
    } else if value <= u64::from(u16::MAX) {
        output.push(prefix | 25);
        output.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u64::from(u32::MAX) {
        output.push(prefix | 26);
        output.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        output.push(prefix | 27);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    cbor_head(output, 2, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

fn cbor_text(output: &mut Vec<u8>, text: &str) {
    cbor_head(output, 3, text.len() as u64);
    output.extend_from_slice(text.as_bytes());
}

/// Writes OGS-1 node envelopes and raw chunks into the repository's object
/// store and records every object for one durability barrier before any
/// index or ref names them (ORNA-PUB-005).
struct NodeWriter<'a> {
    repository: &'a Repository,
    algorithm: GitHashAlgorithm,
    written: BTreeSet<NativeOid>,
}

impl<'a> NodeWriter<'a> {
    fn new(repository: &'a Repository) -> Result<Self, RelationAdmissionError> {
        Ok(Self {
            repository,
            algorithm: repository.native_hash_algorithm()?,
            written: BTreeSet::new(),
        })
    }

    fn blob(&mut self, bytes: &[u8]) -> Result<NativeOid, GraphError> {
        let hex = self
            .repository
            .hash_object(bytes)
            .map_err(|_| GraphError::GitCommandFailed)?;
        let oid = NativeOid::from_hex(self.algorithm, &hex)?;
        self.written.insert(oid.clone());
        Ok(oid)
    }

    /// Writes one tree through `git mktree`, which normalizes entry order.
    fn tree(
        &mut self,
        entries: &[(String, NativeObjectKind, NativeOid)],
    ) -> Result<NativeOid, GraphError> {
        let mut listing = String::new();
        for (name, kind, oid) in entries {
            let mode = match kind {
                NativeObjectKind::Tree => "040000 tree",
                NativeObjectKind::Blob => "100644 blob",
            };
            listing.push_str(&format!("{mode} {}\t{name}\n", oid.to_hex()));
        }
        let mut command = self.repository.command();
        command
            .args(["mktree"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| GraphError::GitCommandFailed)?;
        child
            .stdin
            .take()
            .ok_or(GraphError::GitCommandFailed)?
            .write_all(listing.as_bytes())
            .map_err(|_| GraphError::GitCommandFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            return Err(GraphError::GitCommandFailed);
        }
        let hex = std::str::from_utf8(&output.stdout)
            .map_err(|_| GraphError::GitObjectMalformed)?
            .trim();
        let oid = NativeOid::from_hex(self.algorithm, hex)?;
        self.written.insert(oid.clone());
        Ok(oid)
    }

    /// Writes one complete node envelope: `data`, plus `refs` naming exactly
    /// the dependencies its typed payload derives.
    fn node(&mut self, node: &NodeData) -> Result<NativeOid, GraphError> {
        let envelope = NativeNodeEnvelope::from_node(node)?;
        let data = self.blob(&envelope.data)?;
        let mut entries = vec![("data".to_owned(), NativeObjectKind::Blob, data)];
        if !envelope.refs.is_empty() {
            let references = envelope
                .refs
                .iter()
                .map(|reference| {
                    (
                        reference.oid().to_hex(),
                        reference.kind(),
                        reference.oid().clone(),
                    )
                })
                .collect::<Vec<_>>();
            let refs = self.tree(&references)?;
            entries.push(("refs".to_owned(), NativeObjectKind::Tree, refs));
        }
        self.tree(&entries)
    }

    /// Flushes every written object through the shared barrier: each loose
    /// object and its fan-out directory, or the pack files when Git stored an
    /// object packed.
    fn sync(&self) -> Result<(), RelationAdmissionError> {
        let objects = self.repository.git_path("objects")?;
        Ok(sync_object_files(&objects, &self.written, || Ok(()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_uuid(label: &str) -> [u8; 16] {
        let mut hasher = Sha256::new();
        hasher.update(b"orna.system-relation.v1\0");
        hasher.update(label.as_bytes());
        let digest = hasher.finalize();
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        id[6] = (id[6] & 0x0f) | 0x80;
        id[8] = (id[8] & 0x3f) | 0x80;
        id
    }

    #[test]
    fn sealed_identities_are_the_documented_uuidv8_derivations() {
        assert_eq!(
            OBJECT_IDENTITY_RELATION_ID,
            system_uuid("orna.object-identity")
        );
        assert_eq!(OBJECT_ID_FIELD, system_uuid("orna.object-identity.id"));
        assert_eq!(OBJECT_KIND_FIELD, system_uuid("orna.object-identity.kind"));
        assert_eq!(
            OBJECT_QUALIFIED_NAME_FIELD,
            system_uuid("orna.object-identity.qualified_name")
        );
        assert_eq!(
            OBJECT_RETIRED_FIELD,
            system_uuid("orna.object-identity.retired")
        );
        assert_eq!(
            Uuid::from_bytes(OBJECT_IDENTITY_RELATION_ID).get_version_num(),
            8
        );
    }

    #[test]
    fn identity_rows_round_trip_through_the_sealed_descriptor() {
        let descriptor = object_identity_descriptor().expect("sealed descriptor is valid");
        let identity = StoredObjectIdentity::new([0x42; 16], 2, "Music.title", true);
        let row_key = identity.row_key().unwrap();
        // The key is the tag-37 Uuid value, not UUID text.
        assert_eq!(
            row_key.canonical_bytes().unwrap(),
            [&[0xd8, 0x25, 0x50][..], &[0x42; 16][..]].concat()
        );
        let key = row_key.canonical_bytes().unwrap();
        let fields = identity.row_fields().unwrap();
        descriptor
            .validate_stored_row(OBJECT_IDENTITY_RELATION_ID, &key, &fields)
            .expect("an identity row matches the sealed descriptor");
        assert_eq!(
            StoredObjectIdentity::from_row(&row_key, &fields).unwrap(),
            identity
        );
        assert!(matches!(
            StoredObjectIdentity::from_row(
                &TypedKey::Text(Uuid::from_bytes([0x42; 16]).hyphenated().to_string()),
                &fields
            ),
            Err(RelationAdmissionError::InvalidIdentity)
        ));
    }

    #[test]
    fn cbor_heads_use_the_shortest_definite_form() {
        let encode = |major, value| {
            let mut output = Vec::new();
            cbor_head(&mut output, major, value);
            output
        };
        assert_eq!(encode(0, 23), vec![0x17]);
        assert_eq!(encode(0, 24), vec![0x18, 24]);
        assert_eq!(encode(0, 256), vec![0x19, 0x01, 0x00]);
        assert_eq!(encode(0, 65_536), vec![0x1a, 0, 1, 0, 0]);
        assert_eq!(encode(0, 1 << 32), vec![0x1b, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(encode(4, 2), vec![0x82]);
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git prints UTF-8")
    }

    /// A UUIDv4-shaped relation identity.
    fn relation(seed: u16) -> [u8; 16] {
        let mut id = [0u8; 16];
        id[..2].copy_from_slice(&seed.to_be_bytes());
        id[2..6].copy_from_slice(&[0x5e; 4]);
        id[6] = 0x40;
        id[8] = 0x80;
        id
    }

    /// A closed `(id: Str) { title: <title_type> }` descriptor.
    fn table_descriptor(relation_id: [u8; 16], title_type: &str) -> SchemaDescriptor {
        let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
        let field = |id: [u8; 16], name: &str, primitive: &str, role: u64| {
            Raw::Array(vec![
                uuid(id),
                Raw::Text(name.to_owned()),
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text(primitive.to_owned())]),
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid(relation_id)),
            (Raw::Int(2.into()), Raw::Array(vec![uuid([0x01; 16])])),
            (
                Raw::Int(3.into()),
                Raw::Array(vec![
                    field([0x01; 16], "id", "Str", 0),
                    field([0x02; 16], "title", title_type, 1),
                ]),
            ),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid test descriptor")
    }

    fn identities_for(relations: &[[u8; 16]]) -> Vec<StoredObjectIdentity> {
        relations
            .iter()
            .enumerate()
            .map(|(index, id)| StoredObjectIdentity::new(*id, 1, format!("Table{index}"), false))
            .collect()
    }

    /// `orna init`, then `main.orna` and the metadata staged with plain Git.
    fn initialized_repository() -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().expect("create repository directory");
        let root = directory.path();
        let repository = crate::initialize_repository(root)
            .expect("orna init")
            .into_repository();
        crate::test_support::configure_fixture_git_identity(root);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(
            root.join("main.orna"),
            "pub table Table0(id: Str) { title: Str, }\n",
        )
        .expect("write main.orna");
        git(root, &["add", "main.orna", ".orna/database.orna"]);
        (directory, repository)
    }

    fn stage(
        repository: &Repository,
        relations: &[[u8; 16]],
    ) -> Result<StoreAdmission, RelationAdmissionError> {
        let staged = repository.capture_staged_candidate()?;
        let tables = relations
            .iter()
            .map(|id| StagedRelation::new(*id, table_descriptor(*id, "Str")))
            .collect::<Vec<_>>();
        repository.stage_store_admission(&staged, &tables, &identities_for(relations))
    }

    #[test]
    fn staging_bootstraps_the_store_and_its_identity_inventory() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let staged = repository
            .capture_staged_candidate()
            .expect("capture the index");
        assert!(staged.store_root().is_none(), "orna init writes no store");
        let music = relation(1);
        let identities = vec![
            StoredObjectIdentity::new(music, 1, "Music", false),
            StoredObjectIdentity::new([0x21; 16], 2, "Music.id", false),
            StoredObjectIdentity::new([0x22; 16], 2, "Music.title", false),
            StoredObjectIdentity::new([0x23; 16], 1, "Retired", true),
        ];
        let admission = repository
            .stage_store_admission(
                &staged,
                &[StagedRelation::new(music, table_descriptor(music, "Str"))],
                &identities,
            )
            .expect("stage the store");
        assert!(admission.changed());
        assert_eq!(admission.admitted(), &[music, OBJECT_IDENTITY_RELATION_ID]);
        git(root, &["commit", "--quiet", "-m", "initial source"]);

        // ORNA-META-001: tracked `.orna/` holds only the metadata and the store.
        assert_eq!(
            git(root, &["ls-tree", "--name-only", "HEAD", ".orna/"]),
            ".orna/database.orna\n.orna/store\n"
        );
        // Node metadata is materialized and payload chunks are skip-worktree,
        // so the staged store leaves no worktree difference behind.
        assert_eq!(git(root, &["status", "--porcelain"]), "");

        let head = repository.head().unwrap().expect("committed HEAD");
        let format = repository
            .open_pinned_format_context(head.as_str())
            .unwrap();
        let rows = format
            .load_row_map(music)
            .expect("the admitted relation resolves");
        assert_eq!(rows.version().row_count(), Some(0));
        let graph = format
            .open_native_graph(&rows)
            .expect("issue the relation graph");
        let scope = graph.open_read_scope().unwrap();
        assert_eq!(
            graph.read_schema_descriptor(&scope).unwrap(),
            table_descriptor(music, "Str")
        );
        let mut expected = identities;
        expected.sort_by_key(|identity| identity.row_key().expect("a valid identity key"));
        assert_eq!(
            repository.read_committed_object_identities(&head).unwrap(),
            Some(expected)
        );

        // A complete store is not staged again.
        assert!(!stage_again_with(&repository, music).changed());
    }

    fn stage_again_with(repository: &Repository, music: [u8; 16]) -> StoreAdmission {
        let staged = repository.capture_staged_candidate().unwrap();
        repository
            .stage_store_admission(
                &staged,
                &[StagedRelation::new(music, table_descriptor(music, "Str"))],
                &{
                    let mut identities = vec![
                        StoredObjectIdentity::new(music, 1, "Music", false),
                        StoredObjectIdentity::new([0x21; 16], 2, "Music.id", false),
                        StoredObjectIdentity::new([0x22; 16], 2, "Music.title", false),
                        StoredObjectIdentity::new([0x23; 16], 1, "Retired", true),
                    ];
                    identities.reverse();
                    identities
                },
            )
            .unwrap()
    }

    #[test]
    fn relation_inserts_match_the_canonical_complete_build() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let first = [relation(7)];
        stage(&repository, &first).expect("admit the first relation");
        git(root, &["commit", "--quiet", "-m", "first table"]);

        // 260 relations force a 256-entry cut, so the relation map is a branch.
        let all = (0..260).map(relation).collect::<Vec<_>>();
        let incremental = stage(&repository, &all).expect("insert the remaining relations");
        // The first relation and the inventory already exist, so 259 are new.
        assert_eq!(incremental.admitted().len(), 259);

        let mut writer = NodeWriter::new(&repository).unwrap();
        let tables = all
            .iter()
            .map(|id| StagedRelation::new(*id, table_descriptor(*id, "Str")))
            .collect::<Vec<_>>();
        let (entries, _) = admit_relations(
            &mut writer,
            Vec::new(),
            &BTreeMap::new(),
            &tables,
            Some(&identities_for(&all)),
        )
        .unwrap();
        let database_id = *repository
            .capture_staged_candidate()
            .unwrap()
            .database_id()
            .as_bytes();
        let complete = write_store_root(&mut writer, database_id, &entries).unwrap();
        assert_eq!(incremental.store_root(), &complete);

        let relations = NativeGraphContext::read_store_relations(
            &repository,
            complete.algorithm(),
            database_id,
            &complete,
        )
        .expect("read the branched relation map");
        assert_eq!(relations.len(), 261);
        assert!(
            relations
                .windows(2)
                .all(|pair| pair[0].relation_id < pair[1].relation_id)
        );
    }

    #[test]
    fn staging_refuses_schema_changes_orphans_and_duplicates() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let (music, video) = (relation(1), relation(2));
        stage(&repository, &[music, video]).expect("admit two relations");
        git(root, &["commit", "--quiet", "-m", "two tables"]);

        let staged = repository.capture_staged_candidate().unwrap();
        let identities = identities_for(&[music, video]);
        let refuse = |tables: &[StagedRelation]| {
            repository
                .stage_store_admission(&staged, tables, &identities)
                .expect_err("staging must refuse")
        };
        let current = |id| StagedRelation::new(id, table_descriptor(id, "Str"));
        assert!(matches!(
            refuse(&[StagedRelation::new(music, table_descriptor(music, "Int")), current(video)]),
            RelationAdmissionError::SchemaChanged { relation_id } if relation_id == music
        ));
        assert!(matches!(
            refuse(&[current(music)]),
            RelationAdmissionError::OrphanRelation { relation_id } if relation_id == video
        ));
        assert!(matches!(
            refuse(&[current(music), current(music), current(video)]),
            RelationAdmissionError::DuplicateRelation { relation_id } if relation_id == music
        ));
        assert!(matches!(
            refuse(&[
                current(music),
                current(video),
                StagedRelation::new(
                    OBJECT_IDENTITY_RELATION_ID,
                    table_descriptor(OBJECT_IDENTITY_RELATION_ID, "Str")
                ),
            ]),
            RelationAdmissionError::DuplicateRelation { .. }
        ));
        // A refused staging leaves the index exactly as captured.
        assert_eq!(git(root, &["status", "--porcelain"]), "");
    }

    #[test]
    fn staging_moves_the_legacy_identity_sidecar_into_the_store() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        fs::write(root.join(LEGACY_OBJECT_IDENTITY_SIDECAR_PATH), b"legacy").unwrap();
        git(root, &["add", LEGACY_OBJECT_IDENTITY_SIDECAR_PATH]);
        let staged = repository.capture_staged_candidate().unwrap();
        assert_eq!(
            repository
                .read_staged_legacy_identity_sidecar(&staged, 1 << 20)
                .unwrap(),
            Some(b"legacy".to_vec())
        );
        stage(&repository, &[relation(1)]).expect("stage the store");
        let restaged = repository.capture_staged_candidate().unwrap();
        assert_eq!(
            repository
                .read_staged_legacy_identity_sidecar(&restaged, 1 << 20)
                .unwrap(),
            None
        );
        assert_eq!(
            git(
                root,
                &["ls-files", "--", LEGACY_OBJECT_IDENTITY_SIDECAR_PATH]
            ),
            ""
        );
        // The worktree file is not deleted; it is merely no longer staged.
        assert!(root.join(LEGACY_OBJECT_IDENTITY_SIDECAR_PATH).exists());
    }

    /// The staged candidate is an unreachable private commit, so the
    /// inventory a staging wrote is read back before anything is committed,
    /// and a retried commit keeps the IDs it allocated.
    #[test]
    fn a_retried_staging_reads_the_inventory_it_staged() {
        let (_directory, repository) = initialized_repository();
        let unstaged = repository.capture_staged_candidate().unwrap();
        assert_eq!(
            repository.read_staged_object_identities(&unstaged).unwrap(),
            None
        );
        stage(&repository, &[relation(1)]).expect("stage the store");
        assert!(repository.head().unwrap().is_none(), "nothing is committed");
        let restaged = repository.capture_staged_candidate().unwrap();
        assert_eq!(
            repository.read_staged_object_identities(&restaged).unwrap(),
            Some(identities_for(&[relation(1)]))
        );
    }

    /// Deterministic bytes that cut Gear chunks at varied lengths.
    fn varied_bytes(length: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn schema_byte_index_matches_the_capture_writer() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let music = relation(1);
        stage(&repository, &[music]).expect("stage the store");
        git(root, &["commit", "--quiet", "-m", "initial source"]);
        let capability = repository
            .open_format_context()
            .unwrap()
            .capture_capability(music)
            .expect("issue capture authority");
        // Seventeen maximal chunks exceed the 16-entry anchor minimum; the
        // varied input cuts chunks at data-dependent lengths.
        for content in [vec![0u8; 17 << 20], varied_bytes(3 << 20)] {
            let candidate = capability
                .capture_blob_candidate(&content[..], content.len() as u64)
                .expect("capture the content");
            let binding = capability
                .accept_captured_blob(candidate, "application/octet-stream", None)
                .expect("protect the captured content");
            let listing = git(
                root,
                &["cat-file", "-p", &binding.descriptor_oid().to_hex()],
            );
            let data = listing
                .lines()
                .find_map(|line| line.strip_suffix("\tdata"))
                .and_then(|header| header.rsplit(' ').next())
                .expect("descriptor envelope names its data blob")
                .to_owned();
            let bytes = std::process::Command::new("git")
                .current_dir(root)
                .args(["cat-file", "blob", &data])
                .output()
                .expect("read the descriptor data")
                .stdout;
            let NodeData::BlobDescriptor { byte_root, .. } =
                NodeData::decode_canonical(&bytes, GitHashAlgorithm::Sha1).unwrap()
            else {
                panic!("the captured descriptor is a blob descriptor");
            };
            let mut writer = NodeWriter::new(&repository).unwrap();
            assert_eq!(write_byte_index(&mut writer, &content).unwrap(), byte_root);
        }
    }

    #[test]
    fn the_first_reviewed_commit_creates_the_unborn_branch() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        stage(&repository, &[relation(1)]).expect("stage the store");
        fs::write(root.join("unstaged.txt"), b"unstaged").unwrap();
        let branch = git(root, &["symbolic-ref", "HEAD"]);
        assert!(repository.head().unwrap().is_none(), "the branch is unborn");

        let review = repository
            .review_staged_candidate(
                Uuid::from_bytes([10; 16]),
                Uuid::from_bytes([11; 16]),
                3,
                RepositoryFormat::Format3,
                crate::RuntimeGeneration::new(4),
                vec![0x81, 0x00],
            )
            .expect("review the unborn branch's staged candidate");
        assert!(review.cwd.head().is_none());
        assert!(
            review
                .candidate_paths
                .iter()
                .any(|path| path == "main.orna")
        );
        let committed = repository
            .commit_reviewed_candidate(&review, "initial source")
            .expect("the first reviewed commit creates the branch");

        // "The first actual commit creates that branch ref": a root commit of
        // exactly the reviewed index tree, on the same symbolic branch.
        assert_eq!(repository.head().unwrap(), Some(committed.clone()));
        assert_eq!(git(root, &["symbolic-ref", "HEAD"]), branch);
        assert_eq!(
            git(
                root,
                &["rev-list", "--parents", "-n", "1", committed.as_str()]
            )
            .trim(),
            committed.as_str()
        );
        assert_eq!(
            git(
                root,
                &["rev-parse", &format!("{}^{{tree}}", committed.as_str())]
            )
            .trim(),
            review.index_tree.as_str()
        );
        // Unstaged work is not consumed.
        assert_eq!(git(root, &["status", "--porcelain"]), "?? unstaged.txt\n");
        // The recorded result answers a retry without a second commit.
        assert_eq!(
            repository
                .commit_reviewed_candidate(&review, "initial source")
                .unwrap(),
            committed
        );
        assert_eq!(git(root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    }

    fn review_and_commit(repository: &Repository, message: &str) -> GitCommitRef {
        let review = repository
            .review_staged_candidate(
                Uuid::from_bytes([10; 16]),
                Uuid::from_bytes([11; 16]),
                3,
                RepositoryFormat::Format3,
                crate::RuntimeGeneration::new(4),
                vec![0x81, 0x00],
            )
            .expect("review the staged candidate");
        repository
            .commit_reviewed_candidate(&review, message)
            .expect("commit the reviewed candidate")
    }

    #[test]
    fn a_later_reviewed_commit_binds_the_restaged_store() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        fs::write(root.join("notes.txt"), b"tracked").unwrap();
        git(root, &["add", "notes.txt"]);
        stage(&repository, &[relation(1)]).expect("stage the first store");
        let first = review_and_commit(&repository, "first");

        // A new relation plus a staged deletion whose worktree copy stays.
        git(root, &["rm", "--cached", "--quiet", "notes.txt"]);
        stage(&repository, &[relation(1), relation(2)]).expect("admit a second relation");
        let second = review_and_commit(&repository, "second");

        assert_eq!(
            git(root, &["rev-list", "--parents", "-n", "1", second.as_str()]).trim(),
            format!("{} {}", second.as_str(), first.as_str())
        );
        assert_eq!(
            repository.committed_store_relations(&second).unwrap(),
            BTreeSet::from([relation(1), relation(2), OBJECT_IDENTITY_RELATION_ID])
        );
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(git(root, &["ls-files", "--", "notes.txt"]), "");
        assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"tracked");
        assert_eq!(git(root, &["status", "--porcelain"]), "?? notes.txt\n");
    }

    fn review(repository: &Repository) -> crate::ReviewedCandidateReview {
        repository
            .review_staged_candidate(
                Uuid::from_bytes([10; 16]),
                Uuid::from_bytes([11; 16]),
                3,
                RepositoryFormat::Format3,
                crate::RuntimeGeneration::new(4),
                vec![0x81, 0x00],
            )
            .expect("review the staged candidate")
    }

    /// Every file below the store's worktree directory, for byte-exact
    /// comparison.
    fn store_worktree_files(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn walk(directory: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
            let Ok(entries) = fs::read_dir(directory) else {
                return;
            };
            for entry in entries {
                let path = entry.expect("read a store directory entry").path();
                if path.is_dir() {
                    walk(&path, files);
                } else {
                    let bytes = fs::read(&path).expect("read a store worktree file");
                    files.insert(path, bytes);
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&root.join(".orna/store"), &mut files);
        files
    }

    /// ORNA-PUB-005, PUB-3 step 3: the root commit and every object of its
    /// tree are flushed before the new branch ref names them. An object that
    /// cannot be flushed refuses the commit while the branch is still unborn.
    #[test]
    fn the_first_reviewed_commit_flushes_its_objects_before_creating_the_branch() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        fs::write(root.join("notes.txt"), b"tracked notes").unwrap();
        git(root, &["add", "notes.txt"]);
        stage(&repository, &[relation(1)]).expect("stage the store");
        let review = review(&repository);
        let blob = git(root, &["rev-parse", ":notes.txt"]).trim().to_owned();
        let loose = root.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
        let aside = root.join(".git/orna-test-notes-object");
        fs::rename(&loose, &aside).unwrap();
        // The object path no longer names a file that can be flushed.
        fs::create_dir(&loose).unwrap();

        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "initial source"),
            Err(RepositoryError::Graph(GraphError::DurabilityFailed))
        ));
        assert!(
            repository.head().unwrap().is_none(),
            "the branch stays unborn"
        );

        fs::remove_dir(&loose).unwrap();
        fs::rename(&aside, &loose).unwrap();
        let committed = repository
            .commit_reviewed_candidate(&review, "initial source")
            .expect("the flushed candidate creates the branch");
        assert_eq!(repository.head().unwrap(), Some(committed));
    }

    /// 22-publication recovery table, ORNA-PUB-007: a retained publication
    /// journal owns the index and the store worktree until its recovery
    /// completes, so staging refuses before changing either.
    #[test]
    fn staging_refuses_while_a_publication_recovery_is_retained() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        stage(&repository, &[relation(1)]).expect("stage the first store");
        let head = review_and_commit(&repository, "first");
        let index_before = repository.index_generation().unwrap();
        let managed = crate::ManagedPath::new("generated/row.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[crate::ManagedFileChange::new(
                    managed.clone(),
                    Some(b"candidate row\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let journal = crate::PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            index_before.tree().unwrap().clone(),
            [7; 16],
            vec![crate::PublicationJournalEntry::new(
                managed,
                None,
                Some(b"candidate row\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        let index = root.join(".git/index");
        let index_bytes = fs::read(&index).unwrap();
        let store_files = store_worktree_files(root);
        assert!(
            !store_files.is_empty(),
            "the committed store is materialized"
        );
        assert!(matches!(
            stage(&repository, &[relation(1), relation(2)]),
            Err(RelationAdmissionError::Repository(
                RepositoryError::PublicationPending
            ))
        ));
        assert_eq!(fs::read(&index).unwrap(), index_bytes);
        assert_eq!(store_worktree_files(root), store_files);
        assert_eq!(
            repository.read_publication_journal().unwrap(),
            Some(journal)
        );
    }

    /// ORNA-ADMIN-003: a relation that only a failed commit staged was never
    /// committed, so the next staging follows the staged source: the table
    /// may be dropped or its schema changed. Once committed, both are
    /// refused, and so is a staged store that lost a committed relation.
    #[test]
    fn staging_follows_the_source_for_relations_no_commit_received() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let (music, video) = (relation(1), relation(2));
        // An earlier `orna commit` staged both tables and then failed before
        // the branch moved.
        stage(&repository, &[music, video]).expect("stage two relations");
        assert!(repository.head().unwrap().is_none(), "nothing is committed");

        let edited = [StagedRelation::new(music, table_descriptor(music, "Int"))];
        let staged = repository.capture_staged_candidate().unwrap();
        let admission = repository
            .stage_store_admission(&staged, &edited, &identities_for(&[music]))
            .expect("uncommitted relations follow the staged source");
        let mut admitted = vec![music, OBJECT_IDENTITY_RELATION_ID];
        admitted.sort_unstable();
        assert_eq!(admission.admitted(), admitted.as_slice());
        let committed = review_and_commit(&repository, "first");
        assert_eq!(
            repository.committed_store_relations(&committed).unwrap(),
            BTreeSet::from([music, OBJECT_IDENTITY_RELATION_ID])
        );

        let staged = repository.capture_staged_candidate().unwrap();
        let original = [StagedRelation::new(music, table_descriptor(music, "Str"))];
        assert!(matches!(
            repository.stage_store_admission(&staged, &original, &identities_for(&[music])),
            Err(RelationAdmissionError::SchemaChanged { relation_id }) if relation_id == music
        ));
        assert!(matches!(
            repository.stage_store_admission(&staged, &[], &identities_for(&[music])),
            Err(RelationAdmissionError::OrphanRelation { relation_id }) if relation_id == music
        ));

        // The user's removal from the index, unmodified: skip-worktree payload
        // entries survive it, so the index keeps a partial store.
        git(root, &["rm", "--cached", "-r", "--quiet", ".orna/store"]);
        assert!(!git(root, &["ls-files", "--", ".orna/store"]).is_empty());
        let staged = repository.capture_staged_candidate().unwrap();
        assert!(matches!(
            repository.stage_store_admission(&staged, &edited, &identities_for(&[music])),
            Err(RelationAdmissionError::CommittedRelationUnstaged { relation_id })
                if relation_id == music
        ));
    }
}
