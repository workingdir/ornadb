//! The parameter-list binder check for lambdas.
//!
//! Lowering binds a pattern parameter through a prologue `let` whose
//! initializer reads the parameter slot. A parameter list that binds one name
//! twice (for example `({a}, a) => a`) is rejected by the old evaluator at
//! call time. This stage does not reproduce that rejection, so it declines to
//! create such a closure instead of running it.

use orna_hir::{Body, ExprKind, Name, Pat, PatId, Stmt};

/// Whether the parameter list of `body` binds one name twice.
pub(crate) fn binds_a_name_twice(body: &Body) -> bool {
    let prologue: &[Stmt] = match &body.exprs[body.root].kind {
        ExprKind::Block { stmts, .. } => &stmts[..],
        _ => &[],
    };
    let mut referenced = vec![false; body.params.len()];
    let mut names: Vec<Name> = Vec::new();
    for statement in prologue {
        let Stmt::Let { pat, init } = statement else {
            break;
        };
        let ExprKind::Local(slot) = &body.exprs[*init].kind else {
            break;
        };
        let Some(index) = body.params.iter().position(|param| param == slot) else {
            break;
        };
        referenced[index] = true;
        collect_binders(body, *pat, &mut names);
    }
    for (index, param) in body.params.iter().enumerate() {
        if !referenced[index] {
            names.push(body.locals[*param].name.clone());
        }
    }
    (1..names.len()).any(|index| names[..index].contains(&names[index]))
}

/// Pushes the name of every slot `pat` binds.
fn collect_binders(body: &Body, pat: PatId, names: &mut Vec<Name>) {
    match &body.pats[pat] {
        Pat::Wild | Pat::Lit(_) => {}
        Pat::Bind { local } => names.push(body.locals[*local].name.clone()),
        Pat::Tuple(items) => {
            for item in items.iter() {
                collect_binders(body, *item, names);
            }
        }
        Pat::List {
            prefix,
            rest,
            suffix,
        } => {
            for item in prefix.iter().chain(suffix.iter()) {
                collect_binders(body, *item, names);
            }
            if let Some(rest) = rest {
                names.push(body.locals[*rest].name.clone());
            }
        }
        Pat::Record(fields) => {
            for (_, item) in fields.iter() {
                collect_binders(body, *item, names);
            }
        }
        Pat::Ctor { fields, .. } => {
            for (_, item) in fields.iter() {
                collect_binders(body, *item, names);
            }
        }
    }
}
