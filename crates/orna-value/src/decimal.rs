//! Exact Decimal and Money values.

use crate::Result;
use crate::codec::{DecodeError, OvbCodec};
use crate::error::Error;
use crate::identity::{uuid_array, uuid_raw};
use crate::raw::{Raw, Value, array, integer, tag};
use crate::tags::{DECIMAL_TAG, MONEY_TAG};
use num_bigint::BigInt;
use num_bigint::Sign;
use num_integer::Integer;
use num_traits::{Signed, Zero};

/// Exact normalised decimal coefficient times 10 to its exponent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decimal {
    pub(crate) coefficient: BigInt,
    pub(crate) exponent10: BigInt,
}

impl Decimal {
    pub const MAX_ABS_EXPONENT: u64 = 1_000_000;
    pub(crate) fn new(coefficient: BigInt, exponent10: BigInt) -> Self {
        let (coefficient, exponent10) = normal_decimal(coefficient, exponent10);
        Self {
            coefficient,
            exponent10,
        }
    }
    pub fn try_new(coefficient: BigInt, exponent10: BigInt) -> Result<Self> {
        let decimal = Self::new(coefficient, exponent10);
        if decimal.exponent10.magnitude() > BigInt::from(Self::MAX_ABS_EXPONENT).magnitude() {
            return Err(Error::DecimalLimit);
        }
        Ok(decimal)
    }
    pub fn coefficient(&self) -> &BigInt {
        &self.coefficient
    }
    pub fn exponent10(&self) -> &BigInt {
        &self.exponent10
    }
    /// Renders this already-normalized Decimal in the canonical Orna text
    /// spelling. The representation stays exact and never passes through a
    /// binary floating-point value.
    pub fn canonical_text(&self) -> String {
        format!("{}e{}.decimal", self.coefficient, self.exponent10)
    }

    /// Parses only the canonical Orna Decimal text spelling.
    ///
    /// The parser intentionally rejects alternate spellings (including
    /// trailing coefficient zeroes, leading zeroes and a `+` exponent) so a
    /// textual value cannot acquire a second identity from the same exact
    /// decimal.
    pub fn from_canonical_text(text: &str) -> Result<Self> {
        let body = text.strip_suffix(".decimal").ok_or(Error::InvalidValue)?;
        let (coefficient_text, exponent_text) = body.split_once('e').ok_or(Error::InvalidValue)?;
        if coefficient_text.is_empty() || exponent_text.is_empty() {
            return Err(Error::InvalidValue);
        }

        let (negative, digits) = coefficient_text
            .strip_prefix('-')
            .map_or((false, coefficient_text), |digits| (true, digits));
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidValue);
        }
        if (digits.len() > 1 && digits.starts_with('0')) || (negative && digits == "0") {
            return Err(Error::NonCanonical);
        }
        let mut coefficient =
            BigInt::parse_bytes(digits.as_bytes(), 10).ok_or(Error::InvalidValue)?;
        if negative {
            coefficient = -coefficient;
        }

        let (negative_exponent, exponent_digits) = exponent_text
            .strip_prefix('-')
            .map_or((false, exponent_text), |digits| (true, digits));
        if exponent_digits.is_empty() || !exponent_digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Error::InvalidValue);
        }
        if exponent_digits.len() > 1 && exponent_digits.starts_with('0') {
            return Err(Error::NonCanonical);
        }
        if exponent_digits.len() > 7 {
            return Err(Error::DecimalLimit);
        }
        let mut exponent =
            BigInt::parse_bytes(exponent_digits.as_bytes(), 10).ok_or(Error::InvalidValue)?;
        if negative_exponent {
            if exponent.is_zero() {
                return Err(Error::NonCanonical);
            }
            exponent = -exponent;
        }

        let decimal = Self::try_new(coefficient, exponent)?;
        if decimal.canonical_text() != text {
            return Err(Error::NonCanonical);
        }
        Ok(decimal)
    }
    pub(crate) fn raw(&self) -> Raw {
        tag(
            DECIMAL_TAG,
            Raw::Array(vec![
                Raw::Int(self.coefficient.clone()),
                Raw::Int(self.exponent10.clone()),
            ]),
        )
    }
    pub(crate) fn from_raw(raw: &Raw) -> Result<Self> {
        let Raw::Tag(DECIMAL_TAG, inner) = raw else {
            return Err(Error::InvalidTag);
        };
        let fields = array(inner)?;
        if fields.len() != 2 {
            return Err(Error::InvalidTag);
        }
        let coefficient = integer(&fields[0])?.clone();
        let exponent10 = integer(&fields[1])?.clone();
        let decimal = Self::try_new(coefficient, exponent10)?;
        if decimal.raw() != *raw {
            return Err(Error::NonCanonical);
        }
        Ok(decimal)
    }
    pub fn try_multiply(&self, other: &Self) -> Result<Self> {
        Self::try_new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }
    fn try_add_parts(&self, other_coefficient: &BigInt, other_exponent: &BigInt) -> Result<Self> {
        let e = if self.exponent10 < *other_exponent {
            self.exponent10.clone()
        } else {
            other_exponent.clone()
        };
        let shift_a = &self.exponent10 - &e;
        let shift_b = other_exponent - &e;
        if shift_a > BigInt::from(Self::MAX_ABS_EXPONENT)
            || shift_b > BigInt::from(Self::MAX_ABS_EXPONENT)
        {
            return Err(Error::DecimalLimit);
        }
        let shift_a = bounded_exponent(&shift_a)?;
        let shift_b = bounded_exponent(&shift_b)?;
        Self::try_new(
            &self.coefficient * pow10(shift_a) + other_coefficient * pow10(shift_b),
            e,
        )
    }
    pub fn try_add(&self, other: &Self) -> Result<Self> {
        self.try_add_parts(&other.coefficient, &other.exponent10)
    }
    /// Exact finite decimal subtraction. Resource limits fail closed without
    /// rounding or converting through binary floating point.
    pub fn try_subtract(&self, other: &Self) -> Result<Self> {
        let negated_coefficient = -&other.coefficient;
        self.try_add_parts(&negated_coefficient, &other.exponent10)
    }
    /// Exact finite decimal division.  A denominator with primes other than 2
    /// and 5 has no finite base-10 result and is rejected without rounding.
    pub fn divide_exact(&self, other: &Self) -> Result<Self> {
        if other.coefficient.is_zero() {
            return Err(Error::DivisionByZero);
        }
        if self.coefficient.is_zero() {
            return Ok(Self::new(BigInt::zero(), BigInt::zero()));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let mut twos = 0u64;
        let mut fives = 0u64;
        while (&denominator % BigInt::from(2u8)).is_zero() {
            denominator /= 2u8;
            twos += 1;
        }
        while (&denominator % BigInt::from(5u8)).is_zero() {
            denominator /= 5u8;
            fives += 1;
        }
        if denominator != BigInt::from(1) {
            return Err(Error::NonFiniteDecimal);
        }
        let scale = twos.max(fives);
        if scale > Self::MAX_ABS_EXPONENT {
            return Err(Error::DecimalLimit);
        }
        numerator *= BigInt::from(2u8).pow((scale - twos) as u32);
        numerator *= BigInt::from(5u8).pow((scale - fives) as u32);
        Self::try_new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }
}

