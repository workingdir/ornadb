//! ORP-1 typed row-map and overflow primitives for repository format 3.
//!
//! This is a canonical map builder, not a loose-row, Parquet, compact, or
//! hybrid writer.  The repository context will later bind the returned map to
//! an admitted native store root and schema.  Until then the boundary remains
//! an explicit value and no raw row path is an admission API.

use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use orna_value_v1::{
    BlobMetadata, Decimal, MediaAnnotation, Raw, Value, canonical_uuid_text, compare_primary_keys,
    decode_typed,
};

use crate::native_graph::{
    CborValue, GraphError, NODE_DATA_LIMIT, NativeGraphContext, NativeObjectId, NativeObjectKind,
    RepositoryReadScope, canonical_value_bytes, decode_canonical_cbor, direct_blob_reference_count,
    row_fields_dependencies,
};

pub const ROW_INLINE_LIMIT: usize = 8_192;
/// Direct Blob references an inline stored value may hold.
pub const ROW_INLINE_REF_LIMIT: usize = 32;
pub const ROW_CANONICAL_LIMIT: usize = 1_073_741_824;
pub const KEY_CANONICAL_LIMIT: usize = 1_048_576;
pub const MIN_PAGE_ENTRIES: usize = 16;
pub const MAX_PAGE_ENTRIES: usize = 256;
pub const ORP_ANCHOR_BITS: u8 = 6;
pub const ORP_DOMAIN: &[u8] = b"orna.orp.boundary.v1\0";
pub const MAX_ROW_RANGE_LIMIT: usize = 256;

/// The format-3 stored-value tag for an annotated Blob reference (ROV-3).
pub const ROV3_BLOB_TAG: u64 = 60_111;

/// Immutable format-3 schema identity for one relation generation.  The
/// local generation number is an owner fence; semantic identity remains the
/// schema digest, and physical identity remains the native schema OID.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaGeneration {
    database_id: [u8; 16],
    relation_id: [u8; 16],
    schema_oid: NativeObjectId,
    schema_digest: [u8; 32],
    generation: u64,
}

impl SchemaGeneration {
    pub(crate) fn issue(
        database_id: [u8; 16],
        relation_id: [u8; 16],
        schema_oid: NativeObjectId,
        schema_digest: [u8; 32],
        generation: u64,
    ) -> Self {
        Self {
            database_id,
            relation_id,
            schema_oid,
            schema_digest,
            generation,
        }
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn schema_oid(&self) -> &NativeObjectId {
        &self.schema_oid
    }

    pub const fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// Immutable identity of one admitted ORP row map.  It binds database,
/// relation, store root, schema generation and primary-map root; generation is
/// a local monotonic fence and is not substituted for any content identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowMapVersion {
    database_id: [u8; 16],
    relation_id: [u8; 16],
    store_root: NativeObjectId,
    schema: SchemaGeneration,
    primary_root: NativeObjectId,
    generation: u64,
    row_count: Option<u64>,
}

impl RowMapVersion {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue(
        database_id: [u8; 16],
        relation_id: [u8; 16],
        store_root: NativeObjectId,
        schema: SchemaGeneration,
        primary_root: NativeObjectId,
        generation: u64,
        row_count: Option<u64>,
    ) -> Result<Self, RowStoreError> {
        if schema.database_id != database_id || schema.relation_id != relation_id {
            return Err(RowStoreError::VersionIdentityMismatch);
        }
        if store_root.algorithm() != primary_root.algorithm()
            || schema.schema_oid.algorithm() != store_root.algorithm()
        {
            return Err(RowStoreError::VersionIdentityMismatch);
        }
        if row_count.is_some_and(|count| count > i64::MAX as u64) {
            return Err(RowStoreError::RowCountExceeded);
        }
        Ok(Self {
            database_id,
            relation_id,
            store_root,
            schema,
            primary_root,
            generation,
            row_count,
        })
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn store_root(&self) -> &NativeObjectId {
        &self.store_root
    }

    pub fn schema(&self) -> &SchemaGeneration {
        &self.schema
    }

    pub fn primary_root(&self) -> &NativeObjectId {
        &self.primary_root
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn row_count(&self) -> Option<u64> {
        self.row_count
    }
}

/// A bounded logical-key interval: inclusive lower bound, exclusive upper
/// bound, an optional exclusive resume cursor, and maximum number of returned
/// rows.
///
/// The resume cursor is what lets paging avoid successor keys: a page that
/// ends at key `k` resumes with `k` as its cursor, and only keys strictly
/// greater than `k` are returned. Keys are ordered by [`TypedKey`]'s total
/// order, so this is correct for every key type, composite keys included.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyRange {
    lower_inclusive: Option<TypedKey>,
    upper_exclusive: Option<TypedKey>,
    resume_after: Option<TypedKey>,
    limit: usize,
}

impl KeyRange {
    pub fn new(
        lower_inclusive: Option<TypedKey>,
        upper_exclusive: Option<TypedKey>,
        limit: usize,
    ) -> Result<Self, RowStoreError> {
        if limit == 0 || limit > MAX_ROW_RANGE_LIMIT {
            return Err(RowStoreError::InvalidRangeLimit(limit));
        }
        if lower_inclusive
            .as_ref()
            .zip(upper_exclusive.as_ref())
            .is_some_and(|(lower, upper)| !lower.same_key_type(upper) || lower >= upper)
        {
            return Err(RowStoreError::InvalidKeyRange);
        }
        Ok(Self {
            lower_inclusive,
            upper_exclusive,
            resume_after: None,
            limit,
        })
    }

    pub fn lower_inclusive(&self) -> Option<&TypedKey> {
        self.lower_inclusive.as_ref()
    }

    pub fn upper_exclusive(&self) -> Option<&TypedKey> {
        self.upper_exclusive.as_ref()
    }

    pub fn resume_after(&self) -> Option<&TypedKey> {
        self.resume_after.as_ref()
    }

    /// Narrows the range to keys strictly greater than `key`. The cursor must
    /// be a key of the range's type that lies below the upper bound.
    pub fn with_resume_after(mut self, key: TypedKey) -> Result<Self, RowStoreError> {
        let lower_ok = self
            .lower_inclusive
            .as_ref()
            .is_none_or(|lower| lower.same_key_type(&key));
        let upper_ok = self
            .upper_exclusive
            .as_ref()
            .is_none_or(|upper| upper.same_key_type(&key) && key < *upper);
        if !lower_ok || !upper_ok {
            return Err(RowStoreError::InvalidKeyRange);
        }
        self.resume_after = Some(key);
        Ok(self)
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }
}

/// One immutable admitted row.  Construction is restricted to the sealed
/// snapshot path; callers cannot create a row admission from a key/OID pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRow {
    version: RowMapVersion,
    key: TypedKey,
    value: RowValue,
}

impl AdmittedRow {
    fn from_entry(version: &RowMapVersion, entry: &RowEntry) -> Self {
        Self {
            version: version.clone(),
            key: entry.key.clone(),
            value: entry.value.clone(),
        }
    }

    pub(crate) fn issue(version: &RowMapVersion, key: TypedKey, value: RowValue) -> Self {
        Self {
            version: version.clone(),
            key,
            value,
        }
    }

    pub fn version(&self) -> &RowMapVersion {
        &self.version
    }

    pub fn key(&self) -> &TypedKey {
        &self.key
    }

    pub fn value(&self) -> &RowValue {
        &self.value
    }

    /// Projects this row's canonical stored field tuple to payload-free
    /// metadata of one Blob field.
    ///
    /// The fields are decoded from the row's own bytes; the descriptor the
    /// Blob carries is surfaced as-is, so the caller gets length, SHA-256 and
    /// annotation without the graph or any content read being touched. A row
    /// whose Blob fields moved to the shared overflow graph has no local field
    /// tuple and reports [`RowStoreError::BlobMetadataRequiresGraphContext`]
    /// rather than a fabricated summary.
    pub fn blob_metadata(&self, field: usize) -> Result<Option<BlobMetadata>, RowStoreError> {
        let fields = decode_canonical_fields_of(
            self.value
                .encoded_fields()
                .ok_or(RowStoreError::BlobMetadataRequiresGraphContext)?,
        )?;
        let Some(field) = fields.get(field) else {
            return Ok(None);
        };
        blob_field_metadata(field)
    }

    /// Projects every Blob field of this row to payload-free metadata.
    ///
    /// A media row keeps its Blob among ordinary fields, so a listing walks
    /// the tuple once and reports each Blob with its index in canonical
    /// order. A field that is not a Blob in canonical shape is skipped: it is
    /// not a Blob, which is a different answer from a malformed Blob.
    pub fn blob_fields(&self) -> Result<Vec<(usize, BlobMetadata)>, RowStoreError> {
        let fields = decode_canonical_fields_of(
            self.value
                .encoded_fields()
                .ok_or(RowStoreError::BlobMetadataRequiresGraphContext)?,
        )?;
        let mut projected = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            if let Some(metadata) = blob_field_metadata(field)? {
                projected.push((index, metadata));
            }
        }
        Ok(projected)
    }
}

/// Durable production identity for one VFS row directory. The address is
/// issued only from an admitted ORP row: database/relation identities and the
/// canonical typed key come from the row-map version, not from a Git worktree
/// path or generated filename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryVfsRowIdentity {
    database_id: [u8; 16],
    relation_id: [u8; 16],
    store_root: NativeObjectId,
    schema_digest: [u8; 32],
    primary_root: NativeObjectId,
    generation: u64,
    key: TypedKey,
    canonical_key: Vec<u8>,
}

