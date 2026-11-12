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

use orna_syntax::{Expr as AstExpr, SyntaxSpan, TypeExpr};

use crate::{
    BinOp, Call, Capture, DefId, DefKind, Expr, ExprId, ExprKind, FieldRef, Idx, InterpPart,
    LogicOp, Name, NameTarget, Stmt, Ty, TyKind, UnOp,
};

use super::blocks::lower_block;
use super::calls::{attach_type_arguments, classify_callee};
use super::classify::ParamInfo;
use super::control::lower_control;
use super::declare::Builder;
use super::free_names::free_names;
use super::literals::lower_literal;
use super::{BodyState, pats};

/// The synthetic slot name a `|?` handler binds its failure to.
pub(super) const ERROR_SLOT: &str = "_error";

/// Lowers one expression.
pub(crate) fn lower_expr(builder: &mut Builder, state: &mut BodyState, expr: &AstExpr) -> ExprId {
    if let Some(id) = lower_atom(builder, state, expr) {
        return id;
    }
    if let Some(id) = lower_operator(builder, state, expr) {
        return id;
    }
    if let Some(id) = lower_collection(builder, state, expr) {
        return id;
    }
    if let Some(id) = lower_invocation(builder, state, expr) {
        return id;
    }
    match expr {
        AstExpr::Block {
            statements,
            tail,
            span,
        } => lower_block(builder, state, statements, tail.as_deref(), span),
        AstExpr::Control { .. } => lower_control(builder, state, expr),
        _ => unreachable!("every expression shape is handled by one of the four groups"),
    }
}

/// Lowers names, literals and interpolated strings.
fn lower_atom(builder: &mut Builder, state: &mut BodyState, expr: &AstExpr) -> Option<ExprId> {
    Some(match expr {
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
        _ => return None,
    })
}

/// Lowers unary, binary, range, index, field and grouped expressions.
fn lower_operator(builder: &mut Builder, state: &mut BodyState, expr: &AstExpr) -> Option<ExprId> {
    Some(match expr {
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
                    return Some(error_expr(builder, state, span.clone()));
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
        AstExpr::Field { base, name, span } => lower_field(builder, state, base, name, span),
        AstExpr::Group { inner, .. } => {
            // The old evaluator steps once for a group node of its own. Lowering keeps no node for it, so the
            // step folds into the child. Parser nesting is bounded far below `u16::MAX` groups.
            let child = lower_expr(builder, state, inner);
            state.exprs[child].weight = state.exprs[child].weight.saturating_add(1);
            child
        }
        _ => return None,
    })
}

/// Lowers `base.name`: a numeric postfix literal, an enum variant, a field or an unresolved member.
fn lower_field(
    builder: &mut Builder,
    state: &mut BodyState,
    base: &AstExpr,
    name: &str,
    span: &SyntaxSpan,
) -> ExprId {
    if let Some(call) = super::units::lower_suffix_unit(builder, state, base, name, span) {
        return call;
    }
    if let AstExpr::Literal { text, kind, .. } = base
        && let Some(literal) = super::literals::lower_numeric_postfix(text, *kind, name)
    {
        // The old evaluator charges the base literal and the field access that names the unit: two steps.
        let literal = push(builder, state, ExprKind::Lit(literal), span.clone());
        state.exprs[literal].weight = 2;
        return literal;
    }
    let member_of_path = qualified_member(builder, state, base, name);
    let base = lower_expr(builder, state, base);
    if let Some(member) = member_of_path
        .filter(|def| builder.is_variant(*def) || builder.is_static(*def) || builder.is_unit(*def))
    {
        // `Enum.variant`, `Type.static` and `Family.unit` name the member itself; the owner path is only recorded.
        let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
        super::record_body_use(
            &mut builder.program,
            state.module,
            state.id,
            name_span,
            Name::new(name),
            crate::UseContext::Value,
            NameTarget::Def(member),
        );
        return push(builder, state, ExprKind::Def(member), span.clone());
    }
    let field_name = Name::new(name);
    let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
    let base_ty = state.exprs[base].ty;
    match field_of_type(builder, base_ty, &field_name) {
        Some(field) => {
            let ty = field_type(builder, field);
            super::record_body_use(
                &mut builder.program,
                state.module,
                state.id,
                name_span,
                field_name,
                crate::UseContext::Field,
                NameTarget::Def(field),
            );
            push_typed(
                builder,
                state,
                ExprKind::Field {
                    base,
                    field: FieldRef::Def(field),
                },
                span.clone(),
                ty,
            )
        }
        None => {
            let member = member_of_path;
            super::record_body_use(
                &mut builder.program,
                state.module,
                state.id,
                name_span,
                field_name.clone(),
                crate::UseContext::Field,
                member.map_or(NameTarget::Unresolved, NameTarget::Def),
            );
            push(
                builder,
                state,
                ExprKind::Field {
                    base,
                    field: FieldRef::Name(field_name),
                },
                span.clone(),
            )
        }
    }
}

