//! Namespace paths as values and callees (ORNA-NS-005 to ORNA-NS-008).
//!
//! `base.name` where `base` names a module is an export of that module, not a
//! field access on a value: a database table, a namespace's exported definition,
//! or the miss of one. The field-access lowering in `exprs` asks this module
//! first, so the module-export decision lives in one place.

use orna_syntax::{Expr as AstExpr, SyntaxSpan};

use crate::{DefId, ExprId, ExprKind, Name, NameTarget};

use super::BodyState;
use super::declare::Builder;
use super::exprs::{error_expr, flatten_path, push, record_prefix_uses};

/// `base.name` where `base` names a module that has no export `name`: the miss is reported on the
/// member (`ORNA-S012-UNRESOLVED`), as a call path reports it, not on the module used as a value.
/// A `sys` module keeps its own diagnostic, so it is not claimed here.
pub(super) fn missing_module_export(
    builder: &mut Builder,
    state: &mut BodyState,
    base: &AstExpr,
    name: &str,
    span: &SyntaxSpan,
) -> Option<ExprId> {
    let mut segments = Vec::new();
    if !flatten_path(base, &mut segments) {
        return None;
    }
    let root = Name::new(segments.first()?.0);
    if state.local(&root).is_some() {
        return None;
    }
    let mut names = segments
        .iter()
        .map(|(text, _)| Name::new(text))
        .collect::<Vec<_>>();
    names.push(Name::new(name));
    let crate::resolve::PathKind::MissingMember {
        module: target,
        member,
    } = builder.resolve_path(state.module, &names)
    else {
        return None;
    };
    if builder.program.modules[target].origin == crate::ModuleOrigin::Sys {
        return None;
    }
    // The miss is the segment at `member`; an earlier segment is reported by its own access.
    let (miss, miss_span) = match segments.get(member) {
        Some((text, span)) => (Name::new(text), span.clone()),
        None => (
            Name::new(name),
            SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end),
        ),
    };
    let module = state.module;
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        miss_span.clone(),
        miss.clone(),
        crate::UseContext::Value,
        NameTarget::Unresolved,
    );
    builder.diagnose(
        module,
        crate::diag::UNRESOLVED,
        format!("`{miss}` is not an export of the module"),
        miss_span,
    );
    Some(error_expr(builder, state, span.clone()))
}

/// The namespace path and export `base.name` denotes when `base` names a module (ORNA-NS-005 to
/// ORNA-NS-008): a database table, a namespace's exported definition. A `sys` module is not one: its
/// natives keep their own spelling.
pub(super) fn namespace_member(
    builder: &Builder,
    state: &BodyState,
    base: &AstExpr,
    name: &str,
) -> Option<(Vec<(Name, SyntaxSpan)>, DefId)> {
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
    let crate::resolve::PathKind::Module(module) = builder.resolve_path(state.module, &names)
    else {
        return None;
    };
    if builder.program.modules[module].origin == crate::ModuleOrigin::Sys {
        return None;
    }
    let mut full = names;
    full.push(Name::new(name));
    match builder.resolve_path(state.module, &full) {
        crate::resolve::PathKind::Def(def) => {
            let path = segments
                .iter()
                .map(|(text, span)| (Name::new(text), span.clone()))
                .collect();
            Some((path, def))
        }
        _ => None,
    }
}

/// Whether `root` names the `sys` module in `state`, not a local that shadows it.
pub(super) fn is_sys_rooted(state: &BodyState, root: &str) -> bool {
    root == "sys" && state.local(&Name::new(root)).is_none()
}

/// Lowers a `sys`-rooted value path (`sys.a.b`): the host's admission decides the whole path first,
/// then the path resolves as a value.
pub(super) fn lower_sys_value(
    builder: &mut Builder,
    state: &mut BodyState,
    segments: &[(&str, SyntaxSpan)],
    span: &SyntaxSpan,
) -> ExprId {
    let path = segments.iter().map(|(text, _)| *text).collect::<Vec<_>>();
    if super::admission::reject_unlinked(builder, state.module, &path, span.clone()) {
        return error_expr(builder, state, span.clone());
    }
    resolve_sys_value(builder, state, segments, span)
}

/// Resolves an already admitted `sys`-rooted value path to its definition, or to a variant or static
/// of an admitted type. A `sys` declaration not represented yet is UNSUPPORTED.
pub(super) fn resolve_sys_value(
    builder: &mut Builder,
    state: &mut BodyState,
    segments: &[(&str, SyntaxSpan)],
    span: &SyntaxSpan,
) -> ExprId {
    let module = state.module;
    let names = segments
        .iter()
        .map(|(text, _)| Name::new(text))
        .collect::<Vec<_>>();
    let path = segments
        .iter()
        .zip(&names)
        .map(|((_, span), name)| (name.clone(), span.clone()))
        .collect::<Vec<_>>();
    let Some((last, last_span)) = path.last().cloned() else {
        return error_expr(builder, state, span.clone());
    };
    let resolved =
        match builder.resolve_path(module, &names) {
            crate::resolve::PathKind::Def(def) => Some(def),
            _ => names.split_last().and_then(|(last, owner)| {
                match builder.resolve_path(module, owner) {
                    crate::resolve::PathKind::Def(owner) => {
                        builder.member_def(owner, last).filter(|def| {
                            builder.is_variant(*def)
                                || builder.is_static(*def)
                                || builder.is_unit(*def)
                        })
                    }
                    _ => None,
                }
            }),
        };
    let Some(def) = resolved else {
        super::record_body_use(
            &mut builder.program,
            module,
            state.id,
            last_span.clone(),
            last,
            crate::UseContext::Value,
            NameTarget::Unresolved,
        );
        builder.diagnose(
            module,
            crate::diag::UNSUPPORTED,
            "the `sys` declarations are not represented yet",
            last_span,
        );
        return error_expr(builder, state, span.clone());
    };
    record_prefix_uses(
        builder,
        module,
        Some(state.id),
        &path,
        crate::UseContext::Value,
    );
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        last_span,
        last,
        crate::UseContext::Value,
        NameTarget::Def(def),
    );
    push(builder, state, ExprKind::Def(def), span.clone())
}
