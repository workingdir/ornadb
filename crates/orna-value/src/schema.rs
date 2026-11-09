//! Schema descriptors and schema-directed value validation.

use crate::Result;
use crate::blob::MAX_BLOB_LENGTH;
use crate::cbor::{Reader, encode_raw, write_raw};
use crate::error::Error;
use crate::format3::{Format3Context, validate_annotation_fields};
use crate::identity::uuid_array;
use crate::media::MimeRegistry;
use crate::raw::{Raw, Value, array, bytes, int_u64, integer, map, text};
use crate::tags::{
    DATE_TAG, DECIMAL_TAG, DURATION_TAG, ENUM_TAG, INSTANT_TAG, LOCAL_DATE_TIME_TAG, MONEY_TAG,
    OPTION_TAG, QUANTITY_TAG, RANGE_TAG, RECORD_TAG, REFINED_NOMINAL_TAG, ROV3_BLOB_TAG,
    STORED_TABLE_REFERENCE_TAG, TIME_OF_DAY_TAG, TIME_ZONE_TAG, TUPLE_TAG, UNIT_TAG, UUID_TAG,
    ZONED_DATE_TIME_TAG,
};
use crate::validation::{validate_raw, validate_tag};
use num_bigint::BigInt;
use num_traits::Zero;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

/// The closed descriptor map defined in Format §29.  It is retained as a
/// checked raw representation to avoid accidentally dropping unknown details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaDescriptor(Raw);

impl SchemaDescriptor {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        validate_schema(&raw)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v = Value::decode(bytes)?;
        Self::new(v.0)
    }

    /// Validates one canonical ORP key and complete ROV stored-field tuple
    /// against this descriptor and its relation identity. The key is a CBOR
    /// scalar for a one-component primary key, or an array in primary-key
    /// order for a composite key. Stored fields are a CBOR array containing
    /// every role-1 field in descriptor order. The repository owner remains
    /// responsible for authenticating the descriptor bytes and their snapshot.
    pub fn validate_stored_row(
        &self,
        relation_id: [u8; 16],
        canonical_key: &[u8],
        canonical_fields: &[u8],
    ) -> Result<()> {
        validate_schema_stored_row(&self.0, relation_id, canonical_key, canonical_fields)
    }
    /// Validates a stored row and binds every stored table reference to the database
    /// of its containing snapshot. The repository remains responsible for
    /// authenticating the descriptor and snapshot context.
    pub fn validate_stored_row_in_context(
        &self,
        context: &Format3Context,
        relation_id: [u8; 16],
        canonical_key: &[u8],
        canonical_fields: &[u8],
    ) -> Result<()> {
        context.inner.ensure_live()?;
        self.validate_stored_row(relation_id, canonical_key, canonical_fields)?;
        let fields = decode_canonical_storage_value(canonical_fields)?;
        validate_reference_database(&fields, context.database_uuid())
    }
    /// Validates one canonical ORP key against this descriptor and relation.
    /// descriptor bytes and its snapshot.
    pub fn validate_stored_key(&self, relation_id: [u8; 16], canonical_key: &[u8]) -> Result<()> {
        validate_schema_stored_key(&self.0, relation_id, canonical_key)
    }

    /// Names the primitive type of each primary-key component in key order
    /// (for example `"Int"`, `"UInt"` or `"Str"`), so a caller can parse a key
    /// spelling as the type the committed descriptor declares. A component
    /// whose type is not a primitive name yields `None`.
    pub fn primary_key_primitive_types(
        &self,
        relation_id: [u8; 16],
    ) -> Result<Vec<Option<String>>> {
        let (key_types, _, _) = schema_stored_key_types(&self.0, relation_id)?;
        key_types
            .into_iter()
            .map(|key_type| {
                let node = array(key_type)?;
                if node.len() != 2 || int_u64(&node[0])? != 0 {
                    return Ok(None);
                }
                Ok(Some(text(&node[1])?.to_owned()))
            })
            .collect()
    }
}

/// Decodes one canonical CBOR value stored in a repository row.
///
/// Unlike [`Value::decode`], this preserves repository-profile tags such as
/// stored ROV-3 Blob references. Callers must validate the value against its
/// authenticated row schema before treating it as an admitted value.
pub fn decode_canonical_storage_value(bytes: &[u8]) -> Result<Raw> {
    let mut reader = Reader::new(bytes);
    let raw = reader.raw(0)?;
    if reader.at != bytes.len() {
        return Err(Error::TrailingBytes);
    }
    if encode_canonical_storage_value(&raw)?.as_slice() != bytes {
        return Err(Error::NonCanonical);
    }
    Ok(raw)
}

/// Encodes one value as the canonical CBOR a repository row stores, the
/// inverse of [`decode_canonical_storage_value`]. Repository-profile tags pass
/// through unvalidated; callers must validate the bytes against their
/// authenticated row schema.
pub fn encode_canonical_storage_value(raw: &Raw) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_raw(raw, &mut bytes)?;
    Ok(bytes)
}

