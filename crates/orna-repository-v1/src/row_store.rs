//! ORP-1 typed row-map and overflow primitives for repository format 3.
//!
//! This is a canonical map builder, not a loose-row, Parquet, compact, or
//! hybrid writer.  The repository context will later bind the returned map to
//! an admitted native store root and schema.  Until then the boundary remains
//! an explicit value and no raw row path is an admission API.

use std::{cmp::Ordering, collections::BTreeMap, fmt, sync::Arc};

use sha2::{Digest, Sha256};

use orna_value_v1::{BlobMetadata, MimeRegistry};

use crate::native_graph::{
    CborValue, GraphError, MAX_REFS, NODE_DATA_LIMIT, NativeObjectId, NativeObjectKind,
    decode_canonical_cbor, row_fields_dependencies,
};

pub const ROW_INLINE_LIMIT: usize = 8_192;
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
/// bound, and maximum number of returned rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyRange {
    lower_inclusive: Option<TypedKey>,
    upper_exclusive: Option<TypedKey>,
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
            .is_some_and(|(lower, upper)| lower >= upper)
        {
            return Err(RowStoreError::InvalidKeyRange);
        }
        Ok(Self {
            lower_inclusive,
            upper_exclusive,
            limit,
        })
    }

    pub fn lower_inclusive(&self) -> Option<&TypedKey> {
        self.lower_inclusive.as_ref()
    }

    pub fn upper_exclusive(&self) -> Option<&TypedKey> {
        self.upper_exclusive.as_ref()
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
        decode_blob_field_metadata(field).map(Some)
    }
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
        Ok(entries
            .iter()
            .filter(|entry| {
                range
                    .lower_inclusive
                    .as_ref()
                    .map_or(true, |lower| entry.key >= *lower)
                    && range
                        .upper_exclusive
                        .as_ref()
                        .map_or(true, |upper| entry.key < *upper)
            })
            .take(range.limit)
            .map(|entry| AdmittedRow::from_entry(&self.version, entry))
            .collect())
    }
}

/// The identity carried across the row-root/key boundary.  A key is never
/// looked up against a schema or store root different from this identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowMapBoundary {
    database_id: [u8; 16],
    store_root: NativeObjectId,
    schema_digest: [u8; 32],
    relation_id: [u8; 16],
}

impl RowMapBoundary {
    pub fn new(
        database_id: [u8; 16],
        store_root: NativeObjectId,
        schema_digest: [u8; 32],
        relation_id: [u8; 16],
    ) -> Self {
        Self {
            database_id,
            store_root,
            schema_digest,
            relation_id,
        }
    }

    pub fn database_id(&self) -> &[u8; 16] {
        &self.database_id
    }

    pub fn store_root(&self) -> &NativeObjectId {
        &self.store_root
    }

    pub fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }
}

/// The bounded set of logical key values used by the row-map seam.  Ordering
/// is logical type/value ordering, never filename, encoded-byte, or hash
/// order.  The enum intentionally excludes floating point and opaque storage
/// OIDs from primary keys.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TypedKey {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Text(String),
    Bytes(Vec<u8>),
    Tuple(Vec<TypedKey>),
}

impl TypedKey {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RowStoreError> {
        let mut output = Vec::new();
        encode_key(self, &mut output)?;
        if output.len() > KEY_CANONICAL_LIMIT {
            return Err(RowStoreError::KeyTooLarge(output.len()));
        }
        Ok(output)
    }

