//! The `core.stats` variance: the population variance of the rows. Exact for integers and decimals, rounded to the scale
//! when one is given, and a two-pass mean for floats. A copy of the old evaluator's `variance` with its steps and
//! errors.

use num_bigint::BigInt;
use num_traits::Zero;
use orna_hir::Ty;

use super::stats_decimal::{collection_items, divide_stats, numeric_decimal, options};
use super::values::{exactly, list};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::Machine;
use crate::value::Value;

/// `variance<T>(rows: [T], scale: Int?, rounding: Str?): T?`. An empty list is `null`. Rows must all be integers or
/// decimals, or all floats.
pub(in crate::natives) fn variance(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, scale, rounding] = exactly(arguments)?;
    let rows = list(rows)?;
    let scale = options(&machine.limits, &[scale, rounding])?;
    collection_items(machine, rows.len())?;
    let Some(first) = rows.first() else {
        return Ok(Value::Null);
    };
    if rows.iter().all(|value| matches!(value, Value::Float(_))) {
        return float_variance(machine, &rows);
    }
    if !matches!(first, Value::Int(_) | Value::Decimal(_))
        || !rows
            .iter()
            .all(|value| matches!(value, Value::Int(_) | Value::Decimal(_)))
    {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    }
    let preserve_decimal = rows.iter().any(|value| matches!(value, Value::Decimal(_)));
    exact_variance(machine, &rows, scale, preserve_decimal)
}

/// The float variance: the mean in one pass, then the mean squared deviation in a second. Each pass is one step per row.
fn float_variance(machine: &mut Machine<'_>, rows: &[Value]) -> Result<Value, Stop> {
    let mut sum = 0.0;
    for value in rows {
        machine.tick(1)?;
        sum += float_bits(value)?;
    }
    let mean = sum / rows.len() as f64;
    let mut squares = 0.0;
    for value in rows {
        machine.tick(1)?;
        let delta = float_bits(value)? - mean;
        squares += delta * delta;
    }
    Ok(float::canonical(squares / rows.len() as f64))
}

fn float_bits(value: &Value) -> Result<f64, Stop> {
    match value {
        Value::Float(bits) => Ok(f64::from_bits(*bits)),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The exact variance over integers and decimals: `(n * sum of squares - (sum)^2) / n^2`, divided by `divide_stats`.
fn exact_variance(
    machine: &mut Machine<'_>,
    rows: &[Value],
    scale: Option<usize>,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    let zero = Decimal::new(BigInt::zero(), BigInt::zero())?;
    let mut sum = zero.clone();
    let mut squares = zero;
    for value in rows {
        machine.tick(1)?;
        let value = numeric_decimal(value)?;
        sum = sum.add(&value)?;
        squares = squares.add(&value.multiply(&value)?)?;
    }
    let count = Decimal::new(BigInt::from(rows.len()), BigInt::zero())?;
    let numerator = squares
        .multiply(&count)?
        .add(&sum.negate()?.multiply(&sum)?)?;
    let denominator = Decimal::new(
        BigInt::from(rows.len()) * BigInt::from(rows.len()),
        BigInt::zero(),
    )?;
    divide_stats(
        &machine.limits,
        &numerator,
        &denominator,
        scale,
        preserve_decimal,
    )
}
