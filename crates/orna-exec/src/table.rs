//! Table operations: the row writes the executor runs against its store.
//!
//! A row is the canonical map of its stored fields, keyed by field name. A key
//! is the canonical encoding of its one component, or of the array of its
//! components for a composite key, in the table's declared key order.
//!
//! Every write reaches the store through [`put_row`] or a delete in this
//! module, so the rules for a row are stated once, here and in `write`.

mod read;
mod write;

use orna_hir::{
    ArgValue, Body, Call, DefId, DefKind, FieldInitializer, Name, Program, TableDef, TableOp,
};
use orna_value::{CanonicalValue, Raw};

use crate::error::Stop;
use crate::frame::Frame;
use crate::machine::Machine;
use crate::raw_value;
use crate::store::{Expect, StoreError};
use crate::value::Value;

impl<'p> Machine<'p> {
    /// Runs one table operation of `table`. The reads run in slice C of H6.
    pub(crate) fn table_op(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
        table: DefId,
        op: TableOp,
    ) -> Result<Value, Stop> {
        let program = self.program;
        let DefKind::Table(schema) = &program.defs[table].kind else {
            return Err(Stop::NotYet(
                "table operation on a definition that is not a table",
            ));
        };
        match op {
            TableOp::Insert => {
                let [row]: [Value; 1] = self.table_arguments(body, frame, call)?;
                insert(self, table, schema, &row)
            }
            TableOp::Delete => {
                let [key]: [Value; 1] = self.table_arguments(body, frame, call)?;
                if schema.key.is_empty() {
                    return Err(Stop::Error("ORNA-EVAL-TABLE-UNADMITTED"));
                }
                delete(self, table, &key)
            }
            TableOp::Upsert => {
                let [patch]: [Value; 1] = self.table_arguments(body, frame, call)?;
                write::upsert(self, table, schema, &patch)
            }
            TableOp::Update => {
                let [key, patch]: [Value; 2] = self.table_arguments(body, frame, call)?;
                write::update(self, table, schema, &key, &patch)
            }
            TableOp::Rekey => {
                let [old_key, new_key]: [Value; 2] = self.table_arguments(body, frame, call)?;
                write::rekey(self, table, schema, &old_key, &new_key)
            }
            TableOp::Count => read::count(self, table),
            TableOp::First => read::first(self, table),
            TableOp::One => read::one(self, table),
            TableOp::AsOf => Err(Stop::NotYet("as_of needs the activation snapshot (H5)")),
        }
    }

    /// The values of the table call's `N` arguments, in order. A call with another count, or with a
    /// defaulted argument, is an argument error.
    fn table_arguments<const N: usize>(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
    ) -> Result<[Value; N], Stop> {
        let mut values = Vec::with_capacity(N);
        for argument in call.args.iter() {
            let ArgValue::Expr(id) = argument.value else {
                return Err(Stop::Error("ORNA-EVAL-TABLE-ARGUMENT"));
            };
            values.push(self.eval(body, frame, id)?);
        }
        values
            .try_into()
            .map_err(|_| Stop::Error("ORNA-EVAL-TABLE-ARGUMENT"))
    }
}

/// Writes a new row and returns it. The defaults fill the fields the row omits.
fn insert(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    row: &Value,
) -> Result<Value, Stop> {
    let fields = write::fill_defaults(machine, schema, record_fields(row)?, false)?;
    insert_fields(machine, table, schema, fields)
}

/// Checks a complete row and writes it at its key, which must hold no row.
fn insert_fields(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    fields: Vec<(Name, Value)>,
) -> Result<Value, Stop> {
    let program = machine.program;
    check_row(program, schema, &fields)?;
    let key = key_of_row(program, schema, &fields)?;
    let row = raw_value::record(fields);
    put_row(
        machine,
        table,
        &key,
        &row,
        Expect::Absent,
        "ORNA-EVAL-TABLE-DUPLICATE-KEY",
    )?;
    Ok(row)
}

/// Removes the row at the key `key` names. It fails when no row is there.
fn delete(machine: &mut Machine<'_>, table: DefId, key: &Value) -> Result<Value, Stop> {
    let key = key_bytes(key)?;
    if machine
        .store
        .get(table, &key)
        .map_err(|error| store_invariant(&error))?
        .is_none()
    {
        return Err(absent_row(machine));
    }
    machine
        .store
        .delete(table, &key)
        .map_err(|error| store_invariant(&error))?;
    Ok(Value::Unit)
}

/// The stop for a write that names a key with no stored row. A run whose store starts empty knows that no
/// earlier writer left the row behind, so the old pipeline's `MISSING-ROW` is reported. Any other store may hold
/// rows the activation snapshot would show, which this executor does not bind until H5.
pub(super) fn absent_row(machine: &Machine<'_>) -> Stop {
    if machine.empty_snapshot {
        Stop::Error("ORNA-EVAL-TABLE-MISSING-ROW")
    } else {
        Stop::NotYet("missing-row write needs the activation snapshot (H5)")
    }
}

/// The stored fields of the row at `key`, or `None` when no row is there.
fn read_row(
    machine: &mut Machine<'_>,
    table: DefId,
    key: &[u8],
) -> Result<Option<Vec<(Name, Value)>>, Stop> {
    let Some(bytes) = machine
        .store
        .get(table, key)
        .map_err(|error| store_invariant(&error))?
    else {
        return Ok(None);
    };
    decode_row(&bytes).map(Some)
}

