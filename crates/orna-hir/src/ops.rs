//! Operator tables over [`Ty`] (design section 2.3).
//!
//! The table is the one the old checker admits
//! (`orna-check/src/inference.rs:960-1150`), expressed over interned types:
//!
//! * arithmetic is defined on two operands of the same type and there are no
//!   implicit `Int`/`Decimal`/`Float` conversions (decisions D7, ORNA-NUM-001
//!   to ORNA-NUM-003);
//! * `+`/`-` also move `Instant` by a `Duration` and subtract one `Instant`
//!   from another (`Duration` is the result); a `Duration` scales by an exact
//!   `Int` or `Decimal` (ORNA-TIME-004);
//! * ordered comparison is defined only for the ordered scalar types and
//!   requires both operands to have the same type (ORNA-COMPARE-002);
//! * `&&`/`||` require `Bool` because there is no implicit truthiness
//!   (ORNA-CFLOW-009);
//! * `??` removes one option and requires the fallback to have the payload's
//!   type (ORNA-COALESCE-001).
//!
//! A rule the table does not admit returns `None`; the type pass reports the
//! operand pair with ORNA-INFER-011. User-declared units and dimensions are
//! outside this table: decision D8 removes them, and stage H2c deletes the
//! checker's dimensional algebra.

use crate::{BinOp, LogicOp, Ty, TyInterner, TyKind, UnOp};

/// The result type of a unary operator, or `None` when the operand is not
/// admitted.
#[must_use]
pub fn unary(types: &mut TyInterner, op: UnOp, operand: Ty) -> Option<Ty> {
    let kind = types.kind(operand)?.clone();
    match op {
        UnOp::Not => matches!(kind, TyKind::Bool).then(|| types.intern(TyKind::Bool)),
        UnOp::Neg => match kind {
            TyKind::Int
            | TyKind::Decimal
            | TyKind::Float
            | TyKind::Duration
            | TyKind::Instant
            | TyKind::Period => Some(operand),
            _ => None,
        },
    }
}

/// The result type of a binary arithmetic or comparison operator.
#[must_use]
pub fn binary(types: &mut TyInterner, op: BinOp, left: Ty, right: Ty) -> Option<Ty> {
    let left_kind = types.kind(left)?.clone();
    let right_kind = types.kind(right)?.clone();
    match op {
        BinOp::Eq | BinOp::Ne => (left == right).then(|| types.intern(TyKind::Bool)),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            (left == right && ordered(&left_kind)).then(|| types.intern(TyKind::Bool))
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
            arithmetic(types, op, left, &left_kind, &right_kind)
        }
    }
}

/// The result type of a short-circuiting logical operator.
#[must_use]
pub fn logic(types: &mut TyInterner, op: LogicOp, left: Ty, right: Ty) -> Option<Ty> {
    match op {
        LogicOp::And | LogicOp::Or => {
            (is_bool(types, left) && is_bool(types, right)).then(|| types.intern(TyKind::Bool))
        }
        LogicOp::Coalesce => match types.kind(left).cloned() {
            Some(TyKind::Option(inner)) if inner == right => Some(inner),
            _ => None,
        },
    }
}

/// Whether a type has the canonical total order.
#[must_use]
pub fn ordered(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Bool
            | TyKind::Int
            | TyKind::Decimal
            | TyKind::Float
            | TyKind::Str
            | TyKind::Date
            | TyKind::Instant
            | TyKind::Duration
            | TyKind::Period
            | TyKind::Uuid
            | TyKind::Digest
    )
}

fn is_bool(types: &TyInterner, ty: Ty) -> bool {
    matches!(types.kind(ty), Some(TyKind::Bool))
}

