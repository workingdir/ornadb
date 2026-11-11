//! Instant and Duration operators, with the old evaluator's temporal rules.
//!
//! An Instant is `unix_seconds` plus a nanosecond within the second; a Duration is floor-normalised whole seconds plus
//! a nonnegative nanosecond tail. Both are compared and combined on the nanosecond grid. Duration storage has a one
//! nanosecond quantum, so a scale or divide that leaves a remainder is `ORNA-EVAL-VALUE` rather than a rounding.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_hir::BinOp;

use crate::decimal::{Decimal, MAX_EXPONENT};
use crate::error::Stop;
use crate::machine::ExecLimits;
use crate::ops::checked;
use crate::value::Value;

const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// Whether `value` is an Instant or a Duration, so the operator belongs to this module.
pub(crate) fn is_temporal(value: &Value) -> bool {
    matches!(value, Value::Instant { .. } | Value::Duration { .. })
}

/// A binary operator where at least one operand is an Instant or a Duration. Equality is structural and a mixed pair is
/// unequal, as in the old evaluator.
pub(crate) fn binary(
    op: BinOp,
    lhs: Value,
    rhs: Value,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    match op {
        BinOp::Eq => return Ok(Value::Bool(same_time(&lhs, &rhs))),
        BinOp::Ne => return Ok(Value::Bool(!same_time(&lhs, &rhs))),
        _ => {}
    }
    match (lhs, rhs) {
        (
            Value::Instant {
                unix_seconds,
                nanosecond,
            },
            Value::Duration {
                seconds,
                nanosecond: span,
            },
        ) if matches!(op, BinOp::Add | BinOp::Sub) => {
            instant_offset(unix_seconds, nanosecond, &seconds, span, op == BinOp::Sub)
        }
        (
            Value::Instant {
                unix_seconds: a_seconds,
                nanosecond: a_nanosecond,
            },
            Value::Instant {
                unix_seconds: b_seconds,
                nanosecond: b_nanosecond,
            },
        ) if op == BinOp::Sub => duration_from_total(
            instant_total(a_seconds, a_nanosecond) - instant_total(b_seconds, b_nanosecond),
            limits,
        ),
        (
            Value::Instant {
                unix_seconds: a_seconds,
                nanosecond: a_nanosecond,
            },
            Value::Instant {
                unix_seconds: b_seconds,
                nanosecond: b_nanosecond,
            },
        ) => ordering_result(
            op,
            (a_seconds, a_nanosecond).cmp(&(b_seconds, b_nanosecond)),
        ),
        (
            Value::Duration {
                seconds,
                nanosecond,
            },
            Value::Instant {
                unix_seconds,
                nanosecond: instant_nanosecond,
            },
        ) if op == BinOp::Add => instant_offset(
            unix_seconds,
            instant_nanosecond,
            &seconds,
            nanosecond,
            false,
        ),
        (
            Value::Duration {
                seconds: a_seconds,
                nanosecond: a_nanosecond,
            },
            Value::Duration {
                seconds: b_seconds,
                nanosecond: b_nanosecond,
            },
        ) => duration_pair(
            op,
            &a_seconds,
            a_nanosecond,
            &b_seconds,
            b_nanosecond,
            limits,
        ),
        pair => scaled(op, pair, limits),
    }
}

