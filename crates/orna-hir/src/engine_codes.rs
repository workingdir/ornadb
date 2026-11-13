//! The published codes of `Money` and `Float` failures (hir-host.md B5).
//!
//! Typeck reads these pure functions before it words the generic message, so the code stays kernel-owned (P7-A:
//! "Codes are stable and kernel-owned"). The library supplies only the text.

use crate::diag::{MONEY_BINARY_FLOAT, MONEY_CURRENCY_MISMATCH, MONEY_FROM_FLOAT};
use crate::{BinOp, DefId, Program, Ty, TyKind, engine_money};

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
