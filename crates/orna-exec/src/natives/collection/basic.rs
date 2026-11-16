//! The `core.collection` natives over whole lists that take no closure: counting, union, sums, the ends, slices,
//! distinct elements and extremes. Each function charges the steps the old evaluator charged.

use orna_hir::{Ty, TyKind};
use std::cmp::Ordering;
use std::collections::HashSet;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use crate::decimal::Decimal;
use crate::engine_money;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::order::extreme;
use crate::natives::values::{canonical_bytes, charge, exactly, int, list, list_of, none, some};
use crate::ops::checked;
use crate::value::Value;

/// `count<T>(rows: [T]): Int`. One step.
pub(in crate::natives) fn count(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    machine.tick(1)?;
    Ok(Value::Int(BigInt::from(rows.len())))
}

/// `union<T>(left: [T], right: [T]): [T]`, and `__list_concat`. One step per element of both lists.
pub(in crate::natives) fn union_rows(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [left, right] = exactly(arguments)?;
    let left = list(left)?;
    let right = list(right)?;
    charge(machine, left.len() + right.len())?;
    Ok(list_of(left.iter().chain(right.iter()).cloned().collect()))
}

/// `sum<T>(rows: [T]): T`, and `__numeric_sum`. One step per element; only integers are summed here. A non-integer
/// element is unsupported, as the old evaluator reports it. An empty list sums to the additive zero of its element
/// type (ORNA-FLOAT-005): `Int` is `0`, `Float` is `+0.0`, `Decimal` is an exact zero, and `Money<C>` is a zero amount
/// in `C`.
pub(in crate::natives) fn sum(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    if rows.is_empty() {
        return additive_zero(machine, targs);
    }
    let mut total = BigInt::from(0);
    for value in rows.iter() {
        machine.tick(1)?;
        let Value::Int(value) = value else {
            return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
        };
        total = checked(total + value, &machine.limits)?;
    }
    Ok(Value::Int(total))
}

/// The additive zero of the element type `targs[0]` (ORNA-FLOAT-005). Any other element type is unsupported.
fn additive_zero(machine: &mut Machine<'_>, targs: &[Ty]) -> Result<Value, Stop> {
    let element = targs
        .first()
        .copied()
        .ok_or(Stop::Error("ORNA-EVAL-TYPE"))?;
    let program = machine.program;
    match program.tys.kind(element) {
        Some(TyKind::Int) => Ok(Value::Int(BigInt::from(0))),
        Some(TyKind::Float) => Ok(Value::Float(0.0_f64.to_bits())),
        Some(TyKind::Decimal) => Ok(Value::Decimal(Decimal::new(
            BigInt::from(0),
            BigInt::from(0),
        )?)),
        Some(TyKind::Adt(def, args)) if engine_money::is_money_type(program, *def) => {
            let [currency] = &args[..] else {
                return Err(Stop::Error("ORNA-EVAL-TYPE"));
            };
            let Some(name) = engine_money::currency_name(program, *currency) else {
                return Err(Stop::Error("ORNA-EVAL-TYPE"));
            };
            Ok(Value::Money {
                amount: Decimal::new(BigInt::from(0), BigInt::from(0))?,
                currency: machine.host.nominal_type_id(&name),
            })
        }
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// `first<T>(rows: [T]): T?`. No step.
pub(in crate::natives) fn first(
    _machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    Ok(rows.first().cloned().map_or(Value::Null, some))
}

/// `last<T>(rows: [T]): T?`. No step.
pub(in crate::natives) fn last(
    _machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    Ok(rows.last().cloned().map_or_else(none, some))
}

/// `min<T>(rows: [T]): T?`, and `__minimum`.
pub(in crate::natives) fn min_value(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    extreme(machine, &rows, Ordering::Less)
}

/// `max<T>(rows: [T]): T?`, and `__maximum`.
pub(in crate::natives) fn max_value(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    extreme(machine, &rows, Ordering::Greater)
}

/// `take<T>(rows: [T], count: Int): [T]`. A negative count is a value error; a count past the end takes every row.
/// One step per row taken.
pub(in crate::natives) fn take_rows(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, count] = exactly(arguments)?;
    let rows = list(rows)?;
    let count = int(count)?;
    let end = prefix_length(&count, rows.len())?;
    let mut taken = Vec::with_capacity(end);
    for value in &rows[..end] {
        machine.tick(1)?;
        taken.push(value.clone());
    }
    Ok(list_of(taken))
}

/// `drop<T>(rows: [T], count: Int): [T]`. A negative count is a value error; a count past the end drops every row.
/// One step per row kept.
pub(in crate::natives) fn drop_rows(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, count] = exactly(arguments)?;
    let rows = list(rows)?;
    let count = int(count)?;
    let start = prefix_length(&count, rows.len())?;
    let mut kept = Vec::with_capacity(rows.len() - start);
    for value in &rows[start..] {
        machine.tick(1)?;
        kept.push(value.clone());
    }
    Ok(list_of(kept))
}

/// The number of rows a count takes: the count itself, capped at the length of the list.
fn prefix_length(count: &BigInt, length: usize) -> Result<usize, Stop> {
    if count.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    if count >= &BigInt::from(length) {
        return Ok(length);
    }
    count.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))
}

/// `distinct<T>(rows: [T]): [T]`. One step per row, and one more per row kept. Rows are the same when their canonical
/// encodings are, and the first of equal rows is kept, in source order.
pub(in crate::natives) fn distinct(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for value in rows.iter() {
        machine.tick(1)?;
        if !seen.insert(canonical_bytes(value)?) {
            continue;
        }
        machine.tick(1)?;
        kept.push(value.clone());
    }
    Ok(list_of(kept))
}
