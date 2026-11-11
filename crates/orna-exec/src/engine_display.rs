//! The engine-derived `Display` (kernel-and-library.md 4.5, P5): the text an interpolation shows for a scalar kernel
//! value.
//!
//! A type derives `Display` when `orna_hir::engine_display::derives_display` accepts it, and the executor renders its
//! values here without a source instance. The text is canonical: `Bool` is `true` or `false`, `Int` is its minimal
//! signed decimal, `Decimal` is the fixed-point form of `std.numeric` (no trailing zero in the fraction), and `Str` is
//! its own text.

use num_traits::ToPrimitive;

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::value::Value;

/// The text a value of a type that `orna_hir::engine_display::derives_display` accepts shows in an interpolation.
///
/// # Errors
///
/// `ORNA-EVAL-TYPE` when the value is not of a derived type, and `ORNA-EVAL-LIMIT` when a decimal's exponent does not fit
/// the text's width.
pub(crate) fn text(value: &Value) -> Result<String, Stop> {
    match value {
        Value::Bool(value) => Ok(value.to_string()),
        Value::Int(value) => Ok(value.to_str_radix(10)),
        Value::Decimal(value) => decimal_text(value),
        Value::Text(text) => Ok(text.clone()),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The fixed-point text of a normalised decimal: the coefficient with its point placed `-exponent` digits from the
/// right, or the coefficient followed by zeros when the exponent is not negative.
fn decimal_text(value: &Decimal) -> Result<String, Stop> {
    let signed = value.coefficient().to_str_radix(10);
    let (sign, digits) = match signed.strip_prefix('-') {
        Some(magnitude) => ("-", magnitude),
        None => ("", signed.as_str()),
    };
    let exponent = value
        .exponent10()
        .to_i64()
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    if exponent >= 0 {
        let zeros = usize::try_from(exponent).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?;
        return Ok(format!("{sign}{digits}{}", "0".repeat(zeros)));
    }
    let scale = usize::try_from(exponent.unsigned_abs()).map_err(|_| Stop::Error("ORNA-EVAL-LIMIT"))?;
    let padded = if digits.len() <= scale {
        format!("{}{digits}", "0".repeat(scale - digits.len() + 1))
    } else {
        digits.to_owned()
    };
    let point = padded.len() - scale;
    Ok(format!("{sign}{}.{}", &padded[..point], &padded[point..]))
}
