//! The pipeline a relation call denotes: its source and its stages.
//!
//! A pipeline is built from the HIR of the call, not evaluated stage by stage. The stages are recorded upstream
//! first. Rows are then pushed through the stages one at a time into a sink, and a sink or a stage that is done
//! stops the source, so no row past the last one a consumer needs is read.

use std::rc::Rc;

use num_bigint::Sign;
use num_traits::ToPrimitive;
use orna_hir::{ArgValue, Body, Call, Callee, DefId, DefKind, ExprId, ExprKind, RelOp};

use crate::error::Stop;
use crate::frame::Frame;
use crate::machine::Machine;
use crate::raw_value;
use crate::store::{CursorId, Direction, KeyBounds};
use crate::table::{decode_row, store_invariant};
use crate::value::Value;

/// The consumer of one row. It returns `false` to stop the pipeline.
pub(super) type Sink<'a, 'p> = dyn FnMut(&mut Machine<'p>, Value) -> Result<bool, Stop> + 'a;

/// Whether `op` is a stage a pipeline carries, rather than a terminal or a constructor.
pub(super) fn is_stage(op: RelOp) -> bool {
    matches!(op, RelOp::Filter | RelOp::Map | RelOp::Take | RelOp::Drop)
}

/// Where a pipeline's rows come from.
enum Source {
    /// Every row of a table, in ascending key order.
    Table(DefId),
    /// The elements of a list value.
    Rows(Rc<[Value]>),
}

/// One stage a row passes through.
enum Stage {
    /// Passes a row on when its predicate is true.
    Filter(Value),
    /// Passes the transform of each row.
    Map(Value),
    /// Passes the first `n` rows, then ends the pipeline.
    Take(usize),
    /// Skips the first `n` rows.
    Drop(usize),
}

/// Whether the pipeline continues after a row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// A source and the stages applied to its rows, in order.
pub(super) struct Pipeline {
    source: Source,
    stages: Vec<Stage>,
}

impl Pipeline {
    /// The pipeline ending in the stage call `call`, which is `op` over its input.
    pub(super) fn of_stage<'p>(
        machine: &mut Machine<'p>,
        body: &'p Body,
        frame: &mut Frame,
        call: &Call,
        op: RelOp,
    ) -> Result<Self, Stop> {
        let mut stages = Vec::new();
        let source = append(machine, body, frame, call, op, &mut stages)?;
        Ok(Self { source, stages })
    }

    /// The pipeline whose rows are the value of the expression `input`.
    pub(super) fn of_input<'p>(
        machine: &mut Machine<'p>,
        body: &'p Body,
        frame: &mut Frame,
        input: ExprId,
    ) -> Result<Self, Stop> {
        let mut stages = Vec::new();
        let source = source_of(machine, body, frame, input, &mut stages)?;
        Ok(Self { source, stages })
    }

    /// Pushes every row of the source through the stages into `sink`, until the sink or a stage stops it.
    pub(super) fn drive<'p>(
        &self,
        machine: &mut Machine<'p>,
        sink: &mut Sink<'_, 'p>,
    ) -> Result<(), Stop> {
        // A `take(0)` anywhere means no row can reach the end, so no row is read.
        if self
            .stages
            .iter()
            .any(|stage| matches!(stage, Stage::Take(0)))
        {
            return Ok(());
        }
        let mut counts = vec![0usize; self.stages.len()];
        match &self.source {
            Source::Rows(items) => {
                for row in items.iter() {
                    if push(machine, &self.stages, &mut counts, 0, row.clone(), sink)? == Flow::Stop
                    {
                        break;
                    }
                }
                Ok(())
            }
            Source::Table(table) => scan_table(machine, *table, &self.stages, &mut counts, sink),
        }
    }
}

/// Appends the stages of the stage call `call` and its input to `stages`, returning the source.
fn append<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
    op: RelOp,
    stages: &mut Vec<Stage>,
) -> Result<Source, Stop> {
    let input = expr_argument(call, 0)?;
    let source = source_of(machine, body, frame, input, stages)?;
    let stage = match op {
        RelOp::Filter => Stage::Filter(argument_value(machine, body, frame, call, 1)?),
        RelOp::Map => Stage::Map(argument_value(machine, body, frame, call, 1)?),
        RelOp::Take => Stage::Take(count_argument(machine, body, frame, call)?),
        RelOp::Drop => Stage::Drop(count_argument(machine, body, frame, call)?),
        _ => return Err(Stop::NotYet("relation stage (H7)")),
    };
    stages.push(stage);
    Ok(source)
}