impl RepositoryVfsRowIdentity {
    pub fn from_admitted_row(row: &AdmittedRow) -> Result<Self, RowStoreError> {
        let version = row.version();
        Ok(Self {
            database_id: *version.database_id(),
            relation_id: *version.relation_id(),
            store_root: version.store_root().clone(),
            schema_digest: *version.schema().schema_digest(),
            primary_root: version.primary_root().clone(),
            generation: version.generation(),
            key: row.key().clone(),
            canonical_key: row.key().canonical_bytes()?,
        })
    }

    pub const fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn store_root(&self) -> &NativeObjectId {
        &self.store_root
    }

    pub const fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub fn primary_root(&self) -> &NativeObjectId {
        &self.primary_root
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn key(&self) -> &TypedKey {
        &self.key
    }

    pub fn canonical_key(&self) -> &[u8] {
        &self.canonical_key
    }
}

/// Stable field path identity within one production VFS row projection. The
/// digest is derived from row-map identity plus schema-issued field identities,
/// so long-name aliases cannot be forged from a host filename alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryVfsFieldIdentity {
    row: RepositoryVfsRowIdentity,
    field_path: Vec<[u8; 16]>,
    projection_digest: [u8; 32],
}

impl RepositoryVfsFieldIdentity {
    pub fn new(
        row: RepositoryVfsRowIdentity,
        field_path: impl Into<Vec<[u8; 16]>>,
    ) -> Result<Self, RowStoreError> {
        let field_path = field_path.into();
        if field_path.is_empty() || field_path.iter().any(|identity| identity == &[0; 16]) {
            return Err(RowStoreError::VersionIdentityMismatch);
        }
        let projection_digest = vfs_field_projection_digest(&row, &field_path);
        Ok(Self {
            row,
            field_path,
            projection_digest,
        })
    }

    pub fn row(&self) -> &RepositoryVfsRowIdentity {
        &self.row
    }

    pub fn field_path(&self) -> &[[u8; 16]] {
        &self.field_path
    }

    pub const fn projection_digest(&self) -> &[u8; 32] {
        &self.projection_digest
    }
}

fn vfs_field_projection_digest(
    row: &RepositoryVfsRowIdentity,
    field_path: &[[u8; 16]],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.vfs.field.identity.v1\0");
    hasher.update(row.database_id());
    hasher.update(row.relation_id());
    hasher.update(row.store_root().as_bytes());
    hasher.update(row.schema_digest());
    hasher.update(row.primary_root().as_bytes());
    hasher.update(row.generation().to_be_bytes());
    hasher.update((row.canonical_key().len() as u64).to_be_bytes());
    hasher.update(row.canonical_key());
    hasher.update((field_path.len() as u64).to_be_bytes());
    for field in field_path {
        hasher.update(field);
    }
    hasher.finalize().into()
}

/// Owner-issued immutable row-map view.  The authority token and rows are
/// private; only repository-context verification can call `issue`.
#[derive(Clone, Debug)]
pub struct RowMapSnapshot {
    version: RowMapVersion,
    authority: [u8; 32],
    entries: Option<Arc<[RowEntry]>>,
}

impl RowMapSnapshot {
    pub(crate) fn issue(
        version: RowMapVersion,
        authority: [u8; 32],
        entries: Vec<RowEntry>,
    ) -> Result<Self, RowStoreError> {
        if entries.windows(2).any(|pair| pair[0].key >= pair[1].key) {
            return Err(RowStoreError::RowsNotStrictlyOrdered);
        }
        if version
            .row_count
            .is_some_and(|count| count != entries.len() as u64)
        {
            return Err(RowStoreError::RowCountMismatch);
        }
        Ok(Self {
            version,
            authority,
            entries: Some(entries.into()),
        })
    }

    /// Seals a persisted ORP map without materializing its rows. Reads are
    /// performed through the matching repository-issued NativeGraphContext;
    /// this snapshot carries identity only and cannot authorize arbitrary OIDs.
    pub(crate) fn issue_lazy(
        version: RowMapVersion,
        authority: [u8; 32],
    ) -> Result<Self, RowStoreError> {
        if version.row_count.is_none() {
            return Err(RowStoreError::RowCountMismatch);
        }
        Ok(Self {
            version,
            authority,
            entries: None,
        })
    }

    pub fn version(&self) -> &RowMapVersion {
        &self.version
    }

    pub(crate) const fn authority(&self) -> &[u8; 32] {
        &self.authority
    }

    pub const fn is_materialized(&self) -> bool {
        self.entries.is_some()
    }

    pub fn get(&self, key: &TypedKey) -> Result<Option<AdmittedRow>, RowStoreError> {
        let entries = self
            .entries
            .as_ref()
            .ok_or(RowStoreError::PersistedLookupRequiresGraphContext)?;
        Ok(entries
            .binary_search_by(|entry| entry.key.cmp(key))
            .ok()
            .map(|index| AdmittedRow::from_entry(&self.version, &entries[index])))
    }

    pub fn range(&self, range: &KeyRange) -> Result<Vec<AdmittedRow>, RowStoreError> {
        let entries = self
            .entries
            .as_ref()
            .ok_or(RowStoreError::PersistedLookupRequiresGraphContext)?;
        let start = range.lower_inclusive.as_ref().map_or(0, |lower| {
            entries.partition_point(|entry| entry.key < *lower)
        });
        let start = range.resume_after.as_ref().map_or(start, |after| {
            start.max(entries.partition_point(|entry| entry.key <= *after))
        });
        Ok(entries[start..]
            .iter()
            .take_while(|entry| {
                range
                    .upper_exclusive
                    .as_ref()
                    .map_or(true, |upper| entry.key < *upper)
            })
            .take(range.limit)
            .map(|entry| AdmittedRow::from_entry(&self.version, entry))
            .collect())
    }

    /// Reads a bounded range from a persisted snapshot through its matching
    /// repository graph context. The graph remains the only decoder: it checks
    /// the admitted schema domain on each traversed page before exposing rows.
    pub fn range_with_graph_context(
        &self,
        context: &NativeGraphContext,
        scope: &RepositoryReadScope,
        range: &KeyRange,
    ) -> Result<Vec<AdmittedRow>, RowStoreError> {
        let version = &self.version;
        if context.row_map_version() != version {
            return Err(RowStoreError::BoundaryMismatch);
        }

        let row_count = version.row_count.ok_or(RowStoreError::RowCountMismatch)?;
        let mut authority = Sha256::new();
        authority.update(b"orna.repository.orp.snapshot-authority.v1\0");
        authority.update(context.database_id());
        authority.update(context.snapshot_id());
        authority.update(version.store_root.as_bytes());
        authority.update(version.relation_id);
        authority.update(version.primary_root.as_bytes());
        authority.update(context.schema_digest());
        authority.update(row_count.to_be_bytes());
        let expected_authority: [u8; 32] = authority.finalize().into();
        if version.database_id() != context.database_id()
            || version.schema().database_id() != context.database_id()
            || version.schema().schema_digest() != context.schema_digest()
            || self.authority != expected_authority
        {
            return Err(RowStoreError::BoundaryMismatch);
        }

        context
            .range_rows(range, scope)
            .map_err(RowStoreError::from)
    }
}

/// The logical key values of the row-map seam. Ordering is the language's
/// logical key order, never filename, encoded-byte or hash order, and the
/// enum excludes floating point and opaque storage OIDs from primary keys.
///
/// `Int` and `UInt` are two spellings of one Orna `Int`: they compare, hash
/// and encode identically, and decoding yields `UInt` for a non-negative value
/// and `Int` for a negative one. Equality, ordering and hashing all follow the
/// logical value, so `Int(5) == UInt(5)`.
#[derive(Clone, Debug)]
pub enum TypedKey {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Text(String),
    Bytes(Vec<u8>),
    Tuple(Vec<TypedKey>),
    /// A permitted key component without a variant above: a Uuid, Decimal,
    /// Date, Instant, payload-free enum variant, stored table reference or
    /// tuple value, or an Int beyond the 64-bit range.
    Value(KeyValue),
}

impl TypedKey {
    /// The row-map key for one canonical Orna key value, using the shorthand
    /// variant where one exists.
    pub fn from_value(value: &Value) -> Result<Self, RowStoreError> {
        let bytes = value
            .encode()
            .map_err(|_| RowStoreError::InvalidCanonicalKey)?;
        Self::decode_canonical(&bytes)
    }

    fn same_key_type(&self, other: &Self) -> bool {
        self.type_cmp(other).is_eq()
    }