/// Lowers tuple, list and record expressions.
fn lower_collection(
    builder: &mut Builder,
    state: &mut BodyState,
    expr: &AstExpr,
) -> Option<ExprId> {
    Some(match expr {
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
        _ => return None,
    })
}

/// Lowers calls, constructions and lambdas.
fn lower_invocation(
    builder: &mut Builder,
    state: &mut BodyState,
    expr: &AstExpr,
) -> Option<ExprId> {
    Some(match expr {
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
        AstExpr::Nominal { path, fields, span } => {
            lower_construct(builder, state, path, fields, span)
        }
        AstExpr::Lambda {
            parameters,
            body,
            span,
        } => lower_lambda(builder, state, parameters, body, span),
        _ => return None,
    })
}

/// Adds one expression with the untyped stage's type.
pub(crate) fn push(
    builder: &mut Builder,
    state: &mut BodyState,
    kind: ExprKind,
    span: SyntaxSpan,
) -> ExprId {
    let ty = builder.program.tys.error();
    state.push_expr(Expr {
        kind,
        ty,
        span,
        weight: 1,
    })
}

/// Adds one expression with the type its declaration already states.
pub(crate) fn push_typed(
    builder: &mut Builder,
    state: &mut BodyState,
    kind: ExprKind,
    span: SyntaxSpan,
    ty: Ty,
) -> ExprId {
    let _ = builder;
    state.push_expr(Expr {
        kind,
        ty,
        span,
        weight: 1,
    })
}

/// The field definition `name` denotes on a value of `ty`, when the written
/// type states it. This reads declared types only; inference is stage H2.
fn field_of_type(builder: &Builder, ty: Ty, name: &Name) -> Option<DefId> {
    match builder.program.tys.kind(ty)? {
        TyKind::Adt(def, _) => payload_fields(builder, *def)
            .into_iter()
            .find(|field| builder.def_name(*field) == name),
        TyKind::Option(inner)
        | TyKind::List(inner)
        | TyKind::Range(inner)
        | TyKind::Relation(inner)
        | TyKind::Stream(inner) => field_of_type(builder, *inner, name),
        _ => None,
    }
}

/// The definition a `base.name` access that has no field type names: a member of the type or enum the
/// dotted `base` path names (`Status.draft`, `Length.kilometre`).
fn qualified_member(
    builder: &Builder,
    state: &BodyState,
    base: &AstExpr,
    name: &str,
) -> Option<DefId> {
    let mut segments = Vec::new();
    if !flatten_path(base, &mut segments) {
        return None;
    }
    let root = Name::new(segments.first()?.0);
    if state.local(&root).is_some() {
        return None;
    }
    let names = segments
        .iter()
        .map(|(text, _)| Name::new(text))
        .collect::<Vec<_>>();
    match builder.resolve_path(state.module, &names) {
        crate::resolve::PathKind::Def(owner) => builder.member_def(owner, &Name::new(name)),
        _ => None,
    }
}

/// The declared type of a field definition.
fn field_type(builder: &mut Builder, field: DefId) -> Ty {
    match &builder.pending[field.index()].kind {
        super::declare::PendingKind::Built(DefKind::Field(definition)) => definition.ty,
        _ => builder.program.tys.error(),
    }
}

