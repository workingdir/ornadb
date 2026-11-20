//! Project-context admission for editable loose rows.
//!
//! This boundary deliberately receives a declared table owner and schema.  A
//! row source unit alone has neither, so it cannot prove path-key or field
//! conformance.

use crate::{Limits, evaluate_parsed};
use num_bigint::BigInt;
use orna_check::{Analysis, Namespace, TableAdmission, TableSchema, Type};
use orna_syntax::parse_row;
use orna_syntax_ast::Expr;
use orna_value::{Diagnostic, DiagnosticSeverity, SafeText, Value};
use orna_value::{Raw, path_decode_key_components};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableOwner {
    pub namespace: Namespace,
    pub table: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedLooseRow {
    pub owner: TableOwner,
    pub key: Vec<Value>,
    /// Evaluated stored body only. Omitted defaults and computed selectors are
    /// intentionally not materialised by this schema-only slice.
    pub body: BTreeMap<String, Value>,
}

pub type RowResult<T> = Result<T, Box<Diagnostic>>;

/// Snapshot identity supplied by the caller for stored-reference admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowAdmissionIdentity {
    pub database_id: [u8; 16],
    /// Tables and keys present in the caller's snapshot.
    pub tables: BTreeMap<[u8; 16], (TableOwner, Vec<Vec<Value>>)>,
}

/// One loose-row source unit: its logical source identifier and body text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowSource<'a> {
    pub source_id: &'a str,
    pub source: &'a str,
}

/// Reject resource-invalid row input before semantic/module or row parsing.
pub fn preflight_row_sources(
    module_sources: &[&str],
    loose_rows: &[RowSource<'_>],
    limits: Limits,
) -> RowResult<()> {
    let units = module_sources
        .len()
        .checked_add(loose_rows.len())
        .ok_or_else(|| {
            row_error(
                "ORNA-EVAL-LIMIT",
                "row admission input exceeds configured resource limits",
            )
        })?;
    limits.check_items(units).map_err(|_| {
        row_error(
            "ORNA-EVAL-LIMIT",
            "row admission input exceeds configured resource limits",
        )
    })?;
    for source in module_sources
        .iter()
        .copied()
        .chain(loose_rows.iter().map(|row| row.source))
    {
        limits.check_source(source).map_err(|_| {
            row_error(
                "ORNA-EVAL-LIMIT",
                "row admission input exceeds configured resource limits",
            )
        })?;
    }
    Ok(())
}

/// Admit loose rows against the declared table schemas of one project.
///
/// Each row's `source_id` must lie under `project_id`; the table owner is the
/// unique declared table whose namespace and name prefix the remaining path.
/// Stored references are not resolved here: the caller resolves them with
/// [`resolve_stored_references`] once every table's admitted keys are known.
pub fn admit_row_sources(
    project_id: &str,
    module_sources: &[&str],
    analysis: &Analysis,
    loose_rows: &[RowSource<'_>],
    limits: Limits,
) -> RowResult<Vec<AdmittedLooseRow>> {
    preflight_row_sources(module_sources, loose_rows, limits)?;
    loose_rows
        .iter()
        .map(|row| admit_row_source(project_id, analysis, *row, limits))
        .collect()
}

/// Resolves every stored table reference in admitted rows against `identity`.
///
/// Run after all tables are admitted, so the identity can carry the admitted
/// keys of each target table.
pub fn resolve_stored_references(
    rows: &[AdmittedLooseRow],
    identity: &RowAdmissionIdentity,
) -> RowResult<()> {
    for row in rows {
        for value in row.body.values() {
            validate_stored_references(value.raw(), identity)?;
        }
    }
    Ok(())
}

fn admit_row_source(
    project_id: &str,
    analysis: &Analysis,
    row: RowSource<'_>,
    limits: Limits,
) -> RowResult<AdmittedLooseRow> {
    let (owner, schema, encoded_key) = owner_and_path(project_id, analysis, row)?;
    admit_row(owner, schema, encoded_key, row.source, limits)
}

fn owner_and_path<'a>(
    project_id: &str,
    analysis: &'a Analysis,
    row: RowSource<'_>,
) -> RowResult<(TableOwner, &'a TableSchema, Vec<String>)> {
    let prefix = format!("{}/", project_id.trim_end_matches('/'));
    let relative = row.source_id.strip_prefix(&prefix).ok_or_else(|| {
        row_error(
            "ORNA-CONFORMANCE-ROW-OWNER",
            "loose row has no unique declared table owner",
        )
    })?;
    let components = relative.split('/').map(str::to_owned).collect::<Vec<_>>();
    if components.iter().any(String::is_empty) {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-OWNER",
            "loose row has no unique declared table owner",
        ));
    }
    let mut candidates = Vec::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            let Some(schema) = &symbol.table_schema else {
                continue;
            };
            let mut root = namespace.0.clone();
            root.push(name.clone());
            if components.starts_with(&root) && components.len() > root.len() {
                candidates.push((
                    TableOwner {
                        namespace: namespace.clone(),
                        table: name.clone(),
                    },
                    schema,
                    components[root.len()..].to_vec(),
                ));
            }
        }
    }
    if candidates.len() != 1 {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-OWNER",
            "loose row has no unique declared table owner",
        ));
    }
    Ok(candidates.pop().expect("one owner candidate"))
}

