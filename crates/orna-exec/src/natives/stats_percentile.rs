//! The `core.stats` percentile: the rows sorted by their numeric total order, then the value at the position the
//! probability picks, interpolated by the named method. A float probability reads only float rows and ignores the
//! scale options, as the old evaluator does. A copy of the old evaluator's `percentile`, `percentile_float` and
//! `interpolate_exact`, with their steps and errors.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use orna_hir::Ty;

use super::stats_decimal::{collection_items, exact_result, options};
use super::stats_median::{average_values, compare_rows};
use super::stats_round::{decimal_floor_fraction, decimal_to_f64, round_to_scale};
use super::values::{exactly, list};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::{ExecLimits, Machine};
use crate::value::Value;

/// The probability as the percentile reads it: an exact non-negative integer or decimal, or a finite float.
enum Probability {
    Exact(Decimal),
    Float(f64),
}

/// `percentile<T, P>(rows: [T], p: P, interpolation: Str, scale: Int?, rounding: Str?): T?`. An empty list is `null`
/// once the probability and the interpolation are valid.
pub(in crate::natives) fn percentile(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, probability, interpolation, scale, rounding] = exactly(arguments)?;
    let rows = list(rows)?;
    let scale = options(&machine.limits, &[scale, rounding])?;
    let Value::Text(interpolation) = interpolation else {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    };
    if !matches!(
        interpolation.as_str(),
        "linear" | "lower" | "higher" | "nearest" | "midpoint"
    ) {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    collection_items(machine, rows.len())?;
    match probability_of(&probability)? {
        Probability::Float(probability) => percentile_float(&rows, probability, &interpolation),
        Probability::Exact(probability) => {
            percentile_exact(&machine.limits, &rows, &probability, &interpolation, scale)
        }
    }
}

/// The probability as an exact decimal or a float. A negative integer, a non-finite or out-of-range float, and any
/// other value are value errors. A decimal's range is checked later, before the empty-list check.
fn probability_of(value: &Value) -> Result<Probability, Stop> {
    match value {
        Value::Int(value) if !value.is_negative() => Ok(Probability::Exact(Decimal::new(
            value.clone(),
            BigInt::zero(),
        )?)),
        Value::Decimal(value) => Ok(Probability::Exact(value.clone())),
        Value::Float(bits) if f64::from_bits(*bits).is_finite() => {
            let probability = f64::from_bits(*bits);
            if (0.0..=1.0).contains(&probability) {
                Ok(Probability::Float(probability))
            } else {
                Err(Stop::Error("ORNA-EVAL-VALUE"))
            }
        }
        _ => Err(Stop::Error("ORNA-EVAL-VALUE")),
    }
}

