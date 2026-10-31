//! Committed-table row codec shared by every consumer of table storage.
//!
//! A row exists in three forms. A *runtime* key is the byte string the native
//! store orders by: a text primary key is stored as its UTF-8 bytes, and every
//! other key as its canonical OVB encoding. A *stored tuple* is a canonical
//! array of the table's non-key field values in descriptor storage order. An
//! *evaluator* record is a canonical map from field name to value, key fields
//! included. The functions here perform these conversions for committed-table reads and
//! evaluator-form mutations; `orna-storage-v1` keeps its own descriptor-level projection.
//!
//! A stored tuple keeps each ROV-3 Blob reference exactly as committed. That
//! reference has no portable evaluator value, so an evaluator record carries
//! [`blob_placeholder`] in its place, and [`restore_stored_blobs`] writes the
//! committed reference back when the row is stored again.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use num_bigint::BigInt;
use orna_repository_v1::{
    GitCommitRef, KeyRange, PrivateCommit, Repository, RepositoryFormatContext,
};
use orna_runtime_v1::TableObjectId;
use orna_semantic_v1::{ModuleHeader, SymbolKind, TableSchema, Type};
use orna_value_v1::{
    decode_canonical_storage_value, encode_canonical_storage_value, Raw, Value, ROV3_BLOB_TAG,
};

use crate::{ProjectLoadError, ProjectLoader, ResolvedTableSchema};

/// Committed table storage violates the descriptor or row contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableStorageError;

impl fmt::Display for TableStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("committed table storage is invalid")
    }
}

impl std::error::Error for TableStorageError {}

/// One committed table's rows in the runtime keyspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTable {
    /// Stored-field names in the order every stored tuple lists its values.
    pub storage_order: Vec<String>,
    /// `(runtime key, stored tuple)` pairs in ascending runtime-key order. A
    /// stored tuple is the committed one, Blob references included.
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
}

impl CommittedTable {
    /// The committed stored tuple of the row at `runtime_key`.
    pub fn stored_row(&self, runtime_key: &[u8]) -> Option<&[u8]> {
        self.rows
            .binary_search_by(|(key, _)| key.as_slice().cmp(runtime_key))
            .ok()
            .map(|index| self.rows[index].1.as_slice())
    }
}

/// Bounds one committed read: the rows one table may hold, and the key and
/// stored-tuple bytes of every row the read admits across all tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedReadLimits {
    /// Most rows one table may hold.
    pub max_rows_per_table: usize,
    /// Most key and stored-tuple bytes across every table of the read.
    pub max_bytes: usize,
}

/// Why a committed read stopped before returning every table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommittedReadError {
    /// The committed storage violates the descriptor or row contract.
    Storage(TableStorageError),
    /// The read reached a row past its limits. `rows_seen` counts the rows of
    /// the table that reached the limit, including the refused row. That row is
    /// never retained, and no later row of any table is read.
    Limit { rows_seen: usize },
}

impl From<TableStorageError> for CommittedReadError {
    fn from(error: TableStorageError) -> Self {
        Self::Storage(error)
    }
}

/// Admits committed rows one at a time against [`CommittedReadLimits`].
#[derive(Debug)]
struct CommittedReadBudget {
    limits: CommittedReadLimits,
    bytes: usize,
}

impl CommittedReadBudget {
    fn new(limits: CommittedReadLimits) -> Self {
        Self { limits, bytes: 0 }
    }

    /// Checks that the next row of a table already holding `table_rows` admitted rows
    /// is within the per-table row limit. Runs before that row's tuple is decoded.
    fn check_row_slot(&self, table_rows: usize) -> Result<(), CommittedReadError> {
        let rows_seen = table_rows.saturating_add(1);
        if rows_seen > self.limits.max_rows_per_table {
            return Err(CommittedReadError::Limit { rows_seen });
        }
        Ok(())
    }