fn admit_row(
    owner: TableOwner,
    schema: &TableSchema,
    encoded_key: Vec<String>,
    source: &str,
    limits: Limits,
) -> RowResult<AdmittedLooseRow> {
    let (key, admission) = admitted_key(&encoded_key, schema)?;

    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-PARSE",
            "loose row body is not a record",
        ));
    }
    let Expr::Record { fields, .. } = &parsed.value else {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-PARSE",
            "loose row body is not a record",
        ));
    };
    check_field_names(
        schema,
        admission,
        fields.iter().map(|field| field.name.as_str()),
    )?;
    let expressions = fields
        .iter()
        .map(|field| (field.name.as_str(), &field.value))
        .collect::<BTreeMap<_, _>>();

    let value = evaluate_parsed(&parsed.value, &BTreeMap::new(), limits).map_err(|error| {
        if error.diagnostic().code() == "ORNA-EVAL-LIMIT" {
            return Box::new(error.diagnostic().clone().redacted());
        }
        row_error(
            "ORNA-CONFORMANCE-ROW-EVALUATION",
            "loose row body could not be evaluated",
        )
    })?;
    let Raw::Map(fields) = value.raw() else {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-PARSE",
            "loose row body is not a record",
        ));
    };
    let body = admitted_body(schema, &expressions, fields)?;
    Ok(AdmittedLooseRow { owner, key, body })
}

/// Decodes the declared primary key from the loose row path, rejecting paths
/// that do not match the key arity or key types.
fn admitted_key<'a>(
    encoded_key: &[String],
    schema: &'a TableSchema,
) -> RowResult<(Vec<Value>, &'a TableAdmission)> {
    let admission = schema.admission.as_ref().ok_or_else(|| {
        row_error(
            "ORNA-CONFORMANCE-ROW-SCHEMA",
            "table declaration lacks row admission metadata",
        )
    })?;
    let key_text = path_decode_key_components(encoded_key).map_err(|_| {
        row_error(
            "ORNA-CONFORMANCE-ROW-PATH",
            "loose row path does not decode to the declared key schema",
        )
    })?;
    if key_text.len() != admission.keys.len() {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-PATH",
            "loose row path does not decode to the declared key schema",
        ));
    }
    let key = admission
        .keys
        .iter()
        .zip(key_text)
        .map(|((_, ty), text)| decode_key(&text, ty))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((key, admission))
}

