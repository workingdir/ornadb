//! Blocks and assignment targets, split out of `exprs.rs` to keep that file within its budget.
//!
//! A block lowers its statements in order and its tail as its value. An assignment target
//! lowers to a [`Place`], the slot or field an assignment writes.

use orna_syntax::{AssignmentOperator, AssignmentTarget, Expr as AstExpr, Statement, SyntaxSpan};

use crate::{AssignOp, ExprId, ExprKind, FieldRef, Name, NameTarget, Place, Stmt};

use super::declare::Builder;
use super::exprs::{ERROR_SLOT, lower_expr, push};
use super::{BodyState, pats};

/// Lowers a block: statements in order and an optional tail.
pub(super) fn lower_block(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    statements: &[Statement],
    tail: Option<&AstExpr>,
    span: &SyntaxSpan,
) -> ExprId {
    state.push_scope(span.start);
    let mut lowered = Vec::with_capacity(statements.len());
    for statement in statements {
        match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                ..
            } => {
                // The annotation is the initializer's expected type and the
                // binding slots' written type; a pattern without one leaves
                // the slots to the type pass (stage H2).
                let expected = annotation.as_ref().map(|annotation| {
                    super::types::lower_type(builder, state.module, &[], annotation)
                });
                let init = lower_expr(builder, state, value);
                let pat = pats::lower_pattern_with(builder, state, pattern, expected);
                lowered.push(Stmt::Let { pat, init });
            }
            Statement::Assert { value, .. } => {
                lowered.push(Stmt::Assert(lower_expr(builder, state, value)));
            }
            Statement::Expression { value, .. } | Statement::Control { value, .. } => {
                lowered.push(Stmt::Expr(lower_expr(builder, state, value)));
            }
            Statement::Return {
                value,
                span: stmt_span,
            } => {
                let value = value
                    .as_ref()
                    .map(|value| lower_expr(builder, state, value));
                let expr = push(builder, state, ExprKind::Return(value), stmt_span.clone());
                lowered.push(Stmt::Expr(expr));
            }
            Statement::Break {
                value,
                span: stmt_span,
            } => {
                let value = value
                    .as_ref()
                    .map(|value| lower_expr(builder, state, value));
                let expr = push(builder, state, ExprKind::Break(value), stmt_span.clone());
                lowered.push(Stmt::Expr(expr));
            }
            Statement::Continue { span: stmt_span } => {
                let expr = push(builder, state, ExprKind::Continue, stmt_span.clone());
                lowered.push(Stmt::Expr(expr));
            }
            Statement::Assignment {
                target,
                operator,
                value,
                span: stmt_span,
            } => {
                let place = lower_place(builder, state, target);
                let value = lower_expr(builder, state, value);
                let op = match operator {
                    AssignmentOperator::Set => AssignOp::Set,
                    AssignmentOperator::Add => AssignOp::Add,
                    AssignmentOperator::Subtract => AssignOp::Subtract,
                    AssignmentOperator::Multiply => AssignOp::Multiply,
                    AssignmentOperator::Divide => AssignOp::Divide,
                };
                let expr = push(
                    builder,
                    state,
                    ExprKind::Assign { place, op, value },
                    stmt_span.clone(),
                );
                lowered.push(Stmt::Expr(expr));
            }
        }
    }
    let tail = tail.map(|tail| lower_expr(builder, state, tail));
    state.pop_scope(span.end);
    push(
        builder,
        state,
        ExprKind::Block {
            stmts: lowered.into_boxed_slice(),
            tail,
        },
        span.clone(),
    )
}

/// Lowers an assignment target to a place.
fn lower_place(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    target: &AssignmentTarget,
) -> Place {
    match target {
        AssignmentTarget::Name { name, span } => {
            let name = Name::new(name);
            match state.local(&name) {
                Some(local) => {
                    super::record_body_use(
                        &mut builder.program,
                        state.module,
                        state.id,
                        span.clone(),
                        name,
                        crate::UseContext::Value,
                        NameTarget::Local(local),
                    );
                    super::mark_written(&mut builder.program, state.module, span);
                    if state.is_blocked_capture(local) {
                        let captured = state.locals[local].name.clone();
                        builder.diagnose(
                            state.module,
                            crate::diag::UNSUPPORTED,
                            format!("`{captured}` is captured by a concurrent body and cannot be assigned"),
                            span.clone(),
                        );
                    }
                    Place::Local(local)
                }
                None => {
                    super::record_body_use(
                        &mut builder.program,
                        state.module,
                        state.id,
                        span.clone(),
                        name.clone(),
                        crate::UseContext::Value,
                        NameTarget::Unresolved,
                    );
                    builder.diagnose(
                        state.module,
                        crate::diag::UNRESOLVED,
                        format!("`{name}` is not an assignable local"),
                        span.clone(),
                    );
                    let error_ty = builder.program.tys.error();
                    Place::Local(state.add_local(Name::new(ERROR_SLOT), error_ty, span.clone()))
                }
            }
        }
        AssignmentTarget::Field {
            base, name, span, ..
        } => {
            let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
            super::record_body_use(
                &mut builder.program,
                state.module,
                state.id,
                name_span.clone(),
                Name::new(name),
                crate::UseContext::Field,
                NameTarget::Unresolved,
            );
            super::mark_written(&mut builder.program, state.module, &name_span);
            Place::Field {
                base: Box::new(lower_place(builder, state, base)),
                field: FieldRef::Name(Name::new(name)),
            }
        }
        AssignmentTarget::Index {
            base, index, span, ..
        } => {
            let base = lower_place(builder, state, base);
            let index = lower_expr(builder, state, index);
            let _ = span;
            Place::Index {
                base: Box::new(base),
                index,
            }
        }
    }
}
