//! Project-context admission for editable loose rows.
//!
//! This boundary deliberately receives a declared table owner and schema.  A
//! row source unit alone has neither, so it cannot prove path-key or field
//! conformance.

use crate::{ProjectUnit, SourceUnit};
use num_bigint::BigInt;
use orna_evaluator_v1::{Limits, evaluate_parsed};
use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, SafeText, Value};
use orna_semantic_v1::{Analysis, Namespace, TableSchema, Type};
use orna_syntax_v1::{Expr, parse_row};
use orna_value_v1::{Raw, path_decode_key_components};
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

/// Reject resource-invalid project input before semantic/module or row parsing.
pub fn preflight_project_rows(project: &ProjectUnit, limits: Limits) -> RowResult<()> {
    let units = project
        .modules
        .len()
        .checked_add(project.loose_rows.len())
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
    for unit in project.modules.iter().chain(&project.loose_rows) {
        limits.check_source(&unit.source).map_err(|_| {
            row_error(
                "ORNA-EVAL-LIMIT",
                "row admission input exceeds configured resource limits",
            )
        })?;
    }
    Ok(())
}

pub fn admit_project_rows(
    project: &ProjectUnit,
    analysis: &Analysis,
    limits: Limits,
) -> RowResult<Vec<AdmittedLooseRow>> {
    preflight_project_rows(project, limits)?;
    project
        .loose_rows
        .iter()
        .map(|row| admit_project_row(project, analysis, row, limits))
        .collect()
}

fn admit_project_row(
    project: &ProjectUnit,
    analysis: &Analysis,
    row: &SourceUnit,
    limits: Limits,
) -> RowResult<AdmittedLooseRow> {
    let (owner, schema, encoded_key) = owner_and_path(project, analysis, row)?;
    admit_row(owner, schema, encoded_key, row, limits)
}

fn owner_and_path<'a>(
    project: &ProjectUnit,
    analysis: &'a Analysis,
    row: &SourceUnit,
) -> RowResult<(TableOwner, &'a TableSchema, Vec<String>)> {
    let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
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
    row: &SourceUnit,
    limits: Limits,
) -> RowResult<AdmittedLooseRow> {
    let admission = schema.admission.as_ref().ok_or_else(|| {
        row_error(
            "ORNA-CONFORMANCE-ROW-SCHEMA",
            "table declaration lacks row admission metadata",
        )
    })?;
    let key_text = path_decode_key_components(&encoded_key).map_err(|_| {
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

    let parsed = parse_row(&row.source);
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
    let key_names = admission
        .keys
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let mut supplied = BTreeSet::new();
    for field in fields {
        if !supplied.insert(field.name.as_str()) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-DUPLICATE",
                "loose row body repeats a field",
            ));
        }
        if key_names.contains(field.name.as_str()) {
            return Err(row_error(
                "E3004",
                "loose row body must not repeat path key",
            ));
        }
        if admission.computed.contains(&field.name) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-COMPUTED",
                "loose row body cannot supply a computed field",
            ));
        }
        if !schema.fields.contains_key(&field.name) {
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
        if !matches_value(value.raw(), expected, expression) {
            return Err(row_error(
                "ORNA-CONFORMANCE-ROW-TYPE",
                "loose row body field has an incompatible type",
            ));
        }
        body.insert(name.clone(), value);
    }
    Ok(AdmittedLooseRow { owner, key, body })
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

fn matches_value(value: &Raw, ty: &Type, expression: &Expr) -> bool {
    match ty {
        Type::Text => matches!(value, Raw::Text(_)),
        Type::Int => matches!(value, Raw::Int(_)),
        Type::Bool => matches!(value, Raw::Bool(_)),
        Type::Decimal => matches!(value, Raw::Tag(60000, _)),
        Type::List(inner) => match (value, expression) {
            (Raw::Array(values), Expr::List { elements, .. }) if values.len() == elements.len() => {
                values
                    .iter()
                    .zip(elements)
                    .all(|(value, expression)| matches_value(value, inner, expression))
            }
            _ => false,
        },
        Type::Tuple(types) => match (value, expression) {
            (Raw::Array(values), Expr::Tuple { elements, .. })
                if values.len() == types.len() && elements.len() == types.len() =>
            {
                values
                    .iter()
                    .zip(types)
                    .zip(elements)
                    .all(|((value, ty), expression)| matches_value(value, ty, expression))
            }
            _ => false,
        },
        Type::Record(types) => match (value, expression) {
            (Raw::Map(values), Expr::Record { fields, .. }) if values.len() == types.len() => {
                types.iter().all(|(name, ty)| {
                    let value = values.iter().find_map(|(key, value)| match key {
                        Raw::Text(key) if key == name => Some(value),
                        _ => None,
                    });
                    let expression = fields
                        .iter()
                        .find(|field| field.name == *name)
                        .map(|field| &field.value);
                    matches!((value, expression), (Some(value), Some(expression))
                        if matches_value(value, ty, expression))
                })
            }
            _ => false,
        },
        // Source evaluation represents optional absence as bare `null`; its
        // OVB option wrapper is introduced only by a type-directed codec.
        Type::Optional(inner) => {
            matches!(value, Raw::Null) || matches_value(value, inner, expression)
        }
        _ => false,
    }
}

fn supports_field_type(ty: &Type) -> bool {
    match ty {
        Type::Text | Type::Int | Type::Bool | Type::Decimal => true,
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
