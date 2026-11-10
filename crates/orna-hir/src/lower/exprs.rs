//! Expressions to HIR (design sections 2.4, 2.6 and 2.7).
//!
//! Every expression becomes one [`crate::Expr`] whose names are slots or
//! definitions, whose operators are enums, and whose calls are one classified
//! [`crate::Callee`]. Expression types are [`crate::TyKind::Error`]: stage H2
//! infers them, and claiming a type here would be a guess.
//!
//! The parser's operator strings become enums here, not in the parser.
//! `a | f(x)` is a call with `a` piped into parameter 0, and `a |? handler` is
//! [`crate::ExprKind::Recover`]: the handler receives the failure through one
//! synthetic slot, which is what the evaluator's `|?` does today.

use orna_syntax::{
    AssignmentOperator, AssignmentTarget, Expr as AstExpr, Statement, SyntaxSpan, TypeExpr,
};

use crate::{
    AssignOp, BinOp, Call, Capture, Expr, ExprId, ExprKind, FieldRef, InterpPart, LogicOp, Name,
    NameTarget, Place, Stmt, UnOp,
};

use super::calls::{attach_type_arguments, classify_callee};
use super::control::lower_control;
use super::declare::Builder;
use super::free_names::free_names;
use super::literals::lower_literal;
use super::{BodyState, pats};

/// The synthetic slot name a `|?` handler binds its failure to.
const ERROR_SLOT: &str = "_error";

/// Lowers one expression.
pub(crate) fn lower_expr(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    expr: &AstExpr,
) -> ExprId {
    match expr {
        AstExpr::Name { text, span } => lower_name(builder, state, text, span),
        AstExpr::Literal { text, kind, span } => match lower_literal(text, *kind) {
            Some(literal) => push(builder, state, ExprKind::Lit(literal), span.clone()),
            None => {
                builder.diagnose(
                    state.module,
                    crate::diag::UNSUPPORTED,
                    "literal has no canonical payload",
                    span.clone(),
                );
                error_expr(builder, state, span.clone())
            }
        },
        AstExpr::InterpolatedString { segments, span } => {
            let mut parts = Vec::with_capacity(segments.len());
            for segment in segments {
                match segment {
                    orna_syntax::StringSegment::Text { text, .. } => {
                        match orna_syntax::unescape_string_body(text) {
                            Ok(text) => parts.push(InterpPart::Text(text.into_boxed_str())),
                            Err(_) => {
                                builder.diagnose(
                                    state.module,
                                    crate::diag::UNSUPPORTED,
                                    "invalid string escape",
                                    span.clone(),
                                );
                                parts.push(InterpPart::Text(String::new().into_boxed_str()));
                            }
                        }
                    }
                    orna_syntax::StringSegment::Expression { value, .. } => {
                        parts.push(InterpPart::Expr(lower_expr(builder, state, value)));
                    }
                }
            }
            push(
                builder,
                state,
                ExprKind::Interp(parts.into_boxed_slice()),
                span.clone(),
            )
        }
        AstExpr::ReplBinding { span, .. } => {
            builder.diagnose(
                state.module,
                crate::diag::UNSUPPORTED,
                "REPL bindings are not lowered yet",
                span.clone(),
            );
            error_expr(builder, state, span.clone())
        }
        AstExpr::Unary { op, rhs, span } => {
            let operand = lower_expr(builder, state, rhs);
            let op = match op.as_str() {
                "-" => UnOp::Neg,
                "!" => UnOp::Not,
                _ => {
                    builder.diagnose(
                        state.module,
                        crate::diag::UNSUPPORTED,
                        "unknown unary operator",
                        span.clone(),
                    );
                    return error_expr(builder, state, span.clone());
                }
            };
            push(
                builder,
                state,
                ExprKind::Unary { op, operand },
                span.clone(),
            )
        }
        AstExpr::Binary { lhs, op, rhs, span } => lower_binary(builder, state, lhs, op, rhs, span),
        AstExpr::Range {
            lower,
            operator,
            upper,
            span,
        } => {
            let lo = lower
                .as_ref()
                .map(|lower| lower_expr(builder, state, lower));
            let hi = upper
                .as_ref()
                .map(|upper| lower_expr(builder, state, upper));
            push(
                builder,
                state,
                ExprKind::Range {
                    lo,
                    hi,
                    inclusive: operator == "..=",
                },
                span.clone(),
            )
        }
        AstExpr::Call {
            callee,
            arguments,
            span,
        } => lower_call(builder, state, callee, &[], arguments, None, span),
        AstExpr::GenericCall {
            callee,
            type_arguments,
            arguments,
            span,
        } => lower_call(
            builder,
            state,
            callee,
            type_arguments,
            arguments,
            None,
            span,
        ),
        AstExpr::Index { base, index, span } => {
            let base = lower_expr(builder, state, base);
            let index = lower_expr(builder, state, index);
            push(
                builder,
                state,
                ExprKind::Index { base, index },
                span.clone(),
            )
        }
        AstExpr::Field { base, name, span } => {
            let base = lower_expr(builder, state, base);
            push(
                builder,
                state,
                ExprKind::Field {
                    base,
                    field: FieldRef::Name(Name::new(name)),
                },
                span.clone(),
            )
        }
        AstExpr::Group { inner, .. } => lower_expr(builder, state, inner),
        AstExpr::Tuple { elements, span } => {
            let elements = lower_all(builder, state, elements);
            push(builder, state, ExprKind::Tuple(elements), span.clone())
        }
        AstExpr::Record { fields, span } => {
            let mut lowered = fields
                .iter()
                .map(|field| {
                    (
                        Name::new(&field.name),
                        lower_expr(builder, state, &field.value),
                    )
                })
                .collect::<Vec<_>>();
            lowered.sort_by(|left, right| left.0.cmp(&right.0));
            push(
                builder,
                state,
                ExprKind::Record(lowered.into_boxed_slice()),
                span.clone(),
            )
        }
        AstExpr::List { elements, span } => {
            let elements = lower_all(builder, state, elements);
            push(builder, state, ExprKind::List(elements), span.clone())
        }
        AstExpr::Nominal { path, fields, span } => {
            lower_construct(builder, state, path, fields, span)
        }
        AstExpr::Lambda {
            parameters,
            body,
            span,
        } => lower_lambda(builder, state, parameters, body, span),
        AstExpr::Block {
            statements,
            tail,
            span,
        } => lower_block(builder, state, statements, tail.as_deref(), span),
        AstExpr::Control { .. } => lower_control(builder, state, expr),
    }
}

