//! Compact manifest and projection model owned by the format layer.
//!
//! These are the values a compact reader returns after decoding: the immutable
//! manifest coordinates, the authenticated metadata of one segment, the
//! canonical path layout, and the verified row projection. They carry plain
//! repository-relative strings, never a repository-managed path type.

use std::fmt;

use orna_value::{CanonicalValue, SchemaDescriptor};
use uuid::{Uuid, Version};

use super::CompactError;
use super::manifest::{canonical_key_order, is_safe_atom, validate_event_time_bounds};

/// Maximum declared uncompressed size of one compact Parquet page.
pub const COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES: usize = 16 * 1024 * 1024;

/// The compact role of one immutable segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactSegmentRole {
    Data,
    Replacement,
    Deletion,
}

impl CompactSegmentRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Replacement => "replacement",
            Self::Deletion => "deletion",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CompactError> {
        match value {
            "data" => Ok(Self::Data),
            "replacement" => Ok(Self::Replacement),
            "deletion" => Ok(Self::Deletion),
            _ => Err(CompactError::InvalidManifest),
        }
    }
}

impl fmt::Display for CompactSegmentRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Repository-relative compact segment path for `table` and `segment`.
#[must_use]
pub fn segment_path(table: Uuid, segment: Uuid) -> String {
    let id = segment.to_string();
    let prefix = id.get(..2).unwrap_or_default();
    data_root(table) + "/" + prefix + "/" + &id + ".parquet"
}

/// Repository-relative compact data root for `table`.
#[must_use]
pub fn data_root(table: Uuid) -> String {
    format!(".orna/storage/{table}/data")
}

/// Repository-relative compact manifest path for `table`.
#[must_use]
pub fn manifest_path(table: Uuid) -> String {
    format!(".orna/storage/{table}/manifest.orna")
}

/// Repository-relative compact shard path for `table` and `number`.
#[must_use]
pub fn shard_path(table: Uuid, number: u64) -> String {
    format!(".orna/storage/{table}/shards/{number:08}.orna")
}

/// Reports whether `path` is a safe repository-relative path.
#[must_use]
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

/// The canonical metadata for one immutable compact segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactManifestEntry {
    segment_id: Uuid,
    role: CompactSegmentRole,
    generation: u64,
    schema: [u8; 32],
    encoder_version: String,
    relative_path: String,
    git_object_id: String,
    sha256: [u8; 32],
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    min_event_time: Option<String>,
    max_event_time: Option<String>,
    row_count: u64,
    compressed_bytes: u64,
    columns: Vec<u8>,
    row_group_index: bool,
    bloom: bool,
}

