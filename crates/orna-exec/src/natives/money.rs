//! The `core.money` engine natives (hir-host.md B6): the arithmetic and comparison of `Money<C>` values. Two amounts add,
//! subtract and compare only in the same currency; the currency is the id each value carries. Each call charges one step.

use std::cmp::Ordering;

use orna_hir::Ty;

use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives::values::exactly;
use crate::value::Value;

/// The two operands of a binary money native, which must name the same currency.
fn same_currency<'a>(
    left: &'a Value,
    right: &'a Value,
) -> Result<(&'a Decimal, &'a Decimal, [u8; 16]), Stop> {
    let (
        Value::Money {
            amount: left_amount,
            currency: left_currency,
        },
        Value::Money {
            amount: right_amount,
            currency: right_currency,
        },
    ) = (left, right)
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    if left_currency != right_currency {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    }
    Ok((left_amount, right_amount, *left_currency))
}

/// The amount and currency of one money operand.
fn money(value: &Value) -> Result<(&Decimal, [u8; 16]), Stop> {
    let Value::Money { amount, currency } = value else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    Ok((amount, *currency))
}

/// `money_add(Money<C>, Money<C>): Money<C>`.
pub(super) fn add(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [left, right] = exactly::<2>(arguments)?;
    let (left_amount, right_amount, currency) = same_currency(&left, &right)?;
    Ok(Value::Money {
        amount: left_amount.add(right_amount)?,
        currency,
    })
}

/// `money_sub(Money<C>, Money<C>): Money<C>`.
pub(super) fn sub(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [left, right] = exactly::<2>(arguments)?;
    let (left_amount, right_amount, currency) = same_currency(&left, &right)?;
    Ok(Value::Money {
        amount: left_amount.sub(right_amount)?,
        currency,
    })
}

/// `money_scale(Money<C>, Decimal): Money<C>`: the amount times a decimal factor.
pub(super) fn scale(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount, factor] = exactly::<2>(arguments)?;
    let (amount, currency) = money(&amount)?;
    let Value::Decimal(factor) = factor else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    Ok(Value::Money {
        amount: amount.multiply(&factor)?,
        currency,
    })
}

/// `money_divide(Money<C>, Decimal): Money<C>`: the amount divided by a decimal divisor, exactly.
pub(super) fn divide(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount, divisor] = exactly::<2>(arguments)?;
    let (amount, currency) = money(&amount)?;
    let Value::Decimal(divisor) = divisor else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    Ok(Value::Money {
        amount: amount.divide(&divisor)?,
        currency,
    })
}

/// `money_negate(Money<C>): Money<C>`.
pub(super) fn negate(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount] = exactly::<1>(arguments)?;
    let (amount, currency) = money(&amount)?;
    Ok(Value::Money {
        amount: amount.negate()?,
        currency,
    })
}

/// `money_compare(Money<C>, Money<C>): Ordering`.
pub(super) fn compare(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [left, right] = exactly::<2>(arguments)?;
    let (left_amount, right_amount, _) = same_currency(&left, &right)?;
    let ordering: Ordering = left_amount.compare(right_amount)?;
    machine.ordering_value(ordering)
}
