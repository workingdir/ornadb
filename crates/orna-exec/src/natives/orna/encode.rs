//! Type-directed canonical Orna text encoding. A value is written against the type it has, in the canonical layout the
//! format specifies: two-space indentation for nonempty records, `name: value,` fields with a trailing comma, and
//! comma-separated elements. A value that does not match its type is a type error; a value or type the codec does not
//! handle is unsupported.

use std::fmt::Write;

use num_traits::Zero;
use orna_hir::{DefKind, Name, Ty, TyKind};

use crate::calendar::date_text;
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::schema::{Field, Scope, record_fields, resolve};
use crate::value::Value;

/// Appends the canonical text of `value`, written as type `ty` in `scope` and nested `depth` records deep, to `out`.
pub(super) fn encode<'a>(
    machine: &mut Machine<'_>,
    value: &Value,
    ty: Ty,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
    out: &mut String,
) -> Result<(), Stop> {
    if depth > machine.limits.max_depth {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let program = machine.program;
    let (ty, scope) = resolve(program, ty, scope)?;
    if is_unsupported_value(value) {
        return Err(unsupported());
    }
    let kind = program.tys.kind(ty).ok_or_else(unsupported)?;
    match kind {
        TyKind::Unit => match value {
            Value::Unit => out.push_str("()"),
            _ => return Err(mismatch()),
        },
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
            Value::Decimal(decimal) => out.push_str(&decimal_text(decimal)),
            _ => return Err(mismatch()),
        },
        TyKind::Float => match value {
            Value::Float(bits) => out.push_str(&float_text(f64::from_bits(*bits))?),
            _ => return Err(mismatch()),
        },
        TyKind::Str => match value {
            Value::Text(text) => escape(text, out),
            _ => return Err(mismatch()),
        },
        TyKind::Blob => match value {
            Value::Blob(bytes) => escape(&orna_value::base64_encode(bytes), out),
            _ => return Err(mismatch()),
        },
        TyKind::Uuid => match value {
            Value::Uuid(bytes) => escape(&uuid_text(bytes), out),
            _ => return Err(mismatch()),
        },
        TyKind::Date => match value {
            Value::Date(days) => out.push_str(&date_text(*days).map_err(Stop::Error)?),
            _ => return Err(mismatch()),
        },
        TyKind::Option(inner) => match value {
            Value::Option(None) | Value::Null => out.push_str("null"),
            Value::Option(Some(item)) => {
                out.push_str("Some(");
                encode(machine, item, *inner, scope, depth + 1, out)?;
                out.push(')');
            }
            _ => return Err(mismatch()),
        },
        TyKind::List(inner) => match value {
            Value::List(items) => {
                check_collection(items.len(), machine)?;
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    separate(index, out);
                    encode(machine, item, *inner, scope, depth + 1, out)?;
                }
                out.push(']');
            }
            _ => return Err(mismatch()),
        },
        TyKind::Tuple(types) => match value {
            Value::Tuple(items) if items.len() == types.len() => {
                out.push('(');
                for (index, (item, &item_ty)) in items.iter().zip(types.iter()).enumerate() {
                    separate(index, out);
                    encode(machine, item, item_ty, scope, depth + 1, out)?;
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
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
                write_record(machine, values, &fields, None, scope, depth, out)?;
            }
            _ => return Err(mismatch()),
        },
        TyKind::Adt(def, args) => match &program.defs[*def].kind {
            DefKind::Type(_) => match value {
                Value::Record(values) => {
                    let fields = record_fields(program, *def)?;
                    write_record(machine, values, &fields, Some(&args[..]), scope, depth, out)?;
                }
                _ => return Err(mismatch()),
            },
            _ => return Err(unsupported()),
        },
        _ => return Err(unsupported()),
    }
    Ok(())
}

/// Writes a record as a braced block. Every field the type declares must be present in the value, and no other field
/// may be; the fields follow `fields`, which is the record's declaration or type order.
fn write_record<'a>(
    machine: &mut Machine<'_>,
    values: &[(Name, Value)],
    fields: &[Field<'_>],
    args: Option<&'a [Ty]>,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
    out: &mut String,
) -> Result<(), Stop> {
    if values.len() != fields.len() {
        return Err(mismatch());
    }
    if fields.is_empty() {
        out.push_str("{}");
        return Ok(());
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
    out.push_str("{\n");
    for field in fields {
        let (_, value) = values
            .iter()
            .find(|(name, _)| name.as_str() == field.name.as_str())
            .ok_or_else(mismatch)?;
        push_indent(depth + 1, out);
        out.push_str(field.name.as_str());
        out.push_str(": ");
        encode(machine, value, field.ty, field_scope, depth + 1, out)?;
        out.push_str(",\n");
    }
    push_indent(depth, out);
    out.push('}');
    Ok(())
}

/// A value the codec cannot write at all, whatever type it is checked against.
fn is_unsupported_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Instant { .. }
            | Value::Duration { .. }
            | Value::Period(_)
            | Value::Digest(_)
            | Value::Money { .. }
            | Value::Closure(_)
            | Value::RowRef(_)
            | Value::Error(_)
            | Value::Enum { .. }
    )
}

/// The canonical `Decimal`: its normalised coefficient, `e`, its signed exponent, and the `.decimal` suffix. Zero is
/// `0e0.decimal`.
pub(super) fn decimal_text(value: &Decimal) -> String {
    if value.coefficient().is_zero() {
        return "0e0.decimal".to_owned();
    }
    format!("{}e{}.decimal", value.coefficient(), value.exponent10())
}

/// The canonical `Float`: 17 significant digits in lowercase scientific form with a minimal exponent and an `f` suffix,
/// or one of the three named nonfinite values.
pub(super) fn float_text(value: f64) -> Result<String, Stop> {
    if value.is_nan() {
        return Ok("Float.nan".to_owned());
    }
    if value == f64::INFINITY {
        return Ok("Float.infinity".to_owned());
    }
    if value == f64::NEG_INFINITY {
        return Ok("-Float.infinity".to_owned());
    }
    let text = format!("{value:.16e}");
    let (mantissa, exponent) = text.split_once('e').ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    let exponent = exponent
        .parse::<i32>()
        .map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))?;
    Ok(format!("{mantissa}e{exponent}f"))
}

/// The lowercase canonical 8-4-4-4-12 form of a UUID's 16 bytes.
pub(super) fn uuid_text(bytes: &[u8; 16]) -> String {
    let mut text = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        // Writing hex into a `String` cannot fail.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn check_collection(len: usize, machine: &Machine<'_>) -> Result<(), Stop> {
    if len > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(())
}

fn separate(index: usize, out: &mut String) {
    if index > 0 {
        out.push_str(", ");
    }
}

fn push_indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn mismatch() -> Stop {
    Stop::Error("ORNA-EVAL-TYPE")
}

fn unsupported() -> Stop {
    Stop::Error("ORNA-EVAL-UNSUPPORTED")
}

/// Writes a string as a quoted literal: quote and backslash escaped, newline, carriage return and tab named, a literal
/// opening brace and every other control scalar as a minimal lowercase `\u{...}` escape.
pub(super) fn escape(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '{' => out.push_str("\\u{7b}"),
            control if control.is_control() => {
                out.push_str(&format!("\\u{{{:x}}}", control as u32))
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
