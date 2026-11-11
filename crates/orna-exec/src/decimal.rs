//! Exact decimal values, with the old evaluator's rules.
//!
//! A decimal is a coefficient times a power of ten, stored normalised: the coefficient has no trailing zero, and zero
//! has exponent zero, so equal decimals are equal structurally. Its canonical raw form is a tag over the coefficient
//! and the exponent, the bytes the old evaluator writes. Division is exact: a quotient without a finite decimal
//! expansion fails.

use std::cmp::Ordering;

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_hir::BinOp;
use orna_value::{DECIMAL_TAG, Raw};

use crate::error::Stop;
use crate::value::Value;

/// The largest exponent magnitude, and so the largest scale a decimal operation may align or create. This is the old
/// evaluator's `DEFAULT_INTEGER_DIGITS`.
const MAX_EXPONENT: usize = 1_024;

/// An exact decimal: `coefficient * 10^exponent10`, normalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    coefficient: BigInt,
    exponent10: BigInt,
}

impl Decimal {
    /// The normalised decimal of `coefficient * 10^exponent10`. Trailing zeros of the coefficient move into the exponent,
    /// and zero keeps exponent zero.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-LIMIT` when the normalised exponent is beyond [`MAX_EXPONENT`].
    pub(crate) fn new(mut coefficient: BigInt, mut exponent10: BigInt) -> Result<Self, Stop> {
        if coefficient.is_zero() {
            return Ok(Self {
                coefficient,
                exponent10: BigInt::zero(),
            });
        }
        let ten = BigInt::from(10u8);
        while (&coefficient % &ten).is_zero() {
            coefficient /= &ten;
            exponent10 += BigInt::from(1u8);
        }
        if exponent10
            .abs()
            .to_usize()
            .is_none_or(|value| value > MAX_EXPONENT)
        {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        Ok(Self {
            coefficient,
            exponent10,
        })
    }

    /// The canonical raw form: a decimal tag over `[coefficient, exponent]`.
    pub(crate) fn to_raw(&self) -> Raw {
        Raw::Tag(
            DECIMAL_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(self.coefficient.clone()),
                Raw::Int(self.exponent10.clone()),
            ])),
        )
    }

    /// The additive inverse.
    pub(crate) fn negate(&self) -> Result<Self, Stop> {
        Self::new(-&self.coefficient, self.exponent10.clone())
    }

    /// Exact sum.
    pub(crate) fn add(&self, other: &Self) -> Result<Self, Stop> {
        let (exponent, left, right) = self.aligned(other)?;
        Self::new(left + right, exponent)
    }

    /// Exact difference.
    pub(crate) fn sub(&self, other: &Self) -> Result<Self, Stop> {
        self.add(&other.negate()?)
    }

    /// Exact product.
    pub(crate) fn multiply(&self, other: &Self) -> Result<Self, Stop> {
        Self::new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }

    /// Truncated remainder with the sign of the dividend, on the common scale.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-DIVIDE-BY-ZERO` for a zero divisor.
    pub(crate) fn remainder(&self, other: &Self) -> Result<Self, Stop> {
        if other.coefficient.is_zero() {
            return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        let (exponent, left, right) = self.aligned(other)?;
        Self::new(left % right, exponent)
    }

    /// Exact quotient. The quotient must have a finite decimal expansion.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-DIVIDE-BY-ZERO` for a zero divisor, `InexactDivision` when the quotient does not terminate, and
    /// `ORNA-EVAL-LIMIT` when the scale exceeds [`MAX_EXPONENT`].
    pub(crate) fn divide(&self, other: &Self) -> Result<Self, Stop> {
        if other.coefficient.is_zero() {
            return Err(Stop::Error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let twos = strip_factor(&mut denominator, 2);
        let fives = strip_factor(&mut denominator, 5);
        if denominator != BigInt::from(1u8) {
            return Err(Stop::Error("InexactDivision"));
        }
        let scale = twos.max(fives);
        if scale > MAX_EXPONENT {
            return Err(Stop::Error("ORNA-EVAL-LIMIT"));
        }
        numerator *= BigInt::from(2u8).pow(power(scale - twos));
        numerator *= BigInt::from(5u8).pow(power(scale - fives));
        Self::new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }

    /// The numeric order, exact and independent of scale (`1.5 == 1.50`).
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-LIMIT` when the two exponents are too far apart to align.
    pub(crate) fn compare(&self, other: &Self) -> Result<Ordering, Stop> {
        let delta = &self.exponent10 - &other.exponent10;
        let digits = delta
            .abs()
            .to_usize()
            .filter(|value| *value <= MAX_EXPONENT)
            .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
        let factor = BigInt::from(10u8).pow(power(digits));
        Ok(if delta.sign() == Sign::Minus {
            self.coefficient.cmp(&(&other.coefficient * factor))
        } else {
            (&self.coefficient * factor).cmp(&other.coefficient)
        })
    }

    /// Both coefficients scaled to the smaller exponent, with that exponent.
    fn aligned(&self, other: &Self) -> Result<(BigInt, BigInt, BigInt), Stop> {
        let exponent = self.exponent10.clone().min(other.exponent10.clone());
        let left = gap(&self.exponent10, &exponent)?;
        let right = gap(&other.exponent10, &exponent)?;
        Ok((
            exponent,
            &self.coefficient * BigInt::from(10u8).pow(power(left)),
            &other.coefficient * BigInt::from(10u8).pow(power(right)),
        ))
    }
}

/// The decimal operators. Mixed operand kinds are the type pass's job and never reach here.
///
/// # Errors
///
/// The error of the operation that fails. `^` and membership are not decimal operators, so they fail as unsupported.
pub(crate) fn binary(op: BinOp, lhs: &Decimal, rhs: &Decimal) -> Result<Value, Stop> {
    let value = match op {
        BinOp::Add => lhs.add(rhs)?,
        BinOp::Sub => lhs.sub(rhs)?,
        BinOp::Mul => lhs.multiply(rhs)?,
        BinOp::Div => lhs.divide(rhs)?,
        BinOp::Rem => lhs.remainder(rhs)?,
        BinOp::Eq => return Ok(Value::Bool(lhs == rhs)),
        BinOp::Ne => return Ok(Value::Bool(lhs != rhs)),
        BinOp::Lt => return Ok(Value::Bool(lhs.compare(rhs)?.is_lt())),
        BinOp::Le => return Ok(Value::Bool(lhs.compare(rhs)?.is_le())),
        BinOp::Gt => return Ok(Value::Bool(lhs.compare(rhs)?.is_gt())),
        BinOp::Ge => return Ok(Value::Bool(lhs.compare(rhs)?.is_ge())),
        BinOp::Pow | BinOp::In => return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    };
    Ok(Value::Decimal(value))
}

/// Divides `value` by `factor` for as long as it divides evenly, and returns how many times it did.
fn strip_factor(value: &mut BigInt, factor: u8) -> usize {
    let factor = BigInt::from(factor);
    let mut count = 0;
    while (&*value % &factor).is_zero() {
        *value /= &factor;
        count += 1;
    }
    count
}

/// The number of digits `exponent` sits above `floor`, which must be within [`MAX_EXPONENT`].
fn gap(exponent: &BigInt, floor: &BigInt) -> Result<usize, Stop> {
    (exponent - floor)
        .to_usize()
        .filter(|value| *value <= MAX_EXPONENT)
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))
}

/// A digit count as the exponent `pow` takes. Every count here is at most [`MAX_EXPONENT`].
fn power(digits: usize) -> u32 {
    u32::try_from(digits).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::Decimal;
    use num_bigint::BigInt;
    use orna_value::CanonicalValue;

    fn dec(coefficient: i64, exponent: i64) -> Decimal {
        Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("in range")
    }

    fn bytes(value: &Decimal) -> Vec<u8> {
        CanonicalValue::new(value.to_raw())
            .expect("canonical")
            .encode()
            .expect("encodes")
    }

    #[test]
    fn literals_normalise_and_match_golden_bytes() {
        assert_eq!(dec(1230, -3), dec(123, -2));
        assert_eq!(dec(0, 7), dec(0, 0));
        assert_eq!(hex(&bytes(&dec(1234, -2))), "d9ea60821904d221");
        assert_eq!(hex(&bytes(&dec(0, 0))), "d9ea60820000");
    }

    #[test]
    fn addition_and_subtraction_are_exact_on_the_common_scale() {
        assert_eq!(dec(1, -1).add(&dec(2, -1)).expect("sum"), dec(3, -1));
        assert_eq!(dec(1, -1).add(&dec(2, -2)).expect("sum"), dec(12, -2));
        assert_eq!(
            dec(120, -2).sub(&dec(34, -2)).expect("difference"),
            dec(86, -2)
        );
        assert_eq!(
            dec(1, -1).sub(&dec(25, -2)).expect("difference"),
            dec(-15, -2)
        );
    }

    #[test]
    fn exact_division_keeps_terminating_quotients_only() {
        assert_eq!(
            dec(1, 0).divide(&dec(8, 0)).expect("quotient"),
            dec(125, -3)
        );
        assert_eq!(
            dec(1, 0).divide(&dec(3, 0)).err().map(stop_code),
            Some("InexactDivision")
        );
        assert_eq!(
            dec(1, 0).divide(&dec(0, 0)).err().map(stop_code),
            Some("ORNA-EVAL-DIVIDE-BY-ZERO")
        );
    }

    #[test]
    fn remainder_truncates_toward_the_dividend() {
        assert_eq!(
            dec(-75, -1).remainder(&dec(2, 0)).expect("remainder"),
            dec(-15, -1)
        );
    }

    #[test]
    fn comparison_ignores_scale() {
        let one_and_a_half = dec(15, -1);
        let one_fifty = dec(150, -2);
        assert_eq!(one_and_a_half, one_fifty);
        assert_eq!(
            one_and_a_half.compare(&dec(2, 0)).expect("order"),
            std::cmp::Ordering::Less
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn stop_code(stop: crate::error::Stop) -> &'static str {
        match stop {
            crate::error::Stop::Error(code) => code,
            _ => "not an error",
        }
    }
}
