//! Committed-table row codec shared by every consumer of table storage.
//!
//! A row exists in three forms. A *runtime* key is the byte string the native
//! store orders by: a text primary key is stored as its UTF-8 bytes, and every
//! other key as its canonical OVB encoding. A *stored tuple* is a canonical
//! array of the table's non-key field values in descriptor storage order. An
//! *evaluator* record is a canonical map from field name to value, key fields
//! included. The functions here perform these conversions for committed-table reads and
//! evaluator-form mutations; `orna-storage-v1` keeps its own descriptor-level projection.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use num_bigint::BigInt;
use orna_repository_v1::{GitCommitRef, KeyRange, Repository, RepositoryFormatContext};
use orna_runtime_v1::TableObjectId;
use orna_semantic_v1::{ModuleHeader, SymbolKind, TableSchema, Type};
use orna_value_v1::{decode_canonical_storage_value, Raw, Value};

use crate::ProjectLoader;

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
    /// `(runtime key, stored tuple)` pairs in ascending runtime-key order.
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Reads every table's committed rows at `commit`, keyed in the runtime keyspace.
pub fn committed_tables(
    repository: &Repository,
    format_context: &RepositoryFormatContext,
    commit: &GitCommitRef,
    tables: &BTreeMap<String, TableSchema>,
    table_ids: &BTreeMap<String, TableObjectId>,
) -> Result<BTreeMap<String, CommittedTable>, TableStorageError> {
    let mut result = BTreeMap::new();
    for (table_name, table_schema) in tables {
        let object_id = table_ids
            .get(table_name)
            .copied()
            .ok_or(TableStorageError)?;
        let resolved_schema = ProjectLoader::default()
            .resolve_committed_source_table_schema(repository, commit, object_id)
            .map_err(|_| TableStorageError)?;
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
                let decoded = graph
                    .decode_admitted_row_fields(&row, binding, &read_scope)
                    .map_err(|_| TableStorageError)?;
                let Raw::Array(stored_values) =
                    decode_canonical_storage_value(decoded.canonical_fields())
                        .map_err(|_| TableStorageError)?
                else {
                    return Err(TableStorageError);
                };
                if stored_values.len() != decoded.fields().len() {
                    return Err(TableStorageError);
                }
                let canonical_key = row.key().canonical_bytes().map_err(|_| TableStorageError)?;
                let key_value = Value::decode(&canonical_key).map_err(|_| TableStorageError)?;
                let key_bytes = key_value.encode().map_err(|_| TableStorageError)?;
                // Reject a key whose shape does not match the primary key declaration.
                key_values(admission.keys.len(), key_value.raw())?;
                let runtime_key = runtime_key_from_evaluator(&key_bytes, text_primary_key)?;
                let mut by_name = BTreeMap::new();
                for (field, value) in decoded.fields().iter().zip(stored_values) {
                    let name = field_names.get(field.field_id()).ok_or(TableStorageError)?;
                    Value::new(value.clone()).map_err(|_| TableStorageError)?;
                    if by_name.insert(name.clone(), value).is_some() {
                        return Err(TableStorageError);
                    }
                }
                let storage_row = storage_order
                    .iter()
                    .map(|name| by_name.remove(name).ok_or(TableStorageError))
                    .collect::<Result<Vec<_>, _>>()?;
                let row_value = Value::new(Raw::Array(storage_row))
                    .and_then(|value| value.encode())
                    .map_err(|_| TableStorageError)?;
                if committed.insert(runtime_key, row_value).is_some() {
                    return Err(TableStorageError);
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
pub fn evaluator_rows(
    table: &CommittedTable,
    schema: &TableSchema,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, TableStorageError> {
    let (key_names, text_primary_key) = evaluator_keying(schema)?;
    let mut rows = BTreeMap::new();
    for (runtime_key, storage_row) in &table.rows {
        let (key_bytes, record) = evaluator_row_with_keying(
            &key_names,
            text_primary_key,
            runtime_key,
            storage_row,
            &table.storage_order,
        )?;
        if rows.insert(key_bytes, record).is_some() {
            return Err(TableStorageError);
        }
    }
    Ok(rows.into_iter().collect())
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
}
