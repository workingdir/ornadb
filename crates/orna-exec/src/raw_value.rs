//! Stored canonical values read back as executor values.
//!
//! A row is written with [`Value::to_raw`]; this module is its inverse for the shapes a row holds. A shape this
//! stage does not read back is [`Stop::NotYet`], never a guess.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::Name;
use orna_value::{OPTION_TAG, Raw, TUPLE_TAG, UNIT_TAG};

use crate::error::Stop;
use crate::value::Value;

/// The executor value of the canonical raw value `raw`.
pub(crate) fn from_raw(raw: &Raw) -> Result<Value, Stop> {
    match raw {
        Raw::Bool(value) => Ok(Value::Bool(*value)),
        Raw::Int(value) => Ok(Value::Int(value.clone())),
        Raw::Text(value) => Ok(Value::Text(value.clone())),
        Raw::Bytes(bytes) => Ok(Value::Blob(Rc::from(bytes.as_slice()))),
        Raw::Array(items) => items
            .iter()
            .map(from_raw)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| Value::List(items.into())),
        Raw::Map(entries) => record_from(entries),
        Raw::Tag(UNIT_TAG, body) if is_empty_array(body) => Ok(Value::Unit),
        Raw::Tag(OPTION_TAG, body) => option_from(body),
        Raw::Tag(TUPLE_TAG, body) => match &**body {
            Raw::Array(items) => items
                .iter()
                .map(from_raw)
                .collect::<Result<Vec<_>, _>>()
                .map(|items| Value::Tuple(items.into())),
            _ => Err(Stop::NotYet("stored tuple shape (H6)")),
        },
        Raw::Null | Raw::Float(_) | Raw::Tag(..) => Err(Stop::NotYet("stored value shape (H6)")),
    }
}

/// A record of `fields`, with its fields in name order.
pub(crate) fn record(mut fields: Vec<(Name, Value)>) -> Value {
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    Value::Record(fields.into())
}

fn is_empty_array(body: &Raw) -> bool {
    matches!(body, Raw::Array(items) if items.is_empty())
}

fn record_from(entries: &[(Raw, Raw)]) -> Result<Value, Stop> {
    let mut fields = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let Raw::Text(name) = key else {
            return Err(Stop::NotYet("stored record key (H6)"));
        };
        fields.push((Name::new(name), from_raw(value)?));
    }
    Ok(record(fields))
}

/// An optional value: `[0]` for none, `[1, value]` for some.
fn option_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(Stop::NotYet("stored option shape (H6)"));
    };
    match parts.as_slice() {
        [Raw::Int(tag)] if *tag == BigInt::from(0) => Ok(Value::Option(None)),
        [Raw::Int(tag), payload] if *tag == BigInt::from(1) => {
            Ok(Value::Option(Some(Rc::new(from_raw(payload)?))))
        }
        _ => Err(Stop::NotYet("stored option shape (H6)")),
    }
}
