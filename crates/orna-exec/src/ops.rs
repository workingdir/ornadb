//! Unary and binary operators on runtime values.
//!
//! Operand shapes the executor does not run yet return [`Stop::NotYet`]; an
//! operand shape that the type pass should have rejected returns the old
//! evaluator's `ORNA-EVAL-TYPE`.

use std::cmp::Ordering;

use orna_hir::{BinOp, UnOp};

use crate::error::Stop;
use crate::machine::ExecLimits;
use crate::value::Value;

pub(crate) fn unary(op: UnOp, operand: Value) -> Result<Value, Stop> {
    match (op, operand) {
        (UnOp::Neg, Value::Int(value)) => Ok(Value::Int(-value)),
        (UnOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

pub(crate) fn binary(
    op: BinOp,
    lhs: Value,
    rhs: Value,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => int_binary(op, &lhs, &rhs, limits),
        (Value::Bool(lhs), Value::Bool(rhs)) => match op {
            BinOp::Eq => Ok(Value::Bool(lhs == rhs)),
            BinOp::Ne => Ok(Value::Bool(lhs != rhs)),
            _ => Err(Stop::NotYet("operator on booleans")),
        },
        (Value::Text(lhs), Value::Text(rhs)) => match op {
            BinOp::Eq => Ok(Value::Bool(lhs == rhs)),
            BinOp::Ne => Ok(Value::Bool(lhs != rhs)),
            _ => Err(Stop::NotYet("operator on text")),
        },
        _ => Err(Stop::NotYet("operator on operands of different shapes")),
    }
}

fn int_binary(
    op: BinOp,
    lhs: &num_bigint::BigInt,
    rhs: &num_bigint::BigInt,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    let result = match op {
        BinOp::Add => lhs + rhs,
        BinOp::Sub => lhs - rhs,
        BinOp::Mul => lhs * rhs,
        BinOp::Eq => return Ok(Value::Bool(lhs == rhs)),
        BinOp::Ne => return Ok(Value::Bool(lhs != rhs)),
        BinOp::Lt => return Ok(Value::Bool(lhs.cmp(rhs) == Ordering::Less)),
        BinOp::Le => return Ok(Value::Bool(lhs.cmp(rhs) != Ordering::Greater)),
        BinOp::Gt => return Ok(Value::Bool(lhs.cmp(rhs) == Ordering::Greater)),
        BinOp::Ge => return Ok(Value::Bool(lhs.cmp(rhs) != Ordering::Less)),
        BinOp::Div | BinOp::Rem | BinOp::Pow => {
            return Err(Stop::NotYet("integer division or power"));
        }
    };
    if result.to_str_radix(10).len() > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Int(result))
}
