//! The `core.bits` natives: bitwise operations and shifts on integers, copied from the old evaluator's `bits` family.

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

pub(super) fn bit_or(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let (lhs, rhs) = binary(arguments)?;
    integer(machine, lhs | rhs)
}

pub(super) fn bit_and(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let (lhs, rhs) = binary(arguments)?;
    integer(machine, lhs & rhs)
}

pub(super) fn bit_xor(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let (lhs, rhs) = binary(arguments)?;
    integer(machine, lhs ^ rhs)
}

/// The bitwise complement, which for an integer is `-value - 1`.
pub(super) fn bit_not(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let value = unary(arguments)?;
    integer(machine, -value - BigInt::from(1))
}

/// `value` shifted left by `count` bits. A negative count is a value error, and a shift past the digit limit is a
/// limit error.
pub(super) fn shift_left(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let (value, count) = binary(arguments)?;
    if count.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    if value.is_zero() {
        return integer(machine, value);
    }
    let bit_limit = u64::try_from(machine.limits.max_integer_digits.saturating_mul(4)).unwrap_or(u64::MAX);
    let shift = count
        .to_u64()
        .filter(|shift| value.bits().saturating_add(*shift) <= bit_limit)
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let shift = usize::try_from(shift).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?;
    integer(machine, value << shift)
}

/// `value` shifted right by `count` bits, arithmetically. A count at or past the bit length leaves `0`, or `-1` for a
/// negative value. A negative count is a value error.
pub(super) fn shift_right(machine: &mut Machine<'_>, arguments: Vec<Value>) -> Result<Value, Stop> {
    let (value, count) = binary(arguments)?;
    if count.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let bits = value.bits();
    let shifted = match count.to_u64() {
        Some(shift) if shift < bits => value >> usize::try_from(shift).unwrap_or(usize::MAX),
        _ if value.is_negative() => BigInt::from(-1),
        _ => BigInt::zero(),
    };
    integer(machine, shifted)
}

/// The integer `value` as a value, or `ORNA-EVAL-LIMIT` when its decimal form exceeds the digit limit.
fn integer(machine: &Machine<'_>, value: BigInt) -> Result<Value, Stop> {
    if value.to_str_radix(10).len() > machine.limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(Value::Int(value))
}

/// The two integer arguments of a binary native, or an error for any other arity or type.
fn binary(arguments: Vec<Value>) -> Result<(BigInt, BigInt), Stop> {
    let [lhs, rhs] = <[Value; 2]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
    Ok((int(lhs)?, int(rhs)?))
}

/// The one integer argument of a unary native, or an error for any other arity or type.
fn unary(arguments: Vec<Value>) -> Result<BigInt, Stop> {
    let [value] = <[Value; 1]>::try_from(arguments).map_err(|_| Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
    int(value)
}

fn int(value: Value) -> Result<BigInt, Stop> {
    match value {
        Value::Int(value) => Ok(value),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}