    pub(crate) fn decode_canonical(bytes: &[u8]) -> Result<Self, RowStoreError> {
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
            CborValue::Negative(magnitude) => {
                let value = -1i128 - i128::from(magnitude);
                Ok(Self::Int(
                    i64::try_from(value).map_err(|_| RowStoreError::InvalidCanonicalKey)?,
                ))
            }
            CborValue::Text(value) => Ok(Self::Text(value)),
            CborValue::Bytes(value) => Ok(Self::Bytes(value)),
            CborValue::Array(values) => Ok(Self::Tuple(
                values
                    .into_iter()
                    .map(Self::from_cbor)
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            CborValue::Map(_) | CborValue::Tag(_, _) | CborValue::Float64(_) => {
                Err(RowStoreError::InvalidCanonicalKey)
            }
        }
    }
}

impl Ord for TypedKey {
    fn cmp(&self, other: &Self) -> Ordering {
        use TypedKey::*;
        let rank = |key: &TypedKey| match key {
            Null => 0,
            Bool(_) => 1,
            Int(_) => 2,
            UInt(_) => 3,
            Text(_) => 4,
            Bytes(_) => 5,
            Tuple(_) => 6,
        };
        rank(self)
            .cmp(&rank(other))
            .then_with(|| match (self, other) {
                (Null, Null) => Ordering::Equal,
                (Bool(left), Bool(right)) => left.cmp(right),
                (Int(left), Int(right)) => left.cmp(right),
                (UInt(left), UInt(right)) => left.cmp(right),
                (Text(left), Text(right)) => left.as_bytes().cmp(right.as_bytes()),
                (Bytes(left), Bytes(right)) => left.cmp(right),
                (Tuple(left), Tuple(right)) => left.cmp(right),
                _ => Ordering::Equal,
            })
    }
}

impl PartialOrd for TypedKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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
    pub fn inline(
        encoded: Vec<u8>,
        dependencies: Vec<RowDependency>,
    ) -> Result<Self, RowStoreError> {
        if encoded.len() > ROW_INLINE_LIMIT {
            return Err(RowStoreError::InlineValueTooLarge(encoded.len()));
        }
        if dependencies.len() > 32 {
            return Err(RowStoreError::InlineDependencyFanout(dependencies.len()));
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

    pub fn encoded_metadata_len(&self) -> usize {
        match self {
            Self::Inline { encoded, .. } => encoded.len(),
            Self::Overflow(reference) => {
                1 + 8
                    + 32
                    + reference
                        .native_root()
                        .map_or(0, |root| root.as_bytes().len())
                    + reference.byte_root().as_bytes().len()
                    + reference
                        .dependency_root()
                        .map_or(1, |root| root.as_bytes().len())
            }
        }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowPage {
    height: u8,
    entries: Vec<RowEntry>,
    encoded_size: usize,
    direct_refs: usize,
}

impl RowPage {
    pub fn height(&self) -> u8 {
        self.height
    }

    pub fn entries(&self) -> &[RowEntry] {
        &self.entries
    }

    pub const fn encoded_size(&self) -> usize {
        self.encoded_size
    }

    pub const fn direct_refs(&self) -> usize {
        self.direct_refs
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedRowMap {
    boundary: RowMapBoundary,
    entries: Vec<RowEntry>,
    pages: Vec<RowPage>,
}

impl OrderedRowMap {
    pub fn boundary(&self) -> &RowMapBoundary {
        &self.boundary
    }

    pub fn entries(&self) -> &[RowEntry] {
        &self.entries
    }

    pub fn pages(&self) -> &[RowPage] {
        &self.pages
    }

    /// The boundary identity is part of lookup, so a row map cannot be used
    /// with a different store root or schema pin by accident.
    pub fn lookup(
        &self,
        boundary: &RowMapBoundary,
        key: &TypedKey,
    ) -> Result<Option<&RowEntry>, RowStoreError> {
        if boundary != &self.boundary {
            return Err(RowStoreError::BoundaryMismatch);
        }
        Ok(self
            .entries
            .binary_search_by(|entry| entry.key.cmp(key))
            .ok()
            .map(|index| &self.entries[index]))
    }
}

pub struct RowMapBuilder {
    boundary: RowMapBoundary,
    entries: BTreeMap<TypedKey, RowValue>,
}

impl RowMapBuilder {
    pub fn new(boundary: RowMapBoundary) -> Self {
        Self {
            boundary,
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: TypedKey, value: RowValue) -> Result<(), RowStoreError> {
        let key_size = key.canonical_bytes()?.len();
        if key_size > KEY_CANONICAL_LIMIT {
            return Err(RowStoreError::KeyTooLarge(key_size));
        }
        if value.encoded_metadata_len() > ROW_CANONICAL_LIMIT {
            return Err(RowStoreError::ValueTooLarge(
                value.encoded_metadata_len() as u64
            ));
        }
        if self.entries.insert(key, value).is_some() {
            return Err(RowStoreError::DuplicateKey);
        }
        Ok(())
    }

    pub fn build(self) -> Result<OrderedRowMap, RowStoreError> {
        let entries = self
            .entries
            .into_iter()
            .map(|(key, value)| RowEntry::new(key, value))
            .collect::<Vec<_>>();
        let pages = partition_pages(&entries, 0)?;
        Ok(OrderedRowMap {
            boundary: self.boundary,
            entries,
            pages,
        })
    }
}

fn partition_pages(entries: &[RowEntry], height: u8) -> Result<Vec<RowPage>, RowStoreError> {
    let mut pages = Vec::new();
    let mut current = Vec::new();
    let mut current_size = 16usize;
    let mut current_refs = 0usize;
    for entry in entries {
        let size = entry_encoded_size(entry)?;
        let refs = entry.value.dependencies().len();
        if refs > MAX_REFS {
            return Err(RowStoreError::PageFanoutExceeded(refs));
        }
        if size + 16 > NODE_DATA_LIMIT {
            return Err(RowStoreError::EntryTooLarge(size));
        }
        let would_overflow = !current.is_empty()
            && (current.len() == MAX_PAGE_ENTRIES
                || current_refs + refs > MAX_REFS
                || current_size + size > NODE_DATA_LIMIT);
        if would_overflow {
            pages.push(RowPage {
                height,
                entries: std::mem::take(&mut current),
                encoded_size: current_size,
                direct_refs: current_refs,
            });
            current_size = 16;
            current_refs = 0;
        }
        current_size += size;
        current_refs += refs;
        current.push(entry.clone());
        let anchor = boundary_anchor(height, entry.key())?;
        if current.len() >= MIN_PAGE_ENTRIES
            && (anchor || current.len() == MAX_PAGE_ENTRIES)
            && !current.is_empty()
        {
            pages.push(RowPage {
                height,
                entries: std::mem::take(&mut current),
                encoded_size: current_size,
                direct_refs: current_refs,
            });
            current_size = 16;
            current_refs = 0;
        }
    }
    if !current.is_empty() {
        pages.push(RowPage {
            height,
            entries: current,
            encoded_size: current_size,
            direct_refs: current_refs,
        });
    }
    Ok(pages)
}

fn entry_encoded_size(entry: &RowEntry) -> Result<usize, RowStoreError> {
    let key = entry.key.canonical_bytes()?;
    let size = key
        .len()
        .checked_add(entry.value.encoded_metadata_len())
        .and_then(|size| size.checked_add(8))
        .ok_or(RowStoreError::EntryTooLarge(usize::MAX))?;
    Ok(size)
}

fn boundary_anchor(height: u8, key: &TypedKey) -> Result<bool, RowStoreError> {
    let canonical = key.canonical_bytes()?;
    let mut hasher = Sha256::new();
    hasher.update(ORP_DOMAIN);
    hasher.update(u32::from(height).to_be_bytes());
    hasher.update(canonical);
    let digest = hasher.finalize();
    Ok(digest[31] & ((1 << ORP_ANCHOR_BITS) - 1) == 0)
}

fn encode_key(key: &TypedKey, output: &mut Vec<u8>) -> Result<(), RowStoreError> {
    use TypedKey::*;
    match key {
        Null => output.push(0xf6),
        Bool(false) => output.push(0xf4),
        Bool(true) => output.push(0xf5),
        Int(value) => {
            if *value >= 0 {
                cbor_head(output, 0, *value as u64);
            } else {
                let magnitude = value
                    .checked_neg()
                    .map(|value| value as u64)
                    .unwrap_or(i64::MAX as u64 + 1);
                cbor_head(output, 1, magnitude - 1);
            }
        }
        UInt(value) => cbor_head(output, 0, *value),
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
/// A field that is not a Blob in canonical shape is not a Blob.
fn decode_blob_field_metadata(field: &CborValue) -> Result<BlobMetadata, RowStoreError> {
    let CborValue::Tag(ROV3_BLOB_TAG, payload) = field else {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    };
    let CborValue::Array(fields) = payload.as_ref() else {
        return Err(RowStoreError::InvalidCanonicalRowValue);
    };
    let [length, sha256, media_type, suffix, _descriptor] = fields.as_slice() else {
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
    let annotation = MimeRegistry::mime1()
        .canonical_annotation(media_type, suffix)
        .map_err(|_| RowStoreError::InvalidCanonicalRowValue)?;
    Ok(BlobMetadata::from_stored_reference(
        length,
        sha256,
        annotation.media_type(),
        annotation.suffix(),
    ))
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let base = major << 5;
    match value {
        0..=23 => output.push(base | value as u8),
        24..=255 => {
            output.push(base | 24);
            output.push(value as u8);
        }
        256..=65_535 => {
            output.push(base | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push(base | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(base | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowStoreError {
    DuplicateKey,
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
    UnsupportedWriterProfile,
    LegacyFormatReadOnly(u8),
    Native(GraphError),
}

impl fmt::Display for RowStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateKey => f.write_str("duplicate typed row key"),
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
            Self::InvalidKeyRange => f.write_str("row range lower bound is not below upper bound"),
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