/// The fields of a stored row, from its canonical bytes.
pub(crate) fn decode_row(bytes: &[u8]) -> Result<Vec<(Name, Value)>, Stop> {
    let stored =
        CanonicalValue::decode(bytes).map_err(|_| Stop::NotYet("stored row bytes (H6)"))?;
    match raw_value::from_raw(stored.raw())? {
        Value::Record(fields) => Ok(fields.to_vec()),
        _ => Err(Stop::NotYet("stored row that is not a record (H6)")),
    }
}

/// Writes `row` at `key`. A violated `expect` fails with `code`.
fn put_row(
    machine: &mut Machine<'_>,
    table: DefId,
    key: &[u8],
    row: &Value,
    expect: Expect,
    code: &'static str,
) -> Result<(), Stop> {
    let bytes = encode(row.to_raw().map_err(Stop::Error)?, "ORNA-EVAL-VALUE")?;
    machine
        .store
        .put(table, key, &bytes, expect)
        .map_err(|error| match error {
            StoreError::Expectation(_) => Stop::Error(code),
            other => store_invariant(&other),
        })
}

/// The fields of a row value, or `ORNA-EVAL-TABLE-ROW` when the value is not a record.
fn record_fields(row: &Value) -> Result<Vec<(Name, Value)>, Stop> {
    let Value::Record(fields) = row else {
        return Err(Stop::Error("ORNA-EVAL-TABLE-ROW"));
    };
    Ok(fields.to_vec())
}

/// The canonical key of the key value `key`, as a table's delete, update and rekey name it.
fn key_bytes(key: &Value) -> Result<Vec<u8>, Stop> {
    encode(key.to_raw().map_err(Stop::Error)?, "ORNA-EVAL-TABLE-KEY")
}

/// Checks that `row` names only stored fields of `schema` and names every stored field. A computed field is
/// never stored, so a row that names one is rejected.
fn check_row(program: &Program, schema: &TableDef, row: &[(Name, Value)]) -> Result<(), Stop> {
    for (name, _) in row {
        match field_named(program, schema, name) {
            Some(field) if !is_computed(program, field) => {}
            _ => return Err(Stop::Error("ORNA-EVAL-TABLE-ROW")),
        }
    }
    for field in &schema.fields {
        if is_computed(program, *field) {
            continue;
        }
        let named = row
            .iter()
            .any(|(name, _)| name.as_str() == field_text(program, *field));
        if !named {
            return Err(Stop::Error("ORNA-EVAL-TABLE-ROW"));
        }
    }
    Ok(())
}

/// The key of `fields` in the table's declared key order.
fn key_of_row(
    program: &Program,
    schema: &TableDef,
    fields: &[(Name, Value)],
) -> Result<Vec<u8>, Stop> {
    let mut components = Vec::with_capacity(schema.key.len());
    for key in &schema.key {
        let name = field_text(program, *key);
        let value = fields
            .iter()
            .find(|(field, _)| field.as_str() == name)
            .map(|(_, value)| value)
            .ok_or(Stop::Error("ORNA-EVAL-TABLE-KEY"))?;
        components.push(value.to_raw().map_err(Stop::Error)?);
    }
    let raw = if components.len() == 1 {
        components.remove(0)
    } else {
        Raw::Array(components)
    };
    encode(raw, "ORNA-EVAL-TABLE-KEY")
}

/// The canonical bytes of `raw`, or `code` when the value has no canonical form.
fn encode(raw: Raw, code: &'static str) -> Result<Vec<u8>, Stop> {
    CanonicalValue::new(raw)
        .and_then(|value| value.encode())
        .map_err(|_| Stop::Error(code))
}

/// The stored field of `schema` named `name`.
fn field_named(program: &Program, schema: &TableDef, name: &Name) -> Option<DefId> {
    schema
        .fields
        .iter()
        .copied()
        .find(|field| field_text(program, *field) == name.as_str())
}

fn field_text(program: &Program, field: DefId) -> &str {
    program.defs[field].name.as_str()
}

fn initializer(program: &Program, field: DefId) -> Option<FieldInitializer> {
    match &program.defs[field].kind {
        DefKind::Field(def) => def.initializer,
        _ => None,
    }
}

fn is_computed(program: &Program, field: DefId) -> bool {
    matches!(
        initializer(program, field),
        Some(FieldInitializer::Computed(_))
    )
}

/// The value of the table field `field` in `row`, as a field access or a `map` of the field reads it. A computed
/// field runs its body with the row as `self`; a stored field is read from the row by name.
pub(crate) fn field_of_row(
    machine: &mut Machine<'_>,
    field: DefId,
    row: Value,
) -> Result<Value, Stop> {
    let program = machine.program;
    if let Some(FieldInitializer::Computed(body)) = initializer(program, field) {
        return machine.invoke(body, vec![(0, row)], &[], None);
    }
    let Value::Record(fields) = row else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let name = &program.defs[field].name;
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .ok_or(Stop::Error("ORNA-EVAL-FIELD"))
}

/// A store failure other than a violated expectation. The executor's own calls keep the store's
/// activation open and its key lookups valid, so reaching this is an invariant break, not a language error.
pub(crate) fn store_invariant(_error: &StoreError) -> Stop {
    Stop::NotYet("table store invariant (H6)")
}
