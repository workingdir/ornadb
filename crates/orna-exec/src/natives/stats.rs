//! The `core.stats` engine natives over whole lists: the extremes, the range and the mode. `sum` is the collection sum.
//! Each function charges the steps the old evaluator charged.
//!
//! The old evaluator's stats extremes return `null` for an empty list, where the collection extremes return `none`;
//! the stats range of an empty list is `null` too.

use std::cmp::Ordering;

use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::order::extreme;
use crate::natives::values::{exactly, list, list_of};
use crate::ops::checked;
use crate::value::Value;

/// `min<T>(rows: [T]): T?`. `null` for no rows; otherwise the option of the smallest row. One step per row.
pub(in crate::natives) fn min(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    stats_extreme(machine, &rows, Ordering::Less)
}

/// `max<T>(rows: [T]): T?`. `null` for no rows; otherwise the option of the largest row. One step per row.
pub(in crate::natives) fn max(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    stats_extreme(machine, &rows, Ordering::Greater)
}

/// `range<T>(rows: [T]): T?`. `null` for no rows; otherwise the largest minus the smallest row. Each extreme costs one
/// step per row. Only integer rows subtract here; other row kinds are a type error.
pub(in crate::natives) fn range(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    let low = stats_extreme(machine, &rows, Ordering::Less)?;
    let high = stats_extreme(machine, &rows, Ordering::Greater)?;
    let (Some(low), Some(high)) = (present(low), present(high)) else {
        return Ok(Value::Null);
    };
    match (low, high) {
        (Value::Int(low), Value::Int(high)) => {
            Ok(Value::Int(checked(high - low, &machine.limits)?))
        }
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// `mode<T>(rows: [T]): [T]`. The rows that occur most often, in the order each first occurs. One step per row; an
/// empty list has no mode.
pub(in crate::natives) fn mode(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut counts: Vec<(Value, usize)> = Vec::new();
    for value in rows.iter() {
        machine.tick(1)?;
        if let Some((_, count)) = counts.iter_mut().find(|(current, _)| current == value) {
            *count = count.checked_add(1).ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
        } else {
            counts.push((value.clone(), 1));
        }
    }
    let most = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
    Ok(list_of(
        counts
            .into_iter()
            .filter(|(_, count)| most > 0 && *count == most)
            .map(|(value, _)| value)
            .collect(),
    ))
}

/// The stats extreme: `null` for no rows, otherwise the collection extreme (an option).
fn stats_extreme(
    machine: &mut Machine<'_>,
    rows: &[Value],
    replace_on: Ordering,
) -> Result<Value, Stop> {
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    extreme(machine, rows, replace_on)
}

/// The value inside a present option, or `None` for an absent one.
fn present(value: Value) -> Option<Value> {
    match value {
        Value::Option(Some(inner)) => Some(inner.as_ref().clone()),
        _ => None,
    }
}
