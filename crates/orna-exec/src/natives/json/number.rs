//! JSON number tokens: how a number is read into an `Int`, `Decimal` or `Float`, and how a `Decimal` is written. A token
//! is already valid JSON grammar when it reaches here; these functions only read its shape and bound its size.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::ExecLimits;

/// An integer token: no fraction and no exponent. More digits than the limit is a limit error.
pub(super) fn integer(token: &str, limits: &ExecLimits) -> Result<BigInt, Stop> {
    if token.contains(['.', 'e', 'E']) {
        return Err(super::parse::invalid());
    }
    if digit_count(token) > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    token.parse::<BigInt>().map_err(|_| super::parse::invalid())
}

/// A decimal token: a fraction or an exponent is required. The coefficient is the digits without the point, and the
/// exponent is the written exponent less the fraction's length, so the value is exact.
pub(super) fn decimal(token: &str, limits: &ExecLimits) -> Result<Decimal, Stop> {
    if !token.contains(['.', 'e', 'E']) {
        return Err(super::parse::invalid());
    }
    let (mantissa, exponent) = match token.find(['e', 'E']) {
        Some(index) => (&token[..index], Some(&token[index + 1..])),
        None => (token, None),
    };
    let (negative, unsigned) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.len() + fraction.len() > limits.max_integer_digits {
        return Err(Stop::Error("ORNA-EVAL-LIMIT"));
    }
    let mut coefficient = format!("{whole}{fraction}")
        .parse::<BigInt>()
        .map_err(|_| super::parse::invalid())?;
    if negative {
        coefficient = -coefficient;
    }
    let written = match exponent {
        Some(text) => text
            .parse::<BigInt>()
            .map_err(|_| super::parse::invalid())?,
        None => BigInt::zero(),
    };
    let exponent10 = written - BigInt::from(fraction.len());
    Decimal::new(coefficient, exponent10)
}

/// A float token, read as binary64. A value that overflows to infinity is not a finite JSON number.
pub(super) fn float(token: &str) -> Result<f64, Stop> {
    let value = token.parse::<f64>().map_err(|_| super::parse::invalid())?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(super::parse::invalid())
    }
}

/// The JSON token for a `Decimal`: always a fraction or an exponent-free point form, so it reads back as a decimal.
/// A zero is `0.0`. A value with more zeros than the string limit is a limit error.
pub(super) fn decimal_token(value: &Decimal, limits: &ExecLimits) -> Result<String, Stop> {
    let coefficient = value.coefficient();
    if coefficient.is_zero() {
        return Ok("0.0".to_owned());
    }
    let exponent = value.exponent10();
    let digits = coefficient.abs().to_string();
    let mut token = String::with_capacity(digits.len() + 2);
    if coefficient.is_negative() {
        token.push('-');
    }
    if exponent.sign() != Sign::Minus {
        let zeros = bounded(exponent, limits)?;
        token.push_str(&digits);
        token.extend(std::iter::repeat_n('0', zeros));
        token.push_str(".0");
    } else {
        let places = bounded(&exponent.abs(), limits)?;
        if digits.len() > places {
            let (whole, fraction) = digits.split_at(digits.len() - places);
            token.push_str(whole);
            token.push('.');
            token.push_str(fraction);
        } else {
            token.push_str("0.");
            token.extend(std::iter::repeat_n('0', places - digits.len()));
            token.push_str(&digits);
        }
    }
    Ok(token)
}

fn bounded(count: &BigInt, limits: &ExecLimits) -> Result<usize, Stop> {
    count
        .to_usize()
        .filter(|&n| n <= limits.max_string_bytes)
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))
}

fn digit_count(token: &str) -> usize {
    token.bytes().filter(u8::is_ascii_digit).count()
}