    /// Admits the key and stored-tuple bytes of a row whose slot was checked. The
    /// row is of a table already holding `table_rows` admitted rows.
    fn admit_row_bytes(
        &mut self,
        table_rows: usize,
        row_bytes: usize,
    ) -> Result<(), CommittedReadError> {
        let rows_seen = table_rows.saturating_add(1);
        let bytes = self.bytes.saturating_add(row_bytes);
        if bytes > self.limits.max_bytes {
            return Err(CommittedReadError::Limit { rows_seen });
        }
        self.bytes = bytes;
        Ok(())
    }
}

/// Reads every table's committed rows at `commit`, keyed in the runtime keyspace.
/// The read stops at the first row past `limits`, before that row is retained.
pub fn committed_tables(
    repository: &Repository,
    format_context: &RepositoryFormatContext,
    commit: &GitCommitRef,
    tables: &BTreeMap<String, TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
    limits: CommittedReadLimits,
) -> Result<BTreeMap<String, CommittedTable>, CommittedReadError> {
    read_tables(format_context, tables, table_ids, limits, |object_id| {
        ProjectLoader::default()
            .resolve_committed_source_table_schema(repository, commit, object_id)
    })
}

/// Reads every table's rows of one private publication candidate, keyed in the
/// runtime keyspace, before any ref names it. `format_context` must be the
/// candidate's own format context.
pub fn private_candidate_tables(
    repository: &Repository,
    candidate: &PrivateCommit,
    format_context: &RepositoryFormatContext,
    tables: &BTreeMap<String, TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
    limits: CommittedReadLimits,
) -> Result<BTreeMap<String, CommittedTable>, CommittedReadError> {
    read_tables(format_context, tables, table_ids, limits, |object_id| {
        ProjectLoader::default().resolve_private_candidate_source_table_schema(
            repository,
            candidate,
            object_id,
        )
    })
}

/// Shared reader behind [`committed_tables`] and [`private_candidate_tables`].
/// `resolve` supplies each table's schema for the snapshot the caller names.
fn read_tables(
    format_context: &RepositoryFormatContext,
    tables: &BTreeMap<String, TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
    limits: CommittedReadLimits,
    resolve: impl Fn(TableObjectId) -> Result<ResolvedTableSchema, ProjectLoadError>,
) -> Result<BTreeMap<String, CommittedTable>, CommittedReadError> {
    let mut result = BTreeMap::new();
    let mut budget = CommittedReadBudget::new(limits);
    for (table_name, table_schema) in tables {
        let object_id = table_ids
            .get(table_name)
            .copied()
            .ok_or(TableStorageError)?;
        let resolved_schema = resolve(object_id).map_err(|_| TableStorageError)?;
        let binding = resolved_schema.binding();
        let row_map = format_context
            .load_row_map(object_id.bytes())
            .map_err(|_| TableStorageError)?;
        let graph = format_context
            .open_native_graph(&row_map)
            .map_err(|_| TableStorageError)?;
        let read_scope = graph.open_read_scope().map_err(|_| TableStorageError)?;
        let (field_names, storage_order) = stored_field_names(binding.descriptor().raw())?;
        let admission = table_schema.admission.as_ref().ok_or(TableStorageError)?;
        let text_primary_key = has_text_primary_key(table_schema);
        let mut committed = BTreeMap::<Vec<u8>, Vec<u8>>::new();
        let mut lower = None;
        loop {
            let range = KeyRange::new(lower.clone(), None, 64).map_err(|_| TableStorageError)?;
            let page = graph
                .range_rows(&range, &read_scope)
                .map_err(|_| TableStorageError)?;
            if page.is_empty() {
                break;
            }
            let next_lower = page.last().map(|row| row.key().clone());
            for row in page.into_iter().skip(usize::from(lower.is_some())) {
                let canonical_key = row.key().canonical_bytes().map_err(|_| TableStorageError)?;
                let key_value = Value::decode(&canonical_key).map_err(|_| TableStorageError)?;
                let key_bytes = key_value.encode().map_err(|_| TableStorageError)?;
                // Reject a key whose shape does not match the primary key declaration.
                key_values(admission.keys.len(), key_value.raw())?;
                let runtime_key = runtime_key_from_evaluator(&key_bytes, text_primary_key)?;
                // The row slot is checked before the stored tuple is decoded, so the row
                // past the per-table row limit is never decoded or retained.
                budget.check_row_slot(committed.len())?;
                // The committed descriptor validates the whole stored tuple,
                // Blob references included, so the tuple is kept as stored.
                let decoded = graph
                    .decode_admitted_row_fields(&row, binding, &read_scope)
                    .map_err(|_| TableStorageError)?;
                if decoded.fields().len() != storage_order.len()
                    || decoded
                        .fields()
                        .iter()
                        .zip(&storage_order)
                        .any(|(field, name)| field_names.get(field.field_id()) != Some(name))
                {
                    return Err(TableStorageError.into());
                }
                let value = decoded.canonical_fields();
                // The byte bound needs the stored tuple's size, so it is admitted once
                // decoded and before the row is retained.
                budget.admit_row_bytes(committed.len(), runtime_key.len() + value.len())?;
                if committed.insert(runtime_key, value.to_vec()).is_some() {
                    return Err(TableStorageError.into());
                }
            }
            if next_lower == lower {
                break;
            }
            lower = next_lower;
        }
        result.insert(
            table_name.clone(),
            CommittedTable {
                storage_order,
                rows: committed.into_iter().collect(),
            },
        );
    }
    Ok(result)
}

