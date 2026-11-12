//! The `core.decimal` engine native: `divide(dividend, value, scale, rounding)`, the quotient of two decimals rounded to a
//! scale under a `core.Rounding` member. Each call charges one step, as the other families do.

use num_bigint::Sign;
use num_traits::ToPrimitive;
use orna_hir::Ty;
use orna_value::enum_variant_id;

use crate::decimal_round::RoundingMode;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::values::exactly;
use crate::value::Value;

/// The `core.Rounding` members and the mode each one selects.
const MODES: [(&str, RoundingMode); 6] = [
    ("half_even", RoundingMode::HalfEven),
    ("half_up", RoundingMode::HalfUp),
    ("toward_zero", RoundingMode::TowardZero),
    ("away_from_zero", RoundingMode::AwayFromZero),
    ("floor", RoundingMode::Floor),
    ("ceil", RoundingMode::Ceil),
];

/// `divide(Decimal, Decimal, Int, Rounding): Decimal`. A negative scale is `ORNA-EVAL-VALUE`; a scale too large for a
/// count is `ORNA-EVAL-LIMIT`. The zero divisor and the scale limit are the errors of the rounding itself.
pub(super) fn divide(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [dividend, value, scale, rounding] = exactly::<4>(arguments)?;
    let (Value::Decimal(dividend), Value::Decimal(value)) = (dividend, value) else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let Value::Int(scale) = scale else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if scale.sign() == Sign::Minus {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let scale = scale.to_usize().ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    let mode = rounding_mode(&rounding)?;
    Ok(Value::Decimal(
        dividend.divide_rounded_with(&value, scale, mode)?,
    ))
}

/// The mode a payload-free `core.Rounding` member selects, or `ORNA-EVAL-TYPE` for any other value.
fn rounding_mode(rounding: &Value) -> Result<RoundingMode, Stop> {
    let Value::Enum {
        variant_id,
        payload: None,
        ..
    } = rounding
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    MODES
        .iter()
        .find(|(name, _)| enum_variant_id("core.Rounding", name) == *variant_id)
        .map(|(_, mode)| *mode)
        .ok_or(Stop::Error("ORNA-EVAL-TYPE"))
}
