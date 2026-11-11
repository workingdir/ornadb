//! Half-even rounding of exact decimals for the `core.stats` natives: a rescale to a scale, a divide rounded to a scale,
//! and the floor-and-fraction split that percentile interpolation reads. A copy of the old evaluator's
//! `round_to_scale`, `divide_rounded` and `decimal_floor_fraction`, with the same error codes.

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::ExecLimits;

/// A power of ten the limits allow, or `ORNA-EVAL-LIMIT`.
fn ten_pow(power: usize, limits: &ExecLimits) -> Result<BigInt, Stop> {
    if power > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    Ok(BigInt::from(10u8).pow(power as u32))
}

/// A decimal exponent as a count, or `ORNA-EVAL-LIMIT` when it is too large to count.
fn count(value: &BigInt) -> Result<usize, Stop> {
    value.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))
}

/// Whether a truncated quotient moves away from zero: the remainder is past half the divisor, or exactly half with an
/// odd quotient.
fn rounds_away(remainder: &BigInt, divisor: &BigInt, quotient: &BigInt) -> bool {
    let twice = remainder.abs() * 2u8;
    twice > *divisor || (twice == *divisor && (quotient % 2u8).abs() == BigInt::from(1u8))
}

/// The decimal rounded half-even to `scale` digits after the point.
pub(super) fn round_to_scale(
    limits: &ExecLimits,
    value: &Decimal,
    scale: usize,
) -> Result<Decimal, Stop> {
    if scale > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let shift = value.exponent10() + BigInt::from(scale);
    let mut coefficient = value.coefficient().clone();
    if shift.sign() != Sign::Minus {
        coefficient *= ten_pow(count(&shift)?, limits)?;
    } else {
        let divisor = ten_pow(count(&-&shift)?, limits)?;
        let (mut quotient, remainder) = coefficient.div_rem(&divisor);
        if !remainder.is_zero() && rounds_away(&remainder, &divisor, &quotient) {
            if value.coefficient().sign() == Sign::Minus {
                quotient -= 1;
            } else {
                quotient += 1;
            }
        }
        coefficient = quotient;
    }
    Decimal::new(coefficient, -BigInt::from(scale))
}

/// The exact quotient rounded half-even to `scale` digits after the point. A zero divisor is an error.
pub(super) fn divide_rounded(
    limits: &ExecLimits,
    left: &Decimal,
    right: &Decimal,
    scale: usize,
) -> Result<Decimal, Stop> {
    if right.coefficient().is_zero() {
        return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
    }
    if scale > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let gcd = left.coefficient().gcd(right.coefficient());
    let mut numerator = left.coefficient() / &gcd;
    let mut denominator = (right.coefficient() / &gcd).abs();
    if right.coefficient().sign() == Sign::Minus {
        numerator = -numerator;
    }
    let power = left.exponent10() - right.exponent10() + BigInt::from(scale);
    if power.sign() != Sign::Minus {
        numerator *= ten_pow(count(&power)?, limits)?;
    } else {
        denominator *= ten_pow(count(&-&power)?, limits)?;
    }
    let (mut quotient, remainder) = numerator.div_rem(&denominator);
    if !remainder.is_zero() && rounds_away(&remainder, &denominator, &quotient) {
        if numerator.sign() == Sign::Minus {
            quotient -= 1;
        } else {
            quotient += 1;
        }
    }
    Decimal::new(quotient, -BigInt::from(scale))
}
