//! Descriptor-driven physical compact key decoding.
//!
//! This module is the first physical reader slice. It decodes only required
//! INT64, BOOLEAN, and UTF-8 BYTE_ARRAY key columns from an already verified compact Parquet segment and
//! emits the canonical OVB scalar/tuple representation consumed by the frozen
//! logical generation index. It does not decode arbitrary rows or infer a
//! logical type from a Parquet sample.

use std::{cmp::Ordering, collections::BTreeMap, error::Error, fmt};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::{
    CompactManifest, CompactManifestEntry, GitCommitRef, Repository, RepositoryError, Uuid,
};
use parquet::{
    basic::Type,
    column::reader::ColumnReader,
    file::reader::{FileReader, SerializedFileReader},
};

use crate::compact::{
    CompactExactKeySource, CompactKeyError, CompactOvbProfile, COMPACT_STORAGE_PROFILE,
};

/// A physical reader failure. Unsupported mappings are explicit: this slice
/// does not claim to implement the complete compact profile.
#[derive(Debug)]
pub enum CompactParquetError {
    Repository(RepositoryError),
    InvalidParquet,
    InvalidMetadata,
    InvalidUtf8,
    UnsupportedKeyMapping,
    MissingKeyColumn([u8; 16]),
    DuplicateFieldColumn([u8; 16]),
    NullKey,
    RowCountMismatch { expected: u64, observed: u64 },
    UnorderedPrimaryKeys,
    Key(CompactKeyError),
    SegmentUnavailable(Uuid),
}

impl fmt::Display for CompactParquetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => error.fmt(f),
            Self::InvalidParquet => f.write_str("invalid compact Parquet data"),
            Self::InvalidMetadata => f.write_str("invalid compact Parquet metadata"),
            Self::InvalidUtf8 => f.write_str("compact string key is not valid UTF-8"),
            Self::UnsupportedKeyMapping => f.write_str("unsupported compact key mapping"),
            Self::MissingKeyColumn(id) => write!(f, "compact key column is missing: {id:?}"),
            Self::DuplicateFieldColumn(id) => {
                write!(f, "compact field column is duplicated: {id:?}")
            }
            Self::NullKey => f.write_str("compact primary-key column contains a null"),
            Self::RowCountMismatch { expected, observed } => {
                write!(
                    f,
                    "compact row count mismatch: expected {expected}, observed {observed}"
                )
            }
            Self::UnorderedPrimaryKeys => {
                f.write_str("compact primary keys are not in canonical order")
            }
            Self::Key(error) => error.fmt(f),
            Self::SegmentUnavailable(id) => write!(f, "compact segment is unavailable: {id}"),
        }
    }
}

impl Error for CompactParquetError {}

impl From<RepositoryError> for CompactParquetError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

/// A repository-backed exact-key source for one validated compact manifest.
/// Segment bytes are loaded through the repository's verified committed-byte
/// boundary before any logical key is exposed.
pub struct CompactParquetKeySource {
    profile: CompactOvbProfile,
    table: Uuid,
    segments: BTreeMap<Uuid, Vec<u8>>,
}

impl fmt::Debug for CompactParquetKeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompactParquetKeySource")
            .field("table", &self.table)
            .field("segments", &self.segments.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CompactParquetKeySource {
    /// Loads every segment named by a validated manifest from one committed
    /// Git snapshot. The profile and manifest coordinates are checked before
    /// the physical reader is made available.
    pub fn from_verified_manifest(
        repository: &Repository,
        commit: &GitCommitRef,
        manifest: &CompactManifest,
        profile: CompactOvbProfile,
    ) -> Result<Self, CompactParquetError> {
        if manifest.table().as_bytes() != &profile.table_id() {
            return Err(CompactParquetError::Key(CompactKeyError::WrongTable));
        }
        if manifest.schema() != profile.schema_fingerprint() {
            return Err(CompactParquetError::Key(CompactKeyError::WrongSchema));
        }
        let mut segments = BTreeMap::new();
        for entry in manifest.entries() {
            let bytes =
                repository.read_verified_compact_segment(commit, manifest.table(), entry)?;
            segments.insert(entry.segment_id(), bytes);
        }
        Ok(Self {
            table: manifest.table(),
            profile,
            segments,
        })
    }

    /// Decodes one physical segment directly. This is useful for storage
    /// adapters which already obtained bytes through an equivalent verified
    /// repository boundary and for cross-reader tests.
    pub fn decode_verified_bytes(
        profile: &CompactOvbProfile,
        table: Uuid,
        bytes: &[u8],
        expected_row_count: u64,
    ) -> Result<Vec<Vec<u8>>, CompactParquetError> {
        if table.as_bytes() != &profile.table_id() {
            return Err(CompactParquetError::Key(CompactKeyError::WrongTable));
        }
        ensure_supported_profile(profile)?;
        let reader = SerializedFileReader::new(Bytes::copy_from_slice(bytes))
            .map_err(|_| CompactParquetError::InvalidParquet)?;
        validate_file_metadata(&reader, profile, table, expected_row_count)?;
        let key_columns = key_columns(&reader, profile)?;
        let mut values: Vec<Vec<OvbRaw>> = vec![Vec::new(); key_columns.len()];
        let mut observed = 0u64;
        for row_group_index in 0..reader.num_row_groups() {
            let row_group = reader
                .get_row_group(row_group_index)
                .map_err(|_| CompactParquetError::InvalidParquet)?;
            let rows = row_group.metadata().num_rows();
            if rows <= 0 {
                return Err(CompactParquetError::InvalidParquet);
            }
            let rows = usize::try_from(rows).map_err(|_| CompactParquetError::InvalidParquet)?;
            let mut group_values: Vec<Vec<OvbRaw>> = Vec::with_capacity(key_columns.len());
            for column in &key_columns {
                let column_values = match column.kind {
                    KeyColumnKind::Int => read_int64_column(&*row_group, column.index, rows)?
                        .into_iter()
                        .map(|value| OvbRaw::Int(value.into()))
                        .collect(),
                    KeyColumnKind::Bool => read_bool_column(&*row_group, column.index, rows)?
                        .into_iter()
                        .map(OvbRaw::Bool)
                        .collect(),
                    KeyColumnKind::Str => read_str_column(&*row_group, column.index, rows)?
                        .into_iter()
                        .map(OvbRaw::Text)
                        .collect(),
                    KeyColumnKind::Date => read_int32_column(&*row_group, column.index, rows)?
                        .into_iter()
                        .map(date_value)
                        .collect::<Result<_, _>>()?,
                };
                group_values.push(column_values);
            }
            for (destination, source) in values.iter_mut().zip(group_values) {
                destination.extend(source);
            }
            observed = observed
                .checked_add(u64::try_from(rows).map_err(|_| CompactParquetError::InvalidParquet)?)
                .ok_or(CompactParquetError::RowCountMismatch {
                    expected: expected_row_count,
                    observed: u64::MAX,
                })?;
        }
        if observed != expected_row_count {
            return Err(CompactParquetError::RowCountMismatch {
                expected: expected_row_count,
                observed,
            });
        }
        let rows =
            usize::try_from(expected_row_count).map_err(|_| CompactParquetError::InvalidParquet)?;
        let mut encoded = Vec::with_capacity(rows);
        let mut previous: Option<Vec<OvbRaw>> = None;
        for row in 0..rows {
            let components = key_columns
                .iter()
                .enumerate()
                .map(|(column, _)| values[column][row].clone())
                .collect::<Vec<_>>();
            if let Some(previous) = &previous {
                if compare_key_components(previous, &components)? == Ordering::Greater {
                    return Err(CompactParquetError::UnorderedPrimaryKeys);
                }
            }
            previous = Some(components.clone());
            let raw = match components.as_slice() {
                [value] => value.clone(),
                [] => return Err(CompactParquetError::UnsupportedKeyMapping),
                _ => OvbRaw::Tag(60015, Box::new(OvbRaw::Array(components))),
            };
            let bytes = CanonicalValue::new(raw)
                .map_err(|_| CompactParquetError::InvalidMetadata)?
                .encode()
                .map_err(|_| CompactParquetError::InvalidMetadata)?;
            profile
                .decode_key(&bytes)
                .map_err(CompactParquetError::Key)?;
            encoded.push(bytes);
        }
        Ok(encoded)
    }

    fn exact_keys_for_entry(
        &self,
        entry: &CompactManifestEntry,
    ) -> Result<Vec<Vec<u8>>, CompactParquetError> {
        let bytes = self
            .segments
            .get(&entry.segment_id())
            .ok_or_else(|| CompactParquetError::SegmentUnavailable(entry.segment_id()))?;
        Self::decode_verified_bytes(&self.profile, self.table, bytes, entry.row_count())
    }
}

fn compare_key_components(
    left: &[OvbRaw],
    right: &[OvbRaw],
) -> Result<Ordering, CompactParquetError> {
    if left.len() != right.len() {
        return Err(CompactParquetError::InvalidMetadata);
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (OvbRaw::Int(left), OvbRaw::Int(right)) => left.cmp(right),
            (OvbRaw::Bool(left), OvbRaw::Bool(right)) => left.cmp(right),
            (OvbRaw::Text(left), OvbRaw::Text(right)) => left.cmp(right),
            (OvbRaw::Tag(60001, left), OvbRaw::Tag(60001, right)) => {
                let (OvbRaw::Text(left), OvbRaw::Text(right)) = (left.as_ref(), right.as_ref())
                else {
                    return Err(CompactParquetError::InvalidMetadata);
                };
                left.cmp(right)
            }
            _ => return Err(CompactParquetError::InvalidMetadata),
        };
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

impl CompactExactKeySource for CompactParquetKeySource {
    type Error = CompactParquetError;

    fn exact_keys<'a>(
        &'a self,
        entry: &'a CompactManifestEntry,
    ) -> Result<Box<dyn Iterator<Item = Result<Vec<u8>, Self::Error>> + 'a>, Self::Error> {
        Ok(Box::new(
            self.exact_keys_for_entry(entry)?.into_iter().map(Ok),
        ))
    }
}

