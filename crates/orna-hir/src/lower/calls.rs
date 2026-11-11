//! Classifying a written callee (design section 2.6).
//!
//! A call's callee is resolved and classified once, during lowering:
//!
//! | written callee | [`Callee`] |
//! |---|---|
//! | a local or parameter (`f(..)`) | `Value` |
//! | a function definition | `Source`, `Native` or `Sys`, by declaring module |
//! | a variant or nominal type | `Ctor` |
//! | `Table.insert(..)` and siblings | `TableOp` |
//! | `fail`, `parallel`, `Some`, `now`, ... | `Intrinsic` |
//! | a relation stage (`filter`, `map`, ...) with no declaration | `Relational` |
//! | any other member call (`row.field(..)`, `x.divide(..)`) | `Pending` |
//!
//! A path the language cannot resolve is a diagnostic and
//! [`crate::ExprKind::Error`]; it is never a silent guess.

use orna_syntax::{Expr as AstExpr, SyntaxSpan, TypeExpr};

use crate::{Callee, DefId, ExprId, ExprKind, FieldRef, Name, NameTarget};

use super::BodyState;
use super::classify::{Classified, ParamInfo};
use super::declare::Builder;
use super::exprs::{error_expr, flatten_path, lower_expr, push};

/// One callee and the parameters its arguments bind to, when it declares them.
pub(crate) type ClassifiedCallee = Option<(Callee, Option<Vec<ParamInfo>>)>;

/// Whether a callee starts concurrent bodies: the lambdas it receives run as
/// separate activations that capture snapshots (design 03, "Concurrent bodies
/// capture snapshots"). These are `parallel`, `race` and `timeout`, and
/// `sys.start`.
pub(crate) fn starts_concurrent_bodies(builder: &Builder<'_>, callee: &Callee) -> bool {
    match callee {
        Callee::Intrinsic(form) => matches!(
            form,
            crate::IntrinsicForm::Parallel
                | crate::IntrinsicForm::Race
                | crate::IntrinsicForm::Timeout
        ),
        Callee::Sys { def, .. } => builder.def_name(*def).as_str() == "start",
        _ => false,
    }
}

/// Resolves and classifies the callee of a call.
pub(crate) fn classify_callee(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    callee: &AstExpr,
    span: &SyntaxSpan,
) -> ClassifiedCallee {
    if let AstExpr::Name { text, span } = callee {
        return classify_name(builder, state, text, span);
    }
    let mut segments = Vec::new();
    if flatten_path(callee, &mut segments) {
        return classify_path(builder, state, &segments, span);
    }
    if let AstExpr::Field { base, name, .. } = callee {
        let base = lower_expr(builder, state, base);
        return Some((
            Callee::Pending {
                base,
                name: Name::new(name),
            },
            None,
        ));
    }
    let value = lower_expr(builder, state, callee);
    Some((Callee::Value { callee: value }, None))
}

/// Classifies a bare-name callee.
fn classify_name(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    text: &str,
    span: &SyntaxSpan,
) -> ClassifiedCallee {
    let name = Name::new(text);
    if state.local(&name).is_some() {
        let value = lower_expr(
            builder,
            state,
            &AstExpr::Name {
                text: text.to_owned(),
                span: span.clone(),
            },
        );
        return Some((Callee::Value { callee: value }, None));
    }
    let module = state.module;
    let target = builder.resolve_bare(module, &name);
    // A bare name with no declaration is still a language-defined relation
    // stage or terminal when its spelling is one; record it as the form it
    // denotes, not as an unresolved name.
    let recorded =
        if target == NameTarget::Unresolved && super::classify::rel_op(name.as_str()).is_some() {
            NameTarget::Intrinsic
        } else {
            target
        };
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        span.clone(),
        name.clone(),
        crate::UseContext::Value,
        recorded,
    );
    match target {
        NameTarget::Def(def) => classified_def(builder, state, def, span),
        NameTarget::Intrinsic => {
            let form = super::classify::intrinsic_form(name.as_str())?;
            Some((Callee::Intrinsic(form), None))
        }
        NameTarget::Unresolved => match super::classify::rel_op(name.as_str()) {
            Some(op) => Some((Callee::Relational { op }, None)),
            None => {
                builder.diagnose(
                    module,
                    crate::diag::UNRESOLVED,
                    format!("`{name}` is not in scope"),
                    span.clone(),
                );
                None
            }
        },
        NameTarget::Ambiguous => {
            builder.diagnose(
                module,
                crate::diag::AMBIGUOUS,
                format!("`{name}` is ambiguous"),
                span.clone(),
            );
            None
        }
        NameTarget::Module(_) => {
            builder.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                format!("`{name}` is a module, not a function"),
                span.clone(),
            );
            None
        }
        NameTarget::Local(_) => unreachable!("a local was resolved before module scope"),
    }
}