/// Scales a duration by an integer or a decimal, or divides it by one. Any other pair is a type error.
fn scaled(op: BinOp, pair: (Value, Value), limits: &ExecLimits) -> Result<Value, Stop> {
    match pair {
        (
            Value::Duration {
                seconds,
                nanosecond,
            },
            Value::Int(factor),
        ) if op == BinOp::Mul => {
            duration_from_total(duration_total(&seconds, nanosecond) * factor, limits)
        }
        (
            Value::Int(factor),
            Value::Duration {
                seconds,
                nanosecond,
            },
        ) if op == BinOp::Mul => {
            duration_from_total(factor * duration_total(&seconds, nanosecond), limits)
        }
        (
            Value::Duration {
                seconds,
                nanosecond,
            },
            Value::Int(divisor),
        ) if op == BinOp::Div => duration_int_divide(&seconds, nanosecond, &divisor, limits),
        (
            Value::Duration {
                seconds,
                nanosecond,
            },
            Value::Decimal(scalar),
        ) if op == BinOp::Mul => {
            duration_decimal_scale(duration_total(&seconds, nanosecond), &scalar, false, limits)
        }
        (
            Value::Decimal(scalar),
            Value::Duration {
                seconds,
                nanosecond,
            },
        ) if op == BinOp::Mul => {
            duration_decimal_scale(duration_total(&seconds, nanosecond), &scalar, false, limits)
        }
        (
            Value::Duration {
                seconds,
                nanosecond,
            },
            Value::Decimal(scalar),
        ) if op == BinOp::Div => {
            duration_decimal_scale(duration_total(&seconds, nanosecond), &scalar, true, limits)
        }
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// Structural equality of two temporal values. A mixed pair is unequal.
fn same_time(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (
            Value::Instant {
                unix_seconds: a_seconds,
                nanosecond: a_nanosecond,
            },
            Value::Instant {
                unix_seconds: b_seconds,
                nanosecond: b_nanosecond,
            },
        ) => a_seconds == b_seconds && a_nanosecond == b_nanosecond,
        (
            Value::Duration {
                seconds: a_seconds,
                nanosecond: a_nanosecond,
            },
            Value::Duration {
                seconds: b_seconds,
                nanosecond: b_nanosecond,
            },
        ) => a_seconds == b_seconds && a_nanosecond == b_nanosecond,
        _ => false,
    }
}

/// The ordering operators of one temporal family. Any other operator on a pair of the same family is unsupported.
fn ordering_result(op: BinOp, ordering: Ordering) -> Result<Value, Stop> {
    match op {
        BinOp::Lt => Ok(Value::Bool(ordering.is_lt())),
        BinOp::Le => Ok(Value::Bool(ordering.is_le())),
        BinOp::Gt => Ok(Value::Bool(ordering.is_gt())),
        BinOp::Ge => Ok(Value::Bool(ordering.is_ge())),
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

fn instant_total(unix_seconds: i64, nanosecond: u32) -> BigInt {
    BigInt::from(unix_seconds) * NANOS_PER_SECOND + nanosecond
}

fn duration_total(seconds: &BigInt, nanosecond: u32) -> BigInt {
    seconds * NANOS_PER_SECOND + nanosecond
}

/// Shifts an instant by a duration. The result must fit the i64 seconds axis, else `ORNA-EVAL-VALUE`.
fn instant_offset(
    unix_seconds: i64,
    nanosecond: u32,
    span_seconds: &BigInt,
    span_nanosecond: u32,
    subtract: bool,
) -> Result<Value, Stop> {
    let instant = instant_total(unix_seconds, nanosecond);
    let span = duration_total(span_seconds, span_nanosecond);
    let total = if subtract {
        instant - span
    } else {
        instant + span
    };
    let (seconds, nanosecond) = total.div_mod_floor(&BigInt::from(NANOS_PER_SECOND));
    let unix_seconds = seconds.to_i64().ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    let nanosecond = nanosecond.to_u32().ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    Ok(Value::Instant {
        unix_seconds,
        nanosecond,
    })
}

/// Adds or subtracts two durations on the nanosecond grid.
fn duration_pair(
    op: BinOp,
    lhs_seconds: &BigInt,
    lhs_nanosecond: u32,
    rhs_seconds: &BigInt,
    rhs_nanosecond: u32,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    let lhs = duration_total(lhs_seconds, lhs_nanosecond);
    let rhs = duration_total(rhs_seconds, rhs_nanosecond);
    match op {
        BinOp::Add => duration_from_total(lhs + rhs, limits),
        BinOp::Sub => duration_from_total(lhs - rhs, limits),
        _ => ordering_result(
            op,
            (lhs_seconds, lhs_nanosecond).cmp(&(rhs_seconds, rhs_nanosecond)),
        ),
    }
}

/// Divides a duration by an integer. The quotient must be exact on the nanosecond grid.
fn duration_int_divide(
    seconds: &BigInt,
    nanosecond: u32,
    divisor: &BigInt,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    if divisor.is_zero() {
        return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
    }
    let (quotient, remainder) = duration_total(seconds, nanosecond).div_rem(divisor);
    if !remainder.is_zero() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    duration_from_total(quotient, limits)
}

/// Multiplies or divides a duration by a decimal, exactly. The decimal's power of ten moves to the numerator or the
/// denominator; a remainder or a power beyond [`MAX_EXPONENT`] is refused.
fn duration_decimal_scale(
    total: BigInt,
    scalar: &Decimal,
    divide: bool,
    limits: &ExecLimits,
) -> Result<Value, Stop> {
    let coefficient = scalar.coefficient();
    let exponent = scalar.exponent10();
    checked(coefficient.clone(), limits)?;
    checked(exponent.clone(), limits)?;
    if divide && coefficient.is_zero() {
        return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
    }
    let (mut numerator, mut denominator) = if divide {
        (total, coefficient.clone())
    } else {
        (total * coefficient, BigInt::from(1u8))
    };
    let shift = if divide { -exponent } else { exponent.clone() };
    let power = shift
        .abs()
        .to_usize()
        .filter(|power| *power <= MAX_EXPONENT)
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let scale = BigInt::from(10u8).pow(power as u32);
    if shift.is_negative() {
        denominator *= scale;
    } else {
        numerator *= scale;
    }
    let (quotient, remainder) = numerator.div_rem(&denominator);
    if !remainder.is_zero() {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    duration_from_total(quotient, limits)
}

/// A duration from a total on the nanosecond grid, floor-normalised. The whole seconds are checked against the digit
/// quota.
fn duration_from_total(total: BigInt, limits: &ExecLimits) -> Result<Value, Stop> {
    let (seconds, nanosecond) = total.div_mod_floor(&BigInt::from(NANOS_PER_SECOND));
    let nanosecond = nanosecond.to_u32().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    Ok(Value::Duration {
        seconds: checked(seconds, limits)?,
        nanosecond,
    })
}

#[cfg(test)]
mod tests {
    use super::{binary, duration_from_total};
    use crate::decimal::Decimal;
    use crate::error::Stop;
    use crate::machine::ExecLimits;
    use crate::value::Value;
    use num_bigint::BigInt;
    use orna_hir::BinOp;

    const LIMITS: ExecLimits = ExecLimits {
        max_steps: 1_000,
        max_depth: 8,
        max_collection_items: 2_048,
        max_string_bytes: 64,
        max_integer_digits: 40,
    };

    fn span(seconds: i64, nanosecond: u32) -> Value {
        Value::Duration {
            seconds: BigInt::from(seconds),
            nanosecond,
        }
    }

    fn dec(coefficient: i64, exponent: i64) -> Value {
        Value::Decimal(
            Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("in range"),
        )
    }

    fn error_code(result: Result<Value, Stop>) -> &'static str {
        match result {
            Err(Stop::Error(code)) => code,
            _ => "no error",
        }
    }

    #[test]
    fn negative_duration_floors_to_nonnegative_tail() {
        let value = duration_from_total(BigInt::from(-1), &LIMITS).expect("in range");
        assert!(matches!(
            value,
            Value::Duration { ref seconds, nanosecond: 999_999_999 } if *seconds == BigInt::from(-1)
        ));
    }

    #[test]
    fn duration_times_decimal_is_exact() {
        let value = binary(BinOp::Mul, span(2, 0), dec(15, -1), &LIMITS).expect("exact");
        assert!(
            matches!(value, Value::Duration { ref seconds, nanosecond: 0 } if *seconds == BigInt::from(3))
        );
    }

    #[test]
    fn duration_divide_remainder_is_refused() {
        let result = binary(BinOp::Div, span(1, 0), Value::Int(BigInt::from(3)), &LIMITS);
        assert_eq!(error_code(result), "ORNA-EVAL-VALUE");
    }

    #[test]
    fn duration_divide_by_zero_is_refused() {
        let result = binary(BinOp::Div, span(1, 0), Value::Int(BigInt::from(0)), &LIMITS);
        assert_eq!(error_code(result), "ORNA-EVAL-DIVIDE-BY-ZERO");
    }

    #[test]
    fn instant_out_of_i64_seconds_is_a_value_error() {
        let instant = Value::Instant {
            unix_seconds: i64::MAX,
            nanosecond: 0,
        };
        let result = binary(BinOp::Add, instant, span(1, 0), &LIMITS);
        assert_eq!(error_code(result), "ORNA-EVAL-VALUE");
    }

    #[test]
    fn instant_difference_is_a_duration() {
        let later = Value::Instant {
            unix_seconds: 10,
            nanosecond: 500,
        };
        let earlier = Value::Instant {
            unix_seconds: 9,
            nanosecond: 0,
        };
        let value = binary(BinOp::Sub, later, earlier, &LIMITS).expect("difference");
        assert!(
            matches!(value, Value::Duration { ref seconds, nanosecond: 500 } if *seconds == BigInt::from(1))
        );
    }

    #[test]
    fn mixed_temporal_equality_is_false() {
        let value = binary(BinOp::Eq, span(1, 0), Value::Int(BigInt::from(1)), &LIMITS)
            .expect("structural");
        assert!(matches!(value, Value::Bool(false)));
    }
}