impl OvbCodec for Decimal {
    fn type_label() -> &'static str {
        "Decimal"
    }

    fn encode_value(&self) -> Result<Value> {
        Value::new(self.raw())
    }

    fn decode_value(
        value: &Value,
        path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        Decimal::from_raw(value.raw()).map_err(|error| DecodeError::new(error, path.clone()))
    }
}

/// An exact amount paired with its opaque nominal currency object identity.
///
/// The currency ID is a witness at the value boundary: it is carried in the
/// canonical tag-60007 payload and is never reduced to a display code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Money {
    amount: Decimal,
    currency: [u8; 16],
    value: Value,
}

impl Money {
    pub fn new(amount: Decimal, currency: [u8; 16]) -> Result<Self> {
        let value = Value::new(tag(
            MONEY_TAG,
            Raw::Array(vec![amount.raw(), uuid_raw(currency)]),
        ))?;
        Ok(Self {
            amount,
            currency,
            value,
        })
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let Raw::Tag(MONEY_TAG, inner) = value.raw() else {
            return Err(Error::InvalidTag);
        };
        let fields = array(inner)?;
        if fields.len() != 2 {
            return Err(Error::InvalidTag);
        }
        let amount = Decimal::from_raw(&fields[0])?;
        let currency = uuid_array(&fields[1])?;
        Ok(Self {
            amount,
            currency,
            value,
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(Value::decode(bytes)?)
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn amount(&self) -> &Decimal {
        &self.amount
    }

    pub fn currency(&self) -> [u8; 16] {
        self.currency
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.value.encode()
    }
}

fn bounded_exponent(n: &BigInt) -> Result<u64> {
    if n.sign() == Sign::Minus {
        return Err(Error::DecimalLimit);
    }
    let limbs = n.to_u64_digits().1;
    match limbs.as_slice() {
        [] => Ok(0),
        [value] if *value <= Decimal::MAX_ABS_EXPONENT => Ok(*value),
        _ => Err(Error::DecimalLimit),
    }
}

fn pow10(n: u64) -> BigInt {
    BigInt::from(10u8).pow(n as u32)
}

fn normal_decimal(mut c: BigInt, mut e: BigInt) -> (BigInt, BigInt) {
    if c.is_zero() {
        return (BigInt::zero(), BigInt::zero());
    }
    let ten = BigInt::from(10);
    while (&c % &ten).is_zero() {
        c /= &ten;
        e += 1;
    }
    (c, e)
}