/// Resolves the declared schema of one table owner in the analysed project.
pub fn declared_table<'a>(
    analysis: &'a Analysis,
    owner: &TableOwner,
) -> RowResult<&'a TableSchema> {
    analysis
        .modules
        .get(&owner.namespace)
        .and_then(|module| module.symbols.get(&owner.table))
        .and_then(|symbol| symbol.table_schema.as_ref())
        .ok_or_else(|| {
            row_error(
                "ORNA-CONFORMANCE-ROW-OWNER",
                "stored row has no declared table owner",
            )
        })
}

/// Resolves the declared table owner whose namespace and name are exactly the
/// `/`-separated stored table `path`.
pub fn table_owner_for_path(analysis: &Analysis, path: &str) -> RowResult<TableOwner> {
    let components = path.split('/').map(str::to_owned).collect::<Vec<_>>();
    let mut owners = Vec::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            if symbol.table_schema.is_none() {
                continue;
            }
            let mut root = namespace.0.clone();
            root.push(name.clone());
            if root == components {
                owners.push(TableOwner {
                    namespace: namespace.clone(),
                    table: name.clone(),
                });
            }
        }
    }
    if owners.len() != 1 {
        return Err(row_error(
            "ORNA-CONFORMANCE-ROW-OWNER",
            "stored row has no declared table owner",
        ));
    }
    Ok(owners.pop().expect("one owner candidate"))
}

/// Admits one stored compact-table row against its declared table owner.
///
/// `key` holds one decoded value per declared key column and `fields` holds
/// stored values under their declared field names. Field-name, computed and
/// type rules are the same as for loose rows.
pub fn admit_stored_row(
    analysis: &Analysis,
    owner: &TableOwner,
    key: Vec<Value>,
    fields: &[(String, Raw)],
) -> RowResult<AdmittedLooseRow> {
    let schema = declared_table(analysis, owner)?;
    let admission = schema.admission.as_ref().ok_or_else(|| {
        row_error(
            "ORNA-CONFORMANCE-ROW-SCHEMA",
            "table declaration lacks row admission metadata",
        )
    })?;
    let key_mismatch = || {
        row_error(
            "ORNA-CONFORMANCE-ROW-PATH",
            "stored row key does not match the declared key schema",
        )
    };
    if key.len() != admission.keys.len() {
        return Err(key_mismatch());
    }
    for (value, (_, ty)) in key.iter().zip(&admission.keys) {
        if !matches!(ty, Type::Text | Type::Int | Type::Bool | Type::Date) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE",
                "row admission does not support this key type",
            ));
        }
        if !matches_value(value.raw(), ty, None) {
            return Err(key_mismatch());
        }
    }
    check_field_names(
        schema,
        admission,
        fields.iter().map(|(name, _)| name.as_str()),
    )?;
    let body = admitted_stored_body(schema, fields)?;
    Ok(AdmittedLooseRow {
        owner: owner.clone(),
        key,
        body,
    })
}

/// Checks supplied field names against the table's admission rules: no
/// duplicates, no path keys, no computed fields, only declared fields, and
/// every required stored field present.
fn check_field_names<'a>(
    schema: &TableSchema,
    admission: &TableAdmission,
    names: impl IntoIterator<Item = &'a str>,
) -> RowResult<()> {
    let key_names = admission
        .keys
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let mut supplied = BTreeSet::new();
    for name in names {
        if !supplied.insert(name) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-DUPLICATE",
                "loose row body repeats a field",
            ));
        }
        if key_names.contains(name) {
            return Err(row_error(
                "E3004",
                "loose row body must not repeat path key",
            ));
        }
        if admission.computed.contains(name) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-COMPUTED",
                "loose row body cannot supply a computed field",
            ));
        }
        if !schema.fields.contains_key(name) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-UNKNOWN",
                "loose row body contains an unknown field",
            ));
        }
    }
    for required in &admission.required {
        if !key_names.contains(required.as_str()) && !supplied.contains(required.as_str()) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-MISSING",
                "loose row body omits a required stored field",
            ));
        }
    }
    Ok(())
}

