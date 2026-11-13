//! Typing of quantities: the unit-key kernel (units.md sections 6 and 11, ORNA-UNIT-011 to 016).
//!
//! A `Quantity<N, U>` is a numeric value of type `N` in the unit key `U`. Its arithmetic is kernel work that runs before
//! the protocol path: keys combine by exponent and scale, and numeric types follow the exactness rules. A binary
//! operator is claimed here only when one of its operands is a quantity; plain numerics keep their existing typing.

use std::cmp::Ordering;

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use super::{BodyCheck, NotYetReason};
use crate::diag::{INFER_CONFLICT, UNIT_DIM, UNIT_INEXACT, UNIT_LIMIT, UNIT_POW};
use crate::ids::UnitKeyId;
use crate::units::print::key_text;
use crate::units::{KeyError, Rational, UnitKey};
use crate::{
    ArgValue, BinOp, Body, Call, DefId, DefKind, ExprId, ExprKind, HirDiagnostic, Lit, Program, Ty,
    TyKind,
};

/// The numeric type of a quantity's value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Num {
    Int,
    Decimal,
    Float,
}

impl Num {
    pub(super) fn of(kind: &TyKind) -> Option<Self> {
        match kind {
            TyKind::Int => Some(Self::Int),
            TyKind::Decimal => Some(Self::Decimal),
            TyKind::Float => Some(Self::Float),
            _ => None,
        }
    }

    fn kind(self) -> TyKind {
        match self {
            Self::Int => TyKind::Int,
            Self::Decimal => TyKind::Decimal,
            Self::Float => TyKind::Float,
        }
    }

    /// The common numeric type of two values: a `Float` never mixes with an exact type, and `Decimal` absorbs `Int`.
    fn join(self, other: Self) -> Result<Self, Fail> {
        match (self, other) {
            (Self::Float, Self::Float) => Ok(Self::Float),
            (Self::Float, _) | (_, Self::Float) => Err(Fail::new(
                INFER_CONFLICT,
                "a Float value does not combine with an Int or Decimal value",
            )),
            (Self::Int, Self::Int) => Ok(Self::Int),
            _ => Ok(Self::Decimal),
        }
    }
}

/// A rejected quantity operation: its diagnostic code and message.
pub(super) struct Fail {
    code: &'static str,
    message: String,
}

impl Fail {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Maps a unit-key arithmetic failure to the evaluator's limit diagnostic.
pub(super) fn budget(_: KeyError) -> Fail {
    Fail::new(
        UNIT_LIMIT,
        "the unit key arithmetic leaves the evaluator's budget",
    )
}

/// One operand of a quantity operation: its numeric type, its unit key and whether it is a quantity.
#[derive(Clone, Copy)]
struct Operand {
    num: Num,
    key: UnitKeyId,
    quantity: bool,
}

/// Converts a value of type `num` by `factor`, keeping it exact. An integer factor keeps `Int` and `Decimal`; a finite
/// decimal factor turns an `Int` into a `Decimal`; any other factor has no exact decimal form. A `Float` stays a
/// `Float` (ORNA-UNIT-016).
pub(super) fn scaled(num: Num, factor: &Rational) -> Result<Num, Fail> {
    if num == Num::Float || factor.is_integer() {
        return Ok(num);
    }
    if factor.is_finite_decimal() {
        return Ok(Num::Decimal);
    }
    Err(Fail::new(
        UNIT_INEXACT,
        "the conversion between these units has no exact decimal form",
    ))
}

/// Whether `def` names a unit declaration, whose literal is typed by [`BodyCheck::unit_ctor_ty`].
pub(super) fn is_unit(program: &Program, def: DefId) -> bool {
    matches!(
        program.defs.get(def).map(|definition| &definition.kind),
        Some(DefKind::Unit(_))
    )
}

impl<'a> BodyCheck<'a> {
    /// Types a binary operator with a quantity operand, or returns `None` when neither operand is a quantity, or when
    /// an operand is not numeric, leaving the operands to the existing typing. `%` on quantities is not kernel work
    /// and also returns `None`.
    pub(super) fn quantity_binary(
        &mut self,
        body: &Body,
        op: BinOp,
        lhs: Ty,
        rhs: Ty,
        rhs_expr: ExprId,
        span: SyntaxSpan,
    ) -> Option<Ty> {
        if matches!(op, BinOp::Rem | BinOp::In)
            || (!self.is_quantity(lhs) && !self.is_quantity(rhs))
        {
            return None;
        }
        let left = self.operand(lhs)?;
        let right = self.operand(rhs)?;
        let result = if matches!(op, BinOp::Pow) {
            if !left.quantity {
                return None;
            }
            self.power(body, left, rhs_expr)
        } else {
            self.binary(op, left, right)
        };
        Some(match result {
            Ok(ty) => ty,
            Err(fail) => self.report(fail, span),
        })
    }

