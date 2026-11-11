//! Table operations: the row writes the executor runs against its store.
//!
//! A row is the canonical map of its stored fields, keyed by field name. A key
//! is the canonical encoding of its one component, or of the array of its
//! components for a composite key, in the table's declared key order.

use orna_hir::{
    Arg, ArgValue, Body, Call, DefId, DefKind, FieldInitializer, Name, TableDef, TableOp,
};
use orna_value::{CanonicalValue, Raw};

use crate::error::Stop;
use crate::frame::Frame;
use crate::machine::Machine;
use crate::store::{Expect, StoreError};
use crate::value::Value;

impl<'p> Machine<'p> {
    /// Runs one table operation of `table`. Only `insert` and `delete` run in this stage.
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
                let row = self.single_argument(body, frame, call)?;
                insert(self, table, schema, row)
            }
            TableOp::Delete => {
                let key = self.single_argument(body, frame, call)?;
                delete(self, table, &key)
            }
            TableOp::Upsert | TableOp::Update | TableOp::Rekey => {
                Err(Stop::NotYet("table write (H6 slice B3)"))
            }
            TableOp::Count | TableOp::First | TableOp::One | TableOp::AsOf => {
                Err(Stop::NotYet("table read (H6 slice C)"))
            }
        }
    }

    /// The value of the call's only argument.
    fn single_argument(
        &mut self,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
    ) -> Result<Value, Stop> {
        let [
            Arg {
                value: ArgValue::Expr(argument),
                ..
            },
        ] = &call.args[..]
        else {
            return Err(Stop::Error("ORNA-EVAL-TABLE-ARGUMENT"));
        };
        self.eval(body, frame, *argument)
    }
}

/// Writes a new row and returns it. It fails when the row does not fit the table or its key already holds a row.
fn insert(
    machine: &mut Machine<'_>,
    table: DefId,
    schema: &TableDef,
    row: Value,
) -> Result<Value, Stop> {
    let Value::Record(fields) = &row else {
        return Err(Stop::Error("ORNA-EVAL-TABLE-ROW"));
    };
    check_row(machine.program, schema, fields)?;
    let key = key_of_row(machine.program, schema, fields)?;
    let bytes = encode(row.to_raw().map_err(Stop::Error)?, "ORNA-EVAL-VALUE")?;
    machine
        .store
        .put(table, &key, &bytes, Expect::Absent)
        .map_err(|error| match error {
            StoreError::Expectation(_) => Stop::Error("ORNA-EVAL-TABLE-DUPLICATE-KEY"),
            other => store_invariant(&other),
        })?;
    Ok(row)
}

/// Removes the row at the key `key` names. It fails when no row is there.
fn delete(machine: &mut Machine<'_>, table: DefId, key: &Value) -> Result<Value, Stop> {
    let key = encode(key.to_raw().map_err(Stop::Error)?, "ORNA-EVAL-TABLE-KEY")?;
    if machine
        .store
        .get(table, &key)
        .map_err(|error| store_invariant(&error))?
        .is_none()
    {
        return Err(Stop::Error("ORNA-EVAL-TABLE-MISSING-ROW"));
    }
    machine
        .store
        .delete(table, &key)
        .map_err(|error| store_invariant(&error))?;
    Ok(Value::Unit)
}

/// Checks that `row` names only stored fields of `schema` and gives every stored field that has no default.
fn check_row(
    program: &orna_hir::Program,
    schema: &TableDef,
    row: &[(Name, Value)],
) -> Result<(), Stop> {
    for (name, _) in row {
        match field_named(program, schema, name) {
            Some(field) if !is_computed(program, field) => {}
            _ => return Err(Stop::Error("ORNA-EVAL-TABLE-ROW")),
        }
    }
    for field in &schema.fields {
        let named = row
            .iter()
            .any(|(name, _)| name.as_str() == field_text(program, *field));
        if named || is_computed(program, *field) {
            continue;
        }
        return Err(match initializer(program, *field) {
            Some(FieldInitializer::Default(_)) => Stop::NotYet("table field default (H6 slice B3)"),
            _ => Stop::Error("ORNA-EVAL-TABLE-ROW"),
        });
    }
    Ok(())
}

/// The key of `fields` in the table's declared key order.
fn key_of_row(
    program: &orna_hir::Program,
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
fn field_named(program: &orna_hir::Program, schema: &TableDef, name: &Name) -> Option<DefId> {
    schema
        .fields
        .iter()
        .copied()
        .find(|field| field_text(program, *field) == name.as_str())
}

fn field_text(program: &orna_hir::Program, field: DefId) -> &str {
    program.defs[field].name.as_str()
}

fn initializer(program: &orna_hir::Program, field: DefId) -> Option<FieldInitializer> {
    match &program.defs[field].kind {
        DefKind::Field(def) => def.initializer,
        _ => None,
    }
}

fn is_computed(program: &orna_hir::Program, field: DefId) -> bool {
    matches!(
        initializer(program, field),
        Some(FieldInitializer::Computed(_))
    )
}

/// A store failure other than a violated expectation. The executor's own calls keep the store's
/// activation open and its key lookups valid, so reaching this is an invariant break, not a language error.
pub(crate) fn store_invariant(_error: &StoreError) -> Stop {
    Stop::NotYet("table store invariant (H6)")
}
