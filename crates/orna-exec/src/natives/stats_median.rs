//! The `core.stats` median: an insertion sort of the rows by their numeric total order, then the middle element or the
//! average of the two middle elements. A copy of the old evaluator's `median` and `average_values`, with its steps and
//! errors.

use std::cmp::Ordering;

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};
use orna_hir::Ty;

use super::stats_decimal::{collection_items, divide_stats, options};
use super::values::{exactly, list};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::{ExecLimits, Machine};
use crate::ops::checked;
use crate::value::Value;

/// `median<T>(rows: [T], scale: Int?, rounding: Str?): T?`. An empty list is `null`. Rows must all be integers, all
/// decimals, or all floats.
pub(in crate::natives) fn median(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, scale, rounding] = exactly(arguments)?;
    let rows = list(rows)?;
    let scale = options(&machine.limits, &[scale, rounding])?;
    collection_items(machine, rows.len())?;
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    let homogeneous = match &rows[0] {
        Value::Int(_) => rows.iter().all(|value| matches!(value, Value::Int(_))),
        Value::Decimal(_) => rows.iter().all(|value| matches!(value, Value::Decimal(_))),
        Value::Float(_) => rows.iter().all(|value| matches!(value, Value::Float(_))),
        _ => false,
    };
    if !homogeneous {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    }
    let mut sorted = rows.to_vec();
    insertion_sort(machine, &mut sorted)?;
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        return Ok(sorted[middle].clone());
    }
    average_values(&machine.limits, &sorted[middle - 1], &sorted[middle], scale)
}

/// Sorts in place by the numeric total order. Each inner comparison is one step, as the old evaluator charges it.
fn insertion_sort(machine: &mut Machine<'_>, sorted: &mut [Value]) -> Result<(), Stop> {
    for index in 1..sorted.len() {
        let mut current = index;
        while current > 0 {
            machine.tick(1)?;
            if compare_rows(&machine.limits, &sorted[current - 1], &sorted[current])?
                != Ordering::Greater
            {
                break;
            }
            sorted.swap(current - 1, current);
            current -= 1;
        }
    }
    Ok(())
}

/// The total order over median rows. Integers compare by value, floats by the IEEE total order, and decimals exactly
/// after aligning their exponents. An exponent gap beyond the integer digit limit is a limit error.
fn compare_rows(limits: &ExecLimits, left: &Value, right: &Value) -> Result<Ordering, Stop> {
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Float(left), Value::Float(right)) => Ok(orna_value::float_total_cmp(*left, *right)),
        (Value::Decimal(left), Value::Decimal(right)) => compare_decimals(limits, left, right),
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

fn compare_decimals(
    limits: &ExecLimits,
    left: &Decimal,
    right: &Decimal,
) -> Result<Ordering, Stop> {
    let delta = left.exponent10() - right.exponent10();
    let gap = delta
        .abs()
        .to_usize()
        .filter(|gap| *gap <= limits.max_integer_digits);
    let gap = gap.ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let factor =
        BigInt::from(10u8).pow(u32::try_from(gap).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?);
    Ok(if delta.sign() == Sign::Minus {
        left.coefficient().cmp(&(right.coefficient() * factor))
    } else {
        (left.coefficient() * factor).cmp(right.coefficient())
    })
}

/// The average of two middle elements: exact for integers and decimals (rounded to the scale when one is given), and
/// the canonical float mean for floats.
fn average_values(
    limits: &ExecLimits,
    left: &Value,
    right: &Value,
    scale: Option<usize>,
) -> Result<Value, Stop> {
    let two = Decimal::new(BigInt::from(2u8), BigInt::zero())?;
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => {
            let total = checked(left + right, limits)?;
            divide_stats(
                limits,
                &Decimal::new(total, BigInt::zero())?,
                &two,
                scale,
                false,
            )
        }
        (Value::Decimal(left), Value::Decimal(right)) => {
            let total = left.add(right)?;
            divide_stats(limits, &total, &two, scale, true)
        }
        (Value::Float(left), Value::Float(right)) => Ok(float::canonical(
            (f64::from_bits(*left) + f64::from_bits(*right)) / 2.0,
        )),
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}
