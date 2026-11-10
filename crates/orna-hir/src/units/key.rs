//! Exact unit-key algebra (units.md sections 3.1 to 3.5 and the kernel interface of section 5, items 3b and 3c).
//!
//! A [`UnitKey`] is an exponent map over base axes with an exact positive rational scale. Keys multiply and divide
//! by adding exponents and multiplying scales; a key's dimension is its exponent map alone. Every operation is
//! checked: an exponent outside `[-32, 32]` or a rational wider than [`MAX_BITS`] is a [`KeyError`], which the
//! evaluator reports as ORNA-UNIT-LIMIT (rule ORNA-UNIT-019).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use num_bigint::{BigInt, Sign};

use crate::DefId;

/// The largest magnitude of an exponent in an exponent map (ORNA-UNIT-019).
pub const MAX_EXPONENT: i32 = 32;

/// The largest bit length of a rational's numerator or denominator: the evaluator's integer budget.
pub const MAX_BITS: u64 = 4096;

/// Why a unit-key operation is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    /// An exponent, or the exponent of a scale, left `[-32, 32]`.
    Exponent,
    /// A rational's numerator or denominator exceeded [`MAX_BITS`].
    Budget,
    /// A rational had a zero denominator, or a division by zero.
    ZeroDivision,
    /// A unit key's scale was zero or negative.
    NonPositiveScale,
}

/// An exact rational `num / den` with `den > 0` and `gcd(num, den) = 1`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Rational {
    num: BigInt,
    den: BigInt,
}

fn gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
    if a.sign() == Sign::Minus {
        a = -a;
    }
    if b.sign() == Sign::Minus {
        b = -b;
    }
    while b.sign() != Sign::NoSign {
        let rest = &a % &b;
        a = b;
        b = rest;
    }
    a
}

/// Whether `width`-bit parts raised to `exponent` would exceed the budget, checked before any power is computed.
fn power_too_wide(width: u64, exponent: u32) -> bool {
    width.saturating_mul(u64::from(exponent)) > MAX_BITS
}

impl Rational {
    /// The rational `1`.
    #[must_use]
    pub fn one() -> Self {
        Self::from_int(1)
    }

    /// The integer `value`.
    #[must_use]
    pub fn from_int(value: i64) -> Self {
        Self {
            num: BigInt::from(value),
            den: BigInt::from(1),
        }
    }

    /// The reduced rational `num / den`.
    ///
    /// # Errors
    ///
    /// [`KeyError::ZeroDivision`] when `den` is zero; [`KeyError::Budget`] when the reduced parts exceed
    /// [`MAX_BITS`].
    pub fn new(num: BigInt, den: BigInt) -> Result<Self, KeyError> {
        if den.sign() == Sign::NoSign {
            return Err(KeyError::ZeroDivision);
        }
        let (num, den) = if den.sign() == Sign::Minus {
            (-num, -den)
        } else {
            (num, den)
        };
        let divisor = gcd(num.clone(), den.clone());
        let reduced = Self {
            num: num / &divisor,
            den: den / divisor,
        };
        reduced.within_budget()
    }