impl CompactManifestEntry {
    /// Constructs one entry from decoded metadata without validating it.
    /// Callers run [`Self::validate`] against their object-id length.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_parts(
        segment_id: Uuid,
        role: CompactSegmentRole,
        generation: u64,
        schema: [u8; 32],
        encoder_version: String,
        relative_path: String,
        git_object_id: String,
        sha256: [u8; 32],
        min_key: Vec<u8>,
        max_key: Vec<u8>,
        min_event_time: Option<String>,
        max_event_time: Option<String>,
        row_count: u64,
        compressed_bytes: u64,
        columns: Vec<u8>,
        row_group_index: bool,
        bloom: bool,
    ) -> Self {
        Self {
            segment_id,
            role,
            generation,
            schema,
            encoder_version,
            relative_path,
            git_object_id,
            sha256,
            min_key,
            max_key,
            min_event_time,
            max_event_time,
            row_count,
            compressed_bytes,
            columns,
            row_group_index,
            bloom,
        }
    }

    pub const fn segment_id(&self) -> Uuid {
        self.segment_id
    }

    pub const fn role(&self) -> CompactSegmentRole {
        self.role
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    pub const fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    pub const fn schema_id(&self) -> [u8; 32] {
        self.schema
    }

    pub fn columns(&self) -> &[u8] {
        &self.columns
    }

    pub fn encoder_version(&self) -> &str {
        &self.encoder_version
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn git_object_id(&self) -> &str {
        &self.git_object_id
    }

    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub fn min_key(&self) -> &[u8] {
        &self.min_key
    }

    pub fn max_key(&self) -> &[u8] {
        &self.max_key
    }

    pub fn min_event_time(&self) -> Option<&str> {
        self.min_event_time.as_deref()
    }

    pub fn max_event_time(&self) -> Option<&str> {
        self.max_event_time.as_deref()
    }

    pub const fn row_group_index(&self) -> bool {
        self.row_group_index
    }

    pub const fn bloom(&self) -> bool {
        self.bloom
    }

    /// Validates manifest bookkeeping for this entry.
    pub fn validate(
        &self,
        table: Uuid,
        object_id_length: Option<usize>,
    ) -> Result<(), CompactError> {
        if self.segment_id.get_version() != Some(Version::SortRand)
            || self.generation == 0
            || self.encoder_version.is_empty()
            || !self.encoder_version.bytes().all(is_safe_atom)
            || canonical_key_order(&self.min_key, &self.max_key)? == std::cmp::Ordering::Greater
            || self.row_count == 0
            || self.compressed_bytes == 0
            || orna_base::hex_decode(&self.git_object_id).is_err()
            || object_id_length.is_some_and(|length| self.git_object_id.len() != length)
            || !is_safe_relative_path(&self.relative_path)
            || self.relative_path != segment_path(table, self.segment_id)
            || std::path::Path::new(&self.relative_path)
                .extension()
                .and_then(|value| value.to_str())
                != Some("parquet")
        {
            return Err(CompactError::InvalidManifest);
        }
        validate_event_time_bounds(
            self.min_event_time.as_deref(),
            self.max_event_time.as_deref(),
        )
    }
}

/// A complete canonical compact manifest for one table snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactManifest {
    table: Uuid,
    schema: [u8; 32],
    next_generation: u64,
    entries: Vec<CompactManifestEntry>,
}

impl CompactManifest {
    pub fn empty(table: Uuid, schema: [u8; 32]) -> Self {
        Self {
            table,
            schema,
            next_generation: 1,
            entries: Vec::new(),
        }
    }

    /// Constructs a manifest from decoded parts without validating it.
    #[must_use]
    pub fn from_parts(
        table: Uuid,
        schema: [u8; 32],
        next_generation: u64,
        entries: Vec<CompactManifestEntry>,
    ) -> Self {
        Self {
            table,
            schema,
            next_generation,
            entries,
        }
    }

    pub fn table(&self) -> Uuid {
        self.table
    }

    pub const fn schema(&self) -> [u8; 32] {
        self.schema
    }

    pub const fn next_generation(&self) -> u64 {
        self.next_generation
    }

    pub fn entries(&self) -> &[CompactManifestEntry] {
        &self.entries
    }

    /// Validates manifest bookkeeping.
    pub fn validate(&self, object_id_length: Option<usize>) -> Result<(), CompactError> {
        if self.next_generation == 0 {
            return Err(CompactError::InvalidManifest);
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for entry in &self.entries {
            entry.validate(self.table, object_id_length)?;
            if entry.generation >= self.next_generation
                || !ids.insert(entry.segment_id)
                || !paths.insert(entry.relative_path.as_str())
            {
                return Err(CompactError::InvalidManifest);
            }
        }
        Ok(())
    }

    /// Entries sorted by segment identity, as the canonical writer orders them.
    pub fn sorted_entries(&self) -> Vec<&CompactManifestEntry> {
        let mut entries = self.entries.iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.segment_id.as_bytes().to_owned());
        entries
    }
}

/// One canonical logical row projected from a verified committed segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactCommittedRow {
    key: CanonicalValue,
    value: Option<CanonicalValue>,
}

impl CompactCommittedRow {
    #[must_use]
    pub fn new(key: CanonicalValue, value: Option<CanonicalValue>) -> Self {
        Self { key, value }
    }

    pub fn key(&self) -> &CanonicalValue {
        &self.key
    }

    pub fn value(&self) -> Option<&CanonicalValue> {
        self.value.as_ref()
    }
}

/// The verified logical projection of one committed compact segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactCommittedSegmentProjection {
    table_id: Uuid,
    schema_id: [u8; 32],
    schema: SchemaDescriptor,
    source_schema_id: [u8; 32],
    source_schema: SchemaDescriptor,
    profile: &'static str,
    encoder_version: String,
    generation: u64,
    role: CompactSegmentRole,
    rows: Vec<CompactCommittedRow>,
}

