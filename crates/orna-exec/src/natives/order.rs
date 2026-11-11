//! Sort, group and time keys: the checks that make a value a lawful key, the comparisons between keys, and the stable
//! merge that orders a list by its keys. These are the old evaluator's rules, shared by the collection natives.

use std::cmp::Ordering;

use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::values::{none, some};
use crate::value::Value;

/// Fails unless `value` is a lawful sort key: a boolean, integer, blob or text, or a tuple of those.
pub(super) fn check_sort_key(value: &Value) -> Result<(), Stop> {
    match value {
        Value::Bool(_) | Value::Int(_) | Value::Blob(_) | Value::Text(_) => Ok(()),
        Value::Tuple(items) => items.iter().try_for_each(check_sort_key),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Fails unless `value` is a lawful group key: a boolean, integer or text, or a tuple of those.
pub(super) fn check_group_key(value: &Value) -> Result<(), Stop> {
    match value {
        Value::Bool(_) | Value::Int(_) | Value::Text(_) => Ok(()),
        Value::Tuple(items) => items.iter().try_for_each(check_group_key),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Fails unless `value` is an as-of time: an integer.
pub(super) fn check_asof_time(value: &Value) -> Result<(), Stop> {
    match value {
        Value::Int(_) => Ok(()),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Orders two lawful sort keys. Blobs compare as unsigned bytes and tuples lexically, then by length.
pub(super) fn compare_sort_keys(left: &Value, right: &Value) -> Result<Ordering, Stop> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        (Value::Blob(left), Value::Blob(right)) => Ok(left[..].cmp(&right[..])),
        (Value::Text(left), Value::Text(right)) => Ok(left.cmp(right)),
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Tuple(left), Value::Tuple(right)) => compare_lexical(left, right, compare_sort_keys),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Orders two lawful group keys. Groups never compare blobs, so a blob is a type error here.
pub(super) fn compare_group_keys(left: &Value, right: &Value) -> Result<Ordering, Stop> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Text(left), Value::Text(right)) => Ok(left.cmp(right)),
        (Value::Tuple(left), Value::Tuple(right)) => compare_lexical(left, right, compare_group_keys),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Compares two tuples element by element, then by length.
fn compare_lexical(
    left: &[Value],
    right: &[Value],
    compare: fn(&Value, &Value) -> Result<Ordering, Stop>,
) -> Result<Ordering, Stop> {
    for (left, right) in left.iter().zip(right) {
        match compare(left, right)? {
            Ordering::Equal => {}
            other => return Ok(other),
        }
    }
    Ok(left.len().cmp(&right.len()))
}

/// The stable order of `keys`: the indices of the keys, smallest first, with equal keys in their source order. This is
/// a bottom-up merge. Each comparison costs one step, so the charge is the old evaluator's.
pub(super) fn stable_order(machine: &mut Machine<'_>, keys: &[Value]) -> Result<Vec<usize>, Stop> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    let mut merged = Vec::with_capacity(order.len());
    let mut width = 1;
    while width < order.len() {
        merged.clear();
        for start in (0..order.len()).step_by(width * 2) {
            let middle = (start + width).min(order.len());
            let end = (start + width * 2).min(order.len());
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                machine.tick(1)?;
                if compare_sort_keys(&keys[order[right]], &keys[order[left]])? == Ordering::Less {
                    merged.push(order[right]);
                    right += 1;
                } else {
                    merged.push(order[left]);
                    left += 1;
                }
            }
            merged.extend_from_slice(&order[left..middle]);
            merged.extend_from_slice(&order[right..end]);
        }
        std::mem::swap(&mut order, &mut merged);
        width *= 2;
    }
    Ok(order)
}

/// The smallest (`replace_on` is `Less`) or largest (`Greater`) of `rows` as an option. Each element costs one step and
/// must be a lawful sort key. An element replaces the candidate only when it compares strictly past it, so the first
/// of equal elements wins.
pub(super) fn extreme(machine: &mut Machine<'_>, rows: &[Value], replace_on: Ordering) -> Result<Value, Stop> {
    let mut candidate: Option<&Value> = None;
    for value in rows {
        machine.tick(1)?;
        check_sort_key(value)?;
        candidate = match candidate {
            None => Some(value),
            Some(current) if compare_sort_keys(value, current)? == replace_on => Some(value),
            kept => kept,
        };
    }
    Ok(candidate.map_or_else(none, |value| some(value.clone())))
}