/// Converts one table's runtime rows into evaluator records, ordered by evaluator key.
/// Each present Blob field reads as its [`blob_placeholder`].
pub fn evaluator_rows(
    table: &CommittedTable,
    schema: &TableSchema,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, TableStorageError> {
    evaluator_rows_from_stored(schema, &table.storage_order, &table.rows)
}

/// Converts runtime `(key, stored tuple)` rows of one table into evaluator
/// records, ordered by evaluator key. `storage_order` names the stored fields
/// every tuple lists in order. Each present Blob field reads as its
/// [`blob_placeholder`]. This is the single stored-to-evaluator conversion: a
/// merged activation snapshot (committed base plus live tail) is converted
/// through it once, at the application boundary.
pub fn evaluator_rows_from_stored(
    schema: &TableSchema,
    storage_order: &[String],
    rows: &[(Vec<u8>, Vec<u8>)],
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, TableStorageError> {
    let (key_names, text_primary_key) = evaluator_keying(schema)?;
    let mut evaluator = BTreeMap::new();
    for (runtime_key, storage_row) in rows {
        let storage_row = portable_storage_row(storage_row, storage_order, schema)?;
        let (key_bytes, record) = evaluator_row_with_keying(
            &key_names,
            text_primary_key,
            runtime_key,
            &storage_row,
            storage_order,
        )?;
        if evaluator.insert(key_bytes, record).is_some() {
            return Err(TableStorageError);
        }
    }
    Ok(evaluator.into_iter().collect())
}

/// The key-field names and text-key flag that evaluator records of `schema` use.
fn evaluator_keying(schema: &TableSchema) -> Result<(Vec<String>, bool), TableStorageError> {
    let admission = schema.admission.as_ref().ok_or(TableStorageError)?;
    let key_names = admission
        .keys
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    Ok((key_names, has_text_primary_key(schema)))
}

/// Builds one runtime row's evaluator key and evaluator record from its key names.
fn evaluator_row_with_keying(
    key_names: &[String],
    text_primary_key: bool,
    runtime_key: &[u8],
    storage_row: &[u8],
    storage_order: &[String],
) -> Result<(Vec<u8>, Vec<u8>), TableStorageError> {
    let key_bytes = evaluator_key_from_runtime(runtime_key, text_primary_key)?;
    let record = evaluator_record(key_names, &key_bytes, storage_order, storage_row)?;
    Ok((key_bytes, record))
}

/// The OVB tag of an option value: `[0]` when absent, `[1, value]` when present.
const OPTION_TAG: u64 = 60_013;

/// Whether a field of this type stores a Blob reference when present: a
/// `Blob` field always does, and a `Blob?` field unless it is absent.
pub fn holds_blob(ty: &Type) -> bool {
    let is_blob = |ty: &Type| matches!(ty, Type::Named(name) if name == "Blob");
    match ty {
        Type::Optional(inner) => is_blob(inner.as_ref()),
        _ => is_blob(ty),
    }
}

/// The portable evaluator value that stands in for a present Blob of a field
/// typed `ty`: an empty byte string, wrapped as the present option for `Blob?`.
pub fn blob_placeholder(ty: &Type) -> Raw {
    let placeholder = Raw::Bytes(Vec::new());
    if matches!(ty, Type::Optional(_)) {
        Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(BigInt::from(1)), placeholder])),
        )
    } else {
        placeholder
    }
}

