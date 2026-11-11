//! Table reads: `count`, `first` and `one` over every row of a table, in ascending key order.
//!
//! Each read opens one forward cursor over all keys and closes it on every path. Each row charges one step
//! against the activation's step budget, so a read of a large table stops at the limit like any other evaluation.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::DefId;

use super::{decode_row, store_invariant};
use crate::error::Stop;
use crate::machine::Machine;
use crate::raw_value;
use crate::store::{CursorId, Direction, KeyBounds};
use crate::value::Value;

/// `count()`: the number of rows in `table`.
pub(super) fn count(machine: &mut Machine<'_>, table: DefId) -> Result<Value, Stop> {
    let mut total = 0u64;
    scan(machine, table, |_| {
        total += 1;
        Ok(true)
    })?;
    Ok(Value::Int(BigInt::from(total)))
}

/// `first()`: the row with the least key, or none when the table is empty.
pub(super) fn first(machine: &mut Machine<'_>, table: DefId) -> Result<Value, Stop> {
    let mut found = None;
    scan(machine, table, |bytes| {
        found = Some(decode_row(bytes)?);
        Ok(false)
    })?;
    let found = found.map(|fields| Rc::new(raw_value::record(fields)));
    Ok(Value::Option(found))
}

/// `one()`: the only row of `table`. It fails when the table has no row or more than one.
pub(super) fn one(machine: &mut Machine<'_>, table: DefId) -> Result<Value, Stop> {
    let mut found = None;
    scan(machine, table, |bytes| {
        if found.is_some() {
            return Err(Stop::Error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
        }
        found = Some(decode_row(bytes)?);
        Ok(true)
    })?;
    let fields = found.ok_or(Stop::Error("ORNA-EVAL-RELATION-ONE-ZERO"))?;
    Ok(raw_value::record(fields))
}

/// Visits the canonical row bytes of `table` in ascending key order until `visit` returns `false`. The cursor is
/// closed whether the walk finishes or fails.
fn scan(
    machine: &mut Machine<'_>,
    table: DefId,
    mut visit: impl FnMut(&[u8]) -> Result<bool, Stop>,
) -> Result<(), Stop> {
    let cursor = machine
        .store
        .scan(table, KeyBounds::all(), Direction::Forward)
        .map_err(|error| store_invariant(&error))?;
    let outcome = walk(machine, cursor, &mut visit);
    machine.store.close(cursor);
    outcome
}

/// Advances `cursor` one row at a time, charging a step for each, until `visit` stops it or the rows run out.
fn walk(
    machine: &mut Machine<'_>,
    cursor: CursorId,
    visit: &mut impl FnMut(&[u8]) -> Result<bool, Stop>,
) -> Result<(), Stop> {
    loop {
        machine.tick(1)?;
        let Some(row) = machine
            .store
            .next(cursor)
            .map_err(|error| store_invariant(&error))?
        else {
            return Ok(());
        };
        if !visit(row.row)? {
            return Ok(());
        }
    }
}
