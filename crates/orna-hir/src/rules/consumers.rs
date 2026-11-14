//! Durable consumer rules: one source consumed twice in one `parallel([...])`
//! (E9101, ORNA-CONSUMER-001).
//!
//! The rule compares the written path of each list element, not its
//! resolution: a name that does not resolve still keys by its text, so the
//! duplicate is reported whatever the environment supplies. Only source
//! modules (`ModuleOrigin::Project`) are checked.

use std::collections::BTreeSet;

use super::Found;
use crate::{
    ArgValue, Body, Callee, DefId, ExprId, ExprKind, FieldRef, HirDiagnostic, IntrinsicForm,
    LocalId, ModuleOrigin, Name, Program, diag,
};

/// The message the old checker gives for the duplicate (`DIAG_AMBIGUOUS_DURABLE_CONSUMER`).
const MESSAGE: &str = "same durable source consumed twice without distinct consumer identity";

/// The root a written path starts from. A resolved name keys by its definition
/// or local slot; an unresolved one keys by its text.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Root {
    Def(DefId),
    Local(LocalId),
    Name(Name),
}

/// Reports each `parallel([...])` list that names one source twice.
pub(super) fn check(program: &Program) -> Vec<Found> {
    let mut found = Vec::new();
    for body in program.bodies.iter() {
        let module = program.defs[body.owner].module;
        if program.modules[module].origin != ModuleOrigin::Project {
            continue;
        }
        for expr in body.exprs.iter() {
            let ExprKind::Call(call) = &expr.kind else {
                continue;
            };
            if !matches!(call.callee, Callee::Intrinsic(IntrinsicForm::Parallel)) {
                continue;
            }
            let [argument] = &*call.args else {
                continue;
            };
            let ArgValue::Expr(list) = argument.value else {
                continue;
            };
            let ExprKind::List(elements) = &body.exprs[list].kind else {
                continue;
            };
            if let Some(span) = first_repeat(body, elements) {
                found.push((
                    module,
                    HirDiagnostic::new(diag::DURABLE_CONSUMER_DUPLICATE, MESSAGE, span),
                ));
            }
        }
    }
    found
}

/// The span of the first element whose path an earlier element already named,
/// or `None` when every path is distinct.
fn first_repeat(body: &Body, elements: &[ExprId]) -> Option<orna_syntax_ast::SourceSpan> {
    let mut seen = BTreeSet::new();
    for &element in elements {
        if let Some(path) = written_path(body, element)
            && !seen.insert(path)
        {
            return Some(body.exprs[element].span.clone());
        }
    }
    None
}

/// The root and the field names of a path written as a name or a field chain
/// (`mail.google.sync`), or `None` for any other expression.
fn written_path(body: &Body, expr: ExprId) -> Option<(Root, Vec<Name>)> {
    let mut fields = Vec::new();
    let mut current = expr;
    loop {
        match &body.exprs[current].kind {
            ExprKind::Field {
                base,
                field: FieldRef::Name(name),
            } => {
                fields.push(name.clone());
                current = *base;
            }
            ExprKind::Def(def) => return Some((Root::Def(*def), reversed(fields))),
            ExprKind::Local(local) => return Some((Root::Local(*local), reversed(fields))),
            ExprKind::BareName(name) => return Some((Root::Name(name.clone()), reversed(fields))),
            _ => return None,
        }
    }
}

fn reversed(mut fields: Vec<Name>) -> Vec<Name> {
    fields.reverse();
    fields
}
