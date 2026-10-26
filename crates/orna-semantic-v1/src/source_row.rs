use std::collections::BTreeMap;

use orna_syntax_v1::{Expr, LiteralKind};
use orna_value_v1::{Decimal, Raw, Value};

use super::{
    Analysis, DIAG_TYPE, DIAG_UNRESOLVED, RowUnitAdmission, RowUnitInput, SymbolKind, Type,
    admit_row_unit_with_owner_fields, analysis_named_symbol, canonical_date, canonical_instant,
    diag, row_key_component_types,
};
/// Source-authored row values after schema admission and canonical key lowering.
/// Blob and owner-supplied fields are absent: their owner must supply pinned
/// repository bindings or other authorized metadata projections.
///
/// Field values are data values lowered without a schema: optional absence
/// is `null`, a variant or nominal constructor keeps its source name in the
/// owner slot of a record value, and a stored reference is its target key.
/// The table's storage descriptor directs their stored encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoweredRowUnit {
    pub owner: String,
    pub canonical_key: Vec<u8>,
    pub fields: BTreeMap<String, Raw>,
}

/// Lowers one closed data expression, such as a row document field or a
/// constant insert-time default, to its schema-free data value. `None` for
/// any expression that needs evaluation: a call other than `Some(value)`, an
/// operator other than negation, an interpolated string, a range, a block or
/// a lambda.
pub fn lower_data_expression(expr: &Expr) -> Option<Raw> {
    source_row_value(expr)
}

/// Admit one project row and lower its source-authored, non-Blob values to
/// canonical value encodings. Blob expressions are evaluated by their owner
/// and must arrive separately as repository-issued bindings.
pub fn lower_row_unit(
    analysis: &Analysis,
    input: &RowUnitInput<'_>,
) -> Result<LoweredRowUnit, RowUnitAdmission> {
    lower_row_unit_with_owner_fields(analysis, input, &[])
}

/// Lowers a row whose named required stored fields are supplied by an owner
/// binding instead of source text.
pub fn lower_row_unit_with_owner_fields(
    analysis: &Analysis,
    input: &RowUnitInput<'_>,
    owner_fields: &[String],
) -> Result<LoweredRowUnit, RowUnitAdmission> {
    let mut admission = admit_row_unit_with_owner_fields(analysis, input, owner_fields);
    if !admission.is_ok() {
        return Err(admission);
    }
    let owner = admission
        .owner
        .clone()
        .expect("successful admission has an owner");
    let table_schema = analysis.modules.iter().find_map(|(namespace, module)| {
        module.symbols.iter().find_map(|(name, symbol)| {
            let table = symbol.table_schema.as_ref()?;
            let candidate_owner = if namespace.0.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", namespace.0.join("/"), name)
            };
            (candidate_owner == owner).then_some(table)
        })
    });
    let Some(table_schema) = table_schema else {
        admission
            .diagnostics
            .push(diag(DIAG_UNRESOLVED, "admitted row owner disappeared"));
        return Err(admission);
    };
    let Some(key_admission) = table_schema.admission.as_ref() else {
        admission
            .diagnostics
            .push(diag(DIAG_TYPE, "admitted row has no key schema"));
        return Err(admission);
    };
    let Some(key_types) = row_key_component_types(key_admission, analysis) else {
        admission
            .diagnostics
            .push(diag(DIAG_TYPE, "admitted row key schema is unsupported"));
        return Err(admission);
    };
    if key_types.len() != admission.key.len() {
        admission
            .diagnostics
            .push(diag(DIAG_TYPE, "admitted row key arity changed"));
        return Err(admission);
    }
    let mut key_values = Vec::with_capacity(key_types.len());
    for (ty, text) in key_types.iter().zip(&admission.key) {
        let Some(value) = source_key_value(text, ty, analysis) else {
            admission.diagnostics.push(diag(
                DIAG_TYPE,
                "admitted row key cannot be canonically encoded",
            ));
            return Err(admission);
        };
        key_values.push(value);
    }
    let key = if key_values.len() == 1 {
        key_values.pop().expect("one key value")
    } else {
        Raw::Array(key_values)
    };
    let Ok(key) = Value::new(key).and_then(|value| value.encode()) else {
        admission
            .diagnostics
            .push(diag(DIAG_TYPE, "admitted row key is not canonical"));
        return Err(admission);
    };

    let parsed = orna_syntax_v1::parse_row_with_file(input.source, input.logical_path);
    let Expr::Record { fields, .. } = parsed.value else {
        admission
            .diagnostics
            .push(diag(DIAG_TYPE, "project row unit must contain one record"));
        return Err(admission);
    };
    let mut values = BTreeMap::new();
    for field in fields {
        let Some(ty) = admission.fields.get(&field.name) else {
            continue;
        };
        if matches!(ty, Type::Named(name) if matches!(name.as_str(), "Blob" | "std.BINARY_LARGE_OBJECT"))
        {
            continue;
        }
        let Some(value) = source_row_value(&field.value) else {
            admission.diagnostics.push(diag(
                DIAG_TYPE,
                "source row field is not a canonical data expression",
            ));
            return Err(admission);
        };
        values.insert(field.name, value);
    }
    Ok(LoweredRowUnit {
        owner,
        canonical_key: key,
        fields: values,
    })
}