/// The field definitions a table, nominal type or enum variant owns.
fn payload_fields(builder: &Builder, owner: DefId) -> Vec<DefId> {
    payload(builder, owner)
}

fn payload(builder: &Builder, owner: DefId) -> Vec<DefId> {
    use super::declare::PendingKind;
    match &builder.pending[owner.index()].kind {
        PendingKind::Built(DefKind::Variant(variant)) => variant.fields.clone(),
        PendingKind::Built(DefKind::Type(definition)) => definition.members.clone(),
        PendingKind::Built(DefKind::Table(table)) => table.fields.clone(),
        _ => Vec::new(),
    }
}

/// Adds one diagnostic expression.
pub(crate) fn error_expr(builder: &mut Builder, state: &mut BodyState, span: SyntaxSpan) -> ExprId {
    push(builder, state, ExprKind::Error, span)
}

fn lower_all(builder: &mut Builder, state: &mut BodyState, exprs: &[AstExpr]) -> Box<[ExprId]> {
    exprs
        .iter()
        .map(|expr| lower_expr(builder, state, expr))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Lowers a bare name: a local slot, a definition, or a diagnostic.
pub(crate) fn lower_name(
    builder: &mut Builder,
    state: &mut BodyState,
    text: &str,
    span: &SyntaxSpan,
) -> ExprId {
    let name = Name::new(text);
    let span = span.clone();
    if let Some(local) = state.local(&name) {
        super::record_body_use(
            &mut builder.program,
            state.module,
            state.id,
            span.clone(),
            name,
            crate::UseContext::Value,
            NameTarget::Local(local),
        );
        let ty = state.locals[local].ty;
        return push_typed(builder, state, ExprKind::Local(local), span.clone(), ty);
    }
    if let Some(sibling) = super::row::lower_sibling_field(builder, state, &name, &span) {
        return sibling;
    }
    let module = state.module;
    let target = builder.resolve_bare(module, &name);
    super::record_value_use(
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
        // Typeck resolves a bare word to a payload-free variant of the expected
        // enum type (decision P3b) and reports it as unresolved otherwise.
        NameTarget::Unresolved => push(builder, state, ExprKind::BareName(name), span),
    }
}

/// Lowers an arithmetic, comparison, logical or pipeline operator.
fn lower_binary(
    builder: &mut Builder,
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
        "in" => BinOp::In,
        _ => return None,
    })
}

/// Lowers `lhs |? handler` into recovery with one synthetic error slot.
fn lower_recover(
    builder: &mut Builder,
    state: &mut BodyState,
    lhs: &AstExpr,
    rhs: &AstExpr,
    span: &SyntaxSpan,
) -> ExprId {
    let body = lower_expr(builder, state, lhs);
    state.push_scope(span.start);
    let error_ty = builder.program.tys.error();
    let handler = state.add_local(Name::new(ERROR_SLOT), error_ty, span.clone());
    let error_value = push(builder, state, ExprKind::Local(handler), span.clone());
    state.exprs[error_value].weight = 0;
    let arm = lower_stage(builder, state, rhs, Some(error_value), span);
    // The old evaluator applies the handler to the failure without a call step or a step for the piped
    // error value; only the handler's own callee and body are charged.
    state.exprs[arm].weight = 0;
    state.pop_scope(span.end);
    push(
        builder,
        state,
        ExprKind::Recover { body, handler, arm },
        span.clone(),
    )
}

/// Lowers a pipeline stage: the stage's callee applied to `input`.
fn lower_stage(
    builder: &mut Builder,
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
        // A parenthesised stage stays the callee exactly as the evaluator sees
        // it, so the recorded callee span keeps its parentheses; the group
        // lowers to its inner expression as a value.
        other => lower_call(builder, state, other, &[], &[], input, span),
    }
}

