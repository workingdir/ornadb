//! Control-flow expressions (design section 2.4).
//!
//! `if`, `case`, `for`, `while` and `loop` are separate variants with required
//! fields. The parser's single `Expr::Control` carries whichever fields its
//! keyword uses; lowering reads exactly those fields for that keyword and never
//! enumerates illegal combinations at run time.

use orna_syntax::{ControlKind, Expr as AstExpr, SyntaxSpan};

use crate::{Arm, ExprId, ExprKind};

use super::declare::Builder;
use super::exprs::{error_expr, lower_expr, push};
use super::{BodyState, pats};

/// Lowers one control expression.
pub(crate) fn lower_control(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    expr: &AstExpr,
) -> ExprId {
    let AstExpr::Control {
        kind,
        binding,
        condition,
        body,
        arms,
        alternate,
        span,
    } = expr
    else {
        return error_expr(builder, state, expr.span());
    };
    match kind {
        ControlKind::If => lower_if(
            builder,
            state,
            condition.as_deref(),
            body.as_deref(),
            alternate.as_deref(),
            span,
        ),
        ControlKind::While => {
            let cond = match condition.as_deref() {
                Some(condition) => lower_expr(builder, state, condition),
                None => error_expr(builder, state, span.clone()),
            };
            let body = match body.as_deref() {
                Some(body) => lower_expr(builder, state, body),
                None => error_expr(builder, state, span.clone()),
            };
            push(builder, state, ExprKind::While { cond, body }, span.clone())
        }
        ControlKind::Loop => {
            let body = match body.as_deref() {
                Some(body) => lower_expr(builder, state, body),
                None => error_expr(builder, state, span.clone()),
            };
            push(builder, state, ExprKind::Loop { body }, span.clone())
        }
        ControlKind::For => lower_for(
            builder,
            state,
            binding.as_ref(),
            condition.as_deref(),
            body.as_deref(),
            span,
        ),
        ControlKind::Case => lower_case(builder, state, condition.as_deref(), arms, span),
    }
}

fn lower_if(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    condition: Option<&AstExpr>,
    then: Option<&AstExpr>,
    els: Option<&AstExpr>,
    span: &SyntaxSpan,
) -> ExprId {
    let cond = match condition {
        Some(condition) => lower_expr(builder, state, condition),
        None => error_expr(builder, state, span.clone()),
    };
    let then = match then {
        Some(then) => lower_expr(builder, state, then),
        None => error_expr(builder, state, span.clone()),
    };
    let els = els.map(|els| lower_expr(builder, state, els));
    push(
        builder,
        state,
        ExprKind::If { cond, then, els },
        span.clone(),
    )
}

fn lower_for(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    binding: Option<&orna_syntax::Pattern>,
    iter: Option<&AstExpr>,
    body: Option<&AstExpr>,
    span: &SyntaxSpan,
) -> ExprId {
    let iter = match iter {
        Some(iter) => lower_expr(builder, state, iter),
        None => error_expr(builder, state, span.clone()),
    };
    state.push_scope();
    let pat = match binding {
        Some(pattern) => pats::lower_pattern(builder, state, pattern),
        None => state.push_pat(crate::Pat::Wild),
    };
    let body = match body {
        Some(body) => lower_expr(builder, state, body),
        None => error_expr(builder, state, span.clone()),
    };
    state.pop_scope();
    push(
        builder,
        state,
        ExprKind::For { pat, iter, body },
        span.clone(),
    )
}

fn lower_case(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    scrutinee: Option<&AstExpr>,
    arms: &[orna_syntax::CaseArm],
    span: &SyntaxSpan,
) -> ExprId {
    let scrutinee = match scrutinee {
        Some(scrutinee) => lower_expr(builder, state, scrutinee),
        None => error_expr(builder, state, span.clone()),
    };
    let mut lowered = Vec::with_capacity(arms.len());
    for arm in arms {
        state.push_scope();
        let pat = pats::lower_pattern(builder, state, &arm.pattern);
        let guard = arm
            .guard
            .as_ref()
            .map(|guard| lower_expr(builder, state, guard));
        let body = lower_expr(builder, state, &arm.body);
        state.pop_scope();
        lowered.push(Arm { pat, guard, body });
    }
    push(
        builder,
        state,
        ExprKind::Case {
            scrutinee,
            arms: lowered.into_boxed_slice(),
        },
        span.clone(),
    )
}
