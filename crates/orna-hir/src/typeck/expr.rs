//! Expression type inference, grouped into functions below the budget's
//! function-size cap.

use orna_syntax::SyntaxSpan;

use crate::{Arm, AssignOp, BinOp, Body, ExprId, ExprKind, Ty, TyKind};

use super::{BodyCheck, NotYetReason};

impl<'a> BodyCheck<'a> {
    /// Types one expression by dispatching to the family that owns its shape.
    pub(super) fn infer_shallow(
        &mut self,
        body: &mut Body,
        id: ExprId,
        kind: ExprKind,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let group = match &kind {
            ExprKind::Lit(_)
            | ExprKind::Local(_)
            | ExprKind::Def(_)
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::Logic { .. }
            | ExprKind::Range { .. } => 0u8,
            ExprKind::Call(_)
            | ExprKind::Recover { .. }
            | ExprKind::Field { .. }
            | ExprKind::Index { .. }
            | ExprKind::Tuple(_)
            | ExprKind::List(_)
            | ExprKind::Record(_)
            | ExprKind::Construct { .. }
            | ExprKind::Lambda { .. }
            | ExprKind::Interp(_) => 1,
            ExprKind::Block { .. }
            | ExprKind::If { .. }
            | ExprKind::Case { .. }
            | ExprKind::For { .. }
            | ExprKind::While { .. }
            | ExprKind::Loop { .. } => 2,
            _ => 3,
        };
        match group {
            0 => self.infer_shallow_atoms(body, id, kind, span),
            1 => self.infer_shallow_values(body, id, kind, span),
            2 => self.infer_shallow_control(body, id, kind, span),
            _ => self.infer_shallow_jumps(body, id, kind, span),
        }
    }