/// Adds one expression with the untyped stage's type.
pub(crate) fn push(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    kind: ExprKind,
    span: SyntaxSpan,
) -> ExprId {
    let ty = builder.program.tys.error();
    state.push_expr(Expr { kind, ty, span })
}

/// Adds one diagnostic expression.
pub(crate) fn error_expr(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    span: SyntaxSpan,
) -> ExprId {
    push(builder, state, ExprKind::Error, span)
}

fn lower_all(builder: &mut Builder<'_>, state: &mut BodyState, exprs: &[AstExpr]) -> Box<[ExprId]> {
    exprs
        .iter()
        .map(|expr| lower_expr(builder, state, expr))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Lowers a bare name: a local slot, a definition, or a diagnostic.
pub(crate) fn lower_name(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    text: &str,
    span: &SyntaxSpan,
) -> ExprId {
    let name = Name::new(text);
    let span = span.clone();
    if let Some(local) = state.local(&name) {
        super::record_use(
            &mut builder.program,
            state.module,
            span.clone(),
            name,
            NameTarget::Local(local),
        );
        return push(builder, state, ExprKind::Local(local), span.clone());
    }
    let module = state.module;
    let target = builder.resolve_bare(module, &name);
    super::record_use(
        &mut builder.program,
        module,
        span.clone(),
        name.clone(),
        target,
    );
    match target {
        NameTarget::Def(def) => push(builder, state, ExprKind::Def(def), span),
        NameTarget::Intrinsic | NameTarget::Local(_) => push(builder, state, ExprKind::Error, span),
        NameTarget::Module(_) => {
            builder.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                format!("`{name}` is a module, not a value"),
                span.clone(),
            );
            error_expr(builder, state, span)
        }
        NameTarget::Ambiguous => {
            builder.diagnose(
                module,
                crate::diag::AMBIGUOUS,
                format!("`{name}` is ambiguous"),
                span.clone(),
            );
            error_expr(builder, state, span)
        }
        NameTarget::Unresolved => {
            builder.diagnose(
                module,
                crate::diag::UNRESOLVED,
                format!("`{name}` is not in scope"),
                span.clone(),
            );
            error_expr(builder, state, span)
        }
    }
}