    /// Types a unit literal such as `5.kilometre` or `Length.kilometre(5)`: its single numeric value, folded with the
    /// unit's key.
    pub(super) fn unit_ctor_ty(
        &mut self,
        body: &mut Body,
        def: DefId,
        call: &Call,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let [argument] = &*call.args else {
            return Err(NotYetReason::Unsupported(
                "a unit literal takes exactly one value".into(),
            ));
        };
        let ArgValue::Expr(value) = argument.value else {
            return Err(NotYetReason::Unsupported(
                "a unit literal takes its value as written".into(),
            ));
        };
        let value_ty = self.infer_expr(body, value, None)?;
        let value_ty = self.resolved(value_ty);
        let Some(kind) = self.kind_of(value_ty) else {
            return Ok(self.error());
        };
        if matches!(kind, TyKind::Infer(_)) {
            return Err(NotYetReason::Unsupported(
                "a unit literal waits for its value's type".into(),
            ));
        }
        let Some(num) = Num::of(&kind) else {
            self.conflict("a unit literal needs a numeric value", span);
            return Ok(self.error());
        };
        let Some(key) = self.program.units.unit_key(def) else {
            return Err(NotYetReason::Unsupported(
                "the unit's key is not computed yet".into(),
            ));
        };
        Ok(match self.fold(num, key) {
            Ok(ty) => ty,
            Err(fail) => self.report(fail, span),
        })
    }

    fn is_quantity(&self, ty: Ty) -> bool {
        matches!(self.kind_of(self.resolved(ty)), Some(TyKind::Quantity(..)))
    }

    /// The operand view of a numeric or quantity type; `None` for any other type, including an unresolved variable.
    fn operand(&mut self, ty: Ty) -> Option<Operand> {
        let ty = self.resolved(ty);
        match self.kind_of(ty)? {
            TyKind::Quantity(value, key) => {
                let num = Num::of(&self.kind_of(value)?)?;
                Some(Operand {
                    num,
                    key,
                    quantity: true,
                })
            }
            kind => {
                let num = Num::of(&kind)?;
                let key = self.program.units.intern(UnitKey::dimensionless());
                Some(Operand {
                    num,
                    key,
                    quantity: false,
                })
            }
        }
    }

    fn binary(&mut self, op: BinOp, left: Operand, right: Operand) -> Result<Ty, Fail> {
        match op {
            BinOp::Add | BinOp::Sub => {
                let (num, key) = self.common(left, right)?;
                self.fold(num, key)
            }
            BinOp::Mul | BinOp::Div => {
                let (num, key) = self.product(op, left, right)?;
                self.fold(num, key)
            }
            BinOp::Eq | BinOp::Ne => {
                if left.key != right.key {
                    return Err(self.mismatch(left.key, right.key));
                }
                left.num.join(right.num)?;
                Ok(self.intern(TyKind::Bool))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.common(left, right)?;
                Ok(self.intern(TyKind::Bool))
            }
            BinOp::Rem | BinOp::Pow | BinOp::In => {
                unreachable!("quantity_binary routes Rem, Pow and In before binary")
            }
        }
    }

    /// The common numeric type and unit of two operands for addition, subtraction and ordering (ORNA-UNIT-011). The
    /// result is the finer unit; the other operand converts by the exact ratio of its scale to the finer one.
    fn common(&mut self, left: Operand, right: Operand) -> Result<(Num, UnitKeyId), Fail> {
        let left_key = self.program.units.key(left.key).clone();
        let right_key = self.program.units.key(right.key).clone();
        if !left_key.same_dimension(&right_key) {
            return Err(self.dimension_mismatch(left.key, right.key));
        }
        let fine = if left_key.scale().compare(right_key.scale()) == Ordering::Greater {
            right.key
        } else {
            left.key
        };
        let fine_key = self.program.units.key(fine).clone();
        let left_factor = left_key.ratio_to(&fine_key).map_err(budget)?;
        let right_factor = right_key.ratio_to(&fine_key).map_err(budget)?;
        let num = scaled(left.num, &left_factor)?.join(scaled(right.num, &right_factor)?)?;
        Ok((num, fine))
    }

