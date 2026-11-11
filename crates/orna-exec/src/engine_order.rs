//! The engine-derived `Order` (D16): the canonical total order of the kernel types.
//!
//! A type derives `Order` when [`derives_order`] accepts it. The executor compares values of such a type natively,
//! without a source instance. `Float` has no canonical order here (the design excludes every type that contains a
//! `Float`), so a `Float` never reaches [`compare`] through a checked program.

use std::cmp::Ordering;

use orna_hir::{Program, Ty, TyKind};

use crate::error::Stop;
use crate::value::Value;

/// Whether the static type `ty` has the engine-derived order: the scalar kernel types, and tuples of them.
pub(crate) fn derives_order(program: &Program, ty: Ty) -> bool {
    match program.tys.kind(ty) {
        Some(TyKind::Bool | TyKind::Int | TyKind::Decimal | TyKind::Str | TyKind::Blob) => true,
        Some(TyKind::Instant | TyKind::Duration) => true,
        Some(TyKind::Tuple(items)) => items.iter().all(|item| derives_order(program, *item)),
        _ => false,
    }
}

/// Orders two values of a type that [`derives_order`] accepts. Text compares by its code points, which is the byte
/// order of its UTF-8 form. Blobs compare as unsigned bytes, and tuples lexically, then by length.
///
/// # Errors
///
/// `ORNA-EVAL-TYPE` when the two values are not of one derived type.
pub(crate) fn compare(left: &Value, right: &Value) -> Result<Ordering, Stop> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Decimal(left), Value::Decimal(right)) => left.compare(right),
        (Value::Text(left), Value::Text(right)) => Ok(left.cmp(right)),
        (Value::Blob(left), Value::Blob(right)) => Ok(left[..].cmp(&right[..])),
        (
            Value::Instant {
                unix_seconds: left_seconds,
                nanosecond: left_nanos,
            },
            Value::Instant {
                unix_seconds: right_seconds,
                nanosecond: right_nanos,
            },
        ) => Ok((left_seconds, left_nanos).cmp(&(right_seconds, right_nanos))),
        (
            Value::Duration {
                seconds: left_seconds,
                nanosecond: left_nanos,
            },
            Value::Duration {
                seconds: right_seconds,
                nanosecond: right_nanos,
            },
        ) => Ok((left_seconds, left_nanos).cmp(&(right_seconds, right_nanos))),
        (Value::Tuple(left), Value::Tuple(right)) => {
            for (left, right) in left.iter().zip(right.iter()) {
                match compare(left, right)? {
                    Ordering::Equal => {}
                    other => return Ok(other),
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}
