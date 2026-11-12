//! Rounding of exact decimals to a scale under a rounding mode, with the old evaluator's limits.
//!
//! `divide_rounded` is the quotient of two decimals rounded to a scale in one step. `round_to_scale` rounds an exact
//! decimal to a scale. Both keep the sign of the rounded value and fail `ORNA-EVAL-LIMIT` past [`MAX_EXPONENT`].

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::decimal::{Decimal, MAX_EXPONENT};
use crate::error::Stop;

impl Decimal {
    /// The quotient `self / other` rounded half to even to `scale` digits after the point.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-DIVIDE-BY-ZERO` for a zero divisor and `ORNA-EVAL-LIMIT` when the scale or an alignment power
    /// exceeds [`MAX_EXPONENT`].
    pub(crate) fn divide_rounded(&self, other: &Self, scale: usize) -> Result<Self, Stop> {
        self.divide_rounded_with(other, scale, RoundingMode::HalfEven)
    }

    /// The quotient `self / other` rounded to `scale` digits after the point under `mode`. The errors are those of
    /// [`Self::divide_rounded`].
    pub(crate) fn divide_rounded_with(
        &self,
        other: &Self,
        scale: usize,
        mode: RoundingMode,
    ) -> Result<Self, Stop> {
        if other.coefficient().is_zero() {
            return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        if scale > MAX_EXPONENT {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        let gcd = self.coefficient().gcd(other.coefficient());
        let mut numerator = self.coefficient() / &gcd;
        let mut denominator = (other.coefficient() / gcd).abs();
        if other.coefficient().sign() == Sign::Minus {
            numerator = -numerator;
        }
        let power = self.exponent10() - other.exponent10() + BigInt::from(scale);
        if power.sign() == Sign::Plus || power.is_zero() {
            numerator *= BigInt::from(10u8).pow(digits(&power)?);
        } else {
            denominator *= BigInt::from(10u8).pow(digits(&-power)?);
        }
        Self::new(
            round_with(mode, &numerator, &denominator),
            -BigInt::from(scale),
        )
    }

    /// `self` rounded half to even to `scale` digits after the point.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-LIMIT` when the scale or the shift exceeds [`MAX_EXPONENT`].
    pub(crate) fn round_to_scale(&self, scale: usize) -> Result<Self, Stop> {
        if scale > MAX_EXPONENT {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        let shift = self.exponent10() + BigInt::from(scale);
        let coefficient = if shift.sign() == Sign::Plus || shift.is_zero() {
            self.coefficient() * BigInt::from(10u8).pow(digits(&shift)?)
        } else {
            let divisor = BigInt::from(10u8).pow(digits(&-shift)?);
            round_with(RoundingMode::HalfEven, self.coefficient(), &divisor)
        };
        Self::new(coefficient, -BigInt::from(scale))
    }
}

/// The power of ten a shift needs: a non-negative count no larger than [`MAX_EXPONENT`].
fn digits(shift: &BigInt) -> Result<u32, Stop> {
    let count = shift
        .to_usize()
        .filter(|count| *count <= MAX_EXPONENT)
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    u32::try_from(count).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))
}

/// The rounding modes of `Decimal.divide`, one per `core.Rounding` member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoundingMode {
    /// Ties to the even neighbour.
    HalfEven,
    /// Ties away from zero.
    HalfUp,
    /// Truncation toward zero.
    TowardZero,
    /// Always away from zero.
    AwayFromZero,
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceil,
}

/// `numerator / denominator` rounded under `mode`, for a positive denominator. The truncated quotient moves one unit
/// away from zero when the mode says so: down for a negative numerator, up otherwise.
fn round_with(mode: RoundingMode, numerator: &BigInt, denominator: &BigInt) -> BigInt {
    let (mut quotient, remainder) = numerator.div_rem(denominator);
    if remainder.is_zero() {
        return quotient;
    }
    let negative = numerator.sign() == Sign::Minus;
    let twice = remainder.abs() * 2u8;
    let away = match mode {
        RoundingMode::TowardZero => false,
        RoundingMode::AwayFromZero => true,
        RoundingMode::Floor => negative,
        RoundingMode::Ceil => !negative,
        RoundingMode::HalfUp => twice >= *denominator,
        RoundingMode::HalfEven => {
            twice > *denominator
                || (twice == *denominator && (&quotient % 2u8).abs() == BigInt::from(1u8))
        }
    };
    if away {
        if negative {
            quotient -= 1;
        } else {
            quotient += 1;
        }
    }
    quotient
}
