//! Assertion rules: owner pipes (A091-002), effects (A091-007) and table
//! scope of module assertions (A091-003, A091-012).
//!
//! Every assertion body lives in its own arena, so a flat scan of
//! [`crate::Body::exprs`] sees each expression once. Lambdas and source
//! functions called from the body are separate bodies and are followed
//! explicitly, with a visited set so recursion through them terminates.

use std::collections::{BTreeSet, HashSet};

use super::Found;
use crate::effect_pass::direct_effects;
use crate::{
    ArgValue, BodyId, Callee, DefId, DefKind, EffectKind, EffectSet, ExprKind, FnBody,
    HirDiagnostic, ModuleOrigin, Program, diag,
};

/// Judges every assertion declared in a source module.
pub(super) fn check(program: &Program) -> Vec<Found> {
    let mut found = Vec::new();
    for (id, def) in program.defs.iter_enumerated() {
        let DefKind::Assertion(assertion) = &def.kind else {
            continue;
        };
        if program.modules[def.module].origin != ModuleOrigin::Project {
            continue;
        }
        let body = &program.bodies[assertion.body];
        let module = def.module;
        let owner = assertion.owner;
        let is_table = matches!(program.defs[owner].kind, DefKind::Table(_));
        if owner != id && !is_table {
            // Type-owned assertions need the type pass (A091-004, not on HIR yet).
            continue;
        }
        let root_span = body.exprs[body.root].span.clone();
        if is_table && owns_pipe(program, assertion.body, owner) {
            let message = "remove the repeated table owner before the assertion predicate";
            found.push((
                module,
                HirDiagnostic::new(diag::ASSERTION_OWNER_PIPE, message, root_span.clone()),
            ));
        }
        if let Some(message) = forbidden_effect(&effects_of(program, assertion.body)) {
            found.push((
                module,
                HirDiagnostic::new(diag::ASSERTION_EFFECT, message, root_span.clone()),
            ));
        }
        if owner == id {
            let mut tables = BTreeSet::new();
            let mut visited = HashSet::new();
            collect_tables(program, assertion.body, &mut tables, &mut visited);
            match tables.len() {
                0 => {
                    let message = "module assertions must depend on at least two distinct tables";
                    found.push((
                        module,
                        HirDiagnostic::new(diag::ASSERTION_SCOPE, message, root_span),
                    ));
                }
                1 => {
                    let message = "a one-table invariant belongs inside that table";
                    found.push((
                        module,
                        HirDiagnostic::new(diag::ASSERTION_ONE_TABLE, message, root_span),
                    ));
                }
                _ => {}
            }
        }
    }
    found
}

/// Whether a table assertion pipes its own table into the predicate.
fn owns_pipe(program: &Program, body: BodyId, owner: DefId) -> bool {
    let body = &program.bodies[body];
    let ExprKind::Call(call) = &body.exprs[body.root].kind else {
        return false;
    };
    if !call.piped {
        return false;
    }
    let Some(first) = call.args.first() else {
        return false;
    };
    let ArgValue::Expr(lhs) = first.value else {
        return false;
    };
    matches!(body.exprs[lhs].kind, ExprKind::Def(def) if def == owner)
}

/// The effects an assertion body performs, including lambdas it defines.
fn effects_of(program: &Program, body: BodyId) -> EffectSet {
    let mut effects = EffectSet::empty();
    let mut visited = HashSet::new();
    collect_effects(program, body, &mut effects, &mut visited);
    effects
}

fn collect_effects(
    program: &Program,
    body: BodyId,
    effects: &mut EffectSet,
    visited: &mut HashSet<BodyId>,
) {
    if !visited.insert(body) {
        return;
    }
    for expr in program.bodies[body].exprs.iter() {
        match &expr.kind {
            ExprKind::Call(call) => match &call.callee {
                Callee::Source { def, .. } => {
                    if let DefKind::Fn(function) = &program.defs[*def].kind {
                        effects.join_with(function.effects);
                    }
                }
                other => effects.join_with(direct_effects(program, other)),
            },
            ExprKind::Lambda { body, .. } => collect_effects(program, *body, effects, visited),
            _ => {}
        }
    }
}

/// The message for a forbidden effect, following the old checker's wording.
fn forbidden_effect(effects: &EffectSet) -> Option<&'static str> {
    if effects.is_empty() {
        return None;
    }
    Some(if effects.contains(EffectKind::Network) {
        "declaration assertion uses forbidden network effect"
    } else if effects.contains(EffectKind::Filesystem) {
        "declaration assertion uses forbidden filesystem effect"
    } else {
        "assertion has forbidden effects or failure"
    })
}

/// Every table an assertion body reads, through reads, table operations and
/// the source functions and lambdas it reaches.
fn collect_tables(
    program: &Program,
    body: BodyId,
    tables: &mut BTreeSet<DefId>,
    visited: &mut HashSet<BodyId>,
) {
    if !visited.insert(body) {
        return;
    }
    for expr in program.bodies[body].exprs.iter() {
        match &expr.kind {
            ExprKind::Def(def) => note_def(program, *def, tables, visited),
            ExprKind::Call(call) => match &call.callee {
                Callee::TableOp { table, .. } => {
                    tables.insert(*table);
                }
                Callee::Source { def, .. } => note_def(program, *def, tables, visited),
                _ => {}
            },
            ExprKind::Lambda { body, .. } => collect_tables(program, *body, tables, visited),
            _ => {}
        }
    }
}

/// Records a referenced table, or follows a referenced source function.
fn note_def(
    program: &Program,
    def: DefId,
    tables: &mut BTreeSet<DefId>,
    visited: &mut HashSet<BodyId>,
) {
    match &program.defs[def].kind {
        DefKind::Table(_) => {
            tables.insert(def);
        }
        DefKind::Fn(function) => {
            if let FnBody::Source(body) = function.body {
                collect_tables(program, body, tables, visited);
            }
        }
        _ => {}
    }
}