impl CompactCommittedSegmentProjection {
    /// Constructs a projection from already verified physical parts.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        table_id: Uuid,
        schema_id: [u8; 32],
        schema: SchemaDescriptor,
        source_schema_id: [u8; 32],
        source_schema: SchemaDescriptor,
        profile: &'static str,
        encoder_version: String,
        generation: u64,
        role: CompactSegmentRole,
        rows: Vec<CompactCommittedRow>,
    ) -> Self {
        Self {
            table_id,
            schema_id,
            schema,
            source_schema_id,
            source_schema,
            profile,
            encoder_version,
            generation,
            role,
            rows,
        }
    }

    pub const fn table_id(&self) -> Uuid {
        self.table_id
    }

    pub const fn schema_id(&self) -> [u8; 32] {
        self.schema_id
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.schema
    }

    pub fn source_schema(&self) -> &SchemaDescriptor {
        &self.source_schema
    }

    pub const fn source_schema_id(&self) -> [u8; 32] {
        self.source_schema_id
    }

    pub const fn profile(&self) -> &'static str {
        self.profile
    }

    pub fn encoder_version(&self) -> &str {
        &self.encoder_version
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn role(&self) -> CompactSegmentRole {
        self.role
    }

    pub fn rows(&self) -> &[CompactCommittedRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(table: Uuid, segment: Uuid) -> CompactManifestEntry {
        CompactManifestEntry::from_parts(
            segment,
            CompactSegmentRole::Data,
            1,
            [0x11; 32],
            "v1".to_owned(),
            segment_path(table, segment),
            "aa".repeat(20),
            [0x22; 32],
            vec![0],
            vec![1],
            None,
            None,
            1,
            1,
            Vec::new(),
            false,
            false,
        )
    }

    #[test]
    fn paths_follow_the_canonical_compact_layout() {
        let table = Uuid::from_u128(7);
        let segment = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        assert_eq!(
            manifest_path(table),
            format!(".orna/storage/{table}/manifest.orna")
        );
        assert_eq!(
            shard_path(table, 1),
            format!(".orna/storage/{table}/shards/00000001.orna")
        );
        let id = segment.to_string();
        assert_eq!(
            segment_path(table, segment),
            format!(".orna/storage/{table}/data/{}/{id}.parquet", &id[..2])
        );
    }

    #[test]
    fn an_entry_decodes_its_declared_metadata() {
        let table = Uuid::from_u128(7);
        let segment = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        let entry = entry(table, segment);
        entry.validate(table, Some(40)).unwrap();
        assert_eq!(entry.segment_id(), segment);
        assert_eq!(entry.role(), CompactSegmentRole::Data);
        assert_eq!(entry.schema_id(), [0x11; 32]);
        assert_eq!(entry.relative_path(), segment_path(table, segment));
    }

    #[test]
    fn an_entry_rejects_a_foreign_relative_path() {
        let table = Uuid::from_u128(7);
        let segment = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        let mut entry = entry(table, segment);
        entry.relative_path = ".orna/storage/other/data/00/x.parquet".to_owned();
        assert_eq!(
            entry.validate(table, Some(40)),
            Err(CompactError::InvalidManifest)
        );
    }

    #[test]
    fn a_manifest_rejects_duplicate_segments_and_generations() {
        let table = Uuid::from_u128(7);
        let segment = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        let manifest = CompactManifest::from_parts(table, [0; 32], 2, vec![entry(table, segment)]);
        manifest.validate(Some(40)).unwrap();
        let duplicate = CompactManifest::from_parts(
            table,
            [0; 32],
            3,
            vec![entry(table, segment), entry(table, segment)],
        );
        assert_eq!(
            duplicate.validate(Some(40)),
            Err(CompactError::InvalidManifest)
        );
        let forward = CompactManifest::from_parts(table, [0; 32], 1, vec![entry(table, segment)]);
        assert_eq!(
            forward.validate(Some(40)),
            Err(CompactError::InvalidManifest)
        );
    }
}