#[derive(Clone, Copy, Debug)]
struct KeyColumn {
    index: usize,
    kind: KeyColumnKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyColumnKind {
    Int,
    Bool,
    Str,
    Date,
}

fn ensure_supported_profile(
    profile: &CompactOvbProfile,
) -> Result<Vec<KeyColumnKind>, CompactParquetError> {
    let kinds = profile_key_kinds(profile)?;
    if kinds.is_empty() {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    }
    let components = kinds
        .iter()
        .map(|kind| match kind {
            KeyColumnKind::Int => OvbRaw::Int(0.into()),
            KeyColumnKind::Bool => OvbRaw::Bool(false),
            KeyColumnKind::Str => OvbRaw::Text(String::new()),
            KeyColumnKind::Date => OvbRaw::Tag(60001, Box::new(OvbRaw::Text("1970-01-01".into()))),
        })
        .collect::<Vec<_>>();
    let raw = match components.as_slice() {
        [value] => value.clone(),
        [] => return Err(CompactParquetError::UnsupportedKeyMapping),
        _ => OvbRaw::Tag(60015, Box::new(OvbRaw::Array(components))),
    };
    let bytes = CanonicalValue::new(raw)
        .map_err(|_| CompactParquetError::InvalidMetadata)?
        .encode()
        .map_err(|_| CompactParquetError::InvalidMetadata)?;
    profile
        .decode_key(&bytes)
        .map(|_| kinds)
        .map_err(|_| CompactParquetError::UnsupportedKeyMapping)
}

fn validate_file_metadata(
    reader: &SerializedFileReader<Bytes>,
    profile: &CompactOvbProfile,
    table: Uuid,
    expected_row_count: u64,
) -> Result<(), CompactParquetError> {
    let metadata = reader.metadata().file_metadata();
    if metadata.num_rows()
        != i64::try_from(expected_row_count).map_err(|_| CompactParquetError::InvalidMetadata)?
    {
        return Err(CompactParquetError::RowCountMismatch {
            expected: expected_row_count,
            observed: u64::try_from(metadata.num_rows()).unwrap_or(0),
        });
    }
    let mut values = BTreeMap::new();
    for item in metadata.key_value_metadata().into_iter().flatten() {
        if values
            .insert(item.key.as_str(), item.value.as_deref())
            .is_some()
        {
            return Err(CompactParquetError::InvalidMetadata);
        }
    }
    let expected_table = table.to_string();
    let profile_value = values.get("orna.profile").and_then(|value| *value);
    let table_value = values.get("orna.table").and_then(|value| *value);
    if profile_value != Some(COMPACT_STORAGE_PROFILE)
        || table_value != Some(expected_table.as_str())
    {
        return Err(CompactParquetError::InvalidMetadata);
    }
    let Some(schema_hash) = values.get("orna.schema.sha256").and_then(|value| *value) else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    if schema_hash != hex_digest(profile.schema_fingerprint()) {
        return Err(CompactParquetError::InvalidMetadata);
    }
    let Some(columns) = values.get("orna.columns.ovb").and_then(|value| *value) else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    if BASE64.decode(columns).is_err() {
        return Err(CompactParquetError::InvalidMetadata);
    }
    Ok(())
}

fn key_columns(
    reader: &SerializedFileReader<Bytes>,
    profile: &CompactOvbProfile,
) -> Result<Vec<KeyColumn>, CompactParquetError> {
    let expected_kinds = ensure_supported_profile(profile)?;
    let expected_by_id = profile
        .key_field_ids()
        .zip(expected_kinds)
        .collect::<BTreeMap<_, _>>();
    let metadata = reader.metadata().file_metadata();
    let columns = metadata
        .key_value_metadata()
        .into_iter()
        .flatten()
        .find(|item| item.key == "orna.columns.ovb")
        .and_then(|item| item.value.as_deref())
        .and_then(|value| BASE64.decode(value).ok())
        .ok_or(CompactParquetError::InvalidMetadata)?;
    let descriptors =
        CanonicalValue::decode(&columns).map_err(|_| CompactParquetError::InvalidMetadata)?;
    let OvbRaw::Array(descriptors) = descriptors.raw() else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    let schema = metadata.schema_descr();
    if descriptors.len() != schema.num_columns() {
        return Err(CompactParquetError::InvalidMetadata);
    }
    let mut by_id = BTreeMap::new();
    for (index, (descriptor, column)) in descriptors.iter().zip(schema.columns()).enumerate() {
        let (id, kind) = descriptor_field_id(descriptor, column, profile)?;
        if let Some(expected) = expected_by_id.get(&id) {
            if Some(*expected) != kind {
                return Err(CompactParquetError::UnsupportedKeyMapping);
            }
        }
        if by_id.insert(id, (index, kind)).is_some() {
            return Err(CompactParquetError::DuplicateFieldColumn(id));
        }
    }
    profile
        .key_field_ids()
        .map(|id| {
            by_id
                .get(&id)
                .copied()
                .and_then(|(index, kind)| kind.map(|kind| KeyColumn { index, kind }))
                .ok_or(CompactParquetError::MissingKeyColumn(id))
        })
        .collect()
}

fn descriptor_field_id(
    descriptor: &OvbRaw,
    column: &parquet::schema::types::ColumnDescriptor,
    profile: &CompactOvbProfile,
) -> Result<([u8; 16], Option<KeyColumnKind>), CompactParquetError> {
    let OvbRaw::Array(fields) = descriptor else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    if fields.len() != 5 {
        return Err(CompactParquetError::InvalidMetadata);
    }
    let OvbRaw::Array(field_path) = &fields[0] else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    let [OvbRaw::Tag(37, field_id)] = field_path.as_slice() else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    let OvbRaw::Bytes(field_id) = field_id.as_ref() else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    let id: [u8; 16] = field_id
        .as_slice()
        .try_into()
        .map_err(|_| CompactParquetError::InvalidMetadata)?;
    let OvbRaw::Array(path) = &fields[1] else {
        return Err(CompactParquetError::InvalidMetadata);
    };
    if path.len() != 1
        || !matches!(&path[0], OvbRaw::Text(value) if value == &format!("f_{}", Uuid::from_bytes(id).simple()))
        || column.path().parts().len() != 1
        || column.path().parts()[0] != format!("f_{}", Uuid::from_bytes(id).simple())
    {
        return Err(CompactParquetError::InvalidMetadata);
    }
    let kind = if profile.key_field_ids().any(|key| key == id) {
        let OvbRaw::Array(logical_type) = &fields[2] else {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        };
        let kind = if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Int".to_owned())]
            && matches!(&fields[3], OvbRaw::Text(value) if value == "int64")
            && column.physical_type() == Type::INT64
        {
            KeyColumnKind::Int
        } else if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Bool".to_owned())]
            && matches!(&fields[3], OvbRaw::Text(value) if value == "bool")
            && column.physical_type() == Type::BOOLEAN
        {
            KeyColumnKind::Bool
        } else if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Str".to_owned())]
            && matches!(&fields[3], OvbRaw::Text(value) if value == "utf8")
            && column.physical_type() == Type::BYTE_ARRAY
            && column.logical_type_ref() == Some(&parquet::basic::LogicalType::String)
        {
            KeyColumnKind::Str
        } else if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Date".to_owned())]
            && matches!(&fields[3], OvbRaw::Text(value) if value == "date")
            && column.physical_type() == Type::INT32
            && column.logical_type_ref() == Some(&parquet::basic::LogicalType::Date)
        {
            KeyColumnKind::Date
        } else {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        };
        if !matches!(&fields[4], OvbRaw::Array(parameters) if parameters.is_empty())
            || (!(kind == KeyColumnKind::Str || kind == KeyColumnKind::Date)
                && column.logical_type_ref().is_some())
            || column.max_rep_level() != 0
        {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        }
        Some(kind)
    } else {
        None
    };
    Ok((id, kind))
}

fn profile_key_kinds(
    profile: &CompactOvbProfile,
) -> Result<Vec<KeyColumnKind>, CompactParquetError> {
    let OvbRaw::Map(schema) = profile.schema().raw() else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let field = |key: i64| {
        schema
            .iter()
            .find(|(candidate, _)| *candidate == OvbRaw::Int(key.into()))
            .map(|(_, value)| value)
    };
    let OvbRaw::Array(key_ids) = field(2).ok_or(CompactParquetError::UnsupportedKeyMapping)? else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let OvbRaw::Array(fields) = field(3).ok_or(CompactParquetError::UnsupportedKeyMapping)? else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let mut field_types = BTreeMap::new();
    for field in fields {
        let OvbRaw::Array(field) = field else {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        };
        if field.len() != 5 {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        }
        let OvbRaw::Tag(37, value) = &field[0] else {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        };
        let OvbRaw::Bytes(value) = value.as_ref() else {
            return Err(CompactParquetError::UnsupportedKeyMapping);
        };
        let id: [u8; 16] = value
            .as_slice()
            .try_into()
            .map_err(|_| CompactParquetError::UnsupportedKeyMapping)?;
        let kind = match &field[2] {
            OvbRaw::Array(logical_type)
                if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Int".to_owned())] =>
            {
                KeyColumnKind::Int
            }
            OvbRaw::Array(logical_type)
                if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Bool".to_owned())] =>
            {
                KeyColumnKind::Bool
            }
            OvbRaw::Array(logical_type)
                if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Str".to_owned())] =>
            {
                KeyColumnKind::Str
            }
            OvbRaw::Array(logical_type)
                if logical_type == &[OvbRaw::Int(0.into()), OvbRaw::Text("Date".to_owned())] =>
            {
                KeyColumnKind::Date
            }
            _ => continue,
        };
        field_types.insert(id, kind);
    }
    key_ids
        .iter()
        .map(|key_id| {
            let OvbRaw::Tag(37, value) = key_id else {
                return Err(CompactParquetError::UnsupportedKeyMapping);
            };
            let OvbRaw::Bytes(value) = value.as_ref() else {
                return Err(CompactParquetError::UnsupportedKeyMapping);
            };
            let id: [u8; 16] = value
                .as_slice()
                .try_into()
                .map_err(|_| CompactParquetError::UnsupportedKeyMapping)?;
            field_types
                .get(&id)
                .copied()
                .ok_or(CompactParquetError::UnsupportedKeyMapping)
        })
        .collect()
}

fn read_int64_column(
    row_group: &dyn parquet::file::reader::RowGroupReader,
    index: usize,
    expected_rows: usize,
) -> Result<Vec<i64>, CompactParquetError> {
    let reader = row_group
        .get_column_reader(index)
        .map_err(|_| CompactParquetError::InvalidParquet)?;
    let ColumnReader::Int64ColumnReader(mut reader) = reader else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let descriptor = row_group.metadata().schema_descr().column(index);
    if descriptor.max_rep_level() != 0 {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    }
    let mut values = Vec::with_capacity(expected_rows);
    let mut definition_levels = Vec::new();
    let mut records = 0usize;
    loop {
        let remaining = expected_rows.saturating_sub(records);
        if remaining == 0 {
            break;
        }
        let definition_levels_ref =
            (descriptor.max_def_level() != 0).then_some(&mut definition_levels);
        let (read_records, values_read, levels_read) = reader
            .read_records(remaining, definition_levels_ref, None, &mut values)
            .map_err(|_| CompactParquetError::InvalidParquet)?;
        if read_records == 0 {
            break;
        }
        if levels_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        if descriptor.max_def_level() != 0
            && definition_levels
                .iter()
                .any(|level| *level != descriptor.max_def_level())
        {
            return Err(CompactParquetError::NullKey);
        }
        if values_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        records = records
            .checked_add(read_records)
            .ok_or(CompactParquetError::InvalidParquet)?;
    }
    if records != expected_rows || values.len() != expected_rows {
        return Err(CompactParquetError::RowCountMismatch {
            expected: expected_rows as u64,
            observed: records as u64,
        });
    }
    Ok(values)
}

fn read_int32_column(
    row_group: &dyn parquet::file::reader::RowGroupReader,
    index: usize,
    expected_rows: usize,
) -> Result<Vec<i32>, CompactParquetError> {
    let reader = row_group
        .get_column_reader(index)
        .map_err(|_| CompactParquetError::InvalidParquet)?;
    let ColumnReader::Int32ColumnReader(mut reader) = reader else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let descriptor = row_group.metadata().schema_descr().column(index);
    if descriptor.max_rep_level() != 0 {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    }
    let mut values = Vec::with_capacity(expected_rows);
    let mut definition_levels = Vec::new();
    let mut records = 0usize;
    while records < expected_rows {
        let remaining = expected_rows - records;
        let definition_levels_ref =
            (descriptor.max_def_level() != 0).then_some(&mut definition_levels);
        let (read_records, values_read, levels_read) = reader
            .read_records(remaining, definition_levels_ref, None, &mut values)
            .map_err(|_| CompactParquetError::InvalidParquet)?;
        if read_records == 0 {
            break;
        }
        if levels_read != read_records || values_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        if descriptor.max_def_level() != 0
            && definition_levels
                .iter()
                .any(|level| *level != descriptor.max_def_level())
        {
            return Err(CompactParquetError::NullKey);
        }
        records = records
            .checked_add(read_records)
            .ok_or(CompactParquetError::InvalidParquet)?;
    }
    if records != expected_rows || values.len() != expected_rows {
        return Err(CompactParquetError::RowCountMismatch {
            expected: expected_rows as u64,
            observed: records as u64,
        });
    }
    Ok(values)
}

fn date_value(days: i32) -> Result<OvbRaw, CompactParquetError> {
    let z = i64::from(days) + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month0 = (5 * doy + 2) / 153;
    let day = doy - (153 * month0 + 2) / 5 + 1;
    let month = month0 + if month0 < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    if !(1..=9_999).contains(&year) {
        return Err(CompactParquetError::InvalidMetadata);
    }
    Ok(OvbRaw::Tag(
        60001,
        Box::new(OvbRaw::Text(format!("{year:04}-{month:02}-{day:02}"))),
    ))
}

fn read_bool_column(
    row_group: &dyn parquet::file::reader::RowGroupReader,
    index: usize,
    expected_rows: usize,
) -> Result<Vec<bool>, CompactParquetError> {
    let reader = row_group
        .get_column_reader(index)
        .map_err(|_| CompactParquetError::InvalidParquet)?;
    let ColumnReader::BoolColumnReader(mut reader) = reader else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let descriptor = row_group.metadata().schema_descr().column(index);
    if descriptor.max_rep_level() != 0 {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    }
    let mut values = Vec::with_capacity(expected_rows);
    let mut definition_levels = Vec::new();
    let mut records = 0usize;
    loop {
        let remaining = expected_rows.saturating_sub(records);
        if remaining == 0 {
            break;
        }
        let definition_levels_ref =
            (descriptor.max_def_level() != 0).then_some(&mut definition_levels);
        let (read_records, values_read, levels_read) = reader
            .read_records(remaining, definition_levels_ref, None, &mut values)
            .map_err(|_| CompactParquetError::InvalidParquet)?;
        if read_records == 0 {
            break;
        }
        if levels_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        if descriptor.max_def_level() != 0
            && definition_levels
                .iter()
                .any(|level| *level != descriptor.max_def_level())
        {
            return Err(CompactParquetError::NullKey);
        }
        if values_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        records = records
            .checked_add(read_records)
            .ok_or(CompactParquetError::InvalidParquet)?;
    }
    if records != expected_rows || values.len() != expected_rows {
        return Err(CompactParquetError::RowCountMismatch {
            expected: expected_rows as u64,
            observed: records as u64,
        });
    }
    Ok(values)
}

fn read_str_column(
    row_group: &dyn parquet::file::reader::RowGroupReader,
    index: usize,
    expected_rows: usize,
) -> Result<Vec<String>, CompactParquetError> {
    let reader = row_group
        .get_column_reader(index)
        .map_err(|_| CompactParquetError::InvalidParquet)?;
    let ColumnReader::ByteArrayColumnReader(mut reader) = reader else {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    };
    let descriptor = row_group.metadata().schema_descr().column(index);
    if descriptor.max_rep_level() != 0 {
        return Err(CompactParquetError::UnsupportedKeyMapping);
    }
    let mut values = Vec::with_capacity(expected_rows);
    let mut definition_levels = Vec::new();
    let mut records = 0usize;
    loop {
        let remaining = expected_rows.saturating_sub(records);
        if remaining == 0 {
            break;
        }
        let definition_levels_ref =
            (descriptor.max_def_level() != 0).then_some(&mut definition_levels);
        let (read_records, values_read, levels_read) = reader
            .read_records(remaining, definition_levels_ref, None, &mut values)
            .map_err(|_| CompactParquetError::InvalidParquet)?;
        if read_records == 0 {
            break;
        }
        if levels_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        if descriptor.max_def_level() != 0
            && definition_levels
                .iter()
                .any(|level| *level != descriptor.max_def_level())
        {
            return Err(CompactParquetError::NullKey);
        }
        if values_read != read_records {
            return Err(CompactParquetError::NullKey);
        }
        records = records
            .checked_add(read_records)
            .ok_or(CompactParquetError::InvalidParquet)?;
    }
    if records != expected_rows || values.len() != expected_rows {
        return Err(CompactParquetError::RowCountMismatch {
            expected: expected_rows as u64,
            observed: records as u64,
        });
    }
    values
        .into_iter()
        .map(|value| {
            String::from_utf8(value.data().to_vec()).map_err(|_| CompactParquetError::InvalidUtf8)
        })
        .collect()
}

fn hex_digest(bytes: [u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in bytes {
        value.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        value.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_foundation_v1::SchemaDescriptor;
    use orna_repository_v1::{
        CompactManifest, CompactSegment, CompactSegmentRole, ManagedFileChange, ManagedPath,
    };
    use parquet::{
        basic::Compression,
        data_type::{BoolType, ByteArray, ByteArrayType, Int32Type, Int64Type},
        file::{
            metadata::KeyValue,
            properties::{WriterProperties, WriterVersion},
            writer::SerializedFileWriter,
        },
        schema::parser::parse_message_type,
    };
    use sha2::{Digest, Sha256};
    use std::{fs, path::Path, process::Command, sync::Arc};
    use tempfile::TempDir;

    const TABLE: Uuid = Uuid::from_u128(1);
    const KEY_A: [u8; 16] = [
        0x01, 0x8f, 0, 0, 0, 0, 0x70, 0, 0x80, 0, 0, 0, 0, 0, 0, 0x01,
    ];
    const KEY_B: [u8; 16] = [0x21; 16];
    const SEGMENT_ID: Uuid = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
    const VERIFIED_PARQUET: &str = "UEFSMRUGFQoVChXq3ovdBUwVAhUAFQIVChUAFQASAACAAgQBAhkSAhkYCAEAAAAAAAAAGRgIAQAAAAAAAAAVAhkWAAAZHBYIFTYWAAAAFQIZLEgGc2NoZW1hFQIAFQQlABgiZl8wMThmMDAwMDAwMDA3MDAwODAwMDAwMDAwMDAwMDAwMQAWAhkcGRwmABwVBBklBgoZGCJmXzAxOGYwMDAwMDAwMDcwMDA4MDAwMDAwMDAwMDAwMDAxFQwWAhY2FkImCDw2ACgIAQAAAAAAAAAYCAEAAAAAAAAAEREAABZ8FRQWPhU+ABY2FgImCBZCFAAAGXwYDG9ybmEucHJvZmlsZRgSY29tcGFjdC1zdG9yYWdlLXYxABgKb3JuYS50YWJsZRgkMDAwMDAwMDAtMDAwMC0wMDAwLTAwMDAtMDAwMDAwMDAwMDAxABgSb3JuYS5zY2hlbWEuc2hhMjU2GEAwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3MDcwNzA3ABgPb3JuYS5zY2hlbWEub3ZiGARBQT09ABgQb3JuYS5jb2x1bW5zLm92YhhgZ1lXQjJDVlFBWThBQUFBQWNBQ0FBQUFBQUFBQUFZRjRJbVpmTURFNFpqQXdNREF3TURBd056QXdNRGd3TURBd01EQXdNREF3TURBd01ER0NBR05KYm5SbGFXNTBOalNBABgMb3JuYS5lbmNvZGVyGA90ZXN0LWVuY29kZXItdjEAGBFvcm5hLnRlc3QucGF5bG9hZBgOY29tcGFjdCBvYmplY3QAGBlwYXJxdWV0LXJzIHZlcnNpb24gNTkuMy4wGRwcAAAARgIAAFBBUjE=";

    fn uuid_raw(id: [u8; 16]) -> OvbRaw {
        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(id.to_vec())))
    }

    fn int_type() -> OvbRaw {
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".into())])
    }

    fn bool_type() -> OvbRaw {
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Bool".into())])
    }

    fn str_type() -> OvbRaw {
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Str".into())])
    }

    fn date_type() -> OvbRaw {
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Date".into())])
    }

    fn field_with_type(id: [u8; 16], logical_type: OvbRaw) -> OvbRaw {
        OvbRaw::Array(vec![
            uuid_raw(id),
            OvbRaw::Text(format!("f_{}", Uuid::from_bytes(id).simple())),
            logical_type,
            OvbRaw::Int(0.into()),
            OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
        ])
    }

    fn profile(key_ids: &[[u8; 16]]) -> CompactOvbProfile {
        profile_with_types(
            key_ids,
            &key_ids.iter().map(|_| int_type()).collect::<Vec<_>>(),
        )
    }

    fn profile_with_types(key_ids: &[[u8; 16]], key_types: &[OvbRaw]) -> CompactOvbProfile {
        assert_eq!(key_ids.len(), key_types.len());
        let mut field_ids = key_ids.to_vec();
        field_ids.sort();
        let types = key_ids
            .iter()
            .copied()
            .zip(key_types.iter().cloned())
            .collect::<BTreeMap<_, _>>();
        let schema = OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (OvbRaw::Int(1.into()), uuid_raw(*TABLE.as_bytes())),
            (
                OvbRaw::Int(2.into()),
                OvbRaw::Array(key_ids.iter().copied().map(uuid_raw).collect()),
            ),
            (
                OvbRaw::Int(3.into()),
                OvbRaw::Array(
                    field_ids
                        .into_iter()
                        .map(|id| field_with_type(id, types[&id].clone()))
                        .collect(),
                ),
            ),
            (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
        ]);
        let descriptor = SchemaDescriptor::new(schema);
        assert!(descriptor.is_ok(), "schema descriptor: {descriptor:?}");
        CompactOvbProfile::new(descriptor.unwrap()).unwrap()
    }

