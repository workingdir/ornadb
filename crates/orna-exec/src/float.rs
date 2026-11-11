//! IEEE-754 binary64 operators on the executor's `Float` values.
//!
//! A `Float` is held as its bit pattern, as the canonical encoding stores it.
//! Every computed NaN is replaced by the canonical NaN bits, so two NaNs that
//! the old evaluator would canonicalise compare equal bytewise here too.
//! Ordinary comparison follows `docs/language/reference/02-types.md`: `NaN` is
//! unequal to everything, `-0.0 == 0.0`, and every ordered comparison with NaN
//! is false. Mixing `Float` with `Int` or `Decimal` never reaches this module;
//! the type pass rejects it.

use orna_hir::BinOp;
use orna_value::{CANONICAL_NAN_BITS, float_ordinary_eq};

use crate::error::Stop;
use crate::value::Value;

/// The value of a computed float, with every NaN replaced by the canonical
/// NaN bits.
pub(crate) fn canonical(value: f64) -> Value {
    Value::Float(if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        value.to_bits()
    })
}

/// Unary minus on a float.
pub(crate) fn negate(bits: u64) -> Value {
    canonical(-f64::from_bits(bits))
}

/// Binary operators on two floats. Division by zero yields infinity or NaN as
/// IEEE-754 specifies; it is not an error.
pub(crate) fn binary(op: BinOp, lhs: u64, rhs: u64) -> Result<Value, Stop> {
    let (a, b) = (f64::from_bits(lhs), f64::from_bits(rhs));
    match op {
        BinOp::Add => Ok(canonical(a + b)),
        BinOp::Sub => Ok(canonical(a - b)),
        BinOp::Mul => Ok(canonical(a * b)),
        BinOp::Div => Ok(canonical(a / b)),
        BinOp::Rem => Ok(canonical(a % b)),
        BinOp::Eq => Ok(Value::Bool(float_ordinary_eq(lhs, rhs))),
        BinOp::Ne => Ok(Value::Bool(!float_ordinary_eq(lhs, rhs))),
        BinOp::Lt => Ok(Value::Bool(a < b)),
        BinOp::Le => Ok(Value::Bool(a <= b)),
        BinOp::Gt => Ok(Value::Bool(a > b)),
        BinOp::Ge => Ok(Value::Bool(a >= b)),
        BinOp::Pow => Err(Stop::NotYet("Float exponentiation (needs libm)")),
        BinOp::In => Err(Stop::NotYet("membership is a Contains call after typeck")),
    }
}

#[cfg(test)]
mod tests {
    use orna_value::{CANONICAL_NAN_BITS, Raw};

    use super::*;

    fn float(value: f64) -> u64 {
        value.to_bits()
    }

    fn bits_of(value: Value) -> u64 {
        match value {
            Value::Float(bits) => bits,
            other => panic!("expected a Float, got {other:?}"),
        }
    }

    fn truth(value: Value) -> bool {
        match value {
            Value::Bool(value) => value,
            other => panic!("expected a Bool, got {other:?}"),
        }
    }

    #[test]
    fn computed_nan_is_canonical() {
        let nan = canonical(f64::NAN);
        assert_eq!(bits_of(nan.clone()), CANONICAL_NAN_BITS);
        // A NaN with a payload that arithmetic produced is canonicalised too.
        let payload_nan = f64::from_bits(0x7ff8_0000_0000_0001);
        assert_eq!(bits_of(canonical(payload_nan)), CANONICAL_NAN_BITS);
        assert_eq!(nan.to_raw().ok(), Some(Raw::Float(CANONICAL_NAN_BITS)));
    }

    #[test]
    fn raw_form_is_the_float_bits() {
        assert_eq!(
            Value::Float(float(1.5)).to_raw().ok(),
            Some(Raw::Float(float(1.5)))
        );
    }

    #[test]
    fn arithmetic_is_binary64() {
        let sum = binary(BinOp::Add, float(0.1), float(0.2)).ok();
        assert_eq!(sum.map(bits_of), Some(float(0.1 + 0.2)));
        let quotient = binary(BinOp::Div, float(1.0), float(0.0)).ok();
        assert_eq!(quotient.map(bits_of), Some(float(f64::INFINITY)));
        let remainder = binary(BinOp::Rem, float(-7.5), float(2.0)).ok();
        assert_eq!(remainder.map(bits_of), Some(float(-1.5)));
    }

    #[test]
    fn nan_is_unequal_and_unordered() {
        let nan = float(f64::NAN);
        assert!(!truth(
            binary(BinOp::Eq, nan, nan).unwrap_or(Value::Bool(true))
        ));
        assert!(truth(
            binary(BinOp::Ne, nan, nan).unwrap_or(Value::Bool(false))
        ));
        for op in [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            assert!(!truth(
                binary(op, nan, float(1.0)).unwrap_or(Value::Bool(true))
            ));
        }
    }

    #[test]
    fn signed_zeros_are_equal() {
        let zero = binary(BinOp::Eq, float(-0.0), float(0.0)).ok();
        assert!(zero.is_some_and(truth));
    }

    #[test]
    fn negation_flips_the_sign_bit() {
        assert_eq!(bits_of(negate(float(2.5))), float(-2.5));
    }
}
