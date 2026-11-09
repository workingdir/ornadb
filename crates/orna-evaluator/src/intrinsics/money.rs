//! Standard-library dispatch for the money family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn money(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("__quantize", [amount, Value::Int(scale), Value::String(rounding)]) => {
                let scale = scale
                    .to_usize()
                    .filter(|scale| *scale <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let decimal = money_amount(amount)?;
                let rounded = quantize_decimal(&decimal, scale, rounding)?;
                replace_money_amount(amount, rounded)
            }
            (
                "__allocate",
                [
                    amount,
                    Value::List(weight_values),
                    Value::Int(scale),
                    rounding,
                ],
            ) => {
                self.items(weight_values.len())?;
                let scale = scale
                    .to_usize()
                    .filter(|scale| *scale <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let rounding = match rounding {
                    Value::Null | Value::Option(None) => None,
                    Value::String(mode) => Some(mode.as_str()),
                    Value::Option(Some(inner)) => match inner.as_ref() {
                        Value::String(mode) => Some(mode.as_str()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    },
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                };
                let weights = weight_values
                    .iter()
                    .map(|weight| match weight {
                        Value::Int(weight) if !weight.is_negative() => self.integer(weight.clone()),
                        Value::Int(_) => Err(error("ORNA-EVAL-VALUE")),
                        _ => Err(error("ORNA-EVAL-TYPE")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let shares = allocate_minor_units(amount, &weights, scale, rounding)?;
                self.items(shares.len())?;
                shares
                    .into_iter()
                    .map(|share| replace_money_amount(amount, share))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::list)
            }
            (
                "__format",
                [
                    amount,
                    Value::String(currency_code),
                    Value::Int(minor_digits),
                    Value::String(rounding),
                    Value::String(locale),
                ],
            ) => {
                let minor_digits = minor_digits
                    .to_usize()
                    .filter(|digits| *digits <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let formatted =
                    format_money_value(amount, currency_code, minor_digits, rounding, locale)?;
                self.string(formatted).map(Value::String)
            }
            ("__quantize" | "__allocate" | "__format", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
}

pub(crate) fn money_amount(value: &Value) -> Result<DecimalValue, EvaluationError> {
    match value {
        Value::Int(value) => DecimalValue::new(value.clone(), BigInt::zero()),
        Value::Decimal(value) => Ok(value.clone()),
        Value::Money { amount, .. } => Ok(amount.clone()),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
pub(crate) fn replace_money_amount(
    value: &Value,
    amount: DecimalValue,
) -> Result<Value, EvaluationError> {
    match value {
        Value::Money { currency, .. } => Ok(Value::Money {
            amount,
            currency: *currency,
        }),
        Value::Int(_) | Value::Decimal(_) => Ok(Value::Decimal(amount)),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