    fn infer_shallow_atoms(
        &mut self,
        body: &mut Body,
        _id: ExprId,
        kind: ExprKind,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match kind {
            ExprKind::Lit(literal) => self.literal_ty(&literal, span),
            ExprKind::Local(local) => Ok(body.locals[local].ty),
            ExprKind::Def(def) => self.def_value_ty(def, span),
            ExprKind::Unary { op, operand } => self.unary_ty(body, op, operand, span),
            ExprKind::Binary { op, lhs, rhs } => self.binary_ty(body, op, lhs, rhs, span),
            ExprKind::Logic { op, lhs, rhs } => self.logic_ty(body, op, lhs, rhs, span),
            ExprKind::Range {
                lo,
                hi,
                inclusive: _,
            } => {
                let bound = match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        let left = self.infer_expr(body, lo, None)?;
                        let right = self.infer_expr(body, hi, None)?;
                        self.unify(left, right, span.clone());
                        self.resolved(left)
                    }
                    (Some(bound), None) | (None, Some(bound)) => {
                        self.infer_expr(body, bound, None)?
                    }
                    (None, None) => {
                        return Err(NotYetReason::Unsupported(
                            "an unbounded range has no H2a type".into(),
                        ));
                    }
                };
                match self.kind_of(self.resolved(bound)) {
                    Some(kind) if crate::ops::ordered(&kind) => {
                        Ok(self.intern(TyKind::Range(self.resolved(bound))))
                    }
                    _ => {
                        self.conflict("range bounds must have one ordered type", span);
                        Ok(self.error())
                    }
                }
            }
            _ => unreachable!("the dispatcher selected the atom group"),
        }
    }

    fn infer_shallow_values(
        &mut self,
        body: &mut Body,
        id: ExprId,
        kind: ExprKind,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match kind {
            ExprKind::Call(call) => {
                let mut call = *call;
                let ty = self.call_ty(body, &mut call, span.clone())?;
                body.exprs[id].kind = ExprKind::Call(Box::new(call));
                Ok(ty)
            }
            ExprKind::Recover { .. } => Err(NotYetReason::Unsupported(
                "`|?` recovery needs the `core` `Error` declaration; it is not on HIR until \
                 stage H2b/H5"
                    .into(),
            )),
            ExprKind::Field { base, field } => self.field_ty(body, id, base, &field, span),
            ExprKind::Index { base, index } => {
                let base_ty = self.infer_expr(body, base, None)?;
                let index_ty = self.infer_expr(body, index, None)?;
                let int = self.intern(TyKind::Int);
                self.unify(index_ty, int, span.clone());
                match self.kind_of(self.resolved(base_ty)) {
                    Some(TyKind::List(element)) => Ok(element),
                    Some(TyKind::Error) => Ok(self.error()),
                    _ => Err(NotYetReason::Unsupported(
                        "indexing is only on HIR for lists in H2a".into(),
                    )),
                }
            }
            ExprKind::Tuple(elements) => {
                let mut types = Vec::with_capacity(elements.len());
                for element in &elements {
                    types.push(self.infer_expr(body, *element, None)?);
                }
                Ok(self.intern(TyKind::Tuple(types.into_boxed_slice())))
            }
            ExprKind::List(elements) => {
                let element = self.infer.new_var(&mut self.program.tys, span.clone());
                for element_expr in &elements {
                    let ty = self.infer_expr(body, *element_expr, None)?;
                    self.unify(ty, element, span.clone());
                }
                Ok(self.intern(TyKind::List(self.resolved(element))))
            }
            ExprKind::Record(fields) => {
                let mut types = Vec::with_capacity(fields.len());
                for (name, value) in &fields {
                    types.push((name.clone(), self.infer_expr(body, *value, None)?));
                }
                Ok(self.intern(TyKind::Record(types.into_boxed_slice())))
            }
            ExprKind::Construct { def, fields } => {
                self.check_construct(body, def, &fields, None, span)
            }
            ExprKind::Lambda { body: child, .. } => self.check_lambda(child, None, body),
            ExprKind::Interp(parts) => {
                for part in parts.iter() {
                    if let crate::InterpPart::Expr(value) = part {
                        let ty = self.infer_expr(body, *value, None)?;
                        if !self.displayable(ty) {
                            return Err(NotYetReason::Unsupported(
                                "interpolation of this type needs a protocol member that H2a \
                                 does not resolve"
                                    .into(),
                            ));
                        }
                    }
                }
                Ok(self.intern(TyKind::Str))
            }
            _ => unreachable!("the dispatcher selected the value group"),
        }
    }

    fn infer_shallow_control(
        &mut self,
        body: &mut Body,
        _id: ExprId,
        kind: ExprKind,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match kind {
            ExprKind::Block { stmts, tail } => self.check_block(body, &stmts, tail, None, span),
            ExprKind::If { cond, then, els } => self.check_if(body, cond, then, els, None, span),
            ExprKind::Case { scrutinee, arms } => {
                self.check_case(body, scrutinee, &arms, None, span)
            }
            ExprKind::For {
                pat,
                iter,
                body: loop_body,
            } => {
                let iter_ty = self.infer_expr(body, iter, None)?;
                let element = self.iter_element(self.resolved(iter_ty)).ok_or_else(|| {
                    NotYetReason::Unsupported(
                        "`for` iterates a collection value that is not on HIR in H2a".into(),
                    )
                })?;
                self.bind_pattern(body, pat, element, span.clone())?;
                self.infer_expr(body, loop_body, None)?;
                Ok(self.intern(TyKind::Unit))
            }
            ExprKind::While {
                cond,
                body: loop_body,
            } => {
                let bool_ty = self.intern(TyKind::Bool);
                let cond_ty = self.infer_expr(body, cond, Some(bool_ty))?;
                self.unify(cond_ty, bool_ty, span.clone());
                self.infer_expr(body, loop_body, None)?;
                Ok(self.intern(TyKind::Unit))
            }
            ExprKind::Loop { body: loop_body } => {
                self.infer_expr(body, loop_body, None)?;
                Ok(self.intern(TyKind::Never))
            }
            _ => unreachable!("the dispatcher selected the control group"),
        }
    }

    fn infer_shallow_jumps(
        &mut self,
        body: &mut Body,
        _id: ExprId,
        kind: ExprKind,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        match kind {
            ExprKind::Return(value) => {
                let expected = self.returns.last().copied().flatten().ok_or_else(|| {
                    NotYetReason::Unsupported(
                        "`return` in a body without an expected result type is not on HIR".into(),
                    )
                })?;
                match value {
                    Some(value) => {
                        let ty = self.infer_expr(body, value, Some(expected))?;
                        self.unify(ty, expected, span.clone());
                    }
                    None => {
                        let unit = self.intern(TyKind::Unit);
                        self.unify(expected, unit, span.clone());
                    }
                }
                Ok(self.intern(TyKind::Never))
            }
            ExprKind::Break(value) => {
                if let Some(value) = value {
                    self.infer_expr(body, value, None)?;
                }
                Ok(self.intern(TyKind::Never))
            }
            ExprKind::Continue => Ok(self.intern(TyKind::Never)),
            ExprKind::Assign { place, op, value } => {
                let place_ty = self.place_ty(body, &place, span.clone())?;
                let value_ty =
                    self.infer_expr(body, value, matches!(op, AssignOp::Set).then_some(place_ty))?;
                match op {
                    AssignOp::Set => {
                        self.unify(place_ty, value_ty, span.clone());
                    }
                    AssignOp::Add | AssignOp::Subtract | AssignOp::Multiply | AssignOp::Divide => {
                        let binary = match op {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Subtract => BinOp::Sub,
                            AssignOp::Multiply => BinOp::Mul,
                            AssignOp::Divide => BinOp::Div,
                            AssignOp::Set => unreachable!("handled above"),
                        };
                        let mut left = self.resolved(place_ty);
                        let mut right = self.resolved(value_ty);
                        if matches!(self.kind_of(left), Some(TyKind::Infer(_)))
                            || matches!(self.kind_of(right), Some(TyKind::Infer(_)))
                        {
                            self.unify(place_ty, value_ty, span.clone());
                            left = self.resolved(place_ty);
                            right = self.resolved(value_ty);
                        }
                        match crate::ops::binary(&mut self.program.tys, binary, left, right) {
                            Some(result) => self.unify(place_ty, result, span.clone()),
                            None => {
                                self.conflict(
                                    format!(
                                        "the compound assignment operator is not defined for {} \
                                         and {}",
                                        crate::print::print_type(self.program, left),
                                        crate::print::print_type(self.program, right)
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                }
                Ok(self.intern(TyKind::Unit))
            }
            ExprKind::Error => Ok(self.error()),
            _ => unreachable!("the dispatcher selected the jump group"),
        }
    }

    /// Types an `if`. Each branch is checked against `expected`, so a lifted
    /// branch (decision D3) is accepted where the result is optional.
    pub(super) fn check_if(
        &mut self,
        body: &mut Body,
        cond: ExprId,
        then: ExprId,
        els: Option<ExprId>,
        expected: Option<Ty>,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let bool_ty = self.intern(TyKind::Bool);
        let cond_ty = self.infer_expr(body, cond, Some(bool_ty))?;
        self.unify(cond_ty, bool_ty, span.clone());
        let Some(els) = els else {
            self.infer_expr(body, then, None)?;
            let unit = self.intern(TyKind::Unit);
            return Ok(match expected {
                Some(expected) => self.coerce(unit, expected, span),
                None => unit,
            });
        };
        let then_ty = self.infer_expr(body, then, expected)?;
        let else_ty = self.infer_expr(body, els, expected)?;
        match expected {
            Some(expected) => Ok(expected),
            None => {
                self.unify(then_ty, else_ty, span);
                Ok(self.resolved(then_ty))
            }
        }
    }

    /// Types a `case`. Each arm body is checked against `expected` when the
    /// context fixes the result type.
    pub(super) fn check_case(
        &mut self,
        body: &mut Body,
        scrutinee: ExprId,
        arms: &[Arm],
        expected: Option<Ty>,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let scrutinee_ty = self.infer_expr(body, scrutinee, None)?;
        let mut result = None;
        for arm in arms {
            self.bind_pattern(body, arm.pat, scrutinee_ty, span.clone())?;
            if let Some(guard) = arm.guard {
                let bool_ty = self.intern(TyKind::Bool);
                let guard_ty = self.infer_expr(body, guard, Some(bool_ty))?;
                self.unify(guard_ty, bool_ty, span.clone());
            }
            let arm_ty = self.infer_expr(body, arm.body, expected)?;
            match result {
                Some(previous) => self.unify(previous, arm_ty, span.clone()),
                None => result = Some(arm_ty),
            }
        }
        let ty = result.ok_or_else(|| {
            NotYetReason::Unsupported("a `case` with no arms has no H2a type".into())
        })?;
        Ok(match expected {
            Some(expected) => expected,
            None => self.resolved(ty),
        })
    }
}