fn source_key_value(text: &str, ty: &Type, analysis: &Analysis) -> Option<Raw> {
    Some(match ty {
        Type::Text => Raw::Text(text.to_owned()),
        Type::Int => Raw::Int(text.parse().ok()?),
        Type::Decimal => decimal_raw(text)?,
        Type::Bool => Raw::Bool(match text {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        Type::Date => Raw::Tag(60001, Box::new(Raw::Text(text.to_owned()))),
        Type::Instant => instant_raw(text)?,
        Type::Named(name) if matches!(name.as_str(), "Uuid" | "UUID" | "std.UUID") => Raw::Tag(
            37,
            Box::new(Raw::Bytes(
                uuid::Uuid::parse_str(text).ok()?.as_bytes().to_vec(),
            )),
        ),
        Type::Named(name) => {
            let symbol = analysis_named_symbol(name, analysis)?;
            if symbol.kind != SymbolKind::Enum || !symbol.enum_variants.contains(text) {
                return None;
            }
            Raw::Text(text.to_owned())
        }
        _ => return None,
    })
}

fn source_row_value(expr: &Expr) -> Option<Raw> {
    Some(match expr {
        Expr::Group { inner, .. } => return source_row_value(inner),
        Expr::Literal { text, kind, .. } => match kind {
            LiteralKind::Null => Raw::Null,
            LiteralKind::Boolean => Raw::Bool(match text.as_str() {
                "true" => true,
                "false" => false,
                _ => return None,
            }),
            LiteralKind::Integer => Raw::Int(text.replace('_', "").parse().ok()?),
            LiteralKind::Decimal => decimal_raw(text)?,
            LiteralKind::Float => {
                let number = text
                    .strip_suffix('f')?
                    .replace('_', "")
                    .parse::<f64>()
                    .ok()?;
                if !number.is_finite() {
                    return None;
                }
                Raw::Float(number.to_bits())
            }
            LiteralKind::Date => Raw::Tag(60001, Box::new(Raw::Text(text.clone()))),
            LiteralKind::Instant => instant_raw(text)?,
            LiteralKind::String => Raw::Text(serde_json::from_str(text).ok()?),
        },
        Expr::Name { text, .. } => Raw::Text(text.clone()),
        Expr::Unary { op, rhs, .. } if op == "-" => match source_row_value(rhs)? {
            Raw::Int(value) => Raw::Int(-value),
            Raw::Float(bits) => Raw::Float((-f64::from_bits(bits)).to_bits()),
            Raw::Tag(60000, value) => {
                let Raw::Array(parts) = *value else {
                    return None;
                };
                let [Raw::Int(coefficient), exponent] = parts.as_slice() else {
                    return None;
                };
                Raw::Tag(
                    60000,
                    Box::new(Raw::Array(vec![
                        Raw::Int(-coefficient.clone()),
                        exponent.clone(),
                    ])),
                )
            }
            _ => return None,
        },
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => Raw::Array(
            elements
                .iter()
                .map(source_row_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        Expr::Record { fields, .. } => {
            let mut entries = fields
                .iter()
                .map(|field| {
                    Some((
                        Raw::Text(field.name.clone()),
                        source_row_value(&field.value)?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            entries.sort_by(|(left, _), (right, _)| {
                canonical_raw_key(left).cmp(&canonical_raw_key(right))
            });
            Raw::Map(entries)
        }
        Expr::Field { base, name, .. } => {
            let Expr::Name { text, .. } = base.as_ref() else {
                return None;
            };
            Raw::Text(format!("{text}.{name}"))
        }
        // `Some(value)`: canonical text's present optional (29-formats.md).
        Expr::Call {
            callee, arguments, ..
        } => {
            let (Expr::Name { text, .. }, [argument]) = (callee.as_ref(), arguments.as_slice())
            else {
                return None;
            };
            if text != "Some" || argument.name.is_some() {
                return None;
            }
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![
                    Raw::Int(1.into()),
                    source_row_value(&argument.value)?,
                ])),
            )
        }
        // A nominal record or variant constructor, `Point { x: 1 }` or
        // `circle { radius: 1 }`, keeps its source name in the owner slot;
        // the storage descriptor resolves it to the declared ObjectIds.
        Expr::Nominal { path, fields, .. } => {
            let name = path
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let entries = fields
                .iter()
                .map(|field| {
                    Some(Raw::Array(vec![
                        Raw::Text(field.name.clone()),
                        source_row_value(&field.value)?,
                    ]))
                })
                .collect::<Option<Vec<_>>>()?;
            Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![Raw::Text(name), Raw::Array(entries)])),
            )
        }
        _ => return None,
    })
}

fn canonical_raw_key(raw: &Raw) -> Vec<u8> {
    Value::new(raw.clone())
        .and_then(|value| value.encode())
        .unwrap_or_default()
}

fn decimal_raw(text: &str) -> Option<Raw> {
    let clean = text.replace('_', "");
    let clean = clean.strip_suffix(".decimal").unwrap_or(&clean);
    let (mantissa, explicit_exponent) = clean.split_once(['e', 'E']).unwrap_or((clean, "0"));
    let (negative, unsigned) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |value| (true, value));
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut coefficient_text = format!("{whole}{fraction}");
    while coefficient_text.len() > 1 && coefficient_text.starts_with('0') {
        coefficient_text.remove(0);
    }
    let mut coefficient = coefficient_text.parse::<num_bigint::BigInt>().ok()?;
    if negative {
        coefficient = -coefficient;
    }
    let exponent = explicit_exponent
        .parse::<i64>()
        .ok()?
        .checked_sub(fraction.len() as i64)?;
    let value = Decimal::try_new(coefficient, exponent.into()).ok()?;
    Some(Raw::Tag(
        60000,
        Box::new(Raw::Array(vec![
            Raw::Int(value.coefficient().clone()),
            Raw::Int(value.exponent10().clone()),
        ])),
    ))
}

fn instant_raw(text: &str) -> Option<Raw> {
    if !canonical_instant(text) {
        return None;
    }
    let body = text.strip_suffix('Z')?;
    let (date, time) = body.split_once('T')?;
    if !canonical_date(date) {
        return None;
    }
    let (clock, fraction) = time
        .split_once('.')
        .map_or((time, ""), |(clock, fraction)| (clock, fraction));
    let mut parts = clock.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some()
        || hour > 23
        || minute > 59
        || second > 59
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut nanos = fraction.to_owned();
    while nanos.len() < 9 {
        nanos.push('0');
    }
    let nanosecond = if nanos.is_empty() {
        0
    } else {
        nanos.parse::<i64>().ok()?
    };
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?;
    Some(Raw::Tag(
        60002,
        Box::new(Raw::Array(vec![
            Raw::Int(seconds.into()),
            Raw::Int(nanosecond.into()),
        ])),
    ))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
