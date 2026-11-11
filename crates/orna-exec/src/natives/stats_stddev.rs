//! The `core.stats` standard deviation: the square root of the population variance. An exact square root is kept as it
//! is; otherwise the root is rounded half-even to the scale, and a missing scale is a value error. A copy of the old
//! evaluator's `standard_deviation` and `square_root` with their steps and errors.

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive};
use orna_hir::Ty;

use super::stats_decimal::{exact_result, numeric_decimal, options};
use super::stats_variance::variance_of;
use super::values::{exactly, list};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::Machine;
use crate::value::Value;

/// `standard_deviation<T>(rows: [T], scale: Int?, rounding: Str?): T?`. An empty list is `null`. Rows must all be
/// integers or decimals, or all floats.
pub(in crate::natives) fn standard_deviation(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [rows, scale, rounding] = exactly(arguments)?;
    let rows = list(rows)?;
    let scale = options(&machine.limits, &[scale, rounding])?;
    match variance_of(machine, &rows, scale)? {
        Value::Null => Ok(Value::Null),
        Value::Float(bits) => Ok(float::canonical(f64::from_bits(bits).sqrt())),
        variance @ (Value::Int(_) | Value::Decimal(_)) => {
            let preserve_decimal = matches!(variance, Value::Decimal(_));
            let variance = numeric_decimal(&variance)?;
            square_root(machine, &variance, scale, preserve_decimal)
        }
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The square root of a non-negative decimal: exact when it has one, otherwise rounded half-even to `scale`.
fn square_root(
    machine: &Machine<'_>,
    value: &Decimal,
    scale: Option<usize>,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    if value.coefficient().is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let mut coefficient = value.coefficient().clone();
    let mut exponent = value.exponent10().clone();
    if exponent.is_odd() {
        coefficient *= 10;
        exponent -= 1;
    }
    let root = coefficient.sqrt();
    if &root * &root == coefficient {
        let result = Decimal::new(root, exponent / 2)?;
        return exact_result(&machine.limits, result, preserve_decimal);
    }
    let Some(scale) = scale else {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    };
    let scale_i32 = i32::try_from(scale).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?;
    let shift = value.exponent10() + BigInt::from(scale_i32 * 2);
    let (numerator, denominator) = scaled_fraction(machine, value, &shift)?;
    let floor = (&numerator / &denominator).sqrt();
    let exact = &floor * &floor * &denominator == numerator;
    let rounded = if exact {
        floor
    } else {
        let midpoint_twice = &floor * 2 + 1;
        let lhs = &numerator * 4;
        let rhs = &denominator * &midpoint_twice * &midpoint_twice;
        if lhs > rhs || (lhs == rhs && floor.is_odd()) {
            floor + 1
        } else {
            floor
        }
    };
    exact_result(
        &machine.limits,
        Decimal::new(rounded, -BigInt::from(scale_i32))?,
        preserve_decimal,
    )
}

/// The coefficient over the power of ten that scales it by `shift`, as a numerator and denominator. Each power is
/// bounded by the integer digit limit.
fn scaled_fraction(
    machine: &Machine<'_>,
    value: &Decimal,
    shift: &BigInt,
) -> Result<(BigInt, BigInt), Stop> {
    let limit = machine.limits.max_integer_digits as u32;
    if shift.is_negative() {
        let power = (-shift)
            .to_u32()
            .filter(|power| *power <= limit)
            .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
        Ok((value.coefficient().clone(), BigInt::from(10u8).pow(power)))
    } else {
        let power = shift
            .to_u32()
            .filter(|power| *power <= limit)
            .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
        Ok((
            value.coefficient() * BigInt::from(10u8).pow(power),
            BigInt::from(1u8),
        ))
    }
}