    /// The canonical text of one key component (profiles/vfs-1.md:44-50).
    /// `None` for a tuple, which projects component by component, and for a
    /// value whose text needs its schema.
    pub fn component_text(&self) -> Option<String> {
        Some(match self {
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::UInt(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Bytes(bytes) => {
                let mut text = String::with_capacity(2 + bytes.len() * 2);
                text.push_str("0x");
                for byte in bytes {
                    text.push_str(&format!("{byte:02x}"));
                }
                text
            }
            Self::Text(value) => value.clone(),
            Self::Tuple(_) => return None,
            Self::Value(value) => value.canonical_text()?,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RowStoreError> {
        let mut output = Vec::new();
        encode_key(self, &mut output)?;
        if output.len() > KEY_CANONICAL_LIMIT {
            return Err(RowStoreError::KeyTooLarge(output.len()));
        }
        Ok(output)
    }

    /// Decodes the canonical typed-key encoding used by format-3 row maps.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RowStoreError> {
        if bytes.len() > KEY_CANONICAL_LIMIT {
            return Err(RowStoreError::KeyTooLarge(bytes.len()));
        }
        let value = decode_canonical_cbor(bytes)?;
        let key = Self::from_cbor(value)?;
        if key.canonical_bytes()?.as_slice() != bytes {
            return Err(RowStoreError::InvalidCanonicalKey);
        }
        Ok(key)
    }

    fn from_cbor(value: CborValue) -> Result<Self, RowStoreError> {
        match value {
            CborValue::Null => Ok(Self::Null),
            CborValue::Bool(value) => Ok(Self::Bool(value)),
            CborValue::Unsigned(value) => Ok(Self::UInt(value)),
            CborValue::Negative(magnitude) => match i64::try_from(magnitude) {
                Ok(magnitude) => Ok(Self::Int(-1 - magnitude)),
                Err(_) => KeyValue::from_cbor(&CborValue::Negative(magnitude)).map(Self::Value),
            },
            CborValue::Text(value) => Ok(Self::Text(value)),
            CborValue::Bytes(value) => Ok(Self::Bytes(value)),
            CborValue::Array(values) => Ok(Self::Tuple(
                values
                    .into_iter()
                    .map(Self::from_cbor)
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            tagged @ CborValue::Tag(..) => KeyValue::from_cbor(&tagged).map(Self::Value),
            CborValue::Map(_) | CborValue::Float64(_) => Err(RowStoreError::InvalidCanonicalKey),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Int(_) | Self::UInt(_) => 2,
            Self::Text(_) => 3,
            Self::Bytes(_) => 4,
            Self::Value(value) => raw_key_rank(value.value.raw()),
            Self::Tuple(_) => TUPLE_KEY_RANK,
        }
    }

    /// Orders key types: the rank of each component, then component counts.
    /// All keys of one table share a type and compare equal here.
    fn type_cmp(&self, other: &Self) -> Ordering {
        self.rank()
            .cmp(&other.rank())
            .then_with(|| match (self, other) {
                (Self::Tuple(left), Self::Tuple(right)) => left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| left.type_cmp(right))
                    .find(|ordering| ordering.is_ne())
                    .unwrap_or_else(|| left.len().cmp(&right.len())),
                (Self::Value(left), Self::Value(right)) => {
                    raw_key_type_cmp(left.value.raw(), right.value.raw())
                }
                _ => Ordering::Equal,
            })
    }

    /// Orders two keys of one type by logical value. Tagged components use
    /// `orna_value_v1::compare_primary_keys`, the language's key comparison.
    fn value_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Bool(left), Self::Bool(right)) => left.cmp(right),
            (Self::Text(left), Self::Text(right)) => left.as_bytes().cmp(right.as_bytes()),
            (Self::Bytes(left), Self::Bytes(right)) => left.cmp(right),
            (Self::Tuple(left), Self::Tuple(right)) => left
                .iter()
                .zip(right)
                .map(|(left, right)| left.value_cmp(right))
                .find(|ordering| ordering.is_ne())
                .unwrap_or_else(|| left.len().cmp(&right.len())),
            // Admitted components of one type always compare; the byte order
            // only keeps the order total should that ever fail.
            (Self::Value(left), Self::Value(right)) => {
                compare_primary_keys(&left.value, &right.value)
                    .unwrap_or_else(|_| left.canonical.cmp(&right.canonical))
            }
            _ => match (self.narrow_int(), other.narrow_int()) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => other.wide_int_order().reverse(),
                (None, Some(_)) => self.wide_int_order(),
                (None, None) => Ordering::Equal,
            },
        }
    }

    fn narrow_int(&self) -> Option<i128> {
        match self {
            Self::Int(value) => Some(i128::from(*value)),
            Self::UInt(value) => Some(i128::from(*value)),
            _ => None,
        }
    }

    /// Where an Int beyond the 64-bit spellings sits relative to all of them.
    fn wide_int_order(&self) -> Ordering {
        match self {
            Self::Value(value) if wide_int_negative(&value.canonical) == Some(true) => {
                Ordering::Less
            }
            _ => Ordering::Greater,
        }
    }

    /// Feeds the canonical key encoding to `state` without building it, so
    /// equal keys, including the two spellings of one Int, hash equally.
    fn hash_canonical<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Null => state.write_u8(0xf6),
            Self::Bool(value) => state.write_u8(if *value { 0xf5 } else { 0xf4 }),
            Self::Int(value) => hash_head(state, int_head(i128::from(*value))),
            Self::UInt(value) => hash_head(state, int_head(i128::from(*value))),
            Self::Text(value) => {
                hash_head(state, (3, value.len() as u64));
                state.write(value.as_bytes());
            }
            Self::Bytes(value) => {
                hash_head(state, (2, value.len() as u64));
                state.write(value);
            }
            Self::Tuple(values) => {
                hash_head(state, (4, values.len() as u64));
                for value in values {
                    value.hash_canonical(state);
                }
            }
            Self::Value(value) => state.write(&value.canonical),
        }
    }
}

impl PartialEq for TypedKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // One logical value has one canonical encoding.
            (Self::Value(left), Self::Value(right)) => left == right,
            (Self::Tuple(left), Self::Tuple(right)) => left == right,
            _ => self.cmp(other).is_eq(),
        }
    }
}

impl Eq for TypedKey {}

impl Hash for TypedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_canonical(state);
    }
}

impl Ord for TypedKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.type_cmp(other).then_with(|| self.value_cmp(other))
    }
}

impl PartialOrd for TypedKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One tagged primary-key component, held as its validated canonical value.
///
/// Admission comes from `orna_value_v1`: the bytes decode as a registered
/// canonical value, and `compare_primary_keys` accepts it as a key component.
/// A Float, Blob, payload-carrying enum variant or Option is never a key.
#[derive(Clone, Debug)]
pub struct KeyValue {
    value: Value,
    canonical: Vec<u8>,
}

