//! Primary-key ordering over raw OVB values.

use crate::Result;
use crate::decimal::Decimal;
use crate::error::Error;
use crate::identity::uuid_array;
use crate::raw::{Raw, Value, array, bytes, text};
use crate::tags::{
    DATE_TAG, DECIMAL_TAG, ENUM_TAG, INSTANT_TAG, PINNED_ROW_REFERENCE_TAG,
    STORED_TABLE_REFERENCE_TAG, TUPLE_TAG, UNIT_TAG, UUID_TAG,
};
use num_bigint::BigInt;
use num_bigint::Sign;
use num_traits::Zero;
use std::cmp::Ordering;

/// Compares canonical table keys by their logical values, not by OVB bytes.
///
/// The input must be one permitted primary-key value or a non-empty OVB tuple
/// of key components. Schema admission remains responsible for proving that
/// every component belongs to its declared key type; this boundary rejects
/// malformed and non-key values rather than assigning them an incidental
/// host order.
pub fn compare_primary_keys(left: &Value, right: &Value) -> Result<Ordering> {
    let left = primary_key_components(left.raw())?;
    let right = primary_key_components(right.raw())?;
    if left.len() != right.len() {
        return Err(Error::InvalidValue);
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = compare_key_component(left, right)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

fn primary_key_components(raw: &Raw) -> Result<Vec<&Raw>> {
    match raw {
        Raw::Tag(TUPLE_TAG, payload) => {
            let values = array(payload)?;
            if values.is_empty() {
                return Err(Error::InvalidValue);
            }
            Ok(values.iter().collect())
        }
        Raw::Array(values) if !values.is_empty() => Ok(values.iter().collect()),
        _ if is_primary_key_component(raw) => Ok(vec![raw]),
        _ => Err(Error::InvalidValue),
    }
}

fn is_primary_key_component(raw: &Raw) -> bool {
    matches!(
        raw,
        Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Text(_)
            | Raw::Tag(UUID_TAG, _)
            | Raw::Tag(DECIMAL_TAG, _)
            | Raw::Tag(DATE_TAG, _)
            | Raw::Tag(INSTANT_TAG, _)
            | Raw::Tag(ENUM_TAG, _)
            | Raw::Tag(PINNED_ROW_REFERENCE_TAG, _)
            | Raw::Tag(TUPLE_TAG, _)
            | Raw::Tag(STORED_TABLE_REFERENCE_TAG, _)
    )
}

/// Strict identity rule for [`Value::table_reference`] and for decoded 60021
/// keys: the permitted-key clauses of 07-tables.md:272-281 applied recursively.
/// A stored reference identity must be snapshot-free (08-relations.md:31), so
/// pinned references (60010), Float and malformed 60008/60015/60021 keys are
/// refused. The Unit key (60014, empty array) is admitted because a reference
/// to a keyless table carries it (grammar/orna.ebnf:38-39; 29-formats.md:37;
/// stored admission in `validate_table_reference_value`). Tuple primary keys
/// keep base acceptance for pinned (60010) components; that is a deferred
/// decision (see `table_reference_decode_rejects_non_permitted_key`).
pub(crate) fn is_permitted_key(raw: &Raw) -> bool {
    match raw {
        Raw::Bool(_) | Raw::Int(_) | Raw::Text(_) => true,
        Raw::Tag(UUID_TAG | DECIMAL_TAG | DATE_TAG | INSTANT_TAG, _) => true,
        Raw::Tag(UNIT_TAG, payload) => {
            matches!(payload.as_ref(), Raw::Array(values) if values.is_empty())
        }
        Raw::Tag(ENUM_TAG, payload) => matches!(
            payload.as_ref(),
            Raw::Array(values) if matches!(
                values.as_slice(),
                [type_id, variant_id, Raw::Null]
                    if uuid_array(type_id).is_ok() && uuid_array(variant_id).is_ok()
            )
        ),
        Raw::Tag(TUPLE_TAG, payload) => matches!(
            payload.as_ref(),
            Raw::Array(values) if !values.is_empty() && values.iter().all(is_permitted_key)
        ),
        Raw::Tag(STORED_TABLE_REFERENCE_TAG, payload) => matches!(
            payload.as_ref(),
            Raw::Array(values) if matches!(
                values.as_slice(),
                [database_id, table_id, key]
                    if uuid_array(database_id).is_ok()
                        && uuid_array(table_id).is_ok()
                        && is_permitted_key(key)
            )
        ),
        _ => false,
    }
}

fn compare_key_component(left: &Raw, right: &Raw) -> Result<Ordering> {
    let ordering = match (left, right) {
        (Raw::Bool(left), Raw::Bool(right)) => left.cmp(right),
        (Raw::Int(left), Raw::Int(right)) => left.cmp(right),
        (Raw::Text(left), Raw::Text(right)) => left.cmp(right),
        (Raw::Tag(UUID_TAG, left), Raw::Tag(UUID_TAG, right)) => bytes(left)?.cmp(bytes(right)?),
        (Raw::Tag(DECIMAL_TAG, _), Raw::Tag(DECIMAL_TAG, _)) => compare_decimal_raw(left, right)?,
        (Raw::Tag(DATE_TAG, left), Raw::Tag(DATE_TAG, right)) => text(left)?.cmp(text(right)?),
        (Raw::Tag(INSTANT_TAG, left), Raw::Tag(INSTANT_TAG, right)) => {
            let left = array(left)?;
            let right = array(right)?;
            let [Raw::Int(left_seconds), Raw::Int(left_nanos)] = left.as_slice() else {
                return Err(Error::InvalidValue);
            };
            let [Raw::Int(right_seconds), Raw::Int(right_nanos)] = right.as_slice() else {
                return Err(Error::InvalidValue);
            };
            left_seconds
                .cmp(right_seconds)
                .then(left_nanos.cmp(right_nanos))
        }
        (Raw::Tag(ENUM_TAG, left), Raw::Tag(ENUM_TAG, right)) => {
            let left = array(left)?;
            let right = array(right)?;
            let [left_type, left_variant, Raw::Null] = left.as_slice() else {
                return Err(Error::InvalidValue);
            };
            let [right_type, right_variant, Raw::Null] = right.as_slice() else {
                return Err(Error::InvalidValue);
            };
            uuid_array(left_type)?
                .cmp(&uuid_array(right_type)?)
                .then(uuid_array(left_variant)?.cmp(&uuid_array(right_variant)?))
        }
        (Raw::Tag(PINNED_ROW_REFERENCE_TAG, left), Raw::Tag(PINNED_ROW_REFERENCE_TAG, right)) => {
            let left = array(left)?;
            let right = array(right)?;
            let [left_db, left_table, left_key, left_snapshot] = left.as_slice() else {
                return Err(Error::InvalidValue);
            };
            let [right_db, right_table, right_key, right_snapshot] = right.as_slice() else {
                return Err(Error::InvalidValue);
            };
            uuid_array(left_db)?
                .cmp(&uuid_array(right_db)?)
                .then(uuid_array(left_table)?.cmp(&uuid_array(right_table)?))
                .then(compare_key_component(left_key, right_key)?)
                .then(compare_raw_order(left_snapshot, right_snapshot)?)
        }
        // Unit (60014) is the sole key of a keyless table (29-formats.md:37):
        // one possible value, so two Units compare Equal.
        (Raw::Tag(UNIT_TAG, left), Raw::Tag(UNIT_TAG, right)) => {
            if !array(left)?.is_empty() || !array(right)?.is_empty() {
                return Err(Error::InvalidValue);
            }
            Ordering::Equal
        }
        (Raw::Tag(TUPLE_TAG, left), Raw::Tag(TUPLE_TAG, right)) => {
            compare_key_components(array(left)?, array(right)?)?
        }
        (
            Raw::Tag(STORED_TABLE_REFERENCE_TAG, left),
            Raw::Tag(STORED_TABLE_REFERENCE_TAG, right),
        ) => {
            let left = array(left)?;
            let right = array(right)?;
            let [left_db, left_table, left_key] = left.as_slice() else {
                return Err(Error::InvalidValue);
            };
            let [right_db, right_table, right_key] = right.as_slice() else {
                return Err(Error::InvalidValue);
            };
            if uuid_array(left_db)? != uuid_array(right_db)?
                || uuid_array(left_table)? != uuid_array(right_table)?
            {
                // Spec-silent: 08-relations.md:31 defines identity equality for
                // references but no ordering across tables. Reject rather than
                // invent one.
                return Err(Error::InvalidValue);
            }
            compare_key_component(left_key, right_key)?
        }
        _ => return Err(Error::InvalidValue),
    };
    Ok(ordering)
}

fn compare_key_components(left: &[Raw], right: &[Raw]) -> Result<Ordering> {
    for (left, right) in left.iter().zip(right) {
        let ordering = compare_key_component(left, right)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn compare_raw_order(left: &Raw, right: &Raw) -> Result<Ordering> {
    match (left, right) {
        (Raw::Null, Raw::Null) => Ok(Ordering::Equal),
        (Raw::Bool(left), Raw::Bool(right)) => Ok(left.cmp(right)),
        (Raw::Int(left), Raw::Int(right)) => Ok(left.cmp(right)),
        (Raw::Bytes(left), Raw::Bytes(right)) => Ok(left.cmp(right)),
        (Raw::Text(left), Raw::Text(right)) => Ok(left.cmp(right)),
        (Raw::Array(left), Raw::Array(right)) => {
            for (left, right) in left.iter().zip(right) {
                let ordering = compare_raw_order(left, right)?;
                if ordering != Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        (Raw::Tag(left_tag, left), Raw::Tag(right_tag, right)) if left_tag == right_tag => {
            match *left_tag {
                DECIMAL_TAG => compare_decimal_raw(
                    &Raw::Tag(*left_tag, left.clone()),
                    &Raw::Tag(*right_tag, right.clone()),
                ),
                TUPLE_TAG => compare_key_components(array(left)?, array(right)?),
                _ => compare_raw_order(left, right),
            }
        }
        _ => Err(Error::InvalidValue),
    }
}

fn compare_decimal_raw(left: &Raw, right: &Raw) -> Result<Ordering> {
    let left = Decimal::from_raw(left)?;
    let right = Decimal::from_raw(right)?;
    let left_zero = left.coefficient.is_zero();
    let right_zero = right.coefficient.is_zero();
    if left_zero || right_zero {
        return Ok(match (left_zero, right_zero) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if right.coefficient.sign() == Sign::Minus {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if left.coefficient.sign() == Sign::Minus {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => unreachable!(),
        });
    }
    let left_negative = left.coefficient.sign() == Sign::Minus;
    let right_negative = right.coefficient.sign() == Sign::Minus;
    if left_negative != right_negative {
        return Ok(if left_negative {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let left_digits = left
        .coefficient
        .to_str_radix(10)
        .trim_start_matches('-')
        .to_owned();
    let right_digits = right
        .coefficient
        .to_str_radix(10)
        .trim_start_matches('-')
        .to_owned();
    let left_position = &left.exponent10 + BigInt::from(left_digits.len());
    let right_position = &right.exponent10 + BigInt::from(right_digits.len());
    let mut ordering = left_position.cmp(&right_position);
    if ordering == Ordering::Equal {
        for offset in 0..left_digits.len().max(right_digits.len()) {
            let left_digit = left_digits.as_bytes().get(offset).copied().unwrap_or(b'0');
            let right_digit = right_digits.as_bytes().get(offset).copied().unwrap_or(b'0');
            ordering = left_digit.cmp(&right_digit);
            if ordering != Ordering::Equal {
                break;
            }
        }
    }
    Ok(if left_negative {
        ordering.reverse()
    } else {
        ordering
    })
}
