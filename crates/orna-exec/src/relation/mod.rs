//! Relation calls: a pipeline of stages over a table or a list, consumed by a terminal or collected into a list.
//!
//! The pipeline is pushed one row at a time (see [`pipeline`]). Each terminal is a sink that stops the pipeline as soon
//! as its answer is known, so `first`, `exists` and `every` never read past that row.

mod pipeline;

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{Body, Call, RelOp};

use crate::error::Stop;
use crate::frame::Frame;
use crate::machine::Machine;
use crate::value::Value;
use pipeline::{Pipeline, truth};

pub(crate) use pipeline::is_stage;

/// Evaluates the relation call `call` of `op`. A stage collects its rows into a list. A terminal consumes them.
pub(crate) fn relational<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
    op: RelOp,
) -> Result<Value, Stop> {
    if is_stage(op) {
        let pipeline = Pipeline::of_stage(machine, body, frame, call, op)?;
        return collect(machine, &pipeline);
    }
    match op {
        RelOp::Count => count(machine, body, frame, call),
        RelOp::First => first(machine, body, frame, call),
        RelOp::One => one(machine, body, frame, call),
        RelOp::Every => every(machine, body, frame, call),
        RelOp::Exists => exists(machine, body, frame, call),
        _ => Err(Stop::NotYet("relation operator (H7)")),
    }
}

/// The input pipeline of a terminal: argument 0.
fn input<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Pipeline, Stop> {
    let Some(argument) = call
        .args
        .iter()
        .find(|argument| argument.slot.as_u32() == 0)
    else {
        return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
    };
    let orna_hir::ArgValue::Expr(id) = argument.value else {
        return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
    };
    Pipeline::of_input(machine, body, frame, id)
}

/// The predicate of a terminal, if it is given as argument 1.
fn predicate<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Option<Value>, Stop> {
    let Some(argument) = call
        .args
        .iter()
        .find(|argument| argument.slot.as_u32() == 1)
    else {
        return Ok(None);
    };
    let orna_hir::ArgValue::Expr(id) = argument.value else {
        return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
    };
    machine.eval(body, frame, id).map(Some)
}

/// Every row of the pipeline, as a list.
fn collect(machine: &mut Machine<'_>, pipeline: &Pipeline) -> Result<Value, Stop> {
    let mut rows = Vec::new();
    pipeline.drive(machine, &mut |_, row| {
        rows.push(row);
        Ok(true)
    })?;
    Ok(Value::List(Rc::from(rows)))
}

/// The number of rows the pipeline yields.
fn count<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Value, Stop> {
    let pipeline = input(machine, body, frame, call)?;
    let mut total = 0usize;
    pipeline.drive(machine, &mut |_, _| {
        total += 1;
        Ok(true)
    })?;
    Ok(Value::Int(BigInt::from(total)))
}

/// The first row the pipeline yields, or none when it yields none.
fn first<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Value, Stop> {
    let pipeline = input(machine, body, frame, call)?;
    let mut found = None;
    pipeline.drive(machine, &mut |_, row| {
        found = Some(Rc::new(row));
        Ok(false)
    })?;
    Ok(Value::Option(found))
}

/// The only row the pipeline yields. It fails when the pipeline yields no row or more than one.
fn one<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Value, Stop> {
    let pipeline = input(machine, body, frame, call)?;
    let mut found = None;
    pipeline.drive(machine, &mut |_, row| {
        if found.is_some() {
            return Err(Stop::Error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
        }
        found = Some(row);
        Ok(true)
    })?;
    found.ok_or(Stop::Error("ORNA-EVAL-RELATION-ONE-ZERO"))
}

/// Whether every row satisfies the predicate. Without a predicate, the rows are Bools and each must be true.
fn every<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Value, Stop> {
    holds_for(machine, body, frame, call, false)
}

/// Whether some row satisfies the predicate. Without a predicate, the rows are Bools and one must be true.
fn exists<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<Value, Stop> {
    holds_for(machine, body, frame, call, true)
}

/// The shared walk of `every` and `exists`. It stops at the first row whose truth equals `decisive`: a failing row
/// settles `every` (`decisive` false), a satisfying row settles `exists` (`decisive` true).
fn holds_for<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
    decisive: bool,
) -> Result<Value, Stop> {
    let pipeline = input(machine, body, frame, call)?;
    let predicate = predicate(machine, body, frame, call)?;
    let mut stopped = false;
    pipeline.drive(machine, &mut |machine, row| {
        let holds = match &predicate {
            Some(callable) => {
                machine.tick(1)?;
                truth(machine.call_closure(callable, vec![row])?)?
            }
            None => truth(row)?,
        };
        if holds == decisive {
            stopped = true;
            return Ok(false);
        }
        Ok(true)
    })?;
    // A decisive row settles `exists` true and `every` false; with none, `every` holds and `exists` does not.
    Ok(Value::Bool(if decisive { stopped } else { !stopped }))
}
