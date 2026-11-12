//! The `core.collection` window natives behind `std.query`'s time-series statistics: `__strictly_ordered_time_series`,
//! and `__window_rate`, `__window_rate_integrate`, `__window_derivative` and `__window_integrate`. A copy of the old
//! evaluator's `strictly_ordered_time_series` and `window_time_series_statistics`, with the same steps and errors.

use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};
use orna_hir::Ty;

use super::stats_decimal::collection_items;
use super::stats_series::{
    area, derivative_pairs, instant_nanos, point_pair, series_points, series_slope,
};
use super::values::{exactly, list, list_of, pair, some};
use crate::error::Stop;
use crate::machine::{ExecLimits, Machine};
use crate::value::Value;

/// `__strictly_ordered_time_series<T>(points: [(Instant, T)]): Bool`. Whether every time is strictly after the one
/// before it. A series of fewer than two points is ordered; a non-instant time is a type error.
pub(in crate::natives) fn strictly_ordered_time_series(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points] = exactly(arguments)?;
    let rows = list(points)?;
    collection_items(machine, rows.len())?;
    let mut previous: Option<BigInt> = None;
    for point in rows.iter() {
        machine.tick(1)?;
        let (time, _) = point_pair(point)?;
        let total = instant_nanos(time)?;
        if previous.as_ref().is_some_and(|last| *last >= total) {
            return Ok(Value::Bool(false));
        }
        previous = Some(total);
    }
    Ok(Value::Bool(true))
}

/// `__window_rate<T>(points: [(Instant, T)], size: Int, step: Int): [T?]`. The rate of each complete window.
pub(in crate::natives) fn window_rate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points, size, step] = exactly(arguments)?;
    windows(machine, points, &size, &step, window_rate_value)
}

/// `__window_rate_integrate<T>(points: [(Instant, T)], size: Int, step: Int): [(T?, T?)]`. The rate and the area of
/// each complete window, as a pair.
pub(in crate::natives) fn window_rate_integrate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points, size, step] = exactly(arguments)?;
    windows(machine, points, &size, &step, window_rate_integrate_value)
}

/// `__window_derivative<T>(points: [(Instant, T)], size: Int, step: Int): [[(Instant, T)]]`. The derivative series of
/// each complete window.
pub(in crate::natives) fn window_derivative(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points, size, step] = exactly(arguments)?;
    windows(machine, points, &size, &step, window_derivative_value)
}

/// `__window_integrate<T>(points: [(Instant, T)], size: Int, step: Int): [T?]`. The area of each complete window.
pub(in crate::natives) fn window_integrate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [points, size, step] = exactly(arguments)?;
    windows(machine, points, &size, &step, window_integrate_value)
}

/// The statistic of one complete window, as the window's result before it is wrapped in a list.
type WindowStatistic = fn(&mut Machine<'_>, &[Value]) -> Result<Value, Stop>;

/// Visits each complete window of `points` in source order, `size` points long and `step` points apart, and collects
/// `statistic` of each. Non-positive sizes or steps are value errors; a window longer than the series yields no
/// windows.
fn windows(
    machine: &mut Machine<'_>,
    points: Value,
    size: &Value,
    step: &Value,
    statistic: WindowStatistic,
) -> Result<Value, Stop> {
    let rows: Rc<[Value]> = list(points)?;
    collection_items(machine, rows.len())?;
    let size = positive_size(size)?;
    let step = positive_size(step)?;
    if size > rows.len() {
        return Ok(list_of(Vec::new()));
    }
    for _ in rows.iter() {
        machine.tick(1)?;
    }

    let last_start = rows.len() - size;
    let mut start = 0usize;
    let mut results = Vec::new();
    while start <= last_start {
        let end = start + size;
        collection_items(machine, size)?;
        machine.tick(1)?;
        machine.tick(1)?;
        results.push(statistic(machine, &rows[start..end])?);
        collection_items(machine, results.len())?;

        let Some(next) = start.checked_add(step) else {
            break;
        };
        start = next;
    }
    Ok(list_of(results))
}

/// A window size or step: a positive integer, clamped to `usize::MAX` when it is too large to address.
fn positive_size(value: &Value) -> Result<usize, Stop> {
    let Value::Int(value) = value else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if !value.is_positive() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    Ok(value.to_usize().unwrap_or(usize::MAX))
}

/// The rate of one window: the slope between its first and last point, wrapped as a present option.
fn window_rate_value(machine: &mut Machine<'_>, window: &[Value]) -> Result<Value, Stop> {
    let points = series_points(window)?;
    Ok(some(rate_of(&machine.limits, &points)?))
}

/// The rate and the area of one window, as a pair of present options.
fn window_rate_integrate_value(machine: &mut Machine<'_>, window: &[Value]) -> Result<Value, Stop> {
    let points = series_points(window)?;
    let rate = rate_of(&machine.limits, &points)?;
    let integral = area(&machine.limits, &points)?;
    Ok(pair(some(rate), some(integral)))
}

/// The derivative series of one window, as a list of `(Instant, slope)` pairs.
fn window_derivative_value(machine: &mut Machine<'_>, window: &[Value]) -> Result<Value, Stop> {
    let points = series_points(window)?;
    Ok(list_of(derivative_pairs(machine, &points)?))
}

/// The area of one window, wrapped as a present option.
fn window_integrate_value(machine: &mut Machine<'_>, window: &[Value]) -> Result<Value, Stop> {
    let points = series_points(window)?;
    Ok(some(area(&machine.limits, &points)?))
}

/// The slope between the first and the last of a checked series.
fn rate_of(limits: &ExecLimits, points: &[(&Value, &Value)]) -> Result<Value, Stop> {
    series_slope(limits, points[0], points[points.len() - 1])
}