/// Lowers an arithmetic, comparison, logical or pipeline operator.
fn lower_binary(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    lhs: &AstExpr,
    op: &str,
    rhs: &AstExpr,
    span: &SyntaxSpan,
) -> ExprId {
    if op == "|" {
        let input = lower_expr(builder, state, lhs);
        return lower_stage(builder, state, rhs, Some(input), span);
    }
    if op == "|?" {
        return lower_recover(builder, state, lhs, rhs, span);
    }
    if let Some(logic) = match op {
        "&&" => Some(LogicOp::And),
        "||" => Some(LogicOp::Or),
        "??" => Some(LogicOp::Coalesce),
        _ => None,
    } {
        let lhs = lower_expr(builder, state, lhs);
        let rhs = lower_expr(builder, state, rhs);
        return push(
            builder,
            state,
            ExprKind::Logic {
                op: logic,
                lhs,
                rhs,
            },
            span.clone(),
        );
    }
    let Some(op) = bin_op(op) else {
        builder.diagnose(
            state.module,
            crate::diag::UNSUPPORTED,
            "unknown binary operator",
            span.clone(),
        );
        return error_expr(builder, state, span.clone());
    };
    let lhs = lower_expr(builder, state, lhs);
    let rhs = lower_expr(builder, state, rhs);
    push(
        builder,
        state,
        ExprKind::Binary { op, lhs, rhs },
        span.clone(),
    )
}

/// The binary operator an operator string denotes.
fn bin_op(op: &str) -> Option<BinOp> {
    Some(match op {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Rem,
        "^" => BinOp::Pow,
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        _ => return None,
    })
}

/// Lowers `lhs |? handler` into recovery with one synthetic error slot.
fn lower_recover(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    lhs: &AstExpr,
    rhs: &AstExpr,
    span: &SyntaxSpan,
) -> ExprId {
    let body = lower_expr(builder, state, lhs);
    state.push_scope();
    let error_ty = builder.program.tys.error();
    let handler = state.add_local(Name::new(ERROR_SLOT), error_ty, span.clone());
    let error_value = push(builder, state, ExprKind::Local(handler), span.clone());
    let arm = lower_stage(builder, state, rhs, Some(error_value), span);
    state.pop_scope();
    push(
        builder,
        state,
        ExprKind::Recover { body, handler, arm },
        span.clone(),
    )
}

/// Lowers a pipeline stage: the stage's callee applied to `input`.
fn lower_stage(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    stage: &AstExpr,
    input: Option<ExprId>,
    span: &SyntaxSpan,
) -> ExprId {
    match stage {
        AstExpr::Call {
            callee,
            arguments,
            span: call_span,
        } => lower_call(builder, state, callee, &[], arguments, input, call_span),
        AstExpr::GenericCall {
            callee,
            type_arguments,
            arguments,
            span: call_span,
        } => lower_call(
            builder,
            state,
            callee,
            type_arguments,
            arguments,
            input,
            call_span,
        ),
        AstExpr::Group { inner, .. } => lower_stage(builder, state, inner, input, span),
        other => lower_call(builder, state, other, &[], &[], input, span),
    }
}

/// The segments of a dotted path, when the expression is one.
pub(crate) fn flatten_path<'a>(
    expr: &'a AstExpr,
    out: &mut Vec<(&'a str, &'a SyntaxSpan)>,
) -> bool {
    match expr {
        AstExpr::Name { text, span } => {
            out.push((text, span));
            true
        }
        AstExpr::Field { base, name, span } => {
            if !flatten_path(base, out) {
                return false;
            }
            out.push((name, span));
            true
        }
        AstExpr::Group { inner, .. } => flatten_path(inner, out),
        _ => false,
    }
}

/// Lowers one call, including its classified callee and bound arguments.
fn lower_call(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    callee: &AstExpr,
    type_arguments: &[TypeExpr],
    arguments: &[orna_syntax::Argument],
    input: Option<ExprId>,
    span: &SyntaxSpan,
) -> ExprId {
    let module = state.module;
    let target = classify_callee(builder, state, callee, span);
    let (callee, params) = match target {
        Some(classified) => classified,
        None => return error_expr(builder, state, span.clone()),
    };
    let callee = attach_type_arguments(builder, state, callee, type_arguments);
    let lowered = arguments
        .iter()
        .map(|argument| {
            (
                argument.name.as_ref().map(|name| Name::new(name)),
                lower_expr(builder, state, &argument.value),
                argument.span.clone(),
            )
        })
        .collect::<Vec<_>>();
    let placeholder = error_expr(builder, state, span.clone());
    let (args, piped) = builder.bind_arguments(
        module,
        params.as_deref(),
        &lowered,
        input,
        placeholder,
        span,
    );
    push(
        builder,
        state,
        ExprKind::Call(Box::new(Call {
            callee,
            args: args.into_boxed_slice(),
            piped,
        })),
        span.clone(),
    )
}

