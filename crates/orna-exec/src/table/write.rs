//! The writes that change an existing row, and the defaults every insert fills.
//!
//! `upsert`, `update` and `rekey` read the stored row, merge the caller's fields into it and write the merged row
//! back. One merge rule serves all three: a change names a stored field, never a computed one, and names a key
//! field only where the write permits it.

use orna_hir::{DefId, FieldInitializer, Name, Program, TableDef};

use super::{
    absent_row_needs_snapshot, check_row, field_named, insert_fields, is_computed, key_bytes,
    key_of_row, put_row, read_row, record_fields, store_invariant,
};
use crate::error::Stop;
use crate::machine::Machine;
use crate::raw_value;
use crate::store::Expect;
use crate::value::Value;

/// Fills the defaults `row` omits, in declaration order. Each default is evaluated with the row as it stands
/// at its turn, as the `self` of its body. With `keys_only`, only key fields are filled.
pub(super) fn fill_defaults(
    machine: &mut Machine<'_>,
    schema: &TableDef,
    mut row: Vec<(Name, Value)>,
    keys_only: bool,
) -> Result<Vec<(Name, Value)>, Stop> {
    let program = machine.program;
    for field in &schema.fields {
        if keys_only && !schema.key.contains(field) {
            continue;
        }
        let name = program.defs[*field].name.clone();
        if row.iter().any(|(present, _)| *present == name) {
            continue;
        }
        let Some(FieldInitializer::Default(default)) = super::initializer(program, *field) else {
            continue;
        };
        let scope = raw_value::record(row.clone());
        let value = machine.invoke(default, vec![(0, scope)], &[])?;
        row.push((name, value));
    }
    Ok(row)
}

/// `upsert`: writes `patch` over the row at its key when one exists, and otherwise inserts it with its defaults.
pub(super) fn upsert(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    patch: &Value,
) -> Result<Value, Stop> {
    let program = machine.program;
    let changes = record_fields(patch)?;
    let keyed = fill_defaults(machine, schema, changes.clone(), true)?;
    let key = key_of_row(program, schema, &keyed)?;
    let Some(existing) = read_row(machine, table, &key)? else {
        let full = fill_defaults(machine, schema, keyed, false)?;
        return insert_fields(machine, table, schema, full);
    };
    let row = raw_value::record(merge(program, schema, existing, changes, true)?);
    put_row(
        machine,
        table,
        &key,
        &row,
        Expect::Present,
        "ORNA-EVAL-TABLE-MISSING-ROW",
    )?;
    Ok(row)
}

/// `update`: writes the fields of `patch` into the row at `key`, which must hold one. A key field stays fixed.
pub(super) fn update(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    key: &Value,
    patch: &Value,
) -> Result<Value, Stop> {
    let program = machine.program;
    let key = key_bytes(key)?;
    let existing = read_row(machine, table, &key)?.ok_or_else(absent_row_needs_snapshot)?;
    let fields = merge(program, schema, existing, record_fields(patch)?, false)?;
    if key_of_row(program, schema, &fields)? != key {
        return Err(Stop::Error("ORNA-EVAL-TABLE-KEY"));
    }
    let row = raw_value::record(fields);
    put_row(
        machine,
        table,
        &key,
        &row,
        Expect::Present,
        "ORNA-EVAL-TABLE-MISSING-ROW",
    )?;
    Ok(row)
}

/// `rekey`: moves the row at `old` to `new`, keeping its fields. The row at `old` must exist and `new` must not.
pub(super) fn rekey(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    old: &Value,
    new: &Value,
) -> Result<Value, Stop> {
    let program = machine.program;
    let old_key = key_bytes(old)?;
    let new_key = key_bytes(new)?;
    if old_key == new_key {
        return Err(Stop::Error("ORNA-EVAL-TABLE-DUPLICATE-KEY"));
    }
    let existing = read_row(machine, table, &old_key)?.ok_or_else(absent_row_needs_snapshot)?;
    if read_row(machine, table, &new_key)?.is_some() {
        return Err(Stop::Error("ORNA-EVAL-TABLE-DUPLICATE-KEY"));
    }
    let key_changes = key_fields(program, schema, new)?;
    let fields = merge(program, schema, existing, key_changes, true)?;
    if key_of_row(program, schema, &fields)? != new_key {
        return Err(Stop::Error("ORNA-EVAL-TABLE-KEY"));
    }
    let row = raw_value::record(fields);
    machine
        .store
        .delete(table, &old_key)
        .map_err(|error| store_invariant(&error))?;
    put_row(
        machine,
        table,
        &new_key,
        &row,
        Expect::Absent,
        "ORNA-EVAL-TABLE-DUPLICATE-KEY",
    )?;
    Ok(row)
}

/// The row `existing` with each of `changes` written over it. A change names a stored field; a computed field
/// is never written, and a key field is written only when `permit_keys`. The merged row is checked as a row.
fn merge(
    program: &Program,
    schema: &TableDef,
    existing: Vec<(Name, Value)>,
    changes: Vec<(Name, Value)>,
    permit_keys: bool,
) -> Result<Vec<(Name, Value)>, Stop> {
    let mut row = existing;
    for (name, value) in changes {
        let Some(field) = field_named(program, schema, &name) else {
            return Err(Stop::Error("ORNA-EVAL-TABLE-ROW"));
        };
        if is_computed(program, field) || (!permit_keys && schema.key.contains(&field)) {
            return Err(Stop::Error("ORNA-EVAL-TABLE-ROW"));
        }
        match row.iter().position(|(present, _)| *present == name) {
            Some(index) => row[index].1 = value,
            None => row.push((name, value)),
        }
    }
    check_row(program, schema, &row)?;
    Ok(row)
}

/// The key fields `key` names, each with its component: the whole key for a single-field key, or the list of
/// components in declared order for a composite key.
fn key_fields(
    program: &Program,
    schema: &TableDef,
    key: &Value,
) -> Result<Vec<(Name, Value)>, Stop> {
    let parts = match (schema.key.len(), key) {
        (1, part) => vec![part.clone()],
        (count, Value::List(parts)) if parts.len() == count => parts.to_vec(),
        _ => return Err(Stop::Error("ORNA-EVAL-TABLE-KEY")),
    };
    Ok(schema
        .key
        .iter()
        .zip(parts)
        .map(|(field, part)| (program.defs[*field].name.clone(), part))
        .collect())
}
