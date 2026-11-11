//! The `core.stats` histogram: per-bin counts of the rows over an ordered list of half-open bins. A copy of the old
//! evaluator's `stats_histogram`, with its steps and errors.

use num_bigint::BigInt;
use orna_hir::Ty;

use super::order::compare_sort_keys;
use super::values::{exactly, list, list_of, truth};
use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// `histogram<T>(rows: [T], bins: [(T, T)], include_final_upper: Bool): [Int]`. One step per row, and one per bin
/// bound the scan reads. Each bin is `[lower, upper)`, or `[lower, upper]` for the final bin when asked.
pub(in crate::natives) fn histogram(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, bins, include_final_upper] = exactly(arguments)?;
    let rows = list(rows)?;
    let bins = list(bins)?;
    let include_final_upper = truth(include_final_upper)?;
    collection_items(machine, rows.len())?;
    collection_items(machine, bins.len())?;
    let bounds = bounds(&bins)?;
    let mut counts = vec![0usize; bounds.len()];
    for value in rows.iter() {
        machine.tick(1)?;
        for (index, (lower, upper)) in bounds.iter().enumerate() {
            let lower_ok = compare_sort_keys(value, lower)?.is_ge();
            let upper_order = compare_sort_keys(value, upper)?;
            let final_inclusive = include_final_upper && index + 1 == bounds.len();
            if lower_ok && (upper_order.is_lt() || (final_inclusive && upper_order.is_eq())) {
                counts[index] = counts[index]
                    .checked_add(1)
                    .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
                break;
            }
        }
    }
    Ok(list_of(
        counts
            .into_iter()
            .map(|count| Value::Int(BigInt::from(count)))
            .collect(),
    ))
}

/// The validated `(lower, upper)` bounds, each strictly ascending and not overlapping the bin before it.
fn bounds(bins: &[Value]) -> Result<Vec<(Value, Value)>, Stop> {
    let mut bounds = Vec::with_capacity(bins.len());
    let mut previous_upper: Option<&Value> = None;
    for bin in bins {
        let Value::Tuple(pair) = bin else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let [lower, upper] = pair.as_ref() else {
            return Err(Stop::Error("ORNA-EVAL-TYPE"));
        };
        let overlaps_previous = match previous_upper {
            Some(previous) => compare_sort_keys(previous, lower)?.is_gt(),
            None => false,
        };
        if compare_sort_keys(lower, upper)?.is_ge() || overlaps_previous {
            return Err(Stop::Error("ORNA-EVAL-VALUE"));
        }
        bounds.push((lower.clone(), upper.clone()));
        previous_upper = Some(upper);
    }
    Ok(bounds)
}

/// Refuses a collection of more elements than the limit allows, as the old evaluator's `items` check does.
fn collection_items(machine: &Machine<'_>, count: usize) -> Result<(), Stop> {
    if count > machine.limits.max_collection_items {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
