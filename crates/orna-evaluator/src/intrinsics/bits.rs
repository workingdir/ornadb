//! Standard-library dispatch for the bits family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn bits(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match (name, values.as_slice()) {
            ("bit_or", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left | right)?))
            }
            ("bit_and", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left & right)?))
            }
            ("bit_xor", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left ^ right)?))
            }
            ("bit_not", [Value::Int(value)]) => {
                Ok(Value::Int(self.integer(-value - BigInt::from(1))?))
            }
            ("shift_left", [Value::Int(value), Value::Int(count)]) => self.shift_left(value, count),
            ("shift_right", [Value::Int(value), Value::Int(count)]) => {
                self.shift_right(value, count)
            }
            ("bit_or" | "bit_and" | "bit_xor" | "bit_not" | "shift_left" | "shift_right", _)
                if values.iter().any(|value| !matches!(value, Value::Int(_))) =>
            {
                Err(error("ORNA-EVAL-TYPE"))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn shift_left(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if value.is_zero() {
            return Ok(Value::Int(BigInt::ZERO));
        }
        let max_bits = self.limits.max_integer_digits.saturating_mul(4);
        if BigInt::from(value.bits()) + count > BigInt::from(max_bits) {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value << count)?))
    }
    fn shift_right(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if count >= &BigInt::from(value.bits()) {
            return Ok(Value::Int(if value.is_negative() {
                BigInt::from(-1)
            } else {
                BigInt::ZERO
            }));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value >> count)?))
    }
}