/// Converts stored field values into typed body values, rejecting unsupported
/// or mismatched fields.
fn admitted_stored_body(
    schema: &TableSchema,
    fields: &[(String, Raw)],
) -> RowResult<BTreeMap<String, Value>> {
    let mut body = BTreeMap::new();
    for (name, raw) in fields {
        let expected = schema.fields.get(name).ok_or_else(|| {
            row_error(
                "ORNA-CONFORMANCE-ROW-UNKNOWN",
                "stored row contains an unknown field",
            )
        })?;
        if !supports_field_type(expected) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE",
                "row admission does not support this field type",
            ));
        }
        if !matches_value(raw, expected, None) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-TYPE",
                "stored row field has an incompatible type",
            ));
        }
        let value = Value::new(raw.clone()).map_err(|_| {
            row_error(
                "ORNA-CONFORMANCE-ROW-EVALUATION",
                "stored row field could not be admitted",
            )
        })?;
        body.insert(name.clone(), value);
    }
    Ok(body)
}

/// Converts the evaluated row map into typed stored fields, rejecting unknown,
/// unsupported or mismatched fields.
fn admitted_body(
    schema: &TableSchema,
    expressions: &BTreeMap<&str, &Expr>,
    fields: &[(Raw, Raw)],
) -> RowResult<BTreeMap<String, Value>> {
    let mut body = BTreeMap::new();
    for (name, value) in fields {
        let Raw::Text(name) = name else {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-EVALUATION",
                "loose row body is not a record",
            ));
        };
        let expected = schema.fields.get(name).ok_or_else(|| {
            row_error(
                "ORNA-CONFORMANCE-ROW-UNKNOWN",
                "loose row body contains an unknown field",
            )
        })?;
        let expression = expressions.get(name.as_str()).ok_or_else(|| {
            row_error(
                "ORNA-CONFORMANCE-ROW-EVALUATION",
                "loose row body could not be evaluated",
            )
        })?;
        let value = Value::new(value.clone()).map_err(|_| {
            row_error(
                "ORNA-CONFORMANCE-ROW-EVALUATION",
                "loose row body could not be evaluated",
            )
        })?;
        if !supports_field_type(expected) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE",
                "row admission does not support this field type",
            ));
        }
        if !matches_value(value.raw(), expected, Some(*expression)) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-TYPE",
                "loose row body field has an incompatible type",
            ));
        }
        body.insert(name.clone(), value);
    }
    Ok(body)
}

fn validate_stored_references(raw: &Raw, identity: &RowAdmissionIdentity) -> RowResult<()> {
    match raw {
        Raw::Tag(60021, value) => {
            let Raw::Array(parts) = value.as_ref() else {
                return Err(stored_reference_error());
            };
            let [database, table, key] = parts.as_slice() else {
                return Err(stored_reference_error());
            };
            if raw_uuid(database) != Some(identity.database_id) {
                return Err(stored_reference_error());
            }
            let Some(table_id) = raw_uuid(table) else {
                return Err(stored_reference_error());
            };
            let Some((_, target_keys)) = identity.tables.get(&table_id) else {
                return Err(stored_reference_error());
            };
            let exists = target_keys
                .iter()
                .any(|target_key| match target_key.as_slice() {
                    [only] => only.raw() == key,
                    [_, _, ..] => {
                        let Raw::Tag(60015, tuple) = key else {
                            return false;
                        };
                        let Raw::Array(components) = tuple.as_ref() else {
                            return false;
                        };
                        target_key.iter().map(Value::raw).eq(components.iter())
                    }
                    [] => false,
                });
            if !exists {
                return Err(stored_reference_error());
            }
        }
        Raw::Array(values) => {
            for value in values {
                validate_stored_references(value, identity)?;
            }
        }
        Raw::Map(entries) => {
            for (key, value) in entries {
                validate_stored_references(key, identity)?;
                validate_stored_references(value, identity)?;
            }
        }
        Raw::Tag(_, value) => validate_stored_references(value, identity)?,
        _ => {}
    }
    Ok(())
}

