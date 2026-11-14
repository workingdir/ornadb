//! Typing of unary, binary and logical operator expressions.

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use crate::{BinOp, Body, ExprId, LogicOp, Ty, TyKind, UnOp};

use super::{BodyCheck, NotYetReason};

impl BodyCheck<'_> {
    pub(super) fn unary_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: UnOp,
        operand: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let operand_ty = self.infer_expr(body, operand, None)?;
        let resolved = self.resolved(operand_ty);
        if let Some(result) = self.unary_operator_ty(body, id, op, operand, resolved, span.clone())
        {
            return Ok(result);
        }
        match crate::ops::unary(&mut self.program.tys, op, resolved) {
            Some(result) => Ok(result),
            None => {
                if matches!(self.kind_of(resolved), Some(TyKind::Error)) {
                    return Ok(self.error());
                }
                self.conflict(
                    format!(
                        "the unary operator is not defined for {}",
                        crate::print::print_type(self.program, resolved)
                    ),
                    span,
                );
                Ok(self.error())
            }
        }
    }

    pub(super) fn binary_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        // A currency literal's lowered `Money<C>` is read before inference overwrites the operand's type (hir-host.md B4).
        let lowered_left = body.exprs[lhs].ty;
        let lowered_right = body.exprs[rhs].ty;
        let left = self.infer_expr(body, lhs, None)?;
        let right = self.infer_expr(body, rhs, None)?;
        let money_left = self.money_operand(lowered_left, left);
        let money_right = self.money_operand(lowered_right, right);
        if let Some(error) =
            self.money_operator_rejection(op, money_left, money_right, span.clone())
        {
            return Ok(error);
        }
        if let Some(result) = self.quantity_binary(body, op, left, right, rhs, span.clone()) {
            return Ok(result);
        }
        if let Some(result) = self.range_membership_ty(op, left, right, span.clone()) {
            return Ok(result);
        }
        let comparison = matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        );
        if comparison {
            let before = self.conflicts;
            self.unify(left, right, span.clone());
            if self.conflicts > before {
                return Ok(self.error());
            }
        }
        let left_resolved = self.zonk(left);
        let right_resolved = self.zonk(right);
        if let Some(result) =
            crate::ops::binary(&mut self.program.tys, op, left_resolved, right_resolved)
        {
            return Ok(result);
        }
        if let Some(result) = self.binary_operator_ty(
            body,
            id,
            op,
            (lhs, rhs),
            (left_resolved, right_resolved),
            span.clone(),
        ) {
            return Ok(result);
        }
        let unsolved = [(lhs, left_resolved), (rhs, right_resolved)]
            .into_iter()
            .find(|&(_, ty)| matches!(self.kind_of(ty), Some(TyKind::Infer(_))));
        if let Some((operand, _)) = unsolved {
            let before = self.conflicts;
            self.unify(left, right, span.clone());
            let left_resolved = self.resolved(left);
            let right_resolved = self.resolved(right);
            if let Some(result) =
                crate::ops::binary(&mut self.program.tys, op, left_resolved, right_resolved)
            {
                return Ok(result);
            }
            if self.conflicts == before {
                // The operand's type is still open: it is underconstrained, so
                // the smallest useful site is the operand itself (ORNA-INFER-007).
                self.ambiguous(body.exprs[operand].span.clone());
            }
            return Ok(self.error());
        }
        if matches!(self.kind_of(left_resolved), Some(TyKind::Error))
            || matches!(self.kind_of(right_resolved), Some(TyKind::Error))
        {
            return Ok(self.error());
        }
        self.conflict(
            format!(
                "the operator is not defined for {} and {}",
                crate::print::print_type(self.program, left_resolved),
                crate::print::print_type(self.program, right_resolved)
            ),
            span,
        );
        Ok(self.error())
    }

    pub(super) fn logic_ty(
        &mut self,
        body: &mut Body,
        op: LogicOp,
        lhs: ExprId,
        rhs: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        if op == LogicOp::Coalesce {
            let left = self.infer_expr(body, lhs, None)?;
            let left_resolved = self.resolved(left);
            let Some(TyKind::Option(inner)) = self.kind_of(left_resolved) else {
                if matches!(self.kind_of(left_resolved), Some(TyKind::Error)) {
                    return Ok(self.error());
                }
                self.conflict("the left side of `??` must be an option", span);
                return Ok(self.error());
            };
            let right = self.infer_expr(body, rhs, Some(inner))?;
            self.unify(inner, right, span.clone());
            return Ok(self.resolved(inner));
        }
        let bool_ty = self.intern(TyKind::Bool);
        let left = self.infer_expr(body, lhs, Some(bool_ty))?;
        let right = self.infer_expr(body, rhs, Some(bool_ty))?;
        self.unify(left, bool_ty, span.clone());
        self.unify(right, bool_ty, span.clone());
        Ok(bool_ty)
    }
}
