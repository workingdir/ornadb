//! The `core.stats` series natives: `rate`, `derivative` and `integrate` over `(Instant, value)` points. A copy of the old
//! evaluator's `stats_rate`, `stats_derivative`, `stats_integrate`, `series_points`, `series_slope` and
//! `elapsed_seconds`, with their steps and errors. Times are compared and differenced on the nanosecond grid.

use num_bigint::BigInt;
use num_traits::Zero;
use orna_hir::Ty;

use super::stats_decimal::{collection_items, divide_stats, exact_result, numeric_decimal};
use super::stats_round::decimal_to_f64;
use super::values::{exactly, list, list_of, pair};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::{ExecLimits, Machine};
use crate::value::Value;

/// Nanoseconds in a second, as the big integer the nanosecond grid is built from.
const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// The nanoseconds since the Unix epoch of an instant. Any other value is a type error.
fn instant_nanos(value: &Value) -> Result<BigInt, Stop> {
    let Value::Instant {
        unix_seconds,
        nanosecond,
    } = value
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    Ok(BigInt::from(*unix_seconds) * NANOS_PER_SECOND + *nanosecond)
}

/// The elapsed seconds from `start` to `end` as an exact decimal with nine fractional digits. A span that is not
/// positive is a value error.
fn elapsed_seconds(start: &Value, end: &Value) -> Result<Decimal, Stop> {
    let nanoseconds = instant_nanos(end)? - instant_nanos(start)?;
    if nanoseconds <= BigInt::zero() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    Decimal::new(nanoseconds, BigInt::from(-9))
}

/// One `(time, value)` point of a series: a pair of two elements.
fn point_pair(point: &Value) -> Result<(&Value, &Value), Stop> {
    let Value::Tuple(pair) = point else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let [time, value] = &pair[..] else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    Ok((time, value))
}

/// The points of a series, in order. Times must be instants and strictly increasing, and there must be at least two.
fn series_points(points: &[Value]) -> Result<Vec<(&Value, &Value)>, Stop> {
    let mut result: Vec<(&Value, &Value)> = Vec::with_capacity(points.len());
    let mut previous: Option<BigInt> = None;
    for point in points {
        let (time, value) = point_pair(point)?;
        let total = instant_nanos(time)?;
        if previous.as_ref().is_some_and(|last| *last >= total) {
            return Err(Stop::Error("ORNA-EVAL-VALUE"));
        }
        previous = Some(total);
        result.push((time, value));
    }
    if result.len() < 2 {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    Ok(result)
}

/// The slope between two points: the value change per elapsed second. Floats divide in binary64; integers and
/// decimals divide exactly, and a decimal operand keeps a decimal result.
fn series_slope(
    limits: &ExecLimits,
    start: (&Value, &Value),
    end: (&Value, &Value),
) -> Result<Value, Stop> {
    let elapsed = elapsed_seconds(start.0, end.0)?;
    match (start.1, end.1) {
        (Value::Float(start), Value::Float(end)) => Ok(float::canonical(
            (f64::from_bits(*end) - f64::from_bits(*start)) / decimal_to_f64(&elapsed)?,
        )),
        (Value::Int(_) | Value::Decimal(_), Value::Int(_) | Value::Decimal(_)) => {
            let preserve =
                matches!(start.1, Value::Decimal(_)) || matches!(end.1, Value::Decimal(_));
            let delta = numeric_decimal(end.1)?.sub(&numeric_decimal(start.1)?)?;
            divide_stats(limits, &delta, &elapsed, None, preserve)
        }
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// `rate<T>(points: [(Instant, T)]): T?`. The slope between the first and the last point.
pub(in crate::natives) fn rate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points] = exactly(arguments)?;
    let rows = list(points)?;
    let points = series_points(&rows)?;
    series_slope(&machine.limits, points[0], points[points.len() - 1])
}

/// `derivative<T>(points: [(Instant, T)]): [(Instant, T)]`. The slope of each adjacent pair, timed at its later point.
pub(in crate::natives) fn derivative(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points] = exactly(arguments)?;
    let rows = list(points)?;
    let points = series_points(&rows)?;
    let mut derivatives = Vec::with_capacity(points.len() - 1);
    for window in points.windows(2) {
        machine.tick(1)?;
        let slope = series_slope(&machine.limits, window[0], window[1])?;
        derivatives.push(pair(window[1].0.clone(), slope));
        collection_items(machine, derivatives.len())?;
    }
    Ok(list_of(derivatives))
}

/// `integrate<T>(points: [(Instant, T)]): T?`. The trapezoidal area under the series.
pub(in crate::natives) fn integrate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points] = exactly(arguments)?;
    let rows = list(points)?;
    let points = series_points(&rows)?;
    if points
        .iter()
        .all(|(_, value)| matches!(value, Value::Float(_)))
    {
        return float_area(&points);
    }
    decimal_area(&machine.limits, &points)
}

/// The trapezoidal area of an all-float series, in binary64.
fn float_area(points: &[(&Value, &Value)]) -> Result<Value, Stop> {
    let mut total = 0.0;
    for window in points.windows(2) {
        let (Value::Float(left), Value::Float(right)) = (window[0].1, window[1].1) else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let elapsed = decimal_to_f64(&elapsed_seconds(window[0].0, window[1].0)?)?;
        total += (f64::from_bits(*left) + f64::from_bits(*right)) * 0.5 * elapsed;
    }
    Ok(float::canonical(total))
}

/// The trapezoidal area of an integer or decimal series, exactly. The running decimal flag follows the old evaluator:
/// a decimal operand, or a decimal average, keeps a decimal result.
fn decimal_area(limits: &ExecLimits, points: &[(&Value, &Value)]) -> Result<Value, Stop> {
    let two = Decimal::new(BigInt::from(2), BigInt::zero())?;
    let mut total = Decimal::new(BigInt::zero(), BigInt::zero())?;
    let mut preserve = false;
    for window in points.windows(2) {
        let (left, right) = (window[0].1, window[1].1);
        preserve |= matches!(left, Value::Decimal(_)) || matches!(right, Value::Decimal(_));
        let sum = numeric_decimal(left)?.add(&numeric_decimal(right)?)?;
        let average = divide_stats(limits, &sum, &two, None, preserve)?;
        preserve |= matches!(average, Value::Decimal(_));
        let area =
            numeric_decimal(&average)?.multiply(&elapsed_seconds(window[0].0, window[1].0)?)?;
        total = total.add(&area)?;
    }
    exact_result(limits, total, preserve)
}