    /// Parses a coefficient written as a decimal literal, such as `1000` or `1609.344`, exactly. `None` when the
    /// text is not a plain decimal literal.
    #[must_use]
    pub fn parse_decimal(text: &str) -> Option<Result<Self, KeyError>> {
        let digits: String = text.chars().filter(|c| *c != '_').collect();
        let (whole, fraction) = match digits.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (digits.as_str(), ""),
        };
        let well_formed = !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit());
        let fraction_ok = fraction.bytes().all(|b| b.is_ascii_digit());
        if !well_formed || !fraction_ok {
            return None;
        }
        let num = format!("{whole}{fraction}").parse::<BigInt>().ok()?;
        let places = u32::try_from(fraction.len()).ok()?;
        if u64::from(places) > MAX_BITS {
            return Some(Err(KeyError::Budget));
        }
        Some(Self::new(num, BigInt::from(10).pow(places)))
    }

    /// The numerator, carrying the sign.
    #[must_use]
    pub fn numerator(&self) -> &BigInt {
        &self.num
    }

    /// The denominator, always positive.
    #[must_use]
    pub fn denominator(&self) -> &BigInt {
        &self.den
    }

    /// Whether the rational is strictly positive.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        self.num.sign() == Sign::Plus
    }

    /// Whether the rational is an integer.
    #[must_use]
    pub fn is_integer(&self) -> bool {
        self.den == BigInt::from(1)
    }

    /// Whether the decimal expansion of the rational terminates (its denominator has no prime factor other than
    /// 2 and 5). This is the "finite decimal" test of ORNA-UNIT-014 and ORNA-UNIT-016.
    #[must_use]
    pub fn is_finite_decimal(&self) -> bool {
        let mut rest = self.den.clone();
        for prime in [2, 5] {
            let prime = BigInt::from(prime);
            while &rest % &prime == BigInt::from(0) {
                rest /= &prime;
            }
        }
        rest == BigInt::from(1)
    }

    /// The product `self * other`.
    ///
    /// # Errors
    ///
    /// [`KeyError::Budget`] when the product exceeds [`MAX_BITS`].
    pub fn mul(&self, other: &Self) -> Result<Self, KeyError> {
        Self::new(&self.num * &other.num, &self.den * &other.den)
    }

    /// The quotient `self / other`.
    ///
    /// # Errors
    ///
    /// [`KeyError::ZeroDivision`] when `other` is zero; [`KeyError::Budget`] when the quotient is too wide.
    pub fn div(&self, other: &Self) -> Result<Self, KeyError> {
        Self::new(&self.num * &other.den, &self.den * &other.num)
    }

    /// The integer power `self ^ exponent`.
    ///
    /// # Errors
    ///
    /// [`KeyError::ZeroDivision`] for a negative power of zero; [`KeyError::Budget`] when the result is too wide.
    pub fn pow(&self, exponent: i32) -> Result<Self, KeyError> {
        let magnitude = exponent.unsigned_abs();
        let width = self.num.bits().max(self.den.bits());
        if power_too_wide(width, magnitude) {
            return Err(KeyError::Budget);
        }
        let num = self.num.pow(magnitude);
        let den = self.den.pow(magnitude);
        if exponent >= 0 {
            Self::new(num, den)
        } else {
            Self::new(den, num)
        }
    }

    /// Orders two positive rationals exactly.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        (&self.num * &other.den).cmp(&(&other.num * &self.den))
    }

    fn within_budget(self) -> Result<Self, KeyError> {
        if self.num.bits() > MAX_BITS || self.den.bits() > MAX_BITS {
            Err(KeyError::Budget)
        } else {
            Ok(self)
        }
    }
}

/// An exponent map over base axes: finite, zero entries removed, each exponent in `[-32, 32]`.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Exponents(BTreeMap<DefId, i32>);

impl Exponents {
    /// The dimensionless map.
    #[must_use]
    pub fn dimensionless() -> Self {
        Self::default()
    }

    /// The map of one axis, with exponent one: the dimension of a base unit.
    #[must_use]
    pub fn axis(axis: DefId) -> Self {
        Self(BTreeMap::from([(axis, 1)]))
    }

    /// The exponent of `axis`, zero when absent.
    #[must_use]
    pub fn get(&self, axis: DefId) -> i32 {
        self.0.get(&axis).copied().unwrap_or(0)
    }

    /// Whether every exponent is zero.
    #[must_use]
    pub fn is_dimensionless(&self) -> bool {
        self.0.is_empty()
    }

    /// The non-zero entries in ascending axis order.
    pub fn iter(&self) -> impl Iterator<Item = (DefId, i32)> + '_ {
        self.0.iter().map(|(axis, exponent)| (*axis, *exponent))
    }

    /// The pointwise sum of two maps.
    ///
    /// # Errors
    ///
    /// [`KeyError::Exponent`] when an entry of the sum leaves `[-32, 32]`.
    pub fn add(&self, other: &Self) -> Result<Self, KeyError> {
        let mut sum = self.0.clone();
        for (axis, exponent) in &other.0 {
            let total = sum.get(axis).copied().unwrap_or(0) + exponent;
            sum.insert(*axis, total);
        }
        Self::checked(sum)
    }

    /// The map with every entry multiplied by the integer `factor`.
    ///
    /// # Errors
    ///
    /// [`KeyError::Exponent`] when an entry leaves `[-32, 32]`.
    pub fn scale(&self, factor: i32) -> Result<Self, KeyError> {
        let mut entries = BTreeMap::new();
        for (axis, exponent) in &self.0 {
            let wide = i64::from(*exponent) * i64::from(factor);
            let narrow = i32::try_from(wide).map_err(|_| KeyError::Exponent)?;
            entries.insert(*axis, narrow);
        }
        Self::checked(entries)
    }

    fn checked(mut entries: BTreeMap<DefId, i32>) -> Result<Self, KeyError> {
        if entries
            .values()
            .any(|exponent| exponent.unsigned_abs() > MAX_EXPONENT.unsigned_abs())
        {
            return Err(KeyError::Exponent);
        }
        entries.retain(|_, exponent| *exponent != 0);
        Ok(Self(entries))
    }
}

