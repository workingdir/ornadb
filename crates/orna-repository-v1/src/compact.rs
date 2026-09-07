//! Canonical compact-manifest publication witnesses.
//!
//! This module deliberately owns only the committed Git witness for compact
//! storage.  Encoding rows, choosing physical Parquet columns, and consuming
//! the runtime prefix remain separate boundaries.  The witness is nevertheless
//! real: it writes canonical tracked manifest/shard records, proves every
//! named data blob is in the candidate tree, and binds that proof into the
//! restart journal before a ref can become visible.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use bytes::Bytes;
use orna_syntax_v1::{parse_row, Expr, LiteralKind};
use parquet::{
    basic::{Compression, PageType},
    file::reader::{FileReader, SerializedFileReader},
};
use sha2::{Digest, Sha256};
use uuid::{Uuid, Version};

use crate::{
    GitCommitRef, IndexGeneration, ManagedFileChange, ManagedPath, PrivateCommit,
    PublicationJournal, PublicationJournalEntry, Repository, RepositoryError,
};

/// Maximum number of entries in one canonical compact-manifest shard.
pub const COMPACT_MANIFEST_SHARD_LIMIT: usize = 256;
const COMPACT_PROFILE: &str = "compact-storage-v1";
const MAX_COMPACT_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPACT_ROW_GROUP_ROWS: i64 = 65_536;

/// The compact role of one immutable segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactSegmentRole {
    Data,
    Replacement,
    Deletion,
}

impl CompactSegmentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Replacement => "replacement",
            Self::Deletion => "deletion",
        }
    }

    fn parse(value: &str) -> Result<Self, RepositoryError> {
        match value {
            "data" => Ok(Self::Data),
            "replacement" => Ok(Self::Replacement),
            "deletion" => Ok(Self::Deletion),
            _ => Err(RepositoryError::InvalidCompactManifest),
        }
    }
}

/// One newly encoded immutable compact segment.  Its generation and native
/// object ID are allocated only after the base manifest has been verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactSegment {
    segment_id: Uuid,
    role: CompactSegmentRole,
    schema: [u8; 32],
    encoder_version: String,
    relative_path: ManagedPath,
    bytes: Vec<u8>,
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    min_event_time: Option<String>,
    max_event_time: Option<String>,
    row_count: u64,
    columns: Vec<u8>,
    row_group_index: bool,
    bloom: bool,
}

impl CompactSegment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        segment_id: Uuid,
        role: CompactSegmentRole,
        schema: [u8; 32],
        encoder_version: impl Into<String>,
        relative_path: ManagedPath,
        bytes: Vec<u8>,
        min_key: Vec<u8>,
        max_key: Vec<u8>,
        row_count: u64,
        columns: Vec<u8>,
        row_group_index: bool,
        bloom: bool,
    ) -> Result<Self, RepositoryError> {
        let value = Self {
            segment_id,
            role,
            schema,
            encoder_version: encoder_version.into(),
            relative_path,
            bytes,
            min_key,
            max_key,
            min_event_time: None,
            max_event_time: None,
            row_count,
            columns,
            row_group_index,
            bloom,
        };
        value.validate_unallocated()?;
        Ok(value)
    }

    pub fn with_event_time_bounds(
        mut self,
        min_event_time: impl Into<String>,
        max_event_time: impl Into<String>,
    ) -> Result<Self, RepositoryError> {
        self.min_event_time = Some(min_event_time.into());
        self.max_event_time = Some(max_event_time.into());
        self.validate_unallocated()?;
        Ok(self)
    }

    fn validate_unallocated(&self) -> Result<(), RepositoryError> {
        if self.segment_id.get_version() != Some(Version::SortRand)
            || self.encoder_version.is_empty()
            || !self.encoder_version.bytes().all(is_safe_atom)
            || self.bytes.is_empty()
            || self.min_key > self.max_key
            || self.row_count == 0
            || self
                .relative_path
                .as_path()
                .extension()
                .and_then(|value| value.to_str())
                != Some("parquet")
            || self.min_event_time.is_some() != self.max_event_time.is_some()
            || self
                .min_event_time
                .iter()
                .chain(self.max_event_time.iter())
                .any(|value| value.is_empty() || !value.bytes().all(is_safe_atom))
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        Ok(())
    }
}

