//! Unary and binary operators on runtime values.
//!
//! Operand shapes the executor does not run yet return [`Stop::NotYet`]; an
//! operand shape that the type pass should have rejected returns the old
//! evaluator's `ORNA-EVAL-TYPE`.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};
use orna_hir::{BinOp, UnOp};

use crate::decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::ExecLimits;
use crate::time;
use crate::value::Value;

pub(crate) fn unary(op: UnOp, operand: Value) -> Result<Value, Stop> {
    match (op, operand) {
        (UnOp::Neg, Value::Int(value)) => Ok(Value::Int(-value)),
        (UnOp::Neg, Value::Decimal(value)) => value.negate().map(Value::Decimal),
        (UnOp::Neg, Value::Float(bits)) => Ok(float::negate(bits)),
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
    if matches!(op, BinOp::Eq | BinOp::Ne) && (contains_closure(&lhs) || contains_closure(&rhs)) {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    }
    if matches!(op, BinOp::Eq | BinOp::Ne)
        && (matches!(lhs, Value::Null) || matches!(rhs, Value::Null))
    {
        let equal = matches!((&lhs, &rhs), (Value::Null, Value::Null));
        return Ok(Value::Bool(equal == matches!(op, BinOp::Eq)));
    }
    if time::is_temporal(&lhs) || time::is_temporal(&rhs) {
        return time::binary(op, lhs, rhs, limits);
    }
    match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => int_binary(op, &lhs, &rhs, limits),
        (Value::Decimal(lhs), Value::Decimal(rhs)) => decimal::binary(op, &lhs, &rhs),
        (Value::Float(lhs), Value::Float(rhs)) => float::binary(op, lhs, rhs),
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

/// Whether `value` is or contains a closure. The old evaluator refuses to
/// compare closures, however deeply they are nested.
pub(crate) fn contains_closure(value: &Value) -> bool {
    match value {
        Value::Closure(_) => true,
        Value::Tuple(items) | Value::List(items) => items.iter().any(contains_closure),
        Value::Record(fields) => fields.iter().any(|(_, field)| contains_closure(field)),
        _ => false,
    }
}

/// Integer operators with the old evaluator's semantics: truncating `/` and
/// `%`, a zero divisor is `ORNA-EVAL-DIVIDE-BY-ZERO`, a negative exponent is
/// `ORNA-EVAL-TYPE`, and every result is checked against the digit quota.
fn int_binary(op: BinOp, lhs: &BigInt, rhs: &BigInt, limits: &ExecLimits) -> Result<Value, Stop> {
    let result = match op {
        BinOp::Add => lhs + rhs,
        BinOp::Sub => lhs - rhs,
        BinOp::Mul => lhs * rhs,
        BinOp::Div | BinOp::Rem if rhs.is_zero() => {
            return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        BinOp::Div => lhs / rhs,
        BinOp::Rem => lhs % rhs,
        BinOp::Pow => return integer_power(lhs.clone(), rhs, limits).map(Value::Int),
        BinOp::Eq => return Ok(Value::Bool(lhs == rhs)),
        BinOp::Ne => return Ok(Value::Bool(lhs != rhs)),
        BinOp::Lt => return Ok(Value::Bool(lhs.cmp(rhs) == Ordering::Less)),
        BinOp::Le => return Ok(Value::Bool(lhs.cmp(rhs) != Ordering::Greater)),
        BinOp::Gt => return Ok(Value::Bool(lhs.cmp(rhs) == Ordering::Greater)),
        BinOp::Ge => return Ok(Value::Bool(lhs.cmp(rhs) != Ordering::Less)),
        BinOp::In => return Err(Stop::NotYet("membership is a Contains call after typeck")),
    };
    checked(result, limits).map(Value::Int)
}

/// Exponentiation by squaring. Each multiplication is checked against the
/// digit quota, so an oversized power ends as `ORNA-EVAL-LIMIT` instead of
/// growing without bound. Mirrors the old evaluator's `integer_power`.
fn integer_power(mut base: BigInt, exponent: &BigInt, limits: &ExecLimits) -> Result<BigInt, Stop> {
    if exponent.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    }
    let mut exponent = exponent.clone();
    let mut result = BigInt::from(1u8);
    while !exponent.is_zero() {
        if exponent.is_odd() {
            result = checked(result * &base, limits)?;
        }
        exponent /= 2u8;
        if !exponent.is_zero() {
            base = checked(&base * &base, limits)?;
        }
    }
    Ok(result)
}

/// Checks one integer result against the digit quota.
pub(crate) fn checked(value: BigInt, limits: &ExecLimits) -> Result<BigInt, Stop> {
    if value.magnitude().to_str_radix(10).len() > limits.max_integer_digits {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(value)
    }
}
