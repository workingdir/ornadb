//! The published codes of `Money` and `Float` failures (hir-host.md B5).
//!
//! Typeck reads these pure functions before it words the generic message, so the code stays kernel-owned (P7-A:
//! "Codes are stable and kernel-owned"). The library supplies only the text.

use crate::diag::{
    AFFINE_POINT_OPERATOR, AFFINE_POINT_SUM, MONEY_BINARY_FLOAT, MONEY_CURRENCY_MISMATCH,
    MONEY_FROM_FLOAT, QUANTITY_DIMENSIONS,
};
use crate::units::print::unit_name;
use crate::units::{Rational, UnitKey};
use crate::{BinOp, DefId, DefKind, Program, Ty, TyKind, engine_money};

/// The affine point type of `core.temperature`, whose `+` and `-` between two points has no instance.
const TEMPERATURE: &str = "Temperature";
const TEMPERATURE_MODULE: &str = "core.temperature";

/// Whether `ty` is a binary64 `Float`, plain or carrying a unit key (`Float<kWh>` is `Quantity(Float, _)`).
fn is_float(program: &Program, ty: Ty) -> bool {
    match program.tys.kind(ty) {
        Some(TyKind::Float) => true,
        Some(TyKind::Quantity(value, _)) => matches!(program.tys.kind(*value), Some(TyKind::Float)),
        _ => false,
    }
}

/// Whether `ty` is a concrete nominal currency, so two of them can be compared for equality.
fn is_concrete_currency(program: &Program, ty: Ty) -> bool {
    matches!(program.tys.kind(ty), Some(TyKind::Adt(..)))
}

/// The published code for a failed binary operator, when the operand types name a known class of failure.
///
/// Callers pass resolved types; an unresolved variable matches no rule. `Money<C> * Money<C>` matches no rule, so
/// it keeps the generic no-instance diagnostic.
#[must_use]
pub fn operator_rejection(
    program: &Program,
    op: BinOp,
    left: Ty,
    right: Ty,
) -> Option<&'static str> {
    money_rejection(program, op, left, right)
        .or_else(|| affine_rejection(program, op, left, right))
        .or_else(|| dimension_rejection(program, op, left, right))
}

/// The `Money` classes of `operator_rejection`: `E5003` for two currencies, `E5103` for a `Float` operand.
fn money_rejection(program: &Program, op: BinOp, left: Ty, right: Ty) -> Option<&'static str> {
    let money_left = engine_money::money_currency(program, left);
    let money_right = engine_money::money_currency(program, right);
    match (money_left, money_right) {
        (Some(left_currency), Some(right_currency)) => {
            let mismatch = matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
            ) && is_concrete_currency(program, left_currency)
                && is_concrete_currency(program, right_currency)
                && left_currency != right_currency;
            mismatch.then_some(MONEY_CURRENCY_MISMATCH)
        }
        (Some(_), None) if is_float(program, right) => {
            money_float_operator(op).then_some(MONEY_BINARY_FLOAT)
        }
        (None, Some(_)) if is_float(program, left) => {
            money_float_operator(op).then_some(MONEY_BINARY_FLOAT)
        }
        _ => None,
    }
}

/// Whether `ty` is the `core.temperature.Temperature` affine point.
fn is_temperature(program: &Program, ty: Ty) -> bool {
    let Some(TyKind::Adt(owner, _)) = program.tys.kind(ty) else {
        return false;
    };
    let definition = &program.defs[*owner];
    matches!(definition.kind, DefKind::Type(_))
        && definition.name.as_str() == TEMPERATURE
        && program.modules[definition.module].path.to_string() == TEMPERATURE_MODULE
}

/// `E5002`: `+` with two affine points. `point - point` is a `Delta` (the library's `Sub<Self, Delta>` instance), so
/// only addition is rejected here.
fn affine_rejection(program: &Program, op: BinOp, left: Ty, right: Ty) -> Option<&'static str> {
    (matches!(op, BinOp::Add) && is_temperature(program, left) && is_temperature(program, right))
        .then_some(AFFINE_POINT_OPERATOR)
}

/// `E5001`: `+` or `-` with two quantities whose units have different dimensions, or with a `Duration` and a quantity
/// that is not a time. A `Duration` is the time dimension (units.md section 8.1), so only a quantity of that dimension
/// may meet it. A quantity and a plain number stay the `ORNA-UNIT-DIM` class.
fn dimension_rejection(program: &Program, op: BinOp, left: Ty, right: Ty) -> Option<&'static str> {
    if !matches!(op, BinOp::Add | BinOp::Sub) {
        return None;
    }
    let mismatch = match (program.tys.kind(left), program.tys.kind(right)) {
        (Some(TyKind::Quantity(_, left_key)), Some(TyKind::Quantity(_, right_key))) => !program
            .units
            .key(*left_key)
            .same_dimension(program.units.key(*right_key)),
        (Some(TyKind::Duration), Some(TyKind::Quantity(_, key)))
        | (Some(TyKind::Quantity(_, key)), Some(TyKind::Duration)) => {
            !is_time_key(program, program.units.key(*key))
        }
        _ => false,
    };
    mismatch.then_some(QUANTITY_DIMENSIONS)
}

/// Whether `key` is the time dimension: the base unit `Time.second` to the first power, at scale 1 (units.md section 8.1).
fn is_time_key(program: &Program, key: &UnitKey) -> bool {
    let mut axes = key.exponents().iter();
    let (Some((axis, 1)), None) = (axes.next(), axes.next()) else {
        return false;
    };
    *key.scale() == Rational::one() && unit_name(program, axis) == "Time.second"
}

/// The published code for a fold (`sum`) over an element type, when the element is an affine point (`E5003`).
#[must_use]
pub fn sum_rejection(program: &Program, element: Ty) -> Option<&'static str> {
    is_temperature(program, element).then_some(AFFINE_POINT_SUM)
}

/// Whether one `Money` operand with one `Float` operand is the `E5103` class for `op`.
fn money_float_operator(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
    )
}

/// The published code for a constructor call `Money<C>(value)` whose amount is a `Float` (hir-host.md B5).
#[must_use]
pub fn construction_rejection(program: &Program, def: DefId, argument: Ty) -> Option<&'static str> {
    (engine_money::is_money_def(program, def) && is_float(program, argument))
        .then_some(MONEY_FROM_FLOAT)
}