/// Lowers a nominal or variant construction.
fn lower_construct(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    path: &[orna_syntax::NameSegment],
    fields: &[orna_syntax::RecordField],
    span: &SyntaxSpan,
) -> ExprId {
    let module = state.module;
    let segments = path
        .iter()
        .map(|segment| Name::new(&segment.text))
        .collect::<Vec<_>>();
    let last = segments.last().cloned().unwrap_or_else(|| Name::new(""));
    let def = match builder.resolve_path(module, &segments) {
        crate::resolve::PathKind::Def(def) => {
            super::record_use(
                &mut builder.program,
                module,
                span.clone(),
                last,
                NameTarget::Def(def),
            );
            Some(def)
        }
        _ => {
            super::record_use(
                &mut builder.program,
                module,
                span.clone(),
                last,
                NameTarget::Unresolved,
            );
            builder.diagnose(
                module,
                crate::diag::UNRESOLVED,
                "constructor does not resolve",
                span.clone(),
            );
            None
        }
    };
    let mut lowered = Vec::with_capacity(fields.len());
    for field in fields {
        let value = lower_expr(builder, state, &field.value);
        if let Some(field_def) = def.and_then(|def| {
            pats::payload(builder, def)
                .into_iter()
                .find(|candidate| builder.def_name(*candidate).as_str() == field.name)
        }) {
            lowered.push((field_def, value));
        }
    }
    match def {
        Some(def) => push(
            builder,
            state,
            ExprKind::Construct {
                def,
                fields: lowered.into_boxed_slice(),
            },
            span.clone(),
        ),
        None => error_expr(builder, state, span.clone()),
    }
}

/// Lowers a lambda: its own body, its parameter slots and its captures.
fn lower_lambda(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    parameters: &[orna_syntax::LambdaParameter],
    body: &AstExpr,
    span: &SyntaxSpan,
) -> ExprId {
    let owner = state.owner;
    let module = state.module;
    let id = builder.allocate_body(owner);
    let mut lambda = BodyState::new(id, owner, module);
    lambda.push_scope();
    for (index, parameter) in parameters.iter().enumerate() {
        let ty = match &parameter.annotation {
            Some(annotation) => super::types::lower_type(builder, module, &[], annotation),
            None => builder.program.tys.error(),
        };
        let name = match &parameter.pattern {
            orna_syntax::Pattern::Name(name, _) => Name::new(name),
            _ => Name::new(&format!("_arg{index}")),
        };
        let local = lambda.add_local(name, ty, parameter.span.clone());
        lambda.params.push(local);
    }
    let free = free_names(body);
    for (name, free_span) in free {
        let name = Name::new(&name);
        if let Some(outer) = state.local(&name) {
            let outer_local = state.locals[outer].clone();
            let inner = lambda.add_local(outer_local.name.clone(), outer_local.ty, free_span);
            lambda.captures.push(Capture { outer, inner });
        }
    }
    let root = lower_expr(builder, &mut lambda, body);
    lambda.root = Some(root);
    lambda.pop_scope();
    let error_ty = builder.program.tys.error();
    builder.complete_body(lambda, error_ty);
    push(
        builder,
        state,
        ExprKind::Lambda {
            body: id,
            captured: Box::new([]),
        },
        span.clone(),
    )
}

/// Lowers a block: statements in order and an optional tail.
fn lower_block(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    statements: &[Statement],
    tail: Option<&AstExpr>,
    span: &SyntaxSpan,
) -> ExprId {
    state.push_scope();
    let mut lowered = Vec::with_capacity(statements.len());
    for statement in statements {
        match statement {
            Statement::Let {
                pattern,
                annotation: _,
                value,
                ..
            } => {
                let init = lower_expr(builder, state, value);
                let pat = pats::lower_pattern(builder, state, pattern);
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
    state.pop_scope();
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
                Some(local) => Place::Local(local),
                None => {
                    super::record_use(
                        &mut builder.program,
                        state.module,
                        span.clone(),
                        name.clone(),
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
        AssignmentTarget::Field { base, name, .. } => Place::Field {
            base: Box::new(lower_place(builder, state, base)),
            field: FieldRef::Name(Name::new(name)),
        },
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