/// The source of `input`. A stage call is part of the same pipeline, so it is appended and not evaluated.
fn source_of<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    input: ExprId,
    stages: &mut Vec<Stage>,
) -> Result<Source, Stop> {
    let program = machine.program;
    match &body.exprs[input].kind {
        ExprKind::Call(call) => match &call.callee {
            Callee::Relational { op } if is_stage(*op) => {
                machine.tick(body.exprs[input].weight)?;
                append(machine, body, frame, call, *op, stages)
            }
            _ => evaluate_rows(machine, body, frame, input),
        },
        ExprKind::Def(def) if matches!(program.defs[*def].kind, DefKind::Table(_)) => {
            Ok(Source::Table(*def))
        }
        _ => evaluate_rows(machine, body, frame, input),
    }
}

/// Evaluates `input` to a list, whose elements are the pipeline's rows.
fn evaluate_rows<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    input: ExprId,
) -> Result<Source, Stop> {
    match machine.eval(body, frame, input)? {
        Value::List(items) => Ok(Source::Rows(items)),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The expression of argument `slot` of `call`. A missing or defaulted argument is an argument error.
fn expr_argument(call: &Call, slot: usize) -> Result<ExprId, Stop> {
    match call
        .args
        .iter()
        .find(|argument| argument.slot.as_u32() as usize == slot)
    {
        Some(argument) => match argument.value {
            ArgValue::Expr(id) => Ok(id),
            ArgValue::Default => Err(Stop::Error("ORNA-EVAL-ARGUMENT")),
        },
        None => Err(Stop::Error("ORNA-EVAL-ARGUMENT")),
    }
}

/// The value of argument `slot` of `call`.
fn argument_value<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
    slot: usize,
) -> Result<Value, Stop> {
    let id = expr_argument(call, slot)?;
    machine.eval(body, frame, id)
}

/// The count of a `take` or `drop`: a nonnegative integer that fits the machine.
fn count_argument<'p>(
    machine: &mut Machine<'p>,
    body: &'p Body,
    frame: &mut Frame,
    call: &Call,
) -> Result<usize, Stop> {
    let Value::Int(count) = argument_value(machine, body, frame, call, 1)? else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if count.sign() == Sign::Minus {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    count.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))
}

/// Walks every row of `table` through the stages, closing the cursor on every path.
fn scan_table<'p>(
    machine: &mut Machine<'p>,
    table: DefId,
    stages: &[Stage],
    counts: &mut [usize],
    sink: &mut Sink<'_, 'p>,
) -> Result<(), Stop> {
    let cursor = machine
        .store
        .scan(table, KeyBounds::all(), Direction::Forward)
        .map_err(|error| store_invariant(&error))?;
    let outcome = walk_table(machine, cursor, stages, counts, sink);
    machine.store.close(cursor);
    outcome
}

/// Reads the rows of an open cursor one at a time, charging a step for each, until the pipeline stops.
fn walk_table<'p>(
    machine: &mut Machine<'p>,
    cursor: CursorId,
    stages: &[Stage],
    counts: &mut [usize],
    sink: &mut Sink<'_, 'p>,
) -> Result<(), Stop> {
    loop {
        machine.tick(1)?;
        let row = match machine
            .store
            .next(cursor)
            .map_err(|error| store_invariant(&error))?
        {
            None => return Ok(()),
            Some(stored) => raw_value::record(decode_row(stored.row)?),
        };
        if push(machine, stages, counts, 0, row, sink)? == Flow::Stop {
            return Ok(());
        }
    }
}

/// Passes `row` through the stages from `index` on, and into the sink after the last one.
fn push<'p>(
    machine: &mut Machine<'p>,
    stages: &[Stage],
    counts: &mut [usize],
    index: usize,
    row: Value,
    sink: &mut Sink<'_, 'p>,
) -> Result<Flow, Stop> {
    let Some(stage) = stages.get(index) else {
        return Ok(if sink(machine, row)? {
            Flow::Continue
        } else {
            Flow::Stop
        });
    };
    match stage {
        Stage::Filter(predicate) => {
            machine.tick(1)?;
            if truth(machine.call_closure(predicate, vec![row.clone()])?)? {
                push(machine, stages, counts, index + 1, row, sink)
            } else {
                Ok(Flow::Continue)
            }
        }
        Stage::Map(transform) => {
            let mapped = machine.call_closure(transform, vec![row])?;
            push(machine, stages, counts, index + 1, mapped, sink)
        }
        Stage::Drop(count) => {
            if counts[index] < *count {
                counts[index] += 1;
                Ok(Flow::Continue)
            } else {
                push(machine, stages, counts, index + 1, row, sink)
            }
        }
        Stage::Take(count) => {
            counts[index] += 1;
            let flow = push(machine, stages, counts, index + 1, row, sink)?;
            Ok(if counts[index] >= *count {
                Flow::Stop
            } else {
                flow
            })
        }
    }
}

/// The truth of a predicate's value. A predicate that does not yield a Bool is a type error.
pub(super) fn truth(value: Value) -> Result<bool, Stop> {
    match value {
        Value::Bool(flag) => Ok(flag),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}
