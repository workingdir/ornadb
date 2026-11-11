//! The `core.collection` natives that call a closure on each row: transforms, predicates, the single match, sorts and
//! ranks. Each closure call charges what the closure's body charges; the natives charge the steps the old evaluator
//! charged on top of that.

use std::cmp::Ordering;

use num_bigint::BigInt;

use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::order::{check_sort_key, compare_sort_keys, stable_order};
use crate::natives::values::{exactly, list, list_of, pair, truth};
use crate::value::Value;

/// `map<T, U>(rows: [T], transform: fn(T): U): [U]`. No step of its own.
pub(in crate::natives) fn map(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, transform] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut mapped = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        mapped.push(machine.call_closure(&transform, vec![row.clone()])?);
    }
    Ok(list_of(mapped))
}

/// `flat_map<T, U>(rows: [T], transform: fn(T): [U]): [U]`. Each transform result must be a list. No step of its own.
pub(in crate::natives) fn flat_map(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, transform] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut flattened = Vec::new();
    for row in rows.iter() {
        let inner = list(machine.call_closure(&transform, vec![row.clone()])?)?;
        flattened.extend(inner.iter().cloned());
    }
    Ok(list_of(flattened))
}

/// `filter<T>(rows: [T], predicate: fn(T): Bool): [T]`. One step per row; a row is kept only on an explicit true.
pub(in crate::natives) fn filter(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, predicate] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut kept = Vec::new();
    for row in rows.iter() {
        machine.tick(1)?;
        if truth(machine.call_closure(&predicate, vec![row.clone()])?)? {
            kept.push(row.clone());
        }
    }
    Ok(list_of(kept))
}

/// `every<T>(rows: [T], predicate: fn(T): Bool): Bool`. One step per row examined; stops at the first false.
pub(in crate::natives) fn every(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, predicate] = exactly(arguments)?;
    let rows = list(rows)?;
    for row in rows.iter() {
        machine.tick(1)?;
        if !truth(machine.call_closure(&predicate, vec![row.clone()])?)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

/// `exists<T>(rows: [T], predicate: fn(T): Bool): Bool`. One step per row examined; stops at the first true.
pub(in crate::natives) fn exists(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, predicate] = exactly(arguments)?;
    let rows = list(rows)?;
    for row in rows.iter() {
        machine.tick(1)?;
        if truth(machine.call_closure(&predicate, vec![row.clone()])?)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// `one<T>(rows: [T], predicate: fn(T): Bool): T`. One step per row. Exactly one row must match: none is a zero error,
/// and more than one is a multiple error, reported after every row has been examined.
pub(in crate::natives) fn one(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, predicate] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut matching = None;
    let mut multiple = false;
    for row in rows.iter() {
        machine.tick(1)?;
        if truth(machine.call_closure(&predicate, vec![row.clone()])?)? {
            if matching.is_some() {
                multiple = true;
            } else {
                matching = Some(row.clone());
            }
        }
    }
    if multiple {
        return Err(Stop::Error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
    }
    matching.ok_or(Stop::Error("ORNA-EVAL-RELATION-ONE-ZERO"))
}

/// `sort_by<T, K>(rows: [T], key: fn(T): K): [T]`, and `__stable_sort`. Every key is computed in source order before
/// any reordering, and each key must be a lawful sort key. The sort is stable.
pub(in crate::natives) fn sort_by(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, key] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let sort_key = machine.call_closure(&key, vec![row.clone()])?;
        check_sort_key(&sort_key)?;
        keys.push(sort_key);
    }
    let order = stable_order(machine, &keys)?;
    Ok(list_of(
        order.into_iter().map(|index| rows[index].clone()).collect(),
    ))
}

/// `rank<T, K>(values: [T], key: fn(T): K): [(T, Int)]`. Competition ranking: equal keys share the rank of the first of
/// them, and the next distinct key skips the ranks the tied rows took. One step per row keyed and per adjacent pair
/// compared.
pub(in crate::natives) fn rank(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, key] = exactly(arguments)?;
    let rows = list(rows)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        machine.tick(1)?;
        let sort_key = machine.call_closure(&key, vec![row.clone()])?;
        check_sort_key(&sort_key)?;
        keys.push(sort_key);
    }
    let order = stable_order(machine, &keys)?;
    let mut ranked = Vec::with_capacity(order.len());
    let mut competition_rank = 1usize;
    for (position, &index) in order.iter().enumerate() {
        if position > 0 {
            machine.tick(1)?;
            if compare_sort_keys(&keys[order[position - 1]], &keys[index])? != Ordering::Equal {
                competition_rank = position
                    .checked_add(1)
                    .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
            }
        }
        ranked.push(pair(
            rows[index].clone(),
            Value::Int(BigInt::from(competition_rank)),
        ));
    }
    Ok(list_of(ranked))
}