impl KeyValue {
    fn from_cbor(value: &CborValue) -> Result<Self, RowStoreError> {
        let canonical = canonical_value_bytes(value);
        let value = Value::decode(&canonical).map_err(|_| RowStoreError::InvalidCanonicalKey)?;
        let admitted = match value.raw() {
            // Ints inside the 64-bit heads are spelled `Int` or `UInt`.
            Raw::Int(_) => wide_int_negative(&canonical).is_some(),
            Raw::Tag(tag, _) => matches!(
                *tag,
                37 | 60_000 | 60_001 | 60_002 | 60_008 | 60_015 | 60_021
            ),
            _ => false,
        };
        if !admitted || compare_primary_keys(&value, &value).is_err() {
            return Err(RowStoreError::InvalidCanonicalKey);
        }
        Ok(Self { value, canonical })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// The language's canonical key text where it needs no schema: lowercase
    /// hyphenated Uuid, canonical Date, exact canonical Decimal and shortest
    /// decimal Int. Instants, enum variants, references and tuples are spelled
    /// under their schema, so they have none here.
    pub fn canonical_text(&self) -> Option<String> {
        match self.value.raw() {
            Raw::Int(value) => Some(value.to_string()),
            Raw::Tag(37, payload) => match payload.as_ref() {
                Raw::Bytes(bytes) => Some(canonical_uuid_text(bytes.as_slice().try_into().ok()?)),
                _ => None,
            },
            Raw::Tag(60_000, _) => decode_typed::<Decimal>(&self.canonical)
                .ok()
                .map(|decimal| decimal.canonical_text()),
            Raw::Tag(60_001, payload) => match payload.as_ref() {
                Raw::Text(date) => Some(date.clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

impl PartialEq for KeyValue {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for KeyValue {}

impl Hash for KeyValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.canonical);
    }
}

const TUPLE_KEY_RANK: u8 = 12;

/// Rank of each logical key type in the total key order. A table's keys share
/// one type, so the order across types only has to be total and stable; the
/// shorthand variants keep their earlier relative order.
fn raw_key_rank(raw: &Raw) -> u8 {
    match raw {
        Raw::Null => 0,
        Raw::Bool(_) => 1,
        Raw::Int(_) => 2,
        Raw::Text(_) => 3,
        Raw::Bytes(_) => 4,
        Raw::Tag(37, _) => 5,
        Raw::Tag(60_000, _) => 6,
        Raw::Tag(60_001, _) => 7,
        Raw::Tag(60_002, _) => 8,
        Raw::Tag(60_008, _) => 9,
        Raw::Tag(60_015, _) => 10,
        Raw::Tag(60_021, _) => 11,
        Raw::Array(_) => TUPLE_KEY_RANK,
        _ => TUPLE_KEY_RANK + 1,
    }
}

/// Orders the types of two tagged components: rank, then the component types
/// of a tuple value or of the key a table reference names.
fn raw_key_type_cmp(left: &Raw, right: &Raw) -> Ordering {
    raw_key_rank(left)
        .cmp(&raw_key_rank(right))
        .then_with(|| match (left, right) {
            (Raw::Tag(60_015, left), Raw::Tag(60_015, right)) => {
                match (left.as_ref(), right.as_ref()) {
                    (Raw::Array(left), Raw::Array(right)) => left
                        .iter()
                        .zip(right)
                        .map(|(left, right)| raw_key_type_cmp(left, right))
                        .find(|ordering| ordering.is_ne())
                        .unwrap_or_else(|| left.len().cmp(&right.len())),
                    _ => Ordering::Equal,
                }
            }
            (Raw::Tag(60_021, left), Raw::Tag(60_021, right)) => {
                match (left.as_ref(), right.as_ref()) {
                    (Raw::Array(left), Raw::Array(right)) => match (left.get(2), right.get(2)) {
                        (Some(left), Some(right)) => raw_key_type_cmp(left, right),
                        _ => Ordering::Equal,
                    },
                    _ => Ordering::Equal,
                }
            }
            _ => Ordering::Equal,
        })
}

/// The sign of an Int beyond the 64-bit `Int`/`UInt` spellings, read from its
/// canonical encoding: a tag 2 or 3 bignum, or a negative head below
/// `i64::MIN`. `None` for every other encoding.
fn wide_int_negative(canonical: &[u8]) -> Option<bool> {
    match canonical {
        [0xc2, ..] => Some(false),
        [0xc3, ..] => Some(true),
        [0x3b, magnitude @ ..] => {
            let magnitude: [u8; 8] = magnitude.try_into().ok()?;
            (u64::from_be_bytes(magnitude) > i64::MAX as u64).then_some(true)
        }
        _ => None,
    }
}

/// The CBOR major type and argument spelling one 64-bit Int.
fn int_head(value: i128) -> (u8, u64) {
    if value >= 0 {
        (0, value as u64)
    } else {
        (1, (-1 - value) as u64)
    }
}

fn hash_head<H: Hasher>(state: &mut H, (major, value): (u8, u64)) {
    let (head, length) = cbor_head_bytes(major, value);
    state.write(&head[..length]);
}

/// A direct stored-value dependency.  Blob payloads are references and never
/// become row-page bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowDependency {
    oid: NativeObjectId,
    kind: NativeObjectKind,
}

impl RowDependency {
    pub fn new(oid: NativeObjectId, kind: NativeObjectKind) -> Self {
        Self { oid, kind }
    }

    pub fn oid(&self) -> &NativeObjectId {
        &self.oid
    }

    pub const fn kind(&self) -> NativeObjectKind {
        self.kind
    }
}

/// Kind-3 value-overflow reference.  The encoded value remains in the shared
/// graph; this object carries only bounded metadata and its graph roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueOverflowRef {
    native_root: Option<NativeObjectId>,
    encoded_length: u64,
    semantic_digest: [u8; 32],
    byte_root: NativeObjectId,
    dependency_root: Option<NativeObjectId>,
}

impl ValueOverflowRef {
    pub fn new(
        encoded_length: u64,
        semantic_digest: [u8; 32],
        byte_root: NativeObjectId,
        dependency_root: Option<NativeObjectId>,
    ) -> Result<Self, RowStoreError> {
        if encoded_length > i64::MAX as u64 {
            return Err(RowStoreError::ValueTooLarge(encoded_length));
        }
        Ok(Self {
            native_root: None,
            encoded_length,
            semantic_digest,
            byte_root,
            dependency_root,
        })
    }

    pub(crate) fn from_native_node(
        native_root: NativeObjectId,
        encoded_length: u64,
        semantic_digest: [u8; 32],
        byte_root: NativeObjectId,
        dependency_root: Option<NativeObjectId>,
    ) -> Result<Self, RowStoreError> {
        let mut reference = Self::new(encoded_length, semantic_digest, byte_root, dependency_root)?;
        reference.native_root = Some(native_root);
        Ok(reference)
    }

    pub(crate) fn native_root(&self) -> Option<&NativeObjectId> {
        self.native_root.as_ref()
    }

    pub const fn encoded_length(&self) -> u64 {
        self.encoded_length
    }

    pub const fn semantic_digest(&self) -> &[u8; 32] {
        &self.semantic_digest
    }

    pub fn byte_root(&self) -> &NativeObjectId {
        &self.byte_root
    }

    pub fn dependency_root(&self) -> Option<&NativeObjectId> {
        self.dependency_root.as_ref()
    }
}

/// A stored row value in its one canonical format-3 reference form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowValue {
    Inline {
        encoded: Vec<u8>,
        dependencies: Vec<RowDependency>,
    },
    Overflow(ValueOverflowRef),
}

impl RowValue {
    /// An inline stored value: at most 8 KiB encoded and at most 32 Blob
    /// references of its own (store-3.json `row_inline_limit` and
    /// `row_inline_ref_limit`). References inside a nested overflow value
    /// belong to that value, so `dependencies` may also name overflow nodes.
    pub fn inline(
        encoded: Vec<u8>,
        dependencies: Vec<RowDependency>,
    ) -> Result<Self, RowStoreError> {
        if encoded.len() > ROW_INLINE_LIMIT {
            return Err(RowStoreError::InlineValueTooLarge(encoded.len()));
        }
        let blob_references = direct_blob_reference_count(&decode_canonical_cbor(&encoded)?)?;
        if blob_references > ROW_INLINE_REF_LIMIT {
            return Err(RowStoreError::InlineDependencyFanout(blob_references));
        }
        if dependencies
            .iter()
            .map(|dependency| dependency.oid())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != dependencies.len()
        {
            return Err(RowStoreError::DuplicateDependency);
        }
        Ok(Self::Inline {
            encoded,
            dependencies,
        })
    }

    pub(crate) fn decode_canonical_fields(encoded: Vec<u8>) -> Result<Self, RowStoreError> {
        if !matches!(decode_canonical_cbor(&encoded), Ok(CborValue::Array(_))) {
            return Err(RowStoreError::InvalidCanonicalRowValue);
        }
        let dependencies = row_fields_dependencies(&encoded)?
            .into_iter()
            .map(|dependency| RowDependency::new(dependency.oid().clone(), dependency.kind()))
            .collect();
        Self::inline(encoded, dependencies).map_err(|_| RowStoreError::InvalidCanonicalRowValue)
    }

    pub fn overflow(reference: ValueOverflowRef) -> Self {
        Self::Overflow(reference)
    }

    /// The canonical CBOR spelling of this stored value exactly as it belongs
    /// in an ORP-1 page. Inline values keep their admitted field encoding; an
    /// overflow value is the kind-3 tag naming its shared value graph root, so
    /// re-reading the page re-derives the same idempotent reference.
    ///
    /// A reference that has no resolved native root has no canonical page
    /// spelling: the byte root is not the graph root the reader resolves, and
    /// substituting it would name a different node. Such a value is rejected
    /// rather than spelled ambiguously.
    pub(crate) fn canonical_page_bytes(&self) -> Result<Vec<u8>, RowStoreError> {
        match self {
            Self::Inline { encoded, .. } => Ok(encoded.clone()),
            Self::Overflow(reference) => {
                let root = reference
                    .native_root()
                    .ok_or(RowStoreError::UnresolvedValueOverflow)?;
                let mut output = Vec::with_capacity(root.as_bytes().len() + 6);
                cbor_head(&mut output, 6, orna_value_v1::ROV3_OVERFLOW_TAG);
                cbor_head(&mut output, 4, 1);
                cbor_head(&mut output, 2, root.as_bytes().len() as u64);
                output.extend_from_slice(root.as_bytes());
                Ok(output)
            }
        }
    }

    /// The complete canonical ORP-1 page body for an ordered set of entries:
    /// the CBOR array of `[key, value]` pairs a reader decodes into one leaf.
    /// Entry order is the order supplied, which the caller owns.
    pub(crate) fn page_bytes(entries: &[RowEntry]) -> Result<Vec<u8>, RowStoreError> {
        if entries.len() > MAX_PAGE_ENTRIES {
            return Err(RowStoreError::PageFanoutExceeded(entries.len()));
        }
        let mut output = Vec::new();
        // The shortest definite array header, so a 256-entry page stays
        // canonical rather than being truncated to a one-byte count.
        cbor_head(&mut output, 4, entries.len() as u64);
        for entry in entries {
            let key = entry.key.canonical_bytes()?;
            let value = entry.value.canonical_page_bytes()?;
            let mut pair = Vec::with_capacity(key.len() + value.len() + 4);
            // The pair is a two-element array: the canonical typed key, then
            // the canonical stored value.
            cbor_head(&mut pair, 4, 2);
            pair.extend_from_slice(&key);
            pair.extend_from_slice(&value);
            if output.len() + pair.len() > NODE_DATA_LIMIT {
                return Err(RowStoreError::EntryTooLarge(pair.len()));
            }
            output.extend_from_slice(&pair);
        }
        Ok(output)
    }

    /// The canonical encoded field tuple of an inline row value.
    ///
    /// This is the stored representation a metadata projection decodes: row
    /// fields with their format-3 Blob references. An overflow value keeps its
    /// tuple in the shared graph, so it returns `None` rather than pretending
    /// the bounded metadata here is the row's fields.
    pub fn encoded_fields(&self) -> Option<&[u8]> {
        match self {
            Self::Inline { encoded, .. } => Some(encoded),
            Self::Overflow(_) => None,
        }
    }

