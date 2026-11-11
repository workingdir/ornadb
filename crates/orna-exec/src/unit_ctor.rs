//! Unit values at run time (units.md section 3, decision D15).
//!
//! A unit constructor such as `1.MiB` yields its argument unchanged, a number in its own unit. Typeck rewrites a
//! quantity that reaches its family's base-unit position into `UnitConvert`, and that conversion multiplies by the
//! exact scale. Scaling in the constructor as well would apply the factor twice.

use num_bigint::BigInt;
use orna_hir::{DefId, DefKind};

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::Machine;
use crate::ops;
use crate::value::Value;

impl Machine<'_> {
    /// Whether `def` names a unit declaration, so a call to it is a unit constructor.
    pub(crate) fn is_unit_ctor(&self, def: DefId) -> bool {
        matches!(self.program.defs[def].kind, DefKind::Unit(_))
    }

    /// `value` times the exact scale `numerator / denominator`.
    ///
    /// A whole scale keeps an `Int` an `Int`. Any other scale must be an exact finite decimal, so the denominator has
    /// only the factors 2 and 5; the product is then `coefficient * 10^-digits` for a whole coefficient. A scale with
    /// any other denominator has no finite decimal form and stops with `NotYet`.
    pub(crate) fn scale_value(
        &self,
        value: &Value,
        numerator: &BigInt,
        denominator: &BigInt,
    ) -> Result<Value, Stop> {
        if *denominator == BigInt::from(1) {
            return match value {
                Value::Int(number) => {
                    ops::checked(number * numerator, &self.limits).map(Value::Int)
                }
                Value::Decimal(decimal) => {
                    let product = ops::checked(decimal.coefficient() * numerator, &self.limits)?;
                    Decimal::new(product, decimal.exponent10().clone()).map(Value::Decimal)
                }
                _ => Err(Stop::NotYet("unit value of a non-numeric value (H4)")),
            };
        }
        let (power, digits) = power_of_ten_divisible_by(denominator).ok_or(Stop::NotYet(
            "unit scale without a finite decimal form (H4)",
        ))?;
        let coefficient = numerator * (&power / denominator);
        match value {
            Value::Int(number) => {
                let product = ops::checked(number * coefficient, &self.limits)?;
                Decimal::new(product, digits).map(Value::Decimal)
            }
            Value::Decimal(decimal) => {
                let product = ops::checked(decimal.coefficient() * coefficient, &self.limits)?;
                Decimal::new(product, decimal.exponent10() + digits).map(Value::Decimal)
            }
            _ => Err(Stop::NotYet("unit value of a non-numeric value (H4)")),
        }
    }
}

/// The smallest power of ten divisible by `denominator`, with its exponent (a non-positive `BigInt`), or `None` when
/// `denominator` has a prime factor other than 2 or 5 and so no power of ten is divisible by it.
fn power_of_ten_divisible_by(denominator: &BigInt) -> Option<(BigInt, BigInt)> {
    let mut rest = denominator.clone();
    for prime in [2, 5] {
        let prime = BigInt::from(prime);
        while (&rest % &prime) == BigInt::from(0) {
            rest /= &prime;
        }
    }
    if rest != BigInt::from(1) {
        return None;
    }
    let mut power = BigInt::from(1);
    let mut digits = BigInt::from(0);
    while (&power % denominator) != BigInt::from(0) {
        power *= 10;
        digits -= 1;
    }
    Some((power, digits))
}