fn raw_uuid(raw: &Raw) -> Option<[u8; 16]> {
    let Raw::Tag(37, bytes) = raw else {
        return None;
    };
    let Raw::Bytes(bytes) = bytes.as_ref() else {
        return None;
    };
    bytes.as_slice().try_into().ok()
}

fn stored_reference_error() -> Box<Diagnostic> {
    row_error(
        "ORNA-CONFORMANCE-ROW-REFERENCE",
        "stored table reference target does not exist in this snapshot",
    )
}

fn decode_key(text: &str, ty: &Type) -> RowResult<Value> {
    let raw = match ty {
        Type::Text => Raw::Text(text.into()),
        Type::Int => {
            let value = text.parse::<BigInt>().map_err(|_| {
                row_error(
                    "ORNA-CONFORMANCE-ROW-PATH",
                    "loose row path does not decode to the declared key schema",
                )
            })?;
            if value.to_string() != text {
                return Err(row_error(
                    "ORNA-CONFORMANCE-ROW-PATH",
                    "loose row path does not decode to the declared key schema",
                ));
            }
            Raw::Int(value)
        }
        Type::Bool => match text {
            "true" => Raw::Bool(true),
            "false" => Raw::Bool(false),
            _ => {
                return Err(row_error(
                    "ORNA-CONFORMANCE-ROW-PATH",
                    "loose row path does not decode to the declared key schema",
                ));
            }
        },
        _ => {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE",
                "row admission does not support this key type",
            ));
        }
    };
    Value::new(raw).map_err(|_| {
        row_error(
            "ORNA-CONFORMANCE-ROW-PATH",
            "loose row path does not decode to the declared key schema",
        )
    })
}