/// The exact percentile of integer, decimal or float rows: the probability must lie in `[0, 1]`, the rows must share a
/// kind, and the interpolation is exact in the row's own kind.
fn percentile_exact(
    limits: &ExecLimits,
    rows: &[Value],
    probability: &Decimal,
    interpolation: &str,
    scale: Option<usize>,
) -> Result<Value, Stop> {
    let zero = Decimal::new(BigInt::zero(), BigInt::zero())?;
    let one = Decimal::new(BigInt::from(1u8), BigInt::zero())?;
    if probability.compare(&zero)? == Ordering::Less
        || probability.compare(&one)? == Ordering::Greater
    {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    if !is_homogeneous(rows) {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    }
    let mut sorted = rows.to_vec();
    sort_rows(limits, &mut sorted)?;
    let span = Decimal::new(BigInt::from(sorted.len() - 1), BigInt::zero())?;
    let position = probability.multiply(&span)?;
    let (index, fraction) = decimal_floor_fraction(limits, &position)?;
    let lower = sorted.get(index).ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    let upper = sorted.get(index.saturating_add(1)).unwrap_or(lower);
    interpolate(limits, lower, upper, &fraction, interpolation, scale)
}

/// Whether every row is an integer, every row a decimal, or every row a float, as the first row is.
fn is_homogeneous(rows: &[Value]) -> bool {
    match rows.first() {
        Some(Value::Int(_)) => rows.iter().all(|value| matches!(value, Value::Int(_))),
        Some(Value::Decimal(_)) => rows.iter().all(|value| matches!(value, Value::Decimal(_))),
        Some(Value::Float(_)) => rows.iter().all(|value| matches!(value, Value::Float(_))),
        _ => false,
    }
}

/// Sorts in place by the numeric total order. Unlike median, the percentile sort charges no steps.
fn sort_rows(limits: &ExecLimits, sorted: &mut [Value]) -> Result<(), Stop> {
    for index in 1..sorted.len() {
        let mut current = index;
        while current > 0
            && compare_rows(limits, &sorted[current - 1], &sorted[current])? == Ordering::Greater
        {
            sorted.swap(current - 1, current);
            current -= 1;
        }
    }
    Ok(())
}

/// The value at the fraction between two neighbours, under the named interpolation. A zero fraction takes the lower
/// neighbour for every interpolation except `higher` and `nearest`, which the fraction decides.
fn interpolate(
    limits: &ExecLimits,
    lower: &Value,
    upper: &Value,
    fraction: &Decimal,
    interpolation: &str,
    scale: Option<usize>,
) -> Result<Value, Stop> {
    let at_floor = fraction.coefficient().is_zero();
    match interpolation {
        "lower" => Ok(lower.clone()),
        "higher" if at_floor => Ok(lower.clone()),
        "higher" => Ok(upper.clone()),
        "nearest" => {
            let half = Decimal::new(BigInt::from(5u8), BigInt::from(-1))?;
            if fraction.compare(&half)? == Ordering::Less {
                Ok(lower.clone())
            } else {
                Ok(upper.clone())
            }
        }
        "midpoint" if at_floor => Ok(lower.clone()),
        "midpoint" => average_values(limits, lower, upper, scale),
        "linear" if at_floor => Ok(lower.clone()),
        "linear" => interpolate_exact(limits, lower, upper, fraction, scale),
        _ => Err(Stop::Error("ORNA-EVAL-VALUE")),
    }
}

/// The linear interpolation between two neighbours: exact for integers and decimals (rounded to the scale when one is
/// given), and the float mix for floats. Mixed kinds are unsupported.
fn interpolate_exact(
    limits: &ExecLimits,
    lower: &Value,
    upper: &Value,
    fraction: &Decimal,
    scale: Option<usize>,
) -> Result<Value, Stop> {
    match (lower, upper) {
        (Value::Int(lower), Value::Int(upper)) => {
            let low = Decimal::new(lower.clone(), BigInt::zero())?;
            let high = Decimal::new(upper.clone(), BigInt::zero())?;
            let result = low.add(&high.sub(&low)?.multiply(fraction)?)?;
            exact_result(limits, rounded(limits, &result, scale)?, false)
        }
        (Value::Decimal(lower), Value::Decimal(upper)) => {
            let result = lower.add(&upper.sub(lower)?.multiply(fraction)?)?;
            exact_result(limits, rounded(limits, &result, scale)?, true)
        }
        (Value::Float(lower), Value::Float(upper)) => {
            let (low, high) = (f64::from_bits(*lower), f64::from_bits(*upper));
            Ok(float::canonical(
                low + (high - low) * decimal_to_f64(fraction)?,
            ))
        }
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// The decimal rounded half-even to the scale when one is given, otherwise the decimal itself.
fn rounded(limits: &ExecLimits, value: &Decimal, scale: Option<usize>) -> Result<Decimal, Stop> {
    match scale {
        Some(scale) => round_to_scale(limits, value, scale),
        None => Ok(value.clone()),
    }
}

/// The percentile of float rows at a float probability. Rows must all be floats, and the scale options do not apply.
fn percentile_float(rows: &[Value], probability: f64, interpolation: &str) -> Result<Value, Stop> {
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    let mut sorted = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Float(bits) = row else {
            return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
        };
        sorted.push(*bits);
    }
    sorted.sort_by(|left, right| orna_value::float_total_cmp(*left, *right));
    let position = probability * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    if interpolation == "linear" {
        let (low, high) = (f64::from_bits(sorted[lower]), f64::from_bits(sorted[upper]));
        return Ok(float::canonical(low + (high - low) * fraction));
    }
    if interpolation == "midpoint" {
        let (low, high) = (f64::from_bits(sorted[lower]), f64::from_bits(sorted[upper]));
        return Ok(float::canonical((low + high) / 2.0));
    }
    let choose = match interpolation {
        "higher" => upper,
        "nearest" if fraction >= 0.5 => upper,
        _ => lower,
    };
    Ok(Value::Float(sorted[choose]))
}
