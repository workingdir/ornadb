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
//!
//! A second sealed relation, the allocator summary, records the automatic-key
//! watermark each snapshot knew (ORNA-AUTOID-005). It is historical only:
//! allocation reads the hidden allocator refs (ORNA-AUTOID-004), and checking
//! out an old summary rewinds nothing (ORNA-META-004).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    process::Stdio,
};

use num_bigint::BigInt;
use orna_value_v1::{Raw, SchemaDescriptor, Value, decode_canonical_storage_value};
use sha2::{Digest, Sha256};

use crate::{
    DatabaseId, FormatContextError, GitCommitRef, GitIndexLock, IndexGeneration, IndexTreeRef,
    HistoricalFormatContext,
    PrivateCommit, Repository, RepositoryError, RepositoryFormat, RepositoryFormatContext, Uuid,
    allocator::AllocatorError,
    bind_tree_entry_in_index,
    blob_store::GearChunker,
    native_graph::{
        ByteIndexEntry, GitHashAlgorithm, GraphError, IndexMaintenance, MAX_GRAPH_HEIGHT, MAX_REFS,
        NativeGraphContext, NativeNodeEnvelope, NativeObjectKind, NativeOid, NodeData,
        RowIndexMutation, StoreRelationEntry, hex_decode, hex_encode,
        sync_object_files,
    },
    overflow::{OverflowStore, OverflowWriter},
    row_store::{
        KeyRange, MAX_ROW_RANGE_LIMIT, RowDependency, RowEntry, RowMapSnapshot, RowValue, TypedKey,
    },
    schema_evolution::{EvolutionPlan, EvolutionRefusal, SchemaChanges},
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

/// Stable relation UUID of the sealed automatic-key allocator summary,
/// derived like [`OBJECT_IDENTITY_RELATION_ID`] from `sys.Allocator`.
///
/// Each committed snapshot records in it, per automatic-key table, the
/// allocator watermark known when the snapshot was made (ORNA-AUTOID-005).
/// It is a historical observation only: allocation reads the hidden ref
/// `refs/orna/ids/<table>`, and checking out a snapshot never rewinds it
/// (ORNA-META-004).
pub const ALLOCATOR_RELATION_ID: [u8; 16] = [
    0x68, 0x05, 0x1d, 0xd0, 0xc0, 0xf0, 0x8c, 0x8d, 0xa5, 0xb7, 0x03, 0x0c, 0x77, 0x36, 0x5f, 0x38,
];
/// Field UUIDs of the allocator summary, derived from `sys.Allocator.<field>`.
const ALLOCATOR_TABLE_FIELD: [u8; 16] = [
    0x55, 0xc3, 0xce, 0x50, 0x3c, 0xc4, 0x8e, 0x73, 0x81, 0xaa, 0x6c, 0xcc, 0x23, 0x41, 0xa4, 0xbd,
];
const ALLOCATOR_WATERMARK_FIELD: [u8; 16] = [
    0xbf, 0x18, 0x5f, 0xf3, 0x4f, 0x55, 0x81, 0x5f, 0x9f, 0x8e, 0xbe, 0x22, 0x36, 0x37, 0x02, 0x70,
];

/// Automatic-key allocator watermarks by table ObjectId.
pub type AllocatorWatermarks = BTreeMap<[u8; 16], BigInt>;

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
/// Field UUIDs of the rekey descriptor, derived like the relation UUID from
/// `orna.object-rekey.<field>`.
const REKEY_ORDINAL_FIELD: [u8; 16] = [
    0x0a, 0xa4, 0x2f, 0xc2, 0xc7, 0xeb, 0x8d, 0x3e, 0xa1, 0x50, 0xf0, 0xec, 0xe1, 0xdf, 0x2d, 0xd5,
];
const REKEY_OLD_KEY_FIELD: [u8; 16] = [
    0x57, 0x3e, 0xe3, 0x4b, 0x3c, 0x58, 0x84, 0xb5, 0xa8, 0xc2, 0x0f, 0x05, 0xbc, 0x79, 0x67, 0xb4,
];
const REKEY_NEW_KEY_FIELD: [u8; 16] = [
    0xb0, 0xfa, 0x34, 0xe6, 0x82, 0xc0, 0x87, 0xb9, 0x96, 0xcf, 0x6a, 0x10, 0x74, 0xe1, 0x27, 0xc2,
];
const REKEY_RELATION_FIELD: [u8; 16] = [
    0xe1, 0x13, 0x1f, 0x2c, 0xf6, 0x74, 0x82, 0x24, 0x96, 0x75, 0xdd, 0x1b, 0x86, 0xbf, 0x9f, 0x25,
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
    /// A committed relation would evolve to the staged schema, or be removed
    /// with its retired table, while unpublished runtime rows written under
    /// the committed schemas wait to be published; they would no longer fit
    /// the store they publish into.
    PendingRuntimeRows {
        relation_id: [u8; 16],
    },
    /// The staged source changes a committed relation's schema in a way its
    /// committed rows cannot follow (ORNA-EVOL-002).
    Evolution {
        relation_id: [u8; 16],
        refusal: EvolutionRefusal,
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
    /// An allocator summary row is not a table's watermark.
    InvalidAllocatorSummary,
    /// The ordinary index stages no `.orna/store`, so it has no identity
    /// inventory to change; the first `orna commit` creates one.
    NoStagedStore,
    /// A table's hidden allocator ref could not be read.
    Allocator(AllocatorError),
    /// A rekey log record is not a valid system relation entry.
    InvalidRekeyLog,
    /// A source file to restage is not a regular file the captured index
    /// tracks, or it names Orna metadata.
    SourceNotStaged {
        path: String,
    },
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
            Self::PendingRuntimeRows { relation_id } => write!(
                formatter,
                "relation {} would change while unpublished runtime rows wait to be published",
                Uuid::from_bytes(*relation_id)
            ),
            Self::Evolution {
                relation_id,
                refusal,
            } => write!(
                formatter,
                "relation {} cannot evolve to the staged schema: {refusal}",
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
            Self::InvalidAllocatorSummary => {
                formatter.write_str("an allocator summary record is invalid")
            }
            Self::NoStagedStore => formatter.write_str("the Git index stages no .orna/store"),
            Self::Allocator(error) => write!(formatter, "{error}"),
            Self::InvalidRekeyLog => formatter.write_str("an object rekey log record is invalid"),
            Self::SourceNotStaged { path } => write!(
                formatter,
                "{path} is not a source file the index tracks; stage it with `git add`"
            ),
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

impl From<AllocatorError> for RelationAdmissionError {
    fn from(error: AllocatorError) -> Self {
        Self::Allocator(error)
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

/// The sealed closed descriptor of the allocator summary relation. Its key
/// is the table ObjectId's hyphenated UUID text; its stored field is the
/// highest automatic ID reserved for that table.
pub fn allocator_descriptor() -> Result<SchemaDescriptor, RelationAdmissionError> {
    sealed_descriptor(
        ALLOCATOR_RELATION_ID,
        ALLOCATOR_TABLE_FIELD,
        &[
            (ALLOCATOR_TABLE_FIELD, "table", "Str", 0),
            (ALLOCATOR_WATERMARK_FIELD, "watermark", "Int", 1),
        ],
    )
    .ok_or(RelationAdmissionError::InvalidAllocatorSummary)
}

/// A sealed system relation's closed descriptor over primitive `fields`,
/// each a field UUID, name, primitive type and role (0 key, 1 stored), which
/// a closed descriptor lists in ascending field-UUID order.
fn sealed_descriptor(
    relation_id: [u8; 16],
    key: [u8; 16],
    fields: &[([u8; 16], &str, &str, u64)],
) -> Option<SchemaDescriptor> {
    let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
    let fields = fields
        .iter()
        .map(|(id, name, primitive, role)| {
            Raw::Array(vec![
                uuid(*id),
                Raw::Text((*name).to_owned()),
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text((*primitive).to_owned())]),
                Raw::Int((*role).into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        })
        .collect();
    SchemaDescriptor::new(Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid(relation_id)),
        (Raw::Int(2.into()), Raw::Array(vec![uuid(key)])),
        (Raw::Int(3.into()), Raw::Array(fields)),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .ok()
}

/// Whether `relation_id` names one of the store's sealed system relations
/// (the identity inventory, the allocator summary or the rekey log) rather
/// than a table.
pub fn is_system_relation(relation_id: &[u8; 16]) -> bool {
    *relation_id == OBJECT_IDENTITY_RELATION_ID
        || *relation_id == ALLOCATOR_RELATION_ID
        || *relation_id == OBJECT_REKEY_RELATION_ID
}

/// The allocator summary row of `table`: its key and stored fields.
fn allocator_summary_row(
    table: &[u8; 16],
    watermark: &BigInt,
) -> Result<(TypedKey, Vec<u8>), RelationAdmissionError> {
    let fields = Value::new(Raw::Array(vec![Raw::Int(watermark.clone())]))
        .and_then(|value| value.encode())
        .map_err(|_| RelationAdmissionError::InvalidAllocatorSummary)?;
    Ok((
        TypedKey::Text(Uuid::from_bytes(*table).hyphenated().to_string()),
        fields,
    ))
}

/// Reads one allocator summary row back, validated against `descriptor`.
fn decode_allocator_summary_row(
    descriptor: &SchemaDescriptor,
    key: &TypedKey,
    fields: &[u8],
) -> Result<([u8; 16], BigInt), RelationAdmissionError> {
    let invalid = || RelationAdmissionError::InvalidAllocatorSummary;
    let key_bytes = key.canonical_bytes().map_err(|_| invalid())?;
    descriptor
        .validate_stored_row(ALLOCATOR_RELATION_ID, &key_bytes, fields)
        .map_err(|_| invalid())?;
    let TypedKey::Text(text) = key else {
        return Err(invalid());
    };
    let table = Uuid::parse_str(text).map_err(|_| invalid())?;
    if table.hyphenated().to_string() != *text {
        return Err(invalid());
    }
    let Raw::Array(values) = decode_canonical_storage_value(fields).map_err(|_| invalid())? else {
        return Err(invalid());
    };
    match values.as_slice() {
        [Raw::Int(watermark)] if watermark.sign() != num_bigint::Sign::Minus => {
            Ok((*table.as_bytes(), watermark.clone()))
        }
        _ => Err(invalid()),
    }
}

/// The sealed closed descriptor of the rekey log relation. Its key is the
/// dense publication ordinal; its stored fields are the old canonical key,
/// the new canonical key and the relation UUID, in ascending field-UUID order.
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
                field(REKEY_NEW_KEY_FIELD, "new_key", "Str", 1),
                field(REKEY_RELATION_FIELD, "relation", "Uuid", 1),
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

    /// The stored fields in descriptor (field-UUID) order: `old_key` and
    /// `new_key` as their lowercase hex spellings, then `relation`.
    fn row_fields(&self) -> Result<Vec<u8>, RelationAdmissionError> {
        Value::new(Raw::Array(vec![
            Raw::Text(hex_encode(&self.old_key)),
            Raw::Text(hex_encode(&self.new_key)),
            uuid_value(self.relation_id),
        ]))
        .and_then(|value| value.encode())
        .map_err(|_| RelationAdmissionError::InvalidRekeyLog)
    }

    fn from_row(key: &TypedKey, fields: &[u8]) -> Result<Self, RelationAdmissionError> {
        // The writer emits the ordinal as `UInt`, the one spelling a
        // non-negative canonical Int key decodes to.
        let TypedKey::UInt(ordinal) = key else {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        };
        let Raw::Array(values) = decode_canonical_storage_value(fields)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?
        else {
            return Err(RelationAdmissionError::InvalidRekeyLog);
        };
        let [
            Raw::Text(old_key),
            Raw::Text(new_key),
            Raw::Tag(37, relation),
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
            ordinal: *ordinal,
            relation_id,
            old_key,
            new_key,
        })
    }
}

/// Decodes one rekey log address: the canonical lowercase hex spelling
/// `hex_encode` writes, naming a canonical ORP key.
fn canonical_key_from_hex(text: &str) -> Result<Vec<u8>, RelationAdmissionError> {
    let key = hex_decode(text).ok_or(RelationAdmissionError::InvalidRekeyLog)?;
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
            // A long address is stored through the overflow graph, as its own
            // field value or with the whole row, so the tuple is read back and
            // expanded through the graph rather than taken from the page.
            let fields = graph.stored_row_fields(row, &scope)?;
            let key = row
                .key()
                .canonical_bytes()
                .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
            descriptor
                .validate_stored_row(OBJECT_REKEY_RELATION_ID, &key, &fields)
                .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
            let rekey = CommittedRekey::from_row(row.key(), &fields)?;
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

/// Writes the rekey log's schema node and its complete row map. Each row is
/// lowered like any other row (store-format-3 §5): an address too long to
/// stay inline is lowered to its own kind-3 node first, and the row entry
/// itself only when it still exceeds the inline limits.
fn write_rekey_relation(
    writer: &mut NodeWriter<'_>,
    log: &[CommittedRekey],
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let descriptor = object_rekey_descriptor()?;
    let (schema_oid, schema_digest) = write_schema_node(writer, &descriptor)?;
    let mut rows = Vec::with_capacity(log.len());
    let mut overflow = OverflowWriter::new(&mut *writer);
    for rekey in log {
        let key = rekey.row_key();
        let key_bytes = key
            .canonical_bytes()
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        let fields = rekey.row_fields()?;
        descriptor
            .validate_stored_row(OBJECT_REKEY_RELATION_ID, &key_bytes, &fields)
            .map_err(|_| RelationAdmissionError::InvalidRekeyLog)?;
        rows.push(RowEntry::new(key, overflow.lower_row(&fields)?));
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

/// One source table the staged candidate admits: its stable relation UUID,
/// its closed storage descriptor, whether its key is the implicit automatic
/// `id` and, for schema evolution, the stored value of each closed constant
/// insert-time default by field ObjectId.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRelation {
    relation_id: [u8; 16],
    descriptor: SchemaDescriptor,
    automatic_key: bool,
    introductions: BTreeMap<[u8; 16], Raw>,
}

impl StagedRelation {
    pub fn new(relation_id: [u8; 16], descriptor: SchemaDescriptor) -> Self {
        Self {
            relation_id,
            descriptor,
            automatic_key: false,
            introductions: BTreeMap::new(),
        }
    }

    /// Marks the table's key as the implicit automatic `id`
    /// (ORNA-AUTOID-001), so the staged snapshot records the table's
    /// allocator watermark (ORNA-AUTOID-005).
    #[must_use]
    pub fn with_automatic_key(mut self) -> Self {
        self.automatic_key = true;
        self
    }

    /// The stored values of the table's closed constant insert-time
    /// defaults, keyed by field ObjectId. When schema evolution introduces
    /// such a required field, each committed row receives this value, frozen
    /// at the introducing commit (ORNA-FIELD-003, ORNA-EVOL-009).
    #[must_use]
    pub fn with_introductions(mut self, introductions: BTreeMap<[u8; 16], Raw>) -> Self {
        self.introductions = introductions;
        self
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }

    pub const fn automatic_key(&self) -> bool {
        self.automatic_key
    }
}

/// Whether a staging may change the relations the staged store already
/// commits: evolve one to the staged schema, or remove the relation of a
/// retired table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommittedRelationChanges {
    Allowed,
    /// Unpublished runtime rows were written under the committed schemas, so
    /// a changed relation would strand them.
    PendingRuntimeRows,
}

/// A committed relation whose rows this staging rewrote to the staged schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvolvedRelation {
    relation_id: [u8; 16],
    changes: SchemaChanges,
    rows: u64,
}

impl EvolvedRelation {
    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn changes(&self) -> &SchemaChanges {
        &self.changes
    }

    /// The committed rows rewritten to the staged schema.
    pub const fn rows(&self) -> u64 {
        self.rows
    }
}

/// A committed relation this staging removed because its table's identity
/// retired from the staged source (ORNA-OBJECT-004).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovedRelation {
    relation_id: [u8; 16],
    rows: u64,
}

impl RemovedRelation {
    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    /// The committed rows that leave the staged store with the relation.
    pub const fn rows(&self) -> u64 {
        self.rows
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
    evolved: Vec<EvolvedRelation>,
    removed: Vec<RemovedRelation>,
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

    /// Committed relations this staging evolved, in relation-UUID order.
    pub fn evolved(&self) -> &[EvolvedRelation] {
        &self.evolved
    }

    /// Committed relations this staging removed, in relation-UUID order.
    pub fn removed(&self) -> &[RemovedRelation] {
        &self.removed
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
    /// node, one empty leaf and a canonical relation-map insert. A table whose
    /// relation exists under another descriptor evolves: every committed row
    /// is rewritten to the staged schema and admitted against it, and the
    /// relation gets the new schema node and primary map (ORNA-EVOL-002). A
    /// committed relation whose table retired from the staged source leaves
    /// the store with its rows (ORNA-OBJECT-004). Neither change is made
    /// while `changes` reports unpublished runtime rows. The identity
    /// inventory relation is rewritten from `identities`, and the legacy
    /// identity sidecar leaves the index. The allocator summary records the
    /// current watermark of every automatic-key table, zero before its first
    /// reservation (ORNA-AUTOID-005); a store with no such table has none.
    /// The new store is bound at `.orna/store` in the ordinary index under
    /// Git's index lock, its node metadata is materialized in the worktree
    /// like a publication's, and every other staged or unstaged path is
    /// preserved.
    pub fn stage_store_admission(
        &self,
        staged: &StagedCandidate,
        relations: &[StagedRelation],
        identities: &[StoredObjectIdentity],
        changes: CommittedRelationChanges,
    ) -> Result<StoreAdmission, RelationAdmissionError> {
        let database_id = *staged.database_id.as_bytes();
        let mut watermarks = AllocatorWatermarks::new();
        for relation in relations.iter().filter(|relation| relation.automatic_key) {
            let watermark = self
                .automatic_id_watermark(&relation.relation_id)?
                .unwrap_or_default();
            watermarks.insert(relation.relation_id, watermark);
        }
        let mut writer = NodeWriter::new(self)?;
        let committed = self.committed_schema_nodes(staged)?;
        let base = self.staged_store_entries(&writer, staged, &committed)?;
        let evolution_source = staged
            .store_root
            .as_ref()
            .map(|_| self.open_private_candidate_format_context(&staged.candidate))
            .transpose()?;
        let admitted = admit_relations(
            &mut writer,
            base,
            &committed,
            evolution_source.as_ref(),
            relations,
            identities,
            &watermarks,
            changes,
        )?;
        let store_root = write_store_root(&mut writer, database_id, &admitted.entries)?;
        self.install_staged_store_root(&writer, staged, store_root, admitted, &[])
    }

    /// Restages only the identity inventory of the staged store, as an
    /// explicit semantic rename (`orna mv`) does: every table relation, its
    /// schema node and its rows are kept exactly, and the inventory relation
    /// is rewritten from `identities` (ORNA-OBJECT-003, ORNA-META-001). Each
    /// table relation must stay owned by an active identity, so a rename
    /// cannot orphan committed rows. A staged index without a store gets one
    /// holding only the inventory; one whose `HEAD` commits relations it
    /// lacks is refused as unstaged.
    ///
    /// `sources` replaces the staged bytes of source files the captured index
    /// already tracks, keeping each file's mode, so the renamed declaration
    /// is staged with the inventory that names it, as `git mv` stages a
    /// move. Every other staged and unstaged path is kept. The store and the
    /// sources are installed as one index update under the same guards as
    /// [`Self::stage_store_admission`]: the captured index and `HEAD` must be
    /// unchanged, and the legacy identity sidecar leaves the index.
    pub fn stage_object_identities(
        &self,
        staged: &StagedCandidate,
        identities: &[StoredObjectIdentity],
        sources: &[(&str, &[u8])],
    ) -> Result<StoreAdmission, RelationAdmissionError> {
        let database_id = *staged.database_id.as_bytes();
        let mut writer = NodeWriter::new(self)?;
        let committed = self.committed_schema_nodes(staged)?;
        let mut entries = self
            .staged_store_entries(&writer, staged, &committed)?
            .into_iter()
            .map(|entry| (entry.relation_id, entry))
            .collect::<BTreeMap<_, _>>();
        if let Some(relation_id) = committed
            .keys()
            .find(|relation_id| !entries.contains_key(*relation_id))
        {
            return Err(RelationAdmissionError::CommittedRelationUnstaged {
                relation_id: *relation_id,
            });
        }
        let owners = identities
            .iter()
            .filter(|identity| !identity.retired)
            .map(|identity| identity.id)
            .collect::<BTreeSet<_>>();
        if let Some(relation_id) = entries.keys().find(|relation_id| {
            !is_system_relation(relation_id) && !owners.contains(*relation_id)
        }) {
            return Err(RelationAdmissionError::OrphanRelation {
                relation_id: *relation_id,
            });
        }
        let mut blobs = Vec::with_capacity(sources.len());
        for (path, bytes) in sources {
            let path = crate::ManagedPath::new(path)?;
            if path.as_path().starts_with(".orna") {
                return Err(RelationAdmissionError::SourceNotStaged {
                    path: path.as_path().display().to_string(),
                });
            }
            blobs.push((path, writer.blob(bytes)?));
        }
        entries.insert(
            OBJECT_IDENTITY_RELATION_ID,
            write_identity_relation(&mut writer, identities)?,
        );
        let created = if committed.contains_key(&OBJECT_IDENTITY_RELATION_ID) {
            Vec::new()
        } else {
            vec![OBJECT_IDENTITY_RELATION_ID]
        };
        let entries = entries.into_values().collect::<Vec<_>>();
        let store_root = write_store_root(&mut writer, database_id, &entries)?;
        let admitted = AdmittedRelations {
            entries,
            created,
            evolved: Vec::new(),
            removed: Vec::new(),
        };
        self.install_staged_store_root(&writer, staged, store_root, admitted, &blobs)
    }

    /// The schema node of each relation `HEAD`'s store commits; empty on an
    /// unborn branch, which commits no relation.
    fn committed_schema_nodes(
        &self,
        staged: &StagedCandidate,
    ) -> Result<BTreeMap<[u8; 16], NativeOid>, RelationAdmissionError> {
        let Some(head) = staged.index.head() else {
            return Ok(BTreeMap::new());
        };
        Ok(self
            .store_entries_in(head.as_str(), *staged.database_id.as_bytes())?
            .into_iter()
            .map(|entry| (entry.relation_id, entry.schema_oid))
            .collect())
    }

    /// Binds `store_root` at `.orna/store` in the ordinary index, with the
    /// `sources` blobs at their paths, unless the index already stages
    /// exactly that store, no source change and no legacy sidecar.
    fn install_staged_store_root(
        &self,
        writer: &NodeWriter<'_>,
        staged: &StagedCandidate,
        store_root: NativeOid,
        admitted: AdmittedRelations,
        sources: &[(crate::ManagedPath, NativeOid)],
    ) -> Result<StoreAdmission, RelationAdmissionError> {
        let sidecar_staged = self.index_tracks(None, LEGACY_OBJECT_IDENTITY_SIDECAR_PATH)?;
        let changed = staged.store_root.as_ref() != Some(&store_root)
            || sidecar_staged
            || !sources.is_empty();
        if changed {
            writer.sync()?;
            self.install_staged_store(staged, &store_root, sources)?;
        }
        Ok(StoreAdmission {
            store_root,
            admitted: admitted.created,
            evolved: admitted.evolved,
            removed: admitted.removed,
            changed,
        })
    }

    /// The validated relation entries of the staged store, empty when the
    /// index has none. `git rm --cached` leaves skip-worktree payload entries
    /// in the index, so the store tree survives while some node data does
    /// not; a committed relation whose node is absent is unstaged, not a
    /// corrupt store (ORNA-STAGE-004 refuses the candidate).
    fn staged_store_entries(
        &self,
        writer: &NodeWriter<'_>,
        staged: &StagedCandidate,
        committed: &BTreeMap<[u8; 16], NativeOid>,
    ) -> Result<Vec<StoreRelationEntry>, RelationAdmissionError> {
        let Some(root) = staged.store_root.as_ref() else {
            return Ok(Vec::new());
        };
        NativeGraphContext::read_store_relations(
            self,
            writer.algorithm,
            *staged.database_id.as_bytes(),
            root,
        )
        .map_err(|error| match (error, committed.keys().next()) {
            (GraphError::MissingNodeData, Some(relation_id)) => {
                RelationAdmissionError::CommittedRelationUnstaged {
                    relation_id: *relation_id,
                }
            }
            (error, _) => error.into(),
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

    /// The allocator watermark of each automatic-key table that a format-3
    /// commit recorded (ORNA-AUTOID-005), by table ObjectId. It is a
    /// historical observation: allocation reads the hidden allocator refs,
    /// which no checkout rewinds (ORNA-META-004). `Ok(None)` when the
    /// snapshot has no allocator summary: it has no automatic-key table, or
    /// its store predates the summary.
    pub fn read_committed_allocator_watermarks(
        &self,
        commit: &GitCommitRef,
    ) -> Result<Option<AllocatorWatermarks>, RelationAdmissionError> {
        let Some((_, graph)) = self.committed_allocator_summary(commit)? else {
            return Ok(None);
        };
        let descriptor = allocator_descriptor()?;
        let mut watermarks = AllocatorWatermarks::new();
        for (key, fields) in graph.committed_rows_with_fields(&graph.open_read_scope()?)? {
            let (table, watermark) = decode_allocator_summary_row(&descriptor, &key, &fields)?;
            watermarks.insert(table, watermark);
        }
        Ok(Some(watermarks))
    }

    /// The allocator summary change a publication on `base` makes: every
    /// summary row whose table's allocator ref advanced since `base` was
    /// committed is raised to the ref's watermark, so the publication commit
    /// records the watermark known when it was made (ORNA-AUTOID-005).
    /// `Ok(None)` when `base` has no summary or every recorded watermark is
    /// current. The row map and maintenance derive from `base`'s exact store
    /// root, as [`NativeGraphContext::maintain_relations_rows`] requires.
    pub fn allocator_summary_maintenance(
        &self,
        base: &GitCommitRef,
    ) -> Result<Option<(RowMapSnapshot, IndexMaintenance)>, RelationAdmissionError> {
        let Some((rows, graph)) = self.committed_allocator_summary(base)? else {
            return Ok(None);
        };
        let descriptor = allocator_descriptor()?;
        let mut operations = Vec::new();
        for (key, fields) in graph.committed_rows_with_fields(&graph.open_read_scope()?)? {
            let (table, recorded) = decode_allocator_summary_row(&descriptor, &key, &fields)?;
            let Some(current) = self.automatic_id_watermark(&table)? else {
                continue;
            };
            if current <= recorded {
                continue;
            }
            let (key, fields) = allocator_summary_row(&table, &current)?;
            let key = key
                .canonical_bytes()
                .map_err(|_| RelationAdmissionError::InvalidAllocatorSummary)?;
            operations.push(RowIndexMutation::delete_canonical(&key)?);
            operations.push(RowIndexMutation::insert_canonical(&key, &fields)?);
        }
        if operations.is_empty() {
            return Ok(None);
        }
        let maintenance = graph.maintain_row_index(&operations, &graph.open_read_scope()?)?;
        Ok(Some((rows, maintenance)))
    }

    /// The allocator summary relation of a format-3 commit's store, with its
    /// sealed descriptor verified, or `None` when the store has none.
    fn committed_allocator_summary(
        &self,
        commit: &GitCommitRef,
    ) -> Result<Option<(RowMapSnapshot, NativeGraphContext)>, RelationAdmissionError> {
        let format = self.open_pinned_format_context(commit.as_str())?;
        if format.repository_format() != RepositoryFormat::Format3
            || !self
                .store_relations_in(commit.as_str(), *format.require_database_id()?.as_bytes())?
                .contains(&ALLOCATOR_RELATION_ID)
        {
            return Ok(None);
        }
        let rows = format.load_row_map(ALLOCATOR_RELATION_ID)?;
        let graph = format.open_native_graph(&rows)?;
        if graph.read_schema_descriptor(&graph.open_read_scope()?)? != allocator_descriptor()? {
            return Err(RelationAdmissionError::InvalidAllocatorSummary);
        }
        Ok(Some((rows, graph)))
    }

    /// Reads the identity inventory the staged candidate's store carries:
    /// the committed one, or one an earlier staging wrote, so a retried
    /// commit keeps its IDs. `Ok(None)` when the staged store has none.
    pub fn read_staged_object_identities(
        &self,
        staged: &StagedCandidate,
    ) -> Result<Option<Vec<StoredObjectIdentity>>, RelationAdmissionError> {
        self.read_private_candidate_object_identities(&staged.candidate)
    }

    /// The identity inventory a private candidate commit's store carries. The
    /// commit gate reads it before publication, so table identities resolve
    /// against exactly the serialized candidate (ORNA-ASSERT-046).
    pub fn read_private_candidate_object_identities(
        &self,
        candidate: &PrivateCommit,
    ) -> Result<Option<Vec<StoredObjectIdentity>>, RelationAdmissionError> {
        let format = self.open_private_candidate_format_context(candidate)?;
        self.read_object_identities_in(&format, candidate.tree().as_str())
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
        self.read_private_candidate_legacy_identity_sidecar(&staged.candidate, max_bytes)
    }

    /// The legacy identity sidecar a private candidate commit's tree tracks.
    pub fn read_private_candidate_legacy_identity_sidecar(
        &self,
        candidate: &PrivateCommit,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RelationAdmissionError> {
        let tree = candidate.tree().as_str();
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
        let mut command = self.private_commit_command();
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

    /// Whether the ordinary index stages anything that can hold an object
    /// identity inventory: a `.orna/store` tree or the legacy sidecar. An
    /// index of a database whose first commit is not yet staged has neither,
    /// so no object has an identity yet.
    pub fn index_stages_object_identities(&self) -> Result<bool, RelationAdmissionError> {
        let mut command = self.command();
        command.args(["ls-files", "-z", "--", STORE_PATH]);
        let store_prefix = format!("{STORE_PATH}/");
        let stages_store = self
            .run(command)?
            .stdout
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(store_prefix.as_bytes()));
        Ok(stages_store || self.index_tracks(None, LEGACY_OBJECT_IDENTITY_SIDECAR_PATH)?)
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
    /// and each `sources` path's staged blob, and drops the legacy identity
    /// sidecar, while holding the coordination lock and Git's own
    /// `index.lock`. A retained publication or checkout recovery record owns
    /// the index and store worktree until its recovery completes, so staging
    /// refuses before touching either (22-publication recovery table,
    /// ORNA-PUB-007; CHECKOUT-3 step 7). The captured index and `HEAD` must be
    /// unchanged, and the new index is installed by an atomic rename. If the
    /// index cannot be installed after the store worktree moved, the worktree
    /// is moved back to the captured store.
    fn install_staged_store(
        &self,
        staged: &StagedCandidate,
        store_root: &NativeOid,
        sources: &[(crate::ManagedPath, NativeOid)],
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
            for (path, blob) in sources {
                self.replace_staged_blob(&candidate_index, path, blob)?;
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

    /// Points the stage-0 entry of `path` in `index` at `blob`, keeping its
    /// file mode. The path must already be a tracked regular file, so a
    /// rename never adds, removes or retypes a staged path.
    fn replace_staged_blob(
        &self,
        index: &Path,
        path: &crate::ManagedPath,
        blob: &NativeOid,
    ) -> Result<(), RelationAdmissionError> {
        let not_staged = || RelationAdmissionError::SourceNotStaged {
            path: path.as_path().display().to_string(),
        };
        let text = path.as_path().to_str().ok_or_else(not_staged)?;
        let mut list = self.command();
        list.env("GIT_INDEX_FILE", index)
            .args(["ls-files", "--stage", "-z", "--"])
            .arg(path.as_path());
        let listed = self.run(list)?.stdout;
        let mut entries = listed.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
        let entry = entries.next().ok_or_else(not_staged)?;
        if entries.next().is_some() {
            return Err(not_staged());
        }
        let entry = std::str::from_utf8(entry).map_err(|_| not_staged())?;
        let (header, listed_path) = entry.split_once('\t').ok_or_else(not_staged)?;
        let mut fields = header.split(' ');
        let (Some(mode @ ("100644" | "100755")), Some(_), Some("0"), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(not_staged());
        };
        if listed_path != text {
            return Err(not_staged());
        }
        let mut update = self.command();
        update
            .env("GIT_INDEX_FILE", index)
            .args(["update-index", "--cacheinfo"])
            .arg(format!("{mode},{},{text}", blob.to_hex()));
        self.run(update)?;
        Ok(())
    }
}

/// The relation entries of a staged store and how they changed.
struct AdmittedRelations {
    /// Every relation entry, in relation-UUID order.
    entries: Vec<StoreRelationEntry>,
    /// Relations created by this staging, in relation-UUID order.
    created: Vec<[u8; 16]>,
    evolved: Vec<EvolvedRelation>,
    removed: Vec<RemovedRelation>,
}

/// Merges `relations` into the validated `base` relation entries of the
/// staged store. `committed` maps each relation `HEAD` commits to its schema
/// node. A committed relation keeps its rows and evolves when its descriptor
/// changes (ORNA-EVOL-002), reading them from `evolution_source`; it cannot
/// be unstaged. Any other relation is admitted by this commit: a new one, or
/// one only an earlier staging created, gets its schema node and one empty
/// leaf, and an uncommitted relation the staged source no longer declares is
/// dropped; either is refused when the relation holds rows. When a committed
/// relation's table retires it leaves the store with its rows (ORNA-OBJECT-004).
fn admit_relations(
    writer: &mut NodeWriter<'_>,
    base: Vec<StoreRelationEntry>,
    committed: &BTreeMap<[u8; 16], NativeOid>,
    evolution_source: Option<&RepositoryFormatContext>,
    relations: &[StagedRelation],
    identities: &[StoredObjectIdentity],
    watermarks: &AllocatorWatermarks,
    changes: CommittedRelationChanges,
) -> Result<AdmittedRelations, RelationAdmissionError> {
    let mut entries = base
        .into_iter()
        .map(|entry| (entry.relation_id, entry))
        .collect::<BTreeMap<_, _>>();
    let mut staged = BTreeSet::new();
    let mut created = Vec::new();
    let mut evolved = Vec::new();
    let allow = |relation_id: [u8; 16]| match changes {
        CommittedRelationChanges::Allowed => Ok(()),
        CommittedRelationChanges::PendingRuntimeRows => {
            Err(RelationAdmissionError::PendingRuntimeRows { relation_id })
        }
    };
    let mut removed = Vec::new();
    for relation in relations {
        let relation_id = relation.relation_id;
        if is_system_relation(&relation_id) || !staged.insert(relation_id) {
            return Err(RelationAdmissionError::DuplicateRelation { relation_id });
        }
        let (schema_oid, schema_digest) = write_schema_node(writer, &relation.descriptor)?;
        let existing = entries.get(&relation_id).cloned();
        if committed.contains_key(&relation_id) && existing.is_none() {
            return Err(RelationAdmissionError::CommittedRelationUnstaged { relation_id });
        }
        match existing {
            Some(existing) if existing.schema_oid == schema_oid => {}
            Some(existing) if committed.contains_key(&relation_id) => {
                allow(relation_id)?;
                let source = evolution_source.ok_or(RelationAdmissionError::InvalidStoreEntry)?;
                let (entry, schema_changes) = evolve_relation(
                    writer,
                    source,
                    relation,
                    &existing,
                    schema_oid,
                    schema_digest,
                )?;
                evolved.push(EvolvedRelation {
                    relation_id,
                    changes: schema_changes,
                    rows: entry.row_count,
                });
                entries.insert(relation_id, entry);
            }
            Some(existing) if existing.row_count != 0 => {
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
                created.push(relation_id);
            }
        }
    }
    let retired = entries
        .keys()
        .filter(|id| !is_system_relation(id) && !staged.contains(*id))
        .copied()
        .collect::<Vec<_>>();
    for relation_id in retired {
        let uncommitted_rows = !committed.contains_key(&relation_id)
            && entries
                .get(&relation_id)
                .is_some_and(|entry| entry.row_count != 0);
        if uncommitted_rows {
            return Err(RelationAdmissionError::UncommittedRelationRows { relation_id });
        }
        allow(relation_id)?;
        if let Some(entry) = entries.remove(&relation_id) {
            removed.push(RemovedRelation {
                relation_id,
                rows: entry.row_count,
            });
        }
    }
    let entry = write_identity_relation(writer, identities)?;
    if entries.insert(OBJECT_IDENTITY_RELATION_ID, entry).is_none() {
        created.push(OBJECT_IDENTITY_RELATION_ID);
    }
    if watermarks.is_empty() {
        entries.remove(&ALLOCATOR_RELATION_ID);
    } else {
        let entry = write_allocator_relation(writer, watermarks)?;
        if entries.insert(ALLOCATOR_RELATION_ID, entry).is_none() {
            created.push(ALLOCATOR_RELATION_ID);
        }
    }
    created.sort_unstable();
    Ok(AdmittedRelations {
        entries: entries.into_values().collect(),
        created,
        evolved,
        removed,
    })
}

/// Rewrites every committed row of `existing` to the staged schema and
/// builds the relation's new primary map under the new schema digest.
fn evolve_relation(
    writer: &mut NodeWriter<'_>,
    committed: &RepositoryFormatContext,
    relation: &StagedRelation,
    existing: &StoreRelationEntry,
    schema_oid: NativeOid,
    schema_digest: [u8; 32],
) -> Result<(StoreRelationEntry, SchemaChanges), RelationAdmissionError> {
    let relation_id = relation.relation_id;
    let refused = |refusal| RelationAdmissionError::Evolution {
        relation_id,
        refusal,
    };
    if existing.secondary_root.is_some() {
        return Err(refused(EvolutionRefusal::IndexedRelation));
    }
    let rows = committed.load_row_map(relation_id)?;
    let graph = committed.open_native_graph(&rows)?;
    let scope = graph.open_read_scope()?;
    let old = graph.read_schema_descriptor(&scope)?;
    let plan = EvolutionPlan::new(
        relation_id,
        &old,
        &relation.descriptor,
        existing.row_count,
        &relation.introductions,
    )
    .map_err(refused)?;
    let committed_rows = graph.committed_rows_with_fields(&scope)?;
    let mut rewritten = Vec::with_capacity(committed_rows.len());
    for (key, fields) in committed_rows {
        let canonical_key = key
            .canonical_bytes()
            .map_err(|_| GraphError::NonCanonicalData)?;
        let fields = plan.rewrite_row(&canonical_key, &fields).map_err(refused)?;
        let value = RowValue::decode_canonical_fields(fields)
            .map_err(|_| refused(EvolutionRefusal::RowTooLarge))?;
        rewritten.push(RowEntry::new(key, value));
    }
    let primary_root = NativeGraphContext::build_canonical_ordered_map(
        &rows_domain(&relation_id, &schema_digest),
        &rewritten,
        &mut |node| writer.node(node),
    )?;
    Ok((
        StoreRelationEntry {
            relation_id,
            schema_oid,
            primary_root,
            secondary_root: None,
            row_count: rewritten.len() as u64,
        },
        plan.changes().clone(),
    ))
}

/// Writes the inventory relation's schema node and its complete row map.
fn write_identity_relation(
    writer: &mut NodeWriter<'_>,
    identities: &[StoredObjectIdentity],
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let descriptor = object_identity_descriptor()?;
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
        rows.push((key, fields));
    }
    write_sealed_relation(
        writer,
        OBJECT_IDENTITY_RELATION_ID,
        &descriptor,
        rows,
        || RelationAdmissionError::InvalidIdentity,
    )
}

/// Writes the allocator summary's schema node and its complete row map.
fn write_allocator_relation(
    writer: &mut NodeWriter<'_>,
    watermarks: &AllocatorWatermarks,
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let rows = watermarks
        .iter()
        .map(|(table, watermark)| allocator_summary_row(table, watermark))
        .collect::<Result<Vec<_>, _>>()?;
    write_sealed_relation(
        writer,
        ALLOCATOR_RELATION_ID,
        &allocator_descriptor()?,
        rows,
        || RelationAdmissionError::InvalidAllocatorSummary,
    )
}

/// Writes a sealed relation's schema node and the complete row map of
/// `rows`, each validated against the relation's sealed `descriptor`.
fn write_sealed_relation(
    writer: &mut NodeWriter<'_>,
    relation_id: [u8; 16],
    descriptor: &SchemaDescriptor,
    rows: Vec<(TypedKey, Vec<u8>)>,
    invalid: fn() -> RelationAdmissionError,
) -> Result<StoreRelationEntry, RelationAdmissionError> {
    let (schema_oid, schema_digest) = write_schema_node(writer, descriptor)?;
    let mut entries = Vec::with_capacity(rows.len());
    for (key, fields) in rows {
        let key_bytes = key.canonical_bytes().map_err(|_| invalid())?;
        descriptor
            .validate_stored_row(relation_id, &key_bytes, &fields)
            .map_err(|_| invalid())?;
        let value = RowValue::decode_canonical_fields(fields).map_err(|_| invalid())?;
        entries.push(RowEntry::new(key, value));
    }
    entries.sort_by(|left, right| left.key().cmp(right.key()));
    let primary_root = NativeGraphContext::build_canonical_ordered_map(
        &rows_domain(&relation_id, &schema_digest),
        &entries,
        &mut |node| writer.node(node),
    )?;
    Ok(StoreRelationEntry {
        relation_id,
        schema_oid,
        primary_root,
        secondary_root: None,
        row_count: entries.len() as u64,
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
/// index. The digest is the format-3 schema identity, `orna.schema.v3` over
/// the encoded descriptor (store-format-3 section 5), which every rows
/// domain of the relation binds.
fn write_schema_node(
    writer: &mut NodeWriter<'_>,
    descriptor: &SchemaDescriptor,
) -> Result<(NativeOid, [u8; 32]), RelationAdmissionError> {
    let bytes = descriptor
        .encode()
        .map_err(|_| GraphError::NonCanonicalData)?;
    let schema_digest = crate::native_graph::schema_identity_v3(&bytes);
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

impl OverflowStore for NodeWriter<'_> {
    fn oid_width(&self) -> usize {
        self.algorithm.width()
    }

    fn write_node(&mut self, node: &NodeData) -> Result<NativeOid, GraphError> {
        self.node(node)
    }

    fn write_byte_index(&mut self, bytes: &[u8]) -> Result<NativeOid, GraphError> {
        write_byte_index(self, bytes)?.ok_or(GraphError::InvalidByteIndex)
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
        assert_eq!(ALLOCATOR_RELATION_ID, system_uuid("sys.Allocator"));
        assert_eq!(ALLOCATOR_TABLE_FIELD, system_uuid("sys.Allocator.table"));
        assert_eq!(
            ALLOCATOR_WATERMARK_FIELD,
            system_uuid("sys.Allocator.watermark")
        );
        assert_eq!(OBJECT_REKEY_RELATION_ID, system_uuid("orna.object-rekey"));
        assert_eq!(
            REKEY_ORDINAL_FIELD,
            system_uuid("orna.object-rekey.ordinal")
        );
        assert_eq!(
            REKEY_OLD_KEY_FIELD,
            system_uuid("orna.object-rekey.old_key")
        );
        assert_eq!(
            REKEY_NEW_KEY_FIELD,
            system_uuid("orna.object-rekey.new_key")
        );
        assert_eq!(
            REKEY_RELATION_FIELD,
            system_uuid("orna.object-rekey.relation")
        );
        // The descriptor and the row codec list fields in this order.
        assert!(REKEY_ORDINAL_FIELD < REKEY_OLD_KEY_FIELD);
        assert!(REKEY_OLD_KEY_FIELD < REKEY_NEW_KEY_FIELD);
        assert!(REKEY_NEW_KEY_FIELD < REKEY_RELATION_FIELD);
    }

    #[test]
    fn allocator_rows_round_trip_through_the_sealed_descriptor() {
        let descriptor = allocator_descriptor().expect("sealed descriptor is valid");
        let table = relation(9);
        // Watermarks are unbounded Ints, as allocator refs hold up to 64
        // decimal digits.
        let watermark = BigInt::from(10_u8).pow(40);
        let (key, fields) = allocator_summary_row(&table, &watermark).unwrap();
        assert_eq!(
            decode_allocator_summary_row(&descriptor, &key, &fields).unwrap(),
            (table, watermark)
        );
        let (_, negative) = allocator_summary_row(&table, &BigInt::from(-1)).unwrap();
        assert!(matches!(
            decode_allocator_summary_row(&descriptor, &key, &negative),
            Err(RelationAdmissionError::InvalidAllocatorSummary)
        ));
        assert!(matches!(
            decode_allocator_summary_row(
                &descriptor,
                &TypedKey::Text("not-a-uuid".to_owned()),
                &fields
            ),
            Err(RelationAdmissionError::InvalidAllocatorSummary)
        ));
    }

    #[test]
    fn rekey_rows_round_trip_through_the_sealed_descriptor() {
        let descriptor = object_rekey_descriptor().expect("sealed descriptor is valid");
        let text_key = |text: &str| TypedKey::Text(text.to_owned()).canonical_bytes().unwrap();
        let rekey = CommittedRekey {
            ordinal: 3,
            relation_id: [0x11; 16],
            old_key: text_key("london"),
            new_key: text_key("paris"),
        };
        let row_key = rekey.row_key();
        let key = row_key.canonical_bytes().unwrap();
        assert_eq!(key, [0x03]);
        let fields = rekey.row_fields().unwrap();
        descriptor
            .validate_stored_row(OBJECT_REKEY_RELATION_ID, &key, &fields)
            .expect("a rekey row matches the sealed descriptor");
        assert_eq!(CommittedRekey::from_row(&row_key, &fields).unwrap(), rekey);
        assert_eq!(
            CommittedRekey::from_row(&TypedKey::decode_canonical(&key).unwrap(), &fields).unwrap(),
            rekey
        );
        assert!(matches!(
            CommittedRekey::from_row(&TypedKey::Text("3".to_owned()), &fields),
            Err(RelationAdmissionError::InvalidRekeyLog)
        ));

        let row = |old: &str, new: &str| {
            Value::new(Raw::Array(vec![
                Raw::Text(old.to_owned()),
                Raw::Text(new.to_owned()),
                uuid_value([0x11; 16]),
            ]))
            .and_then(|value| value.encode())
            .unwrap()
        };
        let old = hex_encode(&text_key("london"));
        let new = hex_encode(&text_key("paris"));
        for fields in [
            // Only the lowercase spelling is canonical.
            row(&old.to_uppercase(), &new),
            // An odd-length spelling names no byte string.
            row(&old[1..], &new),
            // A non-shortest Int is not a canonical ORP key.
            row("1800", &new),
            // A rekey moves a row to a different address.
            row(&old, &old),
        ] {
            assert!(matches!(
                CommittedRekey::from_row(&row_key, &fields),
                Err(RelationAdmissionError::InvalidRekeyLog)
            ));
        }
    }

    /// store-format-3 §5: a rekey log row is lowered bottom-up like every
    /// other row. An address whose hex spelling passes 8 KiB is lowered to
    /// its own kind-3 node and its row stays inline, naming that node with
    /// tag 60113; a row whose addresses fit inline but whose tuple does not
    /// is lowered whole. The log's primary map stores exactly those values.
    #[test]
    fn rekey_log_rows_lower_long_addresses_first() {
        use crate::native_graph::{CborValue, decode_canonical_content};

        let (_directory, repository) = initialized_repository();
        let mut writer = NodeWriter::new(&repository).unwrap();
        let text_key = |letter: &str, length: usize| {
            TypedKey::Text(letter.repeat(length))
                .canonical_bytes()
                .unwrap()
        };
        let log = [
            // 5,000 letters spell about 10 KiB of hex.
            CommittedRekey {
                ordinal: 0,
                relation_id: [0x11; 16],
                old_key: text_key("o", 5_000),
                new_key: text_key("n", 5),
            },
            // 3,000 letters spell about 6 KiB: each address fits, the row not.
            CommittedRekey {
                ordinal: 1,
                relation_id: [0x11; 16],
                old_key: text_key("p", 3_000),
                new_key: text_key("q", 3_000),
            },
        ];
        let entry = write_rekey_relation(&mut writer, &log).unwrap();

        let rows = log
            .iter()
            .map(|rekey| {
                let fields = rekey.row_fields().unwrap();
                let value = OverflowWriter::new(&mut writer).lower_row(&fields).unwrap();
                RowEntry::new(rekey.row_key(), value)
            })
            .collect::<Vec<_>>();
        let RowValue::Inline { encoded, .. } = rows[0].value() else {
            panic!("a row with one long address stays inline");
        };
        let CborValue::Array(fields) = decode_canonical_content(encoded).unwrap() else {
            panic!("a row is a field tuple");
        };
        assert!(
            matches!(fields[0], CborValue::Tag(60_113, _)),
            "the long old address is lowered alone"
        );
        assert!(matches!(fields[1], CborValue::Text(_)));
        assert!(
            matches!(rows[1].value(), RowValue::Overflow(_)),
            "a tuple still over 8 KiB is lowered whole"
        );

        let (_, schema_digest) =
            write_schema_node(&mut writer, &object_rekey_descriptor().unwrap()).unwrap();
        let primary_root = NativeGraphContext::build_canonical_ordered_map(
            &rows_domain(&OBJECT_REKEY_RELATION_ID, &schema_digest),
            &rows,
            &mut |node| writer.node(node),
        )
        .unwrap();
        assert_eq!(entry.primary_root, primary_root);
        assert_eq!(entry.row_count, 2);
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
        repository.stage_store_admission(
            &staged,
            &tables,
            &identities_for(relations),
            CommittedRelationChanges::Allowed,
        )
    }

    #[test]
    fn private_carrier_capture_needs_no_configured_identity() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        // An empty local identity makes an ambient-identity commit fail on
        // any host, even one with a global identity, because local config
        // overrides global config. The carrier must not read it.
        git(root, &["config", "user.name", ""]);
        git(root, &["config", "user.email", ""]);
        let staged = repository
            .capture_staged_candidate()
            .expect("a private carrier commit must not require a configured identity");
        assert!(staged.store_root().is_none());
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
                CommittedRelationChanges::Allowed,
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
                CommittedRelationChanges::Allowed,
            )
            .unwrap()
    }

    #[test]
    fn snapshots_record_the_allocator_watermark_of_automatic_key_tables() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let music = relation(3);
        let automatic = |id: [u8; 16]| {
            StagedRelation::new(id, table_descriptor(id, "Str")).with_automatic_key()
        };
        let stage_automatic = |relations: &[StagedRelation]| {
            let staged = repository.capture_staged_candidate()?;
            repository.stage_store_admission(
                &staged,
                relations,
                &identities_for(&[music]),
                CommittedRelationChanges::Allowed,
            )
        };
        let watermarks = |commit: &GitCommitRef| {
            repository
                .read_committed_allocator_watermarks(commit)
                .expect("read the allocator summary")
        };
        let admission = stage_automatic(&[automatic(music)]).expect("stage the table");
        let mut expected = [music, ALLOCATOR_RELATION_ID, OBJECT_IDENTITY_RELATION_ID];
        expected.sort_unstable();
        assert_eq!(admission.admitted(), &expected);
        git(root, &["commit", "--quiet", "-m", "automatic table"]);
        let first = repository.head().unwrap().expect("committed HEAD");
        // Before its first reservation a table's watermark is zero.
        assert_eq!(
            watermarks(&first),
            Some(AllocatorWatermarks::from([(music, BigInt::from(0_u8))]))
        );
        assert!(
            repository
                .allocator_summary_maintenance(&first)
                .unwrap()
                .is_none()
        );

        // A reservation after the commit is what a publication on it records.
        repository
            .advance_automatic_id_watermark(&music, None, &BigInt::from(7_u8))
            .expect("reserve IDs");
        let (rows, maintenance) = repository
            .allocator_summary_maintenance(&first)
            .unwrap()
            .expect("the summary is behind the allocator");
        assert_eq!(rows.version().relation_id(), &ALLOCATOR_RELATION_ID);
        assert_eq!(maintenance.row_count(), 1);
        assert_ne!(maintenance.primary_root(), rows.version().primary_root());

        // The next snapshot records it; the earlier one keeps the watermark it
        // knew, and neither rewinds the allocator (ORNA-META-004).
        assert!(stage_automatic(&[automatic(music)]).unwrap().changed());
        git(root, &["commit", "--quiet", "-m", "record the watermark"]);
        let second = repository.head().unwrap().expect("committed HEAD");
        assert_eq!(
            watermarks(&second),
            Some(AllocatorWatermarks::from([(music, BigInt::from(7_u8))]))
        );
        assert_eq!(
            watermarks(&first),
            Some(AllocatorWatermarks::from([(music, BigInt::from(0_u8))]))
        );
        assert_eq!(
            repository.automatic_id_watermark(&music).unwrap(),
            Some(BigInt::from(7_u8))
        );

        // A store without automatic-key tables holds no summary.
        stage(&repository, &[music]).expect("stage an explicit key");
        git(root, &["commit", "--quiet", "-m", "explicit key"]);
        let third = repository.head().unwrap().expect("committed HEAD");
        assert!(
            !repository
                .committed_store_relations(&third)
                .unwrap()
                .contains(&ALLOCATOR_RELATION_ID)
        );
        assert_eq!(watermarks(&third), None);

        // No table may claim a sealed system relation's identity.
        assert!(matches!(
            stage_automatic(&[automatic(ALLOCATOR_RELATION_ID)]),
            Err(RelationAdmissionError::DuplicateRelation { relation_id })
                if relation_id == ALLOCATOR_RELATION_ID
        ));
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
        let entries = admit_relations(
            &mut writer,
            Vec::new(),
            &BTreeMap::new(),
            None,
            &tables,
            &identities_for(&all),
            &AllocatorWatermarks::new(),
            CommittedRelationChanges::Allowed,
        )
        .unwrap()
        .entries;
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

    /// A changed descriptor evolves its relation and a retired table's
    /// relation leaves the store, but only while no unpublished runtime rows
    /// were written under the committed schemas; duplicates never stage.
    #[test]
    fn committed_relations_change_only_without_pending_runtime_rows() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let (music, video) = (relation(1), relation(2));
        stage(&repository, &[music, video]).expect("admit two relations");
        git(root, &["commit", "--quiet", "-m", "two tables"]);

        let staged = repository.capture_staged_candidate().unwrap();
        let identities = identities_for(&[music, video]);
        let refuse = |tables: &[StagedRelation], changes| {
            repository
                .stage_store_admission(&staged, tables, &identities, changes)
                .expect_err("staging must refuse")
        };
        let current = |id| StagedRelation::new(id, table_descriptor(id, "Str"));
        let retyped = StagedRelation::new(music, table_descriptor(music, "Int"));
        let pending = CommittedRelationChanges::PendingRuntimeRows;
        assert!(matches!(
            refuse(&[retyped.clone(), current(video)], pending),
            RelationAdmissionError::PendingRuntimeRows { relation_id } if relation_id == music
        ));
        assert!(matches!(
            refuse(&[current(music)], pending),
            RelationAdmissionError::PendingRuntimeRows { relation_id } if relation_id == video
        ));
        for changes in [pending, CommittedRelationChanges::Allowed] {
            assert!(matches!(
                refuse(&[current(music), current(music), current(video)], changes),
                RelationAdmissionError::DuplicateRelation { relation_id } if relation_id == music
            ));
            assert!(matches!(
                refuse(
                    &[
                        current(music),
                        current(video),
                        StagedRelation::new(
                            OBJECT_IDENTITY_RELATION_ID,
                            table_descriptor(OBJECT_IDENTITY_RELATION_ID, "Str")
                        ),
                    ],
                    changes
                ),
                RelationAdmissionError::DuplicateRelation { .. }
            ));
        }
        // A refused staging leaves the index exactly as captured.
        assert_eq!(git(root, &["status", "--porcelain"]), "");

        // With no pending rows the empty relation takes the new schema and
        // the retired table's relation leaves the store.
        let admission = repository
            .stage_store_admission(
                &staged,
                &[retyped],
                &identities_for(&[music]),
                CommittedRelationChanges::Allowed,
            )
            .expect("evolve one relation and remove the other");
        assert_eq!(admission.evolved().len(), 1);
        assert_eq!(admission.evolved()[0].relation_id(), &music);
        assert_eq!(
            admission.evolved()[0].changes().converted(),
            &["title".to_owned()]
        );
        assert_eq!(admission.removed().len(), 1);
        assert_eq!(admission.removed()[0].relation_id(), &video);
        let head = review_and_commit(&repository, "evolve");
        assert_eq!(
            repository.committed_store_relations(&head).unwrap(),
            BTreeSet::from([music, OBJECT_IDENTITY_RELATION_ID])
        );
        let format = repository
            .open_pinned_format_context(head.as_str())
            .unwrap();
        let rows = format.load_row_map(music).unwrap();
        let graph = format.open_native_graph(&rows).unwrap();
        assert_eq!(
            graph
                .read_schema_descriptor(&graph.open_read_scope().unwrap())
                .unwrap(),
            table_descriptor(music, "Int")
        );
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

    /// `orna mv` restages only the inventory (ORNA-OBJECT-003): every
    /// committed relation keeps its exact entry and `HEAD` is untouched. An
    /// inventory that would leave a relation without an active owner, and a
    /// capture the index has moved past, are refused without changing it.
    #[test]
    fn identity_restaging_keeps_every_relation_and_refuses_orphans() {
        let (directory, repository) = initialized_repository();
        let root = directory.path();
        let music = relation(1);
        stage(&repository, &[music]).expect("stage the store");
        git(root, &["commit", "--quiet", "-m", "first table"]);
        let head = repository.head().unwrap().expect("committed HEAD");
        let staged = repository.capture_staged_candidate().unwrap();
        let database_id = *staged.database_id().as_bytes();
        let music_entry = |store_root: &NativeOid| {
            NativeGraphContext::read_store_relations(
                &repository,
                store_root.algorithm(),
                database_id,
                store_root,
            )
            .expect("read the staged relation map")
            .into_iter()
            .find(|entry| entry.relation_id == music)
            .expect("the table relation is staged")
        };
        let committed_entry = music_entry(staged.store_root().expect("a staged store"));

        let orphaned = repository
            .stage_object_identities(
                &staged,
                &[StoredObjectIdentity::new(music, 1, "Table0", true)],
                &[],
            )
            .expect_err("a retired owner orphans its relation");
        assert!(matches!(
            orphaned,
            RelationAdmissionError::OrphanRelation { relation_id } if relation_id == music
        ));
        assert_eq!(git(root, &["status", "--porcelain"]), "");

        // Neither metadata nor an untracked path is a source to restage.
        for path in [".orna/database.orna", "untracked.orna"] {
            assert!(matches!(
                repository
                    .stage_object_identities(
                        &staged,
                        &identities_for(&[music]),
                        &[(path, b"x".as_slice())],
                    )
                    .expect_err("only tracked source files are restaged"),
                RelationAdmissionError::SourceNotStaged { .. }
            ));
        }
        assert_eq!(git(root, &["status", "--porcelain"]), "");

        // The renamed source is staged with the inventory, keeping its mode,
        // and the worktree copy is left alone.
        const RENAMED: &[u8] = b"pub table Archive(id: Str) { title: Str, }\n";
        let worktree_before = fs::read(root.join("main.orna")).unwrap();
        let renamed = vec![StoredObjectIdentity::new(music, 1, "Archive", false)];
        let admission = repository
            .stage_object_identities(&staged, &renamed, &[("main.orna", RENAMED)])
            .expect("restage the renamed inventory");
        assert!(admission.changed());
        assert!(admission.admitted().is_empty());
        assert_eq!(music_entry(admission.store_root()), committed_entry);
        assert_eq!(git(root, &["show", ":main.orna"]).as_bytes(), RENAMED);
        assert!(git(root, &["ls-files", "--stage", "--", "main.orna"]).starts_with("100644 "));
        assert_eq!(fs::read(root.join("main.orna")).unwrap(), worktree_before);
        let restaged = repository.capture_staged_candidate().unwrap();
        assert_eq!(
            repository.read_staged_object_identities(&restaged).unwrap(),
            Some(renamed.clone())
        );
        assert!(
            !repository
                .stage_object_identities(&restaged, &renamed, &[])
                .unwrap()
                .changed()
        );
        assert!(matches!(
            repository
                .stage_object_identities(
                    &staged,
                    &[StoredObjectIdentity::new(music, 1, "Backup", false)],
                    &[],
                )
                .expect_err("the first capture is stale"),
            RelationAdmissionError::StaleIndex
        ));
        assert_eq!(
            repository
                .read_staged_object_identities(&repository.capture_staged_candidate().unwrap())
                .unwrap(),
            Some(renamed)
        );
        assert_eq!(
            repository.read_committed_object_identities(&head).unwrap(),
            Some(identities_for(&[music]))
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
            .commit_reviewed_candidate(&review, "initial source", &crate::AcceptAllAssertions)
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
                .commit_reviewed_candidate(&review, "initial source", &crate::AcceptAllAssertions)
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
            .commit_reviewed_candidate(&review, message, &crate::AcceptAllAssertions)
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
            repository.commit_reviewed_candidate(&review, "initial source", &crate::AcceptAllAssertions),
            Err(RepositoryError::Graph(GraphError::DurabilityFailed))
        ));
        assert!(
            repository.head().unwrap().is_none(),
            "the branch stays unborn"
        );

        fs::remove_dir(&loose).unwrap();
        fs::rename(&aside, &loose).unwrap();
        let committed = repository
            .commit_reviewed_candidate(&review, "initial source", &crate::AcceptAllAssertions)
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
            .stage_store_admission(&staged, &edited, &identities_for(&[music]), CommittedRelationChanges::Allowed)
            .expect("uncommitted relations follow the staged source");
        let admitted = [music];
        assert_eq!(admission.admitted(), admitted.as_slice());
        let committed = review_and_commit(&repository, "first");
        assert_eq!(
            repository.committed_store_relations(&committed).unwrap(),
            BTreeSet::from([music, OBJECT_IDENTITY_RELATION_ID])
        );

        let staged = repository.capture_staged_candidate().unwrap();
        let original = [StagedRelation::new(music, table_descriptor(music, "Str"))];
        let evolved = repository
            .stage_store_admission(&staged, &original, &identities_for(&[music]), CommittedRelationChanges::Allowed)
            .expect("a committed relation without rows evolves to its staged schema");
        assert!(evolved.evolved().iter().any(|change| *change.relation_id() == music));
        let staged = repository.capture_staged_candidate().unwrap();
        let removed = repository
            .stage_store_admission(&staged, &[], &identities_for(&[music]), CommittedRelationChanges::Allowed)
            .expect("a relation the source no longer declares leaves the store");
        assert!(removed.removed().iter().any(|change| *change.relation_id() == music));

        // The user's removal from the index, unmodified: skip-worktree payload
        // entries survive it, so the index keeps a partial store.
        git(root, &["rm", "--cached", "-r", "--quiet", ".orna/store"]);
        assert!(!git(root, &["ls-files", "--", ".orna/store"]).is_empty());
        let staged = repository.capture_staged_candidate().unwrap();
        assert!(matches!(
            repository.stage_store_admission(&staged, &edited, &identities_for(&[music]), CommittedRelationChanges::Allowed),
            Err(RelationAdmissionError::CommittedRelationUnstaged { relation_id })
                if relation_id == music
        ));
    }
}