/// Checks a raw value against a declared field type.
///
/// Loose rows pass their source expression, so list, tuple, and record shape is
/// read from the source. Stored rows pass `None` and are checked from the stored
/// shape alone.
fn matches_value(value: &Raw, ty: &Type, expression: Option<&Expr>) -> bool {
    match ty {
        Type::Text => matches!(value, Raw::Text(_)),
        Type::Int => matches!(value, Raw::Int(_)),
        Type::Bool => matches!(value, Raw::Bool(_)),
        Type::Decimal => matches!(value, Raw::Tag(60000, _)),
        // Spec 29-formats: a Date is Tag 60001 over canonical text. Only stored
        // keys reach this arm, and `Value` construction already validated them.
        Type::Date => matches!(value, Raw::Tag(60001, _)),
        Type::List(inner) => match (value, expression) {
            (Raw::Array(values), Some(Expr::List { elements, .. }))
                if values.len() == elements.len() =>
            {
                values
                    .iter()
                    .zip(elements)
                    .all(|(value, expression)| matches_value(value, inner, Some(expression)))
            }
            (Raw::Array(values), None) => {
                values.iter().all(|value| matches_value(value, inner, None))
            }
            _ => false,
        },
        // Spec 29-formats: tuples are Tag(60015, Array).
        Type::Tuple(types) => {
            let Raw::Tag(60015, tuple) = value else {
                return false;
            };
            let Raw::Array(values) = tuple.as_ref() else {
                return false;
            };
            if values.len() != types.len() {
                return false;
            }
            match expression {
                None => values
                    .iter()
                    .zip(types)
                    .all(|(value, ty)| matches_value(value, ty, None)),
                Some(Expr::Tuple { elements, .. }) if elements.len() == types.len() => values
                    .iter()
                    .zip(types)
                    .zip(elements)
                    .all(|((value, ty), expression)| matches_value(value, ty, Some(expression))),
                Some(_) => false,
            }
        }
        Type::Record(types) => {
            let Raw::Map(values) = value else {
                return false;
            };
            if values.len() != types.len() {
                return false;
            }
            let fields = match expression {
                None => None,
                Some(Expr::Record { fields, .. }) => Some(fields),
                Some(_) => return false,
            };
            types.iter().all(|(name, ty)| {
                let Some(value) = values.iter().find_map(|(key, value)| match key {
                    Raw::Text(key) if key == name => Some(value),
                    _ => None,
                }) else {
                    return false;
                };
                match fields {
                    None => matches_value(value, ty, None),
                    Some(fields) => fields
                        .iter()
                        .find(|field| field.name == *name)
                        .is_some_and(|field| matches_value(value, ty, Some(&field.value))),
                }
            })
        }
        // Source evaluation represents optional absence as bare `null`; its
        // OVB option wrapper is introduced only by a type-directed codec.
        Type::Optional(inner) => {
            matches!(value, Raw::Null) || matches_value(value, inner, expression)
        }
        // A Blob field admits a blob reference: its stored text, or a record that
        // carries that text and the blob's length. A stored compact blob slot
        // holds its payload bytes, which loose source rows cannot express.
        Type::Named(name) if name == "Blob" => match value {
            Raw::Text(_) => true,
            Raw::Bytes(_) => expression.is_none(),
            Raw::Map(entries) => {
                entries.len() == 2
                    && entries.iter().any(|(key, item)| {
                        matches!((key, item), (Raw::Text(name), Raw::Text(_)) if name == "blob")
                    })
                    && entries.iter().any(|(key, item)| {
                        matches!((key, item), (Raw::Text(name), Raw::Int(_)) if name == "length")
                    })
            }
            _ => false,
        },
        _ => false,
    }
}

fn supports_field_type(ty: &Type) -> bool {
    match ty {
        Type::Text | Type::Int | Type::Bool | Type::Decimal => true,
        Type::Named(name) if name == "Blob" => true,
        Type::List(inner) | Type::Optional(inner) => supports_field_type(inner),
        Type::Tuple(types) => types.iter().all(supports_field_type),
        Type::Record(fields) => fields.values().all(supports_field_type),
        _ => false,
    }
}

fn row_error(code: &str, message: &str) -> Box<Diagnostic> {
    Box::new(
        Diagnostic::new(
            SafeText::new(code).expect("static row admission code"),
            DiagnosticSeverity::Error,
            SafeText::new(message).expect("static row admission message"),
        )
        .expect("static row admission diagnostic")
        .redacted(),
    )
}

#[cfg(test)]
mod stored_reference_tests {
    use super::*;

