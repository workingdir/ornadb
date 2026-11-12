//! Schema-directed decoding of canonical Orna text. The input is parsed as one expression and read against the type it
//! must produce. No expression is evaluated: only the data constructors of the canonical form are accepted, and a
//! declared field's default initializer runs only when the text omits that field. Text that does not match its type is
//! a value error; a type the codec does not handle is unsupported.

use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::Zero;
use orna_hir::{DefKind, Ty, TyKind};
use orna_syntax::unescape_string_literal;
use orna_syntax_ast::{Expr, LiteralKind, RecordField};

use super::encode::uuid_text;
use crate::calendar::parse_date_text;
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::Machine;
use crate::natives::schema::{Field, Scope, default_value, record_fields, resolve};
use crate::value::Value;

/// Decodes `expr` as a value of type `ty`, which is written in `scope`, `depth` records deep.
pub(super) fn decode<'a>(
    machine: &mut Machine<'_>,
    expr: &Expr,
    ty: Ty,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
) -> Result<Value, Stop> {
    if depth > machine.limits.max_depth {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let program = machine.program;
    let (ty, scope) = resolve(program, ty, scope)?;
    let kind = program.tys.kind(ty).ok_or_else(unsupported)?;
    match kind {
        TyKind::Unit => match expr {
            Expr::Tuple { elements, .. } if elements.is_empty() => Ok(Value::Unit),
            _ => Err(invalid()),
        },
        TyKind::Bool => boolean(expr),
        TyKind::Int => Ok(Value::Int(integer(expr)?)),
        TyKind::Decimal => Ok(Value::Decimal(decimal(expr)?)),
        TyKind::Float => float_value(expr),
        TyKind::Str => Ok(Value::Text(string_value(machine, expr)?)),
        TyKind::Blob => Ok(Value::Blob(blob(machine, expr)?)),
        TyKind::Uuid => Ok(Value::Uuid(uuid(expr)?)),
        TyKind::Date => Ok(Value::Date(date(expr)?)),
        TyKind::Option(inner) => option(machine, expr, *inner, scope, depth),
        TyKind::List(inner) => list(machine, expr, *inner, scope, depth),
        TyKind::Tuple(types) => tuple(machine, expr, types, scope, depth),
        TyKind::Record(declared) => {
            let fields: Vec<Field<'_>> = declared
                .iter()
                .map(|(name, ty)| Field {
                    name,
                    ty: *ty,
                    default: None,
                })
                .collect();
            record(machine, expr, &fields, None, scope, depth)
        }
        TyKind::Adt(def, args) => match &program.defs[*def].kind {
            DefKind::Type(_) => {
                let fields = record_fields(program, *def)?;
                record(machine, expr, &fields, Some(&args[..]), scope, depth)
            }
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// `true` or `false`.
fn boolean(expr: &Expr) -> Result<Value, Stop> {
    match expr {
        Expr::Literal {
            kind: LiteralKind::Boolean,
            text,
            ..
        } if text == "true" => Ok(Value::Bool(true)),
        Expr::Literal {
            kind: LiteralKind::Boolean,
            text,
            ..
        } if text == "false" => Ok(Value::Bool(false)),
        _ => Err(invalid()),
    }
}

/// `null` or `Some(value)`.
fn option<'a>(
    machine: &mut Machine<'_>,
    expr: &Expr,
    inner: Ty,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
) -> Result<Value, Stop> {
    match expr {
        Expr::Literal {
            kind: LiteralKind::Null,
            ..
        } => Ok(Value::Option(None)),
        Expr::Call {
            callee, arguments, ..
        } if is_name(callee, "Some") => {
            let [argument] = arguments.as_slice() else {
                return Err(invalid());
            };
            if argument.name.is_some() {
                return Err(invalid());
            }
            let value = decode(machine, &argument.value, inner, scope, depth + 1)?;
            Ok(Value::Option(Some(Rc::new(value))))
        }
        _ => Err(invalid()),
    }
}

/// A bracketed list, bounded by the collection limit.
fn list<'a>(
    machine: &mut Machine<'_>,
    expr: &Expr,
    inner: Ty,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
) -> Result<Value, Stop> {
    let Expr::List { elements, .. } = expr else {
        return Err(invalid());
    };
    if elements.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let values = elements
        .iter()
        .map(|element| decode(machine, element, inner, scope, depth + 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::List(Rc::from(values)))
}

/// A parenthesised tuple with one element per declared component. A one-element tuple is written with its comma.
fn tuple<'a>(
    machine: &mut Machine<'_>,
    expr: &Expr,
    types: &[Ty],
    scope: Option<&'a Scope<'a>>,
    depth: usize,
) -> Result<Value, Stop> {
    let Expr::Tuple { elements, .. } = expr else {
        return Err(invalid());
    };
    if elements.len() != types.len() {
        return Err(invalid());
    }
    let values = elements
        .iter()
        .zip(types.iter())
        .map(|(element, &item_ty)| decode(machine, element, item_ty, scope, depth + 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Tuple(Rc::from(values)))
}

/// A braced record. Every declared field must be present, or take its default initializer; a key the record does not
/// declare, or a repeated key, is a value error. A nominal record resolves its field types in a scope of its own
/// arguments; an anonymous record resolves them in `scope`.
fn record<'a>(
    machine: &mut Machine<'_>,
    expr: &Expr,
    declared: &[Field<'_>],
    args: Option<&'a [Ty]>,
    scope: Option<&'a Scope<'a>>,
    depth: usize,
) -> Result<Value, Stop> {
    let Expr::Record { fields, .. } = expr else {
        return Err(invalid());
    };
    check_keys(fields, declared)?;
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
    let mut values = Vec::with_capacity(declared.len());
    for field in declared {
        let found = fields
            .iter()
            .find(|candidate| candidate.name.as_str() == field.name.as_str());
        let value = match found {
            Some(found) => decode(machine, &found.value, field.ty, field_scope, depth + 1)?,
            None => match field.default {
                Some(body) => default_value(machine, body, args.unwrap_or(&[]), scope)?,
                None => return Err(invalid()),
            },
        };
        values.push((field.name.clone(), value));
    }
    values.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    Ok(Value::Record(Rc::from(values)))
}

/// Rejects a key the record does not declare and a key written twice.
fn check_keys(fields: &[RecordField], declared: &[Field<'_>]) -> Result<(), Stop> {
    for (index, field) in fields.iter().enumerate() {
        let known = declared
            .iter()
            .any(|candidate| candidate.name.as_str() == field.name.as_str());
        let repeated = fields[..index].iter().any(|prior| prior.name == field.name);
        if !known || repeated {
            return Err(invalid());
        }
    }
    Ok(())
}

/// An integer literal in minimal decimal digits, optionally under a unary minus.
pub(super) fn integer(expr: &Expr) -> Result<BigInt, Stop> {
    let (negative, inner) = signed(expr);
    let Expr::Literal {
        kind: LiteralKind::Integer,
        text,
        ..
    } = inner
    else {
        return Err(invalid());
    };
    let magnitude = minimal_digits(text).ok_or_else(invalid)?;
    apply_sign(negative, magnitude).ok_or_else(invalid)
}

/// The canonical `<coefficient>e<exponent>.decimal` form: a `Decimal` literal coefficient and exponent, read as field
/// `decimal` of that literal, under an optional unary minus on the coefficient.
pub(super) fn decimal(expr: &Expr) -> Result<Decimal, Stop> {
    let (negative, inner) = signed(expr);
    let Expr::Field { base, name, .. } = inner else {
        return Err(invalid());
    };
    let Expr::Literal {
        kind: LiteralKind::Decimal,
        text,
        ..
    } = base.as_ref()
    else {
        return Err(invalid());
    };
    if name.as_str() != "decimal" {
        return Err(invalid());
    }
    let (coefficient_text, exponent_text) = text.split_once('e').ok_or_else(invalid)?;
    let magnitude = minimal_digits(coefficient_text).ok_or_else(invalid)?;
    let coefficient = apply_sign(negative, magnitude).ok_or_else(invalid)?;
    let exponent = signed_exponent(exponent_text)?;
    Decimal::new(coefficient, exponent)
}

/// A binary64 float: a `Float` literal with its `f` suffix, `Float.nan`, or `Float.infinity` under an optional minus.
/// A finite literal that overflows to infinity is a value error.
pub(super) fn float_value(expr: &Expr) -> Result<Value, Stop> {
    let (negative, inner) = signed(expr);
    match inner {
        Expr::Literal {
            kind: LiteralKind::Float,
            text,
            ..
        } => {
            let magnitude: f64 = text
                .strip_suffix('f')
                .ok_or_else(invalid)?
                .parse::<f64>()
                .map_err(|_| invalid())?;
            if !magnitude.is_finite() {
                return Err(invalid());
            }
            Ok(float::canonical(if negative {
                -magnitude
            } else {
                magnitude
            }))
        }
        Expr::Field { base, name, .. } => match (base.as_ref(), name.as_str(), negative) {
            (Expr::Name { text, .. }, "nan", false) if text == "Float" => {
                Ok(float::canonical(f64::NAN))
            }
            (Expr::Name { text, .. }, "infinity", _) if text == "Float" => {
                Ok(float::canonical(if negative {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }))
            }
            _ => Err(invalid()),
        },
        _ => Err(invalid()),
    }
}

/// A quoted, padded standard Base64 blob. Only the canonical padded spelling is accepted.
fn blob(machine: &Machine<'_>, expr: &Expr) -> Result<Rc<[u8]>, Stop> {
    let encoded = string_value(machine, expr)?;
    let bytes = orna_value::base64_decode(&encoded).map_err(|_| invalid())?;
    if orna_value::base64_encode(&bytes) != encoded {
        return Err(invalid());
    }
    Ok(Rc::from(bytes))
}

/// A quoted lowercase canonical 8-4-4-4-12 UUID.
pub(super) fn uuid(expr: &Expr) -> Result<[u8; 16], Stop> {
    let text = string_literal(expr)?;
    if text.len() != 36
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(invalid());
    }
    let digits: String = text.chars().filter(|character| *character != '-').collect();
    if digits.len() != 32 {
        return Err(invalid());
    }
    let mut bytes = [0u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&digits[2 * index..2 * index + 2], 16).map_err(|_| invalid())?;
    }
    if uuid_text(&bytes) != text {
        return Err(invalid());
    }
    Ok(bytes)
}

/// A `Date` calendar literal, read as days since the epoch.
pub(super) fn date(expr: &Expr) -> Result<i64, Stop> {
    match expr {
        Expr::Literal {
            kind: LiteralKind::Date,
            text,
            ..
        } => parse_date_text(text).ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// The unescaped content of a string literal, bounded by the string limit.
fn string_value(machine: &Machine<'_>, expr: &Expr) -> Result<String, Stop> {
    let value = string_literal(expr)?;
    if value.len() > machine.limits.max_string_bytes {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(value)
}

/// The unescaped content of a plain string literal. An interpolated string is not canonical data.
fn string_literal(expr: &Expr) -> Result<String, Stop> {
    match expr {
        Expr::Literal {
            kind: LiteralKind::String,
            text,
            ..
        } => unescape_string_literal(text).map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}

/// Splits a leading unary minus off a numeric expression. Canonical output writes a negative number that way.
fn signed(expr: &Expr) -> (bool, &Expr) {
    match expr {
        Expr::Unary { op, rhs, .. } if op == "-" => (true, rhs.as_ref()),
        _ => (false, expr),
    }
}

/// The signed exponent of a decimal literal: an optional minus and minimal digits. A plus sign is not canonical.
fn signed_exponent(text: &str) -> Result<BigInt, Stop> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let magnitude = minimal_digits(digits).ok_or_else(invalid)?;
    apply_sign(negative, magnitude).ok_or_else(invalid)
}

/// The sign applied to a magnitude. A negative zero is not canonical, so it yields `None`.
fn apply_sign(negative: bool, magnitude: BigInt) -> Option<BigInt> {
    match (negative, magnitude.is_zero()) {
        (true, true) => None,
        (true, false) => Some(-magnitude),
        (false, _) => Some(magnitude),
    }
}

/// The value of unsigned decimal digits written minimally: digits only, with no leading zero unless the digits are
/// a single zero.
fn minimal_digits(text: &str) -> Option<BigInt> {
    let bytes = text.as_bytes();
    let well_formed = !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0');
    if !well_formed {
        return None;
    }
    BigInt::parse_bytes(bytes, 10)
}

/// Whether a callee is the bare name `want`.
fn is_name(callee: &Expr, want: &str) -> bool {
    matches!(callee, Expr::Name { text, .. } if text == want)
}

fn invalid() -> Stop {
    Stop::Error("ORNA-EVAL-VALUE")
}

fn unsupported() -> Stop {
    Stop::Error("ORNA-EVAL-UNSUPPORTED")
}
