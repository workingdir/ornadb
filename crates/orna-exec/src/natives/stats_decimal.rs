//! Shared decimal and option helpers for the `core.stats` natives: the `scale` and `rounding` options, the decimal view
//! of a numeric row, the integer-or-decimal result the old evaluator returns, the exact-or-rounded divide, and the
//! collection size check. A copy of the old evaluator's `stats_options`, `exact_decimal_result`, `divide_stats`,
//! `numeric_decimal` and `items`.

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use super::stats_round::{divide_rounded, round_to_scale};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::{ExecLimits, Machine};
use crate::ops::checked;
use crate::value::Value;

/// The `scale` and `rounding` options as the old evaluator reads them: `None` when both are absent, otherwise the
/// scale. Only `half_even` is a rounding mode the stats natives accept.
pub(super) fn options(limits: &ExecLimits, values: &[Value]) -> Result<Option<usize>, Stop> {
    if matches!(values, [Value::Null, Value::Null]) {
        return Ok(None);
    }
    let [Value::Int(scale), rounding] = values else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if scale.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let scale = scale.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    if scale > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let Value::Text(mode) = rounding else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if mode != "half_even" {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    Ok(Some(scale))
}

/// Refuses a collection of more elements than the limit allows.
pub(super) fn collection_items(machine: &Machine<'_>, count: usize) -> Result<(), Stop> {
    if count > machine.limits.max_collection_items {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}

/// The decimal view of an integer or decimal row: an integer is the decimal with exponent zero.
pub(super) fn numeric_decimal(value: &Value) -> Result<Decimal, Stop> {
    match value {
        Value::Int(value) => Decimal::new(value.clone(), BigInt::zero()),
        Value::Decimal(value) => Ok(value.clone()),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The result of an exact decimal: a decimal when `preserve_decimal` holds or the scale is negative, otherwise the
/// integer it denotes. Both the coefficient and the exponent must fit the digit quota.
pub(super) fn exact_result(
    limits: &ExecLimits,
    value: Decimal,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    checked(value.coefficient().clone(), limits)?;
    checked(value.exponent10().clone(), limits)?;
    if preserve_decimal || value.exponent10().is_negative() {
        return Ok(Value::Decimal(value));
    }
    let power = value
        .exponent10()
        .to_usize()
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let power = u32::try_from(power).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?;
    let integer = checked(value.coefficient() * BigInt::from(10u8).pow(power), limits)?;
    Ok(Value::Int(integer))
}

/// The quotient of two decimals. An exact quotient is kept as it is, rounded half-even to `options` when a scale is
/// given. A quotient without a finite expansion is rounded to the scale, and is a value error when no scale is given.
pub(super) fn divide_stats(
    limits: &ExecLimits,
    left: &Decimal,
    right: &Decimal,
    options: Option<usize>,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    match left.divide(right) {
        Ok(value) => {
            let value = match options {
                Some(scale) => round_to_scale(limits, &value, scale)?,
                None => value,
            };
            exact_result(limits, value, preserve_decimal)
        }
        Err(Stop::Error("InexactDivision" | "ORNA-EVAL-VALUE")) => {
            let Some(scale) = options else {
                return Err(Stop::Error("ORNA-EVAL-VALUE"));
            };
            exact_result(
                limits,
                divide_rounded(limits, left, right, scale)?,
                preserve_decimal,
            )
        }
        Err(failure) => Err(failure),
    }
}