/// The segments of a dotted path, when the expression is one.
pub(crate) fn flatten_path<'a>(expr: &'a AstExpr, out: &mut Vec<(&'a str, SyntaxSpan)>) -> bool {
    match expr {
        AstExpr::Name { text, span } => {
            out.push((text, span.clone()));
            true
        }
        AstExpr::Field { base, name, span } => {
            if !flatten_path(base, out) {
                return false;
            }
            // A field expression's span covers the receiver too; the name is
            // its last `name.len()` bytes.
            let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
            out.push((name, name_span));
            true
        }
        AstExpr::Group { inner, .. } => flatten_path(inner, out),
        _ => false,
    }
}

/// Lowers one argument of a concurrent call. A lambda there, or a lambda in a
/// list literal, is a concurrent body; any other argument lowers as usual, so
/// a nested lambda never inherits the flag.
fn lower_concurrent_argument(
    builder: &mut Builder,
    state: &mut BodyState,
    value: &AstExpr,
) -> ExprId {
    match value {
        AstExpr::Lambda { .. } => {
            state.lowering_concurrent_argument = true;
            let id = lower_expr(builder, state, value);
            state.lowering_concurrent_argument = false;
            id
        }
        AstExpr::List { elements, span } => {
            let elements = elements
                .iter()
                .map(|element| lower_concurrent_argument(builder, state, element))
                .collect::<Vec<_>>();
            push(
                builder,
                state,
                ExprKind::List(elements.into_boxed_slice()),
                span.clone(),
            )
        }
        _ => lower_expr(builder, state, value),
    }
}

/// The deprecated `as: T` witness of a call whose callee declares no parameter
/// named `as`: the 1.2 alias for the type argument `<T>`. Returns the index of
/// the witness argument and the type it names. A witness that is not a plain
/// path does not name a type, so it stays an ordinary argument and fails there.
fn as_witness_type(
    params: Option<&[ParamInfo]>,
    type_arguments: &[TypeExpr],
    arguments: &[orna_syntax::Argument],
) -> Option<(usize, TypeExpr)> {
    if !type_arguments.is_empty() || params?.iter().any(|param| param.name == Name::new("as")) {
        return None;
    }
    let index = arguments
        .iter()
        .position(|argument| argument.name.as_deref() == Some("as"))?;
    let value = &arguments[index].value;
    let mut segments = Vec::new();
    if !flatten_path(value, &mut segments) {
        return None;
    }
    let path = segments
        .iter()
        .map(|(segment, _)| (*segment).to_owned())
        .collect();
    Some((
        index,
        TypeExpr::Name {
            path,
            arguments: Vec::new(),
            span: value.span().clone(),
        },
    ))
}