/// The canonical metadata for one immutable compact file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactManifestEntry {
    segment_id: Uuid,
    role: CompactSegmentRole,
    generation: u64,
    schema: [u8; 32],
    encoder_version: String,
    relative_path: ManagedPath,
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
    pub fn segment_id(&self) -> Uuid {
        self.segment_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn relative_path(&self) -> &ManagedPath {
        &self.relative_path
    }

    pub fn git_object_id(&self) -> &str {
        &self.git_object_id
    }

    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    fn from_segment(
        table: Uuid,
        segment: &CompactSegment,
        generation: u64,
        git_object_id: String,
    ) -> Result<Self, RepositoryError> {
        segment.validate_unallocated()?;
        verify_physical_segment(table, segment, &segment.bytes)?;
        if generation == 0 || git_object_id.is_empty() || !git_object_id.bytes().all(is_hex) {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        let compressed_bytes = u64::try_from(segment.bytes.len())
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        Ok(Self {
            segment_id: segment.segment_id,
            role: segment.role,
            generation,
            schema: segment.schema,
            encoder_version: segment.encoder_version.clone(),
            relative_path: segment.relative_path.clone(),
            git_object_id,
            sha256: Sha256::digest(&segment.bytes).into(),
            min_key: segment.min_key.clone(),
            max_key: segment.max_key.clone(),
            min_event_time: segment.min_event_time.clone(),
            max_event_time: segment.max_event_time.clone(),
            row_count: segment.row_count,
            compressed_bytes,
            columns: segment.columns.clone(),
            row_group_index: segment.row_group_index,
            bloom: segment.bloom,
        })
    }

    fn validate(
        &self,
        table: Uuid,
        object_id_length: Option<usize>,
    ) -> Result<(), RepositoryError> {
        if self.segment_id.get_version() != Some(Version::SortRand)
            || self.generation == 0
            || self.encoder_version.is_empty()
            || !self.encoder_version.bytes().all(is_safe_atom)
            || self.min_key > self.max_key
            || self.row_count == 0
            || self.compressed_bytes == 0
            || self.min_event_time.is_some() != self.max_event_time.is_some()
            || self
                .min_event_time
                .iter()
                .chain(self.max_event_time.iter())
                .any(|value| value.is_empty() || !value.bytes().all(is_safe_atom))
            || !self.git_object_id.bytes().all(is_hex)
            || object_id_length.is_some_and(|length| self.git_object_id.len() != length)
            || self.relative_path != compact_segment_path(table, self.segment_id)?
            || self
                .relative_path
                .as_path()
                .extension()
                .and_then(|value| value.to_str())
                != Some("parquet")
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        Ok(())
    }
}

/// Reads only the physical representation boundary needed by this repository
/// witness.  Logical row decoding and the complete descriptor/OVB semantics
/// remain owned by the storage reader; publication nevertheless rejects a
/// non-Parquet blob or a manifest that lies about its required file metadata.
fn verify_physical_segment(
    table: Uuid,
    segment: &CompactSegment,
    bytes: &[u8],
) -> Result<(), RepositoryError> {
    verify_physical_metadata(
        table,
        segment.schema,
        &segment.encoder_version,
        &segment.columns,
        segment.row_count,
        bytes,
    )
}

fn verify_physical_entry(
    table: Uuid,
    entry: &CompactManifestEntry,
    bytes: &[u8],
) -> Result<(), RepositoryError> {
    if entry.compressed_bytes
        != u64::try_from(bytes.len()).map_err(|_| RepositoryError::InvalidCompactManifest)?
    {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    verify_physical_metadata(
        table,
        entry.schema,
        &entry.encoder_version,
        &entry.columns,
        entry.row_count,
        bytes,
    )
}

fn verify_physical_metadata(
    table: Uuid,
    schema: [u8; 32],
    encoder_version: &str,
    columns: &[u8],
    row_count: u64,
    bytes: &[u8],
) -> Result<(), RepositoryError> {
    if bytes.is_empty() || bytes.len() > MAX_COMPACT_FILE_BYTES {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let reader = SerializedFileReader::new(Bytes::copy_from_slice(bytes))
        .map_err(|_| RepositoryError::InvalidCompactManifest)?;
    let metadata = reader.metadata().file_metadata();
    if metadata.version() != 1
        || metadata.num_rows()
            != i64::try_from(row_count).map_err(|_| RepositoryError::InvalidCompactManifest)?
    {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let required = required_physical_metadata(table, schema, encoder_version, columns);
    let mut found = BTreeMap::new();
    for item in metadata.key_value_metadata().into_iter().flatten() {
        let Some(value) = item.value.as_deref() else {
            return Err(RepositoryError::InvalidCompactManifest);
        };
        if (required.contains_key(item.key.as_str()) || item.key == "orna.schema.ovb")
            && found.insert(item.key.as_str(), value).is_some()
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
    }
    if required
        .iter()
        .any(|(key, value)| found.get(key.as_str()) != Some(&value.as_str()))
        || found
            .get("orna.schema.ovb")
            .and_then(|value| decode_base64(value).ok())
            .is_none_or(|value| value.is_empty())
    {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    if reader.num_row_groups() == 0 {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    verify_page_checksums(bytes, reader.metadata().row_groups())?;
    for row_group in reader.metadata().row_groups() {
        if row_group.num_rows() <= 0
            || row_group.num_rows() > MAX_COMPACT_ROW_GROUP_ROWS
            || row_group.num_columns() == 0
            || row_group.columns().iter().any(|column| {
                !matches!(column.compression(), Compression::ZSTD(_))
                    || column.num_values() < row_group.num_rows()
                    || column.compressed_size() <= 0
                    || column.uncompressed_size() <= 0
                    || column.statistics().is_none()
            })
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
    }
    // Drive every compressed page through parquet-rs so malformed compressed
    // data and any supplied page checksum are rejected before publication.
    for row_group in 0..reader.num_row_groups() {
        let group = reader
            .get_row_group(row_group)
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        for column in 0..group.num_columns() {
            let pages = group
                .get_column_page_reader(column)
                .map_err(|_| RepositoryError::InvalidCompactManifest)?;
            let mut data_pages = 0usize;
            for page in pages {
                let page = page.map_err(|_| RepositoryError::InvalidCompactManifest)?;
                if page.is_data_page() {
                    data_pages = data_pages
                        .checked_add(1)
                        .ok_or(RepositoryError::InvalidCompactManifest)?;
                    if page.page_type() != PageType::DATA_PAGE_V2 {
                        return Err(RepositoryError::InvalidCompactManifest);
                    }
                }
            }
            if data_pages == 0 {
                return Err(RepositoryError::InvalidCompactManifest);
            }
        }
    }
    Ok(())
}

/// The Parquet reader verifies a present CRC while decoding a page, but the
/// compact profile also requires every serialized page to carry one. The
/// reader intentionally accepts absent CRC fields for generic Parquet input,
/// so inspect each compact Thrift page header before treating a segment as
/// authoritative.
fn verify_page_checksums(
    bytes: &[u8],
    row_groups: &[parquet::file::metadata::RowGroupMetaData],
) -> Result<(), RepositoryError> {
    for row_group in row_groups {
        for column in row_group.columns() {
            let start = column
                .dictionary_page_offset()
                .unwrap_or_else(|| column.data_page_offset());
            let length = column.compressed_size();
            let start =
                usize::try_from(start).map_err(|_| RepositoryError::InvalidCompactManifest)?;
            let length =
                usize::try_from(length).map_err(|_| RepositoryError::InvalidCompactManifest)?;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            let mut cursor = start;
            while cursor < end {
                let header = parse_compact_page_header(&bytes[cursor..end])?;
                if !header.has_checksum {
                    return Err(RepositoryError::InvalidCompactManifest);
                }
                cursor = cursor
                    .checked_add(header.encoded_len)
                    .and_then(|value| value.checked_add(header.compressed_len))
                    .filter(|value| *value <= end)
                    .ok_or(RepositoryError::InvalidCompactManifest)?;
            }
            if cursor != end {
                return Err(RepositoryError::InvalidCompactManifest);
            }
        }
    }
    Ok(())
}

struct CompactPageHeader {
    encoded_len: usize,
    compressed_len: usize,
    has_checksum: bool,
}

fn parse_compact_page_header(bytes: &[u8]) -> Result<CompactPageHeader, RepositoryError> {
    let mut cursor = 0;
    let mut last_field = 0_i16;
    let mut compressed_len = None;
    let mut has_checksum = false;
    loop {
        let tag = take_compact_byte(bytes, &mut cursor)?;
        let kind = tag & 0x0f;
        if kind == 0 {
            break;
        }
        let delta = i16::from(tag >> 4);
        let field = if delta == 0 {
            decode_compact_i16(bytes, &mut cursor)?
        } else {
            last_field
                .checked_add(delta)
                .ok_or(RepositoryError::InvalidCompactManifest)?
        };
        if field <= last_field {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        last_field = field;
        if field == 3 && kind == 5 {
            let value = decode_compact_i32(bytes, &mut cursor)?;
            compressed_len =
                Some(usize::try_from(value).map_err(|_| RepositoryError::InvalidCompactManifest)?);
        } else {
            if field == 4 && kind == 5 {
                has_checksum = true;
            }
            skip_compact_value(bytes, &mut cursor, kind, 0)?;
        }
    }
    Ok(CompactPageHeader {
        encoded_len: cursor,
        compressed_len: compressed_len.ok_or(RepositoryError::InvalidCompactManifest)?,
        has_checksum,
    })
}

fn skip_compact_value(
    bytes: &[u8],
    cursor: &mut usize,
    kind: u8,
    depth: usize,
) -> Result<(), RepositoryError> {
    if depth > 64 {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    match kind {
        1 | 2 => Ok(()),
        3 => {
            take_compact_byte(bytes, cursor)?;
            Ok(())
        }
        4..=6 => {
            let _ = decode_compact_varint(bytes, cursor)?;
            Ok(())
        }
        7 => {
            let end = cursor
                .checked_add(8)
                .filter(|end| *end <= bytes.len())
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            *cursor = end;
            Ok(())
        }
        8 => {
            let length = usize::try_from(decode_compact_varint(bytes, cursor)?)
                .map_err(|_| RepositoryError::InvalidCompactManifest)?;
            let end = cursor
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            *cursor = end;
            Ok(())
        }
        9 | 10 => {
            let size_and_kind = take_compact_byte(bytes, cursor)?;
            let size = if size_and_kind >> 4 == 15 {
                usize::try_from(decode_compact_varint(bytes, cursor)?)
                    .map_err(|_| RepositoryError::InvalidCompactManifest)?
            } else {
                usize::from(size_and_kind >> 4)
            };
            let item_kind = size_and_kind & 0x0f;
            if size > bytes.len() {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            for _ in 0..size {
                skip_compact_value(bytes, cursor, item_kind, depth + 1)?;
            }
            Ok(())
        }
        11 => {
            let size = usize::try_from(decode_compact_varint(bytes, cursor)?)
                .map_err(|_| RepositoryError::InvalidCompactManifest)?;
            if size == 0 {
                return Ok(());
            }
            let kinds = take_compact_byte(bytes, cursor)?;
            let key_kind = kinds >> 4;
            let value_kind = kinds & 0x0f;
            for _ in 0..size {
                skip_compact_value(bytes, cursor, key_kind, depth + 1)?;
                skip_compact_value(bytes, cursor, value_kind, depth + 1)?;
            }
            Ok(())
        }
        12 => skip_compact_struct(bytes, cursor, depth + 1),
        _ => Err(RepositoryError::InvalidCompactManifest),
    }
}

fn skip_compact_struct(
    bytes: &[u8],
    cursor: &mut usize,
    depth: usize,
) -> Result<(), RepositoryError> {
    let mut last_field = 0_i16;
    loop {
        let tag = take_compact_byte(bytes, cursor)?;
        let kind = tag & 0x0f;
        if kind == 0 {
            return Ok(());
        }
        let delta = i16::from(tag >> 4);
        let field = if delta == 0 {
            decode_compact_i16(bytes, cursor)?
        } else {
            last_field
                .checked_add(delta)
                .ok_or(RepositoryError::InvalidCompactManifest)?
        };
        if field <= last_field {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        last_field = field;
        skip_compact_value(bytes, cursor, kind, depth)?;
    }
}

fn take_compact_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, RepositoryError> {
    let value = *bytes
        .get(*cursor)
        .ok_or(RepositoryError::InvalidCompactManifest)?;
    *cursor = cursor
        .checked_add(1)
        .ok_or(RepositoryError::InvalidCompactManifest)?;
    Ok(value)
}

fn decode_compact_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, RepositoryError> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = take_compact_byte(bytes, cursor)?;
        value |= u64::from(byte & 0x7f)
            .checked_shl(shift)
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(RepositoryError::InvalidCompactManifest)
}

fn decode_compact_i16(bytes: &[u8], cursor: &mut usize) -> Result<i16, RepositoryError> {
    let value = decode_compact_varint(bytes, cursor)?;
    let value = i64::try_from(value).map_err(|_| RepositoryError::InvalidCompactManifest)?;
    i16::try_from((value >> 1) ^ -(value & 1)).map_err(|_| RepositoryError::InvalidCompactManifest)
}

fn decode_compact_i32(bytes: &[u8], cursor: &mut usize) -> Result<i32, RepositoryError> {
    let value = decode_compact_varint(bytes, cursor)?;
    let value = i64::try_from(value).map_err(|_| RepositoryError::InvalidCompactManifest)?;
    i32::try_from((value >> 1) ^ -(value & 1)).map_err(|_| RepositoryError::InvalidCompactManifest)
}

fn required_physical_metadata(
    table: Uuid,
    schema: [u8; 32],
    encoder_version: &str,
    columns: &[u8],
) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orna.profile".to_owned(), COMPACT_PROFILE.to_owned()),
        ("orna.table".to_owned(), table.to_string()),
        ("orna.schema.sha256".to_owned(), hex(&schema)),
        ("orna.columns.ovb".to_owned(), base64(columns)),
        ("orna.encoder".to_owned(), encoder_version.to_owned()),
    ])
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

    pub fn table(&self) -> Uuid {
        self.table
    }

    pub const fn next_generation(&self) -> u64 {
        self.next_generation
    }

    pub fn entries(&self) -> &[CompactManifestEntry] {
        &self.entries
    }

    pub fn manifest_path(&self) -> ManagedPath {
        managed_child(&compact_root(self.table), "manifest.orna").expect("constant safe path")
    }

    fn validate(&self, object_id_length: Option<usize>) -> Result<(), RepositoryError> {
        if self.next_generation == 0 {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            entry.validate(self.table, object_id_length)?;
            if entry.schema != self.schema
                || entry.generation >= self.next_generation
                || !ids.insert(entry.segment_id)
                || !paths.insert(path_key(&entry.relative_path)?)
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
        }
        Ok(())
    }

    fn sorted_entries(&self) -> Vec<&CompactManifestEntry> {
        let mut entries = self.entries.iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.segment_id.as_bytes().to_owned());
        entries
    }

    fn canonical_files(&self) -> Result<Vec<ManagedFileChange>, RepositoryError> {
        self.validate(None)?;
        let entries = self.sorted_entries();
        let mut shards = Vec::new();
        for (number, entries) in entries.chunks(COMPACT_MANIFEST_SHARD_LIMIT).enumerate() {
            let number =
                u64::try_from(number).map_err(|_| RepositoryError::InvalidCompactManifest)?;
            let file = shard_path(self.table, number)?;
            let bytes = canonical_shard(entries)?;
            let min_key = entries
                .iter()
                .map(|entry| entry.min_key.as_slice())
                .min()
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            let max_key = entries
                .iter()
                .map(|entry| entry.max_key.as_slice())
                .max()
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            shards.push(ShardDescriptor {
                number,
                min_key: min_key.to_vec(),
                max_key: max_key.to_vec(),
                entries: entries.len(),
                file,
                hash: Sha256::digest(&bytes).into(),
                bytes,
            });
        }
        let manifest = canonical_manifest(self, &shards)?;
        let mut files = shards
            .into_iter()
            .map(|shard| ManagedFileChange::new(shard.file, Some(shard.bytes)))
            .collect::<Vec<_>>();
        files.push(ManagedFileChange::new(self.manifest_path(), Some(manifest)));
        Ok(files)
    }

    fn witness(
        &self,
        base_sha256: [u8; 32],
        selected_ref: String,
        runtime_intent_id: [u8; 16],
        cleanup_watermark: [u8; 32],
    ) -> Result<CompactManifestWitness, RepositoryError> {
        let manifest = self
            .canonical_files()?
            .into_iter()
            .find(|change| change.path() == &self.manifest_path())
            .and_then(|change| change.bytes().map(ToOwned::to_owned))
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        Ok(CompactManifestWitness {
            table: self.table,
            selected_ref,
            runtime_intent_id,
            cleanup_watermark,
            manifest_path: self.manifest_path(),
            base_sha256,
            manifest_sha256: Sha256::digest(manifest).into(),
            generation: self
                .next_generation
                .checked_sub(1)
                .ok_or(RepositoryError::InvalidCompactManifest)?,
            entries: self
                .entries
                .iter()
                .map(|entry| CompactManifestObject {
                    path: entry.relative_path.clone(),
                    object_id: entry.git_object_id.clone(),
                    sha256: entry.sha256,
                })
                .collect(),
        })
    }
}

struct ShardDescriptor {
    number: u64,
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    entries: usize,
    file: ManagedPath,
    hash: [u8; 32],
    bytes: Vec<u8>,
}

/// The compact-specific portion of a durable publication journal.  It records
/// the base and candidate manifest identities plus every data object identity
/// named by the candidate, so restart cannot consume a runtime prefix on a
/// merely plausible commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactManifestWitness {
    table: Uuid,
    selected_ref: String,
    runtime_intent_id: [u8; 16],
    cleanup_watermark: [u8; 32],
    manifest_path: ManagedPath,
    base_sha256: [u8; 32],
    manifest_sha256: [u8; 32],
    generation: u64,
    entries: Vec<CompactManifestObject>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompactManifestObject {
    path: ManagedPath,
    object_id: String,
    sha256: [u8; 32],
}

impl CompactManifestWitness {
    pub fn table(&self) -> Uuid {
        self.table
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }

    pub(crate) fn selected_ref(&self) -> &str {
        &self.selected_ref
    }

    pub(crate) const fn runtime_intent_id(&self) -> [u8; 16] {
        self.runtime_intent_id
    }

    pub const fn cleanup_watermark(&self) -> [u8; 32] {
        self.cleanup_watermark
    }

    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn validate(&self, object_id_length: Option<usize>) -> Result<(), RepositoryError> {
        if self.generation == 0
            || self.runtime_intent_id == [0; 16]
            || !is_selected_ref(&self.selected_ref)
            || self.entries.is_empty()
            || self.manifest_path != managed_child(&compact_root(self.table), "manifest.orna")?
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            if !paths.insert(path_key(&entry.path)?)
                || !entry
                    .path
                    .as_path()
                    .starts_with(compact_data_root(self.table).as_path())
                || !entry.object_id.bytes().all(is_hex)
                || object_id_length.is_some_and(|length| entry.object_id.len() != length)
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(())
    }

    pub(crate) fn encode(&self, bytes: &mut Vec<u8>) -> Result<(), RepositoryError> {
        self.validate(None)?;
        bytes.extend_from_slice(self.table.as_bytes());
        put_string(bytes, &self.selected_ref)?;
        bytes.extend_from_slice(&self.runtime_intent_id);
        bytes.extend_from_slice(&self.cleanup_watermark);
        put_string(
            bytes,
            self.manifest_path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::InvalidPublicationJournal)?,
        )?;
        bytes.extend_from_slice(&self.base_sha256);
        bytes.extend_from_slice(&self.manifest_sha256);
        put_u64(bytes, self.generation);
        put_u32(bytes, self.entries.len())?;
        for entry in &self.entries {
            put_string(
                bytes,
                entry
                    .path
                    .as_path()
                    .to_str()
                    .ok_or(RepositoryError::InvalidPublicationJournal)?,
            )?;
            put_string(bytes, &entry.object_id)?;
            bytes.extend_from_slice(&entry.sha256);
        }
        Ok(())
    }

    pub(crate) fn decode(
        bytes: &[u8],
        cursor: &mut usize,
        object_id_length: usize,
    ) -> Result<Self, RepositoryError> {
        let table = Uuid::from_bytes(take_fixed::<16>(bytes, cursor)?);
        let selected_ref = take_string(bytes, cursor)?;
        let runtime_intent_id = take_fixed::<16>(bytes, cursor)?;
        let cleanup_watermark = take_fixed::<32>(bytes, cursor)?;
        let manifest_path = ManagedPath::new(take_string(bytes, cursor)?)?;
        let base_sha256 = take_fixed::<32>(bytes, cursor)?;
        let manifest_sha256 = take_fixed::<32>(bytes, cursor)?;
        let generation = take_u64(bytes, cursor)?;
        let count = take_u32(bytes, cursor)? as usize;
        if count == 0 || count > 1_000_000 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut entries = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            entries.push(CompactManifestObject {
                path: ManagedPath::new(take_string(bytes, cursor)?)?,
                object_id: take_string(bytes, cursor)?,
                sha256: take_fixed::<32>(bytes, cursor)?,
            });
        }
        let witness = Self {
            table,
            selected_ref,
            runtime_intent_id,
            cleanup_watermark,
            manifest_path,
            base_sha256,
            manifest_sha256,
            generation,
            entries,
        };
        witness.validate(Some(object_id_length))?;
        Ok(witness)
    }
}

/// A private compact candidate and its journal binding.  Calling `publish`
/// still uses the ordinary loose-publication executor, preserving its index and
/// worktree protections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactPublicationPlan {
    expected_index: IndexGeneration,
    candidate: PrivateCommit,
    manifest: CompactManifest,
    journal: PublicationJournal,
    source: CompactPublicationSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompactPublicationSource {
    table: Uuid,
    empty_schema: [u8; 32],
    runtime_intent_id: [u8; 16],
    cleanup_watermark: [u8; 32],
    segments: Vec<CompactSegment>,
    message: String,
}

impl CompactPublicationPlan {
    pub fn candidate(&self) -> &PrivateCommit {
        &self.candidate
    }

    pub fn manifest(&self) -> &CompactManifest {
        &self.manifest
    }

    pub fn journal(&self) -> &PublicationJournal {
        &self.journal
    }

    pub fn publish(&mut self, repository: &Repository) -> Result<IndexGeneration, RepositoryError> {
        for attempt in 0..=2 {
            match repository.publish_candidate(
                &self.expected_index,
                &self.candidate,
                &mut self.journal,
            ) {
                Ok(result) => return Ok(result),
                Err(RepositoryError::StaleHead)
                    if attempt < 2
                        && repository.head()?.as_ref() != Some(self.journal.old_head()) =>
                {
                    *self = repository.rebuild_compact_publication_with_watermark(
                        self.source.table,
                        self.source.empty_schema,
                        self.source.runtime_intent_id,
                        self.source.cleanup_watermark,
                        &self.source.segments,
                        &self.source.message,
                    )?;
                }
                Err(RepositoryError::StaleIndex { actual, .. })
                    if attempt < 2 && actual.head() != Some(self.journal.old_head()) =>
                {
                    *self = repository.rebuild_compact_publication_with_watermark(
                        self.source.table,
                        self.source.empty_schema,
                        self.source.runtime_intent_id,
                        self.source.cleanup_watermark,
                        &self.source.segments,
                        &self.source.message,
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(RepositoryError::StaleHead)
    }
}

impl Repository {
    /// Reads and verifies the canonical compact manifest at `commit`.  A
    /// missing manifest is represented by `None`; malformed or incomplete
    /// committed state is never treated as an empty table.
    pub fn read_compact_manifest(
        &self,
        commit: &GitCommitRef,
        table: Uuid,
    ) -> Result<Option<CompactManifest>, RepositoryError> {
        let manifest_path = managed_child(&compact_root(table), "manifest.orna")?;
        let Some(bytes) = self.committed_file_bytes(commit, &manifest_path)? else {
            return Ok(None);
        };
        let header = parse_manifest_header(&bytes)?;
        if header.table != table {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        let mut entries = Vec::new();
        let mut shard_bytes_by_path = Vec::new();
        for shard in &header.shards {
            let shard_bytes = self
                .committed_file_bytes(commit, &shard.file)?
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            if Sha256::digest(&shard_bytes).as_slice() != shard.hash {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            let parsed = parse_shard(&shard_bytes)?;
            if parsed.len() != shard.entries
                || parsed.iter().map(|entry| entry.min_key.as_slice()).min()
                    != Some(shard.min_key.as_slice())
                || parsed.iter().map(|entry| entry.max_key.as_slice()).max()
                    != Some(shard.max_key.as_slice())
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            shard_bytes_by_path.push((shard.file.clone(), shard_bytes));
            entries.extend(parsed);
        }
        let manifest = CompactManifest {
            table: header.table,
            schema: header.schema,
            next_generation: header.next_generation,
            entries,
        };
        manifest.validate(Some(self.native_object_id_length()?))?;
        verify_canonical_manifest_files(&manifest, &bytes, &shard_bytes_by_path)?;
        self.verify_manifest_at_commit(commit, &manifest)?;
        Ok(Some(manifest))
    }

    /// Builds a compact publication from the manifest committed by
    /// `expected_head`.  The candidate generation is taken solely from that
    /// verified base manifest, and the returned journal binds all resulting
    /// manifest/data identities before publication can advance a ref.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_compact_publication(
        &self,
        expected_head: &GitCommitRef,
        expected_index: IndexGeneration,
        base: CompactManifest,
        runtime_intent_id: [u8; 16],
        cleanup_watermark: [u8; 32],
        segments: &[CompactSegment],
        message: &str,
    ) -> Result<CompactPublicationPlan, RepositoryError> {
        if runtime_intent_id == [0; 16]
            || segments.is_empty()
            || expected_index.head() != Some(expected_head)
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        self.verify_manifest_at_commit(expected_head, &base)?;
        let object_id_length = self.native_object_id_length()?;
        let generation = base.next_generation;
        let root = compact_data_root(base.table);
        let mut paths = BTreeSet::new();
        let mut additions = Vec::with_capacity(segments.len());
        for segment in segments {
            segment.validate_unallocated()?;
            if !segment.relative_path.as_path().starts_with(root.as_path())
                || !paths.insert(path_key(&segment.relative_path)?)
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            let object_id = self.hash_object(&segment.bytes)?;
            if object_id.len() != object_id_length {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            additions.push(CompactManifestEntry::from_segment(
                base.table, segment, generation, object_id,
            )?);
        }
        let mut entries = base.entries.clone();
        entries.extend(additions);
        entries.sort_by_key(|entry| entry.segment_id.as_bytes().to_owned());
        let next_generation = generation
            .checked_add(1)
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        let manifest = CompactManifest {
            table: base.table,
            schema: base.schema,
            next_generation,
            entries,
        };
        manifest.validate(Some(object_id_length))?;
        let base_sha256 = self.manifest_digest_at_commit(expected_head, &base)?;
        let selected_ref = self.symbolic_head_ref()?;
        let witness = manifest.witness(
            base_sha256,
            selected_ref,
            runtime_intent_id,
            cleanup_watermark,
        )?;

        let mut changes = segments
            .iter()
            .map(|segment| {
                ManagedFileChange::new(segment.relative_path.clone(), Some(segment.bytes.clone()))
            })
            .collect::<Vec<_>>();
        changes.extend(manifest.canonical_files()?);
        let candidate = self.build_private_commit(expected_head, &changes, message)?;
        self.verify_candidate_compact_manifest(&candidate, &manifest, &witness)?;

        let journal_entries = changes
            .iter()
            .map(|change| {
                Ok(PublicationJournalEntry::new(
                    change.path().clone(),
                    self.managed_file_bytes(change.path())?,
                    change.bytes().map(ToOwned::to_owned),
                ))
            })
            .collect::<Result<Vec<_>, RepositoryError>>()?;
        let base_index_tree = expected_index
            .tree()
            .cloned()
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        let journal = PublicationJournal::new_with_runtime_intent(
            expected_head.clone(),
            candidate.commit().clone(),
            base_index_tree,
            runtime_intent_id,
            journal_entries,
        )?
        .with_compact_manifest(witness)?;
        Ok(CompactPublicationPlan {
            expected_index,
            candidate,
            manifest,
            journal,
            source: CompactPublicationSource {
                table: base.table,
                empty_schema: base.schema,
                runtime_intent_id,
                cleanup_watermark,
                segments: segments.to_vec(),
                message: message.to_owned(),
            },
        })
    }

    /// Rebuilds a failed compact publication against the actual current head.
    /// Immutable segment bytes are supplied again, but their generation is
    /// derived afresh from the reloaded committed manifest and is never forced
    /// from the stale candidate.
    pub fn rebuild_compact_publication(
        &self,
        table: Uuid,
        empty_schema: [u8; 32],
        runtime_intent_id: [u8; 16],
        segments: &[CompactSegment],
        message: &str,
    ) -> Result<CompactPublicationPlan, RepositoryError> {
        self.rebuild_compact_publication_with_watermark(
            table,
            empty_schema,
            runtime_intent_id,
            [0; 32],
            segments,
            message,
        )
    }

    /// Rebuilds a failed compact publication using the durable frozen cleanup
    /// watermark supplied by the runtime owner. This boundary records that
    /// watermark only; it never consumes runtime state itself.
    pub fn rebuild_compact_publication_with_watermark(
        &self,
        table: Uuid,
        empty_schema: [u8; 32],
        runtime_intent_id: [u8; 16],
        cleanup_watermark: [u8; 32],
        segments: &[CompactSegment],
        message: &str,
    ) -> Result<CompactPublicationPlan, RepositoryError> {
        let head = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        let base = self
            .read_compact_manifest(&head, table)?
            .unwrap_or_else(|| CompactManifest::empty(table, empty_schema));
        self.prepare_compact_publication(
            &head,
            self.index_generation()?,
            base,
            runtime_intent_id,
            cleanup_watermark,
            segments,
            message,
        )
    }

    pub(crate) fn verify_compact_journal(
        &self,
        candidate: &PrivateCommit,
        witness: &CompactManifestWitness,
    ) -> Result<(), RepositoryError> {
        witness.validate(Some(self.native_object_id_length()?))?;
        let manifest_path = &witness.manifest_path;
        let bytes = self
            .candidate_file_bytes(candidate, manifest_path)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        if Sha256::digest(&bytes).as_slice() != witness.manifest_sha256 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let manifest = self
            .read_compact_manifest_from_candidate(candidate, &bytes)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if manifest.table != witness.table
            || manifest.next_generation.checked_sub(1) != Some(witness.generation)
            || manifest.entries.len() != witness.entries.len()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        for object in &witness.entries {
            let Some((_mode, actual)) = self.candidate_tree_entry(candidate, &object.path)? else {
                return Err(RepositoryError::InvalidPublicationJournal);
            };
            let bytes = self.git_bytes(["cat-file", "blob", &actual])?;
            let manifest_entry = manifest
                .entries
                .iter()
                .find(|entry| entry.relative_path == object.path)
                .ok_or(RepositoryError::InvalidPublicationJournal)?;
            if actual != object.object_id
                || manifest_entry.git_object_id != object.object_id
                || manifest_entry.sha256 != object.sha256
                || Sha256::digest(&bytes).as_slice() != object.sha256
                || verify_physical_entry(witness.table, manifest_entry, &bytes).is_err()
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(())
    }

    pub(crate) fn verify_compact_publication_binding(
        &self,
        journal: &PublicationJournal,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let witness = journal
            .compact_manifest()
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        if journal.runtime_intent_id() != Some(witness.runtime_intent_id())
            || self.symbolic_head_ref()? != witness.selected_ref()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let manifest_path = witness.manifest_path.clone();
        let base = self.committed_file_bytes(journal.old_head(), &manifest_path)?;
        match base {
            Some(bytes) if Sha256::digest(&bytes).as_slice() == witness.base_sha256 => {
                let manifest = self
                    .read_compact_manifest(journal.old_head(), witness.table)?
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                if manifest.next_generation != witness.generation {
                    return Err(RepositoryError::InvalidPublicationJournal);
                }
            }
            None if witness.base_sha256 == [0; 32] => {}
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        }
        self.verify_compact_journal(candidate, witness)
    }

    fn verify_manifest_at_commit(
        &self,
        commit: &GitCommitRef,
        manifest: &CompactManifest,
    ) -> Result<(), RepositoryError> {
        let expected = manifest.canonical_files()?;
        if manifest.entries.is_empty() {
            if self
                .committed_file_bytes(commit, &manifest.manifest_path())?
                .is_some()
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            return Ok(());
        }
        for change in expected {
            if self.committed_file_bytes(commit, change.path())?.as_deref() != change.bytes() {
                return Err(RepositoryError::InvalidCompactManifest);
            }
        }
        for entry in &manifest.entries {
            let Some((_mode, object)) = self.tree_entry_at(commit, &entry.relative_path)? else {
                return Err(RepositoryError::InvalidCompactManifest);
            };
            let bytes = self.git_bytes(["cat-file", "blob", &object])?;
            if object != entry.git_object_id
                || Sha256::digest(&bytes).as_slice() != entry.sha256
                || verify_physical_entry(manifest.table, entry, &bytes).is_err()
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
        }
        Ok(())
    }

    fn manifest_digest_at_commit(
        &self,
        commit: &GitCommitRef,
        manifest: &CompactManifest,
    ) -> Result<[u8; 32], RepositoryError> {
        if manifest.entries.is_empty() {
            return Ok([0; 32]);
        }
        Ok(Sha256::digest(
            self.committed_file_bytes(commit, &manifest.manifest_path())?
                .ok_or(RepositoryError::InvalidCompactManifest)?,
        )
        .into())
    }

    fn verify_candidate_compact_manifest(
        &self,
        candidate: &PrivateCommit,
        manifest: &CompactManifest,
        witness: &CompactManifestWitness,
    ) -> Result<(), RepositoryError> {
        self.verify_compact_journal(candidate, witness)
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        let actual = self
            .candidate_file_bytes(candidate, &manifest.manifest_path())?
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        let parsed = self.read_compact_manifest_from_candidate(candidate, &actual)?;
        if parsed != *manifest {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        Ok(())
    }

    fn committed_file_bytes(
        &self,
        commit: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let Some((_mode, object)) = self.tree_entry_at(commit, path)? else {
            return Ok(None);
        };
        Ok(Some(self.git_bytes(["cat-file", "blob", &object])?))
    }

    fn candidate_file_bytes(
        &self,
        candidate: &PrivateCommit,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let Some((_mode, object)) = self.candidate_tree_entry(candidate, path)? else {
            return Ok(None);
        };
        Ok(Some(self.git_bytes(["cat-file", "blob", &object])?))
    }

    fn tree_entry_at(
        &self,
        commit: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        let mut command = self.command();
        command
            .args(["ls-tree", "-z", "--full-tree", commit.as_str(), "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        parse_tree_entry(&output, path, self.native_object_id_length()?)
    }

    fn read_compact_manifest_from_candidate(
        &self,
        candidate: &PrivateCommit,
        manifest_bytes: &[u8],
    ) -> Result<CompactManifest, RepositoryError> {
        let header = parse_manifest_header(manifest_bytes)?;
        let mut entries = Vec::new();
        let mut shard_bytes_by_path = Vec::new();
        for shard in &header.shards {
            let shard_bytes = self
                .candidate_file_bytes(candidate, &shard.file)?
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            if Sha256::digest(&shard_bytes).as_slice() != shard.hash {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            let parsed = parse_shard(&shard_bytes)?;
            if parsed.len() != shard.entries
                || parsed.iter().map(|entry| entry.min_key.as_slice()).min()
                    != Some(shard.min_key.as_slice())
                || parsed.iter().map(|entry| entry.max_key.as_slice()).max()
                    != Some(shard.max_key.as_slice())
            {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            shard_bytes_by_path.push((shard.file.clone(), shard_bytes));
            entries.extend(parsed);
        }
        let manifest = CompactManifest {
            table: header.table,
            schema: header.schema,
            next_generation: header.next_generation,
            entries,
        };
        manifest.validate(Some(self.native_object_id_length()?))?;
        verify_canonical_manifest_files(&manifest, manifest_bytes, &shard_bytes_by_path)?;
        Ok(manifest)
    }
}

/// Reject committed compact records which decode correctly but do not use the
/// single canonical manifest/shard serialization.  A shard hash proves only
/// that a manifest names the bytes it carries; canonical reconstruction also
/// proves that those bytes are the required representation of their content.
fn verify_canonical_manifest_files(
    manifest: &CompactManifest,
    manifest_bytes: &[u8],
    shard_bytes_by_path: &[(ManagedPath, Vec<u8>)],
) -> Result<(), RepositoryError> {
    for expected in manifest.canonical_files()? {
        let actual = if expected.path() == &manifest.manifest_path() {
            manifest_bytes
        } else {
            shard_bytes_by_path
                .iter()
                .find(|(path, _)| path == expected.path())
                .map(|(_, bytes)| bytes.as_slice())
                .ok_or(RepositoryError::InvalidCompactManifest)?
        };
        if expected.bytes() != Some(actual) {
            return Err(RepositoryError::InvalidCompactManifest);
        }
    }
    Ok(())
}

fn parse_tree_entry(
    output: &[u8],
    path: &ManagedPath,
    object_id_length: usize,
) -> Result<Option<(String, String)>, RepositoryError> {
    let Some(entry) = output.split(|byte| *byte == 0).next() else {
        return Ok(None);
    };
    if entry.is_empty() {
        return Ok(None);
    }
    let tab = entry
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or(RepositoryError::GitOperationFailed)?;
    if &entry[tab + 1..] != path.as_path().as_os_str().as_encoded_bytes() {
        return Err(RepositoryError::GitOperationFailed);
    }
    let mut fields = entry[..tab].split(|byte| *byte == b' ');
    let mode = String::from_utf8(fields.next().unwrap_or_default().to_vec())
        .map_err(|_| RepositoryError::GitOperationFailed)?;
    let kind = fields.next().unwrap_or_default();
    let object = String::from_utf8(fields.next().unwrap_or_default().to_vec())
        .map_err(|_| RepositoryError::GitOperationFailed)?;
    if kind != b"blob"
        || fields.next().is_some()
        || object.len() != object_id_length
        || !object.bytes().all(is_hex)
    {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(Some((mode, object)))
}

fn compact_root(table: Uuid) -> ManagedPath {
    ManagedPath::new(format!(".orna/storage/{table}")).expect("UUID produces safe managed path")
}

fn compact_data_root(table: Uuid) -> ManagedPath {
    managed_child(&compact_root(table), "data").expect("constant safe path")
}

fn compact_segment_path(table: Uuid, segment_id: Uuid) -> Result<ManagedPath, RepositoryError> {
    let segment = segment_id.to_string();
    let prefix = segment
        .get(..2)
        .ok_or(RepositoryError::InvalidCompactManifest)?;
    managed_child(
        &compact_data_root(table),
        &format!("{prefix}/{segment}.parquet"),
    )
}

fn shard_path(table: Uuid, number: u64) -> Result<ManagedPath, RepositoryError> {
    managed_child(&compact_root(table), &format!("shards/{number:08}.orna"))
}

fn managed_child(root: &ManagedPath, child: &str) -> Result<ManagedPath, RepositoryError> {
    let root = root
        .as_path()
        .to_str()
        .ok_or(RepositoryError::UnsafeManagedPath)?;
    ManagedPath::new(format!("{root}/{child}"))
}

fn path_key(path: &ManagedPath) -> Result<String, RepositoryError> {
    path.as_path()
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or(RepositoryError::InvalidCompactManifest)
}

fn canonical_manifest(
    manifest: &CompactManifest,
    shards: &[ShardDescriptor],
) -> Result<Vec<u8>, RepositoryError> {
    let mut output = format!(
        "{{profile: \"{COMPACT_PROFILE}\", table: \"{}\", schema: \"{}\", next_generation: {}, shards: [",
        manifest.table,
        hex(&manifest.schema),
        manifest.next_generation,
    );
    for (index, shard) in shards.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        let root = compact_root(manifest.table);
        let relative = shard
            .file
            .as_path()
            .strip_prefix(root.as_path())
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        output.push_str(&format!(
            "{{number: {}, min_key: \"{}\", max_key: \"{}\", entries: {}, file: \"{}\", hash: \"{}\"}}",
            shard.number,
            hex(&shard.min_key),
            hex(&shard.max_key),
            shard.entries,
            relative.to_str().ok_or(RepositoryError::InvalidCompactManifest)?,
            hex(&shard.hash),
        ));
    }
    output.push_str("]}\n");
    parse_row(&output)
        .is_ok()
        .then_some(output.into_bytes())
        .ok_or(RepositoryError::InvalidCompactManifest)
}

fn canonical_shard(entries: &[&CompactManifestEntry]) -> Result<Vec<u8>, RepositoryError> {
    if entries.is_empty() || entries.len() > COMPACT_MANIFEST_SHARD_LIMIT {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let mut output = String::from("{entries: [");
    for (index, entry) in entries.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str(&format!(
            "{{segment_id: \"{}\", role: \"{}\", generation: {}, schema_id: \"{}\", profile_version: 1, encoder_version: \"{}\", relative_path: \"{}\", git_object_id: \"{}\", sha256: \"{}\", min_key: \"{}\", max_key: \"{}\", min_event_time: {}, max_event_time: {}, row_count: {}, compressed_bytes: {}, columns: \"{}\", row_group_index: {}, bloom: {}}}",
            entry.segment_id,
            entry.role.as_str(),
            entry.generation,
            hex(&entry.schema),
            entry.encoder_version,
            entry.relative_path.as_path().to_str().ok_or(RepositoryError::InvalidCompactManifest)?,
            entry.git_object_id,
            hex(&entry.sha256),
            hex(&entry.min_key),
            hex(&entry.max_key),
            entry.min_event_time.as_ref().map(|value| format!("\"{value}\"")).unwrap_or_else(|| "null".into()),
            entry.max_event_time.as_ref().map(|value| format!("\"{value}\"")).unwrap_or_else(|| "null".into()),
            entry.row_count,
            entry.compressed_bytes,
            base64(&entry.columns),
            entry.row_group_index,
            entry.bloom,
        ));
    }
    output.push_str("]}\n");
    parse_row(&output)
        .is_ok()
        .then_some(output.into_bytes())
        .ok_or(RepositoryError::InvalidCompactManifest)
}

struct ParsedManifestHeader {
    table: Uuid,
    schema: [u8; 32],
    next_generation: u64,
    shards: Vec<ParsedShard>,
}

struct ParsedShard {
    file: ManagedPath,
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    entries: usize,
    hash: [u8; 32],
}

fn parse_manifest_header(bytes: &[u8]) -> Result<ParsedManifestHeader, RepositoryError> {
    let record = parse_record(bytes)?;
    reject_unknown(
        &record,
        &["profile", "table", "schema", "next_generation", "shards"],
    )?;
    if string_field(&record, "profile")? != COMPACT_PROFILE {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let table = Uuid::parse_str(string_field(&record, "table")?)
        .map_err(|_| RepositoryError::InvalidCompactManifest)?;
    let schema = parse_hex_32(string_field(&record, "schema")?)?;
    let next_generation = integer_field(&record, "next_generation")?;
    if next_generation == 0 {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let mut shards = Vec::new();
    let mut expected_number = 0_u64;
    let mut expected_shard_paths = BTreeSet::new();
    for shard in list_field(&record, "shards")? {
        let shard = as_record(shard)?;
        reject_unknown(
            &shard,
            &["number", "min_key", "max_key", "entries", "file", "hash"],
        )?;
        let number = integer_field(&shard, "number")?;
        let declared_entries = usize::try_from(integer_field(&shard, "entries")?)
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        if number != expected_number
            || declared_entries == 0
            || declared_entries > COMPACT_MANIFEST_SHARD_LIMIT
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        expected_number = expected_number
            .checked_add(1)
            .ok_or(RepositoryError::InvalidCompactManifest)?;
        let file = shard_path(table, number)?;
        let expected_relative = file
            .as_path()
            .strip_prefix(compact_root(table).as_path())
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        if string_field(&shard, "file")?
            != expected_relative
                .to_str()
                .ok_or(RepositoryError::InvalidCompactManifest)?
            || !expected_shard_paths.insert(path_key(&file)?)
        {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        let hash = parse_hex_32(string_field(&shard, "hash")?)?;
        let min_key = decode_hex(string_field(&shard, "min_key")?)?;
        let max_key = decode_hex(string_field(&shard, "max_key")?)?;
        if min_key > max_key {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        shards.push(ParsedShard {
            file,
            min_key,
            max_key,
            entries: declared_entries,
            hash,
        });
    }
    Ok(ParsedManifestHeader {
        table,
        schema,
        next_generation,
        shards,
    })
}

fn parse_shard(bytes: &[u8]) -> Result<Vec<CompactManifestEntry>, RepositoryError> {
    let record = parse_record(bytes)?;
    reject_unknown(&record, &["entries"])?;
    let mut entries = Vec::new();
    for value in list_field(&record, "entries")? {
        let fields = as_record(value)?;
        reject_unknown(
            &fields,
            &[
                "segment_id",
                "role",
                "generation",
                "schema_id",
                "profile_version",
                "encoder_version",
                "relative_path",
                "git_object_id",
                "sha256",
                "min_key",
                "max_key",
                "min_event_time",
                "max_event_time",
                "row_count",
                "compressed_bytes",
                "columns",
                "row_group_index",
                "bloom",
            ],
        )?;
        if integer_field(&fields, "profile_version")? != 1 {
            return Err(RepositoryError::InvalidCompactManifest);
        }
        let segment_id = Uuid::parse_str(string_field(&fields, "segment_id")?)
            .map_err(|_| RepositoryError::InvalidCompactManifest)?;
        let min_event_time = nullable_string_field(&fields, "min_event_time")?;
        let max_event_time = nullable_string_field(&fields, "max_event_time")?;
        let entry = CompactManifestEntry {
            segment_id,
            role: CompactSegmentRole::parse(string_field(&fields, "role")?)?,
            generation: integer_field(&fields, "generation")?,
            schema: parse_hex_32(string_field(&fields, "schema_id")?)?,
            encoder_version: string_field(&fields, "encoder_version")?.to_owned(),
            relative_path: ManagedPath::new(string_field(&fields, "relative_path")?)?,
            git_object_id: string_field(&fields, "git_object_id")?.to_owned(),
            sha256: parse_hex_32(string_field(&fields, "sha256")?)?,
            min_key: decode_hex(string_field(&fields, "min_key")?)?,
            max_key: decode_hex(string_field(&fields, "max_key")?)?,
            min_event_time,
            max_event_time,
            row_count: integer_field(&fields, "row_count")?,
            compressed_bytes: integer_field(&fields, "compressed_bytes")?,
            columns: decode_base64(string_field(&fields, "columns")?)?,
            row_group_index: boolean_field(&fields, "row_group_index")?,
            bloom: boolean_field(&fields, "bloom")?,
        };
        entries.push(entry);
    }
    if entries.is_empty() || entries.len() > COMPACT_MANIFEST_SHARD_LIMIT {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    Ok(entries)
}

fn parse_record(bytes: &[u8]) -> Result<Vec<orna_syntax_v1::RecordField>, RepositoryError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RepositoryError::InvalidCompactManifest)?;
    let parsed = parse_row(text);
    if !parsed.is_ok() {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    as_record(&parsed.value)
}

fn as_record(value: &Expr) -> Result<Vec<orna_syntax_v1::RecordField>, RepositoryError> {
    match value {
        Expr::Record { fields, .. } => Ok(fields.clone()),
        _ => Err(RepositoryError::InvalidCompactManifest),
    }
}

fn reject_unknown(
    fields: &[orna_syntax_v1::RecordField],
    names: &[&str],
) -> Result<(), RepositoryError> {
    let mut seen = BTreeSet::new();
    if fields
        .iter()
        .any(|field| !names.contains(&field.name.as_str()) || !seen.insert(field.name.as_str()))
    {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    Ok(())
}

fn field<'a>(
    fields: &'a [orna_syntax_v1::RecordField],
    name: &str,
) -> Result<&'a Expr, RepositoryError> {
    let mut values = fields
        .iter()
        .filter(|field| field.name == name)
        .map(|field| &field.value);
    let Some(value) = values.next() else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    if values.next().is_some() {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    Ok(value)
}

fn string_field<'a>(
    fields: &'a [orna_syntax_v1::RecordField],
    name: &str,
) -> Result<&'a str, RepositoryError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = field(fields, name)?
    else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    let value = text
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(RepositoryError::InvalidCompactManifest)?;
    if value.contains('\\') {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    Ok(value)
}

fn integer_field(
    fields: &[orna_syntax_v1::RecordField],
    name: &str,
) -> Result<u64, RepositoryError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = field(fields, name)?
    else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    text.parse()
        .map_err(|_| RepositoryError::InvalidCompactManifest)
}

fn nullable_string_field(
    fields: &[orna_syntax_v1::RecordField],
    name: &str,
) -> Result<Option<String>, RepositoryError> {
    match field(fields, name)? {
        Expr::Literal {
            text,
            kind: LiteralKind::Null,
            ..
        } if text == "null" => Ok(None),
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => {
            let value = text
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            if value.is_empty() || value.contains('\\') || !value.bytes().all(is_safe_atom) {
                return Err(RepositoryError::InvalidCompactManifest);
            }
            Ok(Some(value.to_owned()))
        }
        _ => Err(RepositoryError::InvalidCompactManifest),
    }
}

fn boolean_field(
    fields: &[orna_syntax_v1::RecordField],
    name: &str,
) -> Result<bool, RepositoryError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Boolean,
        ..
    } = field(fields, name)?
    else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    match text.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(RepositoryError::InvalidCompactManifest),
    }
}

fn list_field<'a>(
    fields: &'a [orna_syntax_v1::RecordField],
    name: &str,
) -> Result<&'a [Expr], RepositoryError> {
    let Expr::List { elements, .. } = field(fields, name)? else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    Ok(elements)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[usize::from(byte >> 4)] as char);
        result.push(HEX[usize::from(byte & 15)] as char);
    }
    result
}

fn decode_hex(value: &str) -> Result<Vec<u8>, RepositoryError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(is_hex) {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let (pairs, []) = value.as_bytes().as_chunks::<2>() else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    pairs
        .iter()
        .map(|pair| {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or(RepositoryError::InvalidCompactManifest)?;
            u8::try_from((high << 4) | low).map_err(|_| RepositoryError::InvalidCompactManifest)
        })
        .collect()
}

fn parse_hex_32(value: &str) -> Result<[u8; 32], RepositoryError> {
    let bytes = decode_hex(value)?;
    bytes
        .try_into()
        .map_err(|_| RepositoryError::InvalidCompactManifest)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let first = group[0];
        let second = group.get(1).copied().unwrap_or(0);
        let third = group.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[usize::from(first >> 2)] as char);
        output.push(ALPHABET[usize::from((first & 0x03) << 4 | second >> 4)] as char);
        output.push(if group.len() > 1 {
            ALPHABET[usize::from((second & 0x0f) << 2 | third >> 6)] as char
        } else {
            '='
        });
        output.push(if group.len() > 2 {
            ALPHABET[usize::from(third & 0x3f)] as char
        } else {
            '='
        });
    }
    output
}

fn decode_base64(value: &str) -> Result<Vec<u8>, RepositoryError> {
    if !value.len().is_multiple_of(4) {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let (groups, []) = value.as_bytes().as_chunks::<4>() else {
        return Err(RepositoryError::InvalidCompactManifest);
    };
    for (index, group) in groups.iter().enumerate() {
        let last = index + 1 == value.len() / 4;
        let first = base64_value(group[0])?;
        let second = base64_value(group[1])?;
        let (third, fourth) = match (group[2], group[3]) {
            (b'=', b'=') if last => (None, None),
            (third, b'=') if last => (Some(base64_value(third)?), None),
            (third, fourth) if third != b'=' && fourth != b'=' => {
                (Some(base64_value(third)?), Some(base64_value(fourth)?))
            }
            _ => return Err(RepositoryError::InvalidCompactManifest),
        };
        output.push(first << 2 | second >> 4);
        if let Some(third) = third {
            output.push(second << 4 | third >> 2);
            if let Some(fourth) = fourth {
                output.push(third << 6 | fourth);
            }
        }
    }
    if base64(&output) != value {
        return Err(RepositoryError::InvalidCompactManifest);
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Result<u8, RepositoryError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(RepositoryError::InvalidCompactManifest),
    }
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}
fn is_safe_atom(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'+')
}

fn is_selected_ref(value: &str) -> bool {
    value.starts_with("refs/heads/")
        && value.len() > "refs/heads/".len()
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.contains("..")
        && value.bytes().all(|byte| is_safe_atom(byte) || byte == b'/')
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), RepositoryError> {
    let value = u32::try_from(value).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    bytes.extend_from_slice(&value.to_be_bytes());
    Ok(())
}
fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), RepositoryError> {
    put_u32(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}
fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, RepositoryError> {
    Ok(u32::from_be_bytes(take_fixed::<4>(bytes, cursor)?))
}
fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, RepositoryError> {
    Ok(u64::from_be_bytes(take_fixed::<8>(bytes, cursor)?))
}
fn take_string(bytes: &[u8], cursor: &mut usize) -> Result<String, RepositoryError> {
    let length = usize::try_from(take_u32(bytes, cursor)?)
        .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    String::from_utf8(value.to_vec()).map_err(|_| RepositoryError::InvalidPublicationJournal)
}
fn take_fixed<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<[u8; N], RepositoryError> {
    let end = cursor
        .checked_add(N)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    value
        .try_into()
        .map_err(|_| RepositoryError::InvalidPublicationJournal)
}

impl fmt::Display for CompactSegmentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