    pub fn dependencies(&self) -> Vec<RowDependency> {
        match self {
            Self::Inline { dependencies, .. } => dependencies.clone(),
            Self::Overflow(reference) => {
                if let Some(root) = reference.native_root() {
                    return vec![RowDependency::new(root.clone(), NativeObjectKind::Tree)];
                }
                let mut dependencies = vec![RowDependency::new(
                    reference.byte_root.clone(),
                    NativeObjectKind::Tree,
                )];
                if let Some(root) = reference.dependency_root() {
                    dependencies.push(RowDependency::new(root.clone(), NativeObjectKind::Tree));
                }
                dependencies
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowEntry {
    key: TypedKey,
    value: RowValue,
}

impl RowEntry {
    pub fn new(key: TypedKey, value: RowValue) -> Self {
        Self { key, value }
    }

    pub fn key(&self) -> &TypedKey {
        &self.key
    }

    pub fn value(&self) -> &RowValue {
        &self.value
    }
}

fn encode_key(key: &TypedKey, output: &mut Vec<u8>) -> Result<(), RowStoreError> {
    use TypedKey::*;
    match key {
        Null => output.push(0xf6),
        Bool(false) => output.push(0xf4),
        Bool(true) => output.push(0xf5),
        Int(value) => {
            let (major, argument) = int_head(i128::from(*value));
            cbor_head(output, major, argument);
        }
        UInt(value) => {
            let (major, argument) = int_head(i128::from(*value));
            cbor_head(output, major, argument);
        }
        Text(value) => {
            cbor_head(output, 3, value.len() as u64);
            output.extend_from_slice(value.as_bytes());
        }
        Bytes(value) => {
            cbor_head(output, 2, value.len() as u64);
            output.extend_from_slice(value);
        }
        Tuple(values) => {
            if values.len() > MAX_PAGE_ENTRIES {
                return Err(RowStoreError::KeyTooLarge(values.len()));
            }
            cbor_head(output, 4, values.len() as u64);
            for value in values {
                encode_key(value, output)?;
            }
        }
        Value(value) => output.extend_from_slice(&value.canonical),
    }
    if output.len() > KEY_CANONICAL_LIMIT {
        return Err(RowStoreError::KeyTooLarge(output.len()));
    }
    Ok(())
}

/// Decodes the canonical CBOR field tuple of a stored row value.
///
/// This is the read side of `RowValue::encoded_fields`: a row that is stored
/// inline carries its fields here, and an overflow row has none. It is the
/// decode a metadata-only projection performs, so it never crosses into the
/// graph or reads a payload byte.
pub(crate) fn decode_canonical_fields_of(encoded: &[u8]) -> Result<Vec<CborValue>, RowStoreError> {
    match decode_canonical_cbor(encoded) {
        Ok(CborValue::Array(fields)) => Ok(fields),
        _ => Err(RowStoreError::InvalidCanonicalRowValue),
    }
}

/// Projects one stored row field to payload-free Blob metadata.
///
/// A Blob field is the format-3 ROV-3 tag 60111: `[length, sha256,
/// media_type, suffix, descriptor]`. The descriptor is deliberately not
/// resolved here, so a listing reads the row's own bytes and nothing else.
/// A field that is not a Blob in canonical shape is not a Blob: it yields
/// `None` rather than being reported or rejected. A field that *is* a tagged
/// Blob but whose payload is malformed is a real error, not a silent miss.
pub(crate) fn blob_field_metadata(field: &CborValue) -> Result<Option<BlobMetadata>, RowStoreError> {
    match field {
        CborValue::Tag(60013, payload) => {
            let CborValue::Array(option) = payload.as_ref() else {
                return Err(RowStoreError::InvalidCanonicalRowValue);
            };
            match option.as_slice() {
                [CborValue::Unsigned(0)] => Ok(None),
                [CborValue::Unsigned(1), value] => blob_field_metadata(value),
                _ => Err(RowStoreError::InvalidCanonicalRowValue),
            }
        }
        CborValue::Tag(ROV3_BLOB_TAG, _) => decode_blob_field_metadata(field).map(Some),
        _ => Ok(None),
    }
}

fn decode_blob_field_metadata(field: &CborValue) -> Result<BlobMetadata, RowStoreError> {
    let CborValue::Tag(ROV3_BLOB_TAG, payload) = field else {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    };
    let CborValue::Array(fields) = payload.as_ref() else {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    };
    let [length, sha256, media_type, suffix, descriptor] = fields.as_slice() else {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    };
    let length = match length {
        CborValue::Unsigned(length) => *length,
        _ => return Err(RowStoreError::InvalidCanonicalRowValue),
    };
    let sha256: [u8; 32] = match sha256 {
        CborValue::Bytes(sha256) => sha256
            .as_slice()
            .try_into()
            .map_err(|_| RowStoreError::InvalidCanonicalRowValue)?,
        _ => return Err(RowStoreError::InvalidCanonicalRowValue),
    };
    let media_type = match media_type {
        CborValue::Text(media_type) => media_type,
        _ => return Err(RowStoreError::InvalidCanonicalRowValue),
    };
    let suffix = match suffix {
        CborValue::Null => None,
        CborValue::Text(suffix) => Some(suffix.as_str()),
        _ => return Err(RowStoreError::InvalidCanonicalRowValue),
    };
    let annotation = MediaAnnotation::new(media_type, suffix)
        .map_err(|_| RowStoreError::InvalidCanonicalRowValue)?;
    if annotation.media_type() != media_type || annotation.suffix() != suffix {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    }
    let descriptor = match descriptor {
        CborValue::Bytes(descriptor) => descriptor,
        _ => return Err(RowStoreError::InvalidCanonicalRowValue),
    };
    Ok(BlobMetadata::from_stored_reference(
        length,
        sha256,
        annotation.media_type(),
        annotation.suffix(),
        Some(
            orna_value_v1::NativeOid::from_bytes(descriptor)
                .map_err(|_| RowStoreError::InvalidCanonicalRowValue)?,
        ),
    ))
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let (head, length) = cbor_head_bytes(major, value);
    output.extend_from_slice(&head[..length]);
}

/// The shortest definite CBOR head for `major` and `value`, and its length.
fn cbor_head_bytes(major: u8, value: u64) -> ([u8; 9], usize) {
    let base = major << 5;
    let mut head = [0; 9];
    let length = match value {
        0..=23 => {
            head[0] = base | value as u8;
            1
        }
        24..=255 => {
            head[0] = base | 24;
            head[1] = value as u8;
            2
        }
        256..=65_535 => {
            head[0] = base | 25;
            head[1..3].copy_from_slice(&(value as u16).to_be_bytes());
            3
        }
        65_536..=4_294_967_295 => {
            head[0] = base | 26;
            head[1..5].copy_from_slice(&(value as u32).to_be_bytes());
            5
        }
        _ => {
            head[0] = base | 27;
            head[1..].copy_from_slice(&value.to_be_bytes());
            9
        }
    };
    (head, length)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowStoreError {
    DuplicateDependency,
    KeyTooLarge(usize),
    InlineValueTooLarge(usize),
    InlineDependencyFanout(usize),
    ValueTooLarge(u64),
    EntryTooLarge(usize),
    PageFanoutExceeded(usize),
    BoundaryMismatch,
    VersionIdentityMismatch,
    RowCountExceeded,
    InvalidRangeLimit(usize),
    InvalidKeyRange,
    RowsNotStrictlyOrdered,
    RowCountMismatch,
    PersistedLookupRequiresGraphContext,
    BlobMetadataRequiresGraphContext,
    InvalidCanonicalKey,
    InvalidCanonicalRowValue,
    UnresolvedValueOverflow,
    UnsupportedWriterProfile,
    LegacyFormatReadOnly(u8),
    Native(GraphError),
}

impl fmt::Display for RowStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDependency => f.write_str("duplicate direct row dependency"),
            Self::KeyTooLarge(size) => write!(f, "canonical key is {size} bytes, over 1 MiB"),
            Self::InlineValueTooLarge(size) => {
                write!(f, "inline value is {size} bytes, over 8192")
            }
            Self::InlineDependencyFanout(count) => {
                write!(f, "inline value has {count} direct Blob refs, over 32")
            }
            Self::ValueTooLarge(length) => write!(f, "overflow value length {length} is too large"),
            Self::EntryTooLarge(size) => write!(f, "row entry is {size} bytes, over node bound"),
            Self::PageFanoutExceeded(count) => write!(f, "row page fanout is {count}, over 256"),
            Self::BoundaryMismatch => {
                f.write_str("row lookup boundary does not match map identity")
            }
            Self::VersionIdentityMismatch => {
                f.write_str("row-map version identities or native hash modes disagree")
            }
            Self::RowCountExceeded => f.write_str("row count exceeds the format-3 bound"),
            Self::InvalidRangeLimit(limit) => write!(
                f,
                "row range limit {limit} is outside 1..={MAX_ROW_RANGE_LIMIT}"
            ),
            Self::InvalidKeyRange => {
                f.write_str("row range bounds must share one logical key type and be ordered")
            }
            Self::RowsNotStrictlyOrdered => {
                f.write_str("admitted row keys are not strictly logically ordered")
            }
            Self::RowCountMismatch => f.write_str("row-map count does not match admitted rows"),
            Self::PersistedLookupRequiresGraphContext => {
                f.write_str("persisted row lookup requires its repository graph context")
            }
            Self::BlobMetadataRequiresGraphContext => f.write_str(
                "row Blob metadata lives in the shared overflow graph, so it requires its \
                 repository graph context",
            ),
            Self::InvalidCanonicalKey => {
                f.write_str("persisted row key is not a supported canonical typed key")
            }
            Self::InvalidCanonicalRowValue => {
                f.write_str("persisted row value is not a canonical stored-field tuple")
            }
            Self::UnresolvedValueOverflow => {
                f.write_str("overflow value has no resolved native graph root")
            }
            Self::UnsupportedWriterProfile => f.write_str("legacy writer profile is unsupported"),
            Self::LegacyFormatReadOnly(format) => {
                write!(f, "repository format {format} is reader-only")
            }
            Self::Native(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RowStoreError {}

impl From<GraphError> for RowStoreError {
    fn from(error: GraphError) -> Self {
        Self::Native(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_graph::{GitHashAlgorithm, NativeOid};

    fn oid(byte: u8) -> NativeObjectId {
        NativeOid::new(GitHashAlgorithm::Sha1, [byte; 20]).expect("valid test oid")
    }

    fn admitted_row(generation: u64) -> AdmittedRow {
        let schema = SchemaGeneration::issue([0x11; 16], [0x22; 16], oid(0x33), [0x44; 32], 7);
        let version = RowMapVersion::issue(
            [0x11; 16],
            [0x22; 16],
            oid(0x55),
            schema,
            oid(0x66),
            generation,
            Some(1),
        )
        .expect("coherent row-map version");
        let value = RowValue::decode_canonical_fields(vec![0x80]).expect("empty field tuple");
        AdmittedRow::issue(&version, TypedKey::Text("london".to_owned()), value)
    }
    fn encoded_blob_row(media_type: &str, suffix: Option<&str>) -> Vec<u8> {
        let mut encoded = vec![0x81, 0xd9, 0xea, 0xcf, 0x85, 0x00, 0x58, 0x20];
        encoded.extend_from_slice(&[0; 32]);
        encoded.push(0x60 + media_type.len() as u8);
        encoded.extend_from_slice(media_type.as_bytes());
        if let Some(suffix) = suffix {
            encoded.push(0x60 + suffix.len() as u8);
            encoded.extend_from_slice(suffix.as_bytes());
        } else {
            encoded.push(0xf6);
        }
        encoded.push(0x54);
        encoded.extend_from_slice(&[0x77; 20]);
        encoded
    }

    #[test]
    fn committed_blob_annotation_must_be_exactly_canonical_mime1() {
        let valid = RowValue::decode_canonical_fields(encoded_blob_row("text/plain", None))
            .expect("canonical annotation is admitted");
        let base = admitted_row(8);
        let row = AdmittedRow::issue(base.version(), TypedKey::Text("london".to_owned()), valid);
        let metadata = row
            .blob_metadata(0)
            .expect("project canonical metadata")
            .expect("Blob field");
        assert_eq!(metadata.media_type(), "text/plain");
        assert_eq!(metadata.suffix(), None);

        for (media_type, suffix) in [("TEXT/PLAIN", None), ("text/plain", Some("txt"))] {
            let encoded = encoded_blob_row(media_type, suffix);
            assert!(
                RowValue::decode_canonical_fields(encoded.clone()).is_err(),
                "noncanonical annotation {media_type:?} {suffix:?} must fail row admission"
            );
            let fields = decode_canonical_fields_of(&encoded).expect("well-formed CBOR tuple");
            assert!(decode_blob_field_metadata(&fields[0]).is_err());
        }
    }

    #[test]
    fn materialized_range_reads_a_bounded_interval_and_keeps_snapshot_pin() {
        let schema = SchemaGeneration::issue([0x11; 16], [0x22; 16], oid(0x33), [0x44; 32], 7);
        let version = RowMapVersion::issue(
            [0x11; 16],
            [0x22; 16],
            oid(0x55),
            schema,
            oid(0x66),
            8,
            Some(600),
        )
        .expect("coherent row-map version");
        let value = RowValue::decode_canonical_fields(vec![0x80]).expect("empty row value");
        let entries = (0..600)
            .map(|key| RowEntry::new(TypedKey::Int(key), value.clone()))
            .collect::<Vec<_>>();
        let snapshot = RowMapSnapshot::issue(version.clone(), [0x77; 32], entries)
            .expect("seal materialized snapshot");
        let start = 63;
        let range = KeyRange::new(
            Some(TypedKey::Int(start)),
            Some(TypedKey::Int(start + 4)),
            3,
        )
        .expect("valid bounded range");

        let rows = snapshot.range(&range).expect("read bounded range");
        assert_eq!(
            rows.iter()
                .map(AdmittedRow::key)
                .cloned()
                .collect::<Vec<_>>(),
            [
                TypedKey::Int(start),
                TypedKey::Int(start + 1),
                TypedKey::Int(start + 2),
            ]
        );
        assert!(rows.iter().all(|row| row.version() == &version));
    }

    /// Seals an in-memory row map over `keys`, which must be strictly
    /// ascending and of one key type.
    fn sealed_snapshot(keys: &[TypedKey]) -> RowMapSnapshot {
        let schema = SchemaGeneration::issue([0x11; 16], [0x22; 16], oid(0x33), [0x44; 32], 7);
        let version = RowMapVersion::issue(
            [0x11; 16],
            [0x22; 16],
            oid(0x55),
            schema,
            oid(0x66),
            8,
            Some(keys.len() as u64),
        )
        .expect("coherent row-map version");
        let value = RowValue::decode_canonical_fields(vec![0x80]).expect("empty row value");
        let entries = keys
            .iter()
            .map(|key| RowEntry::new(key.clone(), value.clone()))
            .collect();
        RowMapSnapshot::issue(version, [0x77; 32], entries).expect("seal row map")
    }

    /// The keys a resume cursor must return: strictly after `after`, inside
    /// the bounds, at most `limit` of them, in key order.
    fn expected_after(
        keys: &[TypedKey],
        lower: Option<&TypedKey>,
        upper: Option<&TypedKey>,
        after: &TypedKey,
        limit: usize,
    ) -> Vec<TypedKey> {
        keys.iter()
            .filter(|key| lower.is_none_or(|lower| *key >= lower))
            .filter(|key| upper.is_none_or(|upper| *key < upper))
            .filter(|key| *key > after)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Asserts that a resume cursor returns exactly the sorted suffix that
    /// [`expected_after`] names, and that a cursor at or above the upper
    /// bound is refused. Cursors may be present, absent or outside the bounds.
    fn assert_resume_skips_to_the_sorted_suffix(
        keys: &[TypedKey],
        lower: Option<TypedKey>,
        upper: Option<TypedKey>,
        cursors: &[TypedKey],
        limit: usize,
    ) {
        let snapshot = sealed_snapshot(keys);
        for cursor in cursors {
            let range = KeyRange::new(lower.clone(), upper.clone(), limit).expect("valid range");
            if upper.as_ref().is_some_and(|upper| cursor >= upper) {
                assert!(
                    range.with_resume_after(cursor.clone()).is_err(),
                    "cursor {cursor:?} at or above the upper bound is refused"
                );
                continue;
            }
            let resumed = range
                .with_resume_after(cursor.clone())
                .expect("cursor below the upper bound");
            let rows: Vec<TypedKey> = snapshot
                .range(&resumed)
                .expect("read the resumed range")
                .iter()
                .map(|row| row.key().clone())
                .collect();
            assert_eq!(
                rows,
                expected_after(keys, lower.as_ref(), upper.as_ref(), cursor, limit),
                "resume after {cursor:?} with limit {limit}"
            );
        }
    }

    /// Pages every key of `[lower, upper)` with `limit` rows per page, each
    /// page resuming after the last key the previous page returned.
    fn page_through(
        snapshot: &RowMapSnapshot,
        lower: Option<TypedKey>,
        upper: Option<TypedKey>,
        limit: usize,
    ) -> Vec<TypedKey> {
        let base = KeyRange::new(lower, upper, limit).expect("valid page range");
        let mut collected = Vec::new();
        let mut range = base.clone();
        loop {
            let page: Vec<TypedKey> = snapshot
                .range(&range)
                .expect("read one page")
                .iter()
                .map(|row| row.key().clone())
                .collect();
            let short = page.len() < limit;
            let last = page.last().cloned();
            collected.extend(page);
            let Some(last) = last.filter(|_| !short) else {
                break;
            };
            range = base
                .clone()
                .with_resume_after(last)
                .expect("resume cursor inside the range");
        }
        collected
    }

    /// ORNA-ROW-002: `Int` and `UInt` share one key rank, so a resume cursor
    /// on either spelling skips the same keys across zero. Cursors below the
    /// first key, on the first, middle and last keys, on absent keys, and
    /// past the last key are each exclusive.
    #[test]
    fn resume_skip_is_exclusive_for_mixed_int_spellings_across_zero() {
        let keys = vec![
            TypedKey::Int(-9),
            TypedKey::Int(-2),
            TypedKey::Int(-1),
            TypedKey::UInt(0),
            TypedKey::Int(3),
            TypedKey::UInt(4),
            TypedKey::UInt(10),
        ];
        assert_logical_order(&keys);
        let cursors = [
            TypedKey::Int(-100),
            TypedKey::Int(-9),
            TypedKey::Int(-5),
            TypedKey::UInt(0),
            TypedKey::Int(2),
            TypedKey::UInt(10),
            TypedKey::UInt(99),
        ];
        for limit in [1, 2, 7] {
            assert_resume_skips_to_the_sorted_suffix(&keys, None, None, &cursors, limit);
        }
        let snapshot = sealed_snapshot(&keys);
        for limit in [1, 2, 3, 256] {
            assert_eq!(page_through(&snapshot, None, None, limit), keys, "limit {limit}");
        }
    }

    /// ORNA-ROW-002: a resume cursor is checked against the range's bounds.
    /// A cursor below the lower bound returns the intersection with the
    /// bounds; a cursor at or above the upper bound is refused. Tuple keys
    /// compare component by component.
    #[test]
    fn resume_skip_respects_tuple_bounds_and_refuses_a_cursor_at_the_upper_bound() {
        let keys: Vec<TypedKey> = (0..3_i64)
            .flat_map(|first| {
                ["a", "b", "c"].into_iter().map(move |second| {
                    TypedKey::Tuple(vec![
                        TypedKey::Int(first),
                        TypedKey::Text(second.to_owned()),
                    ])
                })
            })
            .collect();
        let pair = |first: i64, second: &str| {
            TypedKey::Tuple(vec![TypedKey::Int(first), TypedKey::Text(second.to_owned())])
        };
        let lower = pair(1, "b");
        let upper = pair(2, "b");
        let in_range: Vec<TypedKey> = keys
            .iter()
            .filter(|key| *key >= &lower && *key < &upper)
            .cloned()
            .collect();
        assert_eq!(in_range.len(), 3, "(1,b), (1,c) and (2,a) lie in [(1,b), (2,b))");
        let cursors = [
            pair(0, "z"),
            pair(1, "b"),
            pair(1, "bb"),
            pair(1, "c"),
            pair(2, "a"),
            pair(2, "b"),
            pair(2, "c"),
        ];
        for limit in [1, 2, 256] {
            assert_resume_skips_to_the_sorted_suffix(
                &keys,
                Some(lower.clone()),
                Some(upper.clone()),
                &cursors,
                limit,
            );
        }
        let snapshot = sealed_snapshot(&keys);
        for limit in [1, 2, 256] {
            assert_eq!(
                page_through(&snapshot, Some(lower.clone()), Some(upper.clone()), limit),
                in_range,
                "limit {limit}"
            );
        }
    }

    /// Paging crosses each page boundary by the resume cursor alone, for Text,
    /// Bytes and Bool keys, and a composite key whose canonical encoding is
    /// exactly 256 bytes pages in order beside a 257-byte neighbour.
    #[test]
    fn paging_is_exact_for_text_bytes_bool_and_a_256_byte_tuple_key() {
        let texts: Vec<TypedKey> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|text| TypedKey::Text(text.to_owned()))
            .collect();
        let bytes: Vec<TypedKey> = [vec![0x00], vec![0x00, 0x01], vec![0x01], vec![0xff]]
            .into_iter()
            .map(TypedKey::Bytes)
            .collect();
        let bools = vec![TypedKey::Bool(false), TypedKey::Bool(true)];
        for keys in [&texts, &bytes, &bools] {
            assert_logical_order(keys);
            let snapshot = sealed_snapshot(keys);
            for limit in [1, 2, 3, 256] {
                assert_eq!(page_through(&snapshot, None, None, limit), *keys, "limit {limit}");
            }
        }

        let tuple_of = |text_len: usize, last: i64| {
            TypedKey::Tuple(vec![
                TypedKey::Text("x".repeat(text_len)),
                TypedKey::Int(last),
            ])
        };
        let canonical_len = |key: &TypedKey| key.canonical_bytes().expect("canonical key").len();
        let text_len = (0..512)
            .find(|&len| canonical_len(&tuple_of(len, 1)) == 256)
            .expect("some text length encodes the tuple in exactly 256 bytes");
        let exact = tuple_of(text_len, 1);
        let longer = tuple_of(text_len + 1, 1);
        let keys = vec![
            TypedKey::Tuple(vec![TypedKey::Text("a".to_owned()), TypedKey::Int(1)]),
            exact,
            longer,
        ];
        assert_eq!(canonical_len(&keys[1]), 256);
        assert_eq!(canonical_len(&keys[2]), 257);
        assert_logical_order(&keys);
        let snapshot = sealed_snapshot(&keys);
        for limit in [1, 2, 256] {
            assert_eq!(page_through(&snapshot, None, None, limit), keys, "limit {limit}");
        }
    }

    /// ORNA-ROW-002: a resume cursor at or above the upper bound is refused
    /// for Text keys, and one just below it returns nothing.
    #[test]
    fn resume_cursor_at_or_above_a_text_upper_bound_is_refused() {
        let keys: Vec<TypedKey> = (0..4)
            .map(|index| TypedKey::Text(format!("row-{index}")))
            .collect();
        assert_resume_skips_to_the_sorted_suffix(
            &keys,
            None,
            Some(TypedKey::Text("row-2".to_owned())),
            &[
                TypedKey::Text("row-1".to_owned()),
                TypedKey::Text("row-2".to_owned()),
                TypedKey::Text("row-9".to_owned()),
            ],
            2,
        );
    }

    fn key_of(raw: Raw) -> TypedKey {
        TypedKey::from_value(&Value::new(raw).expect("registered canonical value"))
            .expect("permitted key value")
    }

    fn uuid_key_raw(byte: u8) -> Raw {
        Raw::Tag(37, Box::new(Raw::Bytes(vec![byte; 16])))
    }

    fn hash_of(key: &TypedKey) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Asserts `keys` ascend strictly, survive their canonical encoding
    /// unchanged, and sort back into place from reverse order.
    fn assert_logical_order(keys: &[TypedKey]) {
        for pair in keys.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{:?} sorts before {:?}",
                pair[0],
                pair[1]
            );
        }
        for key in keys {
            let bytes = key.canonical_bytes().expect("canonical key bytes");
            let decoded = TypedKey::decode_canonical(&bytes).expect("decode the canonical key");
            assert_eq!(&decoded, key);
            assert_eq!(decoded.canonical_bytes().unwrap(), bytes);
            assert_eq!(hash_of(&decoded), hash_of(key));
        }
        let mut sorted = keys.iter().rev().cloned().collect::<Vec<_>>();
        sorted.sort();
        assert_eq!(sorted, keys);
    }

    /// ORNA-ROW-002: `Int` and `UInt` spell one key type, so a range may mix
    /// them; bounds of different key types still fail.
    #[test]
    fn key_range_spans_one_int_type_and_rejects_other_key_types() {
        let range = KeyRange::new(Some(TypedKey::Int(1)), Some(TypedKey::UInt(2)), 1)
            .expect("Int and UInt bounds share the Int key type");
        assert_eq!(range.upper_exclusive(), Some(&TypedKey::Int(2)));
        assert!(matches!(
            KeyRange::new(Some(TypedKey::UInt(2)), Some(TypedKey::Int(2)), 1),
            Err(RowStoreError::InvalidKeyRange)
        ));
        assert!(matches!(
            KeyRange::new(Some(TypedKey::Int(1)), Some(TypedKey::Text("2".into())), 1),
            Err(RowStoreError::InvalidKeyRange)
        ));
        let composite = |id: TypedKey| TypedKey::Tuple(vec![TypedKey::Text("id".into()), id]);
        KeyRange::new(
            Some(composite(TypedKey::Int(1))),
            Some(composite(TypedKey::UInt(2))),
            1,
        )
        .expect("composite Int components share one key type");
        assert!(matches!(
            KeyRange::new(
                Some(composite(TypedKey::Int(1))),
                Some(composite(TypedKey::Text("2".into()))),
                1,
            ),
            Err(RowStoreError::InvalidKeyRange)
        ));
        let date = |text: &str| key_of(Raw::Tag(60_001, Box::new(Raw::Text(text.to_owned()))));
        KeyRange::new(Some(date("2026-01-01")), Some(date("2026-02-01")), 1)
            .expect("Date bounds share one key type");
        assert!(matches!(
            KeyRange::new(Some(date("2026-01-01")), Some(key_of(uuid_key_raw(1))), 1),
            Err(RowStoreError::InvalidKeyRange)
        ));
    }

    /// A07-storage-rows-20: one Int is one key whichever 64-bit variant spells
    /// it: equal, hashed alike, encoded alike and ordered by value.
    #[test]
    fn int_and_uint_spellings_are_one_key() {
        assert_eq!(TypedKey::Int(5), TypedKey::UInt(5));
        assert_eq!(hash_of(&TypedKey::Int(5)), hash_of(&TypedKey::UInt(5)));
        assert_eq!(TypedKey::Int(5).cmp(&TypedKey::UInt(5)), Ordering::Equal);
        assert_eq!(
            TypedKey::Int(5).canonical_bytes().unwrap(),
            TypedKey::UInt(5).canonical_bytes().unwrap()
        );
        assert!(TypedKey::Int(5) > TypedKey::UInt(3));
        assert!(TypedKey::Int(-1) < TypedKey::UInt(0));
        assert!(TypedKey::UInt(u64::MAX) > TypedKey::Int(i64::MAX));

        let composite = |id: TypedKey| TypedKey::Tuple(vec![TypedKey::Text("id".into()), id]);
        assert_eq!(composite(TypedKey::Int(7)), composite(TypedKey::UInt(7)));
        assert_eq!(
            hash_of(&composite(TypedKey::Int(7))),
            hash_of(&composite(TypedKey::UInt(7)))
        );
        let mut keys = std::collections::HashSet::new();
        keys.insert(TypedKey::UInt(3));
        assert!(keys.contains(&TypedKey::Int(3)));

        let mut map = std::collections::BTreeSet::new();
        for key in [
            TypedKey::UInt(3),
            TypedKey::Int(-2),
            TypedKey::Int(5),
            TypedKey::UInt(4),
        ] {
            assert!(map.insert(key), "distinct Int keys");
        }
        assert!(
            !map.insert(TypedKey::Int(3)),
            "Int(3) is the key UInt(3) already holds"
        );
        assert_eq!(
            map.iter().cloned().collect::<Vec<_>>(),
            [
                TypedKey::Int(-2),
                TypedKey::Int(3),
                TypedKey::Int(4),
                TypedKey::Int(5)
            ]
        );
        assert!(map.contains(&TypedKey::Int(4)));
    }

    /// A07-storage-rows-02, ORNA-ROW-002: every permitted tagged key decodes
    /// from its canonical encoding and orders by logical value through
    /// `compare_primary_keys`, not by CBOR bytes.
    #[test]
    fn tagged_keys_round_trip_and_order_by_logical_value() {
        use num_bigint::BigInt;

        let pair = |tag: u64, first: Raw, second: Raw| {
            Raw::Tag(tag, Box::new(Raw::Array(vec![first, second])))
        };
        let decimal = |coefficient: i64, exponent: i64| {
            key_of(pair(
                60_000,
                Raw::Int(coefficient.into()),
                Raw::Int(exponent.into()),
            ))
        };
        let decimals = [
            decimal(-1, 1),
            decimal(-2, 0),
            decimal(0, 0),
            decimal(5, -1),
            decimal(2, 0),
            decimal(1, 1),
        ];
        assert_logical_order(&decimals);
        // 2 sorts before 10 although the encoding of 10 sorts first.
        assert!(decimals[4].canonical_bytes().unwrap() > decimals[5].canonical_bytes().unwrap());

        let instant = |seconds: i64, nanos: u32| {
            key_of(pair(
                60_002,
                Raw::Int(seconds.into()),
                Raw::Int(nanos.into()),
            ))
        };
        assert_logical_order(&[
            instant(-5, 0),
            instant(-1, 999_999_999),
            instant(0, 0),
            instant(3, 1),
        ]);
        let date = |text: &str| key_of(Raw::Tag(60_001, Box::new(Raw::Text(text.to_owned()))));
        assert_logical_order(&[date("1999-12-31"), date("2026-01-02"), date("2026-10-20")]);
        assert_logical_order(&[
            key_of(uuid_key_raw(0x01)),
            key_of(uuid_key_raw(0x7f)),
            key_of(uuid_key_raw(0xff)),
        ]);
        let variant = |enum_type: u8, variant: u8| {
            key_of(Raw::Tag(
                60_008,
                Box::new(Raw::Array(vec![
                    uuid_key_raw(enum_type),
                    uuid_key_raw(variant),
                    Raw::Null,
                ])),
            ))
        };
        assert_logical_order(&[variant(1, 1), variant(1, 2), variant(2, 1)]);
        let tuple = |number: i64, text: &str| {
            key_of(pair(
                60_015,
                Raw::Int(number.into()),
                Raw::Text(text.to_owned()),
            ))
        };
        assert_logical_order(&[tuple(-2, "z"), tuple(1, "a"), tuple(1, "b"), tuple(10, "a")]);
        let reference = |number: i64| {
            key_of(Raw::Tag(
                60_021,
                Box::new(Raw::Array(vec![
                    uuid_key_raw(3),
                    uuid_key_raw(4),
                    Raw::Int(number.into()),
                ])),
            ))
        };
        assert_logical_order(&[reference(-1), reference(2)]);

        // Ints beyond the 64-bit spellings order with them by value.
        let wide = |value: BigInt| key_of(Raw::Int(value));
        let ints = [
            wide(-BigInt::from(u64::MAX) - 2),
            wide(BigInt::from(i64::MIN) - 1),
            TypedKey::Int(i64::MIN),
            TypedKey::Int(-1),
            TypedKey::UInt(0),
            TypedKey::UInt(u64::MAX),
            wide(BigInt::from(u64::MAX) + 1),
        ];
        for index in [0, 1, 6] {
            assert!(matches!(&ints[index], TypedKey::Value(_)));
        }
        assert_eq!(
            key_of(Raw::Int(BigInt::from(i64::MIN))),
            TypedKey::Int(i64::MIN)
        );
        assert_logical_order(&ints);

        let text = |key: &TypedKey| match key {
            TypedKey::Value(value) => value.canonical_text(),
            _ => panic!("a tagged key"),
        };
        assert_eq!(
            text(&key_of(uuid_key_raw(0xab))).as_deref(),
            Some("abababab-abab-abab-abab-abababababab")
        );
        assert_eq!(text(&date("2026-10-20")).as_deref(), Some("2026-10-20"));
        assert_eq!(text(&decimal(15, -1)).as_deref(), Some("15e-1.decimal"));
        assert_eq!(text(&ints[6]).as_deref(), Some("18446744073709551616"));
        assert_eq!(text(&instant(0, 0)), None);
    }

    /// Values outside the permitted key types fail key admission explicitly,
    /// as do unregistered tags and noncanonical bignums.
    #[test]
    fn non_key_values_are_rejected_as_keys() {
        let encoded = |raw: Raw| {
            Value::new(raw)
                .expect("registered canonical value")
                .encode()
                .expect("canonical value bytes")
        };
        let payload = Raw::Tag(
            60_009,
            Box::new(Raw::Array(vec![Raw::Null, Raw::Array(Vec::new())])),
        );
        for raw in [
            Raw::Float(1.5f64.to_bits()),
            Raw::Map(vec![(Raw::Text("k".to_owned()), Raw::Int(1.into()))]),
            // Duration and Option are stored values but never keys.
            Raw::Tag(
                60_005,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
            ),
            Raw::Tag(60_013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
            // An enum variant carrying a payload record.
            Raw::Tag(
                60_008,
                Box::new(Raw::Array(vec![uuid_key_raw(1), uuid_key_raw(2), payload])),
            ),
            // A tuple with a Float component.
            Raw::Tag(
                60_015,
                Box::new(Raw::Array(vec![Raw::Float(0.5f64.to_bits())])),
            ),
        ] {
            assert!(matches!(
                TypedKey::decode_canonical(&encoded(raw)),
                Err(RowStoreError::InvalidCanonicalKey)
            ));
        }

        let tagged = |tag: u64, payload: &[u8]| {
            let mut bytes = Vec::new();
            cbor_head(&mut bytes, 6, tag);
            bytes.extend_from_slice(payload);
            bytes
        };
        let mut overflow = vec![0x81, 0x54];
        overflow.extend_from_slice(&[0x77; 20]);
        assert!(matches!(
            TypedKey::decode_canonical(&tagged(60_113, &overflow)),
            Err(RowStoreError::InvalidCanonicalKey)
        ));
        let mut narrow = vec![0x48];
        narrow.extend_from_slice(&[0xff; 8]);
        for bytes in [
            tagged(60_026, &[0x80]),
            tagged(1, &[0x00]),
            tagged(2, &narrow),
        ] {
            assert!(matches!(
                TypedKey::decode_canonical(&bytes),
                Err(RowStoreError::Native(GraphError::NonCanonicalData))
            ));
        }
    }

    #[test]
    fn lazy_snapshot_range_requires_graph_context_instead_of_empty_result() {
        let schema = SchemaGeneration::issue([0x11; 16], [0x22; 16], oid(0x33), [0x44; 32], 7);
        let version = RowMapVersion::issue(
            [0x11; 16],
            [0x22; 16],
            oid(0x55),
            schema,
            oid(0x66),
            8,
            Some(1),
        )
        .expect("coherent row-map version");
        let snapshot = RowMapSnapshot::issue_lazy(version, [0x77; 32])
            .expect("seal persisted snapshot without materializing rows");
        let range = KeyRange::new(None, None, 1).expect("valid bounded range");
        assert!(matches!(
            snapshot.range(&range),
            Err(RowStoreError::PersistedLookupRequiresGraphContext)
        ));
    }

    #[test]
    fn repository_vfs_row_identity_is_issued_from_admitted_orp_row() {
        let row = admitted_row(8);
        let identity =
            RepositoryVfsRowIdentity::from_admitted_row(&row).expect("valid row identity");

        assert_eq!(identity.database_id(), &[0x11; 16]);
        assert_eq!(identity.relation_id(), &[0x22; 16]);
        assert_eq!(identity.store_root(), &oid(0x55));
        assert_eq!(identity.primary_root(), &oid(0x66));
        assert_eq!(identity.schema_digest(), &[0x44; 32]);
        assert_eq!(identity.generation(), 8);
        assert_eq!(identity.key(), row.key());
        assert_eq!(
            identity.canonical_key(),
            row.key().canonical_bytes().unwrap()
        );
    }

    #[test]
    fn repository_vfs_field_identity_binds_row_and_schema_field_path() {
        let row_identity =
            RepositoryVfsRowIdentity::from_admitted_row(&admitted_row(8)).expect("row identity");
        let title = RepositoryVfsFieldIdentity::new(row_identity.clone(), vec![[0x77; 16]])
            .expect("field identity");
        let cover = RepositoryVfsFieldIdentity::new(row_identity.clone(), vec![[0x78; 16]])
            .expect("field identity");
        let later_row =
            RepositoryVfsRowIdentity::from_admitted_row(&admitted_row(9)).expect("row identity");
        let later_title =
            RepositoryVfsFieldIdentity::new(later_row, vec![[0x77; 16]]).expect("field identity");

        assert_eq!(title.row(), &row_identity);
        assert_eq!(title.field_path(), &[[0x77; 16]]);
        assert_ne!(title.projection_digest(), cover.projection_digest());
        assert_ne!(title.projection_digest(), later_title.projection_digest());
        assert!(matches!(
            RepositoryVfsFieldIdentity::new(row_identity, vec![[0; 16]]),
            Err(RowStoreError::VersionIdentityMismatch)
        ));
    }
}