/// The arithmetic table.
///
/// Arithmetic on a numeric type keeps that type. Time arithmetic is explicit:
/// only the pairs below are admitted, and every other pair is an operator
/// error rather than an implicit conversion.
fn arithmetic(
    types: &mut TyInterner,
    op: BinOp,
    left: Ty,
    left_kind: &TyKind,
    right_kind: &TyKind,
) -> Option<Ty> {
    if left_kind == right_kind && matches!(left_kind, TyKind::Int | TyKind::Decimal | TyKind::Float)
    {
        return Some(left);
    }
    match op {
        BinOp::Add => match (left_kind, right_kind) {
            (TyKind::Duration, TyKind::Duration) => Some(types.intern(TyKind::Duration)),
            (TyKind::Instant, TyKind::Duration) | (TyKind::Duration, TyKind::Instant) => {
                Some(types.intern(TyKind::Instant))
            }
            (TyKind::Period, TyKind::Period) => Some(types.intern(TyKind::Period)),
            _ => None,
        },
        BinOp::Sub => match (left_kind, right_kind) {
            (TyKind::Instant, TyKind::Duration) => Some(types.intern(TyKind::Instant)),
            (TyKind::Instant, TyKind::Instant) => Some(types.intern(TyKind::Duration)),
            (TyKind::Duration, TyKind::Duration) => Some(types.intern(TyKind::Duration)),
            (TyKind::Period, TyKind::Period) => Some(types.intern(TyKind::Period)),
            _ => None,
        },
        BinOp::Mul => match (left_kind, right_kind) {
            (TyKind::Duration, TyKind::Int | TyKind::Decimal)
            | (TyKind::Int | TyKind::Decimal, TyKind::Duration) => {
                Some(types.intern(TyKind::Duration))
            }
            _ => None,
        },
        BinOp::Div => match (left_kind, right_kind) {
            (TyKind::Duration, TyKind::Int | TyKind::Decimal) => {
                Some(types.intern(TyKind::Duration))
            }
            _ => None,
        },
        BinOp::Rem | BinOp::Pow => None,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{binary, logic, unary};
    use crate::{BinOp, LogicOp, TyInterner, TyKind, UnOp};

    #[test]
    fn numeric_arithmetic_keeps_its_type_and_never_mixes() {
        let mut types = TyInterner::new();
        let int = types.intern(TyKind::Int);
        let decimal = types.intern(TyKind::Decimal);
        assert_eq!(binary(&mut types, BinOp::Add, int, int), Some(int));
        assert_eq!(binary(&mut types, BinOp::Add, int, decimal), None);
        assert_eq!(
            binary(&mut types, BinOp::Pow, decimal, decimal),
            Some(decimal)
        );
    }

    #[test]
    fn time_arithmetic_is_explicit() {
        let mut types = TyInterner::new();
        let instant = types.intern(TyKind::Instant);
        let duration = types.intern(TyKind::Duration);
        assert_eq!(
            binary(&mut types, BinOp::Sub, instant, instant),
            Some(duration)
        );
        assert_eq!(
            binary(&mut types, BinOp::Add, instant, duration),
            Some(instant)
        );
        assert_eq!(binary(&mut types, BinOp::Add, instant, instant), None);
    }

    #[test]
    fn comparison_requires_one_ordered_type() {
        let mut types = TyInterner::new();
        let int = types.intern(TyKind::Int);
        let str_ty = types.intern(TyKind::Str);
        let list = types.intern(TyKind::List(int));
        assert_eq!(
            binary(&mut types, BinOp::Lt, int, int),
            Some(types.intern(TyKind::Bool))
        );
        assert_eq!(
            binary(&mut types, BinOp::Lt, str_ty, str_ty),
            Some(types.intern(TyKind::Bool))
        );
        assert_eq!(binary(&mut types, BinOp::Lt, list, list), None);
        assert_eq!(
            binary(&mut types, BinOp::Eq, list, list),
            Some(types.intern(TyKind::Bool))
        );
    }

    #[test]
    fn logic_requires_bool_and_coalesce_removes_one_option() {
        let mut types = TyInterner::new();
        let bool_ty = types.intern(TyKind::Bool);
        let int = types.intern(TyKind::Int);
        let option = types.intern(TyKind::Option(int));
        assert_eq!(
            logic(&mut types, LogicOp::And, bool_ty, bool_ty),
            Some(bool_ty)
        );
        assert_eq!(logic(&mut types, LogicOp::And, int, bool_ty), None);
        assert_eq!(logic(&mut types, LogicOp::Coalesce, option, int), Some(int));
        assert_eq!(logic(&mut types, LogicOp::Coalesce, option, bool_ty), None);
    }

    #[test]
    fn unary_sign_keeps_numeric_and_rejects_bool() {
        let mut types = TyInterner::new();
        let int = types.intern(TyKind::Int);
        let bool_ty = types.intern(TyKind::Bool);
        let duration = types.intern(TyKind::Duration);
        assert_eq!(unary(&mut types, UnOp::Neg, int), Some(int));
        assert_eq!(unary(&mut types, UnOp::Neg, duration), Some(duration));
        assert_eq!(unary(&mut types, UnOp::Neg, bool_ty), None);
        assert_eq!(unary(&mut types, UnOp::Not, bool_ty), Some(bool_ty));
        assert_eq!(unary(&mut types, UnOp::Not, int), None);
    }
}