/// A unit key `(V, s)`: an exponent map over base axes and an exact positive scale (units.md section 3.3).
///
/// The key is a function of its unit expression only; names do not take part. Two keys are equal exactly when
/// they are compatible (ORNA-UNIT-001).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnitKey {
    exponents: Exponents,
    scale: Rational,
}

impl UnitKey {
    /// A key from an exponent map and a scale.
    ///
    /// # Errors
    ///
    /// [`KeyError::NonPositiveScale`] when `scale` is not positive.
    pub fn new(exponents: Exponents, scale: Rational) -> Result<Self, KeyError> {
        if !scale.is_positive() {
            return Err(KeyError::NonPositiveScale);
        }
        Ok(Self { exponents, scale })
    }

    /// The key of a base unit of `axis`: `({axis: 1}, 1)`.
    #[must_use]
    pub fn base(axis: DefId) -> Self {
        Self {
            exponents: Exponents::axis(axis),
            scale: Rational::one(),
        }
    }

    /// The dimensionless key `({}, 1)`, the identity of multiplication.
    #[must_use]
    pub fn dimensionless() -> Self {
        Self {
            exponents: Exponents::dimensionless(),
            scale: Rational::one(),
        }
    }

    /// The exponent map `V`, which is the dimension.
    #[must_use]
    pub fn exponents(&self) -> &Exponents {
        &self.exponents
    }

    /// The exact scale `s`.
    #[must_use]
    pub fn scale(&self) -> &Rational {
        &self.scale
    }

    /// Whether the key has no dimension (`V` is empty).
    #[must_use]
    pub fn is_dimensionless(&self) -> bool {
        self.exponents.is_dimensionless()
    }

    /// Whether two keys have the same dimension, that is equal exponent maps (units.md section 3.5).
    #[must_use]
    pub fn same_dimension(&self, other: &Self) -> bool {
        self.exponents == other.exponents
    }

    /// The key of `self * other`: exponents add and scales multiply (ORNA-UNIT-012).
    ///
    /// # Errors
    ///
    /// The [`KeyError`] of the exponent or scale arithmetic.
    pub fn mul(&self, other: &Self) -> Result<Self, KeyError> {
        Ok(Self {
            exponents: self.exponents.add(&other.exponents)?,
            scale: self.scale.mul(&other.scale)?,
        })
    }

    /// The key of `self / other`.
    ///
    /// # Errors
    ///
    /// The [`KeyError`] of the exponent or scale arithmetic.
    pub fn div(&self, other: &Self) -> Result<Self, KeyError> {
        Ok(Self {
            exponents: self.exponents.add(&other.exponents.scale(-1)?)?,
            scale: self.scale.div(&other.scale)?,
        })
    }

    /// The key of `self ^ exponent`: exponents and the scale are raised to the power (ORNA-UNIT-013).
    ///
    /// # Errors
    ///
    /// The [`KeyError`] of the exponent or scale arithmetic.
    pub fn pow(&self, exponent: i32) -> Result<Self, KeyError> {
        Ok(Self {
            exponents: self.exponents.scale(exponent)?,
            scale: self.scale.pow(exponent)?,
        })
    }

    /// The key with its scale multiplied by `factor`, the positive coefficient a derived unit writes (`1000 metre`).
    ///
    /// # Errors
    ///
    /// [`KeyError::NonPositiveScale`] when `factor` is not positive; otherwise the rational's error.
    pub fn scaled_by(&self, factor: &Rational) -> Result<Self, KeyError> {
        if !factor.is_positive() {
            return Err(KeyError::NonPositiveScale);
        }
        Ok(Self {
            exponents: self.exponents.clone(),
            scale: self.scale.mul(factor)?,
        })
    }

    /// The exact ratio `s_self / s_other` of two keys (section 5, item 3c). Meaningful for two keys of one
    /// dimension; a value of `other` is expressed in `self` by multiplying by this ratio.
    ///
    /// # Errors
    ///
    /// The [`KeyError`] of the rational arithmetic.
    pub fn ratio_to(&self, other: &Self) -> Result<Rational, KeyError> {
        self.scale.div(&other.scale)
    }

    /// The finer of two keys of one dimension: the one with the smaller scale (ORNA-UNIT-011). Equal scales give
    /// the first.
    #[must_use]
    pub fn finer<'a>(first: &'a Self, second: &'a Self) -> &'a Self {
        match first.scale.compare(&second.scale) {
            Ordering::Greater => second,
            _ => first,
        }
    }
}
