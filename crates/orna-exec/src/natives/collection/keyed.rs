//! The `core.collection` natives that group or join rows by keys: `group_by` and `asof_join`.

use orna_hir::Ty;
use std::cmp::Ordering;

use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::order::{
    check_asof_time, check_group_key, compare_group_keys, compare_sort_keys,
};
use crate::natives::values::{canonical_bytes, exactly, list, list_of, pair, some};
use crate::value::Value;

/// `group_by<T, K>(values: [T], key: fn(T): K): [(K, [T])]`. One step per row keyed, and each key must be a lawful group
/// key. Groups keep their first-seen row order, and the groups themselves are sorted by key.
pub(in crate::natives) fn group_by(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, key] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
    for row in rows.iter() {
        machine.tick(1)?;
        let group_key = machine.call_closure(&key, vec![row.clone()])?;
        check_group_key(&group_key)?;
        let mut matched = false;
        for (existing_key, members) in &mut groups {
            if compare_group_keys(existing_key, &group_key)? == Ordering::Equal {
                members.push(row.clone());
                matched = true;
                break;
            }
        }
        if !matched {
            groups.push((group_key, vec![row.clone()]));
        }
    }
    // An insertion sort keeps the ordering decision fallible, as the old evaluator's does.
    for index in 1..groups.len() {
        let mut current = index;
        while current > 0
            && compare_group_keys(&groups[current - 1].0, &groups[current].0)? == Ordering::Greater
        {
            groups.swap(current - 1, current);
            current -= 1;
        }
    }
    Ok(list_of(
        groups
            .into_iter()
            .map(|(group_key, members)| pair(group_key, list_of(members)))
            .collect(),
    ))
}

/// `asof_join<T, Time, Key>(left, right, time, by): [(T, T?)]`. Each left row pairs with the right row of its group
/// whose time is the latest at or before the left row's time. Equal times break on the right row's canonical bytes,
/// the greater winning. One step per left and right row for each selector, and one per candidate pair compared.
/// An empty left list returns at once, without evaluating the right side.
pub(in crate::natives) fn asof_join(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [left, right, time, by] = exactly(arguments)?;
    let left = list(left)?;
    let right = list(right)?;
    if left.is_empty() {
        return Ok(list_of(Vec::new()));
    }
    let mut left_keys = Vec::with_capacity(left.len());
    for row in left.iter() {
        let (time_key, group_key) = keys_of(machine, &time, &by, row)?;
        left_keys.push((time_key, group_key));
    }
    let mut right_keys = Vec::with_capacity(right.len());
    for row in right.iter() {
        let (time_key, group_key) = keys_of(machine, &time, &by, row)?;
        right_keys.push((time_key, group_key, canonical_bytes(row)?));
    }
    let mut joined = Vec::with_capacity(left.len());
    for (left_row, (left_time, left_group)) in left.iter().zip(left_keys) {
        let mut selected: Option<usize> = None;
        for (index, (right_time, right_group, right_bytes)) in right_keys.iter().enumerate() {
            machine.tick(1)?;
            if compare_group_keys(&left_group, right_group)? != Ordering::Equal {
                continue;
            }
            if compare_sort_keys(right_time, &left_time)? == Ordering::Greater {
                continue;
            }
            let replace = match selected {
                None => true,
                Some(best) => {
                    let (best_time, _, best_bytes) = &right_keys[best];
                    match compare_sort_keys(right_time, best_time)? {
                        Ordering::Greater => true,
                        Ordering::Equal => right_bytes > best_bytes,
                        Ordering::Less => false,
                    }
                }
            };
            if replace {
                selected = Some(index);
            }
        }
        let matched = selected.map_or(Value::Null, |index| some(right[index].clone()));
        joined.push(pair(left_row.clone(), matched));
    }
    Ok(list_of(joined))
}

/// The as-of time key and the group key of one row. Each selector call charges one step before it runs.
fn keys_of(
    machine: &mut Machine<'_>,
    time: &Value,
    by: &Value,
    row: &Value,
) -> Result<(Value, Value), Stop> {
    machine.tick(1)?;
    let time_key = machine.call_closure(time, vec![row.clone()])?;
    check_asof_time(&time_key)?;
    machine.tick(1)?;
    let group_key = machine.call_closure(by, vec![row.clone()])?;
    check_group_key(&group_key)?;
    Ok((time_key, group_key))
}