/// The present value a Blob-holding field stores, or `None` for an absent
/// `Blob?`.
fn present_blob<'a>(ty: &Type, value: &'a Raw) -> Result<Option<&'a Raw>, TableStorageError> {
    if !matches!(ty, Type::Optional(_)) {
        return Ok(Some(value));
    }
    let Raw::Tag(OPTION_TAG, option) = value else {
        return Err(TableStorageError);
    };
    let Raw::Array(parts) = option.as_ref() else {
        return Err(TableStorageError);
    };
    match parts.as_slice() {
        [Raw::Int(state)] if *state == BigInt::from(0) => Ok(None),
        [Raw::Int(state), present] if *state == BigInt::from(1) => Ok(Some(present)),
        _ => Err(TableStorageError),
    }
}

/// Whether any stored tuple of `rows` holds a present Blob reference. Such a
/// reference has no portable evaluator value (see [`blob_placeholder`]), so a
/// caller that serves a read to a client refuses it instead of exposing the
/// placeholder as data.
pub fn stored_rows_hold_blob(
    schema: &TableSchema,
    storage_order: &[String],
    rows: &[(Vec<u8>, Vec<u8>)],
) -> Result<bool, TableStorageError> {
    for (_, storage_row) in rows {
        let Raw::Array(values) =
            decode_canonical_storage_value(storage_row).map_err(|_| TableStorageError)?
        else {
            return Err(TableStorageError);
        };
        if values.len() != storage_order.len() {
            return Err(TableStorageError);
        }
        for (value, name) in values.iter().zip(storage_order) {
            let ty = schema.fields.get(name).ok_or(TableStorageError)?;
            if holds_blob(ty) && present_blob(ty, value)?.is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// One stored tuple in portable form: each present Blob reference becomes its
/// [`blob_placeholder`], and every other value must already be portable.
fn portable_storage_row(
    storage_row: &[u8],
    storage_order: &[String],
    schema: &TableSchema,
) -> Result<Vec<u8>, TableStorageError> {
    let Raw::Array(values) =
        decode_canonical_storage_value(storage_row).map_err(|_| TableStorageError)?
    else {
        return Err(TableStorageError);
    };
    if values.len() != storage_order.len() {
        return Err(TableStorageError);
    }
    let values = values
        .into_iter()
        .zip(storage_order)
        .map(|(value, name)| {
            let ty = schema.fields.get(name).ok_or(TableStorageError)?;
            if !holds_blob(ty) {
                return Ok(value);
            }
            match present_blob(ty, &value)? {
                Some(Raw::Tag(ROV3_BLOB_TAG, _)) => Ok(blob_placeholder(ty)),
                Some(_) => Err(TableStorageError),
                None => Ok(value),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Value::new(Raw::Array(values))
        .and_then(|value| value.encode())
        .map_err(|_| TableStorageError)
}

/// Writes committed Blob references back into a stored tuple projected from an
/// evaluator record. `source` is the stored tuple of the row the record was
/// read from, if any. A field holding its [`blob_placeholder`] takes that row's
/// committed reference unchanged, and an absent `Blob?` stays absent. Any
/// other Blob field value is rejected: an evaluator record cannot name a
/// stored Blob reference of its own.
pub fn restore_stored_blobs(
    stored: &[u8],
    storage_order: &[String],
    schema: &TableSchema,
    source: Option<&[u8]>,
) -> Result<Vec<u8>, TableStorageError> {
    let types = storage_order
        .iter()
        .map(|name| schema.fields.get(name).ok_or(TableStorageError))
        .collect::<Result<Vec<_>, _>>()?;
    if !types.iter().any(|ty| holds_blob(ty)) {
        return Ok(stored.to_vec());
    }
    let decode = |bytes: &[u8]| match decode_canonical_storage_value(bytes) {
        Ok(Raw::Array(values)) if values.len() == storage_order.len() => Ok(values),
        _ => Err(TableStorageError),
    };
    let mut values = decode(stored)?;
    let source = source.map(decode).transpose()?;
    for (index, ty) in types.into_iter().enumerate() {
        if !holds_blob(ty) || present_blob(ty, &values[index])?.is_none() {
            continue;
        }
        // Sound only while contains_blob_value and handle_contextual_blob_insert reject non-tag Blobs.
        if values[index] != blob_placeholder(ty) {
            return Err(TableStorageError);
        }
        let committed = source
            .as_ref()
            .map(|values| &values[index])
            .ok_or(TableStorageError)?;
        if !matches!(
            present_blob(ty, committed)?,
            Some(Raw::Tag(ROV3_BLOB_TAG, _))
        ) {
            return Err(TableStorageError);
        }
        values[index] = committed.clone();
    }
    encode_canonical_storage_value(&Raw::Array(values)).map_err(|_| TableStorageError)
}

/// The admitted table schemas a module header declares, keyed by table name. A table whose
/// admission did not succeed is omitted.
pub fn admitted_table_schemas(header: &ModuleHeader) -> BTreeMap<String, TableSchema> {
    header
        .symbols
        .iter()
        .filter(|(_, symbol)| symbol.kind == SymbolKind::Table)
        .filter_map(|(name, symbol)| {
            symbol
                .table_schema
                .clone()
                .filter(|schema| schema.admission.is_some())
                .map(|schema| (name.clone(), schema))
        })
        .collect()
}

/// Projects a canonical evaluator record onto its stored tuple: the values of
/// the stored fields in storage order.
pub fn storage_row_from_record(
    record: &[u8],
    storage_order: &[String],
) -> Result<Vec<u8>, TableStorageError> {
    let decoded = Value::decode(record).map_err(|_| TableStorageError)?;
    let Raw::Map(entries) = decoded.raw() else {
        return Err(TableStorageError);
    };
    let stored_values = storage_order
        .iter()
        .map(|name| {
            entries
                .iter()
                .find_map(|(key, field_value)| match key {
                    Raw::Text(field_name) if field_name == name => Some(field_value.clone()),
                    _ => None,
                })
                .ok_or(TableStorageError)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Value::new(Raw::Array(stored_values))
        .and_then(|row| row.encode())
        .map_err(|_| TableStorageError)
}

/// Field names of a table descriptor: by field id, and the stored (non-key)
/// fields' names in storage order.
pub fn stored_field_names(
    descriptor: &Raw,
) -> Result<(BTreeMap<[u8; 16], String>, Vec<String>), TableStorageError> {
    let Raw::Map(entries) = descriptor else {
        return Err(TableStorageError);
    };
    let fields = entries
        .iter()
        .find_map(|(key, value)| (key == &Raw::Int(BigInt::from(3))).then_some(value))
        .ok_or(TableStorageError)?;
    let Raw::Array(fields) = fields else {
        return Err(TableStorageError);
    };
    let mut by_id = BTreeMap::new();
    let mut in_storage_order = Vec::new();
    for field in fields {
        let Raw::Array(parts) = field else {
            return Err(TableStorageError);
        };
        let Some(Raw::Tag(37, id)) = parts.first() else {
            return Err(TableStorageError);
        };
        let Raw::Bytes(id) = id.as_ref() else {
            return Err(TableStorageError);
        };
        let id: [u8; 16] = id.as_slice().try_into().map_err(|_| TableStorageError)?;
        let Some(Raw::Text(name)) = parts.get(1) else {
            return Err(TableStorageError);
        };
        if matches!(parts.get(3), Some(Raw::Int(role)) if role == &BigInt::from(1)) {
            if by_id.insert(id, name.clone()).is_some() {
                return Err(TableStorageError);
            }
            in_storage_order.push(name.clone());
        }
    }
    Ok((by_id, in_storage_order))
}

/// Maps an evaluator (canonical) key to its runtime key.
pub fn runtime_key_from_evaluator(
    key: &[u8],
    text_primary_key: bool,
) -> Result<Vec<u8>, TableStorageError> {
    if !text_primary_key {
        return Ok(key.to_vec());
    }
    let value = Value::decode(key).map_err(|_| TableStorageError)?;
    let Raw::Text(text) = value.raw() else {
        return Err(TableStorageError);
    };
    Ok(text.as_bytes().to_vec())
}

/// Maps a runtime key back to its canonical evaluator key.
fn evaluator_key_from_runtime(
    key: &[u8],
    text_primary_key: bool,
) -> Result<Vec<u8>, TableStorageError> {
    if !text_primary_key {
        return Value::decode(key)
            .and_then(|value| value.encode())
            .map_err(|_| TableStorageError);
    }
    let text = std::str::from_utf8(key).map_err(|_| TableStorageError)?;
    Value::new(Raw::Text(text.to_owned()))
        .and_then(|value| value.encode())
        .map_err(|_| TableStorageError)
}

/// Builds one evaluator record: the key fields from the evaluator key, then the
/// stored fields from the stored tuple, ordered by field name.
fn evaluator_record(
    key_names: &[String],
    key_bytes: &[u8],
    storage_order: &[String],
    storage_row: &[u8],
) -> Result<Vec<u8>, TableStorageError> {
    let key_value = Value::decode(key_bytes).map_err(|_| TableStorageError)?;
    let key_parts = key_values(key_names.len(), key_value.raw())?;
    let stored = Value::decode(storage_row).map_err(|_| TableStorageError)?;
    let Raw::Array(stored_values) = stored.raw() else {
        return Err(TableStorageError);
    };
    if stored_values.len() != storage_order.len() {
        return Err(TableStorageError);
    }
    let mut fields = Vec::with_capacity(key_names.len() + stored_values.len());
    for (name, value) in key_names.iter().zip(key_parts) {
        fields.push((Raw::Text(name.clone()), value));
    }
    for (name, value) in storage_order.iter().zip(stored_values) {
        fields.push((Raw::Text(name.clone()), value.clone()));
    }
    fields.sort_by(|(left, _), (right, _)| match (left, right) {
        (Raw::Text(left), Raw::Text(right)) => canonical_text_key_order(left, right),
        _ => Ordering::Equal,
    });
    Value::new(Raw::Map(fields))
        .and_then(|value| value.encode())
        .map_err(|_| TableStorageError)
}

/// The primary-key field values a canonical key encodes, in key order.
fn key_values(key_count: usize, key: &Raw) -> Result<Vec<Raw>, TableStorageError> {
    match (key_count, key) {
        (1, _) => Ok(vec![key.clone()]),
        (_, Raw::Array(values)) if values.len() == key_count => Ok(values.clone()),
        _ => Err(TableStorageError),
    }
}

/// Whether the primary key is a single text column, stored as raw UTF-8.
pub fn has_text_primary_key(schema: &TableSchema) -> bool {
    schema
        .admission
        .as_ref()
        .is_some_and(|admission| matches!(admission.keys.as_slice(), [(_, Type::Text)]))
}

/// Canonical map-key order for text: encoding-length class, then length, then bytes.
fn canonical_text_key_order(left: &str, right: &str) -> Ordering {
    fn length_class(length: usize) -> u8 {
        match length {
            0..=23 => 0,
            24..=255 => 1,
            256..=65_535 => 2,
            65_536..=4_294_967_295 => 3,
            _ => 4,
        }
    }

    length_class(left.len())
        .cmp(&length_class(right.len()))
        .then_with(|| left.len().cmp(&right.len()))
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(raw: Raw) -> Vec<u8> {
        Value::new(raw)
            .and_then(|value| value.encode())
            .expect("test value encodes")
    }

    fn instant() -> Raw {
        Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![Raw::Int(7.into()), Raw::Int(99.into())])),
        )
    }

    fn date() -> Raw {
        Raw::Tag(60001, Box::new(Raw::Text("2024-01-01".to_owned())))
    }

    #[test]
    fn text_primary_key_round_trips_through_utf8_runtime_key() {
        let evaluator = encode(Raw::Text("héllo".to_owned()));
        let runtime = runtime_key_from_evaluator(&evaluator, true).expect("text key");
        assert_eq!(runtime, "héllo".as_bytes());
        assert_eq!(
            evaluator_key_from_runtime(&runtime, true).expect("text key"),
            evaluator
        );
    }

    #[test]
    fn text_primary_key_rejects_non_text_and_invalid_utf8() {
        assert!(runtime_key_from_evaluator(&encode(Raw::Int(7.into())), true).is_err());
        assert!(evaluator_key_from_runtime(&[0xff, 0xfe], true).is_err());
    }

    #[test]
    fn non_text_keys_are_identity_for_int_composite_instant_and_date() {
        let keys = [
            encode(Raw::Int(42.into())),
            encode(Raw::Array(vec![
                Raw::Text("a".to_owned()),
                Raw::Int(7.into()),
            ])),
            encode(instant()),
            encode(date()),
        ];
        for key in keys {
            let runtime = runtime_key_from_evaluator(&key, false).expect("identity key");
            assert_eq!(runtime, key);
            assert_eq!(
                evaluator_key_from_runtime(&runtime, false).expect("identity key"),
                key
            );
        }
    }

    #[test]
    fn storage_row_projects_record_fields_in_storage_order() {
        let record = encode(Raw::Map(vec![
            (Raw::Text("a".to_owned()), Raw::Int(1.into())),
            (Raw::Text("b".to_owned()), Raw::Int(2.into())),
        ]));
        let order = vec!["b".to_owned(), "a".to_owned()];
        let row = storage_row_from_record(&record, &order).expect("projects");
        assert_eq!(
            Value::decode(&row).expect("tuple").raw(),
            &Raw::Array(vec![Raw::Int(2.into()), Raw::Int(1.into())])
        );
        assert!(storage_row_from_record(&record, &["missing".to_owned()]).is_err());
    }

    #[test]
    fn evaluator_record_restores_composite_key_fields_and_orders_names() {
        let key = encode(Raw::Array(vec![
            Raw::Text("x".to_owned()),
            Raw::Int(2.into()),
        ]));
        let row = encode(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Text("q".to_owned()),
        ]));
        let key_names = vec!["k1".to_owned(), "k2".to_owned()];
        let order = vec!["z".to_owned(), "m".to_owned()];
        let record = evaluator_record(&key_names, &key, &order, &row).expect("record");
        assert_eq!(
            Value::decode(&record).expect("record").raw(),
            &Raw::Map(vec![
                (Raw::Text("m".to_owned()), Raw::Text("q".to_owned())),
                (Raw::Text("z".to_owned()), Raw::Int(1.into())),
                (Raw::Text("k1".to_owned()), Raw::Text("x".to_owned())),
                (Raw::Text("k2".to_owned()), Raw::Int(2.into())),
            ])
        );
    }

    #[test]
    fn evaluator_record_restores_instant_and_date_key_fields() {
        let row = encode(Raw::Array(vec![]));
        let at_key = encode(instant());
        let at = evaluator_record(&["at".to_owned()], &at_key, &[], &row).expect("instant");
        assert_eq!(
            Value::decode(&at).expect("instant").raw(),
            &Raw::Map(vec![(Raw::Text("at".to_owned()), instant())])
        );
        let on_key = encode(date());
        let on = evaluator_record(&["on".to_owned()], &on_key, &[], &row).expect("date");
        assert_eq!(
            Value::decode(&on).expect("date").raw(),
            &Raw::Map(vec![(Raw::Text("on".to_owned()), date())])
        );
    }

    fn stored_blob(seed: u8) -> Raw {
        Raw::Tag(
            ROV3_BLOB_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(23.into()),
                Raw::Bytes(vec![seed; 32]),
                Raw::Text("audio/mpeg".to_owned()),
                Raw::Null,
                Raw::Bytes(vec![seed; 20]),
            ])),
        )
    }

    fn option(value: Option<Raw>) -> Raw {
        let parts = match value {
            Some(value) => vec![Raw::Int(1.into()), value],
            None => vec![Raw::Int(0.into())],
        };
        Raw::Tag(OPTION_TAG, Box::new(Raw::Array(parts)))
    }

    fn storage(values: Vec<Raw>) -> Vec<u8> {
        encode_canonical_storage_value(&Raw::Array(values)).expect("stored tuple encodes")
    }

    fn media_schema() -> TableSchema {
        TableSchema {
            fields: BTreeMap::from([
                ("id".to_owned(), Type::Text),
                ("title".to_owned(), Type::Text),
                ("content".to_owned(), Type::Named("Blob".into())),
                (
                    "cover".to_owned(),
                    Type::Optional(Box::new(Type::Named("Blob".into()))),
                ),
            ]),
            admission: None,
            conversions: Vec::new(),
            currency: false,
        }
    }

    #[test]
    fn committed_blob_references_survive_the_evaluator_round_trip() {
        let schema = media_schema();
        let order = vec!["content".to_owned(), "cover".to_owned(), "title".to_owned()];
        let committed = storage(vec![
            stored_blob(1),
            option(Some(stored_blob(2))),
            Raw::Text("Live in London".to_owned()),
        ]);
        let portable = portable_storage_row(&committed, &order, &schema).expect("portable");
        assert_eq!(
            Value::decode(&portable).expect("portable tuple").raw(),
            &Raw::Array(vec![
                blob_placeholder(&Type::Named("Blob".into())),
                blob_placeholder(&Type::Optional(Box::new(Type::Named("Blob".into())))),
                Raw::Text("Live in London".to_owned()),
            ])
        );
        assert_eq!(
            restore_stored_blobs(&portable, &order, &schema, Some(&committed)).expect("restored"),
            committed
        );

        // An absent `Blob?` stays absent and needs no committed reference.
        let absent = storage(vec![
            stored_blob(1),
            option(None),
            Raw::Text("t".to_owned()),
        ]);
        let portable = portable_storage_row(&absent, &order, &schema).expect("portable");
        assert_eq!(
            restore_stored_blobs(&portable, &order, &schema, Some(&absent)).expect("restored"),
            absent
        );

        // A placeholder needs the committed row it was read from.
        assert!(restore_stored_blobs(&portable, &order, &schema, None).is_err());
        // An evaluator value other than the placeholder cannot become a Blob.
        let invented = encode(Raw::Array(vec![
            Raw::Bytes(vec![7]),
            option(None),
            Raw::Text("t".to_owned()),
        ]));
        assert!(restore_stored_blobs(&invented, &order, &schema, Some(&absent)).is_err());
        // A table without Blob fields passes its tuple through unchanged.
        let titles = vec!["title".to_owned()];
        let plain = encode(Raw::Array(vec![Raw::Text("t".to_owned())]));
        assert_eq!(
            restore_stored_blobs(&plain, &titles, &schema, None).expect("plain"),
            plain
        );
    }

    #[test]
    fn read_budget_refuses_the_row_past_the_table_limit() {
        let budget = CommittedReadBudget::new(CommittedReadLimits {
            max_rows_per_table: 2,
            max_bytes: usize::MAX,
        });
        assert_eq!(budget.check_row_slot(0), Ok(()));
        assert_eq!(budget.check_row_slot(1), Ok(()));
        assert_eq!(
            budget.check_row_slot(2),
            Err(CommittedReadError::Limit { rows_seen: 3 })
        );
    }

    #[test]
    fn read_budget_refuses_the_row_past_the_byte_limit_across_tables() {
        let mut budget = CommittedReadBudget::new(CommittedReadLimits {
            max_rows_per_table: 10,
            max_bytes: 5,
        });
        // Two tables share one byte budget: 3 bytes in the first, 2 in the second.
        assert_eq!(budget.admit_row_bytes(0, 3), Ok(()));
        assert_eq!(budget.admit_row_bytes(0, 2), Ok(()));
        assert_eq!(
            budget.admit_row_bytes(1, 1),
            Err(CommittedReadError::Limit { rows_seen: 2 })
        );
    }
}