/// Classifies a dotted-path callee.
fn classify_path(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    segments: &[(&str, SyntaxSpan)],
    span: &SyntaxSpan,
) -> ClassifiedCallee {
    let module = state.module;
    let names = segments
        .iter()
        .map(|(text, _)| Name::new(text))
        .collect::<Vec<_>>();
    let last = names.last().cloned().unwrap_or_else(|| Name::new(""));
    let last_span = segments
        .last()
        .map_or_else(|| span.clone(), |(_, span)| span.clone());
    let path_segments = segments
        .iter()
        .zip(&names)
        .map(|((_, span), name)| (name.clone(), span.clone()))
        .collect::<Vec<_>>();
    if state.local(&names[0]).is_some() {
        let base = lower_path_value(builder, state, &segments[..segments.len() - 1]);
        return Some((Callee::Pending { base, name: last }, None));
    }
    super::exprs::record_prefix_uses(
        builder,
        module,
        Some(state.id),
        &path_segments,
        crate::UseContext::Value,
    );
    match builder.resolve_path(module, &names) {
        crate::resolve::PathKind::Def(def) => {
            super::record_body_use(
                &mut builder.program,
                module,
                state.id,
                last_span.clone(),
                last,
                crate::UseContext::Value,
                NameTarget::Def(def),
            );
            classified_def(builder, state, def, span)
        }
        crate::resolve::PathKind::Member { base, .. } => {
            classify_member_path(builder, state, segments, base, last, last_span, span)
        }
        crate::resolve::PathKind::MissingMember { module: target, .. } => {
            super::record_body_use(
                &mut builder.program,
                module,
                state.id,
                last_span.clone(),
                last.clone(),
                crate::UseContext::Value,
                NameTarget::Unresolved,
            );
            if builder.program.modules[target].origin == crate::ModuleOrigin::Sys {
                builder.diagnose(
                    module,
                    crate::diag::UNSUPPORTED,
                    "the `sys` declarations are not represented yet",
                    last_span,
                );
            } else {
                builder.diagnose(
                    module,
                    crate::diag::UNRESOLVED,
                    format!("`{last}` is not an export of the module"),
                    last_span,
                );
            }
            None
        }
        crate::resolve::PathKind::NotAModule
        | crate::resolve::PathKind::Ambiguous
        | crate::resolve::PathKind::Unresolved => {
            report_unresolved_root(builder, state, segments, &names)
        }
        crate::resolve::PathKind::Module(_) => {
            builder.diagnose(
                module,
                crate::diag::UNSUPPORTED,
                "a module is not a function",
                last_span,
            );
            None
        }
    }
}

/// Classifies a path whose prefix names a definition, then a member of it (`Note.insert`).
fn classify_member_path(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    segments: &[(&str, SyntaxSpan)],
    base: DefId,
    last: Name,
    last_span: SyntaxSpan,
    span: &SyntaxSpan,
) -> ClassifiedCallee {
    let module = state.module;
    if let Some(unit) = builder
        .member_def(base, &last)
        .filter(|def| builder.is_unit(*def))
    {
        super::record_body_use(
            &mut builder.program,
            module,
            state.id,
            last_span.clone(),
            last,
            crate::UseContext::Value,
            NameTarget::Def(unit),
        );
        return classified_def(builder, state, unit, span);
    }
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        last_span,
        last.clone(),
        crate::UseContext::Value,
        NameTarget::Def(base),
    );
    if builder.def_is_table(base) {
        if let Some(op) = super::classify::table_op(last.as_str()) {
            return Some((Callee::TableOp { table: base, op }, None));
        }
        if let Some(op) = super::classify::rel_op(last.as_str()) {
            return Some((Callee::Relational { op }, None));
        }
    }
    let base = lower_path_value(builder, state, &segments[..segments.len() - 1]);
    Some((Callee::Pending { base, name: last }, None))
}

/// Reports a path whose root name does not resolve.
fn report_unresolved_root(
    builder: &mut Builder<'_>,
    state: &BodyState,
    segments: &[(&str, SyntaxSpan)],
    names: &[Name],
) -> ClassifiedCallee {
    // The root is the name that failed, not the member written after
    // it: a member spelling is not a name in any scope.
    super::record_body_use(
        &mut builder.program,
        state.module,
        state.id,
        segments[0].1.clone(),
        names[0].clone(),
        crate::UseContext::Value,
        NameTarget::Unresolved,
    );
    builder.diagnose(
        state.module,
        crate::diag::UNRESOLVED,
        format!("`{}` is not in scope", names[0]),
        segments[0].1.clone(),
    );
    None
}

/// The classification of a resolved definition.
fn classified_def(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    def: crate::DefId,
    span: &SyntaxSpan,
) -> ClassifiedCallee {
    match builder.classify_def(def) {
        Classified::Declared { callee, params } => Some((callee, Some(params))),
        Classified::Open { callee } => Some((callee, None)),
        Classified::NotCallable => {
            builder.diagnose(
                state.module,
                crate::diag::UNSUPPORTED,
                format!("`{}` is not callable", builder.def_name(def)),
                span.clone(),
            );
            None
        }
    }
}

/// Lowers the value part of a dotted path, dropping the member name.
fn lower_path_value(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    segments: &[(&str, SyntaxSpan)],
) -> ExprId {
    let Some((first, first_span)) = segments.first() else {
        return error_expr(builder, state, SyntaxSpan::new(0, 0));
    };
    let mut value = super::exprs::lower_name(builder, state, first, first_span);
    for (name, span) in &segments[1..] {
        let span = span.clone();
        value = push(
            builder,
            state,
            ExprKind::Field {
                base: value,
                field: FieldRef::Name(Name::new(name)),
            },
            span,
        );
    }
    value
}

/// Attaches written type arguments to a callee that carries them.
pub(crate) fn attach_type_arguments(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    callee: Callee,
    type_arguments: &[TypeExpr],
) -> Callee {
    if type_arguments.is_empty() {
        return callee;
    }
    let lowered = type_arguments
        .iter()
        .map(|argument| super::types::lower_type(builder, state.module, &[], argument))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    match callee {
        Callee::Source { def, .. } => Callee::Source {
            def,
            targs: lowered,
        },
        Callee::Native { def, .. } => Callee::Native {
            def,
            targs: lowered,
        },
        Callee::Sys { def, .. } => Callee::Sys {
            def,
            targs: lowered,
        },
        other => other,
    }
}
