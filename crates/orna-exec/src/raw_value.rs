//! Stored canonical values read back as executor values.
//!
//! A row is written with [`Value::to_raw`]; this module is its inverse for the shapes a row holds. A shape this
//! stage does not read back is [`Stop::NotYet`], never a guess.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::Name;
use orna_value::{
    DATE_TAG, DECIMAL_TAG, DIGEST_TAG, ENUM_TAG, INSTANT_TAG, MONEY_TAG, OPAQUE_SYSTEM_ID_TAG,
    OPTION_TAG, PINNED_ROW_REFERENCE_TAG, Raw, TUPLE_TAG, UNIT_TAG, UUID_TAG,
};

use crate::calendar;
use crate::decimal::Decimal;
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
        Raw::Tag(DATE_TAG, body) => date_from(body),
        Raw::Tag(UUID_TAG, body) => uuid_from(body),
        Raw::Tag(DIGEST_TAG, body) => digest_from(body),
        Raw::Tag(DECIMAL_TAG, body) => decimal_from(body),
        Raw::Tag(MONEY_TAG, body) => money_from(body),
        Raw::Tag(ENUM_TAG, body) => enum_from(body),
        Raw::Tag(PINNED_ROW_REFERENCE_TAG | OPAQUE_SYSTEM_ID_TAG, body)
            if matches!(&**body, Raw::Array(_)) =>
        {
            Ok(Value::RowRef(raw.clone()))
        }
        Raw::Null => Ok(Value::Null),
        Raw::Tag(INSTANT_TAG, body) => instant_from(body),
        Raw::Float(_) | Raw::Tag(..) => Err(Stop::NotYet("stored value shape (H6)")),
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

/// A Date body: the `YYYY-MM-DD` text of a real date.
fn date_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Text(text) = body else {
        return Err(value_error());
    };
    calendar::parse_date_text(text)
        .map(Value::Date)
        .ok_or_else(value_error)
}

/// An Instant body: its whole seconds and its nanoseconds, the latter below one second.
fn instant_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(value_error());
    };
    match parts.as_slice() {
        [Raw::Int(seconds), Raw::Int(nanosecond)] => {
            let nanosecond: u32 = nanosecond.try_into().map_err(|_| value_error())?;
            if nanosecond >= 1_000_000_000 {
                return Err(value_error());
            }
            let unix_seconds: i64 = seconds.try_into().map_err(|_| value_error())?;
            Ok(Value::Instant {
                unix_seconds,
                nanosecond,
            })
        }
        _ => Err(value_error()),
    }
}

/// A Uuid body: its 16 bytes.
fn uuid_from(body: &Raw) -> Result<Value, Stop> {
    object_id(body).map(Value::Uuid).ok_or_else(value_error)
}

/// A Digest body: the `sha256` algorithm name over its 32 bytes.
fn digest_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(value_error());
    };
    match parts.as_slice() {
        [Raw::Text(algorithm), Raw::Bytes(bytes)] if algorithm == "sha256" => {
            <[u8; 32]>::try_from(bytes.as_slice())
                .map(Value::Digest)
                .map_err(|_| value_error())
        }
        _ => Err(value_error()),
    }
}

/// A Decimal body: a coefficient and an exponent, normalised as the executor's decimals are.
fn decimal_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(value_error());
    };
    match parts.as_slice() {
        [Raw::Int(coefficient), Raw::Int(exponent)] => {
            Decimal::new(coefficient.clone(), exponent.clone()).map(Value::Decimal)
        }
        _ => Err(value_error()),
    }
}

/// A Money body: a decimal amount in canonical form, then the currency's object id.
fn money_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(value_error());
    };
    let [Raw::Tag(DECIMAL_TAG, amount_body), currency] = parts.as_slice() else {
        return Err(value_error());
    };
    let Value::Decimal(amount) = decimal_from(amount_body)? else {
        return Err(value_error());
    };
    if amount.to_raw() != Raw::Tag(DECIMAL_TAG, amount_body.clone()) {
        return Err(value_error());
    }
    let Raw::Tag(UUID_TAG, currency_body) = currency else {
        return Err(value_error());
    };
    let currency = object_id(currency_body).ok_or_else(value_error)?;
    Ok(Value::Money { amount, currency })
}

/// An enum variant: its type id, its variant id, then its payload record or none.
fn enum_from(body: &Raw) -> Result<Value, Stop> {
    let Raw::Array(parts) = body else {
        return Err(value_error());
    };
    let [
        Raw::Tag(UUID_TAG, type_body),
        Raw::Tag(UUID_TAG, variant_body),
        payload,
    ] = parts.as_slice()
    else {
        return Err(value_error());
    };
    let type_id = object_id(type_body).ok_or_else(value_error)?;
    let variant_id = object_id(variant_body).ok_or_else(value_error)?;
    let payload = match payload {
        Raw::Null => None,
        Raw::Map(entries) => match record_from(entries)? {
            Value::Record(fields) => Some(fields),
            _ => return Err(value_error()),
        },
        _ => return Err(value_error()),
    };
    Ok(Value::Enum {
        type_id,
        variant_id,
        payload,
    })
}

/// The 16 bytes of an object id body.
fn object_id(body: &Raw) -> Option<[u8; 16]> {
    let Raw::Bytes(bytes) = body else {
        return None;
    };
    <[u8; 16]>::try_from(bytes.as_slice()).ok()
}

fn value_error() -> Stop {
    Stop::Error("ORNA-EVAL-VALUE")
}

#[cfg(test)]
mod tests {
    use orna_value::{INSTANT_TAG, PINNED_ROW_REFERENCE_TAG, Raw};

    use super::from_raw;
    use crate::value::Value;

    #[test]
    fn null_decodes_to_null() {
        assert!(matches!(from_raw(&Raw::Null), Ok(Value::Null)));
    }

    #[test]
    fn instant_round_trips_byte_exact() {
        let raw = Raw::Tag(
            INSTANT_TAG,
            Box::new(Raw::Array(vec![Raw::Int(42.into()), Raw::Int(7.into())])),
        );
        let value = from_raw(&raw).expect("instant decodes");
        assert_eq!(value.to_raw().expect("instant encodes"), raw);
    }

    #[test]
    fn instant_rejects_out_of_range_nanoseconds() {
        let raw = Raw::Tag(
            INSTANT_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Int(1_000_000_000.into()),
            ])),
        );
        assert!(from_raw(&raw).is_err());
    }

    #[test]
    fn row_reference_round_trips_byte_exact() {
        let raw = Raw::Tag(
            PINNED_ROW_REFERENCE_TAG,
            Box::new(Raw::Array(vec![
                Raw::Text("run".to_owned()),
                Raw::Int(3.into()),
            ])),
        );
        let value = from_raw(&raw).expect("reference decodes");
        assert!(matches!(value, Value::RowRef(_)));
        assert_eq!(value.to_raw().expect("reference encodes"), raw);
    }

    #[test]
    fn row_reference_encodes_as_its_raw_form() {
        let value = Value::RowRef(Raw::Null);
        assert!(value.to_raw().is_ok());
    }
}