    fn descriptor(id: [u8; 16], logical_type: OvbRaw) -> OvbRaw {
        descriptor_with_encoding(id, logical_type, "int64")
    }

    fn descriptor_with_encoding(id: [u8; 16], logical_type: OvbRaw, encoding: &str) -> OvbRaw {
        OvbRaw::Array(vec![
            OvbRaw::Array(vec![uuid_raw(id)]),
            OvbRaw::Array(vec![OvbRaw::Text(format!(
                "f_{}",
                Uuid::from_bytes(id).simple()
            ))]),
            logical_type,
            OvbRaw::Text(encoding.into()),
            OvbRaw::Array(Vec::new()),
        ])
    }

    fn parquet(
        profile: &CompactOvbProfile,
        ids: &[[u8; 16]],
        values: &[Vec<i64>],
        optional_first: bool,
        descriptor_types: Option<Vec<OvbRaw>>,
    ) -> Vec<u8> {
        let mut message = String::from("message schema {");
        for (index, id) in ids.iter().enumerate() {
            let repetition = if optional_first && index == 0 {
                "OPTIONAL"
            } else {
                "REQUIRED"
            };
            message.push_str(&format!(
                " {repetition} INT64 f_{};",
                Uuid::from_bytes(*id).simple()
            ));
        }
        message.push('}');
        let schema = Arc::new(parse_message_type(&message).unwrap());
        let descriptors = descriptor_types.unwrap_or_else(|| {
            ids.iter()
                .copied()
                .map(|id| descriptor(id, int_type()))
                .collect()
        });
        let columns = CanonicalValue::new(OvbRaw::Array(descriptors))
            .unwrap()
            .encode()
            .unwrap();
        let metadata = vec![
            KeyValue::new("orna.profile".into(), Some(COMPACT_STORAGE_PROFILE.into())),
            KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
            KeyValue::new(
                "orna.schema.sha256".into(),
                Some(hex_digest(profile.schema_fingerprint())),
            ),
            KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
        ];
        let properties = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::ZSTD(Default::default()))
                .set_dictionary_enabled(false)
                .set_writer_version(WriterVersion::PARQUET_2_0)
                .set_key_value_metadata(Some(metadata))
                .build(),
        );
        let mut bytes = Vec::new();
        let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
        let mut row_group = writer.next_row_group().unwrap();
        for (index, column_values) in values.iter().enumerate() {
            let mut column = row_group.next_column().unwrap().unwrap();
            if optional_first && index == 0 {
                let definitions = vec![0, 1];
                column
                    .typed::<Int64Type>()
                    .write_batch(&column_values[1..], Some(&definitions), None)
                    .unwrap();
            } else {
                column
                    .typed::<Int64Type>()
                    .write_batch(column_values, None, None)
                    .unwrap();
            }
            column.close().unwrap();
        }
        row_group.close().unwrap();
        writer.close().unwrap();
        bytes
    }

    enum TestColumn<'a> {
        Int(&'a [i64]),
        Bool(&'a [bool]),
        Str(&'a [Vec<u8>]),
        Date(&'a [i32]),
        DateWithoutAnnotation(&'a [i32]),
    }

    fn mixed_parquet(
        profile: &CompactOvbProfile,
        ids: &[[u8; 16]],
        values: &[TestColumn<'_>],
        optional_first: bool,
        descriptor_types: Option<Vec<OvbRaw>>,
    ) -> Vec<u8> {
        mixed_parquet_with_dictionary(
            profile,
            ids,
            values,
            optional_first,
            descriptor_types,
            false,
        )
    }

    fn mixed_parquet_with_dictionary(
        profile: &CompactOvbProfile,
        ids: &[[u8; 16]],
        values: &[TestColumn<'_>],
        optional_first: bool,
        descriptor_types: Option<Vec<OvbRaw>>,
        dictionary_enabled: bool,
    ) -> Vec<u8> {
        assert_eq!(ids.len(), values.len());
        let mut message = String::from("message schema {");
        for (index, id) in ids.iter().enumerate() {
            let repetition = if optional_first && index == 0 {
                "OPTIONAL"
            } else {
                "REQUIRED"
            };
            let physical = match &values[index] {
                TestColumn::Int(_) => "INT64",
                TestColumn::Bool(_) => "BOOLEAN",
                TestColumn::Str(_) => "BYTE_ARRAY",
                TestColumn::Date(_) | TestColumn::DateWithoutAnnotation(_) => "INT32",
            };
            let annotation = match &values[index] {
                TestColumn::Str(_) => " (STRING)",
                TestColumn::Date(_) => " (DATE)",
                _ => "",
            };
            message.push_str(&format!(
                " {repetition} {physical} f_{}{};",
                Uuid::from_bytes(*id).simple(),
                annotation
            ));
        }
        message.push('}');
        let schema = Arc::new(parse_message_type(&message).unwrap());
        let descriptors = descriptor_types.unwrap_or_else(|| {
            ids.iter()
                .copied()
                .zip(values)
                .map(|(id, value)| match value {
                    TestColumn::Int(_) => descriptor(id, int_type()),
                    TestColumn::Bool(_) => descriptor_with_encoding(id, bool_type(), "bool"),
                    TestColumn::Str(_) => descriptor_with_encoding(id, str_type(), "utf8"),
                    TestColumn::Date(_) | TestColumn::DateWithoutAnnotation(_) => {
                        descriptor_with_encoding(id, date_type(), "date")
                    }
                })
                .collect()
        });
        let columns = CanonicalValue::new(OvbRaw::Array(descriptors))
            .unwrap()
            .encode()
            .unwrap();
        let metadata = vec![
            KeyValue::new("orna.profile".into(), Some(COMPACT_STORAGE_PROFILE.into())),
            KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
            KeyValue::new(
                "orna.schema.sha256".into(),
                Some(hex_digest(profile.schema_fingerprint())),
            ),
            KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
        ];
        let properties = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::ZSTD(Default::default()))
                .set_dictionary_enabled(dictionary_enabled)
                .set_writer_version(WriterVersion::PARQUET_2_0)
                .set_key_value_metadata(Some(metadata))
                .build(),
        );
        let mut bytes = Vec::new();
        let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
        let mut row_group = writer.next_row_group().unwrap();
        for (index, values) in values.iter().enumerate() {
            let mut column = row_group.next_column().unwrap().unwrap();
            match values {
                TestColumn::Int(values) if optional_first && index == 0 => {
                    column
                        .typed::<Int64Type>()
                        .write_batch(&values[1..], Some(&[0, 1]), None)
                        .unwrap();
                }
                TestColumn::Bool(values) if optional_first && index == 0 => {
                    column
                        .typed::<BoolType>()
                        .write_batch(&values[1..], Some(&[0, 1]), None)
                        .unwrap();
                }
                TestColumn::Str(values) if optional_first && index == 0 => {
                    let values = values
                        .iter()
                        .skip(1)
                        .map(|value| ByteArray::from(value.as_slice()))
                        .collect::<Vec<_>>();
                    column
                        .typed::<ByteArrayType>()
                        .write_batch(&values, Some(&[0, 1]), None)
                        .unwrap();
                }
                TestColumn::Date(values) if optional_first && index == 0 => {
                    column
                        .typed::<Int32Type>()
                        .write_batch(&values[1..], Some(&[0, 1]), None)
                        .unwrap();
                }
                TestColumn::DateWithoutAnnotation(values) if optional_first && index == 0 => {
                    column
                        .typed::<Int32Type>()
                        .write_batch(&values[1..], Some(&[0, 1]), None)
                        .unwrap();
                }
                TestColumn::Int(values) => {
                    column
                        .typed::<Int64Type>()
                        .write_batch(values, None, None)
                        .unwrap();
                }
                TestColumn::Bool(values) => {
                    column
                        .typed::<BoolType>()
                        .write_batch(values, None, None)
                        .unwrap();
                }
                TestColumn::Str(values) => {
                    let values = values
                        .iter()
                        .map(|value| ByteArray::from(value.as_slice()))
                        .collect::<Vec<_>>();
                    column
                        .typed::<ByteArrayType>()
                        .write_batch(&values, None, None)
                        .unwrap();
                }
                TestColumn::Date(values) => {
                    column
                        .typed::<Int32Type>()
                        .write_batch(values, None, None)
                        .unwrap();
                }
                TestColumn::DateWithoutAnnotation(values) => {
                    column
                        .typed::<Int32Type>()
                        .write_batch(values, None, None)
                        .unwrap();
                }
            }
            column.close().unwrap();
        }
        row_group.close().unwrap();
        writer.close().unwrap();
        bytes
    }

    fn expected_scalar(value: i64) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Int(value.into()))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn expected_bool(value: bool) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Bool(value))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn expected_text(value: &str) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Text(value.to_owned()))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn expected_date(value: &str) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Tag(60001, Box::new(OvbRaw::Text(value.to_owned()))))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn verified_date_fixture(profile: &CompactOvbProfile) -> Vec<u8> {
        let days = [0_i32, 1_i32];
        let mut original =
            mixed_parquet(profile, &[KEY_A], &[TestColumn::Date(&days)], false, None);
        let footer_start = original.len()
            - 8
            - u32::from_le_bytes(
                original[original.len() - 8..original.len() - 4]
                    .try_into()
                    .unwrap(),
            ) as usize;
        assert_eq!(&original[footer_start..footer_start + 2], &[0x15, 0x04]);
        original[footer_start + 1] = 0x02;
        let reader = SerializedFileReader::new(Bytes::from(original.clone())).unwrap();
        let file = reader.metadata().file_metadata();
        let metadata = parquet::file::metadata::FileMetaData::new(
            1,
            file.num_rows(),
            file.created_by().map(str::to_owned),
            Some(vec![
                KeyValue::new("orna.profile".into(), Some(COMPACT_STORAGE_PROFILE.into())),
                KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
                KeyValue::new(
                    "orna.schema.sha256".into(),
                    Some(hex_digest(profile.schema_fingerprint())),
                ),
                KeyValue::new(
                    "orna.schema.ovb".into(),
                    Some(BASE64.encode(profile.schema().encode().unwrap())),
                ),
                KeyValue::new(
                    "orna.columns.ovb".into(),
                    Some(
                        BASE64.encode(
                            CanonicalValue::new(OvbRaw::Array(vec![descriptor_with_encoding(
                                KEY_A,
                                date_type(),
                                "date",
                            )]))
                            .unwrap()
                            .encode()
                            .unwrap(),
                        ),
                    ),
                ),
                KeyValue::new("orna.encoder".into(), Some("test-encoder-v1".into())),
            ]),
            file.schema_descr_ptr(),
            file.column_orders().cloned(),
        );
        let rewritten = parquet::file::metadata::ParquetMetaData::new(
            metadata,
            reader.metadata().row_groups().to_vec(),
        );
        let mut footer = Vec::new();
        parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer, &rewritten)
            .finish()
            .unwrap();
        let mut bytes = original[..footer_start].to_vec();
        bytes.extend(footer);
        with_page_checksums(bytes)
    }

    fn expected_tuple(first: i64, second: i64) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(first.into()),
                OvbRaw::Int(second.into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap()
    }

    fn expected_bool_int_tuple(first: bool, second: i64) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Bool(first),
                OvbRaw::Int(second.into()),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap()
    }

    fn expected_text_bool_tuple(first: &str, second: bool) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Text(first.to_owned()),
                OvbRaw::Bool(second),
            ])),
        ))
        .unwrap()
        .encode()
        .unwrap()
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().unwrap();
        git(temp.path(), &["init", "-b", "main"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "storage-test"]);
        git(temp.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
        fs::create_dir_all(temp.path().join(".orna")).unwrap();
        fs::write(temp.path().join(".orna/format.orna"), "format 1\n").unwrap();
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    fn verified_fixture(profile: &CompactOvbProfile) -> Vec<u8> {
        let schema_ovb = profile.schema().encode().unwrap();
        verified_fixture_with_schema_ovb(profile, &schema_ovb)
    }

    fn verified_fixture_with_schema_ovb(profile: &CompactOvbProfile, schema_ovb: &[u8]) -> Vec<u8> {
        let original = BASE64.decode(VERIFIED_PARQUET).unwrap();
        let reader = SerializedFileReader::new(Bytes::from(original.clone())).unwrap();
        let file = reader.metadata().file_metadata();
        let columns = CanonicalValue::new(OvbRaw::Array(vec![descriptor(KEY_A, int_type())]))
            .unwrap()
            .encode()
            .unwrap();
        let metadata = parquet::file::metadata::FileMetaData::new(
            file.version(),
            file.num_rows(),
            file.created_by().map(str::to_owned),
            Some(vec![
                KeyValue::new("orna.profile".into(), Some(COMPACT_STORAGE_PROFILE.into())),
                KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
                KeyValue::new(
                    "orna.schema.sha256".into(),
                    Some(hex_digest(profile.schema_fingerprint())),
                ),
                KeyValue::new("orna.schema.ovb".into(), Some(BASE64.encode(schema_ovb))),
                KeyValue::new("orna.columns.ovb".into(), Some(BASE64.encode(columns))),
                KeyValue::new("orna.encoder".into(), Some("test-encoder-v1".into())),
            ]),
            file.schema_descr_ptr(),
            file.column_orders().cloned(),
        );
        let rewritten = parquet::file::metadata::ParquetMetaData::new(
            metadata,
            reader.metadata().row_groups().to_vec(),
        );
        let mut footer = Vec::new();
        parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer, &rewritten)
            .finish()
            .unwrap();
        let footer_start = original.len()
            - 8
            - u32::from_le_bytes(
                original[original.len() - 8..original.len() - 4]
                    .try_into()
                    .unwrap(),
            ) as usize;
        let mut bytes = original[..footer_start].to_vec();
        bytes.extend(footer);
        bytes
    }

    fn verified_string_fixture(profile: &CompactOvbProfile) -> Vec<u8> {
        let strings = [b"alpha".to_vec(), b"beta".to_vec()];
        let mut original =
            mixed_parquet(profile, &[KEY_A], &[TestColumn::Str(&strings)], false, None);
        let footer_start = original.len()
            - 8
            - u32::from_le_bytes(
                original[original.len() - 8..original.len() - 4]
                    .try_into()
                    .unwrap(),
            ) as usize;
        assert_eq!(&original[footer_start..footer_start + 2], &[0x15, 0x04]);
        original[footer_start + 1] = 0x02;
        let reader = SerializedFileReader::new(Bytes::from(original.clone())).unwrap();
        let file = reader.metadata().file_metadata();
        let metadata = parquet::file::metadata::FileMetaData::new(
            1,
            file.num_rows(),
            file.created_by().map(str::to_owned),
            Some(vec![
                KeyValue::new("orna.profile".into(), Some(COMPACT_STORAGE_PROFILE.into())),
                KeyValue::new("orna.table".into(), Some(TABLE.to_string())),
                KeyValue::new(
                    "orna.schema.sha256".into(),
                    Some(hex_digest(profile.schema_fingerprint())),
                ),
                KeyValue::new(
                    "orna.schema.ovb".into(),
                    Some(BASE64.encode(profile.schema().encode().unwrap())),
                ),
                KeyValue::new(
                    "orna.columns.ovb".into(),
                    Some(
                        BASE64.encode(
                            CanonicalValue::new(OvbRaw::Array(vec![descriptor_with_encoding(
                                KEY_A,
                                str_type(),
                                "utf8",
                            )]))
                            .unwrap()
                            .encode()
                            .unwrap(),
                        ),
                    ),
                ),
                KeyValue::new("orna.encoder".into(), Some("test-encoder-v1".into())),
            ]),
            file.schema_descr_ptr(),
            file.column_orders().cloned(),
        );
        let rewritten = parquet::file::metadata::ParquetMetaData::new(
            metadata,
            reader.metadata().row_groups().to_vec(),
        );
        let mut footer = Vec::new();
        parquet::file::metadata::ParquetMetaDataWriter::new(&mut footer, &rewritten)
            .finish()
            .unwrap();
        let mut bytes = original[..footer_start].to_vec();
        bytes.extend(footer);
        let bytes = with_page_checksums(bytes);
        bytes
    }

    struct TestPageHeader {
        encoded_len: usize,
        compressed_len: usize,
        checksum_predecessor: u8,
        next_field_offset: Option<usize>,
        next_field_id: Option<u8>,
        stop_offset: usize,
    }

    fn page_header(bytes: &[u8]) -> TestPageHeader {
        let mut cursor = 0;
        let mut previous = 0_u8;
        let mut compressed_len = None;
        let mut next_field_offset = None;
        let mut next_field_id = None;
        let mut checksum_predecessor = None;
        loop {
            let field_offset = cursor;
            let tag = compact_byte(bytes, &mut cursor);
            let kind = tag & 0x0f;
            if kind == 0 {
                return TestPageHeader {
                    encoded_len: cursor,
                    compressed_len: compressed_len.unwrap(),
                    checksum_predecessor: checksum_predecessor.unwrap_or(previous),
                    next_field_offset,
                    next_field_id,
                    stop_offset: field_offset,
                };
            }
            let delta = tag >> 4;
            let field = if delta == 0 {
                compact_i16(bytes, &mut cursor) as u8
            } else {
                previous.checked_add(delta).unwrap()
            };
            if field > 4 && next_field_offset.is_none() {
                next_field_offset = Some(field_offset);
                next_field_id = Some(field);
                checksum_predecessor = Some(previous);
            }
            if field == 3 && kind == 5 {
                compressed_len = Some(compact_i32(bytes, &mut cursor) as usize);
            } else {
                compact_skip(bytes, &mut cursor, kind);
            }
            previous = field;
        }
    }

    fn with_page_checksums(bytes: Vec<u8>) -> Vec<u8> {
        let reader = SerializedFileReader::new(Bytes::from(bytes.clone())).unwrap();
        let metadata = reader.metadata().clone();
        let column = metadata.row_group(0).column(0);
        let start = column.data_page_offset() as usize;
        let length = column.compressed_size() as usize;
        let header = page_header(&bytes[start..start + length]);
        let body_start = start + header.encoded_len;
        let body_end = body_start + header.compressed_len;
        let checksum = crc32(&bytes[body_start..body_end]);
        let checksum_delta = 4_u8.checked_sub(header.checksum_predecessor).unwrap();
        let mut checksum_field = vec![(checksum_delta << 4) | 5];
        compact_varint(
            ((i64::from(checksum as i32) << 1) ^ (i64::from(checksum as i32) >> 31)) as u64,
            &mut checksum_field,
        );
        let footer = footer_start(&bytes);
        let mut data = bytes[..footer].to_vec();
        let insertion = start + header.next_field_offset.unwrap_or(header.stop_offset);
        data.splice(insertion..insertion, checksum_field.iter().copied());
        if let Some(next) = header.next_field_offset {
            let position = start + next + checksum_field.len();
            let delta = header.next_field_id.unwrap().checked_sub(4).unwrap();
            data[position] = (data[position] & 0x0f) | (delta << 4);
        }

        let mut metadata = metadata.into_builder();
        let mut row_groups = metadata.take_row_groups();
        let row_group = row_groups.pop().unwrap();
        let mut row_group = row_group.into_builder();
        let mut columns = row_group.take_columns();
        let column = columns.pop().unwrap();
        let compressed_size = column.compressed_size();
        columns.push(
            column
                .into_builder()
                .set_total_compressed_size(
                    compressed_size + i64::try_from(data.len() - footer).unwrap(),
                )
                .build()
                .unwrap(),
        );
        let row_group = row_group.set_column_metadata(columns).build().unwrap();
        let metadata = metadata.add_row_group(row_group).build();
        let mut new_footer = Vec::new();
        parquet::file::metadata::ParquetMetaDataWriter::new(&mut new_footer, &metadata)
            .finish()
            .unwrap();
        data.extend(new_footer);
        data
    }

    fn footer_start(bytes: &[u8]) -> usize {
        let length =
            u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap());
        bytes.len() - 8 - length as usize
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut value = !0_u32;
        for byte in bytes {
            value ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(value & 1);
                value = (value >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !value
    }

    fn compact_byte(bytes: &[u8], cursor: &mut usize) -> u8 {
        let value = bytes[*cursor];
        *cursor += 1;
        value
    }

    fn compact_varint(mut value: u64, output: &mut Vec<u8>) {
        while value >= 0x80 {
            output.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        output.push(value as u8);
    }

    fn compact_read_varint(bytes: &[u8], cursor: &mut usize) -> u64 {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = compact_byte(bytes, cursor);
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
        }
        panic!("invalid compact fixture varint");
    }

    fn compact_i16(bytes: &[u8], cursor: &mut usize) -> i16 {
        let value = compact_read_varint(bytes, cursor) as i64;
        ((value >> 1) ^ -(value & 1)).try_into().unwrap()
    }

    fn compact_i32(bytes: &[u8], cursor: &mut usize) -> i32 {
        let value = compact_read_varint(bytes, cursor) as i64;
        ((value >> 1) ^ -(value & 1)).try_into().unwrap()
    }

    fn compact_skip(bytes: &[u8], cursor: &mut usize, kind: u8) {
        match kind {
            1 | 2 => {}
            3 => *cursor += 1,
            4..=6 => {
                let _ = compact_read_varint(bytes, cursor);
            }
            7 => *cursor += 8,
            8 => {
                let length = compact_read_varint(bytes, cursor) as usize;
                *cursor += length;
            }
            9 | 10 => {
                let size_and_kind = compact_byte(bytes, cursor);
                let size = if size_and_kind >> 4 == 15 {
                    compact_read_varint(bytes, cursor) as usize
                } else {
                    usize::from(size_and_kind >> 4)
                };
                for _ in 0..size {
                    compact_skip(bytes, cursor, size_and_kind & 0x0f);
                }
            }
            12 => loop {
                let tag = compact_byte(bytes, cursor);
                if tag & 0x0f == 0 {
                    break;
                }
                if tag >> 4 == 0 {
                    let _ = compact_i16(bytes, cursor);
                }
                compact_skip(bytes, cursor, tag & 0x0f);
            },
            _ => panic!("unsupported compact fixture field"),
        }
    }

    #[test]
    fn repository_backed_source_reads_verified_manifest_and_rejects_drift() {
        let profile = profile(&[KEY_A]);
        let bytes = verified_fixture(&profile);
        let (temp, repository) = repository();
        let segment_path = ManagedPath::new(format!(
            ".orna/storage/{TABLE}/data/{}/{SEGMENT_ID}.parquet",
            &SEGMENT_ID.to_string()[..2]
        ))
        .unwrap();
        let key = expected_scalar(1);
        let segment = CompactSegment::new(
            SEGMENT_ID,
            CompactSegmentRole::Data,
            profile.schema_fingerprint(),
            "test-encoder-v1",
            segment_path.clone(),
            bytes,
            key.clone(),
            key,
            1,
            CanonicalValue::new(OvbRaw::Array(vec![descriptor(KEY_A, int_type())]))
                .unwrap()
                .encode()
                .unwrap(),
            true,
            false,
        )
        .unwrap();
        let head = repository.head().unwrap().unwrap();
        let plan = repository
            .prepare_compact_publication(
                &head,
                repository.index_generation().unwrap(),
                CompactManifest::empty(TABLE, profile.schema_fingerprint()),
                [9; 16],
                [8; 32],
                &[segment],
                "compact physical reader fixture",
            )
            .unwrap();
        let manifest = plan.manifest().clone();
        let pending = repository
            .publish_compact_repository_boundary(plan)
            .unwrap();
        let committed = pending.commit().clone();
        let source = CompactParquetKeySource::from_verified_manifest(
            &repository,
            &committed,
            &manifest,
            profile.clone(),
        )
        .unwrap();
        let entry = &manifest.entries()[0];
        let keys = source
            .exact_keys(entry)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(keys, vec![expected_scalar(1)]);

        let missing_path = repository
            .build_private_commit(
                &committed,
                &[ManagedFileChange::new(segment_path.clone(), None)],
                "path drift",
            )
            .unwrap();
        assert!(matches!(
            repository.read_verified_compact_segment(&missing_path.commit().clone(), TABLE, entry),
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
        ));

        let changed = b"different committed bytes".to_vec();
        assert_ne!(Sha256::digest(&changed).as_slice(), entry.sha256());
        let changed_object = repository
            .build_private_commit(
                &committed,
                &[ManagedFileChange::new(segment_path, Some(changed))],
                "object and digest drift",
            )
            .unwrap();
        assert!(matches!(
            repository.read_verified_compact_segment(
                &changed_object.commit().clone(),
                TABLE,
                entry
            ),
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
        ));
        drop(temp);
    }

    #[test]
    fn repository_rejects_unrelated_valid_schema_descriptor_before_logical_use() {
        let expected_profile = profile(&[KEY_A]);
        let unrelated = profile(&[KEY_B]);
        assert_ne!(
            expected_profile.schema_fingerprint(),
            unrelated.schema_fingerprint()
        );
        let valid = verified_fixture(&expected_profile);
        let unrelated_ovb = unrelated.schema().encode().unwrap();
        let mismatched = verified_fixture_with_schema_ovb(&expected_profile, &unrelated_ovb);
        let path = ManagedPath::new(format!(
            ".orna/storage/{TABLE}/data/{}/{SEGMENT_ID}.parquet",
            &SEGMENT_ID.to_string()[..2]
        ))
        .unwrap();
        let key = expected_scalar(1);
        let columns = CanonicalValue::new(OvbRaw::Array(vec![descriptor(KEY_A, int_type())]))
            .unwrap()
            .encode()
            .unwrap();

        let valid = CompactSegment::new(
            SEGMENT_ID,
            CompactSegmentRole::Data,
            expected_profile.schema_fingerprint(),
            "test-encoder-v1",
            path.clone(),
            valid,
            key.clone(),
            key.clone(),
            1,
            columns.clone(),
            true,
            false,
        )
        .unwrap();
        let mismatched = CompactSegment::new(
            SEGMENT_ID,
            CompactSegmentRole::Data,
            expected_profile.schema_fingerprint(),
            "test-encoder-v1",
            path,
            mismatched,
            key.clone(),
            key,
            1,
            columns,
            true,
            false,
        )
        .unwrap();
        let (temp, repository) = repository();
        let head = repository.head().unwrap().unwrap();
        let generation = repository.index_generation().unwrap();
        assert!(repository
            .prepare_compact_publication(
                &head,
                generation.clone(),
                CompactManifest::empty(TABLE, expected_profile.schema_fingerprint()),
                [9; 16],
                [8; 32],
                &[valid],
                "valid schema descriptor fixture",
            )
            .is_ok());
        assert!(matches!(
            repository.prepare_compact_publication(
                &head,
                generation,
                CompactManifest::empty(TABLE, expected_profile.schema_fingerprint()),
                [9; 16],
                [8; 32],
                &[mismatched],
                "mismatched schema descriptor fixture",
            ),
            Err(orna_repository_v1::RepositoryError::InvalidCompactManifest)
        ));
        drop(temp);
    }

    #[test]
    fn repository_backed_string_source_reads_verified_manifest_bytes() {
        let profile = profile_with_types(&[KEY_A], &[str_type()]);
        let bytes = verified_string_fixture(&profile);
        let (temp, repository) = repository();
        let segment_path = ManagedPath::new(format!(
            ".orna/storage/{TABLE}/data/{}/{SEGMENT_ID}.parquet",
            &SEGMENT_ID.to_string()[..2]
        ))
        .unwrap();
        // Bounds use canonical OVB byte order: the length prefix puts beta
        // before alpha even though their textual order is the reverse.
        let min_key = expected_text("beta");
        let max_key = expected_text("alpha");
        let columns = CanonicalValue::new(OvbRaw::Array(vec![descriptor_with_encoding(
            KEY_A,
            str_type(),
            "utf8",
        )]))
        .unwrap()
        .encode()
        .unwrap();
        let segment = CompactSegment::new(
            SEGMENT_ID,
            CompactSegmentRole::Data,
            profile.schema_fingerprint(),
            "test-encoder-v1",
            segment_path,
            bytes,
            min_key,
            max_key,
            2,
            columns,
            true,
            false,
        )
        .unwrap();
        let head = repository.head().unwrap().unwrap();
        let plan = repository
            .prepare_compact_publication(
                &head,
                repository.index_generation().unwrap(),
                CompactManifest::empty(TABLE, profile.schema_fingerprint()),
                [7; 16],
                [8; 32],
                &[segment],
                "compact string physical reader fixture",
            )
            .unwrap();
        let manifest = plan.manifest().clone();
        let pending = repository
            .publish_compact_repository_boundary(plan)
            .unwrap();
        let source = CompactParquetKeySource::from_verified_manifest(
            &repository,
            pending.commit(),
            &manifest,
            profile,
        )
        .unwrap();
        let keys = source
            .exact_keys(&manifest.entries()[0])
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(keys, vec![expected_text("alpha"), expected_text("beta")]);
        drop(temp);
    }

    #[test]
    fn reads_real_scalar_date_as_canonical_ovb_date() {
        let profile = profile_with_types(&[KEY_A], &[date_type()]);
        let days = [0_i32, 1_i32];
        let bytes = mixed_parquet(&profile, &[KEY_A], &[TestColumn::Date(&days)], false, None);
        let keys =
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap();
        assert_eq!(
            keys,
            vec![expected_date("1970-01-01"), expected_date("1970-01-02")]
        );

        assert_eq!(
            date_value(-719_162).unwrap(),
            OvbRaw::Tag(60001, Box::new(OvbRaw::Text("0001-01-01".into())))
        );
        assert!(matches!(
            date_value(-719_163),
            Err(CompactParquetError::InvalidMetadata)
        ));
        assert_eq!(
            date_value(2_932_896).unwrap(),
            OvbRaw::Tag(60001, Box::new(OvbRaw::Text("9999-12-31".into())))
        );
        assert!(matches!(
            date_value(2_932_897),
            Err(CompactParquetError::InvalidMetadata)
        ));
    }

    #[test]
    fn reads_date_from_plain_and_rle_dictionary_pages_and_requires_date_annotation() {
        let profile = profile_with_types(&[KEY_A], &[date_type()]);
        let dates = [0_i32, 1_i32, 1_i32];
        let dictionary = mixed_parquet_with_dictionary(
            &profile,
            &[KEY_A],
            &[TestColumn::Date(&dates)],
            false,
            None,
            true,
        );
        let reader = SerializedFileReader::new(Bytes::copy_from_slice(&dictionary)).unwrap();
        assert!(
            reader
                .metadata()
                .row_group(0)
                .column(0)
                .encodings()
                .any(|encoding| encoding == parquet::basic::Encoding::RLE_DICTIONARY)
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &dictionary, 3)
                .unwrap(),
            vec![
                expected_date("1970-01-01"),
                expected_date("1970-01-02"),
                expected_date("1970-01-02"),
            ]
        );

        let unannotated = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::DateWithoutAnnotation(&dates)],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &unannotated, 3),
            Err(CompactParquetError::UnsupportedKeyMapping)
        ));
    }

    #[test]
    fn reads_date_in_declared_composite_order_and_rejects_nonrepresentable_dates() {
        let profile = profile_with_types(&[KEY_A, KEY_B], &[date_type(), int_type()]);
        let dates = [0_i32, 1_i32];
        let numbers = [9_i64, 8_i64];
        let bytes = mixed_parquet(
            &profile,
            &[KEY_B, KEY_A],
            &[TestColumn::Int(&numbers), TestColumn::Date(&dates)],
            false,
            None,
        );
        let keys =
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap();
        assert_eq!(
            keys,
            vec![
                CanonicalValue::new(OvbRaw::Tag(
                    60015,
                    Box::new(OvbRaw::Array(vec![
                        OvbRaw::Tag(60001, Box::new(OvbRaw::Text("1970-01-01".into()))),
                        OvbRaw::Int(9.into()),
                    ])),
                ))
                .unwrap()
                .encode()
                .unwrap(),
                CanonicalValue::new(OvbRaw::Tag(
                    60015,
                    Box::new(OvbRaw::Array(vec![
                        OvbRaw::Tag(60001, Box::new(OvbRaw::Text("1970-01-02".into()))),
                        OvbRaw::Int(8.into()),
                    ])),
                ))
                .unwrap()
                .encode()
                .unwrap(),
            ]
        );
        let invalid = [-2_147_483_648_i32];
        let invalid_bytes = mixed_parquet(
            &profile_with_types(&[KEY_A], &[date_type()]),
            &[KEY_A],
            &[TestColumn::Date(&invalid)],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &profile_with_types(&[KEY_A], &[date_type()]),
                TABLE,
                &invalid_bytes,
                1,
            ),
            Err(CompactParquetError::InvalidMetadata)
        ));

        let wrong_descriptor = mixed_parquet(
            &profile_with_types(&[KEY_A], &[date_type()]),
            &[KEY_A],
            &[TestColumn::Date(&dates)],
            false,
            Some(vec![descriptor_with_encoding(KEY_A, date_type(), "int64")]),
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &profile_with_types(&[KEY_A], &[date_type()]),
                TABLE,
                &wrong_descriptor,
                2,
            ),
            Err(CompactParquetError::UnsupportedKeyMapping)
        ));

        let nullable = mixed_parquet(
            &profile_with_types(&[KEY_A], &[date_type()]),
            &[KEY_A],
            &[TestColumn::Date(&dates)],
            true,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &profile_with_types(&[KEY_A], &[date_type()]),
                TABLE,
                &nullable,
                2,
            ),
            Err(CompactParquetError::NullKey)
        ));

        let valid = mixed_parquet(
            &profile_with_types(&[KEY_A], &[date_type()]),
            &[KEY_A],
            &[TestColumn::Date(&dates)],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &profile_with_types(&[KEY_A], &[date_type()]),
                TABLE,
                &valid,
                3,
            ),
            Err(CompactParquetError::RowCountMismatch { .. })
        ));
    }

    #[test]
    fn repository_publication_verifies_date_bytes_before_logical_read() {
        let profile = profile_with_types(&[KEY_A], &[date_type()]);
        let bytes = verified_date_fixture(&profile);
        let (temp, repository) = repository();
        let path = ManagedPath::new(format!(
            ".orna/storage/{TABLE}/data/{}/{SEGMENT_ID}.parquet",
            &SEGMENT_ID.to_string()[..2]
        ))
        .unwrap();
        let columns = CanonicalValue::new(OvbRaw::Array(vec![descriptor_with_encoding(
            KEY_A,
            date_type(),
            "date",
        )]))
        .unwrap()
        .encode()
        .unwrap();
        let segment = CompactSegment::new(
            SEGMENT_ID,
            CompactSegmentRole::Data,
            profile.schema_fingerprint(),
            "test-encoder-v1",
            path,
            bytes,
            expected_date("1970-01-01"),
            expected_date("1970-01-02"),
            2,
            columns,
            true,
            false,
        )
        .unwrap();
        let head = repository.head().unwrap().unwrap();
        let plan = repository
            .prepare_compact_publication(
                &head,
                repository.index_generation().unwrap(),
                CompactManifest::empty(TABLE, profile.schema_fingerprint()),
                [4; 16],
                [5; 32],
                &[segment],
                "verified date compact publication",
            )
            .unwrap();
        let manifest = plan.manifest().clone();
        let pending = repository
            .publish_compact_repository_boundary(plan)
            .unwrap();
        let source = CompactParquetKeySource::from_verified_manifest(
            &repository,
            pending.commit(),
            &manifest,
            profile,
        )
        .unwrap();
        let keys = source
            .exact_keys(&manifest.entries()[0])
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            keys,
            vec![expected_date("1970-01-01"), expected_date("1970-01-02")]
        );
        drop(temp);
    }

    #[test]
    fn reads_real_scalar_int64_as_canonical_ovb_int() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(&profile, &[KEY_A], &[vec![7, 42]], false, None);
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap(),
            vec![expected_scalar(7), expected_scalar(42)]
        );
    }

    #[test]
    fn rejects_decreasing_scalar_primary_keys_without_reordering() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(&profile, &[KEY_A], &[vec![42, 7]], false, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
            Err(CompactParquetError::UnorderedPrimaryKeys)
        ));
    }

    #[test]
    fn rejects_decreasing_bool_and_string_primary_keys() {
        let bool_profile = profile_with_types(&[KEY_A], &[bool_type()]);
        let bool_bytes = mixed_parquet(
            &bool_profile,
            &[KEY_A],
            &[TestColumn::Bool(&[true, false])],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&bool_profile, TABLE, &bool_bytes, 2),
            Err(CompactParquetError::UnorderedPrimaryKeys)
        ));

        let string_profile = profile_with_types(&[KEY_A], &[str_type()]);
        let string_values = [b"beta".to_vec(), b"alpha".to_vec()];
        let string_bytes = mixed_parquet(
            &string_profile,
            &[KEY_A],
            &[TestColumn::Str(&string_values)],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &string_profile,
                TABLE,
                &string_bytes,
                2
            ),
            Err(CompactParquetError::UnorderedPrimaryKeys)
        ));
    }

    #[test]
    fn rejects_decreasing_composite_keys_in_declared_primary_key_order() {
        // The physical leaf order is A then B, but the declared primary key is
        // B then A.  B decreases even as A increases, so this must fail.
        let profile = profile_with_types(&[KEY_B, KEY_A], &[int_type(), int_type()]);
        let bytes = mixed_parquet(
            &profile,
            &[KEY_A, KEY_B],
            &[TestColumn::Int(&[1, 2]), TestColumn::Int(&[42, 7])],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
            Err(CompactParquetError::UnorderedPrimaryKeys)
        ));
    }

    #[test]
    fn preserves_equal_keys_for_logical_duplicate_detection() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(&profile, &[KEY_A], &[vec![7, 7]], false, None);
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap(),
            vec![expected_scalar(7), expected_scalar(7)]
        );
    }

    #[test]
    fn reads_real_scalar_bool_as_canonical_ovb_bool() {
        let profile = profile_with_types(&[KEY_A], &[bool_type()]);
        let bytes = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Bool(&[false, true])],
            false,
            None,
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2).unwrap(),
            vec![expected_bool(false), expected_bool(true)]
        );
    }

    #[test]
    fn reads_bool_int_tuple_in_declared_primary_key_order() {
        // Physical columns are Int B then Bool A. The PK declaration is Bool
        // A then Int B, so the canonical tuple must follow the declaration.
        let profile = profile_with_types(&[KEY_A, KEY_B], &[bool_type(), int_type()]);
        let bytes = mixed_parquet(
            &profile,
            &[KEY_B, KEY_A],
            &[TestColumn::Int(&[10]), TestColumn::Bool(&[true])],
            false,
            None,
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
            vec![expected_bool_int_tuple(true, 10)]
        );
    }

    #[test]
    fn reads_utf8_scalar_from_plain_and_rle_dictionary_pages() {
        let profile = profile_with_types(&[KEY_A], &[str_type()]);
        let strings = [b"alpha".to_vec(), b"beta".to_vec()];
        let plain = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Str(&strings)],
            false,
            None,
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &plain, 2).unwrap(),
            vec![expected_text("alpha"), expected_text("beta")]
        );

        let dictionary_strings = [b"other".to_vec(), b"same".to_vec(), b"same".to_vec()];
        let dictionary = mixed_parquet_with_dictionary(
            &profile,
            &[KEY_A],
            &[TestColumn::Str(&dictionary_strings)],
            false,
            None,
            true,
        );
        let reader = SerializedFileReader::new(Bytes::copy_from_slice(&dictionary)).unwrap();
        assert!(reader
            .metadata()
            .row_group(0)
            .column(0)
            .encodings()
            .any(|encoding| { encoding == parquet::basic::Encoding::RLE_DICTIONARY }));
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &dictionary, 3)
                .unwrap(),
            vec![
                expected_text("other"),
                expected_text("same"),
                expected_text("same")
            ]
        );
    }

    #[test]
    fn reads_str_bool_tuple_in_declared_primary_key_order() {
        let profile = profile_with_types(&[KEY_A, KEY_B], &[str_type(), bool_type()]);
        let strings = [b"key".to_vec()];
        let bytes = mixed_parquet(
            &profile,
            &[KEY_B, KEY_A],
            &[TestColumn::Bool(&[true]), TestColumn::Str(&strings)],
            false,
            None,
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
            vec![expected_text_bool_tuple("key", true)]
        );
    }

    #[test]
    fn rejects_invalid_utf8_mapping_null_and_row_counts() {
        let profile = profile_with_types(&[KEY_A], &[str_type()]);
        let invalid = [vec![0xff, 0xfe]];
        let invalid_bytes = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Str(&invalid)],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &invalid_bytes, 1),
            Err(CompactParquetError::InvalidUtf8)
        ));

        let wrong_mapping = [b"key".to_vec()];
        let wrong_descriptor = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Str(&wrong_mapping)],
            false,
            Some(vec![descriptor_with_encoding(KEY_A, str_type(), "blob")]),
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &wrong_descriptor, 1),
            Err(CompactParquetError::UnsupportedKeyMapping)
        ));

        let nullable = [b"key".to_vec(), b"other".to_vec()];
        let nullable_bytes = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Str(&nullable)],
            true,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &nullable_bytes, 2),
            Err(CompactParquetError::NullKey)
        ));

        let valid = [b"key".to_vec(), b"other".to_vec()];
        let valid_bytes =
            mixed_parquet(&profile, &[KEY_A], &[TestColumn::Str(&valid)], false, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &valid_bytes, 1),
            Err(CompactParquetError::RowCountMismatch {
                expected: 1,
                observed: 2
            })
        ));
        let one = [b"key".to_vec()];
        let one_bytes = mixed_parquet(&profile, &[KEY_A], &[TestColumn::Str(&one)], false, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &one_bytes, 2),
            Err(CompactParquetError::RowCountMismatch {
                expected: 2,
                observed: 1
            })
        ));
    }

    #[test]
    fn rejects_bool_mapping_null_and_profile_table_mismatch() {
        let profile = profile_with_types(&[KEY_A], &[bool_type()]);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(
                &profile,
                Uuid::from_u128(2),
                b"not parquet",
                1
            ),
            Err(CompactParquetError::Key(CompactKeyError::WrongTable))
        ));

        let wrong_descriptor = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Bool(&[true])],
            false,
            Some(vec![descriptor(KEY_A, bool_type())]),
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &wrong_descriptor, 1),
            Err(CompactParquetError::UnsupportedKeyMapping)
        ));

        let nullable = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Bool(&[true, false])],
            true,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &nullable, 2),
            Err(CompactParquetError::NullKey)
        ));

        let valid = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Bool(&[true, false])],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &valid, 1),
            Err(CompactParquetError::RowCountMismatch {
                expected: 1,
                observed: 2
            })
        ));
        let one_row = mixed_parquet(
            &profile,
            &[KEY_A],
            &[TestColumn::Bool(&[true])],
            false,
            None,
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &one_row, 2),
            Err(CompactParquetError::RowCountMismatch {
                expected: 2,
                observed: 1
            })
        ));
    }

    #[test]
    fn reads_real_tuple_in_declared_primary_key_order() {
        // Physical descriptors/columns are A then B, while the PK declaration
        // is B then A. The emitted tuple must follow the latter.
        let profile = profile(&[KEY_B, KEY_A]);
        let bytes = parquet(
            &profile,
            &[KEY_A, KEY_B],
            &[vec![10], vec![20]],
            false,
            None,
        );
        assert_eq!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1).unwrap(),
            vec![expected_tuple(20, 10)]
        );
    }

    #[test]
    fn rejects_wrong_key_descriptor_before_interpretation() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(
            &profile,
            &[KEY_A],
            &[vec![7]],
            false,
            Some(vec![descriptor(
                KEY_A,
                OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Str".into())]),
            )]),
        );
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1),
            Err(CompactParquetError::UnsupportedKeyMapping)
        ));
    }

    #[test]
    fn rejects_null_key_without_synthesizing_a_value() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(&profile, &[KEY_A], &[vec![7, 42]], true, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
            Err(CompactParquetError::NullKey)
        ));
    }

    #[test]
    fn rejects_truncated_and_overlong_manifest_row_counts() {
        let profile = profile(&[KEY_A]);
        let bytes = parquet(&profile, &[KEY_A], &[vec![7, 42]], false, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 1),
            Err(CompactParquetError::RowCountMismatch {
                expected: 1,
                observed: 2
            })
        ));
        let bytes = parquet(&profile, &[KEY_A], &[vec![7]], false, None);
        assert!(matches!(
            CompactParquetKeySource::decode_verified_bytes(&profile, TABLE, &bytes, 2),
            Err(CompactParquetError::RowCountMismatch {
                expected: 2,
                observed: 1
            })
        ));
    }
}