    fn uuid(id: [u8; 16]) -> Raw {
        Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())))
    }

    fn reference(database: [u8; 16], table: [u8; 16], key: Raw) -> Raw {
        Raw::Tag(
            60021,
            Box::new(Raw::Array(vec![uuid(database), uuid(table), key])),
        )
    }

    #[test]
    fn stored_reference_requires_database_table_and_key_in_snapshot() {
        let owner = TableOwner {
            namespace: Namespace(Vec::new()),
            table: "Target".into(),
        };
        let key = Value::new(Raw::Int(7.into())).expect("canonical key");
        let identity = RowAdmissionIdentity {
            database_id: [1; 16],
            tables: BTreeMap::from([([8; 16], (owner, vec![vec![key]]))]),
        };

        let valid = reference([1; 16], [8; 16], Raw::Int(7.into()));
        assert!(validate_stored_references(&valid, &identity).is_ok());
        let missing_key = reference([1; 16], [8; 16], Raw::Int(9.into()));
        assert!(validate_stored_references(&missing_key, &identity).is_err());
        let wrong_database = reference([2; 16], [8; 16], Raw::Int(7.into()));
        assert!(validate_stored_references(&wrong_database, &identity).is_err());
        let missing_table = reference([1; 16], [9; 16], Raw::Int(7.into()));
        assert!(validate_stored_references(&missing_table, &identity).is_err());
    }

    #[test]
    fn tuple_field_admits_only_the_tag_60015_array_encoding() {
        // Spec 29-formats: tuples are Tag(60015, Array). A bare array in a tuple column is
        // not that encoding and must be refused (matches_value false => ORNA-CONFORMANCE-ROW-TYPE).
        let parsed = parse_row("{ coordinates: (51, \"north\") }");
        let Expr::Record { fields, .. } = &parsed.value else {
            panic!("row fixture must parse as a record");
        };
        let expression = &fields[0].value;
        let ty = Type::Tuple(vec![Type::Int, Type::Text]);
        let bare = Raw::Array(vec![Raw::Int(51.into()), Raw::Text("north".into())]);
        let tagged = Raw::Tag(60015, Box::new(bare.clone()));

        assert!(matches_value(&tagged, &ty, Some(expression)));
        assert!(
            !matches_value(&bare, &ty, Some(expression)),
            "a bare array in a tuple column must not admit"
        );
    }

    #[test]
    fn blob_field_admits_only_blob_reference_representations() {
        let parsed = parse_row("{ photo: \"blob-ref\" }");
        let Expr::Record { fields, .. } = &parsed.value else {
            panic!("row fixture must parse as a record");
        };
        let expression = &fields[0].value;
        let ty = Type::Named("Blob".to_owned());
        let record = Raw::Map(vec![
            (Raw::Text("blob".into()), Raw::Text("blob-ref".into())),
            (Raw::Text("length".into()), Raw::Int(3.into())),
        ]);
        let without_length = Raw::Map(vec![(
            Raw::Text("blob".into()),
            Raw::Text("blob-ref".into()),
        )]);
        let text_length = Raw::Map(vec![
            (Raw::Text("blob".into()), Raw::Text("blob-ref".into())),
            (Raw::Text("length".into()), Raw::Text("3".into())),
        ]);

        assert!(supports_field_type(&ty));
        assert!(matches_value(
            &Raw::Text("blob-ref".into()),
            &ty,
            Some(expression)
        ));
        assert!(matches_value(&record, &ty, Some(expression)));
        assert!(!matches_value(&Raw::Int(3.into()), &ty, Some(expression)));
        assert!(!matches_value(&without_length, &ty, Some(expression)));
        assert!(!matches_value(&text_length, &ty, Some(expression)));
    }

    #[test]
    fn stored_shapes_are_checked_without_source_expressions() {
        // Stored rows carry no source expression, so shape comes from the stored value.
        let list = Type::List(Box::new(Type::Int));
        assert!(matches_value(
            &Raw::Array(vec![Raw::Int(1.into())]),
            &list,
            None
        ));
        assert!(!matches_value(
            &Raw::Array(vec![Raw::Text("x".into())]),
            &list,
            None
        ));
        assert!(!matches_value(&Raw::Int(1.into()), &list, None));

        let tuple = Type::Tuple(vec![Type::Int, Type::Text]);
        let bare = Raw::Array(vec![Raw::Int(51.into()), Raw::Text("north".into())]);
        assert!(matches_value(
            &Raw::Tag(60015, Box::new(bare.clone())),
            &tuple,
            None
        ));
        assert!(!matches_value(&bare, &tuple, None));

        let record = Type::Record(BTreeMap::from([("n".to_owned(), Type::Int)]));
        let stored = Raw::Map(vec![(Raw::Text("n".into()), Raw::Int(2.into()))]);
        assert!(matches_value(&stored, &record, None));
        let renamed = Raw::Map(vec![(Raw::Text("m".into()), Raw::Int(2.into()))]);
        assert!(!matches_value(&renamed, &record, None));
    }
}
