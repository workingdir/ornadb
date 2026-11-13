//! Type-directed JSON encoding. A value is written against the Orna type it has: a nominal record writes its stored
//! fields in declaration order, and an anonymous record writes its fields in type order. A value that does not match
//! its type is a type error; a value or type the codec does not handle is unsupported.

use orna_hir::{DefKind, Name, Ty, TyKind};

use super::number;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::schema::{Field, Scope, record_fields, resolve};
use crate::value::Value;

/// Appends the JSON form of `value`, written as type `ty` in `scope`, to `out`.
pub(super) fn encode<'a>(
    machine: &mut Machine<'_>,
    value: &Value,
    ty: Ty,
    scope: Option<&'a Scope<'a>>,
    out: &mut String,
) -> Result<(), Stop> {
    let program = machine.program;
    let (ty, scope) = resolve(program, ty, scope)?;
    if is_unsupported_value(value) {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    }
    let kind = program
        .tys
        .kind(ty)
        .ok_or(Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
    match kind {
        TyKind::Bool => match value {
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            _ => return Err(mismatch()),
        },
        TyKind::Int => match value {
            Value::Int(integer) => out.push_str(&integer.to_string()),
            _ => return Err(mismatch()),
        },
        TyKind::Decimal => match value {
            Value::Decimal(decimal) => {
                out.push_str(&number::decimal_token(decimal, &machine.limits)?)
            }
            _ => return Err(mismatch()),
        },
        TyKind::Float => match value {
            Value::Float(bits) => {
                let float = f64::from_bits(*bits);
                if !float.is_finite() {
                    return Err(Stop::Error("ORNA-EVAL-VALUE"));
                }
                out.push_str(&float.to_string());
            }
            _ => return Err(mismatch()),
        },
        TyKind::Str => match value {
            Value::Text(text) => escape(text, out),
            _ => return Err(mismatch()),
        },
        TyKind::Option(inner) => match value {
            Value::Option(None) | Value::Null => out.push_str("null"),
            Value::Option(Some(item)) => encode(machine, item, *inner, scope, out)?,
            _ => return Err(mismatch()),
        },
        TyKind::List(inner) => match value {
            Value::List(items) => {
                check_collection(items.len(), machine)?;
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    separate(index, out);
                    encode(machine, item, *inner, scope, out)?;
                }
                out.push(']');
            }
            _ => return Err(mismatch()),
        },
        TyKind::Tuple(types) => match value {
            Value::Tuple(items) if items.len() == types.len() => {
                out.push('[');
                for (index, (item, &item_ty)) in items.iter().zip(types.iter()).enumerate() {
                    separate(index, out);
                    encode(machine, item, item_ty, scope, out)?;
                }
                out.push(']');
            }
            _ => return Err(mismatch()),
        },
        TyKind::Record(declared) => match value {
            Value::Record(values) => {
                let fields: Vec<Field<'_>> = declared
                    .iter()
                    .map(|(name, ty)| Field {
                        name,
                        ty: *ty,
                        default: None,
                    })
                    .collect();
                write_record(machine, values, &fields, None, scope, out)?;
            }
            _ => return Err(mismatch()),
        },
        TyKind::Adt(def, args) => match &program.defs[*def].kind {
            DefKind::Type(_) => match value {
                Value::Record(values) => {
                    let fields = record_fields(program, *def)?;
                    write_record(machine, values, &fields, Some(&args[..]), scope, out)?;
                }
                _ => return Err(mismatch()),
            },
            _ => return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
        },
        _ => return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
    Ok(())
}

/// Writes a record as a JSON object. Every field the type declares must be present in the value, and no other field
/// may be; the object's keys follow `fields`, which is the record's declaration or type order.
fn write_record<'a>(
    machine: &mut Machine<'_>,
    values: &[(Name, Value)],
    fields: &[Field<'_>],
    args: Option<&'a [Ty]>,
    scope: Option<&'a Scope<'a>>,
    out: &mut String,
) -> Result<(), Stop> {
    if values.len() != fields.len() {
        return Err(mismatch());
    }
    let child;
    let field_scope: Option<&Scope<'_>> = match args {
        Some(args) => {
            child = Scope {
                args,
                parent: scope,
            };
            Some(&child)
        }
        None => scope,
    };
    out.push('{');
    for (index, field) in fields.iter().enumerate() {
        let (_, value) = values
            .iter()
            .find(|(name, _)| name.as_str() == field.name.as_str())
            .ok_or_else(mismatch)?;
        separate(index, out);
        escape(field.name.as_str(), out);
        out.push(':');
        encode(machine, value, field.ty, field_scope, out)?;
    }
    out.push('}');
    Ok(())
}

/// A value the codec cannot write at all, whatever type it is checked against.
fn is_unsupported_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Unit
            | Value::Instant { .. }
            | Value::Duration { .. }
            | Value::Blob(_)
            | Value::Date(_)
            | Value::Period(_)
            | Value::Uuid(_)
            | Value::Digest(_)
            | Value::Money { .. }
            | Value::Closure(_)
            | Value::RowRef(_)
            | Value::Error(_)
            | Value::Enum { .. }
    )
}

fn check_collection(len: usize, machine: &Machine<'_>) -> Result<(), Stop> {
    if len > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(())
}

fn separate(index: usize, out: &mut String) {
    if index > 0 {
        out.push(',');
    }
}

fn mismatch() -> Stop {
    Stop::Error("ORNA-EVAL-TYPE")
}

/// Writes a string as a JSON string literal: quote and backslash escaped, the common controls named, and any other
/// control character as a lowercase `\u00xx` escape.
fn escape(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod row_reference_tests {
    use orna_value::Raw;

    use super::is_unsupported_value;
    use crate::value::Value;

    #[test]
    fn row_reference_is_refused_by_the_codec() {
        assert!(is_unsupported_value(&Value::RowRef(Raw::Null)));
    }
}