    /// The product or quotient (ORNA-UNIT-012): the numeric type follows the operator's rule on the value types, and
    /// the key follows exponent arithmetic.
    fn product(
        &mut self,
        op: BinOp,
        left: Operand,
        right: Operand,
    ) -> Result<(Num, UnitKeyId), Fail> {
        let left_ty = self.intern(left.num.kind());
        let right_ty = self.intern(right.num.kind());
        let num_ty =
            crate::ops::binary(&mut self.program.tys, op, left_ty, right_ty).ok_or_else(|| {
                Fail::new(
                    INFER_CONFLICT,
                    "the operator is not defined for these numeric types",
                )
            })?;
        let num = self
            .kind_of(num_ty)
            .as_ref()
            .and_then(Num::of)
            .ok_or_else(|| {
                Fail::new(
                    INFER_CONFLICT,
                    "the operator does not produce a numeric type",
                )
            })?;
        let left_key = self.program.units.key(left.key).clone();
        let right_key = self.program.units.key(right.key).clone();
        let key = if matches!(op, BinOp::Mul) {
            left_key.mul(&right_key)
        } else {
            left_key.div(&right_key)
        }
        .map_err(budget)?;
        Ok((num, self.program.units.intern(key)))
    }

    /// `base ^ n` for a literal `Int` exponent `n` (ORNA-UNIT-013): the key is raised to `n`, the numeric type kept.
    fn power(&mut self, body: &Body, base: Operand, exponent: ExprId) -> Result<Ty, Fail> {
        let ExprKind::Lit(Lit::Int(value)) = &body.exprs[exponent].kind else {
            return Err(Fail::new(
                UNIT_POW,
                "a quantity power takes a literal Int exponent",
            ));
        };
        let exponent = value
            .to_string()
            .parse::<i32>()
            .map_err(|_| Fail::new(UNIT_POW, "a quantity power exponent is out of range"))?;
        let base_key = self.program.units.key(base.key).clone();
        let key = base_key.pow(exponent).map_err(budget)?;
        let key = self.program.units.intern(key);
        self.fold(base.num, key)
    }

    /// Folds a value of type `num` in unit `key`: a quantity when the key has dimension (ORNA-UNIT-011 to 013), otherwise
    /// the plain numeric type its scale leaves (ORNA-UNIT-014).
    pub(super) fn fold(&mut self, num: Num, key: UnitKeyId) -> Result<Ty, Fail> {
        let unit = self.program.units.key(key).clone();
        if !unit.is_dimensionless() {
            let value = self.intern(num.kind());
            return Ok(self.intern(TyKind::Quantity(value, key)));
        }
        let num = scaled(num, unit.scale())?;
        Ok(self.intern(num.kind()))
    }

    /// The diagnostic for `==` or `!=` on quantities whose keys differ (C15): a dimension mismatch is ORNA-UNIT-DIM,
    /// otherwise a conflict.
    fn mismatch(&self, left: UnitKeyId, right: UnitKeyId) -> Fail {
        if !self
            .program
            .units
            .key(left)
            .same_dimension(self.program.units.key(right))
        {
            return self.dimension_mismatch(left, right);
        }
        Fail::new(
            INFER_CONFLICT,
            format!(
                "{} and {} are different quantity types",
                key_text(self.program, left),
                key_text(self.program, right)
            ),
        )
    }

    /// The ORNA-UNIT-DIM diagnostic naming both printed dimensions.
    fn dimension_mismatch(&self, left: UnitKeyId, right: UnitKeyId) -> Fail {
        Fail::new(
            UNIT_DIM,
            format!(
                "{} and {} have different dimensions",
                key_text(self.program, left),
                key_text(self.program, right)
            ),
        )
    }

    /// Records a rejected quantity operation as a constraint conflict and yields the error type.
    pub(super) fn report(&mut self, fail: Fail, span: SyntaxSpan) -> Ty {
        self.conflicts += 1;
        self.diagnostics
            .push(HirDiagnostic::new(fail.code, fail.message, span));
        self.error()
    }
}
