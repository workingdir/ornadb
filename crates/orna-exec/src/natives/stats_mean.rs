//! `core.stats.mean`: the arithmetic mean of a list, with the old evaluator's exact decimal rules. Integer rows give an
//! integer when the mean is whole; otherwise `scale` and `rounding` round the quotient half to even. Decimal rows keep
//! their decimal form. Float rows average in order, with the canonical NaN.

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_hir::Ty;

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::float;
use crate::machine::{ExecLimits, Machine};
use crate::natives::values::list;
use crate::value::Value;

/// `mean<T>(rows: [T], scale: Int? = null, rounding: Str? = null): T?`. `null` for no rows. One item-limit check
/// over the rows; no per-row steps, as the old evaluator charged none.
pub(in crate::natives) fn mean(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let mut arguments = arguments.into_iter();
    let rows = list(arguments.next().ok_or(Stop::Error("ORNA-EVAL-TYPE"))?)?;
    let options: Vec<Value> = arguments.collect();
    let scale = rounding(&machine.limits, &options)?;
    if rows.len() > machine.limits.max_collection_items {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let Some(head) = rows.first() else {
        return Ok(Value::Null);
    };
    let count = Decimal::new(BigInt::from(rows.len()), BigInt::zero())?;
    match head {
        Value::Int(_) if rows.iter().all(|value| matches!(value, Value::Int(_))) => {
            let mut total = BigInt::zero();
            for value in rows.iter() {
                if let Value::Int(value) = value {
                    let next = total + value;
                    check_digits(&machine.limits, &next)?;
                    total = next;
                }
            }
            divide_stats(
                &machine.limits,
                &Decimal::new(total, BigInt::zero())?,
                &count,
                scale,
                false,
            )
        }
        Value::Decimal(first) if rows.iter().all(|value| matches!(value, Value::Decimal(_))) => {
            let mut total = first.clone();
            for value in rows.iter().skip(1) {
                if let Value::Decimal(value) = value {
                    total = total.add(value)?;
                }
            }
            divide_stats(&machine.limits, &total, &count, scale, true)
        }
        Value::Float(_) if rows.iter().all(|value| matches!(value, Value::Float(_))) => {
            float_mean(&rows)
        }
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// The mean of float rows, summed in order from the first row.
fn float_mean(rows: &[Value]) -> Result<Value, Stop> {
    let mut floats = rows.iter().map(|value| match value {
        Value::Float(bits) => Ok(f64::from_bits(*bits)),
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    });
    let mut total = floats
        .next()
        .ok_or(Stop::Error("ORNA-EVAL-UNSUPPORTED"))??;
    for value in floats {
        total += value?;
    }
    Ok(float::canonical(total / rows.len() as f64))
}

/// The rounding of a stats call: `None` when both options are absent, otherwise the scale. The only rounding mode is
/// `half_even`.
fn rounding(limits: &ExecLimits, options: &[Value]) -> Result<Option<usize>, Stop> {
    if options.iter().all(|value| present(value).is_none()) {
        return Ok(None);
    }
    let [scale, rounding] = options else {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    };
    let Some(Value::Int(scale)) = present(scale) else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if scale.is_negative() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let scale = scale.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    if scale > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let Some(Value::Text(rounding)) = present(rounding) else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if rounding.as_str() != "half_even" {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    Ok(Some(scale))
}

/// The value inside an optional argument; `None` for an absent one.
fn present(value: &Value) -> Option<&Value> {
    match value {
        Value::Null | Value::Option(None) => None,
        Value::Option(Some(inner)) => Some(inner.as_ref()),
        other => Some(other),
    }
}

/// The quotient `left / right`. An inexact quotient is an error unless a scale rounds it; the rounded quotient is
/// rounded to the scale when one is given.
fn divide_stats(
    limits: &ExecLimits,
    left: &Decimal,
    right: &Decimal,
    scale: Option<usize>,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    match left.divide(right) {
        Ok(value) => {
            let value = match scale {
                Some(scale) => value.round_to_scale(scale)?,
                None => value,
            };
            exact_result(limits, &value, preserve_decimal)
        }
        Err(Stop::Error(code)) if code == "InexactDivision" || code == "ORNA-EVAL-VALUE" => {
            let Some(scale) = scale else {
                return Err(Stop::Error("ORNA-EVAL-VALUE"));
            };
            exact_result(
                limits,
                &left.divide_rounded(right, scale)?,
                preserve_decimal,
            )
        }
        Err(failure) => Err(failure),
    }
}

/// A decimal result as the old evaluator returns it: a decimal when it must stay one, otherwise the integer it names.
fn exact_result(
    limits: &ExecLimits,
    value: &Decimal,
    preserve_decimal: bool,
) -> Result<Value, Stop> {
    check_digits(limits, value.coefficient())?;
    check_digits(limits, value.exponent10())?;
    if preserve_decimal || value.exponent10().is_negative() {
        return Ok(Value::Decimal(value.clone()));
    }
    let power = value
        .exponent10()
        .to_u32()
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let integer = value.coefficient() * BigInt::from(10u8).pow(power);
    check_digits(limits, &integer)?;
    Ok(Value::Int(integer))
}

/// `ORNA-EVAL-LIMIT` when `value` has more digits than the integer limit allows.
fn check_digits(limits: &ExecLimits, value: &BigInt) -> Result<(), Stop> {
    if value.to_str_radix(10).len() > limits.max_integer_digits {
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