fn validate_schema_stored_row(
    schema: &Raw,
    relation_id: [u8; 16],
    canonical_key: &[u8],
    canonical_fields: &[u8],
) -> Result<()> {
    let (key_types, declared_fields, definitions) = schema_stored_key_types(schema, relation_id)?;
    let key = decode_canonical_storage_value(canonical_key)?;
    validate_stored_key_value(&key, &key_types, definitions)?;
    let values = decode_canonical_storage_value(canonical_fields)?;
    let values = array(&values)?;

    let stored_types = declared_fields
        .iter()
        .filter_map(|field| {
            let field = array(field).ok()?;
            (int_u64(field.get(3)?).ok()? == 1)
                .then(|| field.get(2))
                .flatten()
        })
        .collect::<Vec<_>>();
    if values.len() != stored_types.len() {
        return Err(Error::InvalidValue);
    }
    for (value, ty) in values.iter().zip(stored_types) {
        validate_stored_value_as_type(value, ty, definitions)?;
    }
    Ok(())
}

fn validate_reference_database(value: &Raw, database_id: [u8; 16]) -> Result<()> {
    match value {
        Raw::Tag(STORED_TABLE_REFERENCE_TAG, payload) => {
            let fields = array(payload)?;
            if fields.len() != 3 || uuid_array(&fields[0])? != database_id {
                return Err(Error::InvalidValue);
            }
            for field in fields {
                validate_reference_database(field, database_id)?;
            }
            Ok(())
        }
        Raw::Tag(_, payload) => validate_reference_database(payload, database_id),
        Raw::Array(values) => {
            for value in values {
                validate_reference_database(value, database_id)?;
            }
            Ok(())
        }
        Raw::Map(entries) => {
            for (key, value) in entries {
                validate_reference_database(key, database_id)?;
                validate_reference_database(value, database_id)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_schema_stored_key(
    schema: &Raw,
    relation_id: [u8; 16],
    canonical_key: &[u8],
) -> Result<()> {
    let (key_types, _, definitions) = schema_stored_key_types(schema, relation_id)?;
    let key = decode_canonical_storage_value(canonical_key)?;
    validate_stored_key_value(&key, &key_types, definitions)
}

fn schema_stored_key_types(
    schema: &Raw,
    relation_id: [u8; 16],
) -> Result<(Vec<&Raw>, &[Raw], &[Raw])> {
    let mut schema_map = BTreeMap::new();
    for (key, value) in map(schema)? {
        schema_map.insert(int_u64(key)?, value);
    }
    if uuid_array(schema_map.get(&1).ok_or(Error::InvalidSchema)?)? != relation_id {
        return Err(Error::InvalidSchema);
    }
    let key_ids = array(schema_map.get(&2).ok_or(Error::InvalidSchema)?)?;
    let declared_fields = array(schema_map.get(&3).ok_or(Error::InvalidSchema)?)?;
    let definitions = array(schema_map.get(&4).ok_or(Error::InvalidSchema)?)?;
    let mut key_types = Vec::with_capacity(key_ids.len());
    for key_id in key_ids {
        let key_id = uuid_array(key_id)?;
        let field = declared_fields
            .iter()
            .find(|field| {
                let Ok(fields) = array(field) else {
                    return false;
                };
                fields.first().and_then(|id| uuid_array(id).ok()) == Some(key_id)
            })
            .ok_or(Error::InvalidSchema)?;
        key_types.push(array(field)?.get(2).ok_or(Error::InvalidSchema)?);
    }
    if key_types.is_empty() {
        return Err(Error::InvalidSchema);
    }
    Ok((key_types, declared_fields, definitions))
}

fn validate_stored_key_value(key: &Raw, key_types: &[&Raw], definitions: &[Raw]) -> Result<()> {
    if key_types.len() == 1 {
        validate_stored_value_as_type(key, key_types[0], definitions)
    } else {
        let key_values = array(key)?;
        if key_values.len() != key_types.len() {
            return Err(Error::InvalidValue);
        }
        for (value, ty) in key_values.iter().zip(key_types) {
            validate_stored_value_as_type(value, ty, definitions)?;
        }
        Ok(())
    }
}

fn validate_stored_value_as_type(value: &Raw, ty: &Raw, definitions: &[Raw]) -> Result<()> {
    let node = array(ty)?;
    let code = int_u64(node.first().ok_or(Error::InvalidSchema)?)?;
    match code {
        0 => {
            if matches!(node.get(1), Some(Raw::Text(name)) if name == "Blob") {
                return match value {
                    Raw::Tag(ROV3_BLOB_TAG, payload) if valid_stored_blob_reference(payload) => {
                        Ok(())
                    }
                    _ => Err(Error::InvalidValue),
                };
            }
            validate_raw(value, 0)?;
            validate_value_as_type(value, ty)
        }
        1 => {
            for item in array(value)? {
                validate_stored_value_as_type(
                    item,
                    node.get(1).ok_or(Error::InvalidSchema)?,
                    definitions,
                )?;
            }
            Ok(())
        }
        2 => validate_stored_option(value, node.get(1).ok_or(Error::InvalidSchema)?, definitions),
        3 => {
            let types = array(node.get(1).ok_or(Error::InvalidSchema)?)?;
            if types.is_empty() {
                let Raw::Tag(UNIT_TAG, unit) = value else {
                    return Err(Error::InvalidValue);
                };
                return validate_tag(UNIT_TAG, unit);
            }
            let Raw::Tag(TUPLE_TAG, tuple) = value else {
                return Err(Error::InvalidValue);
            };
            validate_tag(TUPLE_TAG, tuple)?;
            let values = array(tuple)?;
            if values.len() != types.len() {
                return Err(Error::InvalidValue);
            }
            for (value, ty) in values.iter().zip(types) {
                validate_stored_value_as_type(value, ty, definitions)?;
            }
            Ok(())
        }
        4 => validate_anonymous_record(
            value,
            array(node.get(1).ok_or(Error::InvalidSchema)?)?,
            definitions,
        ),
        5 => validate_nominal_stored_value(
            value,
            uuid_array(node.get(1).ok_or(Error::InvalidSchema)?)?,
            definitions,
        ),
        6 => validate_table_reference_value(value, node, definitions),
        7 => {
            let Raw::Tag(QUANTITY_TAG, quantity) = value else {
                return Err(Error::InvalidValue);
            };
            validate_tag(QUANTITY_TAG, quantity)?;
            let quantity = array(quantity)?;
            if quantity[1] != node[2] {
                return Err(Error::InvalidValue);
            }
            validate_stored_value_as_type(&quantity[0], &node[1], definitions)
        }
        8 => {
            let currency = uuid_array(node.get(1).ok_or(Error::InvalidSchema)?)?;
            let Raw::Tag(MONEY_TAG, money) = value else {
                return Err(Error::InvalidValue);
            };
            validate_tag(MONEY_TAG, money)?;
            let money = array(money)?;
            if uuid_array(&money[1])? != currency {
                return Err(Error::InvalidValue);
            }
            let Raw::Tag(DECIMAL_TAG, decimal) = &money[0] else {
                return Err(Error::InvalidValue);
            };
            validate_tag(DECIMAL_TAG, decimal)
        }
        9 => {
            let Raw::Tag(RANGE_TAG, range) = value else {
                return Err(Error::InvalidValue);
            };
            validate_tag(RANGE_TAG, range)?;
            let endpoints = array(range)?;
            validate_stored_option(&endpoints[0], &node[1], definitions)?;
            validate_stored_option(&endpoints[1], &node[1], definitions)
        }
        _ => Err(Error::InvalidSchema),
    }
}

fn validate_stored_option(value: &Raw, ty: &Raw, definitions: &[Raw]) -> Result<()> {
    let Raw::Tag(OPTION_TAG, option) = value else {
        return Err(Error::InvalidValue);
    };
    validate_tag(OPTION_TAG, option)?;
    match array(option)?.as_slice() {
        [Raw::Int(index)] if index.is_zero() => Ok(()),
        [Raw::Int(index), value] if *index == BigInt::from(1) => {
            validate_stored_value_as_type(value, ty, definitions)
        }
        _ => Err(Error::InvalidValue),
    }
}

pub(crate) fn validate_anonymous_record(
    value: &Raw,
    fields: &[Raw],
    definitions: &[Raw],
) -> Result<()> {
    let Raw::Tag(RECORD_TAG, record) = value else {
        return Err(Error::InvalidValue);
    };
    let record = array(record)?;
    if record.len() != 2 || !matches!(record[0], Raw::Null) {
        return Err(Error::InvalidValue);
    }
    let values = array(&record[1])?;
    if values.len() != fields.len() {
        return Err(Error::InvalidValue);
    }
    // Stored fields are ordered by encoded field key (ORNA-FORMAT-004), which
    // need not match descriptor declaration order, so match each entry by key.
    let mut prior = None;
    for value in values {
        let entry = array(value)?;
        if entry.len() != 2 {
            return Err(Error::InvalidValue);
        }
        let key = encode_raw(&entry[0])?;
        if prior.as_ref().is_some_and(|x: &Vec<u8>| key <= *x) {
            return Err(Error::DuplicateOrUnorderedMapKey);
        }
        prior = Some(key);
        let field = fields
            .iter()
            .map(array)
            .find_map(|field| match field {
                Ok(field) if field.first() == Some(&entry[0]) => Some(Ok(field)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .ok_or(Error::InvalidValue)??;
        validate_stored_value_as_type(
            &entry[1],
            field.get(1).ok_or(Error::InvalidSchema)?,
            definitions,
        )?;
    }
    Ok(())
}

fn valid_stored_blob_reference(payload: &Raw) -> bool {
    let Ok(fields) = array(payload) else {
        return false;
    };
    let [
        length,
        digest,
        Raw::Text(media_type),
        suffix,
        Raw::Bytes(oid),
    ] = fields.as_slice()
    else {
        return false;
    };
    int_u64(length).is_ok_and(|length| length < MAX_BLOB_LENGTH)
        && bytes(digest).is_ok_and(|digest| digest.len() == 32)
        && matches!(oid.len(), 20 | 32)
        && validate_annotation_fields(media_type, suffix, MimeRegistry::mime1()).is_ok()
}

fn validate_table_reference_value(value: &Raw, node: &[Raw], definitions: &[Raw]) -> Result<()> {
    let Raw::Tag(STORED_TABLE_REFERENCE_TAG, reference) = value else {
        return Err(Error::InvalidValue);
    };
    let reference = array(reference)?;
    if reference.len() != 3
        || uuid_array(&reference[0])? != uuid_array(node.get(1).ok_or(Error::InvalidSchema)?)?
        || uuid_array(&reference[1])? != uuid_array(node.get(2).ok_or(Error::InvalidSchema)?)?
    {
        return Err(Error::InvalidValue);
    }
    let key_types = array(node.get(3).ok_or(Error::InvalidSchema)?)?;
    match key_types.as_slice() {
        [] => {
            if !matches!(&reference[2], Raw::Tag(UNIT_TAG, unit) if array(unit).is_ok_and(|items| items.is_empty()))
            {
                return Err(Error::InvalidValue);
            }
        }
        [ty] => validate_stored_value_as_type(&reference[2], ty, definitions)?,
        types => {
            let values = match &reference[2] {
                Raw::Tag(TUPLE_TAG, values) => array(values)?,
                _ => return Err(Error::InvalidValue),
            };
            if values.len() != types.len() {
                return Err(Error::InvalidValue);
            }
            for (value, ty) in values.iter().zip(types) {
                validate_stored_value_as_type(value, ty, definitions)?;
            }
        }
    }
    Ok(())
}

fn validate_nominal_stored_value(value: &Raw, id: [u8; 16], definitions: &[Raw]) -> Result<()> {
    let definition = definitions
        .iter()
        .find(|definition| {
            let Ok(fields) = array(definition) else {
                return false;
            };
            fields.first().and_then(|raw| uuid_array(raw).ok()) == Some(id)
        })
        .ok_or(Error::InvalidSchema)?;
    let definition = array(definition)?;
    let kind = int_u64(definition.get(1).ok_or(Error::InvalidSchema)?)?;
    let body = definition.get(2).ok_or(Error::InvalidSchema)?;
    match kind {
        0 => validate_record_fields(value, id, array(body)?, definitions),
        1 => validate_enum_fields(value, id, array(body)?, definitions),
        // A refined nominal value is tag 60020 `[type_object_id, base_value]`
        // (29-formats.md); the base value has the definition's base type.
        2 => {
            let Raw::Tag(REFINED_NOMINAL_TAG, payload) = value else {
                return Err(Error::InvalidValue);
            };
            let refined = array(payload)?;
            if refined.len() != 2 || uuid_array(&refined[0])? != id {
                return Err(Error::InvalidValue);
            }
            validate_stored_value_as_type(
                &refined[1],
                array(body)?.first().ok_or(Error::InvalidSchema)?,
                definitions,
            )
        }
        _ => Err(Error::InvalidSchema),
    }
}

fn validate_record_fields(
    value: &Raw,
    id: [u8; 16],
    fields: &[Raw],
    definitions: &[Raw],
) -> Result<()> {
    let Raw::Tag(RECORD_TAG, payload) = value else {
        return Err(Error::InvalidValue);
    };
    let record = array(payload)?;
    if record.len() != 2 || uuid_array(&record[0])? != id {
        return Err(Error::InvalidValue);
    }
    let values = array(&record[1])?;
    if values.len() != fields.len() {
        return Err(Error::InvalidValue);
    }
    for (field, value) in fields.iter().zip(values) {
        let field = array(field)?;
        let entry = array(value)?;
        if entry.len() != 2 || entry[0] != field[0] {
            return Err(Error::InvalidValue);
        }
        validate_stored_value_as_type(&entry[1], &field[2], definitions)?;
    }
    Ok(())
}

fn validate_enum_fields(
    value: &Raw,
    id: [u8; 16],
    variants: &[Raw],
    definitions: &[Raw],
) -> Result<()> {
    let Raw::Tag(ENUM_TAG, payload) = value else {
        return Err(Error::InvalidValue);
    };
    let encoded = array(payload)?;
    if encoded.len() != 3 || uuid_array(&encoded[0])? != id {
        return Err(Error::InvalidValue);
    }
    let variant_id = uuid_array(&encoded[1])?;
    let variant = variants
        .iter()
        .find(|variant| {
            let Ok(fields) = array(variant) else {
                return false;
            };
            fields.first().and_then(|raw| uuid_array(raw).ok()) == Some(variant_id)
        })
        .ok_or(Error::InvalidValue)?;
    let variant_fields = array(array(variant)?.get(2).ok_or(Error::InvalidSchema)?)?;
    if variant_fields.is_empty() {
        return if matches!(encoded[2], Raw::Null) {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        };
    }
    validate_record_fields(&encoded[2], variant_id, variant_fields, definitions)
}

fn validate_field_fallback(role: u64, fallback: &Raw, field_type: &Raw) -> Result<()> {
    match array(fallback)?.as_slice() {
        [Raw::Int(kind)] if kind.is_zero() => {}
        [Raw::Int(kind), value] if *kind == BigInt::from(1) && role == 1 => {
            validate_value_as_type(value, field_type).map_err(|_| Error::InvalidSchema)?;
        }
        [Raw::Int(kind), Raw::Bytes(bytes)]
            if *kind == BigInt::from(2) && role == 2 && bytes.len() == 32 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn validate_schema(raw: &Raw) -> Result<()> {
    let m = map(raw)?;
    if m.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    let mut got = BTreeMap::new();
    for (k, v) in m {
        got.insert(int_u64(k)?, v);
    }
    if got.keys().copied().collect::<Vec<_>>() != vec![0, 1, 2, 3, 4] || int_u64(got[&0])? != 1 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(got[&1])?;
    let keys = array(got[&2])?;
    let fields = array(got[&3])?;
    let defs = array(got[&4])?;
    let mut ids = Vec::new();
    let mut key_role_ids = Vec::new();
    for f in fields {
        let a = array(f)?;
        if a.len() != 5 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        ids.push(id);
        text(&a[1])?;
        validate_type(&a[2])?;
        let role = int_u64(&a[3])?;
        if role > 2 {
            return Err(Error::InvalidSchema);
        }
        validate_field_fallback(role, &a[4], &a[2])?;
        if role == 0 {
            key_role_ids.push(id);
        }
    }
    if ids.windows(2).any(|x| x[0] >= x[1]) {
        return Err(Error::InvalidSchema);
    }
    let mut key_ids = Vec::new();
    let mut key_id_set = BTreeSet::new();
    for key in keys {
        let key = uuid_array(key)?;
        if ids.binary_search(&key).is_err() || !key_id_set.insert(key) {
            return Err(Error::InvalidSchema);
        }
        key_ids.push(key);
    }
    let key_role_set: BTreeSet<_> = key_role_ids.iter().copied().collect();
    if key_id_set != key_role_set || key_ids.len() != key_role_ids.len() {
        return Err(Error::InvalidSchema);
    }
    let mut definition_ids = Vec::new();
    let mut unit_definition_ids = Vec::new();
    for d in defs {
        let a = array(d)?;
        if a.len() != 3 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        if definition_ids
            .last()
            .is_some_and(|previous| *previous >= id)
        {
            return Err(Error::InvalidSchema);
        }
        definition_ids.push(id);
        let kind = int_u64(&a[1])?;
        if kind == 3 {
            unit_definition_ids.push(id);
        }
        validate_definition(kind, &a[2])?;
    }
    for field in fields {
        validate_type_refs(&array(field)?[2], &definition_ids, &unit_definition_ids)?;
    }
    for definition in defs {
        validate_definition_refs(
            int_u64(&array(definition)?[1])?,
            &array(definition)?[2],
            &definition_ids,
            &unit_definition_ids,
        )?;
    }
    validate_schema_constructibility(fields, defs, &definition_ids)?;
    Ok(())
}

fn validate_type_refs(
    node: &Raw,
    definitions: &[[u8; 16]],
    unit_definitions: &[[u8; 16]],
) -> Result<()> {
    let a = array(node)?;
    match int_u64(&a[0])? {
        5 | 8 => {
            if definitions.binary_search(&uuid_array(&a[1])?).is_err() {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => validate_type_refs(&a[1], definitions, unit_definitions)?,
        3 => {
            for x in array(&a[1])? {
                validate_type_refs(x, definitions, unit_definitions)?
            }
        }
        4 => {
            for x in array(&a[1])? {
                validate_type_refs(&array(x)?[1], definitions, unit_definitions)?
            }
        }
        6 => {
            for x in array(&a[3])? {
                validate_type_refs(x, definitions, unit_definitions)?
            }
        }
        7 => {
            if unit_definitions.binary_search(&uuid_array(&a[2])?).is_err() {
                return Err(Error::InvalidSchema);
            }
            validate_type_refs(&a[1], definitions, unit_definitions)?;
        }
        0 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn validate_definition_refs(
    kind: u64,
    body: &Raw,
    definitions: &[[u8; 16]],
    unit_definitions: &[[u8; 16]],
) -> Result<()> {
    match kind {
        0 => {
            for f in array(body)? {
                validate_type_refs(&array(f)?[2], definitions, unit_definitions)?
            }
        }
        1 => {
            for v in array(body)? {
                for f in array(&array(v)?[2])? {
                    validate_type_refs(&array(f)?[2], definitions, unit_definitions)?
                }
            }
        }
        2 => validate_type_refs(&array(body)?[0], definitions, unit_definitions)?,
        3 | 4 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn validate_schema_constructibility(
    fields: &[Raw],
    definitions: &[Raw],
    definition_ids: &[[u8; 16]],
) -> Result<()> {
    let mut dependents = vec![Vec::new(); definitions.len()];
    for (index, definition) in definitions.iter().enumerate() {
        collect_constructibility_dependencies(definition, index, definition_ids, &mut dependents)?;
    }

    let mut constructible = vec![false; definitions.len()];
    let mut pending = (0..definitions.len()).collect::<Vec<_>>();
    while let Some(index) = pending.pop() {
        if constructible[index] {
            continue;
        }
        if definition_is_constructible(&definitions[index], definition_ids, &constructible)? {
            constructible[index] = true;
            pending.extend(dependents[index].iter().copied());
        }
    }
    // Definitions are value types even when no descriptor field uses them.
    if constructible.contains(&false) {
        return Err(Error::InvalidSchema);
    }

    for field in fields {
        if !type_node_constructible(&array(field)?[2], definition_ids, &constructible)? {
            return Err(Error::InvalidSchema);
        }
    }
    Ok(())
}

fn definition_is_constructible(
    definition: &Raw,
    definition_ids: &[[u8; 16]],
    constructible: &[bool],
) -> Result<bool> {
    let definition = array(definition)?;
    let kind = int_u64(&definition[1])?;
    let body = &definition[2];
    match kind {
        0 => constructible_fields(array(body)?, definition_ids, constructible),
        1 => {
            for variant in array(body)? {
                if constructible_fields(array(&array(variant)?[2])?, definition_ids, constructible)?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        2 => type_node_constructible(&array(body)?[0], definition_ids, constructible),
        3 | 4 => Ok(true),
        _ => Err(Error::InvalidSchema),
    }
}

fn collect_constructibility_dependencies(
    definition: &Raw,
    dependent: usize,
    definition_ids: &[[u8; 16]],
    dependents: &mut [Vec<usize>],
) -> Result<()> {
    let definition = array(definition)?;
    let body = &definition[2];
    match int_u64(&definition[1])? {
        0 => {
            for field in array(body)? {
                collect_type_dependencies(
                    &array(field)?[2],
                    dependent,
                    definition_ids,
                    dependents,
                )?;
            }
        }
        1 => {
            for variant in array(body)? {
                for field in array(&array(variant)?[2])? {
                    collect_type_dependencies(
                        &array(field)?[2],
                        dependent,
                        definition_ids,
                        dependents,
                    )?;
                }
            }
        }
        2 => collect_type_dependencies(&array(body)?[0], dependent, definition_ids, dependents)?,
        3 | 4 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn collect_type_dependencies(
    ty: &Raw,
    dependent: usize,
    definition_ids: &[[u8; 16]],
    dependents: &mut [Vec<usize>],
) -> Result<()> {
    let node = array(ty)?;
    match int_u64(&node[0])? {
        0 | 1 | 2 | 9 => {}
        3 => {
            for component in array(&node[1])? {
                collect_type_dependencies(component, dependent, definition_ids, dependents)?;
            }
        }
        4 => {
            for field in array(&node[1])? {
                collect_type_dependencies(
                    &array(field)?[1],
                    dependent,
                    definition_ids,
                    dependents,
                )?;
            }
        }
        5 | 8 => {
            let id = uuid_array(&node[1])?;
            let index = definition_ids
                .binary_search(&id)
                .map_err(|_| Error::InvalidSchema)?;
            dependents[index].push(dependent);
        }
        6 => {
            for component in array(&node[3])? {
                collect_type_dependencies(component, dependent, definition_ids, dependents)?;
            }
        }
        7 => {
            collect_type_dependencies(&node[1], dependent, definition_ids, dependents)?;
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn constructible_fields(
    fields: &[Raw],
    definition_ids: &[[u8; 16]],
    constructible: &[bool],
) -> Result<bool> {
    for field in fields {
        if !type_node_constructible(&array(field)?[2], definition_ids, constructible)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn type_node_constructible(
    ty: &Raw,
    definition_ids: &[[u8; 16]],
    constructible: &[bool],
) -> Result<bool> {
    let node = array(ty)?;
    match int_u64(&node[0])? {
        0 | 1 | 2 | 9 => Ok(true),
        3 => {
            for component in array(&node[1])? {
                if !type_node_constructible(component, definition_ids, constructible)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        4 => {
            for field in array(&node[1])? {
                let field = array(field)?;
                if !type_node_constructible(&field[1], definition_ids, constructible)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        5 | 8 => {
            let id = uuid_array(&node[1])?;
            let index = definition_ids
                .binary_search(&id)
                .map_err(|_| Error::InvalidSchema)?;
            Ok(constructible[index])
        }
        6 => {
            for key_type in array(&node[3])? {
                if !type_node_constructible(key_type, definition_ids, constructible)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        7 => type_node_constructible(&node[1], definition_ids, constructible),
        _ => Err(Error::InvalidSchema),
    }
}

fn validate_definition(kind: u64, body: &Raw) -> Result<()> {
    match kind {
        0 => {
            validate_nested_field_list(array(body)?)?;
        }
        1 => {
            let mut previous = None;
            for variant in array(body)? {
                let v = array(variant)?;
                if v.len() != 3 {
                    return Err(Error::InvalidSchema);
                }
                let id = uuid_array(&v[0])?;
                if previous.is_some_and(|p| p >= id) {
                    return Err(Error::InvalidSchema);
                }
                previous = Some(id);
                text(&v[1])?;
                validate_nested_field_list(array(&v[2])?)?;
            }
        }
        2 => {
            let a = array(body)?;
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[0])?;
            for digest in array(&a[1])? {
                if bytes(digest)?.len() != 32 {
                    return Err(Error::InvalidSchema);
                }
            }
        }
        3 => {
            let a = array(body)?;
            if a.len() != 6 {
                return Err(Error::InvalidSchema);
            }
            let dimension = array(&a[0])?;
            if dimension.iter().any(|v| !matches!(v, Raw::Int(_))) {
                return Err(Error::InvalidSchema);
            }
            for value in &a[1..5] {
                integer(value)?;
            }
            if !matches!(a[5], Raw::Bool(_)) {
                return Err(Error::InvalidSchema);
            }
        }
        4 => {
            let a = array(body)?;
            if a.len() != 2 || text(&a[0])?.len() != 3 || int_u64(&a[1])? > 18 {
                return Err(Error::InvalidSchema);
            }
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

fn validate_nested_field(field: &[Raw]) -> Result<()> {
    if field.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(&field[0])?;
    let name = text(&field[1])?;
    if name.is_empty() {
        return Err(Error::InvalidSchema);
    }
    validate_type(&field[2])?;
    let role = int_u64(&field[3])?;
    if role > 2 {
        return Err(Error::InvalidSchema);
    }
    validate_field_fallback(role, &field[4], &field[2])?;
    Ok(())
}

fn validate_nested_field_list(fields: &[Raw]) -> Result<()> {
    let mut previous = None;
    for field in fields {
        let f = array(field)?;
        let id = uuid_array(&f[0])?;
        if previous.is_some_and(|p| p >= id) {
            return Err(Error::InvalidSchema);
        }
        previous = Some(id);
        validate_nested_field(f)?;
    }
    Ok(())
}

pub(crate) fn validate_type(v: &Raw) -> Result<()> {
    let a = array(v)?;
    if a.is_empty() {
        return Err(Error::InvalidSchema);
    }
    match int_u64(&a[0])? {
        0 => {
            if a.len() != 2
                || !matches!(
                    text(&a[1])?,
                    "Bool"
                        | "Int"
                        | "Float"
                        | "Decimal"
                        | "Str"
                        | "Blob"
                        | "Uuid"
                        | "Date"
                        | "Instant"
                        | "LocalDateTime"
                        | "TimeOfDay"
                        | "Duration"
                        | "TimeZone"
                        | "ZonedDateTime"
                        | "Unit"
                )
            {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
        }
        3 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            for x in array(&a[1])? {
                validate_type(x)?;
            }
        }
        4 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let mut last = "";
            for x in array(&a[1])? {
                let p = array(x)?;
                if p.len() != 2 {
                    return Err(Error::InvalidSchema);
                }
                let n = text(&p[0])?;
                if n.nfc().ne(n.chars()) || n <= last {
                    return Err(Error::InvalidSchema);
                }
                last = n;
                validate_type(&p[1])?;
            }
        }
        5 | 8 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
        }
        6 => {
            if a.len() != 4 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
            let _ = uuid_array(&a[2])?;
            for x in array(&a[3])? {
                validate_type(x)?;
            }
        }
        7 => {
            if a.len() != 3 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
            let _ = uuid_array(&a[2])?;
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}

/// Validates the portable portion of a value against its exact closed type
/// node. Nominal and table semantics are deliberately rejected here because a
/// standalone node cannot prove membership without the complete descriptor.
pub(crate) fn validate_value_as_type(value: &Raw, ty: &Raw) -> Result<()> {
    let node = array(ty)?;
    match int_u64(&node[0])? {
        0 => match text(&node[1])? {
            "Bool" if matches!(value, Raw::Bool(_)) => Ok(()),
            "Int" if matches!(value, Raw::Int(_)) => Ok(()),
            "Float" if matches!(value, Raw::Float(_)) => Ok(()),
            "Decimal" if matches!(value, Raw::Tag(DECIMAL_TAG, _)) => Ok(()),
            "Str" if matches!(value, Raw::Text(_)) => Ok(()),
            "Blob" if matches!(value, Raw::Bytes(_)) => Ok(()),
            "Uuid" if matches!(value, Raw::Tag(UUID_TAG, _)) => Ok(()),
            "Unit" if matches!(value, Raw::Tag(UNIT_TAG, _)) => Ok(()),
            "Date" if matches!(value, Raw::Tag(DATE_TAG, _)) => Ok(()),
            "Instant" if matches!(value, Raw::Tag(INSTANT_TAG, _)) => Ok(()),
            "LocalDateTime" if matches!(value, Raw::Tag(LOCAL_DATE_TIME_TAG, _)) => Ok(()),
            "TimeOfDay" if matches!(value, Raw::Tag(TIME_OF_DAY_TAG, _)) => Ok(()),
            "Duration" if matches!(value, Raw::Tag(DURATION_TAG, _)) => Ok(()),
            "TimeZone" if matches!(value, Raw::Tag(TIME_ZONE_TAG, _)) => Ok(()),
            "ZonedDateTime" if matches!(value, Raw::Tag(ZONED_DATE_TIME_TAG, _)) => Ok(()),
            _ => Err(Error::InvalidValue),
        },
        1 => {
            for item in array(value)? {
                validate_value_as_type(item, &node[1])?;
            }
            Ok(())
        }
        2 => {
            let Raw::Tag(OPTION_TAG, option) = value else {
                return Err(Error::InvalidValue);
            };
            let option = array(option)?;
            if option.len() == 2 {
                validate_value_as_type(&option[1], &node[1])?;
            }
            Ok(())
        }
        3 => {
            let types = array(&node[1])?;
            if types.is_empty() {
                return if matches!(value, Raw::Tag(UNIT_TAG, _)) {
                    Ok(())
                } else {
                    Err(Error::InvalidValue)
                };
            }
            let Raw::Tag(TUPLE_TAG, tuple) = value else {
                return Err(Error::InvalidValue);
            };
            let tuple = array(tuple)?;
            if tuple.len() != types.len() {
                return Err(Error::InvalidValue);
            }
            for (v, t) in tuple.iter().zip(types) {
                validate_value_as_type(v, t)?;
            }
            Ok(())
        }
        4 => {
            let fields = array(&node[1])?;
            let Raw::Tag(RECORD_TAG, record) = value else {
                return Err(Error::InvalidValue);
            };
            let record = array(record)?;
            if record.len() != 2 {
                return Err(Error::InvalidValue);
            }
            if !matches!(record[0], Raw::Null) {
                return Err(Error::InvalidValue);
            }
            let values = array(&record[1])?;
            if values.len() != fields.len() {
                return Err(Error::InvalidValue);
            }
            for (field, entry) in fields.iter().zip(values) {
                let spec = array(field)?;
                let entry = array(entry)?;
                if entry[0] != spec[0] {
                    return Err(Error::InvalidValue);
                }
                validate_value_as_type(&entry[1], &spec[1])?;
            }
            Ok(())
        }
        7 => {
            let Raw::Tag(QUANTITY_TAG, quantity) = value else {
                return Err(Error::InvalidValue);
            };
            let quantity = array(quantity)?;
            if quantity.len() != 2 || quantity[1] != node[2] {
                return Err(Error::InvalidValue);
            }
            validate_value_as_type(&quantity[0], &node[1])
        }
        8 => {
            uuid_array(&node[1])?;
            let Raw::Tag(MONEY_TAG, money) = value else {
                return Err(Error::InvalidValue);
            };
            let money = array(money)?;
            if money.len() != 2 || !matches!(money[0], Raw::Tag(DECIMAL_TAG, _)) {
                return Err(Error::InvalidValue);
            };
            if uuid_array(&money[1])? != uuid_array(&node[1])? {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        9 => {
            let Raw::Tag(RANGE_TAG, range) = value else {
                return Err(Error::InvalidValue);
            };
            let range = array(range)?;
            for endpoint in &range[..2] {
                let Raw::Tag(OPTION_TAG, option) = endpoint else {
                    return Err(Error::InvalidValue);
                };
                let option = array(option)?;
                if option.len() == 2 {
                    validate_value_as_type(&option[1], &node[1])?;
                }
            }
            Ok(())
        }
        5 | 6 => Err(Error::InvalidSchema),
        _ => Err(Error::InvalidSchema),
    }
}