/// Lowers one call, including its classified callee and bound arguments.
fn lower_call(
    builder: &mut Builder,
    state: &mut BodyState,
    callee: &AstExpr,
    type_arguments: &[TypeExpr],
    arguments: &[orna_syntax::Argument],
    input: Option<ExprId>,
    span: &SyntaxSpan,
) -> ExprId {
    let module = state.module;
    let callee_span = callee.span();
    let target = classify_callee(builder, state, callee, span);
    let (callee, params) = match target {
        Some(classified) => classified,
        None => {
            // The callee did not resolve: keep the occurrence the callee
            // wrote, and lower the arguments so their names are still indexed.
            super::mark_callee(&mut builder.program, module, &callee_span);
            for argument in arguments {
                let _ = lower_expr(builder, state, &argument.value);
            }
            return error_expr(builder, state, span.clone());
        }
    };
    super::mark_callee(&mut builder.program, module, &callee_span);
    let witness = as_witness_type(params.as_deref(), type_arguments, arguments);
    let witness_index = witness.as_ref().map(|(index, _)| *index);
    let witnessed = witness.map(|(_, ty)| vec![ty]);
    let type_arguments = witnessed.as_deref().unwrap_or(type_arguments);
    let callee = attach_type_arguments(builder, state, callee, type_arguments);
    let concurrent = super::calls::starts_concurrent_bodies(builder, &callee);
    let lowered = arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != witness_index)
        .map(|(_, argument)| {
            let value = if concurrent {
                lower_concurrent_argument(builder, state, &argument.value)
            } else {
                lower_expr(builder, state, &argument.value)
            };
            (
                argument.name.as_ref().map(|name| Name::new(name)),
                value,
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
    let ty = call_type(builder, &callee);
    push_typed(
        builder,
        state,
        ExprKind::Call(Box::new(Call {
            callee,
            callee_span: callee_span.clone(),
            args: args.into_boxed_slice(),
            piped,
        })),
        span.clone(),
        ty,
    )
}

/// The declared result type of a call, when the callee states one.
fn call_type(builder: &mut Builder, callee: &crate::Callee) -> Ty {
    match callee {
        crate::Callee::Source { def, .. }
        | crate::Callee::Native { def, .. }
        | crate::Callee::Sys { def, .. } => match &builder.pending[def.index()].kind {
            super::declare::PendingKind::Function(function) => function.ret,
            super::declare::PendingKind::Built(DefKind::Fn(function)) => function.ret,
            _ => builder.program.tys.error(),
        },
        crate::Callee::Ctor { def } if !builder.is_unit(*def) => {
            builder.program.tys.intern(TyKind::Adt(*def, Box::new([])))
        }
        _ => builder.program.tys.error(),
    }
}

/// Records the module and definition prefixes of a dotted path, excluding the
/// final segment, which the caller records with the resolution it computed.
/// A prefix that does not resolve is not recorded here: the caller reports the
/// failing root itself, so recording it again would count one unresolved name
/// twice. Only the root segment carries `context`; a later segment is a member
/// and records as a field.
pub(crate) fn record_prefix_uses(
    builder: &mut Builder,
    module: crate::ModuleId,
    body: Option<crate::BodyId>,
    segments: &[(Name, SyntaxSpan)],
    context: crate::UseContext,
) {
    for index in 0..segments.len().saturating_sub(1) {
        let names = segments[..=index]
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let target = match builder.resolve_path(module, &names) {
            crate::resolve::PathKind::Module(module) => NameTarget::Module(module),
            crate::resolve::PathKind::Def(def) => NameTarget::Def(def),
            crate::resolve::PathKind::Ambiguous => NameTarget::Ambiguous,
            crate::resolve::PathKind::Member { base, member } if member == index => {
                NameTarget::Def(base)
            }
            _ => continue,
        };
        let (name, span) = &segments[index];
        match body {
            Some(body) => super::record_body_use(
                &mut builder.program,
                module,
                body,
                span.clone(),
                name.clone(),
                if index == 0 {
                    context
                } else {
                    crate::UseContext::Field
                },
                target,
            ),
            None => super::record_use(
                &mut builder.program,
                module,
                span.clone(),
                name.clone(),
                if index == 0 {
                    context
                } else {
                    crate::UseContext::Field
                },
                target,
            ),
        }
    }
}

/// Lowers a nominal or variant construction.
fn lower_construct(
    builder: &mut Builder,
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
    let last_span = path
        .last()
        .map_or_else(|| span.clone(), |segment| segment.span.clone());
    let path_segments = path
        .iter()
        .zip(&segments)
        .map(|(segment, name)| (name.clone(), segment.span.clone()))
        .collect::<Vec<_>>();
    record_prefix_uses(
        builder,
        module,
        Some(state.id),
        &path_segments,
        crate::UseContext::Value,
    );
    let resolved = match builder.resolve_path(module, &segments) {
        crate::resolve::PathKind::Member { base, .. } => builder.member_def(base, &last).map_or(
            crate::resolve::PathKind::Unresolved,
            crate::resolve::PathKind::Def,
        ),
        other => other,
    };
    let def = match resolved {
        crate::resolve::PathKind::Def(def) => {
            super::record_value_use(
                &mut builder.program,
                module,
                last_span,
                last,
                NameTarget::Def(def),
            );
            Some(def)
        }
        _ => {
            super::record_value_use(
                &mut builder.program,
                module,
                last_span,
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
        let name_span = SyntaxSpan::new(field.span.start, field.span.start + field.name.len());
        if let Some(field_def) = def.and_then(|def| {
            pats::payload(builder, def)
                .into_iter()
                .find(|candidate| builder.def_name(*candidate).as_str() == field.name)
        }) {
            super::record_body_use(
                &mut builder.program,
                module,
                state.id,
                name_span,
                Name::new(&field.name),
                crate::UseContext::Field,
                NameTarget::Def(field_def),
            );
            lowered.push((field_def, value));
        } else {
            super::record_body_use(
                &mut builder.program,
                module,
                state.id,
                name_span,
                Name::new(&field.name),
                crate::UseContext::Field,
                NameTarget::Unresolved,
            );
        }
    }
    match def {
        Some(def) => {
            let ty = builder.program.tys.intern(TyKind::Adt(def, Box::new([])));
            push_typed(
                builder,
                state,
                ExprKind::Construct {
                    def,
                    fields: lowered.into_boxed_slice(),
                },
                span.clone(),
                ty,
            )
        }
        None => error_expr(builder, state, span.clone()),
    }
}

/// Lowers a lambda: its own body, its parameter slots and its captures.
fn lower_lambda(
    builder: &mut Builder,
    state: &mut BodyState,
    parameters: &[orna_syntax::LambdaParameter],
    body: &AstExpr,
    span: &SyntaxSpan,
) -> ExprId {
    let owner = state.owner;
    let module = state.module;
    let id = builder.allocate_body(owner);
    let mut lambda = BodyState::new(id, owner, module);
    lambda.concurrent = std::mem::take(&mut state.lowering_concurrent_argument);
    lambda.push_scope(body.span().start);
    for (index, parameter) in parameters.iter().enumerate() {
        let ty = match &parameter.annotation {
            Some(annotation) => super::types::lower_type(builder, module, &[], annotation),
            None => builder.program.tys.error(),
        };
        let name = match &parameter.pattern {
            orna_syntax::Pattern::Name(name, _) => Name::new(name),
            _ => Name::new(&format!("_arg{index}")),
        };
        let local = lambda.add_local(name.clone(), ty, parameter.span.clone());
        lambda.params.push(local);
        if matches!(parameter.pattern, orna_syntax::Pattern::Name(..)) {
            super::record_binding_use(
                &mut builder.program,
                module,
                lambda.id,
                parameter.span.clone(),
                name,
                local,
                false,
            );
        }
    }
    let free = free_names(body);
    for (name, free_span) in free {
        let name = Name::new(&name);
        // A name the lambda itself binds is not a capture, even when the
        // enclosing body has a slot of the same name.
        if lambda.local(&name).is_some() {
            continue;
        }
        if let Some(outer) = state.local(&name) {
            let outer_local = state.locals[outer].clone();
            let inner = lambda.add_local(outer_local.name.clone(), outer_local.ty, free_span);
            // A capture is blocked when this lambda is created inside a concurrent body, or when the
            // enclosing body reached the slot through its own blocked capture.
            let blocked =
                state.concurrent || state.captures.iter().any(|c| c.inner == outer && c.blocked);
            lambda.captures.push(Capture {
                outer,
                inner,
                blocked,
            });
        }
    }
    let mut prologue = Vec::new();
    for (index, parameter) in parameters.iter().enumerate() {
        if matches!(parameter.pattern, orna_syntax::Pattern::Name(..)) {
            continue;
        }
        let slot = lambda.params[index];
        let expected = lambda.locals[slot].ty;
        let init = push(
            builder,
            &mut lambda,
            ExprKind::Local(slot),
            parameter.pattern.span().clone(),
        );
        // The old evaluator binds a destructured parameter directly and charges no step for this read.
        lambda.exprs[init].weight = 0;
        let pat =
            pats::lower_pattern_with(builder, &mut lambda, &parameter.pattern, Some(expected));
        prologue.push(Stmt::Let { pat, init });
    }
    let root = lower_expr(builder, &mut lambda, body);
    lambda.root = Some(if prologue.is_empty() {
        root
    } else {
        let block = push(
            builder,
            &mut lambda,
            ExprKind::Block {
                stmts: prologue.into_boxed_slice(),
                tail: Some(root),
            },
            body.span(),
        );
        // The prologue wrapper is lowering scaffolding; only the body's own nodes are charged.
        lambda.exprs[block].weight = 0;
        block
    });
    lambda.pop_scope(body.span().end);
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
